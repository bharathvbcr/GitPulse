//! Swift lexical scopes: which names a callable binds itself, and the type each
//! one is written with.
//!
//! The shared binding passes in `treesitter` read `identifier` nodes and a
//! `parameters` list, and tree-sitter-swift has neither: every name is a
//! `simple_identifier`, parameters are direct children of the declaration, a
//! `let` is a `pattern` whose name sits on the `bound_identifier` field, and a
//! closure is a `lambda_literal`. So no Swift value was ever recorded as a
//! local, and a receiver such as `s` in `s.run()` fell through to the file-wide
//! `receiver_types["file:s"]` map, which another function's parameter had
//! written — `usesLocal` bound `Alpha.run` at 1.0 because `usesParam(_ s: Alpha)`
//! sat beside it (the audit's F2). A name bound by the enclosing scope is
//! authoritative: it either carries a written type here or it abstains.
//!
//! Only what the text states is typed: a parameter or annotated `let`, an
//! unwrapped optional (`guard let m = o`), the element of a collection a
//! `for` loop or an element closure (`xs.map { s in … }`) walks, and a generic
//! parameter, which is typed by its protocol bound (`T: Greeter` → `Greeter`).
//! Anything else is bound but untyped.

use std::cell::RefCell;
use std::collections::{BTreeMap, HashMap, HashSet};
use std::rc::Rc;

use tree_sitter::Node;

use crate::treesitter::{bounded_parent, get_node_text, note_nesting_cap, TYPE_NESTING_MAX_DEPTH};

/// What a scope knows about one name it binds.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct Bound {
    /// The nominal type the source writes or the text implies; `None` when it
    /// states none.
    pub ty: Option<String>,
    /// The call shape a `let` is initialised from — `make`, `Factory.make` —
    /// which the resolver types by the callee's written return type.
    pub init: Option<String>,
}

/// A scope's bound names.
pub(crate) type Bindings = BTreeMap<String, Bound>;

thread_local! {
    static CACHE: RefCell<HashMap<usize, Rc<Bindings>>> = RefCell::new(HashMap::new());
    /// Scopes whose bindings are being computed right now.
    static IN_PROGRESS: RefCell<HashSet<usize>> = RefCell::new(HashSet::new());
    /// One [`FileIndex`] per file, built the first time a typing rule needs a
    /// declaration outside the scope chain.
    static FILE_INDEX: RefCell<HashMap<usize, Rc<FileIndex>>> = RefCell::new(HashMap::new());
    /// One [`AliasIndex`] per file. Kept apart from [`FileIndex`] because reading
    /// a written type consults it, and building the file index reads written types.
    static ALIAS_INDEX: RefCell<HashMap<usize, Rc<AliasIndex>>> = RefCell::new(HashMap::new());
}

/// Drop the per-scope cache. Node ids are arena addresses, reused once a tree
/// is dropped, so the cache must not outlive its file.
pub(crate) fn reset() {
    CACHE.with(|cache| cache.borrow_mut().clear());
    IN_PROGRESS.with(|open| open.borrow_mut().clear());
    FILE_INDEX.with(|index| index.borrow_mut().clear());
    ALIAS_INDEX.with(|index| index.borrow_mut().clear());
    crate::langdecl::swift::reset_roles();
}

/// What one file declares that a scope's own bindings cannot say: the written
/// return type of its functions and the written type of each type's stored
/// properties. Every entry abstains (`None`) rather than choose between two
/// declarations, so a name declared twice with different answers types nothing.
#[derive(Debug, Default)]
struct FileIndex {
    /// Function name → its written return type. Only a function that takes no
    /// parameters has one: a call with no arguments can then only mean that
    /// signature, which Swift allows once per scope. Any same-named declaration
    /// with parameters, or one that disagrees, makes the name unknowable.
    returns: HashMap<String, Option<String>>,
    /// (qualified type, property name) → its written type; `None` when the
    /// type states none or the property is declared more than once.
    members: HashMap<(String, String), Option<String>>,
    /// (qualified type, property name) → the type its initializer constructs,
    /// for a property with no written type: `var history = Hist()`.
    constructed: HashMap<(String, String), Option<String>>,
    /// (qualified type, method name) → its written return type, under the same
    /// rule as `returns`.
    methods: HashMap<(String, String), Option<String>>,
    /// Short type name → the qualified name of every non-extension declaration
    /// that carries it. More than one makes the short name unreadable, and one
    /// nested declaration is readable only from inside the type that holds it.
    declared: HashMap<String, Vec<String>>,
    /// The modules this file imports, by the name that follows `import`.
    imports: HashSet<String>,
}

/// One `typealias` this file declares.
#[derive(Debug)]
struct AliasDef {
    /// The qualified type whose body declares it; `None` at file scope.
    holder: Option<String>,
    /// What it stands for, read like any written type; `None` when it is
    /// generic, sits in a generic type or a protocol, or names no readable type.
    target: Option<String>,
    /// Written as one bare nominal type, not an optional, array or tuple.
    plain: bool,
}

/// What a file says about its `typealias` names. A written type that names one
/// is not the nominal type of that name; it is the alias's target, when the file
/// states exactly one and nothing else of that name.
#[derive(Debug, Default)]
struct AliasIndex {
    defs: HashMap<String, Vec<AliasDef>>,
    /// Short names of the types and protocols this file declares.
    types: HashSet<String>,
    /// Names this file declares with `associatedtype`.
    associated: HashSet<String>,
}

/// The root of the tree `node` sits in, `None` when the climb was cut short.
fn file_root(node: Node) -> Option<Node> {
    let mut current = node;
    while let Some(parent) = bounded_parent(current) {
        current = parent;
    }
    (current.kind() == "source_file").then_some(current)
}

fn file_index(from: Node, source: &str) -> Option<Rc<FileIndex>> {
    let root = file_root(from)?;
    let id = root.id();
    if let Some(known) = FILE_INDEX.with(|index| index.borrow().get(&id).cloned()) {
        return Some(known);
    }
    let mut built = FileIndex::default();
    let mut stack = vec![root];
    while let Some(node) = stack.pop() {
        match node.kind() {
            "function_declaration" => index_function(node, source, &mut built),
            "class_declaration" => index_type(node, source, &mut built),
            "import_declaration" => {
                // `import SwiftData`, `import struct Foundation.Date`.
                let text = get_node_text(node, source);
                if let Some(module) = text
                    .split_whitespace()
                    .last()
                    .and_then(|path| path.split('.').next())
                {
                    built.imports.insert(module.to_string());
                }
            }
            _ => {}
        }
        let mut cursor = node.walk();
        stack.extend(node.named_children(&mut cursor));
    }
    let built = Rc::new(built);
    FILE_INDEX.with(|index| index.borrow_mut().insert(id, Rc::clone(&built)));
    Some(built)
}

/// Whether `node` sits in the initializer of the binding that introduces
/// `name`: `let task = task(from: x)`, `if let task = task(from: x)`,
/// `guard let task = task(from: x)`. A binding is not in scope in its own
/// initializer, so a use there names whatever `name` meant outside it.
pub(crate) fn use_is_in_own_initializer(node: Node, source: &str, name: &str) -> bool {
    let within = |outer: Node| {
        outer.start_byte() <= node.start_byte() && node.end_byte() <= outer.end_byte()
    };
    let mut current = bounded_parent(node);
    while let Some(parent) = current {
        match parent.kind() {
            "property_declaration" => {
                let Some(value) = parent.child_by_field_name("value") else {
                    return false;
                };
                if !within(value) {
                    return false;
                }
                let mut bound = Vec::new();
                for index in 0..parent.child_count() {
                    let Some(child) = parent.child(index) else {
                        continue;
                    };
                    if parent.field_name_for_child(index as u32) == Some("name")
                        && child.kind() == "pattern"
                    {
                        collect_bound(child, source, &mut bound);
                    }
                }
                return bound.iter().any(|each| each == name);
            }
            // `for x in x.sorted()`: the sequence is read before `x` is bound.
            "for_statement" => {
                let (Some(item), Some(collection)) = (
                    parent.child_by_field_name("item"),
                    parent.child_by_field_name("collection"),
                ) else {
                    return false;
                };
                let mut bound = Vec::new();
                collect_bound(item, source, &mut bound);
                return within(collection) && bound.iter().any(|each| each == name);
            }
            "if_statement" | "guard_statement" | "while_statement" => {
                // `introduced` is the name a condition binds, and `assigned` that
                // its `=` was seen: `guard let s, s.ok` binds `s` with no
                // initializer, and the condition after it is a plain test that
                // already sees the new `s`.
                let mut introduced: Option<String> = None;
                let mut assigned = false;
                for index in 0..parent.child_count() {
                    let Some(child) = parent.child(index) else {
                        continue;
                    };
                    match (parent.field_name_for_child(index as u32), child.kind()) {
                        (Some("bound_identifier"), _) => {
                            introduced = Some(get_node_text(child, source));
                            assigned = false;
                        }
                        (Some("condition"), "value_binding_pattern") => {}
                        // The `=` and the `,` between conditions carry the field too.
                        (Some("condition"), _) if !child.is_named() => {
                            if get_node_text(child, source) == "=" {
                                assigned = true;
                            }
                        }
                        (Some("condition"), _) => {
                            if assigned && within(child) && introduced.as_deref() == Some(name) {
                                return true;
                            }
                            introduced = None;
                            assigned = false;
                        }
                        _ => {}
                    }
                }
                return false;
            }
            _ if is_scope(parent) => return false,
            _ => {}
        }
        current = bounded_parent(parent);
    }
    false
}

fn alias_index(from: Node, source: &str) -> Option<Rc<AliasIndex>> {
    let root = file_root(from)?;
    let id = root.id();
    if let Some(known) = ALIAS_INDEX.with(|index| index.borrow().get(&id).cloned()) {
        return Some(known);
    }
    let mut built = AliasIndex::default();
    let mut stack = vec![root];
    while let Some(node) = stack.pop() {
        match node.kind() {
            // Neither a type nor an alias is declared inside these bodies in a
            // way a name used elsewhere could mean, and they are most of a file.
            "function_body"
            | "computed_property"
            | "lambda_literal"
            | "property_declaration"
            | "init_declaration"
            | "deinit_declaration"
            | "function_declaration" => continue,
            "class_declaration" => {
                if let Some((path, false)) = type_path(node, source) {
                    built.types.insert(short_name(&path).to_string());
                }
            }
            "protocol_declaration" => {
                if let Some(name) = node.child_by_field_name("name") {
                    built.types.insert(get_node_text(name, source));
                }
            }
            "typealias_declaration" => index_alias(node, source, &mut built),
            "associatedtype_declaration" => {
                if let Some(name) = node.child_by_field_name("name") {
                    built.associated.insert(get_node_text(name, source));
                }
            }
            _ => {}
        }
        let mut cursor = node.walk();
        stack.extend(node.named_children(&mut cursor));
    }
    let built = Rc::new(built);
    ALIAS_INDEX.with(|index| index.borrow_mut().insert(id, Rc::clone(&built)));
    Some(built)
}

fn index_alias(node: Node, source: &str, built: &mut AliasIndex) {
    let Some(name) = node.child_by_field_name("name") else {
        return;
    };
    let mut readable = true;
    let mut holder = None;
    let mut ancestor = bounded_parent(node);
    while let Some(outer) = ancestor {
        match outer.kind() {
            "class_declaration" => {
                let mut cursor = outer.walk();
                if outer
                    .named_children(&mut cursor)
                    .any(|child| child.kind() == "type_parameters")
                {
                    readable = false;
                }
                if holder.is_none() {
                    holder = type_path(outer, source).map(|(path, _)| path);
                    readable &= holder.is_some();
                }
            }
            // An associated type's alias stands for whatever conforms.
            "protocol_declaration" => readable = false,
            _ => {}
        }
        ancestor = bounded_parent(outer);
    }
    let mut cursor = node.walk();
    let written = node
        .named_children(&mut cursor)
        .filter(|child| {
            child.id() != name.id() && !matches!(child.kind(), "modifiers" | "type_parameters")
        })
        .last();
    let generic = node
        .named_children(&mut node.walk())
        .any(|child| child.kind() == "type_parameters");
    let target = (readable && !generic)
        .then(|| {
            written
                .and_then(|written| normalize_within(written, source, &BTreeMap::new(), 0, false))
        })
        .flatten();
    built
        .defs
        .entry(get_node_text(name, source))
        .or_default()
        .push(AliasDef {
            holder,
            target,
            plain: written.is_some_and(|written| written.kind() == "user_type"),
        });
}

/// What a bare type name written at `from` stands for, when this file's own
/// `typealias` says: `typealias Simulator = Sim` makes `Simulator` another
/// spelling of `Sim`. The file must declare exactly one alias of the name and no
/// type of it, the alias must not stand for another alias, and one declared in a
/// type's body is the name's meaning only inside that type: anywhere else the
/// same spelling may be a type this file cannot see.
pub(crate) fn alias_target(from: Node, source: &str, name: &str) -> Option<String> {
    alias_target_in(from, source, name, false)
}

/// Whether `name` is a generic parameter of a declaration `from` sits in.
pub(crate) fn is_generic_parameter(from: Node, source: &str, name: &str) -> bool {
    let mut ancestor = bounded_parent(from);
    while let Some(node) = ancestor {
        if matches!(
            node.kind(),
            "function_declaration"
                | "init_declaration"
                | "class_declaration"
                | "typealias_declaration"
        ) && generic_names(node, source).contains(name)
        {
            return true;
        }
        ancestor = bounded_parent(node);
    }
    false
}

/// Whether a callable `from` sits in declares a `typealias` of `name` in its body.
fn declares_local_alias(from: Node, source: &str, name: &str) -> bool {
    let mut ancestor = bounded_parent(from);
    while let Some(scope) = ancestor {
        if matches!(
            scope.kind(),
            "function_declaration"
                | "init_declaration"
                | "deinit_declaration"
                | "lambda_literal"
                | "computed_property"
        ) {
            let mut stack = vec![scope];
            let mut seen = 0usize;
            while let Some(node) = stack.pop() {
                seen += 1;
                if seen > 20_000 {
                    // Too large to read: not knowing is not a no.
                    return true;
                }
                if node.kind() == "typealias_declaration"
                    && node
                        .child_by_field_name("name")
                        .is_some_and(|each| get_node_text(each, source) == name)
                {
                    return true;
                }
                let mut cursor = node.walk();
                stack.extend(node.named_children(&mut cursor));
            }
        }
        ancestor = bounded_parent(scope);
    }
    false
}

/// [`alias_target`]; `receiver` also demands a plain nominal alias (`Parcel`,
/// not `Parcel?` or `[Parcel]`), the only kind a static member is looked up on.
fn alias_target_in(from: Node, source: &str, name: &str, receiver: bool) -> Option<String> {
    let index = alias_index(from, source)?;
    let [def] = index.defs.get(name)?.as_slice() else {
        return None;
    };
    if index.types.contains(name) || index.associated.contains(name) {
        return None;
    }
    if receiver && !def.plain {
        return None;
    }
    // A generic parameter or a local alias of the name in scope is what it names.
    if is_generic_parameter(from, source, name) || declares_local_alias(from, source, name) {
        return None;
    }
    let target = def.target.clone()?;
    let bare = target.trim_matches(['[', ']', '?']);
    if bare == name || index.defs.contains_key(bare) {
        return None;
    }
    if let Some(holder) = &def.holder {
        let mut cursor = Some(from);
        let mut inside = false;
        while let Some(node) = cursor {
            if node.kind() == "class_declaration" {
                if let Some((path, _)) = type_path(node, source) {
                    if path == *holder || path.starts_with(&format!("{holder}.")) {
                        inside = true;
                        break;
                    }
                }
            }
            cursor = bounded_parent(node);
        }
        if !inside {
            return None;
        }
    }
    Some(target)
}

/// [`alias_target`] for a name that heads a receiver or a constructor call: only
/// a capitalised name is a type, and only a plain nominal target can be one.
pub(crate) fn alias_of_type_name(from: Node, source: &str, name: &str) -> Option<String> {
    if !name.chars().next().is_some_and(char::is_uppercase) {
        return None;
    }
    alias_target_in(from, source, name, true).filter(|target| !target.contains(['[', ']', '?']))
}

fn index_function(node: Node, source: &str, built: &mut FileIndex) {
    let Some(name) = node.child_by_field_name("name") else {
        return;
    };
    let bounds = generic_bounds(node, source);
    let mut returned = None;
    for index in 0..node.child_count() {
        let Some(child) = node.child(index) else {
            continue;
        };
        if node.field_name_for_child(index as u32) == Some("name")
            && child.kind() != "simple_identifier"
        {
            returned = normalize(child, source, &bounds);
            break;
        }
    }
    // A generic parameter of the function or of the type around it is no
    // nominal type: `-> [T]` says nothing a name lookup can use.
    let mut generics = generic_names(node, source);
    let owner = enclosing_type(node);
    if let Some(owner) = owner {
        generics.extend(generic_names(owner, source));
    }
    if returned
        .as_deref()
        .is_some_and(|ty| names_generic(ty, &generics))
    {
        returned = None;
    }
    // A function with parameters never states what a bare `name()` returns.
    let mut cursor = node.walk();
    let takes_parameters = node
        .named_children(&mut cursor)
        .any(|child| child.kind() == "parameter");
    if takes_parameters {
        returned = None;
    }
    let name = get_node_text(name, source);
    // A method is also keyed by the type it is declared on. Only a function
    // that is a direct member of a type body counts; a nested function is not.
    let direct_owner = owner
        .filter(|owner| {
            bounded_parent(node)
                .and_then(bounded_parent)
                .is_some_and(|grand| grand.id() == owner.id())
        })
        .and_then(|owner| type_path(owner, source));
    if let Some((qualified, _)) = direct_owner {
        merge_declared(
            &mut built.methods,
            (qualified, name.clone()),
            returned.clone(),
        );
    }
    merge_declared(&mut built.returns, name, returned);
}

/// Record `value` under `key`; a second declaration that does not say the same
/// thing makes the key unknowable.
fn merge_declared<K: std::hash::Hash + Eq>(
    map: &mut HashMap<K, Option<String>>,
    key: K,
    value: Option<String>,
) {
    match map.entry(key) {
        std::collections::hash_map::Entry::Vacant(slot) => {
            slot.insert(value);
        }
        std::collections::hash_map::Entry::Occupied(mut slot) => {
            if *slot.get() != value {
                slot.insert(None);
            }
        }
    }
}

/// The generic parameter names a declaration lists: `<T, U: P>`.
fn generic_names(node: Node, source: &str) -> HashSet<String> {
    let mut names = HashSet::new();
    let mut cursor = node.walk();
    for child in node.named_children(&mut cursor) {
        if child.kind() != "type_parameters" {
            continue;
        }
        let mut params = child.walk();
        for param in child.named_children(&mut params) {
            let mut inner = param.walk();
            let first = param
                .named_children(&mut inner)
                .find(|item| item.kind() == "type_identifier");
            if let Some(first) = first {
                names.insert(get_node_text(first, source));
            }
        }
    }
    names
}

/// Whether a normalized type (`T`, `[T]`) is one of `generics`.
fn names_generic(ty: &str, generics: &HashSet<String>) -> bool {
    let bare = ty.trim_start_matches('[').trim_end_matches([']', '?']);
    generics.contains(bare)
}

/// `var history = Hist()`: the type a property with no written type is
/// initialised by construction of. `None` for anything but one parenthesised
/// call of a capitalised name.
fn constructed_property_type(node: Node, source: &str) -> Option<String> {
    let value = node.child_by_field_name("value")?;
    if value.kind() != "call_expression" || swift_is_subscript_call(value) {
        return None;
    }
    let mut cursor = value.walk();
    let callee = value
        .named_children(&mut cursor)
        .find(|child| child.kind() != "call_suffix")?;
    if callee.kind() != "simple_identifier" {
        return None;
    }
    let name = get_node_text(callee, source);
    name.chars()
        .next()
        .is_some_and(char::is_uppercase)
        .then_some(name)
}

/// The qualified name a `class_declaration` declares or extends — `Outer.Inner`
/// for a nested type, the dotted path as written for `extension A.B` — and
/// whether it is an extension. A bare short name says nothing about which of
/// two nested namesakes is meant; the qualified one does.
fn type_path(node: Node, source: &str) -> Option<(String, bool)> {
    let name = node.child_by_field_name("name")?;
    if name.kind() == "user_type" {
        let mut cursor = name.walk();
        let segments: Vec<String> = name
            .named_children(&mut cursor)
            .filter(|child| child.kind() == "type_identifier")
            .map(|child| get_node_text(child, source))
            .collect();
        return (!segments.is_empty()).then(|| (segments.join("."), true));
    }
    if name.kind() != "type_identifier" {
        return None;
    }
    let mut path = get_node_text(name, source);
    let mut ancestor = bounded_parent(node);
    while let Some(outer) = ancestor {
        if outer.kind() == "class_declaration" {
            let (outer_path, _) = type_path(outer, source)?;
            path = format!("{outer_path}.{path}");
            break;
        }
        ancestor = bounded_parent(outer);
    }
    Some((path, false))
}

/// The last segment of a qualified name.
fn short_name(qualified: &str) -> &str {
    qualified.rsplit('.').next().unwrap_or(qualified)
}

fn index_type(node: Node, source: &str, built: &mut FileIndex) {
    let Some((type_name, extension)) = type_path(node, source) else {
        return;
    };
    if !extension {
        built
            .declared
            .entry(short_name(&type_name).to_string())
            .or_default()
            .push(type_name.clone());
    }
    let Some(body) = node.child_by_field_name("body") else {
        return;
    };
    let bounds = generic_bounds(node, source);
    let mut cursor = body.walk();
    for member in body.named_children(&mut cursor) {
        if member.kind() != "property_declaration" {
            continue;
        }
        let declared = declared_properties(member, source, &bounds);
        let constructed = (declared.len() == 1 && declared[0].1.is_none())
            .then(|| constructed_property_type(member, source))
            .flatten();
        for (name, ty) in declared {
            let key = (type_name.clone(), name);
            // A second declaration of the same name never agrees with the first.
            for (map, value) in [
                (&mut built.members, ty),
                (&mut built.constructed, constructed.clone()),
            ] {
                match map.entry(key.clone()) {
                    std::collections::hash_map::Entry::Vacant(slot) => {
                        slot.insert(value);
                    }
                    std::collections::hash_map::Entry::Occupied(mut slot) => {
                        slot.insert(None);
                    }
                }
            }
        }
    }
}

/// The names a `property_declaration` binds and the type each one is written
/// with. A pattern that binds several names (`let (a, b)`) types none of them.
fn declared_properties(
    node: Node,
    source: &str,
    bounds: &BTreeMap<String, String>,
) -> Vec<(String, Option<String>)> {
    let mut found: Vec<(String, Option<String>)> = Vec::new();
    let mut pending: Vec<String> = Vec::new();
    let mut typed: Option<String> = None;
    let mut flush = |pending: &mut Vec<String>, typed: &mut Option<String>| {
        let single = pending.len() == 1;
        for name in pending.drain(..) {
            found.push((name, if single { typed.clone() } else { None }));
        }
        *typed = None;
    };
    for index in 0..node.child_count() {
        let Some(child) = node.child(index) else {
            continue;
        };
        match (node.field_name_for_child(index as u32), child.kind()) {
            (Some("name"), "pattern") => {
                flush(&mut pending, &mut typed);
                collect_bound(child, source, &mut pending);
            }
            (_, "type_annotation") => {
                let mut cursor = child.walk();
                typed = child
                    .named_children(&mut cursor)
                    .find_map(|inner| normalize(inner, source, bounds));
            }
            _ => {}
        }
    }
    flush(&mut pending, &mut typed);
    found
}

/// Whether `node` opens a Swift scope that binds names.
pub(crate) fn is_scope(node: Node) -> bool {
    matches!(
        node.kind(),
        "function_declaration" | "init_declaration" | "deinit_declaration" | "lambda_literal"
    )
}

/// Methods whose closure argument receives one element of the receiver.
const ELEMENT_METHODS: &[&str] = &[
    "map",
    "compactMap",
    "flatMap",
    "filter",
    "forEach",
    "first",
    "contains",
    "allSatisfy",
    "sorted",
    "min",
    "max",
    "removeAll",
    "drop",
    "prefix",
    "firstIndex",
    "lastIndex",
    "last",
];

/// The bindings of `scope`, computed once per scope node.
pub(crate) fn bindings(scope: Node, source: &str) -> Rc<Bindings> {
    let id = scope.id();
    if let Some(known) = CACHE.with(|cache| cache.borrow().get(&id).cloned()) {
        return known;
    }
    // Computing a scope looks names up in the enclosing scopes, and the walk
    // up starts at the scope being computed: `for h in xs` where `xs` is not
    // bound yet asks for this very scope. A scope that is already open
    // answers with nothing, uncached, so the lookup moves on to its parent
    // instead of recursing until the stack is gone.
    if !IN_PROGRESS.with(|open| open.borrow_mut().insert(id)) {
        return Rc::new(Bindings::new());
    }
    let computed = Rc::new(compute(scope, source));
    IN_PROGRESS.with(|open| open.borrow_mut().remove(&id));
    CACHE.with(|cache| cache.borrow_mut().insert(id, Rc::clone(&computed)));
    computed
}

fn compute(scope: Node, source: &str) -> Bindings {
    let mut out = Bindings::new();
    let bounds = generic_bounds(scope, source);
    let mut cursor = scope.walk();
    let children: Vec<Node> = scope.named_children(&mut cursor).collect();
    if scope.kind() == "lambda_literal" {
        lambda_parameters(scope, source, &bounds, &mut out);
    } else {
        for child in &children {
            if child.kind() == "parameter" {
                parameter(*child, source, &bounds, &mut out);
            }
        }
    }
    if scope.kind() != "lambda_literal" {
        for member in function_typed_members(scope, source) {
            out.entry(member).or_default();
        }
    }
    let mut stack: Vec<Node> = children
        .into_iter()
        .filter(|child| child.kind() != "parameter")
        .collect();
    stack.reverse();
    while let Some(node) = stack.pop() {
        match node.kind() {
            "function_declaration" => {
                // A nested function is a name this scope binds. It is not a
                // module-level symbol, and a call to it is not a call of a
                // same-named function elsewhere.
                if let Some(name) = node.child_by_field_name("name") {
                    bind(&mut out, get_node_text(name, source), None);
                }
                continue;
            }
            "lambda_literal" | "init_declaration" | "deinit_declaration" => continue,
            "class_declaration" | "protocol_declaration" => continue,
            "property_declaration" => {
                property(node, source, &bounds, &mut out);
                continue;
            }
            "for_statement" => for_statement(node, source, &bounds, &mut out),
            "if_statement" | "guard_statement" | "while_statement" => {
                conditions(node, source, &mut out);
            }
            _ => {}
        }
        // Any other pattern binds a name with no type this pass can state:
        // `case let .some(x)`, `catch let e`, a tuple destructuring.
        let mut walker = node.walk();
        let mut named: Vec<Node> = node.named_children(&mut walker).collect();
        if node.kind() == "catch_block" {
            bind(&mut out, "error".to_string(), None);
        }
        named.reverse();
        for child in named {
            if child.kind() == "simple_identifier"
                && field_of(node, child) == Some("bound_identifier")
            {
                // Already bound, and typed, by the statement's own handler.
                let name = get_node_text(child, source);
                // A pattern that binds a name again hides what the first
                // binding's members were typed through.
                hide_members(&mut out, &name);
                out.entry(name).or_default();
            } else {
                stack.push(child);
            }
        }
    }
    out
}

/// The field `child` occupies on `parent`.
fn field_of<'a>(parent: Node<'a>, child: Node<'a>) -> Option<&'static str> {
    let mut cursor = parent.walk();
    if !cursor.goto_first_child() {
        return None;
    }
    loop {
        if cursor.node().id() == child.id() {
            return cursor.field_name();
        }
        if !cursor.goto_next_sibling() {
            return None;
        }
    }
}

/// Record `name`. The first declaration in source order keeps its type: a later
/// shadowing `let` of the same name in an inner block must not retype uses that
/// precede it, and not knowing is the safe answer.
fn bind(out: &mut Bindings, name: String, ty: Option<String>) {
    if name.is_empty() || name == "_" {
        return;
    }
    match out.get_mut(&name) {
        Some(existing) => {
            if existing.ty != ty {
                existing.ty = None;
                existing.init = None;
            }
            // A second binding of the name: which one a use means is not known,
            // so what was typed through the first (`open.transform`) is not either.
            hide_members(out, &name);
        }
        None => {
            out.insert(name, Bound { ty, init: None });
        }
    }
}

/// Drop the member paths (`name.label`, `name.0`) a name was typed through.
fn hide_members(out: &mut Bindings, name: &str) {
    let prefix = format!("{name}.");
    out.retain(|key, _| !key.starts_with(&prefix));
}

fn parameter(node: Node, source: &str, bounds: &BTreeMap<String, String>, out: &mut Bindings) {
    let mut name: Option<String> = None;
    let mut ty: Option<String> = None;
    let mut tuple: Option<Node> = None;
    for index in 0..node.child_count() {
        let Some(child) = node.child(index) else {
            continue;
        };
        if node.field_name_for_child(index as u32) != Some("name") {
            continue;
        }
        if child.kind() == "simple_identifier" && name.is_none() {
            name = Some(get_node_text(child, source));
        } else if child.kind() != "simple_identifier" {
            ty = normalize(child, source, bounds);
            tuple = (child.kind() == "tuple_type").then_some(child);
        }
    }
    if let Some(name) = name {
        if let Some(tuple) = tuple {
            tuple_members(tuple, source, bounds, &name, out);
        }
        bind(out, name, ty);
    }
}

/// The type of a tuple member path written at `from`: `open.transform` where the
/// nearest scope that binds `open` binds it as a tuple with a typed `transform`.
/// A scope that rebinds `open` without that member hides it.
pub(crate) fn tuple_member_type(from: Node, source: &str, path: &str) -> Option<String> {
    let (root, _) = path.split_once('.')?;
    let mut ancestor = bounded_parent(from);
    while let Some(scope) = ancestor {
        if is_scope(scope) {
            let found = bindings(scope, source);
            if let Some(member) = found.get(path) {
                return member.ty.clone();
            }
            if found.contains_key(root) {
                return None;
            }
        }
        ancestor = bounded_parent(scope);
    }
    None
}

/// The members of a tuple-typed name: `open.transform` and `open.0` for
/// `open: (transform: Case, start: Int)`. A member path is bound only where the
/// tuple's own name is, so a scope that rebinds the name hides it.
fn tuple_members(
    tuple: Node,
    source: &str,
    bounds: &BTreeMap<String, String>,
    name: &str,
    out: &mut Bindings,
) {
    let mut cursor = tuple.walk();
    let mut position = 0usize;
    for item in tuple.named_children(&mut cursor) {
        if item.kind() != "tuple_type_item" {
            continue;
        }
        let mut inner = item.walk();
        let parts: Vec<Node> = item.named_children(&mut inner).collect();
        let (label, written) = match parts.as_slice() {
            [label, written] if label.kind() == "simple_identifier" => {
                (Some(get_node_text(*label, source)), *written)
            }
            [written] => (None, *written),
            _ => {
                position += 1;
                continue;
            }
        };
        let ty = normalize(written, source, bounds);
        if let Some(label) = label {
            bind(out, format!("{name}.{label}"), ty.clone());
        }
        bind(out, format!("{name}.{position}"), ty);
        position += 1;
    }
}

fn lambda_parameters(
    scope: Node,
    source: &str,
    bounds: &BTreeMap<String, String>,
    out: &mut Bindings,
) {
    let mut explicit: Vec<(String, Option<String>)> = Vec::new();
    let mut cursor = scope.walk();
    for child in scope.named_children(&mut cursor) {
        if child.kind() != "lambda_function_type" {
            continue;
        }
        let mut inner = child.walk();
        for list in child.named_children(&mut inner) {
            if list.kind() != "lambda_function_type_parameters" {
                continue;
            }
            let mut params = list.walk();
            for param in list.named_children(&mut params) {
                if param.kind() != "lambda_parameter" {
                    continue;
                }
                let mut name = None;
                let mut ty = None;
                let mut fields = param.walk();
                for item in param.named_children(&mut fields) {
                    if item.kind() == "simple_identifier" && name.is_none() {
                        name = Some(get_node_text(item, source));
                    } else if item.kind() != "simple_identifier" {
                        ty = normalize(item, source, bounds);
                    }
                }
                if let Some(name) = name {
                    explicit.push((name, ty));
                }
            }
        }
    }
    let element = element_type(scope, source);
    // The function type the closure is written against types its parameters
    // when no collection does; the two are exclusive, because a closure handed
    // to `xs.map` is an argument, not an element of an annotated literal.
    let mut annotated = annotated_closure_parameters(scope, source, bounds).unwrap_or_default();
    for ty in annotated.iter_mut() {
        if ty
            .as_deref()
            .is_some_and(|ty| names_alias(scope, source, ty))
        {
            *ty = None;
        }
    }
    let annotated_at = |index: usize| annotated.get(index).cloned().flatten();
    match explicit.as_slice() {
        [] => {
            bind(out, "$0".to_string(), element.or_else(|| annotated_at(0)));
            // `$1` and up exist only when the annotation says the closure takes
            // that many; an untyped `$1` elsewhere stays as it was.
            for index in 1..annotated.len() {
                bind(out, format!("${index}"), annotated_at(index));
            }
        }
        [(name, ty)] => bind(
            out,
            name.clone(),
            ty.clone().or(element).or_else(|| annotated_at(0)),
        ),
        many => {
            let arity_agrees = annotated.len() == many.len();
            for (index, (name, ty)) in many.iter().enumerate() {
                let from_annotation = if arity_agrees {
                    annotated_at(index)
                } else {
                    None
                };
                bind(out, name.clone(), ty.clone().or(from_annotation));
            }
        }
    }
}

fn property(node: Node, source: &str, bounds: &BTreeMap<String, String>, out: &mut Bindings) {
    let mut pending: Vec<String> = Vec::new();
    let mut typed: Option<String> = None;
    let mut init: Option<String> = None;
    let flush = |pending: &mut Vec<String>,
                 typed: &mut Option<String>,
                 init: &mut Option<String>,
                 out: &mut Bindings| {
        let single = pending.len() == 1;
        for name in pending.drain(..) {
            bind(out, name.clone(), if single { typed.clone() } else { None });
            if let Some(entry) = out.get_mut(&name) {
                if single && entry.init.is_none() && entry.ty.is_none() {
                    entry.init = init.clone();
                }
            }
        }
        *typed = None;
        *init = None;
    };
    for index in 0..node.child_count() {
        let Some(child) = node.child(index) else {
            continue;
        };
        match (node.field_name_for_child(index as u32), child.kind()) {
            (Some("name"), "pattern") => {
                flush(&mut pending, &mut typed, &mut init, out);
                collect_bound(child, source, &mut pending);
            }
            (_, "type_annotation") => {
                let mut cursor = child.walk();
                typed = child
                    .named_children(&mut cursor)
                    .find_map(|inner| normalize(inner, source, bounds));
            }
            (Some("value"), "simple_identifier") if typed.is_none() && pending.len() == 1 => {
                typed = lookup_outer(node, source, &get_node_text(child, source), out);
            }
            (Some("value"), "call_expression") if pending.len() == 1 => {
                init = call_shape(child, source);
                if typed.is_none() {
                    typed = collection_type(child, source, out)
                        .or_else(|| own_method_call_type(child, source, out));
                }
            }
            // `let xs = (try? load()) ?? []`, `let xs = (opt?.items ?? [])`: a
            // collection the initializer's own text types.
            (
                Some("value"),
                "try_expression"
                | "tuple_expression"
                | "nil_coalescing_expression"
                | "navigation_expression",
            ) if pending.len() == 1 && typed.is_none() => {
                typed = collection_type(child, source, out);
            }
            // `let c = a <+> b`: typed by the custom operator's declared result.
            (Some("value"), "infix_expression") if pending.len() == 1 => {
                init = child
                    .child_by_field_name("op")
                    .filter(|op| op.kind() == "custom_operator")
                    .map(|op| get_node_text(op, source));
            }
            _ => {}
        }
    }
    flush(&mut pending, &mut typed, &mut init, out);
}

/// `make(…)` → `make`; `Factory.make(…)` → `Factory.make`: the shapes the
/// resolver types by the callee's written return type. A constructor call has
/// the same spelling and is typed by the constructed type instead.
fn call_shape(call: Node, source: &str) -> Option<String> {
    let mut cursor = call.walk();
    let callee = call
        .named_children(&mut cursor)
        .find(|child| child.kind() != "call_suffix")?;
    match callee.kind() {
        "simple_identifier" => Some(get_node_text(callee, source)),
        "navigation_expression" => {
            let target = callee.child_by_field_name("target")?;
            let name = callee
                .child_by_field_name("suffix")?
                .child_by_field_name("suffix")?;
            (target.kind() == "simple_identifier").then(|| {
                format!(
                    "{}.{}",
                    get_node_text(target, source),
                    get_node_text(name, source)
                )
            })
        }
        _ => None,
    }
}

fn collect_bound(pattern: Node, source: &str, out: &mut Vec<String>) {
    let mut stack = vec![pattern];
    while let Some(node) = stack.pop() {
        if node.kind() == "simple_identifier" && field_of_parent(node) == Some("bound_identifier") {
            out.push(get_node_text(node, source));
        }
        let mut cursor = node.walk();
        for child in node.named_children(&mut cursor) {
            stack.push(child);
        }
    }
    out.reverse();
}

fn field_of_parent(node: Node) -> Option<&'static str> {
    let parent = bounded_parent(node)?;
    field_of(parent, node)
}

fn for_statement(node: Node, source: &str, _bounds: &BTreeMap<String, String>, out: &mut Bindings) {
    let mut names = Vec::new();
    if let Some(item) = node.child_by_field_name("item") {
        collect_bound(item, source, &mut names);
    }
    let element = node
        .child_by_field_name("collection")
        .filter(|collection| collection.kind() == "simple_identifier")
        .and_then(|collection| lookup_outer(node, source, &get_node_text(collection, source), out))
        .and_then(|ty| {
            ty.strip_prefix('[')
                .and_then(|t| t.strip_suffix(']'))
                .map(str::to_string)
        });
    let single = names.len() == 1;
    for name in names {
        bind(out, name, if single { element.clone() } else { None });
    }
}

/// `if let x = y`, `guard let x = y`, `if let x`: each `bound_identifier` is
/// typed by the identifier that follows it when that identifier has a type.
fn conditions(node: Node, source: &str, out: &mut Bindings) {
    let mut pending: Option<String> = None;
    let resolved = |name: String, value: Option<Node>, out: &mut Bindings| {
        let ty = match value {
            Some(value) if value.kind() == "simple_identifier" => {
                lookup_outer(node, source, &get_node_text(value, source), out)
            }
            // `if let u = history.pop()`: a call whose return type this file
            // writes down.
            Some(value) if value.kind() == "call_expression" => {
                expression_type(value, source, node, out, 0)
            }
            Some(_) => None,
            None => lookup_outer(node, source, &name, out),
        };
        // The binding unwraps the optional it is made from.
        let ty = ty.map(|ty| ty.strip_suffix('?').map_or(ty.clone(), str::to_string));
        // `if let u = history.pop()`: the call shape a `let` is typed by. A name
        // this scope already bound is a second binding of it, and which one a use
        // means is not known, so it keeps no shape.
        let shape = match value {
            Some(value) if value.kind() == "call_expression" && ty.is_none() => {
                call_shape(value, source)
            }
            _ => None,
        };
        let existed = out.contains_key(&name);
        bind(out, name.clone(), ty);
        if let Some(entry) = out.get_mut(&name) {
            if let (true, Some(shape)) = (entry.ty.is_none(), shape) {
                entry.init = (!existed || entry.init.as_ref() == Some(&shape)).then_some(shape);
            }
        }
    };
    for index in 0..node.child_count() {
        let Some(child) = node.child(index) else {
            continue;
        };
        match node.field_name_for_child(index as u32) {
            Some("bound_identifier") => {
                if let Some(name) = pending.take() {
                    resolved(name, None, out);
                }
                pending = Some(get_node_text(child, source));
            }
            Some("condition") if child.is_named() && child.kind() != "value_binding_pattern" => {
                if let Some(name) = pending.take() {
                    resolved(name, Some(child), out);
                }
            }
            _ => {}
        }
    }
    if let Some(name) = pending.take() {
        resolved(name, None, out);
    }
}

/// The type `name` has where `from` sits: the bindings collected so far in this
/// scope, then each enclosing scope's. Optionals are already unwrapped.
///
/// A name no scope binds is a member of the enclosing type when that type, read
/// from this file, declares it once with a written type.
fn lookup_outer(from: Node, source: &str, name: &str, current: &Bindings) -> Option<String> {
    match lookup_scoped(from, source, name, current) {
        Some(ty) => ty,
        None => member_type(from, source, name),
    }
}

/// Whether a scope binds `name` where `from` sits: `Some(type)` when one does
/// (the type is `None` when it is bound untyped), `None` when no scope does.
fn lookup_scoped(
    from: Node,
    source: &str,
    name: &str,
    current: &Bindings,
) -> Option<Option<String>> {
    if let Some(found) = current.get(name) {
        return Some(found.ty.clone());
    }
    let mut ancestor = bounded_parent(from);
    while let Some(scope) = ancestor {
        // A computed property's getter binds locals the way a function does.
        if is_scope(scope) || scope.kind() == "computed_property" {
            let found = bindings(scope, source);
            if let Some(found) = found.get(name) {
                return Some(found.ty.clone());
            }
        }
        ancestor = bounded_parent(scope);
    }
    None
}

/// The type of `name` as a stored property of the type `from` sits in.
fn member_type(from: Node, source: &str, name: &str) -> Option<String> {
    let index = file_index(from, source)?;
    let owner = lexical_owner(&index, from, source)?;
    index
        .members
        .get(&(owner, name.to_string()))
        .cloned()
        .flatten()
}

/// The qualified name of the type `from` sits in, `None` when the file declares
/// that name more than once (a type is then not told apart by its name).
fn lexical_owner(index: &FileIndex, from: Node, source: &str) -> Option<String> {
    let (qualified, extension) = type_path(enclosing_type(from)?, source)?;
    let copies = index.declared.get(short_name(&qualified)).map_or(0, |all| {
        all.iter().filter(|each| **each == qualified).count()
    });
    (extension || copies <= 1).then_some(qualified)
}

/// The declaration a written short type name means where `from` sits. The file
/// must declare exactly one type of that name, and a nested one is the name's
/// meaning only inside the type that holds it: anywhere else the same spelling
/// may be a top-level type in a file this index cannot see.
fn resolve_type(index: &FileIndex, from: Node, source: &str, short: &str) -> Option<String> {
    let [qualified] = index.declared.get(short)?.as_slice() else {
        return None;
    };
    let Some((holder, _)) = qualified.rsplit_once('.') else {
        return Some(qualified.clone());
    };
    let mut cursor = Some(from);
    while let Some(node) = cursor {
        if node.kind() == "class_declaration" {
            if let Some((path, _)) = type_path(node, source) {
                if path == holder || path.starts_with(&format!("{holder}.")) {
                    return Some(qualified.clone());
                }
            }
        }
        cursor = bounded_parent(node);
    }
    None
}

/// Whether a written type names something this file declares with `typealias`.
/// A typealias another file declares is the resolver's to refuse: it alone sees
/// every file (`Extraction::swift_type_aliases`).
fn names_alias(from: Node, source: &str, ty: &str) -> bool {
    let bare = ty.trim_matches(['[', ']', '?']);
    alias_index(from, source).is_some_and(|index| index.defs.contains_key(bare))
}

/// Every name the file declares with `typealias` outside a function or closure
/// body (a local alias cannot be another file's member alias), sorted.
pub(crate) fn declared_aliases(root: Node, source: &str) -> Vec<String> {
    let mut names: Vec<String> = alias_index(root, source)
        .map(|index| index.defs.keys().cloned().collect())
        .unwrap_or_default();
    names.sort();
    names
}

fn enclosing_type(from: Node) -> Option<Node> {
    let mut owner = bounded_parent(from);
    while let Some(node) = owner {
        if node.kind() == "class_declaration" {
            return Some(node);
        }
        owner = bounded_parent(node);
    }
    None
}

/// The written type of `member` on `type_name`, read from this file. Abstains
/// when the file declares the type more than once or the member more than once
/// (two extensions), because the name alone cannot say which one is meant.
fn member_of_type(from: Node, source: &str, type_name: &str, member: &str) -> Option<String> {
    let index = file_index(from, source)?;
    let qualified = resolve_type(&index, from, source, type_name)?;
    index
        .members
        .get(&(qualified, member.to_string()))
        .cloned()
        .flatten()
}

/// [`member_of_type`] for a property with no written type, typed by the
/// construction that initialises it. Kept apart because it is a weaker claim
/// than a written type: only an expression chain that ends in a method of the
/// constructed type reads it.
fn constructed_member_type(
    from: Node,
    source: &str,
    type_name: &str,
    member: &str,
) -> Option<String> {
    let index = file_index(from, source)?;
    let qualified = resolve_type(&index, from, source, type_name)?;
    index
        .constructed
        .get(&(qualified, member.to_string()))
        .cloned()
        .flatten()
}

/// The type a method of `type_name` is written to return, read from this file.
fn method_return_type(from: Node, source: &str, type_name: &str, method: &str) -> Option<String> {
    let index = file_index(from, source)?;
    let qualified = resolve_type(&index, from, source, type_name)?;
    index
        .methods
        .get(&(qualified, method.to_string()))
        .cloned()
        .flatten()
}

/// How deep [`expression_type`] follows an expression. A chain this long is not
/// a collection a reader named.
const EXPRESSION_DEPTH: usize = 8;

/// The type an expression has when the file's own text says so: a bound name, a
/// member reached through a typed value, a call of a function whose return type
/// is written, and `try?`, parentheses and `?? []` around any of those.
/// Optionals are already unwrapped, so `opt?.items` is the type of `items`.
fn expression_type(
    node: Node,
    source: &str,
    from: Node,
    current: &Bindings,
    depth: usize,
) -> Option<String> {
    let ty = expression_type_unaliased(node, source, from, current, depth)?;
    // A nested `typealias Entry = Item` makes `Entry` another spelling of
    // `Item`, not the top-level `Entry` a name lookup would find.
    (!names_alias(from, source, &ty)).then_some(ty)
}

/// The written return type of `name`, a parameterless method the type `from`
/// sits in declares (once, or every time alike): what a bare `name()` inside
/// that type calls, whatever free function shares the name. `Self` is no type
/// a name lookup can use.
fn own_method_return(from: Node, source: &str, name: &str) -> Option<String> {
    let index = file_index(from, source)?;
    let owner = lexical_owner(&index, from, source)?;
    let ty = index
        .methods
        .get(&(owner, name.to_string()))
        .cloned()
        .flatten()?;
    (ty != "Self").then_some(ty)
}

/// The type a `let x = name()` is, when `name` is a parameterless method of the
/// type the statement sits in and this file writes its return type.
fn own_method_call_type(call: Node, source: &str, current: &Bindings) -> Option<String> {
    if swift_is_subscript_call(call) || !call_passes_nothing(call) {
        return None;
    }
    let mut cursor = call.walk();
    let callee = call
        .named_children(&mut cursor)
        .find(|child| child.kind() != "call_suffix")?;
    if callee.kind() != "simple_identifier" {
        return None;
    }
    let name = get_node_text(callee, source);
    // A local closure or a function-typed member named like the method is what
    // the call names.
    if lookup_scoped(call, source, &name, current).is_some() {
        return None;
    }
    enclosing_type(call)?;
    let ty = own_method_return(call, source, &name)?;
    (!names_alias(call, source, &ty)).then_some(ty)
}

/// Whether a call passes no arguments and no trailing closure.
fn call_passes_nothing(call: Node) -> bool {
    let mut cursor = call.walk();
    let Some(suffix) = call
        .named_children(&mut cursor)
        .find(|child| child.kind() == "call_suffix")
    else {
        return false;
    };
    let mut inner = suffix.walk();
    let mut parts = suffix.named_children(&mut inner);
    let first = parts.next();
    first.is_some_and(|args| args.kind() == "value_arguments" && args.named_child_count() == 0)
        && parts.next().is_none()
}

fn expression_type_unaliased(
    node: Node,
    source: &str,
    from: Node,
    current: &Bindings,
    depth: usize,
) -> Option<String> {
    if depth > EXPRESSION_DEPTH {
        return None;
    }
    let recurse = |inner: Node| expression_type(inner, source, from, current, depth + 1);
    match node.kind() {
        "simple_identifier" => {
            let name = get_node_text(node, source);
            match lookup_scoped(from, source, &name, current) {
                Some(ty) => ty,
                None => member_type(from, source, &name).or_else(|| {
                    let index = file_index(from, source)?;
                    let owner = lexical_owner(&index, from, source)?;
                    index
                        .constructed
                        .get(&(owner, name.clone()))
                        .cloned()
                        .flatten()
                }),
            }
        }
        "self_expression" => enclosing_type(node)
            .and_then(|owner| type_path(owner, source))
            .map(|(qualified, _)| short_name(&qualified).to_string()),
        "navigation_expression" => {
            let member = get_node_text(
                node.child_by_field_name("suffix")?
                    .child_by_field_name("suffix")?,
                source,
            );
            let owner = recurse(node.child_by_field_name("target")?)?;
            member_of_type(node, source, &owner, &member)
                .or_else(|| constructed_member_type(node, source, &owner, &member))
        }
        "tuple_expression" => {
            let mut cursor = node.walk();
            let mut values = node.named_children(&mut cursor);
            let only = values.next()?;
            values.next().is_none().then(|| recurse(only)).flatten()
        }
        "try_expression" => {
            let inner = recurse(node.child_by_field_name("expr")?)?;
            // `try? f()` is an Optional: `.map` on it hands the closure the whole
            // collection, so the collection is marked optional.
            let optional = {
                let mut cursor = node.walk();
                let found = node.named_children(&mut cursor).any(|child| {
                    child.kind() == "try_operator" && get_node_text(child, source) == "try?"
                });
                found
            };
            Some(
                if optional && inner.starts_with('[') && !inner.ends_with('?') {
                    format!("{inner}?")
                } else {
                    inner
                },
            )
        }
        "nil_coalescing_expression" => {
            // `xs ?? []` is `xs`'s type, no longer optional. Any other default may
            // be a different type, and a collection typed wrongly binds a wrong edge.
            let default = node.child_by_field_name("if_nil")?;
            let empty = default.kind() == "array_literal" && default.named_child_count() == 0;
            if !empty {
                return None;
            }
            let inner = recurse(node.child_by_field_name("value")?)?;
            Some(
                inner
                    .strip_suffix('?')
                    .map_or(inner.clone(), str::to_string),
            )
        }
        "call_expression" => {
            if let Some(collection) = collection_call_type(node, source, current, &recurse) {
                return Some(collection);
            }
            // `f()` and `r.m()` name a signature only when they pass nothing: a
            // call with arguments may be any overload, including one declared
            // where this file cannot see it.
            if swift_is_subscript_call(node) || !call_passes_nothing(node) {
                return None;
            }
            let mut cursor = node.walk();
            let callee = node
                .named_children(&mut cursor)
                .find(|child| child.kind() != "call_suffix")?;
            // `history.pop()`: a method of the receiver's type, declared in this
            // file, with a written return type.
            if callee.kind() == "navigation_expression" {
                let method = get_node_text(
                    callee
                        .child_by_field_name("suffix")?
                        .child_by_field_name("suffix")?,
                    source,
                );
                let owner = recurse(callee.child_by_field_name("target")?)?;
                return method_return_type(node, source, &owner, &method);
            }
            if callee.kind() != "simple_identifier" {
                return None;
            }
            let name = get_node_text(callee, source);
            // A local closure or a function-typed member named like a function
            // is what the call names.
            if lookup_scoped(from, source, &name, current).is_some() {
                return None;
            }
            // Inside a type, a bare call may name a method — of the type, an
            // extension of it in another file, or a superclass — which shadows
            // the free function. A method this very type declares is what the
            // call names; one it does not declare this file cannot rule out, so
            // it abstains.
            if enclosing_type(node).is_some() {
                return own_method_return(node, source, &name);
            }
            file_index(node, source)?
                .returns
                .get(&name)
                .cloned()
                .flatten()
        }
        _ => None,
    }
}

/// `Sequence` methods that hand back the same elements of an `Array`. Typed
/// only on a receiver this pass already typed as `[T]`.
const SAME_ELEMENT_METHODS: &[&str] = &[
    "filter",
    "sorted",
    "prefix",
    "suffix",
    "dropFirst",
    "dropLast",
];

/// The collection a call over a collection is: `xs.filter { … }` and
/// `xs.sorted(by: …)` are `xs`'s elements again, and `context.fetch(
/// FetchDescriptor<T>(…))` is SwiftData's `[T]` (`ModelContext.fetch(_:)`,
/// read only in a file that imports SwiftData).
fn collection_call_type(
    call: Node,
    source: &str,
    current: &Bindings,
    recurse: &dyn Fn(Node) -> Option<String>,
) -> Option<String> {
    if swift_is_subscript_call(call) {
        return None;
    }
    let mut cursor = call.walk();
    let callee = call
        .named_children(&mut cursor)
        .find(|child| child.kind() != "call_suffix")?;
    if callee.kind() != "navigation_expression" {
        return None;
    }
    let method = get_node_text(
        callee
            .child_by_field_name("suffix")?
            .child_by_field_name("suffix")?,
        source,
    );
    if SAME_ELEMENT_METHODS.contains(&method.as_str()) {
        let owner = recurse(callee.child_by_field_name("target")?)?;
        return owner.starts_with('[').then_some(owner);
    }
    if method == "fetch" {
        let target = callee.child_by_field_name("target")?;
        if !is_model_context(target, source, current, recurse) {
            return None;
        }
        return fetched_collection(call, source);
    }
    None
}

/// Whether `target` is a SwiftData `ModelContext`: a name written as one, bound
/// by `ModelContext(…)`, or a property wrapped `@Environment(\.modelContext)`.
/// A file that declares its own `fetch`, `FetchDescriptor` or `ModelContext` is
/// not asking SwiftData.
fn is_model_context(
    target: Node,
    source: &str,
    current: &Bindings,
    recurse: &dyn Fn(Node) -> Option<String>,
) -> bool {
    let Some(index) = file_index(target, source) else {
        return false;
    };
    if index.returns.contains_key("fetch")
        || index.declared.contains_key("FetchDescriptor")
        || index.declared.contains_key("ModelContext")
    {
        return false;
    }
    if recurse(target).as_deref() == Some("ModelContext") {
        return true;
    }
    if target.kind() != "simple_identifier" {
        return false;
    }
    let name = get_node_text(target, source);
    // `let context = ModelContext(container)`.
    if let Some(found) = current.get(&name) {
        return found.init.as_deref() == Some("ModelContext");
    }
    let mut ancestor = bounded_parent(target);
    while let Some(scope) = ancestor {
        if is_scope(scope) || scope.kind() == "computed_property" {
            if let Some(found) = bindings(scope, source).get(&name) {
                return found.init.as_deref() == Some("ModelContext");
            }
        }
        ancestor = bounded_parent(scope);
    }
    // `@Environment(\.modelContext) private var context` on the enclosing type.
    let Some(owner) = enclosing_type(target) else {
        return false;
    };
    let Some(body) = owner.child_by_field_name("body") else {
        return false;
    };
    let mut cursor = body.walk();
    let members: Vec<Node> = body.named_children(&mut cursor).collect();
    members.into_iter().any(|member| {
        if member.kind() != "property_declaration" {
            return false;
        }
        let text = get_node_text(member, source);
        let head = text.split(['=', '{']).next().unwrap_or("");
        let compact: String = head.split_whitespace().collect();
        compact.contains("@Environment(\\.modelContext)")
            && (compact.ends_with(&format!("var{name}"))
                || compact.contains(&format!("var{name}:")))
    })
}

/// `[T]` for `x.fetch(FetchDescriptor<T>(…))`, the one argument a SwiftData
/// fetch takes, in a file that imports SwiftData.
fn fetched_collection(call: Node, source: &str) -> Option<String> {
    if !file_index(call, source)?.imports.contains("SwiftData") {
        return None;
    }
    let mut cursor = call.walk();
    let suffix = call
        .named_children(&mut cursor)
        .find(|child| child.kind() == "call_suffix")?;
    let mut inner = suffix.walk();
    let mut parts = suffix.named_children(&mut inner);
    let arguments = parts
        .next()
        .filter(|args| args.kind() == "value_arguments")?;
    if parts.next().is_some() || arguments.named_child_count() != 1 {
        return None;
    }
    let value = arguments.named_child(0)?.child_by_field_name("value")?;
    if value.kind() != "constructor_expression" {
        return None;
    }
    let constructed = value.child_by_field_name("constructed_type")?;
    if constructed.kind() != "user_type"
        || user_type_name(constructed, source)? != "FetchDescriptor"
    {
        return None;
    }
    let mut cursor = constructed.walk();
    let arguments = constructed
        .named_children(&mut cursor)
        .find(|child| child.kind() == "type_arguments")?;
    if arguments.named_child_count() != 1 {
        return None;
    }
    // A bare nominal type: `Habit?` and `[Habit]` normalise to something else.
    let written = arguments.named_child(0)?;
    if written.kind() != "user_type" {
        return None;
    }
    let element = normalize(written, source, &BTreeMap::new())?;
    // A generic parameter is no nominal type.
    if is_generic_parameter(call, source, &element) {
        return None;
    }
    (!element.contains(['[', '?'])).then(|| format!("[{element}]"))
}

/// `items[0]` is a `call_expression` whose argument list opens with `[`.
fn swift_is_subscript_call(call: Node) -> bool {
    let mut cursor = call.walk();
    for child in call.named_children(&mut cursor) {
        if child.kind() != "call_suffix" {
            continue;
        }
        let mut inner = child.walk();
        for grandchild in child.named_children(&mut inner) {
            if grandchild.kind() == "value_arguments" {
                return grandchild.child(0).is_some_and(|open| open.kind() == "[");
            }
        }
    }
    false
}

/// The collection type of a `let`'s initializer, when the text states it.
fn collection_type(value: Node, source: &str, current: &Bindings) -> Option<String> {
    expression_type(value, source, value, current, 0).filter(|ty| ty.starts_with('['))
}

/// `[T]` → `T`.
fn element_of(collection: &str) -> Option<String> {
    collection
        .strip_prefix('[')
        .and_then(|t| t.strip_suffix(']'))
        .map(str::to_string)
}

/// The element a closure passed to `xs.map { … }` receives, when `xs` is a
/// collection whose type is written.
fn element_type(lambda: Node, source: &str) -> Option<String> {
    let suffix = bounded_parent(lambda).filter(|n| n.kind() == "call_suffix")?;
    let call = bounded_parent(suffix).filter(|n| n.kind() == "call_expression")?;
    let mut cursor = call.walk();
    let callee = call
        .named_children(&mut cursor)
        .find(|child| child.kind() != "call_suffix")?;
    if callee.kind() != "navigation_expression" {
        return None;
    }
    let method = callee
        .child_by_field_name("suffix")?
        .child_by_field_name("suffix")
        .map(|node| get_node_text(node, source))?;
    if !ELEMENT_METHODS.contains(&method.as_str()) {
        return None;
    }
    let target = callee.child_by_field_name("target")?;
    let collection = expression_type(target, source, lambda, &Bindings::new(), 0)?;
    match collection.strip_suffix('?') {
        // `Optional.map` hands its closure the whole collection. Only optional
        // chaining (`xs?.map`) reaches the elements of an optional collection.
        Some(unwrapped) => {
            let name = callee.child_by_field_name("suffix")?;
            let between = source.get(target.end_byte()..name.end_byte())?;
            // `xs?.filter(…).map { … }`: the `?.` earlier in the same postfix
            // chain unwrapped the optional, so this `.map` is the collection's.
            let chained =
                between.trim_start().starts_with('?') || in_optional_chain(target, source);
            chained.then(|| element_of(unwrapped)).flatten()
        }
        None => element_of(&collection),
    }
}

/// Whether the postfix chain `node` ends with has an optional link (`?.`) in
/// it: what follows such a link is applied to the unwrapped value. A
/// parenthesis ends the chain.
fn in_optional_chain(node: Node, source: &str) -> bool {
    let mut node = node;
    for _ in 0..2 * EXPRESSION_DEPTH {
        match node.kind() {
            "navigation_expression" => {
                let (Some(target), Some(suffix)) = (
                    node.child_by_field_name("target"),
                    node.child_by_field_name("suffix"),
                ) else {
                    return false;
                };
                // Only a `?.` on the chain's root — a parenthesised `try?` — is
                // known to unwrap the optional this collection carries. After a
                // named root (`h?.maybe.map`) the optional may be the member's
                // own, and that `.map` is `Optional.map`.
                if target.kind() == "tuple_expression" {
                    return source
                        .get(target.end_byte()..suffix.end_byte())
                        .is_some_and(|between| between.trim_start().starts_with('?'));
                }
                node = target;
            }
            "call_expression" => {
                let mut cursor = node.walk();
                let Some(callee) = node
                    .named_children(&mut cursor)
                    .find(|child| child.kind() != "call_suffix")
                else {
                    return false;
                };
                node = callee;
            }
            _ => return false,
        }
    }
    false
}

/// The parameter types of the function type a closure literal is written
/// against: `let f: (T) -> Void = { … }`, an element of `let fs: [(T) -> Void] =
/// [{ … }]`, or a closure inside a labelled-tuple element of
/// `[(name: String, apply: (T) -> Void)]`. `None` when the closure sits anywhere
/// else, the annotation is not one of those shapes or the arities disagree.
fn annotated_closure_parameters(
    lambda: Node,
    source: &str,
    bounds: &BTreeMap<String, String>,
) -> Option<Vec<Option<String>>> {
    let mut holder = bounded_parent(lambda)?;
    let mut tuple_slot: Option<(usize, usize)> = None;
    let mut child = lambda;
    if holder.kind() == "tuple_expression" {
        let mut position = None;
        let mut count = 0usize;
        for index in 0..holder.child_count() {
            let Some(item) = holder.child(index) else {
                continue;
            };
            if holder.field_name_for_child(index as u32) != Some("value") {
                continue;
            }
            if item.id() == lambda.id() {
                position = Some(count);
            }
            count += 1;
        }
        tuple_slot = Some((position?, count));
        child = holder;
        holder = bounded_parent(holder)?;
    }
    let (declaration, in_array) = if holder.kind() == "array_literal" {
        (bounded_parent(holder)?, true)
    } else {
        (holder, false)
    };
    if declaration.kind() != "property_declaration" {
        return None;
    }
    let value_node = if in_array { holder } else { child };
    if declaration.child_by_field_name("value")?.id() != value_node.id() {
        return None;
    }
    // One declared name with one written type; `let a: T, b: U = …` is not read.
    let mut annotations = Vec::new();
    let mut patterns = 0usize;
    for index in 0..declaration.child_count() {
        let Some(item) = declaration.child(index) else {
            continue;
        };
        match (declaration.field_name_for_child(index as u32), item.kind()) {
            (Some("name"), "pattern") => patterns += 1,
            (_, "type_annotation") => annotations.push(item),
            _ => {}
        }
    }
    if patterns != 1 || annotations.len() != 1 {
        return None;
    }
    let mut cursor = annotations[0].walk();
    let mut written = annotations[0].named_children(&mut cursor).next()?;
    written = unwrap_type_wrappers(written)?;
    if in_array {
        if written.kind() != "array_type" {
            return None;
        }
        let mut inner = written.walk();
        written = unwrap_type_wrappers(written.named_children(&mut inner).next()?)?;
    }
    if let Some((position, count)) = tuple_slot {
        if written.kind() != "tuple_type" {
            return None;
        }
        let mut items = written.walk();
        let elements: Vec<Node> = written
            .named_children(&mut items)
            .filter(|item| item.kind() == "tuple_type_item")
            .collect();
        if elements.len() != count {
            return None;
        }
        let mut inner = elements[position].walk();
        let element = elements[position]
            .named_children(&mut inner)
            .filter(|node| node.kind() != "simple_identifier")
            .last()?;
        written = unwrap_type_wrappers(element)?;
    }
    // `((T) -> Void)?` parses the parenthesised function as a one-item tuple.
    for _ in 0..=TYPE_NESTING_MAX_DEPTH {
        if written.kind() != "tuple_type" || written.named_child_count() != 1 {
            break;
        }
        let item = written.named_child(0)?;
        if item.kind() != "tuple_type_item" {
            break;
        }
        let mut inner = item.walk();
        let Some(next) = item
            .named_children(&mut inner)
            .filter(|node| node.kind() != "simple_identifier")
            .last()
        else {
            break;
        };
        written = unwrap_type_wrappers(next)?;
    }
    if written.kind() != "function_type" {
        return None;
    }
    let params = written.child_by_field_name("params")?;
    if params.kind() != "tuple_type" {
        return None;
    }
    let mut cursor = params.walk();
    let mut out = Vec::new();
    for item in params.named_children(&mut cursor) {
        if item.kind() != "tuple_type_item" {
            return None;
        }
        let mut inner = item.walk();
        let ty = item
            .named_children(&mut inner)
            .filter(|node| node.kind() != "simple_identifier")
            .last()
            .and_then(|node| normalize(node, source, bounds));
        out.push(ty);
    }
    Some(out)
}

/// `((T) -> Void)?` → the function type inside: optionals and parentheses.
fn unwrap_type_wrappers(node: Node) -> Option<Node> {
    let mut node = node;
    for _ in 0..=TYPE_NESTING_MAX_DEPTH {
        if !matches!(node.kind(), "optional_type" | "parenthesized_type") {
            return Some(node);
        }
        let mut cursor = node.walk();
        node = node.named_children(&mut cursor).next()?;
    }
    note_nesting_cap("swiftscope::unwrap_type_wrappers");
    None
}

/// The stored properties of the type `scope` is a member of whose type is a
/// function: `let onDone: () -> Void`. `onDone()` written in a method of that
/// type calls the property — implicit `self` — never a free function that
/// happens to share the name, so the name is bound here like a local. The
/// scope's own parameters and locals shadow it, because they are inserted first.
fn function_typed_members(scope: Node, source: &str) -> Vec<String> {
    let mut owner = bounded_parent(scope);
    while let Some(node) = owner {
        if node.kind() == "class_declaration" {
            break;
        }
        if node.kind() == "function_declaration" || node.kind() == "source_file" {
            return Vec::new();
        }
        owner = bounded_parent(node);
    }
    let Some(body) = owner.and_then(|node| node.child_by_field_name("body")) else {
        return Vec::new();
    };
    let mut names = Vec::new();
    let mut cursor = body.walk();
    for member in body.named_children(&mut cursor) {
        if member.kind() != "property_declaration" {
            continue;
        }
        let mut inner = member.walk();
        let function_typed = member.named_children(&mut inner).any(|child| {
            child.kind() == "type_annotation" && {
                let mut types = child.walk();
                let found = child
                    .named_children(&mut types)
                    .any(|ty| is_function_type(ty));
                found
            }
        });
        if function_typed {
            if let Some(pattern) = member.child_by_field_name("name") {
                collect_bound(pattern, source, &mut names);
            }
        }
    }
    names
}

/// `() -> Void`, `((Int) -> Int)?`, `(() -> Void)!`.
fn is_function_type(node: Node) -> bool {
    is_function_type_within(node, 0)
}

/// [`is_function_type`], `depth` wrappers down. tree-sitter-swift 0.7 cannot
/// nest an optional directly in an optional, so this stops within two levels
/// today; the bound keeps that a property of this function rather than of the
/// grammar version.
fn is_function_type_within(node: Node, depth: usize) -> bool {
    if depth > TYPE_NESTING_MAX_DEPTH {
        note_nesting_cap("swiftscope::is_function_type");
        return false;
    }
    match node.kind() {
        "function_type" => true,
        "optional_type" | "implicitly_unwrapped_type" | "parenthesized_type" => {
            let mut cursor = node.walk();
            let found = node
                .named_children(&mut cursor)
                .any(|child| is_function_type_within(child, depth + 1));
            found
        }
        _ => false,
    }
}

/// `T` → its protocol bound, for every generic parameter the declaration lists
/// and for each `where T: P` clause.
fn generic_bounds(scope: Node, source: &str) -> BTreeMap<String, String> {
    let mut bounds = BTreeMap::new();
    let mut cursor = scope.walk();
    for child in scope.named_children(&mut cursor) {
        match child.kind() {
            "type_parameters" => {
                let mut params = child.walk();
                for param in child.named_children(&mut params) {
                    if param.kind() != "type_parameter" {
                        continue;
                    }
                    let mut name = None;
                    let mut bound = None;
                    let mut inner = param.walk();
                    for item in param.named_children(&mut inner) {
                        match item.kind() {
                            "type_identifier" if name.is_none() => {
                                name = Some(get_node_text(item, source));
                            }
                            "user_type" => bound = user_type_name(item, source),
                            _ => {}
                        }
                    }
                    if let (Some(name), Some(bound)) = (name, bound) {
                        bounds.insert(name, bound);
                    }
                }
            }
            "type_constraints" => {
                let mut items = child.walk();
                for constraint in child.named_children(&mut items) {
                    let mut inner = constraint.walk();
                    let mut name = None;
                    let mut bound = None;
                    for item in constraint.named_children(&mut inner) {
                        match item.kind() {
                            "identifier" | "type_identifier" | "user_type" if name.is_none() => {
                                name = Some(get_node_text(item, source));
                            }
                            "user_type" => bound = user_type_name(item, source),
                            _ => {}
                        }
                    }
                    if let (Some(name), Some(bound)) = (name, bound) {
                        bounds.insert(name, bound);
                    }
                }
            }
            _ => {}
        }
    }
    bounds
}

/// Whether a `user_type` is one bare name, not `Outer.Inner`.
fn single_segment(node: Node) -> bool {
    let mut cursor = node.walk();
    let segments = node
        .named_children(&mut cursor)
        .filter(|child| child.kind() == "type_identifier")
        .count();
    segments == 1
}

fn user_type_name(node: Node, source: &str) -> Option<String> {
    let mut cursor = node.walk();
    let name = node
        .named_children(&mut cursor)
        .filter(|child| child.kind() == "type_identifier")
        .last()
        .map(|child| get_node_text(child, source));
    name
}

/// The type a written type node names: a nominal name, `[Name]` for an array,
/// `None` for anything a method cannot be dispatched on (functions, tuples,
/// dictionaries). Optionals are unwrapped — `m?.bump()` and `guard let` both
/// reach the wrapped type — and a generic parameter is its protocol bound.
fn normalize(node: Node, source: &str, bounds: &BTreeMap<String, String>) -> Option<String> {
    normalize_within(node, source, bounds, 0, true)
}

/// [`normalize`], `depth` wrappers down. Each `[` and each optional is one
/// level, and a written type of 20,000 nested `[` aborted the process on a
/// 256 KiB stack, so past [`TYPE_NESTING_MAX_DEPTH`] the type is not read and
/// the file says so. `resolve` reads a name this file declares with `typealias`
/// as what it stands for; the alias index itself reads its targets without it.
fn normalize_within(
    node: Node,
    source: &str,
    bounds: &BTreeMap<String, String>,
    depth: usize,
    resolve: bool,
) -> Option<String> {
    if depth > TYPE_NESTING_MAX_DEPTH {
        note_nesting_cap("swiftscope::normalize");
        return None;
    }
    let normalize = |child: Node| normalize_within(child, source, bounds, depth + 1, resolve);
    match node.kind() {
        "user_type" => {
            let name = user_type_name(node, source)?;
            if resolve && single_segment(node) {
                if let Some(target) = alias_target(node, source, &name) {
                    return Some(target);
                }
            }
            // `Array<T>` is `[T]`: one type argument, itself a written type.
            if name == "Array" {
                let mut cursor = node.walk();
                let arguments = node
                    .named_children(&mut cursor)
                    .find(|child| child.kind() == "type_arguments")?;
                let mut inner = arguments.walk();
                let mut items = arguments.named_children(&mut inner);
                let only = items.next()?;
                if items.next().is_some() {
                    return None;
                }
                return Some(format!("[{}]", normalize(only)?));
            }
            Some(bounds.get(&name).cloned().unwrap_or(name))
        }
        "optional_type" | "implicitly_unwrapped_type" => {
            let mut cursor = node.walk();
            let inner = node.named_children(&mut cursor).find_map(normalize)?;
            // An optional collection stays marked: `Optional.map` is not the
            // collection's `map`. A wrapped nominal type is still the type.
            Some(if inner.starts_with('[') && !inner.ends_with('?') {
                format!("{inner}?")
            } else {
                inner
            })
        }
        "opaque_type" | "existential_type" | "parenthesized_type" => {
            let mut cursor = node.walk();
            let inner = node.named_children(&mut cursor).find_map(normalize);
            inner
        }
        "array_type" => {
            let mut cursor = node.walk();
            let element = node.named_children(&mut cursor).find_map(normalize)?;
            Some(format!("[{element}]"))
        }
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn bound(source: &str, function: &str) -> Bindings {
        // Node ids are arena addresses, so every parse starts from a clean cache.
        reset();
        let mut parser = tree_sitter::Parser::new();
        parser
            .set_language(&tree_sitter_swift::LANGUAGE.into())
            .expect("swift grammar");
        let tree = parser.parse(source, None).expect("parse");
        let mut stack = vec![tree.root_node()];
        while let Some(node) = stack.pop() {
            if node.kind() == "function_declaration"
                && node
                    .child_by_field_name("name")
                    .is_some_and(|name| get_node_text(name, source) == function)
            {
                return (*bindings(node, source)).clone();
            }
            let mut cursor = node.walk();
            for child in node.named_children(&mut cursor) {
                stack.push(child);
            }
        }
        panic!("no function {function}");
    }

    /// The bindings of the `nth` closure literal in `source`, in source order.
    fn lambda_bound(source: &str, nth: usize) -> Bindings {
        reset();
        let mut parser = tree_sitter::Parser::new();
        parser
            .set_language(&tree_sitter_swift::LANGUAGE.into())
            .expect("swift grammar");
        let tree = parser.parse(source, None).expect("parse");
        let mut lambdas = Vec::new();
        let mut stack = vec![tree.root_node()];
        while let Some(node) = stack.pop() {
            if node.kind() == "lambda_literal" {
                lambdas.push(node);
            }
            let mut cursor = node.walk();
            stack.extend(node.named_children(&mut cursor));
        }
        lambdas.sort_by_key(|node| node.start_byte());
        (*bindings(lambdas[nth], source)).clone()
    }

    fn ty_of(found: &Bindings, name: &str) -> Option<String> {
        found.get(name).and_then(|b| b.ty.clone())
    }

    const PARCEL: &str = "struct Parcel { func weight() -> Int { 1 } }\n";

    #[test]
    fn a_closure_over_a_stored_collection_is_typed_by_its_element() {
        let source = format!(
            "{PARCEL}struct Crate {{ var items: [Parcel]\n func total() {{ _ = items.map {{ $0.weight() }} }} }}"
        );
        assert_eq!(
            ty_of(&lambda_bound(&source, 0), "$0"),
            Some("Parcel".to_string())
        );
        // `self.items` names the same member.
        let source = format!(
            "{PARCEL}struct Crate {{ var items: [Parcel]\n func total() {{ _ = self.items.map {{ $0.weight() }} }} }}"
        );
        assert_eq!(
            ty_of(&lambda_bound(&source, 0), "$0"),
            Some("Parcel".to_string())
        );
    }

    #[test]
    fn an_optional_or_generic_array_member_is_typed_and_a_dictionary_is_not() {
        for written in ["[Parcel]?", "Array<Parcel>", "[Parcel]!"] {
            let source = format!(
                "{PARCEL}struct Crate {{ var items: {written}\n func total() {{ _ = items?.map {{ $0.weight() }} }} }}"
            );
            assert_eq!(
                ty_of(&lambda_bound(&source, 0), "$0"),
                Some("Parcel".to_string()),
                "{written}"
            );
        }
        let source = format!(
            "{PARCEL}struct Crate {{ var items: [String: Parcel]\n func total() {{ _ = items.map {{ $0.weight() }} }} }}"
        );
        assert_eq!(ty_of(&lambda_bound(&source, 0), "$0"), None);
        let source = format!(
            "{PARCEL}struct Crate {{ var items: Set<Parcel>\n func total() {{ _ = items.map {{ $0.weight() }} }} }}"
        );
        assert_eq!(
            ty_of(&lambda_bound(&source, 0), "$0"),
            None,
            "a Set is not an array"
        );
    }

    #[test]
    fn a_member_with_no_written_type_types_nothing() {
        let source = format!(
            "{PARCEL}struct Crate {{ var items = [Parcel]()\n func total() {{ _ = items.map {{ $0.weight() }} }} }}"
        );
        assert_eq!(ty_of(&lambda_bound(&source, 0), "$0"), None);
    }

    #[test]
    fn a_local_or_parameter_named_like_the_member_shadows_it() {
        let local = format!(
            "{PARCEL}struct Crate {{ var items: [Parcel]\n func total() {{ let items = make(); _ = items.map {{ $0.weight() }} }} }}"
        );
        assert_eq!(
            ty_of(&lambda_bound(&local, 0), "$0"),
            None,
            "an untyped local wins"
        );
        let param = format!(
            "{PARCEL}struct Pod {{ func w() {{}} }}\nstruct Crate {{ var items: [Parcel]\n func total(items: [Pod]) {{ _ = items.map {{ $0.w() }} }} }}"
        );
        assert_eq!(
            ty_of(&lambda_bound(&param, 0), "$0"),
            Some("Pod".to_string())
        );
        let closure_param = format!(
            "{PARCEL}struct Crate {{ var items: [Parcel]\n func total() {{ _ = [1].map {{ items in items.map {{ $0.weight() }} }} }} }}"
        );
        assert_eq!(
            ty_of(&lambda_bound(&closure_param, 1), "$0"),
            None,
            "a closure parameter named like the member is untyped, not the member"
        );
    }

    #[test]
    fn a_member_declared_twice_or_on_a_twice_declared_type_is_not_read() {
        let two_extensions = format!(
            "{PARCEL}struct Pod {{ func w() {{}} }}\nstruct Crate {{ var items: [Parcel] }}\n\
             extension Crate {{ var items: [Pod] {{ [] }} }}\n\
             extension Crate {{ func total() {{ _ = items.map {{ $0.weight() }} }} }}"
        );
        assert_eq!(ty_of(&lambda_bound(&two_extensions, 0), "$0"), None);
        let nested_twice = format!(
            "{PARCEL}struct A {{ struct Crate {{ var items: [Parcel] }} }}\n\
             struct B {{ struct Crate {{ var items: [Parcel]\n func total() {{ _ = items.map {{ $0.weight() }} }} }} }}"
        );
        // Inside `B.Crate` the implicit member is `B.Crate`'s, whatever else is
        // called `Crate`; a use that only spells `Crate` cannot tell the two apart.
        assert_eq!(
            ty_of(&lambda_bound(&nested_twice, 0), "$0"),
            Some("Parcel".to_string())
        );
        let by_name = format!(
            "{PARCEL}struct A {{ struct Crate {{ var items: [Parcel] }} }}\n\
             struct B {{ struct Crate {{ var items: [Parcel] }} }}\n\
             func g(c: Crate) {{ _ = c.items.map {{ $0.weight() }} }}"
        );
        assert_eq!(ty_of(&lambda_bound(&by_name, 0), "$0"), None);
        // One declaration in a struct and its extension is not ambiguous.
        let extension = format!(
            "{PARCEL}struct Crate {{ var items: [Parcel] }}\n\
             extension Crate {{ func total() {{ _ = items.map {{ $0.weight() }} }} }}"
        );
        assert_eq!(
            ty_of(&lambda_bound(&extension, 0), "$0"),
            Some("Parcel".to_string())
        );
    }

    #[test]
    fn a_nested_type_is_read_by_name_only_from_inside_its_holder() {
        let outside = format!(
            "{PARCEL}struct Outer {{ struct Pouch {{ var items: [Parcel] }} }}\n\
             struct Host {{ func run(p: Pouch) {{ p.items.forEach {{ $0.weight() }} }} }}"
        );
        assert_eq!(ty_of(&lambda_bound(&outside, 0), "$0"), None);
        let inside = format!(
            "{PARCEL}struct Outer {{ struct Pouch {{ var items: [Parcel] }}\n\
             func run(p: Pouch) {{ p.items.forEach {{ $0.weight() }} }} }}"
        );
        assert_eq!(
            ty_of(&lambda_bound(&inside, 0), "$0"),
            Some("Parcel".to_string())
        );
        let top_level_and_nested = format!(
            "{PARCEL}struct Pouch {{ var items: [Parcel] }}\n\
             struct Outer {{ struct Pouch {{ var items: [Parcel] }} }}\n\
             func run(p: Pouch) {{ p.items.forEach {{ $0.weight() }} }}"
        );
        assert_eq!(ty_of(&lambda_bound(&top_level_and_nested, 0), "$0"), None);
        // An extension of a qualified name is not the namesake declared here.
        let extension = format!(
            "{PARCEL}struct B {{ struct Crate {{ var items: [Parcel] }} }}\n\
             extension A.Crate {{ func run() {{ items.forEach {{ $0.weight() }} }} }}"
        );
        assert_eq!(ty_of(&lambda_bound(&extension, 0), "$0"), None);
    }

    #[test]
    fn a_typealias_is_the_type_it_stands_for_and_never_the_type_of_that_name() {
        // `Entry` is another spelling of `Parcel`, not a top-level `Entry`.
        let source = format!(
            "{PARCEL}struct Host {{ typealias Entry = Parcel\n var items: [Entry]\n\
             func run() {{ items.forEach {{ $0.weight() }} }} }}"
        );
        assert_eq!(
            ty_of(&lambda_bound(&source, 0), "$0"),
            Some("Parcel".to_string())
        );
        let annotated = format!(
            "{PARCEL}typealias Step = Parcel\n\
             func f() {{ let g: (Step) -> Void = {{ $0.weight() }}; _ = g }}"
        );
        assert_eq!(
            ty_of(&lambda_bound(&annotated, 0), "$0"),
            Some("Parcel".to_string())
        );
        // A qualified spelling and a generic argument list are the alias's target.
        let generic = format!(
            "{PARCEL}typealias Step = Outer.Parcel\ntypealias Box = Holder<Int>\n\
             func f(_ s: Step, _ b: Box) {{ let g: (Step) -> Void = {{ $0.weight() }}; _ = g }}"
        );
        let found = bound(&generic, "f");
        assert_eq!(ty_of(&found, "s"), Some("Parcel".to_string()));
        assert_eq!(ty_of(&found, "b"), Some("Holder".to_string()));
    }

    #[test]
    fn a_typealias_the_file_cannot_pin_to_one_meaning_types_nothing() {
        let closure = |decls: &str, element: &str| {
            format!(
                "{PARCEL}{decls}struct Host {{ var items: [{element}]\n\
                 func run() {{ items.forEach {{ $0.weight() }} }} }}"
            )
        };
        // Declared twice, or beside a type of the same name.
        let twice = closure(
            "typealias Entry = Parcel\nextension X { typealias Entry = Pod }\n",
            "Entry",
        );
        assert_eq!(ty_of(&lambda_bound(&twice, 0), "$0"), None);
        let beside = closure("typealias Entry = Parcel\nstruct Entry {}\n", "Entry");
        assert_eq!(ty_of(&lambda_bound(&beside, 0), "$0"), None);
        // An alias of an alias, of a generic parameter, or inside a generic type or a protocol.
        let chain = closure("typealias Entry = Mid\ntypealias Mid = Parcel\n", "Entry");
        assert_eq!(ty_of(&lambda_bound(&chain, 0), "$0"), None);
        let generic = "struct Box<T> { typealias Entry = T\n var items: [Entry]\n\
                       func run() { items.forEach { $0.weight() } } }";
        assert_eq!(ty_of(&lambda_bound(generic, 0), "$0"), None);
        let in_generic = format!(
            "{PARCEL}struct Box<T> {{ typealias Entry = Parcel\n var items: [Entry]\n\
             func run() {{ items.forEach {{ $0.weight() }} }} }}"
        );
        assert_eq!(ty_of(&lambda_bound(&in_generic, 0), "$0"), None);
        let protocol = format!(
            "{PARCEL}protocol P {{ typealias Entry = Parcel }}\nstruct Host {{ var items: [Entry]\n\
             func run() {{ items.forEach {{ $0.weight() }} }} }}"
        );
        assert_eq!(ty_of(&lambda_bound(&protocol, 0), "$0"), None);
        // A nested alias means its name inside its own type only.
        let outside = format!(
            "{PARCEL}struct Host {{ typealias Entry = Parcel }}\nstruct Other {{ var items: [Entry]\n\
             func run() {{ items.forEach {{ $0.weight() }} }} }}"
        );
        assert_eq!(ty_of(&lambda_bound(&outside, 0), "$0"), None);
    }

    #[test]
    fn a_tuple_parameter_types_its_members_where_it_is_bound() {
        let source = "struct Case { func apply() {} }\n\
                      func f(_ open: (transform: Case, Int), _ plain: Int) {}";
        let found = bound(source, "f");
        assert_eq!(ty_of(&found, "open.transform"), Some("Case".to_string()));
        assert_eq!(ty_of(&found, "open.0"), Some("Case".to_string()));
        assert!(found.contains_key("open.1"), "a positional member is bound");
        assert_eq!(ty_of(&found, "open.1"), Some("Int".to_string()));
        assert_eq!(ty_of(&found, "plain"), Some("Int".to_string()));
        assert!(!found.contains_key("plain.transform"));
        // A function-typed member has no type a method can be dispatched on.
        let function = "func f(_ h: (run: (Int) -> Void, Case)) {}";
        assert_eq!(ty_of(&bound(function, "f"), "h.run"), None);
    }

    #[test]
    fn a_bare_call_of_a_method_this_type_declares_is_typed_by_its_written_return() {
        let own = format!(
            "{PARCEL}struct Host {{ func make() -> Parcel {{ Parcel() }}\n\
             func run() {{ let p = make(); p.weight() }} }}"
        );
        assert_eq!(ty_of(&bound(&own, "run"), "p"), Some("Parcel".to_string()));
        // A free function of the name is not what a call inside a type names.
        let free = format!(
            "{PARCEL}func make() -> Parcel {{ Parcel() }}\n\
             struct Host {{ func run() {{ let p = make(); p.weight() }} }}"
        );
        assert_eq!(ty_of(&bound(&free, "run"), "p"), None);
        // Declared by an in-file extension of the type is the same type.
        let extension = format!(
            "{PARCEL}struct Host {{ }}\nextension Host {{ func make() -> Parcel {{ Parcel() }} }}\n\
             extension Host {{ func run() {{ let p = make(); p.weight() }} }}"
        );
        assert_eq!(
            ty_of(&bound(&extension, "run"), "p"),
            Some("Parcel".to_string())
        );
        // Two declarations that disagree, a call with arguments, a method with
        // parameters, a nested type's own call, `Self` and a shadowing local type nothing.
        let disagree = format!(
            "{PARCEL}struct Pod {{}}\nstruct Host {{ func make() -> Parcel {{ Parcel() }}\n\
             func make() -> Pod {{ Pod() }}\n func run() {{ let p = make(); p.weight() }} }}"
        );
        assert_eq!(ty_of(&bound(&disagree, "run"), "p"), None);
        let arguments = format!(
            "{PARCEL}struct Host {{ func make() -> Parcel {{ Parcel() }}\n\
             func run() {{ let p = make(1); p.weight() }} }}"
        );
        assert_eq!(ty_of(&bound(&arguments, "run"), "p"), None);
        let parameters = format!(
            "{PARCEL}struct Host {{ func make(_ n: Int = 1) -> Parcel {{ Parcel() }}\n\
             func run() {{ let p = make(); p.weight() }} }}"
        );
        assert_eq!(ty_of(&bound(&parameters, "run"), "p"), None);
        let nested = format!(
            "{PARCEL}struct Outer {{ func make() -> Parcel {{ Parcel() }}\n\
             struct Inner {{ func run() {{ let p = make(); p.weight() }} }} }}"
        );
        assert_eq!(ty_of(&bound(&nested, "run"), "p"), None);
        let own_type = "struct Host { func make() -> Self { self }\n\
                        func run() { let p = make(); p.weight() } }";
        assert_eq!(ty_of(&bound(own_type, "run"), "p"), None);
        let shadow = format!(
            "{PARCEL}struct Pod {{}}\nstruct Host {{ func make() -> Parcel {{ Parcel() }}\n\
             func run(make: () -> Pod) {{ let p = make(); p.weight() }} }}"
        );
        assert_eq!(ty_of(&bound(&shadow, "run"), "p"), None);
        // The collection a method returns types an element closure the same way.
        let items = format!(
            "{PARCEL}struct Host {{ func load() -> [Parcel] {{ [] }}\n\
             func run() {{ let xs = load(); _ = xs.map {{ $0.weight() }} }} }}"
        );
        assert_eq!(
            ty_of(&lambda_bound(&items, 0), "$0"),
            Some("Parcel".to_string())
        );
    }

    #[test]
    fn a_method_call_is_typed_by_the_type_it_sits_in_not_a_namesake_elsewhere() {
        // Two types declare `make`; each call reads its own type's.
        let two = format!(
            "{PARCEL}struct Pod {{ func w() {{}} }}\n\
             struct A {{ func make() -> Parcel {{ Parcel() }}\n func run() {{ let p = make(); p.weight() }} }}\n\
             struct B {{ func make() -> Pod {{ Pod() }}\n func run() {{ let p = make(); p.w() }} }}"
        );
        reset();
        let mut parser = tree_sitter::Parser::new();
        parser
            .set_language(&tree_sitter_swift::LANGUAGE.into())
            .expect("swift grammar");
        let tree = parser.parse(two.as_str(), None).expect("parse");
        let mut runs = Vec::new();
        let mut stack = vec![tree.root_node()];
        while let Some(node) = stack.pop() {
            if node.kind() == "function_declaration"
                && node
                    .child_by_field_name("name")
                    .is_some_and(|name| get_node_text(name, &two) == "run")
            {
                runs.push(node);
            }
            let mut cursor = node.walk();
            stack.extend(node.named_children(&mut cursor));
        }
        runs.sort_by_key(|node| node.start_byte());
        assert_eq!(runs.len(), 2);
        let types: Vec<Option<String>> = runs
            .iter()
            .map(|run| ty_of(&bindings(*run, &two), "p"))
            .collect();
        assert_eq!(
            types,
            vec![Some("Parcel".to_string()), Some("Pod".to_string())]
        );
        // A closure inside the method, and an optional return, read the same method.
        let closure = format!(
            "{PARCEL}struct Host {{ func make() -> Parcel? {{ nil }}\n\
             func run() {{ _ = [1].map {{ _ in let p = make(); return p?.weight() }} }} }}"
        );
        assert_eq!(
            ty_of(&lambda_bound(&closure, 0), "p"),
            Some("Parcel".to_string())
        );
        // A protocol extension's own method types a call inside that extension.
        let protocol = format!(
            "{PARCEL}protocol P {{}}\nextension P {{ func make() -> Parcel {{ Parcel() }}\n\
             func run() {{ let p = make(); p.weight() }} }}"
        );
        assert_eq!(
            ty_of(&bound(&protocol, "run"), "p"),
            Some("Parcel".to_string())
        );
        // A method of another type with the name does not type a call in a type without it.
        let elsewhere = format!(
            "{PARCEL}struct Other {{ func make() -> Parcel {{ Parcel() }} }}\n\
             struct Host {{ func run() {{ let p = make(); p.weight() }} }}"
        );
        assert_eq!(ty_of(&bound(&elsewhere, "run"), "p"), None);
    }

    #[test]
    fn a_fetch_is_swiftdatas_only_on_a_model_context() {
        let map = |body: &str| {
            format!(
                "import SwiftData\nstruct Habit {{ func done() -> Bool {{ true }} }}\n\
                 struct V {{ @Environment(\\.modelContext) private var env\n\
                 @Environment(\\.other) private var other\n func f(_ ctx: ModelContext, _ x: Repo) {{ {body} }} }}"
            )
        };
        let typed = |body: &str| ty_of(&lambda_bound(&map(body), 0), "$0");
        let habit = Some("Habit".to_string());
        let call = |receiver: &str| {
            format!(
                "_ = ((try? {receiver}.fetch(FetchDescriptor<Habit>())) ?? []).map {{ $0.done() }}"
            )
        };
        assert_eq!(typed(&call("ctx")), habit);
        assert_eq!(typed(&call("env")), habit);
        assert_eq!(
            typed(&format!("let c = ModelContext(k); {}", call("c"))),
            habit
        );
        // A receiver the file does not show to be a ModelContext, and a property
        // wrapped with another key, is not read.
        assert_eq!(typed(&call("x")), None);
        assert_eq!(typed(&call("other")), None);
        assert_eq!(typed(&call("store.mainContext")), None);
        assert_eq!(typed(&format!("let c = make(); {}", call("c"))), None);
        // A file with its own `fetch` or `FetchDescriptor` is not asking SwiftData.
        let own = format!("struct FetchDescriptor<T> {{}}\n{}", map(&call("ctx")));
        assert_eq!(ty_of(&lambda_bound(&own, 0), "$0"), None);
        // An optional member after `?.` keeps its own `Optional.map`.
        let member = "struct Parcel { func weight() -> Int { 1 } }\n\
                      struct Holder { var maybe: [Parcel]? }\n\
                      func f(_ h: Holder?) { _ = h?.maybe.map { $0.weight() } }";
        assert_eq!(ty_of(&lambda_bound(member, 0), "$0"), None);
    }

    const FETCH: &str = "import SwiftData\nstruct Habit { func done() -> Bool { true } }\n";

    #[test]
    fn a_swiftdata_fetch_is_a_collection_of_its_descriptors_type() {
        let fetch = |expression: &str| {
            format!(
                "{FETCH}func f(_ context: ModelContext) {{ _ = {expression}.map {{ $0.done() }} }}"
            )
        };
        let typed = |expression: &str| ty_of(&lambda_bound(&fetch(expression), 0), "$0");
        let habit = Some("Habit".to_string());
        assert_eq!(
            typed("((try? context.fetch(FetchDescriptor<Habit>())) ?? [])"),
            habit
        );
        assert_eq!(
            typed("(try! context.fetch(FetchDescriptor<Habit>()))"),
            habit
        );
        assert_eq!(
            typed("(try context.fetch(FetchDescriptor<Habit>(predicate: nil)))"),
            habit
        );
        // `xs?.filter(…).map`: the `?.` earlier in the chain unwraps the optional.
        assert_eq!(
            typed("(try? context.fetch(FetchDescriptor<Habit>()))?.filter(\\.isActive)"),
            habit
        );
        assert_eq!(
            typed("((try? context.fetch(FetchDescriptor<Habit>())) ?? []).filter { $0.done() }.sorted { _, _ in true }"),
            habit
        );
        assert_eq!(
            typed("((try? context.fetch(FetchDescriptor<Habit>())) ?? []).filter { $0.done() }.prefix(8)"),
            habit
        );
        // Optional.map hands the closure the collection; a parenthesis ends the chain.
        assert_eq!(
            typed("(try? context.fetch(FetchDescriptor<Habit>()))"),
            None
        );
        assert_eq!(
            typed("((try? context.fetch(FetchDescriptor<Habit>()))?.filter(\\.isActive))"),
            None
        );
    }

    #[test]
    fn a_fetch_is_read_only_where_swiftdata_is_imported_and_the_shape_is_exact() {
        let map = |imports: &str, expression: &str| {
            format!(
                "{imports}struct Habit {{ func done() -> Bool {{ true }} }}\n\
                 func f(_ context: ModelContext) {{ _ = {expression}.map {{ $0.done() }} }}"
            )
        };
        let typed = |source: String| ty_of(&lambda_bound(&source, 0), "$0");
        let call = "((try? context.fetch(FetchDescriptor<Habit>())) ?? [])";
        assert_eq!(
            typed(map("import SwiftData\n", call)),
            Some("Habit".to_string())
        );
        assert_eq!(
            typed(map("import Foundation\n", call)),
            None,
            "no SwiftData import"
        );
        assert_eq!(typed(map("", call)), None);
        // Another descriptor type, two arguments, a variable descriptor and a
        // generic argument that is itself a collection are not read.
        for other in [
            "((try? context.fetch(Descriptor<Habit>())) ?? [])",
            "((try? context.fetch(FetchDescriptor<Habit>(), 1)) ?? [])",
            "((try? context.fetch(descriptor)) ?? [])",
            "((try? context.fetch(FetchDescriptor<[Habit]>())) ?? [])",
            "((try? context.fetch(FetchDescriptor<Habit?>())) ?? [])",
            "((try? context.fetchCount(FetchDescriptor<Habit>())) ?? [])",
        ] {
            assert_eq!(typed(map("import SwiftData\n", other)), None, "{other}");
        }
        // `filter` keeps the elements of a collection it can type, and no other.
        let untyped = "func f(_ xs: Seq) { _ = xs.filter { true }.map { $0.done() } }";
        assert_eq!(ty_of(&lambda_bound(untyped, 1), "$0"), None);
        // An alias of the descriptor's type is not read as that type.
        let alias = "import SwiftData\ntypealias Habit = Other\nstruct Habit2 {}\n\
                     func f(_ c: ModelContext) { _ = ((try? c.fetch(FetchDescriptor<Habit>())) ?? []).map { $0.done() } }";
        assert_eq!(
            ty_of(&lambda_bound(alias, 0), "$0"),
            Some("Other".to_string())
        );
    }

    /// Whether the `nth` identifier spelled `name` in `source` sits in the
    /// initializer of the binding that introduces `name`.
    fn in_own_initializer(source: &str, name: &str, nth: usize) -> bool {
        reset();
        let mut parser = tree_sitter::Parser::new();
        parser
            .set_language(&tree_sitter_swift::LANGUAGE.into())
            .expect("swift grammar");
        let tree = parser.parse(source, None).expect("parse");
        let mut found = Vec::new();
        let mut stack = vec![tree.root_node()];
        while let Some(node) = stack.pop() {
            if node.kind() == "simple_identifier" && get_node_text(node, source) == name {
                found.push(node);
            }
            let mut cursor = node.walk();
            stack.extend(node.named_children(&mut cursor));
        }
        found.sort_by_key(|node| node.start_byte());
        use_is_in_own_initializer(found[nth], source, name)
    }

    #[test]
    fn a_name_is_not_in_scope_in_its_own_initializer() {
        // `task` at index 1 is the use; 0 is the binding.
        assert!(in_own_initializer(
            "func f() { if let task = task(from: 1) { _ = task } }",
            "task",
            1
        ));
        assert!(in_own_initializer(
            "func f() { guard let task = task(from: 1) else { return } }",
            "task",
            1
        ));
        assert!(in_own_initializer(
            "func f() { let task = task(from: 1) }",
            "task",
            1
        ));
        assert!(in_own_initializer(
            "func f() { while let task = task() { } }",
            "task",
            1
        ));
        assert!(in_own_initializer(
            "func f() { if let a = g(), let task = task(from: a) { } }",
            "task",
            1
        ));
        // The body of the `if`, a later statement, another name and a closure are not.
        assert!(!in_own_initializer(
            "func f() { if let task = other() { task.go() } }",
            "task",
            1
        ));
        assert!(!in_own_initializer(
            "func f() { let task = other(); task.go() }",
            "task",
            1
        ));
        assert!(!in_own_initializer(
            "func f() { if let t = task(from: 1) { _ = t } }",
            "task",
            0
        ));
        assert!(!in_own_initializer(
            "func f() { let task = xs.map { task($0) } }",
            "task",
            1
        ));
        assert!(!in_own_initializer(
            "func f() { if let a = task(), let task = g() { } }",
            "task",
            0
        ));
        // `guard let s, s.ok` has no initializer: the test after it sees the new `s`.
        assert!(!in_own_initializer(
            "func f() { guard let session, session.ok else { return } }",
            "session",
            1
        ));
        assert!(!in_own_initializer(
            "func f() { if let session, session.ok == 1 { } }",
            "session",
            1
        ));
        assert!(in_own_initializer(
            "func f() { guard let session = session(), session.ok else { return } }",
            "session",
            1
        ));
    }

    #[test]
    fn declared_aliases_lists_every_typealias_including_member_ones() {
        let source = "typealias Step = Parcel\nextension Host { typealias Entry = Item }\n\
             struct Host { typealias Row = Item }";
        reset();
        let mut parser = tree_sitter::Parser::new();
        parser
            .set_language(&tree_sitter_swift::LANGUAGE.into())
            .expect("swift grammar");
        let tree = parser.parse(source, None).expect("parse");
        assert_eq!(
            declared_aliases(tree.root_node(), source),
            vec!["Entry".to_string(), "Row".to_string(), "Step".to_string()]
        );
    }

    #[test]
    fn optional_map_is_not_an_element_closure() {
        let member = format!(
            "{PARCEL}struct Host {{ var items: [Parcel]?\n func run() {{ _ = items.map {{ $0.weight() }} }} }}"
        );
        assert_eq!(ty_of(&lambda_bound(&member, 0), "$0"), None);
        let try_local = format!(
            "{PARCEL}func load() throws -> [Parcel] {{ [] }}\n\
             func f() {{ let xs = try? load(); _ = xs.map {{ $0.weight() }} }}"
        );
        assert_eq!(ty_of(&lambda_bound(&try_local, 0), "$0"), None);
        let try_paren = format!(
            "{PARCEL}func load() throws -> [Parcel] {{ [] }}\n\
             func f() {{ _ = (try? load()).map {{ $0.weight() }} }}"
        );
        assert_eq!(ty_of(&lambda_bound(&try_paren, 0), "$0"), None);
        // Chaining, `?? []` and an `if let` unwrap reach the elements.
        let chained = format!(
            "{PARCEL}func load() throws -> [Parcel] {{ [] }}\n\
             func f() {{ let xs = try? load(); _ = xs?.map {{ $0.weight() }} }}"
        );
        assert_eq!(
            ty_of(&lambda_bound(&chained, 0), "$0"),
            Some("Parcel".to_string())
        );
        let unwrapped = format!(
            "{PARCEL}func f(xs: [Parcel]?) {{ if let ys = xs {{ _ = ys.map {{ $0.weight() }} }} }}"
        );
        assert_eq!(
            ty_of(&lambda_bound(&unwrapped, 0), "$0"),
            Some("Parcel".to_string())
        );
    }

    #[test]
    fn a_bare_call_inside_a_type_or_with_arguments_names_no_return_type() {
        // Inside a type the call may be a method this file cannot see.
        let inside = format!(
            "{PARCEL}func load() -> [Parcel] {{ [] }}\n\
             struct Host {{ func run() {{ let xs = load(); _ = xs.map {{ $0.weight() }} }} }}"
        );
        assert_eq!(ty_of(&lambda_bound(&inside, 0), "$0"), None);
        // A call with arguments may be an overload declared elsewhere.
        let arguments = format!(
            "{PARCEL}func load() -> [Parcel] {{ [] }}\n\
             func f() {{ let xs = load(1); _ = xs.map {{ $0.weight() }} }}"
        );
        assert_eq!(ty_of(&lambda_bound(&arguments, 0), "$0"), None);
        // A function with parameters types no bare call of its name.
        let parameters = format!(
            "{PARCEL}func load(_ n: Int = 1) -> [Parcel] {{ [] }}\n\
             func f() {{ let xs = load(); _ = xs.map {{ $0.weight() }} }}"
        );
        assert_eq!(ty_of(&lambda_bound(&parameters, 0), "$0"), None);
    }

    #[test]
    fn a_computed_property_getter_is_a_scope_for_lookup() {
        let getter = format!(
            "{PARCEL}struct Crate {{ var items: [Parcel]\n var heavy: Int {{ items.filter {{ $0.weight() > 1 }}.count }} }}"
        );
        assert_eq!(
            ty_of(&lambda_bound(&getter, 0), "$0"),
            Some("Parcel".to_string())
        );
        let shadowed = format!(
            "{PARCEL}struct Crate {{ var items: [Parcel]\n var heavy: Int {{ let items = make(); return items.filter {{ $0.weight() > 1 }}.count }} }}"
        );
        assert_eq!(
            ty_of(&lambda_bound(&shadowed, 0), "$0"),
            None,
            "a local of the getter shadows the member"
        );
    }

    #[test]
    fn a_nested_closures_dollar_zero_is_its_own() {
        let source = "struct Part { func m() {} }\n\
             struct Parcel { var parts: [Part] }\n\
             struct Crate { var items: [Parcel]\n\
             func f() { _ = items.map { $0.parts.map { $0.m() } } } }";
        assert_eq!(
            ty_of(&lambda_bound(source, 0), "$0"),
            Some("Parcel".to_string())
        );
        assert_eq!(
            ty_of(&lambda_bound(source, 1), "$0"),
            Some("Part".to_string()),
            "the inner $0 walks `parts`, not the outer collection"
        );
    }

    #[test]
    fn a_member_reached_through_an_optional_chain_and_an_empty_default_is_typed() {
        let source = "struct Parcel { func weight() -> Int { 1 } }\n\
             struct Pouch { var items: [Parcel] }\n\
             struct Crate { func f(_ opt: Pouch?) { _ = (opt?.items ?? []).filter { $0.weight() > 1 } } }";
        assert_eq!(
            ty_of(&lambda_bound(source, 0), "$0"),
            Some("Parcel".to_string())
        );
        for default in ["fallback()", "[Parcel()]", "other"] {
            let source = format!(
                "struct Parcel {{ func weight() -> Int {{ 1 }} }}\n\
                 struct Pouch {{ var items: [Parcel] }}\n\
                 struct Crate {{ func f(_ opt: Pouch?, other: [Parcel]) {{ _ = (opt?.items ?? {default}).filter {{ $0.weight() > 1 }} }} }}"
            );
            assert_eq!(
                ty_of(&lambda_bound(&source, 0), "$0"),
                None,
                "a default that is not `[]` may be another type: {default}"
            );
        }
        // Pouch declared in another file: nothing here says what `items` is.
        let elsewhere = "struct Crate { func f(_ opt: Pouch?) { _ = (opt?.items ?? []).filter { $0.weight() > 1 } } }";
        assert_eq!(ty_of(&lambda_bound(elsewhere, 0), "$0"), None);
    }

    #[test]
    fn a_local_initialised_by_a_function_with_a_written_collection_return_is_typed() {
        let source = "struct Parcel { func weight() -> Int { 1 } }\n\
             func loadItems() -> [Parcel] { [] }\n\
             func f() { let xs = loadItems(); _ = xs.map { $0.weight() } }";
        assert_eq!(
            ty_of(&lambda_bound(source, 0), "$0"),
            Some("Parcel".to_string())
        );
        let through_try = "struct Parcel { func weight() -> Int { 1 } }\n\
             func loadItems() throws -> [Parcel] { [] }\n\
             func f() { let xs = (try? loadItems()) ?? []; _ = xs.map { $0.weight() } }";
        assert_eq!(
            ty_of(&lambda_bound(through_try, 0), "$0"),
            Some("Parcel".to_string())
        );
        // `try?` of a non-collection types no collection.
        let scalar = "struct Parcel { func weight() -> Int { 1 } }\n\
             func loadOne() throws -> Parcel { Parcel() }\n\
             func f() { let xs = try? loadOne(); _ = xs.map { $0.weight() } }";
        assert_eq!(ty_of(&lambda_bound(scalar, 0), "$0"), None);
        // A function the file declares twice with different returns abstains.
        let overloaded = "struct Parcel { func weight() -> Int { 1 } }\n\
             func loadItems() -> [Parcel] { [] }\n\
             func loadItems(_ n: Int) -> [Int] { [] }\n\
             func f() { let xs = loadItems(); _ = xs.map { $0.weight() } }";
        assert_eq!(ty_of(&lambda_bound(overloaded, 0), "$0"), None);
        // Inside a type a bare call names the type's own method, not the free
        // function beside it: `loadItems()` here is the `[Int]` one. (It abstained
        // before the type's own declaration was read; it never meant `[Parcel]`.)
        let method = "struct Parcel { func weight() -> Int { 1 } }\n\
             func loadItems() -> [Parcel] { [] }\n\
             struct Crate { func loadItems() -> [Int] { [] }\n\
             func f() { let xs = loadItems(); _ = xs.map { $0.weight() } } }";
        assert_eq!(
            ty_of(&lambda_bound(method, 0), "$0"),
            Some("Int".to_string())
        );
        // A local closure named like the function is what the call names.
        let local = "struct Parcel { func weight() -> Int { 1 } }\n\
             func loadItems() -> [Parcel] { [] }\n\
             func f(loadItems: () -> [Int]) { let xs = loadItems(); _ = xs.map { $0.weight() } }";
        assert_eq!(ty_of(&lambda_bound(local, 0), "$0"), None);
        // A generic return is not a type.
        let generic = "func loadItems<T>() -> [T] { [] }\n\
             func f() { let xs = loadItems(); _ = xs.map { $0.weight() } }";
        assert_eq!(ty_of(&lambda_bound(generic, 0), "$0"), None);
    }

    #[test]
    fn a_closure_is_typed_by_the_function_type_it_is_written_against() {
        let array = "struct Motor { func tick() {} }\n\
             func f() { let steps: [(Motor) -> Void] = [{ $0.tick() }, { $0.tick() }]; _ = steps }";
        assert_eq!(
            ty_of(&lambda_bound(array, 0), "$0"),
            Some("Motor".to_string())
        );
        assert_eq!(
            ty_of(&lambda_bound(array, 1), "$0"),
            Some("Motor".to_string())
        );
        let tuple = "struct Motor { func tick() {} }\n\
             func f() { let cases: [(name: String, apply: (Motor) -> Void)] = [(\"t\", { $0.tick() })]; _ = cases }";
        assert_eq!(
            ty_of(&lambda_bound(tuple, 0), "$0"),
            Some("Motor".to_string())
        );
        let direct = "struct Motor { func tick() {} }\n\
             func f() { let step: (Motor) -> Void = { $0.tick() }; _ = step }";
        assert_eq!(
            ty_of(&lambda_bound(direct, 0), "$0"),
            Some("Motor".to_string())
        );
        let optional = "struct Motor { func tick() {} }\n\
             func f() { let step: ((Motor) -> Void)? = { $0.tick() }; _ = step }";
        assert_eq!(
            ty_of(&lambda_bound(optional, 0), "$0"),
            Some("Motor".to_string())
        );
        let named = "struct Motor { func tick() {} }\n\
             func f() { let step: (Motor) -> Void = { m in m.tick() }; _ = step }";
        assert_eq!(
            ty_of(&lambda_bound(named, 0), "m"),
            Some("Motor".to_string())
        );
    }

    #[test]
    fn a_closure_of_several_parameters_types_each_shorthand_argument() {
        let source = "struct A {}\nstruct B {}\n\
             func f() { let step: (A, B) -> Void = { $0.x(); $1.y() }; _ = step }";
        let found = lambda_bound(source, 0);
        assert_eq!(ty_of(&found, "$0"), Some("A".to_string()));
        assert_eq!(ty_of(&found, "$1"), Some("B".to_string()));
        // Without the annotation `$1` is not bound at all, as before.
        let plain = "func f() { let step = { $0.x(); $1.y() }; _ = step }";
        let found = lambda_bound(plain, 0);
        assert!(!found.contains_key("$1"));
        // Explicit parameters take their types from the annotation only when the
        // arity agrees.
        let named = "struct A {}\nstruct B {}\n\
             func f() { let step: (A, B) -> Void = { a, b in a.x(); b.y() }; _ = step }";
        let found = lambda_bound(named, 0);
        assert_eq!(ty_of(&found, "a"), Some("A".to_string()));
        assert_eq!(ty_of(&found, "b"), Some("B".to_string()));
        let mismatch = "struct A {}\n\
             func f() { let step: (A) -> Void = { a, b in a.x(); b.y() }; _ = step }";
        let found = lambda_bound(mismatch, 0);
        assert_eq!(ty_of(&found, "a"), None);
    }

    #[test]
    fn a_closure_is_not_typed_by_an_annotation_it_is_not_an_element_of() {
        // A tuple element count that disagrees with the annotation.
        let arity = "struct Motor { func tick() {} }\n\
             func f() { let cases: [(name: String, apply: (Motor) -> Void)] = [(\"t\", 1, { $0.tick() })]; _ = cases }";
        assert_eq!(ty_of(&lambda_bound(arity, 0), "$0"), None);
        // A closure passed as an argument is not an element.
        let argument = "struct Motor { func tick() {} }\n\
             func f() { let steps: [(Motor) -> Void] = make({ $0.tick() }); _ = steps }";
        assert_eq!(ty_of(&lambda_bound(argument, 0), "$0"), None);
        // A dictionary of closures is not read.
        let dictionary = "struct Motor { func tick() {} }\n\
             func f() { let steps: [String: (Motor) -> Void] = [\"a\": { $0.tick() }]; _ = steps }";
        assert_eq!(ty_of(&lambda_bound(dictionary, 0), "$0"), None);
        // A closure with no annotation binds `$0` untyped.
        let none = "func f() { let step = { $0.tick() }; _ = step }";
        let found = lambda_bound(none, 0);
        assert!(found.contains_key("$0"));
        assert_eq!(ty_of(&found, "$0"), None);
    }

    #[test]
    fn an_if_let_of_a_method_call_is_typed_by_that_methods_written_return() {
        let source = "struct Cmd { func inverse() {} }\n\
             struct Hist { func pop() -> Cmd? { nil } }\n\
             final class Runner { var history = Hist()\n\
             func go() { if let u = history.pop() { u.inverse() } } }";
        assert_eq!(ty_of(&bound(source, "go"), "u"), Some("Cmd".to_string()));
        // A method overloaded on the type with different returns abstains, and
        // keeps the call shape for the resolver instead.
        let overloaded = "struct Cmd { func inverse() {} }\n\
             struct Hist { func pop() -> Cmd? { nil }\n func pop(_ n: Int) -> Int? { nil } }\n\
             final class Runner { var history = Hist()\n\
             func go() { if let u = history.pop() { u.inverse() } } }";
        let found = bound(overloaded, "go");
        assert_eq!(ty_of(&found, "u"), None);
        assert_eq!(
            found.get("u").and_then(|b| b.init.clone()),
            Some("history.pop".to_string())
        );
        // A receiver the file does not type leaves the shape and no type.
        let unknown = "func go(other: Other) { if let u = other.pop() { u.inverse() } }";
        let found = bound(unknown, "go");
        assert_eq!(ty_of(&found, "u"), None);
        assert_eq!(
            found.get("u").and_then(|b| b.init.clone()),
            Some("other.pop".to_string())
        );
        // A generic return is not a type.
        let generic = "final class Box<T> { func take() -> T? { nil } }\n\
             struct Cmd { func inverse() {} }\n\
             final class Runner { var box = Box<Cmd>()\n\
             func go() { if let u = box.take() { u.inverse() } } }";
        assert_eq!(ty_of(&bound(generic, "go"), "u"), None);
        // The same name bound by two `if let`s keeps no shape.
        let twice =
            "func go(a: A, b: B) { if let u = a.pop() { u.x() }\n if let u = b.pop() { u.y() } }";
        assert_eq!(
            bound(twice, "go").get("u").and_then(|b| b.init.clone()),
            None
        );
    }

    #[test]
    fn parameters_annotations_unwraps_and_loops_are_typed() {
        reset();
        let source = "func f(_ m: Meter, o: Meter?, xs: [Beta]) {\n\
            let g: Gauge = .zero\n\
            guard let q = o else { return }\n\
            if let o { o.x() }\n\
            for x in xs { x.run() }\n\
            let copy = m\n\
            let untyped = make()\n\
        }";
        let found = bound(source, "f");
        assert_eq!(
            found.get("m").and_then(|b| b.ty.clone()),
            Some("Meter".to_string())
        );
        assert_eq!(
            found.get("o").and_then(|b| b.ty.clone()),
            Some("Meter".to_string())
        );
        assert_eq!(
            found.get("xs").and_then(|b| b.ty.clone()),
            Some("[Beta]".to_string())
        );
        assert_eq!(
            found.get("g").and_then(|b| b.ty.clone()),
            Some("Gauge".to_string())
        );
        assert_eq!(
            found.get("q").and_then(|b| b.ty.clone()),
            Some("Meter".to_string())
        );
        assert_eq!(
            found.get("x").and_then(|b| b.ty.clone()),
            Some("Beta".to_string())
        );
        assert_eq!(
            found.get("copy").and_then(|b| b.ty.clone()),
            Some("Meter".to_string())
        );
        let untyped = found.get("untyped").expect("bound");
        assert_eq!(untyped.ty, None, "a value with no written type is untyped");
        assert_eq!(untyped.init.as_deref(), Some("make"));
    }

    #[test]
    fn a_generic_parameter_is_its_protocol_bound() {
        reset();
        let found = bound("func loud<T: Greeter>(_ xs: [T], one: T) {}", "loud");
        assert_eq!(
            found.get("xs").and_then(|b| b.ty.clone()),
            Some("[Greeter]".to_string())
        );
        assert_eq!(
            found.get("one").and_then(|b| b.ty.clone()),
            Some("Greeter".to_string())
        );
    }

    #[test]
    fn a_name_bound_twice_with_different_types_is_untyped() {
        reset();
        let found = bound(
            "func f() { let a: One = x(); if true { let a: Two = y() } }",
            "f",
        );
        assert_eq!(found.get("a").map(|b| b.ty.clone()), Some(None));
    }

    #[test]
    fn a_function_typed_member_is_a_name_its_methods_bind() {
        reset();
        let found = bound(
            "final class Job { let onDone: () -> Void; var count = 0; func finish() { onDone() } }",
            "finish",
        );
        assert!(found.contains_key("onDone"));
        assert!(
            !found.contains_key("count"),
            "an ordinary property is not a function value"
        );
    }

    #[test]
    fn array_types_are_read_to_the_nesting_cap_and_no_further() {
        let nested = |depth: usize| {
            format!(
                "func f(_ a: {}Beta{}) {{}}",
                "[".repeat(depth),
                "]".repeat(depth)
            )
        };
        for depth in [0, 1, 2, TYPE_NESTING_MAX_DEPTH] {
            reset();
            let found = bound(&nested(depth), "f");
            assert_eq!(
                found.get("a").and_then(|b| b.ty.clone()),
                Some(format!("{}Beta{}", "[".repeat(depth), "]".repeat(depth))),
                "depth {depth}"
            );
        }
        reset();
        let found = bound(&nested(TYPE_NESTING_MAX_DEPTH + 1), "f");
        assert_eq!(
            found.get("a").map(|b| b.ty.clone()),
            Some(None),
            "past the cap the name is bound and untyped"
        );
    }

    #[test]
    fn a_nested_function_is_a_local_name() {
        reset();
        let found = bound("func outer() { func helper() {} helper() }", "outer");
        assert!(found.contains_key("helper"));
    }
}
