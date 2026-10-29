use super::*;

fn assert_finalization_refused(hook: fn()) {
    struct ClearHook;
    impl Drop for ClearHook {
        fn drop(&mut self) {
            EXTRACTION_FINISH_HOOK.with(|hook| hook.set(None));
        }
    }
    let _clear = ClearHook;
    EXTRACTION_FINISH_HOOK.with(|slot| slot.set(Some(hook)));
    let extraction = extract_treesitter_with_budget(
        "finish.py",
        "python",
        "def work():\n    return 1\ndef caller():\n    return work()\n",
        std::time::Duration::from_secs(2),
    );
    assert!(
        EXTRACTION_FINISH_HOOK.with(|slot| slot.get().is_none()),
        "the finalization boundary must be reached"
    );
    assert!(
        matches!(&extraction.parse_outcome, ParseOutcome::Failed { reason } if reason.contains("budget")),
        "late finalization published {:?}",
        extraction.parse_outcome
    );
    assert_eq!(extraction.symbols.len(), 1, "only the File node may remain");
    assert!(extraction.calls.is_empty());
}

#[test]
fn finalization_cannot_publish_after_deadline_expiry() {
    fn expire() {
        let deadline = WALK_DEADLINE.with(|slot| slot.get().expect("armed extraction"));
        while let Some(remaining) = deadline.checked_duration_since(std::time::Instant::now()) {
            std::thread::sleep(remaining);
        }
    }
    assert_finalization_refused(expire);
}

#[test]
fn finalization_cannot_publish_after_a_late_incomplete_walk() {
    fn incomplete() {
        WALK_OVERRAN.with(|slot| slot.set(true));
    }
    assert_finalization_refused(incomplete);
}

/// Every discovered file is addressable, whether or not it parsed. (K1)
///
/// The `File` node used to be pushed only inside the tree-sitter branch, so
/// a language with no linked grammar produced an `Extraction` with an empty
/// `symbols` vector: the file was recorded in `generation_files` and was
/// absent from the graph entirely — not returnable by a file-level query
/// and not usable as the target of any edge.
///
/// All three cases below reached that same nodeless state, and they are
/// kept apart because they fail for different reasons: a grammarless
/// language whose declarations *are* recoverable, one whose declarations
/// are not, and a prose format that is deliberately never scanned. Only
/// the first would be fixed by improving the fallback scanner, which is
/// why "tier-2 recovers declarations" does not subsume this test.
#[test]
fn a_grammarless_file_still_gets_a_file_node() {
    for (path, source, why) in [
        (
            "api/user.proto",
            "message User {\n  string id = 1;\n}\n",
            "grammarless source with recoverable declarations",
        ),
        (
            "scripts/manifest.psd1",
            "@{ ModuleVersion = '1.0' }\n",
            "grammarless source with nothing to recover",
        ),
        (
            "docs/design.md",
            "# Design\n\nProse, never declaration-scanned.\n",
            "prose format excluded from tier-2 by NON_DECLARATIVE_LANGUAGES",
        ),
    ] {
        let extraction = crate::extract_file(path, source);
        let files: Vec<&ExtractedSymbol> = extraction
            .symbols
            .iter()
            .filter(|symbol| symbol.kind == SymbolKind::File)
            .collect();
        assert_eq!(
            files.len(),
            1,
            "{path} ({why}) must contribute exactly one File node, got {:?}",
            extraction.symbols
        );
        let file = files[0];
        assert_eq!(
            file.qualified_name, path,
            "the File node is addressed by full path, like every parsed file"
        );
        assert_eq!(
            file.span.end_byte,
            source.len(),
            "the File node spans the whole file"
        );
    }
}

/// The `File` node is not a declaration, so it must not be read as one.
///
/// Emitting it unconditionally would be an easy way to make a nodeless file
/// *look* recovered. It must not move `parse_outcome`: a file whose
/// declarations could not be recovered still reports `Failed`, and only a
/// real declaration recovery reports `Fallback`.
#[test]
fn the_file_node_alone_is_never_reported_as_a_recovery() {
    let bare = crate::extract_file("scripts/manifest.psd1", "@{ ModuleVersion = '1.0' }\n");
    assert!(
        matches!(bare.parse_outcome, ParseOutcome::Failed { .. }),
        "a File node is not a recovered declaration, got {:?}",
        bare.parse_outcome
    );
    assert!(
        matches!(bare.engine, ExtractionEngine::Unavailable { .. }),
        "engine must stay Unavailable, got {:?}",
        bare.engine
    );

    let recovered = crate::extract_file("api/user.proto", "message User {\n  string id = 1;\n}\n");
    assert!(
        matches!(recovered.parse_outcome, ParseOutcome::Fallback { .. }),
        "a real declaration recovery still reports Fallback, got {:?}",
        recovered.parse_outcome
    );
    assert!(
        recovered
            .symbols
            .iter()
            .any(|symbol| symbol.name == "User" && symbol.kind != SymbolKind::File),
        "the recovered declaration must survive alongside the File node"
    );
}

/// An error region holding no identifier character is recorded as
/// nameless; one holding a name, or a zero-width one, is not.
///
/// The two nameless shapes are the ones measured: the `>()` tree-sitter-
/// typescript strands after a `typeof import(…)` type argument, and the
/// lone `}` the CUDA preprocessor scanner leaves behind a macro whose body
/// line ends in a comment. `abstract?:` holds a name and stays charged
/// wherever it lies outside a declaration.
#[test]
fn error_regions_without_a_name_are_recorded_as_nameless() {
    let outcome = |path: &str, lang: &str, source: &str| {
        let ParseOutcome::Partial {
            error_ranges,
            nameless_ranges,
        } = extract_treesitter(path, lang, source).parse_outcome
        else {
            panic!("{path} must parse partially");
        };
        let text = |ranges: &[TextRange]| -> Vec<String> {
            ranges
                .iter()
                .map(|range| source[range.start_byte..range.end_byte].to_string())
                .collect()
        };
        (text(&error_ranges), text(&nameless_ranges))
    };

    let (errors, nameless) = outcome(
        "a.test.ts",
        "typescript",
        "vi.mock('m', async (o) => ({\n  ...(await o<typeof import('m')>()),\n}));\n\
             function after(): number { return 2; }\n",
    );
    assert_eq!(errors, vec![">()"]);
    assert_eq!(nameless, vec![">()"]);

    let (errors, nameless) = outcome(
        "k.cu",
        "cuda",
        "#define STAMP(NAME) \\\n  __global__ void NAME(float *p) { \\\n    \
             f(p); /* g(p) */ \\\n    f(p); \\\n  }\n__device__ void f(float *p) {}\n",
    );
    assert!(errors.contains(&"}".to_string()), "{errors:?}");
    assert_eq!(
        nameless,
        vec!["}"],
        "only the brace is nameless: {errors:?}"
    );

    let (errors, nameless) = outcome(
        "p.ts",
        "typescript",
        "export interface Paper {\n  title: string;\n  abstract?: string;\n}\n",
    );
    assert_eq!(errors, vec!["abstract?:"]);
    assert!(
        nameless.is_empty(),
        "a keyword is spelled in letters: {nameless:?}"
    );

    assert!(
        range_is_nameless(
            "a)}",
            &TextRange {
                start_byte: 1,
                end_byte: 3
            }
        ),
        "punctuation alone is nameless"
    );
    assert!(
        !range_is_nameless(
            "a)}",
            &TextRange {
                start_byte: 1,
                end_byte: 1
            }
        ),
        "a zero-width region is not listed: it consumed nothing, and is not charged anyway"
    );
    assert!(
        !range_is_nameless(
            "})",
            &TextRange {
                start_byte: 0,
                end_byte: 9
            }
        ),
        "a range off the end of the source cannot be read, so it is not nameless"
    );
    assert!(
        !range_is_nameless(
            "(é)",
            &TextRange {
                start_byte: 0,
                end_byte: 4
            }
        ),
        "a non-ASCII byte may spell an identifier"
    );
}

/// A real syntax error must never be reported as a clean parse.
///
/// `is_benign_jsx_ampersand` suppresses a known upstream grammar
/// false-error on a lone `&` in JSX text. Suppression is the dangerous
/// direction: if it over-matches, genuinely broken source reports
/// `Clean`, and X6's rule — that symbols overlapping a parse error are
/// never dead-code evidence — stops firing, because there is no recorded
/// error to overlap. `treesitter.rs` had no tests at all, so nothing held
/// this boundary.
#[test]
fn benign_jsx_ampersand_is_suppressed_but_real_errors_are_not() {
    // A lone `&` in JSX text is valid TSX the grammar mis-flags.
    let jsx = extract_treesitter(
        "ok.tsx",
        "tsx",
        "export function A() {\n  return <div>Tom & Jerry</div>;\n}\n",
    );
    assert!(
        matches!(jsx.parse_outcome, ParseOutcome::Clean),
        "a lone & in JSX text must not degrade the outcome: {:?}",
        jsx.parse_outcome
    );

    // Genuinely broken source must stay Partial, with ranges recorded.
    let broken = extract_treesitter("bad.tsx", "tsx", "export function A( {\n");
    match &broken.parse_outcome {
        ParseOutcome::Partial { error_ranges, .. } => {
            assert!(
                !error_ranges.is_empty(),
                "a partial parse must record ranges"
            );
        }
        other => panic!("broken source must not report as {other:?}"),
    }

    // A plain-text error *inside* a JSX element must stay Partial. The
    // suppression's first operand is a veto on non-ampersand text, and
    // because `&&` binds tighter than `||` its removal turns the whole
    // guard into "no braces, no angle bracket" — so any ordinary syntax
    // error inside JSX (here `{a b}`) is silently reported Clean. Verified
    // to produce exactly one error range, `"b"`, carrying none of the four
    // characters the guard tests.
    let plain_error_in_jsx = extract_treesitter(
        "plain.tsx",
        "tsx",
        "export function A() {\n  return <div>{a b}</div>;\n}\n",
    );
    assert!(
        matches!(
            plain_error_in_jsx.parse_outcome,
            ParseOutcome::Partial { .. }
        ),
        "an ordinary syntax error inside JSX must not be suppressed by the \
             ampersand rule: {:?}",
        plain_error_in_jsx.parse_outcome
    );

    // A stray brace *inside* a JSX element, alongside an ampersand, must
    // stay Partial. This is the only shape that separates the first guard
    // from the rest: the error node contains both `&` and `}` and does have
    // a JSX ancestor, so if `!contains('&')` stops being a veto the node is
    // suppressed and a genuine JSX error reports as Clean. Verified to
    // produce exactly one error range, `"& b }"`, so nothing else can carry
    // the assertion.
    let stray_brace = extract_treesitter(
        "brace.tsx",
        "tsx",
        "export function A() {\n  return <div>a & b }</div>;\n}\n",
    );
    assert!(
        matches!(stray_brace.parse_outcome, ParseOutcome::Partial { .. }),
        "a stray `}}` in JSX is a real error and must not be suppressed by the \
             ampersand rule: {:?}",
        stray_brace.parse_outcome
    );

    // Broken source that also contains an ampersand must still be Partial:
    // the suppression must not generalise from "contains &" to "is fine".
    let broken_with_amp = extract_treesitter(
        "bad2.tsx",
        "tsx",
        "export function A( {\n  const x = 1 & 2;\n",
    );
    assert!(
        !matches!(broken_with_amp.parse_outcome, ParseOutcome::Clean),
        "an ampersand must not launder a real syntax error: {:?}",
        broken_with_amp.parse_outcome
    );
}

/// A macro body is only probed when it could contain a call.
///
/// `macro_token_body` strips the delimiter pair and skips bodies with no
/// parenthesis. The guard is what keeps SC13's recovery from re-parsing
/// every `vec![1, 2, 3]` in a repository, and the delimiter handling is
/// what makes `[]` and `{}` macros work at all.
#[test]
fn macro_bodies_are_unwrapped_and_skipped_when_they_hold_no_call() {
    assert_eq!(
        macro_token_body("(\"{}\", s.work())").as_deref(),
        Some("\"{}\", s.work()"),
        "parenthesised bodies unwrap"
    );
    assert_eq!(
        macro_token_body("[a.b()]").as_deref(),
        Some("a.b()"),
        "bracket macros unwrap too"
    );
    assert_eq!(
        macro_token_body("{a.b()}").as_deref(),
        Some("a.b()"),
        "brace macros unwrap too"
    );

    // No parenthesis inside means no call to recover.
    assert_eq!(macro_token_body("[1, 2, 3]"), None);
    assert_eq!(macro_token_body("()"), None, "an empty body holds nothing");
    // Unbalanced or undelimited text is not a token tree body.
    assert_eq!(macro_token_body("no delimiters"), None);
}

/// Rust type names strip references, pointers and generics.
///
/// This feeds parameter-type receiver bindings (SC12). Leaving `&` or a
/// generic argument attached makes the binding name a type that no symbol
/// has, so the call silently fails to resolve.
#[test]
fn rust_type_names_reduce_to_the_bare_identifier() {
    let probe = |source: &str| -> Option<String> {
        let ext = extract_treesitter("t.rs", "rust", source);
        ext.references
            .iter()
            .find(|reference| reference.assigned_to.as_deref() == Some("value"))
            .map(|reference| reference.name.clone())
    };
    assert_eq!(probe("fn f(value: &Store) {}").as_deref(), Some("Store"));
    assert_eq!(
        probe("fn f(value: &mut Store) {}").as_deref(),
        Some("Store")
    );
    assert_eq!(probe("fn f(value: Vec<Store>) {}").as_deref(), Some("Vec"));
    assert_eq!(
        probe("fn f(value: crate::inner::Store) {}").as_deref(),
        Some("Store"),
        "a scoped path reduces to its last segment"
    );
}

/// A non-callee identifier is still recorded as a name use.
///
/// Positive control for the suppression in `maybe_push_name_reference`:
/// `is_call_callee` was replaceable with a constant in *both* directions,
/// and the `false` direction is caught by the duplicate-count assertion
/// above. This covers the `true` direction — a suppression that widens to
/// every identifier erases the non-call uses that liveness relies on, so a
/// symbol used only as a value starts reading as dead.
#[test]
fn identifiers_used_as_values_are_still_name_references() {
    let extraction = extract_treesitter(
        "f.py",
        "python",
        "import os\ndef run():\n    os.path.join('a')\n",
    );
    let names: Vec<&str> = extraction
        .references
        .iter()
        .filter(|r| r.kind == ReferenceKind::Name)
        .map(|r| r.name.as_str())
        .collect();
    assert!(
        names.contains(&"os"),
        "the receiver of a call is a name use, not a callee: {names:?}"
    );
    assert!(
        names.contains(&"path"),
        "an intermediate attribute is a name use: {names:?}"
    );
    assert!(
        !names.contains(&"join"),
        "the callee itself must not also be a name use: {names:?}"
    );
}

/// Only identifier-shaped text is treated as a user identifier.
///
/// This predicate decides whether a token becomes a graph node at all.
/// Every operator in it was mutable: with `&&` -> `||` a leading digit is
/// accepted, with `all` -> `any` any punctuation-bearing text is accepted,
/// and with either `||` collapsed the `_`/`$` prefixes stop being
/// identifiers. Loosened, operators and literals become symbols and every
/// reference count is inflated; tightened, `_private` and `$el` disappear
/// from the graph and read as dead.
#[test]
fn only_identifier_shaped_text_is_a_user_identifier() {
    for accepted in [
        "foo", "_foo", "$foo", "_", "$", "a1", "foo$bar", "foo_bar9", "A", "_9",
    ] {
        assert!(
            is_user_ident(accepted),
            "{accepted:?} is identifier-shaped and must be accepted"
        );
    }

    for rejected in [
        "",        // no first character at all
        "9foo",    // a leading digit is not an identifier start
        "1",       //
        "foo-bar", // punctuation is not an identifier character
        "foo bar",
        "foo.bar",
        "foo()",
        "&",
        "...",
        "\u{e9}t\u{e9}", // non-ASCII
        "\u{4f60}\u{597d}",
    ] {
        assert!(
            !is_user_ident(rejected),
            "{rejected:?} is not identifier-shaped and must be rejected"
        );
    }
}

fn name_refs(extraction: &Extraction) -> Vec<&str> {
    extraction
        .references
        .iter()
        .filter(|r| r.kind == ReferenceKind::Name)
        .map(|r| r.name.as_str())
        .collect()
}

/// A C-family declaration must not reference the name it declares.
///
/// C, C++, Objective-C, CUDA and Metal name a declaration through a
/// `declarator` chain, not a `name` field, so `is_defining_name` did not
/// recognise a function's own identifier and every declaration emitted a
/// `Name` reference to itself. `analyze_liveness` skips only
/// `Contains`/`Defines`/`MemberOf`, so that self-reference resolved to a
/// `file -> file::symbol` `References` edge and counted as a call: no
/// C-family symbol could ever be `!is_called`, which disables dead-code
/// detection for the whole language family rather than degrading it.
#[test]
fn c_family_declarations_do_not_reference_themselves() {
    let c = extract_treesitter(
        "a.c",
        "c",
        "int helper(int x) { return x + 1; }\nint caller(void) { return helper(2); }\n",
    );
    assert!(
        !name_refs(&c).contains(&"helper"),
        "a C function's own declarator is a definition, not a use: {:?}",
        name_refs(&c)
    );
    assert!(!name_refs(&c).contains(&"caller"), "{:?}", name_refs(&c));

    let cpp = extract_treesitter(
        "b.cpp",
        "cpp",
        "float scale(float v) { return v * 2.0f; }\n",
    );
    assert!(!name_refs(&cpp).contains(&"scale"), "{:?}", name_refs(&cpp));

    // The symbols themselves must survive: the suppression removes the
    // bogus reference, not the declaration it was attached to.
    let qualified: Vec<&str> = c
        .symbols
        .iter()
        .map(|s| s.qualified_name.as_str())
        .collect();
    assert!(qualified.contains(&"a.c::helper"), "{qualified:?}");
    assert!(qualified.contains(&"a.c::caller"), "{qualified:?}");
}

/// A C-family template call names its callee; it binds no local.
///
/// `dev_t<T>(p)` puts the callee identifier on the `name` field of a
/// `template_function`, and the generic "a `name` field is a binding"
/// rule in `is_defining_name` filed it as a local of the calling
/// function. The resolver then classified the recorded call as
/// `local_binding` — "the callee is a local binding whose value is not
/// known" — and `analyze_liveness` reported the helper confidently dead.
/// Measured on MLSystemsLab: every `__device__` helper and every Metal
/// `inline` template helper called through `f<T>(…)` — `load_b`,
/// `copy_wait`, `stage_tile`, `norm_rope_row` — was published at 0.9 with
/// no reason, while the plain-call twins beside them resolved.
#[test]
fn a_c_family_template_call_names_its_callee_and_binds_no_local() {
    for (path, lang, source) in [
            (
                "k.cu",
                "cuda",
                "template <typename T>\n__device__ __forceinline__ void dev_t(T *p) { p[0] = 1; }\n\
                 template <typename T>\n__global__ void kern(T *p) {\n    dev_t<T>(p);\n}\n",
            ),
            (
                "a.metal",
                "cpp",
                "template <bool B>\ninline void dev_t(device float *row, uint n) { row[0] = n; }\n\
                 kernel void entry_k(device float *row [[buffer(0)]], uint gid [[thread_position_in_grid]]) {\n\
                 \x20   dev_t<false>(row, gid);\n}\n",
            ),
        ] {
            let extraction = extract_treesitter(path, lang, source);
            let call = extraction
                .calls
                .iter()
                .find(|call| call.callee_name == "dev_t")
                .unwrap_or_else(|| panic!("{path}: the template call is recorded: {:?}", extraction.calls));
            assert!(
                extraction.local_binding_at(call.span.start_byte, "dev_t").is_none(),
                "{path}: a template callee is a use of the helper, not a local of the caller: {:?}",
                extraction.local_bindings
            );
            assert!(
                !name_refs(&extraction).contains(&"dev_t"),
                "{path}: the callee position is a Call reference, not also a Name one: {:?}",
                extraction.references
            );
        }
}

/// The template wrapper is transparent in both directions.
///
/// An explicit specialization `spec<float>` is a declaration: its name
/// sits on the same `template_function` node a call uses, and clearing
/// that node blanket-wise would have the specialization *reference* the
/// primary template — the self-reference shape
/// `c_family_declarations_do_not_reference_themselves` pins. A
/// `template_type` use, `Tiles<4>::BR`, is the mirror image: it was
/// suppressed as a declaration, so a template struct whose only mentions
/// were instantiations looked confidently dead (`Tiles`, `MmaTiles` in
/// `flash_attn_cuda.cu`). The partial specialization keeps declaring.
#[test]
fn a_template_specialization_declares_and_a_template_type_use_references() {
    let specialised = extract_treesitter(
        "s.cu",
        "cuda",
        "template <typename T>\n__device__ void spec(T *p) { p[0] = 2; }\n\
             template <>\n__device__ void spec<float>(float *p) { p[0] = 3; }\n",
    );
    assert!(
        !name_refs(&specialised).contains(&"spec"),
        "an explicit specialization declares its name; it does not reference the primary: {:?}",
        specialised.references
    );
    assert!(
        specialised.local_bindings.is_empty(),
        "a specialization's own name is a symbol binding, not a local: {:?}",
        specialised.local_bindings
    );

    let used = extract_treesitter(
        "t.cu",
        "cuda",
        "template <int N>\nstruct Tiles { static constexpr int BR = N; };\n\
             __global__ void kern(float *p) { int x = Tiles<4>::BR; p[0] = x; }\n",
    );
    assert!(
        used.references
            .iter()
            .any(|reference| reference.name == "Tiles"
                && matches!(reference.kind, ReferenceKind::Type | ReferenceKind::Name)),
        "`Tiles<4>::BR` is a use of the template struct: {:?}",
        used.references
    );

    let partial = extract_treesitter(
        "u.cu",
        "cuda",
        "template <int N>\nstruct Tiles { static constexpr int BR = N; };\n\
             template <>\nstruct Tiles<8> { static constexpr int BR = 1; };\n",
    );
    assert!(
        !partial
            .references
            .iter()
            .any(|reference| reference.name == "Tiles"),
        "a specialization's head declares; it is not a use of the primary: {:?}",
        partial.references
    );
}

/// A nested function decorated with a parameter of its enclosing function
/// is handed to that parameter — a callable the caller injected — and the
/// callee decides when it runs.
///
/// The `register(test, Skip)` seam: `nanolab/tests.py` calls each test
/// module's `register(test, Skip)`, and inside it every test is
/// `@test def name(): …`. Nothing else in the module names the function;
/// 26 of them were published at 0.9 with no reason. The gate is the
/// *binding* of the decorator's root, not its spelling: an imported
/// decorator, a module-level function of the same name reached from a
/// sibling scope, a member access whose root is not the parameter, and a
/// nearer scope that rebinds the name all stay ordinary decorated
/// functions — and `never_passed_anywhere` stays dead.
#[test]
fn an_injected_decorator_hands_the_nested_function_to_its_caller() {
    let extraction = extract_treesitter(
        "c.py",
        "python",
        "import functools\n\
             \n\
             def register(test, Skip):\n\
             \x20   @test\n\
             \x20   def collected_by_injected_decorator():\n\
             \x20       pass\n\
             \n\
             \x20   @test.skip('reason')\n\
             \x20   def collected_through_a_member_of_the_parameter():\n\
             \x20       pass\n\
             \n\
             \x20   @functools.lru_cache\n\
             \x20   def decorated_by_import_dead():\n\
             \x20       pass\n\
             \n\
             \x20   def never_passed_anywhere():\n\
             \x20       pass\n\
             \n\
             \x20   def inner():\n\
             \x20       test = functools.lru_cache\n\
             \x20       @test\n\
             \x20       def rebound_in_the_nearer_scope():\n\
             \x20           pass\n\
             \n\
             def test(fn):\n\
             \x20   return fn\n\
             \n\
             def sibling_collision(unrelated):\n\
             \x20   @test\n\
             \x20   def decorated_by_the_module_function():\n\
             \x20       pass\n\
             \n\
             def qualified_collision(test):\n\
             \x20   @other.test\n\
             \x20   def decorated_by_a_member_named_like_the_parameter():\n\
             \x20       pass\n\
             \n\
             @test\n\
             def module_level_decorated():\n\
             \x20   pass\n",
    );
    let mut targets: Vec<&str> = extraction
        .wiring
        .iter()
        .filter(|annotation| annotation.kind == WiringKind::RuntimeEntryPoint)
        .map(|annotation| annotation.target_symbol.as_str())
        .collect();
    targets.sort_unstable();
    assert_eq!(
        targets,
        [
            "c.py::register.collected_by_injected_decorator",
            "c.py::register.collected_through_a_member_of_the_parameter",
        ],
        "only a decorator rooted at a parameter of an enclosing callable is injected: {:?}",
        extraction.wiring
    );
    let reason = extraction
        .wiring
        .iter()
        .find(|annotation| {
            annotation.target_symbol == "c.py::register.collected_by_injected_decorator"
        })
        .map(|annotation| annotation.details.as_str())
        .expect("annotated");
    assert!(
        reason.contains("`test`") && reason.contains("`register`"),
        "the reason names the decorator and the function that received it: {reason}"
    );
}

/// `Plain::K` reaches into `Plain`; the scope half is a use of the type.
///
/// tree-sitter-cpp spells the scope of a `qualified_identifier` as a
/// `namespace_identifier`, a node kind the name-reference walk never
/// visited, so a struct whose only mentions were `MmaTiles::kThreads`
/// and `MmaTiles::BR` had no inbound reference at all — not an
/// unresolved one, none — and was published confidently dead. A
/// `namespace ns { … }` head is the declaration of `ns`, not a use.
#[test]
fn a_qualified_scope_is_a_use_of_the_type_it_reaches_into() {
    let extraction = extract_treesitter(
        "q.cu",
        "cuda",
        "struct Plain { static constexpr int K = 4; };\n\
             __global__ void kern(float *p) { constexpr int k = Plain::K; p[0] = k; }\n\
             namespace ns { int x; }\n",
    );
    assert!(
        extraction
            .references
            .iter()
            .any(|reference| reference.name == "Plain"
                && matches!(reference.kind, ReferenceKind::Type | ReferenceKind::Name)),
        "`Plain::K` names Plain: {:?}",
        extraction.references
    );
    assert!(
        !extraction
            .references
            .iter()
            .any(|reference| reference.name == "ns"),
        "a namespace head declares; it is not a use: {:?}",
        extraction.references
    );
}

/// A function-like macro's body calls what it names, and the macro is the
/// caller.
///
/// The grammar leaves a `#define` body as one `preproc_arg` token, so a
/// helper whose only call sites sit inside macro bodies — every
/// `mm_bf16_*`, `mm_nn_coop_f32acc` and `mm_i8_dequant_coop` in
/// MLSystemsLab's Metal kernels, each stamped out by a `KERNEL(...)`
/// macro — had no inbound edge and was published confidently dead. The
/// body is parsed by the file's own grammar, the way `probe_macro_body`
/// already does for Rust, so a name inside a comment or a string, a
/// keyword before `(`, and the macro's own parameters contribute nothing.
///
/// The bodies are shaped to what the grammar's preprocessor scanner
/// accepts: measured, a block comment closing a continuation line that is
/// followed by a keyword line (`if`) or a template-call line (`f<T>(…)`)
/// makes tree-sitter drop the whole macro into `preproc_def` + `ERROR`,
/// in both the C++ and CUDA grammars. That macro is then not a symbol at
/// all — fail-open, one more finding per helper it calls — and nothing
/// here can reach it, so the comment and the template call live in
/// different macros.
#[test]
fn a_function_like_macro_body_calls_what_it_names() {
    let extraction = extract_treesitter(
        "k.cu",
        "cuda",
        "#define STAMP(NAME) \\\n\
             \x20 __global__ void NAME(float *p) { \\\n\
             \x20   dev_plain(p); /* dead_in_comment(p) */ \\\n\
             \x20   dev_plain(p); \\\n\
             \x20 }\n\
             #define STAMP2(NAME) \\\n\
             \x20 __global__ void NAME(float *p) { \\\n\
             \x20   macro_only_helper<float>(p); p[0] = sizeof(\"dead_in_string(p)\"); \\\n\
             \x20   constexpr int k = Plain::K; p[1] = k; \\\n\
             \x20 }\n\
             #define GUARD(x) if (!(x)) return;\n\
             #define APPLY(f, x) f(x)\n\
             #define TORN(x) torn_helper(x) +\n\
             __device__ void dev_plain(float *p) { p[1] = 1; }\n\
             __device__ int torn_helper(float *p) { return 1; }\n\
             template <typename T> __device__ void macro_only_helper(T *p) { p[3] = 1; }\n\
             __device__ void dead_in_comment(float *p) { p[4] = 1; }\n\
             __device__ void dead_in_string(float *p) { p[5] = 1; }\n\
             struct Plain { static constexpr int K = 4; };\n\
             STAMP(stamped_kernel)\n\
             STAMP2(stamped_kernel2)\n",
    );
    let calls_from = |caller: &str| -> Vec<&str> {
        let mut names: Vec<&str> = extraction
            .calls
            .iter()
            .filter(|call| call.caller_symbol.as_deref() == Some(caller))
            .map(|call| call.callee_name.as_str())
            .collect();
        names.sort_unstable();
        names.dedup();
        names
    };
    assert_eq!(
        calls_from("k.cu::STAMP"),
        ["dev_plain"],
        "a name inside a comment is not a callee: {:?}",
        extraction.calls
    );
    assert_eq!(
        calls_from("k.cu::STAMP2"),
        ["macro_only_helper"],
        "a template call is a call; a name inside a string and `sizeof` are not: {:?}",
        extraction.calls
    );
    assert!(
        calls_from("k.cu::GUARD").is_empty(),
        "a keyword before `(` is not a callee: {:?}",
        extraction.calls
    );
    assert!(
        calls_from("k.cu::APPLY").is_empty(),
        "a macro parameter in callee position is the caller's argument, not a symbol: {:?}",
        extraction.calls
    );
    // `torn_helper(x) +` is an expression fragment: both probe parses can
    // only place the call inside an `ERROR` node. Refusing it is the
    // fail-open direction — the helper shows up as one finding too many
    // rather than being resurrected by a token the grammar could not
    // place.
    assert!(
        calls_from("k.cu::TORN").is_empty(),
        "a call the grammar could only place under an ERROR node is not a syntactic fact: {:?}",
        extraction.calls
    );
    assert!(
        extraction
            .references
            .iter()
            .any(|reference| reference.name == "macro_only_helper"
                && reference.kind == ReferenceKind::Call
                && reference.enclosing_symbol.as_deref() == Some("k.cu::STAMP2")),
        "the call is also a reference the resolver can attribute: {:?}",
        extraction.references
    );
    // The Metal spelling: `kernel void` plus `[[buffer(n)]]` attributes
    // inside the body, parsed by the C++ grammar.
    let metal = extract_treesitter(
            "a.metal",
            "cpp",
            "#define TUNE(NAME, SM) \\\n\
             \x20 kernel void NAME(device float *A [[buffer(0)]], uint tg [[threadgroup_position_in_grid]]) { \\\n\
             \x20   mm_tune<SM>(A, tg); \\\n\
             \x20 }\n\
             template <int SM> inline void mm_tune(device float *A, uint tg) { A[tg] = SM; }\n\
             TUNE(mm_64, 64)\n",
        );
    assert!(
        metal.calls.iter().any(|call| call.callee_name == "mm_tune"
            && call.caller_symbol.as_deref() == Some("a.metal::TUNE")),
        "a Metal kernel-stamping macro calls its template helper: {:?}",
        metal.calls
    );
}

/// The declarator suppression follows `declarator` identity, not subtree
/// containment.
///
/// `field_contains` — the test every other arm of `is_defining_name` uses —
/// is a *subtree* test, and a C declarator subtree also holds the signature
/// (`function_declarator.parameters`) and the array bound
/// (`array_declarator.size`). Widening to containment therefore silently
/// erases parameter types and array bounds, which are genuine cross-symbol
/// references, and the symbols they name start reading as dead. These are
/// the identifiers that must survive on the other side of the fix.
#[test]
fn c_declarator_suppression_spares_real_references() {
    let c = extract_treesitter(
        "b.c",
        "c",
        concat!(
            "int table[LIMIT];\n",
            "int total = origin;\n",
            "void takes(Store *s);\n",
        ),
    );
    let names = name_refs(&c);
    assert!(
        names.contains(&"LIMIT"),
        "an array bound sits under `size`, not the declarator chain: {names:?}"
    );
    assert!(
        names.contains(&"origin"),
        "an initializer sits under `value`, not the declarator chain: {names:?}"
    );
    let types: Vec<&str> = c
        .references
        .iter()
        .filter(|r| r.kind == ReferenceKind::Type)
        .map(|r| r.name.as_str())
        .collect();
    assert!(
        types.contains(&"Store"),
        "a parameter type is inside the function declarator but is a use: {types:?}"
    );

    // The bindings on the near side stay suppressed.
    for declared in ["table", "total", "takes", "s"] {
        assert!(
            !names.contains(&declared),
            "{declared:?} is declared here, not used: {names:?}"
        );
    }
}

/// A C function name is a symbol binding, not a scope-local.
///
/// `collect_non_symbol_locals` files every `is_defining_name` that is not an
/// `is_symbol_binding` into a per-scope shadow set, and
/// `name_is_shadowed_by_local` then drops `Name` references matching it. Once
/// the declarator arm makes a function's own identifier a defining name, the
/// function would shadow itself inside its own body and non-call uses of it —
/// taking its address, passing it as a callback — would vanish.
#[test]
fn c_function_names_are_symbol_bindings_not_locals() {
    let c = extract_treesitter(
        "c.c",
        "c",
        concat!(
            "int helper(int x) { return x + 1; }\n",
            "void install(void) { register_cb(helper); }\n",
        ),
    );
    let names = name_refs(&c);
    assert!(
        names.contains(&"helper"),
        "a function passed as a value is a real use: {names:?}"
    );
    assert_eq!(
        names.iter().filter(|name| **name == "helper").count(),
        1,
        "only the callback use, never the declarator: {names:?}"
    );
}

/// Every call syntax splits into the right `(callee, receiver)` pair.
///
/// `split_call_target` has one arm per grammar's member-call node, and each
/// arm was independently deletable — deleting the `member_expression` arm
/// makes JS `obj.method()` fall through to the catch-all, which reports the
/// callee as the literal text `obj.method` with no receiver. Nothing errors:
/// the call is still recorded, it just names a symbol that does not exist,
/// so the edge never joins and `method` looks uncalled. This is the same
/// shape as SC9/SC10, where unjoinable call edges produced 74,312 orphans.
///
/// Each expected pair below was read off the extractor before being pinned.
#[test]
fn member_calls_split_into_callee_and_receiver_in_every_grammar() {
    let cases: [(&str, &str, &str, &str, Option<&str>); 5] = [
        // (file, grammar, source, expected callee, expected receiver)
        (
            "f.py",
            "python",
            "import os\ndef run():\n    os.path.join('a')\n",
            "join",
            Some("os.path"),
        ),
        (
            "f.js",
            "javascript",
            "function run() {\n  obj.method(1);\n}\n",
            "method",
            Some("obj"),
        ),
        (
            "f.rs",
            "rust",
            "fn run() {\n    let x = Thing::new();\n    x.method();\n}\n",
            "method",
            Some("x"),
        ),
        (
            "f.go",
            "go",
            "package main\nfunc run() {\n\tsvc.Method()\n}\n",
            "Method",
            Some("svc"),
        ),
        (
            "f.ts",
            "typescript",
            "function run(): void {\n  this.svc.doIt();\n}\n",
            "doIt",
            Some("this.svc"),
        ),
    ];

    for (path, grammar, source, callee, receiver) in cases {
        let extraction = extract_treesitter(path, grammar, source);
        let call = extraction
            .calls
            .iter()
            .find(|call| call.callee_name == callee)
            .unwrap_or_else(|| {
                panic!(
                    "{grammar}: no call named {callee:?}; got {:?}",
                    extraction
                        .calls
                        .iter()
                        .map(|c| (&c.callee_name, &c.receiver_expr))
                        .collect::<Vec<_>>()
                )
            });
        assert_eq!(
            call.receiver_expr.as_deref(),
            receiver,
            "{grammar}: wrong receiver for {callee}"
        );
        assert!(
            !call.callee_name.contains('.'),
            "{grammar}: the callee must be the bare method name, not the whole \
                 member expression, or the edge cannot join: {:?}",
            call.callee_name
        );
    }
}

/// A plain call has no receiver, and a definition name is not a reference.
///
/// The catch-all arm of `split_call_target` must leave `receiver_expr`
/// `None` — a fabricated receiver turns an ordinary free-function call into
/// a method call on a type that does not exist. Separately, `is_defining_name`
/// decides whether an identifier at a declaration site is a *use*: inverted,
/// every function would reference itself, which is precisely the shape that
/// makes a dead symbol look live.
#[test]
fn free_calls_have_no_receiver_and_definitions_are_not_uses() {
    for (path, grammar, source) in [
        ("f.py", "python", "def run():\n    helper()\n"),
        ("f.js", "javascript", "function run() {\n  helper();\n}\n"),
        ("f.rs", "rust", "fn run() {\n    helper();\n}\n"),
        ("f.go", "go", "package main\nfunc run() {\n\thelper()\n}\n"),
    ] {
        let extraction = extract_treesitter(path, grammar, source);

        let call = extraction
            .calls
            .iter()
            .find(|call| call.callee_name == "helper")
            .unwrap_or_else(|| panic!("{grammar}: `helper()` must be recorded as a call"));
        assert_eq!(
            call.receiver_expr, None,
            "{grammar}: a free call must not invent a receiver"
        );
        assert!(
            call.caller_symbol
                .as_deref()
                .is_some_and(|c| c.ends_with("run")),
            "{grammar}: the call must be attributed to its enclosing function, or it \
                 cannot make `run` reach `helper`: {:?}",
            call.caller_symbol
        );

        // The callee is a Call-kind reference, so liveness sees an invocation
        // rather than a bare name occurrence.
        assert!(
            extraction
                .references
                .iter()
                .any(|r| r.name == "helper" && r.kind == ReferenceKind::Call),
            "{grammar}: the callee must be a Call reference: {:?}",
            extraction
                .references
                .iter()
                .map(|r| (&r.name, r.kind))
                .collect::<Vec<_>>()
        );

        // ...and *only* a Call reference. `is_call_callee` exists to stop a
        // callee being pushed a second time as a bare Name; inverted, every
        // call is double-counted as both an invocation and a name use.
        assert_eq!(
            extraction
                .references
                .iter()
                .filter(|r| r.name == "helper")
                .count(),
            1,
            "{grammar}: the callee must be recorded once, not as both Call and Name: {:?}",
            extraction
                .references
                .iter()
                .map(|r| (&r.name, r.kind))
                .collect::<Vec<_>>()
        );

        // The declaration site of `run` is not a use of `run`.
        assert!(
            !extraction.references.iter().any(|r| r.name == "run"),
            "{grammar}: a function's own declaration name must not be recorded as a \
                 reference to itself: {:?}",
            extraction
                .references
                .iter()
                .map(|r| (&r.name, r.kind))
                .collect::<Vec<_>>()
        );
    }
}

/// Definition sites are suppressed; the values they bind are not.
///
/// Four separate guards decide whether an identifier is a *use*, and each
/// was mutable without a failure. They all fail in the same direction —
/// a use stops being recorded, so the symbol it names loses an incoming
/// edge and moves toward `dead` — except the object-key case, which fails
/// the other way and invents a use of whatever the key is named.
///
/// Every expectation below was read off the extractor before being pinned.
#[test]
fn definition_sites_are_suppressed_but_their_values_are_recorded() {
    // Type annotations are Type references. Deleting the `type_identifier`
    // arm drops every annotation, which is how a type used only in
    // signatures starts reading as unreferenced.
    let types = extract_treesitter(
        "f.ts",
        "typescript",
        "function f(x: Foo): Bar {\n  return x as Baz;\n}\n",
    );
    let type_refs: Vec<&str> = types
        .references
        .iter()
        .filter(|r| r.kind == ReferenceKind::Type)
        .map(|r| r.name.as_str())
        .collect();
    for expected in ["Foo", "Bar", "Baz"] {
        assert!(
            type_refs.contains(&expected),
            "parameter, return and cast annotations are all type uses: {type_refs:?}"
        );
    }

    // The right-hand side of an assignment is a use even though the
    // statement as a whole is a binding. Collapsing the `matches!(..) &&
    // field == left` conjunction to `||` swallows every RHS identifier.
    let assign = extract_treesitter(
        "f.py",
        "python",
        "def run():\n    x = helper_value\n    return x\n",
    );
    let assign_names: Vec<&str> = assign.references.iter().map(|r| r.name.as_str()).collect();
    assert!(
        assign_names.contains(&"helper_value"),
        "the value assigned is a use of that value: {assign_names:?}"
    );
    assert!(
        !assign_names.contains(&"x"),
        "the assignment target is a binding, not a use: {assign_names:?}"
    );

    // In an object literal the key is a binding and the value is a use.
    // Inverting the `pair` test makes the key a reference to whatever
    // symbol shares its name.
    let pair = extract_treesitter(
        "f.js",
        "javascript",
        "function run() {\n  return { key: value };\n}\n",
    );
    let pair_names: Vec<&str> = pair.references.iter().map(|r| r.name.as_str()).collect();
    assert_eq!(
        pair_names,
        ["value"],
        "an object literal records its value and not its key"
    );

    // A member expression passed as an *argument* is a use, not a callee.
    // Widening the callee test to `||` suppresses it, so `b` in `f(a.b)`
    // disappears from the graph entirely.
    let argument = extract_treesitter("f.js", "javascript", "function run() {\n  f(a.b);\n}\n");
    let arg_names: Vec<&str> = argument
        .references
        .iter()
        .filter(|r| r.kind == ReferenceKind::Name)
        .map(|r| r.name.as_str())
        .collect();
    assert!(
        arg_names.contains(&"a") && arg_names.contains(&"b"),
        "both halves of an argument member expression are uses: {arg_names:?}"
    );
    assert!(
        argument
            .references
            .iter()
            .any(|r| r.name == "f" && r.kind == ReferenceKind::Call),
        "the actual callee is still a Call reference"
    );
}

/// The exact symbol set for each linked grammar.
///
/// `extract_node` is one large dispatch and carried 58 surviving mutants:
/// individual node kinds could be dropped, exportedness flipped, and
/// qualified names rebuilt, all without a failure, because no test asserted
/// the *whole* set — only that particular symbols were present. A missing
/// symbol is the dangerous direction: it is not an error anywhere, the file
/// still extracts "successfully", and the symbol simply never exists to be
/// called, so nothing downstream can notice.
///
/// This pins each tuple as `(kind, qualified_name, is_exported)`. The
/// qualified names are load-bearing: they are the join keys the call graph
/// resolves against, and two of them encode fixes that were expensive to
/// get right — `f.rs::Thing.go` vs `f.rs::Doer.go` is SC11's type-first
/// qualification of impl items, and `f.go::Client.Run` is SC9's receiver
/// ownership, without which Go method edges do not join at all.
///
/// This is a record of current behavior, not an endorsement of it. One
/// entry is a parity artifact rather than intent: a TS `type` alias is
/// reported as `Interface`. It is pinned so that changing it is a visible
/// decision instead of a silent drift.
///
/// Exported module-level bindings *are* emitted now (`f.go::Limit`); this
/// golden previously pinned their absence as deliberate, which is how the
/// omission survived unnoticed. A Python module constant is a symbol too —
/// every module-scope name is importable, so `__all__` decides only
/// `is_exported` — while a Python class field still produces none.
#[test]
fn each_grammar_extracts_its_exact_symbol_set() {
    /// `(kind, qualified_name, is_exported)`.
    type Symbol<'a> = (SymbolKind, &'a str, bool);
    /// `(path, grammar, source, expected symbols)`.
    type Case<'a> = (&'a str, &'a str, &'a str, &'a [Symbol<'a>]);

    let cases: [Case<'_>; 4] = [
            (
                "f.py",
                "python",
                "import os\n\nCONST = 1\n\nclass Widget:\n    attr: int = 0\n                     def render(self):\n        return helper()\n\ndef helper():\n    return 1\n",
                &[
                    (SymbolKind::File, "f.py", true),
                    (SymbolKind::Variable, "f.py::CONST", false),
                    (SymbolKind::Class, "f.py::Widget", false),
                    (SymbolKind::Method, "f.py::Widget.render", false),
                    (SymbolKind::Function, "f.py::helper", false),
                ],
            ),
            (
                "f.ts",
                "typescript",
                "export interface Shape { area(): number }\nexport type Alias = string;\n                 export class Box implements Shape {\n  private size = 1;\n                   area(): number { return this.size; }\n}\n                 export function make(): Box { return new Box(); }\nconst local = 5;\n",
                &[
                    (SymbolKind::File, "f.ts", true),
                    (SymbolKind::Interface, "f.ts::Shape", true),
                    (SymbolKind::Method, "f.ts::Shape.area", true),
                    (SymbolKind::Interface, "f.ts::Alias", true),
                    (SymbolKind::Class, "f.ts::Box", true),
                    (SymbolKind::Method, "f.ts::Box.area", true),
                    (SymbolKind::Function, "f.ts::make", true),
                ],
            ),
            (
                "f.rs",
                "rust",
                "pub struct Thing { pub id: u32 }\npub enum Mode { On, Off }\n                 pub trait Doer { fn go(&self); }\nimpl Doer for Thing { fn go(&self) {} }\n                 impl Thing { pub fn new() -> Self { Thing { id: 0 } } }\n                 pub fn top() {}\nfn private_fn() {}\n",
                &[
                    (SymbolKind::File, "f.rs", true),
                    (SymbolKind::Struct, "f.rs::Thing", true),
                    (SymbolKind::Enum, "f.rs::Mode", true),
                    (SymbolKind::Trait, "f.rs::Doer", true),
                    (SymbolKind::Method, "f.rs::Doer.go", false),
                    (SymbolKind::Method, "f.rs::Thing.go", false),
                    (SymbolKind::Method, "f.rs::Thing.new", true),
                    (SymbolKind::Function, "f.rs::top", true),
                    (SymbolKind::Function, "f.rs::private_fn", false),
                ],
            ),
            (
                "f.go",
                "go",
                "package svc\n\ntype Client struct { ID int }\n\n                 type Runner interface { Run() error }\n\n                 func (c *Client) Run() error { return nil }\n\n                 func New() *Client { return &Client{} }\n\nfunc helper() {}\n\n                 const Limit = 10\n",
                &[
                    (SymbolKind::File, "f.go", true),
                    (SymbolKind::Struct, "f.go::Client", true),
                    // An interface is an `Interface`, and each method it
                    // requires is a symbol a call through it can name.
                    (SymbolKind::Interface, "f.go::Runner", true),
                    (SymbolKind::Method, "f.go::Runner.Run", true),
                    (SymbolKind::Method, "f.go::Client.Run", true),
                    (SymbolKind::Function, "f.go::New", true),
                    (SymbolKind::Function, "f.go::helper", false),
                    // An exported package-level constant is public API surface.
                    (SymbolKind::Variable, "f.go::Limit", true),
                ],
            ),
        ];

    for (path, grammar, source, expected) in cases {
        let extraction = extract_treesitter(path, grammar, source);
        assert!(
            matches!(extraction.parse_outcome, ParseOutcome::Clean),
            "{grammar}: the fixture must parse cleanly, or the symbol set below is \
                 measuring a broken parse: {:?}",
            extraction.parse_outcome
        );

        let actual: Vec<Symbol<'_>> = extraction
            .symbols
            .iter()
            .map(|symbol| {
                (
                    symbol.kind,
                    symbol.qualified_name.as_str(),
                    symbol.is_exported,
                )
            })
            .collect();
        assert_eq!(actual, expected.to_vec(), "{grammar}: symbol set drifted");

        // Every symbol's parent chain resolves inside this file, so the
        // containment edges the graph is built from cannot dangle.
        for symbol in &extraction.symbols {
            let Some(parent) = symbol.parent_symbol.as_deref() else {
                assert_eq!(
                    symbol.kind,
                    SymbolKind::File,
                    "{grammar}: only the file symbol may be parentless"
                );
                continue;
            };
            assert!(
                extraction
                    .symbols
                    .iter()
                    .any(|candidate| candidate.qualified_name == parent),
                "{grammar}: {} names parent {parent:?}, which is not a symbol in this file",
                symbol.qualified_name
            );
        }
    }
}

/// The exact import and export set for each linked grammar.
///
/// The companion to the symbol golden above, aimed at the other half of
/// `extract_node`'s surviving mutants: whole `import_statement`,
/// `import_spec` and `variable_declarator` arms were deletable, and the
/// export-detection conditions were invertible in every direction.
///
/// Imports are what make a symbol reachable from another file. Drop an
/// import arm and the file's edges to its dependencies vanish — the
/// extraction still succeeds, so the only visible effect is that the
/// imported symbols stop having callers and drift toward `dead`. Invert an
/// export test and the reverse happens: private declarations are published
/// as public API, which suppresses genuine dead-code findings.
///
/// `(module_specifier, imported_names, local_names, alias)` is pinned
/// because the resolver joins on all four — SC12's binding work depends on
/// `imported_names` and `local_names` staying aligned and in order.
#[test]
fn each_grammar_extracts_its_exact_imports_and_exports() {
    /// `(module_specifier, imported_names, local_names, alias)`.
    type Import<'a> = (&'a str, Vec<&'a str>, Vec<&'a str>, Option<&'a str>);
    /// `(path, grammar, source, expected imports, expected exports)`.
    type Case<'a> = (&'a str, &'a str, &'a str, Vec<Import<'a>>, Vec<&'a str>);

    let cases: [Case<'_>; 4] = [
            (
                "f.py",
                "python",
                "import os\nimport os.path as osp\nfrom .rel import thing\n                 from pkg.mod import a, b as c\n",
                vec![
                    ("os", vec![], vec![], None),
                    ("os.path", vec![], vec![], Some("osp")),
                    (".rel", vec!["thing"], vec!["thing"], None),
                    // `b as c` must keep the imported and local halves aligned:
                    // the resolver binds `c` in this file to `b` in `pkg.mod`.
                    ("pkg.mod", vec!["a", "b"], vec!["a", "c"], None),
                ],
                vec!["f.py"],
            ),
            (
                "f.ts",
                "typescript",
                "import def from 'm';\nimport { x, y as z } from './local';\n                 import * as ns from 'pkg';\nexport const val = 1;\nexport { x };\n                 export default function d() {}\n",
                vec![
                    ("m", vec![], vec![], Some("def")),
                    ("./local", vec!["x", "y"], vec!["x", "z"], None),
                    // A namespace import records the `*` sentinel, which is how
                    // `ns.anything` stays resolvable without enumerating names.
                    ("pkg", vec!["*"], vec!["ns"], Some("ns")),
                    // `export { x };` is deliberately absent. It used to appear
                    // here as `("", ["x"], ["x"], None)` — an import with an
                    // empty module specifier — and this expectation pinned the
                    // defect rather than the intent: a local re-export with no
                    // `from` is an export and nothing else, and `""` is an
                    // endpoint no resolver can ever bind. It still appears in
                    // the export set below, which is where it belongs.
                ],
                // `default` is the name `export default function d` is
                // published under, beside `d` itself.
                vec!["d", "default", "f.ts", "val", "x"],
            ),
            (
                "f.go",
                "go",
                "package svc\n\nimport (\n\t\"fmt\"\n\t                 alias \"example.com/pkg/sub\"\n)\n\nimport \"single\"\n",
                vec![
                    ("fmt", vec![], vec![], None),
                    // Both the block form and the single form must be read, and
                    // a Go import alias is the package's local name.
                    ("example.com/pkg/sub", vec![], vec![], Some("alias")),
                    ("single", vec![], vec![], None),
                ],
                vec!["f.go"],
            ),
            (
                "f.rs",
                "rust",
                "use std::collections::BTreeMap;\nuse crate::thing::{One, Two as Three};\n                 pub use inner::Exported;\n",
                vec![
                    // X41. Each `use` names a module and the names it takes out
                    // of it, the shape every other grammar here produces. This
                    // expectation used to read
                    // `("crate::thing::{One, Two as Three}", [], [], None)` and
                    // said so in a comment — "pinned as current behavior, not
                    // intent". The intent is this: a module specifier that is
                    // the statement's own source text matches no file and binds
                    // no name, so nothing downstream could use it.
                    ("std::collections", vec!["BTreeMap"], vec!["BTreeMap"], None),
                    ("crate::thing", vec!["One", "Two"], vec!["One", "Three"], None),
                    ("inner", vec!["Exported"], vec!["Exported"], None),
                ],
                // `pub use inner::Exported;` was in this fixture from the start
                // and the export set recorded only the file node, which is the
                // defect stated as a pin: a Rust re-export produced no export at
                // all, so `compute_reexport_chains` had no Rust input and a
                // crate root's republished surface was invisible to it.
                vec!["Exported", "f.rs"],
            ),
        ];

    for (path, grammar, source, expected_imports, expected_exports) in cases {
        let extraction = extract_treesitter(path, grammar, source);
        assert!(
            matches!(extraction.parse_outcome, ParseOutcome::Clean),
            "{grammar}: fixture must parse cleanly: {:?}",
            extraction.parse_outcome
        );

        let actual: Vec<Import<'_>> = extraction
            .imports
            .iter()
            .map(|import| {
                (
                    import.module_specifier.as_str(),
                    import.imported_names.iter().map(String::as_str).collect(),
                    import.local_names.iter().map(String::as_str).collect(),
                    import.alias.as_deref(),
                )
            })
            .collect();
        assert_eq!(actual, expected_imports, "{grammar}: import set drifted");

        let mut exports: Vec<&str> = extraction
            .exports
            .iter()
            .map(|export| export.exported_name.as_str())
            .collect();
        exports.sort_unstable();
        assert_eq!(exports, expected_exports, "{grammar}: export set drifted");

        // No import names an empty module.
        //
        // The loop below already refused an empty binding *name*, and this
        // test still passed while every `export { x };` in the corpus
        // pushed an import whose module was `""` — the check was one field
        // short of the class it was written for.
        for import in &extraction.imports {
            assert!(
                !import.module_specifier.is_empty(),
                "{grammar}: an import must name a module, got {:?}",
                import.raw_import
            );
        }

        // Every import's binding pairs are well formed, so the resolver
        // cannot be handed a binding keyed on an empty name.
        for import in &extraction.imports {
            for (local, imported) in import.binding_pairs() {
                assert!(
                    !local.is_empty() && !imported.is_empty(),
                    "{grammar}: {import:?} produced an empty binding"
                );
            }
        }
    }
}

/// The macro probe recovers real calls and fabricates none.
///
/// `probe_macro_body` is a pure string function and every one of its 18
/// possible return values was substitutable without a failure, including
/// `("xyzzy", Some("xyzzy"))`. It is how SC13 recovers calls hidden inside
/// macro expansions: `println!("{}", helper())` is the only place `helper`
/// is called in a great deal of Rust, so an empty return makes those callees
/// look uncalled, and a fabricated return invents edges to symbols that do
/// not exist. Both were unobservable.
///
/// Every expectation below was read off the function before being pinned.
#[test]
fn the_macro_probe_recovers_real_calls_and_fabricates_none() {
    // A direct call inside a macro body is recovered, and the synthetic
    // probe wrapper never leaks out as a call of its own.
    assert_eq!(
        probe_macro_body("\"{}\", helper()"),
        (vec![("helper".to_string(), None)], vec![]),
        "a call inside a macro body is a real call"
    );

    // A nested macro is queued as a body rather than recursed into, so the
    // parser borrow stays scoped to one parse.
    assert_eq!(
        probe_macro_body("a, format!(\"{}\", b.c())"),
        (vec![], vec!["\"{}\", b.c()".to_string()]),
        "an inner macro is deferred, not expanded in place"
    );

    // Receivers survive the probe, so a macro-hidden method call still
    // joins to its type.
    let (calls, nested) = probe_macro_body("thing.go(), other()");
    assert!(nested.is_empty());
    assert_eq!(
        calls,
        vec![
            ("other".to_string(), None),
            ("go".to_string(), Some("thing".to_string())),
        ],
        "a method call inside a macro keeps its receiver"
    );

    // A declarative macro definition is not an argument list. Returning
    // nothing is required: a fabricated call is worse than a missing one.
    assert_eq!(
        probe_macro_body("$x:expr) => { $x "),
        (vec![], vec![]),
        "a macro_rules body must not be read as calls"
    );

    // A `select!`-shaped body is not a valid argument list. The fallback
    // scan still recovers ident-shaped calls, including method calls.
    let (calls, nested) = probe_macro_body("_ = ticker.tick() => { self.vanished_reason(); }");
    assert!(nested.is_empty());
    assert!(
        calls.contains(&("vanished_reason".to_string(), Some("self".to_string()))),
        "a method call inside a select! arm is a real call: {calls:?}"
    );
    assert!(
        !calls.iter().any(|(name, _)| name == "if" || name == "let"),
        "keywords followed by parentheses are not callees: {calls:?}"
    );
}

/// A macro body is unwrapped from any delimiter, and skipped when it holds
/// no call.
#[test]
fn macro_bodies_unwrap_every_delimiter_and_skip_callless_ones() {
    for wrapped in ["(a())", "[a()]", "{a()}"] {
        assert_eq!(
            macro_token_body(wrapped).as_deref(),
            Some("a()"),
            "`{wrapped}` must unwrap: `vec![..]` and `matches!{{..}}` are as common as \
                 parenthesised macros"
        );
    }

    // Without a parenthesis there is no call to recover, so the parse is
    // skipped entirely rather than run and discarded.
    for skipped in ["(nocall)", "()", "raw"] {
        assert_eq!(
            macro_token_body(skipped),
            None,
            "`{skipped}` holds no call and must not be probed"
        );
    }
}

/// Nested macros are followed to a bounded depth, and the bound holds.
///
/// The depth comparison was mutable in every direction and the `depth + 1`
/// increment to `-` and `*`. Loosened, a pathological nest runs away inside
/// extraction; tightened, ordinary two- and three-level nests such as
/// `assert_eq!(a, format!("{}", b.c()))` stop yielding their calls. The
/// boundary is measured, not assumed: five levels resolve, six do not.
#[test]
fn nested_macros_are_followed_to_a_bounded_depth() {
    let calls_at = |depth: usize| -> Vec<String> {
        let mut body = String::from("deep()");
        for _ in 0..depth {
            body = format!("m!({body})");
        }
        extract_treesitter("f.rs", "rust", &format!("fn run() {{ {body}; }}\n"))
            .calls
            .iter()
            .map(|call| call.callee_name.clone())
            .collect()
    };

    for depth in 1..=5 {
        assert!(
            calls_at(depth).contains(&"deep".to_string()),
            "a call nested {depth} macro levels deep must still be recovered"
        );
    }
    assert!(
        !calls_at(6).contains(&"deep".to_string()),
        "the depth bound must actually stop: six levels is past it"
    );
}

/// Attribute scanning walks past comments and stops at real code.
///
/// `rust_attribute_paths` was replaceable with an empty vector and with a
/// fabricated one, and both of its match arms were deletable. It feeds the
/// runtime-entry-point exemption, so an empty result marks `#[tokio::main]`
/// as ordinary dead code — the highest-volume Rust false positive the
/// exemption exists to prevent. Dropping the comment arm breaks the walk at
/// the first doc comment, which is where attributes usually sit.
#[test]
fn attribute_scanning_walks_past_comments_to_reach_the_entry_attribute() {
    let extraction = extract_treesitter(
            "f.rs",
            "rust",
            "#[tokio::main]\n// a comment between the attributes\n             #[allow(dead_code)]\nfn main() {}\n",
        );
    assert!(
        extraction.wiring.iter().any(|annotation| {
            annotation.kind == WiringKind::RuntimeEntryPoint
                && annotation.target_symbol == "f.rs::main"
        }),
        "`#[tokio::main]` must still be found with a comment and another attribute \
             between it and the function: {:?}",
        extraction.wiring
    );

    // A function with no attributes at all gets no fabricated annotation.
    let plain = extract_treesitter("g.rs", "rust", "fn helper() {}\n");
    assert!(
        plain.wiring.is_empty(),
        "no attribute means no annotation, not an invented one: {:?}",
        plain.wiring
    );
}

/// Rust's structural exemptions name their exact reason, and stop at the
/// right boundary.
///
/// `rust_method_structural_reason` was replaceable with `None`, `Some("")`
/// and `Some("xyzzy")`, and each of its three match arms was deletable.
/// It exists because a trait-impl method and a trait method both *reject*
/// `pub`, so `is_exported == false` carries no information about them —
/// treating that constant as evidence of deadness is the highest-volume
/// Rust false positive. `None` brings every one of those back; a constant
/// string makes all three indistinguishable to whoever reads the reason.
#[test]
fn rust_structural_exemptions_name_their_reason_and_stop_at_nested_fns() {
    let extraction = extract_treesitter(
            "f.rs",
            "rust",
            "pub trait Doer {\n    fn declared(&self);\n    fn defaulted(&self) {}\n}\n             pub struct Thing;\nimpl Doer for Thing {\n    fn declared(&self) {\n                     fn nested_helper() {}\n        nested_helper();\n    }\n}\n             impl Thing {\n    pub fn inherent(&self) {}\n}\n",
        );

    let reason_for = |symbol: &str| -> Option<String> {
        extraction
            .wiring
            .iter()
            .find(|annotation| annotation.target_symbol == symbol)
            .map(|annotation| annotation.details.clone())
    };

    // A bare signature and a defaulted method are exempt for the same
    // structural reason, but the wording must not call a declaration a
    // default — the reason is what a human reads before deleting the code.
    assert_eq!(
        reason_for("f.rs::Doer.declared").as_deref(),
        Some("declared trait method; Rust forbids `pub` on trait items")
    );
    assert_eq!(
        reason_for("f.rs::Doer.defaulted").as_deref(),
        Some("defaulted trait method; Rust forbids `pub` on trait items")
    );
    assert_eq!(
        reason_for("f.rs::Thing.declared").as_deref(),
        Some("implements a trait method; Rust forbids `pub` on trait-impl items")
    );

    // An inherent method *can* be `pub`, so its exportedness is real
    // evidence and it must not be exempted.
    assert_eq!(
        reason_for("f.rs::Thing.inherent"),
        None,
        "an inherent impl method can be `pub`, so it gets no structural pass"
    );

    // A `fn` nested inside a trait-impl method is not itself a method: the
    // walk must stop at the enclosing `function_item` rather than inherit
    // the impl block's exemption from farther up the ancestor chain.
    assert_eq!(
        reason_for("f.rs::Thing.nested_helper"),
        None,
        "a nested fn must not inherit its enclosing method's exemption"
    );

    // Every exemption is a StructuralExempt, not a runtime entry point.
    for annotation in &extraction.wiring {
        assert_eq!(annotation.kind, WiringKind::StructuralExempt);
        assert!(!annotation.details.is_empty(), "an exemption must say why");
    }
}

/// A Go file records its package clause and its qualified type uses.
///
/// `go_package_name` was replaceable with `None`, `Some("")` and
/// `Some("xyzzy")`. The package name survives durable persist precisely so
/// package-level import edges and G20 stars can be rebuilt after
/// `source_code` is stripped, so losing it silently breaks those edges on
/// reload rather than at extraction time. The `qualified_type` arm is what
/// records `pkg.Thing` as a use of `Thing`.
#[test]
fn go_records_its_package_and_qualified_type_uses() {
    let extraction = extract_treesitter(
            "f.go",
            "go",
            "package svc\n\ntype Client struct{}\n\n             func (c *Client) Do(x pkg.Thing) error { return nil }\n",
        );

    assert_eq!(
        extraction.go_package.as_deref(),
        Some("svc"),
        "the package clause is the file's package identity"
    );

    let type_refs: Vec<&str> = extraction
        .references
        .iter()
        .filter(|reference| reference.kind == ReferenceKind::Type)
        .map(|reference| reference.name.as_str())
        .collect();
    assert!(
        type_refs.contains(&"Thing"),
        "a qualified type `pkg.Thing` is a use of `Thing`: {type_refs:?}"
    );
    assert!(
        type_refs.contains(&"Client"),
        "the receiver type is a use: {type_refs:?}"
    );

    // A file with no package clause reports none rather than inventing one.
    let bare = extract_treesitter("g.go", "go", "type Loose struct{}\n");
    assert_eq!(bare.go_package, None);
}

fn exported_names(source: &str) -> Vec<(String, bool)> {
    extract_treesitter("m.js", "javascript", source)
        .symbols
        .into_iter()
        .filter(|symbol| symbol.kind != SymbolKind::File)
        .map(|symbol| (symbol.name, symbol.is_exported))
        .collect()
}

fn is_exported(source: &str, name: &str) -> bool {
    exported_names(source)
        .into_iter()
        .find(|(symbol, _)| symbol == name)
        .unwrap_or_else(|| panic!("{name} must be extracted from: {source}"))
        .1
}

/// A member of an object or class *expression* is exported when the value
/// holding it escapes the module.
///
/// The export check used to look only for a `class_declaration` ancestor
/// with an `export_statement` parent, so every member of an object literal
/// or an anonymous class read as private. Nothing in the corpus calls them
/// — the caller is the bundler, the browser, or the test harness — so each
/// one became a *confident* dead-code finding on code that is certainly
/// running. Measured on a first-party web corpus, this shape was
/// `manualChunks` (Rollup's chunking callback) plus the whole
/// `ResizeObserver`/`IntersectionObserver` test polyfill.
#[test]
fn a_member_of_an_escaping_object_or_class_expression_is_exported() {
    assert!(
        is_exported("export const handlers = { onClick() {} };\n", "onClick"),
        "an exported const's object members are public API"
    );
    assert!(
        is_exported(
            "export default { build: { output: { manualChunks(id) { return id; } } } };\n",
            "manualChunks"
        ),
        "a callback nested in the default export is reached through it"
    );
    assert!(
        is_exported("export function make() { return { get() {} }; }\n", "get"),
        "a method on the object an exported factory returns is public API"
    );
    assert!(
        is_exported("export const make = () => ({ get() {} });\n", "get"),
        "the same escape through an arrow's implicit return"
    );
    assert!(
        is_exported(
            "export function make() { return { nested: { inner() {} } }; }\n",
            "inner"
        ),
        "a method nested on the returned object still escapes through the factory"
    );
    for global in ["globalThis", "window", "global", "self"] {
        let source = format!("{global}.ResizeObserver = class {{ observe() {{}} }};\n");
        assert!(
            is_exported(&source, "observe"),
            "assigning to `{global}` publishes the member to the runtime"
        );
    }
}

/// A value that does not escape keeps every member detectable.
///
/// This is the half that stops the rule from being a blanket amnesty for
/// object literals. Losing it would silently disable dead-code detection
/// for every method-shaped property in JavaScript and TypeScript.
#[test]
fn members_of_a_value_that_never_escapes_stay_private() {
    for (source, name) in [
        ("const handlers = { onDead() {} };\n", "onDead"),
        ("class Hidden { neverUsed() {} }\n", "neverUsed"),
        ("const anon = class { alsoDead() {} };\n", "alsoDead"),
        ("let obj = { deepDead() {} };\n", "deepDead"),
        // A bare identifier target is a binding, not the global object.
        ("ResizeObserver = class { observe() {} };\n", "observe"),
        // A nested declaration does not inherit its enclosing function's
        // export.
        (
            "export function outer() { function inner() {} return inner; }\n",
            "inner",
        ),
        // Returned from a function that is itself not exported is not an
        // escape; the factory is the thing that would have to be public.
        ("function make() { return { get() {} }; }\n", "get"),
        // An unused helper nested inside an exported function must not
        // inherit that export. Skipping every enclosing function would
        // publish `get` here.
        (
            "export function outer() { function make() { return { get() {} }; } }\n",
            "get",
        ),
        // An object passed as an argument is not the return value.
        (
            "export function make() { foo({ get() {} }); return 1; }\n",
            "get",
        ),
        // Bound then returned by name needs escape analysis this extractor
        // does not do, so it reports what is evident.
        (
            "export function make() { const x = { get() {} }; return x; }\n",
            "get",
        ),
    ] {
        assert!(
            !is_exported(source, name),
            "`{name}` does not escape and must stay detectable: {source}"
        );
    }
}

/// The ordinary declaration forms are unchanged.
///
/// Every object-literal method in one file, with the exemption each got.
fn object_literal_exemptions(source: &str) -> Vec<(String, bool)> {
    let extraction = extract_treesitter("src/t.ts", "typescript", source);
    let exempt: std::collections::BTreeSet<&str> = extraction
        .wiring
        .iter()
        .filter(|annotation| annotation.kind == WiringKind::RuntimeEntryPoint)
        .map(|annotation| annotation.target_symbol.as_str())
        .collect();
    extraction
        .symbols
        .iter()
        .filter(|symbol| symbol.kind == SymbolKind::Method)
        .map(|symbol| {
            (
                symbol.name.clone(),
                exempt.contains(symbol.qualified_name.as_str()),
            )
        })
        .collect()
}

/// A callback interface handed to a library has no call site in this corpus
/// and never will, and the name lists could only cover libraries somebody
/// had thought of.
///
/// Measured on GitPulse: `registerLinkProvider({ provideLinks() {…} })` is
/// xterm's `ILinkProvider`, and `provideLinks` was reported dead at the
/// *confident* tier — 0.9, the tier whose contract is "safe to act on",
/// with `lcov.info` recording it executed.
#[test]
fn a_method_handed_to_a_callee_in_an_object_literal_is_exempt() {
    assert_eq!(
        object_literal_exemptions(
            "export function ensureTerm(created) {\n  \
                 return created.registerLinkProvider({\n    \
                 provideLinks(line, callback) { callback(undefined); },\n  });\n}\n"
        ),
        vec![("provideLinks".to_string(), true)]
    );
}

/// The case the rule must NOT take away, and the reason it is a walk rather
/// than "is this method inside an object literal".
///
/// `const o = { m() {} }` hands `m` to nobody. If nothing calls `o.m()`
/// then it really is dead, and reporting it is the correct answer.
#[test]
fn a_method_on_a_plain_local_object_is_still_reported() {
    assert_eq!(
        object_literal_exemptions(
            "export function f() {\n  const o = { standalone() { return 2; } };\n  \
                 return o;\n}\n"
        ),
        vec![("standalone".to_string(), false)]
    );
}

/// A method nested deeper inside the argument is still handed over: the
/// callee receives the whole structure and can reach it.
#[test]
fn a_method_nested_inside_the_argument_is_still_handed_over() {
    assert_eq!(
        object_literal_exemptions(
            "export function f() {\n  return wrap({ outer: { inner() { return 1; } } });\n}\n"
        ),
        vec![("inner".to_string(), true)]
    );
}

/// The walk stops at a function boundary, which is what keeps it from
/// exempting everything textually inside a call.
///
/// `f({ cb() { const o = { helper() {} }; } })` hands `cb` to `f` and keeps
/// `helper` to itself — `helper` is a local of `cb`'s body, which the call
/// never sees.
#[test]
fn a_method_declared_inside_a_handed_over_function_body_is_not_itself_handed_over() {
    let exemptions = object_literal_exemptions(
        "export function f() {\n  return reg({ cb() { const o = { helper() { return 1; } }; \
             return o; } });\n}\n",
    );
    assert_eq!(
        exemptions,
        vec![("cb".to_string(), true), ("helper".to_string(), false)],
        "the argument's own member is handed over; a local built inside its body is not"
    );
}

/// An array of handlers is a literal structure like any other.
#[test]
fn a_method_in_an_array_argument_is_handed_over() {
    assert_eq!(
        object_literal_exemptions(
            "export function f() {\n  return use([{ setup() { return 1; } }]);\n}\n"
        ),
        vec![("setup".to_string(), true)]
    );
}

/// `new Thing({ m() {} })` hands the object over exactly as a call does.
#[test]
fn a_method_handed_to_a_constructor_is_exempt() {
    assert_eq!(
        object_literal_exemptions(
            "export function f() {\n  return new Observer({ handleEvent(e) { return e; } });\n}\n"
        ),
        vec![("handleEvent".to_string(), true)]
    );
}

/// A TypeScript `satisfies` or `as` changes the type and not the value, so
/// the object still reaches the callee.
#[test]
fn a_type_assertion_between_the_literal_and_the_call_does_not_break_the_walk() {
    assert_eq!(
        object_literal_exemptions(
            "export function f() {\n  return reg({ provide() { return 1; } } as Provider);\n}\n"
        ),
        vec![("provide".to_string(), true)]
    );
}

/// A method on an object that is *returned* rather than passed is not
/// handed to a callee by this rule. It may still be spared by the
/// factory-return rule that owns that case; what matters here is that this
/// walk does not claim it, because `return` is not an argument list.
#[test]
fn a_method_on_a_returned_object_is_not_claimed_by_this_rule() {
    let extraction = extract_treesitter(
        "src/t.ts",
        "typescript",
        "function build() {\n  return { run() { return 1; } };\n}\n",
    );
    assert!(
        !extraction.wiring.iter().any(|annotation| {
            annotation
                .details
                .contains("declared in an object literal passed to")
        }),
        "a returned object was not passed to anything: {:?}",
        extraction.wiring
    );
}

/// The walk that follows escapes replaced a hand-rolled per-kind match, and
/// these are the cases that match had to keep getting right.
#[test]
fn ordinary_javascript_export_forms_are_unchanged() {
    assert!(is_exported("export class W { render() {} }\n", "render"));
    assert!(is_exported("export class W { render() {} }\n", "W"));
    assert!(!is_exported("class W { render() {} }\n", "render"));
    assert!(is_exported("export const helper = () => {};\n", "helper"));
    assert!(!is_exported("const helper = () => {};\n", "helper"));
    assert!(is_exported("export function fn() {}\n", "fn"));
    assert!(!is_exported("function fn() {}\n", "fn"));
}

/// A Go build constraint is recorded, from either the directive or the name.
///
/// The flag is what lets `analyze_liveness` tell a *spurious* ambiguity
/// between platform variants of one identity from a genuine one between two
/// unrelated symbols. Reporting no constraint makes every `_unix.go` pair a
/// dead-code finding on live code; reporting one everywhere silently
/// disables detection for the whole language.
#[test]
fn a_go_build_constraint_is_recorded_from_the_directive_or_the_filename() {
    let directive = extract_treesitter(
        "procgroup_other.go",
        "go",
        "//go:build !unix\n\npackage store\nfunc configure() {}\n",
    );
    assert!(
        directive.go_build_constrained,
        "`//go:build !unix` constrains the build"
    );

    let legacy = extract_treesitter(
        "old.go",
        "go",
        "// +build linux\n\npackage store\nfunc configure() {}\n",
    );
    assert!(
        legacy.go_build_constrained,
        "the legacy `// +build` form too"
    );

    // `unix` is a legal build *tag* but not a GOOS, so this file is
    // constrained by its directive and never by its name.
    let by_name = extract_treesitter("sock_linux.go", "go", "package net\nfunc opt() {}\n");
    assert!(
        by_name.go_build_constrained,
        "Go applies the `_GOOS` filename constraint with no directive present"
    );
    let arch = extract_treesitter("asm_amd64.go", "go", "package net\nfunc opt() {}\n");
    assert!(arch.go_build_constrained, "`_GOARCH` constrains too");
    let goos_arch = extract_treesitter("asm_linux_amd64.go", "go", "package net\nfunc opt() {}\n");
    assert!(goos_arch.go_build_constrained, "`_GOOS_GOARCH` constrains");
    let suffixed_test =
        extract_treesitter("net_linux_test.go", "go", "package net\nfunc opt() {}\n");
    assert!(
        suffixed_test.go_build_constrained,
        "`_test` is stripped first, so the constraint sits one component left"
    );
}

/// Nothing that merely resembles a constraint is read as one.
///
/// Each of these was a way for the flag to be true everywhere, which is the
/// direction that silently disables Go dead-code detection rather than the
/// one that reports it loudly.
#[test]
fn a_go_file_without_a_real_constraint_reports_none() {
    let plain = extract_treesitter("store.go", "go", "package store\nfunc configure() {}\n");
    assert!(
        !plain.go_build_constrained,
        "an ordinary file is unconstrained"
    );

    // The header ends at the package clause; below it, `//go:build` is an
    // ordinary comment. Reading it would let a comment anywhere in a file
    // suppress a real finding.
    let below = extract_treesitter(
        "late.go",
        "go",
        "package store\n\n//go:build unix\nfunc configure() {}\n",
    );
    assert!(
        !below.go_build_constrained,
        "a `//go:build` line below the package clause is a comment, not a constraint"
    );

    for name in [
        "helpers_other.go", // `other` is not a GOOS
        "raw_unix.go",      // `unix` is a build tag, not a GOOS
        "linux.go",         // Go needs something before the suffix
        "config_test.go",   // `_test` alone is not a platform
    ] {
        let extraction = extract_treesitter(name, "go", "package p\nfunc f() {}\n");
        assert!(
            !extraction.go_build_constrained,
            "{name} carries no GOOS/GOARCH suffix and no directive"
        );
    }

    // The flag is Go-only: a Python file named like a Go platform variant
    // must never set it.
    let python = extract_treesitter("sock_linux.py", "python", "def f():\n    pass\n");
    assert!(!python.go_build_constrained, "the flag is Go-only");
}

/// `__all__` is read only where it is actually assigned.
///
/// The slice offset past the `__all__` token was mutable from `+` to `-`
/// and `*`. `__all__` is a module's explicit public API, so a name in it can
/// never be dead; reading the wrong slice either loses the list (every
/// exported name becomes deletable) or reads an arbitrary window of source
/// as export names.
#[test]
fn python_all_is_read_only_where_it_is_assigned() {
    let exports = python_all_exports("__all__ = [\"alpha\", 'beta']\n__all__ += [\"gamma\"]\n");
    assert_eq!(
        exports.iter().map(String::as_str).collect::<Vec<_>>(),
        ["alpha", "beta", "gamma"],
        "both the assignment and the accumulation form declare exports, in \
             either quote style"
    );

    // A name that merely contains `__all__` is not an `__all__` assignment.
    assert!(
        python_all_exports("my__all__ = [\"nope\"]\n").is_empty(),
        "a longer identifier must not be read as `__all__`"
    );

    // An `__all__` inside a comment is not a declaration.
    assert!(
        python_all_exports("# __all__ = [\"ignored\"]\n").is_empty(),
        "a commented-out `__all__` must not export"
    );
    assert!(
        python_all_exports("x = 1  # see __all__ = [\"ignored\"]\n").is_empty(),
        "a trailing comment must not export"
    );

    // A real declaration after a decoy on an earlier line is still found,
    // so skipping a decoy must not abandon the scan.
    assert_eq!(
        python_all_exports(
            "my__all__ = [\"nope\"]\n# __all__ = [\"ignored\"]\n__all__ = [\"real\"]\n"
        )
        .iter()
        .map(String::as_str)
        .collect::<Vec<_>>(),
        ["real"],
        "decoys are skipped without abandoning the scan"
    );

    // An indented `__all__` (a conditional or class-level export list) is
    // still a declaration: the character before it is whitespace, which is
    // neither an identifier character nor a comment.
    assert_eq!(
        python_all_exports("if TYPE_CHECKING:\n    __all__ = [\"scoped\"]\n")
            .iter()
            .map(String::as_str)
            .collect::<Vec<_>>(),
        ["scoped"],
        "an indented `__all__` must still be read"
    );

    // A decoy and the real declaration on the *same* line: the scan must
    // resume just past the decoy token, not overshoot it.
    assert_eq!(
        python_all_exports("mod__all__ = 1; __all__ = [\"real\"]\n")
            .iter()
            .map(String::as_str)
            .collect::<Vec<_>>(),
        ["real"],
        "a decoy earlier on the same line must not hide the declaration"
    );

    // No `__all__` at all yields nothing.
    assert!(python_all_exports("def f(): pass\n").is_empty());

    // A truncated list does not panic and does not invent names.
    assert!(python_all_exports("__all__ = [\n").is_empty());
    assert!(python_all_exports("__all__").is_empty());
}

/// A function nested inside a method belongs to the method, not the class.
///
/// `enclosing_type_name` walks up for an owning type and must stop at the
/// first enclosing callable — deleting that arm lets a nested helper
/// inherit the class from farther up the chain. The qualified name is the
/// call graph's join key, so `f.py::C.inner` collides with a real method of
/// `C` named `inner` if one exists, and fails to join with the actual
/// nested definition either way.
#[test]
fn a_function_nested_in_a_method_is_owned_by_the_method() {
    let python = extract_treesitter(
            "f.py",
            "python",
            "class C:\n    def m(self):\n        def inner():\n            pass\n                     return inner\n",
        );
    let names: Vec<&str> = python
        .symbols
        .iter()
        .map(|symbol| symbol.qualified_name.as_str())
        .collect();
    assert_eq!(
        names,
        ["f.py", "f.py::C", "f.py::C.m", "f.py::C.m.inner"],
        "the nested function is qualified through its enclosing method"
    );

    let typescript = extract_treesitter(
        "f.ts",
        "typescript",
        "class B {\n  go() {\n    function helper() {}\n    return helper;\n  }\n}\n",
    );
    let ts_names: Vec<&str> = typescript
        .symbols
        .iter()
        .map(|symbol| symbol.qualified_name.as_str())
        .collect();
    assert_eq!(
        ts_names,
        ["f.ts", "f.ts::B", "f.ts::B.go", "f.ts::B.go.helper"],
        "the same rule holds for a function nested in a class method"
    );

    let rust = extract_treesitter(
        "f.rs",
        "rust",
        concat!(
            "struct Store;\n",
            "impl Store {\n",
            "    fn checkpoint_wal(&self) {\n",
            "        fn run() {}\n",
            "        run();\n",
            "    }\n",
            "    fn run(&self) {}\n",
            "}\n",
        ),
    );
    let rust_names: Vec<&str> = rust
        .symbols
        .iter()
        .map(|symbol| symbol.qualified_name.as_str())
        .collect();
    assert!(
        rust_names.contains(&"f.rs::Store.checkpoint_wal.run"),
        "a fn nested in an impl method belongs to the method, not the type: {rust_names:?}"
    );
    assert!(
        !rust_names.iter().any(|name| *name == "f.rs::Store.run"
            && rust
                .symbols
                .iter()
                .filter(|s| s.qualified_name == *name)
                .count()
                > 1),
        "the nested helper must not collide with Store.run: {rust_names:?}"
    );
    let nested = rust
        .symbols
        .iter()
        .find(|symbol| symbol.qualified_name == "f.rs::Store.checkpoint_wal.run")
        .expect("nested run");
    assert_eq!(
        nested.kind,
        SymbolKind::Function,
        "a nested fn is not a method of the impl type"
    );
    assert_eq!(
        nested.parent_symbol.as_deref(),
        Some("f.rs::Store.checkpoint_wal"),
        "lexical parent is the enclosing method"
    );
}

/// A same-named let initializer is a call, not a local of the binding it
/// introduces. Rust's `let x = x()` does not put `x` in scope on the RHS.
#[test]
fn a_rust_let_initializer_does_not_shadow_the_function_it_calls() {
    let extraction = extract_treesitter(
        "inv.rs",
        "rust",
        concat!(
            "fn test_commands() -> u8 { 1 }\n",
            "fn go() {\n",
            "    let test_commands = test_commands();\n",
            "    let _ = test_commands;\n",
            "}\n",
        ),
    );
    assert!(
        extraction
            .calls
            .iter()
            .any(|call| call.callee_name == "test_commands" && call.receiver_expr.is_none()),
        "the initializer is a call: {:?}",
        extraction.calls
    );
    let call = extraction
        .calls
        .iter()
        .find(|call| call.callee_name == "test_commands")
        .expect("call");
    assert!(
        extraction
            .local_binding_at(call.span.start_byte, "test_commands")
            .is_none(),
        "the initializer must not be a local of the binding it introduces: {:?}",
        extraction.local_bindings
    );
}

/// A closure parameter must not leak into the enclosing function.
#[test]
fn a_rust_closure_parameter_does_not_shadow_an_outer_function() {
    let extraction = extract_treesitter(
        "hyg.rs",
        "rust",
        concat!(
            "fn marker(_path: &str, _prefix: &str) -> bool { false }\n",
            "fn local_provider() {\n",
            "    let has = |marker: &str| marker.starts_with(\"x\");\n",
            "    let _ = has(\"n\") && marker(\"Cargo.toml\");\n",
            "}\n",
        ),
    );
    let calls: Vec<_> = extraction
        .calls
        .iter()
        .map(|call| (call.callee_name.as_str(), call.receiver_expr.as_deref()))
        .collect();
    assert!(
        calls
            .iter()
            .any(|(name, recv)| *name == "marker" && recv.is_none()),
        "the outer marker(...) is a call: {calls:?}"
    );
    let outer = extraction
        .calls
        .iter()
        .find(|call| call.callee_name == "marker" && call.receiver_expr.is_none())
        .expect("outer marker call");
    assert!(
        extraction
            .local_binding_at(outer.span.start_byte, "marker")
            .is_none(),
        "the outer call must not inherit the closure parameter: {:?}",
        extraction.local_bindings
    );
}

/// `#[serde(default = "name")]` is a use of that function.
#[test]
fn a_serde_default_attribute_is_a_name_reference() {
    let extraction = extract_treesitter(
        "art.rs",
        "rust",
        concat!(
            "fn unknown_ctime() -> i128 { -1 }\n",
            "#[derive(serde::Deserialize)]\n",
            "struct Stamp {\n",
            "    #[serde(default = \"unknown_ctime\")]\n",
            "    ctime_ns: i128,\n",
            "}\n",
        ),
    );
    assert!(
        extraction.references.iter().any(|reference| {
            reference.kind == ReferenceKind::Name && reference.name == "unknown_ctime"
        }),
        "the serde default string names the helper: {:?}",
        extraction
            .references
            .iter()
            .map(|r| (&r.name, r.kind))
            .collect::<Vec<_>>()
    );
}

/// `Self::stamp_rule` as a value is a use of that method.
#[test]
fn a_self_qualified_function_pointer_is_a_name_reference() {
    let extraction = extract_treesitter(
        "w.rs",
        "rust",
        concat!(
            "struct Cache;\n",
            "impl Cache {\n",
            "    fn stamp_rule() {}\n",
            "    fn rule_stamps() {\n",
            "        let _ = [()].into_iter().map(Self::stamp_rule);\n",
            "    }\n",
            "}\n",
        ),
    );
    assert!(
        extraction.references.iter().any(|reference| {
            reference.kind == ReferenceKind::Name
                && reference.name == "stamp_rule"
                && reference.receiver_expr.as_deref() == Some("Self")
        }),
        "Self::stamp_rule as a value must keep its receiver: {:?}",
        extraction
            .references
            .iter()
            .map(|r| (&r.name, r.kind, &r.receiver_expr))
            .collect::<Vec<_>>()
    );
}

/// An extern fn used as a value is a Function, not a method of a type.
#[test]
fn an_extern_fn_is_a_free_function() {
    let extraction = extract_treesitter(
        "ts.rs",
        "rust",
        concat!(
            "unsafe extern \"C\" {\n",
            "    fn tree_sitter_liquid() -> *const ();\n",
            "}\n",
            "const LANG: u8 = unsafe { from_raw(tree_sitter_liquid) };\n",
            "fn from_raw(_: unsafe extern \"C\" fn() -> *const ()) -> u8 { 0 }\n",
        ),
    );
    let liquid = extraction
        .symbols
        .iter()
        .find(|symbol| symbol.name == "tree_sitter_liquid")
        .expect("extern fn");
    assert_eq!(liquid.kind, SymbolKind::Function);
    assert_eq!(liquid.qualified_name.as_str(), "ts.rs::tree_sitter_liquid");
    assert!(
        extraction
            .references
            .iter()
            .any(|r| r.kind == ReferenceKind::Name && r.name == "tree_sitter_liquid"),
        "from_raw(tree_sitter_liquid) is a value use: {:?}",
        extraction
            .references
            .iter()
            .map(|r| (&r.name, r.kind))
            .collect::<Vec<_>>()
    );
}

/// `select!` arms are not a valid argument list; the fallback still sees
/// the calls inside them.
#[test]
fn a_select_macro_recovers_method_calls() {
    let extraction = extract_treesitter(
        "d.rs",
        "rust",
        concat!(
            "struct Daemon;\n",
            "impl Daemon {\n",
            "    fn vanished_reason(&self) -> Option<String> { None }\n",
            "    fn run_loop(&self) {\n",
            "        tokio::select! {\n",
            "            _ = ticker.tick() => {\n",
            "                self.vanished_reason();\n",
            "            }\n",
            "        }\n",
            "    }\n",
            "}\n",
        ),
    );
    assert!(
        extraction.calls.iter().any(|call| {
            call.callee_name == "vanished_reason" && call.receiver_expr.as_deref() == Some("self")
        }),
        "select! must recover self.vanished_reason(): {:?}",
        extraction.calls
    );
}

/// A constructed value binds its local name to its type, in every grammar.
///
/// `assignment_binding` was replaceable with `None`, `Some("")` and
/// `Some("xyzzy")`, and each of its three arms was deletable. It is what
/// records `worker = Worker()` so that a later `worker.go()` knows what
/// `worker` is — without it a receiver has a name and no type, so receiver
/// resolution has nothing to look up and the call resolves to nothing
/// rather than resolving uncertainly (SC12).
///
/// Writing this found the Rust arm passing a literal `None` where the
/// Python, TypeScript and Go arms all passed the binding, so Rust values
/// constructed with `let` were the one case with no type behind the
/// receiver. All four are asserted together here so the arms cannot drift
/// apart again.
#[test]
fn a_constructed_value_binds_its_local_name_in_every_grammar() {
    for (path, grammar, source, constructor) in [
        (
            "f.py",
            "python",
            "def run():\n    worker = Worker()\n    worker.go()\n",
            "Worker",
        ),
        (
            "f.js",
            "javascript",
            "function run() {\n  const worker = new Worker();\n  worker.go();\n}\n",
            "Worker",
        ),
        (
            "f.ts",
            "typescript",
            "function run() {\n  let worker = new Worker();\n  worker.go();\n}\n",
            "Worker",
        ),
        (
            "f.rs",
            "rust",
            "fn run() {\n    let worker = Worker::new();\n    worker.go();\n}\n",
            // SC26 split `Worker::new` into callee `new` on receiver
            // `Worker`. The binding this test guards is unchanged, and
            // dispatch demonstrably still works — see
            // `rust_associated_function_calls_resolve_and_keep_receiver_typing`
            // in devmap-resolve, which asserts both `Worker.go` and the
            // newly recovered `Worker.new` edge.
            "new",
        ),
        (
            "f.go",
            "go",
            "package p\nfunc run() {\n\tworker := NewWorker()\n\tworker.Go()\n}\n",
            "NewWorker",
        ),
    ] {
        let extraction = extract_treesitter(path, grammar, source);
        let bound = extraction
            .references
            .iter()
            .find(|reference| reference.name == constructor)
            .unwrap_or_else(|| {
                panic!(
                    "{grammar}: no reference named {constructor}: {:?}",
                    extraction
                        .references
                        .iter()
                        .map(|r| &r.name)
                        .collect::<Vec<_>>()
                )
            });
        assert_eq!(
            bound.assigned_to.as_deref(),
            Some("worker"),
            "{grammar}: the constructed value must bind to the name it is assigned to"
        );

        // And the later method call carries that same name as its receiver,
        // which is the join the binding exists to enable.
        assert!(
            extraction
                .calls
                .iter()
                .any(|call| call.receiver_expr.as_deref() == Some("worker")),
            "{grammar}: the method call must name `worker` as its receiver: {:?}",
            extraction
                .calls
                .iter()
                .map(|c| (&c.callee_name, &c.receiver_expr))
                .collect::<Vec<_>>()
        );
    }
}

/// A Rust tuple-struct constructor binds its local name to the struct.
///
/// This is the shape that makes the Rust arm's binding usable downstream:
/// the callee is the bare type name `Worker`, which the resolver can find
/// as a `Struct`. Measured end to end, it upgrades `w.go()` from a
/// speculative 0.2 name guess among every `go` in the corpus to a
/// deterministic 1.0 resolution.
///
/// The associated-function form `Worker::new()` still does **not** bind:
/// its callee name is a scoped path that matches no symbol. That gap is
/// real and recorded in STATUS.md rather than papered over here.
#[test]
fn a_rust_tuple_struct_constructor_binds_its_local_name() {
    let extraction = extract_treesitter(
            "f.rs",
            "rust",
            "pub struct Worker(u32);\nimpl Worker { pub fn go(&self) {} }\n             fn run() {\n    let w = Worker(1);\n    w.go();\n}\n",
        );
    let constructor = extraction
        .references
        .iter()
        .find(|reference| reference.name == "Worker" && reference.kind == ReferenceKind::Call)
        .expect("the tuple-struct constructor is a call");
    assert_eq!(
        constructor.assigned_to.as_deref(),
        Some("w"),
        "the constructed struct must bind the local name it is assigned to"
    );
}

/// A declared parameter type binds the parameter name, and `_` binds nothing.
///
/// `param_type_bindings` is the other half of SC12: for a function that
/// operates on a value it did not construct, the declared type is the only
/// binding available. The Go arm was deletable and both emptiness guards
/// were collapsible, which would bind the throwaway name `_` to a type and
/// let every `_` parameter in the package collide on one binding.
#[test]
fn a_declared_parameter_type_binds_its_name_but_never_underscore() {
    let go = extract_treesitter(
        "f.go",
        "go",
        "package p\nfunc run(g *English, n int) {\n\tg.Greet()\n}\n",
    );
    let go_bindings: Vec<(&str, Option<&str>)> = go
        .references
        .iter()
        .filter(|reference| reference.kind == ReferenceKind::Type)
        .map(|reference| (reference.name.as_str(), reference.assigned_to.as_deref()))
        .collect();
    assert!(
        go_bindings.contains(&("English", Some("g"))),
        "a pointer parameter type binds its name: {go_bindings:?}"
    );
    assert!(
        go_bindings.contains(&("int", Some("n"))),
        "a plain parameter type binds its name too: {go_bindings:?}"
    );

    let rust = extract_treesitter("f.rs", "rust", "fn run(g: &English) {\n    g.greet();\n}\n");
    assert!(
        rust.references.iter().any(|reference| {
            reference.name == "English" && reference.assigned_to.as_deref() == Some("g")
        }),
        "a Rust reference parameter binds through its type: {:?}",
        rust.references
            .iter()
            .map(|r| (&r.name, &r.assigned_to))
            .collect::<Vec<_>>(),
    );

    let swift = extract_treesitter(
        "f.swift",
        "swift",
        "func load(_ reader: Reader) -> Int {\n    return reader.read()\n}\n",
    );
    assert!(
        swift.references.iter().any(|reference| {
            reference.name == "Reader" && reference.assigned_to.as_deref() == Some("reader")
        }),
        "a Swift parameter type must bind the parameter name: {:?}",
        swift
            .references
            .iter()
            .map(|r| (&r.name, &r.assigned_to, r.kind))
            .collect::<Vec<_>>(),
    );

    let cpp = extract_treesitter(
        "f.cpp",
        "cpp",
        "struct S {};\nint driver(S s, S *p) { return 0; }\n",
    );
    let cpp_bindings: Vec<(&str, Option<&str>)> = cpp
        .references
        .iter()
        .filter(|reference| reference.kind == ReferenceKind::Type)
        .map(|reference| (reference.name.as_str(), reference.assigned_to.as_deref()))
        .collect();
    assert!(
        cpp_bindings.contains(&("S", Some("s"))),
        "a C++ value parameter type binds its name: {cpp_bindings:?}"
    );
    assert!(
        cpp_bindings.contains(&("S", Some("p"))),
        "a C++ pointer parameter type binds through its declarator: {cpp_bindings:?}"
    );

    // A multi-target assignment binds nothing: `value, err := New()` does
    // not say here which target receives the constructed value, and a wrong
    // binding is worse than an absent one.
    let multi = extract_treesitter(
        "m.go",
        "go",
        "package p\nfunc run() {\n\tvalue, err := NewWorker()\n\t_ = err\n}\n",
    );
    let bound = multi
        .references
        .iter()
        .find(|reference| reference.name == "NewWorker")
        .expect("the constructor is still a call");
    assert_eq!(
        bound.assigned_to, None,
        "a two-target `:=` must not guess which name receives the value"
    );

    // `_` is a throwaway, not a name worth binding.
    let anonymous = extract_treesitter("g.go", "go", "package p\nfunc run(_ *English) {}\n");
    assert!(
        anonymous
            .references
            .iter()
            .all(|reference| reference.assigned_to.is_none()),
        "`_` must not be bound to a type: {:?}",
        anonymous
            .references
            .iter()
            .map(|r| (&r.name, &r.assigned_to))
            .collect::<Vec<_>>()
    );
    let rust_anonymous = extract_treesitter("g.rs", "rust", "fn run(_: &English) {}\n");
    assert!(
        rust_anonymous
            .references
            .iter()
            .all(|reference| reference.assigned_to.is_none()),
        "the same holds in Rust"
    );
}

/// Every JS/TS declaration form reports its exportedness, both ways.
///
/// Five of the six forms had inlined their own copy of the export test and
/// each copy was independently mutable — inverting one marks that whole
/// *kind* of declaration unexported while the rest stay correct, which
/// reads as "this enum is private and unused" rather than as a bug.
/// `is_exported` is direct evidence for dead code, so a wrong answer here
/// produces confident false positives; in the other direction it hides real
/// ones. The copies are now one helper, and this covers every form through
/// it with a matching unexported control so neither direction can be
/// satisfied by a constant.
#[test]
fn every_js_declaration_form_reports_its_exportedness() {
    let extraction = extract_treesitter(
        "f.ts",
        "typescript",
        "export const arrow = () => {};\nconst plain = () => {};\n\
             export class Klass { m() {} }\nclass Priv { p() {} }\n\
             export enum Color { A }\nenum Hidden { B }\n\
             export interface Shape { x: number }\ninterface Inner { y: number }\n\
             export type Alias = string;\ntype Local = number;\n\
             export namespace Outer { export const q = 1; }\nnamespace Quiet { }\n",
    );

    let exported_of = |qualified: &str| -> bool {
        extraction
            .symbols
            .iter()
            .find(|symbol| symbol.qualified_name == qualified)
            .unwrap_or_else(|| {
                panic!(
                    "{qualified} was not extracted at all; got {:?}",
                    extraction
                        .symbols
                        .iter()
                        .map(|s| &s.qualified_name)
                        .collect::<Vec<_>>()
                )
            })
            .is_exported
    };

    // Each pair is (exported form, unexported form of the same kind), so a
    // constant `true` and a constant `false` both fail.
    for (exported, private) in [
        // `export const f = () => {}` — the declarator's owner is the
        // lexical_declaration, not the declarator itself.
        ("f.ts::arrow", "f.ts::plain"),
        ("f.ts::Klass", "f.ts::Priv"),
        // A method is exported exactly when its class is: its own parent is
        // always `class_body`, which is never an export statement.
        ("f.ts::Klass.m", "f.ts::Priv.p"),
        ("f.ts::Color", "f.ts::Hidden"),
        ("f.ts::Shape", "f.ts::Inner"),
        ("f.ts::Alias", "f.ts::Local"),
        ("f.ts::Outer", "f.ts::Quiet"),
    ] {
        assert!(
            exported_of(exported),
            "{exported} is exported and must report so"
        );
        assert!(
            !exported_of(private),
            "{private} is not exported and must not report so"
        );
    }
}

/// A re-export names its source module; a local re-export names none.
///
/// The brace slice and the `!mod_spec.is_empty()` guard were both mutable.
/// `export { x }` and `export { x } from './m'` are different facts — the
/// first republishes a local symbol, the second forwards another module's —
/// and giving the local form an empty module specifier makes the resolver
/// look for a module named `""`, so the export binds to nothing.
#[test]
fn re_exports_carry_their_source_module_only_when_they_have_one() {
    let extraction = extract_treesitter(
        "f.ts",
        "typescript",
        "export { a, b as c } from './m';\nexport { local };\n",
    );

    let export = |name: &str| {
        extraction
            .exports
            .iter()
            .find(|export| export.exported_name == name)
            .unwrap_or_else(|| panic!("no export named {name}"))
    };

    // `b as c` publishes `c` and points at the local name `b`; getting the
    // brace slice wrong loses the rename and publishes the wrong name.
    assert_eq!(export("a").local_name.as_deref(), Some("a"));
    assert_eq!(export("c").local_name.as_deref(), Some("b"));
    assert_eq!(export("a").module_specifier.as_deref(), Some("./m"));
    assert_eq!(export("c").module_specifier.as_deref(), Some("./m"));

    // A local re-export forwards nothing, so it must carry no module at all
    // rather than an empty one.
    assert_eq!(
        export("local").module_specifier,
        None,
        "a local re-export must have no module specifier, not an empty one"
    );

    // The brace contents are parsed as bindings, aligned and in order.
    let braced = extraction
        .imports
        .iter()
        .find(|import| import.module_specifier == "./m")
        .expect("the re-export is recorded as an import of `./m`");
    assert_eq!(braced.imported_names, ["a", "b"]);
    assert_eq!(braced.local_names, ["a", "c"]);
}

/// A default import takes an alias; a braced import does not.
///
/// The `!candidate.is_empty() && candidate.chars().all(..)` guard decides
/// whether the text before `from` is a single default binding. Collapsed to
/// `||`, the braced form `{ x }` passes and becomes the module's alias, so
/// every later `{` -prefixed reference resolves through a binding named
/// `{ x }` that no symbol has.
#[test]
fn only_a_bare_default_import_becomes_an_alias() {
    let extraction = extract_treesitter(
        "f.ts",
        "typescript",
        "import def from 'pkg';\nimport { x } from 'q';\n",
    );

    let import = |module: &str| {
        extraction
            .imports
            .iter()
            .find(|import| import.module_specifier == module)
            .unwrap_or_else(|| panic!("no import of {module}"))
    };
    assert_eq!(
        import("pkg").alias.as_deref(),
        Some("def"),
        "a bare default import binds its name as the alias"
    );
    assert_eq!(
        import("q").alias,
        None,
        "a braced import list is not an alias"
    );
}

/// A generic type's `name` field is a use of the instantiated type.
///
/// `is_defining_name` treats a grammar `name` field as a binding, which is
/// right for `type AssertTrue<T> = T` and wrong for `AssertTrue<Extends<…>>`
/// — that second `AssertTrue` is the only use the alias has. Forced to a
/// binding, the Type reference is suppressed and a compile-time contract
/// is reported dead at the extracted tier.
#[test]
fn a_generic_type_name_is_a_use_not_a_binding() {
    let extraction = extract_treesitter(
        "f.ts",
        "typescript",
        "type Extends<A, B> = A extends B ? true : false;\n\
             type AssertTrue<T extends true> = T;\n\
             export type Contract = AssertTrue<Extends<string, string>>;\n",
    );
    let type_uses = |name: &str| {
        extraction
            .references
            .iter()
            .filter(|reference| reference.name == name && reference.kind == ReferenceKind::Type)
            .count()
    };
    assert!(
        type_uses("AssertTrue") >= 1,
        "`AssertTrue<…>` is a use of the alias: {:?}",
        extraction
            .references
            .iter()
            .map(|r| (&r.name, r.kind))
            .collect::<Vec<_>>()
    );
    assert!(
        type_uses("Extends") >= 1,
        "`Extends<…>` inside the argument list is a use of that alias"
    );
    assert!(
        extraction
            .references
            .iter()
            .all(|reference| reference.name != "AssertTrue"
                || reference.kind != ReferenceKind::Type
                || reference.enclosing_symbol.as_deref() != Some("f.ts::AssertTrue")),
        "the alias's own name is a binding, not a self-reference"
    );
}

/// A nested `const walk = () => {}` is owned by the enclosing function.
///
/// The symbol emitter used the file as `parent_symbol` for every const
/// arrow, so `collapseAll.walk` looked like a sibling of `collapseAll`
/// rather than a child. The resolver's lexical join then could not tell
/// `walk()` inside `collapseAll` from a module-level `walk`.
#[test]
fn a_nested_const_arrow_is_parented_on_the_enclosing_function() {
    let extraction = extract_treesitter(
            "a.js",
            "javascript",
            "function collapseAll() {\n  const walk = (folders) => { walk(folders); };\n  walk([]);\n}\nconst top = () => {};\n",
        );
    let walk = extraction
        .symbols
        .iter()
        .find(|symbol| symbol.qualified_name == "a.js::collapseAll.walk")
        .expect("the nested arrow is a symbol");
    assert_eq!(
        walk.parent_symbol.as_deref(),
        Some("a.js::collapseAll"),
        "the nested arrow's parent is the function that binds it, not the file"
    );
    let top = extraction
        .symbols
        .iter()
        .find(|symbol| symbol.name == "top")
        .expect("the module-level arrow is a symbol");
    assert_eq!(
        top.parent_symbol.as_deref(),
        Some("a.js"),
        "a module-level const arrow is still parented on the file"
    );
}

/// A React lifecycle hook is an entry point only as a class method.
///
/// `kind == "method_definition"` was invertible. The annotation says "the
/// renderer calls this, so never report it dead"; applied to a plain
/// function that merely shares the name, it suppresses a real dead-code
/// finding, and withheld from the actual method it produces a confident
/// false positive on every component in the tree.
#[test]
fn a_lifecycle_hook_is_an_entry_point_only_as_a_method() {
    let extraction = extract_treesitter(
        "f.jsx",
        "javascript",
        "class Comp {\n  componentDidMount() {}\n}\nfunction componentDidMount() {}\n",
    );
    let targets: Vec<&str> = extraction
        .wiring
        .iter()
        .filter(|annotation| annotation.kind == WiringKind::RuntimeEntryPoint)
        .map(|annotation| annotation.target_symbol.as_str())
        .collect();
    assert_eq!(
        targets,
        ["f.jsx::Comp.componentDidMount"],
        "only the class method is a renderer entry point"
    );
}

/// Bundler plugin hooks are object methods the bundler looks up by name.
///
/// The same name as a free function is ordinary code. Applied there, the
/// annotation would suppress a real dead-code finding; withheld from the
/// plugin method it produces a confident false positive on every Vite plugin
/// in the tree, none of which has an in-repo caller.
#[test]
fn a_bundler_plugin_hook_is_an_entry_point_only_as_a_method() {
    let extraction = extract_treesitter(
            "vite.config.ts",
            "typescript",
            "function generateBundle() {}\nfunction gitpulseBundleBudget() {\n  return {\n    generateBundle() {}\n  };\n}\n",
        );
    let targets: Vec<&str> = extraction
        .wiring
        .iter()
        .filter(|annotation| annotation.kind == WiringKind::RuntimeEntryPoint)
        .map(|annotation| annotation.target_symbol.as_str())
        .collect();
    assert_eq!(
        targets,
        ["vite.config.ts::gitpulseBundleBudget.generateBundle"],
        "only the plugin object method is a bundler entry point"
    );
    assert!(
        !is_exported(
            "function gitpulseBundleBudget() { return { generateBundle() {} }; }\n",
            "generateBundle"
        ),
        "a non-exported factory does not publish the hook; the hook name does"
    );
}

/// A named Go receiver binds its type; an anonymous one binds nothing.
///
/// The `!recv_name.is_empty()` guard was deletable, which inverts SC9's
/// method-scoped receiver binding: named receivers stop binding and only
/// anonymous ones would, so `c.help()` inside `func (c *Client) Do()` loses
/// the type of `c` and the intra-type call edge disappears.
#[test]
fn a_named_go_receiver_binds_its_type_and_an_anonymous_one_does_not() {
    let extraction = extract_treesitter(
            "f.go",
            "go",
            "package p\ntype Client struct{}\nfunc (c *Client) Do() { c.help() }\n             func (*Client) Anon() {}\nfunc (c *Client) help() {}\n             func mk() *Client { return &Client{} }\n",
        );

    // The binding is scoped to the method it was declared in (SC9), not to
    // the file, so two methods each get their own.
    assert!(
        extraction.references.iter().any(|reference| {
            reference.name == "Client"
                && reference.assigned_to.as_deref() == Some("c")
                && reference.enclosing_symbol.as_deref() == Some("f.go::Client.Do")
        }),
        "a named receiver binds its type within its own method: {:?}",
        extraction
            .references
            .iter()
            .map(|r| (&r.name, &r.assigned_to, &r.enclosing_symbol))
            .collect::<Vec<_>>()
    );

    // An anonymous receiver has no name to bind.
    assert!(
        extraction.references.iter().all(|reference| {
            reference.enclosing_symbol.as_deref() != Some("f.go::Client.Anon")
                || reference.assigned_to.is_none()
        }),
        "an anonymous receiver must bind nothing"
    );

    // A composite literal resolves its type through the child scan, so
    // `&Client{}` is a construction of `Client`.
    assert!(
        extraction.references.iter().any(|reference| {
            reference.name == "Client" && reference.kind == ReferenceKind::Constructor
        }),
        "a composite literal is a constructor reference to its type: {:?}",
        extraction
            .references
            .iter()
            .map(|r| (&r.name, r.kind))
            .collect::<Vec<_>>()
    );
}

/// Only a bare identifier before `from` is a default binding.
///
/// The guard sits on the *brace-less* import path, so the discriminating
/// shape is a modified default import: `import type Foo from 'm'` has no
/// braces, and its pre-`from` text is two words. Collapsed to `||`, that
/// whole string becomes the module's alias and every reference resolved
/// through it looks for a binding named `type Foo` that no symbol has.
#[test]
fn a_multi_word_import_clause_is_not_an_alias() {
    let extraction = extract_treesitter(
        "f.ts",
        "typescript",
        "import type Foo from 'm';\nimport def from 'pkg';\n",
    );
    let alias_of = |module: &str| {
        extraction
            .imports
            .iter()
            .find(|import| import.module_specifier == module)
            .unwrap_or_else(|| panic!("no import of {module}"))
            .alias
            .clone()
    };
    assert_eq!(
        alias_of("m"),
        None,
        "`type Foo` is not a single default binding and must not become an alias"
    );
    assert_eq!(
        alias_of("pkg").as_deref(),
        Some("def"),
        "positive control: a bare default import still aliases"
    );
}

/// The braced binding list starts *after* the brace.
///
/// `text[idx1 + 1..idx2]` was mutable to `idx1 - 1`, which pulls in the
/// character before the brace. Every spaced form survives it because the
/// binding parser trims whitespace and braces, so the discriminating shape
/// is the unspaced one a minifier emits: with `- 1` the slice starts inside
/// `export`, and the imported name comes out as `t{a` rather than `a`.
///
/// (`idx1 * 1` is an equivalent mutant: it yields `idx1`, and the leading
/// brace is trimmed by the binding parser, so the result is unchanged.)
#[test]
fn a_braced_binding_list_starts_after_its_brace() {
    let extraction = extract_treesitter("g.ts", "typescript", "export{a}from'./m';\n");
    let braced = extraction
        .imports
        .iter()
        .find(|import| import.module_specifier == "./m")
        .expect("the unspaced re-export is still recorded");
    assert_eq!(
        braced.imported_names,
        ["a"],
        "the binding is `a`, not the text around the brace"
    );
    assert_eq!(braced.local_names, ["a"]);
    assert!(
        extraction
            .exports
            .iter()
            .any(|export| export.exported_name == "a"),
        "and it is published under that same name: {:?}",
        extraction
            .exports
            .iter()
            .map(|e| &e.exported_name)
            .collect::<Vec<_>>()
    );
}

/// A Rust binary entry point is a free `main` in a file that declares one.
///
/// All three conjuncts were mutable. The annotation means "the toolchain
/// calls this, so it can never be dead"; widened, it exempts every function
/// in `main.rs` and every method named `main`, silently suppressing real
/// dead-code findings across the whole binary crate. Narrowed, `main`
/// itself is reported dead, which is the most conspicuous false positive a
/// Rust dead-code pass can produce.
#[test]
fn a_rust_binary_entry_point_is_a_free_main_in_an_entry_file() {
    let entry_points = |path: &str, source: &str| -> Vec<String> {
        extract_treesitter(path, "rust", source)
            .wiring
            .iter()
            .filter(|annotation| annotation.kind == WiringKind::RuntimeEntryPoint)
            .map(|annotation| annotation.target_symbol.clone())
            .collect()
    };

    // `main.rs` declares an entry point, but only the free `main` is one:
    // not a sibling function, and not a method that shares the name.
    assert_eq!(
        entry_points(
            "src/main.rs",
            "fn main() {}\nfn helper() {}\nstruct S;\nimpl S { fn main(&self) {} }\n",
        ),
        ["src/main.rs::main"],
        "only the free `main` in an entry file is a toolchain entry point"
    );

    // `bin/` files declare entry points too.
    assert_eq!(
        entry_points("src/bin/tool.rs", "fn main() {}\n"),
        ["src/bin/tool.rs::main"]
    );

    // A `main` in a file that declares no entry point is an ordinary
    // function and a real dead-code candidate.
    assert!(
        entry_points("src/lib.rs", "fn main() {}\n").is_empty(),
        "`main` in a library file is not an entry point"
    );
    assert!(entry_points("src/other.rs", "fn main() {}\n").is_empty());
}

/// A function expression is named by the binding it is assigned to.
///
/// `callable_binding_name` decides what a call inside an arrow or function
/// expression is attributed to. Losing it leaves the call attributed to an
/// enclosing symbol or to nothing, so `f` never reaches `helper` — the
/// unjoinable-edge shape behind SC9/SC10, where the edge exists but names
/// something no node has.
#[test]
fn a_function_expression_is_named_by_its_binding() {
    let extraction = extract_treesitter("f.js", "javascript", "const f = () => { helper(); };\n");
    let call = extraction
        .calls
        .iter()
        .find(|call| call.callee_name == "helper")
        .expect("the call inside the arrow is recorded");
    assert_eq!(
        call.caller_symbol.as_deref(),
        Some("f.js::f"),
        "the call belongs to the binding `f`, which is the symbol that exists"
    );
    assert!(
        extraction
            .symbols
            .iter()
            .any(|symbol| symbol.qualified_name == "f.js::f"),
        "and that symbol is emitted, so the edge joins"
    );
}

/// A Go method that satisfies an interface is annotated as exempt.
///
/// The whole `go_interface_method_exemptions` body was replaceable with
/// nothing. It is the wiring half of the interface-satisfaction join: the
/// pure matcher in `model.rs` is tested there, but nothing asserted that
/// its results were ever turned into annotations. Without them, every method
/// reached only through an interface reads as dead.
#[test]
fn a_go_method_satisfying_an_interface_is_annotated_exempt() {
    let extraction = extract_treesitter(
            "f.go",
            "go",
            "package p\ntype Runner interface { Run() error }\ntype Impl struct{}\n             func (i *Impl) Run() error { return nil }\n",
        );
    let exemption = extraction
        .wiring
        .iter()
        .find(|annotation| annotation.target_symbol == "f.go::Impl.Run")
        .unwrap_or_else(|| {
            panic!(
                "Impl.Run satisfies Runner and must be exempt: {:?}",
                extraction.wiring
            )
        });
    assert_eq!(exemption.kind, WiringKind::StructuralExempt);
    assert!(
        exemption.details.contains("Runner"),
        "the reason must name the interface: {}",
        exemption.details
    );
}

/// An imported name is bound once, not counted again as a use.
///
/// `is_inside_import_or_export` suppresses the identifier *inside* the
/// import specifier, which is a binding site rather than a use. Forced to
/// `false`, every import contributes a phantom reference to the symbol it
/// imports, so an imported-but-never-used symbol looks used and its real
/// dead-code finding disappears.
#[test]
fn an_import_specifier_is_a_binding_not_a_use() {
    let extraction = extract_treesitter(
        "f.ts",
        "typescript",
        "import { x } from 'm';\nexport function run() { return x; }\n",
    );
    let uses: Vec<&str> = extraction
        .references
        .iter()
        .filter(|reference| reference.name == "x")
        .map(|reference| reference.enclosing_symbol.as_deref().unwrap_or("<file>"))
        .collect();
    assert_eq!(
        uses,
        ["f.ts::run"],
        "`x` is used exactly once, inside `run`; the import specifier is not a use"
    );
}

/// A local variable shadows the name for its own scope only.
///
/// Three guards cooperate here. `is_callable_node` bounds the scope: forced
/// to `false` the scope becomes the whole file, so one function's local
/// silently suppresses another function's genuine use of that name.
/// `is_symbol_binding` keeps a named function binding out of the local set,
/// so `const f = () => {}` stays a symbol and references to it survive.
/// `is_binding_wrapper` lets the defining-name walk climb through the list
/// and pattern wrappers that Go and Python put around assignment targets.
#[test]
fn a_local_shadows_its_own_scope_only() {
    // Scope is the function, not the file.
    let cross = extract_treesitter(
            "f.js",
            "javascript",
            "function a() { const shared = 1; return shared; }\n             function b() { return shared; }\n",
        );
    let shared_uses: Vec<&str> = cross
        .references
        .iter()
        .filter(|reference| reference.name == "shared")
        .map(|reference| reference.enclosing_symbol.as_deref().unwrap_or("<file>"))
        .collect();
    assert_eq!(
        shared_uses,
        ["f.js::b"],
        "`shared` is a local in `a` and a real use in `b`; a file-wide scope \
             would suppress both"
    );

    // A function binding is a symbol, not a shadowing local.
    let binding = extract_treesitter(
        "g.js",
        "javascript",
        "function run() { const f = () => {}; return f; }\n",
    );
    assert!(
        binding
            .symbols
            .iter()
            .any(|symbol| symbol.qualified_name == "g.js::run.f"),
        "the arrow binding is emitted as a symbol"
    );
    assert!(
        binding
            .references
            .iter()
            .any(|reference| reference.name == "f" && reference.kind == ReferenceKind::Name),
        "and referring to it is a use, not a shadowed local: {:?}",
        binding
            .references
            .iter()
            .map(|r| (&r.name, r.kind))
            .collect::<Vec<_>>()
    );

    // Assignment targets wrapped in a list are still binding sites.
    let go = extract_treesitter(
            "f.go",
            "go",
            "package p\nfunc run() {\n\tvar a, b int\n\ta, b = one(), two()\n\t             _ = a\n\t_ = b\n}\n",
        );
    assert!(
        !go.references
            .iter()
            .any(|reference| reference.kind == ReferenceKind::Name
                && (reference.name == "a" || reference.name == "b")),
        "a multi-target assignment binds its names rather than using them: {:?}",
        go.references
            .iter()
            .map(|r| (&r.name, r.kind))
            .collect::<Vec<_>>()
    );

    // A Python augmented-assignment target is a binding too.
    let python = extract_treesitter(
        "f.py",
        "python",
        "def run():\n    total = 0\n    total += step\n    return total\n",
    );
    let python_names: Vec<&str> = python
        .references
        .iter()
        .map(|reference| reference.name.as_str())
        .collect();
    assert_eq!(
        python_names,
        ["step"],
        "only the value read is a use; the accumulator is a binding"
    );
}

/// The scope-local cache is cleared between files.
///
/// `reset_scope_locals` was replaceable with nothing. Its keys are
/// `Node::id()` — arena addresses that are reused after a tree is dropped —
/// so a stale entry can be served for an unrelated scope in the next file,
/// suppressing that file's genuine references. Address reuse is
/// nondeterministic, so this asserts the contract directly rather than
/// hoping to observe a collision.
#[test]
fn the_scope_local_cache_is_cleared_between_files() {
    // Extraction populates the cache: this source has a local that shadows,
    // which is what forces a scope's locals to be collected.
    extract_treesitter(
        "f.js",
        "javascript",
        "function run() { const helper = 1; return helper; }\n",
    );
    assert!(
        scope_locals_len() > 0,
        "the fixture must actually populate the cache, or this test is vacuous"
    );

    reset_scope_locals();
    assert_eq!(
        scope_locals_len(),
        0,
        "the cache must be empty before the next file is extracted"
    );

    // And extraction itself performs the clear, so two files in sequence
    // cannot leak into one another.
    extract_treesitter(
        "f.js",
        "javascript",
        "function run() { const helper = 1; return helper; }\n",
    );
    let after_first = scope_locals_len();
    extract_treesitter("g.js", "javascript", "function other() { return 1; }\n");
    assert!(
        scope_locals_len() <= after_first,
        "extracting a second file must start from a cleared cache, not accumulate"
    );
}

/// Alias and parameter binding sites are definitions, not uses.
///
/// `is_defining_name` tests four fields, and each disjunction was mutable.
/// The `alias` field covers `with … as handle` and `except … as e`; the
/// `parameter` field covers an unparenthesised arrow parameter and a catch
/// clause. Losing either turns the binding site into a *use* of whatever
/// module-level symbol shares that name, which both inflates that symbol's
/// reference count and hides its real dead-code finding.
#[test]
fn alias_and_parameter_binding_sites_are_not_uses() {
    // `parameter` field: `x` binds, `offset` is the only real use.
    let arrow = extract_treesitter("f.js", "javascript", "const f = x => x + offset;\n");
    assert_eq!(
        arrow
            .references
            .iter()
            .map(|reference| reference.name.as_str())
            .collect::<Vec<_>>(),
        ["offset"],
        "an arrow parameter is a binding, not a use"
    );

    // `alias` field: `handle` binds and is then a local, so neither the
    // binding nor the later mention is a module-level use.
    let with_stmt = extract_treesitter(
        "f.py",
        "python",
        "def run():\n    with open(path) as handle:\n        return handle\n",
    );
    let with_names: Vec<&str> = with_stmt
        .references
        .iter()
        .map(|reference| reference.name.as_str())
        .collect();
    assert!(
        !with_names.contains(&"handle"),
        "`as handle` is a binding site: {with_names:?}"
    );
    assert!(
        with_names.contains(&"open") && with_names.contains(&"path"),
        "positive control: the call and its argument are still uses: {with_names:?}"
    );

    // The same field carries `except … as e`.
    let except = extract_treesitter(
        "g.py",
        "python",
        "def run():\n    try:\n        go()\n    except Err as e:\n        return e\n",
    );
    let except_names: Vec<&str> = except
        .references
        .iter()
        .map(|reference| reference.name.as_str())
        .collect();
    assert!(
        !except_names.contains(&"e"),
        "`as e` is a binding site: {except_names:?}"
    );
    assert!(
        except_names.contains(&"Err"),
        "positive control: the exception type is a use: {except_names:?}"
    );
}

/// Names inside a Rust `use` list are imports, not uses.
///
/// `is_inside_import_or_export` is what keeps a `use` list from counting as
/// a reference to everything it names. Forced to `false`, every imported
/// name gains a phantom use, so an item that is imported and never called
/// looks live and its dead-code finding disappears. A Rust `use` list is the
/// clean case: unlike a TS import specifier, its names are not also caught
/// by the defining-name check ahead of it.
#[test]
fn names_in_a_rust_use_list_are_not_uses() {
    let extraction = extract_treesitter(
        "f.rs",
        "rust",
        "use crate::thing::{One, Two};\nfn run() -> One { One }\n",
    );
    let two_uses = extraction
        .references
        .iter()
        .filter(|reference| reference.name == "Two")
        .count();
    assert_eq!(
        two_uses,
        0,
        "`Two` is only imported, never used, and must have no reference: {:?}",
        extraction
            .references
            .iter()
            .map(|r| (&r.name, r.kind))
            .collect::<Vec<_>>()
    );
    assert!(
        extraction
            .references
            .iter()
            .any(|reference| reference.name == "One"),
        "positive control: `One` is genuinely used and does have references"
    );
}

/// The defining-name walk climbs wrappers but still stops.
///
/// `is_binding_wrapper` forced to `true` makes the walk climb out of every
/// expression until it finds *some* ancestor that looks like a binding, so
/// ordinary uses are reclassified as definitions and disappear. Measured on
/// a five-language corpus that mutation silently drops 86 edges, among them
/// real `References` between methods of the same class. The shape below is
/// the minimal reproduction: attribute uses and parameter occurrences stop
/// being recorded, so a method reached only through `self` loses its
/// incoming edge and moves toward `dead`.
#[test]
fn the_defining_name_walk_climbs_wrappers_but_still_stops() {
    let extraction = extract_treesitter(
            "f.py",
            "python",
            "class C:\n    def client(self):\n        return 1\n    def reset(self):\n                     self._c = self.client\n",
        );
    let mut names: Vec<&str> = extraction
        .references
        .iter()
        .map(|reference| reference.name.as_str())
        .collect();
    names.sort_unstable();
    assert_eq!(
        names,
        ["_c", "client", "self", "self", "self", "self"],
        "every `self` occurrence and both attribute names are uses; a walk that \
             never stops reclassifies them as definitions and drops them"
    );
}

/// Exported module-level bindings are symbols; private and local ones are not.
///
/// An exported constant is public API surface. Without a symbol it is
/// invisible to the map: unsearchable, unreferenceable, and neither
/// confirmable as live nor reportable as dead. The frozen Python baseline
/// emits these (misclassified as `function`); devmap records them as
/// `Variable`. Scope is deliberate — private and function-local bindings
/// stay out, since their only graph effect would be dead-code noise.
#[test]
fn exported_module_level_bindings_are_symbols() {
    let rust = extract_treesitter(
            "a.rs",
            "rust",
            "pub const LIMIT: u32 = 10;\nstatic PRIV: u32 = 1;\npub static G: u32 = 2;\n             fn f() { const INNER: u32 = 3; }\n",
        );
    let rust_names: Vec<&str> = rust
        .symbols
        .iter()
        .filter(|symbol| symbol.kind == SymbolKind::Variable)
        .map(|symbol| symbol.name.as_str())
        .collect();
    assert_eq!(
        rust_names,
        ["LIMIT", "G"],
        "only `pub` module-level bindings; not private, not function-local"
    );

    let go = extract_treesitter(
            "b.go",
            "go",
            "package p\nconst Limit = 10\nvar Global = 1\nconst lower = 2\n             func f() { const Inner = 4 }\n",
        );
    let go_names: Vec<&str> = go
        .symbols
        .iter()
        .filter(|symbol| symbol.kind == SymbolKind::Variable)
        .map(|symbol| symbol.name.as_str())
        .collect();
    assert_eq!(
        go_names,
        ["Limit", "Global"],
        "Go exports by capitalisation; `lower` and the function-local are out"
    );

    let typescript = extract_treesitter(
        "c.ts",
        "typescript",
        "export const VALUE = 42;\nconst local = 1;\nexport const fn = () => {};\n",
    );
    let ts_variables: Vec<&str> = typescript
        .symbols
        .iter()
        .filter(|symbol| symbol.kind == SymbolKind::Variable)
        .map(|symbol| symbol.name.as_str())
        .collect();
    assert_eq!(
        ts_variables,
        ["VALUE"],
        "only the exported non-function const"
    );
    assert!(
        typescript
            .symbols
            .iter()
            .any(|symbol| symbol.name == "fn" && symbol.kind == SymbolKind::Function),
        "an exported arrow is still a Function, not a Variable"
    );
}

/// Terraform blocks are symbols addressed the way Terraform addresses them.
#[test]
fn terraform_blocks_are_symbols_with_terraform_addresses() {
    let extraction = extract_treesitter(
            "main.tf",
            "hcl",
            "resource \"aws_s3_bucket\" \"b\" {\n  bucket = \"x\"\n               lifecycle {\n    prevent_destroy = true\n  }\n}\n\n             variable \"region\" {\n  type = string\n}\n\n             module \"vpc\" {\n  source = \"./vpc\"\n}\n",
        );
    let rows: Vec<String> = extraction
        .symbols
        .iter()
        .filter(|symbol| symbol.kind != SymbolKind::File)
        .map(|symbol| format!("{:?}:{}", symbol.kind, symbol.name))
        .collect();
    assert_eq!(
        rows,
        [
            "Module:resource.aws_s3_bucket.b",
            "Variable:variable.region",
            "Module:module.vpc",
        ],
        "a nested `lifecycle` block is an attribute of its resource, not an \
             addressable unit, and must not be emitted"
    );
}

/// Every Python module-scope binding is a symbol; `__all__` decides only
/// whether it is exported.
///
/// This test used to pin the opposite — "public exactly when `__all__` says
/// so", with a module lacking `__all__` contributing no constants at all —
/// on the premise that `__all__` is Python's only declaration of public
/// surface and that the remaining bindings' only graph effect is dead-code
/// noise. Both halves were wrong. `__all__` governs `from m import *` and
/// nothing else: `from m import OTHER` imports any module-scope name, and
/// real code does exactly that with constants `__all__` omits. And a
/// binding's graph effect is not only liveness — without a symbol it is
/// unsearchable and its importers have no target. Functions and classes
/// were never filtered this way; constants now follow the same rule, and
/// liveness judges them by their edges. A class attribute and a function
/// local are still not module bindings.
#[test]
fn a_python_module_binding_is_a_symbol_and_all_decides_only_export() {
    let declared = extract_treesitter(
            "m.py",
            "python",
            "__all__ = [\"CONST\", \"Widget\"]\nCONST = 1\nOTHER = 2\n_priv = 3\n             class Widget:\n    ATTR = 5\ndef f():\n    LOCAL = 6\n",
        );
    let variables: Vec<(&str, bool)> = declared
        .symbols
        .iter()
        .filter(|symbol| symbol.kind == SymbolKind::Variable)
        .map(|symbol| (symbol.name.as_str(), symbol.is_exported))
        .collect();
    assert_eq!(
        variables,
        [("CONST", true), ("OTHER", false), ("_priv", false)],
        "every module-scope binding is a symbol and only the `__all__` member \
             is exported; `ATTR` is a class attribute and `LOCAL` a function local"
    );

    let undeclared = extract_treesitter("n.py", "python", "CONST = 1\nOTHER = 2\n");
    let variables: Vec<(&str, bool)> = undeclared
        .symbols
        .iter()
        .filter(|symbol| symbol.kind == SymbolKind::Variable)
        .map(|symbol| (symbol.name.as_str(), symbol.is_exported))
        .collect();
    assert_eq!(
        variables,
        [("CONST", false), ("OTHER", false)],
        "a module with no `__all__` still binds its constants"
    );
}

/// SC17a. Every JSX tag used to emit a call edge, including intrinsic host
/// elements. `<div/>` compiles to the string `"div"`, so that edge names a
/// symbol that cannot exist — on the 12,817-file corpus `div` alone was the
/// 5th most common unresolved callee at 10,307 rows, and `button` 2,280.
/// The noise is not merely cosmetic: it buries real resolution failures.
///
/// The tag must still be recorded as a `JsxTag` reference — dropping the
/// call must not cost the graph its knowledge that the tag was used.
#[test]
fn jsx_intrinsic_tags_emit_no_call_but_components_still_do() {
    let source = concat!(
        "export function View() {\n",
        "  return (\n",
        "    <div>\n",
        "      <Widget />\n",
        "      <Panel.Header />\n",
        "      <svg:circle />\n",
        "      <button onClick={go} />\n",
        "    </div>\n",
        "  );\n",
        "}\n",
    );
    let extraction = extract_treesitter("v.tsx", "tsx", source);
    let callees: Vec<&str> = extraction
        .calls
        .iter()
        .map(|call| call.callee_name.as_str())
        .collect();

    for component in ["Widget", "Header"] {
        assert!(
            callees.contains(&component),
            "component tag {component:?} must still emit a call; got {callees:?}"
        );
    }
    // SC26: a member tag is a member access, so the namespace is the
    // receiver rather than part of the name.
    assert!(
        extraction
            .calls
            .iter()
            .any(|c| c.callee_name == "Header" && c.receiver_expr.as_deref() == Some("Panel")),
        "`<Panel.Header/>` must split into receiver `Panel`; got {:?}",
        extraction
            .calls
            .iter()
            .map(|c| (&c.callee_name, &c.receiver_expr))
            .collect::<Vec<_>>()
    );
    for intrinsic in ["div", "button", "svg:circle"] {
        assert!(
            !callees.contains(&intrinsic),
            "intrinsic tag {intrinsic:?} must not emit a call; got {callees:?}"
        );
    }

    // The tag itself is still known to the graph.
    assert!(
        extraction
            .references
            .iter()
            .any(|r| r.name == "div" && r.kind == ReferenceKind::JsxTag),
        "the `div` JsxTag reference must survive: {:?}",
        extraction
            .references
            .iter()
            .map(|r| (&r.name, &r.kind))
            .collect::<Vec<_>>()
    );
}

/// SC22. Shell and SQL had no grammar, so every `.sh` and `.sql` file
/// returned `ParseOutcome::Failed` and contributed nothing but a File node —
/// 86 shell and 19 SQL files on the personal corpus. Neither is in the
/// frozen Python 35, so neither has a `LanguageSpec`; they reach a grammar
/// through `detect_language`'s fallback table.
///
/// Asserts the symbols, not merely that the parse succeeded: a Clean parse
/// that yields nothing is what SQL did before `generic_symbol_kind` and
/// `generic_declaration_name` learned its node shapes.
#[test]
fn shell_and_sql_extract_their_declarations() {
    let shell = extract_treesitter(
        "deploy.sh",
        "shell",
        "#!/bin/bash\nhelper() {\n  echo hi\n}\nmain() {\n  helper\n}\nmain\n",
    );
    assert_eq!(shell.parse_outcome, ParseOutcome::Clean);
    let shell_names: Vec<&str> = shell
        .symbols
        .iter()
        .filter(|s| s.kind == SymbolKind::Function)
        .map(|s| s.name.as_str())
        .collect();
    assert_eq!(
        shell_names,
        vec!["helper", "main"],
        "shell function definitions must be symbols"
    );

    let sql = extract_treesitter(
        "schema.sql",
        "sql",
        concat!(
            "CREATE TABLE analytics.users (id INT);\n",
            "CREATE VIEW active AS SELECT id FROM users;\n",
            "CREATE FUNCTION tally() RETURNS INT AS $$ SELECT 1 $$ LANGUAGE SQL;\n",
            "CREATE INDEX users_id_idx ON users(id);\n",
        ),
    );
    assert_eq!(sql.parse_outcome, ParseOutcome::Clean);
    let sql_symbols: Vec<(&str, &SymbolKind)> = sql
        .symbols
        .iter()
        .filter(|s| s.kind != SymbolKind::File)
        .map(|s| (s.name.as_str(), &s.kind))
        .collect();
    assert_eq!(
        sql_symbols,
        vec![
            // Schema-qualified: the identity is the name, not `analytics.users`.
            ("users", &SymbolKind::Struct),
            ("active", &SymbolKind::Struct),
            ("tally", &SymbolKind::Function),
        ],
        "an index names no callable and owns no fields, so it must not \
             become a symbol"
    );
}

/// SC26. Three call shapes recorded a whole expression as the callee, so
/// each named a symbol nothing declares and joined to nothing. Found by
/// reading the `unresolved` tier once SC18/SC25 had cleared the expected
/// rows out of it — together they were 11,847 of its 34,013 rows.
///
/// The Rust case is not merely noise: `MyType::create()` is a *real edge* to
/// a user-defined associated function that the graph was dropping.
#[test]
fn call_targets_split_instead_of_recording_whole_expressions() {
    // Rust associated functions and module paths.
    let rust = extract_treesitter(
        "b.rs",
        "rust",
        concat!(
            "fn run() {\n",
            "    let v = MyType::create();\n",
            "    std::fs::write(\"a\", \"b\");\n",
            "}\n",
        ),
    );
    let rust_calls: Vec<(&str, Option<&str>)> = rust
        .calls
        .iter()
        .map(|c| (c.callee_name.as_str(), c.receiver_expr.as_deref()))
        .collect();
    assert!(
        rust_calls.contains(&("create", Some("MyType"))),
        "a Rust associated function must split into callee + type; got {rust_calls:?}"
    );
    assert!(
        rust_calls.contains(&("write", Some("std::fs"))),
        "a Rust path call must split into callee + module path; got {rust_calls:?}"
    );
    assert!(
        !rust_calls.iter().any(|(callee, _)| callee.contains("::")),
        "no callee may still carry a path separator; got {rust_calls:?}"
    );

    // `await` only breaks the callee when a type argument is present, which
    // is why plain awaited calls never showed the defect.
    let ts = extract_treesitter(
        "a.ts",
        "typescript",
        concat!(
            "async function run() {\n",
            "  const r = await invoke<Raw>('x');\n",
            "  const q = await api.post<Raw>('y');\n",
            "}\n",
        ),
    );
    let ts_calls: Vec<(&str, Option<&str>)> = ts
        .calls
        .iter()
        .map(|c| (c.callee_name.as_str(), c.receiver_expr.as_deref()))
        .collect();
    assert!(
        ts_calls.contains(&("invoke", None)),
        "an awaited generic call must name the function; got {ts_calls:?}"
    );
    assert!(
        ts_calls.contains(&("post", Some("api"))),
        "an awaited generic member call must split; got {ts_calls:?}"
    );
    assert!(
        !ts_calls
            .iter()
            .any(|(callee, _)| callee.starts_with("await")),
        "`await` must never be part of a callee name; got {ts_calls:?}"
    );

    // A JSX member tag is a member access, not a component of that name.
    let tsx = extract_treesitter(
        "v.tsx",
        "tsx",
        "export const V = () => <motion.div><Plain /></motion.div>;\n",
    );
    let tsx_calls: Vec<(&str, Option<&str>)> = tsx
        .calls
        .iter()
        .map(|c| (c.callee_name.as_str(), c.receiver_expr.as_deref()))
        .collect();
    assert!(
        tsx_calls.contains(&("div", Some("motion"))),
        "a JSX member tag must split into namespace + property; got {tsx_calls:?}"
    );
    assert!(
        tsx_calls.contains(&("Plain", None)),
        "a plain component tag must keep its bare name; got {tsx_calls:?}"
    );
}

/// SC26b. Two more whole-expression callees, found by re-reading the
/// `unresolved` tier after the first round of splits cleared it further.
#[test]
fn turbofish_and_inline_literals_do_not_become_callee_names() {
    // Type arguments are not part of a callee's identity.
    let rust = extract_treesitter(
        "q.rs",
        "rust",
        concat!(
            "fn run(row: Row) {\n",
            "    let a = row.get::<_, String>(0);\n",
            "    let b = serde_json::from_str::<Value>(\"{}\");\n",
            "}\n",
        ),
    );
    let rust_calls: Vec<(&str, Option<&str>)> = rust
        .calls
        .iter()
        .map(|c| (c.callee_name.as_str(), c.receiver_expr.as_deref()))
        .collect();
    assert!(
        rust_calls.contains(&("get", Some("row"))),
        "a turbofish method call must split to its method; got {rust_calls:?}"
    );
    assert!(
        rust_calls.contains(&("from_str", Some("serde_json"))),
        "a turbofish path call must split to its function; got {rust_calls:?}"
    );
    assert!(
        !rust_calls.iter().any(|(callee, _)| callee.contains('<')),
        "no callee may carry type arguments; got {rust_calls:?}"
    );

    // An immediately-invoked literal has no callee identity at all.
    let go = extract_treesitter(
        "d.go",
        "go",
        concat!(
            "package p\n",
            "func run() {\n",
            "\tdefer func() { _ = recover() }()\n",
            "\tnamed()\n",
            "}\n",
        ),
    );
    let go_calls: Vec<&str> = go.calls.iter().map(|c| c.callee_name.as_str()).collect();
    assert!(
        go_calls.contains(&"named"),
        "ordinary calls must survive; got {go_calls:?}"
    );
    assert!(
        !go_calls.iter().any(|c| c.starts_with("func(")),
        "an inline function literal must not become a callee name; got {go_calls:?}"
    );
}

/// SC17b. A Go composite literal recorded its whole *type expression* as the
/// callee, so `[]*genai.Part{...}` produced a call to the literal text
/// `[]*genai.Part`. No symbol is ever named that, so the edge could not
/// join, and the real construction of `Part` was lost. 24,382 such rows on
/// the corpus.
///
/// Unwrapping recovers the real reference. A literal whose core is a
/// predeclared type constructs nothing indexable and must stay silent
/// rather than emit a call to `string`.
#[test]
fn go_composite_literals_reference_the_named_type_they_construct() {
    let source = concat!(
        "package main\n",
        "func build() {\n",
        "\t_ = []*Hypothesis{}\n",
        "\t_ = map[string]*Finding{}\n",
        "\t_ = Session{}\n",
        "\t_ = []genai.Part{}\n",
        "\t_ = []string{}\n",
        "\t_ = map[string]int{}\n",
        "}\n",
    );
    let extraction = extract_treesitter("b.go", "go", source);
    let callees: Vec<&str> = extraction
        .calls
        .iter()
        .map(|call| call.callee_name.as_str())
        .collect();

    for named in ["Hypothesis", "Finding", "Session", "Part"] {
        assert!(
            callees.contains(&named),
            "literal must reference the named type {named:?}; got {callees:?}"
        );
    }
    // The wrapper text must never be the callee.
    for literal_text in ["[]*Hypothesis", "map[string]*Finding", "[]string"] {
        assert!(
            !callees.contains(&literal_text),
            "type expression {literal_text:?} must not be a callee; got {callees:?}"
        );
    }
    // Predeclared cores construct nothing indexable.
    for predeclared in ["string", "int"] {
        assert!(
            !callees.contains(&predeclared),
            "predeclared type {predeclared:?} must not be a callee; got {callees:?}"
        );
    }
}

/// Every `grammar_for` arm is counted, and every counted key has an arm.
///
/// `linked_grammar_count` is what hosts compare against a vendored
/// expectation. If a new language grows an arm without joining
/// `GRAMMAR_LANGUAGE_KEYS`, the count stays stale and the host cannot
/// detect the mismatch; if a key is listed without an arm, the count
/// under-reports silently the other way.
#[test]
fn linked_grammar_keys_cover_every_grammar_for_arm() {
    for lang in GRAMMAR_LANGUAGE_KEYS {
        assert!(
            grammar_for(lang).is_some()
                || UNSAFE_GRAMMARS
                    .iter()
                    .any(|(unsafe_lang, _)| unsafe_lang == lang),
            "{lang} is listed in GRAMMAR_LANGUAGE_KEYS but has no grammar_for arm \
                 and is not in UNSAFE_GRAMMARS"
        );
    }
    // Spot-check: a language that is not listed must not suddenly grow an
    // arm without updating the list. Probe a few ids that are deliberately
    // absent from the table.
    for absent in ["vb", "cobol", "fortran", "haskell"] {
        if GRAMMAR_LANGUAGE_KEYS.contains(&absent) {
            continue;
        }
        assert!(
            grammar_for(absent).is_none()
                || UNSAFE_GRAMMARS
                    .iter()
                    .any(|(unsafe_lang, _)| *unsafe_lang == absent),
            "{absent} has a grammar_for arm but is missing from GRAMMAR_LANGUAGE_KEYS"
        );
    }
    let count = linked_grammar_count();
    assert!(
        count >= 30,
        "linked_grammar_count()={count} is too low for this workspace's grammar set"
    );
    assert_eq!(count, linked_grammar_keys().len());
}

/// The Kotlin loss detector's two ways of not finishing are reported by the
/// outcome, never read as "nothing lost".
fn kotlin_root_outcome(lost: &crate::kotlinloss::LostScan) -> (usize, ParseOutcome) {
    let source = "fun f() {}\n";
    let (_, language) = grammar_for("kotlin").expect("kotlin grammar");
    let mut parser = Parser::new();
    parser.set_language(&language).expect("load grammar");
    let tree = parser.parse(source, None).expect("tree");
    (
        source.len(),
        parse_outcome_of(tree.root_node(), source, false, false, true, lost),
    )
}

#[test]
fn a_kotlin_loss_scan_stopped_by_its_node_cap_charges_the_whole_file() {
    let lost = crate::kotlinloss::LostScan {
        capped: true,
        ..Default::default()
    };
    let (len, outcome) = kotlin_root_outcome(&lost);
    let ParseOutcome::Partial { error_ranges, .. } = outcome else {
        panic!("a capped scan is not Clean: {outcome:?}");
    };
    assert_eq!(
        error_ranges,
        vec![TextRange {
            start_byte: 0,
            end_byte: len
        }]
    );
}

#[test]
fn a_kotlin_loss_scan_stopped_by_the_deadline_fails_the_outcome() {
    let lost = crate::kotlinloss::LostScan {
        deadline_passed: true,
        ..Default::default()
    };
    let (_, outcome) = kotlin_root_outcome(&lost);
    assert!(
        matches!(&outcome, ParseOutcome::Failed { reason } if reason.contains("budget")),
        "{outcome:?}"
    );
}

thread_local! {
    /// Whether the thread's walk deadline was armed each time the finalization
    /// hook ran, in order. The hook is one-shot and the recovery's inner
    /// extraction runs first and takes it, so the hook puts itself back: the
    /// last entry is the outer file's, after the recovery and every other phase.
    static DEADLINE_ARMED_AT_FINISH: std::cell::RefCell<Vec<bool>> =
        const { std::cell::RefCell::new(Vec::new()) };
}

fn note_deadline_at_finish() {
    let armed = WALK_DEADLINE.with(|slot| slot.get().is_some());
    DEADLINE_ARMED_AT_FINISH.with(|seen| seen.borrow_mut().push(armed));
    EXTRACTION_FINISH_HOOK.with(|slot| slot.set(Some(note_deadline_at_finish)));
}

/// A recovery runs an extraction inside the file's extraction. Its budget guard
/// used to disarm the thread's deadline when it dropped, so every phase of the
/// outer file after it (and the publication gate's helpers) ran against a clock
/// that was off. The outer deadline is armed again afterwards. Fails against
/// `974edaa3`.
#[test]
fn the_walk_deadline_is_armed_again_after_a_kotlin_recovery() {
    struct ClearHook;
    impl Drop for ClearHook {
        fn drop(&mut self) {
            EXTRACTION_FINISH_HOOK.with(|hook| hook.set(None));
        }
    }
    let _clear = ClearHook;
    DEADLINE_ARMED_AT_FINISH.with(|seen| seen.borrow_mut().clear());
    EXTRACTION_FINISH_HOOK.with(|slot| slot.set(Some(note_deadline_at_finish)));
    let extraction = extract_treesitter_with_budget(
        "A.kt",
        "kotlin",
        "@A\n@B\nprivate fun f() {}\nfun after() {}\n",
        std::time::Duration::from_secs(30),
    );
    assert!(
        extraction
            .symbols
            .iter()
            .any(|symbol| symbol.qualified_name == "A.kt::f"),
        "the statement was recovered, so an inner extraction ran: {:?}",
        extraction.diagnostics
    );
    let seen = DEADLINE_ARMED_AT_FINISH.with(|seen| seen.borrow().clone());
    assert_eq!(
        seen.len(),
        2,
        "the recovery's inner extraction finished, then the outer file: {seen:?}"
    );
    assert_eq!(
        seen.last(),
        Some(&true),
        "the deadline must be armed when the outer file finishes"
    );
}
