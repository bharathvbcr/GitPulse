//! A declared field's type, for every grammar that declares one.
//!
//! `h.engine.tick()` and `lease.revalidate()` name a method on whatever
//! `engine` and `lease` are, and a typed field declaration is the file's own
//! written answer. Without it the resolver reaches `UninferredReceiver` and the
//! method reads as callerless.
//!
//! # What the 57% is, and is not
//!
//! 57% of this workspace's `UninferredReceiver` rows, and 60% of an independent
//! Swift corpus's, have a bare lowercase identifier for a receiver —
//! `directory`, `lock`, `workspace`. That is the **opportunity size**: the share
//! of rows shaped like something a field declaration *could* answer. It is not
//! a predicted reduction, and this module does not deliver one anywhere near it,
//! because most of those receivers are locals, or fields whose type the author
//! never wrote, or fields typed by a collection this module refuses on purpose.
//!
//! Nor is `unresolved_uninferred_receiver` the number to read it against. Every
//! `Type` reference this module emits is itself an attribution site, so reading
//! more declarations *adds* rows to the denominator while resolving others: on a
//! 306-file Swift corpus the total unresolved count rose from 47,217 to 48,748
//! in the same build where unexplained sites fell by 218 and the dead-symbol
//! list returned to its untouched baseline. Judge it on edges and on explained
//! share, and only against a corpus held identical across the two builds —
//! measured that way here, `uninferred_receiver` fell 66,083 to 66,017 and edges
//! rose 35,476 to 35,530. A run that indexes this module's own new files is
//! comparing two different corpora and will show the count going up.
//!
//! The evidence was already consumed: [`Resolver::field_type_on`] reads
//! `declared_types["{file}:{name}@type"]`, written from any `Type` reference
//! carrying `assigned_to`. It was only ever *produced* by two grammars. Rust
//! grew the arm first and Go copied it, each with its own owner walk and its own
//! type reducer, and the Go copy's comment records the cost of the gap it was
//! closing — "Rust already emits this; Go did not, so every method reached only
//! through a typed field stayed untyped." That sentence was true of the other
//! twenty-nine grammars when it was written.
//!
//! This module is that arm, once, for all of them. Both originals are deleted
//! rather than left beside it: a third copy is how the second one happened.
//!
//! # Where it runs
//!
//! At the universal seam at the end of `treesitter::extract_node`, beside
//! `langimports::extract_imports`, and for its reason — Rust, Go, Python and
//! HCL reach the walk through *specialised* `match lang` arms, and the C family
//! through `c_family`, so an inner position silently excludes whichever
//! language gains a specialised arm next.
//!
//! # What it refuses
//!
//! A wrapper that denotes the same value is unwrapped — a reference, a pointer,
//! an optional. A generic reduces to its head, because `Box<Lease>` is a `Box`
//! and `List<Lease>` is a `List`.
//!
//! A **collection is not its element**, and that is the line this module holds:
//! `var many: [Lease]` makes `many.first()` a call on `Array`, not on `Lease`,
//! so an array, slice, map, tuple or function type yields nothing at all.
//! Answering `Lease` there would hand the resolver a confident wrong type, and
//! a wrong `DETERMINISTIC` edge is worse than the missing one it replaces.
//! Both original arms already abstained here; the rule is theirs, made explicit.

use tree_sitter::Node;

use crate::model::{ExtractedReference, ReferenceKind};
use crate::treesitter::{
    bounded_parent, get_child_text, get_node_text, go_type_qualifier, is_callable_node, node_span,
};

/// Push the `Type` reference a field declaration states, if this node is one.
///
/// Called per AST node in the hottest loop in the workspace, so the shape is a
/// single `match` on the language key and an early return for the overwhelming
/// majority of nodes, which are not field declarations in any grammar.
pub(crate) fn extract_field_type(
    lang: &str,
    node: Node,
    source: &str,
    file_symbol_name: &str,
    references: &mut Vec<ExtractedReference>,
) -> Option<()> {
    let field = field_of(lang, node, source)?;
    if field.name.is_empty() {
        return None;
    }
    let owner = owning_type_name(node, source)?;
    // Both halves are gated on the type resolving, which is the original Go
    // arm's shape and not an accident of it: a package qualifier with no type
    // beside it says a field came from somewhere without saying what it is, and
    // `field_type_on` has nothing to do with the first fact alone.
    let type_name = field.type_name.filter(|name| !name.is_empty())?;
    let enclosing = format!("{file_symbol_name}::{owner}");
    let span = node_span(node);
    // Go names a foreign type through its package (`*pkg.Lease`), and that is
    // the half saying which file the type lives in. `declared_types` namespaces
    // it under `@mod` rather than `@type`, so the two cannot overwrite.
    if let Some(qualifier) = field.qualifier.filter(|q| !q.is_empty()) {
        references.push(ExtractedReference {
            name: qualifier,
            kind: ReferenceKind::TypeQualifier,
            span: span.clone(),
            enclosing_symbol: Some(enclosing.clone()),
            assigned_to: Some(field.name.clone()),
            receiver_expr: None,
        });
    }
    references.push(ExtractedReference {
        name: type_name,
        kind: ReferenceKind::Type,
        span,
        enclosing_symbol: Some(enclosing),
        assigned_to: Some(field.name),
        receiver_expr: None,
    });
    Some(())
}

/// Whether this build reads field types for `lang`, asked of the dispatcher.
///
/// The derivation, in `langimports`' sense: `FIELD_TYPE_LANGUAGES` is checked
/// against *this*, so an arm without an entry or an entry without an arm fails
/// the build rather than being noticed by a corpus probe later.
pub fn extracts_field_types(lang: &str) -> bool {
    field_node_kinds(lang).is_some()
}

/// The language keys this module reads field types for.
///
/// Hand-written only in the sense that the registry test wants a `&[&str]` to
/// iterate; [`extracts_field_types`] is the derivation it is checked against.
pub const FIELD_TYPE_LANGUAGES: &[&str] = &[
    "c",
    "cpp",
    "csharp",
    "cuda",
    "dart",
    "go",
    "java",
    "kotlin",
    "objc",
    "pascal",
    "php",
    "python",
    "rust",
    "scala",
    "solidity",
    "swift",
    "tsx",
    "typescript",
];

/// One field declaration, reduced to what `declared_types` is keyed by.
struct FieldType {
    name: String,
    type_name: Option<String>,
    /// Go only: the package a foreign type is named through.
    qualifier: Option<String>,
}

/// The node kind a field declaration has in `lang`, or `None` for a grammar
/// with no typed field to read.
///
/// Six grammars spell it `field_declaration` and mean six different interior
/// shapes, and three spell a property `property_declaration` and likewise, so
/// this cannot be a match on kind alone: the language key selects the reader,
/// and the kind only gets it to the right one quickly.
///
/// A `match` rather than a table scan, and for the reason `langimports` gives
/// its own dispatcher — this runs per AST node in `treesitter`'s walk, the
/// hottest loop in the workspace, and the compiler turns a string match into a
/// length-and-prefix switch where a `&[(&str, _)]` scan stays eighteen
/// comparisons. [`extracts_field_types`] asks it, so the published list cannot
/// drift from the arms.
fn field_node_kinds(lang: &str) -> Option<&'static [&'static str]> {
    Some(match lang {
        // Metal borrows the `cpp` grammar and arrives here as `cpp`, exactly as
        // it does in `langimports`.
        "c" | "cpp" | "cuda" | "go" | "java" | "rust" => &["field_declaration"],
        "csharp" => &["field_declaration", "property_declaration"],
        "dart" => &["declaration"],
        "kotlin" | "php" | "swift" => &["property_declaration"],
        "objc" => &["struct_declaration"],
        "pascal" => &["declField"],
        // Python has no field declaration; an annotated assignment is one.
        "python" => &["assignment"],
        "scala" => &["val_definition", "var_definition"],
        "solidity" => &["state_variable_declaration"],
        // ArkTS routes to `typescript`; TSX is its own grammar key.
        "tsx" | "typescript" => &["public_field_definition"],
        _ => return None,
    })
}

/// Read `node` as a field declaration in `lang`, or `None` when it is not one.
fn field_of(lang: &str, node: Node, source: &str) -> Option<FieldType> {
    let kinds = field_node_kinds(lang)?;
    let kind = node.kind();
    if !kinds.contains(&kind) {
        return None;
    }
    let plain = |name: Option<String>, type_node: Option<Node>| {
        Some(FieldType {
            name: name?,
            type_name: type_node.and_then(|ty| nominal_type_name(ty, source, 0)),
            qualifier: None,
        })
    };
    match lang {
        // `name:` and `type:` are both direct fields.
        "rust" | "java" | "solidity" => plain(
            get_child_text(node, "name", source).or_else(|| {
                // Java puts the name one level down, on the declarator.
                let declarator = node.child_by_field_name("declarator")?;
                get_child_text(declarator, "name", source)
            }),
            node.child_by_field_name("type"),
        ),
        // Go additionally records the package a foreign type is named through.
        "go" => {
            let type_node = node.child_by_field_name("type");
            Some(FieldType {
                name: get_child_text(node, "name", source)?,
                type_name: type_node.and_then(|ty| nominal_type_name(ty, source, 0)),
                qualifier: type_node.and_then(|ty| go_type_qualifier(ty, source, 0)),
            })
        }
        // `declarator:` is the name itself, not a wrapper around it.
        "c" | "cpp" | "cuda" => plain(
            node.child_by_field_name("declarator")
                .and_then(|d| declarator_name(d, source, 0)),
            node.child_by_field_name("type"),
        ),
        // C# wraps a field's name and type together in `variable_declaration`;
        // a property states both directly.
        "csharp" => {
            if kind == "property_declaration" {
                return plain(
                    get_child_text(node, "name", source),
                    node.child_by_field_name("type"),
                );
            }
            let declaration = node
                .named_children(&mut node.walk())
                .find(|child| child.kind() == "variable_declaration")?;
            let declarator = declaration
                .named_children(&mut declaration.walk())
                .find(|child| child.kind() == "variable_declarator")?;
            plain(
                get_child_text(declarator, "name", source),
                declaration.child_by_field_name("type"),
            )
        }
        // Swift's `name:` is a pattern wrapping the binding; the type sits in a
        // sibling `type_annotation` rather than on a `type:` field.
        "swift" => plain(
            node.child_by_field_name("name")
                .and_then(|pattern| declarator_name(pattern, source, 0)),
            node.named_children(&mut node.walk())
                .find(|child| child.kind() == "type_annotation"),
        ),
        // Kotlin pairs them positionally inside `variable_declaration`.
        "kotlin" => {
            let declaration = node
                .named_children(&mut node.walk())
                .find(|child| child.kind() == "variable_declaration")?;
            let mut cursor = declaration.walk();
            let mut children = declaration.named_children(&mut cursor);
            let name_node = children.next()?;
            plain(
                Some(get_node_text(name_node, source)),
                children.find(|child| child.kind() != "modifiers"),
            )
        }
        // PHP's name is a `$`-sigil variable inside a `property_element`, and
        // the sigil is part of how every receiver in the language is spelled.
        "php" => {
            let element = node
                .named_children(&mut node.walk())
                .find(|child| child.kind() == "property_element")?;
            plain(
                get_child_text(element, "name", source),
                node.child_by_field_name("type"),
            )
        }
        "scala" => plain(
            get_child_text(node, "pattern", source),
            node.child_by_field_name("type"),
        ),
        "typescript" | "tsx" => plain(
            get_child_text(node, "name", source),
            node.child_by_field_name("type"),
        ),
        // Dart lists the names a single type declares.
        "dart" => {
            let list = node
                .named_children(&mut node.walk())
                .find(|child| child.kind() == "initialized_identifier_list")?;
            let first = list
                .named_children(&mut list.walk())
                .find(|child| child.kind() == "initialized_identifier")?;
            plain(
                get_child_text(first, "name", source),
                node.named_children(&mut node.walk())
                    .find(|child| child.kind() == "type"),
            )
        }
        // Objective-C states the type and the declarator as bare siblings.
        "objc" => {
            let declarator = node
                .named_children(&mut node.walk())
                .find(|child| child.kind() == "struct_declarator")?;
            plain(
                declarator_name(declarator, source, 0),
                node.named_children(&mut node.walk())
                    .find(|child| is_type_bearing(child.kind())),
            )
        }
        "pascal" => plain(
            get_child_text(node, "name", source),
            node.child_by_field_name("type"),
        ),
        // Python has no field declaration: an annotated assignment is one,
        // whether written in the class body (`lease: Lease`) or against `self`
        // in a method (`self.lease: Lease = …`), which is the common idiom.
        "python" => {
            let type_node = node.child_by_field_name("type")?;
            let target = node.child_by_field_name("left")?;
            let name = match target.kind() {
                "identifier" => get_node_text(target, source),
                // `self.lease` — the attribute is the field, the receiver is not.
                "attribute" => get_child_text(target, "attribute", source)?,
                _ => return None,
            };
            plain(Some(name), Some(type_node))
        }
        _ => None,
    }
}

/// Whether a node of this kind can carry a type rather than a name.
///
/// Objective-C states a field's type and declarator as bare siblings with no
/// field names, so the type has to be recognised by its own kind.
///
/// Not observable on the shapes tree-sitter-objc produces: the type is always
/// the first named child, so a stub returning `true` gives the same answer for
/// a plain field, a qualified one, a struct tag and a two-field interface —
/// all four measured. Kept rather than reduced to `named_child(0)` because the
/// predicate is what makes an attribute-first shape skip to the type instead of
/// reading the attribute as one; that the current grammar never produces such a
/// shape is a fact about this grammar, not a licence to drop the check.
fn is_type_bearing(kind: &str) -> bool {
    matches!(
        kind,
        "type_identifier"
            | "primitive_type"
            | "sized_type_specifier"
            | "struct_specifier"
            | "union_specifier"
            | "enum_specifier"
            | "qualified_identifier"
            | "template_type"
    )
}

/// The bare name a declarator declares, past any pointer or pattern wrapping.
fn declarator_name(node: Node, source: &str, depth: usize) -> Option<String> {
    if depth > 8 {
        return None;
    }
    match node.kind() {
        "identifier" | "field_identifier" | "simple_identifier" | "type_identifier" => {
            let text = get_node_text(node, source);
            (!text.is_empty()).then_some(text)
        }
        _ => node
            .named_children(&mut node.walk())
            .find_map(|child| declarator_name(child, source, depth + 1)),
    }
}

/// A type expression reduced to the nominal name a receiver dispatches on.
///
/// One reducer for every grammar, because the shapes fall into three classes
/// and the class is legible from the node kind: a wrapper that denotes the same
/// value is unwrapped, a generic reduces to its head, and a leaf is the answer.
/// A collection is none of those and is refused — see the module doc.
fn nominal_type_name(node: Node, source: &str, depth: usize) -> Option<String> {
    if depth > 16 {
        return None;
    }
    let recurse = |child: Node| nominal_type_name(child, source, depth + 1);
    match node.kind() {
        // A collection is not its element. Answering here would type
        // `many.first()` as a call on `Lease` when it is a call on `Array`.
        //
        // Redundant with `_ => None` *today*, and deliberately kept: deleting
        // it is an equivalent mutation, which is exactly what makes it worth
        // writing down. The refusal is the module's headline claim, and a later
        // edit that adds `array_type` to the wrapper arm above would otherwise
        // reverse it with nothing on the page saying it had been decided.
        "array_type" | "slice_type" | "map_type" | "dictionary_type" | "tuple_type"
        | "function_type" | "lambda_type" | "channel_type" => None,
        // Wrappers denoting the same value as their operand.
        "reference_type" | "pointer_type" | "optional_type" | "nullable_type"
        | "type_annotation" | "type" | "type_name" | "named_type" | "user_type"
        | "user_defined_type" | "typeref" | "type_descriptor" | "parenthesized_type"
        | "annotated_type" | "nullable" => node
            .child_by_field_name("type")
            .or_else(|| node.child_by_field_name("name"))
            .or_else(|| node.child_by_field_name("wrapped"))
            .and_then(recurse)
            .or_else(|| node.named_children(&mut node.walk()).find_map(recurse)),
        // A generic is its head: `Box<Lease>` is a `Box`.
        "generic_type" | "template_type" => node
            .child_by_field_name("type")
            .or_else(|| node.child_by_field_name("name"))
            .and_then(recurse)
            .or_else(|| {
                node.named_children(&mut node.walk())
                    .find(|child| {
                        !matches!(child.kind(), "type_arguments" | "template_argument_list")
                    })
                    .and_then(recurse)
            }),
        // `std::vector<Lease>` — the scope is not the type, the name is.
        "qualified_identifier" | "scoped_type_identifier" | "scoped_identifier" => node
            .child_by_field_name("name")
            .and_then(recurse)
            .or_else(|| bare_leaf(node, source)),
        // C spells `struct Lease lease;` with the tag inside the specifier.
        "struct_specifier" | "union_specifier" | "enum_specifier" | "class_specifier" => {
            get_child_text(node, "name", source).filter(|name| !name.is_empty())
        }
        // Go's `pkg.Lease`.
        //
        // Dropping the emptiness check is an equivalent mutation: a name that
        // fails it falls to `bare_leaf`, which reduces the whole `pkg.Lease`
        // text past its dot to the same answer the `name` child would have
        // given. It is kept so an absent or empty `name` reaches the fallback
        // by intent rather than by that coincidence continuing to hold.
        "qualified_type" => get_child_text(node, "name", source)
            .filter(|name| !name.is_empty())
            .or_else(|| bare_leaf(node, source)),
        // A builtin is spelled by its own node kind in every grammar that has
        // one — `u64`, `number`, `uint256`, `unsigned int` — and no corpus
        // symbol can ever bear that name, so typing a field as one buys no
        // method and costs a real answer: `receiver_type_for` returns at the
        // first `Some`, so `u64` would pre-empt whatever evidence came next.
        // Measured: admitting them added 696 rows on this repository alone, all
        // of them `bool`, `u32` and `u64`, and the Rust arm this module
        // replaces had always refused them by simply not matching the kind.
        //
        // Redundant with `_ => None` *today*, and kept for the reason the
        // collection arm above is: deleting it is an equivalent mutation, so
        // nothing fails when a later edit moves these kinds into the leaf arm
        // instead — and that edit is the one that costs 696 wrong rows.
        "primitive_type" | "predefined_type" | "sized_type_specifier" => None,
        "type_identifier" | "identifier" | "simple_identifier" | "name" | "field_identifier" => {
            bare_leaf(node, source)
        }
        _ => None,
    }
}

/// A leaf type's own text, reduced past any path qualification and generics.
fn bare_leaf(node: Node, source: &str) -> Option<String> {
    let text = get_node_text(node, source);
    let bare = text.rsplit("::").next().unwrap_or(&text);
    let bare = bare.rsplit('.').next().unwrap_or(bare);
    let bare = bare.split('<').next().unwrap_or(bare).trim();
    (!bare.is_empty()).then(|| bare.to_string())
}

/// The type a field is declared inside, or `None` at file level.
///
/// One walk for every grammar. Stops at a callable for the reason
/// `treesitter::enclosing_type_name` does: a binding written inside a method is
/// that method's local, and typing it as a field of the surrounding class would
/// claim a member the class does not have.
fn owning_type_name(node: Node, source: &str) -> Option<String> {
    let mut ancestor = bounded_parent(node);
    while let Some(parent) = ancestor {
        match parent.kind() {
            // Objective-C names the interface with a bare identifier, not a
            // `name:` field, and its second identifier is the superclass.
            "class_interface" | "class_implementation" => {
                return parent
                    .named_children(&mut parent.walk())
                    .find(|child| child.kind() == "identifier")
                    .map(|child| get_node_text(child, source))
                    .filter(|name| !name.is_empty());
            }
            "struct_item"
            | "enum_item"
            | "union_item"
            | "type_spec"
            | "class_declaration"
            | "abstract_class_declaration"
            | "class_definition"
            | "struct_declaration"
            | "record_declaration"
            | "interface_declaration"
            | "object_declaration"
            | "trait_item"
            | "contract_declaration"
            | "library_declaration"
            | "class_specifier"
            | "struct_specifier"
            | "union_specifier"
            | "declType"
            | "mixin_declaration"
            | "extension_declaration" => {
                return get_child_text(parent, "name", source).filter(|name| !name.is_empty());
            }
            _ if is_callable_node(parent) => {
                // Python's own idiom is a field written inside `__init__`, so a
                // method body is where its evidence lives rather than a place
                // the walk must refuse. The annotation is on `self`, which is
                // what makes it a field and not a local — `field_of` already
                // required that shape before this walk runs.
                if parent.kind() == "function_definition"
                    && node.kind() == "assignment"
                    && node
                        .child_by_field_name("left")
                        .is_some_and(|left| left.kind() == "attribute")
                {
                    ancestor = bounded_parent(parent);
                    continue;
                }
                return None;
            }
            _ => ancestor = bounded_parent(parent),
        }
    }
    None
}
