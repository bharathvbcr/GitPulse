//! Calls made by Erlang code.
//!
//! Erlang reached `extract_node`'s generic arm, which recovered nothing for it:
//! measured on `helper(X) -> X + 1.` / `run(N) -> Y = helper(N),
//! lists:sum([Y]).` the extractor produced **1 symbol** (the `File` node),
//! **0 calls** and **0 references**. `langdecl::erlang` supplies the symbols;
//! this module supplies the edges between them.
//!
//! Shapes were read from `tree-sitter-erlang`'s own parse tree. Three of them
//! are not what an Erlang reader would predict, and each one changes the code:
//!
//! * **A remote call nests the other way round.** `lists:sum([Y])` is not a
//!   call whose callee is `lists:sum`; it is `(remote module: (remote_module
//!   module: (atom)) fun: (call expr: (atom) args: (…)))` — a `remote` node
//!   whose `fun` field is the entire `call`. So the module is found by looking
//!   *up* from the call, not down into its callee.
//! * **Type specs are full of `call` nodes.** `-spec f(integer()) ->
//!   integer().`, `-type t() :: atom().`, `-opaque h() :: reference().` and a
//!   record field's `:: list()` annotation all contain nodes of kind `call`
//!   naming a type, not a function. Extracting them would fabricate an edge per
//!   type annotation, which on real Erlang is a large fraction of all edges.
//!   They are excluded by the *type context* they sit in, not by the form they
//!   belong to — a record's `b = f(1)` default value is a real call inside the
//!   same `record_decl` as a `:: list()` that is not.
//! * **A macro invocation is not a `call`.** `?LOG("x")` is a
//!   `macro_call_expr`, so nothing here has to exclude it; the body of the
//!   `-define` that declares it *is* ordinary code and its calls are recorded
//!   from the definition site, where they belong.
//!
//! # Function references are recorded as calls
//!
//! `fun helper/1` and `fun lists:map/2` are `internal_fun` and `external_fun`.
//! They invoke nothing at the point they appear, and they are the language's
//! only way to hand a function to `lists:foreach`, `spawn`, a `gen_server`
//! callback or a supervisor child spec. Leaving them out reports every
//! callback-only function as uncalled, which is a proposal to delete working
//! code; recording them over-approximates reachability instead, which is the
//! direction that cannot. Their callee is the bare atom the reference names, so
//! the edge joins to exactly the symbol `langdecl::erlang` emitted.

use tree_sitter::Node;

use crate::model::{ExtractedCall, ExtractedReference, ReferenceKind};
use crate::treesitter::{get_child_text, get_node_text, node_span, split_call_target};

use super::scope::{clamp_receiver, enclosing_emitted_symbol};

/// Node kinds that make every `call` beneath them a *type* application.
///
/// Read from the grammar rather than assumed: `-spec` and `-callback` wrap
/// their signature in `type_sig`, `-type` and `-opaque` put the type directly
/// in their own `ty` field, and a record field's annotation sits in
/// `field_type`. The set is deliberately narrow — excluding a whole `record_decl`
/// would also drop `b = f(1)`, which is a real call.
const TYPE_CONTEXTS: &[&str] = &[
    "callback",
    "field_type",
    "opaque",
    "spec",
    "type_alias",
    "type_name",
    "type_sig",
];

/// The module name that means "this module", which is a local call.
///
/// `?MODULE:handle(X)` is how Erlang code calls its own function through the
/// module's current version, and the preprocessor expands `?MODULE` to the name
/// in this file's `-module` attribute. Recording `?MODULE` as a receiver would
/// send the edge looking for a module of that literal name and it would resolve
/// to nothing; dropping the receiver lets it resolve to the same file's
/// function, which is what the code calls.
const SELF_MODULE: &str = "?MODULE";

/// Record the call `node` makes, if it makes one.
pub fn extract_erlang_call(
    node: Node,
    source: &str,
    file_symbol_name: &str,
    calls: &mut Vec<ExtractedCall>,
    references: &mut Vec<ExtractedReference>,
) {
    let Some(site) = call_site(node, source) else {
        return;
    };
    let caller_symbol = enclosing_emitted_symbol(site.span, source, "erlang", file_symbol_name);
    references.push(ExtractedReference {
        name: site.callee.clone(),
        kind: ReferenceKind::Call,
        span: node_span(site.name),
        enclosing_symbol: caller_symbol.clone(),
        // Erlang binds values, not types. `Y = helper(N)` proves only that `Y`
        // holds whatever `helper` returned, and the language declares no type
        // for receiver inference to look up afterwards, so a binding recorded
        // here would be a fact nothing consumes.
        assigned_to: None,
        // The mirrored call already carries the receiver; repeating it here would be a second copy of one fact.
        receiver_expr: None,
    });
    calls.push(ExtractedCall {
        caller_symbol,
        callee_name: site.callee,
        receiver_expr: site.receiver,
        span: node_span(site.span),
    });
}

/// One recorded edge: the callee's name, the module it is reached through, the
/// node whose span identifies the callee, and the node the edge spans.
struct Site<'tree> {
    callee: String,
    receiver: Option<String>,
    name: Node<'tree>,
    span: Node<'tree>,
}

fn call_site<'tree>(node: Node<'tree>, source: &str) -> Option<Site<'tree>> {
    match node.kind() {
        "call" => invocation(node, source),
        // `fun helper/1`
        "internal_fun" => {
            let name = node.child_by_field_name("fun")?;
            Some(Site {
                callee: atom_identity(name, source)?,
                receiver: None,
                name,
                span: node,
            })
        }
        // `fun lists:map/2`
        "external_fun" => {
            let name = node.child_by_field_name("fun")?;
            let module = node
                .child_by_field_name("module")
                .and_then(|module| get_child_text(module, "name", source));
            Some(Site {
                callee: atom_identity(name, source)?,
                receiver: module_receiver(module),
                name,
                span: node,
            })
        }
        _ => None,
    }
}

/// A `call` node, unless it is a type application or names nothing.
fn invocation<'tree>(node: Node<'tree>, source: &str) -> Option<Site<'tree>> {
    if in_type_context(node) {
        return None;
    }
    let target = node.child_by_field_name("expr")?;
    // Only an `atom` is a callee identity in Erlang. `F(2)` — a `var` — is a
    // call through a variable holding a fun, and the name a call site could
    // join to is not present in the source at all; `?M(2)` is a macro. Refusing
    // both is the fail-closed direction: `F` is identifier-shaped and would
    // otherwise be recorded as a callee no symbol can carry.
    let callee = atom_identity(target, source)?;
    let (receiver, span) = match remote_module(node, source) {
        Some((module, remote)) => (module_receiver(module), remote),
        None => (None, node),
    };
    Some(Site {
        callee,
        receiver,
        name: target,
        span,
    })
}

/// `(module text, the node the whole remote call spans)` when this `call` is
/// the `fun` half of a `remote`.
///
/// The grammar makes `lists:sum([Y])` a `remote` whose `fun` field is the
/// `call`, so a remote call is recognised by looking at the parent rather than
/// at the callee.
fn remote_module<'tree>(node: Node<'tree>, source: &str) -> Option<(Option<String>, Node<'tree>)> {
    let parent = node.parent()?;
    if parent.kind() != "remote" {
        return None;
    }
    if parent.child_by_field_name("fun")?.id() != node.id() {
        return None;
    }
    let module = parent
        .child_by_field_name("module")
        .and_then(|remote_module| remote_module.child_by_field_name("module"))
        .map(|module| get_node_text(module, source));
    Some((module, parent))
}

/// The receiver a module qualifier contributes, with `?MODULE` resolved to none.
fn module_receiver(module: Option<String>) -> Option<String> {
    let module = module?;
    if module.trim() == SELF_MODULE {
        return None;
    }
    clamp_receiver(&module)
}

/// The text of an `atom` node, refused for every other kind and for a quoted
/// atom.
///
/// Routed through `split_call_target` so the callee gate — the one place a
/// callee name is built — applies here exactly as it does everywhere else, and
/// an expression can never become a callee (SC26/SC32).
fn atom_identity(node: Node, source: &str) -> Option<String> {
    if node.kind() != "atom" {
        return None;
    }
    split_call_target(node, source).map(|(name, _)| name)
}

/// Whether `node` sits inside a type annotation rather than inside code.
fn in_type_context(node: Node) -> bool {
    let mut ancestor = node.parent();
    while let Some(parent) = ancestor {
        if TYPE_CONTEXTS.contains(&parent.kind()) {
            return true;
        }
        // Every form is a child of the file, so the walk is bounded by the
        // tree's depth and stops at the top rather than running off it.
        if parent.kind() == "source_file" {
            return false;
        }
        ancestor = parent.parent();
    }
    false
}
