//! A minimal openCypher subset over the graph.
//!
//! Ported from `src/devcouncil/indexing/graph/cypher.py`. The supported shape is
//! exactly one pattern:
//!
//! ```text
//! MATCH (a)[-[r:REL|REL]->(b)] [WHERE …] RETURN a[, r][, b] [LIMIT n]
//! ```
//!
//! # Everything here refuses rather than widens
//!
//! Five separate places could turn "I did not understand your question" into
//! "here is an answer", and all five are closed:
//!
//! * **The pattern is read, not skipped over.** The parser used to take
//!   whatever lay between `MATCH ` and ` RETURN ` and look only for `-[`, so
//!   `MATCH (((( RETURN ))))` — read back from the release binary on a real
//!   store — answered `ok: true` with fifty rows and `total: 18404`. Now the
//!   pattern is `(a)` or `(a)-[r:…]->(b)` exactly. The variables are fixed
//!   because the `WHERE` grammar and the row keys name them; a node label is
//!   refused rather than ignored, because this engine filters on nothing a
//!   label names and `(a:Function)` answering with every kind is the same
//!   silent over-answer.
//! * **`RETURN` names what the pattern bound.** The projection was never read,
//!   so `RETURN ))))` and `RETURN c` both answered with the pattern's rows. The
//!   row is the same whatever is projected — this engine projects nothing — so
//!   an item it cannot honour is refused instead of quietly not honoured.
//! * **An unreadable `WHERE` term is a refusal.** Dropping the term it could not
//!   parse and running the rest returns *every* row under `ok: true` — a
//!   strictly wrong answer to the question asked. `OR` is the sharpest case: an
//!   engine that only conjoins cannot honour it, so accepting it would silently
//!   run a different query.
//! * **`LIMIT` is a request, not an instruction.** `LIMIT 999999999` is caller
//!   text; the ceiling is the server's. Both numbers are reported, so a capped
//!   answer cannot be mistaken for the whole one.
//! * **`total` counts every match, not the ones that fit.** The Python loop used
//!   to `break` at the cap, which made `count` equal the cap: a query over 5,000
//!   matches and one over exactly 50 were indistinguishable.
//!
//! # The relationship vocabulary is the emitter's
//!
//! Validated against [`crate::EDGE_KIND_LABELS`] rather than a list restated
//! here. The Python version restated it, and drifted: it accepted `EXTENDS` and
//! `DECORATES` while the graph emits `inherits` and never emits a decoration
//! edge at all. Both queries parsed, both were "supported", and both matched
//! nothing — reported as an empty result rather than as a name that cannot
//! match. `EXTENDS` is kept as an explicit alias for `inherits`, and the answer
//! says which kind it actually ran against.

use serde_json::{json, Map, Value};

/// Most rows one query may materialise, whatever `LIMIT` asked for.
///
/// Matches the store's own search ceiling so the two surfaces agree about what
/// a large answer is.
pub const MAX_ROW_LIMIT: usize = 500;

/// Relationship names a caller may write that are not the emitted label.
///
/// One entry, and it is a rename rather than a synonym: the resolver's
/// `EdgeKind::Extends` is emitted as `inherits`. Written down so a query
/// carried over from the Python subset keeps working *and* the answer can say
/// which kind it ran against, instead of matching nothing.
const RELATIONSHIP_ALIASES: &[(&str, &str)] = &[("extends", "inherits")];

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct WhereClause {
    /// `contains(a.name, '…')`
    pub name_filter: Option<String>,
    /// `starts with(b.path, '…')`
    pub path_prefix: Option<String>,
    /// Terms nobody could read.
    ///
    /// The point of the type. A parser that returned only the filters it
    /// understood reported an unreadable clause as "no filters", and the query
    /// then matched every row with nothing in the payload saying the filter had
    /// not run.
    pub unparsed: Vec<String>,
}

/// Split a `WHERE` clause into recognised filters and unrecognised terms.
///
/// Terms are conjunctions: `A AND B`. Anything else — an `OR`, a `NOT`, an
/// infix comparison, a field this subset does not index — lands in `unparsed`
/// rather than being dropped.
pub fn parse_where(clause: &str) -> WhereClause {
    let mut parsed = WhereClause::default();
    if clause.trim().is_empty() {
        return parsed;
    }
    // An engine that only conjoins cannot honour a disjunction, and accepting
    // one would run a strictly narrower query than the caller asked for. Caught
    // before the AND split, because `a OR b AND c` would otherwise be split into
    // terms that each look fine on their own.
    if split_on_keyword(clause, "OR").len() > 1 {
        parsed.unparsed.push(clause.trim().to_string());
        return parsed;
    }
    for term in split_on_keyword(clause, "AND") {
        let text = term.trim();
        if text.is_empty() {
            continue;
        }
        if let Some(value) = call_argument(text, "contains", "a.name") {
            parsed.name_filter = Some(value);
        } else if let Some(value) = call_argument(text, "starts with", "b.path") {
            parsed.path_prefix = Some(value);
        } else {
            parsed.unparsed.push(text.to_string());
        }
    }
    parsed
}

/// Split on a bare keyword, case-insensitively, respecting quotes.
///
/// A naive split would cut `contains(a.name, 'AND')` in half and then fail to
/// parse both halves — reporting a perfectly good filter as unreadable.
fn split_on_keyword<'a>(text: &'a str, keyword: &str) -> Vec<&'a str> {
    // ASCII folding only: offsets found in the folded copy slice `text`, and
    // Unicode case mapping changes byte lengths (`ﬁ` → `FI`, `ŉ` → `ʼN`). With
    // `to_uppercase()` every offset past such a character in a filter value
    // was off by one — `'ŉx') AND …` split as `'ŉx') A` — and a value that
    // grew enough would slice inside a character and panic. The keywords are
    // ASCII, so ASCII folding finds exactly the same ones.
    let upper = text.to_ascii_uppercase();
    let keyword = keyword.to_ascii_uppercase();
    let bytes = upper.as_bytes();
    let mut parts = Vec::new();
    let mut start = 0usize;
    let mut index = 0usize;
    let mut quote: Option<u8> = None;

    while index < bytes.len() {
        let byte = bytes[index];
        match quote {
            Some(open) => {
                if byte == open {
                    quote = None;
                }
                index += 1;
                continue;
            }
            None if byte == b'\'' || byte == b'"' => {
                quote = Some(byte);
                index += 1;
                continue;
            }
            None => {}
        }
        let ends_word = |at: usize| -> bool {
            at == 0 || at >= bytes.len() || !bytes[at].is_ascii_alphanumeric() && bytes[at] != b'_'
        };
        if upper[index..].starts_with(&keyword)
            && ends_word(index.wrapping_sub(1).min(bytes.len()))
            && (index == 0 || !bytes[index - 1].is_ascii_alphanumeric())
            && ends_word(index + keyword.len())
        {
            parts.push(&text[start..index]);
            index += keyword.len();
            start = index;
            continue;
        }
        index += 1;
    }
    parts.push(&text[start..]);
    parts
}

/// Match `name(field, 'value')` exactly, returning the quoted value.
///
/// Anchored on the *whole* term, and strictly: the closing parenthesis must be
/// the one that matches the opening parenthesis, and the quoted value must run
/// to just before it.
///
/// The loose version of this check — "starts with `contains(` and ends with
/// `)`" — accepted `contains(a.name, 'x') OR contains(a.name, 'y')` as a single
/// `contains` filter on `x`, silently running half the query that was asked and
/// reporting `ok: true`. That is the exact failure this module exists to
/// prevent, and it got in through the parser.
fn call_argument(term: &str, name: &str, field: &str) -> Option<String> {
    let term = term.trim();
    // The head is matched case-insensitively and with flexible spacing, because
    // `starts with (b.path, …)` is written every way a person writes it.
    let squashed: String = term.split_whitespace().collect::<Vec<_>>().join(" ");
    // ASCII folding, for the same reason as `split_on_keyword`: `head.len()`
    // is used as an offset into `squashed`.
    let lower = squashed.to_ascii_lowercase();
    let head = format!("{name}(");
    let head_alt = format!("{name} (");
    let open_at = if lower.starts_with(&head) {
        head.len()
    } else if lower.starts_with(&head_alt) {
        head_alt.len()
    } else {
        return None;
    };

    // The matching close paren, respecting quotes. It must be the last
    // character of the term: anything after it is a second expression this
    // engine cannot evaluate.
    let bytes = squashed.as_bytes();
    let mut depth = 1usize;
    let mut quote: Option<u8> = None;
    let mut close_at = None;
    for (index, byte) in bytes.iter().enumerate().skip(open_at) {
        match quote {
            Some(open) if *byte == open => quote = None,
            Some(_) => {}
            None => match *byte {
                b'\'' | b'"' => quote = Some(*byte),
                b'(' => depth += 1,
                b')' => {
                    depth -= 1;
                    if depth == 0 {
                        close_at = Some(index);
                        break;
                    }
                }
                _ => {}
            },
        }
    }
    let close_at = close_at?;
    if close_at != bytes.len() - 1 {
        return None;
    }

    let inner = &squashed[open_at..close_at];
    let (lhs, rhs) = inner.split_once(',')?;
    if lhs.trim().to_lowercase() != field {
        return None;
    }
    let value = rhs.trim();
    let open = value.chars().next()?;
    if open != '\'' && open != '"' {
        return None;
    }
    // The closing quote must be the final character: `'x') or contains(a.name,
    // 'y'` also starts and ends with a quote, and accepting it is the bug above.
    if value.len() < 2 || !value.ends_with(open) {
        return None;
    }
    let unquoted = &value[open.len_utf8()..value.len() - open.len_utf8()];
    if unquoted.contains(open) {
        return None;
    }
    Some(unquoted.to_string())
}

/// The parsed form of a supported query.
#[derive(Debug, Clone)]
struct Pattern {
    relationships: Vec<String>,
    where_clause: WhereClause,
    limit_requested: usize,
}

/// Parse the one supported pattern, or say why it is not supported.
fn parse_query(query: &str, default_limit: usize) -> Result<Pattern, Value> {
    let normalized = query.split_whitespace().collect::<Vec<_>>().join(" ");
    // ASCII folding: see `split_on_keyword`. These offsets slice `normalized`.
    let upper = normalized.to_ascii_uppercase();

    for clause in ["CREATE", "DELETE", "MERGE", "SET", "REMOVE", "DETACH"] {
        if upper
            .split(|c: char| !c.is_ascii_alphabetic())
            .any(|w| w == clause)
        {
            return Err(json!({
                "ok": false,
                "code": "mutating_clause",
                "error": "Mutating Cypher clauses are not supported. This is a read-only index."
            }));
        }
    }

    let Some(match_at) = upper.find("MATCH ") else {
        return Err(unsupported(&normalized));
    };
    let Some(return_at) = upper.find(" RETURN ") else {
        return Err(unsupported(&normalized));
    };
    if return_at < match_at {
        return Err(unsupported(&normalized));
    }

    let pattern_and_where = &normalized[match_at + "MATCH ".len()..return_at];
    let tail = &normalized[return_at + " RETURN ".len()..];

    // LIMIT, if present, closes the query; what precedes it is the projection.
    let tail_upper = tail.to_ascii_uppercase();
    let (projection, limit_requested) = match tail_upper.rfind(" LIMIT ") {
        Some(at) => (
            &tail[..at],
            tail[at + " LIMIT ".len()..]
                .trim()
                .parse::<usize>()
                .map_err(|_| unsupported(&normalized))?,
        ),
        None => (tail, default_limit),
    };

    let upper_pw = pattern_and_where.to_ascii_uppercase();
    let (pattern, where_text) = match upper_pw.find(" WHERE ") {
        Some(at) => (
            &pattern_and_where[..at],
            &pattern_and_where[at + " WHERE ".len()..],
        ),
        None => (pattern_and_where, ""),
    };

    let bound = parse_pattern(pattern).ok_or_else(|| unsupported(&normalized))?;
    if let Err(item) = validate_return(projection, &bound.variables) {
        return Err(unsupported_return(&normalized, &item, &bound.variables));
    }
    let relationships = bound.relationships;

    let mut resolved = Vec::with_capacity(relationships.len());
    for name in &relationships {
        let canonical = RELATIONSHIP_ALIASES
            .iter()
            .find(|(alias, _)| alias == name)
            .map(|(_, canonical)| (*canonical).to_string())
            .unwrap_or_else(|| name.clone());
        if !crate::EDGE_KIND_LABELS.contains(&canonical.as_str()) {
            return Err(json!({
                "ok": false,
                "code": "unknown_relationship",
                "error": format!(
                    "Unknown relationship type '{name}'. This graph emits: {}. \
            Refusing rather than returning an empty result, which would read as 'nothing matches' \
            rather than 'that name cannot match'.",
                    crate::EDGE_KIND_LABELS.join(", ")
                ),
                "known_relationships": crate::EDGE_KIND_LABELS,
            }));
        }
        resolved.push(canonical);
    }

    Ok(Pattern {
        relationships: resolved,
        where_clause: parse_where(where_text),
        limit_requested,
    })
}

/// What a pattern binds: its variables, and the relationship names between them.
struct BoundPattern {
    variables: Vec<&'static str>,
    relationships: Vec<String>,
}

/// Read `(a)` or `(a)-[r:k1|k2]->(b)`, and nothing else.
///
/// The variables are fixed as `a` and `b` because the `WHERE` grammar names
/// them (`contains(a.name, …)`, `starts with(b.path, …)`) and the rows are
/// keyed `a_*`/`b_*`: a pattern binding `x` would run with filters that can
/// never apply. `r` is bound only when the relationship is written `r:…`.
/// A label is refused rather than ignored — see the module docs.
fn parse_pattern(pattern: &str) -> Option<BoundPattern> {
    let rest = pattern.trim().strip_prefix('(')?;
    let close = rest.find(')')?;
    if rest[..close].trim() != "a" {
        return None;
    }
    let rest = rest[close + 1..].trim();
    if rest.is_empty() {
        return Some(BoundPattern {
            variables: vec!["a"],
            relationships: Vec::new(),
        });
    }
    let rest = rest.strip_prefix("-[")?;
    let close = rest.find(']')?;
    let spec = rest[..close].trim();
    let rest = rest[close + 1..]
        .trim()
        .strip_prefix("->")?
        .trim()
        .strip_prefix('(')?;
    let close = rest.find(')')?;
    if rest[..close].trim() != "b" || !rest[close + 1..].trim().is_empty() {
        return None;
    }
    let (binds_r, names) = match spec.strip_prefix("r:") {
        Some(names) => (true, names),
        None => (false, spec.strip_prefix(':').unwrap_or(spec)),
    };
    let relationships: Vec<String> = names
        .split('|')
        .map(|name| name.trim().to_lowercase())
        .filter(|name| !name.is_empty())
        .collect();
    if relationships.is_empty() {
        return None;
    }
    let variables = if binds_r {
        vec!["a", "r", "b"]
    } else {
        vec!["a", "b"]
    };
    Some(BoundPattern {
        variables,
        relationships,
    })
}

/// The fields a row carries for a node variable, and so the only projections
/// this engine can honour.
const ROW_FIELDS: &[&str] = &["id", "name", "path", "kind"];

/// Every projected item names a variable the pattern bound, bare or with one
/// of [`ROW_FIELDS`]. Returns the first item it cannot honour.
fn validate_return(projection: &str, bound: &[&str]) -> Result<(), String> {
    let projection = projection.trim();
    if projection.is_empty() {
        return Err(String::new());
    }
    for item in projection.split(',') {
        let item = item.trim();
        let (variable, field) = match item.split_once('.') {
            Some((variable, field)) => (variable.trim(), Some(field.trim())),
            None => (item, None),
        };
        if !bound.contains(&variable) {
            return Err(item.to_string());
        }
        if let Some(field) = field {
            if variable == "r" || !ROW_FIELDS.contains(&field) {
                return Err(item.to_string());
            }
        }
    }
    Ok(())
}

fn unsupported_return(query: &str, item: &str, bound: &[&str]) -> Value {
    let fields = ROW_FIELDS
        .iter()
        .map(|field| format!("a.{field}"))
        .collect::<Vec<_>>()
        .join(", ");
    json!({
        "ok": false,
        "code": "unsupported_return",
        "error": format!(
            "Unsupported RETURN item {item:?}. This engine returns the bound variables' rows \
             whole: RETURN {}, or a field of a node ({fields}). Refusing rather than answering \
             with rows the caller did not ask for.",
            bound.join(", ")
        ),
        "query": query,
    })
}

fn unsupported(query: &str) -> Value {
    json!({
        "ok": false,
        "code": "unsupported_query",
        "error": "Unsupported Cypher. Supported: \
    MATCH (a)-[r:calls|imports|…]->(b) WHERE … RETURN a, b LIMIT n",
        "query": query,
    })
}

fn text<'a>(value: &'a Value, key: &str) -> &'a str {
    value.get(key).and_then(Value::as_str).unwrap_or("")
}

/// Run a query against an already-built graph value.
pub fn run(graph: &Value, query: &str, default_limit: usize) -> Value {
    let pattern = match parse_query(query, default_limit) {
        Ok(pattern) => pattern,
        Err(error) => return error,
    };

    if !pattern.where_clause.unparsed.is_empty() {
        return json!({
            "ok": false,
            "code": "unsupported_where",
            "error": format!(
                "Unsupported WHERE term(s): {}. Supported: contains(a.name, '…') and \
        starts with(b.path, '…'), joined by AND. Refusing rather than returning unfiltered rows.",
                pattern.where_clause.unparsed.join("; ")
            ),
            "unparsed_where": pattern.where_clause.unparsed,
        });
    }

    let limit = pattern.limit_requested.clamp(1, MAX_ROW_LIMIT);
    let empty = Vec::new();
    let nodes = graph
        .get("nodes")
        .and_then(Value::as_array)
        .unwrap_or(&empty);
    let edges = graph
        .get("edges")
        .and_then(Value::as_array)
        .unwrap_or(&empty);
    let by_id: std::collections::BTreeMap<&str, &Value> =
        nodes.iter().map(|node| (text(node, "id"), node)).collect();

    let name_filter = pattern
        .where_clause
        .name_filter
        .as_deref()
        .map(str::to_lowercase);
    let path_prefix = pattern.where_clause.path_prefix.as_deref();

    let mut rows: Vec<Value> = Vec::new();
    // Counted over every match, never over the page. See the module docs.
    let mut total = 0usize;

    if pattern.relationships.is_empty() {
        for node in nodes {
            if let Some(needle) = &name_filter {
                if !text(node, "name").to_lowercase().contains(needle) {
                    continue;
                }
            }
            total += 1;
            if rows.len() < limit {
                rows.push(json!({
                    "a_id": text(node, "id"),
                    "a_name": text(node, "name"),
                    "a_path": text(node, "path"),
                    "a_kind": text(node, "kind"),
                }));
            }
        }
    } else {
        for edge in edges {
            if !pattern
                .relationships
                .iter()
                .any(|kind| kind == text(edge, "kind"))
            {
                continue;
            }
            let (Some(a), Some(b)) = (
                by_id.get(text(edge, "source")),
                by_id.get(text(edge, "target")),
            ) else {
                continue;
            };
            if let Some(needle) = &name_filter {
                if !text(a, "name").to_lowercase().contains(needle) {
                    continue;
                }
            }
            if let Some(prefix) = path_prefix {
                if !text(b, "path").starts_with(prefix) {
                    continue;
                }
            }
            total += 1;
            if rows.len() < limit {
                rows.push(json!({
                    "a_id": text(a, "id"), "a_name": text(a, "name"),
                    "a_path": text(a, "path"), "a_kind": text(a, "kind"),
                    "rel": text(edge, "kind"),
                    "b_id": text(b, "id"), "b_name": text(b, "name"),
                    "b_path": text(b, "path"), "b_kind": text(b, "kind"),
                }));
            }
        }
    }

    let mut result = Map::new();
    result.insert("ok".into(), json!(true));
    result.insert("shown".into(), json!(rows.len()));
    result.insert("total".into(), json!(total));
    result.insert("truncated".into(), json!(rows.len() < total));
    result.insert("limit_requested".into(), json!(pattern.limit_requested));
    result.insert("limit_applied".into(), json!(limit));
    result.insert(
        "limit_capped".into(),
        json!(limit != pattern.limit_requested),
    );
    // The kinds actually queried, after alias resolution — so a caller who wrote
    // `EXTENDS` can see the answer came from `inherits`.
    result.insert("relationships".into(), json!(pattern.relationships));
    result.insert("rows".into(), json!(rows));
    Value::Object(result)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn graph() -> Value {
        json!({
            "nodes": [
                {"id": "a.py", "name": "a.py", "kind": "file", "path": "a.py"},
                {"id": "b.py", "name": "b.py", "kind": "file", "path": "src/b.py"},
                {"id": "a.py::run", "name": "run", "kind": "function", "path": "a.py"},
                {"id": "b.py::helper", "name": "helper", "kind": "function", "path": "src/b.py"}
            ],
            "edges": [
                {"source": "a.py", "target": "b.py", "kind": "imports"},
                {"source": "a.py::run", "target": "b.py::helper", "kind": "calls"},
                {"source": "a.py::run", "target": "b.py::helper", "kind": "inherits"}
            ]
        })
    }

    #[test]
    fn a_mutating_clause_is_refused() {
        let result = run(&graph(), "MATCH (a) DELETE a RETURN a", 50);
        assert_eq!(result["ok"], json!(false));
        assert_eq!(result["code"], json!("mutating_clause"));
    }

    #[test]
    fn an_unreadable_where_term_refuses_rather_than_matching_everything() {
        // The defect this guards: dropping the term it could not parse returns
        // every row under `ok: true` — a strictly wrong answer to the question
        // that was asked.
        let result = run(
            &graph(),
            "MATCH (a)-[r:calls]->(b) WHERE a.kind = 'function' RETURN a, b",
            50,
        );
        assert_eq!(result["ok"], json!(false));
        assert_eq!(result["code"], json!("unsupported_where"));
        assert_eq!(result["unparsed_where"], json!(["a.kind = 'function'"]));
    }

    #[test]
    fn an_or_is_refused_because_this_engine_only_conjoins() {
        let result = run(
            &graph(),
            "MATCH (a)-[r:calls]->(b) WHERE contains(a.name, 'x') OR contains(a.name, 'y') \
RETURN a, b",
            50,
        );
        assert_eq!(result["ok"], json!(false), "{result}");
        assert_eq!(result["code"], json!("unsupported_where"));
    }

    #[test]
    fn a_relationship_the_graph_cannot_emit_is_named_not_answered_empty() {
        // The Python subset accepted `DECORATES`, the graph never emitted it,
        // and the query returned `ok: true` with zero rows — which reads as
        // "nothing matches" rather than "that name cannot match".
        let result = run(&graph(), "MATCH (a)-[r:DECORATES]->(b) RETURN a, b", 50);
        assert_eq!(result["ok"], json!(false));
        assert_eq!(result["code"], json!("unknown_relationship"));
        assert!(result["error"].as_str().unwrap().contains("inherits"));
    }

    #[test]
    fn extends_still_works_and_says_it_ran_as_inherits() {
        // The one rename: the resolver's `Extends` is emitted as `inherits`. A
        // query carried over from the Python subset keeps working, and the
        // answer says which kind it actually matched.
        let result = run(&graph(), "MATCH (a)-[r:EXTENDS]->(b) RETURN a, b", 50);
        assert_eq!(result["ok"], json!(true), "{result}");
        assert_eq!(result["relationships"], json!(["inherits"]));
        assert_eq!(result["total"], json!(1));
    }

    #[test]
    fn total_counts_every_match_not_the_page() {
        // The Python loop broke at the cap, so `count` equalled the cap: a
        // query over 5,000 matches and one over exactly 50 were
        // indistinguishable.
        let result = run(&graph(), "MATCH (a) RETURN a LIMIT 2", 50);
        assert_eq!(result["shown"], json!(2));
        assert_eq!(result["total"], json!(4));
        assert_eq!(result["truncated"], json!(true));
    }

    #[test]
    fn a_caller_supplied_limit_cannot_exceed_the_servers_ceiling() {
        let result = run(&graph(), "MATCH (a) RETURN a LIMIT 999999999", 50);
        assert_eq!(result["limit_requested"], json!(999999999u64));
        assert_eq!(result["limit_applied"], json!(MAX_ROW_LIMIT));
        assert_eq!(result["limit_capped"], json!(true));
    }

    #[test]
    fn the_two_supported_filters_actually_filter() {
        let named = run(
            &graph(),
            "MATCH (a)-[r:calls]->(b) WHERE contains(a.name, 'run') RETURN a, b",
            50,
        );
        assert_eq!(named["total"], json!(1), "{named}");

        let missed = run(
            &graph(),
            "MATCH (a)-[r:calls]->(b) WHERE contains(a.name, 'nope') RETURN a, b",
            50,
        );
        assert_eq!(missed["total"], json!(0));

        let prefixed = run(
            &graph(),
            "MATCH (a)-[r:imports]->(b) WHERE starts with(b.path, 'src/') RETURN a, b",
            50,
        );
        assert_eq!(prefixed["total"], json!(1), "{prefixed}");
    }

    #[test]
    fn both_filters_conjoin() {
        let result = run(
            &graph(),
            "MATCH (a)-[r:calls]->(b) WHERE contains(a.name, 'run') AND \
starts with(b.path, 'src/') RETURN a, b",
            50,
        );
        assert_eq!(result["total"], json!(1), "{result}");

        let contradictory = run(
            &graph(),
            "MATCH (a)-[r:calls]->(b) WHERE contains(a.name, 'run') AND \
starts with(b.path, 'nowhere/') RETURN a, b",
            50,
        );
        assert_eq!(contradictory["total"], json!(0));
    }

    #[test]
    fn a_quoted_and_does_not_split_the_term() {
        // A naive split cuts `contains(a.name, 'AND')` in half and then reports
        // a perfectly good filter as unreadable.
        let parsed = parse_where("contains(a.name, 'AND')");
        assert_eq!(parsed.name_filter.as_deref(), Some("AND"));
        assert!(parsed.unparsed.is_empty(), "{parsed:?}");
    }

    #[test]
    fn a_negated_term_is_not_read_as_the_positive_one() {
        // Matching a fragment inside `NOT contains(...)` would apply the
        // opposite filter and report success.
        let parsed = parse_where("NOT contains(a.name, 'run')");
        assert_eq!(parsed.name_filter, None);
        assert_eq!(parsed.unparsed.len(), 1);
    }

    #[test]
    fn the_filter_value_keeps_its_case() {
        let parsed = parse_where("contains(a.name, 'RunHandler')");
        assert_eq!(parsed.name_filter.as_deref(), Some("RunHandler"));
    }

    #[test]
    fn a_multi_relationship_query_matches_either() {
        let result = run(
            &graph(),
            "MATCH (a)-[r:calls|inherits]->(b) RETURN a, b",
            50,
        );
        assert_eq!(result["total"], json!(2), "{result}");
    }

    #[test]
    fn a_query_this_subset_cannot_read_is_refused_with_the_shape_it_wants() {
        let result = run(&graph(), "SHOW DATABASES", 50);
        assert_eq!(result["ok"], json!(false));
        assert_eq!(result["code"], json!("unsupported_query"));
        assert!(result["error"].as_str().unwrap().contains("MATCH"));
    }

    /// The parser found `MATCH ` and ` RETURN ` and took whatever lay between
    /// as the pattern, so `MATCH (((( RETURN ))))` — read back from the
    /// release binary on a real store — answered `ok: true` with fifty rows
    /// and `total: 18404`: a query this engine could not read, reported as
    /// every node in the graph. The same hole let `(x)` bind a variable the
    /// WHERE grammar cannot name, and a label the engine never filters on.
    #[test]
    fn a_pattern_this_subset_cannot_read_is_refused_not_answered_as_every_node() {
        for query in [
            "MATCH (((( RETURN ))))",
            "MATCH garbage RETURN a",
            "MATCH (a RETURN a",
            "MATCH (x) RETURN x",
            "MATCH (a:Function) RETURN a",
            "MATCH (a)-[r:calls]->(b RETURN a, b",
            "MATCH (a)-[r:calls]->(b) extra RETURN a, b",
            "MATCH (a)-[r:calls]->(x) RETURN a",
            "MATCH (a) RETURN",
        ] {
            let result = run(&graph(), query, 50);
            assert_eq!(result["ok"], json!(false), "{query}: {result}");
            assert_eq!(
                result["code"],
                json!("unsupported_query"),
                "{query}: {result}"
            );
        }
    }

    /// Keyword positions were found in a `to_uppercase()` copy and used to
    /// slice the original. `ﬁ` uppercases to `FI` (three bytes to two) and
    /// `ŉ` to `ʼN` (two to three), so every offset past such a character was
    /// off by one: read back from the release binary, the filter value `'ﬁle'`
    /// was refused as `contains(a.name, 'ﬁle'` — its own closing paren cut off
    /// — and `'ŉx') AND …` came back as `'ŉx') A`. A value that grows enough
    /// lands the slice inside a character and panics. Case-folding for the
    /// keyword search has to preserve byte offsets, which only ASCII folding
    /// does.
    #[test]
    fn a_non_ascii_filter_value_does_not_shift_the_keywords() {
        for query in [
            "MATCH (a)-[r:calls]->(b) WHERE contains(a.name, 'ﬁle') AND \
             starts with(b.path, 'src/') RETURN a, b",
            "MATCH (a)-[r:calls]->(b) WHERE contains(a.name, 'ŉx') AND \
             starts with(b.path, 'src/') RETURN a, b LIMIT 3",
            "MATCH (a) WHERE contains(a.name, 'ŉŉŉŉ') RETURN a",
        ] {
            let result = run(&graph(), query, 50);
            assert_eq!(result["ok"], json!(true), "{query}: {result}");
        }
    }

    /// The RETURN clause was never read: `RETURN ))))` and `RETURN c` both
    /// answered with the pattern's rows. What comes back is always the bound
    /// variables' fields, so anything else in the projection is a request
    /// this engine silently did not honour.
    #[test]
    fn a_return_naming_nothing_the_pattern_bound_is_refused() {
        for query in [
            "MATCH (a) RETURN c",
            "MATCH (a) RETURN a, b",
            "MATCH (a) RETURN ))))",
            "MATCH (a)-[r:calls]->(b) RETURN a, b, z",
            "MATCH (a) RETURN a.colour",
        ] {
            let result = run(&graph(), query, 50);
            assert_eq!(result["ok"], json!(false), "{query}: {result}");
            assert_eq!(
                result["code"],
                json!("unsupported_return"),
                "{query}: {result}"
            );
        }
        // Every projection the documentation shows still runs.
        for query in [
            "MATCH (a) RETURN a",
            "MATCH (a) RETURN a.name, a.path LIMIT 2",
            "MATCH (a)-[r:calls]->(b) RETURN a, b",
            "MATCH (a)-[r:calls]->(b) RETURN a.id, b.id LIMIT 20",
            "MATCH (a)-[r:CALLS]->(b) RETURN a, r, b",
            "MATCH (a)-[:calls]->(b) RETURN a, b",
        ] {
            let result = run(&graph(), query, 50);
            assert_eq!(result["ok"], json!(true), "{query}: {result}");
        }
    }
}
