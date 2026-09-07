//! Calls made by Solidity code.
//!
//! Solidity reached `extract_node`'s generic arm, which already recovers its
//! declarations and stopped there. Measured on a `contract Vault` whose
//! `deposit` calls `_add`, emits an event and calls `require`: **4 symbols**
//! (`File`, `Vault`, `Vault._add`, `Vault.deposit`), **7 references** and
//! **0 calls**. On a language whose whole point is that one function calling
//! another moves money, the call graph was empty.
//!
//! # Every callee sits behind an `expression` wrapper
//!
//! `tree-sitter-solidity` wraps operands: a call is
//! `(call_expression function: (expression (identifier)))`, never
//! `(call_expression function: (identifier))`. Written against the predictable
//! shape this module would extract nothing at all, which is why the wrapper is
//! unwrapped once at the top of [`callee_of`] rather than in each arm.
//!
//! # What is a call here and what only looks like one
//!
//! Read from the grammar rather than assumed:
//!
//! * `uint256(a)` and `address(this)` are `type_cast_expression`, and
//!   `payable(x)` is `payable_conversion_expression` — separate node kinds, so
//!   a cast never reaches this module and no arm has to exclude it.
//! * `new Vault()` **is** a `call_expression`, whose function is a
//!   `new_expression`. It is recorded as a `Constructor` reference naming the
//!   contract, which is a symbol the emitter produces.
//! * `IThing(addr).ping()` casts to a contract type with ordinary call syntax,
//!   and nothing in the tree distinguishes it from calling a function named
//!   `IThing`. It is recorded, and it names the interface — which is the
//!   dependency the expression actually creates.
//! * `super.baseCall()` is refused. The callee is the *next* contract in the
//!   linearisation and the call site never says which one that is, so recording
//!   `baseCall` would let same-file resolution bind it to this contract's own
//!   override at full confidence — an edge that is wrong rather than missing,
//!   and the same refusal Java's `super.` and C#'s `base.` already make.
//! * A `modifier_invocation` is a call. `function f() onlyOwner` runs
//!   `onlyOwner`'s body around `f`'s, and `langdecl::solidity` emits the
//!   modifier so the edge joins to a symbol.
//! * `emit Transfer(…)` and `revert Unauthorized(…)` are not. An event and an
//!   error have no body to reach; see `langdecl::solidity` for the whole
//!   reasoning.

use tree_sitter::Node;

use crate::model::{ExtractedCall, ExtractedReference, ReferenceKind};
use crate::treesitter::{get_node_text, node_span, split_call_target};

use super::scope::{enclosing_emitted_symbol, receiver_from};

/// The receiver naming a base contract, which the call site never identifies.
const SUPER: &str = "super";

/// Record the call `node` makes, if it makes one.
pub fn extract_solidity_call(
    node: Node,
    source: &str,
    file_symbol_name: &str,
    calls: &mut Vec<ExtractedCall>,
    references: &mut Vec<ExtractedReference>,
) {
    let Some(site) = call_site(node, source) else {
        return;
    };
    let Some((callee_name, inner_receiver)) = split_call_target(site.name, source) else {
        return;
    };
    let caller_symbol = enclosing_emitted_symbol(node, source, "solidity", file_symbol_name);
    references.push(ExtractedReference {
        name: callee_name.clone(),
        kind: site.kind,
        span: node_span(site.name),
        enclosing_symbol: caller_symbol.clone(),
        // Solidity declares the type of every variable at its declaration
        // (`IThing t = IThing(addr);`), so the receiver a binding would carry
        // is already stated where the variable is declared; inferring a weaker
        // one from the call would add nothing the resolver does not have.
        assigned_to: None,
        // The mirrored call already carries the receiver; repeating it here would be a second copy of one fact.
        receiver_expr: None,
    });
    calls.push(ExtractedCall {
        caller_symbol,
        callee_name,
        receiver_expr: site.receiver.or(inner_receiver),
        span: node_span(node),
    });
}

/// A call this module is about to record.
struct Site<'tree> {
    /// The node whose text is the callee's *name*, passed through
    /// `split_call_target` so a name is built in one place (SC26/SC32).
    name: Node<'tree>,
    receiver: Option<String>,
    kind: ReferenceKind,
}

fn call_site<'tree>(node: Node<'tree>, source: &str) -> Option<Site<'tree>> {
    match node.kind() {
        "call_expression" => callee_of(node.child_by_field_name("function")?, source),
        // `function f(uint a) public onlyOwner(a)` — the modifier's name is the
        // invocation's first child.
        "modifier_invocation" => {
            let name = node.named_child(0).filter(|c| c.kind() == "identifier")?;
            Some(Site {
                name,
                receiver: None,
                kind: ReferenceKind::Call,
            })
        }
        _ => None,
    }
}

/// The callee behind a `call_expression`'s `function` field.
fn callee_of<'tree>(function: Node<'tree>, source: &str) -> Option<Site<'tree>> {
    // The grammar wraps every operand in `expression`; the callee is inside.
    let inner = if function.kind() == "expression" {
        function.named_child(0)?
    } else {
        function
    };
    match inner.kind() {
        "identifier" => Some(Site {
            name: inner,
            receiver: None,
            kind: ReferenceKind::Call,
        }),
        // `Math.sq(a)`, `a.sq()`, `this.deposit(a)`, `abi.encodePacked(a)`,
        // `IThing(addr).ping()`.
        "member_expression" => {
            let name = inner.child_by_field_name("property")?;
            let object = inner.child_by_field_name("object")?;
            let receiver = member_receiver(object, source)?;
            Some(Site {
                name,
                receiver,
                kind: ReferenceKind::Call,
            })
        }
        // `new Vault()`: the constructed contract is the callee's identity.
        "new_expression" => Some(Site {
            name: user_defined_type(inner.child_by_field_name("name")?)?,
            receiver: None,
            kind: ReferenceKind::Constructor,
        }),
        // A cast, a conversion or a call through a function-typed expression:
        // refused rather than recorded as its own source text, which would name
        // nothing (SC32). Nothing real is lost — the inner call of a chained
        // expression is its own node and is visited separately.
        _ => None,
    }
}

/// The receiver a member access is reached through, or `None` for the whole
/// call when the receiver is `super`.
///
/// The double `Option` is load-bearing: `Some(None)` is "a call with no
/// nameable receiver", while the outer `None` refuses the call outright.
fn member_receiver(object: Node, source: &str) -> Option<Option<String>> {
    // `super` and `this` are the two receivers with no variable behind them.
    // `this` is kept — it distinguishes an external self-call from a bare
    // internal one, and the resolver already reads it as a self-receiver —
    // while `super` is refused, because keeping the callee without it would
    // resolve to the override in this very contract.
    let unwrapped = if object.kind() == "expression" {
        object.named_child(0).unwrap_or(object)
    } else {
        object
    };
    if unwrapped.kind() == "identifier" && get_node_text(unwrapped, source) == SUPER {
        return None;
    }
    Some(receiver_from(unwrapped, source))
}

/// The identifier inside `new Vault()`'s type name.
fn user_defined_type(type_name: Node) -> Option<Node> {
    // `(type_name (user_defined_type (identifier)))`; an array or mapping type
    // has no identifier here and `new uint[](3)` allocates rather than
    // constructing, so both fall through to `None`.
    let user_defined = descend_while(type_name, &["type_name", "user_defined_type"])?;
    (user_defined.kind() == "identifier").then_some(user_defined)
}

/// Walk down single-child wrappers whose kinds are in `wrappers`.
fn descend_while<'tree>(node: Node<'tree>, wrappers: &[&str]) -> Option<Node<'tree>> {
    let mut current = node;
    // Bounded so a grammar change cannot turn this into an unbounded descent.
    for _ in 0..wrappers.len() {
        if !wrappers.contains(&current.kind()) {
            break;
        }
        current = current.named_child(0)?;
    }
    Some(current)
}
