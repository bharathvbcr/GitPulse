//! Java and C# access modifiers: who may call a declaration from outside the
//! corpus.
//!
//! Both reached `generic_is_exported`, which reads a header for the literal
//! words `private ` and `protected `. That answered two questions wrongly, both
//! measured on 2026-10-09:
//!
//! * **C# defaults a class or struct member to `private`.** `public class K {
//!   void Hidden() {} }` declares a private method, and the header scan, seeing
//!   no keyword, called it exported — so it was exempt and never reported.
//! * **`protected` is an extension point.** `public abstract class Base {
//!   protected void onEvent() {} }` is called by subclasses the corpus need not
//!   contain, and the header scan made it a 0.9 dead row with no reason — a
//!   proposal to delete an API.
//!
//! The rule here reads the declaration's own modifier list, never its body.

use tree_sitter::Node;

use crate::langdecl::Declaration;
use crate::model::{SymbolKind, WiringKind};
use crate::treesitter::{bounded_parent, get_node_text};

/// Why a `protected` member is exempt.
pub(crate) const PROTECTED_REASON: &str =
    "protected member of a public, extensible type — subclasses outside the corpus may call or \
     override it";

/// The node whose modifier list governs `node`: a C# or Java field names its
/// variable on a `variable_declarator` nested under the declaration that
/// carries the modifiers.
fn carrier(node: Node) -> Node {
    let mut current = node;
    while matches!(
        current.kind(),
        "variable_declarator" | "variable_declaration"
    ) {
        match bounded_parent(current) {
            Some(parent) => current = parent,
            None => break,
        }
    }
    current
}

/// The modifier words written on the declaration itself: Java's `modifiers`
/// node, C#'s `modifier` children. Annotations and attributes are not words.
pub(crate) fn modifier_words(node: Node, source: &str) -> Vec<String> {
    let declaration = carrier(node);
    let mut words = Vec::new();
    let mut cursor = declaration.walk();
    for child in declaration.children(&mut cursor) {
        match child.kind() {
            "modifier" => words.push(get_node_text(child, source).trim().to_string()),
            "modifiers" => {
                let mut inner = child.walk();
                for word in child.children(&mut inner) {
                    if word.kind().ends_with("annotation") {
                        continue;
                    }
                    words.extend(
                        get_node_text(word, source)
                            .split_whitespace()
                            .map(str::to_string),
                    );
                }
            }
            _ => {}
        }
    }
    words
}

fn has(words: &[String], word: &str) -> bool {
    words.iter().any(|candidate| candidate == word)
}

/// The type declaration that directly owns a member, if any.
fn owning_type(node: Node) -> Option<Node> {
    let body = bounded_parent(carrier(node))?;
    if !matches!(
        body.kind(),
        "declaration_list" | "class_body" | "interface_body"
    ) {
        return None;
    }
    bounded_parent(body)
}

/// Whether `node` declares a member of a C# class, struct or record — the
/// containers whose members default to `private`.
fn is_csharp_class_member(node: Node) -> bool {
    owning_type(node).is_some_and(|owner| {
        matches!(
            owner.kind(),
            "class_declaration"
                | "struct_declaration"
                | "record_declaration"
                | "record_struct_declaration"
        )
    })
}

/// Whether a C# method implements an interface member explicitly
/// (`void IFoo.Bar()`): it has no access modifier, by rule, and the interface
/// is how it is called.
fn is_explicit_interface_implementation(node: Node) -> bool {
    let declaration = carrier(node);
    let mut cursor = declaration.walk();
    let found = declaration
        .children(&mut cursor)
        .any(|child| child.kind() == "explicit_interface_specifier");
    found
}

/// Whether a Java or C# declaration can be called from outside the corpus.
///
/// `public` and C#'s `internal` (the whole assembly) are visible; `private` is
/// not; `protected` is not visible as such and is judged by
/// [`exemption`], which knows whether a subclass outside the corpus can exist.
/// With no access modifier, a C# class or struct member is private by the
/// language's own rule, and anything else keeps the generic answer — a Java
/// package-private member is visible to the rest of its package, which may lie
/// outside the corpus.
pub(crate) fn is_exported(lang: &str, node: Node, source: &str, name: &str) -> bool {
    let words = modifier_words(node, source);
    if has(&words, "public") || has(&words, "internal") {
        return true;
    }
    if has(&words, "private") || has(&words, "protected") {
        return false;
    }
    if lang == "csharp"
        && is_csharp_class_member(node)
        && !is_explicit_interface_implementation(node)
    {
        return false;
    }
    !name.starts_with('_')
}

/// Why a Java or C# declaration is reachable with no call site in the corpus.
pub(crate) fn exemption(
    lang: &str,
    node: Node,
    source: &str,
    declaration: &Declaration,
) -> Option<(WiringKind, &'static str)> {
    let words = modifier_words(node, source);
    if lang == "csharp" && declaration.name == "Main" && has(&words, "static") {
        return Some((WiringKind::RuntimeEntryPoint, "C# program entry point"));
    }
    if lang == "csharp"
        && declaration.declared_kind == SymbolKind::Function
        && has(&words, "partial")
        && !has(&words, "public")
    {
        return Some((
            WiringKind::StructuralExempt,
            "C# partial method — its other half, often generated, may be the caller",
        ));
    }
    // `private protected` (C#) is visible only to subclasses in the same
    // assembly: no outside subclass can reach it.
    if !has(&words, "protected") || has(&words, "private") {
        return None;
    }
    let owner = owning_type(node)?;
    let owner_words = modifier_words(owner, source);
    let owner_is_public = has(&owner_words, "public") || has(&owner_words, "protected");
    let sealed = has(&owner_words, "final")
        || has(&owner_words, "sealed")
        || has(&owner_words, "static")
        || matches!(
            owner.kind(),
            "struct_declaration" | "record_struct_declaration" | "enum_declaration"
        );
    (owner_is_public && !sealed).then_some((WiringKind::StructuralExempt, PROTECTED_REASON))
}
