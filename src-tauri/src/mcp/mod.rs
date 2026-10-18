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
//! `gitpulse_delete_task`, `gitpulse_merge_tasks`), which write the GitPulse
//! task profile behind the repository trust gate. `gitpulse_delete_task` and
//! `gitpulse_merge_tasks` are the destructive tools — both end in the store's
//! soft delete — and are annotated `destructiveHint: true`.
//! Any tool that would run a git mutation must go through
//! `harness::guard_command`, and every writing tool must be added to the
//! allowlist in `no_advertised_tool_offers_an_ungated_mutation`.

#[cfg(test)]
mod adversarial_deferral_tests;
#[cfg(test)]
mod adversarial_tests;
mod budget;
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
use crate::tasks::file_tasks as ft;
use crate::workbench::intake;
use validate::{tool_args, Spec, ToolArgs};

/// MCP 2.0 / 2026-07-28. The only modern version this build speaks.
pub const PROTOCOL_VERSION: &str = "2026-07-28";
pub const SERVER_NAME: &str = "gitpulse-mcp";

/// Legacy handshake-based revisions this binary still answers, newest first.
/// The first is what an `initialize` for any version not listed here is answered
/// with: the lifecycle spec says the server "MUST respond with another protocol
/// version it supports. This SHOULD be the *latest* version supported by the
/// server", and it is the client's job to disconnect if it cannot use it.
pub const LEGACY_VERSIONS: &[&str] = &["2025-11-25", "2024-11-05"];

/// Largest single JSON-RPC message, in either direction, matching the harness
/// sidecar's frame cap. The one owner of the figure: the transport refuses to
/// read or write a line past it, and every result budget (tool results, ledger
/// pages, resource documents) is derived from it, so they cannot drift apart.
pub const MAX_FRAME_BYTES: usize = 4 * 1024 * 1024;

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
/// Longest value accepted for a `gitpulse_ledger_events` filter. The widest
/// column a filter compares is the object, which the ledger caps well below
/// this, so a longer value could never match a stored row.
pub const LEDGER_FILTER_MAX_BYTES: usize = 4_096;
/// Ceiling on `gitpulse_active_changes`, matching what the backend enforces.
pub const CHANGES_MAX_LIMIT: u32 = 500;
/// Ceiling on the code-intelligence token budget.
pub const BUDGET_MAX: u32 = 200_000;
/// Longest `repo_path` / symbol argument accepted. Well past any real path,
/// short enough that a megabyte of `A`s is refused before it reaches git.
pub(crate) const MAX_ARG_CHARS: u32 = 4_096;
/// Longest resource `uri` accepted: a `repo_path` at its limit with every
/// character percent-encoded as four bytes, and the scheme and facet around it.
/// A `uri` is echoed back in the document and in errors, so one the size of the
/// frame would make every response about it too big to send.
pub(crate) const MAX_URI_BYTES: usize = MAX_ARG_CHARS as usize * 12 + 256;

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

// Tool arguments.
//
// One struct per tool, declared with `tool_args!`, prints the advertised
// `inputSchema` and is what the handler reads. A handler cannot read an argument
// the schema does not declare (the field does not exist), and a declared argument
// nothing reads is dead code, which `clippy -D warnings` refuses. Before this,
// both directions were held by a test that scanned the handler's source text.

pub(crate) fn repo_spec() -> Spec {
    Spec::chars(
        "Absolute path to a git repository",
        1,
        MAX_ARG_CHARS as usize,
    )
}

pub(crate) fn path_spec(description: &'static str) -> Spec {
    Spec::chars(description, 1, MAX_ARG_CHARS as usize)
}

pub(crate) fn budget_spec() -> Spec {
    Spec::int("Maximum tokens of results", 1, u64::from(BUDGET_MAX))
}

/// A task named the way the board names it: its `task_id` or its `item_id`.
fn task_ref_spec(description: &'static str) -> Spec {
    Spec::chars(description, 1, 128)
}

/// The largest revision a client that reads numbers as doubles can name.
const MAX_REVISION: u64 = 9_007_199_254_740_991;

fn revision_spec(description: &'static str) -> Spec {
    Spec::int(description, 1, MAX_REVISION)
}

/// A caller's idempotency key for a task write: the same key with the same
/// request replays the first answer instead of writing again, so a call whose
/// response was lost can be sent again safely.
fn request_id_spec() -> Spec {
    Spec::chars(
        "Optional idempotency key (letters, digits, '-' and '_'). Send the same key to repeat this call after a lost response; the store answers with the first result instead of writing twice",
        1,
        120,
    )
}

const SEVERITIES: &[&str] = &["none", "low", "medium", "high", "critical"];

tool_args! {
    /// The tools that take only the repository.
    struct RepoArgs {
        repo_path: String = repo_spec(),
    }
}

tool_args! {
    /// The code-intelligence tools that answer one budgeted question about a
    /// repository.
    struct BudgetArgs {
        repo_path: String = repo_spec(),
        budget: Option<u32> = budget_spec(),
    }
}

tool_args! {
    struct SearchArgs {
        repo_path: String = repo_spec(),
        query: String = path_spec("Symbol name or prefix"),
        budget: Option<u32> = budget_spec(),
    }
}

tool_args! {
    struct ActiveChangesArgs {
        repo_path: String = repo_spec(),
        worktree_path: Option<String> = path_spec("Worktree to inspect; defaults to repo_path"),
        limit: Option<u32> = Spec::int(
            "Maximum files to return (default 200, max 500)",
            1,
            u64::from(CHANGES_MAX_LIMIT),
        ),
    }
}

tool_args! {
    struct ChangeContextArgs {
        repo_path: String = repo_spec(),
        worktree_path: Option<String> = path_spec("Worktree to describe; defaults to repo_path"),
    }
}

tool_args! {
    struct LedgerEventsArgs {
        repo_path: String = repo_spec(),
        cursor: Option<i64> = Spec::int(
            "Where to continue. Oldest first (the default): return events with an id greater than this; 0 (the default) starts at the beginning of history. With order \"desc\": return events with an id less than this; omit it to start at the newest event.",
            0,
            i64::MAX.unsigned_abs(),
        ),
        limit: Option<u32> = Spec::int(
            "Maximum number of events (default 50, max 1000)",
            1,
            u64::from(LEDGER_MAX_LIMIT),
        ),
        order: Option<String> = Spec::one_of(
            "\"asc\" (the default) pages oldest first; \"desc\" returns the newest events first, so {order: \"desc\", limit: 5} is the five newest without reading older pages",
            &["asc", "desc"],
        ),
        since_ts: Option<String> = Spec::bytes(
            "Only events appended at or after this `ts_utc`, spelled as the ledger stores it: YYYY-MM-DDTHH:MM:SS.mmmZ (24 characters, UTC)",
            1,
            24,
        ),
        action: Option<String> = Spec::bytes("Only events with exactly this action, e.g. session.command", 1, LEDGER_FILTER_MAX_BYTES),
        object: Option<String> = Spec::bytes("Only events whose object is exactly this (the stored, redacted and length-capped text). Not index-backed: stops at `limit` matches", 1, LEDGER_FILTER_MAX_BYTES),
        worktree_path: Option<String> = Spec::bytes("Only events recorded for exactly this worktree path. Not index-backed: stops at `limit` matches", 1, LEDGER_FILTER_MAX_BYTES),
        task_id: Option<String> = Spec::bytes("Only events attributed to this task id", 1, LEDGER_FILTER_MAX_BYTES),
        session_id: Option<String> = Spec::bytes("Only events from this agent session id", 1, LEDGER_FILTER_MAX_BYTES),
        after_ref: Option<String> = Spec::bytes("Only events whose after_ref is exactly this (a commit or ref value). Not index-backed: stops at `limit` matches", 1, LEDGER_FILTER_MAX_BYTES),
    }
}

tool_args! {
    struct TaskViewArgs {
        repo_path: String = repo_spec(),
        task_id: Option<String> = path_spec("Optional task id to filter evidence, gaps, and runs"),
    }
}

tool_args! {
    struct AddTaskArgs {
        repo_path: String = repo_spec(),
        title: String = Spec::chars("Task title: one line", 1, ft::MAX_TASK_TITLE),
        description: Option<String> = Spec::bytes(
            "Background and problem statement (Markdown)",
            0,
            ft::MAX_TASK_DESCRIPTION,
        ),
        task_id: Option<String> = Spec::bytes(
            "Stable key for this task (letters, digits, '-', '_', '.'), e.g. 'gp-auth-flow'. Defaults to 'gp-' plus the title's slug.",
            1,
            ft::MAX_TASK_KEY,
        ),
        status: Option<String> = Spec::one_of("Board column (default inbox)", &ft::STATUSES),
        priority: Option<u8> = Spec::int(
            "Priority: 0 (urgent), 1 (high), 2 (normal, default), 3 (low)",
            0,
            3,
        ),
        severity: Option<String> = Spec::one_of("Severity (default none)", SEVERITIES),
        kind: Option<String> = Spec::bytes(
            "Type, e.g. feature (default), bug, refactor, chore, spike",
            1,
            ft::MAX_KIND_BYTES,
        ),
        owner: Option<String> = Spec::bytes("Assignee, e.g. '@alice'", 1, ft::MAX_OWNER_BYTES),
        due: Option<String> = Spec::chars("Due date as YYYY-MM-DD, or Unix seconds", 1, 32),
        labels: Option<Vec<String>> =
            Spec::bytes_array("Labels", ft::MAX_TASK_LABELS, ft::MAX_LABEL_BYTES),
        repositories: Option<Vec<String>> = Spec::bytes_array(
            "Other related repository names; kept as a section of the description",
            ft::MAX_TASK_REPOSITORIES,
            ft::MAX_REPOSITORY_BYTES,
        ),
        planned_files: Option<Vec<String>> = Spec::bytes_array(
            "Files the work is expected to touch; kept as a section of the description",
            ft::MAX_TASK_PLANNED_FILES,
            ft::MAX_PLANNED_FILE_BYTES,
        ),
        acceptance_criteria: Option<Vec<String>> = Spec::bytes_array(
            "Definition of done, one item each",
            ft::MAX_TASK_CRITERIA,
            ft::MAX_CRITERION_BYTES,
        ),
        logs: Option<String> = Spec::bytes(
            "Raw logs or traces kept verbatim as evidence",
            0,
            ft::MAX_TASK_LOGS,
        ),
        overwrite: Option<bool> = Spec::flag(
            "Replace the content of the task already filed under this task_id, or of the board task whose item_id is task_id (default false). Its column position and links are kept. This is how related work is folded into an existing task: send its current content plus yours.",
        ),
        reviewed_related: Option<Vec<String>> = Spec::chars_array(
            "item_ids of the open tasks a related_tasks_exist refusal listed, once you have read them and this task is genuinely separate work",
            intake::MAX_REVIEWED_RELATED,
            128,
        ),
    }
}

tool_args! {
    struct ImportTasksArgs {
        repo_path: String = repo_spec(),
        tasks_dir: Option<String> = Spec::bytes(
            "Tasks directory relative to the repository root (default 'tasks')",
            1,
            ft::MAX_TASKS_DIR,
        ),
        replace: Option<bool> = Spec::flag(
            "Overwrite the board copy of briefs already imported with the file's current content (default false)",
        ),
    }
}

tool_args! {
    struct ListTasksArgs {
        repo_path: String = repo_spec(),
        status: Option<String> = Spec::one_of("Only this column", &ft::STATUSES),
        archived: Option<bool> = Spec::flag(
            "true lists the archive instead of the board (default false)",
        ),
        limit: Option<u64> = Spec::int("Maximum tasks to return (default 50, max 200)", 1, 200),
        cursor: Option<String> = Spec::chars("next_cursor from a previous page", 1, 180),
    }
}

tool_args! {
    struct GetTaskArgs {
        repo_path: String = repo_spec(),
        task_id: String = task_ref_spec(
            "The task_id it was added or imported with, or its board item_id",
        ),
    }
}

tool_args! {
    struct CompleteTaskArgs {
        repo_path: String = repo_spec(),
        task_id: String = task_ref_spec(
            "The task id from your brief ('Task: <id>'), its board item_id, or the task_id it was filed with",
        ),
        status: Option<String> = Spec::one_of("Where the task goes (default done)", &intake::AGENT_STATUSES),
        summary: Option<String> = Spec::chars(
            "What you changed and how you verified it, in a few lines; appended to the task's logs",
            1,
            intake::MAX_SUMMARY_CHARS,
        ),
        expected_revision: Option<i64> = revision_spec(
            "Only move it if the task is still at this revision (from your brief or gitpulse_get_task); otherwise it is refused so you can re-read it",
        ),
        request_id: Option<String> = request_id_spec(),
    }
}

tool_args! {
    struct DeleteTaskArgs {
        repo_path: String = repo_spec(),
        task_id: String = task_ref_spec(
            "The task_id it was filed with, or its board item_id from gitpulse_list_tasks",
        ),
        reason: String = Spec::chars(
            "Why the task is being deleted, in a line or two; recorded in its history",
            1,
            intake::MAX_REASON_CHARS,
        ),
        expected_revision: Option<i64> = revision_spec(
            "Only delete it if the task is still at this revision (from gitpulse_list_tasks or gitpulse_get_task); otherwise it is refused so you can re-read it",
        ),
        even_if_running: Option<bool> = Spec::flag(
            "Delete it even though an agent attempt on it may still be working (task_in_use). Only when that agent is you, or the person asked for this. Default false",
        ),
        request_id: Option<String> = request_id_spec(),
    }
}

tool_args! {
    struct MergeSourceArgs {
        task_id: String = task_ref_spec("Its task_id or board item_id"),
        expected_revision: Option<i64> = revision_spec(
            "Only merge if this task is still at this revision when the merge starts",
        ),
    }
}

tool_args! {
    struct MergeTasksArgs {
        repo_path: String = repo_spec(),
        into_task_id: String = task_ref_spec("The task that stays: its task_id or board item_id"),
        into_expected_revision: Option<i64> = revision_spec(
            "Only merge if the target is still at this revision when the merge starts",
        ),
        sources: Vec<MergeSourceArgs> = Spec::ObjArray {
            desc: "The tasks to fold in and delete",
            min_items: 1,
            max_items: intake::MAX_MERGE_SOURCES,
            item: MergeSourceArgs::object_schema,
        },
        reason: String = Spec::chars(
            "Why these are one task, in a line or two; recorded on the target and on every source",
            1,
            intake::MAX_REASON_CHARS,
        ),
        even_if_running: Option<bool> = Spec::flag(
            "Merge a source even though an agent attempt on it may still be working (task_in_use). Only when that agent is you, or the person asked for this. Default false",
        ),
    }
}

tool_args! {
    struct CodeintelImpactArgs {
        repo_path: String = repo_spec(),
        target: String = path_spec("Symbol or file path"),
        budget: Option<u32> = budget_spec(),
    }
}

tool_args! {
    struct CodeintelDependenciesArgs {
        repo_path: String = repo_spec(),
        file_path: String = path_spec("Repo-relative file whose dependencies to read"),
        budget: Option<u32> = budget_spec(),
    }
}

tool_args! {
    struct CodeintelTraceArgs {
        repo_path: String = repo_spec(),
        from: String = path_spec("Symbol to trace from"),
        to: String = path_spec("Symbol to trace to"),
        budget: Option<u32> = budget_spec(),
    }
}

tool_args! {
    struct SuspectsArgs {
        repo_path: String = repo_spec(),
        symptom: String = path_spec("Symbol that is misbehaving — a qualified node id, or a bare name"),
        since: String = path_spec("Revision the window opens at (a commit, tag or branch)"),
        // Bounded, like every other numeric argument here. The cone grows with
        // depth and each file it reaches costs a `git blame` subprocess, so an
        // unbounded depth is an unbounded amount of work asked for by one
        // integer. Ten is already far past the point where a dependency chain
        // explains a regression.
        depth: Option<u32> = Spec::int(
            "How many call edges out from the symptom to follow (default 3)",
            1,
            10,
        ),
    }
}

tool_args! {
    struct ProvenanceArgs {
        repo_path: String = repo_spec(),
        commit_sha: String = path_spec("Commit SHA"),
        base_branch: Option<String> = path_spec("Base branch to compute distance against (default HEAD)"),
    }
}

/// Read a tool's arguments, which [`validate_arguments`] has already accepted.
///
/// Extraction repeats none of the bounds; it refuses what a schema cannot say
/// about Rust: a number that is not exactly an integer, one too wide for its
/// target, a string over a byte bound. Never a default in place of a refusal.
pub(crate) fn parse<T: ToolArgs>(name: &str, arguments: &Value) -> Result<T, String> {
    T::from_args(arguments).map_err(|violations| validate::refusal(name, &violations))
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

/// `deferred`: present and true only on a facet whose own Git read the spawn
/// gate deferred (and that stayed deferred after one retry).
fn deferred_property() -> Value {
    json!({ "type": "boolean", "description": "Present and true only when a Git read was deferred under load (and stayed deferred after one retry); nothing was examined, so ask again after retry_after_ms." })
}

/// `retry_after_ms`: what accompanies [`deferred_property`].
fn retry_after_property() -> Value {
    json!({ "type": "integer", "minimum": 0, "description": "With deferred: the soonest worth asking again, one refill interval of the spawn rate budget." })
}

/// An open object facet that can carry the deferral fields.
fn deferrable_object() -> Value {
    json!({
        "type": "object",
        "properties": {
            "deferred": deferred_property(),
            "retry_after_ms": retry_after_property()
        }
    })
}

/// The `agents` facet: it states no cause of its own, so a deferral shows on
/// it and on `live` and each `live.kinds` entry that carries the cause.
fn agents_output() -> Value {
    let mut agents = deferrable_object();
    agents["properties"]["live"] = json!({
        "type": "object",
        "properties": {
            "deferred": deferred_property(),
            "retry_after_ms": retry_after_property(),
            "kinds": { "type": "array", "items": deferrable_object() }
        }
    });
    agents
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

/// The collision facet ([`insights::CollisionRisk`]): the answer of
/// `gitpulse_collision_risk`, and the `collisions` field of
/// `gitpulse_change_context`, which carries the whole facet so its rows keep
/// the `ok` and coverage counts that say whether anything was looked at.
fn collision_risk_output() -> Value {
    ok_error_output(
        json!({
            "overlapping_files": { "type": "integer" },
            "worktrees_involved": { "type": "integer" },
            "scanned_worktrees": { "type": "integer" },
            "unscanned_worktrees": { "type": "integer" },
            "failed_worktrees": { "type": "integer" },
            "truncated": { "type": "boolean" },
            "items": { "type": "array" },
            "shared_worktree_files": { "type": "integer" },
            "shared_worktrees": { "type": "array" },
            "sessions_ok": { "type": "boolean" },
            "sessions_error": { "type": "string" },
            "deferred": deferred_property(),
            "retry_after_ms": retry_after_property()
        }),
        &[
            "overlapping_files",
            "scanned_worktrees",
            "unscanned_worktrees",
            "failed_worktrees",
            "items",
            "shared_worktree_files",
            "shared_worktrees",
            "sessions_ok",
        ],
    )
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
            "One-shot snapshot of worktrees, agent sessions (agents.live: running Claude Code and Codex sessions per worktree path, found by executable in the process table; a kind that could not be observed is reported unknown, never zero), uncommitted changes, overlapping dirty files, ledger, and code-graph availability. Facets fail independently so a missed scan never looks clean.",
            RepoArgs::properties(),
            &RepoArgs::required(),
            json!({
                "type": "object",
                "properties": {
                    "repo_path": { "type": "string" },
                    "worktrees": deferrable_object(),
                    "agents": agents_output(),
                    "changes": deferrable_object(),
                    "collisions": collision_risk_output(),
                    "ledger": deferrable_object(),
                    "codeintel": { "type": "object" }
                },
                "required": ["repo_path", "worktrees", "agents", "changes", "collisions", "ledger", "codeintel"]
            }),
        ),
        tool(
            "gitpulse_status",
            "Repository status",
            "Inspect repository status, worktrees, ledger, and code graph availability",
            RepoArgs::properties(),
            &RepoArgs::required(),
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
            ActiveChangesArgs::properties(),
            &ActiveChangesArgs::required(),
            ok_error_output(
                json!({
                    "repo_path": { "type": "string" },
                    "worktree_path": { "type": "string" },
                    "files": { "type": "array" },
                    "total": { "type": "integer" },
                    "shown": { "type": "integer" },
                    "truncated": { "type": "boolean" },
                    "deferred": deferred_property(),
                    "retry_after_ms": retry_after_property()
                }),
                &["repo_path", "files", "total", "shown", "truncated"],
            ),
        ),
        tool(
            "gitpulse_collision_risk",
            "Collision risk",
            "Files with uncommitted changes in more than one worktree — parallel agent checkouts editing the same path — and, in shared_worktrees, the dirty files of any worktree two or more live agent sessions share, which overlapping_files cannot see. Unscanned worktrees are counted, never implied clean; sessions_ok is false when an agent kind could not be observed.",
            RepoArgs::properties(),
            &RepoArgs::required(),
            collision_risk_output(),
        ),
        tool(
            "gitpulse_change_context",
            "Change context",
            "In-flight context for one worktree: branch, dirty files, parked merge/rebase, bound task, and collisions that involve it.",
            ChangeContextArgs::properties(),
            &ChangeContextArgs::required(),
            json!({
                "type": "object",
                "properties": {
                    "repo_path": { "type": "string" },
                    "worktree": deferrable_object(),
                    "worktree_ok": { "type": "boolean" },
                    "worktree_error": { "type": "string" },
                    "task_id": { "type": "string" },
                    "task_ok": { "type": "boolean" },
                    "task_error": { "type": "string" },
                    "changes": deferrable_object(),
                    "collisions": collision_risk_output(),
                    "operation": { "type": ["object", "null"] },
                    "operation_ok": { "type": "boolean" },
                    "operation_error": { "type": "string" },
                    "deferred": deferred_property(),
                    "retry_after_ms": retry_after_property()
                },
                "required": ["repo_path", "worktree", "worktree_ok", "task_id", "task_ok", "changes", "collisions", "operation", "operation_ok"]
            }),
        ),
        tool(
            "gitpulse_ledger_events",
            "Ledger events",
            "Read durable ledger event history (actor, tool, verdict, changes), oldest first from `cursor` by default. `order: \"desc\"` returns the newest first (`{order: \"desc\", limit: 5}` is the five newest, read by index without paging older rows); `since_ts`, `action`, `object`, `worktree_path`, `task_id`, `session_id` and `after_ref` filter by exact value and combine with AND; `since_ts`, `action`, `task_id` and `session_id` are index-backed, while `object`, `worktree_path` and `after_ref` are not: they walk the ledger newest-first or oldest-first and stop at `limit` matches, so one that matches nothing reads the whole ledger (an event time filter is not offered: event time rides in `detail_json`, not in a column). Page by passing the last `id` you saw back as `cursor` (an upper bound when `order` is `desc`); `truncated` says whether more follow. `ts_utc` is when the row was appended; `detail_json.occurred_at` carries event time when the source knew it, with `occurred_at_basis` (`ingest` means `occurred_at` is the ingest time, the source gave none that could be trusted) and, for transcript and reflog rows, `task_basis` (`bound`, `unbound` or `unresolved`) for how `task_id` was decided. On transcript rows `occurred_at` comes from the agent-written transcript, so task attribution there is as trustworthy as the transcript itself, not an authority. A page is also bounded in bytes (about 1 MiB), so it can hold fewer rows than `limit` with `truncated` true: continue from `next_cursor`. A field too long to return is cut, and says so inside its value (`detail_json.gitpulse_truncated`, `detail_json.object_truncated` for `object`, or a `…[truncated N bytes]` suffix).",
            LedgerEventsArgs::properties(),
            &LedgerEventsArgs::required(),
            json!({
                "type": "object",
                "properties": {
                    "ok": { "type": "boolean" },
                    "error": { "type": "string" },
                    "events": { "type": "array" },
                    "returned": { "type": "integer" },
                    "truncated": { "type": "boolean" },
                    "next_cursor": {
                        "type": ["integer", "null"],
                        "description": "Id of the last event returned; null when this page is empty, so there is nothing further to continue from yet."
                    }
                },
                "required": ["ok", "events", "returned", "truncated"]
            }),
        ),
        tool(
            "gitpulse_task_view",
            "Task view",
            "Read task leases plus evidence, gaps, and verification runs from dc-store. Never acquires a lease.",
            TaskViewArgs::properties(),
            &TaskViewArgs::required(),
            json!({ "type": "object" }),
        ),
        mutating_tool(
            "gitpulse_add_task",
            "Add task",
            "File a task on the GitPulse task board for this repository — the board a person sees and launches agents from. Prefer fewer, larger tasks: before filing, call gitpulse_list_tasks and fold related work into one task (one coherent concern each) rather than one task per finding. A new task is refused with related_tasks_exist while open tasks look like the same work; the refusal lists them. Then either fold yours into one (call again with that task's item_id as task_id and overwrite: true, carrying its content plus yours), or, if it is genuinely separate, pass reviewed_related with every listed item_id. Requires the repository to be trusted in GitPulse. task_id is the stable key: adding the same task_id again is refused unless overwrite is true, so a retry never makes a duplicate; overwrite also accepts a board item_id. Planned files are kept as a section of the description. Recorded in the board's event history.",
            AddTaskArgs::properties(),
            &AddTaskArgs::required(),
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
            ImportTasksArgs::properties(),
            &ImportTasksArgs::required(),
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
            "List the tasks on the GitPulse task board for this repository, in board order. Done tasks stay on the board until a person archives them; archived tasks are off the board and are listed only with archived: true (the archive, most recently completed first). Each task carries archived and completed_at. repository null means no task has ever been filed under it — not that its tasks are done. A page is never cut to fit the result budget (that would skip rows or leave `has_more` with no `next_cursor`): a page too large to return is a tool error telling you to retry with a smaller `limit`.",
            ListTasksArgs::properties(),
            &ListTasksArgs::required(),
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
            GetTaskArgs::properties(),
            &GetTaskArgs::required(),
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
            "Move your GitPulse board task to done when the work is finished. Done is evidence-gated: it lands only if Manvi's verification passed for your worktree's current head, otherwise the task goes to review with the reason (held_for_review: true) for a person to judge, and no argument changes that (or to review to hand it to a person, or in_progress when you start). Pass the task id from your brief's 'Task:' line, or the task_id it was filed with. Only the status changes, plus an optional summary appended to the task's logs so the person sees what you did; a person's concurrent edit is never overwritten. Call it only when the acceptance criteria are met and your verification passed. Idempotent: the same move twice is reported as unchanged. A task already done is never reopened. Requires the repository to be trusted in GitPulse.",
            CompleteTaskArgs::properties(),
            &CompleteTaskArgs::required(),
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
            DeleteTaskArgs::properties(),
            &DeleteTaskArgs::required(),
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
        destructive_tool(
            "gitpulse_merge_tasks",
            "Merge tasks",
            "Merge duplicate or overlapping tasks on the GitPulse task board into one that stays. Each source becomes a '## Merged from <item_id>' section of the target's description, with its status, priority and description; the target also gets every source's acceptance criteria and labels, the most urgent priority, highest severity and earliest due date among them, and a log block with your reason. Its title, status and owner stay. Then each source is deleted — the same soft delete as gitpulse_delete_task, with your reason in its history and the target named, so an agent launched on it is pointed at the target — and only at the revision that was copied, so no edit is ever lost with it. Source logs stay in each source's history. There is no unmerge over MCP; only the person can bring a source back. Refused, with nothing written: a source linked to another repository too (shared_task), one an agent may still be working on (task_in_use, unless even_if_running), a done target with an open source (target_done), a target whose description the person locked (field_locked), a merge too large for one task (merge_too_large). If it is interrupted or the board changes under it, it reports ok: false with outcome partial and every source's outcome; run the same call again to finish — nothing is copied twice. Idempotent: an already-merged source is reported as already_merged. Requires the repository to be trusted in GitPulse.",
            MergeTasksArgs::properties(),
            &MergeTasksArgs::required(),
            json!({
                "type": "object",
                "properties": {
                    "ok": { "type": "boolean" },
                    "outcome": { "type": "string", "enum": ["merged", "unchanged", "partial"] },
                    "item_id": { "type": "string" },
                    "title": { "type": "string" },
                    "status": { "type": "string" },
                    "priority": { "type": "integer" },
                    "revision": { "type": "integer" },
                    "target_live": { "type": "boolean" },
                    "repository": { "type": "object" },
                    "sources": { "type": "array" },
                    "added": { "type": "object" },
                    "escalated": { "type": "array" },
                    "logs_kept_in_history": { "type": "array" },
                    "next_step": { "type": ["string", "null"] },
                    "sequence": { "type": ["integer", "null"] }
                },
                "required": ["ok", "outcome", "item_id", "target_live", "repository", "sources", "added", "escalated", "logs_kept_in_history", "next_step"]
            }),
        ),
        tool(
            "gitpulse_codeintel_search",
            "Symbol search",
            "Search symbols across indexed code files via in-process devmap",
            SearchArgs::properties(),
            &SearchArgs::required(),
            codeintel_output(),
        ),
        tool(
            "gitpulse_codeintel_impact",
            "Impact / blast radius",
            "Compute downstream blast radius / affected callers for a symbol or file",
            CodeintelImpactArgs::properties(),
            &CodeintelImpactArgs::required(),
            codeintel_output(),
        ),
        tool(
            "gitpulse_codeintel_dependencies",
            "File dependencies",
            "What a file depends on — the mirror of impact. Answers outgoing edges from the devmap code graph",
            CodeintelDependenciesArgs::properties(),
            &CodeintelDependenciesArgs::required(),
            codeintel_output(),
        ),
        tool(
            "gitpulse_codeintel_trace",
            "Trace between symbols",
            "Shortest edge path between two symbols in the devmap code graph",
            CodeintelTraceArgs::properties(),
            &CodeintelTraceArgs::required(),
            codeintel_output(),
        ),
        tool(
            "gitpulse_codeintel_dead_symbols",
            "Dead symbols",
            "Unreferenced symbols in the indexed code graph. Unavailable is reported, never an empty list that reads as none found.",
            BudgetArgs::properties(),
            &BudgetArgs::required(),
            codeintel_output(),
        ),
        tool(
            "gitpulse_codeintel_suspects",
            "Regression suspects",
            "Which commits since `since` could have caused a symptom. Blames only the lines belonging to symbols the symptom depends on, so the answer is the dependency cone rather than the file's recent history. The window ends at the commit the index was built at, never HEAD. An empty list with available=true means nothing in the window touched the cone; refusals are reported in walk_incomplete and make the list a lower bound.",
            SuspectsArgs::properties(),
            &SuspectsArgs::required(),
            // [`crate::codeintel::CodeintelSuspectsPayload`]: the ranked list
            // and what was examined, kept together because `scope` is what
            // tells an empty list that is a finding from one that was cut short.
            json!({
                "type": "object",
                "properties": {
                    "response": codeintel_output(),
                    "scope": {
                        "type": ["object", "null"],
                        "description": "Null only when the run was refused before a window could be established.",
                        "properties": {
                            "indexed_head": { "type": "string" },
                            "since": { "type": "string" },
                            "cone_size": { "type": "integer" },
                            "blamed_symbols": { "type": "integer" },
                            "refusals": { "type": "array" }
                        },
                        "required": ["indexed_head", "since", "cone_size", "blamed_symbols", "refusals"]
                    }
                },
                "required": ["response", "scope"]
            }),
        ),
        tool(
            "gitpulse_provenance",
            "Commit provenance",
            "Read Git-native verification notes and confidence decay for a commit. `distance` is how many commits the base has that the commit lacks (`rev-list --count <commit>..<base>`), so it is 0 for a commit AHEAD of the base as well as for the base tip: it says how far the base moved, not whether the commit is on it. Read `landed_on_base` / `on_base` (reachable from the base), `ahead`, `behind`, `merge_base` and `base_oid` for that; each is null with `base_relation_reason` when it could not be measured (unresolvable base, shallow clone), never false.",
            ProvenanceArgs::properties(),
            &ProvenanceArgs::required(),
            json!({
                "type": "object",
                "properties": {
                    "commit_sha": { "type": "string" },
                    "distance": { "type": ["integer", "null"] },
                    "confidence": { "type": ["number", "null"] },
                    "is_fresh": { "type": "boolean" },
                    "unmeasured_reason": { "type": "string" },
                    "notes_readable": { "type": "boolean" },
                    "verification": { "type": ["object", "null"] },
                    "session": { "type": ["object", "null"] },
                    "landed_on_base": { "type": ["boolean", "null"] },
                    "on_base": { "type": ["boolean", "null"] },
                    "base_oid": { "type": ["string", "null"] },
                    "merge_base": { "type": ["string", "null"] },
                    "ahead": { "type": ["integer", "null"] },
                    "behind": { "type": ["integer", "null"] },
                    "base_relation_reason": { "type": "string" }
                },
                "required": [
                    "commit_sha", "distance", "confidence", "is_fresh", "unmeasured_reason",
                    "notes_readable", "verification", "session", "landed_on_base", "on_base",
                    "base_oid", "merge_base", "ahead", "behind", "base_relation_reason"
                ]
            }),
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
                "instructions": "GitPulse control plane. It never mutates git state; its only writes are the task-board tools — gitpulse_add_task, gitpulse_import_tasks, gitpulse_complete_task, gitpulse_delete_task and gitpulse_merge_tasks — which a person sees on the GitPulse board. gitpulse_delete_task removes a card and gitpulse_merge_tasks folds duplicate cards into one and removes the rest (both soft, with a required reason kept in history, and no undelete over MCP); use them only for tasks that should not exist separately, never to finish one, and not on a task another agent is running. Keep the board small: before gitpulse_add_task, read gitpulse_list_tasks and group related findings into one task, or fold them into an existing one with overwrite; gitpulse_add_task refuses a new task that looks like open work until you have reviewed it. When the board already holds duplicates, merge them with gitpulse_merge_tasks rather than deleting all but one. Start with gitpulse_insights for a repository snapshot (worktrees, agent sessions, collisions, ledger, code graph). Use gitpulse_change_context before editing, and gitpulse_collision_risk before parallel agent work. The same views are addressable as gitpulse://<facet>{+repo_path} resources; gitpulse://server/manifest describes the whole surface. Pass absolute repo_path on every call. When GitPulse launched you on a task, your brief names it on its Task: line; when the work is finished and verified, call gitpulse_complete_task with that id and a short summary.",
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

/// A tool's successful result, fitted to `budget` bytes on the wire (see
/// [`budget`]): the payload as `structuredContent` in the modern era, and its
/// compact serialization in a text block in both.
///
/// The text is there because the spec asks for it ("a tool that returns
/// structured content SHOULD also return the serialized JSON in a TextContent
/// block") and legacy clients read nothing else. It is compact because
/// pretty-printing cost a third again, and bounded in the modern era because
/// carrying every large payload twice at full size did the rest. A payload too
/// large to cut is a tool execution error that says so.
fn complete_tool_result(modern: bool, payload: Value, budget: usize) -> Value {
    match budget::fit(modern, payload, budget) {
        Ok(fitted) => {
            let mut body = json!({
                "content": [{ "type": "text", "text": fitted.text }],
                "isError": false
            });
            if let Some(structured) = fitted.structured {
                body["structuredContent"] = structured;
            }
            envelope(modern, body)
        }
        Err(message) => tool_exec_error(modern, message),
    }
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
    Err(validate::refusal(name, &violations))
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
        return open_shared_profile(dc_store::Store::open(&path));
    }
    open_shared_profile(dc_store::Store::open_existing(&path))
}

/// The task profile is shared with the desktop app, which owns its schema.
/// This server may be older or newer than the app, so its handle never
/// upgrades: a request that needs the upgrade fails with
/// `schema_upgrade_required` and leaves the profile as it found it.
fn open_shared_profile(
    opened: Result<dc_store::Store, dc_store::StoreError>,
) -> Result<Option<dc_store::Store>, String> {
    let mut store = opened.map_err(|e| e.to_string())?;
    store.refuse_workbench_upgrades();
    Ok(Some(store))
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

/// How a read that the spawn gate deferred is asked once more.
///
/// `gitpulse-mcp` runs every spawn as background work that waits for budget
/// (`run_process_awaiting_background_budget`): a spawn short of budget waits
/// up to about 2 s (`SPAWN_QUEUE_BUDGET`) for the refill and is then refused
/// as a deferral, so a deferral here means the budget stayed at or below the
/// background reserve for that whole wait. One retry, after a delay of one
/// refill interval plus jitter of up to the same again (under 250 ms at the
/// production 8 tokens a second), so concurrent readers refused in the same
/// instant do not come back in the same instant.
///
/// The real bound on one call: a snapshot pass is soft-bounded at 10 s by
/// `SNAPSHOT_DEADLINE`, plus at most one spawn already waiting (2 s) when that
/// clock runs out. The retry is skipped unless the first pass ended, with its
/// delay, before `DEFERRAL_RETRY_WINDOW` (5 s) after the request began
/// (`the_retry_is_bounded_and_respects_the_deadline`). So the worst case is a
/// first pass that ends just inside the window, the delay, and a second pass of
/// up to 12 s: [`WORST_CASE_DEFERRED_READ`] (5 s + 2 x 12 s = 29 s as a ceiling
/// that counts both passes at their maximum), under the 60 s
/// `DEFAULT_CALL_BUDGET` (pinned by a test in `gitpulse-mcp`).
/// Time, sleeping and randomness are injected so a test is exact and instant.
pub(crate) struct DeferralRetry<'a> {
    pub sleep: &'a dyn Fn(std::time::Duration),
    /// A value in `0..bound`.
    pub jitter: &'a dyn Fn(u64) -> u64,
    pub now: &'a dyn Fn() -> std::time::Instant,
    /// The request's own deadline. A retry that could not finish before it is
    /// not started.
    pub deadline: std::time::Instant,
}

/// Longest a read may run, counted from the start of the request, and still be
/// retried: a call that already spent this long has no budget for a second
/// attempt. Well inside the transport's own per-call budget.
const DEFERRAL_RETRY_WINDOW: std::time::Duration = std::time::Duration::from_secs(5);

/// A ceiling on one insights read that was deferred and retried: the retry
/// window, plus two passes each at the snapshot's soft deadline and one spawn's
/// wait for budget. `gitpulse-mcp` asserts it stays under its per-call budget.
pub const WORST_CASE_DEFERRED_READ: std::time::Duration = std::time::Duration::from_secs(
    DEFERRAL_RETRY_WINDOW.as_secs()
        + 2 * (insights::SNAPSHOT_DEADLINE.as_secs()
            + crate::engine::git_cli::SPAWN_QUEUE_BUDGET.as_secs()),
);

/// A value in `0..bound` that differs between processes and between calls.
/// Not a CSPRNG and not meant to be: it only spreads retries apart.
fn process_jitter(bound: u64) -> u64 {
    use std::collections::hash_map::RandomState;
    use std::hash::{BuildHasher, Hasher};
    use std::sync::atomic::{AtomicU64, Ordering};
    static CALLS: AtomicU64 = AtomicU64::new(0);
    let mut hasher = RandomState::new().build_hasher();
    hasher.write_u64(CALLS.fetch_add(1, Ordering::Relaxed));
    hasher.finish() % bound.max(1)
}

/// Runs `read`; when its answer was a deferral, waits and runs it once more
/// and returns the second answer, whatever it was. Otherwise, or when the
/// deadline leaves no room, the first answer.
pub(crate) fn retry_once_on_deferral<T>(
    mut read: impl FnMut() -> T,
    deferred: impl Fn(&T) -> bool,
    retry: &DeferralRetry<'_>,
) -> T {
    let first = read();
    if !deferred(&first) {
        return first;
    }
    let base = insights::deferral_retry_after_ms();
    let delay = std::time::Duration::from_millis(base + (retry.jitter)(base));
    if (retry.now)() + delay >= retry.deadline {
        return first;
    }
    (retry.sleep)(delay);
    read()
}

/// [`retry_once_on_deferral`] for an insights payload: marks every deferred
/// facet (see [`insights::mark_deferrals`]) on whichever answer is returned,
/// so the reader sees `deferred` and `retry_after_ms` only when the retry
/// also deferred, and sees neither when it succeeded.
pub(crate) fn read_insights(read: impl Fn() -> Value, retry: &DeferralRetry<'_>) -> Value {
    retry_once_on_deferral(
        || {
            let mut value = read();
            let deferred = insights::mark_deferrals(&mut value);
            (value, deferred)
        },
        |(_, deferred)| *deferred,
        retry,
    )
    .0
}

/// Calls `body` with the production retry: real clock, real sleep, a window
/// counted from now.
fn with_production_retry<T>(body: impl FnOnce(&DeferralRetry<'_>) -> T) -> T {
    body(&DeferralRetry {
        sleep: &std::thread::sleep,
        jitter: &process_jitter,
        now: &std::time::Instant::now,
        deadline: std::time::Instant::now() + DEFERRAL_RETRY_WINDOW,
    })
}

/// The repository address a read resolves, retried once if the spawn gate
/// deferred the Git read behind it.
fn read_address(
    repo: &str,
    retry: &DeferralRetry<'_>,
) -> Result<crate::ledger::bindings::BindingAddress, String> {
    retry_once_on_deferral(
        || crate::ledger::bindings::repository_address_readonly(repo),
        |result| {
            result
                .as_ref()
                .is_err_and(|error| error.code == crate::ledger::bindings::DEFERRED_WORKTREE)
        },
        retry,
    )
    .map_err(|error| error.to_string())
}

fn handle_tool_call(name: &str, arguments: &Value) -> Result<Value, String> {
    // The schema refuses what it can say (bounds, enums, unknown keys); `parse`
    // then reads the accepted call into the tool's struct, and refuses what the
    // schema cannot (a number that is not exactly an integer). Neither step
    // ever substitutes a default for an argument that was sent.
    validate_arguments(name, arguments)?;
    match name {
        "gitpulse_insights" => {
            let a: RepoArgs = parse(name, arguments)?;
            Ok(with_production_retry(|retry| {
                read_insights(|| json!(insights::snapshot(&a.repo_path)), retry)
            }))
        }
        "gitpulse_status" => {
            let a: RepoArgs = parse(name, arguments)?;
            resources::status_document(&a.repo_path)
        }
        "gitpulse_active_changes" => {
            let a: ActiveChangesArgs = parse(name, arguments)?;
            Ok(with_production_retry(|retry| {
                read_insights(
                    || {
                        json!(insights::active_changes(
                            &a.repo_path,
                            a.worktree_path.as_deref(),
                            a.limit
                        ))
                    },
                    retry,
                )
            }))
        }
        "gitpulse_collision_risk" => {
            let a: RepoArgs = parse(name, arguments)?;
            Ok(with_production_retry(|retry| {
                read_insights(|| json!(insights::collision_risk(&a.repo_path)), retry)
            }))
        }
        "gitpulse_change_context" => {
            let a: ChangeContextArgs = parse(name, arguments)?;
            Ok(with_production_retry(|retry| {
                read_insights(
                    || {
                        json!(insights::change_context(
                            &a.repo_path,
                            a.worktree_path.as_deref()
                        ))
                    },
                    retry,
                )
            }))
        }
        "gitpulse_ledger_events" => ledger_events(parse(name, arguments)?),
        "gitpulse_task_view" => task_view(parse(name, arguments)?),
        "gitpulse_add_task" => {
            let a: AddTaskArgs = parse(name, arguments)?;
            let key = match a.task_id {
                Some(key) => key,
                None => format!("gp-{}", ft::slugify(&a.title)),
            };
            let task = intake::ExternalTask {
                key,
                title: a.title,
                description: a.description.unwrap_or_default(),
                status: a.status,
                priority: a.priority,
                severity: a.severity,
                kind: a.kind,
                owner: a.owner,
                due: a.due,
                labels: a.labels.unwrap_or_default(),
                acceptance_criteria: a.acceptance_criteria.unwrap_or_default(),
                planned_files: a.planned_files.unwrap_or_default(),
                repositories: a.repositories.unwrap_or_default(),
                logs: a.logs,
            };
            let reviewed = a.reviewed_related.unwrap_or_default();
            let store =
                open_task_profile(true)?.ok_or("the GitPulse task profile could not be created")?;
            intake::add_task(
                &store,
                &a.repo_path,
                task,
                a.overwrite.unwrap_or(false),
                &reviewed,
            )
            .map_err(workbench_message)
        }
        "gitpulse_import_tasks" => {
            let a: ImportTasksArgs = parse(name, arguments)?;
            let store =
                open_task_profile(true)?.ok_or("the GitPulse task profile could not be created")?;
            intake::import_briefs(
                &store,
                &a.repo_path,
                a.tasks_dir.as_deref(),
                a.replace.unwrap_or(false),
            )
            .map(|report| json!(report))
            .map_err(workbench_message)
        }
        "gitpulse_list_tasks" => {
            let a: ListTasksArgs = parse(name, arguments)?;
            let Some(store) = open_task_profile(false)? else {
                return Ok(json!({
                    "ok": true, "repository": null, "returned": 0, "total": 0, "has_more": false,
                    "next_cursor": null, "tasks": [],
                    "note": "GitPulse has no task board on this machine yet: nothing has been filed. That is not the same as a board whose tasks are all done.",
                }));
            };
            intake::list_tasks(
                &store,
                &a.repo_path,
                a.status.as_deref(),
                a.archived.unwrap_or(false),
                a.limit.unwrap_or(50),
                a.cursor.as_deref(),
            )
            .map_err(workbench_message)
        }
        "gitpulse_get_task" => {
            let a: GetTaskArgs = parse(name, arguments)?;
            let store = open_task_profile(false)?.ok_or(NO_BOARD)?;
            intake::get_task(&store, &a.repo_path, &a.task_id).map_err(workbench_message)
        }
        "gitpulse_complete_task" => {
            let a: CompleteTaskArgs = parse(name, arguments)?;
            // Reading, not creating: a task to complete has to exist already.
            let store = open_task_profile(false)?.ok_or(NO_BOARD)?;
            intake::complete_task(
                &store,
                &a.repo_path,
                &a.task_id,
                a.status.as_deref().unwrap_or("done"),
                a.expected_revision,
                a.summary.as_deref(),
                a.request_id.as_deref(),
            )
            .map_err(workbench_message)
        }
        "gitpulse_delete_task" => {
            let a: DeleteTaskArgs = parse(name, arguments)?;
            // Reading, not creating: a task to delete has to exist already.
            let store = open_task_profile(false)?.ok_or(NO_BOARD)?;
            intake::delete_task(
                &store,
                &a.repo_path,
                &a.task_id,
                a.expected_revision,
                &a.reason,
                a.even_if_running.unwrap_or(false),
                a.request_id.as_deref(),
            )
            .map_err(workbench_message)
        }
        "gitpulse_merge_tasks" => {
            let a: MergeTasksArgs = parse(name, arguments)?;
            let into = intake::MergeTask {
                task: a.into_task_id,
                expected_revision: a.into_expected_revision,
            };
            let sources: Vec<intake::MergeTask> = a
                .sources
                .into_iter()
                .map(|source| intake::MergeTask {
                    task: source.task_id,
                    expected_revision: source.expected_revision,
                })
                .collect();
            // Reading, not creating: tasks to merge have to exist already.
            let store = open_task_profile(false)?.ok_or(NO_BOARD)?;
            intake::merge_tasks(
                &store,
                &a.repo_path,
                &into,
                &sources,
                &a.reason,
                a.even_if_running.unwrap_or(false),
            )
            .map_err(workbench_message)
        }
        name if name.starts_with("devmap_") => devmap_parity::handle(name, arguments),
        "gitpulse_codeintel_search" => {
            let a: SearchArgs = parse(name, arguments)?;
            Ok(json!(crate::codeintel::search(
                &a.repo_path,
                &a.query,
                a.budget
            )))
        }
        "gitpulse_codeintel_impact" => {
            let a: CodeintelImpactArgs = parse(name, arguments)?;
            Ok(json!(crate::codeintel::impact(
                &a.repo_path,
                &a.target,
                a.budget
            )))
        }
        "gitpulse_codeintel_dependencies" => {
            let a: CodeintelDependenciesArgs = parse(name, arguments)?;
            Ok(json!(crate::codeintel::dependencies(
                &a.repo_path,
                &a.file_path,
                a.budget
            )))
        }
        "gitpulse_codeintel_trace" => {
            let a: CodeintelTraceArgs = parse(name, arguments)?;
            Ok(json!(crate::codeintel::trace_between(
                &a.repo_path,
                &a.from,
                &a.to,
                a.budget
            )))
        }
        "gitpulse_codeintel_dead_symbols" => {
            let a: BudgetArgs = parse(name, arguments)?;
            Ok(json!(crate::codeintel::dead_symbols(
                &a.repo_path,
                a.budget
            )))
        }
        "gitpulse_codeintel_suspects" => {
            let a: SuspectsArgs = parse(name, arguments)?;
            Ok(json!(crate::codeintel::suspects(
                &a.repo_path,
                &a.symptom,
                &a.since,
                a.depth
            )))
        }
        "gitpulse_provenance" => {
            let a: ProvenanceArgs = parse(name, arguments)?;
            Ok(json!(crate::engine::provenance::compute_freshness(
                &a.repo_path,
                &a.commit_sha,
                a.base_branch.as_deref()
            )))
        }
        _ => Err(format!("Unknown tool: {name}")),
    }
}

/// What a read or a write of a task that must already exist says when this
/// machine has no board.
const NO_BOARD: &str = "GitPulse has no task board on this machine yet: nothing has been filed.";

fn ledger_events(a: LedgerEventsArgs) -> Result<Value, String> {
    let address = with_production_retry(|retry| read_address(&a.repo_path, retry))?;
    // `tail` pages *forward* from a cursor — deliberately, so a
    // consumer holding the last id it saw gets the same answer whether
    // it has been listening for an hour or just opened. This tool
    // pinned the cursor at 0 and exposed no way to move it, so it
    // always returned the oldest events with no way to reach recent
    // ones. `total` was the returned count wearing the name of the
    // history size, which reads as a complete answer over a ledger of
    // any size; `returned` says what it is, `truncated` says when more
    // follow, and `next_cursor` is what to pass to get them.
    let descending = a.order.as_deref() == Some("desc");
    // Newest first, the cursor is an upper bound and its absence means "from
    // the newest"; oldest first it is the lower bound it always was.
    let (after, before) = match (descending, a.cursor) {
        (true, cursor) => (0, cursor),
        (false, cursor) => (cursor.unwrap_or(0), None),
    };
    if let Some(since) = &a.since_ts {
        if !crate::ledger::valid_utc_timestamp(since) {
            return Err(format!(
                "since_ts {since:?} is not a ledger timestamp: spell it YYYY-MM-DDTHH:MM:SS.mmmZ (UTC)"
            ));
        }
    }
    resources::ledger_query_page(
        &address.anchor,
        &crate::ledger::EventQuery {
            after,
            before,
            limit: a.limit.unwrap_or(LEDGER_DEFAULT_LIMIT),
            descending,
            since_ts: a.since_ts,
            action: a.action,
            object: a.object,
            worktree_path: a.worktree_path,
            task_id: a.task_id,
            session_id: a.session_id,
            after_ref: a.after_ref,
        },
    )
}

fn task_view(a: TaskViewArgs) -> Result<Value, String> {
    let address = with_production_retry(|retry| read_address(&a.repo_path, retry))?;
    let mut view = crate::tasks::view_filtered(&address.anchor, a.task_id.as_deref());
    // The view is also what the desktop app reads whole, so the cap is here, at
    // the MCP boundary. Each row's stored JSON text is cut the way a ledger
    // page cuts its own; the result budget then cuts the lists themselves and
    // sets `evidence_truncated` / `gaps_truncated`.
    let cap = crate::ledger::ROW_FIELD_CAP_BYTES;
    for evidence in &mut view.evidence {
        evidence.data_json = crate::ledger::bound_evidence_json(&evidence.data_json, cap);
    }
    for gap in &mut view.gaps {
        gap.evidence_json = crate::ledger::bound_evidence_json(&gap.evidence_json, cap);
    }
    Ok(json!(view))
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
                "{} exceeded the {} ms server budget and the client stopped waiting; the work is not cancelled and may still finish, so a write's outcome is unknown. Read the state back before retrying",
                self.method,
                budget.as_millis()
            ),
            Some(json!({
                "method": self.method,
                "timeoutMs": budget.as_millis() as u64,
                "outcomeUnknown": true,
            })),
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

    /// The answer to send when no thread could be started to run this request.
    /// The request never ran, so the client may retry it.
    pub fn unstarted(&self) -> JsonRpcResponse {
        err(
            self.id.clone(),
            -32603,
            format!(
                "{} did not run: the server could not start a thread for it; retry",
                self.method
            ),
            Some(json!({ "method": self.method, "ran": false })),
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
        // An absent version asks for nothing in particular, so it gets the same
        // answer as an unknown one rather than being assumed to mean the oldest.
        let requested = req.params["protocolVersion"].as_str().unwrap_or("");
        let protocol_version = if LEGACY_VERSIONS.contains(&requested) {
            requested
        } else if requested == PROTOCOL_VERSION {
            // A modern client that still sends initialize: answer the handshake
            // they asked for so they are not stuck, and name the modern version.
            PROTOCOL_VERSION
        } else {
            LEGACY_VERSIONS[0]
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
            Some(target) if target.len() > MAX_URI_BYTES => invalid_params(
                id,
                format!(
                    "Invalid params: uri is {} bytes; the limit is {MAX_URI_BYTES}",
                    target.len()
                ),
            ),
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
                Ok(payload) => {
                    let room = budget::tool_budget(&id);
                    ok(id, complete_tool_result(modern, payload, room))
                }
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
    use crate::procguard::LockedSpawn;

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

    /// Regression (oldest-version fallback): a client asking for a revision this
    /// binary does not list was answered 2024-11-05, the *oldest* it speaks,
    /// under which `outputSchema` and `structuredContent` (the `ok` and
    /// `unscanned_worktrees` facets) may be ignored. The spec says the latest.
    #[test]
    fn an_unlisted_requested_version_is_answered_with_the_newest_supported() {
        assert!(
            LEGACY_VERSIONS.windows(2).all(|pair| pair[0] > pair[1]),
            "LEGACY_VERSIONS must be newest first: {LEGACY_VERSIONS:?}"
        );
        for requested in [
            json!("2025-06-18"),
            json!("2025-03-26"),
            json!("1999-01-01"),
            json!(null),
        ] {
            let mut era = Era::Unknown;
            let resp = process_request(
                request(
                    "initialize",
                    Some(json!(1)),
                    json!({ "protocolVersion": requested, "capabilities": {} }),
                ),
                &mut era,
            )
            .expect("answered");
            assert_eq!(
                resp.result.unwrap()["protocolVersion"],
                LEGACY_VERSIONS[0],
                "requested {requested}"
            );
        }
        // A listed version is still answered with itself.
        for version in LEGACY_VERSIONS {
            let mut era = Era::Unknown;
            let resp = process_request(
                request(
                    "initialize",
                    Some(json!(1)),
                    json!({ "protocolVersion": version }),
                ),
                &mut era,
            )
            .expect("answered");
            assert_eq!(resp.result.unwrap()["protocolVersion"], *version);
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
        // Named as the absence it is, not as a malformed checkout.
        assert!(error.contains("missing_worktree"), "{error}");
        assert!(
            !std::path::Path::new("/no/such/repo").exists(),
            "refusing the read must not have created the repository"
        );
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
                            | "gitpulse_merge_tasks"
                    ),
                    "unexpected mutating tool {name}"
                );
                // Deleting a card, alone or by merging it into another, is
                // the destructive write, and it must say so: clients gate
                // their approval prompt on this hint.
                assert_eq!(
                    tool["annotations"]["destructiveHint"],
                    matches!(name, "gitpulse_delete_task" | "gitpulse_merge_tasks"),
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
            .output_locked()
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
    fn ledger_events_says_where_event_time_lives() {
        // Event time rides in `detail_json`: the event contract is closed to a
        // new top-level field, so the tool is where a reader learns of it.
        let tool = tools()
            .into_iter()
            .find(|t| t["name"] == "gitpulse_ledger_events")
            .expect("tool");
        let description = tool["description"].as_str().expect("a description");
        assert!(
            description.contains("detail_json.occurred_at"),
            "{description}"
        );
        assert!(description.contains("task_basis"), "{description}");
        assert!(description.contains("`ts_utc` is when the row was appended"));
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

    fn ledger_ids(result: &Value) -> Vec<i64> {
        result["events"]
            .as_array()
            .expect("events")
            .iter()
            .map(|event| event["id"].as_i64().expect("id"))
            .collect()
    }

    #[test]
    fn ledger_events_newest_first_returns_the_newest_and_continues_older() {
        let (_dir, repo) = repository_with_ledger_rows(12, 8);
        let newest = handle_tool_call(
            "gitpulse_ledger_events",
            &json!({ "repo_path": repo, "order": "desc", "limit": 5 }),
        )
        .expect("newest");
        assert_eq!(ledger_ids(&newest), vec![12, 11, 10, 9, 8]);
        assert_eq!(newest["truncated"], true);
        assert_eq!(newest["next_cursor"], 8);
        // `cursor` is an upper bound newest first: the next page is older, with
        // nothing repeated and nothing skipped.
        let older = handle_tool_call(
            "gitpulse_ledger_events",
            &json!({ "repo_path": repo, "order": "desc", "limit": 5, "cursor": 8 }),
        )
        .expect("older");
        assert_eq!(ledger_ids(&older), vec![7, 6, 5, 4, 3]);
        // Oldest first stays the default, unchanged.
        let oldest = handle_tool_call(
            "gitpulse_ledger_events",
            &json!({ "repo_path": repo, "limit": 3 }),
        )
        .expect("oldest");
        assert_eq!(ledger_ids(&oldest), vec![1, 2, 3]);
    }

    #[test]
    fn ledger_events_filters_combine_and_a_bad_since_ts_is_refused() {
        let (_dir, repo) = repository_with_ledger_rows(12, 8);
        let one = handle_tool_call(
            "gitpulse_ledger_events",
            &json!({ "repo_path": repo, "object": "echo 3", "action": "session.command" }),
        )
        .expect("filtered");
        assert_eq!(one["returned"], 1);
        assert_eq!(one["events"][0]["object"], "echo 3");
        let none = handle_tool_call(
            "gitpulse_ledger_events",
            &json!({ "repo_path": repo, "action": "git.push" }),
        )
        .expect("filtered");
        assert_eq!(none["returned"], 0);
        assert_eq!(
            none["ok"], true,
            "no match is an answer, not a missing ledger"
        );
        let refused = handle_tool_call(
            "gitpulse_ledger_events",
            &json!({ "repo_path": repo, "since_ts": "2026-01-01" }),
        );
        assert!(refused.is_err(), "{refused:?}");
        let refused = validate_arguments(
            "gitpulse_ledger_events",
            &json!({ "repo_path": repo, "order": "sideways" }),
        );
        assert!(refused.is_err(), "{refused:?}");
    }

    /// A trusted repository whose ledger holds `rows` events carrying `detail`
    /// bytes each, and the anchor to read it back by.
    fn repository_with_ledger_rows(rows: usize, detail: usize) -> (tempfile::TempDir, String) {
        let dir = tempfile::tempdir().expect("tempdir");
        let repo = dir.path().to_string_lossy().into_owned();
        let init = std::process::Command::new("git")
            .args(["init", "-q", &repo])
            .output_locked()
            .expect("git init");
        assert!(init.status.success(), "git init failed");
        crate::test_support::trust_repo(dir.path());
        let address = crate::ledger::bindings::repository_address(&repo).expect("address");
        for n in 0..rows {
            let id = crate::ledger::record(crate::ledger::Draft {
                repo_path: address.anchor.clone(),
                action: "session.command".into(),
                object: Some(format!("echo {n}")),
                actor_kind: Some(crate::ledger::ActorKind::Agent),
                outcome: Some(crate::ledger::Outcome::Ok),
                detail_json: Some(json!({ "note": "d".repeat(detail) }).to_string()),
                ..Default::default()
            });
            assert!(id.is_some(), "row {n} was not recorded");
        }
        (dir, repo)
    }

    /// A resource `uri` is echoed in the document and in every error about it,
    /// so one the size of the frame could never be answered. It is refused at
    /// the boundary, by size, without echoing it.
    #[test]
    fn an_over_long_resource_uri_is_refused_at_the_request_boundary() {
        let long = format!("gitpulse://insights/{}", "a".repeat(MAX_URI_BYTES));
        let response = call(
            "resources/read",
            json!({ "uri": long, "_meta": modern_meta() }),
        );
        let size = wire_len(&response);
        let error = response.error.expect("refused");
        assert_eq!(error["code"], -32602);
        let message = error["message"].as_str().unwrap();
        assert!(message.contains("limit"), "{message}");
        assert!(message.len() < 200, "the uri was echoed: {}", message.len());
        assert!(size < 1024);

        // A uri at the limit still reaches the reader, which answers about it.
        let at_limit = format!(
            "gitpulse://insights/{}",
            "a".repeat(MAX_URI_BYTES - "gitpulse://insights/".len())
        );
        assert_eq!(at_limit.len(), MAX_URI_BYTES);
        let response = call(
            "resources/read",
            json!({ "uri": at_limit, "_meta": modern_meta() }),
        );
        let refused_by_size = response
            .error
            .as_ref()
            .and_then(|error| error["message"].as_str())
            .is_some_and(|message| message.contains("the limit is"));
        assert!(!refused_by_size, "a uri at the limit is not over it");
    }

    fn wire_len(response: &JsonRpcResponse) -> usize {
        serde_json::to_string(response).expect("serializes").len() + 1
    }

    /// 1000 rows of about 6.5 KiB is 6.5 MB of ledger, past the 4 MiB frame the
    /// transport can carry. A page must come back bounded, say it is partial,
    /// and hand back the cursor that continues exactly where it stopped.
    #[test]
    fn a_6_mib_ledger_is_paged_within_the_frame_without_skipping_or_repeating() {
        let (_dir, repo) = repository_with_ledger_rows(1000, 6_500);
        let schema = tools()
            .into_iter()
            .find(|t| t["name"] == "gitpulse_ledger_events")
            .expect("tool")["outputSchema"]
            .clone();
        let mut cursor = 0_i64;
        let mut seen = Vec::new();
        let mut pages = 0;
        loop {
            let response = call(
                "tools/call",
                json!({
                    "name": "gitpulse_ledger_events",
                    "arguments": { "repo_path": repo, "limit": 1000, "cursor": cursor },
                    "_meta": modern_meta(),
                }),
            );
            assert!(
                wire_len(&response) <= 4 * 1024 * 1024,
                "page {pages} is {} bytes on the wire",
                wire_len(&response)
            );
            let result = response.result.expect("a result");
            assert_eq!(result["isError"], false, "{result:.300}");
            let page = result["structuredContent"].clone();
            let violations = validate::validate(&page, &schema);
            assert!(
                violations.is_empty(),
                "page violates its schema: {violations:?}"
            );
            let events = page["events"].as_array().expect("events");
            assert_eq!(page["returned"], events.len());
            seen.extend(events.iter().map(|e| e["id"].as_i64().expect("id")));
            let last = events.last().expect("a page always makes progress")["id"]
                .as_i64()
                .unwrap();
            assert_eq!(
                page["next_cursor"], last,
                "next_cursor is the last returned id"
            );
            pages += 1;
            if pages == 1 {
                assert_eq!(
                    page["truncated"], true,
                    "the first page cannot be the whole ledger"
                );
            }
            if page["truncated"] != true {
                break;
            }
            assert!(pages < 40, "paging does not terminate");
            cursor = last;
        }
        assert!(pages > 1, "6.5 MB of rows came back as one page");
        assert_eq!(seen.len(), 1000, "paging skipped or repeated a row");
        assert!(
            seen.windows(2).all(|pair| pair[1] == pair[0] + 1),
            "paging skipped or repeated a row: {seen:?}"
        );
    }

    /// What the result budget adds to a call, alone: the budget step on a real
    /// insights payload, min of 500, with no git underneath. Timing only, so it
    /// is ignored; run it in release with
    /// `cargo test --release --lib -- --ignored --nocapture the_budget_step`.
    #[test]
    #[ignore = "timing only, no assertion on speed"]
    fn the_budget_step_alone_on_a_real_insights_payload() {
        let (dir, repo) = repository_with_ledger_rows(1, 10);
        let _keep = dir;
        let payload = handle_tool_call("gitpulse_insights", &json!({ "repo_path": repo }))
            .expect("insights answers");
        let bytes = serde_json::to_string(&payload).unwrap().len();
        let room = budget::tool_budget(&json!(1));
        let mut least = std::time::Duration::MAX;
        for _ in 0..500 {
            let copy = payload.clone();
            let started = std::time::Instant::now();
            let fitted = std::hint::black_box(budget::fit(true, copy, room));
            least = least.min(started.elapsed());
            assert!(fitted.is_ok());
        }
        let mut before = std::time::Duration::MAX;
        for _ in 0..500 {
            let started = std::time::Instant::now();
            let text = std::hint::black_box(serde_json::to_string_pretty(&payload));
            before = before.min(started.elapsed());
            assert!(text.is_ok());
        }
        println!(
            "budget::fit on a {bytes} byte insights payload: min {least:?} of 500 \
             (the pretty-printed text it replaced: min {before:?})"
        );
    }

    /// A modern client reads `structuredContent`; the text block exists for
    /// clients that read nothing else. Both full-size doubled every large
    /// result on the wire. Modern gets one full copy and a bounded mirror, and a
    /// legacy client (which has only the text) gets the whole budget in it.
    #[test]
    fn a_large_payload_has_one_full_copy_and_a_bounded_text_mirror() {
        let (_dir, repo) = repository_with_ledger_rows(120, 6_500);
        let arguments = json!({ "repo_path": repo, "limit": 1000 });

        let modern = modern_call(
            "tools/call",
            json!({ "name": "gitpulse_ledger_events", "arguments": arguments }),
        );
        let text = modern["content"][0]["text"].as_str().expect("text");
        assert!(
            text.len() <= 256 * 1024,
            "the modern text mirror is {} bytes",
            text.len()
        );
        let mirror: Value = serde_json::from_str(text).expect("the mirror is still JSON");
        assert!(
            mirror["events"].is_array(),
            "the mirror keeps the payload's shape"
        );
        let structured = &modern["structuredContent"];
        assert_eq!(structured["events"].as_array().expect("events").len(), 120);
        assert!(
            structured["events"].to_string().len() > 256 * 1024,
            "the fixture is not large enough to exercise the mirror"
        );
        assert!(
            !text.contains("\n  "),
            "the text is compact, not pretty-printed"
        );

        let mut era = Era::Unknown;
        process_request(request("initialize", Some(json!(0)), json!({})), &mut era);
        let legacy = process_request(
            request(
                "tools/call",
                Some(json!(1)),
                json!({ "name": "gitpulse_ledger_events", "arguments": arguments }),
            ),
            &mut era,
        )
        .expect("answered")
        .result
        .expect("result");
        assert!(legacy.get("structuredContent").is_none());
        let body: Value =
            serde_json::from_str(legacy["content"][0]["text"].as_str().expect("text"))
                .expect("legacy text is JSON");
        assert_eq!(
            body["events"].as_array().expect("events").len(),
            120,
            "a legacy client has only the text; it must not be shortened"
        );
    }

    /// Every declared property the payload carries as `null` must declare
    /// `null` among its types. `validate` reads an optional `null` as an absent
    /// argument, which is right for input; a client checking `structuredContent`
    /// type-checks every present property, so for output it is a violation.
    fn nulls_the_schema_does_not_allow(value: &Value, schema: &Value, path: &str) -> Vec<String> {
        let mut found = Vec::new();
        let (Some(fields), Some(properties)) = (
            value.as_object(),
            schema.get("properties").and_then(Value::as_object),
        ) else {
            return found;
        };
        for (name, sub) in properties {
            let here = format!("{path}/{name}");
            match fields.get(name) {
                Some(Value::Null) if !validate_allows_null(sub) => found.push(here),
                Some(present) => found.extend(nulls_the_schema_does_not_allow(present, sub, &here)),
                None => {}
            }
        }
        found
    }

    /// Pointer to the non-empty list with the most serialized bytes, at any depth.
    fn largest_nonempty_list(
        value: &Value,
        pointer: &mut String,
        best: &mut Option<(usize, String)>,
    ) {
        match value {
            Value::Array(items) => {
                if !items.is_empty() {
                    let size = value.to_string().len();
                    if best.as_ref().is_none_or(|(most, _)| size > *most) {
                        *best = Some((size, pointer.clone()));
                    }
                }
                for (index, item) in items.iter().enumerate() {
                    let mark = pointer.len();
                    pointer.push_str(&format!("/{index}"));
                    largest_nonempty_list(item, pointer, best);
                    pointer.truncate(mark);
                }
            }
            Value::Object(members) => {
                for (key, item) in members {
                    let mark = pointer.len();
                    pointer.push_str(&format!("/{}", key.replace('~', "~0").replace('/', "~1")));
                    largest_nonempty_list(item, pointer, best);
                    pointer.truncate(mark);
                }
            }
            _ => {}
        }
    }

    /// Pointer to the longest string in `value`, at any depth.
    fn longest_string(value: &Value, pointer: &mut String, best: &mut Option<(usize, String)>) {
        match value {
            Value::String(text) => {
                if best.as_ref().is_none_or(|(most, _)| text.len() > *most) {
                    *best = Some((text.len(), pointer.clone()));
                }
            }
            Value::Array(items) => {
                for (index, item) in items.iter().enumerate() {
                    let mark = pointer.len();
                    pointer.push_str(&format!("/{index}"));
                    longest_string(item, pointer, best);
                    pointer.truncate(mark);
                }
            }
            Value::Object(members) => {
                for (key, item) in members {
                    let mark = pointer.len();
                    pointer.push_str(&format!("/{}", key.replace('~', "~0").replace('/', "~1")));
                    longest_string(item, pointer, best);
                    pointer.truncate(mark);
                }
            }
            _ => {}
        }
    }

    /// Inflate the longest string of a real tool payload to 300 KB of lines (a
    /// log field: every newline costs six bytes once it is escaped into the text
    /// block and again as a string), past the text mirror's half-budget but far
    /// inside the budget, in the modern era. The call must succeed with the
    /// payload whole in `structuredContent` and meet its `outputSchema`; an
    /// error here is a tool that fails on a large field the budget could carry.
    /// False when the payload holds no string.
    fn probe_a_long_string(name: &str, label: &str, schema: &Value, payload: &Value) -> bool {
        let mut best = None;
        longest_string(payload, &mut String::new(), &mut best);
        let Some((_, pointer)) = best else {
            return false;
        };
        let mut inflated = payload.clone();
        if let Some(slot) = inflated.pointer_mut(&pointer) {
            *slot = Value::String("x\n".repeat(150_000));
        }
        let fitted = budget::fit(true, inflated.clone(), budget::tool_budget(&json!(1)))
            .unwrap_or_else(|error| {
                panic!("{name} on {label}: a long string failed: {error:.200}")
            });
        assert_eq!(
            fitted.structured.as_ref(),
            Some(&inflated),
            "{name} on {label}: structuredContent was cut for a string"
        );
        let violations: Vec<String> = validate::validate(&inflated, schema)
            .iter()
            .map(validate::Violation::render)
            .collect();
        assert!(
            violations.is_empty(),
            "{name} on {label}: a long string violates its outputSchema: {violations:?}"
        );
        true
    }

    /// Inflate the largest list of a real tool payload past 200 KiB and fit it
    /// to 64 KiB: the result must still meet the tool's `outputSchema`, say it
    /// was cut, stay inside the budget and keep every top-level key (all seven
    /// facets of an insights snapshot survive). False when the payload holds no
    /// list to inflate.
    fn probe_under_a_small_budget(
        name: &str,
        label: &str,
        schema: &Value,
        payload: &Value,
    ) -> bool {
        const BUDGET: usize = 64 * 1024;
        let mut inflated = payload.clone();
        let mut best = None;
        largest_nonempty_list(&inflated, &mut String::new(), &mut best);
        let Some((_, pointer)) = best else {
            return false;
        };
        let Some(Value::Array(items)) = inflated.pointer_mut(&pointer) else {
            return false;
        };
        for _ in 0..40 {
            if serde_json::to_string(&items).map_or(0, |text| text.len()) > 200_000 {
                break;
            }
            let more = items.clone();
            items.extend(more);
        }
        let fitted = budget::fit(true, inflated.clone(), BUDGET)
            .unwrap_or_else(|error| panic!("{name} on {label}: {error}"));
        let structured = fitted.structured.expect("modern structured content");
        let wire = budget::escaped_len(&fitted.text)
            + serde_json::to_string(&structured)
                .expect("serializes")
                .len();
        assert!(
            wire <= BUDGET,
            "{name} on {label}: {wire} bytes for a {BUDGET} byte budget"
        );
        assert!(
            structured[budget::TRUNCATION_KEY].is_object(),
            "{name} on {label} was cut without saying so"
        );
        let violations: Vec<String> = validate::validate(&structured, schema)
            .iter()
            .map(validate::Violation::render)
            .collect();
        assert!(
            violations.is_empty(),
            "{name} on {label}: the cut payload violates its outputSchema: {violations:?}"
        );
        let nulls = nulls_the_schema_does_not_allow(&structured, schema, "");
        assert!(nulls.is_empty(), "{name} on {label}: {nulls:?}");
        for key in payload.as_object().expect("an object payload").keys() {
            assert!(
                structured.get(key).is_some(),
                "{name} on {label}: the cut dropped {key}"
            );
        }
        true
    }

    fn validate_allows_null(schema: &Value) -> bool {
        match schema.get("type") {
            Some(Value::String(name)) => name == "null",
            Some(Value::Array(names)) => names.iter().any(|n| n == "null"),
            // No `type` constrains nothing.
            None => true,
            _ => false,
        }
    }

    /// Tools whose `outputSchema` is only `{"type":"object"}`, each with why.
    ///
    /// An object-only schema accepts any object, so validating against it
    /// proves nothing; leaving that silent is how "every tool validated" came
    /// to mean "five of them". Each is named here instead, and
    /// `every_advertised_tool_result_matches_its_declared_output_schema` fails
    /// both for a tool missing from this list and for one that no longer
    /// belongs on it.
    const OBJECT_ONLY_OUTPUT_SCHEMAS: &[(&str, &str)] = &[(
        "gitpulse_task_view",
        "relays Manvi's task view; the shape is Manvi's contract",
    )];

    /// Tools whose payload holds no list the budget probe can inflate, each
    /// with why. The probe cannot cut what is not a list, so these are the ones
    /// the 64 KiB budget is not exercised against.
    const NO_LIST_TO_INFLATE: &[(&str, &str)] = &[];

    fn is_object_only_exempt(name: &str) -> bool {
        OBJECT_ONLY_OUTPUT_SCHEMAS
            .iter()
            .any(|(exempt, _)| *exempt == name)
    }

    /// A tool's result is checked by the client against its `outputSchema`,
    /// and a mismatch fails the whole call. `gitpulse_change_context` declared
    /// `collisions` an array while it has always returned the collision facet
    /// object, so every call was refused as "data/collisions must be array".
    /// Checked on a healthy repository, on one whose registered worktree has
    /// been deleted (the collision scan fails), and on a path that is not a
    /// repository at all, because the failure paths are where a payload
    /// drifts from its happy-path shape.
    ///
    /// Every advertised tool is held to it, the task-board writers included
    /// (they run as one lifecycle on the healthy repository), and a tool whose
    /// every call errored is a failure, not a skip: this used to `continue`
    /// past an error and require five of the 27 tools, so the other 22 could
    /// drift without a test noticing. The only tools exempt are those named in
    /// [`OBJECT_ONLY_OUTPUT_SCHEMAS`], each with the reason its schema is not
    /// typed here.
    #[test]
    fn every_advertised_tool_result_matches_its_declared_output_schema() {
        let git = |args: &[&str]| {
            let out = std::process::Command::new("git")
                .args(args)
                .output_locked()
                .expect("git");
            assert!(
                out.status.success(),
                "git {args:?}: {}",
                String::from_utf8_lossy(&out.stderr)
            );
        };
        // `task_fixture` leaves this one untrusted: the trust refusal is a
        // failure path of its own.
        let (_untrusted_dir, _profile, untrusted) = task_fixture();

        let healthy_dir = tempfile::tempdir().expect("tempdir");
        let healthy = healthy_dir.path().to_string_lossy().into_owned();
        git(&["init", "-q", &healthy]);
        crate::test_support::trust_repo(healthy_dir.path());

        let broken_dir = tempfile::tempdir().expect("tempdir");
        let broken = broken_dir.path().to_string_lossy().into_owned();
        git(&["init", "-q", &broken]);
        git(&[
            "-C",
            &broken,
            "-c",
            "user.name=t",
            "-c",
            "user.email=t@t",
            "commit",
            "-q",
            "--allow-empty",
            "-m",
            "init",
        ]);
        crate::test_support::trust_repo(broken_dir.path());
        let doomed_parent = tempfile::tempdir().expect("tempdir");
        let doomed = doomed_parent.path().join("doomed");
        git(&[
            "-C",
            &broken,
            "worktree",
            "add",
            "-q",
            doomed.to_str().unwrap(),
        ]);
        crate::test_support::trust_repo(&doomed);
        std::fs::remove_dir_all(&doomed).expect("remove worktree");

        // A ledger that exists and holds no events: `gitpulse_ledger_events`
        // answers an empty page, whose `next_cursor` has nothing to point at.
        let ledgered_dir = tempfile::tempdir().expect("tempdir");
        let ledgered = ledgered_dir.path().to_string_lossy().into_owned();
        git(&["init", "-q", &ledgered]);
        crate::test_support::trust_repo(ledgered_dir.path());
        let address = crate::ledger::bindings::repository_address(&ledgered).expect("address");
        let _ = crate::ledger::status(&address.anchor);
        assert!(
            crate::ledger::is_initialised(&address.anchor),
            "fixture ledger"
        );

        let missing = "/no/such/gitpulse-output-schema-repo".to_string();

        // One repository with an indexed generation, so the DevMap tools answer
        // with data rather than only their unavailable envelopes.
        let indexed_dir = crate::codeintel::tests::repo_with_one_generation();
        let indexed = indexed_dir.path().to_string_lossy().into_owned();
        crate::test_support::trust_repo(indexed_dir.path());

        let mut validated = std::collections::BTreeSet::new();
        let mut probed = std::collections::BTreeSet::<String>::new();
        let mut strings_probed = std::collections::BTreeSet::<String>::new();
        let mut errors: std::collections::BTreeMap<String, Vec<String>> = Default::default();
        let mut check =
            |name: &str,
             label: &str,
             schema: &Value,
             outcome: Result<Value, String>,
             validated: &mut std::collections::BTreeSet<String>| {
                // An execution error is reported as `isError` text and carries no
                // structured content, so there is no schema to meet. It is recorded
                // rather than skipped: a tool with no successful call is reported
                // below.
                let payload = match outcome {
                    Ok(payload) => payload,
                    Err(error) => {
                        errors
                            .entry(name.to_string())
                            .or_default()
                            .push(format!("{label}: {error}"));
                        return;
                    }
                };
                let violations: Vec<String> = validate::validate(&payload, schema)
                    .iter()
                    .map(validate::Violation::render)
                    .collect();
                assert!(
                violations.is_empty(),
                "{name} on {label} violates its outputSchema: {violations:?}\npayload: {payload}"
            );
                let nulls = nulls_the_schema_does_not_allow(&payload, schema, "");
                assert!(
                    nulls.is_empty(),
                    "{name} on {label} returns null where its outputSchema forbids it: {nulls:?}"
                );
                validated.insert(name.to_string());
                if probe_under_a_small_budget(name, label, schema, &payload) {
                    probed.insert(name.to_string());
                }
                if probe_a_long_string(name, label, schema, &payload) {
                    strings_probed.insert(name.to_string());
                }
            };
        for repo in [&healthy, &broken, &untrusted, &ledgered, &missing, &indexed] {
            for tool in tools() {
                let name = tool["name"].as_str().expect("name");
                // Exempt tools are still run and held to the object schema they
                // declare; only a tool that never succeeds escapes (below).
                if tool["annotations"]["readOnlyHint"] != true {
                    continue;
                }
                let mut arguments = json!({ "repo_path": repo });
                if tool["inputSchema"]["properties"]["targets"].is_object() {
                    arguments["targets"] = json!(["probe_caller"]);
                }
                for extra in [
                    "query",
                    "target",
                    "file_path",
                    "from",
                    "to",
                    "commit_sha",
                    "task_id",
                    "symptom",
                    "since",
                    "file",
                    "content",
                ] {
                    if tool["inputSchema"]["properties"][extra].is_object() {
                        arguments[extra] = json!("probe");
                    }
                }
                check(
                    name,
                    repo,
                    &tool["outputSchema"],
                    handle_tool_call(name, &arguments),
                    &mut validated,
                );
            }
        }

        // The writers, as the lifecycle an agent runs: file two tasks, import
        // briefs, finish one, merge the other into it, delete a third. Each
        // reply is held to the schema the tool advertises. The task profile is
        // the fixture's own (`PROFILE_OVERRIDE`), so nothing here touches a
        // real board.
        let schema_of = |name: &str| -> Value {
            tools()
                .into_iter()
                .find(|tool| tool["name"] == name)
                .map(|tool| tool["outputSchema"].clone())
                .unwrap_or_else(|| panic!("{name} is not advertised"))
        };
        let mut call_writer =
            |name: &str, arguments: Value, validated: &mut std::collections::BTreeSet<String>| {
                let outcome = handle_tool_call(name, &arguments);
                check(
                    name,
                    "the task-board lifecycle",
                    &schema_of(name),
                    outcome,
                    validated,
                );
            };
        for (task, title) in [
            ("gp-schema-first", "Rotate the signing keys"),
            ("gp-schema-second", "Paginate the activity feed"),
            ("gp-schema-third", "Compress the ledger archives"),
        ] {
            call_writer(
                "gitpulse_add_task",
                json!({ "repo_path": healthy, "task_id": task, "title": title, "status": "ready", "priority": 2 }),
                &mut validated,
            );
        }
        call_writer(
            "gitpulse_import_tasks",
            json!({ "repo_path": healthy }),
            &mut validated,
        );
        call_writer(
            "gitpulse_complete_task",
            json!({ "repo_path": healthy, "task_id": "gp-schema-first", "status": "review", "summary": "schema probe" }),
            &mut validated,
        );
        call_writer(
            "gitpulse_merge_tasks",
            json!({
                "repo_path": healthy,
                "into_task_id": "gp-schema-first",
                "sources": [{ "task_id": "gp-schema-second" }],
                "reason": "schema probe: duplicate of the first"
            }),
            &mut validated,
        );
        call_writer(
            "gitpulse_delete_task",
            json!({ "repo_path": healthy, "task_id": "gp-schema-third", "reason": "schema probe cleanup" }),
            &mut validated,
        );
        // And the readers that depend on those tasks existing.
        for name in ["gitpulse_list_tasks", "gitpulse_get_task"] {
            let tool = tools()
                .into_iter()
                .find(|tool| tool["name"] == name)
                .unwrap();
            let arguments = json!({ "repo_path": healthy, "task_id": "gp-schema-first" });
            check(
                name,
                "the task-board lifecycle",
                &tool["outputSchema"],
                handle_tool_call(name, &arguments),
                &mut validated,
            );
        }

        // The budget probe ran on real payloads, not only on the shapes this
        // test invented: a tool with no list in its payload cannot be probed,
        // so the ones that must be are named.
        for name in [
            "gitpulse_insights",
            "gitpulse_change_context",
            "gitpulse_get_task",
        ] {
            assert!(
                probed.contains(name),
                "{name} was never held to the result budget; probed: {probed:?}"
            );
        }
        // Every other advertised tool is probed too, unless it is listed in
        // `NO_LIST_TO_INFLATE` with why: the roster comes from the registry,
        // so a tool added later and left unprobed fails here by name.
        let misfiled: Vec<String> = tools()
            .iter()
            .filter_map(|tool| {
                let name = tool["name"].as_str().expect("name");
                let listed = NO_LIST_TO_INFLATE.iter().any(|(exempt, _)| *exempt == name);
                (probed.contains(name) == listed)
                    .then(|| format!("{name} (probed={}, listed={listed})", probed.contains(name)))
            })
            .collect();
        assert!(
            misfiled.is_empty(),
            "each advertised tool must be budget-probed or listed in NO_LIST_TO_INFLATE with a \
             reason, and not both: {misfiled:?}"
        );
        // Every advertised tool whose result holds a string was run with a
        // long one at the mirror threshold.
        for tool in tools() {
            let name = tool["name"].as_str().expect("name");
            if is_object_only_exempt(name) {
                continue;
            }
            assert!(
                strings_probed.contains(name) || !validated.contains(name),
                "{name} has no string to inflate in any payload this test reached: \
                 strings probed: {strings_probed:?}"
            );
        }

        // Every advertised tool is validated or explicitly exempt. A tool whose
        // every call errored was never checked, and says why.
        for tool in tools() {
            let name = tool["name"].as_str().expect("name");
            if is_object_only_exempt(name) {
                continue;
            }
            assert!(
                validated.contains(name),
                "{name} was never validated against its outputSchema (every call errored): {:?}",
                errors.get(name)
            );
        }
        // The DevMap tools are exempt from a typed schema, not from a payload:
        // each must have produced a checked object on at least one repository.
        for name in devmap_parity::TOOL_NAMES {
            assert!(
                validated.contains(*name),
                "{name} was never validated against its declared outputSchema: {:?}",
                errors.get(*name)
            );
        }
        // The allow-list cannot go stale: an exempt tool must still declare
        // only an object, and a tool that gained a real schema must leave it.
        for (name, reason) in OBJECT_ONLY_OUTPUT_SCHEMAS {
            let tool = tools()
                .into_iter()
                .find(|tool| tool["name"] == *name)
                .unwrap_or_else(|| panic!("{name} is exempt but no longer advertised"));
            assert_eq!(
                tool["outputSchema"],
                json!({ "type": "object" }),
                "{name} now declares a real outputSchema; drop it from OBJECT_ONLY_OUTPUT_SCHEMAS ({reason})"
            );
        }
        assert!(
            tools().iter().all(|tool| {
                let name = tool["name"].as_str().unwrap();
                tool["outputSchema"] != json!({ "type": "object" }) || is_object_only_exempt(name)
            }),
            "a tool declares only {{\"type\":\"object\"}} without being listed, with a reason, in OBJECT_ONLY_OUTPUT_SCHEMAS"
        );
        // A tool that only ever errored here was never checked; these are the
        // ones whose declared shapes this test was written against.
        for name in [
            "gitpulse_insights",
            "gitpulse_change_context",
            "gitpulse_collision_risk",
            "gitpulse_ledger_events",
            "gitpulse_codeintel_suspects",
        ] {
            assert!(
                validated.contains(name),
                "{name} was never validated: {validated:?}"
            );
        }

        // Type-correct is not enough: the scan that could not read the deleted
        // worktree must say so, not render as a clean empty list.
        for name in ["gitpulse_change_context", "gitpulse_insights"] {
            let payload = handle_tool_call(name, &json!({ "repo_path": broken })).expect(name);
            assert_eq!(payload["collisions"]["ok"], false, "{name}: {payload}");
            // The scan ran and failed on the deleted worktree, rather than
            // being refused before it started.
            assert_eq!(
                payload["collisions"]["failed_worktrees"], 1,
                "{name}: {payload}"
            );
            assert!(
                !payload["collisions"]["error"]
                    .as_str()
                    .unwrap_or_default()
                    .is_empty(),
                "{name}: {payload}"
            );
        }
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
            .output_locked()
            .expect("git init");
        assert!(init.status.success(), "git init failed");
        let profile = tempfile::tempdir().expect("profile dir");
        let path = profile.path().join("workbench.sqlite");
        PROFILE_OVERRIDE.with(|cell| *cell.borrow_mut() = Some(path));
        (dir, profile, repo)
    }

    /// The task profile belongs to the desktop app. An MCP server that shares
    /// it may be older or newer, so it never moves the schema: a request that
    /// needs the upgrade is refused with a typed error and leaves the profile,
    /// and every other host's view of it, untouched.
    #[test]
    fn the_mcp_server_never_upgrades_a_shared_task_profile() {
        let (dir, profile, repo) = task_fixture();
        crate::test_support::trust_repo(dir.path());
        let database = profile.path().join("workbench.sqlite");
        let (error, added) = tool_json(
            "gitpulse_add_task",
            json!({"repo_path": repo, "title": "Filed before the profile aged"}),
        );
        assert!(!error, "{added}");

        // The profile as an older desktop app left it.
        let held_back = rusqlite::Connection::open(&database).unwrap();
        held_back
            .execute("UPDATE work_meta SET version = 11 WHERE id = 1", [])
            .unwrap();
        drop(held_back);

        for (tool, arguments) in [
            (
                "gitpulse_add_task",
                json!({"repo_path": repo, "title": "Needs the upgrade"}),
            ),
            ("gitpulse_list_tasks", json!({"repo_path": repo})),
        ] {
            let (error, refused) = tool_json(tool, arguments);
            assert!(error, "{tool}: {refused}");
            assert!(
                refused
                    .as_str()
                    .unwrap_or_default()
                    .contains("schema_upgrade_required"),
                "{tool}: {refused}"
            );
        }
        // A write that fails for another reason still must not upgrade first:
        // the schema step runs before the request is read.
        let store = open_task_profile(true).unwrap().unwrap();
        let error = store
            .workbench_request("items.put", "{}")
            .expect_err("refused before the request is looked at");
        assert_eq!(error.code, "schema_upgrade_required", "{}", error.message);
        drop(store);
        let after = rusqlite::Connection::open(&database).unwrap();
        let version: i64 = after
            .query_row("SELECT version FROM work_meta WHERE id = 1", [], |r| {
                r.get(0)
            })
            .unwrap();
        assert_eq!(version, 11, "the profile was left at the schema it had");
        assert!(
            !profile
                .path()
                .join("workbench.sqlite.pre-schema-11.bak")
                .exists(),
            "an upgrade that did not run took no backup"
        );
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
                crate::workbench::intake::MAX_REVIEWED_RELATED
            ),
            format!(
                "`reason` is required (at most {} characters)",
                grouped(crate::workbench::intake::MAX_REASON_CHARS)
            ),
            format!(
                "At most {} sources per call",
                crate::workbench::intake::MAX_MERGE_SOURCES
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
        assert_eq!(task_tools.len(), 7, "{task_tools:?}");
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

    /// The board an agent lists is the board the person sees: a task archived
    /// on it is not listed as live work unless the archive is asked for.
    #[test]
    fn list_tasks_reads_the_board_or_the_archive_never_both() {
        let (dir, profile, repo) = task_fixture();
        crate::test_support::trust_repo(dir.path());
        let (board, repository_id) = board_task(profile.path(), &repo, "on-the-board");
        board
            .board_request(
                "items.put",
                &json!({
                    "id": "in-the-archive", "request_id": "put-in-the-archive",
                    "expected_revision": 0, "title": "Shelved idea", "status": "backlog",
                    "archived": true,
                    "repository_ids": [repository_id], "primary_repository_id": repository_id,
                })
                .to_string(),
            )
            .unwrap();

        let (error, listed) = tool_json("gitpulse_list_tasks", json!({ "repo_path": repo }));
        assert!(!error, "{listed}");
        assert_eq!(listed["total"], 1, "{listed}");
        assert_eq!(listed["tasks"][0]["item_id"], "on-the-board");
        assert_eq!(listed["tasks"][0]["archived"], false);
        assert_eq!(listed["tasks"][0]["completed_at"], Value::Null);

        let (error, archive) = tool_json(
            "gitpulse_list_tasks",
            json!({ "repo_path": repo, "archived": true }),
        );
        assert!(!error, "{archive}");
        assert_eq!(archive["total"], 1, "{archive}");
        assert_eq!(archive["tasks"][0]["item_id"], "in-the-archive");
        assert_eq!(archive["tasks"][0]["archived"], true);

        // Not a boolean: refused by the schema, never read as "the board".
        let (error, refused) = tool_json(
            "gitpulse_list_tasks",
            json!({ "repo_path": repo, "archived": "yes" }),
        );
        assert!(error, "{refused}");
    }

    #[test]
    fn a_finished_task_reaches_done_on_the_board_with_only_its_status_changed() {
        let (dir, profile, repo) = task_fixture();
        crate::test_support::trust_repo(dir.path());
        let id = "6f1c2a7e-0d4b-4c1e-9a55-3b0e8f2d9c11";
        let (board, _) = board_task(profile.path(), &repo, id);
        // An agent's `done` needs Manvi's passed verdict for the current head.
        crate::test_support::git_in(dir.path(), &["commit", "-q", "--allow-empty", "-m", "base"]);
        crate::workbench::intake::gate_tests::verified(
            &dc_store::Store::open(profile.path().join("workbench.sqlite")).unwrap(),
            &dir,
            id,
        );
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
            logs.starts_with("first trace\n\n--- Verification passed ("),
            "the verdict is attached to the history: {logs}"
        );
        assert!(
            logs.contains("\n\n--- Agent moved this task to done ("),
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
        crate::workbench::intake::gate_tests::verified(
            &dc_store::Store::open(profile.path().join("workbench.sqlite")).unwrap(),
            &dir,
            id,
        );
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
        for key in [
            "revision",
            "updated_at",
            "created_at",
            "completed_at",
            "locked_fields",
        ] {
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
            .status_locked()
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

    /// Through the real dispatch, on the profile the board reads: a filed
    /// task folded into a card the person made, the argument schema enforced
    /// before anything runs, and the same call again writing nothing.
    #[test]
    fn merging_over_mcp_folds_a_filed_task_into_the_persons_card() {
        let (dir, profile, repo) = task_fixture();
        crate::test_support::trust_repo(dir.path());
        let (board, _) = board_task(profile.path(), &repo, "board-keep");
        let (error, added) = tool_json(
            "gitpulse_add_task",
            json!({ "repo_path": repo, "task_id": "gp-dup-import", "title": "Importer survives a crash",
                    "description": "token-dup", "acceptance_criteria": ["Keeps its cursor"],
                    "reviewed_related": ["board-keep"] }),
        );
        assert!(!error, "{added}");

        for (arguments, why) in [
            (
                json!({"repo_path": repo, "into_task_id": "board-keep", "sources": [], "reason": "r"}),
                "no sources",
            ),
            (
                json!({"repo_path": repo, "into_task_id": "board-keep", "sources": [{"task_id": "gp-dup-import", "force": true}], "reason": "r"}),
                "unknown source field",
            ),
            (
                json!({"repo_path": repo, "into_task_id": "board-keep", "sources": (0..=crate::workbench::intake::MAX_MERGE_SOURCES).map(|i| json!({"task_id": format!("t{i}")})).collect::<Vec<_>>(), "reason": "r"}),
                "too many sources",
            ),
            (
                json!({"repo_path": repo, "into_task_id": "board-keep", "sources": [{"task_id": "gp-dup-import"}]}),
                "no reason",
            ),
            (
                json!({"repo_path": repo, "into_task_id": "board-keep", "sources": [{"task_id": "gp-dup-import"}], "reason": "r", "even_if_running": "yes"}),
                "string flag",
            ),
            (
                json!({"repo_path": repo, "into_task_id": "board-keep", "sources": [{"task_id": "gp-dup-import", "expected_revision": "1"}], "reason": "r"}),
                "string revision",
            ),
        ] {
            let (error, message) = tool_json("gitpulse_merge_tasks", arguments);
            assert!(
                error && message.as_str().unwrap().starts_with("invalid arguments"),
                "{why}: {message}"
            );
        }
        assert!(
            item_history(&board, added["item_id"].as_str().unwrap()).len() == 1,
            "a refused call wrote nothing"
        );

        let call = json!({ "repo_path": repo, "into_task_id": "board-keep",
                           "sources": [{"task_id": "gp-dup-import", "expected_revision": 1}],
                           "reason": "Same importer work as the person's card." });
        let (error, merged) = tool_json("gitpulse_merge_tasks", call.clone());
        assert!(!error, "{merged}");
        assert_eq!(
            (merged["ok"].clone(), merged["outcome"].clone()),
            (json!(true), json!("merged")),
            "{merged:#}"
        );
        let kept = board
            .board_request("items.get", r#"{"id":"board-keep"}"#)
            .unwrap()["item"]
            .clone();
        assert!(
            kept["description"].as_str().unwrap().contains("token-dup"),
            "{kept}"
        );
        assert_eq!(
            kept["acceptance_criteria"],
            json!(["Resumes after a crash", "Keeps its cursor"])
        );
        assert_eq!(
            (kept["status"].clone(), kept["owner"].clone()),
            (json!("in_progress"), json!("@sam"))
        );

        let (error, gone) = tool_json(
            "gitpulse_get_task",
            json!({ "repo_path": repo, "task_id": "gp-dup-import" }),
        );
        let gone = gone.as_str().unwrap();
        assert!(
            error && gone.starts_with("task_merged:") && gone.contains("board-keep"),
            "{gone}"
        );

        let (error, again) = tool_json("gitpulse_merge_tasks", call);
        assert!(!error, "{again}");
        assert_eq!(again["outcome"], "unchanged", "{again:#}");
        assert_eq!(again["sources"][0]["outcome"], "already_merged");
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
        // Not "no such task": an agent asking after it is told it was
        // deleted, and why.
        let gone = gone.as_str().unwrap();
        assert!(
            error
                && gone.starts_with("task_deleted:")
                && gone.contains("folded into gp-login-overhaul"),
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
        for key in [
            "revision",
            "updated_at",
            "created_at",
            "completed_at",
            "locked_fields",
        ] {
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
        for key in [
            "revision",
            "updated_at",
            "created_at",
            "completed_at",
            "locked_fields",
        ] {
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
            .status_locked()
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
    fn a_timeout_says_the_work_may_still_finish_and_the_outcome_is_unknown() {
        let ready = Ready {
            id: json!(7),
            modern: true,
            method: "tools/call".into(),
            params: json!({}),
        };
        let response = ready.timed_out(std::time::Duration::from_millis(1500));
        let error = response.error.expect("an error");
        let message = error["message"].as_str().unwrap();
        assert!(message.contains("not cancelled"), "{message}");
        assert!(message.contains("outcome is unknown"), "{message}");
        assert!(!message.contains("was abandoned"), "{message}");
        assert_eq!(error["data"]["outcomeUnknown"], true);
    }

    /// With 26 or more related open tasks, `add_task` used to be impossible:
    /// it required every related id in `reviewed_related`, whose schema capped
    /// the array at the 25 a refusal names.
    #[test]
    fn a_task_can_be_filed_after_reviewing_more_than_twenty_five_related_ones() {
        let (dir, _profile, repo) = task_fixture();
        crate::test_support::trust_repo(dir.path());
        let mut ids: Vec<String> = Vec::new();
        for n in 0..30 {
            let (error, filed) = tool_json(
                "gitpulse_add_task",
                json!({
                    "repo_path": repo, "task_id": format!("imp-{n}"),
                    "title": format!("Importer crash handling case{n}x"), "labels": ["import"],
                    "reviewed_related": ids,
                }),
            );
            assert!(
                !error,
                "filing task {n} with {} reviewed: {filed}",
                ids.len()
            );
            ids.push(filed["item_id"].as_str().unwrap().to_owned());
        }
        // A refusal names one page and says the rest follow.
        let (error, refused) = tool_json(
            "gitpulse_add_task",
            json!({ "repo_path": repo, "task_id": "imp-last", "title": "Importer crash handling again", "labels": ["import"] }),
        );
        assert!(error, "{refused}");
        let message = refused.as_str().unwrap();
        assert!(message.contains("30 open tasks"), "{message}");
        assert!(message.contains("and 5 more"), "{message}");
        assert!(
            message.contains("the next refusal names the following ones"),
            "{message}"
        );
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

    // ---- typed arguments: the integer policy, the advertised bounds, the schema contract ----

    /// The person edits a card after an agent read it at revision 1.
    fn person_edits(board: &crate::workbench::WorkbenchState, id: &str) {
        let mut edit = board
            .board_request("items.get", &json!({ "id": id }).to_string())
            .unwrap()["item"]
            .clone();
        for key in [
            "revision",
            "updated_at",
            "created_at",
            "completed_at",
            "locked_fields",
        ] {
            edit.as_object_mut().unwrap().remove(key);
        }
        edit["request_id"] = json!(format!("person-edit-{id}"));
        edit["expected_revision"] = json!(1);
        edit["title"] = json!("Edited by the person");
        board.board_request("items.put", &edit.to_string()).unwrap();
    }

    fn board_item(board: &crate::workbench::WorkbenchState, id: &str) -> Value {
        board
            .board_request("items.get", &json!({ "id": id }).to_string())
            .unwrap()["item"]
            .clone()
    }

    #[test]
    fn complete_task_with_a_whole_float_revision_still_conflicts() {
        let (dir, profile, repo) = task_fixture();
        crate::test_support::trust_repo(dir.path());
        let id = "board-task-1";
        let (board, _) = board_task(profile.path(), &repo, id);
        person_edits(&board, id);

        let (error, stale) = tool_json(
            "gitpulse_complete_task",
            json!({ "repo_path": repo, "task_id": id, "expected_revision": 1.0 }),
        );
        assert!(
            error && stale.as_str().unwrap().contains("revision_conflict"),
            "{stale}"
        );
        assert_eq!(board_item(&board, id)["status"], "in_progress");
    }

    #[test]
    fn delete_task_with_a_whole_float_revision_still_conflicts() {
        let (dir, profile, repo) = task_fixture();
        crate::test_support::trust_repo(dir.path());
        let id = "board-task-1";
        let (board, _) = board_task(profile.path(), &repo, id);
        person_edits(&board, id);

        let (error, stale) = tool_json(
            "gitpulse_delete_task",
            json!({ "repo_path": repo, "task_id": id, "reason": "Duplicate.", "expected_revision": 1.0 }),
        );
        assert!(
            error && stale.as_str().unwrap().contains("revision_conflict"),
            "{stale}"
        );
        assert_eq!(
            board_item(&board, id)["title"],
            "Edited by the person",
            "the task is still on the board"
        );
    }

    #[test]
    fn merge_tasks_with_a_whole_float_into_revision_still_conflicts() {
        let (dir, profile, repo) = task_fixture();
        crate::test_support::trust_repo(dir.path());
        let (board, _) = board_task(profile.path(), &repo, "board-keep");
        let (error, added) = tool_json(
            "gitpulse_add_task",
            json!({ "repo_path": repo, "task_id": "gp-dup-import", "title": "Importer survives a crash",
                    "reviewed_related": ["board-keep"] }),
        );
        assert!(!error, "{added}");
        person_edits(&board, "board-keep");

        let (error, stale) = tool_json(
            "gitpulse_merge_tasks",
            json!({ "repo_path": repo, "into_task_id": "board-keep", "into_expected_revision": 1.0,
                    "sources": [{ "task_id": "gp-dup-import" }], "reason": "Same work." }),
        );
        assert!(
            error && stale.as_str().unwrap().contains("revision_conflict"),
            "{stale}"
        );
        assert_eq!(
            board_item(&board, "board-keep")["description"],
            "Background",
            "nothing was folded in"
        );
    }

    #[test]
    fn merge_tasks_with_a_whole_float_source_revision_still_conflicts() {
        let (dir, profile, repo) = task_fixture();
        crate::test_support::trust_repo(dir.path());
        let (_board, _) = board_task(profile.path(), &repo, "board-keep");
        let (error, added) = tool_json(
            "gitpulse_add_task",
            json!({ "repo_path": repo, "task_id": "gp-dup-import", "title": "Importer survives a crash",
                    "reviewed_related": ["board-keep"] }),
        );
        assert!(!error, "{added}");
        let board =
            crate::workbench::WorkbenchState::for_profile(&profile.path().join("workbench.sqlite"));
        person_edits(&board, added["item_id"].as_str().unwrap());

        let (error, stale) = tool_json(
            "gitpulse_merge_tasks",
            json!({ "repo_path": repo, "into_task_id": "board-keep",
                    "sources": [{ "task_id": "gp-dup-import", "expected_revision": 1.0 }],
                    "reason": "Same work." }),
        );
        assert!(
            error && stale.as_str().unwrap().contains("revision_conflict"),
            "{stale}"
        );
    }

    fn ledger_repo_with_events(count: usize) -> (tempfile::TempDir, String) {
        let dir = tempfile::tempdir().expect("tempdir");
        crate::test_support::git_in(dir.path(), &["init", "-q"]);
        crate::test_support::trust_repo(dir.path());
        let repo = dir
            .path()
            .canonicalize()
            .expect("canonical")
            .to_string_lossy()
            .into_owned();
        for _ in 0..count {
            crate::ledger::append(crate::ledger::Draft {
                repo_path: repo.clone(),
                action: "git.commit".to_string(),
                actor_kind: Some(crate::ledger::ActorKind::Human),
                outcome: Some(crate::ledger::Outcome::Ok),
                ..Default::default()
            })
            .expect("append");
        }
        (dir, repo)
    }

    #[test]
    fn ledger_events_cursor_3_0_continues_after_event_3() {
        let (_dir, repo) = ledger_repo_with_events(6);
        let (error, page) = tool_json(
            "gitpulse_ledger_events",
            json!({ "repo_path": repo, "cursor": 3.0 }),
        );
        assert!(!error, "{page}");
        assert_eq!(page["returned"], 3, "{page}");
        assert_eq!(page["events"][0]["id"], 4, "never restarts at 0: {page}");
    }

    #[test]
    fn ledger_events_cursor_above_i64_max_is_invalid_params() {
        let (_dir, repo) = ledger_repo_with_events(2);
        for cursor in [json!(9_223_372_036_854_775_808_u64), json!(u64::MAX)] {
            let (error, refused) = tool_json(
                "gitpulse_ledger_events",
                json!({ "repo_path": repo, "cursor": cursor }),
            );
            assert!(error, "{cursor}: {refused}");
            assert!(refused.as_str().unwrap().contains("cursor"), "{refused}");
        }
    }

    #[test]
    fn list_tasks_limit_10_0_returns_ten_not_fifty() {
        let (dir, _profile, repo) = task_fixture();
        crate::test_support::trust_repo(dir.path());
        let words = [
            "alpha", "bravo", "charlie", "delta", "echo", "foxtrot", "golf", "hotel", "india",
            "juliet", "kilo", "lima",
        ];
        let mut filed: Vec<String> = Vec::new();
        for (index, word) in words.iter().enumerate() {
            let (error, added) = tool_json(
                "gitpulse_add_task",
                json!({ "repo_path": repo, "task_id": format!("gp-limit-{index}"),
                        "title": format!("{word} {}", word.len() * 7919),
                        "reviewed_related": filed }),
            );
            assert!(!error, "{added}");
            filed.push(added["item_id"].as_str().unwrap().to_string());
        }
        let (error, ten) = tool_json(
            "gitpulse_list_tasks",
            json!({ "repo_path": repo, "limit": 10 }),
        );
        assert!(!error, "{ten}");
        assert_eq!(ten["returned"], 10, "{ten}");
        let (error, float_ten) = tool_json(
            "gitpulse_list_tasks",
            json!({ "repo_path": repo, "limit": 10.0 }),
        );
        assert!(!error, "{float_ten}");
        assert_eq!(float_ten["returned"], 10, "{float_ten}");
    }

    #[test]
    fn add_task_priority_1_0_is_high() {
        let (dir, _profile, repo) = task_fixture();
        crate::test_support::trust_repo(dir.path());
        let (error, added) = tool_json(
            "gitpulse_add_task",
            json!({ "repo_path": repo, "title": "Prioritised work", "priority": 1.0 }),
        );
        assert!(!error, "{added}");
        assert_eq!(added["priority"], 1, "{added}");
    }

    #[test]
    fn a_u32_argument_of_4294967296_is_refused() {
        for tool in tools() {
            let name = tool["name"].as_str().unwrap();
            for (argument, schema) in tool["inputSchema"]["properties"].as_object().unwrap() {
                // The arguments that reach a `u32` (or narrower) parameter.
                let reaches_u32 = schema["maximum"]
                    .as_u64()
                    .is_some_and(|max| max <= u64::from(u32::MAX));
                if schema["type"] != "integer" || !reaches_u32 {
                    continue;
                }
                let mut arguments = sample(&tool["inputSchema"]);
                arguments["repo_path"] = json!("/no/such/repo");
                arguments[argument] = json!(4_294_967_296_u64);
                let error = handle_tool_call(name, &arguments)
                    .expect_err(&format!("{name}.{argument} accepted 2^32"));
                assert!(
                    error.starts_with("invalid arguments for") && error.contains(argument.as_str()),
                    "{name}.{argument}: {error}"
                );
            }
        }
    }

    /// The slice of this file's production code that holds `start` up to `end`.
    fn between<'a>(source: &'a str, start: &str, end: &str) -> &'a str {
        let from = source.find(start).unwrap_or_else(|| panic!("no {start}"));
        let rest = &source[from..];
        &rest[..rest.find(end).unwrap_or_else(|| panic!("no {end}"))]
    }

    #[test]
    fn no_handler_reads_raw_arguments() {
        let mod_rs = include_str!("mod.rs");
        let parity = include_str!("devmap_parity.rs");
        for (what, body) in [
            (
                "handle_tool_call",
                between(mod_rs, "fn handle_tool_call(", "\nfn meta_object("),
            ),
            (
                "devmap_parity::handle",
                between(parity, "pub fn handle(", "#[cfg(test)]"),
            ),
        ] {
            for needle in ["arguments[", ".as_i64()", ".as_u64()", " as u32"] {
                assert_eq!(
                    body.matches(needle).count(),
                    0,
                    "{what} still reads the raw arguments with `{needle}`"
                );
            }
        }
    }

    // -- bounded strings --

    #[derive(Clone)]
    enum Seg {
        Key(String),
        First,
    }

    struct Bounded {
        tool: String,
        path: Vec<Seg>,
        name: String,
        max: u64,
        description: String,
        in_array: bool,
    }

    fn bounded_strings() -> Vec<Bounded> {
        fn walk(tool: &str, schema: &Value, path: &mut Vec<Seg>, out: &mut Vec<Bounded>) {
            for (name, prop) in schema["properties"].as_object().unwrap() {
                path.push(Seg::Key(name.clone()));
                match prop["type"].as_str() {
                    Some("string") if prop["enum"].is_null() => out.push(Bounded {
                        tool: tool.to_string(),
                        path: path.clone(),
                        name: name.clone(),
                        max: prop["maxLength"].as_u64().expect("maxLength"),
                        description: prop["description"].as_str().unwrap_or_default().into(),
                        in_array: false,
                    }),
                    Some("array") => {
                        path.push(Seg::First);
                        match prop["items"]["type"].as_str() {
                            Some("string") => out.push(Bounded {
                                tool: tool.to_string(),
                                path: path.clone(),
                                name: name.clone(),
                                max: prop["items"]["maxLength"].as_u64().expect("maxLength"),
                                description: prop["description"]
                                    .as_str()
                                    .unwrap_or_default()
                                    .into(),
                                in_array: true,
                            }),
                            Some("object") => walk(tool, &prop["items"], path, out),
                            _ => {}
                        }
                        path.pop();
                    }
                    _ => {}
                }
                path.pop();
            }
        }
        let mut out = Vec::new();
        for tool in tools() {
            walk(
                tool["name"].as_str().unwrap(),
                &tool["inputSchema"],
                &mut Vec::new(),
                &mut out,
            );
        }
        out
    }

    /// The smallest arguments the schema accepts.
    fn sample(schema: &Value) -> Value {
        match schema["type"].as_str() {
            Some("string") => match schema["enum"].as_array().and_then(|e| e.first()) {
                Some(first) => first.clone(),
                None => json!("x"),
            },
            Some("integer" | "number") => schema["minimum"].clone(),
            Some("boolean") => json!(false),
            Some("array") => Value::Array(
                (0..schema["minItems"].as_u64().unwrap_or(0))
                    .map(|_| sample(&schema["items"]))
                    .collect(),
            ),
            Some("object") => {
                let mut object = serde_json::Map::new();
                for name in schema["required"].as_array().into_iter().flatten() {
                    let name = name.as_str().unwrap();
                    object.insert(name.to_string(), sample(&schema["properties"][name]));
                }
                Value::Object(object)
            }
            other => panic!("no sample for {other:?}"),
        }
    }

    fn set_at(root: &mut Value, path: &[Seg], value: Value) {
        let mut at = root;
        for seg in path {
            at = match seg {
                Seg::Key(key) => &mut at[key.as_str()],
                Seg::First => {
                    if at.is_null() {
                        *at = json!([]);
                    }
                    let items = at.as_array_mut().expect("an array");
                    if items.is_empty() {
                        items.push(Value::Null);
                    }
                    &mut items[0]
                }
            };
        }
        *at = value;
    }

    fn declared_unit(field: &Bounded) -> Option<&'static str> {
        let each = if field.in_array { "each item " } else { "" };
        ["characters", "bytes"].into_iter().find(|unit| {
            field
                .description
                .contains(&format!("{each}at most {} {unit}", field.max))
        })
    }

    #[test]
    fn every_bounded_string_description_names_its_unit() {
        let fields = bounded_strings();
        assert!(fields.len() >= 40, "only {} bounded strings", fields.len());
        for field in &fields {
            assert!(
                declared_unit(field).is_some(),
                "{}.{} (max {}) does not say whether its bound is characters or bytes: {:?}",
                field.tool,
                field.name,
                field.max,
                field.description
            );
        }
    }

    #[test]
    fn every_bounded_string_accepts_its_limit_and_refuses_one_more() {
        let (_dir, _profile, _repo) = task_fixture();
        let invalid = |result: &Result<Value, String>| matches!(result, Err(error) if error.starts_with("invalid arguments for"));
        for field in bounded_strings() {
            let unit = declared_unit(&field)
                .unwrap_or_else(|| panic!("{}.{} names no unit", field.tool, field.name));
            let schema = tools()
                .into_iter()
                .find(|t| t["name"] == field.tool.as_str())
                .unwrap()["inputSchema"]
                .clone();
            let attempt = |text: String| {
                let mut arguments = sample(&schema);
                arguments["repo_path"] = json!("/no/such/repo");
                set_at(&mut arguments, &field.path, json!(text));
                handle_tool_call(&field.tool, &arguments)
            };
            let max = field.max as usize;
            let (euro_fit, euro_over) = if unit == "bytes" {
                (max / 3, max / 3 + 1)
            } else {
                (max, max + 1)
            };
            for (what, text) in [
                ("ASCII at the limit", "a".repeat(max)),
                (
                    "3-byte characters at the limit",
                    "\u{20ac}".repeat(euro_fit),
                ),
            ] {
                let result = attempt(text);
                assert!(
                    !invalid(&result),
                    "{}.{} {what} was refused: {result:?}",
                    field.tool,
                    field.name
                );
            }
            for (what, text) in [
                ("ASCII over the limit", "a".repeat(max + 1)),
                (
                    "3-byte characters over the limit",
                    "\u{20ac}".repeat(euro_over),
                ),
            ] {
                let result = attempt(text);
                assert!(
                    invalid(&result),
                    "{}.{} {what} was accepted: {result:?}",
                    field.tool,
                    field.name
                );
            }
        }
    }

    #[test]
    fn add_task_title_bound_is_max_task_title_everywhere() {
        use crate::tasks::file_tasks as ft;
        let tool = tools()
            .into_iter()
            .find(|t| t["name"] == "gitpulse_add_task")
            .unwrap();
        let title = &tool["inputSchema"]["properties"]["title"];
        assert_eq!(title["maxLength"], ft::MAX_TASK_TITLE);
        assert!(
            title["description"]
                .as_str()
                .unwrap()
                .contains(&format!("at most {} characters", ft::MAX_TASK_TITLE)),
            "{title}"
        );
        let call = |text: String| {
            validate_arguments(
                "gitpulse_add_task",
                &json!({ "repo_path": "/r", "title": text }),
            )
        };
        assert!(call("\u{20ac}".repeat(ft::MAX_TASK_TITLE)).is_ok());
        assert!(call("\u{20ac}".repeat(ft::MAX_TASK_TITLE + 1)).is_err());
        assert!(ft::validate_title(&"\u{20ac}".repeat(ft::MAX_TASK_TITLE)).is_ok());
        assert!(ft::validate_title(&"\u{20ac}".repeat(ft::MAX_TASK_TITLE + 1)).is_err());
    }

    // -- the published contract --

    /// A schema without its prose: what a client's code can depend on.
    fn without_descriptions(schema: &Value) -> Value {
        match schema {
            Value::Object(map) => Value::Object(
                map.iter()
                    .filter(|(key, value)| !(key.as_str() == "description" && value.is_string()))
                    .map(|(key, value)| (key.clone(), without_descriptions(value)))
                    .collect(),
            ),
            Value::Array(items) => Value::Array(items.iter().map(without_descriptions).collect()),
            other => other.clone(),
        }
    }

    /// The only bounds that changed on purpose: the schema now says what the
    /// handler enforces: a title was advertised at 1200 characters and refused
    /// past 300, a summary at 16000 and refused past 4000, a reason at 4000 and
    /// refused past 1000. `(tool, JSON pointer into its inputSchema, new maximum)`.
    fn deliberate_bound_changes() -> [(&'static str, &'static str, usize); 4] {
        [
            (
                "gitpulse_add_task",
                "/properties/title/maxLength",
                ft::MAX_TASK_TITLE,
            ),
            (
                "gitpulse_complete_task",
                "/properties/summary/maxLength",
                intake::MAX_SUMMARY_CHARS,
            ),
            (
                "gitpulse_delete_task",
                "/properties/reason/maxLength",
                intake::MAX_REASON_CHARS,
            ),
            (
                "gitpulse_merge_tasks",
                "/properties/reason/maxLength",
                intake::MAX_REASON_CHARS,
            ),
        ]
    }

    #[test]
    fn input_schemas_are_backward_compatible() {
        let snapshot: Value =
            serde_json::from_str(include_str!("tool_input_schemas.snapshot.json")).unwrap();
        let mut snapshot = snapshot.as_object().unwrap().clone();
        for (tool, pointer, value) in deliberate_bound_changes() {
            assert_ne!(
                snapshot[tool].pointer(pointer),
                Some(&json!(value)),
                "{tool} {pointer} was already {value}: drop it from the allowlist"
            );
            *snapshot[tool]
                .pointer_mut(pointer)
                .unwrap_or_else(|| panic!("{tool} has no {pointer}")) = json!(value);
        }
        let current = tools();
        let names: Vec<&str> = current
            .iter()
            .map(|t| t["name"].as_str().unwrap())
            .collect();
        assert_eq!(
            snapshot
                .keys()
                .map(String::as_str)
                .collect::<std::collections::BTreeSet<_>>(),
            names
                .iter()
                .copied()
                .collect::<std::collections::BTreeSet<_>>(),
            "the set of tools changed"
        );
        for tool in &current {
            let name = tool["name"].as_str().unwrap();
            assert_eq!(
                without_descriptions(&tool["inputSchema"]),
                without_descriptions(&snapshot[name]),
                "{name}: the published inputSchema changed beyond the deliberate bounds"
            );
        }
    }

    // -- the typed arguments: one struct per tool --

    /// A value with its nulls removed, so an echoed optional field and an
    /// absent one compare equal.
    fn without_nulls(value: &Value) -> Value {
        match value {
            Value::Object(map) => Value::Object(
                map.iter()
                    .filter(|(_, v)| !v.is_null())
                    .map(|(k, v)| (k.clone(), without_nulls(v)))
                    .collect(),
            ),
            Value::Array(items) => Value::Array(items.iter().map(without_nulls).collect()),
            other => other.clone(),
        }
    }

    /// What extraction must hand back for an argument the validator accepted.
    fn canonical(prop: &Value, probe: &Value) -> Value {
        match (prop["type"].as_str(), probe) {
            (Some("integer"), Value::Number(number)) => {
                let exact = validate::exact_int(number).expect("an accepted integer is exact");
                match u64::try_from(exact) {
                    Ok(unsigned) => json!(unsigned),
                    Err(_) => json!(i64::try_from(exact).expect("fits i64")),
                }
            }
            (Some("number"), Value::Number(number)) => {
                json!(number.as_f64().expect("a float") as f32 as f64)
            }
            _ => probe.clone(),
        }
    }

    /// For every argument of one tool and every awkward value: either the
    /// validator refuses the call, or extraction reads exactly what was sent.
    /// Never a call that passes validation and then reaches the handler as a
    /// default, a clamp, or a refusal the schema did not announce.
    fn property_table<T: ToolArgs>(name: &str) {
        let tool = tools()
            .into_iter()
            .find(|t| t["name"] == name)
            .unwrap_or_else(|| panic!("{name} is not advertised"));
        let schema = tool["inputSchema"].clone();
        let properties = schema["properties"].as_object().unwrap();
        assert_eq!(
            T::FIELDS
                .iter()
                .copied()
                .collect::<std::collections::BTreeSet<_>>(),
            properties
                .keys()
                .map(String::as_str)
                .collect::<std::collections::BTreeSet<_>>(),
            "{name}: the struct and the schema name different arguments"
        );
        let base = sample(&schema);
        assert!(validate::validate(&base, &schema).is_empty(), "{name}");
        assert!(T::from_args(&base).is_ok(), "{name}");

        let mut extra = base.clone();
        extra["zz_not_an_argument"] = json!(1);
        assert!(!validate::validate(&extra, &schema).is_empty(), "{name}");
        assert!(T::from_args(&extra).is_err(), "{name}");

        for (field, prop) in properties {
            let mut probes: Vec<Option<Value>> = vec![None];
            probes.extend(
                [
                    json!(null),
                    json!(true),
                    json!(0),
                    json!(-1),
                    json!(1),
                    json!(5.0),
                    json!(5.5),
                    json!(1e300),
                    json!(9_007_199_254_740_993.0_f64),
                    json!(9_223_372_036_854_775_808_u64),
                    json!(u64::MAX),
                    json!(4_294_967_296_u64),
                    json!(""),
                    json!("5"),
                    json!([]),
                    json!([1]),
                    json!({}),
                    json!(["x"]),
                    json!([{}]),
                    json!([{ "task_id": "x" }]),
                ]
                .map(Some),
            );
            let limit = |schema: &Value| schema["maxLength"].as_u64().unwrap_or(0) as usize;
            if prop["type"] == "string" {
                probes.push(Some(json!("a".repeat(limit(prop)))));
                probes.push(Some(json!("a".repeat(limit(prop) + 1))));
            }
            if prop["items"]["type"] == "string" {
                probes.push(Some(json!(["a".repeat(limit(&prop["items"]))])));
                probes.push(Some(json!(["a".repeat(limit(&prop["items"]) + 1)])));
            }
            for probe in probes {
                let mut args = base.clone();
                match &probe {
                    None => {
                        args.as_object_mut().unwrap().remove(field);
                    }
                    Some(value) => args[field.as_str()] = value.clone(),
                }
                let violations = validate::validate(&args, &schema);
                let extracted = T::from_args(&args);
                let shown = probe.as_ref().map_or("<absent>".to_string(), |v| {
                    v.to_string().chars().take(40).collect()
                });
                if violations.is_empty() {
                    assert!(
                        extracted.is_ok(),
                        "{name}.{field} = {shown}: the validator passed what extraction refuses: {:?}",
                        extracted.err()
                    );
                }
                if let Ok(parsed) = &extracted {
                    let echoed = parsed.to_value();
                    match &probe {
                        Some(value) if !value.is_null() => assert_eq!(
                            without_nulls(&echoed[field.as_str()]),
                            without_nulls(&canonical(prop, value)),
                            "{name}.{field} = {shown}"
                        ),
                        _ => assert!(
                            echoed[field.as_str()].is_null(),
                            "{name}.{field} = {shown} read as {}",
                            echoed[field.as_str()]
                        ),
                    }
                }
            }
        }
    }

    /// A tool's name, the `inputSchema` its args struct prints, and the
    /// property check for that struct.
    type ArgsRow = (&'static str, fn() -> Value, fn(&str));

    /// One row per advertised tool.
    fn args_registry() -> Vec<ArgsRow> {
        macro_rules! rows {
            ($(($name:literal, $ty:ty)),+ $(,)?) => {
                vec![$((
                    $name,
                    <$ty as ToolArgs>::object_schema as fn() -> Value,
                    property_table::<$ty> as fn(&str),
                )),+]
            };
        }
        use devmap_parity as dm;
        rows![
            ("gitpulse_insights", RepoArgs),
            ("gitpulse_status", RepoArgs),
            ("gitpulse_active_changes", ActiveChangesArgs),
            ("gitpulse_collision_risk", RepoArgs),
            ("gitpulse_change_context", ChangeContextArgs),
            ("gitpulse_ledger_events", LedgerEventsArgs),
            ("gitpulse_task_view", TaskViewArgs),
            ("gitpulse_add_task", AddTaskArgs),
            ("gitpulse_import_tasks", ImportTasksArgs),
            ("gitpulse_list_tasks", ListTasksArgs),
            ("gitpulse_get_task", GetTaskArgs),
            ("gitpulse_complete_task", CompleteTaskArgs),
            ("gitpulse_delete_task", DeleteTaskArgs),
            ("gitpulse_merge_tasks", MergeTasksArgs),
            ("gitpulse_codeintel_search", SearchArgs),
            ("gitpulse_codeintel_impact", CodeintelImpactArgs),
            ("gitpulse_codeintel_dependencies", CodeintelDependenciesArgs),
            ("gitpulse_codeintel_trace", CodeintelTraceArgs),
            ("gitpulse_codeintel_dead_symbols", BudgetArgs),
            ("gitpulse_codeintel_suspects", SuspectsArgs),
            ("gitpulse_provenance", ProvenanceArgs),
            ("devmap_status", RepoArgs),
            ("devmap_search", SearchArgs),
            ("devmap_dependencies", dm::DependenciesArgs),
            ("devmap_impact", dm::ImpactArgs),
            ("devmap_trace", dm::TraceArgs),
            ("devmap_neighbors", dm::NeighborsArgs),
            ("devmap_dead_symbols", BudgetArgs),
            ("devmap_clones", BudgetArgs),
            ("devmap_preview", dm::PreviewArgs),
            ("devmap_explore", dm::ExploreArgs),
            ("devmap_affected_tests", dm::AffectedTestsArgs),
        ]
    }

    /// The advertised `inputSchema` is printed by the struct the handler reads,
    /// so a handler cannot read an argument the schema does not declare, and the
    /// schema cannot advertise one the handler never reads (a declared field
    /// nothing reads is dead code, which `clippy -D warnings` refuses). This
    /// replaces the test that scanned the handler's source text for the same
    /// thing.
    #[test]
    fn every_tool_input_schema_is_its_args_struct() {
        let advertised = tools();
        let registry = args_registry();
        let listed: Vec<&str> = advertised
            .iter()
            .map(|t| t["name"].as_str().unwrap())
            .collect();
        let registered: Vec<&str> = registry.iter().map(|(name, ..)| *name).collect();
        assert_eq!(registered, listed, "one row per advertised tool, in order");
        for (name, schema, _) in registry {
            let tool = advertised.iter().find(|t| t["name"] == name).unwrap();
            assert_eq!(tool["inputSchema"], schema(), "{name}");
        }
    }

    #[test]
    fn every_tool_argument_fails_validation_or_extracts_exactly() {
        for (name, _, check) in args_registry() {
            check(name);
        }
    }

    #[test]
    fn suspects_depth_5_0_is_honoured() {
        let call = |depth: Value| {
            SuspectsArgs::from_args(&json!({
                "repo_path": "/r", "symptom": "s", "since": "HEAD~1", "depth": depth
            }))
            .expect("extracts")
            .depth
        };
        assert_eq!(call(json!(5.0)), Some(5));
        assert_eq!(call(json!(5)), Some(5));
        assert_eq!(call(Value::Null), None);
    }

    /// Hostile-input attacks on the typed-argument layer. Everything here runs
    /// the validator and the extractor over what the advertised schemas say,
    /// never over values a person picked to fit the implementation.
    mod adversarial {
        use super::*;
        use validate::Violation;

        type Extract = fn(&Value) -> Result<Value, Vec<Violation>>;

        fn extract<T: ToolArgs>(arguments: &Value) -> Result<Value, Vec<Violation>> {
            T::from_args(arguments).map(|parsed| parsed.to_value())
        }

        /// One extractor per advertised tool, independent of `args_registry`.
        fn extractors() -> Vec<(&'static str, Extract)> {
            macro_rules! rows {
                ($(($name:literal, $ty:ty)),+ $(,)?) => {
                    vec![$(($name, extract::<$ty> as Extract)),+]
                };
            }
            use devmap_parity as dm;
            rows![
                ("gitpulse_insights", RepoArgs),
                ("gitpulse_status", RepoArgs),
                ("gitpulse_active_changes", ActiveChangesArgs),
                ("gitpulse_collision_risk", RepoArgs),
                ("gitpulse_change_context", ChangeContextArgs),
                ("gitpulse_ledger_events", LedgerEventsArgs),
                ("gitpulse_task_view", TaskViewArgs),
                ("gitpulse_add_task", AddTaskArgs),
                ("gitpulse_import_tasks", ImportTasksArgs),
                ("gitpulse_list_tasks", ListTasksArgs),
                ("gitpulse_get_task", GetTaskArgs),
                ("gitpulse_complete_task", CompleteTaskArgs),
                ("gitpulse_delete_task", DeleteTaskArgs),
                ("gitpulse_merge_tasks", MergeTasksArgs),
                ("gitpulse_codeintel_search", SearchArgs),
                ("gitpulse_codeintel_impact", CodeintelImpactArgs),
                ("gitpulse_codeintel_dependencies", CodeintelDependenciesArgs),
                ("gitpulse_codeintel_trace", CodeintelTraceArgs),
                ("gitpulse_codeintel_dead_symbols", BudgetArgs),
                ("gitpulse_codeintel_suspects", SuspectsArgs),
                ("gitpulse_provenance", ProvenanceArgs),
                ("devmap_status", RepoArgs),
                ("devmap_search", SearchArgs),
                ("devmap_dependencies", dm::DependenciesArgs),
                ("devmap_impact", dm::ImpactArgs),
                ("devmap_trace", dm::TraceArgs),
                ("devmap_neighbors", dm::NeighborsArgs),
                ("devmap_dead_symbols", BudgetArgs),
                ("devmap_clones", BudgetArgs),
                ("devmap_preview", dm::PreviewArgs),
                ("devmap_explore", dm::ExploreArgs),
                ("devmap_affected_tests", dm::AffectedTestsArgs),
            ]
        }

        fn schema_of(name: &str) -> Value {
            tools()
                .into_iter()
                .find(|t| t["name"] == name)
                .unwrap_or_else(|| panic!("{name} is not advertised"))["inputSchema"]
                .clone()
        }

        /// The call survives the validator and extraction, as the server does it.
        fn accepted(name: &str, extract: Extract, arguments: &Value) -> bool {
            validate_arguments(name, arguments).is_ok() && extract(arguments).is_ok()
        }

        fn short(value: &Value) -> String {
            value.to_string().chars().take(80).collect()
        }

        /// A number from an integer that may be outside `i64`/`u64`.
        fn int_json(value: i128) -> Value {
            if let Ok(signed) = i64::try_from(value) {
                json!(signed)
            } else if let Ok(unsigned) = u64::try_from(value) {
                json!(unsigned)
            } else {
                json!(value as f64)
            }
        }

        struct Rng(u64);

        impl Rng {
            fn next(&mut self) -> u64 {
                self.0 ^= self.0 << 13;
                self.0 ^= self.0 >> 7;
                self.0 ^= self.0 << 17;
                self.0
            }

            fn below(&mut self, n: usize) -> usize {
                (self.next() % (n.max(1) as u64)) as usize
            }
        }

        const WEIRD: [&str; 12] = [
            "\0",
            "\u{feff}",
            "\u{202e}",
            "e\u{301}",
            "😀",
            "€",
            "\u{10ffff}",
            "\u{7f}",
            "a",
            " ",
            "\n",
            "\u{200d}",
        ];

        /// A random JSON value, leaning on the boundaries `schema` declares.
        fn gen(rng: &mut Rng, schema: &Value, depth: u8) -> Value {
            let max_len = schema["maxLength"].as_u64().unwrap_or(16).min(70_000) as usize;
            let min = schema["minimum"].as_i64().unwrap_or(0) as i128;
            let max = schema["maximum"].as_u64().unwrap_or(100) as i128;
            match rng.below(15) {
                0 => Value::Null,
                1 => json!(rng.below(2) == 0),
                2 => {
                    let candidates = [min - 1, min, min + 1, max - 1, max, max + 1, 0, -1, 1];
                    int_json(candidates[rng.below(candidates.len())])
                }
                3 => {
                    let floats = [
                        5.0,
                        5.5,
                        -0.0,
                        0.0,
                        1e300,
                        -1e300,
                        1e-320,
                        -1e-320,
                        9_007_199_254_740_991.0,
                        9_007_199_254_740_992.0,
                        9_223_372_036_854_775_808.0,
                        18_446_744_073_709_551_616.0,
                        4_294_967_296.0,
                        0.5,
                        1.0,
                        f64::MAX,
                        f64::MIN_POSITIVE,
                    ];
                    json!(floats[rng.below(floats.len())])
                }
                4 => {
                    let anchors = [min, max];
                    let anchor = anchors[rng.below(2)] + rng.below(3) as i128 - 1;
                    json!(anchor as f64)
                }
                5 => json!("a".repeat(rng.below(max_len + 2))),
                6 => json!("a".repeat(max_len)),
                7 => json!("a".repeat(max_len + 1)),
                8 => json!("€".repeat(max_len / 3 + rng.below(2))),
                9 => {
                    let mut text = String::new();
                    for _ in 0..rng.below(24) {
                        text.push_str(WEIRD[rng.below(WEIRD.len())]);
                    }
                    json!(text)
                }
                10 if depth < 2 => {
                    let items = &schema["items"];
                    Value::Array(
                        (0..rng.below(5))
                            .map(|_| gen(rng, items, depth + 1))
                            .collect(),
                    )
                }
                11 => {
                    let wanted = schema["maxItems"].as_u64().unwrap_or(3).min(3_000) as usize;
                    let count = (wanted + rng.below(2)).min(3_000);
                    let element = match schema["items"]["type"].as_str() {
                        Some("string") => json!("a"),
                        Some("object") => json!({ "task_id": "a" }),
                        _ => json!(1),
                    };
                    Value::Array(vec![element; count])
                }
                12 if depth < 2 => {
                    let keys = [
                        "__proto__",
                        "task_id",
                        "a",
                        "Repo_Path",
                        "expected_revision",
                    ];
                    let mut object = serde_json::Map::new();
                    for _ in 0..rng.below(4) {
                        object.insert(
                            keys[rng.below(keys.len())].to_string(),
                            gen(rng, &Value::Null, depth + 1),
                        );
                    }
                    Value::Object(object)
                }
                13 => match schema["enum"].as_array() {
                    Some(values) if !values.is_empty() => {
                        let mut pick = values[rng.below(values.len())].clone();
                        if rng.below(3) == 0 {
                            pick = json!(format!("{}x", pick.as_str().unwrap_or("")));
                        }
                        pick
                    }
                    _ => json!("x"),
                },
                _ => json!(rng.below(1000) as u64),
            }
        }

        /// The invariants the validator and the extractor owe each other.
        fn judge(name: &str, extract: Extract, schema: &Value, arguments: &Value) -> (bool, bool) {
            let violations = validate::validate(arguments, schema);
            let extracted = extract(arguments);
            let shown = short(arguments);

            // Extraction owns the byte bounds, so a call the validator passed may
            // still be refused for those, and only for those.
            let only_bytes = extracted.as_ref().err().is_some_and(|refused| {
                refused.iter().all(|v| v.message.contains("bytes of UTF-8"))
            });
            if violations.is_empty() && !only_bytes {
                assert!(
                    extracted.is_ok(),
                    "{name}: the validator passed what extraction refuses: {shown} -> {:?}",
                    extracted.as_ref().err()
                );
            }
            if let Err(refused) = &extracted {
                assert!(!refused.is_empty(), "{name}: an empty refusal for {shown}");
            }
            if extracted.is_ok() {
                for violation in &violations {
                    assert!(
                        violation.message.starts_with("must "),
                        "{name}: extraction read what the validator refused for a reason that is not a bound ({}): {shown}",
                        violation.render()
                    );
                }
            }
            let known = schema["properties"].as_object().unwrap();
            let named = violations
                .iter()
                .chain(extracted.as_ref().err().into_iter().flatten());
            for violation in named {
                let head = violation
                    .path
                    .split(['[', '.'])
                    .next()
                    .unwrap_or_default()
                    .to_string();
                assert!(
                    known.contains_key(&head) || arguments.get(&head).is_some(),
                    "{name}: a refusal that names no argument: {:?} for {shown}",
                    violation.path
                );
            }
            if let Some(first) = violations.first() {
                let message = validate::refusal(name, &violations);
                assert!(message.starts_with(&format!("invalid arguments for {name}: ")));
                assert!(message.contains(&first.path), "{name}: {message}");
            }
            (violations.is_empty(), extracted.is_ok())
        }

        #[test]
        fn a_seeded_fuzz_keeps_validation_and_extraction_in_agreement() {
            let mut rng = Rng(0x9E37_79B9_7F4A_7C15);
            let (mut accepted_calls, mut refused_calls) = (0usize, 0usize);
            for (name, extract) in extractors() {
                let schema = schema_of(name);
                let fields: Vec<String> = schema["properties"]
                    .as_object()
                    .unwrap()
                    .keys()
                    .cloned()
                    .collect();
                let base = sample(&schema);
                for _ in 0..300 {
                    let mut arguments = base.clone();
                    for _ in 0..=rng.below(3) {
                        let field = &fields[rng.below(fields.len())];
                        match rng.below(10) {
                            0 => {
                                arguments.as_object_mut().unwrap().remove(field);
                            }
                            1 => {
                                arguments[format!("junk{}", rng.below(5))] = json!(1);
                            }
                            _ => {
                                arguments[field.as_str()] =
                                    gen(&mut rng, &schema["properties"][field], 0);
                            }
                        }
                    }
                    let (valid, extracted) = judge(name, extract, &schema, &arguments);
                    if valid && extracted {
                        accepted_calls += 1;
                    } else {
                        refused_calls += 1;
                    }
                }
                // Whole-object noise: keys drawn from the fields and from junk.
                for _ in 0..100 {
                    let mut arguments = serde_json::Map::new();
                    for _ in 0..rng.below(6) {
                        let key = if rng.below(4) == 0 {
                            "__proto__".to_string()
                        } else {
                            fields[rng.below(fields.len())].clone()
                        };
                        let sub = schema["properties"]
                            .get(&key)
                            .cloned()
                            .unwrap_or(Value::Null);
                        arguments.insert(key, gen(&mut rng, &sub, 0));
                    }
                    judge(name, extract, &schema, &Value::Object(arguments));
                }
            }
            assert!(
                accepted_calls > 200 && refused_calls > 200,
                "the fuzz explored one side only: {accepted_calls} accepted, {refused_calls} refused"
            );
        }

        #[test]
        fn every_integer_argument_at_and_around_its_bounds_as_integer_and_whole_float() {
            let max_exact: i128 = 999_999_999_999_999;
            let mut checked = 0;
            for (name, extract) in extractors() {
                let schema = schema_of(name);
                let base = sample(&schema);
                for (field, prop) in schema["properties"].as_object().unwrap() {
                    if prop["type"] != "integer" {
                        continue;
                    }
                    let min = prop["minimum"].as_i64().expect("minimum") as i128;
                    let max = prop["maximum"].as_u64().expect("maximum") as i128;
                    for value in [min - 1, min, max, max + 1] {
                        let inside = (min..=max).contains(&value);
                        let forms = [
                            (int_json(value), true),
                            (
                                json!(value as f64),
                                (-max_exact..=max_exact).contains(&value),
                            ),
                        ];
                        for (sent, representable) in forms {
                            let mut arguments = base.clone();
                            arguments[field.as_str()] = sent.clone();
                            let shown = format!("{name}.{field} = {sent}");
                            let validated = validate_arguments(name, &arguments);
                            if inside && representable {
                                validated.unwrap_or_else(|e| panic!("{shown} refused: {e}"));
                                let read = extract(&arguments)
                                    .unwrap_or_else(|e| panic!("{shown} not extracted: {e:?}"));
                                assert_eq!(read[field.as_str()], int_json(value), "{shown}");
                            } else {
                                let error = validated.expect_err(&format!("{shown} was accepted"));
                                assert!(error.contains(field.as_str()), "{shown}: {error}");
                            }
                            checked += 1;
                        }
                    }
                }
            }
            assert!(checked > 100, "only {checked} integer probes ran");
        }

        #[test]
        fn merge_sources_revisions_are_held_to_the_same_bounds() {
            let schema = schema_of("gitpulse_merge_tasks");
            let item = &schema["properties"]["sources"]["items"]["properties"]["expected_revision"];
            let max = item["maximum"].as_u64().expect("maximum") as i128;
            let min = item["minimum"].as_i64().expect("minimum") as i128;
            let base = |revision: Value| {
                json!({
                    "repo_path": "/no/such/repo",
                    "into_task_id": "a",
                    "reason": "r",
                    "sources": [{ "task_id": "b", "expected_revision": revision }]
                })
            };
            for (value, ok) in [(min - 1, false), (min, true), (max, true), (max + 1, false)] {
                for sent in [int_json(value), json!(value as f64)] {
                    let arguments = base(sent.clone());
                    let validated = validate_arguments("gitpulse_merge_tasks", &arguments);
                    let float_beyond_exact = sent.is_f64() && value.abs() > 999_999_999_999_999;
                    if ok && !float_beyond_exact {
                        validated.unwrap_or_else(|e| panic!("{sent}: {e}"));
                        let read = extract::<MergeTasksArgs>(&arguments).expect("extracts");
                        assert_eq!(
                            read["sources"][0]["expected_revision"],
                            int_json(value),
                            "{sent}"
                        );
                    } else {
                        let error = validated.expect_err(&format!("{sent} accepted"));
                        assert!(error.contains("sources[0].expected_revision"), "{error}");
                    }
                }
            }
        }

        /// How a string field counts its bound, from what its schema says.
        fn unit_of(prop: &Value) -> Option<&'static str> {
            let description = prop["description"].as_str().unwrap_or_default();
            if description.contains("bytes, UTF-8)") {
                Some("bytes")
            } else if description.contains("characters)") {
                Some("characters")
            } else {
                None
            }
        }

        /// Every string argument and every string-array item of every tool.
        fn string_fields() -> Vec<(&'static str, Extract, String, Value, bool)> {
            let mut out = Vec::new();
            for (name, extract) in extractors() {
                let schema = schema_of(name);
                for (field, prop) in schema["properties"].as_object().unwrap() {
                    match prop["type"].as_str() {
                        Some("string") if prop["enum"].is_null() => {
                            out.push((name, extract, field.clone(), prop.clone(), false));
                        }
                        Some("array") if prop["items"]["type"] == "string" => {
                            let mut shaped = prop.clone();
                            shaped["maxLength"] = prop["items"]["maxLength"].clone();
                            out.push((name, extract, field.clone(), shaped, true));
                        }
                        _ => {}
                    }
                }
            }
            out
        }

        fn with_string(base: &Value, field: &str, in_array: bool, text: &str) -> Value {
            let mut arguments = base.clone();
            arguments[field] = if in_array { json!([text]) } else { json!(text) };
            arguments
        }

        #[test]
        fn every_string_bound_counts_what_it_says_for_ascii_three_byte_four_byte_and_combining() {
            let mut checked = 0;
            for (name, extract, field, prop, in_array) in string_fields() {
                let max = prop["maxLength"].as_u64().expect("maxLength") as usize;
                if max > 100_000 {
                    continue;
                }
                let unit = unit_of(&prop)
                    .unwrap_or_else(|| panic!("{name}.{field} names no unit: {prop}"));
                let base = sample(&schema_of(name));
                let bytes = unit == "bytes";
                // (text unit, its length in the bound's unit)
                let pieces: [(&str, usize); 4] = [
                    ("a", 1),
                    ("€", if bytes { 3 } else { 1 }),
                    ("😀", if bytes { 4 } else { 1 }),
                    ("e\u{301}", if bytes { 3 } else { 2 }),
                ];
                for (piece, width) in pieces {
                    let fits = max / width;
                    let at_limit = piece.repeat(fits);
                    let over = piece.repeat(fits + 1);
                    let minimum = prop["minLength"].as_u64().unwrap_or(0) as usize;
                    if fits >= minimum.max(1) {
                        let good = with_string(&base, &field, in_array, &at_limit);
                        validate_arguments(name, &good).unwrap_or_else(|e| {
                            panic!(
                                "{name}.{field}: {fits} x {piece:?} ({unit}) refused: {}",
                                &e[..e.len().min(200)]
                            )
                        });
                        let read = extract(&good)
                            .unwrap_or_else(|e| panic!("{name}.{field} {piece:?}: {e:?}"));
                        assert_eq!(
                            read[field.as_str()],
                            if in_array {
                                json!([at_limit])
                            } else {
                                json!(at_limit)
                            },
                            "{name}.{field} did not echo {piece:?} exactly"
                        );
                    }
                    let bad = with_string(&base, &field, in_array, &over);
                    assert!(
                        !accepted(name, extract, &bad),
                        "{name}.{field}: {} x {piece:?} is over {max} {unit} and was accepted",
                        fits + 1
                    );
                    checked += 1;
                }
            }
            assert!(checked > 150, "only {checked} string probes ran");
        }

        #[test]
        fn awkward_unicode_survives_extraction_byte_for_byte() {
            let text = "\u{0}\u{feff}\u{202e}e\u{301}\u{200d}😀\u{7f}\u{10ffff}\n";
            let mut checked = 0;
            for (name, extract, field, prop, in_array) in string_fields() {
                let max = prop["maxLength"].as_u64().unwrap() as usize;
                if max < text.len()
                    || prop["minLength"].as_u64().unwrap_or(0) > text.chars().count() as u64
                {
                    continue;
                }
                let arguments = with_string(&sample(&schema_of(name)), &field, in_array, text);
                if validate_arguments(name, &arguments).is_err() {
                    continue;
                }
                let read = extract(&arguments).expect("validated, so extracted");
                let echoed = if in_array {
                    read[field.as_str()][0].clone()
                } else {
                    read[field.as_str()].clone()
                };
                assert_eq!(echoed, json!(text), "{name}.{field}");
                checked += 1;
            }
            assert!(checked > 30, "only {checked} fields took the probe");
        }

        #[test]
        fn a_value_of_the_wrong_shape_is_refused_naming_its_argument() {
            let shapes = [
                json!({}),
                json!({ "a": { "b": [] } }),
                json!([]),
                json!([[]]),
                json!([{}]),
                json!(12),
                json!(true),
                json!("text"),
                json!(-0.0),
            ];
            let mut checked = 0;
            for (name, _) in extractors() {
                let schema = schema_of(name);
                let base = sample(&schema);
                for (field, prop) in schema["properties"].as_object().unwrap() {
                    let expects = prop["type"].as_str().unwrap();
                    for shape in &shapes {
                        let fits = match shape {
                            Value::Object(_) => expects == "object",
                            Value::Array(items) => {
                                expects == "array"
                                    && (items.is_empty()
                                        || prop["items"]["type"] == "array"
                                        || (prop["items"]["type"] == "object"
                                            && items.iter().all(Value::is_object)))
                            }
                            Value::Number(_) => matches!(expects, "number" | "integer"),
                            Value::Bool(_) => expects == "boolean",
                            Value::String(_) => expects == "string" && prop["enum"].is_null(),
                            Value::Null => true,
                        };
                        if fits {
                            continue;
                        }
                        let mut arguments = base.clone();
                        arguments[field.as_str()] = shape.clone();
                        let error = validate_arguments(name, &arguments)
                            .expect_err(&format!("{name}.{field} accepted {shape}"));
                        assert!(
                            error.starts_with(&format!("invalid arguments for {name}: "))
                                && error.contains(field.as_str()),
                            "{name}.{field} = {shape}: {error}"
                        );
                        checked += 1;
                    }
                }
            }
            assert!(checked > 500, "only {checked} wrong shapes ran");
        }

        #[test]
        fn a_huge_array_and_a_megabyte_string_are_refused_quickly_and_name_the_argument() {
            let started = std::time::Instant::now();
            let megabyte = "x".repeat(2 << 20);
            for (name, _) in extractors() {
                let schema = schema_of(name);
                let base = sample(&schema);
                for (field, prop) in schema["properties"].as_object().unwrap() {
                    let hostile = match prop["type"].as_str() {
                        Some("string") => json!(megabyte),
                        Some("array") => Value::Array(vec![json!(1); 10_000]),
                        _ => continue,
                    };
                    let mut arguments = base.clone();
                    arguments[field.as_str()] = hostile;
                    let error = validate_arguments(name, &arguments)
                        .expect_err(&format!("{name}.{field} accepted a hostile value"));
                    assert!(error.contains(field.as_str()), "{name}.{field}");
                }
            }
            assert!(
                started.elapsed()
                    < crate::test_support::env::coverage_relaxed(std::time::Duration::from_secs(
                        60
                    )),
                "hostile values took {:?}",
                started.elapsed()
            );
        }

        #[test]
        fn a_key_that_only_differs_in_case_or_is_a_prototype_name_is_unknown() {
            for (name, extract) in extractors() {
                let schema = schema_of(name);
                let base = sample(&schema);
                let mut hostile: Vec<String> = vec![
                    "__proto__".into(),
                    "constructor".into(),
                    "prototype".into(),
                    "$ref".into(),
                    String::new(),
                ];
                for field in schema["properties"].as_object().unwrap().keys() {
                    hostile.push(field.to_uppercase());
                    hostile.push(format!("{field} "));
                    hostile.push(format!(" {field}"));
                    hostile.push(format!("{field}\u{0}"));
                    hostile.push(field.replace('_', "-"));
                }
                for key in hostile {
                    let mut arguments = base.clone();
                    arguments[key.as_str()] = json!(1);
                    if schema["properties"].get(&key).is_some() {
                        continue;
                    }
                    let error = validate_arguments(name, &arguments)
                        .expect_err(&format!("{name} accepted the key {key:?}"));
                    assert!(
                        error.contains("unknown argument"),
                        "{name} {key:?}: {error}"
                    );
                    assert!(extract(&arguments).is_err(), "{name} {key:?}");
                }
            }
        }

        #[test]
        fn a_duplicate_key_is_decided_by_the_json_parser_and_the_winner_is_what_is_validated() {
            let last_wins: Value =
                serde_json::from_str(r#"{"repo_path":5,"repo_path":"/no/such/repo"}"#).unwrap();
            assert_eq!(last_wins["repo_path"], "/no/such/repo");
            validate_arguments("gitpulse_status", &last_wins).expect("the last key is valid");
            assert!(extract::<RepoArgs>(&last_wins).is_ok());

            let bad_last: Value =
                serde_json::from_str(r#"{"repo_path":"/no/such/repo","repo_path":5}"#).unwrap();
            let error =
                validate_arguments("gitpulse_status", &bad_last).expect_err("5 is not a path");
            assert!(error.contains("repo_path"), "{error}");
            assert!(extract::<RepoArgs>(&bad_last).is_err());
        }

        #[test]
        fn json_cannot_carry_what_the_extractor_would_have_to_trust() {
            for text in [
                "NaN",
                "Infinity",
                "-Infinity",
                "1e400",
                "-1e400",
                "+1",
                "01",
                ".5",
                "5.",
                "0x10",
            ] {
                assert!(
                    serde_json::from_str::<Value>(text).is_err(),
                    "{text} parsed as JSON"
                );
            }
            assert!(serde_json::Number::from_f64(f64::NAN).is_none());
            assert!(serde_json::Number::from_f64(f64::INFINITY).is_none());
            // "-0" is a float, whole, and equal to zero.
            let negative_zero: Value = serde_json::from_str("-0").unwrap();
            assert!(validate::exact_int(negative_zero.as_number().unwrap()) == Some(0));
            let subnormal: Value = serde_json::from_str("1e-320").unwrap();
            assert!(validate::exact_int(subnormal.as_number().unwrap()).is_none());
        }

        #[test]
        fn zero_subnormal_and_borderline_confidences_are_read_within_their_range() {
            let schema = schema_of("devmap_impact");
            let base = {
                let mut base = sample(&schema);
                base["repo_path"] = json!("/no/such/repo");
                base
            };
            let min = schema["properties"]["min_confidence"]["minimum"]
                .as_f64()
                .unwrap();
            let max = schema["properties"]["min_confidence"]["maximum"]
                .as_f64()
                .unwrap();
            for text in [
                "0",
                "-0",
                "-0.0",
                "0.0",
                "1e-320",
                "5e-324",
                "1",
                "1.0",
                "0.9999999999999999",
                "0.99999999999999999999",
                "1.0000000000000002",
                "-1e-320",
                "1.0000000000000001",
            ] {
                let sent: Value = serde_json::from_str(text).unwrap();
                let mut arguments = base.clone();
                arguments["min_confidence"] = sent.clone();
                let verdict = validate_arguments("devmap_impact", &arguments);
                let parsed = extract::<devmap_parity::ImpactArgs>(&arguments);
                let value = sent.as_f64().unwrap();
                if (min..=max).contains(&value) {
                    verdict.unwrap_or_else(|e| panic!("{text}: {e}"));
                    let read = parsed.unwrap_or_else(|e| panic!("{text}: {e:?}"));
                    let narrowed = read["min_confidence"].as_f64().expect("a number");
                    assert!(
                        (min..=max).contains(&narrowed),
                        "{text} was read as {narrowed}, outside [{min}, {max}]"
                    );
                } else {
                    assert!(verdict.is_err(), "{text} was accepted");
                }
            }
        }

        #[test]
        fn the_parse_path_has_no_shared_mutable_state_and_is_deterministic_across_threads() {
            let source = include_str!("validate.rs");
            let typed = &source[source.find("// Typed arguments").expect("typed section")
                ..source.find("#[cfg(test)]\nmod tests").expect("tests")];
            for needle in [
                "static mut",
                "thread_local",
                "OnceLock",
                "Mutex",
                "RwLock",
                "lazy_static",
                "AtomicU",
            ] {
                assert!(
                    !typed.contains(needle),
                    "the typed layer holds global state: {needle}"
                );
            }
            let schema = schema_of("gitpulse_ledger_events");
            let cases: Vec<Value> = (0..40)
                .map(|n| {
                    let mut arguments = sample(&schema);
                    arguments["cursor"] = match n % 4 {
                        0 => json!(n),
                        1 => json!(n as f64),
                        2 => json!(-1),
                        _ => json!(1e300),
                    };
                    arguments
                })
                .collect();
            let verdicts = |cases: &[Value]| -> Vec<(bool, Option<Value>)> {
                cases
                    .iter()
                    .map(|arguments| {
                        (
                            validate_arguments("gitpulse_ledger_events", arguments).is_ok(),
                            extract::<LedgerEventsArgs>(arguments).ok(),
                        )
                    })
                    .collect()
            };
            let expected = verdicts(&cases);
            let handles: Vec<_> = (0..8)
                .map(|_| {
                    let cases = cases.clone();
                    std::thread::spawn(move || {
                        (0..200).all(|_| {
                            cases
                                .iter()
                                .map(|arguments| {
                                    (
                                        validate_arguments("gitpulse_ledger_events", arguments)
                                            .is_ok(),
                                        extract::<LedgerEventsArgs>(arguments).ok(),
                                    )
                                })
                                .collect::<Vec<_>>()
                                == verdicts(&cases)
                        })
                    })
                })
                .collect();
            for handle in handles {
                assert!(handle.join().expect("no panic"));
            }
            assert_eq!(expected, verdicts(&cases));
        }

        #[test]
        fn nothing_outside_handle_tool_call_dispatches_a_tool() {
            let production = |source: &'static str| {
                &source[..source
                    .find("#[cfg(test)]\nmod tests")
                    .unwrap_or(source.len())]
            };
            let mod_rs = production(include_str!("mod.rs"));
            assert_eq!(
                mod_rs.matches("handle_tool_call(").count(),
                2,
                "handle_tool_call is defined once and called once, from `run`"
            );
            assert_eq!(mod_rs.matches("devmap_parity::handle(").count(), 1);
            for (file, source) in [
                ("resources.rs", include_str!("resources.rs")),
                ("prompts.rs", include_str!("prompts.rs")),
                ("complete.rs", include_str!("complete.rs")),
                ("page.rs", include_str!("page.rs")),
                (
                    "devmap_parity.rs",
                    production(include_str!("devmap_parity.rs")),
                ),
                (
                    "bin/gitpulse-mcp.rs",
                    include_str!("../bin/gitpulse-mcp.rs"),
                ),
            ] {
                for needle in [
                    "handle_tool_call(",
                    "devmap_parity::handle(",
                    "::from_args(",
                ] {
                    assert!(
                        !source.contains(needle),
                        "{file} dispatches around the validator: {needle}"
                    );
                }
            }
        }
    }
}

#[cfg(test)]
mod deferral_retry_tests {
    use super::*;

    /// A retry harness with no real clock, sleep or randomness: `jitter` is
    /// what the injected RNG returns, `deadline_in` how far away the request's
    /// deadline is, and `sleeps` records every delay it was asked to wait.
    struct Harness {
        sleeps: std::cell::RefCell<Vec<std::time::Duration>>,
        jitter: u64,
        origin: std::time::Instant,
        deadline_in: std::time::Duration,
    }

    impl Harness {
        fn new(jitter: u64, deadline_in: std::time::Duration) -> Self {
            Self {
                sleeps: Default::default(),
                jitter,
                origin: std::time::Instant::now(),
                deadline_in,
            }
        }

        fn run<T>(&self, body: impl FnOnce(&DeferralRetry<'_>) -> T) -> T {
            let sleep = |delay: std::time::Duration| self.sleeps.borrow_mut().push(delay);
            let jitter = |bound: u64| self.jitter.min(bound - 1);
            let now = || self.origin;
            body(&DeferralRetry {
                sleep: &sleep,
                jitter: &jitter,
                now: &now,
                deadline: self.origin + self.deadline_in,
            })
        }
    }

    const ROOMY: std::time::Duration = std::time::Duration::from_secs(5);

    fn deferred_payload() -> Value {
        json!({ "ok": false, "error": "git status deferred under load after 0.000s: background \
                  work is shed while the git spawn rate budget is low" })
    }

    fn ran_payload() -> Value {
        json!({ "ok": true, "error": "" })
    }

    /// A deferral is asked once more, after the refill interval plus the
    /// injected jitter; when the second ask succeeds the caller sees success
    /// and no trace of a deferral.
    #[test]
    fn a_deferred_read_is_retried_once_with_jitter_and_a_success_carries_no_deferral() {
        let harness = Harness::new(7, ROOMY);
        let reads = std::cell::Cell::new(0);
        let answer = harness.run(|retry| {
            read_insights(
                || {
                    reads.set(reads.get() + 1);
                    if reads.get() == 1 {
                        deferred_payload()
                    } else {
                        ran_payload()
                    }
                },
                retry,
            )
        });
        assert_eq!(reads.get(), 2);
        assert_eq!(answer, ran_payload(), "{answer}");
        let base = insights::deferral_retry_after_ms();
        assert_eq!(
            *harness.sleeps.borrow(),
            vec![std::time::Duration::from_millis(base + 7)]
        );
    }

    /// A retry that is also deferred is reported as deferred, after exactly
    /// one retry: never a loop.
    #[test]
    fn a_retry_that_also_defers_is_reported_deferred_after_exactly_one_retry() {
        let harness = Harness::new(0, ROOMY);
        let reads = std::cell::Cell::new(0);
        let answer = harness.run(|retry| {
            read_insights(
                || {
                    reads.set(reads.get() + 1);
                    deferred_payload()
                },
                retry,
            )
        });
        assert_eq!(reads.get(), 2, "one read and one retry");
        assert_eq!(harness.sleeps.borrow().len(), 1);
        assert_eq!(answer["deferred"], json!(true), "{answer}");
        assert_eq!(
            answer["retry_after_ms"],
            json!(insights::deferral_retry_after_ms())
        );
    }

    /// A read that ran, or failed for a real reason, is never retried.
    #[test]
    fn a_read_that_did_not_defer_is_never_retried() {
        for payload in [
            ran_payload(),
            json!({ "ok": false, "error": "fatal: not a git repository" }),
        ] {
            let harness = Harness::new(0, ROOMY);
            let reads = std::cell::Cell::new(0);
            let answer = harness.run(|retry| {
                read_insights(
                    || {
                        reads.set(reads.get() + 1);
                        payload.clone()
                    },
                    retry,
                )
            });
            assert_eq!(reads.get(), 1, "{payload}");
            assert!(harness.sleeps.borrow().is_empty());
            assert_eq!(answer, payload);
        }
    }

    /// The retry is bounded: the delay stays under 250 ms at the largest
    /// jitter, and a deadline that would pass before the retry could finish
    /// (or already has) skips it and reports the first, deferred answer.
    #[test]
    fn the_retry_is_bounded_and_respects_the_deadline() {
        let base = insights::deferral_retry_after_ms();
        let widest = Harness::new(u64::MAX, ROOMY);
        widest.run(|retry| {
            read_insights(deferred_payload, retry);
        });
        let delay = widest.sleeps.borrow()[0];
        assert!(delay < std::time::Duration::from_millis(250), "{delay:?}");
        assert!(delay >= std::time::Duration::from_millis(base));

        for deadline_in in [
            std::time::Duration::ZERO,
            std::time::Duration::from_millis(base),
        ] {
            let harness = Harness::new(0, deadline_in);
            let reads = std::cell::Cell::new(0);
            let answer = harness.run(|retry| {
                read_insights(
                    || {
                        reads.set(reads.get() + 1);
                        deferred_payload()
                    },
                    retry,
                )
            });
            assert_eq!(reads.get(), 1, "{deadline_in:?}");
            assert!(harness.sleeps.borrow().is_empty(), "{deadline_in:?}");
            assert_eq!(answer["deferred"], json!(true), "{answer}");
        }
    }

    /// End to end through the real snapshot and the tool dispatcher: a spawn
    /// gate that sheds every read answers with `deferred`/`retry_after_ms` on
    /// each failed facet (after the one real retry, at most 250 ms) and says
    /// invalid_worktree nowhere; a repository that is genuinely not one still
    /// says so and carries no deferral.
    #[test]
    fn collision_risk_and_insights_under_a_shed_gate_report_deferral_not_invalid() {
        let repo = crate::test_support::git_repo();
        let path = repo.path().to_str().unwrap();
        for tool in ["gitpulse_collision_risk", "gitpulse_insights"] {
            let answer = crate::engine::git_cli::with_forced_deferral(|| {
                handle_tool_call(tool, &json!({ "repo_path": path })).expect(tool)
            });
            assert!(!answer["ok"].as_bool().unwrap_or(false), "{tool}");
            let text = answer.to_string();
            assert!(!text.contains("invalid_worktree"), "{tool}: {text}");
            let marked = if tool == "gitpulse_insights" {
                &answer["worktrees"]
            } else {
                &answer
            };
            assert_eq!(marked["deferred"], json!(true), "{tool}: {text}");
            assert!(marked["retry_after_ms"].as_u64().unwrap() > 0, "{tool}");
        }

        let plain = tempfile::tempdir().unwrap();
        let answer = handle_tool_call(
            "gitpulse_insights",
            &json!({ "repo_path": plain.path().to_str().unwrap() }),
        )
        .unwrap();
        assert!(answer["ledger"].get("deferred").is_none(), "{answer}");
    }

    /// The ledger read resolves its address through the same retry: a shed
    /// read is asked again once, and its error says deferred, not invalid.
    #[test]
    fn a_ledger_read_that_stays_shed_names_the_deferral_after_one_retry() {
        let repo = crate::test_support::git_repo();
        let harness = Harness::new(0, ROOMY);
        let error = crate::engine::git_cli::with_forced_deferral(|| {
            harness.run(|retry| read_address(repo.path().to_str().unwrap(), retry))
        })
        .expect_err("a shed read cannot resolve");
        assert!(error.contains("[deferred_worktree]"), "{error}");
        assert_eq!(harness.sleeps.borrow().len(), 1);
    }
}
