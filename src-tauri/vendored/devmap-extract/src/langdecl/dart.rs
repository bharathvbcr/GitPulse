//! Dart declarations.
//!
//! Dart emitted **no** function or method symbol at all. `tree-sitter-dart`
//! hangs the name off a `signature` child rather than off the declaration:
//! `function_declaration` carries `signature = function_signature`, whose `name`
//! field holds the identifier, and `method_declaration` nests one level deeper
//! again (`method_signature -> function_signature -> name`). The generic path
//! reads `name` on the declaration itself, finds nothing, then looks for a
//! C-style `declarator` that Dart does not have — so every Dart callable was
//! invisible, and `langcalls::dart` attributed every Dart call to its file
//! because there was no callable symbol to attribute it to.

use tree_sitter::Node;

use crate::model::{SymbolKind, WiringKind};
use crate::treesitter::{get_child_text, get_node_text};

use super::{enclosing_owner, Declaration};

/// A Dart declaration, or `None` when `node` declares nothing this graph
/// records.
pub(crate) fn declaration(node: Node, source: &str) -> Option<Declaration> {
    let (declared_kind, name) = self_identity(node, source)?;
    // An enum constant belongs to its enum by that enum's *full* identity.
    let owner = if declared_kind == SymbolKind::Field {
        super::enclosing_owner_path(node, source, declaration)
    } else {
        enclosing_owner(node, source, self_identity)
    };
    Declaration::new(declared_kind, owner, name)
}

/// What `node` declares, ignoring its ancestors.
fn self_identity(node: Node, source: &str) -> Option<(SymbolKind, String)> {
    match node.kind() {
        "class_declaration" => Some((SymbolKind::Class, get_child_text(node, "name", source)?)),
        // A mixin is a set of members another type composes in. That is what
        // `Trait` already means here — the kind exists because the frozen
        // baseline distinguishes a Rust trait from a TS interface — and it says
        // more than `Class` would.
        "mixin_declaration" => Some((SymbolKind::Trait, get_child_text(node, "name", source)?)),
        // `extension Ext on Foo { … }`. Named, unlike a Swift extension, and
        // callable as a type in its own right (`Ext(foo).extra()`), so it is a
        // declaration rather than only an owner.
        "extension_declaration" => Some((SymbolKind::Class, get_child_text(node, "name", source)?)),
        "enum_declaration" => Some((SymbolKind::Enum, get_child_text(node, "name", source)?)),
        "enum_constant" => Some((SymbolKind::Field, get_child_text(node, "name", source)?)),
        "function_declaration" | "method_declaration" => {
            Some((SymbolKind::Function, signature_name(node, source)?))
        }
        // A *named* constructor only. `Foo.named(…)` is called as a member of
        // `Foo` and joins to `Foo.named`; the unnamed `Foo(…)` is spelled
        // exactly like the type at every call site and already joins to the
        // class node, so emitting `Foo.Foo` beside it would add an identity
        // nothing can reach — the same reasoning that keeps Swift's `init` out.
        "constructor_signature" => Some((SymbolKind::Function, named_constructor(node, source)?)),
        _ => None,
    }
}

/// The identifier a Dart callable's signature carries.
///
/// `function_declaration -> function_signature -> name` and
/// `method_declaration -> method_signature -> function_signature -> name` are
/// the two shapes; the walk follows `signature` links and then the one
/// unlabelled `function_signature` nested under a `method_signature`, so a
/// getter, setter or operator signature — which carry their own `name` — is
/// found at whichever depth the grammar put it.
fn signature_name(node: Node, source: &str) -> Option<String> {
    let mut current = node.child_by_field_name("signature")?;
    for _ in 0..4 {
        if let Some(name) = get_child_text(current, "name", source).filter(|n| !n.is_empty()) {
            return Some(name);
        }
        let mut cursor = current.walk();
        let next = current
            .named_children(&mut cursor)
            .find(|child| child.kind().ends_with("_signature"))?;
        current = next;
    }
    None
}

/// The name of a named constructor, or `None` for the unnamed one.
///
/// `Foo.named(…)` puts three children on the `name` field — `Foo`, `.` and
/// `named` — while `Foo(…)` puts one. The constructor's own identity is the last
/// identifier; the first is the class, which the owner walk supplies.
fn named_constructor(node: Node, source: &str) -> Option<String> {
    let mut cursor = node.walk();
    let identifiers: Vec<Node> = node
        .children_by_field_name("name", &mut cursor)
        .filter(|child| child.kind() == "identifier")
        .collect();
    if identifiers.len() < 2 {
        return None;
    }
    let name = get_node_text(*identifiers.last()?, source);
    (!name.is_empty()).then_some(name)
}

/// Why a Dart declaration is reachable without an observable call site.
pub(crate) fn exemption(
    node: Node,
    source: &str,
    declaration: &Declaration,
) -> Option<(WiringKind, &'static str)> {
    if declaration.declared_kind == SymbolKind::Field && node.kind() == "enum_constant" {
        return Some((
            WiringKind::StructuralExempt,
            "Dart enum constant reachable by bare name in a switch branch",
        ));
    }
    // `@override` is the marker Dart's own analyzer requires on a member that
    // implements a supertype's; the caller is the supertype, which routinely
    // lives in the framework rather than the corpus. Flutter's `build`,
    // `initState` and `dispose` all arrive this way and need no name list.
    if annotation_names(node, source)
        .iter()
        .any(|name| name == "override")
    {
        return Some((
            WiringKind::RuntimeEntryPoint,
            "Dart @override member invoked by the declaring supertype or framework",
        ));
    }
    if declaration.declared_kind == SymbolKind::Function
        && declaration.owner.is_none()
        && declaration.name == "main"
    {
        return Some((WiringKind::RuntimeEntryPoint, "Program entry point"));
    }
    None
}

/// The bare names of the `@`-annotations written on a declaration.
///
/// Dart hangs annotations off the declaration's parent `class_member` for a
/// member and off the declaration itself at top level, so both are read.
fn annotation_names(node: Node, source: &str) -> Vec<String> {
    let mut names = Vec::new();
    for scope in [node.parent(), Some(node)].into_iter().flatten() {
        let mut cursor = scope.walk();
        for child in scope.named_children(&mut cursor) {
            if child.kind() != "annotation" && child.kind() != "marker_annotation" {
                continue;
            }
            let mut inner = scope.walk();
            for part in child.named_children(&mut inner) {
                if part.kind() != "identifier" {
                    continue;
                }
                let text = get_node_text(part, source);
                if !text.is_empty() {
                    names.push(text);
                }
            }
        }
    }
    names
}
