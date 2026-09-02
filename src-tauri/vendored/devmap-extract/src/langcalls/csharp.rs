//! C# call extraction.
//!
//! Like Java, C# reached `extract_node`'s generic arm and produced declarations
//! and nothing else — measured at 0 `Calls` edges under this port *and* under
//! the Python implementation it replaces. New capability, not a restored
//! regression.
//!
//! C# declarations were checked separately and are **not** missing: a namespace,
//! class, struct, interface, constructor and method all reach the symbol table.
//! One pre-existing declaration-side inconsistency was found while pinning
//! identity here and is reported rather than worked around — a type nested in a
//! `namespace` is qualified `F::N.S` while its own methods are qualified
//! `F::S.M`, because the emitter only ever prefixes the *immediate* owner. This
//! module mirrors that behaviour exactly rather than correcting it, since an
//! edge must join to the symbol that exists, not to the one that should.

use super::jvm_dotnet::{receiver_of, record, CallSite};
use super::scope::receiver_from;
use crate::model::{ExtractedCall, ExtractedReference, ReferenceKind};
use crate::treesitter::get_node_text;
use tree_sitter::Node;

pub(crate) fn extract_csharp_call(
    node: Node,
    source: &str,
    file_symbol_name: &str,
    calls: &mut Vec<ExtractedCall>,
    references: &mut Vec<ExtractedReference>,
) {
    if let Some(site) = call_site(node, source) {
        record(site, source, "csharp", file_symbol_name, calls, references);
    }
}

fn call_site<'tree>(node: Node<'tree>, source: &str) -> Option<CallSite<'tree>> {
    match node.kind() {
        "invocation_expression" => {
            let function = node.child_by_field_name("function")?;
            // `nameof(x)` is a contextual operator that the grammar happens to
            // parse as an invocation. No method named `nameof` exists in any
            // corpus, so an edge would be a phantom callee of exactly the kind
            // SC17 removed. Its operand still reaches the graph as a `Name`
            // reference through the generic identifier walk.
            if function.kind() == "identifier" && get_node_text(function, source) == "nameof" {
                return None;
            }
            let (name, receiver) = invocation_target(function, source)?;
            Some(CallSite {
                call: node,
                name,
                receiver,
                kind: ReferenceKind::Call,
            })
        }
        // `new T()`, `new T<int>()`, `new N.T()`.
        //
        // `new int[3]` is an `array_creation_expression` and `new()` an
        // `implicit_object_creation_expression`; neither is handled, the first
        // because allocating an array invokes no constructor and the second
        // because the type is named nowhere in the expression.
        "object_creation_expression" => {
            let (name, scope) = type_target(node.child_by_field_name("type")?, source)?;
            Some(CallSite {
                call: node,
                name,
                receiver: scope,
                kind: ReferenceKind::Constructor,
            })
        }
        _ => None,
    }
}

/// `(callee name node, receiver)` for whatever stands in an invocation's
/// `function` field.
///
/// Extension methods need no arm of their own: `s.Trim()` is written and parsed
/// exactly like an instance call, and nothing at the call site distinguishes
/// the two. They arrive here as receiver-bearing calls, which is the honest
/// answer — an extension method that resolves to nothing is a receiver we could
/// not type, not a bare-name failure.
fn invocation_target<'tree>(
    function: Node<'tree>,
    source: &str,
) -> Option<(Node<'tree>, Option<String>)> {
    match function.kind() {
        "identifier" => Some((function, None)),
        // `Foo<int>()`: the type arguments are not part of the callee's
        // identity, the rule SC26 established for Rust's turbofish.
        "generic_name" => Some((generic_name_identifier(function)?, None)),
        // `o.M()`, `T.S()`, `this.M<int>()`, `System.Console.WriteLine()`.
        "member_access_expression" => {
            let name = name_identifier(function.child_by_field_name("name")?)?;
            let receiver = match function.child_by_field_name("expression") {
                // `base.M()` names the base class's `M`, which the call site
                // never identifies; recording callee `M` would let same-file
                // resolution bind it to this class's own override at full
                // confidence. Same refusal as Java's `super.`.
                Some(expression) if expression.kind() == "base" => return None,
                Some(expression) => member_receiver(expression, source),
                None => None,
            };
            Some((name, receiver))
        }
        // `a?.M()`: the callee hangs off a `member_binding_expression` and the
        // receiver is the `condition`. A chain such as `a?.b()?.d()` nests, and
        // each link is its own `invocation_expression`, so each is recorded
        // once from its own node.
        "conditional_access_expression" => {
            let binding = function
                .named_child(function.named_child_count().checked_sub(1)?)
                .filter(|last| last.kind() == "member_binding_expression")?;
            let name = name_identifier(binding.child_by_field_name("name")?)?;
            let receiver = function
                .child_by_field_name("condition")
                .and_then(|condition| member_receiver(condition, source));
            Some((name, receiver))
        }
        _ => None,
    }
}

/// A member-access receiver, reduced the way Java reduces `field_access`: a
/// dotted qualifier contributes its last segment, so `System.Console.WriteLine`
/// is reached through `Console` rather than through the whole path.
fn member_receiver(expression: Node, source: &str) -> Option<String> {
    match expression.kind() {
        "member_access_expression" => expression
            .child_by_field_name("name")
            .and_then(|name| receiver_from(name, source))
            .or_else(|| receiver_from(expression, source)),
        _ => receiver_of(expression, source, "this"),
    }
}

/// `(name node, qualifier)` for the type a `new` expression constructs.
fn type_target<'tree>(
    type_node: Node<'tree>,
    source: &str,
) -> Option<(Node<'tree>, Option<String>)> {
    match type_node.kind() {
        "identifier" => Some((type_node, None)),
        "generic_name" => Some((generic_name_identifier(type_node)?, None)),
        // `new System.Collections.Generic.List<int>()`: right-nested, so the
        // constructed type is the `name` field and the qualifier is the
        // innermost scope reachable through it.
        "qualified_name" => {
            let (name, inner) = type_target(type_node.child_by_field_name("name")?, source)?;
            let scope = inner.or_else(|| {
                type_node
                    .child_by_field_name("qualifier")
                    .and_then(|qualifier| qualifier_tail(qualifier, source))
            });
            Some((name, scope))
        }
        // `new int[3]` never reaches here, and a `predefined_type` names no
        // user symbol, so it is refused rather than recorded.
        _ => None,
    }
}

/// The last segment of a dotted qualifier: `Generic` for
/// `System.Collections.Generic`. The whole path would name a scope no binding
/// carries, the same reduction Java applies to `field_access`.
fn qualifier_tail(qualifier: Node, source: &str) -> Option<String> {
    match qualifier.kind() {
        "qualified_name" => qualifier
            .child_by_field_name("name")
            .and_then(|name| receiver_from(name, source)),
        _ => receiver_from(qualifier, source),
    }
}

/// The bare identifier inside `Foo<int>`.
fn generic_name_identifier(generic: Node) -> Option<Node> {
    generic
        .named_child(0)
        .filter(|child| child.kind() == "identifier")
}

/// An identifier, or the identifier inside a generic name.
fn name_identifier(name: Node) -> Option<Node> {
    match name.kind() {
        "identifier" => Some(name),
        "generic_name" => generic_name_identifier(name),
        _ => None,
    }
}
