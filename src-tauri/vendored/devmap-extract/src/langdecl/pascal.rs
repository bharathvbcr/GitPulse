//! Pascal, Delphi and Free Pascal declarations.
//!
//! Pascal emitted **no declaration at all**. Measured on a unit declaring
//! `function Helper` and `procedure Run` before this module existed:
//! `extract_file` produced one symbol, the `File` node, and eight references
//! every one of which was attributed to the file. `tree-sitter-pascal` spells a
//! routine `defProc`/`declProc` and a type `declType`, none of which
//! `generic_symbol_kind` names.
//!
//! # Only definitions are emitted, and that is the point
//!
//! A Pascal unit declares each routine **twice** in one file: once in the
//! `interface` section as a `declProc`, once in the `implementation` section as
//! a `defProc`. A class does the same — `procedure Go;` inside the `declClass`,
//! and `procedure TWorker.Go;` in the implementation — and a `forward`
//! declaration is a third spelling of the first. Emitting a symbol for every
//! `declProc` would therefore emit two or three nodes carrying **one** qualified
//! name for every routine in every unit: a broken join key (SC14) by
//! construction, not as a corner case.
//!
//! So the definition wins. It is the node that owns the body, which makes it the
//! node `langcalls::scope` must find when it attributes a call, and the
//! definition's `TWorker.Go` name form is what supplies the owner a
//! `declProc` inside a class cannot state on its own.
//!
//! The cost is stated rather than hidden: a routine declared and never defined
//! in the same file — an `abstract` method, an `external` binding, a method of a
//! COM-style `interface` — emits no symbol, so a call to it resolves to nothing.
//! That is the same answer this extractor gave for *every* Pascal routine before
//! this module, and a strict improvement over emitting two nodes that no edge
//! can tell apart.

use tree_sitter::Node;

use crate::model::SymbolKind;
use crate::treesitter::{get_node_text, is_callee_identity};

use super::Declaration;

/// A Pascal declaration, or `None` when `node` declares nothing this graph
/// records.
///
/// Deliberately **not** falling through to [`super::generic`]: the shared
/// node-kind table names nothing this grammar produces, so routing through it
/// would only add a second answer that is always `None`.
pub(crate) fn declaration(node: Node, source: &str) -> Option<Declaration> {
    match node.kind() {
        "defProc" => routine(node, source),
        "declType" => type_declaration(node, source),
        _ => None,
    }
}

/// `function Helper(…): Integer; begin … end;` and
/// `procedure TWorker.Go(…); begin … end;`.
///
/// A routine nested inside another routine — Pascal's local procedures, which
/// sit in the outer `defProc`'s `local` field — is named flat, with no outer
/// segment, because that is what a call to it spells and because prefixing it
/// would be the nested-scope mismatch that orphaned every nested Lua call.
fn routine(node: Node, source: &str) -> Option<Declaration> {
    let header = node.child_by_field_name("header")?;
    if header.kind() != "declProc" {
        return None;
    }
    let name_node = header.child_by_field_name("name")?;
    let (owner, name) = split_qualified_name(name_node, source)?;
    // `kConstructor` and `kDestructor` are routines with bodies exactly as
    // `kFunction` and `kProcedure` are; `Declaration::emitted_kind` promotes
    // any of them to `Method` once an owner is present.
    Declaration::new(SymbolKind::Function, owner, name)
}

/// `(owner, name)` for a routine's `name` field.
///
/// A definition names its owner inline — `TWorker.Go` parses as `genericDot`,
/// and `TA.TB.Go` nests to the left — so the owner is everything before the
/// last dot. Taking the whole prefix rather than only its last segment keeps a
/// member of a nested type from colliding with a same-named member of a
/// differently nested one (SC14), which is the rule
/// [`super::enclosing_owner_path`] states for the languages that reach it.
fn split_qualified_name(name_node: Node, source: &str) -> Option<(Option<String>, String)> {
    match name_node.kind() {
        "identifier" => {
            let name = get_node_text(name_node, source);
            is_callee_identity(&name).then_some((None, name))
        }
        "genericDot" => {
            let name = identity_of(name_node.child_by_field_name("rhs")?, source)?;
            let owner = name_node
                .child_by_field_name("lhs")
                .map(|lhs| get_node_text(lhs, source))
                .filter(|owner| !owner.is_empty());
            Some((owner, name))
        }
        // A generic routine's name (`function Wrap<T>: T;`) or anything else
        // this grammar puts here is refused rather than recorded as its own
        // source text, which would be a name no call can join to (SC32).
        _ => None,
    }
}

/// `type TWorker = class … end;` — but only for the type forms that own
/// members.
///
/// A plain alias (`type TIndex = Integer;`) and a pointer or array type declare
/// no members and answer no query this graph can serve, so they are refused
/// rather than mapped onto whichever `SymbolKind` is nearest. Every kind below
/// is read from the type's own leading keyword, which is where this grammar
/// puts the distinction: `declClass` covers `class`, `record` **and** `object`,
/// and only the keyword tells them apart.
fn type_declaration(node: Node, source: &str) -> Option<Declaration> {
    let name = identity_of(node.child_by_field_name("name")?, source)?;
    let kind = type_kind(node.child_by_field_name("type")?)?;
    Declaration::new(kind, None, name)
}

/// The symbol kind a `declType`'s type expression declares, or `None` when it
/// declares no member-owning type.
fn type_kind(type_node: Node) -> Option<SymbolKind> {
    match type_node.kind() {
        "declIntf" => Some(SymbolKind::Interface),
        "declClass" => match first_child_kind(type_node)? {
            "kRecord" => Some(SymbolKind::Struct),
            // `object` is Turbo Pascal's class; it owns methods exactly as
            // `class` does.
            "kClass" | "kObject" => Some(SymbolKind::Class),
            _ => None,
        },
        // An enumeration arrives wrapped: `type TEnum = (eOne, eTwo);` is a
        // `type` node containing a `declEnum`.
        "type" => (first_child_kind(type_node)? == "declEnum").then_some(SymbolKind::Enum),
        _ => None,
    }
}

fn first_child_kind(node: Node) -> Option<&'static str> {
    node.named_child(0).map(|child| child.kind())
}

/// The text of an `identifier` node, refused unless it is an identity a call
/// can join to.
fn identity_of(node: Node, source: &str) -> Option<String> {
    if node.kind() != "identifier" {
        return None;
    }
    let name = get_node_text(node, source);
    is_callee_identity(&name).then_some(name)
}

/// Whether a Pascal declaration is reachable from outside the indexed corpus.
///
/// Always `true`, and stated here rather than left to the generic fallback so
/// the reason is on the record: Pascal's visibility lives in the *interface*
/// section and in a class's `declSection (kPublic)` / `(kPrivate)` headers,
/// neither of which is part of the `defProc` this module emits from — the
/// definition in the `implementation` section carries no modifier at all. The
/// generic fallback would answer from a leading-underscore convention Pascal
/// does not use, and `false` on no evidence is a dead-code false positive,
/// which is a proposal to delete working code.
pub(crate) fn is_exported(_node: Node, _source: &str) -> bool {
    true
}
