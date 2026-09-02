//! Calls made by R code.
//!
//! **This is a gap, not a regression, and the distinction is load-bearing.**
//! Measured on a two-function file (`helper <- function(a) { a }`,
//! `main <- function() { helper(1) }`): this port recovers zero `Calls`, and so
//! does the Python implementation it replaces — the Python graph for that file
//! contains only `contains` edges. Nothing was restored here; call extraction
//! for R is new in either implementation.
//!
//! Shapes were read from `tree-sitter-r`'s own parse tree. A call is a `call`
//! node with `function` and `arguments` fields; the `function` field is an
//! `identifier`, a `namespace_operator` (`pkg::fn`, `pkg:::fn`), an
//! `extract_operator` (`obj$m`, `obj@slot`), or an expression that names
//! nothing. Calls used as arguments need no handling — `outer(inner(5))` nests a
//! `call` inside an `argument`, and the walk reaches it on its own.
//!
//! ## Two deliberate divergences, both justified below
//!
//! **1. A dot is part of an R name.** The R Language Definition admits `.` in an
//! identifier, and base R is saturated with dotted names — `do.call`,
//! `data.frame`, `as.numeric`, `is.null`, `read.csv`, `Sys.time`. The shared
//! `is_callee_identity` rejects `.` and must keep rejecting it: for every other
//! grammar here, `.` in a callee name is the signature of a member *expression*
//! that reached a text fallback, which is the SC26/SC32 defect. So the widening
//! is local to R and is gated on structure, not on text: it applies only to the
//! text of an `identifier` **token**, which the grammar has already proved is a
//! single lexical name and not an expression. `is_callee_identity` still runs
//! first; `is_r_identifier` only decides names it refuses.
//!
//! **2. The piped-to function is a call.** Both pipe spellings are handled, but
//! only one needs code:
//!
//! * `x |> f()` and `z %>% h()` parse as a `binary_operator` whose `rhs` is a
//!   real `call` node, which the walk visits by itself. Nothing to do.
//! * `w %>% k` — magrittr's bare-name form — has an `identifier` rhs and no
//!   `call` node anywhere. magrittr evaluates it as `k(w)`: `k` is invoked. If
//!   the graph stays silent, `k` looks callerless to dead-code analysis while it
//!   demonstrably runs, which is the one direction this port must not fail in.
//!   So the bare rhs of `%>%`, `%<>%` and `%T>%` is recorded as a call.
//!
//!   The line is drawn by *operator semantics*, not by convenience. `%$%` is
//!   excluded: its rhs is a name looked up in a data mask, not a function to
//!   apply. `%in%` and every other user-defined `%op%` are excluded for the same
//!   reason — tree-sitter-r gives them all the kind `special`, so the operator's
//!   text is what tells them apart, and it is read rather than assumed. And
//!   `|>` needs no bare-name form because R rejects one: the native pipe
//!   requires a call on the right, so inventing an edge there would describe
//!   code that cannot run.

use crate::model::{ExtractedCall, ExtractedReference, ReferenceKind};
use crate::treesitter::{get_child_text, get_node_text, node_span, split_call_target};
use tree_sitter::Node;

/// magrittr operators whose right operand is a function that gets applied, so a
/// bare name on the right is an invocation. Kept sorted; searched linearly
/// because three entries never justify more.
const APPLYING_PIPES: &[&str] = &["%<>%", "%T>%", "%>%"];

/// Record the call `node` makes, if it makes one.
pub fn extract_r_call(
    node: Node,
    source: &str,
    file_symbol_name: &str,
    calls: &mut Vec<ExtractedCall>,
    references: &mut Vec<ExtractedReference>,
) {
    let (callee_name, receiver_expr, target) = match node.kind() {
        "call" => {
            let Some(function) = node.child_by_field_name("function") else {
                return;
            };
            let Some((callee, receiver)) = r_call_target(function, source) else {
                return;
            };
            (callee, receiver, function)
        }
        // `w %>% k`. The parenthesised forms are already `call` nodes and are
        // handled above; only the bare name reaches here.
        "binary_operator" => {
            let Some(operator) = node.child_by_field_name("operator") else {
                return;
            };
            if !APPLYING_PIPES.contains(&get_node_text(operator, source).as_str()) {
                return;
            }
            let Some(rhs) = node.child_by_field_name("rhs") else {
                return;
            };
            let Some(callee) = r_callee_name(rhs, source) else {
                return;
            };
            (callee, None, rhs)
        }
        _ => return,
    };
    let caller_symbol =
        super::scope::enclosing_emitted_symbol(node, source, LANG, file_symbol_name);
    references.push(ExtractedReference {
        name: callee_name.clone(),
        kind: ReferenceKind::Call,
        span: node_span(target),
        enclosing_symbol: caller_symbol.clone(),
        // R has no declared types, so `x <- f()` binds a value and proves
        // nothing a receiver lookup could use. Left unset deliberately rather
        // than filled with a fact nothing consumes.
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
/// Fail-closed by construction: only the three shapes that end at an
/// `identifier` token are accepted. `lst[[1]]()` (`subset2`), `g()(2)` (a `call`
/// target), `(function(x) x)(1)` (a parenthesised literal) and `` `weird name`(1) ``
/// all name nothing a symbol could carry, and recording their text would
/// manufacture rows that can never join — the SC26/SC32 class. The inner call of
/// `g()(2)` is a node in its own right that the walk sees separately, so no real
/// edge is lost by refusing the outer one.
fn r_call_target(target: Node, source: &str) -> Option<(String, Option<String>)> {
    match target.kind() {
        "identifier" => r_callee_name(target, source).map(|name| (name, None)),
        // `pkg::fn()`, `pkg:::fn()`, `obj$method()`, `obj@slot()`: the left
        // operand is the receiver, the right is the callee.
        "namespace_operator" | "extract_operator" => {
            let rhs = target.child_by_field_name("rhs")?;
            let callee = r_callee_name(rhs, source)?;
            let receiver = get_child_text(target, "lhs", source).filter(|text| !text.is_empty());
            Some((callee, receiver))
        }
        _ => None,
    }
}

/// The name an `identifier` token carries, if it can be a callee's identity.
///
/// The structural rung comes first and is absolute: anything that is not a
/// single `identifier` token is refused before its text is ever read, so no
/// expression can reach the lexical rungs below it. `split_call_target` — and
/// therefore `is_callee_identity` — decides first; `is_r_identifier` only ever
/// admits a name it refused, and only for the one character R's own grammar
/// requires.
fn r_callee_name(node: Node, source: &str) -> Option<String> {
    if node.kind() != "identifier" {
        return None;
    }
    if let Some((name, _)) = split_call_target(node, source) {
        return Some(name);
    }
    let text = get_node_text(node, source);
    is_r_identifier(&text).then_some(text)
}

/// Whether `name` is a syntactic R identifier.
///
/// Stricter than `is_callee_identity` on the first character — R has no `$` or
/// `#` in a name, and a name may not begin with a digit or with a dot followed
/// by one — and looser only in admitting `.`. A backtick-quoted name such as
/// `` `weird name` `` fails on the backtick, which is the right answer: it can
/// carry arbitrary text, so it is not an identity anything can be keyed by.
fn is_r_identifier(name: &str) -> bool {
    let mut chars = name.chars();
    let Some(first) = chars.next() else {
        return false;
    };
    if !(first.is_alphabetic() || first == '.') {
        return false;
    }
    if first == '.' && chars.next().is_some_and(|second| second.is_ascii_digit()) {
        return false;
    }
    name.chars()
        .all(|ch| ch.is_alphanumeric() || matches!(ch, '.' | '_'))
}

/// The grammar key this module answers for.
const LANG: &str = "r";

#[cfg(test)]
mod tests {
    use super::*;
    use crate::extract_file;
    use crate::treesitter::{enclosing_callable_qualified, is_callee_identity};
    use tree_sitter::Parser;

    const NESTED: &str = "outer <- function() {\n  inner <- function() { deep(1) }\n  inner()\n}\n";

    fn calls_of(source: &str) -> Vec<ExtractedCall> {
        let mut parser = Parser::new();
        parser
            .set_language(&tree_sitter_r::LANGUAGE.into())
            .unwrap();
        let tree = parser.parse(source, None).unwrap();
        let mut calls = Vec::new();
        let mut references = Vec::new();
        let mut worklist = vec![tree.root_node()];
        while let Some(node) = worklist.pop() {
            extract_r_call(node, source, "u.r", &mut calls, &mut references);
            for index in (0..node.child_count()).rev() {
                if let Some(child) = node.child(index) {
                    worklist.push(child);
                }
            }
        }
        assert_eq!(references.len(), calls.len());
        calls
    }

    /// The widening is not a loosening: `is_r_identifier` admits dotted names
    /// and refuses everything expression-shaped that `is_callee_identity`
    /// refuses.
    #[test]
    fn the_r_identifier_rule_admits_dots_and_nothing_else() {
        for name in ["do.call", "as.numeric", "data.frame", ".hidden", "Sys.time"] {
            assert!(
                !is_callee_identity(name),
                "{name} is exactly what the shared rule refuses"
            );
            assert!(is_r_identifier(name), "{name} is a legal R name");
        }
        for name in [
            "pkg::fn",
            "obj$method",
            "`weird name`",
            "f(1)",
            "x + y",
            "1st",
            ".2way",
            "",
            "$dollar",
        ] {
            assert!(!is_r_identifier(name), "{name:?} must be refused");
        }
    }

    /// The reason this module routes caller attribution through
    /// `langdecl`, as a test rather than a comment.
    ///
    /// The expected strings moved with the fix that named R functions by their
    /// binding instead of by the `function` keyword: before it, `outer` and
    /// `inner` both emitted `f.r::function` — one qualified name for two
    /// symbols. The property under test is unchanged and is the whole point:
    /// whatever the emitter calls the enclosing declaration, this is the same
    /// string, and the shared `enclosing_callable_qualified` is not.
    #[test]
    fn the_shared_scope_builder_would_orphan_a_nested_r_call() {
        let mut parser = Parser::new();
        parser
            .set_language(&tree_sitter_r::LANGUAGE.into())
            .unwrap();
        let tree = parser.parse(NESTED, None).unwrap();

        let mut worklist = vec![tree.root_node()];
        let mut probe = None;
        while let Some(node) = worklist.pop() {
            if node.kind() == "call" && &NESTED[node.start_byte()..node.end_byte()] == "deep(1)" {
                probe = Some(node);
                break;
            }
            for index in (0..node.child_count()).rev() {
                if let Some(child) = node.child(index) {
                    worklist.push(child);
                }
            }
        }
        let probe = probe.expect("`deep(1)` is in the tree");

        assert_eq!(
            enclosing_callable_qualified(probe, NESTED, "f.r").as_deref(),
            Some("f.r::function.function"),
            "the shared builder nests the scope, and still names R functions after the keyword"
        );
        let emitted: Vec<String> = extract_file("f.r", NESTED)
            .symbols
            .into_iter()
            .map(|symbol| symbol.qualified_name)
            .collect();
        assert!(
            !emitted.iter().any(|name| name == "f.r::function.function"),
            "no symbol carries the nested scope string: {emitted:?}"
        );
        assert_eq!(
            crate::langcalls::scope::enclosing_emitted_symbol(probe, NESTED, LANG, "f.r")
                .as_deref(),
            Some("f.r::inner"),
            "this module must agree with the emitter, not with the shared builder"
        );
        assert!(
            emitted.iter().any(|name| name == "f.r::inner"),
            "and the emitter really does carry that name: {emitted:?}"
        );
        assert_eq!(
            emitted,
            vec!["f.r", "f.r::outer", "f.r::inner"],
            "and each R function carries its own name, not three copies of `function`"
        );
    }

    /// Pipes: the parenthesised forms come free, the bare magrittr form does
    /// not, and only the operators that apply their right operand qualify.
    #[test]
    fn pipes_record_the_function_that_is_applied_and_only_that() {
        let calls =
            calls_of("x |> f()\nz %>% h()\nw %>% k\na %T>% tee\nb %<>% mod\nc %$% col\nd %in% e\n");
        let mut names: Vec<&str> = calls.iter().map(|call| call.callee_name.as_str()).collect();
        names.sort_unstable();
        assert_eq!(
            names,
            vec!["f", "h", "k", "mod", "tee"],
            "`%$%` masks a name and `%in%` is a comparison; neither applies its right operand"
        );
    }
}
