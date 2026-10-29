use super::*;

/// The `File`-node-only extraction both no-parse paths publish.
pub(super) fn unparsed_extraction(
    path: &str,
    lang: &str,
    source: &str,
    engine: ExtractionEngine,
    parse_outcome: ParseOutcome,
) -> Extraction {
    Extraction {
        file_path: path.to_string(),
        language: lang.to_string(),
        content_hash: content_hash(source),
        engine,
        parse_outcome,
        // The File node is still emitted: being unable to parse a file is not a
        // reason to deny it exists, and every edge that targets it needs a node.
        symbols: vec![ExtractedSymbol {
            name: path.rsplit('/').next().unwrap_or(path).to_string(),
            qualified_name: path.to_string(),
            kind: SymbolKind::File,
            span: Span {
                start_byte: 0,
                end_byte: source.len(),
            },
            is_exported: true,
            docstring: None,
            signature: None,
            parent_symbol: None,
            body_signature: None,
            declaration_hash: None,
            return_type: None,
        }],
        imports: Vec::new(),
        calls: Vec::new(),
        exports: Vec::new(),
        references: Vec::new(),
        diagnostics: Vec::new(),
        routes: Vec::new(),
        wiring: extract_wiring_annotations(path, source),
        go_package: None,
        jvm_package: None,
        scope_receivers: Vec::new(),
        swift_type_aliases: Vec::new(),
        go_build_constrained: false,
        go_interface_methods: Vec::new(),
        go_method_params: Vec::new(),
        go_member_names: None,
        go_facts: None,
        scope_locals: Vec::new(),
        rust_inline_modules: Vec::new(),
        rust_deref_targets: Vec::new(),
        rust_blanket_impl_params: Vec::new(),
        rust_implicit_calls: Vec::new(),
        rust_try_conversions: Vec::new(),
        local_bindings: Vec::new(),
        source_code: Some(source.to_string()),
        literals: Vec::new(),
        js_objects: Vec::new(),
        jvm_decls: Vec::new(),
        package_scopes: Vec::new(),
        local_extents: Vec::new(),
        decl_gaps: Vec::new(),
        recovered_decl_ranges: Vec::new(),
        dynamic_dispatch: Vec::new(),
    }
}

pub(super) fn unavailable_extraction(path: &str, lang: &str, source: &str) -> Extraction {
    // Whether a grammar was ever expected for this format. A `.proto` or `.ps1`
    // with no linked grammar is a gap in coverage; a `.md` is not, and K5 is
    // the record of what conflating them cost — 294 of 1,310 files reported as
    // parse failures, all prose and data, hiding the 16 real ones.
    let declarative = crate::fallback::applies_to(lang);
    let scan = if declarative {
        // The language, not just the path: two of the languages this tier serves
        // — `css` and `html` — are read by `crate::markup` rather than by the
        // line scanner, and the dispatch belongs to the tier, not here.
        crate::fallback::scan_declarations_in(lang, path, source)
    } else {
        // Prose and data formats declare nothing; see
        // `fallback::NON_DECLARATIVE_LANGUAGES` for what scanning them produced.
        crate::fallback::FallbackScan {
            symbols: Vec::new(),
            references: Vec::new(),
            truncated: 0,
            skipped_long_lines: 0,
            unread_bytes: 0,
        }
    };
    let recovered_count = scan.symbols.len();
    // A reader ran over this file and produced an answer — either because it
    // found declarations, or because the language has a reader of its own and
    // that reader found none.
    //
    // The second half is load-bearing and was not here at first. `html` and `css`
    // moved out of `NON_DECLARATIVE_LANGUAGES` when `crate::markup` gave them a
    // reader, and a page with no ids then landed in the `(false, true)` arm —
    // "a grammar was wanted for this language and was not there" — so every
    // `.html` and `.css` file without an anchor or a rule counted as a parse
    // failure. That is the K5 defect exactly, and the store's
    // `k5_prose_formats_are_not_parse_failures_but_broken_files_still_are`
    // caught it. Finding nothing in a file that declares nothing is a completed
    // read, and it must not report what a failed one reports.
    let recovered = recovered_count > 0 || crate::fallback::has_dedicated_reader(lang);

    // The `File` node, which this path used to omit entirely.
    //
    // It is pushed on the tree-sitter path above as the first symbol of every
    // parsed file, but that push sits *inside* the `if let Some(ts_lang)` block,
    // so a file with no linked grammar was recorded in `generation_files` and
    // contributed nothing to the graph: not addressable by a file-level query,
    // and unusable as the target of any edge. That is placement, not design —
    // nothing downstream wants a nodeless file.
    //
    // Emitted for *every* grammarless file, including the prose and data
    // formats that get no declaration scan. Being unable to recover a file's
    // declarations is not a reason to deny that the file exists; a Markdown
    // document that something links to still has to be a valid edge target.
    //
    // Safe against the dead-code surface by construction: `File` nodes are
    // exempt in `liveness.rs` (both the Go duplicate-identity count and the
    // dead-symbol sweep) and are skipped as `Contains` sources in the resolver,
    // so this widens what the graph can address without adding a single
    // dead-symbol candidate.
    let mut symbols = Vec::with_capacity(scan.symbols.len() + 1);
    symbols.push(ExtractedSymbol {
        name: path.rsplit('/').next().unwrap_or(path).to_string(),
        qualified_name: path.to_string(),
        kind: SymbolKind::File,
        span: Span {
            start_byte: 0,
            end_byte: source.len(),
        },
        is_exported: true,
        docstring: None,
        signature: None,
        parent_symbol: None,
        // A file has no body to fingerprint; clone detection is over symbol
        // bodies, and `None` means "not computed", never "no duplicates".
        body_signature: None,
        declaration_hash: None,
        return_type: None,
    });
    symbols.extend(scan.symbols);
    let mut diagnostics: Vec<String> = Vec::new();
    if scan.truncated > 0 {
        // Reported, not silently dropped: the symbol list is a prefix, and a
        // consumer that reads it as complete would conclude the rest of the
        // file declares nothing.
        diagnostics.push(format!(
            "fallback declaration scan stopped at {} symbols; {} more were dropped",
            crate::fallback::MAX_FALLBACK_SYMBOLS,
            scan.truncated
        ));
    }
    Extraction {
        file_path: path.to_string(),
        language: lang.to_string(),
        content_hash: content_hash(source),
        engine: match (recovered, declarative) {
            (true, _) => ExtractionEngine::RegexFallback {
                requested_language: lang.to_string(),
            },
            // A grammar was wanted for this language and was not there: a real
            // gap, and `Extraction::is_parse_failure` counts it.
            (false, true) => ExtractionEngine::Unavailable {
                requested_language: lang.to_string(),
            },
            // Prose or data. No grammar was ever expected, so this is not a
            // failure to report — see `ExtractionEngine::NotApplicable`.
            (false, false) => ExtractionEngine::NotApplicable {
                language: lang.to_string(),
            },
        },
        parse_outcome: match (recovered, declarative) {
            // The dropped count rides on the *reason*, not on `diagnostics`.
            // `for_durable_store` clears `diagnostics` before the payload is
            // written to `generation_files.extraction_json` and the extraction
            // cache, and nothing in the workspace reads that field in
            // production — so the truncation the scanner correctly computed was
            // erased on the way to storage, and the stored record of a 2,500
            // declaration file read as a complete recovery of 2,000. A
            // truncation is part of the result, not a note about it.
            (true, _) => ParseOutcome::Fallback {
                reason: {
                    // Two different ways to lose a declaration, and they are
                    // not interchangeable. The scan cap drops declarations the
                    // scanner *counted*, so the true total is known exactly.
                    // A skipped line was never pattern-matched at all, so how
                    // many declarations it held is unknown — which makes the
                    // total a lower bound, and saying so is the whole point.
                    let counted_total = recovered_count + scan.truncated;
                    let mut caveats = Vec::new();
                    if scan.truncated > 0 {
                        caveats.push(format!("{} dropped at the scan cap", scan.truncated));
                    }
                    if scan.skipped_long_lines > 0 {
                        caveats.push(format!(
                            "{} line(s) over {} bytes never scanned, so the total is a lower \
                             bound",
                            scan.skipped_long_lines,
                            crate::fallback::MAX_LINE_BYTES
                        ));
                    }
                    // A third way to lose a declaration, and the only one the
                    // markup and stylesheet readers can hit: a byte cap stopped
                    // the reader partway through the file. Like a skipped line
                    // and unlike the scan cap, what those bytes held was never
                    // counted, so the total is a lower bound.
                    if scan.unread_bytes > 0 {
                        caveats.push(format!(
                            "{} byte(s) never read at a reader's byte cap, so the total is a \
                             lower bound",
                            scan.unread_bytes
                        ));
                    }
                    // The "X of Y" form is used only when Y is genuinely
                    // known — that is, when every declaration was counted and
                    // some were dropped afterwards. If a line was never
                    // scanned, printing "2 of 2" would state a total the
                    // scanner cannot know, which is the same overclaim in a
                    // smaller font.
                    let counted = if scan.truncated > 0 {
                        format!("{recovered_count} of {counted_total} declaration(s)")
                    } else {
                        format!("{recovered_count} declaration(s)")
                    };
                    if caveats.is_empty() {
                        format!(
                            "no linked tree-sitter grammar for {lang}; {counted} recovered by \
                             pattern"
                        )
                    } else {
                        format!(
                            "no linked tree-sitter grammar for {lang}; {counted} recovered by \
                             pattern, {} — the symbol list is a prefix, not a set",
                            caveats.join(" and ")
                        )
                    }
                },
            },
            (false, true) => ParseOutcome::Failed {
                reason: format!("no linked tree-sitter grammar for {lang}"),
            },
            (false, false) => ParseOutcome::Failed {
                reason: format!(
                    "{lang} is a prose/data format with no declarations to parse; \
                     indexed as a File node"
                ),
            },
        },
        symbols,
        imports: Vec::new(),
        calls: Vec::new(),
        exports: Vec::new(),
        // Empty for the line scanner, which recovers declarations only. The
        // markup and stylesheet readers behind this same tier do read uses — a
        // `class` attribute, a `var(--x)` — and dropping them here would index
        // one half of a contract that only means anything as a pair.
        references: scan.references,
        diagnostics,
        routes: Vec::new(),
        wiring: extract_wiring_annotations(path, source),
        go_package: None,
        jvm_package: None,
        scope_receivers: Vec::new(),
        swift_type_aliases: Vec::new(),
        go_build_constrained: false,
        go_interface_methods: Vec::new(),
        go_method_params: Vec::new(),
        go_member_names: None,
        go_facts: None,
        scope_locals: Vec::new(),
        rust_inline_modules: Vec::new(),
        rust_deref_targets: Vec::new(),
        rust_blanket_impl_params: Vec::new(),
        rust_implicit_calls: Vec::new(),
        rust_try_conversions: Vec::new(),
        local_bindings: Vec::new(),
        source_code: Some(source.to_string()),
        literals: Vec::new(),
        js_objects: Vec::new(),
        jvm_decls: Vec::new(),
        package_scopes: Vec::new(),
        local_extents: Vec::new(),
        decl_gaps: Vec::new(),
        recovered_decl_ranges: Vec::new(),
        dynamic_dispatch: Vec::new(),
    }
}

// The explicit worklist avoids stack overflow on adversarially deep syntax
// while preserving tree-sitter's deterministic pre-order traversal.
#[allow(clippy::too_many_arguments)]
pub(super) fn walk_tree(
    root: Node,
    source: &str,
    lang: &str,
    file_symbol_name: &str,
    symbols: &mut Vec<ExtractedSymbol>,
    imports: &mut Vec<ExtractedImport>,
    calls: &mut Vec<ExtractedCall>,
    exports: &mut Vec<ExtractedExport>,
    references: &mut Vec<ExtractedReference>,
    wiring: &mut Vec<WiringAnnotation>,
    deadline: std::time::Instant,
) -> bool {
    let mut worklist = vec![root];
    let mut since_check = 0u32;
    while let Some(node) = worklist.pop() {
        // A `Cell` read, cheap enough for every node, unlike a clock read.
        if walk_overran() {
            return false;
        }
        // The clock is read every `DEADLINE_CHECK_STRIDE` nodes rather than
        // every node: an ordinary file walks millions of nodes and each read is
        // a real cost, while the pathological case that needs bounding takes
        // orders of magnitude longer than the stride can hide.
        since_check += 1;
        if since_check >= DEADLINE_CHECK_STRIDE {
            since_check = 0;
            if std::time::Instant::now() >= deadline {
                return false;
            }
        }
        extract_node(
            node,
            source,
            lang,
            file_symbol_name,
            symbols,
            imports,
            calls,
            exports,
            references,
            wiring,
        );
        // Beside `extract_node` rather than inside it: heritage is one decision
        // that fifteen languages spell differently, and putting it in the
        // per-language match would scatter it across every arm.
        crate::heritage::push_heritage_references(node, source, lang, file_symbol_name, references);
        push_children_reversed(node, &mut worklist);
    }
    true
}

/// Nodes walked between deadline checks. See `walk_tree`.
pub(crate) const DEADLINE_CHECK_STRIDE: u32 = 256;

/// The `{ a, b as c }` clause of a JavaScript/TypeScript import or export
/// statement, or `None` when the statement has no binding clause at all.
///
/// Located structurally because the text scan it replaces was wrong in both
/// directions on ordinary source. `text.find('{')` and `text.find('}')` were
/// taken independently with no ordering check, so
/// `export const isClose = (c) => c === '}' || c === '{';` — valid, idiomatic
/// JavaScript — produced an inverted slice range and panicked, aborting the
/// whole build (`extract_all` runs under rayon and release then used
/// `panic = "abort"`). With the two literals the other way round the same scan
/// did not panic; it fabricated an import *and* an export of a name spelled
/// `'`, which is the quieter half of the same defect. `export default function
/// f() { … }` was read the same way, with the function body parsed as a binding
/// list.
///
/// The grammar already answers the question exactly: a binding clause is an
/// `export_clause` (`export { … }`) or a `named_imports` (`import { … }`,
/// reached through the statement's `import_clause`), and a statement that has
/// neither binds no names. Both are direct or once-nested children, so this
/// looks no deeper and cannot mistake a nested object literal for a clause.
pub(super) fn js_binding_clause(node: Node<'_>) -> Option<Node<'_>> {
    let mut cursor = node.walk();
    let mut inner_cursor = node.walk();
    for child in node.named_children(&mut cursor) {
        match child.kind() {
            "export_clause" | "named_imports" => return Some(child),
            "import_clause" => {
                for inner in child.named_children(&mut inner_cursor) {
                    if inner.kind() == "named_imports" {
                        return Some(inner);
                    }
                }
            }
            _ => {}
        }
    }
    None
}

/// The local name a default import binds beside braced ones:
/// `Card` in `import Card, { memo } from './x'`. The grammar puts the default
/// as the first identifier of the `import_clause`.
pub(super) fn js_default_import_local(node: Node<'_>, source: &str) -> Option<String> {
    let mut cursor = node.walk();
    let clause = node
        .named_children(&mut cursor)
        .find(|child| child.kind() == "import_clause")?;
    let mut inner = clause.walk();
    let first = clause.named_children(&mut inner).next()?;
    (first.kind() == "identifier").then(|| get_node_text(first, source))
}

/// The local binding an `export default …` statement publishes, when the
/// statement names one: `export default function App() {}`,
/// `export default Card;`, `export default memo(Card);`.
///
/// A component wrapper ([`JS_FUNCTION_WRAPPERS`]) returns the component it was
/// given, so the default export is the wrapped identifier. Any other call
/// (`connect(mapState)(Card)`, `withRouter(Card)`) is not looked through — what
/// it returns is its own business — and neither is an anonymous function,
/// class or object.
pub(super) fn js_default_export_local(statement: Node<'_>, source: &str) -> Option<String> {
    if statement.kind() != "export_statement" {
        return None;
    }
    let mut cursor = statement.walk();
    let is_default = statement
        .children(&mut cursor)
        .any(|child| child.kind() == "default");
    if !is_default {
        return None;
    }
    if let Some(declaration) = statement.child_by_field_name("declaration") {
        return get_child_text(declaration, "name", source).filter(|name| !name.is_empty());
    }
    let mut value = statement.child_by_field_name("value")?;
    for _ in 0..=JS_WRAPPER_NESTING {
        match value.kind() {
            "identifier" => return Some(get_node_text(value, source)),
            "call_expression" => {
                let callee = value.child_by_field_name("function")?;
                if !JS_FUNCTION_WRAPPERS.contains(&get_node_text(callee, source).as_str()) {
                    return None;
                }
                value = value.child_by_field_name("arguments")?.named_child(0)?;
            }
            _ => return None,
        }
    }
    None
}

pub(super) fn find_string_child(node: Node, source: &str) -> Option<String> {
    if let Some(s) = node.child_by_field_name("source") {
        return Some(
            get_node_text(s, source)
                .trim_matches('"')
                .trim_matches('\'')
                .to_string(),
        );
    }
    let mut cursor = node.walk();
    for c in node.children(&mut cursor) {
        if c.kind() == "string" {
            return Some(
                get_node_text(c, source)
                    .trim_matches('"')
                    .trim_matches('\'')
                    .to_string(),
            );
        }
    }
    None
}

/// Graph identity of the function or method lexically containing `node`.
///
/// Without this every call edge is attributed to the file rather than to the
/// caller, which collapses `app.py::my_func -> app.py::open` into
/// `app.py -> app.py::open` and erases caller granularity from impact and
/// trace. Returns `None` at file scope, where the file itself is the caller.
pub(crate) fn enclosing_callable_qualified(
    node: Node,
    source: &str,
    file_symbol_name: &str,
) -> Option<String> {
    let mut scope_node = node;
    let mut ancestor = bounded_parent(node);
    let mut names = Vec::new();
    let mut base = None;
    let mut since_check = 0;
    while let Some(parent) = ancestor {
        since_check += 1;
        if walk_overran() || (since_check >= PARENT_CHECK_STRIDE && walk_deadline_passed()) {
            return None;
        }
        if since_check >= PARENT_CHECK_STRIDE {
            since_check = 0;
        }
        // Python defaults are evaluated by the enclosing scope, before the
        // function's parameters exist. Calls in the body keep the callable.
        if parent.kind() == "function_definition"
            && scope_node.kind() == "call"
            && parent.child_by_field_name("name").is_some()
            && field_contains(parent, "parameters", scope_node)
        {
            ancestor = bounded_parent(parent);
            continue;
        }
        // Use the symbol emitter's owner/name pair for C-family out-of-line
        // definitions; their owner is not necessarily an ancestor node.
        if is_c_family_callable(parent) {
            if let Some((owner, name)) = c_callable_identity(parent, source) {
                base = Some(match owner {
                    Some(type_name) => format!("{file_symbol_name}::{type_name}.{name}"),
                    None => format!("{file_symbol_name}::{name}"),
                });
                break;
            }
        }
        if let Some(name) = callable_binding_name(parent, source) {
            // Go receivers live on the declaration itself. Other method
            // owners are found through the same type lookup used by symbols.
            let owner = (parent.kind() == "method_declaration")
                .then(|| go_receiver(parent, source).map(|(_, type_name)| type_name))
                .flatten()
                .or_else(|| enclosing_type_name(parent, source));
            if let Some(type_name) = owner {
                base = Some(format!("{file_symbol_name}::{type_name}.{name}"));
                break;
            }
            // A member of a module-level object literal is qualified by the
            // object, as a class member is by its class.
            if let Some(object) = js_object_member_owner(parent) {
                if let Some(object_name) = get_child_text(object, "name", source) {
                    base = Some(format!("{file_symbol_name}::{object_name}.{name}"));
                    break;
                }
            }
            names.push(name);
            // This is the starting node the former recursive scope lookup
            // would receive, including its Python-default evaluation rules.
            scope_node = parent;
        }
        ancestor = bounded_parent(parent);
    }
    let mut qualified = match base {
        Some(base) => base,
        None => format!("{file_symbol_name}::{}", names.pop()?),
    };
    for name in names.iter().rev() {
        qualified.push('.');
        qualified.push_str(name);
    }
    Some(qualified)
}
