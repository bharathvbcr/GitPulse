//! Ruby method visibility.
//!
//! Ruby has no visibility keyword on a declaration. `private` is a method call
//! that changes the default for the `def`s after it in the same class body,
//! `private def x` wraps one definition, and `private :x, :y` names methods
//! defined anywhere in the body. `generic_is_exported` read only the `def`
//! header, saw none of these, and called every method public — so a private
//! method nothing calls was exempt and never reported (measured 2026-10-09).

use tree_sitter::Node;

use crate::model::{ExtractedSymbol, SymbolKind};
use crate::treesitter::{bounded_parent, get_node_text, node_span, walk_deadline_passed};

/// The method a `call` invokes, when it is written with no receiver.
fn bare_call_method(node: Node, source: &str) -> Option<String> {
    if node.kind() != "call" || node.child_by_field_name("receiver").is_some() {
        return None;
    }
    node.child_by_field_name("method")
        .map(|method| get_node_text(method, source))
}

fn restricts(word: &str) -> bool {
    matches!(word, "private" | "protected")
}

/// Whether a Ruby declaration can be called from outside its class.
pub(crate) fn is_exported(node: Node, source: &str, name: &str) -> bool {
    // A `def`, or an `attr_*` macro, which sits in a class body the same way
    // and is made private by the same `private` section.
    if !matches!(node.kind(), "method" | "call") {
        return !name.starts_with('_');
    }
    let Some(parent) = bounded_parent(node) else {
        return true;
    };
    // `private def helper; end`
    if parent.kind() == "argument_list" {
        if let Some(call) = bounded_parent(parent) {
            if bare_call_method(call, source).is_some_and(|word| restricts(&word)) {
                return false;
            }
        }
    }
    if parent.kind() != "body_statement" {
        return true;
    }
    // A bare `private` / `protected` / `public` earlier in the same body sets
    // the default for this `def`; the nearest one wins.
    let mut sibling = node.prev_named_sibling();
    while let Some(previous) = sibling {
        if previous.kind() == "identifier" {
            let word = get_node_text(previous, source);
            if restricts(&word) {
                return false;
            }
            if word == "public" {
                break;
            }
        }
        sibling = previous.prev_named_sibling();
    }
    // `private :helper, :other` anywhere in the same body.
    let mut cursor = parent.walk();
    for statement in parent.named_children(&mut cursor) {
        if !bare_call_method(statement, source).is_some_and(|word| restricts(&word)) {
            continue;
        }
        let Some(arguments) = statement.child_by_field_name("arguments") else {
            continue;
        };
        let mut inner = arguments.walk();
        let names_this = arguments.named_children(&mut inner).any(|argument| {
            let text = get_node_text(argument, source);
            text.trim_start_matches(':')
                .trim_matches(|c| c == '"' || c == '\'')
                == name
        });
        if names_this {
            return false;
        }
    }
    true
}

/// The macros that define a method with no `def`, from the names they take as
/// literals: a reader, a writer, both, or `define_method`.
///
/// `attr_writer :x` defines `x=`, which a call site spells `obj.x = v` and
/// the extractor records as the *reader* (an assignment target is refused), so
/// a writer declares the name `x=` and nothing resolves to it — it exists so
/// the class's member table is complete, not to attract edges.
pub(crate) fn attribute_names(call: Node, source: &str) -> Vec<String> {
    if call.kind() != "call" || call.child_by_field_name("receiver").is_some() {
        return Vec::new();
    }
    let Some(method) = call
        .child_by_field_name("method")
        .map(|method| get_node_text(method, source))
    else {
        return Vec::new();
    };
    let suffix = match method.as_str() {
        "attr_reader" | "attr" | "attr_accessor" | "define_method" => "",
        "attr_writer" => "=",
        _ => return Vec::new(),
    };
    let Some(arguments) = call.child_by_field_name("arguments") else {
        return Vec::new();
    };
    // `define_method` names one method, in its first argument; the attr macros
    // name every literal argument. A splat or a computed name is not a literal
    // and is skipped, which leaves the send refused rather than misbound.
    let mut names = Vec::new();
    let mut cursor = arguments.walk();
    for argument in arguments.named_children(&mut cursor) {
        if !matches!(
            argument.kind(),
            "simple_symbol" | "delimited_symbol" | "string"
        ) {
            if method == "define_method" {
                break;
            }
            continue;
        }
        let text = get_node_text(argument, source);
        let name = text.trim_start_matches(':').trim_matches(['"', '\'']);
        if !name.is_empty()
            && name
                .chars()
                .all(|ch| ch.is_alphanumeric() || matches!(ch, '_' | '?' | '!'))
        {
            names.push(format!("{name}{suffix}"));
        }
        if method == "define_method" {
            break;
        }
    }
    // `attr_accessor :x` is a reader and a writer.
    if method == "attr_accessor" {
        let writers: Vec<String> = names.iter().map(|name| format!("{name}=")).collect();
        names.extend(writers);
    }
    names
}

/// One `Method` symbol per attribute a class or module defines without `def`.
///
/// Before this, `attr_reader :tap` declared nothing, so a bare `tap` in that
/// class had no declaration to bind and could only reach a *different* `tap`
/// elsewhere in the corpus at `UniqueGlobal` — 26 of the first 126 new
/// cross-class edges on Homebrew. With the symbol in the class's member table
/// the send resolves to it, and an attribute inherited from a superclass is
/// found by the same supertype walk every other inherited method is.
///
/// Owned by the class or module the macro sits directly in (the owner rule
/// `generic_enclosing_type` applies to a `def`). A macro in a `class << self`,
/// or outside any class, has no owner a call could join to and declares
/// nothing. A name a `def` already emitted in the same class is skipped, so the
/// qualified name stays a unique join key (SC14).
pub(crate) fn attribute_symbols(
    root: Node,
    source: &str,
    file_symbol_name: &str,
    existing: &[ExtractedSymbol],
) -> Vec<ExtractedSymbol> {
    let mut found: Vec<ExtractedSymbol> = Vec::new();
    let mut worklist = vec![root];
    while let Some(node) = worklist.pop() {
        if walk_deadline_passed() {
            break;
        }
        let names = attribute_names(node, source);
        if !names.is_empty() {
            if let Some(owner) = crate::treesitter::generic_enclosing_type(node, source) {
                for name in names {
                    let qualified = format!("{file_symbol_name}::{owner}.{name}");
                    if existing
                        .iter()
                        .chain(found.iter())
                        .any(|symbol| symbol.qualified_name == qualified)
                    {
                        continue;
                    }
                    found.push(ExtractedSymbol {
                        is_exported: is_exported(node, source, &name),
                        name,
                        qualified_name: qualified,
                        kind: SymbolKind::Method,
                        span: node_span(node),
                        docstring: None,
                        signature: None,
                        parent_symbol: Some(format!("{file_symbol_name}::{owner}")),
                        body_signature: None,
                        declaration_hash: None,
                        return_type: None,
                    });
                }
            }
        }
        let mut cursor = node.walk();
        worklist.extend(node.named_children(&mut cursor));
    }
    found.sort_by_key(|symbol| (symbol.span.start_byte, symbol.qualified_name.clone()));
    found
}
