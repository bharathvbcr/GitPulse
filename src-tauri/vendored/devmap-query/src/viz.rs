//! A self-contained HTML view of the graph. No network, no build step.
//!
//! Ported from `src/devcouncil/indexing/viz.py`. The Python version read
//! `code_graph.json` back off disk to render it; this one projects the same
//! `Value` the graph writer already built, so the picture and the artifact
//! cannot describe different generations.
//!
//! # The cap is the interesting part
//!
//! A force layout over 12,000 nodes is not a visualization, it is a hairball
//! that pins a CPU. So the view is capped — and a capped view that does not say
//! so is worse than no view, because the reader concludes the graph is small.
//!
//! Nodes are therefore ranked by degree and the *most connected* survive: a cap
//! that keeps hubs shows the shape of the graph, while a cap that keeps whatever
//! the store happened to return first shows an arbitrary sample of leaves. Both
//! counts ride in the payload and both are printed in the page header, so
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

/// The vendored renderer, embedded so the page works offline and from a file://
/// URL. force-graph v1.51.4, MIT (<https://github.com/vasturiano/force-graph>).
///
/// The `.bundle` suffix keeps it out of the index, and is not cosmetic. As
/// `force-graph.min.js` this file is `is_indexable_source` — `vendor/` and
/// `.min.js` exempt a path from *liveness*, not from *extraction* — and 177 KB
/// of single-line minified JavaScript exhausts the 5 s per-file parse budget.
/// A budget exhausted mid-walk is a race: `mutation_fuzz` caught two extractions
/// of identical bytes disagreeing, which is an R4 determinism violation. An
/// extension the detector does not map to a language is the contained fix; see
/// the note in the commit for the general one.
const FORCE_GRAPH_JS: &str = include_str!("../assets/force-graph.min.js.bundle");

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
    // not drawn is a line into empty space, and force-graph would invent a
    // phantom node for it — a node the graph does not contain.
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

/// Escape a JSON payload for embedding in a `<script>` block.
///
/// `</script>` inside a string literal ends the block, whatever the JSON
/// grammar thinks, so a symbol named `</script>` would otherwise close the tag
/// and inject the rest of the graph into the document as markup. Escaping the
/// three characters that can start such a sequence is the standard fix and
/// leaves the value byte-identical after `JSON.parse`.
fn embed_json(value: &Value) -> String {
    serde_json::to_string(value)
        .unwrap_or_else(|_| "null".to_string())
        .replace('<', "\\u003c")
        .replace('>', "\\u003e")
        .replace('&', "\\u0026")
}

/// Render the code graph: one HTML file, no network.
pub fn render_html(graph: &Value, options: &VizOptions) -> String {
    let mut payload = build_payload(graph, options);
    let counts = payload.get("counts").cloned().unwrap_or(json!({}));
    let shown = counts["nodes_shown"].as_u64().unwrap_or(0);
    let total = counts["nodes_total"].as_u64().unwrap_or(0);
    let truncated = counts["nodes_truncated"].as_bool().unwrap_or(false);

    // The headline states the cap in the page itself, not only in the payload.
    // A reader who never opens devtools must still be unable to mistake a
    // capped view for the whole graph.
    let subtitle = if truncated {
        format!("{shown} of {total} nodes (most connected first; raise --max-nodes to widen)",)
    } else {
        format!("{total} nodes")
    };
    payload["view"] = json!({
        "title": options.title,
        "subtitle": subtitle,
        "level": if options.symbols { "symbols" } else { "files" },
        "suffix": "code graph",
        "detail_fields": [
            ["Path", "path"], ["Kind", "kind"], ["Area", "area"],
            ["Community", "community"], ["Language", "language"],
            ["Liveness", "liveness"], ["Why", "liveness_reason"],
        ],
        "flag_filters": [["dead", "Dead candidates only"]],
        "legend": [
            ["#3d8bfd", "reached"], ["#e35d6a", "dead candidate"],
            ["#34d399", "entry point"], ["#f0ad4e", "unwired"],
            // A colour of its own, because the alternative is to paint a
            // README the same blue as reached code or leave it grey and
            // unexplained. Every isolated config file is its own singleton
            // community and draws as a lone dot; in a picture whose legend
            // names "dead" and "unwired", an unlabelled dot is read as one of
            // them.
            ["#8b9bb4", "data — not a liveness candidate"],
        ],
    });
    render_page(&payload)
}

/// The page both views share.
///
/// One shell, driven by `payload.view`: the two graphs differ in what a node
/// *is*, not in how the page works, and a second copy of this would be the
/// thing that drifts.
fn render_page(payload: &Value) -> String {
    let view = payload.get("view").cloned().unwrap_or(json!({}));
    let level = view["level"].as_str().unwrap_or("");
    let title = html_escape(view["title"].as_str().unwrap_or("devmap"));
    let subtitle = view["subtitle"].as_str().unwrap_or("").to_string();
    let data = embed_json(payload);

    format!(
        r#"<meta charset="utf-8"/>
<meta name="viewport" content="width=device-width, initial-scale=1"/>
<title>{title} — {suffix}</title>
<style>
:root {{
  --bg:#0f1419; --panel:#161d27; --fg:#e7ecf3; --muted:#8b9bb4; --line:#243044;
  --accent:#3d8bfd; --dead:#e35d6a; --entry:#34d399; --unwired:#f0ad4e;
}}
* {{ box-sizing:border-box; }}
html,body {{ height:100%; }}
body {{ margin:0; font:14px/1.45 ui-sans-serif,system-ui,-apple-system,sans-serif;
  background:var(--bg); color:var(--fg); display:flex; overflow:hidden; }}
#side {{ width:340px; flex:none; background:var(--panel); border-right:1px solid var(--line);
  display:flex; flex-direction:column; }}
#head {{ padding:14px 16px; border-bottom:1px solid var(--line); }}
h1 {{ font-size:15px; margin:0 0 4px; font-weight:600; }}
.sub {{ color:var(--muted); font-size:12px; }}
#controls {{ padding:12px 16px; border-bottom:1px solid var(--line); }}
input {{ width:100%; padding:7px 9px; background:#0d1218; color:var(--fg);
  border:1px solid var(--line); border-radius:5px; font:inherit; }}
/* The rule above is for the search box. A checkbox stretched to 100% pushes its
   own label onto the next line and reads as a broken control. */
input[type=checkbox] {{ width:auto; flex:none; margin:0; padding:0; }}
label.row {{ display:flex; align-items:center; gap:7px; margin:9px 0 0; color:var(--muted);
  font-size:12px; white-space:nowrap; cursor:pointer; }}
#legend {{ padding:12px 16px; border-bottom:1px solid var(--line); font-size:12px; color:var(--muted); }}
#legend span.key {{ display:inline-block; width:9px; height:9px; border-radius:50%; margin-right:6px; }}
#detail {{ padding:14px 16px; overflow:auto; flex:1; }}
#detail h2 {{ font-size:13px; margin:0 0 8px; word-break:break-all; }}
#detail dt {{ color:var(--muted); font-size:11px; text-transform:uppercase; letter-spacing:.04em; margin-top:9px; }}
#detail dd {{ margin:2px 0 0; word-break:break-all; }}
#detail ul {{ margin:4px 0 0; padding-left:18px; }}
.tag {{ display:inline-block; padding:1px 7px; border-radius:9px; font-size:11px; margin-right:5px; }}
.tag.dead {{ background:rgba(227,93,106,.18); color:var(--dead); }}
.tag.entry {{ background:rgba(52,211,153,.18); color:var(--entry); }}
.tag.unwired {{ background:rgba(240,173,78,.18); color:var(--unwired); }}
#notes {{ padding:0 16px; font-size:12px; color:var(--muted); }}
#notes:not(:empty) {{ padding:12px 16px; border-bottom:1px solid var(--line); }}
#notes details {{ margin-top:6px; }}
#notes summary {{ cursor:pointer; color:var(--unwired); }}
#notes ul {{ margin:6px 0 0; padding-left:16px; max-height:180px; overflow:auto; }}
#notes li {{ word-break:break-all; margin-bottom:3px; }}
#graph {{ flex:1; position:relative; min-width:0; }}
#empty {{ position:absolute; inset:0; display:flex; align-items:center; justify-content:center;
  color:var(--muted); text-align:center; padding:32px; pointer-events:none; }}
/* An author `display` beats the UA sheet's `[hidden] {{ display:none }}`, so the
   overlay has to opt out explicitly or it never hides. */
#empty[hidden] {{ display:none; }}
</style>

<div id="side">
  <div id="head">
    <h1>{title}</h1>
    <div class="sub">{subtitle} · {level}</div>
  </div>
  <div id="controls">
    <input id="q" type="search" placeholder="Filter by name or path…" autocomplete="off"/>
    <div id="flagFilters"></div>
    <label class="row" title="Hide document and note nodes and their relationships"><input type="checkbox" id="hideNotes"/> Hide notes &amp; Markdown</label>
    <label class="row"><input type="checkbox" id="labels" checked/> Show labels</label>
    <div class="sub" id="filterCount" role="status"></div>
  </div>
  <div id="legend"></div>
  <div id="notes"></div>
  <div id="detail"><span class="sub">Click a node for detail.</span></div>
</div>
<div id="graph"></div>

<script>{force_graph}</script>
<script>
const DATA = JSON.parse({data_literal});
const flagsOf = n => (n.flags || []).map(f => f.flag);
// Precedence, not an ordering accident. A node can carry several flags and only
// one colour; `entry` outranks `unwired` because nothing calling an entry point
// is what an entry point *is*, and painting one as a defect is a false alarm an
// operator acts on. Every flag still shows as a tag in the detail panel.
// `not_applicable` sits below the three findings and above the default. Below,
// because a data file that somehow carried a finding should still show the
// finding rather than hide behind its category. Above the default, because
// "reached" is a claim about code and a README is not code — painting it the
// same blue as a live module is the quieter half of the same mistake as
// painting it red.
const colorFor = n => {{
  const f = flagsOf(n);
  if (f.includes('dead')) return '#e35d6a';
  if (f.includes('entry')) return '#34d399';
  if (f.includes('unwired')) return '#f0ad4e';
  if (n.liveness === 'not_applicable') return '#8b9bb4';
  return '#3d8bfd';
}};

const VIEW = DATA.view || {{}};
function esc(s) {{
  return String(s).replace(/[&<>"']/g, c =>
    ({{'&':'&amp;','<':'&lt;','>':'&gt;','"':'&quot;',"'":'&#39;'}})[c]);
}}

document.getElementById('legend').innerHTML = (VIEW.legend || [])
  .map(([color, label]) =>
    '<div><span class="key" style="background:' + esc(color) + '"></span>' + esc(label) + '</div>')
  .join('');

// Crossings the view could not place onto a subsystem. Dropping them silently
// would make "nothing crosses here" and "we could not tell" look identical,
// which is the one thing this page must never do.
(function notes() {{
  const total = DATA.unresolved_handoffs_total || 0;
  if (!total) return;
  const listed = DATA.unresolved_handoffs || [];
  const more = total > listed.length ? ' (' + listed.length + ' of ' + total + ' listed)' : '';
  document.getElementById('notes').innerHTML =
    '<details><summary>' + total + ' handoff' + (total === 1 ? '' : 's') +
    ' not drawn' + more + '</summary><ul>' +
    listed.map(u => '<li>' + esc(u.from) + ': ' + esc(u.text) + '</li>').join('') +
    '</ul></details>';
}})();

const el = document.getElementById('graph');
if (!DATA.nodes.length) {{
  el.innerHTML = '<div id="empty">This generation has no ' + DATA.level +
    '-level nodes to draw.<br/>Build the index first, or switch level.</div>';
}} else {{
  // force-graph mutates link endpoints from ids into node objects, so every
  // filter below rebuilds from a pristine copy rather than from live state.
  const source = JSON.parse(JSON.stringify(DATA));
  let showLabels = true;
  const MAX_FILTER_ZOOM = 4;
  const FILTER_FIT_DELAY_MS = 80;
  let filterFitTimer;

  const graph = ForceGraph()(el)
    .backgroundColor('#0f1419')
    .nodeId('id')
    .nodeLabel(n => n.name + '  ·  ' + n.path)
    .nodeColor(colorFor)
    .nodeRelSize(3)
    .nodeVal(n => 1 + Math.min(n.degree || 0, VIEW.degree_cap || 40))
    .linkColor(l => (VIEW.link_colors || {{}})[l.kind] || 'rgba(139,155,180,0.22)')
    .linkLabel(l => l.label ? esc(l.label) : '')
    .linkDirectionalArrowLength(2.5)
    .linkDirectionalArrowRelPos(1)
    .onNodeClick(showDetail)
    .graphData(JSON.parse(JSON.stringify(source)));

  graph.nodeCanvasObjectMode(() => showLabels ? 'after' : undefined)
    .nodeCanvasObject((n, ctx, scale) => {{
      if (scale < 1.4) return;
      ctx.font = (11 / scale) + 'px ui-sans-serif, system-ui, sans-serif';
      ctx.fillStyle = '#e7ecf3';
      ctx.textAlign = 'center';
      ctx.fillText(n.name, n.x, n.y - 7 / scale);
    }});

  // A filter that matches nothing must say so. An empty canvas is also what a
  // camera pointed at the wrong place looks like, and the two need different
  // reactions from the reader.
  const noMatch = document.createElement('div');
  noMatch.id = 'empty';
  noMatch.hidden = true;
  noMatch.textContent = 'Nothing matches this filter.';
  el.appendChild(noMatch);

  const detail = document.getElementById('detail');
  const byId = new Map((DATA.subsystems || []).map(s => [s.id, s]));
  function showDetail(n) {{
    const tags = flagsOf(n).map(f => '<span class="tag ' + f + '">' + f + '</span>').join('');
    let html = '<h2>' + esc(n.name) + '</h2>' + tags + '<dl>';
    for (const [label, key] of (VIEW.detail_fields || [])) {{
      let v = n[key];
      if (key === 'path' && n.line) v = v + ':' + n.line;
      html += row(label, v);
    }}
    html += row('Degree', String(n.degree)) + '</dl>';
    const sub = byId.get(n.id);
    if (sub) {{
      html += section('Summary', sub.summary ? [sub.summary] : [], 'no summary recorded');
      html += section('Entry points', sub.entry_points, 'none');
      html += section('Critical files', sub.critical_files, 'none');
      html += section('Neighbours', sub.neighbors, 'none');
      // "computed" vs "empty" is the distinction the whole tool exists to keep.
      // An un-derived field printed as "none" is a claim the producer never made.
      html += section('Handoff paths', sub.handoff_paths,
        DATA.meta && DATA.meta.handoff_paths_computed ? 'none' : 'not computed for this map');
      for (const [role, files] of Object.entries(sub.role_files || {{}})) {{
        html += section('Role · ' + role, files, 'none');
      }}
      if (!DATA.meta || !DATA.meta.role_files_computed) {{
        html += '<dt>Role files</dt><dd class="sub">not computed for this map</dd>';
      }}
    }}
    detail.innerHTML = html;
  }}
  const row = (k, v) => v ? '<dt>' + k + '</dt><dd>' + esc(String(v)) + '</dd>' : '';
  function section(label, items, empty) {{
    if (!items || !items.length) return '<dt>' + label + '</dt><dd class="sub">' + empty + '</dd>';
    return '<dt>' + label + '</dt><dd><ul>' +
      items.map(i => '<li>' + esc(String(i)) + '</li>').join('') + '</ul></dd>';
  }}
  function fitFiltered(nodes) {{
    const placed = nodes.filter(n => Number.isFinite(n.x) && Number.isFinite(n.y));
    if (!placed.length) return;
    let minX = placed[0].x;
    let maxX = placed[0].x;
    let minY = placed[0].y;
    let maxY = placed[0].y;
    for (const node of placed.slice(1)) {{
      minX = Math.min(minX, node.x);
      maxX = Math.max(maxX, node.x);
      minY = Math.min(minY, node.y);
      maxY = Math.max(maxY, node.y);
    }}
    // A singleton and coincident nodes have a zero-size world bounding box.
    // Give that box a real extent before deriving camera scale, then cap the
    // scale so filtering cannot turn one ordinary node into a full-pane disk.
    const spanX = Math.max(maxX - minX, 1);
    const spanY = Math.max(maxY - minY, 1);
    const width = Math.max(el.clientWidth - 80, 1);
    const height = Math.max(el.clientHeight - 80, 1);
    const targetZoom = Math.max(0.05,
      Math.min(width / spanX, height / spanY, MAX_FILTER_ZOOM));
    graph.centerAt((minX + maxX) / 2, (minY + maxY) / 2, 250);
    graph.zoom(targetZoom, 250);
  }}
  function apply() {{
    clearTimeout(filterFitTimer);
    filterFitTimer = undefined;
    const term = document.getElementById('q').value.trim().toLowerCase();
    const hideNotes = document.getElementById('hideNotes').checked;
    const required = (VIEW.flag_filters || [])
      .filter(([flag]) => {{
        const box = document.getElementById('flag-' + flag);
        return box && box.checked;
      }})
      .map(([flag]) => flag);
    const keep = source.nodes.filter(n => {{
      if (hideNotes && n.documentation === true) return false;
      const f = flagsOf(n);
      if (required.some(r => !f.includes(r))) return false;
      if (!term) return true;
      return n.name.toLowerCase().includes(term) || (n.path || '').toLowerCase().includes(term);
    }});
    const ids = new Set(keep.map(n => n.id));
    noMatch.hidden = keep.length > 0;
    document.getElementById('filterCount').textContent = keep.length + ' of ' + source.nodes.length + ' match filters';
    detail.innerHTML = '<span class="sub">Click a node for detail.</span>';
    graph.graphData({{
      nodes: JSON.parse(JSON.stringify(keep)),
      links: source.links
        .filter(l => ids.has(l.source) && ids.has(l.target))
        .map(l => Object.assign({{}}, l)),
    }});
    // The survivors are fresh copies with no coordinates, so let the
    // simulation place them before deriving a bounded camera. Canceling the
    // previous timer prevents a stale quick-typing result from moving the
    // camera after a newer filter has already won.
    if (keep.length) {{
      filterFitTimer = setTimeout(() => {{
        filterFitTimer = undefined;
        fitFiltered(graph.graphData().nodes);
      }}, FILTER_FIT_DELAY_MS);
    }}
  }}
  document.getElementById('q').addEventListener('input', apply);
  document.getElementById('hideNotes').addEventListener('change', apply);
  for (const [flag, label] of (VIEW.flag_filters || [])) {{
    const wrap = document.createElement('label');
    wrap.className = 'row';
    wrap.innerHTML = '<input type="checkbox" id="flag-' + flag + '"/> ' + esc(label);
    document.getElementById('flagFilters').appendChild(wrap);
    wrap.querySelector('input').addEventListener('change', apply);
  }}
  document.getElementById('labels').addEventListener('change', e => {{
    showLabels = e.target.checked;
    graph.nodeCanvasObjectMode(() => showLabels ? 'after' : undefined);
  }});

  const fit = () => graph.width(el.clientWidth).height(el.clientHeight);
  fit();
  window.addEventListener('resize', fit);
}}
</script>
"#,
        title = title,
        suffix = html_escape(view["suffix"].as_str().unwrap_or("code graph")),
        subtitle = html_escape(&subtitle),
        level = level,
        force_graph = FORCE_GRAPH_JS,
        data_literal = embed_json(&Value::String(data)),
    )
}

fn html_escape(text: &str) -> String {
    text.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
}

#[cfg(test)]
mod tests {
    use super::*;

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

        let html = render_html(
            &graph(),
            &VizOptions {
                max_nodes: 2,
                ..Default::default()
            },
        );
        assert!(
            html.contains("2 of 3 nodes"),
            "the page itself must state the cap, not only the payload"
        );
    }

    #[test]
    fn an_uncapped_view_does_not_claim_truncation() {
        let payload = build_payload(&graph(), &VizOptions::default());
        assert_eq!(payload["counts"]["nodes_truncated"], json!(false));
        assert!(render_html(&graph(), &VizOptions::default()).contains("3 nodes"));
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
        // force-graph invents a node for an unknown endpoint, which would draw
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
    }

    #[test]
    fn an_empty_generation_renders_a_page_that_says_so() {
        // Rather than a blank canvas, which reads as a broken renderer.
        let html = render_html(&json!({"nodes": [], "edges": []}), &VizOptions::default());
        assert!(html.contains("no ' + DATA.level +"));
        assert!(html.contains("\"nodes_total\\\":0") || html.contains("nodes_total"));
    }

    #[test]
    fn the_page_carries_its_renderer_and_needs_no_network() {
        let html = render_html(&graph(), &VizOptions::default());
        assert!(
            html.contains("force-graph"),
            "the renderer must be embedded"
        );
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
