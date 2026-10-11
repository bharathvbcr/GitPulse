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

use crate::codeintel::WalkBounds;

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

/// The `CodeintelResponse<T>` envelope an agent reads before treating an empty
/// list as "nothing exists": freshness, availability, and what was withheld.
/// `item` describes one element of `items`.
fn envelope_schema(item: Value) -> Value {
    json!({
        "type": "object",
        "properties": {
            "source_freshness": { "type": "object" },
            "available": { "type": "boolean" },
            "reason": { "type": ["string", "null"] },
            "items": { "type": "array", "items": item },
            "total": { "type": "integer", "minimum": 0 },
            "shown": { "type": "integer", "minimum": 0 },
            "truncated": { "type": "boolean" },
            "walk_incomplete": { "type": "string" },
            "rungs": { "type": "object" }
        },
        "required": ["source_freshness", "available", "items", "total", "shown", "truncated"]
    })
}

fn edge_schema() -> Value {
    json!({
        "type": "object",
        "properties": {
            "source_file": { "type": "string" },
            "target_file": { "type": "string" },
            "source_symbol": { "type": "string" },
            "target_symbol": { "type": "string" },
            "confidence": { "type": "number" }
        },
        "required": ["source_file", "target_file", "source_symbol", "target_symbol", "confidence"]
    })
}

/// The `available` / `reason` pair every composite answer leads with, plus the
/// fields it requires beyond them.
fn composite_schema(mut properties: Value, required: &[&str]) -> Value {
    let base = properties.as_object_mut().expect("properties object");
    base.insert("available".into(), json!({ "type": "boolean" }));
    base.insert("reason".into(), json!({ "type": ["string", "null"] }));
    let mut required: Vec<&str> = required.to_vec();
    required.push("available");
    json!({ "type": "object", "properties": properties, "required": required })
}

fn blast_radius_schema() -> Value {
    json!({
        "type": "object",
        "properties": {
            "seeds": { "type": "array", "items": { "type": "string" } },
            "unmatched_targets": { "type": "array", "items": { "type": "string" } },
            "layers": envelope_schema(json!({ "type": "object" })),
            "total_impacted": { "type": "integer", "minimum": 0 }
        },
        "required": ["seeds", "unmatched_targets", "layers", "total_impacted"]
    })
}

/// `devmap_neighbors` answers `CodeintelResponse<CodeintelNeighbors>`;
/// `items[].callers` / `.callees` are themselves edge envelopes.
fn neighbors_output_schema() -> Value {
    envelope_schema(json!({
        "type": "object",
        "properties": {
            "target": { "type": "string" },
            "callers": envelope_schema(edge_schema()),
            "callees": envelope_schema(edge_schema())
        },
        "required": ["target", "callers", "callees"]
    }))
}

fn status_output_schema() -> Value {
    json!({
        "type": "object",
        "properties": {
            "available": { "type": "boolean" },
            "db_path": { "type": "string" },
            "reason": { "type": ["string", "null"] }
        },
        "required": ["available", "db_path"]
    })
}

fn search_output_schema() -> Value {
    envelope_schema(json!({
        "type": "object",
        "properties": {
            "symbol_name": { "type": "string" },
            "file_path": { "type": "string" },
            "kind": { "type": "string" },
            "span_start_line": { "type": "integer" },
            "span_end_line": { "type": "integer" },
            "score": { "type": "number" }
        },
        "required": ["symbol_name", "file_path", "kind", "score"]
    }))
}

fn dead_symbols_output_schema() -> Value {
    envelope_schema(json!({
        "type": "object",
        "properties": {
            "symbol_name": { "type": "string" },
            "file_path": { "type": "string" },
            "confidence": { "type": "number" },
            "is_exempt": { "type": "boolean" },
            "exemption_reason": { "type": ["string", "null"] }
        },
        "required": ["symbol_name", "file_path", "confidence", "is_exempt"]
    }))
}

fn clones_output_schema() -> Value {
    composite_schema(
        json!({
            "groups": envelope_schema(json!({
                "type": "object",
                "properties": {
                    "size": { "type": "integer" },
                    "members": { "type": "array", "items": { "type": "string" } }
                },
                "required": ["size", "members"]
            })),
            "signed_symbols": { "type": "integer", "minimum": 0 },
            "unsigned_symbols": { "type": "integer", "minimum": 0 }
        }),
        &["groups", "signed_symbols", "unsigned_symbols"],
    )
}

fn preview_output_schema() -> Value {
    composite_schema(
        json!({
            "file_path": { "type": "string" },
            "delta_available": { "type": "boolean" }
        }),
        &["file_path", "delta_available"],
    )
}

fn explore_output_schema() -> Value {
    composite_schema(
        json!({
            "definitions": envelope_schema(json!({
                "type": "object",
                "properties": {
                    "symbol_name": { "type": "string" },
                    "file_path": { "type": "string" },
                    "kind": { "type": "string" },
                    "id": { "type": "string" }
                },
                "required": ["symbol_name", "file_path", "kind", "id"]
            })),
            "blast_radius": blast_radius_schema(),
            "limit": { "type": "integer", "minimum": 0 }
        }),
        &["definitions", "blast_radius", "limit"],
    )
}

fn affected_tests_output_schema() -> Value {
    composite_schema(
        json!({
            "targets": { "type": "array", "items": { "type": "string" } },
            "tests": envelope_schema(json!({
                "type": "object",
                "properties": {
                    "path": { "type": "string" },
                    "depth": { "type": "integer", "minimum": 0 },
                    "symbols": { "type": "array", "items": { "type": "string" } },
                    "reached_symbols": { "type": "integer", "minimum": 0 }
                },
                "required": ["path", "depth", "symbols", "reached_symbols"]
            })),
            "blast_radius": blast_radius_schema(),
            "fail_closed": { "type": "boolean" },
            "fail_closed_reason": { "type": ["string", "null"] }
        }),
        &["targets", "tests", "blast_radius", "fail_closed"],
    )
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
            status_output_schema(),
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
            search_output_schema(),
        ),
        tool(
            "devmap_dependencies",
            "DevMap dependencies",
            "Outgoing edges from a file in the code graph. These are the file's direct edges, so there is no depth to bound.",
            json!({
                "repo_path": repo_prop(),
                "target": path_prop("File or symbol whose dependencies to read"),
                "budget": budget_default(),
                "min_confidence": confidence_prop(),
                "min_rung": path_prop("Optional minimum resolution rung")
            }),
            &["repo_path", "target"],
            envelope_schema(edge_schema()),
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
                "min_confidence": confidence_prop(),
                "min_rung": path_prop("Optional minimum resolution rung")
            }),
            &["repo_path", "target"],
            envelope_schema(edge_schema()),
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
                "min_confidence": confidence_prop(),
                "min_rung": path_prop("Optional minimum resolution rung")
            }),
            &["repo_path", "from", "to"],
            envelope_schema(edge_schema()),
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
            dead_symbols_output_schema(),
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
            clones_output_schema(),
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
            preview_output_schema(),
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
                "depth": depth_prop(),
                "min_confidence": confidence_prop()
            }),
            &["repo_path", "query"],
            explore_output_schema(),
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
            affected_tests_output_schema(),
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

/// `depth` and `min_confidence`, as the caller sent them. A present value of
/// the wrong type is refused here; a number out of range is refused by
/// [`WalkBounds`], so neither is silently replaced by the default.
fn walk_bounds(arguments: &Value) -> Result<WalkBounds, String> {
    let max_depth = match &arguments["depth"] {
        Value::Null => None,
        v => Some(
            v.as_u64()
                .and_then(|d| usize::try_from(d).ok())
                .ok_or("depth must be an integer")?,
        ),
    };
    let min_confidence = match &arguments["min_confidence"] {
        Value::Null => None,
        v => Some(v.as_f64().ok_or("min_confidence must be a number")? as f32),
    };
    Ok(WalkBounds {
        max_depth,
        min_confidence,
    })
}

/// Only `min_confidence`: for a query that reads one file's direct edges and
/// has no depth to bound (`devmap_dependencies`).
fn confidence_bound(arguments: &Value) -> Result<WalkBounds, String> {
    Ok(WalkBounds {
        max_depth: None,
        min_confidence: walk_bounds(&json!({ "min_confidence": arguments["min_confidence"] }))?
            .min_confidence,
    })
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
                repo,
                target,
                budget,
                min_rung,
                confidence_bound(arguments)?,
                None
            )))
        }
        "devmap_impact" => {
            let target = arguments["target"].as_str().ok_or("missing target")?;
            let min_rung = arguments["min_rung"].as_str();
            Ok(json!(crate::codeintel::impact_at_rung(
                repo,
                target,
                budget,
                min_rung,
                walk_bounds(arguments)?,
                None
            )))
        }
        "devmap_trace" => {
            let from = arguments["from"].as_str().ok_or("missing from")?;
            let to = arguments["to"].as_str().ok_or("missing to")?;
            let min_rung = arguments["min_rung"].as_str();
            Ok(json!(crate::codeintel::trace_between_at_rung(
                repo,
                from,
                to,
                budget,
                min_rung,
                walk_bounds(arguments)?,
                None
            )))
        }
        "devmap_neighbors" => {
            let targets = string_args(arguments, &["targets"])?;
            Ok(json!(crate::codeintel::neighbors_envelope(
                crate::codeintel::neighbors(repo, &targets, budget, None, walk_bounds(arguments)?)
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
            Ok(json!(crate::codeintel::explore(
                repo,
                query,
                budget,
                limit,
                walk_bounds(arguments)?
            )))
        }
        "devmap_affected_tests" => {
            let targets = string_args(arguments, &["targets"])?;
            Ok(json!(crate::codeintel::affected_tests(
                repo,
                &targets,
                budget,
                walk_bounds(arguments)?
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
        conn.execute(
            "INSERT INTO generation_nodes
               (generation_id, ordinal, file_id, name, qualified_name, kind,
                span_start, span_end, is_exported)
             VALUES (1, 2, 3, 'probe_island', 'probe_island', 'Function', 0, 20, 1)",
            [],
        )
        .expect("insert island node");
        let fts_rowid = (1i64 << 32) | 2;
        conn.execute(
            "INSERT INTO nodes_fts (rowid, name, qualified_name, path)
             VALUES (?1, 'probe_island', 'probe_island', 'src/island.rs')",
            [fts_rowid],
        )
        .expect("insert island fts row");
        conn.execute(
            "INSERT INTO nodes_fts_map (rowid_ref, generation_id) VALUES (?1, 1)",
            [fts_rowid],
        )
        .expect("insert island fts map row");
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

    fn edge_symbols(out: &Value) -> Vec<(String, String)> {
        out["items"]
            .as_array()
            .unwrap_or_else(|| panic!("no items: {out}"))
            .iter()
            .map(|e| {
                (
                    e["source_symbol"].as_str().unwrap().to_string(),
                    e["target_symbol"].as_str().unwrap().to_string(),
                )
            })
            .collect()
    }

    fn ask(name: &str, arguments: Value) -> Value {
        handle(name, &arguments).unwrap_or_else(|e| panic!("{name} errored: {e}"))
    }

    #[test]
    fn impact_honours_depth_and_min_confidence() {
        let (_dir, root) = two_hop_repo();
        let at = |extra: Value| {
            let mut args = json!({ "repo_path": root, "target": "probe_island" });
            args.as_object_mut()
                .unwrap()
                .extend(extra.as_object().unwrap().clone());
            edge_symbols(&ask("devmap_impact", args))
        };
        let callee_hop = ("probe_callee".to_string(), "probe_island".to_string());
        let caller_hop = ("probe_caller".to_string(), "probe_callee".to_string());
        assert_eq!(
            at(json!({ "depth": 1 })),
            std::slice::from_ref(&callee_hop),
            "depth 1 stops at the direct caller"
        );
        assert!(
            at(json!({ "depth": 2 })).contains(&caller_hop),
            "depth 2 reaches the second caller"
        );
        assert!(
            !at(json!({ "depth": 2, "min_confidence": 0.5 })).contains(&callee_hop),
            "the 0.3 edge is below min_confidence"
        );
    }

    #[test]
    fn dependencies_honour_min_confidence() {
        let (_dir, root) = two_hop_repo();
        let edges = |min_confidence: f64| {
            edge_symbols(&ask(
                "devmap_dependencies",
                json!({ "repo_path": root, "target": "src/caller.rs", "min_confidence": min_confidence }),
            ))
        };
        assert_eq!(
            edges(0.5),
            [("probe_caller".to_string(), "probe_callee".to_string())]
        );
        assert!(
            edges(0.95).is_empty(),
            "the 0.9 edge is below min_confidence"
        );
        let refused = ask(
            "devmap_dependencies",
            json!({ "repo_path": root, "target": "src/caller.rs", "min_confidence": 2 }),
        );
        assert_eq!(refused["available"], false, "{refused}");
    }

    #[test]
    fn trace_honours_depth_and_min_confidence() {
        let (_dir, root) = two_hop_repo();
        let trace = |extra: Value| {
            let mut args =
                json!({ "repo_path": root, "from": "probe_caller", "to": "probe_island" });
            args.as_object_mut()
                .unwrap()
                .extend(extra.as_object().unwrap().clone());
            edge_symbols(&ask("devmap_trace", args)).len()
        };
        assert_eq!(trace(json!({ "depth": 1 })), 0, "no path within one hop");
        assert_eq!(
            trace(json!({ "depth": 2 })),
            2,
            "a two-edge path within two hops"
        );
        assert_eq!(
            trace(json!({ "depth": 2, "min_confidence": 0.5 })),
            0,
            "the path needs the 0.3 edge"
        );
    }

    #[test]
    fn explore_honours_depth_and_min_confidence() {
        let (_dir, root) = two_hop_repo();
        let reach = |extra: Value| {
            let mut args = json!({ "repo_path": root, "query": "probe_island" });
            args.as_object_mut()
                .unwrap()
                .extend(extra.as_object().unwrap().clone());
            ask("devmap_explore", args)["blast_radius"]["layers"]["items"]
                .as_array()
                .expect("layers")
                .iter()
                .map(|l| l["depth"].as_u64().unwrap())
                .max()
                .unwrap_or(0)
        };
        assert_eq!(
            reach(json!({ "depth": 1 })),
            1,
            "depth 1 yields no depth-2 layer"
        );
        assert_eq!(
            reach(json!({ "depth": 2 })),
            2,
            "depth 2 yields the second layer"
        );
        assert_eq!(
            reach(json!({ "depth": 2, "min_confidence": 0.5 })),
            0,
            "the only edge into probe_island is 0.3"
        );
    }

    #[test]
    fn out_of_range_bounds_are_refused_not_clamped() {
        let (_dir, root) = two_hop_repo();
        for (name, base) in [
            ("devmap_impact", json!({ "target": "probe_island" })),
            ("devmap_dependencies", json!({ "target": "src/caller.rs" })),
            (
                "devmap_trace",
                json!({ "from": "probe_caller", "to": "probe_island" }),
            ),
            ("devmap_explore", json!({ "query": "probe_island" })),
            (
                "devmap_affected_tests",
                json!({ "targets": ["probe_island"] }),
            ),
            ("devmap_neighbors", json!({ "targets": ["probe_island"] })),
        ] {
            for bad in [
                json!({ "depth": 0 }),
                json!({ "depth": 65 }),
                json!({ "min_confidence": 1.5 }),
                json!({ "min_confidence": -0.1 }),
            ] {
                let key = bad.as_object().unwrap().keys().next().unwrap();
                let declared = tool_definitions().into_iter().any(|t| {
                    t["name"] == name && t["inputSchema"]["properties"][key.as_str()].is_object()
                });
                if !declared {
                    continue;
                }
                let mut args = json!({ "repo_path": root });
                args.as_object_mut()
                    .unwrap()
                    .extend(base.as_object().unwrap().clone());
                args.as_object_mut()
                    .unwrap()
                    .extend(bad.as_object().unwrap().clone());
                let out = ask(name, args);
                assert_eq!(
                    out["available"], false,
                    "{name} {bad} must be refused: {out}"
                );
            }
        }
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
    /// and `min_confidence` declared and ignored), and so did every other tool
    /// that walks edges. Both directions are held here, per tool.
    #[test]
    fn handlers_read_exactly_the_arguments_their_input_schema_declares() {
        let source = include_str!("devmap_parity.rs");
        let body = &source[source.find("pub fn handle(").expect("handle")..];
        let body = &body[..body.find("#[cfg(test)]").expect("tests follow handle")];
        // What one arm of `handle` reads: literal `arguments["k"]` lookups, the
        // bounds helpers (which read `depth` / `min_confidence`), and `budget`
        // when the arm passes the shared prologue's value on.
        let reads = |arm: &str| -> std::collections::BTreeSet<String> {
            let mut keys = std::collections::BTreeSet::new();
            let mut rest = arm;
            while let Some(at) = rest.find("arguments[\"") {
                rest = &rest[at + "arguments[\"".len()..];
                keys.insert(rest[..rest.find('"').unwrap()].to_string());
            }
            // `string_args(arguments, &["targets"])` reads the named array.
            let mut rest = arm;
            while let Some(at) = rest.find("string_args(arguments, &[\"") {
                rest = &rest[at + "string_args(arguments, &[\"".len()..];
                keys.insert(rest[..rest.find('"').unwrap()].to_string());
            }
            if arm.contains("walk_bounds(") {
                keys.extend(["depth".to_string(), "min_confidence".to_string()]);
            }
            if arm.contains("confidence_bound(") {
                keys.insert("min_confidence".to_string());
            }
            if arm.contains("budget") {
                keys.insert("budget".to_string());
            }
            keys
        };
        let (_prologue, arms) = body.split_once("match name {").expect("dispatch");
        for definition in tool_definitions() {
            let name = definition["name"].as_str().unwrap();
            let marker = format!("\"{name}\" =>");
            let start = arms
                .find(&marker)
                .unwrap_or_else(|| panic!("no arm for {name}"));
            let arm = &arms[start + marker.len()..];
            let end = arm.find("\n        \"devmap_").unwrap_or(arm.len());
            let mut read = reads(&arm[..end]);
            read.insert("repo_path".to_string());
            let declared: std::collections::BTreeSet<String> = definition["inputSchema"]
                ["properties"]
                .as_object()
                .unwrap()
                .keys()
                .cloned()
                .collect();
            let undeclared: Vec<_> = read.difference(&declared).collect();
            assert!(
                undeclared.is_empty(),
                "{name} reads undeclared {undeclared:?}"
            );
            let ignored: Vec<_> = declared.difference(&read).collect();
            assert!(
                ignored.is_empty(),
                "{name} declares but ignores {ignored:?}"
            );
        }
    }
}
