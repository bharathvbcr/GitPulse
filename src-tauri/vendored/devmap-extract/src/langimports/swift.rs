//! Swift `import`.
//!
//! `import Foundation` names a **module**, not a file. That used to be a reason
//! to decline extraction: same-target Swift files import each other not at all,
//! so a specifier that never names a path cannot answer `unwired_candidates`
//! by itself.
//!
//! The decline was the wrong conclusion from a true premise. A Swift module is
//! the same shape as a Go package — one unqualified namespace spanning every
//! file in a target — and `import MarkDevKit` is the statement that wires the
//! *module*, the way `import "app/store"` wires the package. The resolver maps
//! the specifier onto that module (or records it as external when the module
//! is Foundation, XCTest, SwiftUI, …). Same-module calls still need no import;
//! they resolve through the module's shared namespace, exactly as Java's
//! same-package calls do.
//!
//! Kinded imports (`import struct Foundation.Date`) name a member of a module.
//! The module is the first path segment; the member is recorded so
//! classification can see `Date` as coming from `Foundation`.

use tree_sitter::Node;

use super::{file_import, named_import};
use crate::model::ExtractedImport;
use crate::treesitter::{get_node_text, node_span};

/// Tokens tree-sitter-swift leaves as anonymous children of a kinded import.
///
/// The grammar does not wrap them in an `import_kind` node: `import struct
/// Foundation.Date` is `import` + `struct` + `identifier`.
const IMPORT_KINDS: &[&str] = &[
    "typealias",
    "struct",
    "class",
    "enum",
    "protocol",
    "let",
    "var",
    "func",
];

pub(crate) fn extract_import(node: Node, source: &str, imports: &mut Vec<ExtractedImport>) {
    if node.kind() != "import_declaration" {
        return;
    }
    let mut kinded = false;
    let mut identifier = None;
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        if IMPORT_KINDS.contains(&child.kind()) {
            kinded = true;
        }
        if child.kind() == "identifier" {
            identifier = Some(child);
        }
    }
    let Some(identifier) = identifier else {
        return;
    };
    let path = get_node_text(identifier, source);
    let path = path.trim();
    if path.is_empty() {
        return;
    }
    let raw = get_node_text(node, source);
    let span = node_span(node);
    if kinded {
        let (specifier, member) = match path.split_once('.') {
            Some((module, rest)) if !module.is_empty() => {
                let member = rest.rsplit('.').next().unwrap_or(rest);
                (module.to_string(), member.to_string())
            }
            _ => (path.to_string(), String::new()),
        };
        if member.is_empty() {
            imports.push(file_import(&raw, specifier, span));
        } else {
            imports.push(named_import(&raw, specifier, vec![member], span));
        }
    } else {
        imports.push(file_import(&raw, path.to_string(), span));
    }
}
