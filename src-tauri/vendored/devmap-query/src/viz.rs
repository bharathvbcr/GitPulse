//! A self-contained HTML view of the graph. No network.
//!
//! Ported from `src/devcouncil/indexing/viz.py`. The Python version read
//! `code_graph.json` back off disk to render it; this one projects the same
//! `Value` the graph writer already built, so the picture and the artifact
//! cannot describe different generations.
//!
//! # The cap is the interesting part
//!
//! A layout over 12,000 nodes is not a visualization, it is a hairball. So the view is capped — and a capped view that does not say
//! so is worse than no view, because the reader concludes the graph is small.
//!
//! Nodes are therefore ranked by degree and the *most connected* survive: a cap
//! that keeps hubs shows the shape of the graph, while a cap that keeps whatever
//! the store happened to return first shows an arbitrary sample of leaves. Both
//! counts ride in the payload and the page prints both in its header, so
//! "1,000 of 12,103 nodes" is what the reader sees, never "1,000 nodes".

use serde_json::{json, Map, Value};
use std::collections::BTreeMap;

/// Visibility classification shared by file/symbol and subsystem payloads.
/// Directory names alone do not make source code into a note.
pub(crate) fn is_documentation(node: &Value) -> bool {
    let kind = node["kind"]
        .as_str()
        .unwrap_or("")
        .trim()
        .to_ascii_lowercase();
    let language = node["language"]
        .as_str()
        .unwrap_or("")
        .trim()
        .to_ascii_lowercase();
    if matches!(
        kind.as_str(),
        "doc" | "document" | "documentation" | "note" | "notes"
    ) || matches!(
        language.as_str(),
        "markdown" | "md" | "mdx" | "rst" | "restructuredtext" | "asciidoc"
    ) {
        return true;
    }
    let path = node["path"]
        .as_str()
        .filter(|p| !p.is_empty())
        .or_else(|| node["id"].as_str())
        .unwrap_or("");
    let path = path
        .split("::")
        .next()
        .unwrap_or(path)
        .replace('\\', "/")
        .to_ascii_lowercase();
    let name = path.rsplit('/').next().unwrap_or("");
    matches!(name, "notes" | "notes.txt" | "note.txt")
        || matches!(
            name.rsplit_once('.').map(|(_, ext)| ext),
            Some(
                "md" | "mdx" | "markdown" | "mdown" | "mkd" | "mkdn" | "rst" | "adoc" | "asciidoc"
            )
        )
}

/// Edge kinds that mean one *file* depends on another.
const FILE_EDGE_KINDS: &[&str] = &["imports"];

/// Edge kinds that mean one *symbol* reaches another.
const SYMBOL_EDGE_KINDS: &[&str] = &[
    "calls",
    "references",
    "inherits",
    "implements",
    "overrides",
    "named_import",
];

#[derive(Debug, Clone)]
pub struct VizOptions {
    /// Symbol-level rather than file-level.
    pub symbols: bool,
    /// Most nodes to draw. See the module docs: this is a *ranked* cap, and
    /// what it cut is always reported.
    pub max_nodes: usize,
    pub title: String,
}

impl Default for VizOptions {
    fn default() -> Self {
        Self {
            symbols: false,
            // Measured rather than guessed: past roughly this many nodes the
            // layout stops converging in a browser tab and the page reads as
            // hung. A reader who wants more can raise it and wait.
            max_nodes: 1_500,
            title: "Dev Map".to_string(),
        }
    }
}

fn strings(graph: &Value, key: &str) -> std::collections::BTreeSet<String> {
    graph
        .get(key)
        .and_then(Value::as_array)
        .map(|items| {
            items
                .iter()
                .filter_map(Value::as_str)
                .map(str::to_string)
                .collect()
        })
        .unwrap_or_default()
}

/// Build the drawing payload: the nodes and links to render, plus what was cut.
pub fn build_payload(graph: &Value, options: &VizOptions) -> Value {
    let empty = Vec::new();
    let all_nodes = graph
        .get("nodes")
        .and_then(Value::as_array)
        .unwrap_or(&empty);
    let all_edges = graph
        .get("edges")
        .and_then(Value::as_array)
        .unwrap_or(&empty);

    let dead: std::collections::BTreeMap<&str, &str> = graph
        .get("dead_code")
        .and_then(Value::as_array)
        .map(|rows| {
            rows.iter()
                .filter_map(|row| {
                    Some((
                        row.get("id")?.as_str()?,
                        row.get("confidence").and_then(Value::as_str).unwrap_or(""),
                    ))
                })
                .collect()
        })
        .unwrap_or_default();
    let unwired = strings(graph, "unwired_candidates");
    let entry_roots = strings(graph, "entry_roots");

    // IDs are opaque. Reject ambiguous identities instead of picking whichever
    // duplicate happened to arrive last, and resolve symbol ownership by path.
    let mut node_ids: BTreeMap<&str, Option<&Value>> = BTreeMap::new();
    let mut invalid_nodes = 0usize;
    for node in all_nodes {
        let Some(id) = node
            .get("id")
            .and_then(Value::as_str)
            .filter(|id| !id.is_empty())
        else {
            invalid_nodes += 1;
            continue;
        };
        if node
            .get("kind")
            .and_then(Value::as_str)
            .is_none_or(str::is_empty)
        {
            invalid_nodes += 1;
            continue;
        }
        match node_ids.entry(id) {
            std::collections::btree_map::Entry::Vacant(entry) => {
                entry.insert(Some(node));
            }
            std::collections::btree_map::Entry::Occupied(mut entry) => {
                invalid_nodes += 1 + usize::from(entry.get().is_some());
                entry.insert(None);
            }
        }
    }
    let node_ids: BTreeMap<&str, &Value> = node_ids
        .into_iter()
        .filter_map(|(id, node)| node.map(|node| (id, node)))
        .collect();
    let mut files: BTreeMap<&str, Option<&str>> = BTreeMap::new();
    for (&id, &node) in &node_ids {
        if node["kind"] == "file" {
            let path = node
                .get("path")
                .and_then(Value::as_str)
                .filter(|p| !p.is_empty())
                .unwrap_or(id);
            files
                .entry(path)
                .and_modify(|owner| *owner = None)
                .or_insert(Some(id));
        }
    }
    let owner = |id: &str| -> Option<&str> {
        let node = node_ids.get(id)?;
        if node["kind"] == "file" {
            Some(node.get("id")?.as_str()?)
        } else {
            files.get(node.get("path")?.as_str()?).copied().flatten()
        }
    };
    let in_level = |node: &&Value| (node["kind"] != "file") == options.symbols;

    // Merge duplicate source evidence deterministically before projecting it.
    // Unknown confidence stays unknown; mixed resolutions are never promoted
    // to an exact match. Distinct symbol edges contribute once to each link.
    #[derive(Clone)]
    struct Evidence<'a> {
        confidence: Option<f64>,
        resolution: &'a str,
        count: usize,
    }
    let merge = |previous: &mut Evidence<'_>, next: &Evidence<'_>| {
        previous.confidence = previous
            .confidence
            .zip(next.confidence)
            .map(|(a, b)| a.min(b));
        if previous.resolution != next.resolution {
            previous.resolution = "mixed";
        }
    };
    let mut raw: BTreeMap<(&str, &str, &str), Evidence<'_>> = BTreeMap::new();
    let mut invalid_edges = 0usize;
    let mut duplicate_edges = 0usize;
    for edge in all_edges {
        let Some(kind) = edge.get("kind").and_then(Value::as_str) else {
            invalid_edges += 1;
            continue;
        };
        if !SYMBOL_EDGE_KINDS.contains(&kind)
            && (options.symbols || !FILE_EDGE_KINDS.contains(&kind))
        {
            continue;
        }
        let (Some(source), Some(target)) = (
            edge.get("source").and_then(Value::as_str),
            edge.get("target").and_then(Value::as_str),
        ) else {
            invalid_edges += 1;
            continue;
        };
        let (Some(a), Some(b)) = (node_ids.get(source), node_ids.get(target)) else {
            invalid_edges += 1;
            continue;
        };
        if options.symbols && (a["kind"] == "file" || b["kind"] == "file") {
            continue;
        }
        let evidence = Evidence {
            confidence: edge
                .get("confidence")
                .and_then(Value::as_f64)
                .filter(|v| v.is_finite() && (0.0..=1.0).contains(v)),
            resolution: edge
                .get("resolution")
                .and_then(Value::as_str)
                .unwrap_or("unknown"),
            count: 1,
        };
        match raw.entry((source, target, kind)) {
            std::collections::btree_map::Entry::Vacant(entry) => {
                entry.insert(evidence);
            }
            std::collections::btree_map::Entry::Occupied(mut entry) => {
                merge(entry.get_mut(), &evidence);
                duplicate_edges += 1;
            }
        }
    }
    let mut projected: BTreeMap<(&str, &str, &str), Evidence<'_>> = BTreeMap::new();
    let mut internal_edges = 0usize;
    for ((source, target, kind), evidence) in raw {
        let (source, target) = if options.symbols {
            (source, target)
        } else {
            let (Some(a), Some(b)) = (owner(source), owner(target)) else {
                invalid_edges += 1;
                continue;
            };
            if a == b {
                internal_edges += 1;
                continue;
            }
            (a, b)
        };
        match projected.entry((source, target, kind)) {
            std::collections::btree_map::Entry::Vacant(entry) => {
                entry.insert(evidence);
            }
            std::collections::btree_map::Entry::Occupied(mut entry) => {
                let previous = entry.get_mut();
                merge(previous, &evidence);
                previous.count += evidence.count;
            }
        }
    }
    let mut degree: BTreeMap<&str, usize> = BTreeMap::new();
    for &(source, target, _) in projected.keys() {
        *degree.entry(source).or_default() += 1;
        *degree.entry(target).or_default() += 1;
    }
    let mut candidates: Vec<&Value> = node_ids.values().copied().filter(in_level).collect();
    let total_nodes = candidates.len();
    // Most-connected first, then by id so the choice is deterministic across
    // runs (R4) rather than depending on map iteration.
    candidates.sort_by(|a, b| {
        let id_a = a.get("id").and_then(Value::as_str).unwrap_or("");
        let id_b = b.get("id").and_then(Value::as_str).unwrap_or("");
        degree
            .get(id_b)
            .unwrap_or(&0)
            .cmp(degree.get(id_a).unwrap_or(&0))
            .then_with(|| id_a.cmp(id_b))
    });
    candidates.truncate(options.max_nodes);

    let shown: std::collections::BTreeSet<&str> = candidates
        .iter()
        .filter_map(|node| node.get("id").and_then(Value::as_str))
        .collect();

    let nodes: Vec<Value> = candidates
        .iter()
        .map(|node| {
            let id = node.get("id").and_then(Value::as_str).unwrap_or("");
            let path = node.get("path").and_then(Value::as_str).unwrap_or("");
            let mut flags = Vec::new();
            if let Some(confidence) = dead.get(id) {
                flags.push(json!({"flag": "dead", "confidence": confidence}));
            }
            if unwired.contains(id) || unwired.contains(path) {
                flags.push(json!({"flag": "unwired"}));
            }
            if entry_roots.contains(path) || entry_roots.contains(id) {
                flags.push(json!({"flag": "entry"}));
            }
            // The producer's own three-valued answer, carried through rather
            // than re-derived from the flag lists. An isolated `.yaml` has no
            // flags and no edges, and a reader told this picture shows dead and
            // unwired code reads a lone dot as one of those. `""` for a symbol
            // node, which the detail panel's `row` helper then omits — a symbol
            // has no file-level liveness and must not be shown a blank one.
            let extras = node.get("extras");
            let liveness = extras
                .and_then(|extras| extras.get("liveness"))
                .and_then(Value::as_str)
                .unwrap_or("");
            let liveness_reason = extras
                .and_then(|extras| extras.get("liveness_reason"))
                .and_then(Value::as_str)
                .unwrap_or("");
            json!({
                "id": id,
                "name": node.get("name").and_then(Value::as_str).unwrap_or(id),
                "kind": node.get("kind").and_then(Value::as_str).unwrap_or(""),
                "path": path,
                "area": node.get("area").and_then(Value::as_str).unwrap_or(""),
                "community": node.get("community").and_then(Value::as_str).unwrap_or(""),
                "language": node.get("language").and_then(Value::as_str).unwrap_or(""),
                "liveness": liveness,
                "liveness_reason": liveness_reason,
                "documentation": is_documentation(node),
                "line": node.get("line").and_then(Value::as_u64).unwrap_or(0),
                "degree": degree.get(id).copied().unwrap_or(0),
                "flags": flags,
            })
        })
        .collect();

    // Only edges whose *both* ends survived the cap. An edge to a node that is
    // not drawn is a line into empty space, and a renderer that binds endpoints
    // by id would invent a phantom node for it — a node the graph does not
    // contain.
    let total_links = projected.len();
    // A node cap alone cannot bound a dense graph. Keep the strongest visible
    // relationships with a deterministic tie break and report the full total.
    const MAX_LINKS: usize = 50_000;
    let mut visible: Vec<_> = projected
        .into_iter()
        .filter(|((a, b, _), _)| shown.contains(a) && shown.contains(b))
        .collect();
    visible.sort_by(|(a_key, a), (b_key, b)| b.count.cmp(&a.count).then_with(|| a_key.cmp(b_key)));
    visible.truncate(MAX_LINKS);
    let links: Vec<Value> = visible
        .into_iter()
        .map(|((source, target, kind), evidence)| {
            json!({
                "source": source, "target": target, "kind": kind,
                "confidence": evidence.confidence, "resolution": evidence.resolution,
                "evidence_count": evidence.count,
            })
        })
        .collect();

    let mut communities: Map<String, Value> = Map::new();
    for node in &nodes {
        if let Some(name) = node.get("community").and_then(Value::as_str) {
            if name.is_empty() {
                continue;
            }
            let entry = communities.entry(name.to_string()).or_insert(json!(0));
            *entry = json!(entry.as_u64().unwrap_or(0) + 1);
        }
    }

    json!({
        "level": if options.symbols { "symbol" } else { "file" },
        "nodes": nodes,
        "links": links,
        // Both numbers, always. The whole reason the cap is safe to have.
        "counts": {
            "nodes_shown": nodes.len(),
            "nodes_total": total_nodes,
            "nodes_truncated": total_nodes > nodes.len(),
            "links_shown": links.len(),
            "links_total": total_links,
            "max_nodes": options.max_nodes,
        },
        "communities": communities,
        "meta": {"projection": {
            "scope": if options.symbols { "symbol relationships" } else { "cross-file dependencies" },
            "invalid_nodes": invalid_nodes, "invalid_edges": invalid_edges,
            "internal_edges_omitted": internal_edges, "duplicate_edges_merged": duplicate_edges,
            "max_links": MAX_LINKS,
        }},
        "generation_id": graph.pointer("/meta/generation_id").cloned().unwrap_or(Value::Null),
    })
}

/// Render the code graph: one HTML file, no network.
pub fn render_html(graph: &Value, options: &VizOptions) -> String {
    let mut payload = build_payload(graph, options);
    let truncated = payload["counts"]["nodes_truncated"]
        .as_bool()
        .unwrap_or(false);

    // The page states the cap itself, from `counts`: "1,000 of 12,103 nodes"
    // and that the view is not the whole graph. A reader who never opens
    // devtools must still be unable to mistake a capped view for the whole
    // graph, so the one thing the page cannot derive from the counts is what
    // to do about it, and that rides here.
    payload["view"] = json!({
        "title": options.title,
        "level": if options.symbols { "symbols" } else { "files" },
        "suffix": "code graph",
        "cap_hint": if truncated {
            "Nodes are ranked by connections, most connected first; raise --max-nodes to widen the view."
        } else {
            ""
        },
        "detail_fields": [
            ["Path", "path"], ["Kind", "kind"], ["Area", "area"],
            ["Community", "community"], ["Language", "language"],
            ["Liveness", "liveness"], ["Why", "liveness_reason"],
        ],
        "flag_filters": [["dead", "Dead candidates only"]],
    });
    // One shell, driven by `payload.view`: this view and the repo map differ in
    // what a node *is*, not in how the page works.
    crate::page::render_shell(&format!("{} — code graph", options.title), None, &payload)
}

/// What a page's script reads off its payload that the payload does not carry.
///
/// A page that reads a key no producer emits renders nothing for it, and
/// "nothing to report" is indistinguishable from "never computed" — the dead
/// `unresolved_handoffs_total` note was exactly that. Each `(name, value)` root
/// is a script variable bound to a JSON value (`DATA`, or `META` for
/// `DATA.meta`); every `name.a.b` chain is walked through the value, and a
/// step that names a key an object lacks is reported as `name.a.b`. A step
/// into a non-object (an array's `.length`, a string's method) ends the walk.
#[cfg(test)]
pub(crate) fn page_reads_missing_from(
    page: &str,
    roots: &[(&str, &Value)],
) -> std::collections::BTreeSet<String> {
    let is_ident = |c: char| c.is_ascii_alphanumeric() || c == '_' || c == '$';
    let mut missing = std::collections::BTreeSet::new();
    for (name, root) in roots {
        let needle = format!("{name}.");
        let mut from = 0;
        while let Some(found) = page[from..].find(&needle) {
            let start = from + found;
            from = start + needle.len();
            if page[..start]
                .chars()
                .next_back()
                .is_some_and(|c| is_ident(c) || c == '.')
            {
                continue;
            }
            let mut path = (*name).to_string();
            let mut current = *root;
            let mut rest = &page[from..];
            loop {
                let len = rest.find(|c: char| !is_ident(c)).unwrap_or(rest.len());
                if len == 0 {
                    break;
                }
                let key = &rest[..len];
                path.push('.');
                path.push_str(key);
                let Some(object) = current.as_object() else {
                    break;
                };
                let Some(next) = object.get(key) else {
                    missing.insert(path);
                    break;
                };
                current = next;
                rest = &rest[len..];
                match rest.strip_prefix('.') {
                    Some(after) => rest = after,
                    None => break,
                }
            }
        }
    }
    missing
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The page reads only what `render_html` puts in its payload. The
    /// retired subsystem view left a note reading `unresolved_handoffs_total`
    /// and a detail pane reading `subsystems` here, neither of which the code
    /// graph payload has ever carried.
    ///
    /// Retargeted from the old inline script to the TypeScript that now reads
    /// the payload (`graphView.ts`, `codeGraphPage.ts`): both reach the payload
    /// through the named `DATA` root, which is what this scan can see. The
    /// payload checked is the one the page really embeds, `view` included.
    #[test]
    fn every_key_the_page_reads_is_one_the_payload_carries() {
        let page = [
            include_str!("../assets/code-map/src/page/graphView.ts"),
            include_str!("../assets/code-map/src/page/codeGraphPage.ts"),
        ]
        .join("\n");
        assert!(page.contains("DATA."), "the scan has something to scan");
        for symbols in [false, true] {
            let options = VizOptions {
                symbols,
                ..Default::default()
            };
            let payload = crate::page::embedded_payload(&render_html(&graph(), &options));
            let missing = page_reads_missing_from(&page, &[("DATA", &payload)]);
            assert!(missing.is_empty(), "the page reads {missing:?}");
        }
    }

    #[test]
    fn the_contract_check_reports_a_key_no_producer_emits() {
        let payload = json!({"nodes": [], "meta": {"a": 1}});
        let page = "DATA.nodes.length; DATA.meta.a; DATA.meta.b; DATA.gone; xDATA.no; META.c";
        let missing =
            page_reads_missing_from(page, &[("DATA", &payload), ("META", &payload["meta"])]);
        assert_eq!(
            missing.into_iter().collect::<Vec<_>>(),
            vec!["DATA.gone", "DATA.meta.b", "META.c"]
        );
    }

    fn graph() -> Value {
        json!({
            "nodes": [
                {"id": "a.py", "name": "a.py", "kind": "file", "path": "a.py", "community": "core"},
                {"id": "b.py", "name": "b.py", "kind": "file", "path": "b.py", "community": "core"},
                {"id": "c.py", "name": "c.py", "kind": "file", "path": "c.py", "community": "edge"},
                {"id": "a.py::run", "name": "run", "kind": "function", "path": "a.py", "line": 3},
                {"id": "b.py::helper", "name": "helper", "kind": "function", "path": "b.py", "line": 1}
            ],
            "edges": [
                {"source": "a.py", "target": "b.py", "kind": "imports", "confidence": 1.0},
                {"source": "a.py", "target": "c.py", "kind": "imports", "confidence": 1.0},
                {"source": "a.py::run", "target": "b.py::helper", "kind": "calls", "confidence": 0.9}
            ],
            "dead_code": [{"id": "c.py", "confidence": "extracted"}],
            "unwired_candidates": ["c.py"],
            "entry_roots": ["a.py"]
        })
    }

    #[test]
    fn the_two_levels_never_mix_their_nodes() {
        // Files and symbols in one layout draws two graphs on top of each other.
        let files = build_payload(&graph(), &VizOptions::default());
        assert!(files["nodes"]
            .as_array()
            .unwrap()
            .iter()
            .all(|n| n["kind"] == "file"));

        let symbols = build_payload(
            &graph(),
            &VizOptions {
                symbols: true,
                ..Default::default()
            },
        );
        assert!(symbols["nodes"]
            .as_array()
            .unwrap()
            .iter()
            .all(|n| n["kind"] != "file"));
        assert_eq!(symbols["links"].as_array().unwrap().len(), 1);
    }

    #[test]
    fn a_capped_view_reports_both_numbers() {
        // The claim the whole cap rests on: a reader can never mistake the
        // sample for the graph.
        let payload = build_payload(
            &graph(),
            &VizOptions {
                max_nodes: 2,
                ..Default::default()
            },
        );
        assert_eq!(payload["counts"]["nodes_shown"], json!(2));
        assert_eq!(payload["counts"]["nodes_total"], json!(3));
        assert_eq!(payload["counts"]["nodes_truncated"], json!(true));

        // The page words the cap itself, client-side, from `counts` (bun test
        // `truncationLegend` pins "2 of 3 nodes" and "not the whole graph"; the
        // browser probe pins that the page prints it). What the Rust side owes
        // the page is the numbers, the hint about what to do, and a page that
        // carries the wording at all.
        let html = render_html(
            &graph(),
            &VizOptions {
                max_nodes: 2,
                ..Default::default()
            },
        );
        let embedded = crate::page::embedded_payload(&html);
        assert_eq!(embedded["counts"]["nodes_shown"], json!(2));
        assert_eq!(embedded["counts"]["nodes_total"], json!(3));
        assert!(
            embedded["view"]["cap_hint"]
                .as_str()
                .is_some_and(|hint| hint.contains("--max-nodes")),
            "a capped view says how to widen it"
        );
        assert!(
            html.contains("This is not the whole graph"),
            "the page itself must carry the cap wording, not only the payload"
        );
    }

    #[test]
    fn an_uncapped_view_does_not_claim_truncation() {
        let payload = build_payload(&graph(), &VizOptions::default());
        assert_eq!(payload["counts"]["nodes_truncated"], json!(false));
        let embedded =
            crate::page::embedded_payload(&render_html(&graph(), &VizOptions::default()));
        assert_eq!(embedded["counts"]["nodes_total"], json!(3));
        assert_eq!(embedded["counts"]["nodes_truncated"], json!(false));
        assert_eq!(
            embedded["view"]["cap_hint"],
            json!(""),
            "nothing was cut, so there is nothing to widen"
        );
    }

    #[test]
    fn the_cap_keeps_the_hubs_not_an_arbitrary_sample() {
        // `a.py` has degree 2, the others 1. A cap of one must keep `a.py`, or
        // the picture shows leaves and hides the structure.
        let payload = build_payload(
            &graph(),
            &VizOptions {
                max_nodes: 1,
                ..Default::default()
            },
        );
        assert_eq!(payload["nodes"][0]["id"], json!("a.py"));
    }

    #[test]
    fn no_link_survives_whose_endpoint_was_cut() {
        // A renderer that binds endpoints by id invents a node for an unknown one, which would draw
        // a node the graph does not contain.
        let payload = build_payload(
            &graph(),
            &VizOptions {
                max_nodes: 1,
                ..Default::default()
            },
        );
        assert!(payload["links"].as_array().unwrap().is_empty());
        // Still reported against the true total, so the emptiness is legible.
        // Two imports plus the projected call between a.py and b.py.
        assert_eq!(payload["counts"]["links_total"], json!(3));
    }

    #[test]
    fn flags_come_from_the_graphs_own_verdicts() {
        let payload = build_payload(&graph(), &VizOptions::default());
        let by_id: std::collections::BTreeMap<&str, &Value> = payload["nodes"]
            .as_array()
            .unwrap()
            .iter()
            .map(|n| (n["id"].as_str().unwrap(), n))
            .collect();
        let flags = |id: &str| -> Vec<String> {
            by_id[id]["flags"]
                .as_array()
                .unwrap()
                .iter()
                .map(|f| f["flag"].as_str().unwrap().to_string())
                .collect()
        };
        assert_eq!(flags("a.py"), vec!["entry"]);
        let dead_flags = flags("c.py");
        assert!(dead_flags.contains(&"dead".to_string()));
        assert!(dead_flags.contains(&"unwired".to_string()));
        assert!(flags("b.py").is_empty());
    }

    #[test]
    fn a_symbol_named_like_a_closing_tag_cannot_break_out_of_the_script() {
        // `</script>` inside a JS string literal ends the block whatever JSON
        // thinks, so the rest of the graph would land in the document as markup.
        let mut hostile = graph();
        hostile["nodes"].as_array_mut().unwrap().push(json!({
            "id": "x.py", "name": "</script><img src=x onerror=alert(1)>",
            "kind": "file", "path": "x.py"
        }));
        let html = render_html(&hostile, &VizOptions::default());
        assert!(
            !html.contains("</script><img"),
            "the payload escaped its script block"
        );
        assert!(html.contains("\\u003c/script"));
        // Escaping must not corrupt the value: the page parses it back intact.
        let embedded = crate::page::embedded_payload(&html);
        assert!(embedded["nodes"]
            .as_array()
            .unwrap()
            .iter()
            .any(|n| n["name"] == "</script><img src=x onerror=alert(1)>"));
    }

    #[test]
    fn an_empty_generation_renders_a_page_that_says_so() {
        // Rather than a blank canvas, which reads as a broken renderer.
        let html = render_html(&json!({"nodes": [], "edges": []}), &VizOptions::default());
        let embedded = crate::page::embedded_payload(&html);
        assert_eq!(embedded["counts"]["nodes_total"], json!(0));
        assert!(
            html.contains("No nodes to display"),
            "the page words the empty state"
        );
    }

    #[test]
    fn the_page_carries_its_renderer_and_needs_no_network() {
        let html = render_html(&graph(), &VizOptions::default());
        // Rewritten from `contains("force-graph")`, which pinned the old
        // vendored renderer: the marker is now the shared code-map renderer's
        // own stamp, and the retired renderer must be gone.
        assert!(
            html.contains("devmap-code-map"),
            "the shared renderer must be embedded"
        );
        assert!(!html.contains("force-graph"), "the old renderer is retired");
        assert!(
            !html.contains("src=\"http"),
            "the page must not fetch anything: {}",
            &html[..200.min(html.len())]
        );
    }

    #[test]
    fn ranking_is_deterministic_for_equal_degrees() {
        // R4: two runs must produce the same bytes. Ties break on id.
        let first = build_payload(&graph(), &VizOptions::default());
        let second = build_payload(&graph(), &VizOptions::default());
        assert_eq!(first, second);
    }
}
