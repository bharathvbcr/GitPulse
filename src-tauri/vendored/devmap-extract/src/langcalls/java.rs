//! Java call extraction.
//!
//! Java reached `extract_node`'s generic arm, which emits declarations only, so
//! **no Java call was ever extracted** — not by this port and not by the Python
//! implementation it replaces, both measured at 0 `Calls` edges on a file whose
//! only statement is one method calling another. This is new capability rather
//! than a restored regression, and it is the difference between `impact`,
//! `trace`, dead code and the PDG answering for Java from a call graph and
//! answering from nothing.

use super::jvm_dotnet::{receiver_of, record, CallSite};
use super::scope::receiver_from;
use crate::model::{ExtractedCall, ExtractedReference, ReferenceKind};
use tree_sitter::Node;

pub(crate) fn extract_java_call(
    node: Node,
    source: &str,
    file_symbol_name: &str,
    calls: &mut Vec<ExtractedCall>,
    references: &mut Vec<ExtractedReference>,
) {
    if let Some(site) = call_site(node, source) {
        record(site, source, "java", file_symbol_name, calls, references);
    }
}

fn call_site<'tree>(node: Node<'tree>, source: &str) -> Option<CallSite<'tree>> {
    match node.kind() {
        // `m()`, `o.m()`, `T.s()`, `o.<T>m()` and every link of a chain: the
        // grammar gives them all one shape with the callee in a `name` field,
        // so none needs its own arm and `<T>` cannot leak into the callee the
        // way SC26's turbofish did.
        "method_invocation" => {
            let name = node.child_by_field_name("name")?;
            let receiver = match node.child_by_field_name("object") {
                // `super.m()` names the *superclass's* `m`, and nothing at the
                // call site says which class that is. Recording callee `m`
                // would let same-file resolution bind it to the override in
                // this very class at DETERMINISTIC confidence — a self-edge
                // that is both wrong and, because it makes every overriding
                // method look called, a silent defeat of dead-code analysis.
                // Refusing it is SC17's rule: no edge beats a confident wrong
                // one.
                Some(object) if object.kind() == "super" => return None,
                Some(object) => java_receiver(object, source),
                None => None,
            };
            Some(CallSite {
                call: node,
                name,
                receiver,
                kind: ReferenceKind::Call,
            })
        }
        // `new T()`, `new p.q.T<>()`, `new Outer.Inner()`.
        "object_creation_expression" => {
            let (name, scope) = java_type_target(node.child_by_field_name("type")?)?;
            Some(CallSite {
                call: node,
                name,
                receiver: scope.and_then(|scope| receiver_from(scope, source)),
                kind: ReferenceKind::Constructor,
            })
        }
        // Deliberately absent:
        //
        // * `array_creation_expression` (`new int[3]`, `new Foo[3]`) invokes no
        //   constructor, so an edge would name a callee that cannot exist —
        //   SC17's rule for Go composite literals over predeclared types. The
        //   element type still reaches the graph as a `Type` reference.
        // * `explicit_constructor_invocation` (`this(…)`, `super(…)`) names a
        //   constructor *overload* the call site does not identify. Every
        //   constructor of a class shares one name, so `this(1)` could only be
        //   recorded as a call to the class, and `super()` names a superclass
        //   the expression never mentions. Both would add ambiguity without
        //   adding a distinguishable target.
        _ => None,
    }
}

/// The receiver of a Java method invocation.
///
/// A `field_access` object contributes its *last* segment:
/// `java.util.Collections.emptyList()` is reached through `Collections`, and
/// recording `java.util.Collections` would name a receiver no binding carries.
/// This is the reduction `split_call_target` already performs for C++ through
/// `qualified_identifier` and for Go through `qualified_type`.
fn java_receiver(object: Node, source: &str) -> Option<String> {
    match object.kind() {
        "field_access" => object
            .child_by_field_name("field")
            .and_then(|field| receiver_from(field, source))
            .or_else(|| receiver_from(object, source)),
        _ => receiver_of(object, source, "this"),
    }
}

/// `(constructor name node, qualifying node)` for the type a `new` expression
/// constructs.
fn java_type_target(type_node: Node) -> Option<(Node, Option<Node>)> {
    match type_node.kind() {
        "type_identifier" => Some((type_node, None)),
        // `new ArrayList<>()`: type arguments are not part of the identity.
        "generic_type" => java_type_target(type_node.named_child(0)?),
        // `new Outer.Inner()`, `new java.util.ArrayList<>()`: nested to the
        // left, so the constructed type is the last segment — the mirror image
        // of C++'s right-nested `qualified_identifier`.
        "scoped_type_identifier" => {
            let count = type_node.named_child_count();
            let last = type_node.named_child(count.checked_sub(1)?)?;
            let scope = count
                .checked_sub(2)
                .and_then(|index| type_node.named_child(index))
                .and_then(scope_tail);
            let (name, inner_scope) = java_type_target(last)?;
            Some((name, inner_scope.or(scope)))
        }
        _ => None,
    }
}

/// The last segment of a dotted scope: `util` for `java.util`.
///
/// The whole path would name a scope no binding carries, and it is the same
/// reduction `java_receiver` applies to a `field_access` object and
/// `split_call_target` applies to C++'s `qualified_identifier`.
fn scope_tail(scope: Node) -> Option<Node> {
    match scope.kind() {
        "scoped_type_identifier" => scope.named_child(scope.named_child_count().checked_sub(1)?),
        _ => Some(scope),
    }
}
