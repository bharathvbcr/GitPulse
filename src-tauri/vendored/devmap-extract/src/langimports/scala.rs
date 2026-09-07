//! Scala `import`.
//!
//! Scala's selector syntax has no counterpart in Java or Kotlin, so it does not
//! share their helper:
//!
//! ```text
//! import foo.bar.Baz          -> foo.bar.Baz
//! import foo.bar.{Baz, Qux}   -> foo.bar          (a package; several files)
//! import foo.bar._            -> foo.bar.*        (Scala 2 wildcard)
//! import foo.bar.*            -> foo.bar.*        (Scala 3 wildcard)
//! ```
//!
//! A brace selector list names several types under one prefix. Emitting the
//! prefix rather than one arbitrary member is the honest reduction: it is the
//! package, and the package-wildcard rung in the resolver is what turns a
//! package into the files under it, under a bound.

use tree_sitter::Node;

use super::file_import;
use crate::model::ExtractedImport;
use crate::treesitter::{get_node_text, node_span};

pub(crate) fn extract_import(node: Node, source: &str, imports: &mut Vec<ExtractedImport>) {
    if node.kind() != "import_declaration" {
        return;
    }
    let raw = get_node_text(node, source);
    let Some(specifier) = scala_specifier(&raw) else {
        return;
    };
    imports.push(file_import(&raw, specifier, node_span(node)));
}

/// Reduce one Scala import clause to a dotted specifier.
///
/// Read from the node's text rather than its children: the grammar spells the
/// path as `stable_identifier`, `namespace_selectors`, `import_selectors` or a
/// bare `identifier` depending on the form *and* on the grammar release, and
/// the text is unambiguous for every one of them. Text surgery is the guess
/// elsewhere in this module; here the alternative is four kind names that were
/// never specified.
fn scala_specifier(raw: &str) -> Option<String> {
    let clause = raw
        .trim()
        .strip_prefix("import")?
        .trim()
        .trim_end_matches(';')
        .trim();
    // Everything from the first brace on is a selector list. What precedes it
    // is the package prefix, and the trailing `.` goes with the brace.
    let head = match clause.split_once('{') {
        Some((prefix, _selectors)) => prefix.trim().trim_end_matches('.').trim(),
        None => clause,
    };
    if head.is_empty() {
        return None;
    }
    // Scala 2 writes the wildcard `_`, Scala 3 writes `*`; both mean "every
    // member of this package", which is the same claim Java's `.*` makes, so
    // both are normalised onto it and share the resolver's one rung.
    let normalised = if let Some(pkg) = head.strip_suffix("._") {
        format!("{pkg}.*")
    } else if let Some(pkg) = head.strip_suffix(".*") {
        format!("{pkg}.*")
    } else if raw.contains('{') {
        // A brace list with no wildcard still names a package.
        format!("{head}.*")
    } else {
        head.to_string()
    };
    (!normalised.is_empty() && normalised != "*").then_some(normalised)
}
