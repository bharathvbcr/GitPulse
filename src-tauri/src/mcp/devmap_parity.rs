//! DevMap tool-name parity for `gitpulse-mcp`.
//!
//! Upstream `devmap_serve::mcp::TOOL_NAMES` is the published surface of the
//! standalone `devmap mcp` server. GitPulse does not vendor that crate (it
//! pulls `hyper` / HTTP transport crates GitPulse does not otherwise need);
//! instead this module owns the same ordered name list and the in-process
//! dispatch through [`crate::codeintel`], so `tools/list` cannot silently drop
//! a tool the global server still advertises.
//!
//! Every call still requires GitPulse's `repo_path` — this process mediates
//! many repositories. The standalone `devmap mcp` server accepts the same
//! argument and refuses first-wins when more than one store is in scope.

use serde_json::{json, Value};

use super::{budget_prop, path_prop, repo_prop, tool, MAX_ARG_CHARS};

/// Ordered tool names matching `devmap_serve::mcp::TOOL_NAMES`.
///
/// Keep this list identical to upstream. The parity test fails when either
/// side grows without the other.
pub const TOOL_NAMES: &[&str] = &[
    "devmap_status",
    "devmap_search",
    "devmap_dependencies",
    "devmap_impact",
    "devmap_trace",
    "devmap_neighbors",
    "devmap_dead_symbols",
    "devmap_clones",
    "devmap_preview",
    "devmap_explore",
    "devmap_affected_tests",
];

fn budget_default() -> Value {
    budget_prop()
}

fn depth_prop() -> Value {
    json!({
        "type": "integer",
        "description": "Maximum traversal depth (default 3). A walk stopped by this cap sets walk_incomplete.",
        "minimum": 1,
        "maximum": 64
    })
}

fn confidence_prop() -> Value {
    json!({
        "type": "number",
        "description": "Minimum edge confidence in [0,1] (default 0.0)",
        "minimum": 0.0,
        "maximum": 1.0
    })
}

fn targets_prop() -> Value {
    json!({
        "type": "array",
        "description": "Symbols or file paths (max 16)",
        "items": {
            "type": "string",
            "minLength": 1,
            "maxLength": MAX_ARG_CHARS
        },
        "minItems": 1,
        "maxItems": crate::codeintel::MAX_NEIGHBOR_TARGETS
    })
}

/// `devmap_neighbors` answers `CodeintelResponse<CodeintelNeighbors>`: the
/// envelope an agent reads before treating an empty list as "nothing exists".
/// `items[].callers` / `.callees` are themselves `CodeintelResponse` envelopes
/// of edges and are left open here; the envelope fields are the contract.
fn neighbors_output_schema() -> Value {
    let facet = json!({ "type": "object" });
    json!({
        "type": "object",
        "properties": {
            "source_freshness": { "type": "object" },
            "available": { "type": "boolean" },
            "reason": { "type": ["string", "null"] },
            "items": {
                "type": "array",
                "items": {
                    "type": "object",
                    "properties": {
                        "target": { "type": "string" },
                        "callers": facet,
                        "callees": facet
                    },
                    "required": ["target", "callers", "callees"]
                }
            },
            "total": { "type": "integer", "minimum": 0 },
            "shown": { "type": "integer", "minimum": 0 },
            "truncated": { "type": "boolean" },
            "walk_incomplete": { "type": "string" },
            "rungs": { "type": "object" }
        },
        "required": ["source_freshness", "available", "items", "total", "shown", "truncated"]
    })
}

/// Tool definitions for the DevMap-parity surface, each requiring `repo_path`.
pub fn tool_definitions() -> Vec<Value> {
    vec![
        tool(
            "devmap_status",
            "DevMap status",
            "Index health for a repository: generation, node/edge counts, freshness. Call first when another DevMap tool returns an empty or surprising answer.",
            json!({ "repo_path": repo_prop() }),
            &["repo_path"],
            json!({ "type": "object" }),
        ),
        tool(
            "devmap_search",
            "DevMap search",
            "Find symbols by name across the indexed repository.",
            json!({
                "repo_path": repo_prop(),
                "query": path_prop("Symbol name or prefix"),
                "budget": budget_default()
            }),
            &["repo_path", "query"],
            json!({ "type": "object" }),
        ),
        tool(
            "devmap_dependencies",
            "DevMap dependencies",
            "Outgoing edges from a file or symbol in the code graph.",
            json!({
                "repo_path": repo_prop(),
                "target": path_prop("File or symbol whose dependencies to read"),
                "budget": budget_default(),
                "depth": depth_prop(),
                "min_rung": path_prop("Optional minimum resolution rung")
            }),
            &["repo_path", "target"],
            json!({ "type": "object" }),
        ),
        tool(
            "devmap_impact",
            "DevMap impact",
            "Downstream blast radius / affected callers for a symbol or file.",
            json!({
                "repo_path": repo_prop(),
                "target": path_prop("Symbol or file path"),
                "budget": budget_default(),
                "depth": depth_prop(),
                "min_rung": path_prop("Optional minimum resolution rung")
            }),
            &["repo_path", "target"],
            json!({ "type": "object" }),
        ),
        tool(
            "devmap_trace",
            "DevMap trace",
            "Shortest edge path between two symbols in the code graph.",
            json!({
                "repo_path": repo_prop(),
                "from": path_prop("Origin symbol or file"),
                "to": path_prop("Destination symbol or file"),
                "budget": budget_default(),
                "depth": depth_prop(),
                "min_rung": path_prop("Optional minimum resolution rung")
            }),
            &["repo_path", "from", "to"],
            json!({ "type": "object" }),
        ),
        tool(
            "devmap_neighbors",
            "DevMap neighbors",
            "Callers and callees for up to 16 targets in one exchange.",
            json!({
                "repo_path": repo_prop(),
                "targets": targets_prop(),
                "budget": budget_default(),
                "depth": depth_prop(),
                "min_confidence": confidence_prop()
            }),
            &["repo_path", "targets"],
            neighbors_output_schema(),
        ),
        tool(
            "devmap_dead_symbols",
            "DevMap dead symbols",
            "Unreferenced symbols. Unavailable is reported; an empty list never means the check did not run.",
            json!({
                "repo_path": repo_prop(),
                "budget": budget_default()
            }),
            &["repo_path"],
            json!({ "type": "object" }),
        ),
        tool(
            "devmap_clones",
            "DevMap clones",
            "Duplicate and structurally similar code groups.",
            json!({
                "repo_path": repo_prop(),
                "budget": budget_default()
            }),
            &["repo_path"],
            json!({ "type": "object" }),
        ),
        tool(
            "devmap_preview",
            "DevMap preview",
            "Speculative edit preview. GitPulse's in-process path is parser-free; this tool reports unavailable and points at `devmap preview` / global `devmap mcp` when parse is required.",
            json!({
                "repo_path": repo_prop(),
                "file": path_prop("Repository-relative path being edited"),
                    "content": {
                    "type": "string",
                    "description": "Proposed full replacement content",
                    "maxLength": 1_048_576
                },
                "budget": budget_default()
            }),
            &["repo_path", "file", "content"],
            json!({ "type": "object" }),
        ),
        tool(
            "devmap_explore",
            "DevMap explore",
            "Definitions matching a query plus callers, callees, and layered blast radius.",
            json!({
                "repo_path": repo_prop(),
                "query": path_prop("Symbol name or fragment"),
                "budget": budget_default(),
                "limit": {
                    "type": "integer",
                    "description": "Maximum definitions to return (default 20)",
                    "minimum": 1,
                    "maximum": 100
                },
                "depth": depth_prop()
            }),
            &["repo_path", "query"],
            json!({ "type": "object" }),
        ),
        tool(
            "devmap_affected_tests",
            "DevMap affected tests",
            "Test files reachable through the inbound blast radius of the given targets.",
            json!({
                "repo_path": repo_prop(),
                "targets": targets_prop(),
                "budget": budget_default(),
                "depth": depth_prop(),
                "min_confidence": confidence_prop()
            }),
            &["repo_path", "targets"],
            json!({ "type": "object" }),
        ),
    ]
}

fn string_args(arguments: &Value, keys: &[&str]) -> Result<Vec<String>, String> {
    let Some(arr) = arguments[keys[0]].as_array() else {
        return Err(format!("missing {}", keys[0]));
    };
    let mut out = Vec::with_capacity(arr.len());
    for item in arr {
        let Some(s) = item.as_str() else {
            return Err(format!("{} entries must be strings", keys[0]));
        };
        out.push(s.to_string());
    }
    Ok(out)
}

/// Dispatch one DevMap-parity tool through the in-process codeintel layer.
pub fn handle(name: &str, arguments: &Value) -> Result<Value, String> {
    let repo = arguments["repo_path"].as_str().ok_or("missing repo_path")?;
    let budget = arguments["budget"].as_u64().map(|b| b as u32);
    match name {
        "devmap_status" => Ok(json!(crate::codeintel::status(repo))),
        "devmap_search" => {
            let query = arguments["query"].as_str().ok_or("missing query")?;
            Ok(json!(crate::codeintel::search(repo, query, budget)))
        }
        "devmap_dependencies" => {
            let target = arguments["target"].as_str().ok_or("missing target")?;
            let min_rung = arguments["min_rung"].as_str();
            Ok(json!(crate::codeintel::dependencies_at_rung(
                repo, target, budget, min_rung, None
            )))
        }
        "devmap_impact" => {
            let target = arguments["target"].as_str().ok_or("missing target")?;
            let min_rung = arguments["min_rung"].as_str();
            Ok(json!(crate::codeintel::impact_at_rung(
                repo, target, budget, min_rung, None
            )))
        }
        "devmap_trace" => {
            let from = arguments["from"].as_str().ok_or("missing from")?;
            let to = arguments["to"].as_str().ok_or("missing to")?;
            let min_rung = arguments["min_rung"].as_str();
            Ok(json!(crate::codeintel::trace_between_at_rung(
                repo, from, to, budget, min_rung, None
            )))
        }
        "devmap_neighbors" => {
            let targets = string_args(arguments, &["targets"])?;
            let min_confidence = arguments["min_confidence"].as_f64().map(|c| c as f32);
            let depth = arguments["depth"].as_u64().map(|d| d as usize);
            Ok(json!(crate::codeintel::neighbors_envelope(
                crate::codeintel::neighbors(repo, &targets, budget, None, min_confidence, depth)
            )))
        }
        "devmap_dead_symbols" => Ok(json!(crate::codeintel::dead_symbols(repo, budget))),
        "devmap_clones" => Ok(json!(crate::codeintel::clones(repo, budget))),
        "devmap_preview" => {
            let file = arguments["file"].as_str().ok_or("missing file")?;
            let content = arguments["content"].as_str().ok_or("missing content")?;
            Ok(json!(crate::codeintel::preview(
                repo, file, content, budget
            )))
        }
        "devmap_explore" => {
            let query = arguments["query"].as_str().ok_or("missing query")?;
            let limit = arguments["limit"].as_u64().map(|n| n as u32);
            Ok(json!(crate::codeintel::explore(repo, query, budget, limit)))
        }
        "devmap_affected_tests" => {
            let targets = string_args(arguments, &["targets"])?;
            Ok(json!(crate::codeintel::affected_tests(
                repo, &targets, budget, None
            )))
        }
        _ => Err(format!("Unknown DevMap tool: {name}")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn advertised_names_match_upstream_tool_names() {
        let definitions = tool_definitions();
        let advertised: Vec<&str> = definitions
            .iter()
            .map(|t| t["name"].as_str().unwrap())
            .collect();
        assert_eq!(advertised, TOOL_NAMES);
        assert_eq!(TOOL_NAMES.len(), 11);
    }

    /// A two-hop chain behind one indexed generation: `probe_caller` calls
    /// `probe_callee` (confidence 0.9), which calls `probe_island` (0.3).
    fn two_hop_repo() -> (tempfile::TempDir, String) {
        let dir = crate::codeintel::tests::repo_with_one_generation();
        let root = dir.path().to_string_lossy().into_owned();
        let conn = rusqlite::Connection::open(crate::codeintel::devmap_db_path(&root))
            .expect("open fixture db");
        conn.execute(
            "INSERT INTO edge_rows
               (edge_id, source_file_id, target_file_id,
                source_symbol, target_symbol, edge_kind, confidence,
                valid_from, valid_to)
             VALUES (1, 2, 3, 'probe_callee', 'probe_island', 'Calls', 0.3, 1, NULL)",
            [],
        )
        .expect("insert second hop");
        conn.close().expect("close fixture db");
        (dir, root)
    }

    fn callee_symbols(entry: &Value) -> Vec<String> {
        entry["callees"]["items"]
            .as_array()
            .expect("callees.items")
            .iter()
            .map(|e| e["target_symbol"].as_str().unwrap().to_string())
            .collect()
    }

    #[test]
    fn neighbors_answers_an_object_in_the_codeintel_envelope() {
        let (_dir, root) = two_hop_repo();
        let out = handle(
            "devmap_neighbors",
            &json!({ "repo_path": root, "targets": ["probe_caller"] }),
        )
        .expect("neighbors answers");
        assert!(
            out.is_object(),
            "an array fails the declared outputSchema: {out}"
        );
        for key in [
            "source_freshness",
            "available",
            "total",
            "shown",
            "truncated",
        ] {
            assert!(out.get(key).is_some(), "envelope lacks {key}: {out}");
        }
        assert_eq!(out["available"], true, "{out}");
        assert_eq!(out["items"][0]["target"], "probe_caller", "{out}");
    }

    #[test]
    fn neighbors_honours_depth() {
        let (_dir, root) = two_hop_repo();
        let walk = |depth: u64| {
            let out = handle(
                "devmap_neighbors",
                &json!({ "repo_path": root, "targets": ["probe_caller"], "depth": depth }),
            )
            .expect("neighbors answers");
            callee_symbols(&out["items"][0])
        };
        assert_eq!(
            walk(1),
            ["probe_callee"],
            "depth 1 must not reach the second hop"
        );
        assert!(
            walk(2).contains(&"probe_island".to_string()),
            "depth 2 reaches the second hop"
        );
    }

    #[test]
    fn neighbors_honours_min_confidence() {
        let (_dir, root) = two_hop_repo();
        let out = handle(
            "devmap_neighbors",
            &json!({ "repo_path": root, "targets": ["probe_caller"], "depth": 2, "min_confidence": 0.5 }),
        )
        .expect("neighbors answers");
        assert_eq!(
            callee_symbols(&out["items"][0]),
            ["probe_callee"],
            "the 0.3 edge is below min_confidence: {out}"
        );
    }

    #[test]
    fn neighbors_reports_a_failed_walk_as_unavailable_not_an_error() {
        let out = handle(
            "devmap_neighbors",
            &json!({ "repo_path": "/no/such/repo", "targets": ["x"] }),
        )
        .expect("an unavailable envelope, not a protocol error");
        assert_eq!(out["available"], false, "{out}");
        assert!(out["reason"].is_string(), "{out}");
    }

    /// A handler that reads an argument its `inputSchema` does not declare is
    /// unreachable for a validating client (`additionalProperties: false`), and
    /// one that never reads a declared argument advertises a control that does
    /// nothing. `devmap_neighbors` did both (`min_rung` read undeclared, `depth`
    /// and `min_confidence` declared and ignored).
    #[test]
    fn every_argument_a_handler_reads_is_declared_in_its_input_schema() {
        let source = include_str!("devmap_parity.rs");
        let body = &source[source.find("pub fn handle(").expect("handle")..];
        let body = &body[..body.find("#[cfg(test)]").expect("tests follow handle")];
        let read = |arm: &str| -> std::collections::BTreeSet<String> {
            let mut keys = std::collections::BTreeSet::new();
            let mut rest = arm;
            while let Some(at) = rest.find("arguments[\"") {
                rest = &rest[at + "arguments[\"".len()..];
                keys.insert(rest[..rest.find('"').unwrap()].to_string());
            }
            keys
        };
        // The shared prologue reads `repo_path` for every arm; `budget` counts
        // only for an arm that passes it on.
        let (prologue, arms) = body.split_once("match name {").expect("dispatch");
        let shared = read(prologue);
        let shared: std::collections::BTreeSet<String> =
            shared.into_iter().filter(|key| key != "budget").collect();
        for definition in tool_definitions() {
            let name = definition["name"].as_str().unwrap();
            let marker = format!("\"{name}\" =>");
            let start = arms
                .find(&marker)
                .unwrap_or_else(|| panic!("no arm for {name}"));
            let arm = &arms[start + marker.len()..];
            let end = arm.find("\n        \"devmap_").unwrap_or(arm.len());
            let mut reads: std::collections::BTreeSet<String> =
                read(&arm[..end]).union(&shared).cloned().collect();
            if arm[..end].contains("budget") {
                reads.insert("budget".to_string());
            }
            let declared: std::collections::BTreeSet<String> = definition["inputSchema"]
                ["properties"]
                .as_object()
                .unwrap()
                .keys()
                .cloned()
                .collect();
            let undeclared: Vec<_> = reads.difference(&declared).collect();
            assert!(
                undeclared.is_empty(),
                "{name} reads undeclared {undeclared:?}"
            );
        }
    }
}
