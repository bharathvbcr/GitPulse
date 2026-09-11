#[cfg(feature = "parse")]
use crate::model::Extraction;
use crate::model::ParseOutcome;

/// Analyzer version baked into cache keys (S14 / X7 admission contract).
pub const ANALYZER_VERSION: &str = env!("CARGO_PKG_VERSION");
/// Bump whenever serialized extraction semantics change independently of the
/// package version. This prevents older, valid JSON from silently omitting
/// newly authoritative fields.
/// v7: Go methods are `file::Type.name`, Name/Type references are extracted,
/// and Go import aliases are recorded. A v6 payload would omit those uses and
/// resurrect false-dead methods.
/// v8: `wiring` carries symbol-scoped `RuntimeEntryPoint`/`StructuralExempt`
/// annotations, and JS/TS class methods report `is_exported` from their
/// enclosing class. A v7 payload has neither, so reusing one would resurrect
/// every runtime/framework/harness entry point as confidently dead.
/// v9: Go extractions carry `go_interface_methods` and `go_method_params` so a
/// method implementing an interface declared in another file of the same package
/// can be exempted. A v8 payload has neither, so reusing one would resurrect
/// every cross-file Go interface implementation as confidently dead.
/// v10: Go callables report their receiver type, so `caller_symbol` and a
/// receiver binding's `enclosing_symbol` are `file::Type.method` rather than the
/// bare `file::method`, and the receiver binding is scoped to its own method. A
/// v9 payload cannot distinguish two types' same-named methods, which is what
/// let their receiver bindings collide and resolve `s.method()` to the wrong
/// type at full confidence (SC9).
/// v11: Rust method calls record the method as `callee_name` with the receiver
/// in `receiver_expr` instead of the whole dotted chain, and Rust/Go parameters
/// contribute `name -> declared type` bindings. A v10 payload has neither, so
/// reusing one leaves every Rust method call unresolvable and every
/// typed-parameter receiver unbound (SC12).
/// v12: Rust `impl Trait for Type` methods are qualified by the TYPE, and a
/// trait's bare method signatures are extracted as symbols in their own right.
/// A v11 payload gives every implementor of a trait the same qualified name and
/// omits the trait's declared surface entirely, so reusing one restores a broken
/// graph join key (SC11/SC6b).
/// v13: calls hidden inside Rust macro bodies are recovered, and definitions
/// nested inside a function body are qualified by that function rather than by
/// the file. A v12 payload omits every macro-borne call and collapses N
/// same-named local definitions into one identity (SC13/SC14).
/// v16: JSX intrinsic host elements (`<div/>`) no longer emit call edges, and a
/// Go composite literal's type is unwrapped to the named type it constructs
/// (`[]*Foo{}` references `Foo`, `[]string{}` references nothing). A v15 payload
/// carries both classes of phantom call, which no symbol can ever match (SC17).
/// v17: a parameter whose declared type is written with a module qualifier
/// (`*testing.T`, `&reqwest::Client`) emits a `TypeQualifier` reference naming
/// that module. A v16 payload has none, so every method on an externally-typed
/// receiver is misreported as an unexplained failure instead of external (SC25).
/// v18: a Rust path call (`MyType::create()`, `std::fs::write()`) splits into
/// callee + path receiver instead of recording the whole path as the callee; a
/// call wrapped in `await`/parens unwraps to its real callee; and a JSX member
/// tag (`<motion.div/>`) splits into receiver + property. A v17 payload records
/// all three as single unjoinable names, which is 11,847 phantom callees (SC26).
/// v19: turbofish type arguments are stripped from a callee (`row.get::<_, T>`
/// becomes `get` on `row`), and an immediately-invoked function literal emits no
/// call at all instead of its own source text. A v18 payload carries both as
/// callee names nothing can ever match (SC26b).
/// v20: a Metal file whose only parse errors are its own declaration qualifiers
/// (`kernel`, `vertex`, `fragment`, `device`, `constant`, `threadgroup`, …),
/// an address-space cast, or an atomic in an address space reports `Clean`
/// instead of `Partial`; and a `kernel`/`vertex`/`fragment` entry point carries
/// a `RuntimeEntryPoint` wiring annotation naming the host dispatch that
/// reaches it. Measured on 55 real `.metal` files: 54 of 55 `Partial` with
/// 2,584 error ranges becomes 0 and 0, with the declaration count unchanged at
/// 287, and 240 entry points annotated where there were none. A v19 payload
/// marks every Metal file permanently degraded — which arms
/// `overlaps_parse_error` to exempt every shader from dead-code analysis and
/// makes a real grammar regression invisible against that background — and
/// records no evidence at all distinguishing an entry point from a private
/// helper (SC19).
/// v21: a C-family declaration no longer emits a `Name` reference to the name it
/// declares. C, C++, Objective-C, CUDA and Metal name a declaration through a
/// `declarator` chain rather than a `name` field, so `is_defining_name` treated
/// every function's own identifier as a use; the resulting
/// `file -> file::symbol` `References` edge is not one of the structural kinds
/// `analyze_liveness` skips, so it counted as a call and no C-family symbol
/// could ever be `!is_called`. Measured on 47 `.c`, 36 `.h` and 16 `.metal`
/// files (46,988 lines): self-referencing `References` edges **716 -> 104**, all
/// 104 survivors genuine (static functions installed into extension vtables),
/// non-self `References` unchanged at 4, symbol count unchanged at 1,095, and
/// uncalled-symbol rows 228 -> 808. A v20 payload carries the self-references
/// that silently disable dead-code detection for the whole C family.
/// v22: two independent changes to extraction semantics land together.
/// (a) SC31 gives the C family a call graph — `call_expression`,
/// `new_expression` and ObjC `message_expression` are extracted where the
/// generic arm previously emitted declarations only (0 -> 1,828 `Calls` on 183
/// first-party files; 0 -> 154,107 on 9,082 LibTorch headers), C-family
/// identity gets a single canonical owner so an out-of-line `int S::m()` and
/// its in-class declaration agree, and export defaults to header evidence
/// rather than a leading-underscore guess (1,393/1,464 symbols reported
/// exported -> 270/1,362).
/// (b) SC32 removes the last text fallbacks from callee naming and emits Python
/// re-export aliases as symbols (non-identifier callee names 216 -> 0; nodes
/// +289, edges +514, `scope_locals` added to the serialized payload).
/// A v21 payload predates both: it would resurrect a C family with no calls and
/// a callee named by its own source text, and it carries no `scope_locals`, so
/// every `LocalBinding` classification would silently revert to `Unresolved`.
/// v23: function-like macros (`preproc_function_def`) are emitted as C-family
/// `Function` symbols. SC31 made the gap observable rather than creating it —
/// once C-family calls are extracted, `ACTIONS(1)` is a recorded call whose
/// `#define` target was never emitted, and on this repository that asymmetry
/// alone put 13,630 rows into the defect tier (7,372 `ACTIONS` in one generated
/// `parser.c` table). Object-like `preproc_def` stays out: it is a constant,
/// never a callee. A v22 payload carries calls to macro targets that do not
/// exist, which is the SC18/SC30 signal being drowned by its own new coverage.
/// v24: eleven languages gain call extraction (SC34) — Ruby, PHP, Swift, Scala,
/// Lua, Luau, R, Java, C#, Kotlin and Dart. Five of those (Ruby, Swift, PHP,
/// Scala, Lua) were measured recovering calls under the Python implementation
/// this port replaces and so were migration regressions; the rest are new
/// capability neither engine had. Measured on real code: Kotlin 0 -> 91,149
/// calls over 1,361 first-party files, Java 0 -> 3,622, Ruby 0 -> 97, PHP
/// 0 -> 65, all at 0 orphaned call edges. A v23 payload carries an empty
/// `calls`/`references` list for every file in those languages, so a cached
/// build would resurrect the blackout with no signal that it had.
/// v25: the declaration path becomes per-language (`langdecl`), the way call
/// extraction already was. Swift and Kotlin visibility is read from the
/// declaration's own modifier list instead of from a substring scan of its whole
/// subtree, so `is_exported` stops being a guess and dead-code analysis becomes
/// answerable for both; Swift `extension Person` stops emitting a second
/// `Person` node; Swift and Kotlin enums and Kotlin interfaces get their real
/// `SymbolKind`; a Kotlin `fun Person.extra()` keeps its receiver; Dart emits
/// function, method and named-constructor symbols for the first time; and an R
/// function is named after the variable it is bound to rather than after the
/// `function` keyword. Entry-point and structural exemptions are emitted
/// alongside, because reading visibility is what first made a symbol capable of
/// being reported dead. A v24 payload carries the old identities: duplicate
/// Swift type nodes, bare Kotlin extension names, no Dart callables, three R
/// functions sharing the name `function`, and an `is_exported` that says nothing
/// — every one of which is a join key or a dead-code verdict, so reusing it
/// would silently restore the defects this version fixes.
/// v26 adds tier-2 declaration recovery: a file whose language has no linked
/// grammar now contributes pattern-matched symbols instead of nothing, and is
/// marked `ExtractionEngine::RegexFallback` / `ParseOutcome::Fallback` so a
/// consumer can tell a matched symbol from a parsed one. A v25 payload for such
/// a file carries an empty symbol list under a real content hash, so reusing it
/// would leave every `.proto`, `.ps1` and `.vb` in the tree permanently
/// invisible while looking freshly indexed.
/// v27 adds `body_signature`: a Type-1 and Type-2 hash of each symbol body,
/// computed from the parse tree. A v26 payload has the field absent, which
/// `serde(default)` reads back as `None` — and `None` means "no signature was
/// computed", which is exactly what a clone report would then conclude about
/// every cached file. Without the bump the first incremental build after this
/// change would report clones found only among the handful of files that
/// happened to be edited, and report it as a whole-repository answer.
/// v28 splits `ExtractionEngine::NotApplicable` out of `Unavailable` (K5): a
/// prose or data format has no grammar *by design*, and a `.proto` this build
/// cannot parse is a gap in coverage. Both were `Unavailable` before, so a v27
/// payload cannot say which it is — and `Extraction::is_parse_failure` asks
/// exactly that. Reusing v27 rows would keep reporting every Markdown file in
/// the repository as a parse failure while the classifier that stops doing so
/// sits right beside them.
/// v29 moves the fallback scan's truncation count out of `diagnostics` and into
/// the `ParseOutcome::Fallback` reason. `for_durable_store` clears
/// `diagnostics` before the payload reaches `generation_files.extraction_json`
/// and this cache, and nothing in the workspace reads that field in production,
/// so a v28 row for a 2,500-declaration file says "2000 declaration(s)
/// recovered by pattern" with no trace of the 500 that were dropped. Reusing
/// those rows would keep serving a prefix under a reason that reads as a set.
/// v31 moves the notebook cell cap and the unlocatable-symbol count out of
/// `diagnostics` and into the `ParseOutcome::Fallback` reason, for exactly the
/// reason v29 did it for the pattern scanner: `for_durable_store` clears
/// `diagnostics`, so a v29 row for a 6,000-cell notebook says `Clean` with no
/// trace of the 1,000 cells never read. It also adds the pattern scanner's
/// `skipped_long_lines` to that reason — a v29 row for a file with an
/// over-long declaration line reports only what it kept. Reusing either would
/// keep serving a prefix under an outcome that reads as a set.
/// v31 also reads the `<script>` blocks of Svelte, Vue, Astro and Liquid files
/// (`crate::embedded`). A v29 payload for any of those four is the outer
/// grammar's answer alone: one `File` node, no imports, no calls, no exports,
/// under `ParseOutcome::Clean` — a complete-looking result over a file whose
/// entire code half was never read. Reusing those rows would leave every
/// component in the tree permanently symbol-less while looking freshly indexed,
/// and would keep reporting `Clean` for a block that fails to parse.
/// Both landed independently as "v30"; the merged tree carries both
/// behaviours, so it is v31. A single bump covering two changes is
/// correct — the version answers "may a stored row be reused?", and
/// either change on its own already answers no.
/// v32 adds two things to the payload that a v31 row cannot contain, and by
/// the same rule either one on its own already answers no:
///
/// * **Heritage references (W1.2).** `heritage.rs` pushes
///   `ReferenceKind::Heritage` and `HeritageInterface` for `extends` /
///   `implements` / `impl … for` across fifteen languages, which the resolver
///   turns into `Extends` and `Implements` edges. A v31 row has none of them,
///   so every subtype relation in a cached file is simply absent — and a
///   polymorphic override reached only through its base type then reads as
///   uncalled, which is a delete-this verdict built on an edge that was never
///   extracted.
/// * **Wiring annotations (W3.3).** `WiringKind::AllowUnwired` records the
///   author's explicit `devcouncil: allow-unwired` declaration and
///   `WiringKind::DynamicImport` records the file forms an `importlib`,
///   `import('./x')` or worker-URL reference names. A v31 row carries neither,
///   so a cached file that declares itself intentionally unwired is reported
///   unwired anyway, and a lazily imported module stays invisible to the
///   liveness join.
///
/// Both are additive to the payload, which is exactly why the bump is
/// necessary: nothing about a v31 row *looks* stale, so without it a warm cache
/// serves a complete-looking extraction with the new evidence silently missing.
/// v33 adds `imports` for nineteen grammar keys that had none (W0.3 move 2):
/// the whole C family, the JVM family, Dart, PHP, Ruby, Lua, Luau, R, Nix,
/// Pascal, Solidity, Erlang, CFML and HCL. Before it the extractor had five
/// `imports.push` sites in total and no `#include` handler anywhere, so a v32
/// row for any file in those languages carries an **empty** import list — not a
/// partial one, and not one marked incomplete.
///
/// That is the worst shape a stale row can have here, because the consumer is
/// `unwired_candidates`, whose whole question is whether an inbound `Imports`
/// edge exists. A reused v32 row answers "nothing imports this file" with the
/// full confidence of a fresh extraction, for every header, every Java class
/// and every Terraform module in a warm cache — a delete-this verdict resting
/// on evidence that was never collected. The capability bit moved in the same
/// change, so the coverage machinery would no longer even charge the file as
/// import-blind: it would look examined and be blind.
///
/// v33 also adds `WiringKind::TargetRoot` for Cargo target roots — crate roots,
/// build scripts, `bin/`, `examples/` and `benches/` — which a v32 row cannot
/// carry either, and which decides whether a file is an entry root. One bump
/// covers both for the reason the v31 note records: the version answers "may a
/// stored row be reused?", and either change on its own already answers no.
///
/// v33 stops parsing minified bundles (`wiring::is_minified_bundle`) and
/// reports them `ParseOutcome::Skipped`. Every v32 row for such a file is one
/// of the three answers the coin flip produced — `Clean` with several hundred
/// mangled symbols, `Partial`, or `Failed` with a budget reason — and each is a
/// claim this build no longer makes. The `Clean` rows are the reason the bump
/// is not optional: they are cache-admitted, they look freshly indexed, and
/// they would keep publishing a minifier's `t`, `e` and `n` as declarations of
/// the repository long after the extractor stopped producing them.
///
/// v34 (X40) changes the payload in two ways, both of which a v33 row gets
/// wrong rather than merely misses. `scope_locals` now carries a callable's
/// **type parameters**, so a v33 row asserts that `read<T>` binds no `T` — the
/// classifier reads that set as complete and files the generic in the tier
/// reserved for probable defects. And `references` no longer carries the
/// inferred-type placeholder `_`, so a v33 row still holds one reference per
/// turbofish argument, each of which resolves to nothing by construction.
/// Reusing either would leave a warm cache reporting the old classification
/// with no sign that it is the old one.
///
/// v35 (X41) changes what a Rust `use` statement contributes. A v34 row for a
/// `.rs` file carries one import per statement whose `module_specifier` is the
/// statement's own source text (`"tree_sitter::{Language, Node, Parser}"`) and
/// whose `imported_names` is empty — a specifier that matches no file and binds
/// no name. The rows are not merely thinner: they are the shape that made every
/// `.rs` file in a warm cache report zero `Imports` edges and zero `External`
/// classifications while looking completely indexed.
///
/// v36 (X44) changes what `receiver_expr` means, on calls and on references
/// alike. A v35 row carries `get_node_text` of the receiver node, whole: the
/// entire left-hand expression of a chained call, newlines included — 13,387
/// such rows on this repository, 4,973 of them multi-line, the longest 38,644
/// characters. A v36 row carries the receiver's *identity* — the callee name of
/// an inner call, a bounded dotted path otherwise. That is a different string
/// for the same source, so mixing generations would put two spellings of one
/// receiver in one ledger and split every grouping over it without saying so.
///
/// v37 (K) changes which wiring annotations a file carries, and a v36 row
/// carries the old answers with nothing about it looking stale — the shape the
/// v31 note calls the reason a bump is not optional. A wiring annotation is an
/// *exemption*: reusing a stale one either hides a real finding or publishes a
/// delete-this verdict about code a framework reaches.
///
/// * `is_test_path` no longer returns early on the `/src/test/` and
///   `/src/androidTest/` layouts, so a dotfile there is no longer annotated
///   `TestFile` — a v36 row exempts every one of them from liveness.
/// * `is_wiring_decorator` matches a dotted hint as a prefix and a bare hint as
///   a whole segment instead of as a substring, so a v36 row can carry a
///   `FrameworkDecorator` for `@multitask` or `@preregister` — and that
///   annotation exempts every symbol in its file.
/// * `is_generated_path` gains `*_pb2_grpc.pyi` and requires `.go` after
///   `zz_generated`, so a v36 row is wrong in both directions: missing a
///   `GeneratedFile` on a gRPC type stub, and carrying one on a `.txt`.
/// * `WiringKind::ConfigEntryPoint` is new. A v36 `pyproject.toml` row carries
///   only the file-scoped `ScriptEntry`, so every console-script entry
///   function stays a dead-symbol candidate at the `extracted` tier —
///   a confident proposal to delete a program's entry point.
///
/// Each on its own already answers "no" to "may a stored row be reused?", by
/// the rule the v31 note records. All four are the sharpest shape of stale:
/// they change an *exemption*, so a reused row either hides a real finding or
/// publishes a delete-this verdict about code a framework reaches, and nothing
/// about the row looks old.
///
/// v39 adds Swift `import` (including `@testable` and kinded
/// `import struct Foundation.Date`) and shell `source` / `.` whose specifier
/// is a word that names a file. v33 covered nineteen grammar keys and did not
/// include either language, so a v38 row for a `.swift` file still carries an
/// **empty** import list — the same worst shape v33 named. The capability bit
/// moved in the same change, so `unwired_candidates` no longer charges the
/// file import-blind: it looks examined and is blind. `classify_unresolved`
/// cannot mark Foundation / XCTest names External without the import, so
/// `devmap dead` sets `walk_incomplete` over tens of thousands of SDK sites
/// that mean "the file imported Foundation", not "the graph is incomplete".
/// Measured on MarkDev against a warm v38 cache: 288 files reused, status
/// still reported `swift has no import extractor` for 219 files, and 28,207
/// of 39,870 unresolved sites stayed unexplained.
///
/// v39 also refuses `defer { }` as a callee. A v38 Swift row records those as
/// unresolved sites named `defer`, which is "the file uses defer", not a
/// missing function.
///
/// v40 changes two Swift identities a v39 row asserts as complete. A nested
/// enum's qualified name is now the full owner path
/// (`Workspace.ActiveSaveRequest.CancellationDisposition`), not the shallow
/// parent (`ActiveSaveRequest.CancellationDisposition`) — a v39 row cannot
/// join a Type reference attributed to the nested class, so a live field type
/// is confident-dead. And a Swift parameter's Type reference now carries
/// `assigned_to` for the parameter name (`reader: Reader` binds `reader`). A
/// v39 row leaves that empty, so `reader.read()` cannot dispatch on `Reader`
/// and falls to AmbiguousGlobal the moment a second type also declares `read`
/// — the MarkDev save/highlight shape, reported as examined and empty.
/// v41 records Svelte/Vue/HTML dynamic wiring the v40 row never saw:
/// `import('./CloneModal.svelte')` as an Import, HTML `src=` / `from` as
/// DynamicImport forms, `package.json` script CLIs, JS/TS TargetRoot for
/// `src/main.ts` / Vite configs / `scripts/`, and template `{handler}` as a
/// Call. A warm v40 cache keeps those files unwired and those handlers
/// confident-dead.
///
/// v42 parses non-identifier template expressions as the embedded script
/// language, so `onclick={() => copyText(name)}` is a Call to `copyText`
/// rather than opaque text. A v41 row still reports those handlers dead.
///
/// v43 publishes methods on objects an exported factory returns, treats
/// Vite/Rollup plugin methods as RuntimeEntryPoints, and vendors
/// `src-tauri/framework/`. A v42 row still reports `createPacedQueue.has` dead
/// and Tauri's copied tao/wry sources as unwired first-party files.
///
/// v44 carries the file-liveness annotations: `ScriptEntry` from a shebang,
/// `PackageMarker`, `ToolConfig`, `AmbientDeclaration` and `Fixture`, and it
/// moves `*.d.ts` and `*.config.*` off `TargetRoot`. A warm v43 row has none of
/// them, so every `__init__.py`, every shebang script, every `testdata/**` file
/// and every tool config in it stays an unwired candidate — which is exactly
/// the finding this version exists to withdraw.
///
/// v45 records `Foo<T>` as a Type use of `Foo`. A v44 row still reports a
/// type alias used only as a generic constructor confidently dead.
///
/// v46 records the enclosing callable as `parent_symbol` of a nested
/// `const walk = () => {}`. A v45 row parents those arrows on the file, so
/// `walk()` inside `collapseAll` cannot join to `collapseAll.walk`.
/// v47 refuses malformed notebook code cells instead of silently discarding
/// their contents and keeps the cell-cap verdict when the parsed prefix has
/// only prose. All notebook source spans now map decoded code to the original
/// JSON, and lexical bindings, exports and symbol wiring survive projection.
/// A v46 row can claim Clean for a notebook it did not read, point into prose
/// copies or metadata, drop Unicode-escaped declarations and omit bindings.
/// Notebook cells now parse independently under one shared budget; a v46 row
/// can invent a function that spans execution units. Python empty required
/// suites also report Partial instead of inheriting the grammar's Clean bit.
///
/// v48 extends every `LocalBinding` with optional `declared_type` and
/// `initializer`, and fills those fields from facts the extractor already
/// collected (typed parameters / `let x: T`, simple construction shapes such
/// as `T::new` / `factory()`, Go/Rust field types). It also emits Go
/// `field_declaration` Type references with `assigned_to` (the gap that left
/// `w.Priority.valid()` untyped), strengthens Python `isinstance` / annotation
/// Type uses, and records Svelte `$state` / `$derived` / `$effect` as Call
/// sites so resolve's HostGlobal table can classify them. A v47 row has none
/// of the new binding fields and no Go field Type edges, so a warm cache would
/// keep serving receiver-dispatch failures that look freshly examined.
///
/// v49 records `dsl.Matcher` Go parameters as `RuntimeEntryPoint` wiring so
/// ruleguard rules are not confident-dead. A v48 row has no such annotation.
pub const EXTRACTION_SCHEMA_VERSION: &str = "49";

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct CacheKey {
    pub content_hash: u64,
    pub language: String,
    pub grammar_version: String,
    pub analyzer_version: String,
}

/// Building a key means asking "may a payload extracted by *this* build be
/// reused", and only a build with grammars can answer it. The struct itself
/// stays available with `parse` off: a query-only consumer reads
/// `grammar_version` / `analyzer_version` off stored rows, it just cannot
/// compute what its own build would stamp, because it stamps nothing.
#[cfg(feature = "parse")]
impl CacheKey {
    pub fn for_source(language: &str, source: &str) -> Self {
        Self::for_content_hash(language, crate::content_hash(source))
    }

    fn for_content_hash(language: &str, content_hash: u64) -> Self {
        let (grammar_version, analyzer_version) = current_payload_identity(language);
        Self {
            content_hash,
            language: language.to_string(),
            grammar_version,
            analyzer_version,
        }
    }

    pub fn for_extraction(ext: &Extraction) -> Self {
        Self::for_content_hash(&ext.language, ext.content_hash)
    }
}

/// The `(grammar_version, analyzer_version)` this build stamps on a payload for
/// `language`.
///
/// One owner, because two of them drifted. The extraction cache keys on this
/// identity and correctly re-extracts after a bump, but a stored *generation*
/// carried its rows forward on content hash alone — so `extract-v23` file,
/// symbol and edge rows survived two schema bumps untouched while the analysis
/// beside them was computed from fresh `extract-v25` extractions. Measured on
/// DevCouncil: generation 412 held 1,152 v23 file rows under a v25 binary, and
/// the next changed build was refused outright by the edge/analysis equality in
/// `save_generation_with_metadata` — 65,615 stored against 65,798 analysed.
/// Anything that decides whether a stored payload may be reused must ask this
/// function rather than assemble the string itself.
/// Needs a compiled grammar to answer, so it exists only with `parse` on.
///
/// `devmap-store` already guards this: its own `current_payload_identity`
/// returns `Option` and documents that this one is `#[cfg(feature = "parse")]`.
/// The gate that comment relies on had been lost, so `--no-default-features`
/// did not build and the wrapper guarded a condition that could not arise.
#[cfg(feature = "parse")]
pub fn current_payload_identity(language: &str) -> (String, String) {
    (
        grammar_version_for(language),
        format!("{ANALYZER_VERSION}:extract-v{EXTRACTION_SCHEMA_VERSION}"),
    )
}

/// Real compiled grammar semver — never a constant placeholder (closes S14).
///
/// For a template language this also names the grammars that parse its embedded
/// `<script>` blocks, because the payload depends on them: a `.svelte` file's
/// symbols, calls and imports now come out of `tree-sitter-typescript`, and
/// keying only on `tree-sitter-svelte-ng` would serve a cached extraction back
/// unchanged across a TypeScript grammar bump that changes every one of them.
/// The embedded list is read from [`crate::languages::LanguageSpec::embedded`]
/// through [`crate::embedded::permitted_embedded_languages`] rather than
/// restated here, so the identity can never name a different set from the one
/// extraction routes to.
#[cfg(feature = "parse")]
pub fn grammar_version_for(language: &str) -> String {
    let base = base_grammar_identity(language);
    let mut embedded = crate::embedded::permitted_embedded_languages(language);
    // Kernel selection lives in notebook metadata, which is covered by the
    // content hash. Every grammar that selection can reach must also be in
    // the key so upgrading a linked kernel grammar invalidates warm payloads.
    if language == "notebook" {
        embedded.extend(crate::notebook::kernel_grammars());
    }
    if embedded.is_empty() {
        return base;
    }
    let embedded: Vec<String> = embedded
        .into_iter()
        .map(base_grammar_identity)
        .collect::<Vec<_>>();
    format!("{base}+embedded[{}]", embedded.join(","))
}

/// The identity of `language`'s own compiled grammar, with no embedded
/// component.
///
/// Split out from [`grammar_version_for`] so the embedded suffix is built from
/// a function that cannot itself consult the embedded list: one level, and a
/// registry entry that named its own language could not send this into
/// unbounded recursion.
#[cfg(feature = "parse")]
pub(crate) fn base_grammar_identity(language: &str) -> String {
    let (package, package_version, variant, grammar): (&str, &str, &str, tree_sitter::Language) =
        match language {
            "python" => (
                "tree-sitter-python",
                env!("DEVMAP_GRAMMAR_PYTHON_VERSION"),
                "python",
                tree_sitter_python::LANGUAGE.into(),
            ),
            "javascript" => (
                "tree-sitter-javascript",
                env!("DEVMAP_GRAMMAR_JAVASCRIPT_VERSION"),
                "javascript",
                tree_sitter_javascript::LANGUAGE.into(),
            ),
            "typescript" => (
                "tree-sitter-typescript",
                env!("DEVMAP_GRAMMAR_TYPESCRIPT_VERSION"),
                "typescript",
                tree_sitter_typescript::LANGUAGE_TYPESCRIPT.into(),
            ),
            "tsx" => (
                "tree-sitter-typescript",
                env!("DEVMAP_GRAMMAR_TYPESCRIPT_VERSION"),
                "tsx",
                tree_sitter_typescript::LANGUAGE_TSX.into(),
            ),
            "rust" => (
                "tree-sitter-rust",
                env!("DEVMAP_GRAMMAR_RUST_VERSION"),
                "rust",
                tree_sitter_rust::LANGUAGE.into(),
            ),
            "go" => (
                "tree-sitter-go",
                env!("DEVMAP_GRAMMAR_GO_VERSION"),
                "go",
                tree_sitter_go::LANGUAGE.into(),
            ),
            "hcl" => (
                "tree-sitter-hcl",
                env!("DEVMAP_GRAMMAR_HCL_VERSION"),
                "hcl",
                tree_sitter_hcl::LANGUAGE.into(),
            ),
            "java" => (
                "tree-sitter-java",
                env!("DEVMAP_GRAMMAR_JAVA_VERSION"),
                "java",
                tree_sitter_java::LANGUAGE.into(),
            ),
            "csharp" => (
                "tree-sitter-c-sharp",
                env!("DEVMAP_GRAMMAR_CSHARP_VERSION"),
                "csharp",
                tree_sitter_c_sharp::LANGUAGE.into(),
            ),
            "php" => (
                "tree-sitter-php",
                env!("DEVMAP_GRAMMAR_PHP_VERSION"),
                "php",
                tree_sitter_php::LANGUAGE_PHP.into(),
            ),
            "ruby" => (
                "tree-sitter-ruby",
                env!("DEVMAP_GRAMMAR_RUBY_VERSION"),
                "ruby",
                tree_sitter_ruby::LANGUAGE.into(),
            ),
            "c" => (
                "tree-sitter-c",
                env!("DEVMAP_GRAMMAR_C_VERSION"),
                "c",
                tree_sitter_c::LANGUAGE.into(),
            ),
            "cpp" => (
                "tree-sitter-cpp",
                env!("DEVMAP_GRAMMAR_CPP_VERSION"),
                "cpp",
                tree_sitter_cpp::LANGUAGE.into(),
            ),
            "objc" => (
                "tree-sitter-objc",
                env!("DEVMAP_GRAMMAR_OBJC_VERSION"),
                "objc",
                tree_sitter_objc::LANGUAGE.into(),
            ),
            "cuda" => (
                "tree-sitter-cuda",
                env!("DEVMAP_GRAMMAR_CUDA_VERSION"),
                "cuda",
                tree_sitter_cuda::LANGUAGE.into(),
            ),
            "swift" => (
                "tree-sitter-swift",
                env!("DEVMAP_GRAMMAR_SWIFT_VERSION"),
                "swift",
                tree_sitter_swift::LANGUAGE.into(),
            ),
            "scala" => (
                "tree-sitter-scala",
                env!("DEVMAP_GRAMMAR_SCALA_VERSION"),
                "scala",
                tree_sitter_scala::LANGUAGE.into(),
            ),
            "dart" => (
                "tree-sitter-dart",
                env!("DEVMAP_GRAMMAR_DART_VERSION"),
                "dart",
                tree_sitter_dart::LANGUAGE.into(),
            ),
            "pascal" => (
                "tree-sitter-pascal",
                env!("DEVMAP_GRAMMAR_PASCAL_VERSION"),
                "pascal",
                tree_sitter_pascal::LANGUAGE.into(),
            ),
            "lua" => (
                "tree-sitter-lua",
                env!("DEVMAP_GRAMMAR_LUA_VERSION"),
                "lua",
                tree_sitter_lua::LANGUAGE.into(),
            ),
            "luau" => (
                "tree-sitter-luau",
                env!("DEVMAP_GRAMMAR_LUAU_VERSION"),
                "luau",
                tree_sitter_luau::LANGUAGE.into(),
            ),
            "r" => (
                "tree-sitter-r",
                env!("DEVMAP_GRAMMAR_R_VERSION"),
                "r",
                tree_sitter_r::LANGUAGE.into(),
            ),
            "cfml" => (
                "tree-sitter-cfml",
                env!("DEVMAP_GRAMMAR_CFML_VERSION"),
                "cfml",
                tree_sitter_cfml::LANGUAGE_CFML.into(),
            ),
            "erlang" => (
                "tree-sitter-erlang",
                env!("DEVMAP_GRAMMAR_ERLANG_VERSION"),
                "erlang",
                tree_sitter_erlang::LANGUAGE.into(),
            ),
            "solidity" => (
                "tree-sitter-solidity",
                env!("DEVMAP_GRAMMAR_SOLIDITY_VERSION"),
                "solidity",
                tree_sitter_solidity::LANGUAGE.into(),
            ),
            "nix" => (
                "tree-sitter-nix",
                env!("DEVMAP_GRAMMAR_NIX_VERSION"),
                "nix",
                tree_sitter_nix::LANGUAGE.into(),
            ),
            "shell" => (
                "tree-sitter-bash",
                env!("DEVMAP_GRAMMAR_BASH_VERSION"),
                "shell",
                tree_sitter_bash::LANGUAGE.into(),
            ),
            "sql" => (
                "tree-sitter-sequel",
                env!("DEVMAP_GRAMMAR_SQL_VERSION"),
                "sql",
                tree_sitter_sequel::LANGUAGE.into(),
            ),
            "kotlin" => (
                "tree-sitter-kotlin-ng",
                env!("DEVMAP_GRAMMAR_KOTLIN_VERSION"),
                "kotlin",
                tree_sitter_kotlin_ng::LANGUAGE.into(),
            ),
            "svelte" => (
                "tree-sitter-svelte-ng",
                env!("DEVMAP_GRAMMAR_SVELTE_VERSION"),
                "svelte",
                tree_sitter_svelte_ng::LANGUAGE.into(),
            ),
            "vue" => (
                "vendored/tree-sitter-vue",
                "ce8011a",
                "vue",
                crate::treesitter::vendored::vue(),
            ),
            "astro" => (
                "tree-sitter-astro-next",
                env!("DEVMAP_GRAMMAR_ASTRO_VERSION"),
                "astro",
                tree_sitter_astro_next::LANGUAGE.into(),
            ),
            // Deliberately unlinked (UNSAFE_GRAMMARS / MSVC VLAs). Fingerprint
            // must not call into a grammar that is not in the binary.
            "cobol" => return "unavailable:cobol".to_string(),
            "liquid" => (
                "vendored/tree-sitter-liquid",
                "depth1",
                "liquid",
                crate::treesitter::vendored::liquid(),
            ),
            _ => return format!("unavailable:{language}"),
        };
    format!(
        "{package}@{package_version}:{variant}:abi{}",
        grammar.abi_version()
    )
}

/// Failed parses must never be admitted under a real content hash (closes X7).
pub fn cache_admits(outcome: &ParseOutcome) -> bool {
    !matches!(outcome, ParseOutcome::Failed { .. })
}

#[cfg(test)]
mod tests {
    use super::*;
    // Grammar-dependent. `cache_admits` is not, and its test below runs in both
    // configurations on purpose: gating a whole test module because part of it
    // needs a grammar is coverage removed from the shape an embedder ships.
    #[cfg(feature = "parse")]
    use crate::extract_file;

    #[test]
    fn test_x7_failed_extraction_not_cached() {
        let outcome = ParseOutcome::Failed {
            reason: "fatal".to_string(),
        };
        assert!(!cache_admits(&outcome));
    }

    /// Every linked grammar reports a distinct identity.
    ///
    /// Mutation testing deleted the `rust` and `go` match arms without a
    /// failure — those languages then fall to the `unavailable:` fallback,
    /// which changes the cache key for every file in them. A key that no longer
    /// matches means silent full re-extraction; a key that *collides* across
    /// languages means one language's payload can be served for another's file.
    /// The existing test only compared Python against JavaScript.
    #[test]
    #[cfg(feature = "parse")]
    fn every_linked_grammar_has_a_distinct_real_identity() {
        let linked = [
            "python",
            "javascript",
            "typescript",
            "tsx",
            "rust",
            "go",
            "hcl",
            "java",
            "csharp",
            "php",
            "ruby",
            "c",
            "cpp",
            "objc",
            "cuda",
            "swift",
            "scala",
            "dart",
            "pascal",
            "lua",
            "luau",
            "r",
            "cfml",
            "erlang",
            "solidity",
            "nix",
            "kotlin",
            "svelte",
            "astro",
            "vue",
            "liquid",
        ];
        let mut seen = std::collections::BTreeSet::new();
        for language in linked {
            let identity = grammar_version_for(language);
            assert!(
                !identity.starts_with("unavailable:"),
                "{language} is a linked grammar and must report a real identity, got {identity}"
            );
            assert!(
                identity.contains("tree-sitter-"),
                "{language} identity must name its grammar package: {identity}"
            );
            assert!(
                seen.insert(identity.clone()),
                "{language} shares a cache identity with another language: {identity}"
            );
        }

        // An unlinked language is explicitly unavailable rather than silently
        // sharing someone else's identity.
        // VB.NET never had a grammar. COBOL's sources remain under vendor/ but
        // are deliberately not compiled (non-terminating scanner; MSVC VLAs).
        assert!(grammar_version_for("vb").starts_with("unavailable:"));
        assert_eq!(grammar_version_for("cobol"), "unavailable:cobol");
    }

    #[test]
    #[cfg(feature = "parse")]
    fn test_s14_grammar_version_is_language_specific() {
        let py = grammar_version_for("python");
        let js = grammar_version_for("javascript");
        assert!(py.starts_with("tree-sitter-python@"));
        assert!(js.starts_with("tree-sitter-javascript@"));
        assert_ne!(py, js);
        assert!(py.contains(":python:abi"));
    }

    #[test]
    #[cfg(feature = "parse")]
    fn test_s14_cache_key_uses_compiled_grammar_package_and_variant() {
        let ts = grammar_version_for("typescript");
        let tsx = grammar_version_for("tsx");
        assert!(ts.starts_with("tree-sitter-typescript@"));
        assert!(tsx.starts_with("tree-sitter-typescript@"));
        assert!(ts.contains(":typescript:abi"));
        assert!(tsx.contains(":tsx:abi"));
        assert_ne!(ts, tsx, "TS and TSX must not share a grammar identity");
    }

    #[test]
    #[cfg(feature = "parse")]
    fn test_cache_key_changes_with_content_hash() {
        let a = extract_file("a.py", "def a(): pass\n");
        let b = extract_file("b.py", "def b(): pass\n");
        assert_ne!(CacheKey::for_extraction(&a), CacheKey::for_extraction(&b));
    }

    #[test]
    #[cfg(feature = "parse")]
    fn extraction_schema_version_is_part_of_cache_identity() {
        let ext = extract_file("worker.py", "worker = Worker()\n");
        let key = CacheKey::for_extraction(&ext);
        assert!(key
            .analyzer_version
            .ends_with(&format!(":extract-v{EXTRACTION_SCHEMA_VERSION}")));
    }
}
