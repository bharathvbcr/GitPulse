use crate::model::{Extraction, ParseOutcome};

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
pub const EXTRACTION_SCHEMA_VERSION: &str = "25";

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct CacheKey {
    pub content_hash: u64,
    pub language: String,
    pub grammar_version: String,
    pub analyzer_version: String,
}

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
pub fn current_payload_identity(language: &str) -> (String, String) {
    (
        grammar_version_for(language),
        format!("{ANALYZER_VERSION}:extract-v{EXTRACTION_SCHEMA_VERSION}"),
    )
}

/// Real compiled grammar semver — never a constant placeholder (closes S14).
pub fn grammar_version_for(language: &str) -> String {
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
            "cobol" => (
                "vendored/tree-sitter-cobol",
                "depth1",
                "cobol",
                crate::treesitter::vendored::cobol(),
            ),
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
            "cobol",
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
        // VB.NET is the one declared language with no grammar anywhere — not on
        // crates.io, not in any reachable upstream repository — so it is the
        // honest example of the unavailable path. This assertion previously
        // named `cobol`, which quietly stopped testing anything the moment a
        // cobol grammar was vendored.
        assert!(grammar_version_for("vb").starts_with("unavailable:"));
    }

    #[test]
    fn test_s14_grammar_version_is_language_specific() {
        let py = grammar_version_for("python");
        let js = grammar_version_for("javascript");
        assert!(py.starts_with("tree-sitter-python@"));
        assert!(js.starts_with("tree-sitter-javascript@"));
        assert_ne!(py, js);
        assert!(py.contains(":python:abi"));
    }

    #[test]
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
    fn test_cache_key_changes_with_content_hash() {
        let a = extract_file("a.py", "def a(): pass\n");
        let b = extract_file("b.py", "def b(): pass\n");
        assert_ne!(CacheKey::for_extraction(&a), CacheKey::for_extraction(&b));
    }

    #[test]
    fn extraction_schema_version_is_part_of_cache_identity() {
        let ext = extract_file("worker.py", "worker = Worker()\n");
        let key = CacheKey::for_extraction(&ext);
        assert!(key
            .analyzer_version
            .ends_with(&format!(":extract-v{EXTRACTION_SCHEMA_VERSION}")));
    }
}
