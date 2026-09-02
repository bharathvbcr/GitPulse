//! Kotlin declarations.
//!
//! Three shapes the generic path could not read, each verified against
//! tree-sitter-kotlin-ng's own parse tree:
//!
//! * `class`, `interface` and `enum class` are all `class_declaration`. The word
//!   that separates them is an anonymous child token (`class`/`interface`) or a
//!   `class_modifier`, neither of which a node-kind table sees, so `interface
//!   Greeter` and `enum class Mode` both reported `Class`.
//! * `object Singleton` is `object_declaration`, a kind the generic table has no
//!   entry for at all, so the object emitted no symbol and its members were
//!   attributed to whatever class enclosed it — the file, at top level.
//! * `fun Person.extra()` puts the receiver type in a `user_type` child *before*
//!   the `name` field. Reading only `name` emitted `Main.kt::extra` and lost the
//!   receiver, which is the identity Go receivers keep under SC9 and Rust `impl`
//!   methods under SC11.

use tree_sitter::Node;

use crate::model::{SymbolKind, WiringKind};
use crate::treesitter::{get_child_text, get_node_text};

use super::{enclosing_owner, Declaration};

/// A Kotlin declaration, or `None` when `node` declares nothing this graph
/// records.
pub(crate) fn declaration(node: Node, source: &str) -> Option<Declaration> {
    let (declared_kind, name) = self_identity(node, source)?;
    // An extension function belongs to the type it extends, not to the file:
    // `fun Person.extra()` is called as `person.extra()` and must join to
    // `Person.extra`. A member function keeps the ordinary ancestor walk.
    // An enum entry belongs to its enum by that enum's *full* identity: a
    // nested enum's bare name is not unique within a file.
    let owner = if declared_kind == SymbolKind::Field {
        super::enclosing_owner_path(node, source, declaration)
    } else {
        extension_receiver(node, source).or_else(|| enclosing_owner(node, source, self_identity))
    };
    Declaration::new(declared_kind, owner, name)
}

/// What `node` declares, ignoring its ancestors.
///
/// Doubles as the owner walk: unlike Swift, Kotlin has no declaration that owns
/// members without being a symbol itself. `companion_object` comes closest and
/// is deliberately absent — a companion's members are called as
/// `Holder.create()`, so they belong to `Holder`, and skipping the companion in
/// the walk is what produces that name.
fn self_identity(node: Node, source: &str) -> Option<(SymbolKind, String)> {
    match node.kind() {
        "class_declaration" => Some((
            class_kind(node, source),
            get_child_text(node, "name", source)?,
        )),
        // `object Singleton { … }` and `object : Runnable { … }`. The anonymous
        // form carries no `name` field and is refused by `Declaration::new`.
        "object_declaration" => Some((SymbolKind::Class, get_child_text(node, "name", source)?)),
        "function_declaration" => {
            Some((SymbolKind::Function, get_child_text(node, "name", source)?))
        }
        // `FAST` in `enum class Mode { FAST, SLOW }`. Emitted for the same
        // reason as the Swift case: a `Mode.FAST` reference is already recorded
        // and had no node it could resolve to.
        "enum_entry" => {
            let mut cursor = node.walk();
            let name = node
                .named_children(&mut cursor)
                .find(|child| child.kind() == "identifier")
                .map(|child| get_node_text(child, source))?;
            Some((SymbolKind::Field, name))
        }
        _ => None,
    }
}

/// `class`, `interface` or `enum class`, read from the tokens the grammar keeps
/// outside the `name` field.
fn class_kind(node: Node, source: &str) -> SymbolKind {
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        if child.kind() == "interface" {
            return SymbolKind::Interface;
        }
    }
    if has_modifier(node, source, "class_modifier", "enum") {
        return SymbolKind::Enum;
    }
    SymbolKind::Class
}

/// The receiver type of an extension declaration: `Person` in
/// `fun Person.extra()`.
///
/// The receiver is the `user_type` that precedes the `name` field. Anything
/// after the name is a return type or a parameter list, so position is what
/// separates them — the grammar gives the receiver no field of its own.
fn extension_receiver(node: Node, source: &str) -> Option<String> {
    if node.kind() != "function_declaration" {
        return None;
    }
    let name = node.child_by_field_name("name")?;
    let mut cursor = node.walk();
    let mut receiver = None;
    for child in node.children(&mut cursor) {
        if child.id() == name.id() {
            break;
        }
        if child.kind() == "user_type" {
            receiver = Some(child);
        }
    }
    // `fun Map<String, Int>.pairs()` extends a specialised type; the identity a
    // call can join to is the bare type name, since that is what a declaration
    // of `Map` would be called.
    let text = get_node_text(receiver?, source);
    let bare = text
        .split('<')
        .next()
        .unwrap_or(&text)
        .rsplit('.')
        .next()
        .unwrap_or(&text)
        .trim()
        .to_string();
    (!bare.is_empty()).then_some(bare)
}

/// Kotlin's four visibilities, read from the declaration's own modifier list.
///
/// Kotlin's default is `public`, and that is the answer given for a declaration
/// with no modifier — unlike Swift, whose default is `internal`. The two
/// languages get different answers here because the languages genuinely differ,
/// not because one of them is being approximated: a Kotlin author who writes
/// nothing has declared public API, and treating that as evidence of a
/// module-private symbol would manufacture dead-code candidates out of the
/// absence of a keyword.
///
/// `private` is the only level that confines callers to **one file**, which is
/// the unit this extractor sees whole, so it is the only one claimed.
///
/// `internal` was implemented as non-exported first and reverted on measurement.
/// It is module-scoped, and `devmap` models no Gradle modules — but the decisive
/// evidence was not that argument, it was the output: on a 1,032-file Android
/// corpus the `internal` rule produced 59 findings that were not findings
/// before, and **8 of 8 sampled were false**. Every one was an `internal object`
/// used cross-file as a receiver — `NotificationIcons.categoryBadge(...)`,
/// `ChronosWearPalette.PRIMARY`, `LocalPlanningHeuristics.generateIdealDayPlan(...)`
/// — and a receiver qualifier is not recorded as a use of the type that spells
/// it, so an object is reachable in the source and invisible in the graph. The
/// missing edge is the real defect; until it exists, reading `internal` as
/// private converts it into a proposal to delete working code.
///
/// `protected` is not claimed either: its callers are subclasses, which can be
/// declared in any module that depends on this one.
pub(crate) fn is_exported(node: Node, source: &str) -> bool {
    visibility_modifier(node, source).as_deref() != Some("private")
}

/// The text of the declaration's `visibility_modifier`, if it wrote one.
fn visibility_modifier(node: Node, source: &str) -> Option<String> {
    modifier_nodes(node)
        .into_iter()
        .find(|modifier| modifier.kind() == "visibility_modifier")
        .map(|modifier| get_node_text(modifier, source))
}

/// Whether the declaration carries `word` as a modifier of kind `modifier_kind`.
fn has_modifier(node: Node, source: &str, modifier_kind: &str, word: &str) -> bool {
    modifier_nodes(node)
        .into_iter()
        .any(|modifier| modifier.kind() == modifier_kind && get_node_text(modifier, source) == word)
}

/// The children of the declaration's own `modifiers` list.
///
/// Only the declaration's own list. `generic_is_exported` read the node's whole
/// source text, so one `private` member marked its enclosing class private.
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

/// The bare names of the annotations written on the declaration.
fn annotation_names(node: Node, source: &str) -> Vec<String> {
    let mut names = Vec::new();
    for modifier in modifier_nodes(node) {
        if modifier.kind() != "annotation" {
            continue;
        }
        let mut cursor = modifier.walk();
        for child in modifier.named_children(&mut cursor) {
            if child.kind() != "user_type" {
                continue;
            }
            let text = get_node_text(child, source);
            let bare = text.rsplit('.').next().unwrap_or(&text).trim();
            if !bare.is_empty() {
                names.push(bare.to_string());
            }
        }
    }
    names
}

/// Why a Kotlin declaration is reachable without an observable call site.
pub(crate) fn exemption(
    node: Node,
    source: &str,
    declaration: &Declaration,
) -> Option<(WiringKind, &'static str)> {
    // `when (mode) { FAST -> … }` and an import of the entry both name a case
    // without naming its enum, so the type-qualified use site this extractor
    // records is not the only one the language permits.
    if declaration.declared_kind == SymbolKind::Field && node.kind() == "enum_entry" {
        return Some((
            WiringKind::StructuralExempt,
            "Kotlin enum entry reachable by bare name in a `when` branch or import",
        ));
    }
    for annotation in annotation_names(node, source) {
        if let Some(reason) = crate::wiring::kotlin_annotation_entry_reason(&annotation) {
            return Some((WiringKind::RuntimeEntryPoint, reason));
        }
    }
    // An `override` is invoked by whatever declared the member being overridden
    // — `Activity.onCreate`, `Fragment.onViewCreated`, an interface in another
    // module. The call site belongs to the framework and is not in the corpus.
    if has_modifier(node, source, "member_modifier", "override") {
        return Some((
            WiringKind::RuntimeEntryPoint,
            "Kotlin override invoked by the declaring supertype or framework",
        ));
    }
    // `external fun` has no Kotlin body at all: it is bound to a native symbol
    // by the JVM, and the caller is on the other side of JNI.
    if has_modifier(node, source, "function_modifier", "external") {
        return Some((
            WiringKind::RuntimeEntryPoint,
            "Kotlin external function bound to a native implementation over JNI",
        ));
    }
    if declaration.declared_kind == SymbolKind::Function && declaration.name == "main" {
        return Some((WiringKind::RuntimeEntryPoint, "Program entry point"));
    }
    None
}
