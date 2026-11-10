//! Kotlin declarations.
//!
//! Four shapes the generic path could not read, each verified against
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
//! * A property is a `Field` keyed on its **name node**, not on the
//!   `property_declaration`: see [`property_declaring_node`].

use tree_sitter::Node;

use crate::model::{ExtractedSymbol, SymbolKind, WiringKind};
use crate::treesitter::{bounded_parent, get_child_text, get_node_text};

use super::{Declaration, enclosing_owner};

/// A Kotlin declaration, or `None` when `node` declares nothing this graph
/// records.
pub(crate) fn declaration(node: Node, source: &str) -> Option<Declaration> {
    // A property is keyed on its name node, which is a leaf: it can never be
    // the nearest enclosing declaration of a call, so the initializer of a
    // property keeps being attributed to the class or file around it.
    if let Some(property) = property_declaring_node(node, source) {
        // `private` is the one visibility the extractor can act on (see
        // `is_exported`): a private property would be an unexported `Field` and
        // so a dead-code candidate, because a read of it is not an edge.
        if !is_exported(property, source) {
            return None;
        }
        // Owned by the class's bare name, as a method is: a read writes
        // `trip.name` against the type. Only an enum entry takes the full path.
        let owner = enclosing_owner(node, source, self_identity);
        return Declaration::new(SymbolKind::Field, owner, get_node_text(node, source));
    }
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
        // The class the loss recovery wraps a swallowed statement in is not a
        // class the source has. It owns nothing, so what it holds is named as
        // the top-level declaration it is.
        "class_declaration" if crate::kotlinloss::is_recovery_wrapper(node, source) => None,
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
        // `typealias Id = Long`, `actual typealias P = Path`. The grammar puts
        // the alias's own name in the field called `type` (the aliased type is
        // an unnamed `type` child), so `name` would read nothing. `Struct`, as
        // Go's `type_alias` is: an alias of a class is constructible by name
        // (`typealias Pt = Pair<Int, Int>; Pt(1, 2)`), and a `Struct` is a call
        // candidate where an `Interface` is not.
        "type_alias" => Some((SymbolKind::Struct, get_child_text(node, "type", source)?)),
        // `val Task.label: String get() = …` is an extension property: a
        // callable keyed by its receiver exactly like an extension function, and
        // the only kind of Kotlin property that is a symbol here. An ordinary
        // property is a field or a local, which the typed-binding reader owns.
        "property_declaration" => {
            let _ = extension_property_receiver(node)?;
            let declaration = node
                .named_children(&mut node.walk())
                .find(|child| child.kind() == "variable_declaration")?;
            let name = declaration.named_child(0)?;
            Some((SymbolKind::Function, get_node_text(name, source)))
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

/// The `property_declaration` or `class_parameter` that `name` is the name of,
/// when it declares a **member or top-level property**.
///
/// * `val`/`var` written directly in a class, interface, enum, object or
///   companion body, or at file level.
/// * A primary-constructor parameter that carries `val` or `var`.
///
/// Anything else is not a symbol here: a `val` inside a function, accessor,
/// initializer or lambda is a local the scope pass binds; a destructuring
/// declaration (`val (a, b) = pair`, whose names sit in a
/// `multi_variable_declaration`) names no single property; an extension
/// property is already a callable; and a property of an anonymous
/// `object : T { }` belongs to no named owner. Requiring the declaration's
/// *direct* parent to be a body is what keeps a `val` in a getter from being
/// read as a member of the type that getter belongs to.
pub(crate) fn property_declaring_node<'t>(name: Node<'t>, source: &str) -> Option<Node<'t>> {
    if name.kind() != "identifier" {
        return None;
    }
    let parent = bounded_parent(name)?;
    let first_identifier = |of: Node<'_>| {
        let mut cursor = of.walk();
        let first = of
            .named_children(&mut cursor)
            .find(|child| child.kind() == "identifier");
        first.is_some_and(|first| first.id() == name.id())
    };
    match parent.kind() {
        "variable_declaration" => {
            let declaration = bounded_parent(parent)?;
            (declaration.kind() == "property_declaration"
                && first_identifier(parent)
                && extension_property_receiver(declaration).is_none()
                && is_member_or_top_level(declaration, source))
            .then_some(declaration)
        }
        "class_parameter" => {
            let stored = parent
                .children(&mut parent.walk())
                .any(|child| matches!(child.kind(), "val" | "var"));
            let parameters = bounded_parent(parent)?;
            let constructor = bounded_parent(parameters)?;
            let class = bounded_parent(constructor)?;
            (stored
                && first_identifier(parent)
                && parameters.kind() == "class_parameters"
                && constructor.kind() == "primary_constructor"
                && is_named_owner(class, source))
            .then_some(parent)
        }
        _ => None,
    }
}

/// Whether a property declaration sits at file level or directly in the body of
/// a named owner.
fn is_member_or_top_level(declaration: Node<'_>, source: &str) -> bool {
    let Some(container) = bounded_parent(declaration) else {
        return false;
    };
    match container.kind() {
        "source_file" => true,
        "class_body" | "enum_class_body" => {
            bounded_parent(container).is_some_and(|owner| is_named_owner(owner, source))
        }
        _ => false,
    }
}

/// A class, object or companion whose members belong to a name. The wrapper the
/// loss recovery puts a swallowed statement in owns nothing, so what it holds is
/// top level, exactly as `self_identity` names it.
fn is_named_owner(node: Node<'_>, source: &str) -> bool {
    match node.kind() {
        "class_declaration" => {
            node.child_by_field_name("name").is_some()
                || crate::kotlinloss::is_recovery_wrapper(node, source)
        }
        "object_declaration" => node.child_by_field_name("name").is_some(),
        "companion_object" => true,
        _ => false,
    }
}

/// One qualified name is one node (SC14), so a property whose id is already
/// taken is withheld: a `val size` beside a `fun size()` (Kotlin allows both in
/// one class, and a `val Foo` beside a `class Foo`) leaves the id to the
/// declaration that is not a property; among properties with one id (an outer
/// `val z` and a companion's `val z`, both `Holder.z`) the first in source order
/// keeps it.
///
/// A `typealias` (the one Kotlin `Struct`) is withheld the same way when a
/// callable or type holds its id: `typealias Pt = Pair<Int, Int>` beside
/// `fun Pt(x: Int): Pt`, the fake-constructor idiom, is one id and the
/// declaration that is not an alias keeps it.
///
/// Returns `(withheld for a non-property, withheld for an earlier property,
/// aliases withheld)`.
/// Runs on the symbols the walk emitted, which are in source order; only a
/// group with a duplicate id is looked at, so a file with none pays one hash
/// pass.
pub(crate) fn settle_property_ids(
    source: &str,
    symbols: &mut Vec<ExtractedSymbol>,
) -> (usize, usize, usize) {
    let mut by_id: std::collections::HashMap<&str, Vec<usize>> = std::collections::HashMap::new();
    for (at, symbol) in symbols.iter().enumerate() {
        by_id
            .entry(symbol.qualified_name.as_str())
            .or_default()
            .push(at);
    }
    let mut drop = vec![false; symbols.len()];
    let (mut by_callable, mut by_property, mut by_declaration) = (0, 0, 0);
    for group in by_id.values().filter(|group| group.len() > 1) {
        let is_alias = |at: usize| symbols[at].kind == SymbolKind::Struct;
        let mut group = group.clone();
        if group.iter().any(|&at| !is_alias(at)) {
            for &at in group.iter().filter(|&&at| is_alias(at)) {
                drop[at] = true;
                by_declaration += 1;
            }
            group.retain(|&at| !is_alias(at));
        }
        let mut properties: Vec<usize> = group
            .iter()
            .copied()
            .filter(|&at| is_property_symbol(source, &symbols[at]))
            .collect();
        if properties.is_empty() {
            continue;
        }
        // Source order, not vector order: a recovered declaration is appended
        // after the walk's own symbols.
        properties.sort_by_key(|&at| symbols[at].span.start_byte);
        let others = group.len() - properties.len();
        let keep = usize::from(others == 0);
        for &at in &properties[keep..] {
            drop[at] = true;
            if others > 0 {
                by_callable += 1;
            } else {
                by_property += 1;
            }
        }
    }
    if by_callable + by_property + by_declaration > 0 {
        let mut at = 0;
        symbols.retain(|_| {
            at += 1;
            !drop[at - 1]
        });
    }
    (by_callable, by_property, by_declaration)
}

/// Whether `symbol` is a property: a `Field` whose name is written directly
/// after `val` or `var` (a constructor parameter and a body property both are;
/// an enum entry follows a `,`, `{` or `;`). Read from the symbol and the file
/// text rather than from a tree, so a declaration the loss recovery re-derived
/// from a wrapper, whose outer tree holds an ERROR there, is judged like any
/// other, and no flag has to be added to every symbol.
///
/// Known residual (not fixed): a comment between `val`/`var` and the name
/// (`val /*x*/ n`) hides the keyword from the `trim_end` read, so such a field
/// is not seen as a property and is not settled against a function of its id.
fn is_property_symbol(source: &str, symbol: &ExtractedSymbol) -> bool {
    if symbol.kind != SymbolKind::Field {
        return false;
    }
    let Some(before) = source.get(..symbol.span.start_byte) else {
        return false;
    };
    let before = before.trim_end();
    ["val", "var"].iter().any(|word| {
        before.strip_suffix(word).is_some_and(|rest| {
            !rest
                .chars()
                .next_back()
                .is_some_and(|c| c.is_alphanumeric() || c == '_')
        })
    })
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
    let receiver = if node.kind() == "property_declaration" {
        extension_property_receiver(node)
    } else if node.kind() == "function_declaration" {
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
        receiver
    } else {
        None
    };
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

/// The receiver `user_type` of an extension property: the type written before
/// the `variable_declaration` that names it.
fn extension_property_receiver(node: Node) -> Option<Node> {
    let mut cursor = node.walk();
    for child in node.named_children(&mut cursor) {
        match child.kind() {
            "user_type" => return Some(child),
            "variable_declaration" => return None,
            _ => {}
        }
    }
    None
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
    // A property symbol's node is its name; the modifiers are the declaration's.
    let node = property_declaring_node(node, source).unwrap_or(node);
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
    // An Android component is instantiated by the framework from its
    // registration — the manifest, a layout, WorkManager — and no call in the
    // corpus constructs it. Read from what the class extends, because
    // `AndroidManifest.xml` is not indexed.
    supertype_names(node, source)
        .iter()
        .find_map(|name| crate::wiring::android_component_supertype_reason(name))
        .map(|reason| (WiringKind::RuntimeEntryPoint, reason))
}

/// The bare names a class declaration's supertype list writes:
/// `class BootReceiver : BroadcastReceiver()` gives `BroadcastReceiver`.
fn supertype_names(node: Node, source: &str) -> Vec<String> {
    let mut names = Vec::new();
    let mut cursor = node.walk();
    for child in node.named_children(&mut cursor) {
        if !child.kind().starts_with("delegation_specifier") {
            continue;
        }
        let mut specifiers = vec![child];
        if child.kind() == "delegation_specifiers" {
            let mut inner = child.walk();
            specifiers = child
                .named_children(&mut inner)
                .filter(|specifier| specifier.kind() == "delegation_specifier")
                .collect();
        }
        for specifier in specifiers {
            let text = get_node_text(specifier, source);
            let head = text
                .split(['(', '<', ' '])
                .next()
                .unwrap_or("")
                .rsplit('.')
                .next()
                .unwrap_or("")
                .trim()
                .to_string();
            if !head.is_empty() {
                names.push(head);
            }
        }
    }
    names
}
