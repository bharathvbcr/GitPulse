use devmap_extract::model::*;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::sync::Arc;

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub enum LangFamily {
    Python,
    JsTs,
    Go,
    Rust,
    CStyle,
    /// Swift and Kotlin are primary languages for this repository and each gets
    /// its own bucket rather than sharing `Generic`.
    ///
    /// Candidates are filtered by `*candidate_family == family`
    /// (`resolver.rs:507`), so `Generic` is not a neutral default — it is one
    /// shared namespace spanning Swift, Kotlin, Ruby, PHP, Lua, R, COBOL and
    /// Solidity. A bare `run()` in Swift could resolve to a Ruby `run()` at
    /// full confidence and the graph would carry a cross-language edge that
    /// cannot exist. Harmless only while neither language extracted calls;
    /// separating them before that lands is why this is here now.
    Swift,
    Kotlin,
    Ruby,
    Php,
    Scala,
    Lua,
    R,
    Dart,
    /// Languages that extract **no calls**. Sharing one bucket is harmless here
    /// only because a language with no call sites contributes no edges to
    /// mis-resolve — the moment one gains call extraction it must get its own
    /// variant above, which `every_call_extracting_language_owns_its_bucket`
    /// enforces rather than leaving to memory.
    Generic,
}

impl LangFamily {
    pub fn from_lang(lang: &str) -> Self {
        match lang {
            "python" => LangFamily::Python,
            "javascript" | "typescript" | "tsx" | "jsx" => LangFamily::JsTs,
            "go" => LangFamily::Go,
            "rust" => LangFamily::Rust,
            // `cuda` was absent until SC31 gave the C family a call graph, at
            // which point its omission became observable: CUDA fell to
            // `Generic`, cross-family resolution never fired, and every
            // `cuda_forward` call sat in `generation_unresolved` while an
            // otherwise identical C->C cross-file call resolved. Metal needs no
            // entry here — it rides the `cpp` grammar key.
            "c" | "cpp" | "csharp" | "java" | "objc" | "cuda" => LangFamily::CStyle,
            "swift" => LangFamily::Swift,
            "kotlin" => LangFamily::Kotlin,
            "ruby" => LangFamily::Ruby,
            "php" => LangFamily::Php,
            "scala" => LangFamily::Scala,
            // Luau shares Lua's bucket deliberately: it is a Lua superset and a
            // call genuinely can cross between them.
            "lua" | "luau" => LangFamily::Lua,
            "r" => LangFamily::R,
            "dart" => LangFamily::Dart,
            _ => LangFamily::Generic,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub enum Resolution {
    SameFile {
        target_symbol: String,
        target_file: String,
    },
    ImportScoped {
        target_symbol: String,
        target_file: String,
        imported_from: String,
    },
    ReceiverType {
        target_symbol: String,
        target_file: String,
        receiver_type: String,
    },
    UniqueGlobal {
        target_symbol: String,
        target_file: String,
        family: LangFamily,
    },
    AmbiguousGlobal {
        candidates: Vec<(String, String)>, // (file_path, symbol_name)
        family: LangFamily,
    },
    Unresolved {
        reason: String,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ResolvedEdge {
    pub source_file: String,
    pub target_file: String,
    pub source_symbol: String,
    pub target_symbol: String,
    pub edge_kind: EdgeKind,
    pub confidence: Confidence,
    /// Shared, never cloned per edge.
    ///
    /// One ambiguous call with N candidates fans out into N edges. Giving each
    /// edge its own copy of the N-element candidate list makes a single call
    /// site cost N² owned strings: on a Go-heavy repository that measured
    /// 72.6M pairs and 8.5 GiB of the 10.45 GiB peak, held across the whole
    /// sort. `Arc` shares one allocation across the fan-out.
    ///
    /// `Arc<T>` delegates `Debug`, `PartialEq` and (via serde's `rc` feature)
    /// `Serialize` to `T`, so the sort comparator, the dedup predicate and any
    /// serialized form are byte-for-byte what they were before.
    pub resolution: Option<Arc<Resolution>>,
    pub details: Option<String>,
}

/// Why a call produced no edge.
///
/// SC18. Every call that failed the ladder used to be recorded identically, so
/// 380k rows of `len`, `append`, `Fatalf` and `useState` — none of which *can*
/// resolve, because no indexed file declares them — sat alongside the genuine
/// failures that indicate a defect. That made the ledger unusable as a signal:
/// SC17's two extraction bugs were only found by hand-reading the top-N.
///
/// The distinction is drawn from evidence, never from a guess about what a name
/// looks like. Anything without evidence stays `Unresolved`, so the tier that
/// means "we failed" can only ever over-report, never hide a real defect.
///
/// Deliberately **not** a `Resolution` variant. `Resolution` describes how an
/// edge resolved and is carried on every `ResolvedEdge`, where it feeds the sort
/// comparator, the dedup predicate and the determinism digest. This describes
/// why there is no edge, which is a different question with a different owner.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord)]
pub enum UnresolvedClass {
    /// Declared by the language itself, per its specification — `len` and
    /// `append` in Go, `print` in Python. A closed set, so membership is a fact
    /// rather than a heuristic. No indexed file can ever declare these.
    Builtin,
    /// Supplied by the *runtime* that hosts the program, not by the language:
    /// `setTimeout`, `fetch`, `structuredClone`, `require`.
    ///
    /// Separate from `Builtin` because the authority behind the claim is
    /// different, and the authority is what makes the classification worth
    /// anything. No edition of ECMA-262 declares `setTimeout`; WHATWG HTML and
    /// the Node.js global-objects documentation do. Folding these into
    /// `Builtin` would say "the language declares this" about something the
    /// language says nothing about — a claim not in evidence, which is the one
    /// thing this enum must never do.
    ///
    /// `environment` names the runtime that is the authority for this entry:
    /// `"web"`, `"node"` or `"web+node"`. It is the counterpart of `External`'s
    /// module — the cited source, carried so the tier can be audited instead of
    /// trusted.
    HostGlobal { environment: String },
    /// A bare call to a name the *enclosing symbol itself* declares: a
    /// parameter, a method receiver, or a value it constructed.
    ///
    /// `handler("…")` inside `fn t() { let handler = |s| …; }`, or `fn(x)`
    /// where `fn` is that function's own parameter. The callee is a value in
    /// scope, invoked as a callback; there is no cross-file symbol to find, so
    /// the ladder failing is the correct outcome rather than a defect.
    ///
    /// The evidence is the enclosing scope's own binding table and nothing
    /// else. Deliberately **not** the file-wide one: a binding belongs to one
    /// scope, and reusing a file-wide fallback here is exactly the SC9/SC25
    /// shape that once declared a local type's method external at full
    /// confidence. See the lookup in `classify_unresolved`.
    LocalBinding,
    /// Bound by an import whose module specifier did not resolve to any indexed
    /// file. The import statement is the evidence: the name demonstrably comes
    /// from outside the corpus, so failing to resolve it is correct behaviour.
    ///
    /// Covers two shapes, both import-proven: a bare call to an imported name
    /// (`useState()`), and a method on a receiver that is either an imported
    /// module handle (`strings.TrimSpace()`) or a value whose *declared type*
    /// comes from an import (`t.Fatalf()` where `t` is a `*testing.T`).
    External { module: String },
    /// A method call whose receiver exists but could not be typed.
    ///
    /// `expect(...).toBe(...)`, `value.unwrap()`, `items.append(x)` — the
    /// receiver is an expression or an untyped local, so naming its owner needs
    /// real type inference, which a syntax-directed extractor does not do.
    ///
    /// Split out because it is a *known structural limitation*, not a defect.
    /// Leaving it merged with `Unresolved` is what made that tier unreadable:
    /// this is by far the largest group, and it drowned the bare-name failures
    /// that actually indicate an extraction or resolution bug.
    UninferredReceiver,
    /// A bare-name call, in a corpus that indexes every file, that its
    /// enclosing scope does not declare and that matched no language builtin,
    /// no host global and no import.
    ///
    /// **This is the only tier that indicates a defect** — every other outcome
    /// is explained by evidence. It is the tier to read when hunting bugs.
    Unresolved,
}

impl UnresolvedClass {
    /// Stable name for persistence and reporting.
    pub fn label(&self) -> &'static str {
        match self {
            UnresolvedClass::Builtin => "builtin",
            UnresolvedClass::HostGlobal { .. } => "host_global",
            UnresolvedClass::LocalBinding => "local_binding",
            UnresolvedClass::External { .. } => "external",
            UnresolvedClass::UninferredReceiver => "uninferred_receiver",
            UnresolvedClass::Unresolved => "unresolved",
        }
    }
}

/// A call the resolution ladder could not attribute to any target.
///
/// R5 forbids silence: dropping these makes "we could not resolve this call"
/// indistinguishable from "no call exists here", which silently understates
/// both the call graph and every liveness conclusion drawn from it. Kept out of
/// `edges` deliberately — an unresolved call has no target node to point at, so
/// materialising one would invent graph structure.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct UnresolvedReference {
    pub source_file: String,
    pub source_symbol: String,
    pub callee_name: String,
    pub resolution: Resolution,
    /// Why this call has no edge. See `UnresolvedClass`.
    pub class: UnresolvedClass,
    /// The expression the call was made on, or `None` for a bare call.
    ///
    /// Carried so the classification can be audited rather than trusted: it is
    /// what makes "is the `unresolved` tier really only bare names" a query
    /// instead of an instrumented rebuild.
    pub receiver: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ResolutionResult {
    pub edges: Vec<ResolvedEdge>,
    pub receiver_types: BTreeMap<String, String>, // var_name -> type_name (deterministic R4)
    pub reexport_chains: BTreeMap<String, String>, // symbol -> resolved_target
    /// Calls seen but not attributed. Deterministically ordered (R4).
    pub unresolved: Vec<UnresolvedReference>,
}
