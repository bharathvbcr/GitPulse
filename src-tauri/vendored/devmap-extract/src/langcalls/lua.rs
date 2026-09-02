//! Calls made by Lua and Luau code.
//!
//! Lua reached `extract_node`'s generic arm, which emits declarations and
//! nothing else, so **not one Lua call was extracted**. Measured on a
//! two-function file (`function helper(a) … end`, `function main() return
//! helper(1) end`) with the release binary: this port produced `Contains`
//! edges and **zero** `Calls`, while the Python implementation it replaces
//! produced `i.lua::main -> i.lua::helper calls`. That makes Lua a migration
//! *regression*, not a shared gap — the reason it is fixed here first.
//!
//! Luau shares the grammar family: `tree-sitter-luau` names every node this
//! module reads exactly as `tree-sitter-lua` does (`function_call` with
//! `name`/`arguments`, `dot_index_expression`, `method_index_expression`,
//! `function_declaration`), verified by parsing the same source with both
//! grammars, so one module serves both rather than a near-copy per grammar.
//!
//! Shapes were read from the grammars' own parse trees rather than assumed.
//! Two consequences worth stating, because they are what makes a single arm
//! sufficient:
//!
//! * **Argument sugar needs no handling.** `require "socket"` and `f{a = 1}`
//!   are ordinary `function_call` nodes whose `arguments` child happens to be a
//!   `string` or a `table_constructor`. Keying on the call node rather than on
//!   its arguments covers both for free.
//! * **The colon form is not a separate call kind.** `t:f()` is a
//!   `method_index_expression` where `t.f()` is a `dot_index_expression`; the
//!   colon's only extra effect is passing an implicit `self` *argument*, which
//!   is not part of the callee's identity. Both therefore yield the same
//!   `(callee, receiver)` pair.

use crate::model::{ExtractedCall, ExtractedReference, ReferenceKind};
use crate::treesitter::{get_child_text, node_span, split_call_target};
use tree_sitter::Node;

/// Record the call `node` makes, if it makes one.
///
/// Called for every node in the tree, so the kind check is the first thing that
/// happens and the overwhelmingly common answer is "not a call".
pub fn extract_lua_call(
    node: Node,
    source: &str,
    file_symbol_name: &str,
    calls: &mut Vec<ExtractedCall>,
    references: &mut Vec<ExtractedReference>,
) {
    if node.kind() != "function_call" {
        return;
    }
    let Some(target) = node.child_by_field_name("name") else {
        return;
    };
    let Some((callee_name, receiver_expr)) = lua_call_target(target, source) else {
        return;
    };
    let caller_symbol = lua_enclosing_symbol(node, source, file_symbol_name);
    references.push(ExtractedReference {
        name: callee_name.clone(),
        kind: ReferenceKind::Call,
        span: node_span(target),
        enclosing_symbol: caller_symbol.clone(),
        // Lua binds values, not types: `local x = f()` proves only that `x`
        // holds whatever `f` returned, and there is no declared type for
        // receiver inference to look up afterwards. Recording a binding here
        // would add a fact nothing consumes, so it is left unset deliberately
        // rather than filled with something the language cannot support.
        assigned_to: None,
        // The mirrored call already carries the receiver; repeating it here would be a second copy of one fact.
        receiver_expr: None,
    });
    calls.push(ExtractedCall {
        caller_symbol,
        callee_name,
        receiver_expr,
        span: node_span(node),
    });
}

/// The callee's identity and the receiver it is reached through, or `None` when
/// the target names nothing.
///
/// Every arm ends at `split_call_target`, so the name is validated by
/// `is_callee_identity` exactly once and an expression can never be recorded as
/// a callee — the SC26/SC32 defect class. The indexed arms exist because
/// `split_call_target` has no arm for Lua's index expressions and would fall to
/// its catch-all, producing the text `M.mfun`, which `is_callee_identity`
/// rejects; descending to the field identifier first turns a dropped edge into
/// a real one without widening what counts as a name.
fn lua_call_target(target: Node, source: &str) -> Option<(String, Option<String>)> {
    match target.kind() {
        // `t.f()` — the table is the receiver, the field is the callee.
        "dot_index_expression" => lua_indexed_target(target, "field", source),
        // `t:f()` — same split; `method` is the colon form's field name.
        "method_index_expression" => lua_indexed_target(target, "method", source),
        // An immediately-invoked function literal has no callee identity, and
        // Lua spells that literal `function_definition`.
        //
        // `is_anonymous_callable` deliberately does not name that kind and must
        // not be taught it: Python and the C family use `function_definition`
        // for *named* functions, so adding it there would silence real callees
        // in two families to fix one. The guard is therefore local. It is also
        // belt-and-braces — the literal's text carries parentheses and spaces,
        // so `is_callee_identity` refuses it too — but the intent belongs in
        // the code rather than in a coincidence.
        "function_definition" => None,
        // `f()()`, `t[k]()`, `("x"):upper()`'s parenthesised receiver: anything
        // else is handed to the shared splitter, which unwraps the wrappers it
        // knows and fails closed on the rest. `t[k]` and `f()` carry brackets
        // and parentheses, so they are refused rather than recorded — and the
        // inner call of `f()()` is a node in its own right that this function
        // sees separately, so no real edge is lost.
        _ => split_call_target(target, source),
    }
}

fn lua_indexed_target(
    target: Node,
    name_field: &str,
    source: &str,
) -> Option<(String, Option<String>)> {
    let name_node = target.child_by_field_name(name_field)?;
    let (callee, _) = split_call_target(name_node, source)?;
    let receiver = get_child_text(target, "table", source).filter(|text| !text.is_empty());
    Some((callee, receiver))
}

/// Graph identity of the Lua function lexically containing `node`.
///
/// **Not `enclosing_callable_qualified`, and the difference is measurable.**
/// That helper routes an owner-less callable through `scoped_qualified_name`,
/// which prefixes the enclosing scope: a `local function inner` inside
/// `function outer` comes back as `f.lua::outer.inner`. The generic arm that
/// emits Lua symbols formats them flat — `format!("{file}::{name}")` — so the
/// node is `f.lua::inner` and the edge would name a source no symbol carries.
/// That is precisely the SC9/SC10 orphan shape, and nested `local function` is
/// ordinary Lua rather than a corner case. The unit test below pins both halves
/// so the disagreement cannot reappear silently.
///
/// Walking past an unnamed `function_definition` to the enclosing declaration is
/// the same behaviour `callable_binding_name` already has for unnamed arrows,
/// and it agrees with the emitter, which emits no symbol for a literal.
fn lua_enclosing_symbol(node: Node, source: &str, file_symbol_name: &str) -> Option<String> {
    let mut ancestor = node.parent();
    while let Some(parent) = ancestor {
        if parent.kind() == "function_declaration" {
            if let Some(name) = get_child_text(parent, "name", source).filter(|n| !n.is_empty()) {
                return Some(format!("{file_symbol_name}::{name}"));
            }
        }
        ancestor = parent.parent();
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::extract_file;
    use crate::treesitter::enclosing_callable_qualified;
    use tree_sitter::Parser;

    const NESTED: &str =
        "function outer()\n  local function inner() return 1 end\n  return inner()\nend\n";

    fn calls_of(lang: tree_sitter::Language, path: &str, source: &str) -> Vec<ExtractedCall> {
        let mut parser = Parser::new();
        parser.set_language(&lang).expect("grammar loads");
        let tree = parser.parse(source, None).expect("source parses");
        let mut calls = Vec::new();
        let mut references = Vec::new();
        let mut worklist = vec![tree.root_node()];
        while let Some(node) = worklist.pop() {
            extract_lua_call(node, source, path, &mut calls, &mut references);
            for index in (0..node.child_count()).rev() {
                if let Some(child) = node.child(index) {
                    worklist.push(child);
                }
            }
        }
        assert_eq!(
            references.len(),
            calls.len(),
            "every call must also be recorded as a reference"
        );
        calls
    }

    /// The reason `lua_enclosing_symbol` exists, stated as a test rather than a
    /// comment: the shared scope builder and the generic symbol emitter
    /// disagree for a nested Lua function, and using the shared one would emit
    /// an orphaned call edge.
    #[test]
    fn the_shared_scope_builder_would_orphan_a_nested_lua_call() {
        let mut parser = Parser::new();
        parser
            .set_language(&tree_sitter_lua::LANGUAGE.into())
            .unwrap();
        let tree = parser.parse(NESTED, None).unwrap();

        // A node inside `inner`, which is itself inside `outer`.
        let mut worklist = vec![tree.root_node()];
        let mut probe = None;
        while let Some(node) = worklist.pop() {
            if node.kind() == "return_statement"
                && &NESTED[node.start_byte()..node.end_byte()] == "return 1"
            {
                probe = Some(node);
                break;
            }
            for index in (0..node.child_count()).rev() {
                if let Some(child) = node.child(index) {
                    worklist.push(child);
                }
            }
        }
        let probe = probe.expect("`return 1` is in the tree");

        assert_eq!(
            enclosing_callable_qualified(probe, NESTED, "f.lua").as_deref(),
            Some("f.lua::outer.inner"),
            "the shared builder nests the scope"
        );
        let emitted: Vec<String> = extract_file("f.lua", NESTED)
            .symbols
            .into_iter()
            .map(|symbol| symbol.qualified_name)
            .collect();
        assert!(
            emitted.iter().any(|name| name == "f.lua::inner"),
            "the emitter names the nested function flat: {emitted:?}"
        );
        assert!(
            !emitted.iter().any(|name| name == "f.lua::outer.inner"),
            "no symbol carries the nested scope string: {emitted:?}"
        );
        assert_eq!(
            lua_enclosing_symbol(probe, NESTED, "f.lua").as_deref(),
            Some("f.lua::inner"),
            "this module must agree with the emitter, not with the shared builder"
        );
    }

    /// Both grammars in the family must behave identically; Luau is the one a
    /// fix aimed at Lua alone would miss.
    #[test]
    fn lua_and_luau_agree_on_every_call_shape() {
        let source = "local M = {}\nfunction M.mfun(c) return c end\nfunction main()\n  helper(1)\n  M.mfun(2)\n  M:mcolon(3)\n  require \"socket\"\n  f{a = 1}\nend\n";
        let lua = calls_of(tree_sitter_lua::LANGUAGE.into(), "u.lua", source);
        let luau = calls_of(tree_sitter_luau::LANGUAGE.into(), "u.lua", source);
        let names = |calls: &[ExtractedCall]| -> Vec<String> {
            let mut out: Vec<String> = calls
                .iter()
                .map(|call| match &call.receiver_expr {
                    Some(receiver) => format!("{receiver}::{}", call.callee_name),
                    None => call.callee_name.clone(),
                })
                .collect();
            out.sort();
            out
        };
        assert_eq!(
            names(&lua),
            vec!["M::mcolon", "M::mfun", "f", "helper", "require"],
            "Lua"
        );
        assert_eq!(names(&luau), names(&lua), "Luau must match Lua");
    }
}
