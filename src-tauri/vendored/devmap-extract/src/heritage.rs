//! Supertypes named in a declaration, and the type that names them.
//!
//! `ReferenceKind::Heritage` and `EdgeKind::Extends` / `Implements` were all
//! declared and none produced. The consumer side was already written —
//! `resolver.rs` reads `Heritage` to set `prefer_types` — so the variant was
//! dead on the producer side only, and superclass names arrived as
//! `ReferenceKind::Name`, indistinguishable from any other identifier.
//!
//! The cost: a method reached only through its base type has no inbound edge,
//! so every override is a candidate `extracted` false positive.
//!
//! **One owner, not fifteen match arms.** `extract_node` is already a
//! thousand-line per-language match; adding heritage there would scatter the
//! same decision across every arm and guarantee the arms drift. This is called
//! once from `walk_tree`, beside `extract_node`, and every language's node
//! kinds sit in one table that can be read against the grammars.
//!
//! **Node kinds are measured, not remembered.** Every kind below was read off
//! an actual parse of the language in question; the fixtures that produced them
//! are in `tests/heritage.rs`, which fails if a grammar renames one.

use crate::model::{ExtractedReference, ReferenceKind, Span};
use tree_sitter::Node;

/// Whether a supertype was stated as an interface the type implements.
///
/// **Only where the grammar says so.** Java's `super_interfaces`, TypeScript's
/// `implements_clause`, PHP's `class_interface_clause`, Dart's `interfaces`,
/// Objective-C's protocol list and Rust's `impl Trait for Type` each name the
/// relation explicitly. C#, Swift, Kotlin, Python and Solidity use one syntax
/// for both, and there is no way to tell a base class from an interface without
/// resolving the supertype and inspecting it — which the extractor cannot do.
///
/// Those languages therefore yield `Extends`, because that is what the source
/// states. Guessing `Implements` from a naming convention would make the edge
/// kind mean "the name started with I", which is the class of claim this kernel
/// exists not to make.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Relation {
    Extends,
    Implements,
}

impl Relation {
    fn reference_kind(self) -> ReferenceKind {
        match self {
            Relation::Extends => ReferenceKind::Heritage,
            Relation::Implements => ReferenceKind::HeritageInterface,
        }
    }
}

fn text<'a>(node: Node, source: &'a str) -> &'a str {
    source
        .get(node.start_byte()..node.end_byte())
        .unwrap_or_default()
}

/// A supertype name, stripped of the decoration a grammar leaves attached.
///
/// `Base()` in Kotlin's `delegation_specifier` is a constructor invocation;
/// `public Base` in C++ carries the access specifier; `Base<T>` carries type
/// arguments. All three name the same supertype, and the resolver matches on a
/// symbol name.
fn supertype_name(raw: &str) -> Option<String> {
    let mut name = raw.trim();
    // Drop an access specifier or `virtual` prefix (C++).
    for prefix in ["public ", "private ", "protected ", "virtual "] {
        name = name.strip_prefix(prefix).unwrap_or(name).trim_start();
    }
    // Drop a constructor call (Kotlin, Scala) and then type arguments.
    //
    // Sequentially, not chained: written as one chain, the second
    // `unwrap_or(name)` falls back to the *original* string and silently
    // discards the first strip, so `Base()` came back as `Base()` whenever it
    // carried no type arguments. The integration matrix did not catch it —
    // Kotlin's `delegation_specifier` nests a `user_type` whose text is already
    // bare — which is why the unit test below exercises the function directly.
    let name = match name.split_once('(') {
        Some((head, _)) => head,
        None => name,
    };
    let name = match name.split_once('<') {
        Some((head, _)) => head,
        None => name,
    };
    let name = name.trim();
    // Keep the last segment of a qualified name: the resolver indexes symbols
    // by their own name, so `com.example.Base` must be looked up as `Base`.
    let name = name.rsplit(['.', ':']).next().unwrap_or(name).trim();
    (!name.is_empty()
        && name
            .chars()
            .next()
            .is_some_and(|c| c.is_alphabetic() || c == '_'))
    .then(|| name.to_string())
}

/// Node kinds inside a heritage clause that name a modifier, not a supertype.
///
/// C++ makes `public` a *named* child of `base_class_clause`, so recursing over
/// named children without this reports `class W : public Base` as extending two
/// types, one of them called `public`. Stripping the word from the clause text
/// is not enough — the recursion reaches the access specifier as its own node,
/// where there is no prefix left to strip.
fn is_modifier_node(kind: &str) -> bool {
    matches!(
        kind,
        "access_specifier"
            | "virtual"
            | "visibility_modifier"
            | "annotation"
            | "marker_annotation"
            | "comment"
            | "line_comment"
            | "block_comment"
    )
}

/// Node kinds that are a *complete* type reference: stop, do not recurse.
///
/// Java parses `com.example.Base<String>` as
/// `generic_type > scoped_type_identifier > type_identifier × 3` plus a
/// `type_arguments` child. Recursing to the leaves therefore yielded four
/// supertypes — `Base`, `String`, `com` and `example` — three of which are not
/// types at all and one of which is a type argument. Taking the whole node's
/// text and letting `supertype_name` strip the decoration yields the one name
/// the resolver can look up.
///
/// Listed as terminals rather than listing the containers to recurse through,
/// so an unrecognised container still descends and finds its type nodes. The
/// failure mode of the other direction is silence.
fn is_terminal_type_node(kind: &str) -> bool {
    matches!(
        kind,
        "type_identifier"
            | "nested_type_identifier"
            | "scoped_type_identifier"
            | "generic_type"
            | "member_expression"
            | "qualified_name"
            | "scoped_identifier"
            | "user_type"
            | "user_defined_type"
            | "type_name"
            | "identifier"
            | "simple_identifier"
            | "constant"
            | "name"
            | "type"
    )
}

/// Type arguments are not supertypes.
///
/// `class W extends Base<String>` extends `Base`, not `String`. Left in, every
/// generic base class would emit an edge to each of its type parameters.
fn is_type_argument_node(kind: &str) -> bool {
    matches!(kind, "type_arguments" | "type_parameters")
}

/// Every supertype `node` names.
fn names_under(node: Node, source: &str, out: &mut Vec<String>) {
    if is_modifier_node(node.kind()) || is_type_argument_node(node.kind()) {
        return;
    }
    if is_terminal_type_node(node.kind()) {
        if let Some(name) = supertype_name(text(node, source)) {
            out.push(name);
        }
        return;
    }
    let mut cursor = node.walk();
    let children: Vec<Node> = node
        .named_children(&mut cursor)
        .filter(|child| !is_modifier_node(child.kind()) && !is_type_argument_node(child.kind()))
        .collect();
    if children.is_empty() {
        if let Some(name) = supertype_name(text(node, source)) {
            out.push(name);
        }
        return;
    }
    for child in children {
        names_under(child, source, out);
    }
}

/// The declaring type's own name, from the container node.
fn declared_type_name<'a>(node: Node, source: &'a str) -> Option<&'a str> {
    if let Some(name) = node.child_by_field_name("name") {
        return Some(text(name, source));
    }
    // Grammars without a `name` field: take the first identifier-ish child.
    let mut cursor = node.walk();
    let found = node.named_children(&mut cursor).find(|child| {
        matches!(
            child.kind(),
            "identifier" | "type_identifier" | "constant" | "name" | "simple_identifier"
        )
    })?;
    Some(text(found, source))
}

/// Heritage clauses for one container node, as `(relation, node)` pairs.
///
/// Returns `None` when this node is not a type declaration in this language,
/// which is the overwhelmingly common case — this runs on every node of every
/// walk, so the miss has to be one `match` on a `&str`.
fn heritage_clauses<'a>(node: Node<'a>, lang: &str) -> Option<Vec<(Relation, Node<'a>)>> {
    let container = node.kind();
    let interesting = matches!(
        (lang, container),
        (
            "typescript" | "tsx" | "javascript",
            "class_declaration" | "class"
        ) | ("python", "class_definition")
            | (
                "java",
                "class_declaration" | "interface_declaration" | "record_declaration"
            )
            | (
                "csharp",
                "class_declaration" | "struct_declaration" | "interface_declaration"
            )
            | ("cpp" | "cuda", "class_specifier" | "struct_specifier")
            | ("ruby", "class")
            | ("php", "class_declaration" | "interface_declaration")
            | ("swift", "class_declaration" | "protocol_declaration")
            | ("kotlin", "class_declaration" | "object_declaration")
            | (
                "scala",
                "class_definition" | "object_definition" | "trait_definition"
            )
            | ("dart", "class_declaration")
            | ("rust", "impl_item")
            | ("objc", "class_interface" | "class_implementation")
            | ("solidity", "contract_declaration" | "interface_declaration")
    );
    if !interesting {
        return None;
    }

    let mut clauses = Vec::new();
    let mut cursor = node.walk();
    for child in node.named_children(&mut cursor) {
        // Kinds verified against a real parse of each language; see
        // `tests/heritage.rs`.
        let relation = match (lang, child.kind()) {
            // `class_heritage` wraps `extends_clause` and `implements_clause`
            // in TypeScript; in JavaScript it holds the base directly.
            ("typescript" | "tsx" | "javascript", "class_heritage") => {
                collect_ts_heritage(child, &mut clauses);
                continue;
            }
            // Python states no relation: `class W(Base, Mixin)` is an
            // `argument_list` and a metaclass keyword argument lives there too.
            ("python", "argument_list") => Relation::Extends,
            ("java", "superclass") => Relation::Extends,
            ("java", "super_interfaces" | "extends_interfaces") => Relation::Implements,
            ("csharp", "base_list") => Relation::Extends,
            ("cpp" | "cuda", "base_class_clause") => Relation::Extends,
            ("ruby", "superclass") => Relation::Extends,
            ("php", "base_clause") => Relation::Extends,
            ("php", "class_interface_clause") => Relation::Implements,
            ("swift", "inheritance_specifier") => Relation::Extends,
            ("kotlin", "delegation_specifiers" | "delegation_specifier") => Relation::Extends,
            ("scala", "extends_clause") => Relation::Extends,
            ("dart", "superclass") => Relation::Extends,
            ("dart", "interfaces" | "mixins") => Relation::Implements,
            ("solidity", "inheritance_specifier") => Relation::Extends,
            _ => continue,
        };
        clauses.push((relation, child));
    }

    if lang == "rust" {
        collect_rust_impl(node, &mut clauses);
    }
    if lang == "objc" {
        collect_objc_heritage(node, &mut clauses);
    }
    Some(clauses)
}

/// TypeScript separates the two clauses; JavaScript has only the base.
fn collect_ts_heritage<'a>(heritage: Node<'a>, clauses: &mut Vec<(Relation, Node<'a>)>) {
    let mut cursor = heritage.walk();
    let mut saw_clause = false;
    for child in heritage.named_children(&mut cursor) {
        match child.kind() {
            "extends_clause" => {
                saw_clause = true;
                clauses.push((Relation::Extends, child));
            }
            "implements_clause" => {
                saw_clause = true;
                clauses.push((Relation::Implements, child));
            }
            _ => {}
        }
    }
    if !saw_clause {
        // JavaScript: `class_heritage` holds `extends` and the base directly.
        clauses.push((Relation::Extends, heritage));
    }
}

/// `impl Doer for Thing` — the trait is the supertype, the type is the
/// declarer. `impl Thing { … }` names no supertype and must contribute nothing.
///
/// The grammar gives `impl_item` a `trait` field only in the `for` form, which
/// is exactly the discriminator: without it, this is an inherent impl.
fn collect_rust_impl<'a>(node: Node<'a>, clauses: &mut Vec<(Relation, Node<'a>)>) {
    if let Some(trait_node) = node.child_by_field_name("trait") {
        clauses.push((Relation::Implements, trait_node));
    }
}

/// `@interface W : Base <IFace>` — the superclass is the *second* identifier,
/// and the protocol list is a separate node.
///
/// The first identifier is the class being declared, so taking every identifier
/// would make every class its own supertype.
fn collect_objc_heritage<'a>(node: Node<'a>, clauses: &mut Vec<(Relation, Node<'a>)>) {
    let mut cursor = node.walk();
    let mut identifiers = node
        .named_children(&mut cursor)
        .filter(|child| child.kind() == "identifier");
    let _declared = identifiers.next();
    if let Some(superclass) = identifiers.next() {
        clauses.push((Relation::Extends, superclass));
    }
    let mut cursor = node.walk();
    for child in node.named_children(&mut cursor) {
        if child.kind() == "parameterized_arguments" || child.kind() == "protocol_qualifiers" {
            clauses.push((Relation::Implements, child));
        }
    }
}

/// Record every supertype this node names, attributed to the type that names it.
///
/// Called once per node from `walk_tree`. The `heritage_clauses` miss is one
/// string match, so the cost on a node that is not a type declaration — which
/// is nearly all of them — is a `match` and a return.
pub(crate) fn push_heritage_references(
    node: Node,
    source: &str,
    lang: &str,
    file_symbol_name: &str,
    references: &mut Vec<ExtractedReference>,
) {
    let Some(clauses) = heritage_clauses(node, lang) else {
        return;
    };
    if clauses.is_empty() {
        return;
    }

    // Rust attributes the impl to the type, not to the `impl` block, which has
    // no name of its own.
    let declarer = if lang == "rust" {
        node.child_by_field_name("type").map(|n| text(n, source))
    } else {
        declared_type_name(node, source)
    };
    let Some(declarer) = declarer.and_then(supertype_name) else {
        // No declaring type means an edge with one endpoint, which is not an
        // edge. Dropping is the honest outcome; inventing a source symbol would
        // attribute the supertype to the file and claim the file extends it.
        return;
    };
    let enclosing = format!("{file_symbol_name}::{declarer}");

    for (relation, clause) in clauses {
        let mut names = Vec::new();
        names_under(clause, source, &mut names);
        for name in names {
            // A type never extends itself. Python's `argument_list` also
            // carries `metaclass=…` keyword arguments, and a self-reference
            // would make every such class its own supertype.
            if name == declarer {
                continue;
            }
            references.push(ExtractedReference {
                name,
                kind: relation.reference_kind(),
                span: Span {
                    start_byte: clause.start_byte(),
                    end_byte: clause.end_byte(),
                },
                enclosing_symbol: Some(enclosing.clone()),
                assigned_to: None,
                receiver_expr: None,
            });
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_supertype_name_is_stripped_of_grammar_decoration() {
        assert_eq!(supertype_name("Base"), Some("Base".to_string()));
        assert_eq!(supertype_name("public Base"), Some("Base".to_string()));
        assert_eq!(supertype_name("Base()"), Some("Base".to_string()));
        assert_eq!(supertype_name("Base<T>"), Some("Base".to_string()));
        assert_eq!(
            supertype_name("com.example.Base"),
            Some("Base".to_string()),
            "the resolver indexes symbols by their own name"
        );
        assert_eq!(supertype_name(""), None);
        assert_eq!(supertype_name("   "), None);
        assert_eq!(
            supertype_name("123"),
            None,
            "a supertype name starts like an identifier"
        );
    }
}
