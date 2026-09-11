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
    Erlang,
    Nix,
    Pascal,
    /// `sh`, `bash` and `zsh` all answer `detect_language` with `"shell"`, so
    /// one variant covers the family the grammar already merges.
    Shell,
    Solidity,
    Sql,
    /// Languages that extract **no calls**.
    ///
    /// Sharing one bucket was justified on the grounds that a language with no
    /// call sites contributes no edges to mis-resolve. That reasoning is sound
    /// and the premise kept going stale: embedded `<script>` extraction gave
    /// `.svelte`, `.vue`, `.astro` and `.liquid` files real calls while they
    /// still sat here, and a Svelte function bound to a Solidity contract
    /// method at 0.9 confidence — measured, not hypothesised.
    ///
    /// So `Generic` is now **inert** rather than merely unpopulated:
    /// [`LangFamily::admits`] refuses every cross-file resolution into or out of
    /// it. A language that lands here by mistake now produces a *missing* edge
    /// and an honest `generation_unresolved` row instead of a confident wrong
    /// one, which is the direction SC9 settled on — abstaining is correct,
    /// answering with the winner of a race is not.
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
            // Template languages whose code lives in an embedded `<script>`.
            //
            // `crates/devmap-extract/src/embedded.rs` routes those regions back
            // through the TypeScript or JavaScript grammar under the outer
            // file's own path, so the symbols and calls recorded against a
            // `.svelte` file *are* JS/TS ones and legitimately resolve against
            // `.ts` and `.js` siblings. `LanguageSpec::embedded` names exactly
            // `typescript`/`tsx`/`javascript` (plus `css`/`html`, which no
            // linked grammar reads) for all four, and
            // `an_embedded_script_host_shares_its_script_family` asserts that
            // rather than trusting this comment.
            //
            // Before this arm they were `Generic`, which cost both directions:
            // `Widget.svelte::renderWidget` resolved to
            // `Vault.sol::Vault.helperOnlyInSolidity` at 0.9, and the same
            // file's call to a real `helpers.ts` export produced no edge at all.
            "svelte" | "vue" | "astro" | "liquid" => LangFamily::JsTs,
            "erlang" => LangFamily::Erlang,
            "nix" => LangFamily::Nix,
            "pascal" => LangFamily::Pascal,
            "shell" => LangFamily::Shell,
            "solidity" => LangFamily::Solidity,
            "sql" => LangFamily::Sql,
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

    /// Whether a call in `self` may resolve to a declaration in `other`.
    ///
    /// The single owner of the cross-family rule, because it was previously
    /// spelled `*candidate_family == family` in four places and each one had to
    /// be correct independently.
    ///
    /// Two clauses, and the second is the one that is new. Equality keeps a
    /// Python call from binding a Go function. `self != Generic` keeps a
    /// language that was *misfiled* into the catch-all from binding another
    /// language that was misfiled into the same catch-all — which is not a
    /// hypothetical: `svelte` and `solidity` shared `Generic` and produced
    /// `Widget.svelte::renderWidget -> Vault.sol::Vault.helperOnlyInSolidity`
    /// at 0.9.
    ///
    /// Stated over the *bucket* rather than over the languages in it, because
    /// the list of languages in it is exactly the thing that keeps going stale.
    /// `Generic` means "no evidence that a call here can resolve anywhere", and
    /// the honest response to no evidence is to abstain: the call is still
    /// recorded in `generation_unresolved`, so nothing goes missing silently.
    pub fn admits(self, other: Self) -> bool {
        self == other && self != LangFamily::Generic
    }
}

/// The evidence tier of a [`Resolution`], without its payload.
///
/// This is what the honesty invariants are stated over — [`Self::confidence`]
/// is a function of the kind alone — and it is the kind, not the payload, that
/// the store persists per edge (`generation_edges.resolution`) and that the
/// artifacts label each edge with. One enum, one spelling, one confidence
/// table: a variant added to [`Resolution`] stops compiling in
/// [`Resolution::kind`] until someone decides how it is persisted and what it
/// entitles, and no second hand-written table in another crate can drift from
/// this one.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum ResolutionKind {
    SameFile,
    SamePackage,
    ImportScoped,
    ReceiverType,
    UniqueGlobal,
    AmbiguousGlobal,
    Unresolved,
    Structural,
}

impl ResolutionKind {
    /// Every kind, in declaration order. What [`Self::from_label`] searches and
    /// what a table generated from the enum (the store's SQL check) iterates.
    pub const ALL: [ResolutionKind; 8] = [
        ResolutionKind::SameFile,
        ResolutionKind::SamePackage,
        ResolutionKind::ImportScoped,
        ResolutionKind::ReceiverType,
        ResolutionKind::UniqueGlobal,
        ResolutionKind::AmbiguousGlobal,
        ResolutionKind::Unresolved,
        ResolutionKind::Structural,
    ];

    /// The confidence a rung entitles an edge to.
    ///
    /// - `SameFile`, `SamePackage`, `ImportScoped`, `ReceiverType` —
    ///   deterministic: the declaration is in this file, or in this file's
    ///   package block, or the import or the receiver's type names it outright.
    /// - `UniqueGlobal` — exactly one declaration of that name in the family.
    /// - `AmbiguousGlobal` — several matches and no way to choose (G5).
    /// - `Unresolved` — no edge is ever built from this variant. It scores at
    ///   the floor so that an edge built from it by mistake sorts below every
    ///   honest one rather than above them.
    /// - `Structural` — not a resolved reference at all: a relation the graph
    ///   asserts about its own shape, whose certainty comes from a declaration
    ///   the file carries outright (a Go package clause).
    pub fn confidence(self) -> Confidence {
        match self {
            ResolutionKind::SameFile
            | ResolutionKind::SamePackage
            | ResolutionKind::ImportScoped
            | ResolutionKind::ReceiverType
            | ResolutionKind::Structural => Confidence::DETERMINISTIC,
            ResolutionKind::UniqueGlobal => Confidence::HIGH,
            ResolutionKind::AmbiguousGlobal | ResolutionKind::Unresolved => Confidence::SPECULATIVE,
        }
    }

    /// The stored spelling. The one owner of it: `save_generation` writes this
    /// and [`Self::from_label`] reads it back, so the two cannot disagree.
    pub fn label(self) -> &'static str {
        match self {
            ResolutionKind::SameFile => "SameFile",
            ResolutionKind::SamePackage => "SamePackage",
            ResolutionKind::ImportScoped => "ImportScoped",
            ResolutionKind::ReceiverType => "ReceiverType",
            ResolutionKind::UniqueGlobal => "UniqueGlobal",
            ResolutionKind::AmbiguousGlobal => "AmbiguousGlobal",
            ResolutionKind::Unresolved => "Unresolved",
            ResolutionKind::Structural => "Structural",
        }
    }

    /// Decode a stored spelling. `None` for one this binary does not know —
    /// which the caller must treat as an error, never as some neighbouring
    /// tier: it means the store was written by a binary that knows a rung this
    /// one does not, and rounding it would put a confidence claim on an edge
    /// whose evidence this binary cannot read.
    pub fn from_label(label: &str) -> Option<Self> {
        Self::ALL.iter().copied().find(|kind| kind.label() == label)
    }
}

/// Where an edge's evidence tier came from.
///
/// The distinction is the point of persisting the kind. An edge the resolver
/// just built carries its own evidence; one re-read from a generation written
/// after the column existed carries what the resolver recorded; one re-read
/// from an older generation carries a guess made from the row's file layout.
/// A guess that presents itself as a reading is the shape this codebase treats
/// as the expensive failure, so the three are never spelled the same.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum ResolutionSource {
    /// The resolver built this edge in this process; the payload is in
    /// [`ResolvedEdge::resolution`].
    Resolver,
    /// Read from `generation_edges.resolution`, as the resolver recorded it.
    Stored,
    /// Inferred from the row, because the generation predates the column.
    Reconstructed,
}

impl ResolutionSource {
    pub fn label(self) -> &'static str {
        match self {
            ResolutionSource::Resolver => "resolver",
            ResolutionSource::Stored => "stored",
            ResolutionSource::Reconstructed => "reconstructed",
        }
    }
}

/// An edge's evidence tier, and whether it was resolved, read or guessed.
///
/// What travels with an edge across the store round trip. The full
/// [`Resolution`] payload does not — an `ImportScoped` row does not carry
/// `imported_from` — so this is the type a re-read edge answers "what was this
/// resolved by" with, and [`ResolvedEdge::resolution`] stays `None` on that
/// path rather than holding a payload invented to fill the variant.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct Evidence {
    pub kind: ResolutionKind,
    pub source: ResolutionSource,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub enum Resolution {
    SameFile {
        target_symbol: String,
        target_file: String,
    },
    /// X45. The declaration is in another file of **this file's package**.
    ///
    /// Go puts every package-level identifier in the package block, so a bare
    /// name written in `search/rank.go` reaches `search/provider.go` with no
    /// import and no ambiguity — the language resolves it, not a name count.
    /// None of the other rungs can say that: the declaration is not in this
    /// file, no import names it, no receiver was typed, and the two global
    /// tiers are defined by how many matches the *family* holds, which is a
    /// different question with a different answer.
    ///
    /// `package_name` is carried because a directory is not a package: a
    /// directory holds `package foo` and its external test package `foo_test`,
    /// and the two do not share a scope.
    SamePackage {
        target_symbol: String,
        target_file: String,
        package_name: String,
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
    /// A relation the graph asserts about its own shape rather than a reference
    /// one symbol makes to another: the synthetic Go package node that every
    /// file of a package is a member of.
    ///
    /// A separate variant because `SameFile` was doing this job and saying
    /// something untrue while it did. That rung means "the declaration is in
    /// this very file", and the package star edge runs from `geo/measure.go` to
    /// `package:geo/geo` — two different names — so an edge whose evidence
    /// claimed they were one file was the only shape in the emitted graph where
    /// `resolution` and the edge disagreed about where the target lives. The
    /// tier is unchanged and deserved: a Go package clause is written at the top
    /// of the file, which is as deterministic as evidence gets.
    Structural {
        target_symbol: String,
        target_file: String,
    },
}

impl Resolution {
    /// The evidence tier, without the payload. See [`ResolutionKind`].
    ///
    /// Exhaustive on purpose: a new variant must say here which rung it is
    /// before anything can persist or score it.
    pub fn kind(&self) -> ResolutionKind {
        match self {
            Resolution::SameFile { .. } => ResolutionKind::SameFile,
            Resolution::SamePackage { .. } => ResolutionKind::SamePackage,
            Resolution::ImportScoped { .. } => ResolutionKind::ImportScoped,
            Resolution::ReceiverType { .. } => ResolutionKind::ReceiverType,
            Resolution::UniqueGlobal { .. } => ResolutionKind::UniqueGlobal,
            Resolution::AmbiguousGlobal { .. } => ResolutionKind::AmbiguousGlobal,
            Resolution::Unresolved { .. } => ResolutionKind::Unresolved,
            Resolution::Structural { .. } => ResolutionKind::Structural,
        }
    }

    /// The confidence this evidence entitles an edge to — a function of the
    /// kind alone, so the table lives on [`ResolutionKind::confidence`] where
    /// the store's read-side check can apply it to a kind it decoded without
    /// the payload.
    pub fn confidence(&self) -> Confidence {
        self.kind().confidence()
    }

    /// The single `(file, symbol)` this resolution names, or `None` for the two
    /// variants that name no single target.
    ///
    /// The resolution *is* the record of what was found. The call ladder used
    /// to carry a second copy of these two strings beside it and emit edges
    /// from that copy, which is the same shape as the `confidence`/`resolution`
    /// drift above: two fields obliged to agree, with nothing obliging them.
    pub fn target(&self) -> Option<(&str, &str)> {
        match self {
            Resolution::SameFile {
                target_symbol,
                target_file,
            }
            | Resolution::SamePackage {
                target_symbol,
                target_file,
                ..
            }
            | Resolution::ImportScoped {
                target_symbol,
                target_file,
                ..
            }
            | Resolution::ReceiverType {
                target_symbol,
                target_file,
                ..
            }
            | Resolution::UniqueGlobal {
                target_symbol,
                target_file,
                ..
            }
            | Resolution::Structural {
                target_symbol,
                target_file,
            } => Some((target_file.as_str(), target_symbol.as_str())),
            // Several targets, or none: neither can answer "which one".
            Resolution::AmbiguousGlobal { .. } | Resolution::Unresolved { .. } => None,
        }
    }
}

/// Most candidates one ambiguous *bare-name* call site may fan out into as
/// edges.
///
/// At or below this ceiling every candidate becomes a `Calls` edge (SC4: do
/// not collapse the ambiguity). Above it the site emits **no** edges and one
/// ledger row whose `Resolution::AmbiguousGlobal` still carries the complete
/// candidate list — a capped sample of a 241-candidate site was still 16
/// wrong edges per call, and GitPulse measured 71% of all edges as
/// `AmbiguousGlobal` before receiver calls were barred from this rung.
///
/// Receiver calls never reach this rung at all: they resolve only through
/// receiver evidence, or land in the ledger as External / UninferredReceiver.
pub const AMBIGUOUS_FANOUT_CAP: usize = 16;

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
    /// The evidence tier and where it came from — the part of `resolution`
    /// that survives the store round trip. `Some(.., Resolver)` from
    /// [`Self::resolved`]; `Some(.., Stored | Reconstructed)` on an edge
    /// re-read from a generation; `None` only on an edge built by hand.
    #[serde(default)]
    pub evidence: Option<Evidence>,
    pub details: Option<String>,
}

impl ResolvedEdge {
    /// Build an edge from its evidence. **The only constructor this crate
    /// uses**, so `confidence` cannot disagree with `resolution`.
    ///
    /// The fields stay public because `devmap-query`'s `stored_edge_to_resolved`
    /// rebuilds an edge from a database row, and that read path lives in
    /// another crate. The row carries the evidence *kind*
    /// (`generation_edges.resolution`) and not the payload, so a re-read edge
    /// fills `evidence` and leaves `resolution` `None`; the invariant is held
    /// by routing every *write* through here, by the store's read-side check
    /// that a stored confidence matches its stored kind, and by
    /// `every_edge_confidence_matches_the_evidence_it_names`, which re-checks
    /// it over the whole emitted graph rather than trusting it.
    pub fn resolved(
        source_file: String,
        target_file: String,
        source_symbol: String,
        target_symbol: String,
        edge_kind: EdgeKind,
        resolution: Arc<Resolution>,
        details: Option<String>,
    ) -> Self {
        ResolvedEdge {
            source_file,
            target_file,
            source_symbol,
            target_symbol,
            edge_kind,
            confidence: resolution.confidence(),
            evidence: Some(Evidence {
                kind: resolution.kind(),
                source: ResolutionSource::Resolver,
            }),
            resolution: Some(resolution),
            details,
        }
    }
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
    /// A bare-name call or reference that failed every ladder rung, and **no
    /// indexed symbol carries that bare name**.
    ///
    /// Distinct from [`Unresolved`]: when the corpus has no namesake, the miss
    /// cannot be an extraction gap pointing at a declaration we failed to bind
    /// — there is nothing to bind. Kept in the ledger for completeness; excluded
    /// from gap numerators that treat [`Unresolved`] as a defect count.
    NoNamesake,
    /// A receiver that is a **module path** into this crate (`crate::`,
    /// `super::`, `self::`, or a path rooted at a module this file's own `mod`
    /// / `use` tree names) rather than a value whose type went uninferred.
    ///
    /// The path shape is affirmative evidence — the same argument
    /// [`External`] makes for `std::fs` — so these rows are not bare defects.
    /// A miss still means the named item was not indexed; it does not mean the
    /// classifier guessed.
    ModulePath,
    /// A bare-name call, in a corpus that indexes every file, that its
    /// enclosing scope does not declare and that matched no language builtin,
    /// no host global and no import — **and** at least one indexed symbol
    /// shares that bare name (so the miss may be a real bind failure).
    ///
    /// Also the tier for a use bound by an import whose specifier is
    /// **repo-relative** (`.helpers`, `./util`, `super::x`, `crate::y`) and
    /// whose target is not indexed. Such a specifier resolves against the
    /// importing file's own directory, so it cannot name anything outside the
    /// corpus: failing to resolve it is an index gap — gitignored, over the
    /// size cap, generated — and an index gap is worth acting on. Calling it
    /// `External` would file it under "expected".
    ///
    /// **This is the primary tier that indicates a defect** among bare names —
    /// every other outcome is explained by evidence. It is the tier to read
    /// when hunting bugs. See also [`NoNamesake`] and [`ModulePath`].
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
            UnresolvedClass::NoNamesake => "no_namesake",
            UnresolvedClass::ModulePath => "module_path",
            UnresolvedClass::Unresolved => "unresolved",
        }
    }
}

/// Which edge family a ledger row failed to produce.
///
/// The ledger used to cover calls only, while an unresolvable *reference* and
/// an unbindable *route handler* were dropped without trace — so the completeness
/// ledger `devmap build` prints was a partial denominator presented as a total.
/// Recording all three in one vector needs a discriminator, or the tiers stop
/// meaning anything: "uninferred receiver" is a statement about a call, and a
/// route that failed to bind is not a call at all.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord)]
pub enum UnresolvedKind {
    /// An invocation the resolution ladder could not attribute.
    Call,
    /// A non-call use — a type annotation, a qualifier, a member access — that
    /// the resolution rungs ran on and could not attribute.
    ///
    /// **Scope, stated because the number is read as coverage:** this covers
    /// every reference rung that *runs*. It excludes the one the resolver
    /// declines — a bare `Name` in value position, which
    /// `resolve_name_reference` stops before the global lookup for, so that
    /// `except Exception as e` cannot bind to an unrelated `def e`. A `Name`
    /// carrying a receiver *is* covered, because the member rungs run for it.
    ///
    /// The declined rung is not recorded because there is no tier that means
    /// "not attempted": filing it under `UnresolvedClass::Unresolved` would
    /// claim a check ran and failed when it never ran, and would put ~33k
    /// local-variable mentions per 150 files into the tier that holds 9 real
    /// defects. Closing that gap needs a new `UnresolvedClass` variant, which
    /// is matched exhaustively outside this crate.
    Reference,
    /// A route whose handler name did not bind to any symbol. Distinct because
    /// `HandlesRoute` is what tells liveness a handler is reached from outside
    /// the call graph: a route that failed to bind leaves its handler looking
    /// dead, and that must not read the same as a route with no named handler.
    Route,
    /// A **repo-relative** import specifier that named no indexed file, so no
    /// `Imports` edge exists for it.
    ///
    /// Only relative specifiers are recorded. `import "strings"` naming no
    /// indexed file is the expected case and carries no information; `from
    /// .helpers import thing` naming no indexed file is an index gap, and the
    /// edge that should exist is missing.
    Import,
}

impl UnresolvedKind {
    /// Stable name for persistence and reporting.
    pub fn label(&self) -> &'static str {
        match self {
            UnresolvedKind::Call => "call",
            UnresolvedKind::Reference => "reference",
            UnresolvedKind::Route => "route",
            UnresolvedKind::Import => "import",
        }
    }
}

/// Something the resolution ladder could not attribute to any target.
///
/// R5 forbids silence: dropping these makes "we could not resolve this"
/// indistinguishable from "nothing was here", which silently understates both
/// the graph and every liveness conclusion drawn from it. Kept out of `edges`
/// deliberately — an unresolved use has no target node to point at, so
/// materialising one would invent graph structure.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct UnresolvedReference {
    pub source_file: String,
    pub source_symbol: String,
    pub callee_name: String,
    /// Which edge family this row is about. See `UnresolvedKind`.
    pub kind: UnresolvedKind,
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
    /// `<file>::<exported name>` -> the file that declares it, for every
    /// `export { x } from './m'`, followed through nested barrels to the
    /// terminal declaration.
    ///
    /// **Empty is a real answer here, and a narrow one.** A chain is recorded
    /// only where an export names its own source module — which is JS/TS
    /// syntax. Python's `from .impl import thing` inside an `__init__.py` is a
    /// re-export to any reader, and the extractor records it as an *import*
    /// with no export-side specifier, so no chain exists for it and none is
    /// inferred. `reexport_chains_are_only_claimed_where_an_export_names_its_
    /// source` pins that scope.
    ///
    /// A cycle yields no entry at all: there is no terminal file, and naming
    /// either endpoint would invent one. Depth is bounded by
    /// `REEXPORT_CHAIN_MAX_DEPTH`.
    pub reexport_chains: BTreeMap<String, String>, // "<file>::<name>" -> "<file>::<name>"
    /// Calls, references and route handlers seen but not attributed.
    /// Deterministically ordered (R4).
    pub unresolved: Vec<UnresolvedReference>,
}
