//! Caller attribution for the languages served by the generic declaration arm.
//!
//! A call edge names its caller by qualified name, and that name is a join key:
//! an edge whose `source_symbol` matches no node's `qualified_name` is an
//! orphan, which is the defect SC9 and SC10 were both instances of. So the name
//! this module builds must be *the same string* the symbol emitter builds for
//! the enclosing declaration — not merely a plausible one.
//!
//! It is now that string by construction: `enclosing_emitted_symbol` calls
//! `langdecl::declaration_of`, the function `extract_node` emits from. The
//! previous version transcribed `generic_symbol_kind` and `generic_enclosing_type`
//! into a second table here, and a transcription can only be as current as the
//! last person to notice it had fallen behind. That was already load-bearing —
//!
//! | shape | emitted symbol | `enclosing_callable_qualified` |
//! |---|---|---|
//! | Java method in an `interface` | `F::I.d` | `F::d` |
//! | method of a Scala `object` | `F::Registry.register` | `F::register` |
//! | call in a Swift `init` body | `F::Runner` | `F::` (file) |
//!
//! — and it stopped being maintainable by inspection the moment declarations
//! became per-language: a Swift `extension` owns members without being a symbol,
//! and a Kotlin `fun Person.extra()` takes its owner from a receiver that is not
//! an ancestor at all. Neither rule is expressible in a node-kind table.
//!
//! The agreement is still pinned behaviourally by
//! `every_caller_symbol_names_an_emitted_symbol` in
//! `tests/langcalls_swift_scala.rs` and by the orphan checks in
//! `tests/declarations.rs`, which read the emitter's own output rather than
//! restating the rule.

use tree_sitter::Node;

use crate::treesitter::{get_node_text, is_callee_identity};

/// Qualified name of the nearest declaration enclosing `node` that the emitter
/// names, or `None` at file scope.
///
/// Asks `langdecl::declaration_of` — the *same* function `extract_node` emits
/// from — rather than restating its rules. This module previously carried a
/// hand-transcribed copy of `generic_symbol_kind` plus a copy of
/// `generic_enclosing_type`, and a transcription that falls behind the emitter
/// is an orphaned call edge per site: the whole SC9/SC10 failure. Since the
/// emitter now answers per language (a Swift `extension` owns members without
/// being a symbol, a Kotlin `fun Person.extra()` is owned by `Person`), a copy
/// could not have been kept correct by inspection at all.
///
/// Nearest wins whether it is a callable or a type. A Swift `init` body and a
/// SwiftUI `var body` are not declarations the emitter names, so their calls are
/// attributed to the type that encloses them — a symbol that exists, and a
/// strictly better answer than the file, which is where the shared helper lands
/// them. `None` means the file itself is the caller, exactly as elsewhere.
pub(crate) fn enclosing_emitted_symbol(
    node: Node,
    source: &str,
    lang: &str,
    file_symbol_name: &str,
) -> Option<String> {
    let mut ancestor = node.parent();
    while let Some(parent) = ancestor {
        // A declaration the emitter cannot name emits no symbol, so keep
        // walking rather than inventing one.
        if let Some(declaration) = crate::langdecl::declaration_of(lang, parent, source) {
            return Some(declaration.qualified(file_symbol_name));
        }
        ancestor = parent.parent();
    }
    None
}

/// Longest receiver expression worth carrying on a call.
///
/// A receiver is looked up as a *variable name* (`receiver_types`,
/// `scoped_receiver_types`), so nothing longer than an identifier can ever
/// match. Swift method chains make that limit matter: the receiver of the
/// outermost `.onAppear` in a SwiftUI `body` is the entire view expression
/// underneath it, so a deeply chained file would store a large fraction of
/// itself once per link.
const MAX_RECEIVER_BYTES: usize = 96;

/// A receiver expression, bounded, or `None` when there is nothing to record.
///
/// Truncation keeps the call *receiver-bearing*, which is the property that
/// matters: a method call whose receiver is dropped becomes indistinguishable
/// from a bare call, and the resolution ladder would then let `.padding()`
/// resolve to a same-file `padding` function at deterministic confidence — the
/// SC9 class of confidently-wrong edge. An over-long or multi-line receiver
/// resolves no better truncated than whole; both miss every lookup and land in
/// the `uninferred_receiver` tier, which is where an unnameable receiver
/// belongs.
pub(crate) fn clamp_receiver(text: &str) -> Option<String> {
    let trimmed = text.trim();
    if trimmed.is_empty() {
        return None;
    }
    let single_line = !trimmed.contains('\n');
    if single_line && trimmed.len() <= MAX_RECEIVER_BYTES {
        return Some(trimmed.to_string());
    }
    let head = trimmed.lines().next().unwrap_or(trimmed).trim_end();
    let mut end = head.len().min(MAX_RECEIVER_BYTES);
    while end > 0 && !head.is_char_boundary(end) {
        end -= 1;
    }
    let clipped = &head[..end];
    if clipped.is_empty() {
        return None;
    }
    Some(format!("{clipped}…"))
}

/// A receiver taken from a node, unwrapping Swift's `?` and `!` postfixes.
///
/// `opt?.warm()` and `opt!.warm()` are calls on `opt`, and the binding the
/// resolver would match is keyed by the bare name, so leaving the operator on
/// the text costs a receiver-type resolution for no gain.
pub(crate) fn receiver_from(node: Node, source: &str) -> Option<String> {
    let text = get_node_text(node, source);
    let trimmed = text.trim();
    let unwrapped = trimmed.trim_end_matches(['!', '?']);
    if is_callee_identity(unwrapped) {
        return Some(unwrapped.to_string());
    }
    clamp_receiver(trimmed)
}
