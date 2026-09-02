//! Per-language declaration identity, kind and visibility.
//!
//! # Why this exists
//!
//! `extract_node`'s generic arm answered three separate questions from two
//! guesses: a declaration's *kind* from its node kind alone, its *name* from a
//! `name` field, and its *visibility* from a substring scan of the node's whole
//! source text with `!name.starts_with('_')` behind it. Each guess is right for
//! some of the 35 languages and wrong for most, and the failures do not look
//! like failures — they look like answers.
//!
//! Measured on a two-file Swift/Kotlin fixture before this module existed:
//!
//! * `enum Mode` and `enum class Mode` both reported `Class`, and a Kotlin
//!   `interface` reported `Class`, because tree-sitter-swift and
//!   tree-sitter-kotlin-ng spell all of them `class_declaration` and put the
//!   distinguishing token in a field the kind table never read.
//! * `extension Person` emitted a **second** `App.swift::Person` node, so every
//!   call naming that type resolved ambiguously at 0.2 confidence instead of
//!   1.0 — a duplicate qualified name is a broken join key (SC14).
//! * `fun Person.extra()` emitted `Main.kt::extra`, losing the receiver Go
//!   receivers keep under SC9 and Rust `impl` methods keep under SC11.
//! * Dart emitted **no** function or method symbol at all, because
//!   tree-sitter-dart hangs the `name` field off a `signature` child.
//! * Every R function was named `function`, because tree-sitter-r's `name` field
//!   on `function_definition` points at the *keyword*.
//! * Visibility was read from `get_node_text(node, ..)`, which is the whole
//!   subtree including bodies, so one `private` member made its enclosing class
//!   private (`Main.kt::Runner`, `App.swift::Person`) — a false-positive
//!   dead-code candidate — while a declaration with no modifier at all reported
//!   `is_exported = 1` and was exempt. `dev map dead` returned **0 rows** on a
//!   fixture with an uncalled method. That is the SC3c shape: a check that
//!   could not run reporting what a check that ran and passed reports.
//!
//! # The structure
//!
//! One registry, mirroring `langcalls`: a language answers for its *whole*
//! declaration surface or it does not answer at all, exactly as
//! `c_family_declaration` already did for C, C++, Objective-C and CUDA. This
//! replaces the inline `if c_family { … } else { … }` fork in `extract_node`
//! rather than adding a third branch beside it.
//!
//! The registry is also the **single owner** of declaration identity, which is
//! what keeps call extraction joinable. `langcalls::scope` used to transcribe
//! `generic_symbol_kind` into a second table so a call's `caller_symbol` would
//! match the emitter's `qualified_name`; it now asks `declaration_of` the same
//! question the emitter asks, so the two cannot drift. A transcription that
//! falls behind is an orphaned edge per call site (SC9, SC10).

use tree_sitter::Node;

use crate::model::{SymbolKind, WiringKind};

pub(crate) mod dart;
pub(crate) mod kotlin;
pub(crate) mod r;
pub(crate) mod swift;

/// What the declaration arm emits for one node.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Declaration {
    /// The kind as *declared*. `Function` with an owner becomes `Method` at
    /// emission; keeping the declared kind here means the owner rule lives in
    /// one place rather than in every language module.
    pub declared_kind: SymbolKind,
    /// The type, protocol, object or extension that owns this declaration.
    pub owner: Option<String>,
    pub name: String,
}

impl Declaration {
    /// A declaration, or `None` when the name is empty.
    ///
    /// An empty name has been the silent failure mode of every path here: it
    /// produces a node whose qualified name is `file::`, which no call can join
    /// to and which collides with every other such node in the file.
    pub(crate) fn new(
        declared_kind: SymbolKind,
        owner: Option<String>,
        name: impl Into<String>,
    ) -> Option<Self> {
        let name = name.into();
        (!name.is_empty()).then_some(Self {
            declared_kind,
            owner: owner.filter(|owner| !owner.is_empty()),
            name,
        })
    }

    /// The identity a call must join to.
    pub(crate) fn qualified(&self, file_symbol_name: &str) -> String {
        match &self.owner {
            Some(owner) => format!("{file_symbol_name}::{owner}.{}", self.name),
            None => format!("{file_symbol_name}::{}", self.name),
        }
    }

    /// The containing symbol: the owning type, or the file.
    pub(crate) fn parent_symbol(&self, file_symbol_name: &str) -> String {
        match &self.owner {
            Some(owner) => format!("{file_symbol_name}::{owner}"),
            None => file_symbol_name.to_string(),
        }
    }

    /// The kind actually emitted: a callable with an owner is a method.
    pub(crate) fn emitted_kind(&self) -> SymbolKind {
        if self.owner.is_some() && self.declared_kind == SymbolKind::Function {
            SymbolKind::Method
        } else {
            self.declared_kind
        }
    }
}

/// The declaration `node` is, under `lang`'s own rules.
///
/// The single owner of declaration identity. `extract_node` emits from it and
/// `langcalls::scope` attributes callers from it, so a call's `caller_symbol` is
/// the string the emitter produced rather than a plausible reconstruction of it.
pub(crate) fn declaration_of(lang: &str, node: Node, source: &str) -> Option<Declaration> {
    match lang {
        "swift" => swift::declaration(node, source),
        "kotlin" => kotlin::declaration(node, source),
        "dart" => dart::declaration(node, source),
        "r" => r::declaration(node, source),
        _ if crate::treesitter::is_c_family_grammar(lang) => {
            crate::treesitter::c_family_declaration(node, source)
        }
        _ => generic(node, source),
    }
}

/// The declaration rule for a language with no module of its own.
///
/// Kept as the explicit fallback rather than as an implicit fallthrough: a
/// language reaching this is being answered by a node-kind table and a
/// `name` field, which is a stated approximation, not a per-language rule.
pub(crate) fn generic(node: Node, source: &str) -> Option<Declaration> {
    let (declared_kind, name) = crate::treesitter::generic_declaration(node, source)?;
    Declaration::new(
        declared_kind,
        crate::treesitter::generic_enclosing_type(node, source),
        name,
    )
}

/// Whether `node`'s declaration is reachable from outside the indexed corpus.
///
/// `true` exempts the symbol from dead-code analysis, so the honest default is
/// `true` and every `false` must rest on evidence the language actually
/// provides. Swift and Kotlin provide a visibility keyword; the generic
/// fallback provides only a leading-underscore convention, and says so.
pub(crate) fn is_exported_of(lang: &str, node: Node, source: &str, name: &str, path: &str) -> bool {
    match lang {
        "swift" => swift::is_exported(node, source),
        "kotlin" => kotlin::is_exported(node, source),
        // Dart's privacy *is* the leading underscore — `_helper` is
        // library-private and nothing else is — so the generic fallback is the
        // language rule here, not an approximation of it.
        _ if crate::treesitter::is_c_family_grammar(lang) => {
            crate::treesitter::c_family_is_exported(node, source, path)
        }
        _ => crate::treesitter::generic_is_exported(node, source, name),
    }
}

/// Why a declaration is reachable without an observable call site.
///
/// The counterweight to reading visibility at all: the moment a symbol can be
/// reported *not* exported it can be reported dead, and a false positive there
/// is a proposal to delete working code. Every reason below is read from the
/// declaration itself — an attribute, a modifier, a supertype, the syntax of the
/// use site the language permits — never guessed from a name in isolation.
///
/// `RuntimeEntryPoint` means something outside the corpus calls it.
/// `StructuralExempt` means the language permits a use site that names the
/// symbol without naming a path this extractor can record, so silence is not
/// evidence.
pub(crate) fn exemption(
    lang: &str,
    node: Node,
    source: &str,
    declaration: &Declaration,
) -> Option<(WiringKind, &'static str)> {
    match lang {
        "swift" => swift::exemption(node, source, declaration),
        "kotlin" => kotlin::exemption(node, source, declaration),
        "dart" => dart::exemption(node, source, declaration),
        _ => None,
    }
}

/// The **dotted owner path** of the nearest declaration `node` belongs to.
///
/// `enclosing_owner` yields only that declaration's own name, which is right for
/// a method — a call writes `Person.greet`, not the outer type — but wrong for a
/// member of a type that is routinely nested. Measured on 126 real `.swift`
/// files: emitting enum cases owned by their enum's bare name produced **21
/// duplicate qualified names**, every one of them a case of a nested
/// `enum CodingKeys`, of which one Codable-heavy file declares four. A duplicate
/// qualified name is a broken join key (SC14), so a member of a nested type
/// takes the identity the emitter gave that type, outer segments and all.
///
/// Scoped to members whose owners nest as a matter of course, rather than
/// applied to every declaration: widening it would rewrite the identity of every
/// method of every nested type, which is the open half of SC14 and would move
/// joins that currently work.
pub(crate) fn enclosing_owner_path(
    node: Node,
    source: &str,
    declaration: fn(Node, &str) -> Option<Declaration>,
) -> Option<String> {
    let mut ancestor = node.parent();
    while let Some(parent) = ancestor {
        if let Some(owner) = declaration(parent, source) {
            return Some(match owner.owner {
                Some(outer) => format!("{outer}.{}", owner.name),
                None => owner.name,
            });
        }
        ancestor = parent.parent();
    }
    None
}

/// The nearest ancestor that owns declarations, by a language's own rule.
///
/// `shallow` answers "what does this node declare, ignoring its ancestors" and
/// must never consult ancestors itself, or this walk recurses. Returning `None`
/// at a callable is deliberate and matches `generic_enclosing_type`: a function
/// declared inside a function is not owned by a type.
pub(crate) fn enclosing_owner(
    node: Node,
    source: &str,
    shallow: fn(Node, &str) -> Option<(SymbolKind, String)>,
) -> Option<String> {
    let mut ancestor = node.parent();
    while let Some(parent) = ancestor {
        if let Some((kind, name)) = shallow(parent, source) {
            if kind == SymbolKind::Function {
                return None;
            }
            return (!name.is_empty()).then_some(name);
        }
        ancestor = parent.parent();
    }
    None
}
