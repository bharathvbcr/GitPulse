//! Export the graph in formats other tools read.
//!
//! Ported from the GraphML half of `indexing/graph/export.py`. The OKF half of
//! that module is deliberately **not** here: `build_code_graph_okf` writes a
//! DevCouncil knowledge bundle through `devcouncil.knowledge.okf`, whose reader
//! feeds the planning subsystem. That is orchestration, not code intelligence,
//! and it moves with DevCouncil rather than with the map.

use std::borrow::Cow;
use std::collections::{HashMap, HashSet};
use std::fmt::Write as _;

use serde_json::Value;

/// Escape text for an XML attribute or element body.
///
/// `&` first, or the ampersands introduced by the later replacements are
/// escaped a second time and `<` becomes `&amp;lt;`.
fn xml(text: &str) -> String {
    text.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&apos;")
}

/// Whether a character is one XML 1.0 permits at all.
///
/// Most C0 controls are forbidden outright — no escape represents them, and a
/// document containing one is not repairable by a reader. A symbol name can
/// carry anything the source file did.
fn xml_legal(ch: char) -> bool {
    matches!(ch, '\t' | '\n' | '\r')
        || ('\u{20}'..='\u{d7ff}').contains(&ch)
        || ('\u{e000}'..='\u{fffd}').contains(&ch)
        || ('\u{10000}'..='\u{10ffff}').contains(&ch)
}

/// Text with every character XML cannot represent replaced by U+FFFD.
///
/// Dropping them silently would change an identifier without saying so; leaving
/// them emits a document no parser will open. The count of substitutions rides
/// in the export's own summary, so a caller can tell this happened.
fn sanitize(text: &str, replaced: &mut usize) -> String {
    xml(&replace_illegal(text, replaced))
}

/// [`sanitize`] before escaping: the text XML can hold, not yet markup-safe.
fn replace_illegal<'a>(text: &'a str, replaced: &mut usize) -> Cow<'a, str> {
    if text.chars().all(xml_legal) {
        return Cow::Borrowed(text);
    }
    Cow::Owned(
        text.chars()
            .map(|ch| {
                if xml_legal(ch) {
                    ch
                } else {
                    *replaced += 1;
                    '\u{fffd}'
                }
            })
            .collect(),
    )
}

/// The id each node is written under, by its index; `None` for a node whose
/// id repeats an earlier one exactly. Also the written id for every raw id,
/// for the edges.
///
/// Sanitising is not injective: `a\x01.py` and `a\x02.py` both become
/// `a\u{fffd}.py`, and a document declaring one id twice is invalid GraphML
/// whose edges land on whichever node a reader keeps. So ids are assigned
/// before anything is written. Every id that needed no repair keeps its name —
/// a clean id is never renamed to make room. A repaired id that lands on a
/// taken one is written as `<id>~<n>`, the smallest `n` from 2 not taken,
/// visiting nodes in input order, so the same graph always exports the same
/// document. Each such rename is counted in [`GraphmlReport::id_collisions`].
fn assign_ids<'a>(
    nodes: &'a [Value],
    report: &mut GraphmlReport,
) -> (Vec<Option<String>>, HashMap<&'a str, String>) {
    let mut written: Vec<Option<String>> = vec![None; nodes.len()];
    let mut by_raw: HashMap<&'a str, String> = HashMap::with_capacity(nodes.len());
    let mut taken: HashSet<String> = HashSet::with_capacity(nodes.len());
    let mut repaired: Vec<(usize, &'a str)> = Vec::new();
    let mut seen: HashSet<&'a str> = HashSet::with_capacity(nodes.len());
    for (index, node) in nodes.iter().enumerate() {
        let raw = str_field(node, "id");
        if !seen.insert(raw) {
            report.duplicate_nodes += 1;
            continue;
        }
        if raw.chars().all(xml_legal) {
            taken.insert(raw.to_string());
            written[index] = Some(raw.to_string());
            by_raw.insert(raw, raw.to_string());
        } else {
            repaired.push((index, raw));
        }
    }
    for (index, raw) in repaired {
        let base = replace_illegal(raw, &mut report.characters_replaced).into_owned();
        let id = if taken.contains(&base) {
            report.id_collisions += 1;
            // `taken` holds at most one entry per node, so this ends within
            // `nodes.len() + 2` tries.
            let mut n = 2usize;
            loop {
                let candidate = format!("{base}~{n}");
                if !taken.contains(&candidate) {
                    break candidate;
                }
                n += 1;
            }
        } else {
            base
        };
        taken.insert(id.clone());
        written[index] = Some(id.clone());
        by_raw.insert(raw, id);
    }
    (written, by_raw)
}

fn str_field<'a>(value: &'a Value, key: &str) -> &'a str {
    value.get(key).and_then(Value::as_str).unwrap_or("")
}

/// What the export wrote, and what it had to change to write it.
#[derive(Debug, Default, Clone, Copy)]
pub struct GraphmlReport {
    pub nodes: usize,
    pub edges: usize,
    pub edges_dangling: usize,
    pub characters_replaced: usize,
    /// Nodes whose sanitised id was already taken by another node, and which
    /// were written under a disambiguated id instead (`<id>~<n>`).
    pub id_collisions: usize,
    /// Nodes whose id repeats an earlier node's exactly; only the first is
    /// written, and edges naming the id land on it.
    pub duplicate_nodes: usize,
}

/// GraphML with the node and edge attributes the graph carries.
///
/// Returns the document and a report. The report is not decoration: an edge
/// naming a node the document does not declare makes the file invalid for
/// strict readers, and a silently repaired identifier is a changed identifier.
pub fn export_graphml(graph: &Value) -> (String, GraphmlReport) {
    let mut report = GraphmlReport::default();
    let empty = Vec::new();
    let nodes = graph["nodes"].as_array().unwrap_or(&empty);
    let edges = graph["edges"].as_array().unwrap_or(&empty);

    let dead: std::collections::BTreeSet<&str> = graph["dead_code"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|d| d.get("id").and_then(Value::as_str))
        .collect();
    let unwired: std::collections::BTreeSet<&str> = graph["unwired_candidates"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(Value::as_str)
        .collect();
    // A liveness pass that could not establish reachability marks itself
    // unreliable. Exporting its `unreachable_files` as a per-node boolean would
    // hand a downstream tool a claim the producer withdrew.
    let unreliable = graph["liveness_unreachable_unreliable"]
        .as_bool()
        .unwrap_or(false);
    let unreachable: std::collections::BTreeSet<&str> = if unreliable {
        std::collections::BTreeSet::new()
    } else {
        graph["unreachable_files"]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(Value::as_str)
            .collect()
    };

    let mut out = String::with_capacity(nodes.len() * 256);
    out.push_str(
        r#"<?xml version="1.0" encoding="UTF-8"?>
<graphml xmlns="http://graphml.graphdrawing.org/xmlns">
  <key id="kind" for="node" attr.name="kind" attr.type="string"/>
  <key id="path" for="node" attr.name="path" attr.type="string"/>
  <key id="name" for="node" attr.name="name" attr.type="string"/>
  <key id="area" for="node" attr.name="area" attr.type="string"/>
  <key id="community" for="node" attr.name="community" attr.type="string"/>
  <key id="language" for="node" attr.name="language" attr.type="string"/>
  <key id="line" for="node" attr.name="line" attr.type="int"/>
  <key id="dead" for="node" attr.name="dead" attr.type="boolean"/>
  <key id="unwired" for="node" attr.name="unwired" attr.type="boolean"/>
  <key id="unreachable" for="node" attr.name="unreachable" attr.type="boolean"/>
  <key id="liveness_reliable" for="graph" attr.name="liveness_reliable" attr.type="boolean"/>
  <key id="ekind" for="edge" attr.name="kind" attr.type="string"/>
  <key id="confidence" for="edge" attr.name="confidence" attr.type="string"/>
  <graph id="G" edgedefault="directed">
"#,
    );
    // Carried on the graph, not per node: with this false, every `unreachable`
    // below is false because the pass withdrew its answer, not because the file
    // is reached.
    let _ = writeln!(
        out,
        r#"    <data key="liveness_reliable">{}</data>"#,
        !unreliable
    );

    let (written_ids, by_raw) = assign_ids(nodes, &mut report);
    for (node, written_id) in nodes.iter().zip(&written_ids) {
        let Some(written_id) = written_id else {
            continue;
        };
        let id = str_field(node, "id");
        let path = str_field(node, "path");
        let community = {
            let raw = str_field(node, "community").trim();
            if raw.is_empty() {
                str_field(node, "area")
            } else {
                raw
            }
        };
        let flag = |set: &std::collections::BTreeSet<&str>| {
            if set.contains(id) || (!path.is_empty() && set.contains(path)) {
                "true"
            } else {
                "false"
            }
        };
        let _ = write!(
            out,
            r#"    <node id="{}">
      <data key="kind">{}</data>
      <data key="path">{}</data>
      <data key="name">{}</data>
      <data key="area">{}</data>
      <data key="community">{}</data>
      <data key="language">{}</data>
      <data key="line">{}</data>
      <data key="dead">{}</data>
      <data key="unwired">{}</data>
      <data key="unreachable">{}</data>
    </node>
"#,
            xml(written_id),
            sanitize(str_field(node, "kind"), &mut report.characters_replaced),
            sanitize(path, &mut report.characters_replaced),
            sanitize(str_field(node, "name"), &mut report.characters_replaced),
            sanitize(str_field(node, "area"), &mut report.characters_replaced),
            sanitize(community, &mut report.characters_replaced),
            sanitize(str_field(node, "language"), &mut report.characters_replaced),
            node.get("line").and_then(Value::as_u64).unwrap_or(0),
            flag(&dead),
            flag(&unwired),
            flag(&unreachable),
        );
        report.nodes += 1;
    }

    for (index, edge) in edges.iter().enumerate() {
        let source = str_field(edge, "source");
        let target = str_field(edge, "target");
        // GraphML requires both endpoints to be declared nodes. The graph can
        // carry an edge to a name no node owns — a route handler, an unresolved
        // import — and emitting it produces a document strict readers reject.
        let (Some(source), Some(target)) = (by_raw.get(source), by_raw.get(target)) else {
            report.edges_dangling += 1;
            continue;
        };
        let _ = write!(
            out,
            r#"    <edge id="e{index}" source="{}" target="{}">
      <data key="ekind">{}</data>
      <data key="confidence">{}</data>
    </edge>
"#,
            xml(source),
            xml(target),
            sanitize(str_field(edge, "kind"), &mut report.characters_replaced),
            sanitize(
                str_field(edge, "confidence"),
                &mut report.characters_replaced
            ),
        );
        report.edges += 1;
    }

    out.push_str("  </graph>\n</graphml>\n");
    (out, report)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn graph() -> Value {
        json!({
            "nodes": [
                {"id": "a.py", "kind": "file", "path": "a.py", "name": "a", "area": "src",
                 "community": "", "language": "python", "line": 0},
                {"id": "a.py::run", "kind": "function", "path": "a.py", "name": "run",
                 "area": "src", "community": "c1", "language": "python", "line": 3},
            ],
            "edges": [
                {"source": "a.py", "target": "a.py::run", "kind": "defines",
                 "confidence": "extracted"},
            ],
            "dead_code": [{"id": "a.py::run"}],
            "unwired_candidates": [],
            "unreachable_files": [],
        })
    }

    #[test]
    fn the_document_declares_every_node_it_draws_an_edge_between() {
        let (xml, report) = export_graphml(&graph());
        assert_eq!(report.nodes, 2);
        assert_eq!(report.edges, 1);
        assert_eq!(report.edges_dangling, 0);
        assert!(xml.contains(r#"<node id="a.py::run">"#));
        assert!(xml.contains(r#"<edge id="e0" source="a.py" target="a.py::run">"#));
        assert!(xml.trim_end().ends_with("</graphml>"));
    }

    #[test]
    fn an_edge_to_an_undeclared_node_is_dropped_and_counted() {
        let mut g = graph();
        // A route handler edge names a bare symbol no node owns.
        g["edges"].as_array_mut().unwrap().push(json!({
            "source": "GET /health", "target": "health", "kind": "routes_to",
            "confidence": "extracted"
        }));
        let (xml, report) = export_graphml(&g);
        assert_eq!(report.edges, 1, "only the well-formed edge is written");
        assert_eq!(report.edges_dangling, 1);
        assert!(
            !xml.contains("routes_to"),
            "an edge whose endpoints are not declared makes the document invalid"
        );
    }

    #[test]
    fn a_dead_node_carries_the_flag_and_a_live_one_does_not() {
        let (xml, _) = export_graphml(&graph());
        let run = xml.split(r#"<node id="a.py::run">"#).nth(1).unwrap();
        let file = xml.split(r#"<node id="a.py">"#).nth(1).unwrap();
        assert!(run.contains(r#"<data key="dead">true</data>"#));
        assert!(file.contains(r#"<data key="dead">false</data>"#));
    }

    #[test]
    fn an_unreliable_liveness_pass_does_not_export_its_withdrawn_answer() {
        let mut g = graph();
        g["unreachable_files"] = json!(["a.py"]);
        let (reliable, _) = export_graphml(&g);
        assert!(reliable.contains(r#"<data key="unreachable">true</data>"#));
        assert!(reliable.contains(r#"<data key="liveness_reliable">true</data>"#));

        g["liveness_unreachable_unreliable"] = json!(true);
        let (withdrawn, _) = export_graphml(&g);
        assert!(
            !withdrawn.contains(r#"<data key="unreachable">true</data>"#),
            "a pass that withdrew its answer must not export it as a fact"
        );
        // And the reader can tell why every flag is false.
        assert!(withdrawn.contains(r#"<data key="liveness_reliable">false</data>"#));
    }

    #[test]
    fn markup_in_a_symbol_name_cannot_open_a_tag() {
        let mut g = graph();
        g["nodes"][1]["name"] = json!(r#"</node><node id="x"/>&<"#);
        let (xml, _) = export_graphml(&g);
        assert!(!xml.contains(r#"<node id="x"/>"#));
        assert_eq!(
            xml.matches("<node ").count(),
            2,
            "escaping must not let a name declare a third node"
        );
        // `&<` escapes to `&amp;&lt;` — each character once. `&amp;lt;` here
        // would mean the ampersand pass ran over its own output.
        assert!(xml.contains("&amp;&lt;"), "a literal `&<` survives as text");
        assert!(!xml.contains("&amp;amp;"), "double-escaped");
    }

    #[test]
    fn a_character_xml_cannot_represent_is_replaced_and_counted() {
        let mut g = graph();
        // U+0007 is forbidden by XML 1.0 outright — no escape represents it.
        g["nodes"][1]["name"] = json!("run\u{7}x");
        let (xml, report) = export_graphml(&g);
        assert_eq!(report.characters_replaced, 1);
        assert!(
            !xml.contains('\u{7}'),
            "an unrepresentable byte must not ship"
        );
        assert!(xml.contains("run\u{fffd}x"));

        // A clean graph reports none, so the count means something.
        assert_eq!(export_graphml(&graph()).1.characters_replaced, 0);
    }

    fn node_ids(xml: &str) -> Vec<&str> {
        xml.split(r#"<node id=""#)
            .skip(1)
            .map(|rest| &rest[..rest.find('"').unwrap()])
            .collect()
    }

    /// Two ids that differ only in characters XML cannot hold both sanitise to
    /// `a\u{fffd}.py`. Emitted as is, the document declares one id twice and
    /// every edge to either lands on whichever a reader keeps.
    #[test]
    fn ids_that_sanitise_alike_stay_distinct_and_the_collision_is_counted() {
        let g = json!({
            "nodes": [
                {"id": "a\u{1}.py", "kind": "file", "path": "a\u{1}.py"},
                {"id": "a\u{2}.py", "kind": "file", "path": "a\u{2}.py"},
                {"id": "b.py", "kind": "file", "path": "b.py"},
            ],
            "edges": [
                {"source": "b.py", "target": "a\u{1}.py", "kind": "imports"},
                {"source": "b.py", "target": "a\u{2}.py", "kind": "imports"},
            ],
        });
        let (xml, report) = export_graphml(&g);
        let ids = node_ids(&xml);
        let unique: std::collections::BTreeSet<&str> = ids.iter().copied().collect();
        assert_eq!(ids.len(), 3);
        assert_eq!(unique.len(), 3, "duplicate node ids: {ids:?}");
        assert_eq!(report.id_collisions, 1);
        // Each edge lands on its own node, not on whichever shares its text.
        assert!(xml.contains(&format!(r#"source="b.py" target="{}""#, ids[0])));
        assert!(xml.contains(&format!(r#"source="b.py" target="{}""#, ids[1])));
        // Deterministic: the same graph exports the same bytes.
        assert_eq!(export_graphml(&g).0, xml);
    }

    /// A clean id is never renamed to make room for a sanitised one, even
    /// when the sanitised one comes first.
    #[test]
    fn a_clean_id_keeps_its_name_when_a_sanitised_one_matches_it() {
        let g = json!({
            "nodes": [
                {"id": "a\u{1}.py", "kind": "file"},
                {"id": "a\u{fffd}.py", "kind": "file"},
                {"id": "a\u{fffd}.py~2", "kind": "file"},
            ],
        });
        let (xml, report) = export_graphml(&g);
        let ids = node_ids(&xml);
        let unique: std::collections::BTreeSet<&str> = ids.iter().copied().collect();
        assert_eq!(unique.len(), 3, "duplicate node ids: {ids:?}");
        assert!(ids.contains(&"a\u{fffd}.py"));
        assert!(ids.contains(&"a\u{fffd}.py~2"));
        assert_eq!(report.id_collisions, 1);
    }

    /// The same id declared twice is one node; the second is not written.
    #[test]
    fn a_repeated_id_is_declared_once_and_counted() {
        let mut g = graph();
        g["nodes"]
            .as_array_mut()
            .unwrap()
            .push(json!({"id": "a.py", "kind": "file"}));
        let (xml, report) = export_graphml(&g);
        assert_eq!(xml.matches(r#"<node id="a.py">"#).count(), 1);
        assert_eq!(report.nodes, 2);
        assert_eq!(report.duplicate_nodes, 1);
        assert_eq!(export_graphml(&graph()).1.duplicate_nodes, 0);
    }

    #[test]
    fn a_node_with_no_community_falls_back_to_its_area() {
        let (xml, _) = export_graphml(&graph());
        let file = xml.split(r#"<node id="a.py">"#).nth(1).unwrap();
        assert!(file.contains(r#"<data key="community">src</data>"#));
    }

    #[test]
    fn an_empty_graph_is_still_a_well_formed_document() {
        let (xml, report) = export_graphml(&json!({}));
        assert_eq!((report.nodes, report.edges), (0, 0));
        assert!(xml.starts_with("<?xml"));
        assert!(xml.trim_end().ends_with("</graphml>"));
    }
}
