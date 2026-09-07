//! HTTP routes, their handlers, and the clients that call them.
//!
//! Ported from `indexing/graph/api_routes.py`, but **not** transcribed: the two
//! sides model routes differently, and copying the Python would have produced a
//! view that reports zero routes on every repository.
//!
//! Python reads nodes of kind `ROUTE` carrying `extras.route` / `extras.verb` /
//! `extras.framework`, plus `registers` edges for middleware. This module now
//! reads the same shape, with one of the three still genuinely absent:
//!
//! * `code_graph.rs` emits a node per `ExtractedRoute`, keyed by
//!   [`ExtractedRoute::node_id`] — `"{file}::{VERB} {path}"` — carrying
//!   `extras.route`, `extras.verb` and `extras.framework`. The framework is
//!   read off that node rather than off `ResolvedEdge::details`, which is
//!   still dropped at the store boundary.
//! * The `routes_to` edge names that node as its `source` and the handler's
//!   qualified name as its `target`, so both endpoints are node identities.
//! * There is still no `Registers` edge kind at all, so middleware has no
//!   kernel source whatever.
//!
//! **Both shapes are read, because both exist.** A generation written before
//! route nodes carries `routes_to` edges with a bare `"VERB path"` source and
//! no route node to match — and every store on disk is in that state until its
//! next `devmap build`. Such an edge still produces a route; it simply has no
//! framework, and says so.
//!
//! A field with no source is reported as **absent, by name** — `framework:
//! null`, `middleware: null` with `middleware_available: false` — never as
//! `""` and `[]`. A route with no middleware and a kernel that cannot see
//! middleware must not look the same. `capabilities` carries the two counts
//! that say which case a null `framework` is: how many route nodes the graph
//! held, and how many routes were recovered from an edge with no node.

use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

use regex::Regex;
use serde_json::{json, Value};

/// How much of the tree the client scan may read.
///
/// The Python scans every file in the graph, whole, with no cap on count or
/// size — 2,363 files on this repository, re-read from scratch by `route_map`,
/// again by `shape_check`, and twice more by `api_impact`, which calls both.
/// A view that walks the tree needs a bound, and a bound needs to be reported.
#[derive(Debug, Clone, Copy)]
pub struct ScanBudget {
    /// Files opened before the scan stops.
    pub max_files: usize,
    /// Largest file read. A minified bundle is megabytes of one line and holds
    /// no call site a reader would act on.
    pub max_file_bytes: u64,
    /// Call sites kept.
    pub max_sites: usize,
}

impl Default for ScanBudget {
    fn default() -> Self {
        ScanBudget {
            max_files: 5_000,
            max_file_bytes: 1 << 20,
            max_sites: 2_000,
        }
    }
}

/// What the scan read, and what it did not.
///
/// Every field is emitted. `sites` absent from a route's consumer list because
/// the budget ran out is not the same claim as "nothing calls this route", and
/// the difference has to survive into the answer.
#[derive(Debug, Default, Clone)]
pub struct ScanReport {
    pub files_eligible: usize,
    pub files_read: usize,
    pub files_over_size: usize,
    pub files_unreadable: usize,
    pub files_skipped_budget: usize,
    pub sites_found: usize,
    pub sites_dropped_budget: usize,
}

impl ScanReport {
    fn complete(&self) -> bool {
        self.files_skipped_budget == 0 && self.sites_dropped_budget == 0
    }

    fn to_json(&self) -> Value {
        json!({
            "files_eligible": self.files_eligible,
            "files_read": self.files_read,
            "files_over_size": self.files_over_size,
            "files_unreadable": self.files_unreadable,
            "files_skipped_budget": self.files_skipped_budget,
            "sites_found": self.sites_found,
            "sites_dropped_budget": self.sites_dropped_budget,
            // The one field a caller must branch on: with this false, an empty
            // consumer list means "we stopped looking", not "nothing calls it".
            "complete": self.complete(),
            // And what `complete` is complete *over*. The scan reads the files
            // the graph names, which is not the whole tree: a call site in a
            // file discovery excluded is invisible to it either way, so
            // `complete: true` must not be read as "every file was searched".
            "scope": "files named by the code graph, not the whole working tree",
        })
    }
}

/// Path parameters in every syntax the frameworks spell them.
fn param_re() -> &'static Regex {
    static RE: std::sync::OnceLock<Regex> = std::sync::OnceLock::new();
    RE.get_or_init(|| {
        Regex::new(concat!(
            // Flask and Django, converter included: `<uid>`, `<int:uid>`.
            // **Matched whole** is the load-bearing part: without this
            // alternative the inner `:uid` matches on its own, `<int` survives
            // as a literal segment, and `/api/users/<int:uid>` normalises to
            // `/api/users/<int*>`, which no client path can match.
            //
            // Its position in the alternation is not load-bearing, and an
            // earlier comment here said it was. A match is leftmost by
            // *position* first, and `<` precedes the `:` inside it, so this
            // alternative wins from anywhere in the list — checked by moving
            // it last and re-running
            // `a_flask_converter_normalises_whole_rather_than_from_its_colon`,
            // which still passes.
            r"<[^>]*>",
            r"|:\w+",        // FastAPI
            r"|\{[^}]*\}",   // Express / Spring
            r"|\[[^\]]*\]",  // Next.js dynamic segments
            r"|\$\{[^}]*\}", // JS template-literal fetch URLs
        ))
        .expect("static pattern")
    })
}

/// Rewrite `:id`, `{id}`, `[id]` and `${id}` to a single wildcard.
pub fn normalize_route_path(path: &str) -> String {
    let text = path.replace('\\', "/");
    let text = text.trim();
    let anchored = if text.starts_with('/') {
        text.to_string()
    } else {
        format!("/{}", text.trim_start_matches('/'))
    };
    // `${…}` first: it contains `{…}`, and the inner alternative would match
    // the tail alone and leave a stray `$`.
    param_re().replace_all(&anchored, "*").into_owned()
}

fn segments(path: &str) -> Vec<String> {
    normalize_route_path(path)
        .split('/')
        .filter(|s| !s.is_empty())
        .map(str::to_string)
        .collect()
}

/// Segment-wise match after parameter normalisation.
pub fn paths_match(route_path: &str, fetch_path: &str) -> bool {
    let (a, b) = (segments(route_path), segments(fetch_path));
    if a.len() != b.len() {
        return false;
    }
    a.iter()
        .zip(&b)
        .all(|(left, right)| left == "*" || right == "*" || left == right)
}

/// Whether a client verb could reach a route declared with this one.
pub fn verbs_compatible(route_verb: &str, fetch_verb: &str) -> bool {
    let rv = if route_verb.is_empty() {
        "ANY".to_string()
    } else {
        route_verb.to_ascii_uppercase()
    };
    let fv = if fetch_verb.is_empty() {
        "GET".to_string()
    } else {
        fetch_verb.to_ascii_uppercase()
    };
    if matches!(rv.as_str(), "ANY" | "FILE" | "ROUTE") {
        return true;
    }
    if fv == "GET" && matches!(rv.as_str(), "GET" | "HEAD") {
        return true;
    }
    rv == fv
}

// ---------------------------------------------------------------------------
// Client call sites
// ---------------------------------------------------------------------------

struct SitePatterns {
    fetch_literal: Regex,
    fetch_template: Regex,
    verbed: Regex,
    resp_var: Regex,
    json_var: Regex,
    destructure: Regex,
    ts_return: Regex,
    ts_key: Regex,
}

fn patterns() -> &'static SitePatterns {
    static P: std::sync::OnceLock<SitePatterns> = std::sync::OnceLock::new();
    P.get_or_init(|| SitePatterns {
        fetch_literal: Regex::new(r#"(?i)fetch\s*\(\s*['"]([^'"]+)['"]"#).unwrap(),
        fetch_template: Regex::new(r"(?i)fetch\s*\(\s*`([^`]+)`").unwrap(),
        // One pattern for axios/requests/httpx rather than three that differ
        // only in the library name.
        verbed: Regex::new(
            r#"(?i)\b(?:axios(?:\.default)?|requests|httpx)\.(get|post|put|patch|delete)\s*\(\s*['"`]([^'"`]+)['"`]"#,
        )
        .unwrap(),
        resp_var: Regex::new(
            r"(?i)(?:const|let|var)\s+(\w+)\s*=\s*(?:await\s+)?(?:fetch|axios|requests|httpx)",
        )
        .unwrap(),
        json_var: Regex::new(r"(?i)(?:const|let|var)\s+(\w+)\s*=\s*(?:await\s+)?(\w+)\.json\s*\(")
            .unwrap(),
        destructure: Regex::new(r"(?i)const\s*\{\s*([^}]+)\s*\}\s*=\s*(?:await\s+)?").unwrap(),
        ts_return: Regex::new(r"(?s)return\s*\{([^}]+)\}").unwrap(),
        ts_key: Regex::new(r#"['"]?(\w+)['"]?\s*:"#).unwrap(),
    })
}

/// Lines of context read after a call site when looking for the response keys.
const SCAN_WINDOW: usize = 40;

#[derive(Debug, Clone)]
struct Site {
    path: String,
    line: usize,
    url: String,
    verb: String,
    response_var: String,
    accessed_keys: Vec<String>,
}

impl Site {
    fn to_json(&self) -> Value {
        json!({
            "path": self.path,
            "line": self.line,
            "url": self.url,
            "verb": self.verb,
            "response_var": self.response_var,
            "accessed_keys": self.accessed_keys,
            // Pattern-matched, not parsed. The codebase keeps the two claims
            // apart everywhere else and this is no different: a `fetch` inside
            // a comment or a string is a hit here.
            "evidence": "regex",
        })
    }
}

/// Every file the graph names, in a stable order.
fn graph_files(graph: &Value) -> Vec<String> {
    let mut files: BTreeSet<String> = BTreeSet::new();
    for node in graph["nodes"].as_array().into_iter().flatten() {
        if let Some(path) = node["path"].as_str() {
            if !path.is_empty() {
                files.insert(path.replace('\\', "/"));
            }
        }
    }
    files.into_iter().collect()
}

fn scan_sites(root: &Path, graph: &Value, budget: &ScanBudget) -> (Vec<Site>, ScanReport) {
    let pat = patterns();
    let files = graph_files(graph);
    let mut report = ScanReport {
        files_eligible: files.len(),
        ..Default::default()
    };
    let mut sites: Vec<Site> = Vec::new();

    for rel in &files {
        if report.files_read >= budget.max_files {
            report.files_skipped_budget += 1;
            continue;
        }
        let full = root.join(rel);
        match std::fs::metadata(&full) {
            Ok(meta) if meta.len() > budget.max_file_bytes => {
                report.files_over_size += 1;
                continue;
            }
            Ok(_) => {}
            Err(_) => {
                report.files_unreadable += 1;
                continue;
            }
        }
        let Ok(text) = std::fs::read_to_string(&full) else {
            // Read as text; a binary that slipped past discovery is not a
            // client call site, but it is also not a file we examined.
            report.files_unreadable += 1;
            continue;
        };
        report.files_read += 1;
        let lines: Vec<&str> = text.lines().collect();

        for (index, line) in lines.iter().enumerate() {
            let mut found: Vec<(String, String)> = Vec::new();
            for caps in pat.fetch_literal.captures_iter(line) {
                found.push((caps[1].to_string(), "GET".to_string()));
            }
            for caps in pat.fetch_template.captures_iter(line) {
                found.push((caps[1].to_string(), "GET".to_string()));
            }
            for caps in pat.verbed.captures_iter(line) {
                found.push((caps[2].to_string(), caps[1].to_ascii_uppercase()));
            }
            for (url, verb) in found {
                report.sites_found += 1;
                if sites.len() >= budget.max_sites {
                    report.sites_dropped_budget += 1;
                    continue;
                }
                sites.push(build_site(rel, index + 1, &url, &verb, &lines));
            }
        }
    }
    (sites, report)
}

fn build_site(path: &str, line: usize, url: &str, verb: &str, lines: &[&str]) -> Site {
    let pat = patterns();
    let end = (line - 1 + SCAN_WINDOW).min(lines.len());
    let window = &lines[line - 1..end];

    let mut response_var = String::new();
    for text in window {
        if let Some(caps) = pat.resp_var.captures(text) {
            response_var = caps[1].to_string();
            break;
        }
    }
    let mut data_var = response_var.clone();
    for text in window {
        if let Some(caps) = pat.json_var.captures(text) {
            if response_var.is_empty() || caps[2] == response_var {
                data_var = caps[1].to_string();
                break;
            }
        }
    }
    let var = if data_var.is_empty() {
        response_var.clone()
    } else {
        data_var
    };
    Site {
        path: path.to_string(),
        line,
        url: url.trim().to_string(),
        verb: verb.to_string(),
        accessed_keys: consumer_keys(window, &var),
        response_var: var,
    }
}

/// Keys a consumer reads off the response, by any of the three spellings.
fn consumer_keys(window: &[&str], response_var: &str) -> Vec<String> {
    let pat = patterns();
    let text = window.join("\n");
    let mut keys: BTreeSet<String> = BTreeSet::new();

    if !response_var.is_empty() {
        let quoted = regex::escape(response_var);
        // Built per call because the variable name is data. Anchored on a word
        // boundary so `data` does not match `metadata`.
        if let Ok(dot) = Regex::new(&format!(r"\b{quoted}\.(\w+)")) {
            for caps in dot.captures_iter(&text) {
                keys.insert(caps[1].to_string());
            }
        }
        if let Ok(bracket) = Regex::new(&format!(r#"\b{quoted}\s*\[\s*['"](\w+)['"]\s*\]"#)) {
            for caps in bracket.captures_iter(&text) {
                keys.insert(caps[1].to_string());
            }
        }
    }
    for caps in pat.destructure.captures_iter(&text) {
        for part in caps[1].split(',') {
            let name = part.split(':').next().unwrap_or("").trim();
            if !name.is_empty() && name.chars().all(|c| c.is_alphanumeric() || c == '_') {
                keys.insert(name.to_string());
            }
        }
    }
    keys.into_iter().collect()
}

// ---------------------------------------------------------------------------
// The views
// ---------------------------------------------------------------------------

/// A route, as this kernel can see it.
struct RouteRow {
    verb: String,
    path: String,
    handlers: Vec<String>,
    /// The route node this row came from, when one did.
    ///
    /// **One row per declaration, not one per verb and path.** Three files on
    /// this repository declare `GET /health` — a Flask app, an Axum service
    /// and a test fixture — and merging them reported one route served by
    /// `"axum, fastapi/flask"`, which is not a thing that exists. It also lost
    /// the only field that answers "which file serves this": the node's own.
    /// `indexing/graph/api_routes.py` keys on the node for the same reason,
    /// so this is also what makes the two views agree on a row count.
    ///
    /// `None` is the edge-only case — a `routes_to` edge from a generation
    /// older than route nodes — which still groups by verb and path because
    /// that is all such an edge carries.
    node_id: Option<String>,
    /// The declaring file and the line it is declared on, from the node.
    file: Option<String>,
    line: Option<u64>,
    /// The framework the node declared. `None` on an edge-only row, which is
    /// what keeps a null `framework` meaning "not seen" rather than "none".
    framework: Option<String>,
}

/// What the graph could say about routes, so a null field can be read.
#[derive(Debug, Default, Clone, Copy)]
struct RouteProvenance {
    /// Nodes of kind `route` in the graph.
    route_nodes: usize,
    /// Routes recovered from a `routes_to` edge whose source named no route
    /// node — a generation older than route nodes. These have no framework.
    routes_without_a_node: usize,
}

/// Split a `routes_to` edge source into its verb and path.
///
/// The source is the route's node id, `"{file}::{VERB} {path}"`. A verb never
/// contains a space or a colon, so the token before the first space is
/// `file::VERB`, and the verb is whatever follows its last `::`. The path is
/// untouched, so one containing `::` cannot confuse this.
///
/// Splitting on the first space alone would read `a.py::POST` as the verb, and
/// `verbs_compatible` matches that against nothing: every route would report
/// zero consumers *in a complete scan*, which is the one answer this module
/// exists never to give.
///
/// A generation written before route nodes carries a bare `"VERB path"`, which
/// has no `::` and comes back unchanged. A source with no space at all
/// declares no verb, and gets an empty one — `verbs_compatible` reads that as
/// `ANY`, rather than inventing a `GET` nobody wrote.
fn split_route_source(source: &str) -> (String, String) {
    let (head, path) = match source.split_once(' ') {
        Some((head, path)) => (head, path),
        None => ("", source),
    };
    let verb = head.rsplit("::").next().unwrap_or(head);
    (verb.to_ascii_uppercase(), path.to_string())
}

/// Read routes off the graph: the route nodes, and the edges that bind them.
///
/// Nodes are the authority — they carry the verb, the path and the framework
/// as the extractor recorded them — and the `routes_to` edges only attach
/// handlers. A route node whose handler never bound therefore still appears,
/// with an empty handler list: "nothing in this index serves it" is an answer,
/// and it used to be indistinguishable from the route not existing.
fn routes_from_graph(graph: &Value) -> (Vec<RouteRow>, RouteProvenance) {
    #[derive(Default)]
    struct Accum {
        handlers: BTreeSet<String>,
        file: Option<String>,
        line: Option<u64>,
        framework: Option<String>,
    }
    // `(path, verb, node id)`. The id is last so rows sort by the HTTP surface
    // first and the declaring node only breaks ties; `None` — the edge-only
    // row — sorts before any node, deterministically (R4).
    type Key = (String, String, Option<String>);
    let mut by_route: BTreeMap<Key, Accum> = BTreeMap::new();
    let mut provenance = RouteProvenance::default();
    // Node id -> the key it owns, so an edge naming a node lands on that
    // node's row rather than on a re-parse of its id.
    let mut keyed_by_node: BTreeMap<&str, Key> = BTreeMap::new();

    for node in graph["nodes"].as_array().into_iter().flatten() {
        if node["kind"].as_str() != Some("route") {
            continue;
        }
        let Some(id) = node["id"].as_str() else {
            continue;
        };
        provenance.route_nodes += 1;
        let extras = &node["extras"];
        // The id is the fallback, not the source of truth: it is the same
        // three fields concatenated, so it can only ever agree.
        let (id_verb, id_path) = split_route_source(id);
        let path = match extras["route"].as_str() {
            Some(path) if !path.is_empty() => path.to_string(),
            _ => id_path,
        };
        let verb = match extras["verb"].as_str() {
            Some(verb) if !verb.is_empty() => verb.to_ascii_uppercase(),
            _ => id_verb,
        };
        let key: Key = (path, verb, Some(id.to_string()));
        let entry = by_route.entry(key.clone()).or_default();
        entry.file = node["path"].as_str().map(str::to_string);
        entry.line = node["line"].as_u64();
        entry.framework = extras["framework"]
            .as_str()
            .filter(|framework| !framework.is_empty())
            .map(str::to_string);
        keyed_by_node.insert(id, key);
    }

    for edge in graph["edges"].as_array().into_iter().flatten() {
        if edge["kind"].as_str() != Some("routes_to") {
            continue;
        }
        let Some(source) = edge["source"].as_str() else {
            continue;
        };
        let key = match keyed_by_node.get(source) {
            Some(key) => key.clone(),
            None => {
                let (verb, path) = split_route_source(source);
                let key: Key = (path, verb, None);
                // Counted once per route, not once per handler edge.
                if !by_route.contains_key(&key) {
                    provenance.routes_without_a_node += 1;
                }
                key
            }
        };
        let entry = by_route.entry(key).or_default();
        if let Some(target) = edge["target"].as_str() {
            entry.handlers.insert(target.to_string());
        }
    }

    let rows = by_route
        .into_iter()
        .map(|((path, verb, node_id), accum)| RouteRow {
            verb,
            path,
            handlers: accum.handlers.into_iter().collect(),
            node_id,
            file: accum.file,
            line: accum.line,
            framework: accum.framework,
        })
        .collect();
    (rows, provenance)
}

/// Index nodes by id and by bare name, since a `routes_to` target is the
/// handler's *name* and node ids are qualified.
fn node_index(graph: &Value) -> (BTreeMap<String, Value>, BTreeMap<String, Vec<Value>>) {
    let mut by_id = BTreeMap::new();
    let mut by_name: BTreeMap<String, Vec<Value>> = BTreeMap::new();
    for node in graph["nodes"].as_array().into_iter().flatten() {
        if let Some(id) = node["id"].as_str() {
            by_id.insert(id.to_string(), node.clone());
        }
        if let Some(name) = node["name"].as_str() {
            by_name
                .entry(name.to_string())
                .or_default()
                .push(node.clone());
        }
    }
    (by_id, by_name)
}

/// Resolve a handler reference to the node it names.
///
/// Ambiguity is reported, never resolved by taking the first: two functions
/// named `handler` in different files are two different answers, and picking
/// one silently attributes a route to a file that may not serve it.
fn resolve_handler(
    reference: &str,
    by_id: &BTreeMap<String, Value>,
    by_name: &BTreeMap<String, Vec<Value>>,
) -> Value {
    if let Some(node) = by_id.get(reference) {
        return json!({
            "id": node["id"], "path": node["path"], "name": node["name"],
            "line": node["line"], "kind": node["kind"], "resolution": "id",
        });
    }
    match by_name.get(reference).map(Vec::as_slice) {
        Some([node]) => json!({
            "id": node["id"], "path": node["path"], "name": node["name"],
            "line": node["line"], "kind": node["kind"], "resolution": "name",
        }),
        Some(many) if many.len() > 1 => json!({
            "id": reference,
            "resolution": "ambiguous",
            "candidates": many.iter().filter_map(|n| n["id"].as_str()).collect::<Vec<_>>(),
        }),
        _ => json!({ "id": reference, "resolution": "unresolved" }),
    }
}

/// Keys a Python handler returns in a `return {...}` literal.
///
/// Pattern-based, over the handler's own line range rather than the whole file:
/// the Python re-parses the entire module per handler, and its fallback when
/// the name does not match is to accept any function whose span contains the
/// line — which for a nested function is the enclosing one.
fn handler_return_keys(root: &Path, handler: &Value, budget: &ScanBudget) -> Vec<String> {
    let Some(rel) = handler["path"].as_str() else {
        return Vec::new();
    };
    let full = root.join(rel);
    match std::fs::metadata(&full) {
        Ok(meta) if meta.len() <= budget.max_file_bytes => {}
        _ => return Vec::new(),
    }
    let Ok(text) = std::fs::read_to_string(&full) else {
        return Vec::new();
    };
    let lines: Vec<&str> = text.lines().collect();
    let start = handler["line"].as_u64().unwrap_or(1).max(1) as usize - 1;
    let end = handler["end_line"]
        .as_u64()
        .map(|l| l as usize)
        .filter(|l| *l > start)
        .unwrap_or(start + 120)
        .min(lines.len());
    if start >= lines.len() {
        return Vec::new();
    }
    let chunk = lines[start..end].join("\n");

    let pat = patterns();
    let mut keys: BTreeSet<String> = BTreeSet::new();
    for caps in pat.ts_return.captures_iter(&chunk) {
        for key in pat.ts_key.captures_iter(&caps[1]) {
            keys.insert(key[1].to_string());
        }
    }
    keys.into_iter().collect()
}

/// Routes, their handlers and the clients that call them.
pub fn route_map(root: &Path, graph: &Value, budget: &ScanBudget) -> Value {
    let (by_id, by_name) = node_index(graph);
    let (sites, report) = scan_sites(root, graph, budget);
    let (rows, provenance) = routes_from_graph(graph);

    let mut routes = Vec::new();
    for row in &rows {
        let handlers: Vec<Value> = row
            .handlers
            .iter()
            .map(|h| resolve_handler(h, &by_id, &by_name))
            .collect();
        let mut handler_keys: BTreeSet<String> = BTreeSet::new();
        for handler in &handlers {
            if handler["path"].is_string() {
                handler_keys.extend(handler_return_keys(root, handler, budget));
            }
        }
        let consumers: Vec<Value> = sites
            .iter()
            .filter(|site| paths_match(&row.path, &site.url))
            .filter(|site| verbs_compatible(&row.verb, &site.verb))
            .map(Site::to_json)
            .collect();

        routes.push(json!({
            // The node's id when a node declared this route, so two files
            // serving the same verb and path are two identifiable rows rather
            // than one id naming both. `indexing/graph/api_routes.py` writes
            // the same id.
            "id": match &row.node_id {
                Some(id) => id.clone(),
                None => format!("{} {}", row.verb, row.path).trim().to_string(),
            },
            "path": row.path,
            "normalized_path": normalize_route_path(&row.path),
            "verb": if row.verb.is_empty() { "ANY" } else { &row.verb },
            // Where it is declared, from the node. Null on an edge-only row,
            // which carries no file — not `""`, which names the root.
            "file": match &row.file {
                Some(file) => Value::String(file.clone()),
                None => Value::Null,
            },
            "line": match row.line {
                Some(line) => Value::from(line),
                None => Value::Null,
            },
            "handlers": handlers,
            "handler_keys": handler_keys.into_iter().collect::<Vec<_>>(),
            "consumers": consumers,
            // Absent by name when absent. A null `framework` means no route
            // node declared one — an edge-only route out of a generation older
            // than route nodes — and never "this route has no framework".
            // `middleware` is null on every route: there is no `registers`
            // edge kind for it to be read from.
            "framework": match &row.framework {
                Some(framework) => Value::String(framework.clone()),
                None => Value::Null,
            },
            "middleware": Value::Null,
        }));
    }

    json!({
        "routes": routes,
        "count": rows.len(),
        "scan": report.to_json(),
        "capabilities": {
            // Both numbers, not one flag. A graph can hold route nodes and
            // still carry edge-only routes beside them, and a reader looking
            // at a null `framework` has to be able to tell which of the two it
            // is looking at.
            "framework_available": provenance.route_nodes > 0,
            "route_nodes": provenance.route_nodes,
            "routes_without_a_route_node": provenance.routes_without_a_node,
            "middleware_available": false,
            "reason": "framework is read from the route node the graph carries; a route \
    recovered from a routes_to edge with no such node has none, and every store carries \
    those until its next build. middleware has no `registers` edge kind at all",
        },
    })
}

fn route_matches_filter(route: &Value, filter: &str) -> bool {
    let query = filter.trim();
    if query.is_empty() {
        return true;
    }
    let path = route["path"].as_str().unwrap_or("");
    let id = route["id"].as_str().unwrap_or("");
    if path.contains(query) || id.contains(query) {
        return true;
    }
    if route["normalized_path"].as_str() == Some(normalize_route_path(query).as_str()) {
        return true;
    }
    paths_match(path, query)
}

/// Compare what a handler returns against what its callers read.
pub fn shape_check(
    root: &Path,
    graph: &Value,
    budget: &ScanBudget,
    route_filter: Option<&str>,
) -> Value {
    let mapped = route_map(root, graph, budget);
    shape_check_over(&mapped, route_filter)
}

/// The comparison, over an existing map.
///
/// Split out because `api_impact` needs both views and the Python recomputes
/// the whole tree scan for each — `api_impact` calls `route_map`, then
/// `shape_check`, which calls `route_map` again.
pub fn shape_check_over(mapped: &Value, route_filter: Option<&str>) -> Value {
    let scan_complete = mapped["scan"]["complete"].as_bool().unwrap_or(false);
    let mut checks = Vec::new();

    for route in mapped["routes"].as_array().into_iter().flatten() {
        if let Some(filter) = route_filter {
            if !route_matches_filter(route, filter) {
                continue;
            }
        }
        let handler_keys: BTreeSet<String> = route["handler_keys"]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(|k| k.as_str().map(str::to_string))
            .collect();
        let mut consumer_keys: BTreeSet<String> = BTreeSet::new();
        let consumers = route["consumers"].as_array().map(Vec::len).unwrap_or(0);
        for consumer in route["consumers"].as_array().into_iter().flatten() {
            for key in consumer["accessed_keys"].as_array().into_iter().flatten() {
                if let Some(key) = key.as_str() {
                    consumer_keys.insert(key.to_string());
                }
            }
        }

        let missing: Vec<&String> = consumer_keys.difference(&handler_keys).collect();
        let unused: Vec<&String> = handler_keys.difference(&consumer_keys).collect();
        // A mismatch is a positive finding and needs both sides actually read.
        // With no consumer keys there is nothing to compare, and with an
        // incomplete scan the consumer side is a lower bound — neither is a
        // clean bill of health, so neither reports one.
        let verdict = if consumer_keys.is_empty() {
            "no_consumer_keys"
        } else if !missing.is_empty() {
            "mismatch"
        } else if scan_complete {
            "agrees"
        } else {
            "agrees_on_what_was_scanned"
        };

        checks.push(json!({
            "route": route["path"],
            "verb": route["verb"],
            "route_id": route["id"],
            "handler_keys": handler_keys.iter().collect::<Vec<_>>(),
            "consumer_keys": consumer_keys.iter().collect::<Vec<_>>(),
            "missing_in_handler": missing,
            "unused_by_consumers": unused,
            "mismatch": verdict == "mismatch",
            "verdict": verdict,
            "consumer_count": consumers,
        }));
    }

    let mismatches: Vec<Value> = checks
        .iter()
        .filter(|c| c["mismatch"] == json!(true))
        .cloned()
        .collect();
    json!({
        "checks": checks,
        "mismatch_count": mismatches.len(),
        "mismatches": mismatches,
        "scan": mapped["scan"],
    })
}

/// What one route change reaches: its callers, its shape, and a risk band.
pub fn api_impact(root: &Path, graph: &Value, budget: &ScanBudget, target: &str) -> Value {
    // One tree scan, not the Python's four.
    let mapped = route_map(root, graph, budget);
    let matched: Vec<&Value> = mapped["routes"]
        .as_array()
        .into_iter()
        .flatten()
        .filter(|r| route_matches_filter(r, target))
        .collect();

    let Some(route) = matched
        .iter()
        .find(|r| r["path"].as_str() == Some(target) || r["id"].as_str() == Some(target))
        .or(matched.first())
    else {
        return json!({
            "route": target,
            "found": false,
            "consumers": [],
            "shape_mismatches": [],
            "risk": "unknown",
            // Not "none": the kernel knows of no route by this name, which is a
            // different fact from a route that nothing calls.
            "risk_reason": "no route matched this name",
            "scan": mapped["scan"],
        });
    };

    let shape = shape_check_over(&mapped, route["path"].as_str().or(Some(target)));
    let route_shape = shape["checks"]
        .as_array()
        .into_iter()
        .flatten()
        .find(|c| c["route_id"] == route["id"])
        .cloned();
    let mismatches: Vec<Value> = route_shape
        .iter()
        .filter(|c| c["mismatch"] == json!(true))
        .cloned()
        .collect();

    let consumers = route["consumers"].as_array().cloned().unwrap_or_default();
    let scan_complete = mapped["scan"]["complete"].as_bool().unwrap_or(false);
    let (risk, reason) = risk_band(consumers.len(), mismatches.len(), scan_complete);

    json!({
        "route": route["path"],
        "route_id": route["id"],
        "verb": route["verb"],
        "found": true,
        "matched_routes": matched.len(),
        "handlers": route["handlers"],
        "handler_keys": route["handler_keys"],
        "consumers": consumers,
        "middleware": Value::Null,
        "shape_mismatches": mismatches,
        "risk": risk,
        "risk_reason": reason,
        "scan": mapped["scan"],
    })
}

/// Risk, and why.
///
/// The Python returns `"none"` for a route with no consumers. That reads as
/// "safe to change", and it is the one conclusion an unbounded, regex-based,
/// possibly-truncated client scan cannot support. With an incomplete scan the
/// band is `unknown`; with a complete one, zero consumers is `none_observed`.
fn risk_band(consumers: usize, mismatches: usize, scan_complete: bool) -> (&'static str, String) {
    if consumers == 0 {
        return if scan_complete {
            (
                "none_observed",
                "no client call site matched this route in a complete scan; \
the scan is pattern-based, so this is not proof of no caller"
                    .to_string(),
            )
        } else {
            (
                "unknown",
                "no call site matched, but the scan did not finish — absence here \
is not evidence"
                    .to_string(),
            )
        };
    }
    let band = match (mismatches > 0, consumers >= 2) {
        (true, true) => "high",
        (true, false) => "medium",
        (false, _) => "low",
    };
    (
        band,
        format!("{consumers} consumer(s), {mismatches} shape mismatch(es)"),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn write(dir: &Path, rel: &str, body: &str) {
        let path = dir.join(rel);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, body).unwrap();
    }

    fn tempdir(tag: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "devmap-api-{tag}-{}-{:?}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn graph(nodes: Value, edges: Value) -> Value {
        json!({"nodes": nodes, "edges": edges})
    }

    #[test]
    fn a_template_literal_parameter_normalises_without_leaving_its_dollar() {
        // `${id}` contains `{id}`; matching the inner form first leaves a `$`.
        assert_eq!(normalize_route_path("/api/users/${id}"), "/api/users/*");
        assert_eq!(normalize_route_path("/api/users/:id"), "/api/users/*");
        assert_eq!(normalize_route_path("/api/users/{id}"), "/api/users/*");
        assert_eq!(normalize_route_path("/api/users/[id]"), "/api/users/*");
        assert_eq!(normalize_route_path("api/users"), "/api/users");
    }

    #[test]
    fn a_flask_converter_normalises_whole_rather_than_from_its_colon() {
        // `:uid` matches the FastAPI alternative too. If that one wins, `<int`
        // survives as a literal and the route can never match a real request.
        assert_eq!(normalize_route_path("/api/users/<int:uid>"), "/api/users/*");
        assert_eq!(normalize_route_path("/api/users/<uid>"), "/api/users/*");
        assert_eq!(
            normalize_route_path("/f/<path:rest>/x"),
            "/f/*/x",
            "a converter containing a slash still yields one segment"
        );
        assert!(paths_match("/api/users/<int:uid>", "/api/users/42"));
    }

    #[test]
    fn paths_match_segment_wise_and_reject_different_depths() {
        assert!(paths_match("/api/users/:id", "/api/users/42"));
        assert!(paths_match("/api/users/:id", "/api/users/${uid}"));
        assert!(!paths_match("/api/users/:id", "/api/users"));
        assert!(!paths_match("/api/users/:id", "/api/accounts/42"));
    }

    #[test]
    fn a_route_verb_is_read_from_the_edge_source_not_assumed() {
        // The resolver writes "VERB /path"; a source with no space must not be
        // silently given a GET nobody declared.
        let g = graph(
            json!([{"id": "h", "name": "h", "path": "a.ts", "line": 1, "kind": "function"}]),
            json!([
                {"kind": "routes_to", "source": "POST /api/items", "target": "h"},
                {"kind": "routes_to", "source": "/api/bare", "target": "h"},
            ]),
        );
        let (rows, _) = routes_from_graph(&g);
        let verbs: Vec<(&str, &str)> = rows
            .iter()
            .map(|r| (r.path.as_str(), r.verb.as_str()))
            .collect();
        assert_eq!(verbs, vec![("/api/bare", ""), ("/api/items", "POST")]);
        // An empty verb admits any client verb, rather than claiming GET.
        assert!(verbs_compatible("", "DELETE"));
        assert!(!verbs_compatible("POST", "DELETE"));
        assert!(verbs_compatible("HEAD", "GET"));
    }

    /// The verb survives the route's node identity.
    ///
    /// The edge source is `"{file}::{VERB} {path}"`. Splitting it on the first
    /// space reads the verb as `A.PY::POST`, which `verbs_compatible` matches
    /// against no client verb at all — so every route reports zero consumers
    /// while the scan reports itself complete, and `api_impact` answers
    /// `none_observed` for a route with callers. Nothing else in the suite
    /// catches that: the other fixtures here write the source by hand in the
    /// older shape, and the end-to-end test asserted handlers, not consumers.
    #[test]
    fn a_node_id_source_yields_the_verb_it_declares_not_the_file_prefix() {
        let g = graph(
            json!([{"id": "a.py::h", "name": "h", "path": "a.py", "line": 2, "kind": "function"}]),
            json!([{"kind": "routes_to", "source": "a.py::POST /api/items",
                    "target": "a.py::h"}]),
        );
        let (rows, provenance) = routes_from_graph(&g);
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].verb, "POST");
        assert_eq!(rows[0].path, "/api/items");
        assert!(verbs_compatible(&rows[0].verb, "POST"));
        // No route node declared it, so it is counted as the older shape and
        // carries no framework.
        assert_eq!(provenance.route_nodes, 0);
        assert_eq!(provenance.routes_without_a_node, 1);
        assert!(rows[0].framework.is_none());
        assert!(rows[0].node_id.is_none());
    }

    /// A path containing `::` is a path, not a qualified name.
    #[test]
    fn only_the_verb_token_is_stripped_of_its_file_prefix() {
        assert_eq!(
            split_route_source("a.py::GET /rpc/Foo::bar"),
            ("GET".to_string(), "/rpc/Foo::bar".to_string())
        );
        assert_eq!(
            split_route_source("GET /rpc/Foo::bar"),
            ("GET".to_string(), "/rpc/Foo::bar".to_string())
        );
        // No space: nothing declares a verb, and none is invented.
        assert_eq!(
            split_route_source("/api/bare"),
            (String::new(), "/api/bare".to_string())
        );
    }

    /// A route node carries the framework, and a route without one says so.
    #[test]
    fn the_framework_comes_from_the_route_node_or_is_null() {
        let root = tempdir("framework");
        let g = graph(
            json!([
                {"id": "a.py::h", "name": "h", "path": "a.py", "line": 2, "kind": "function"},
                {"id": "a.py::GET /api/users", "name": "GET /api/users", "path": "a.py",
                 "line": 1, "kind": "route",
                 "extras": {"route": "/api/users", "verb": "GET", "framework": "fastapi/flask"}},
            ]),
            json!([
                {"kind": "routes_to", "source": "a.py::GET /api/users", "target": "a.py::h"},
                // An edge out of an older generation, beside a node-backed one.
                {"kind": "routes_to", "source": "GET /legacy", "target": "a.py::h"},
            ]),
        );
        let mapped = route_map(&root, &g, &ScanBudget::default());
        let routes = mapped["routes"].as_array().unwrap();
        assert_eq!(routes.len(), 2);

        let users = routes.iter().find(|r| r["path"] == "/api/users").unwrap();
        assert_eq!(users["verb"], "GET");
        assert_eq!(users["framework"], "fastapi/flask");
        assert_eq!(users["handlers"][0]["resolution"], "id");

        assert_eq!(users["file"], "a.py");
        assert_eq!(users["line"], 1);
        assert_eq!(users["id"], "a.py::GET /api/users");

        let legacy = routes.iter().find(|r| r["path"] == "/legacy").unwrap();
        assert!(
            legacy["framework"].is_null(),
            "an edge with no route node has no framework, and must not borrow one"
        );
        assert!(
            legacy["file"].is_null(),
            "an edge carries no declaring file, and `\"\"` would name the root"
        );
        assert_eq!(legacy["id"], "GET /legacy");

        // Middleware has no source at all, on either row.
        assert!(users["middleware"].is_null());
        assert_eq!(mapped["capabilities"]["middleware_available"], json!(false));
        // Both numbers, so a reader can tell which kind of null they have.
        assert_eq!(mapped["capabilities"]["framework_available"], json!(true));
        assert_eq!(mapped["capabilities"]["route_nodes"], json!(1));
        assert_eq!(
            mapped["capabilities"]["routes_without_a_route_node"],
            json!(1)
        );
        std::fs::remove_dir_all(&root).ok();
    }

    /// Two files serving one verb and path are two routes, not one.
    ///
    /// Three files on this repository declare `GET /health` — a Flask app, an
    /// Axum service and a test fixture. Keyed on verb and path alone they
    /// merged into a single row whose framework read `"axum, fastapi/flask"`,
    /// which describes nothing that exists, and which dropped the only field
    /// that answers *which file serves this*. `indexing/graph/api_routes.py`
    /// keys on the route node, so this is also what makes the two views agree
    /// on a count.
    #[test]
    fn two_files_declaring_one_route_are_two_rows() {
        let root = tempdir("twofiles");
        let g = graph(
            json!([
                {"id": "app.py::GET /health", "name": "GET /health", "path": "app.py",
                 "line": 3, "kind": "route",
                 "extras": {"route": "/health", "verb": "GET", "framework": "fastapi/flask"}},
                {"id": "main.rs::GET /health", "name": "GET /health", "path": "main.rs",
                 "line": 9, "kind": "route",
                 "extras": {"route": "/health", "verb": "GET", "framework": "axum"}},
            ]),
            json!([]),
        );
        let mapped = route_map(&root, &g, &ScanBudget::default());
        assert_eq!(mapped["count"], json!(2));
        let frameworks: Vec<&str> = mapped["routes"]
            .as_array()
            .unwrap()
            .iter()
            .map(|route| route["framework"].as_str().unwrap())
            .collect();
        // Ordered by the node id, which is deterministic (R4).
        assert_eq!(frameworks, ["fastapi/flask", "axum"]);
        let files: Vec<&str> = mapped["routes"]
            .as_array()
            .unwrap()
            .iter()
            .map(|route| route["file"].as_str().unwrap())
            .collect();
        assert_eq!(files, ["app.py", "main.rs"]);
        std::fs::remove_dir_all(&root).ok();
    }

    /// A route nothing serves is still a route.
    ///
    /// The handler bind can fail — an ambiguous name, a handler in a file the
    /// index does not hold — and before route nodes that produced no edge and
    /// so no row at all. "Nothing in this index serves it" and "no such route"
    /// were the same answer.
    #[test]
    fn a_route_whose_handler_never_bound_is_reported_with_no_handler() {
        let root = tempdir("unbound");
        let g = graph(
            json!([{"id": "a.py::DELETE /api/thing", "name": "DELETE /api/thing",
                    "path": "a.py", "line": 1, "kind": "route",
                    "extras": {"route": "/api/thing", "verb": "DELETE",
                               "framework": "fastapi/flask"}}]),
            json!([]),
        );
        let mapped = route_map(&root, &g, &ScanBudget::default());
        assert_eq!(mapped["count"], json!(1));
        assert_eq!(mapped["routes"][0]["verb"], "DELETE");
        assert_eq!(mapped["routes"][0]["framework"], "fastapi/flask");
        assert!(mapped["routes"][0]["handlers"]
            .as_array()
            .unwrap()
            .is_empty());
        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn two_handlers_with_one_name_are_reported_ambiguous_not_picked() {
        let by_id = BTreeMap::new();
        let mut by_name = BTreeMap::new();
        by_name.insert(
            "handler".to_string(),
            vec![
                json!({"id": "a.ts::handler", "name": "handler", "path": "a.ts"}),
                json!({"id": "b.ts::handler", "name": "handler", "path": "b.ts"}),
            ],
        );
        let resolved = resolve_handler("handler", &by_id, &by_name);
        assert_eq!(resolved["resolution"], "ambiguous");
        assert_eq!(resolved["candidates"].as_array().unwrap().len(), 2);
        assert!(resolved["path"].is_null(), "must not attribute one file");
    }

    #[test]
    fn a_missing_handler_is_unresolved_rather_than_absent() {
        let resolved = resolve_handler("gone", &BTreeMap::new(), &BTreeMap::new());
        assert_eq!(resolved["resolution"], "unresolved");
        assert_eq!(resolved["id"], "gone");
    }

    #[test]
    fn a_consumer_is_matched_to_its_route_with_the_keys_it_reads() {
        let root = tempdir("consumer");
        write(
            &root,
            "web/app.ts",
            r#"
async function load() {
  const res = await fetch('/api/users/42');
  const data = await res.json();
  console.log(data.name, data.email);
}
"#,
        );
        write(
            &root,
            "api/handlers.ts",
            "export function getUser() {\n  return { name: 'x', email: 'y' };\n}\n",
        );
        let g = graph(
            json!([
                {"id": "web/app.ts", "name": "app", "path": "web/app.ts", "line": 0, "kind": "file"},
                {"id": "api/handlers.ts::getUser", "name": "getUser", "path": "api/handlers.ts",
                 "line": 1, "end_line": 3, "kind": "function"},
            ]),
            json!([{"kind": "routes_to", "source": "GET /api/users/:id",
                    "target": "api/handlers.ts::getUser"}]),
        );
        let mapped = route_map(&root, &g, &ScanBudget::default());
        let route = &mapped["routes"][0];

        assert_eq!(route["path"], "/api/users/:id");
        assert_eq!(route["consumers"].as_array().unwrap().len(), 1);
        assert_eq!(
            route["consumers"][0]["accessed_keys"],
            json!(["email", "name"])
        );
        assert_eq!(route["handler_keys"], json!(["email", "name"]));
        assert_eq!(mapped["scan"]["complete"], json!(true));

        let shape = shape_check_over(&mapped, None);
        assert_eq!(shape["checks"][0]["verdict"], "agrees");
        assert_eq!(shape["mismatch_count"], 0);
        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn a_key_the_handler_never_returns_is_a_mismatch() {
        let root = tempdir("mismatch");
        write(
            &root,
            "web/app.ts",
            "const res = await fetch('/api/me');\nconst data = await res.json();\nshow(data.nickname);\n",
        );
        write(
            &root,
            "api/me.ts",
            "export function me() {\n  return { name: 'x' };\n}\n",
        );
        let g = graph(
            json!([
                {"id": "web/app.ts", "name": "app", "path": "web/app.ts", "line": 0, "kind": "file"},
                {"id": "api/me.ts::me", "name": "me", "path": "api/me.ts",
                 "line": 1, "end_line": 3, "kind": "function"},
            ]),
            json!([{"kind": "routes_to", "source": "GET /api/me", "target": "api/me.ts::me"}]),
        );
        let mapped = route_map(&root, &g, &ScanBudget::default());
        let shape = shape_check_over(&mapped, None);
        assert_eq!(shape["mismatch_count"], 1);
        assert_eq!(
            shape["checks"][0]["missing_in_handler"],
            json!(["nickname"])
        );
        assert_eq!(shape["checks"][0]["verdict"], "mismatch");

        let impact = api_impact(&root, &g, &ScanBudget::default(), "/api/me");
        assert_eq!(impact["found"], json!(true));
        assert_eq!(impact["risk"], "medium");
        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn a_truncated_scan_never_reports_a_route_as_safe_to_change() {
        let root = tempdir("budget");
        write(&root, "a.ts", "// nothing here\n");
        write(&root, "b.ts", "fetch('/api/thing');\n");
        let g = graph(
            json!([
                {"id": "a.ts", "name": "a", "path": "a.ts", "line": 0, "kind": "file"},
                {"id": "b.ts", "name": "b", "path": "b.ts", "line": 0, "kind": "file"},
                {"id": "h", "name": "h", "path": "a.ts", "line": 1, "kind": "function"},
            ]),
            json!([{"kind": "routes_to", "source": "GET /api/thing", "target": "h"}]),
        );
        // One file of two: the caller in b.ts is never read.
        let budget = ScanBudget {
            max_files: 1,
            ..ScanBudget::default()
        };
        let impact = api_impact(&root, &g, &budget, "/api/thing");

        assert_eq!(impact["scan"]["complete"], json!(false));
        assert_eq!(impact["scan"]["files_skipped_budget"], 1);
        assert_eq!(impact["consumers"].as_array().unwrap().len(), 0);
        // The whole point: zero consumers from an unfinished scan is `unknown`,
        // never a band that reads as "safe".
        assert_eq!(impact["risk"], "unknown");
        assert!(impact["risk_reason"]
            .as_str()
            .unwrap()
            .contains("not evidence"));

        // The same route with the whole tree read is a different, weaker claim.
        let full = api_impact(&root, &g, &ScanBudget::default(), "/api/thing");
        assert_eq!(full["risk"], "low");
        assert_eq!(full["consumers"].as_array().unwrap().len(), 1);
        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn a_route_nothing_calls_is_not_reported_as_no_risk() {
        let root = tempdir("nocaller");
        write(&root, "a.ts", "export function h() { return {}; }\n");
        let g = graph(
            json!([{"id": "h", "name": "h", "path": "a.ts", "line": 1, "kind": "function"}]),
            json!([{"kind": "routes_to", "source": "GET /api/quiet", "target": "h"}]),
        );
        let impact = api_impact(&root, &g, &ScanBudget::default(), "/api/quiet");
        assert_eq!(impact["risk"], "none_observed");
        assert!(impact["risk_reason"]
            .as_str()
            .unwrap()
            .contains("not proof of no caller"));
        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn a_route_the_kernel_does_not_know_is_not_answered_as_zero_risk() {
        let root = tempdir("unknown");
        let g = graph(json!([]), json!([]));
        let impact = api_impact(&root, &g, &ScanBudget::default(), "/api/nope");
        assert_eq!(impact["found"], json!(false));
        assert_eq!(impact["risk"], "unknown");
        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn fields_the_kernel_cannot_see_are_null_and_say_why() {
        let root = tempdir("caps");
        let g = graph(
            json!([{"id": "h", "name": "h", "path": "a.ts", "line": 1, "kind": "function"}]),
            json!([{"kind": "routes_to", "source": "GET /x", "target": "h"}]),
        );
        let mapped = route_map(&root, &g, &ScanBudget::default());
        // Null, not "" and []: this kernel drops the framework at the store
        // boundary and has no `registers` edge kind.
        assert!(mapped["routes"][0]["framework"].is_null());
        assert!(mapped["routes"][0]["middleware"].is_null());
        assert_eq!(mapped["capabilities"]["framework_available"], json!(false));
        assert_eq!(mapped["capabilities"]["middleware_available"], json!(false));
        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn an_oversized_file_is_counted_rather_than_read() {
        let root = tempdir("big");
        write(
            &root,
            "big.js",
            &format!("fetch('/api/x');{}", " ".repeat(4096)),
        );
        let g = graph(
            json!([{"id": "big.js", "name": "big", "path": "big.js", "line": 0, "kind": "file"}]),
            json!([]),
        );
        let budget = ScanBudget {
            max_file_bytes: 128,
            ..ScanBudget::default()
        };
        let mapped = route_map(&root, &g, &budget);
        assert_eq!(mapped["scan"]["files_over_size"], 1);
        assert_eq!(mapped["scan"]["files_read"], 0);
        std::fs::remove_dir_all(&root).ok();
    }
}
