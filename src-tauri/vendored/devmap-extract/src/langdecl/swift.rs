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
        // A protocol property requirement is declared by the `simple_identifier`
        // naming it (see `property_declaration_of`), exactly as every other
        // property is, so the declaration node itself emits nothing.
        //
        // A protocol *function* requirement is, owned by its protocol. It was
        // withheld while a receiver typed by a protocol could not be resolved at
        // all — `person.greet()` fell from `Person.greet` at 1.0 to the
        // requirement at 0.2 because nothing typed `person`. Receivers are typed
        // now (`swiftscope`), and a call written against the protocol needs a
        // node to name: it is what a dispatch edge's `via` points at.
        "protocol_property_declaration" => None,
        // A function declared inside another function's body is a name that body
        // binds, not a symbol of the module: `func expectation(...)` written
        // inside one XCTest case received 78 confident callers from every other
        // test in the module (the audit's F1). Its calls belong to the function
        // that contains it, as an unnamed closure's do.
        "function_declaration" if is_local_function(node) => None,
        _ => {
            let (declared_kind, name) = self_identity(node, source)?;
            // An enum case belongs to its enum by that enum's *full* identity.
            // Swift nests `enum CodingKeys` inside each Codable type, so the
            // bare enum name is not unique within a file.
            //
            // A nested *enum* is the same shape one level up: `class Workspace
            // { class ActiveSaveRequest { enum CancellationDisposition } }`
            // must be `Workspace.ActiveSaveRequest.CancellationDisposition`,
            // not `ActiveSaveRequest.CancellationDisposition`. The latter
            // cannot join a reference attributed to the nested class — the
            // enclosing symbol is `Workspace.ActiveSaveRequest` — so a field
            // typed as the nested enum was confident-dead on a live save path.
            //
            // A property is owned by its type's bare name, as a method is: a
            // read writes `p.count` against the type, and the resolver keys a
            // type's members by that bare name.
            let owner = if property_declaration_of(node).is_some() {
                enclosing_owner(node, source, owner_identity)
            } else if matches!(declared_kind, SymbolKind::Field | SymbolKind::Enum) {
                super::enclosing_owner_path(node, source, declaration)
            } else {
                enclosing_owner(node, source, owner_identity)
            };
            Declaration::new(declared_kind, owner, name)
        }
    }
}

/// Whether `node` is declared inside a function, initializer, deinitializer or
/// closure body — before any enclosing type — rather than at file or type level.
fn is_local_function(node: Node) -> bool {
    let mut ancestor = crate::treesitter::bounded_parent(node);
    while let Some(parent) = ancestor {
        match parent.kind() {
            "function_declaration"
            | "init_declaration"
            | "deinit_declaration"
            | "lambda_literal" => {
                return true;
            }
            "class_declaration" | "protocol_declaration" | "source_file" => return false,
            _ => {}
        }
        ancestor = crate::treesitter::bounded_parent(parent);
    }
    false
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
        "function_declaration" | "protocol_function_declaration" => {
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
        "simple_identifier" if is_enum_case_name(node) => {
            Some((SymbolKind::Field, get_node_text(node, source)))
        }
        // A stored or computed property of a type, of an extension or of a
        // protocol. It reuses `Field`, the kind an enum case already carries
        // (and the one `devmap-query` presents as `property`), rather than
        // adding a parallel kind.
        //
        // Keyed on the *name node* for the enum-case reason: `var a = 1, b = 2`
        // is one `property_declaration` with two names, and `let (x, y) = pair`
        // binds two names from one pattern.
        "simple_identifier" if property_declaration_of(node).is_some() => {
            Some((SymbolKind::Field, get_node_text(node, source)))
        }
        _ => None,
    }
}

/// The `property_declaration` (or protocol requirement) that `node` names, when
/// `node` is a bound name of a **member** property.
///
/// A member is a property written directly in a `class_body`, `enum_class_body`
/// or `protocol_body`. A `let`/`var` anywhere else is not a symbol: inside a
/// function, initializer, closure or accessor body it is a local, which the
/// scope pass binds, and at file level it is a global this graph does not model.
/// Requiring the declaration's *direct* parent to be a type body is what keeps a
/// `let` inside a computed property's getter from being mistaken for a member of
/// the type that getter belongs to.
pub(crate) fn property_declaration_of(node: Node) -> Option<Node> {
    match name_role(node) {
        Role::Property(raw) => {
            // SAFETY: the same argument `parent_index::parent` makes. `raw` was
            // taken from a live `Node` of the tree `node` belongs to (checked by
            // the tree identity stored beside it in `ROLES`), the cache is
            // emptied by `reset_roles` before the next file's tree exists, and
            // no pointer is cast or fabricated.
            Some(unsafe { Node::from_raw(raw) })
        }
        Role::EnumCase | Role::Other => None,
    }
}

/// What a `simple_identifier` declares, if anything.
#[derive(Clone, Copy)]
enum Role {
    /// A bound name of a member property; carries its declaration.
    Property(tree_sitter::ffi::TSNode),
    /// A name an `enum_entry` declares.
    EnumCase,
    Other,
}

thread_local! {
    /// `(tree, role)` by node id, for the file being extracted.
    ///
    /// Every rule that asks "is this a property name" asks it of the same
    /// identifier several times — the declaration, its owner, its visibility,
    /// its exemption, its signature, the locals pass — and each answer walks
    /// up the tree. tree-sitter's own `parent` costs time proportional to the
    /// siblings it passes, and a type with tens of thousands of members is one
    /// body of that many children, so the repeated walks were quadratic: 30,000
    /// properties retired 3.2x the instructions of the same file with no
    /// property symbols. One answer per identifier makes it a single walk.
    static ROLES: std::cell::RefCell<std::collections::HashMap<usize, (*const tree_sitter::ffi::TSTree, Role)>> =
        std::cell::RefCell::new(std::collections::HashMap::new());
}

/// Forget every cached role. Node ids are arena addresses, reused once a tree
/// is dropped, so the cache must not outlive its file.
pub(crate) fn reset_roles() {
    ROLES.with(|roles| roles.borrow_mut().clear());
}

fn name_role(node: Node) -> Role {
    if node.kind() != "simple_identifier" {
        return Role::Other;
    }
    let tree = node.into_raw().tree;
    let key = node.id();
    if let Some(known) = ROLES.with(|roles| {
        roles
            .borrow()
            .get(&key)
            .filter(|(cached_tree, _)| *cached_tree == tree)
            .map(|(_, role)| *role)
    }) {
        return known;
    }
    let role = compute_role(node);
    ROLES.with(|roles| roles.borrow_mut().insert(key, (tree, role)));
    role
}

fn compute_role(node: Node) -> Role {
    // Called for every identifier in the file, so the cheap test comes first:
    // the parent's own field, not a scan of its children.
    let Some(parent) = crate::treesitter::bounded_parent(node) else {
        return Role::Other;
    };
    if parent.kind() == "enum_entry" {
        return if is_named_child(node, "name") {
            Role::EnumCase
        } else {
            Role::Other
        };
    }
    match member_property_declaration(node, parent) {
        Some(declaration) => Role::Property(declaration.into_raw()),
        None => Role::Other,
    }
}

fn member_property_declaration<'tree>(
    node: Node<'tree>,
    parent: Node<'tree>,
) -> Option<Node<'tree>> {
    if parent.kind() != "pattern"
        || parent
            .child_by_field_name("bound_identifier")
            .is_none_or(|bound| bound.id() != node.id())
    {
        return None;
    }
    // `let (x, y) = …` nests one pattern per tuple element. A `pattern` that is
    // a direct child of the declaration is always a bound name: the initializer
    // is its `value` and a type annotation is its own node.
    let mut ancestor = parent;
    while ancestor.kind() == "pattern" {
        ancestor = crate::treesitter::bounded_parent(ancestor)?;
    }
    if !matches!(
        ancestor.kind(),
        "property_declaration" | "protocol_property_declaration"
    ) {
        return None;
    }
    let body = crate::treesitter::bounded_parent(ancestor)?;
    matches!(
        body.kind(),
        "class_body" | "enum_class_body" | "protocol_body"
    )
    .then_some(ancestor)
}

/// Whether `node` is a name an `enum_entry` declares (`fast` and `slow` in
/// `case fast, slow`).
pub(crate) fn is_enum_case_name(node: Node) -> bool {
    matches!(name_role(node), Role::EnumCase)
}

/// The name node of the property `parent` declares, when `parent` is a
/// `property_declaration` written in an `extension` body and `inside` lies in its
/// value (an initializer or a computed body) rather than in its name or type.
pub(crate) fn extension_property_symbol<'tree>(
    parent: Node<'tree>,
    inside: Node<'tree>,
) -> Option<Node<'tree>> {
    if parent.kind() != "property_declaration" {
        return None;
    }
    let body = crate::treesitter::bounded_parent(parent)?;
    let owner = crate::treesitter::bounded_parent(body)?;
    if body.kind() != "class_body" || owner.kind() != "class_declaration" {
        return None;
    }
    let is_extension = owner
        .child_by_field_name("declaration_kind")
        .is_some_and(|kind| kind.kind() == "extension");
    if !is_extension {
        return None;
    }
    let value = parent
        .child_by_field_name("computed_value")
        .or_else(|| parent.child_by_field_name("value"))?;
    if inside.start_byte() < value.start_byte() || inside.end_byte() > value.end_byte() {
        return None;
    }
    let mut cursor = parent.walk();
    let mut pattern = parent
        .named_children(&mut cursor)
        .find(|child| child.kind() == "pattern")?;
    // `let (a, b) = …` binds several names; the first stands for the declaration.
    while pattern.child_by_field_name("bound_identifier").is_none() {
        let mut inner = pattern.walk();
        let next = pattern
            .named_children(&mut inner)
            .find(|child| child.kind() == "pattern")?;
        pattern = next;
    }
    pattern.child_by_field_name("bound_identifier")
}

/// The `signature` a member property is emitted with (see
/// [`crate::model::SWIFT_STORED_PROPERTY`]), or `None` when `node` is not one.
pub(crate) fn property_signature(node: Node) -> Option<&'static str> {
    let property = property_declaration_of(node)?;
    Some(if property.kind() == "protocol_property_declaration" {
        crate::model::SWIFT_PROPERTY_REQUIREMENT
    } else if property_is_computed(property) {
        crate::model::SWIFT_COMPUTED_PROPERTY
    } else {
        crate::model::SWIFT_STORED_PROPERTY
    })
}

/// Whether `node` occupies `field` on its own parent.
fn is_named_child(node: Node, field: &str) -> bool {
    let Some(parent) = crate::treesitter::bounded_parent(node) else {
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
    // A property's modifiers hang off its `property_declaration`, not off the
    // name node that is the symbol.
    let node = property_declaration_of(node).unwrap_or(node);
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
    // A protocol requirement declares what a conformer must provide. It has no
    // body and no caller of its own is expected: calls are written against the
    // conformers, or against the protocol and dispatched to them.
    if node.kind() == "protocol_function_declaration" {
        return Some((
            WiringKind::StructuralExempt,
            "Swift protocol requirement — declares what a conformer must provide, so no caller of its own is expected",
        ));
    }
    if let Some(property) = property_declaration_of(node) {
        return property_exemption(property, node, source);
    }
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
    // An overload of a *standard* operator — `static func + (l: Meter, r:
    // Meter)`, `==`, `<` — is called by operator syntax the call extractor
    // does not record (every `a + b` on an `Int` would be a ledger row), so it
    // has no call site by construction. A custom operator (`<+>`) is
    // different: its uses are recorded as calls, so it keeps the ordinary
    // verdict. The audit's Swift P1 was the custom case published dead at 0.9.
    if is_standard_operator_name(&declaration.name) {
        return Some((
            WiringKind::StructuralExempt,
            "Swift standard-operator overload, called by operator syntax the extractor does not record",
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

/// Whether `name` spells one of Swift's standard operators.
///
/// The declared spelling of an overload: `+`, `==`, `+=`, `...`, `??`. A custom
/// operator such as `<+>` is not in the list on purpose — its uses are
/// recorded as calls, so it can be judged by them.
fn is_standard_operator_name(name: &str) -> bool {
    matches!(
        name,
        "+" | "-"
            | "*"
            | "/"
            | "%"
            | "=="
            | "!="
            | "==="
            | "!=="
            | "<"
            | ">"
            | "<="
            | ">="
            | "&&"
            | "||"
            | "!"
            | "&"
            | "|"
            | "^"
            | "~"
            | "<<"
            | ">>"
            | "+="
            | "-="
            | "*="
            | "/="
            | "%="
            | "&="
            | "|="
            | "^="
            | "<<="
            | ">>="
            | "~="
            | "..."
            | "..<"
            | "??"
            | "&+"
            | "&-"
            | "&*"
            | "&+="
            | "&-="
            | "&*="
    )
}

/// Whether the type enclosing `node` inherits `XCTestCase`.
fn enclosing_type_is_xctest(node: Node, source: &str) -> bool {
    let mut ancestor = crate::treesitter::bounded_parent(node);
    while let Some(parent) = ancestor {
        if owner_identity(parent, source).is_some() {
            return inherited_types(parent, source)
                .iter()
                .any(|name| name == "XCTestCase");
        }
        ancestor = crate::treesitter::bounded_parent(parent);
    }
    false
}

/// Why a member property is reached without an observable read.
///
/// Every reason is evidence on the declaration or on the type it is written in,
/// and the rule fails closed: a property is a dead-code candidate only when
/// nothing but a read could reach it.
///
/// * A protocol requirement declares what a conformer provides.
/// * An attribute makes the framework a reader — a property wrapper
///   (`@Published`, `@State`, `@AppStorage`) is read through its projected
///   value `$name`, which names no property, and `@objc`/`@IBOutlet`/`@NSManaged`
///   publish it to the Objective-C runtime.
/// * `override` is read by the declaring superclass.
/// * A type that lists any supertype may satisfy a requirement with a
///   non-private property, and its synthesized `Codable`/`Equatable`/`Hashable`
///   conformance reads **every stored property, private ones included**. The
///   supertype named may itself inherit `Codable` (`protocol Model: Codable`),
///   so the conformance list is not narrowed to a known name.
fn property_exemption(
    property: Node,
    name: Node,
    source: &str,
) -> Option<(WiringKind, &'static str)> {
    if property.kind() == "protocol_property_declaration" {
        return Some((
            WiringKind::StructuralExempt,
            "Swift protocol property requirement — declares what a conformer must provide, so no reader of its own is expected",
        ));
    }
    if let Some(attribute) = attribute_names(name, source).first() {
        return Some(match crate::wiring::swift_attribute_entry_reason(attribute) {
            Some(reason) => (WiringKind::RuntimeEntryPoint, reason),
            None => (
                WiringKind::StructuralExempt,
                "Swift attributed property (property wrapper or runtime-visible) is read by the framework through its projected value or by name",
            ),
        });
    }
    if has_member_modifier(name, source, "override") {
        return Some((
            WiringKind::RuntimeEntryPoint,
            "Swift property override read by the declaring superclass or framework",
        ));
    }
    let container = enclosing_type_declaration(property);
    if container.is_some_and(|container| {
        attribute_names(container, source)
            .iter()
            .any(|attribute| attribute == "objcMembers")
    }) {
        return Some((
            WiringKind::RuntimeEntryPoint,
            "published to the Objective-C runtime by the enclosing type's @objcMembers",
        ));
    }
    let conforms =
        container.is_some_and(|container| !inherited_types(container, source).is_empty());
    if !conforms {
        return None;
    }
    let private = !is_exported(name, source);
    let stored = !property_is_computed(property);
    if !private {
        return Some((
            WiringKind::StructuralExempt,
            "Swift property of a type with supertypes — it may witness a protocol requirement or be read by a synthesized conformance",
        ));
    }
    stored.then_some((
        WiringKind::StructuralExempt,
        "Swift private stored property of a type with supertypes — a synthesized Codable, Equatable or Hashable conformance reads every stored property",
    ))
}

/// Whether a `property_declaration` computes its value rather than storing it.
fn property_is_computed(property: Node) -> bool {
    let mut cursor = property.walk();
    let computed = property
        .named_children(&mut cursor)
        .any(|child| child.kind() == "computed_property");
    computed
}

/// The `class_declaration` or `protocol_declaration` a member is written in —
/// an extension included — or `None` outside a type.
fn enclosing_type_declaration(node: Node) -> Option<Node> {
    let mut ancestor = crate::treesitter::bounded_parent(node);
    while let Some(parent) = ancestor {
        if matches!(parent.kind(), "class_declaration" | "protocol_declaration") {
            return Some(parent);
        }
        ancestor = crate::treesitter::bounded_parent(parent);
    }
    None
}

/// Whether an owner path ends in the reserved `CodingKeys` segment.
///
/// The cases of a `CodingKeys` enum are as compiler-referenced as the enum, and
/// a nested one is owned by a dotted path (`SnippetModel.CodingKeys`).
fn is_coding_keys(owner: &str) -> bool {
    owner.rsplit('.').next() == Some("CodingKeys")
}
