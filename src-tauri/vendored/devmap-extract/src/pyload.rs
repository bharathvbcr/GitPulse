//! Python modules loaded **by file path**.
//!
//! ```python
//! spec = importlib.util.spec_from_file_location("x", ROOT / "scripts/x.py")
//! module = importlib.util.module_from_spec(spec)
//! spec.loader.exec_module(module)
//! module.run()
//! ```
//!
//! is how a script or a test reaches a file that is not on `sys.path`. No
//! `import` statement names `scripts/x.py`, so without this pass the loaded
//! file had no import edge from its loader and `module.run()` landed in the
//! unresolved ledger as `uninferred_receiver`: `impact run` answered "no
//! callers" for a function whose callers were in plain view.
//!
//! This pass records two facts, and nothing it cannot read literally:
//!
//! * **Which file is loaded.** The path expression is read for its trailing
//!   string-literal segments — `"x/y.py"`, `ROOT / "scripts/x.py"`,
//!   `Path(__file__).parent / "x.py"`, `os.path.join(HERE, "x.py")`,
//!   `HERE / "sub" / "x.py"` — joined after the last non-literal. A base
//!   written in terms of `__file__` is kept as an anchor (`parents[1]` is two
//!   directories up from the loader); any other base is recorded as unknown.
//!   A module-level constant bound exactly once, never declared `global`,
//!   to an anchor or to a whole `.py` path (`BUILDER = ROOT /
//!   "scripts/build_paper.py"`) is read through wherever it is used.
//!   An f-string, a concatenation, a non-literal tail, or a tail that does not
//!   end in `.py` records nothing.
//! * **Which local is the module.** `module_from_spec(spec)` (following the
//!   latest binding of `spec` in the same scope), `SourceFileLoader(...)
//!   .load_module()` (chained or through a loader variable),
//!   `imp.load_source(...)`, and a same-file loader function that returns such
//!   a handle (`def load(): ...; return module`, used as `load().fn()` or
//!   `m = load(); m.fn()`), or a *parameterised* one whose parameter is the
//!   path's tail (`def load(name, relative): ... ROOT / relative ...`),
//!   bound at each call site to the `.py` literal it passes for that
//!   parameter (`W21 = load("w21", "scripts/aws/analyse_wave21.py")`). A
//!   parameter rebound in the body, used twice, or used anywhere but the tail
//!   is not substituted. `runpy.run_path` is a dependency but returns the
//!   module's globals *dict*, so its result is never bound as a module.
//!
//! A handle that is rebound after its load, or bound to two different paths,
//! is not bound at all: which call sees which value is a flow question this
//! pass does not answer, and a wrong binding would hand a function callers it
//! does not have.
//!
//! Which indexed file the path names is decided by the resolver, never here:
//! the extraction cache keys on `(path, source)` alone, so nothing that
//! depends on the rest of the corpus can be baked into an extraction.

use crate::model::{ExtractedImport, PathLoad, PathLoadKind, Span};
use crate::treesitter::{enclosing_callable_qualified, walk_deadline_passed};
use std::collections::{BTreeMap, HashMap, HashSet};
use tree_sitter::Node;

/// The longest literal path accepted. A real path is a few dozen bytes; past
/// this the literal is data, not a location, and is refused rather than
/// copied.
const MAX_PATH_BYTES: usize = 4096;
/// The most path segments one expression may contribute. A `/` chain is
/// left-nested, so a 10,000-segment path is a 10,000-deep tree; the spine is
/// walked iteratively and refused past this count.
const MAX_SEGMENTS: usize = 256;
/// Recursion bound for the non-spine parts of a path expression — wrappers
/// such as `str(...)`, `os.path.dirname(...)`, `Path(...)`.
const MAX_EVAL_DEPTH: usize = 32;
/// Total nodes one path expression may cost, so breadth (`os.path.join` with
/// ten thousand arguments) is bounded as well as depth.
const MAX_EVAL_NODES: usize = 2048;
/// A callee longer than this is not one of the loader names.
const MAX_CALLEE_BYTES: usize = 128;
/// How much of the load statement is kept as the import's `raw_import`.
const MAX_RAW_CHARS: usize = 160;
/// Deadline check stride over the node walk.
const DEADLINE_STRIDE: usize = 256;
/// The most `sys.path` entries one file may contribute. Each import is
/// checked against every entry inserted before it, so this bounds that cost
/// per import; a file past it abstains rather than pay it.
const MAX_SEARCH_DIRECTORIES: usize = 64;

/// Every path load in a Python file, as imports the resolver can join.
///
/// Returns nothing — not a partial answer — if the walk deadline passes,
/// because a missed later rebinding is exactly what would turn an abstention
/// into a wrong binding.
pub(crate) fn python_path_loads(
    root: Node,
    source: &str,
    file_symbol_name: &str,
) -> Vec<ExtractedImport> {
    // The fast path. Every form below names one of these, so a file that
    // contains none of them — nearly every file — costs one scan.
    if ![
        "spec_from_file_location",
        "run_path",
        "SourceFileLoader",
        "load_source",
        "sys.path",
    ]
    .iter()
    .any(|needle| source.contains(needle))
    {
        return Vec::new();
    }
    let Some(Collected {
        events,
        parameters,
        globals,
    }) = collect_events(root, source, file_symbol_name)
    else {
        return Vec::new();
    };
    let mut pass = Pass::new(source, file_symbol_name, &events, parameters, globals);
    pass.compute_constants();
    pass.run(&BTreeMap::new());
    let loaders = pass.loader_functions();
    if !loaders.is_empty() {
        pass.reset();
        pass.run(&loaders);
    }
    pass.emit(&loaders)
}

/// The kinds of call this pass reads.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum LoadKind {
    SpecFromFileLocation,
    ModuleFromSpec,
    RunPath,
    SourceFileLoader,
    LoadSource,
}

impl LoadKind {
    /// Which argument carries the path: positional index and keyword.
    fn path_argument(self) -> Option<(usize, &'static str)> {
        match self {
            LoadKind::SpecFromFileLocation => Some((1, "location")),
            LoadKind::SourceFileLoader => Some((1, "path")),
            LoadKind::LoadSource => Some((1, "pathname")),
            LoadKind::RunPath => Some((0, "path_name")),
            LoadKind::ModuleFromSpec => None,
        }
    }
}

/// Classify a callee by its written text.
///
/// The qualifier must be the module that really defines the name, or absent
/// (`from importlib.util import spec_from_file_location`). An arbitrary
/// receiver — `self.run_path(...)` — is somebody else's method.
fn load_kind(callee: &str) -> Option<LoadKind> {
    if callee.len() > MAX_CALLEE_BYTES {
        return None;
    }
    let (qualifier, name) = callee.rsplit_once('.').unwrap_or(("", callee));
    let util = matches!(qualifier, "" | "importlib.util" | "util");
    match name {
        "spec_from_file_location" if util => Some(LoadKind::SpecFromFileLocation),
        "module_from_spec" if util => Some(LoadKind::ModuleFromSpec),
        "run_path" if matches!(qualifier, "" | "runpy") => Some(LoadKind::RunPath),
        "SourceFileLoader" if matches!(qualifier, "" | "importlib.machinery" | "machinery") => {
            Some(LoadKind::SourceFileLoader)
        }
        "load_source" if matches!(qualifier, "" | "imp") => Some(LoadKind::LoadSource),
        _ => None,
    }
}

/// Where a loaded file is, as far as the source says.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
struct Loc {
    path: String,
    anchor_up: Option<u32>,
}

/// A path as this pass knows it: a place, or — inside a parameterised loader
/// function — a place still waiting for the one parameter that is its tail.
#[derive(Debug, Clone, PartialEq, Eq)]
enum PathV {
    Fixed(Loc),
    /// Path steps whose **last** step is the only [`Item::Param`]. Filled in at
    /// each call site of the loader with that call's literal argument.
    Template(Vec<Item>),
}

/// The value a local holds, as far as this pass tracks values.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Val {
    Spec(PathV),
    Loader(PathV),
    Module(PathV),
    Other,
}

/// A same-file, module-level function that returns a path-loaded module.
#[derive(Debug, Clone, PartialEq, Eq)]
enum LoaderFn {
    /// Always the same file: `def supplement(): ... return module`.
    Fixed(Loc),
    /// `def load(name, relative): ... ROOT / relative ... return mod` — the
    /// file is whatever literal the call site passes for `param`.
    Param {
        steps: Vec<Item>,
        param: String,
        position: Option<usize>,
    },
}

/// A parameter of a function scope, with its positional index when a
/// positional argument can reach it.
#[derive(Debug, Clone)]
struct Param {
    name: String,
    position: Option<usize>,
}

/// One syntactic fact, in document order.
enum Event<'tree> {
    /// Names bound by one statement. `value` is `Some` only for a single
    /// identifier target whose value this pass may be able to read.
    Bind {
        node: Node<'tree>,
        names: Vec<String>,
        value: Option<Node<'tree>>,
        scope: Option<String>,
        in_class: bool,
        module_level_statement: bool,
    },
    Return {
        value: Node<'tree>,
        scope: Option<String>,
    },
    /// A call to one of the loader APIs, wherever it sits.
    Load {
        node: Node<'tree>,
        scope: Option<String>,
    },
    /// A call of a bare name with arguments — a candidate call of a
    /// parameterised loader function, whose file is a dependency even when
    /// the result is not bound.
    Call {
        node: Node<'tree>,
        scope: Option<String>,
    },
    /// A write to `sys.path`: a directory inserted (`insert`, `append`,
    /// `sys.path[:0] = [...]`), with the expression naming it, or anything
    /// else — `remove`, `pop`, reassignment, `del` — which leaves the search
    /// path unknowable and so `directory` is `None`. `scope` is the
    /// enclosing function's identity, `None` at module level and in a class
    /// body.
    SysPath {
        node: Node<'tree>,
        directory: Option<Node<'tree>>,
        module_level: bool,
        scope: Option<String>,
    },
}

/// The `sys.path` method a callee names, if any: `Some(Some(index))` for an
/// insert whose directory is positional argument `index`, `Some(None)` for
/// any other mutation.
fn sys_path_method(callee: &str) -> Option<Option<usize>> {
    match callee.strip_prefix("sys.path.")? {
        "insert" => Some(Some(1)),
        "append" => Some(Some(0)),
        "remove" | "pop" | "clear" | "extend" | "reverse" | "sort" | "__setitem__"
        | "__delitem__" | "__iadd__" => Some(None),
        _ => None,
    }
}

fn text<'s>(node: Node, source: &'s str) -> &'s str {
    source.get(node.start_byte()..node.end_byte()).unwrap_or("")
}

/// Identifiers a binding target introduces: `a`, `a, b`, `(a, [b, *c])`.
/// Attribute and subscript targets bind no name and contribute nothing.
fn target_names(node: Node, source: &str, out: &mut Vec<String>) {
    let mut stack = vec![node];
    while let Some(current) = stack.pop() {
        if out.len() > MAX_SEGMENTS {
            return;
        }
        match current.kind() {
            "identifier" => out.push(text(current, source).to_string()),
            "pattern_list"
            | "tuple_pattern"
            | "list_pattern"
            | "list_splat_pattern"
            | "parenthesized_expression"
            | "tuple"
            | "list"
            | "expression_list" => {
                let mut cursor = current.walk();
                for child in current.named_children(&mut cursor) {
                    stack.push(child);
                }
            }
            _ => {}
        }
    }
}

/// Names an import statement binds in its scope.
fn import_names(node: Node, source: &str, out: &mut Vec<String>) {
    let mut cursor = node.walk();
    let from = node.kind() == "import_from_statement";
    let module = node.child_by_field_name("module_name").map(|m| m.id());
    for child in node.named_children(&mut cursor) {
        if Some(child.id()) == module {
            continue;
        }
        match child.kind() {
            "aliased_import" => {
                if let Some(alias) = child.child_by_field_name("alias") {
                    out.push(text(alias, source).to_string());
                }
            }
            "dotted_name" => {
                let written = text(child, source);
                let bound = if from {
                    written.rsplit('.').next()
                } else {
                    written.split('.').next()
                };
                if let Some(bound) = bound {
                    out.push(bound.trim().to_string());
                }
            }
            _ => {}
        }
    }
}

/// Where a node sits, carried down the walk so no event pays a parent walk.
///
/// `Node::parent` re-descends from the root on every call, so asking each
/// event for its enclosing function made the pass cost O(events × depth) and
/// pushed a 3,000-load file past the extraction budget. The walk already
/// visits every ancestor first; it records what it saw.
#[derive(Clone, Copy)]
struct Context<'tree> {
    /// The function whose *body* contains the node. A default value or an
    /// annotation belongs to the enclosing scope, as Python evaluates it.
    function: Option<Node<'tree>>,
    /// Inside a class body with no function in between: a class attribute.
    in_class: bool,
    parent_is_module: bool,
    /// The node is the child of a statement that sits directly in the module.
    module_statement: bool,
}

/// The facts [`collect_events`] reads, the per-scope parameters the value
/// model needs for shadowing and substitution, and every name some function
/// declares `global` or `nonlocal` — a name that can be rebound from anywhere.
struct Collected<'tree> {
    events: Vec<Event<'tree>>,
    parameters: HashMap<Option<String>, Vec<Param>>,
    globals: HashSet<String>,
}

fn collect_events<'tree>(
    root: Node<'tree>,
    source: &str,
    file_symbol_name: &str,
) -> Option<Collected<'tree>> {
    // One `enclosing_callable_qualified` per function, not per event. Asked of
    // the function's body so the answer is that function's identity — the
    // string its calls record as `caller_symbol`.
    let mut scopes: HashMap<usize, Option<String>> = HashMap::new();
    let mut parameters: HashMap<Option<String>, Vec<Param>> = HashMap::new();
    let mut globals: HashSet<String> = HashSet::new();
    let mut scope_of = |context: &Context<'tree>| -> Option<String> {
        let function = context.function?;
        scopes
            .entry(function.id())
            .or_insert_with(|| {
                let scope = function
                    .child_by_field_name("body")
                    .and_then(|body| enclosing_callable_qualified(body, source, file_symbol_name));
                let mut params = Vec::new();
                if let Some(list) = function.child_by_field_name("parameters") {
                    let mut walker = list.walk();
                    // Positional indices stop at `*`, `*args` or `**kw`: a
                    // parameter after one of those is keyword-only.
                    let mut position = Some(0usize);
                    for parameter in list.named_children(&mut walker) {
                        let splat = matches!(
                            parameter.kind(),
                            "list_splat_pattern" | "dictionary_splat_pattern" | "keyword_separator"
                        );
                        if splat {
                            position = None;
                        }
                        let named = match parameter.kind() {
                            "identifier" => Some(parameter),
                            "positional_separator" | "keyword_separator" => None,
                            _ => parameter
                                .child_by_field_name("name")
                                .or_else(|| parameter.named_child(0)),
                        };
                        let mut names = Vec::new();
                        if let Some(named) = named {
                            target_names(named, source, &mut names);
                        }
                        for name in names {
                            params.push(Param {
                                name,
                                position: if splat { None } else { position },
                            });
                            position = position.map(|index| index + 1);
                        }
                    }
                }
                parameters.entry(scope.clone()).or_default().extend(params);
                scope
            })
            .clone()
    };
    let mut events = Vec::new();
    let mut stack = vec![(
        root,
        Context {
            function: None,
            in_class: false,
            parent_is_module: false,
            module_statement: false,
        },
    )];
    let mut visited = 0usize;
    while let Some((node, context)) = stack.pop() {
        visited += 1;
        if visited.is_multiple_of(DEADLINE_STRIDE) && walk_deadline_passed() {
            return None;
        }
        let bind = |names: Vec<String>,
                    value: Option<Node<'tree>>,
                    module_level_statement: bool,
                    scope: Option<String>| Event::Bind {
            node,
            names,
            value,
            scope,
            in_class: context.in_class,
            module_level_statement,
        };
        // Writes to `sys.path` other than through a method call.
        let module_level = context.function.is_none() && !context.in_class;
        match node.kind() {
            "assignment" | "augmented_assignment" => {
                if let Some(left) = node.child_by_field_name("left") {
                    let written = text(left, source);
                    if written == "sys.path"
                        || written.starts_with("sys.path[")
                        || written.starts_with("sys.path.")
                    {
                        // `sys.path[:0] = [a, b]` prepends; anything else —
                        // `sys.path = [...]`, `sys.path += [...]`,
                        // `sys.path[0] = x` — is a write this pass cannot
                        // follow.
                        let slice = left
                            .child_by_field_name("subscript")
                            .map(|slice| text(slice, source).replace(char::is_whitespace, ""));
                        let right = node.child_by_field_name("right");
                        let prepend = node.kind() == "assignment"
                            && left.kind() == "subscript"
                            && matches!(slice.as_deref(), Some(":0" | "0:0"))
                            && right.is_some_and(|right| right.kind() == "list");
                        let scope = scope_of(&context);
                        match right.filter(|_| prepend) {
                            Some(list) => {
                                let mut cursor = list.walk();
                                for element in list.named_children(&mut cursor) {
                                    events.push(Event::SysPath {
                                        node,
                                        directory: Some(element),
                                        module_level,
                                        scope: scope.clone(),
                                    });
                                }
                            }
                            None => events.push(Event::SysPath {
                                node,
                                directory: None,
                                module_level,
                                scope,
                            }),
                        }
                    }
                }
            }
            "delete_statement" if text(node, source).contains("sys.path") => {
                events.push(Event::SysPath {
                    node,
                    directory: None,
                    module_level,
                    scope: scope_of(&context),
                });
            }
            _ => {}
        }
        match node.kind() {
            "assignment" => {
                if let Some(left) = node.child_by_field_name("left") {
                    let mut names = Vec::new();
                    target_names(left, source, &mut names);
                    if !names.is_empty() {
                        let value = (left.kind() == "identifier")
                            .then(|| node.child_by_field_name("right"))
                            .flatten();
                        let scope = scope_of(&context);
                        events.push(bind(names, value, context.module_statement, scope));
                    }
                }
            }
            "named_expression" => {
                if let Some(name) = node.child_by_field_name("name") {
                    let scope = scope_of(&context);
                    events.push(bind(
                        vec![text(name, source).to_string()],
                        node.child_by_field_name("value"),
                        false,
                        scope,
                    ));
                }
            }
            "augmented_assignment" | "for_statement" | "delete_statement" | "as_pattern" => {
                let target = match node.kind() {
                    "augmented_assignment" | "for_statement" => node.child_by_field_name("left"),
                    "as_pattern" => node.child_by_field_name("alias"),
                    _ => Some(node),
                };
                let mut names = Vec::new();
                if let Some(target) = target {
                    if node.kind() == "delete_statement" {
                        let mut cursor = target.walk();
                        for child in target.named_children(&mut cursor) {
                            target_names(child, source, &mut names);
                        }
                    } else {
                        target_names(target, source, &mut names);
                        // `as_pattern_target` wraps the bound name.
                        if names.is_empty() {
                            let mut cursor = target.walk();
                            for child in target.named_children(&mut cursor) {
                                target_names(child, source, &mut names);
                            }
                        }
                    }
                }
                if !names.is_empty() {
                    let scope = scope_of(&context);
                    events.push(bind(names, None, false, scope));
                }
            }
            "import_statement" | "import_from_statement" => {
                let mut names = Vec::new();
                import_names(node, source, &mut names);
                if !names.is_empty() {
                    let scope = scope_of(&context);
                    events.push(bind(names, None, false, scope));
                }
            }
            "function_definition" | "class_definition" => {
                // `def mod(): ...` rebinds `mod` in the enclosing scope.
                if let Some(name) = node.child_by_field_name("name") {
                    let scope = scope_of(&context);
                    events.push(bind(
                        vec![text(name, source).to_string()],
                        None,
                        false,
                        scope,
                    ));
                }
            }
            "return_statement" => {
                if let Some(value) = node.named_child(0) {
                    events.push(Event::Return {
                        value,
                        scope: scope_of(&context),
                    });
                }
            }
            "call" => {
                if let Some(function) = node.child_by_field_name("function") {
                    let callee = text(function, source);
                    if let Some(insert) = sys_path_method(callee) {
                        let directory =
                            insert.and_then(|index| positional_arguments(node).get(index).copied());
                        events.push(Event::SysPath {
                            node,
                            directory,
                            module_level,
                            scope: scope_of(&context),
                        });
                    } else if load_kind(callee).is_some_and(|kind| kind != LoadKind::ModuleFromSpec)
                    {
                        events.push(Event::Load {
                            node,
                            scope: scope_of(&context),
                        });
                    } else if function.kind() == "identifier"
                        && node
                            .child_by_field_name("arguments")
                            .is_some_and(|arguments| arguments.named_child_count() > 0)
                    {
                        events.push(Event::Call {
                            node,
                            scope: scope_of(&context),
                        });
                    }
                }
            }
            "global_statement" | "nonlocal_statement" => {
                let mut cursor = node.walk();
                for name in node.named_children(&mut cursor) {
                    if name.kind() == "identifier" {
                        globals.insert(text(name, source).to_string());
                    }
                }
            }
            _ => {}
        }
        // Document order: push children last-first so they pop first-first.
        let body = matches!(node.kind(), "function_definition" | "class_definition")
            .then(|| node.child_by_field_name("body"))
            .flatten()
            .map(|body| body.id());
        let mark = stack.len();
        let mut cursor = node.walk();
        if cursor.goto_first_child() {
            loop {
                let child = cursor.node();
                let mut inner = Context {
                    function: context.function,
                    in_class: context.in_class,
                    parent_is_module: node.kind() == "module",
                    module_statement: node.kind() == "expression_statement"
                        && context.parent_is_module,
                };
                if Some(child.id()) == body {
                    if node.kind() == "function_definition" {
                        inner.function = Some(node);
                        inner.in_class = false;
                    } else {
                        inner.in_class = true;
                    }
                }
                stack.push((child, inner));
                if !cursor.goto_next_sibling() {
                    break;
                }
            }
        }
        stack[mark..].reverse();
    }
    Some(Collected {
        events,
        parameters,
        globals,
    })
}

/// One step of a path expression.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Item {
    Lit(String),
    /// A value written in terms of `__file__`: `0` is the file itself, `n` is
    /// `n` directories up from it.
    Anchor(u32),
    /// A parameter of the function the path is written in, never rebound in
    /// it. Only a parameterised loader's call site can say what it holds.
    Param(String),
    Opaque,
}

/// Refusal marker: the expression cannot be read without guessing.
struct Abstain;

struct Pass<'a, 'tree> {
    source: &'a str,
    file_symbol_name: &'a str,
    events: &'a [Event<'tree>],
    /// Module-level names bound exactly once, never declared `global` or
    /// `nonlocal` anywhere, to a value this pass reads completely: a
    /// `__file__` anchor (`ROOT`) or a whole path (`BUILDER = ROOT /
    /// "scripts/x.py"`). The value is the constant's path steps.
    constants: HashMap<String, Vec<Item>>,
    /// Names some function declares `global` or `nonlocal`.
    globals: HashSet<String>,
    /// `(scope, name)` for every name any statement in that scope binds.
    assigned: HashSet<(Option<String>, String)>,
    /// Parameters of each function scope that has an event, in order.
    parameters: HashMap<Option<String>, Vec<Param>>,
    state: HashMap<(Option<String>, String), Val>,
    bindings: BTreeMap<(Option<String>, String), Binding>,
    /// Loader functions: scope → what `return` hands back.
    returns: BTreeMap<String, Option<PathV>>,
    /// Every load whose path was readable, for the file-level edge.
    loads: Vec<(Loc, Span, String)>,
}

#[derive(Debug, Clone)]
enum Binding {
    Bound(Loc, Span, String),
    Poisoned,
}

impl<'a, 'tree> Pass<'a, 'tree> {
    fn new(
        source: &'a str,
        file_symbol_name: &'a str,
        events: &'a [Event<'tree>],
        parameters: HashMap<Option<String>, Vec<Param>>,
        globals: HashSet<String>,
    ) -> Self {
        let mut assigned = HashSet::new();
        for event in events {
            if let Event::Bind { names, scope, .. } = event {
                for name in names {
                    assigned.insert((scope.clone(), name.clone()));
                }
            }
        }
        Pass {
            source,
            file_symbol_name,
            events,
            constants: HashMap::new(),
            globals,
            assigned,
            parameters,
            state: HashMap::new(),
            bindings: BTreeMap::new(),
            returns: BTreeMap::new(),
            loads: Vec::new(),
        }
    }

    fn reset(&mut self) {
        self.state.clear();
        self.bindings.clear();
        self.returns.clear();
        self.loads.clear();
    }

    fn is_parameter(&self, scope: &Option<String>, name: &str) -> bool {
        scope.is_some()
            && self
                .parameters
                .get(scope)
                .is_some_and(|params| params.iter().any(|param| param.name == name))
    }

    /// Whether `name`, read inside `scope`, is that scope's own local — bound
    /// by a statement in it, or one of its function's parameters. Python
    /// decides locality for the whole function body at once, so a binding
    /// anywhere in the scope makes every read of the name local.
    fn is_local(&self, scope: &Option<String>, name: &str) -> bool {
        if scope.is_none() {
            return false;
        }
        self.assigned.contains(&(scope.clone(), name.to_string())) || self.is_parameter(scope, name)
    }

    /// The value `name` holds when read inside `scope`: the scope's own
    /// binding, else — when the scope never binds it — the module's.
    fn lookup(&self, scope: &Option<String>, name: &str) -> Val {
        if let Some(value) = self.state.get(&(scope.clone(), name.to_string())) {
            return value.clone();
        }
        if scope.is_some() && !self.is_local(scope, name) {
            if let Some(value) = self.state.get(&(None, name.to_string())) {
                return value.clone();
            }
        }
        Val::Other
    }

    /// The module a call of a same-file loader function returns, if `call`
    /// is one and its argument can be read.
    ///
    /// The callee must be a bare name this scope does not bind itself, and
    /// that name must be one of this file's recognised loaders — a `load`
    /// that loads data, or a loader in another file, is not consulted. A
    /// fixed loader takes no positional argument that could change what it
    /// loads; a parameterised one needs the tail argument as a plain `.py`
    /// string literal, passed positionally or by keyword, with no splat that
    /// could move it.
    fn loader_call(
        &self,
        call: Node,
        scope: &Option<String>,
        loaders: &BTreeMap<String, LoaderFn>,
    ) -> Option<Loc> {
        let function = call.child_by_field_name("function")?;
        if function.kind() != "identifier" {
            return None;
        }
        let name = text(function, self.source);
        if self.is_local(scope, name) {
            return None;
        }
        match loaders.get(name)? {
            LoaderFn::Fixed(loc) => positional_arguments(call).is_empty().then(|| loc.clone()),
            LoaderFn::Param {
                steps,
                param,
                position,
            } => {
                let arguments = call.child_by_field_name("arguments")?;
                let mut positional = Vec::new();
                let mut by_keyword = None;
                let mut cursor = arguments.walk();
                for argument in arguments.named_children(&mut cursor) {
                    match argument.kind() {
                        "comment" => {}
                        "list_splat" | "dictionary_splat" => return None,
                        "keyword_argument" => {
                            let keyword = argument.child_by_field_name("name")?;
                            if text(keyword, self.source) == param {
                                by_keyword = argument.child_by_field_name("value");
                            }
                        }
                        _ => positional.push(argument),
                    }
                    if positional.len() > MAX_SEGMENTS {
                        return None;
                    }
                }
                let argument = position
                    .and_then(|index| positional.get(index).copied())
                    .or(by_keyword)?;
                if argument.kind() != "string" {
                    return None;
                }
                let literal = string_literal(argument, self.source).ok()?;
                let mut filled = steps.clone();
                *filled.last_mut()? = Item::Lit(literal);
                loc_from_items(&filled)
            }
        }
    }

    fn run(&mut self, loaders: &BTreeMap<String, LoaderFn>) {
        for event in self.events {
            match event {
                Event::Bind {
                    node,
                    names,
                    value,
                    scope,
                    in_class,
                    ..
                } => {
                    let val = match (value, names.len(), *in_class) {
                        (Some(value), 1, false) => self.classify(*value, scope, loaders),
                        _ => Val::Other,
                    };
                    for name in names {
                        let key = (scope.clone(), name.clone());
                        self.state.insert(key.clone(), val.clone());
                        match &val {
                            Val::Module(PathV::Fixed(loc)) => {
                                let span = Span {
                                    start_byte: node.start_byte(),
                                    end_byte: node.end_byte(),
                                };
                                let raw = raw_text(text(*node, self.source));
                                match self.bindings.get(&key) {
                                    None => {
                                        self.bindings
                                            .insert(key, Binding::Bound(loc.clone(), span, raw));
                                    }
                                    Some(Binding::Bound(held, _, _)) if held == loc => {}
                                    Some(_) => {
                                        self.bindings.insert(key, Binding::Poisoned);
                                    }
                                }
                            }
                            _ => {
                                // Rebound after a load: which use sees which
                                // value is not decided here, so neither is.
                                if self.bindings.contains_key(&key) {
                                    self.bindings.insert(key, Binding::Poisoned);
                                }
                            }
                        }
                    }
                }
                Event::Return { value, scope } => {
                    let Some(function) = scope.clone() else {
                        continue;
                    };
                    let returned = match self.classify(*value, scope, loaders) {
                        Val::Module(path) => Some(path),
                        _ => None,
                    };
                    match self.returns.get(&function) {
                        None => {
                            self.returns.insert(function, returned);
                        }
                        Some(held) if *held == returned => {}
                        Some(_) => {
                            self.returns.insert(function, None);
                        }
                    }
                }
                Event::Load { node, scope } => {
                    let function = node.child_by_field_name("function");
                    let Some(kind) = function.and_then(|f| load_kind(text(f, self.source))) else {
                        continue;
                    };
                    if let Some(PathV::Fixed(loc)) = self.path_of_call(*node, kind, scope) {
                        self.record_load(*node, loc);
                    }
                }
                // Read once, at emission: see `search_directories`.
                Event::SysPath { .. } => {}
                Event::Call { node, scope } => {
                    // Only a parameterised loader's call names a file of its
                    // own; a fixed loader's file was recorded at its `def`.
                    let parameterised = node
                        .child_by_field_name("function")
                        .and_then(|function| loaders.get(text(function, self.source)))
                        .is_some_and(|loader| matches!(loader, LoaderFn::Param { .. }));
                    if parameterised {
                        if let Some(loc) = self.loader_call(*node, scope, loaders) {
                            self.record_load(*node, loc);
                        }
                    }
                }
            }
        }
    }

    fn record_load(&mut self, node: Node, loc: Loc) {
        let span = Span {
            start_byte: node.start_byte(),
            end_byte: node.end_byte(),
        };
        self.loads
            .push((loc, span, raw_text(text(node, self.source))));
    }

    /// What a right-hand side evaluates to, in this pass's small value model.
    fn classify(
        &self,
        value: Node,
        scope: &Option<String>,
        loaders: &BTreeMap<String, LoaderFn>,
    ) -> Val {
        let mut value = value;
        let mut unwrapped = 0;
        while value.kind() == "parenthesized_expression" && unwrapped < MAX_EVAL_DEPTH {
            match value.named_child(0) {
                Some(inner) => value = inner,
                None => return Val::Other,
            }
            unwrapped += 1;
        }
        match value.kind() {
            // An alias of a handle is the same handle.
            "identifier" => self.lookup(scope, text(value, self.source)),
            "call" => {
                let Some(function) = value.child_by_field_name("function") else {
                    return Val::Other;
                };
                let callee = text(function, self.source);
                if let Some(kind) = load_kind(callee) {
                    return match kind {
                        LoadKind::SpecFromFileLocation => self
                            .path_of_call(value, kind, scope)
                            .map_or(Val::Other, Val::Spec),
                        LoadKind::SourceFileLoader => self
                            .path_of_call(value, kind, scope)
                            .map_or(Val::Other, Val::Loader),
                        LoadKind::LoadSource => self
                            .path_of_call(value, kind, scope)
                            .map_or(Val::Other, Val::Module),
                        // A globals dict, not a module.
                        LoadKind::RunPath => Val::Other,
                        LoadKind::ModuleFromSpec => match positional_arguments(value).first() {
                            Some(argument) if argument.kind() == "identifier" => {
                                match self.lookup(scope, text(*argument, self.source)) {
                                    Val::Spec(path) => Val::Module(path),
                                    _ => Val::Other,
                                }
                            }
                            Some(argument) if argument.kind() == "call" => {
                                match self.classify(*argument, scope, loaders) {
                                    Val::Spec(path) => Val::Module(path),
                                    _ => Val::Other,
                                }
                            }
                            _ => Val::Other,
                        },
                    };
                }
                // `SourceFileLoader(...).load_module()` / `loader.load_module()`.
                if function.kind() == "attribute"
                    && function
                        .child_by_field_name("attribute")
                        .is_some_and(|name| text(name, self.source) == "load_module")
                {
                    if let Some(object) = function.child_by_field_name("object") {
                        if object.kind() == "identifier" || object.kind() == "call" {
                            if let Val::Loader(path) = self.classify(object, scope, loaders) {
                                return Val::Module(path);
                            }
                        }
                    }
                    return Val::Other;
                }
                // A same-file loader function: `m = load()`, `W = load("w",
                // "scripts/w.py")`.
                self.loader_call(value, scope, loaders)
                    .map_or(Val::Other, |loc| Val::Module(PathV::Fixed(loc)))
            }
            _ => Val::Other,
        }
    }

    fn path_of_call(&self, call: Node, kind: LoadKind, scope: &Option<String>) -> Option<PathV> {
        let (index, keyword) = kind.path_argument()?;
        let arguments = call.child_by_field_name("arguments")?;
        if arguments.kind() != "argument_list" {
            return None;
        }
        let mut positional = Vec::new();
        let mut by_keyword = None;
        let mut cursor = arguments.walk();
        for argument in arguments.named_children(&mut cursor) {
            match argument.kind() {
                "comment" => {}
                // A splat moves every later positional argument somewhere
                // this pass cannot see.
                "list_splat" | "dictionary_splat" => return None,
                "keyword_argument" => {
                    let name = argument.child_by_field_name("name")?;
                    if text(name, self.source) == keyword {
                        by_keyword = argument.child_by_field_name("value");
                    }
                }
                _ => positional.push(argument),
            }
        }
        let expression = positional.get(index).copied().or(by_keyword)?;
        let mut budget = MAX_EVAL_NODES;
        let items = self.eval(expression, scope, 0, &mut budget).ok()?;
        path_from_items(items)
    }

    /// A path expression as a sequence of literal, anchored and opaque steps.
    fn eval(
        &self,
        node: Node,
        scope: &Option<String>,
        depth: usize,
        budget: &mut usize,
    ) -> Result<Vec<Item>, Abstain> {
        if depth > MAX_EVAL_DEPTH || *budget == 0 {
            return Err(Abstain);
        }
        *budget -= 1;
        match node.kind() {
            "parenthesized_expression" => match node.named_child(0) {
                Some(inner) => self.eval(inner, scope, depth + 1, budget),
                None => Err(Abstain),
            },
            "string" => string_literal(node, self.source).map(|lit| vec![Item::Lit(lit)]),
            // Implicit concatenation of literals, possibly with f-strings.
            // Refused rather than half-read.
            "concatenated_string" => Err(Abstain),
            "identifier" => {
                let name = text(node, self.source);
                if name == "__file__" {
                    return Ok(vec![Item::Anchor(0)]);
                }
                // A parameter the function never rebinds: what it holds is a
                // question only a call site can answer.
                if self.is_parameter(scope, name)
                    && !self.assigned.contains(&(scope.clone(), name.to_string()))
                {
                    return Ok(vec![Item::Param(name.to_string())]);
                }
                if self.is_local(scope, name) {
                    return Ok(vec![Item::Opaque]);
                }
                Ok(self
                    .constants
                    .get(name)
                    .cloned()
                    .unwrap_or_else(|| vec![Item::Opaque]))
            }
            "binary_operator" => {
                // Walk the left spine iteratively: `a / b / c / …` nests to
                // the left, one level per segment.
                let mut rights = Vec::new();
                let mut left = node;
                while left.kind() == "binary_operator" {
                    let operator = left
                        .child_by_field_name("operator")
                        .map(|op| text(op, self.source))
                        .unwrap_or("");
                    if operator != "/" {
                        return Err(Abstain);
                    }
                    if rights.len() >= MAX_SEGMENTS || *budget == 0 {
                        return Err(Abstain);
                    }
                    *budget -= 1;
                    rights.push(left.child_by_field_name("right").ok_or(Abstain)?);
                    left = left.child_by_field_name("left").ok_or(Abstain)?;
                }
                let mut items = self.eval(left, scope, depth + 1, budget)?;
                for right in rights.into_iter().rev() {
                    items.extend(self.eval(right, scope, depth + 1, budget)?);
                    if items.len() > MAX_SEGMENTS {
                        return Err(Abstain);
                    }
                }
                Ok(items)
            }
            "attribute" => {
                let attribute = node
                    .child_by_field_name("attribute")
                    .map(|a| text(a, self.source))
                    .unwrap_or("");
                let object = node.child_by_field_name("object").ok_or(Abstain)?;
                if attribute == "parent" {
                    return Ok(vec![
                        match self.eval(object, scope, depth + 1, budget)?[..] {
                            [Item::Anchor(level)] => Item::Anchor(level.saturating_add(1)),
                            _ => Item::Opaque,
                        },
                    ]);
                }
                Ok(vec![Item::Opaque])
            }
            "subscript" => {
                // `X.parents[N]`
                let value = node.child_by_field_name("value").ok_or(Abstain)?;
                let index = node.child_by_field_name("subscript");
                let parents = value.kind() == "attribute"
                    && value
                        .child_by_field_name("attribute")
                        .is_some_and(|a| text(a, self.source) == "parents");
                let level = index
                    .filter(|index| index.kind() == "integer")
                    .and_then(|index| text(index, self.source).parse::<u32>().ok());
                match (parents, level, value.child_by_field_name("object")) {
                    (true, Some(level), Some(object)) => {
                        Ok(vec![
                            match self.eval(object, scope, depth + 1, budget)?[..] {
                                [Item::Anchor(base)] => {
                                    Item::Anchor(base.saturating_add(level).saturating_add(1))
                                }
                                _ => Item::Opaque,
                            },
                        ])
                    }
                    _ => Ok(vec![Item::Opaque]),
                }
            }
            "call" => self.eval_call(node, scope, depth, budget),
            _ => Ok(vec![Item::Opaque]),
        }
    }

    fn eval_call(
        &self,
        node: Node,
        scope: &Option<String>,
        depth: usize,
        budget: &mut usize,
    ) -> Result<Vec<Item>, Abstain> {
        let function = node.child_by_field_name("function").ok_or(Abstain)?;
        let arguments = node.child_by_field_name("arguments").ok_or(Abstain)?;
        let mut positional = Vec::new();
        let mut cursor = arguments.walk();
        for argument in arguments.named_children(&mut cursor) {
            match argument.kind() {
                "comment" => {}
                "keyword_argument" | "list_splat" | "dictionary_splat" => {
                    return Ok(vec![Item::Opaque])
                }
                _ => positional.push(argument),
            }
            if positional.len() > MAX_SEGMENTS {
                return Err(Abstain);
            }
        }
        let callee = text(function, self.source);
        if callee.len() > MAX_CALLEE_BYTES {
            return Ok(vec![Item::Opaque]);
        }
        let concat = |pass: &Self, budget: &mut usize| -> Result<Vec<Item>, Abstain> {
            let mut items = Vec::new();
            for argument in &positional {
                items.extend(pass.eval(*argument, scope, depth + 1, budget)?);
                if items.len() > MAX_SEGMENTS {
                    return Err(Abstain);
                }
            }
            Ok(items)
        };
        match callee {
            "Path" | "pathlib.Path" | "PurePath" | "pathlib.PurePath" | "PosixPath"
            | "pathlib.PosixPath" | "os.path.join" | "path.join" | "posixpath.join" => {
                if positional.is_empty() {
                    // `Path()` is the working directory: a base, not a path.
                    return Ok(vec![Item::Opaque]);
                }
                concat(self, budget)
            }
            "str" | "os.fspath" | "os.path.abspath" | "os.path.realpath" | "os.path.normpath"
            | "path.abspath" | "path.realpath" | "abspath" | "realpath" => match positional[..] {
                [only] => self.eval(only, scope, depth + 1, budget),
                _ => Ok(vec![Item::Opaque]),
            },
            "os.path.dirname" | "path.dirname" | "dirname" => match positional[..] {
                [only] => Ok(vec![match self.eval(only, scope, depth + 1, budget)?[..] {
                    [Item::Anchor(level)] => Item::Anchor(level.saturating_add(1)),
                    _ => Item::Opaque,
                }]),
                _ => Ok(vec![Item::Opaque]),
            },
            _ => {
                if function.kind() == "attribute" {
                    let method = function
                        .child_by_field_name("attribute")
                        .map(|a| text(a, self.source))
                        .unwrap_or("");
                    let object = function.child_by_field_name("object").ok_or(Abstain)?;
                    match method {
                        "resolve" | "absolute" if positional.is_empty() => {
                            return self.eval(object, scope, depth + 1, budget);
                        }
                        "joinpath" => {
                            let mut items = self.eval(object, scope, depth + 1, budget)?;
                            items.extend(concat(self, budget)?);
                            if items.len() > MAX_SEGMENTS {
                                return Err(Abstain);
                            }
                            return Ok(items);
                        }
                        _ => {}
                    }
                }
                Ok(vec![Item::Opaque])
            }
        }
    }

    /// Module-level loader functions: every top-level function, bound once at
    /// module level and never declared `global` anywhere, whose every
    /// `return` hands back the same path-loaded module — a fixed file, or a
    /// template whose tail is one of its parameters.
    fn loader_functions(&self) -> BTreeMap<String, LoaderFn> {
        let prefix = format!("{}::", self.file_symbol_name);
        let counts = self.module_bind_counts();
        let mut out = BTreeMap::new();
        for (scope, returned) in &self.returns {
            let Some(returned) = returned else { continue };
            let Some(name) = scope.strip_prefix(&prefix) else {
                continue;
            };
            if name.is_empty() || name.contains(['.', ':']) {
                continue;
            }
            // Rebound at module level — `supplement = other` — or from a
            // function through `global`, it is no longer the function this
            // pass read. The `def` itself is the one bind.
            if counts.get(name) != Some(&1) || self.globals.contains(name) {
                continue;
            }
            let loader = match returned {
                PathV::Fixed(loc) => LoaderFn::Fixed(loc.clone()),
                PathV::Template(steps) => {
                    let Some(Item::Param(param)) = steps.last() else {
                        continue;
                    };
                    let Some(position) = self
                        .parameters
                        .get(&Some(scope.clone()))
                        .and_then(|params| params.iter().find(|p| &p.name == param))
                        .map(|p| p.position)
                    else {
                        continue;
                    };
                    LoaderFn::Param {
                        steps: steps.clone(),
                        param: param.clone(),
                        position,
                    }
                }
            };
            out.insert(name.to_string(), loader);
        }
        out
    }

    /// How many statements bind each name at module level.
    fn module_bind_counts(&self) -> HashMap<&'a str, usize> {
        let mut counts: HashMap<&'a str, usize> = HashMap::new();
        for event in self.events {
            if let Event::Bind {
                names, scope: None, ..
            } = event
            {
                for name in names {
                    *counts.entry(name.as_str()).or_default() += 1;
                }
            }
        }
        counts
    }

    /// Module-level constants: names bound exactly once, directly under the
    /// module, never declared `global`/`nonlocal` by any function, to a value
    /// this pass reads completely — a `__file__` anchor (`ROOT =
    /// Path(__file__).resolve().parents[1]`, `HERE =
    /// os.path.dirname(__file__)`) or a whole `.py` path (`BUILDER = ROOT /
    /// "scripts/build_paper.py"`). Evaluated in document order so `ROOT =
    /// HERE.parent` sees `HERE` and `BUILDER` sees `ROOT`.
    fn compute_constants(&mut self) {
        let counts = self.module_bind_counts();
        let module_scope = None;
        for event in self.events {
            let Event::Bind {
                names,
                value: Some(value),
                scope: None,
                in_class: false,
                module_level_statement: true,
                ..
            } = event
            else {
                continue;
            };
            let [name] = &names[..] else { continue };
            if counts.get(name.as_str()) != Some(&1) || self.globals.contains(name) {
                continue;
            }
            let mut budget = MAX_EVAL_NODES;
            let Ok(items) = self.eval(*value, &module_scope, 0, &mut budget) else {
                continue;
            };
            let complete = items
                .iter()
                .all(|item| matches!(item, Item::Lit(_) | Item::Anchor(_)));
            let anchor = matches!(items[..], [Item::Anchor(level)] if level >= 1);
            if complete && (anchor || loc_from_items(&items).is_some()) {
                self.constants.insert(name.clone(), items);
            }
        }
    }

    fn emit(&self, loaders: &BTreeMap<String, LoaderFn>) -> Vec<ExtractedImport> {
        let mut out: Vec<ExtractedImport> = Vec::new();
        let mut bound: HashSet<&Loc> = HashSet::new();
        for ((scope, name), binding) in &self.bindings {
            let Binding::Bound(loc, span, raw) = binding else {
                continue;
            };
            bound.insert(loc);
            out.push(path_import(
                raw,
                loc,
                Some(name.clone()),
                scope.clone(),
                span.clone(),
            ));
        }
        // `load().fn()`. The extractor records a call's receiver identity as
        // the inner call's callee with its argument list dropped — `load` —
        // so the loader function's own name, at module level, is the key that
        // use site joins on. A function-local `load` shadowing it has a site
        // binding of its own and so a different key. Fixed loaders only: a
        // parameterised loader's name names a different file at every call.
        let mut definitions: HashMap<&str, Span> = HashMap::new();
        if !loaders.is_empty() {
            for event in self.events {
                if let Event::Bind {
                    node,
                    names,
                    scope: None,
                    ..
                } = event
                {
                    if node.kind() == "function_definition" {
                        for name in names {
                            definitions.entry(name.as_str()).or_insert(Span {
                                start_byte: node.start_byte(),
                                end_byte: node.end_byte(),
                            });
                        }
                    }
                }
            }
        }
        for (name, loader) in loaders {
            let LoaderFn::Fixed(loc) = loader else {
                continue;
            };
            let span = definitions.get(name.as_str()).cloned().unwrap_or(Span {
                start_byte: 0,
                end_byte: 0,
            });
            bound.insert(loc);
            out.push(path_import(
                &format!("def {name}(): return <module loaded from {}>", loc.path),
                loc,
                Some(name.clone()),
                None,
                span,
            ));
        }
        // The file-level dependency for every load nothing bound: `run_path`,
        // a spec whose module was never built, a handle this pass refused.
        let mut seen: HashSet<&Loc> = HashSet::new();
        for (loc, span, raw) in &self.loads {
            if bound.contains(loc) || !seen.insert(loc) {
                continue;
            }
            out.push(path_import(raw, loc, None, None, span.clone()));
        }
        out.extend(self.search_directories());
        out.sort_by(|a, b| {
            (a.span.start_byte, &a.alias, &a.module_specifier).cmp(&(
                b.span.start_byte,
                &b.alias,
                &b.module_specifier,
            ))
        });
        out
    }

    /// The directories this file puts on `sys.path`, each with where it
    /// happened, as `SearchDirectory` imports.
    ///
    /// A module-level insert has `scope: None`: it runs when the file loads,
    /// before every import that follows it, and is what an import resolves
    /// through. An insert inside a function or class body has `scope:
    /// Some(..)` — the function's identity, or the file's own symbol for a
    /// class body — and is only a veto: it runs whenever the function is
    /// called, which may be before any import in the file, so a module it
    /// could supply is a module the import may not reach where the
    /// module-level entries say. One whose directory cannot be read (or sits
    /// in a class body, whose names this pass does not scope) is recorded
    /// with no `anchor_up` and an empty directory: it could point anywhere,
    /// and vetoes every `sys.path`-derived link in the file.
    ///
    /// All or nothing otherwise. A write this pass cannot follow —
    /// `sys.path.remove`, reassignment, `del`, or a module-level insert of a
    /// directory it cannot read — anywhere in the file leaves the search path
    /// unknown, and an unknown entry ahead of a known one could hold the
    /// module first. So one such write withdraws every directory.
    fn search_directories(&self) -> Vec<ExtractedImport> {
        let mut out = Vec::new();
        for event in self.events {
            let Event::SysPath {
                node,
                directory,
                module_level,
                scope,
            } = event
            else {
                continue;
            };
            let Some(directory) = directory else {
                return Vec::new();
            };
            // A class body is neither module level nor a function scope the
            // evaluator can key on; its insert is read as unreadable.
            let readable_scope = *module_level || scope.is_some();
            let read = readable_scope
                .then(|| {
                    let mut budget = MAX_EVAL_NODES;
                    let items = self.eval(*directory, scope, 0, &mut budget).ok()?;
                    directory_from_items(&items)
                })
                .flatten();
            let (tail, anchor_up, entry_scope) = match (read, *module_level) {
                (Some((tail, anchor_up)), true) => (tail, Some(anchor_up), None),
                (None, true) => return Vec::new(),
                (read, false) => {
                    let entry_scope = scope
                        .clone()
                        .unwrap_or_else(|| self.file_symbol_name.to_string());
                    match read {
                        Some((tail, anchor_up)) => (tail, Some(anchor_up), Some(entry_scope)),
                        None => (String::new(), None, Some(entry_scope)),
                    }
                }
            };
            out.push(ExtractedImport {
                raw_import: raw_text(text(*node, self.source)),
                module_specifier: tail,
                imported_names: Vec::new(),
                local_names: Vec::new(),
                alias: None,
                span: Span {
                    start_byte: node.start_byte(),
                    end_byte: node.end_byte(),
                },
                path_load: Some(PathLoad {
                    scope: entry_scope,
                    anchor_up,
                    kind: PathLoadKind::SearchDirectory,
                }),
            });
        }
        // A real script inserts a handful. Past the cap every import would be
        // checked against every entry, so the file abstains instead.
        if out.len() > MAX_SEARCH_DIRECTORIES {
            return Vec::new();
        }
        out
    }
}

/// A `sys.path` entry's steps as a directory: `(tail, anchor_up)`, where the
/// directory is the loading file's own directory climbed `anchor_up` levels,
/// then `tail` (possibly empty, possibly climbing with `..`). Only an entry
/// anchored on `__file__` is read — a bare literal is relative to the working
/// directory, which the source does not fix.
fn directory_from_items(items: &[Item]) -> Option<(String, u32)> {
    let anchor = items
        .iter()
        .rposition(|item| !matches!(item, Item::Lit(_)))?;
    let Item::Anchor(level) = items[anchor] else {
        return None;
    };
    if level == 0
        || items[..anchor]
            .iter()
            .any(|item| !matches!(item, Item::Lit(_) | Item::Anchor(_)))
    {
        return None;
    }
    let mut parts: Vec<&str> = Vec::new();
    for item in &items[anchor + 1..] {
        let Item::Lit(literal) = item else {
            return None;
        };
        if literal.starts_with('/') || literal.contains('\\') || literal.contains(':') {
            return None;
        }
        parts.extend(
            literal
                .split('/')
                .filter(|part| !part.is_empty() && *part != "."),
        );
    }
    let tail = parts.join("/");
    (tail.len() <= MAX_PATH_BYTES).then_some((tail, level - 1))
}

fn path_import(
    raw: &str,
    loc: &Loc,
    alias: Option<String>,
    scope: Option<String>,
    span: Span,
) -> ExtractedImport {
    ExtractedImport {
        raw_import: raw_text(raw),
        module_specifier: loc.path.clone(),
        imported_names: Vec::new(),
        local_names: Vec::new(),
        alias,
        span,
        path_load: Some(PathLoad {
            scope,
            anchor_up: loc.anchor_up,
            kind: PathLoadKind::Module,
        }),
    }
}

/// One line, bounded: this becomes an edge's `details`.
fn raw_text(raw: &str) -> String {
    let flat = raw.split_whitespace().collect::<Vec<_>>().join(" ");
    if flat.chars().count() <= MAX_RAW_CHARS {
        return flat;
    }
    let mut out: String = flat.chars().take(MAX_RAW_CHARS).collect();
    out.push('\u{2026}');
    out
}

fn positional_arguments(call: Node) -> Vec<Node> {
    let Some(arguments) = call.child_by_field_name("arguments") else {
        return Vec::new();
    };
    let mut cursor = arguments.walk();
    arguments
        .named_children(&mut cursor)
        .filter(|argument| !matches!(argument.kind(), "comment" | "keyword_argument"))
        .take(MAX_SEGMENTS)
        .collect()
}

/// The text of a plain string literal, or a refusal for anything that is not
/// one: an f-string, a bytes literal, an escape sequence, an oversized value.
fn string_literal(node: Node, source: &str) -> Result<String, Abstain> {
    if node.end_byte().saturating_sub(node.start_byte()) > MAX_PATH_BYTES + 16 {
        return Err(Abstain);
    }
    let mut start = None;
    let mut end = None;
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        match child.kind() {
            "string_start" => {
                let prefix = text(child, source);
                if prefix
                    .chars()
                    .any(|ch| matches!(ch, 'f' | 'F' | 'b' | 'B' | 't' | 'T'))
                {
                    return Err(Abstain);
                }
                start = Some(child.end_byte());
            }
            "string_end" => end = Some(child.start_byte()),
            "string_content" => {}
            // `interpolation`, `escape_sequence`, anything else.
            _ => return Err(Abstain),
        }
    }
    let (Some(start), Some(end)) = (start, end) else {
        return Err(Abstain);
    };
    let content = source.get(start..end).ok_or(Abstain)?;
    if content.contains('\\') || content.contains('\n') {
        return Err(Abstain);
    }
    Ok(content.to_string())
}

/// A path expression's steps as a place, or as a template for a
/// parameterised loader.
///
/// A template needs exactly one parameter step, and it must be the **last**
/// step: `ROOT / relative`, or `relative` alone. Then each call site's literal
/// argument completes the path exactly. A parameter anywhere else — `ROOT /
/// folder / "mod.py"`, `ROOT / relative / relative` — is not substituted; it
/// is read as an unknown base, as any opaque value is.
fn path_from_items(items: Vec<Item>) -> Option<PathV> {
    let params = items
        .iter()
        .filter(|item| matches!(item, Item::Param(_)))
        .count();
    if params == 1 && matches!(items.last(), Some(Item::Param(_))) {
        return Some(PathV::Template(items));
    }
    loc_from_items(&items).map(PathV::Fixed)
}

/// The literal tail after the last non-literal step, as a relative path.
fn loc_from_items(items: &[Item]) -> Option<Loc> {
    let last_non_literal = items.iter().rposition(|item| !matches!(item, Item::Lit(_)));
    let tail = &items[last_non_literal.map_or(0, |index| index + 1)..];
    let anchor_up = match last_non_literal.map(|index| &items[index]) {
        // A parameter that is not the path's tail says no more about the
        // base than an opaque value does.
        None | Some(Item::Opaque) | Some(Item::Param(_)) => None,
        // `__file__ / "x.py"` names nothing.
        Some(Item::Anchor(0)) => return None,
        Some(Item::Anchor(level)) => Some(level - 1),
        Some(Item::Lit(_)) => unreachable!("rposition found a non-literal"),
    };
    let mut parts: Vec<&str> = Vec::new();
    for item in tail {
        let Item::Lit(literal) = item else {
            return None;
        };
        // An absolute segment resets the join; a drive letter or a
        // backslash is a path this corpus-relative reader cannot place.
        if literal.starts_with('/') || literal.contains('\\') || literal.contains(':') {
            return None;
        }
        for part in literal.split('/') {
            match part {
                "" | "." => {}
                other => parts.push(other),
            }
        }
    }
    let path = parts.join("/");
    if path.len() > MAX_PATH_BYTES || !path.ends_with(".py") || path.ends_with("/.py") {
        return None;
    }
    let file_name = path.rsplit('/').next().unwrap_or(&path);
    if file_name == ".py" || file_name == ".." {
        return None;
    }
    Some(Loc { path, anchor_up })
}

#[cfg(test)]
mod tests {
    use super::python_path_loads;
    use std::time::{Duration, Instant};

    fn parse(source: &str) -> tree_sitter::Tree {
        let mut parser = tree_sitter::Parser::new();
        parser
            .set_language(&tree_sitter_python::LANGUAGE.into())
            .expect("python grammar");
        parser.parse(source, None).expect("parse")
    }

    /// The loader-function shape, which exercises everything the pass does
    /// per load: a function scope, a spec, a module, a return, a second pass
    /// for loader functions, and a module-level handle bound through one.
    fn loaders(count: usize) -> String {
        let mut source = String::from("import importlib.util\n");
        for i in 0..count {
            source.push_str(&format!(
                "def load{i}():\n    spec = importlib.util.spec_from_file_location(\"m{i}\", \"lib/m{i}.py\")\n    \
                 module = importlib.util.module_from_spec(spec)\n    module.run()\n    return module\n\
                 h{i} = load{i}()\n"
            ));
        }
        source
    }

    /// Ten times the loads for far less than a hundred times the time,
    /// measured on this pass alone so the rest of the extractor is not what
    /// is judged. Interleaved min-of-three, so one scheduling excursion on a
    /// loaded machine cannot fake a quadratic.
    #[test]
    fn thousands_of_loads_cost_linear_time() {
        let small = loaders(400);
        let large = loaders(4_000);
        let small_tree = parse(&small);
        let large_tree = parse(&large);
        let mut small_best = Duration::MAX;
        let mut large_best = Duration::MAX;
        for _ in 0..3 {
            let start = Instant::now();
            let small_loads = python_path_loads(small_tree.root_node(), &small, "lib/s.py");
            small_best = small_best.min(start.elapsed());
            let start = Instant::now();
            let large_loads = python_path_loads(large_tree.root_node(), &large, "lib/l.py");
            large_best = large_best.min(start.elapsed());
            // Per load: the function-local handle, the loader function, and
            // the module-level `h{i}` bound through it.
            assert_eq!(small_loads.len(), 400 * 3);
            assert_eq!(large_loads.len(), 4_000 * 3);
        }
        let ratio = large_best.as_secs_f64() / small_best.as_secs_f64().max(1e-9);
        assert!(
            ratio < 40.0,
            "10x the loads took {ratio:.1}x the time ({small_best:?} -> {large_best:?}), \
             which is the shape of a quadratic pass"
        );
    }

    /// The parameterised-loader shape: one `def load(name, relative)` and a
    /// call per module, half at module level and half inside functions.
    fn parameterised_calls(count: usize) -> String {
        let mut source = String::from(
            "import importlib.util\nfrom pathlib import Path\nROOT = Path(__file__).resolve().parents[1]\n\
             BUILDER = ROOT / \"scripts/build_paper.py\"\n\
             def load(name, relative):\n    spec = importlib.util.spec_from_file_location(name, ROOT / relative)\n    \
             mod = importlib.util.module_from_spec(spec)\n    return mod\n",
        );
        for i in 0..count {
            if i % 2 == 0 {
                source.push_str(&format!("W{i} = load(\"w{i}\", \"scripts/aws/w{i}.py\")\n"));
            } else {
                source.push_str(&format!(
                    "def use{i}():\n    w = load(\"w{i}\", relative=\"scripts/aws/w{i}.py\")\n    return w.run()\n"
                ));
            }
        }
        source
    }

    /// Thousands of parameterised loader calls cost linear time, and every
    /// call is read.
    #[test]
    fn thousands_of_parameterised_loader_calls_cost_linear_time() {
        let small = parameterised_calls(400);
        let large = parameterised_calls(4_000);
        let small_tree = parse(&small);
        let large_tree = parse(&large);
        let mut small_best = Duration::MAX;
        let mut large_best = Duration::MAX;
        for _ in 0..3 {
            let start = Instant::now();
            let small_loads = python_path_loads(small_tree.root_node(), &small, "scripts/s.py");
            small_best = small_best.min(start.elapsed());
            let start = Instant::now();
            let large_loads = python_path_loads(large_tree.root_node(), &large, "scripts/l.py");
            large_best = large_best.min(start.elapsed());
            // One bound handle per call.
            assert_eq!(small_loads.len(), 400);
            assert_eq!(large_loads.len(), 4_000);
        }
        let ratio = large_best.as_secs_f64() / small_best.as_secs_f64().max(1e-9);
        assert!(
            ratio < 40.0,
            "10x the calls took {ratio:.1}x the time ({small_best:?} -> {large_best:?}), \
             which is the shape of a quadratic pass"
        );
    }

    fn search_directories(loads: &[crate::model::ExtractedImport]) -> Vec<&str> {
        loads
            .iter()
            .filter(|import| {
                import
                    .path_load
                    .as_ref()
                    .is_some_and(|load| load.kind == crate::model::PathLoadKind::SearchDirectory)
            })
            .map(|import| import.module_specifier.as_str())
            .collect()
    }

    /// Thousands of `sys.path` writes cost linear time: past the per-file cap
    /// the file abstains, and reaching that verdict is one pass.
    #[test]
    fn thousands_of_sys_path_inserts_cost_linear_time_and_abstain_past_the_cap() {
        let file = |count: usize| -> String {
            let mut source = String::from(
                "import sys\nfrom pathlib import Path\nROOT = Path(__file__).resolve().parent.parent\n",
            );
            for i in 0..count {
                source.push_str(&format!(
                    "sys.path.insert(0, str(ROOT / \"d{i}\"))\nimport m{i}\n"
                ));
            }
            source
        };
        let at_cap = file(super::MAX_SEARCH_DIRECTORIES);
        let at_cap_tree = parse(&at_cap);
        assert_eq!(
            search_directories(&python_path_loads(
                at_cap_tree.root_node(),
                &at_cap,
                "s/a.py"
            ))
            .len(),
            super::MAX_SEARCH_DIRECTORIES
        );
        let small = file(400);
        let large = file(4_000);
        let small_tree = parse(&small);
        let large_tree = parse(&large);
        let mut small_best = Duration::MAX;
        let mut large_best = Duration::MAX;
        for _ in 0..3 {
            let start = Instant::now();
            let small_loads = python_path_loads(small_tree.root_node(), &small, "s/a.py");
            small_best = small_best.min(start.elapsed());
            let start = Instant::now();
            let large_loads = python_path_loads(large_tree.root_node(), &large, "s/a.py");
            large_best = large_best.min(start.elapsed());
            assert!(search_directories(&small_loads).is_empty());
            assert!(search_directories(&large_loads).is_empty());
        }
        let ratio = large_best.as_secs_f64() / small_best.as_secs_f64().max(1e-9);
        assert!(
            ratio < 40.0,
            "10x the inserts took {ratio:.1}x the time ({small_best:?} -> {large_best:?})"
        );
    }

    /// A file that names no loader API costs one substring scan and yields
    /// nothing.
    #[test]
    fn a_file_without_loaders_is_skipped() {
        let source = "import os\nx = os.path.join('a', 'b.py')\n";
        let tree = parse(source);
        assert!(python_path_loads(tree.root_node(), source, "a.py").is_empty());
    }
}
