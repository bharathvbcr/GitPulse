//! Swift declarations.
//!
//! tree-sitter-swift spells `class`, `struct`, `enum`, `actor` **and**
//! `extension` as one node kind, `class_declaration`, and puts the word that
//! tells them apart in a `declaration_kind` field. A kind table keyed on the
//! node kind alone therefore cannot be right for more than one of them: before
//! this module `enum Mode` was reported `Class`, and `extension Person` emitted
//! a second `App.swift::Person` node beside `struct Person`.
//!
//! Visibility is read from the declaration's own `visibility_modifier` rather
//! than from a substring scan of its source text. The scan read the whole
//! subtree, so a `private` member made its enclosing type private — measured on
//! `App.swift::Person` and `Main.kt::Runner`, both of which then became
//! dead-code candidates on no evidence about themselves at all.

use tree_sitter::Node;

use crate::model::{SymbolKind, WiringKind};
use crate::treesitter::{get_child_text, get_node_text};

use super::{enclosing_owner, Declaration};

/// A Swift declaration, or `None` when `node` declares nothing this graph
/// records.
pub(crate) fn declaration(node: Node, source: &str) -> Option<Declaration> {
    match node.kind() {
        // `extension Person { … }` is not a second declaration of `Person`. It
        // is a place to declare *members* of `Person`, and those members already
        // attach to it through `owner_identity`, which keeps the extension in
        // the owner walk. Emitting the extension too produced two nodes carrying
        // `App.swift::Person`, and an ambiguous target downgrades every call to
        // that name from 1.0 to 0.2 — the SC14 broken join key.
        "class_declaration" if declaration_kind(node, source).as_deref() == Some("extension") => {
            None
        }
        // Swift has no `Constructor` symbol kind and needs none: **every**
        // construction site in the corpus already names the *type*.
        // `Person(name:)` is a `call_expression` whose callee is `Person`, and
        // `Person.init(name:)` is rewritten to the same callee by
        // `langcalls::swift::swift_navigation_call`. A `Person.init` node would
        // therefore be a second identity for an operation that always joins to
        // the first, reachable by nothing — and, now that Swift visibility is
        // read, an immediate dead-code candidate for every initializer in the
        // corpus. `deinit` is called by the runtime and named by nothing at all,
        // and `subscript` is reached through `a[i]`, which `langcalls::swift`
        // deliberately refuses to record as a call.
        "init_declaration" | "deinit_declaration" | "subscript_declaration" => None,
        // A protocol *requirement* is likewise not emitted, and this one was
        // measured rather than assumed. Emitting `Greeter.greet` beside
        // `Person.greet` puts two symbols carrying the short name `greet` in one
        // file, and the resolver has no Swift receiver typing to tell them
        // apart: on the fixture below, `person.greet()` fell from a correct
        // `Person.greet` at 1.0 confidence to a wrong `Greeter.greet` at 0.2.
        // The requirement has no body, so nothing is ever attributed to it and
        // it can only be the target of a call the resolver cannot direct. A node
        // that costs a correct edge and gains an unreachable one is not worth
        // emitting; this becomes worth revisiting when receiver types are
        // inferred from Swift property annotations.
        "protocol_function_declaration" | "protocol_property_declaration" => None,
        _ => {
            let (declared_kind, name) = self_identity(node, source)?;
            // An enum case belongs to its enum by that enum's *full* identity.
            // Swift nests `enum CodingKeys` inside each Codable type, so the
            // bare enum name is not unique within a file.
            let owner = if declared_kind == SymbolKind::Field {
                super::enclosing_owner_path(node, source, declaration)
            } else {
                enclosing_owner(node, source, owner_identity)
            };
            Declaration::new(declared_kind, owner, name)
        }
    }
}

/// What `node` declares, ignoring its ancestors.
fn self_identity(node: Node, source: &str) -> Option<(SymbolKind, String)> {
    match node.kind() {
        "class_declaration" => {
            let kind = match declaration_kind(node, source)?.as_str() {
                "struct" => SymbolKind::Struct,
                "enum" => SymbolKind::Enum,
                // An `actor` is a reference type with its own isolation domain;
                // it owns members exactly as a class does and Swift has no
                // separate kind for it.
                "class" | "actor" => SymbolKind::Class,
                _ => return None,
            };
            Some((kind, get_child_text(node, "name", source)?))
        }
        "protocol_declaration" => {
            Some((SymbolKind::Interface, get_child_text(node, "name", source)?))
        }
        "function_declaration" => {
            Some((SymbolKind::Function, get_child_text(node, "name", source)?))
        }
        // An enum case, keyed on the *name node* rather than on `enum_entry`.
        //
        // `case fast, slow` is one `enum_entry` carrying two `name` fields, and
        // a declaration API that returns one identity per node cannot express
        // that from the entry. Keying on the name makes the multi-case line fall
        // out with no special path, and a `simple_identifier` is a leaf, so it
        // can never be an owner of anything.
        //
        // Emitting these closes the asymmetry SC31 named for C macros:
        // `Payload.text("z")` is already recorded as a call, and without the
        // case there is no node it could ever resolve to.
        "simple_identifier"
            if node
                .parent()
                .is_some_and(|parent| parent.kind() == "enum_entry")
                && is_named_child(node, "name") =>
        {
            Some((SymbolKind::Field, get_node_text(node, source)))
        }
        _ => None,
    }
}

/// Whether `node` occupies `field` on its own parent.
fn is_named_child(node: Node, field: &str) -> bool {
    let Some(parent) = node.parent() else {
        return false;
    };
    let mut cursor = parent.walk();
    if !cursor.goto_first_child() {
        return false;
    }
    loop {
        if cursor.node().id() == node.id() {
            return cursor.field_name() == Some(field);
        }
        if !cursor.goto_next_sibling() {
            return false;
        }
    }
}

/// What `node` owns, for the ancestor walk.
///
/// Differs from `self_identity` in exactly one place, and deliberately: an
/// `extension` owns the members written inside it even though it is not itself
/// a declaration. Dropping it from the walk would emit `App.swift::extra`
/// instead of `App.swift::Person.extra`, orphaning every call to it.
fn owner_identity(node: Node, source: &str) -> Option<(SymbolKind, String)> {
    if node.kind() == "class_declaration"
        && declaration_kind(node, source).as_deref() == Some("extension")
    {
        // `extension Array where Element: Equatable` names `Array`; the `name`
        // field is a `user_type`, whose text is the bare type for a plain
        // extension and `Type<Args>` for a specialised one.
        return get_child_text(node, "name", source).map(|name| (SymbolKind::Class, name));
    }
    self_identity(node, source)
}

/// The keyword in a `class_declaration`'s `declaration_kind` field: `class`,
/// `struct`, `enum`, `actor` or `extension`.
fn declaration_kind(node: Node, source: &str) -> Option<String> {
    get_child_text(node, "declaration_kind", source)
}

/// Swift's five access levels, plus `package`, read from the declaration's own
/// modifier list.
///
/// `private` and `fileprivate` confine every caller to **one file**, and one
/// file is exactly the unit this extractor sees whole. That is the only
/// visibility claim Swift evidence supports here, so it is the only one made.
///
/// `internal` — Swift's default, and therefore the answer for the great majority
/// of declarations — is **not** treated as private, and that is a measured
/// decision rather than caution. Internal confines callers to a *module*, and
/// `devmap` does not model Swift modules at all: it indexes a directory, which
/// may hold several modules or part of one. C gets the aggressive treatment
/// (SC31: a definition in a `.c` file is a candidate) because the corpus-wide
/// header-name join in `devmap-analyze` supplies the missing cross-file
/// evidence; Swift has no equivalent join available, so claiming the same
/// completeness would be claiming evidence that does not exist.
///
/// It was implemented the other way first and measured. Treating `internal` as
/// private produced six dead-code findings on the two-file fixture, of which
/// **four were false**: `Mode` and `Payload` are both used inside `Runner.run`
/// but Swift reference extraction records no edge for `Mode.fast`;
/// `Person.greet` is called through a receiver the resolver cannot type; and
/// `Runner.run` is called only from outside the file. A 67% false-positive rate
/// on a hand-written fixture is a proposal to delete working code, which is the
/// one direction this analysis must not fail in.
///
/// A nested declaration does not inherit its container's level. Swift's own rule
/// is that a member's effective access is the *minimum* of its own and its
/// container's, so reading only the declaration's own modifier can never report
/// a member more private than it is — the fail-closed direction for a deletion
/// proposal.
pub(crate) fn is_exported(node: Node, source: &str) -> bool {
    !matches!(
        visibility_modifier(node, source).as_deref(),
        Some("private") | Some("fileprivate")
    )
}

/// The declaration's `visibility_modifier`, if it wrote one.
fn visibility_modifier(node: Node, source: &str) -> Option<String> {
    for modifier in modifier_nodes(node) {
        if modifier.kind() == "visibility_modifier" {
            return Some(get_node_text(modifier, source));
        }
    }
    None
}

/// The children of the declaration's `modifiers` list: attributes, visibility,
/// inheritance and member modifiers.
///
/// Only the declaration's own list is read. Reading its text instead — which is
/// what `generic_is_exported` does — reaches into every body it contains.
fn modifier_nodes(node: Node<'_>) -> Vec<Node<'_>> {
    let mut cursor = node.walk();
    let modifiers = node
        .named_children(&mut cursor)
        .find(|child| child.kind() == "modifiers");
    let Some(modifiers) = modifiers else {
        return Vec::new();
    };
    let mut inner = modifiers.walk();
    modifiers.named_children(&mut inner).collect()
}

/// The bare names of the `@`-attributes written on the declaration.
fn attribute_names(node: Node, source: &str) -> Vec<String> {
    let mut names = Vec::new();
    for modifier in modifier_nodes(node) {
        if modifier.kind() != "attribute" {
            continue;
        }
        // `attribute -> user_type -> type_identifier` is the name;
        // `@_cdecl("entry")` adds an argument list after it that is not part of
        // the attribute's identity.
        let mut cursor = modifier.walk();
        let user_type = modifier
            .named_children(&mut cursor)
            .find(|child| child.kind() == "user_type");
        if let Some(user_type) = user_type {
            let name = get_node_text(user_type, source);
            if !name.is_empty() {
                names.push(name);
            }
        }
    }
    names
}

/// Whether the declaration carries a given member modifier, e.g. `override`.
fn has_member_modifier(node: Node, source: &str, word: &str) -> bool {
    modifier_nodes(node).into_iter().any(|modifier| {
        modifier.kind() == "member_modifier" && get_node_text(modifier, source) == word
    })
}

/// The types and protocols a `class_declaration` or `protocol_declaration`
/// inherits from or conforms to.
fn inherited_types(node: Node, source: &str) -> Vec<String> {
    let mut names = Vec::new();
    let mut cursor = node.walk();
    for child in node.named_children(&mut cursor) {
        if child.kind() != "inheritance_specifier" {
            continue;
        }
        if let Some(name) = get_child_text(child, "inherits_from", source) {
            // `Foo<Bar>` and `Swift.Codable` both appear; the leading module
            // qualifier and the generic arguments are not part of the identity
            // being matched.
            let bare = name
                .rsplit('.')
                .next()
                .unwrap_or(&name)
                .split('<')
                .next()
                .unwrap_or(&name)
                .trim()
                .to_string();
            if !bare.is_empty() {
                names.push(bare);
            }
        }
    }
    names
}

/// Why a Swift declaration is reached without an observable call site.
///
/// Reading visibility is what makes this necessary: before it, every Swift
/// symbol reported `is_exported` and nothing could be dead, so no exemption was
/// needed for anything. Each reason below is evidence carried by the
/// declaration — an attribute, the `override` modifier, a named supertype —
/// never a name matched in isolation.
pub(crate) fn exemption(
    node: Node,
    source: &str,
    declaration: &Declaration,
) -> Option<(WiringKind, &'static str)> {
    // An enum case is reached through leading-dot inference — `let m: Mode =
    // .fast`, `case .number(let n)` — which names the case without naming its
    // type. `split_call_target` refuses `.text` as a callee (SC32: it is not an
    // identifier), so the dominant Swift use site is unobservable *by
    // construction*, and an uncalled case is not evidence of an unused one.
    if declaration.declared_kind == SymbolKind::Field {
        return Some((
            WiringKind::StructuralExempt,
            "Swift enum case reachable by leading-dot inference, which names no type",
        ));
    }
    // `CodingKeys` is a name the Swift compiler itself looks up. A type
    // conforming to `Codable` gets a synthesized `init(from:)` and `encode(to:)`
    // that reference the nested enum and nothing else does, so it has no call
    // site anywhere in the corpus by construction. Measured on 342 real
    // `.swift` files, this was 6 of the 12 confident dead-code findings, every
    // one of them a proposal to delete a type the compiler requires. Matched on
    // the exact reserved name, not a prefix, so a type someone happens to call
    // `CodingKeysCache` is untouched.
    if declaration.name == "CodingKeys" || declaration.owner.as_deref().is_some_and(is_coding_keys)
    {
        return Some((
            WiringKind::StructuralExempt,
            "Swift CodingKeys is referenced only by the compiler-synthesized Codable conformance",
        ));
    }
    for attribute in attribute_names(node, source) {
        if let Some(reason) = crate::wiring::swift_attribute_entry_reason(&attribute) {
            return Some((WiringKind::RuntimeEntryPoint, reason));
        }
    }
    // An `override` is invoked by whatever declared the method being overridden
    // — `UIViewController.viewDidLoad`, `NSObject.isEqual`, a superclass in
    // another module entirely. The call site is the framework's, and it is not
    // in the corpus.
    if has_member_modifier(node, source, "override") {
        return Some((
            WiringKind::RuntimeEntryPoint,
            "Swift override invoked by the declaring superclass or framework",
        ));
    }
    if declaration.declared_kind == SymbolKind::Function {
        // `XCTestCase` collects `test…` methods by reflection, exactly as pytest
        // collects `test_*`. Gated on the enclosing type actually inheriting
        // `XCTestCase`, so an ordinary helper named `testable` in ordinary code
        // is never claimed.
        if declaration.name.starts_with("test") && enclosing_type_is_xctest(node, source) {
            return Some((
                WiringKind::RuntimeEntryPoint,
                "XCTestCase collects test methods by reflection",
            ));
        }
        return None;
    }
    // A type whose conformance list names an application or extension entry
    // protocol is instantiated by the runtime, not by any call in the corpus.
    inherited_types(node, source)
        .iter()
        .find_map(|name| crate::wiring::swift_runtime_supertype_reason(name))
        .map(|reason| (WiringKind::RuntimeEntryPoint, reason))
}

/// Whether the type enclosing `node` inherits `XCTestCase`.
fn enclosing_type_is_xctest(node: Node, source: &str) -> bool {
    let mut ancestor = node.parent();
    while let Some(parent) = ancestor {
        if owner_identity(parent, source).is_some() {
            return inherited_types(parent, source)
                .iter()
                .any(|name| name == "XCTestCase");
        }
        ancestor = parent.parent();
    }
    false
}

/// Whether an owner path ends in the reserved `CodingKeys` segment.
///
/// The cases of a `CodingKeys` enum are as compiler-referenced as the enum, and
/// a nested one is owned by a dotted path (`SnippetModel.CodingKeys`).
fn is_coding_keys(owner: &str) -> bool {
    owner.rsplit('.').next() == Some("CodingKeys")
}
