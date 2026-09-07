//! Commands run by shell scripts.
//!
//! Shell reached `extract_node`'s generic arm, which already recovers its
//! declarations — `function_definition` and its `name` field are both in the
//! shared table — and stopped there. Measured on a script with `helper()` and
//! `run()` where `run` calls `helper` three times: **3 symbols** (`File`,
//! `run.sh::helper`, `run.sh::run`) and **0 calls**. Every shell function in
//! every indexed repository was therefore a node with no outgoing and no
//! incoming edges, which is indistinguishable from a script that does nothing.
//!
//! # A command word is a callee, whatever it names
//!
//! A shell script does not distinguish calling a function from running a
//! program: `helper foo` and `curl foo` are the same syntax, and only the set of
//! defined functions decides which is which — a set that is not knowable from
//! one file. So this module records the command word and lets resolution decide:
//! a word that names a function in the corpus resolves to it, and a word that
//! names `curl` resolves to nothing, exactly as `require()` in Solidity or
//! `console.log` in JavaScript already do. Filtering by a hand-written list of
//! builtins was rejected for the reason `no_builtin_table_admits_a_library_dsl_name`
//! states: a wrong entry permanently exempts a real defect, and a script is free
//! to define a function named `exit`.
//!
//! The gate that *is* applied is `is_callee_identity`, and it does the work that
//! matters. Read from the grammar: `./script.sh`, `/usr/bin/env`, `[` and `:`
//! are all `command_name` nodes whose text is not identifier-shaped, so each is
//! refused rather than recorded as a callee no symbol can carry (SC32). So is
//! `"$cmd" arg`, whose command name is an expansion: the callee is not in the
//! source text, and inventing one would be a confidently wrong edge.
//!
//! # What a wrapper costs
//!
//! `time helper`, `command helper`, `exec helper`, `sudo helper` and
//! `xargs helper` each parse as a command whose *name* is the wrapper and whose
//! *argument* is the real target, so the edge recorded is to the wrapper. That
//! is stated rather than patched: unwrapping would need a list of wrapper
//! commands, which is the hand-written table this module already declined, and
//! the argument still reaches the graph as a `Name` reference from the generic
//! identifier walk.

use tree_sitter::Node;

use crate::model::{ExtractedCall, ExtractedReference, ReferenceKind};
use crate::treesitter::{node_span, split_call_target};

use super::scope::enclosing_emitted_symbol;

/// Record the command `node` runs, if it runs one.
///
/// Called for every node in the tree, so the kind check is the first thing that
/// happens and the overwhelmingly common answer is "not a command".
pub fn extract_shell_call(
    node: Node,
    source: &str,
    file_symbol_name: &str,
    calls: &mut Vec<ExtractedCall>,
    references: &mut Vec<ExtractedReference>,
) {
    if node.kind() != "command" {
        return;
    }
    let Some(name_node) = node.child_by_field_name("name") else {
        return;
    };
    // Every command shape the grammar produces — a pipeline stage, a
    // `command_substitution`, a `case` branch, an `if` condition, a
    // `negated_command`, a subshell — wraps this same `command` node, so one
    // arm covers all of them and no shape needs an arm of its own. Verified by
    // parsing each: they differ in what encloses the command, never in the
    // command.
    let Some((callee_name, receiver_expr)) = split_call_target(name_node, source) else {
        return;
    };
    let caller_symbol = enclosing_emitted_symbol(node, source, "shell", file_symbol_name);
    references.push(ExtractedReference {
        name: callee_name.clone(),
        kind: ReferenceKind::Call,
        span: node_span(name_node),
        enclosing_symbol: caller_symbol.clone(),
        // `out=$(helper)` binds `out` to a *string*, always. The shell has one
        // value type and declares no types at all, so there is nothing for
        // receiver inference to look up and a binding here would be a fact
        // nothing consumes.
        assigned_to: None,
        // The mirrored call already carries the receiver; repeating it here would be a second copy of one fact.
        receiver_expr: None,
    });
    calls.push(ExtractedCall {
        caller_symbol,
        callee_name,
        // A shell command has no receiver: there is no object to reach it
        // through, and `split_call_target` can only produce one for grammars
        // with member access, which this is not.
        receiver_expr,
        span: node_span(node),
    });
}
