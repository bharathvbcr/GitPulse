//! Routines invoked by SQL.
//!
//! SQL reached `extract_node`'s generic arm, which already recovers its
//! declarations — `create_function`, `create_view`, `create_trigger` and
//! `create_table` are all in the shared table, and `generic_declaration_name`
//! already knows to read the name out of an `object_reference` — and stopped
//! there. Measured on a schema where `total()` calls `add_one()` twice and a
//! view selects `total(1)`: **4 symbols** and **0 calls**.
//!
//! # The bodies are not opaque, which is what makes this worth doing
//!
//! A `CREATE FUNCTION … AS $$ … $$` body could easily have been one string
//! token, and if it were there would be no SQL call graph to recover.
//! `tree-sitter-sequel` parses inside the dollar quotes: the body of
//! `total` contains two `invocation` nodes naming `add_one`, so
//! `schema.sql::total -> add_one` is a real intra-file edge that was being
//! dropped, not an edge this module invents.
//!
//! # A trigger's function is an invocation with no call syntax
//!
//! `CREATE TRIGGER t … EXECUTE FUNCTION total()` contains no `invocation` node
//! at all — the executed routine is a bare `object_reference`, indistinguishable
//! by kind from the trigger's own name and from the table it fires on, all three
//! of which are `object_reference` children of the same `create_trigger`. They
//! are told apart by the keyword that precedes them, which is the only place
//! this grammar records the distinction.
//!
//! It is recorded because a trigger function is a callback: nothing in any
//! schema ever writes `total()` at a call site, so without this edge every
//! trigger function is an uncalled `create_function` and a dead-code candidate.
//! That is the reference-only-use shape that makes dead-code analysis propose
//! deleting working code.

use tree_sitter::Node;

use crate::model::{ExtractedCall, ExtractedReference, ReferenceKind};
use crate::treesitter::{node_span, split_call_target};

use super::scope::{enclosing_emitted_symbol, receiver_from};

/// Keywords after which a `create_trigger`'s `object_reference` names the
/// routine the trigger executes.
///
/// Both spellings are current: `EXECUTE PROCEDURE` is the historical form and
/// `EXECUTE FUNCTION` the one PostgreSQL 11 introduced, and real schemas
/// contain both.
const TRIGGER_ROUTINE_KEYWORDS: &[&str] = &["keyword_function", "keyword_procedure"];

/// Record the routine `node` invokes, if it invokes one.
pub fn extract_sql_call(
    node: Node,
    source: &str,
    file_symbol_name: &str,
    calls: &mut Vec<ExtractedCall>,
    references: &mut Vec<ExtractedReference>,
) {
    let Some((reference, span)) = invoked_object_reference(node) else {
        return;
    };
    // `public.add_one(n)` — the schema qualifies the routine and is the
    // receiver, exactly as a module path is for every other language here. The
    // name field is taken rather than the reference's whole text so a
    // schema-qualified callee is a name a symbol can carry (SC32).
    let Some(name_node) = reference.child_by_field_name("name") else {
        return;
    };
    let Some((callee_name, _)) = split_call_target(name_node, source) else {
        return;
    };
    let receiver_expr = reference
        .child_by_field_name("schema")
        .and_then(|schema| receiver_from(schema, source));
    // The walk starts at the *reference*, not at `node`. A trigger's executed
    // routine is a child of the `create_trigger` that is itself the emitted
    // symbol, and `enclosing_emitted_symbol` looks only at ancestors — starting
    // from `node` there would skip the trigger and attribute the edge to the
    // file, which is the orphan shape this directory exists to prevent.
    let caller_symbol = enclosing_emitted_symbol(reference, source, "sql", file_symbol_name);
    references.push(ExtractedReference {
        name: callee_name.clone(),
        kind: ReferenceKind::Call,
        span: node_span(name_node),
        enclosing_symbol: caller_symbol.clone(),
        // SQL has no local variable whose type could be inferred from the
        // routine that produced it: a column alias names a value, not a
        // typed binding a later call could dispatch through.
        assigned_to: None,
        // The mirrored call already carries the receiver; repeating it here would be a second copy of one fact.
        receiver_expr: None,
    });
    calls.push(ExtractedCall {
        caller_symbol,
        callee_name,
        receiver_expr,
        span: node_span(span),
    });
}

/// `(the object_reference naming the invoked routine, the node the edge spans)`.
fn invoked_object_reference<'tree>(node: Node<'tree>) -> Option<(Node<'tree>, Node<'tree>)> {
    match node.kind() {
        // `add_one(n)`, `count(*)`, `total()` — the parenthesised call form.
        // The routine is the first child; `parameter` children are the
        // arguments and each is visited on its own, so taking the first rather
        // than searching keeps a nested call in an argument from being read as
        // this call's callee.
        "invocation" => {
            let reference = node
                .named_child(0)
                .filter(|c| c.kind() == "object_reference")?;
            Some((reference, node))
        }
        // `EXECUTE FUNCTION total()` inside a `CREATE TRIGGER`. The edge spans
        // the reference rather than the whole statement, which would otherwise
        // claim the trigger's entire declaration as one call site.
        "create_trigger" => trigger_routine(node).map(|reference| (reference, reference)),
        _ => None,
    }
}

/// The routine a trigger executes: the `object_reference` whose preceding
/// sibling is `FUNCTION` or `PROCEDURE`.
///
/// Anchored on the keyword rather than on position, because the two other
/// `object_reference` children of a `create_trigger` — the trigger's own name
/// and the table it fires on — are the same kind and their positions move with
/// the optional `FOR EACH ROW`, `WHEN (…)` and column-list clauses.
fn trigger_routine<'tree>(node: Node<'tree>) -> Option<Node<'tree>> {
    let mut previous_kind = "";
    for index in 0..node.child_count() {
        let child = node.child(index)?;
        if child.kind() == "object_reference" && TRIGGER_ROUTINE_KEYWORDS.contains(&previous_kind) {
            return Some(child);
        }
        previous_kind = child.kind();
    }
    None
}
