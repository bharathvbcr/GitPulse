//! CFML `template="…"`.
//!
//! Keyed on the **attribute**, not on the tag name, because the grammar does
//! not give the tag name a node for the form that matters most: `<cfinclude>`
//! parses as a `cf_selfclose_tag` whose children are the `<cf` token and its
//! attributes, with `cfinclude` itself swallowed — while `<cfmodule>` parses as
//! a `cf_start_tag` that *does* carry a `cf_tag_name`. Reading the name would
//! mean text surgery on the node's byte range for one of the two shapes.
//!
//! The attribute is the better key regardless: `template` is CFML's file-naming
//! attribute and only ever holds a path. `<cfinclude template="header.cfm">`,
//! `<cfmodule template="m.cfm">` and `<cferror template="err.cfm">` all name a
//! file; `<cfimport taglib="/tags">` names a directory of custom tags under a
//! different attribute and is left alone.
//!
//! `<cfscript> include "util.cfm"; </cfscript>` is **not** extracted: the
//! grammar exposes the script body as one opaque `cf_script_content` token and
//! parses nothing inside it, so there is no node to key on and no way to tell
//! an `include` from any other text. That is a real residual gap, stated rather
//! than papered over with a regex.

use tree_sitter::Node;

use super::file_import;
use crate::model::ExtractedImport;
use crate::treesitter::{get_node_text, node_span};

pub(crate) fn extract_template_attribute(
    node: Node,
    source: &str,
    imports: &mut Vec<ExtractedImport>,
) {
    if node.kind() != "cf_attribute" {
        return;
    }
    let mut cursor = node.walk();
    let children: Vec<Node> = node.named_children(&mut cursor).collect();
    let is_template = children
        .iter()
        .find(|child| child.kind() == "cf_attribute_name")
        .is_some_and(|child| {
            get_node_text(*child, source)
                .trim()
                .eq_ignore_ascii_case("template")
        });
    if !is_template {
        return;
    }
    let Some(quoted) = children
        .iter()
        .find(|child| child.kind() == "quoted_cf_attribute_value")
    else {
        return;
    };
    let mut value_cursor = quoted.walk();
    let Some(specifier) = quoted
        .named_children(&mut value_cursor)
        .find(|child| child.kind() == "attribute_value")
        .map(|value| get_node_text(value, source).trim().to_string())
        .filter(|specifier| !specifier.is_empty())
    else {
        return;
    };
    imports.push(file_import(
        &get_node_text(node, source),
        specifier,
        node_span(node),
    ));
}
