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
//! many repositories — unlike the global server which resolves the root from
//! MCP `roots/list`.

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
    let mut prop = budget_prop();
    prop["default"] = json!(2000);
    prop
}

fn depth_prop() -> Value {
    json!({
        "type": "integer",
        "description": "Maximum traversal depth. A walk stopped by this cap sets walk_incomplete.",
        "minimum": 1,
        "maximum": 64,
        "default": 3
    })
}

fn confidence_prop() -> Value {
    json!({
        "type": "number",
        "description": "Minimum edge confidence in [0,1]",
        "minimum": 0.0,
        "maximum": 1.0,
        "default": 0.0
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
                "depth": depth_prop()
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
            json!({ "type": "object" }),
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
                    "minimum": 1,
                    "maximum": 100,
                    "default": 20
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
            let min_rung = arguments["min_rung"].as_str().map(str::to_string);
            Ok(json!(crate::codeintel::neighbors(
                repo,
                &targets,
                budget,
                min_rung.as_deref()
            )?))
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
}
