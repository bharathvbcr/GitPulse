//! Nix declarations.
//!
//! Nix emitted **no declaration at all**: measured on a realistic `flake.nix`,
//! `extract_file` produced one symbol — the `File` node — and zero calls.
//! Nothing in `generic_symbol_kind` names a `binding`, which is the only thing
//! Nix declares.
//!
//! # Only a binding whose value is a lambda is a declaration
//!
//! Nix has no function syntax. `helper = x: x + 1;` is an attribute bound to a
//! lambda, spelled identically to `name = "demo";` — a `binding` node either
//! way — so the distinction has to come from the value, and it does:
//! `function_expression` is the lambda, in both its `x: …` and `{ a, b }: …`
//! forms. Emitting every binding instead would put a symbol on every attribute
//! of every attrset in every file, which on a nixpkgs-shaped tree is thousands
//! of nodes naming string literals and paths, and would collide constantly on
//! the attribute names Nix reuses everywhere (`name`, `src`, `default`,
//! `version`) — a broken join key (SC14) many times over per file.
//!
//! Restricting to lambdas also keeps the duplicate risk low where it matters:
//! measured on the same `flake.nix`, three bindings are lambda-valued
//! (`outputs`, `forAllSystems`, `mkShellFor`) and all three names are distinct,
//! while `default` — the name that does repeat, once under `packages` and once
//! under `devShells` — is bound to a derivation and is not emitted.
//!
//! # A dotted attribute path is a name, not an owner
//!
//! `overlays.default = final: prev: …` binds through an `attrpath` of two
//! attributes, and the whole path is the symbol's *name*, with no owner.
//!
//! Splitting it into owner `overlays` + name `default` was tried first and
//! rejected on measurement: `Declaration` promotes a `Function` with an owner
//! to a `Method`, and the resolver then emits a second `Contains` edge **from
//! the owner** — so every dotted binding produced an edge sourced at
//! `f.nix::overlays`, a symbol that does not exist and cannot, because an
//! attribute-path prefix in Nix is not a declaration. That is a dangling edge
//! of exactly the SC9/SC10 shape, traded for a dispatch relationship Nix does
//! not have: it has no types, so there is no receiver type for a method to
//! hang off.
//!
//! Keeping the whole path is unique — `packages.default` and
//! `devShells.default` stay distinct, where a bare `default` would collide
//! (SC14) — and it is contained by the file, which is what actually contains
//! it. The cost is stated: a call site writes `overlays.default x`, whose
//! callee is `default` reached through receiver `overlays`, so a dotted binding
//! is a node that a call cannot yet name. It is still a symbol search,
//! dead-code and containment can see, and calls made *inside* its body are
//! attributed to it rather than to the file.

use tree_sitter::Node;

use crate::model::SymbolKind;
use crate::treesitter::{get_node_text, is_callee_identity};

use super::Declaration;

/// A Nix declaration, or `None` when `node` declares nothing this graph
/// records.
///
/// Deliberately **not** falling through to [`super::generic`]: the shared
/// node-kind table names nothing this grammar produces, so routing through it
/// would only add a second answer that is always `None`.
pub(crate) fn declaration(node: Node, source: &str) -> Option<Declaration> {
    if node.kind() != "binding" {
        return None;
    }
    // `inherit x;` and `inherit (pkgs) lib;` are `inherit`/`inherit_from`
    // nodes, not `binding`s, so they need no arm: they re-export a name bound
    // elsewhere and declare nothing of their own.
    let value = node.child_by_field_name("expression")?;
    if value.kind() != "function_expression" {
        return None;
    }
    let name = attrpath_name(node.child_by_field_name("attrpath")?, source)?;
    Declaration::new(SymbolKind::Function, None, name)
}

/// The dotted name an `attrpath` spells, or `None` when any segment is not a
/// plain identifier.
///
/// `"${system}" = …` and `${cfg.name} = …` bind through an interpolation whose
/// text is not knowable here, and a quoted attribute is not an identity a call
/// site can join to. Both are refused rather than recorded as their own source
/// text, which would be a name no edge can match (SC32).
fn attrpath_name(attrpath: Node, source: &str) -> Option<String> {
    let mut segments = Vec::new();
    for index in 0..attrpath.named_child_count() {
        let attr = attrpath.named_child(index)?;
        if attr.kind() != "identifier" {
            return None;
        }
        let text = get_node_text(attr, source);
        if !is_callee_identity(&text) {
            return None;
        }
        segments.push(text);
    }
    (!segments.is_empty()).then(|| segments.join("."))
}
