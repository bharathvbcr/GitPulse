//! Erlang declarations.
//!
//! Erlang emitted **no declaration at all**. Measured on a two-function module
//! (`helper(X) -> X + 1.` / `run(N) -> Y = helper(N), lists:sum([Y]).`) before
//! this module existed: `extract_file` produced one symbol, the `File` node, and
//! zero calls and zero references. Nothing in `generic_symbol_kind` names
//! `fun_decl`, and `tree-sitter-erlang` hangs the name off a `function_clause`
//! child rather than off a `name` field of the declaration, so the generic
//! table could not have recovered it either.
//!
//! It emitted one symbol it should not have. `generic_symbol_kind` maps the node
//! kind `module` to `SymbolKind::Module`, and in this grammar `module` is not a
//! declaration — it is the *module half of a function reference*, the `lists` in
//! `fun lists:map/2`. Measured: a file whose only mention of `t` is
//! `F = fun t:f/2` emitted `t.erl::t` as a `Module`, a node for a module that is
//! not declared in that file and may not exist in the corpus at all. Answering
//! for Erlang here removes it, because this module answers `None` for every kind
//! it does not name rather than falling through to the shared table.
//!
//! # Same name, different arity
//!
//! `f/1` and `f/2` are different functions in Erlang and are two `fun_decl`
//! nodes. They are emitted as one qualified name, `m.erl::f`, exactly as Java,
//! C++ and Solidity overloads already are in this extractor (verified:
//! `O.java` with two `f` emits `O.java::C.f` twice). Encoding the arity in the
//! name would make the identity unjoinable instead — a call site spells the
//! callee `f`, and `is_callee_identity` refuses `f/2` — so the collapse is the
//! shape that keeps edges resolving, and it over-approximates in the direction
//! that cannot propose deleting live code.

use tree_sitter::Node;

use crate::model::SymbolKind;
use crate::treesitter::{get_child_text, is_callee_identity};

use super::Declaration;

/// An Erlang declaration, or `None` when `node` declares nothing this graph
/// records.
///
/// Deliberately **not** falling through to [`super::generic`]: the shared
/// node-kind table names nothing Erlang declares and does name `module`, which
/// in this grammar is a use site rather than a declaration.
pub(crate) fn declaration(node: Node, source: &str) -> Option<Declaration> {
    match node.kind() {
        // `helper(X) -> …;` — one `fun_decl` per name/arity, carrying one
        // `function_clause` per clause. Every clause of one function repeats
        // the name, so the first is the declaration's name and the rest are
        // the same name again.
        "fun_decl" => Declaration::new(SymbolKind::Function, None, clause_name(node, source)?),
        // `-record(state, {a, b}).` — a named set of typed fields, which is
        // what `Struct` already means for every other language here.
        "record_decl" => {
            Declaration::new(SymbolKind::Struct, None, named_atom(node, "name", source)?)
        }
        _ => None,
    }
}

/// The name carried by a `fun_decl`'s first `function_clause`.
fn clause_name(node: Node, source: &str) -> Option<String> {
    let clause = node.child_by_field_name("clause")?;
    if clause.kind() != "function_clause" {
        return None;
    }
    named_atom(clause, "name", source)
}

/// The text of an `atom`-valued field, refused unless it is an identity a call
/// can join to.
///
/// A quoted atom (`'my fun'()`) is legal Erlang and is *not* an identity: the
/// call site spells it with the quotes, and nothing downstream strips them.
/// Refusing it drops a symbol rather than emitting one whose name no edge can
/// match.
fn named_atom(node: Node, field: &str, source: &str) -> Option<String> {
    let name = get_child_text(node, field, source)?;
    is_callee_identity(&name).then_some(name)
}
