//! Shell `source` and `.`.
//!
//! A shell script has no import keyword. Loading another file is the builtin
//! `source` (and its POSIX spelling `.`), which names a path outright when the
//! argument is a literal word. `source "$HELPER"` computes its path at runtime
//! and is refused: emitting the expansion's text would invent a specifier the
//! author never wrote.

use tree_sitter::Node;

use super::file_import;
use crate::model::ExtractedImport;
use crate::treesitter::{get_node_text, node_span};

pub(crate) fn extract_source(node: Node, source: &str, imports: &mut Vec<ExtractedImport>) {
    if node.kind() != "command" {
        return;
    }
    let Some(name_node) = node.child_by_field_name("name") else {
        return;
    };
    if !matches!(get_node_text(name_node, source).as_str(), "source" | ".") {
        return;
    }
    let Some(argument) = node.child_by_field_name("argument") else {
        return;
    };
    // Only a `word` is a static path. A `string` here is `"$HELPER"` or
    // `"$(find_helper)"` — computed, and the same refusal `first_string_literal`
    // applies to a nested invocation in the other languages.
    if argument.kind() != "word" {
        return;
    }
    let specifier = get_node_text(argument, source);
    let specifier = specifier.trim();
    if specifier.is_empty() || specifier.contains('$') {
        return;
    }
    // `alias = "."` is the resolver's glob marker, and `source` is a glob: it
    // runs the file in the current shell, so every function and variable it
    // defines is in scope afterwards under its own name. There is no local
    // binding name to record — nothing is renamed and nothing is namespaced.
    //
    // Emitted as a plain `file_import` at first, which resolved the file but
    // bound none of its names. The functions it defines were then left to the
    // global tier, which matches on the name alone: in this repository
    // `verify.sh`'s calls to `peak_rss_bytes` landed on **six archived copies**
    // of `peak_rss.sh` under `benchmarks/results/competition/` and never on
    // `rust/tools/peak_rss.sh`, the file it sources. Reusing the existing glob
    // marker rather than adding a shell arm beside it keeps one owner for
    // "this import brings in everything".
    let mut import = file_import(
        &get_node_text(node, source),
        specifier.to_string(),
        node_span(node),
    );
    import.alias = Some(".".to_string());
    imports.push(import);
}
