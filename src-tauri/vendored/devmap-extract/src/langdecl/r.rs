//! R declarations.
//!
//! Every R function was named `function`. `tree-sitter-r` puts a `name` field on
//! `function_definition` and points it at the **keyword**, so a three-function
//! file emitted `j.r::function` three times: one qualified name for three
//! symbols, which is a broken join key (SC14) and leaves no R call able to
//! resolve to anything.
//!
//! R has no declaration syntax for a function. `helper <- function(a) …` is an
//! assignment of an anonymous value to a name, so the name has to come from the
//! binding, exactly as it does for a JS `const f = () => …`.
//!
//! The frozen Python baseline has the identical defect —
//! `testdata/golden/languages/r/nodes.json` pins `main.R::function` — so that
//! golden now disagrees with this extractor. It is left as it is: regenerating
//! it would freeze the defect into the parity contract, and the golden is not
//! one of the five fixtures the parity harness compares.

use tree_sitter::Node;

use crate::model::SymbolKind;
use crate::treesitter::{get_node_text, is_callee_identity};

use super::Declaration;

/// An R declaration, or `None` when `node` declares nothing this graph records.
pub(crate) fn declaration(node: Node, source: &str) -> Option<Declaration> {
    if node.kind() != "function_definition" {
        // Everything else R declares is handled by the shared node-kind table,
        // which for this grammar means nothing at all — R has no class, struct
        // or module node kind. Routing through it anyway keeps the language's
        // answer in one place rather than splitting it across two.
        return super::generic(node, source);
    }
    Declaration::new(SymbolKind::Function, None, bound_name(node, source)?)
}

/// The name a `function_definition` is bound to by its enclosing assignment.
///
/// `<-`, `<<-` and `=` all bind, and `->` binds in the other direction. An
/// anonymous function — `lapply(xs, function(x) x + 1)` — is bound to nothing
/// and yields no symbol, which is correct: there is no name a call could use.
fn bound_name(node: Node, source: &str) -> Option<String> {
    let parent = node.parent()?;
    if parent.kind() != "binary_operator" {
        return None;
    }
    let operator = get_node_text(parent.child_by_field_name("operator")?, source);
    let lhs = parent.child_by_field_name("lhs")?;
    let rhs = parent.child_by_field_name("rhs")?;
    let binding = match operator.as_str() {
        "<-" | "<<-" | "=" if rhs.id() == node.id() => lhs,
        "->" | "->>" if lhs.id() == node.id() => rhs,
        _ => return None,
    };
    // `x$f <- function() …` and `"f" <- function() …` both parse here. Only a
    // bare identifier is an identity a call can join to; anything else is
    // refused rather than recorded as its own source text (SC32).
    if binding.kind() != "identifier" {
        return None;
    }
    let name = get_node_text(binding, source);
    is_callee_identity(&name).then_some(name)
}
