//! The Kotlin silent declaration drop (extract-v98 row 15): the loss detector.
//!
//! tree-sitter-kotlin-ng parses some declarations as *expressions* and reports
//! no error. After two or more annotations and one modifier-like word,
//! `private fun f() {}` is `annotated_expression > annotated_expression >
//! infix_expression [identifier private, identifier fun, call_expression ...]`:
//! a clean tree in which the declaration does not exist. The file was published
//! `Clean`, so no consumer could tell that `f` had been dropped, and a caller of
//! `f` read as a call to nothing. The same shape swallows classes, objects,
//! interfaces, properties and type aliases, and `fun f() = 1` arrives as an
//! `assignment` around the same expression.
//!
//! # The detector is a tree check
//!
//! `fun`, `class`, `interface`, `object`, `val`, `var` and `typealias` are hard
//! keywords: valid Kotlin never spells one as an `identifier`, so an `identifier`
//! node whose whole text is one of them is the grammar having read a declaration
//! as an expression. A text scan would have to know about strings, comments and
//! backticks; the tree already does. Strings and comments are not `identifier`
//! nodes, and a backtick-escaped `` `fun` `` is an `identifier` whose text
//! includes the backticks, so it is not equal to `fun` and does not match.
//!
//! One legitimate `identifier` spelled like a keyword exists: `class` after `::`
//! in a class reference (`String::class`, `this::class`), which the grammar
//! reads as a `navigation_expression` of two identifiers. It is skipped by its
//! `::` sibling, measured on the locked grammar (the first draft of this
//! detector flagged every `X::class` in the corpus).
//!
//! The detector never looks inside an ERROR node: an error range already charges
//! the file there.
//!
//! # Bounded
//!
//! The walk checks the extraction deadline every [`DEADLINE_CHECK_STRIDE`] nodes
//! and stops at a node cap. Neither stop is read as "nothing lost": a deadline
//! passing fails the outcome exactly as `parse_outcome_of`'s own walk does, and a
//! node cap marks the whole file as an error range, because a check that could
//! not finish must never report what a check that ran and found nothing reports.

use std::cell::Cell;
use std::time::Instant;

use tree_sitter::Node;

use crate::model::{
    ExtractedCall, ExtractedReference, ExtractedSymbol, Extraction, JvmDeclFact, LocalBinding,
    LocalExtent, ParseOutcome, ScopeReceiver, Span, SymbolKind, TextRange, WiringAnnotation,
};
use crate::treesitter::{
    bounded_parent, extraction_overran, walk_deadline_passed, DEADLINE_CHECK_STRIDE,
};

/// Words that cannot be an `identifier` in valid Kotlin and that open a
/// declaration.
const HARD_KEYWORDS: [&str; 7] = [
    "fun",
    "class",
    "interface",
    "object",
    "val",
    "var",
    "typealias",
];

/// The longest keyword, so a longer identifier is rejected without a compare.
const LONGEST_KEYWORD: usize = 9;

/// Node kinds an expression-shaped declaration is wrapped in, outermost last.
///
/// Climbed from the keyword to find the statement the grammar swallowed:
/// `annotated_expression` is one level per annotation, `infix_expression` holds
/// the modifier and keyword identifiers, and `assignment` wraps a declaration
/// with an initializer (`fun f() = 1`, `val y = 1`, `typealias T = Int`).
const WRAPPER_KINDS: [&str; 3] = ["annotated_expression", "assignment", "infix_expression"];

/// One statement the grammar read as an expression although it is a
/// declaration.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct LostRegion {
    /// The outermost wrapper node's byte range: the whole swallowed statement.
    pub(crate) range: TextRange,
    /// Start byte and spelling of every hard-keyword `identifier` inside the
    /// region, in document order. Recovery must account for each one.
    pub(crate) keywords: Vec<(usize, &'static str)>,
    /// Whether the region is a direct child of the file's root. Only these have
    /// the shape the recovery was measured on.
    pub(crate) top_level: bool,
    /// Whether the region's own subtree holds a genuine syntax error. Such a
    /// region is not a silent loss and is never recovered: its error range is
    /// the file's own.
    pub(crate) has_error: bool,
}

/// What the detector found.
#[derive(Debug, Default)]
pub(crate) struct LostScan {
    /// Sorted by start byte, one per distinct region.
    pub(crate) regions: Vec<LostRegion>,
    /// The node cap stopped the walk before it covered the tree.
    pub(crate) capped: bool,
    /// The extraction deadline passed during the walk.
    pub(crate) deadline_passed: bool,
}

impl LostScan {
    /// The ranges to charge to the file's parse outcome.
    pub(crate) fn ranges(&self) -> impl Iterator<Item = TextRange> + '_ {
        self.regions.iter().map(|region| region.range.clone())
    }
}

/// The node cap for a source of `len` bytes.
///
/// A tree has at most a couple of nodes per source byte (every token spans a
/// byte, and each named node wraps tokens), so this is never reached by a real
/// parse; it bounds a pathological one. The floor keeps tiny inputs from
/// tripping it on zero-width `MISSING` nodes.
fn node_cap(len: usize) -> usize {
    len.saturating_mul(4).saturating_add(4096)
}

/// Find every statement of a Kotlin tree that the grammar read as an
/// expression. Empty for a tree in which no hard keyword is an `identifier`.
pub(crate) fn detect(root: Node, source: &str) -> LostScan {
    detect_capped(root, source, node_cap(source.len()))
}

/// [`detect`] with an explicit node cap, so the cap can be exercised.
pub(crate) fn detect_capped(root: Node, source: &str, cap: usize) -> LostScan {
    let mut scan = LostScan::default();
    let bytes = source.as_bytes();
    let mut cursor = root.walk();
    let mut visited = 0usize;
    let mut since_check = 0u32;
    let mut found: Vec<LostRegion> = Vec::new();
    'walk: loop {
        let node = cursor.node();
        visited += 1;
        if visited > cap {
            scan.capped = true;
            break;
        }
        since_check += 1;
        if since_check >= DEADLINE_CHECK_STRIDE {
            since_check = 0;
            if walk_deadline_passed() {
                scan.deadline_passed = true;
                break;
            }
        }
        // An ERROR subtree is already charged by its own range.
        let descend = !node.is_error();
        if node.child_count() == 0 && node.kind() == "identifier" {
            let (start, end) = (node.start_byte(), node.end_byte());
            if end > start && end - start <= LONGEST_KEYWORD {
                let word = bytes.get(start..end).unwrap_or_default();
                if let Some(keyword) = HARD_KEYWORDS
                    .iter()
                    .find(|keyword| keyword.as_bytes() == word)
                {
                    // After the spelling test, so the sibling hop is paid only
                    // by an identifier that is spelled like a keyword.
                    if !follows_double_colon(node) {
                        record(node, start, keyword, &mut found);
                    }
                }
            }
        }
        if descend && cursor.goto_first_child() {
            continue;
        }
        loop {
            if cursor.goto_next_sibling() {
                break;
            }
            if !cursor.goto_parent() {
                break 'walk;
            }
        }
    }
    found.sort_by_key(|region| (region.range.start_byte, region.range.end_byte));
    for region in found {
        match scan.regions.last_mut() {
            Some(last) if last.range == region.range => {
                last.keywords.extend(region.keywords);
            }
            _ => scan.regions.push(region),
        }
    }
    for region in &mut scan.regions {
        region.keywords.sort_unstable();
        region.keywords.dedup();
    }
    scan
}

/// Whether `node` is the member of a callable or class reference: the `class`
/// of `String::class`.
fn follows_double_colon(node: Node) -> bool {
    // A comment between the colons and the word (`String:: /* c */ class`) is a
    // sibling too, and says nothing about what the word is.
    let mut before = node.prev_sibling();
    while let Some(sibling) = before {
        if !sibling.kind().ends_with("comment") {
            return sibling.kind() == "::";
        }
        before = sibling.prev_sibling();
    }
    false
}

/// File the region a keyword `identifier` sits in.
fn record(keyword: Node, start: usize, word: &'static str, found: &mut Vec<LostRegion>) {
    let mut top = keyword;
    while let Some(parent) = bounded_parent(top) {
        if WRAPPER_KINDS.contains(&parent.kind()) {
            top = parent;
        } else {
            break;
        }
    }
    let top_level = bounded_parent(top).is_some_and(|parent| parent.parent().is_none());
    found.push(LostRegion {
        range: TextRange {
            start_byte: top.start_byte(),
            end_byte: top.end_byte(),
        },
        keywords: vec![(start, word)],
        top_level,
        has_error: top.has_error(),
    });
}

// ------------------------------------------------------------------ recovery

/// Regions re-parsed per file. Far above any real file (the measured corpus
/// has none past a handful); a region past the cap is not re-parsed, stays in
/// `error_ranges`, and the cap is recorded in the extraction's diagnostics.
pub(crate) const MAX_RECOVERY_REGIONS: usize = 64;

/// The class a swallowed statement is wrapped in for the re-parse. A class body
/// is the one Kotlin context measured to read every swallowed shape as the
/// declaration it is. The name is not a Kotlin identifier anyone writes; a
/// source that spells it is not recovered at all.
const WRAPPER_NAME: &str = "DevmapKotlinRecoveryWrapper";
const WRAPPER_OPEN: &str = "class DevmapKotlinRecoveryWrapper {\n";
const WRAPPER_CLOSE: &str = "\n}";

/// Node kinds a recovered statement may be, as members of the wrapper's body.
const DECLARATION_KINDS: [&str; 5] = [
    "function_declaration",
    "class_declaration",
    "object_declaration",
    "property_declaration",
    "type_alias",
];

thread_local! {
    /// Whether this thread is already recovering a region. The re-parse is a
    /// whole extraction, and a wrapper that still held a swallowed statement
    /// would otherwise recover it in turn without bound.
    static INSIDE_RECOVERY: Cell<bool> = const { Cell::new(false) };
}

/// Sets [`INSIDE_RECOVERY`] for its lifetime and clears it on drop, so a panic
/// in an inner extraction cannot leave a worker thread poisoned.
struct DepthGuard;

impl DepthGuard {
    fn enter() -> Option<Self> {
        INSIDE_RECOVERY.with(|inside| {
            if inside.get() {
                None
            } else {
                inside.set(true);
                Some(Self)
            }
        })
    }
}

impl Drop for DepthGuard {
    fn drop(&mut self) {
        INSIDE_RECOVERY.with(|inside| inside.set(false));
    }
}

/// Whether `node` is the class the recovery wraps a statement in.
///
/// `langdecl::kotlin` asks, and answers "declares nothing" for it, so the
/// wrapper is not an owner and a recovered member is named as the top-level
/// declaration it is: `f`, never `Wrapper.f`. Asking here, in the one place the
/// declaration identity is decided, is what keeps a recovered symbol's
/// `qualified_name` and its calls' `caller_symbol` from drifting.
///
/// Only while a recovery is running: a file whose own first bytes spell the
/// wrapper's header is an ordinary file with an ordinary class, and the outer
/// extraction must not drop it.
pub(crate) fn is_recovery_wrapper(node: Node, source: &str) -> bool {
    node.start_byte() == 0
        && node.kind() == "class_declaration"
        && source.starts_with(WRAPPER_OPEN)
        && INSIDE_RECOVERY.with(|inside| inside.get())
}

/// Re-derive the declarations the grammar swallowed, merge them, and settle
/// every region the detector charged.
///
/// Each top-level region the detector found is parsed again, wrapped in a
/// class body. A region is recovered only if the wrapper parses cleanly, is
/// exactly one class holding declarations, reads every hard keyword of the
/// region as that declaration's own keyword token, and holds no swallowed
/// statement of its own. A symbol that is not in the source is worse than a
/// symbol that is missing.
///
/// A recovered region's own symbols, calls, references, exports, wiring,
/// scope locals and local bindings replace what the original tree produced
/// inside it. The original tree had read the declaration as an expression, so
/// its calls were attributed to the file and `f() { .. }` was recorded as a call
/// to `f`. Recovered ranges are listed in `recovered_decl_ranges`.
///
/// A charged region that is **not** recovered is replaced by nothing: the same
/// replacement runs with an empty result, so no symbol, call, reference, export,
/// wiring, local binding or scope local the expression reading produced inside
/// it is published (it would be a top-level `m` for the member of a class, and a
/// call to `class`). The region stays in `parse_outcome`'s `error_ranges`, so
/// consumers abstain, and is listed in `decl_gaps`. A region nested inside a
/// recovered one is not separate: the recovered range replaces it, and its own
/// charge goes with it. Where nothing is left charged the outcome is `Clean`.
pub(crate) fn recover(
    extraction: &mut Extraction,
    source: &str,
    lost: &LostScan,
    deadline: Instant,
) {
    if lost.deadline_passed || (lost.regions.is_empty() && !lost.capped) {
        return;
    }
    let Some(_guard) = DepthGuard::enter() else {
        return;
    };
    // Before any inner extraction: the inner call re-arms the thread's budget,
    // which would clear a latch an earlier phase had already set. A file past
    // its deadline is refused by the publication gate, so nothing is settled.
    if extraction_overran(deadline) {
        return;
    }
    let mut recovered: Vec<TextRange> = Vec::new();
    if source.contains(WRAPPER_NAME) {
        extraction.diagnostics.push(
            "kotlin loss recovery skipped: the source spells the recovery wrapper's own name"
                .to_string(),
        );
    } else if !lost.capped {
        let mut attempted = 0usize;
        for region in &lost.regions {
            if !region.top_level || region.has_error {
                continue;
            }
            if attempted >= MAX_RECOVERY_REGIONS {
                extraction.diagnostics.push(format!(
                    "kotlin loss recovery stopped at {MAX_RECOVERY_REGIONS} regions; the \
                     remaining ones stay in error_ranges and decl_gaps"
                ));
                break;
            }
            attempted += 1;
            if Instant::now() >= deadline {
                extraction.diagnostics.push(
                    "kotlin loss recovery stopped at the extraction deadline; the remaining \
                     regions stay in error_ranges and decl_gaps"
                        .to_string(),
                );
                break;
            }
            match recover_region(extraction, source, region, deadline) {
                Ok(()) => recovered.push(region.range.clone()),
                Err(why) => extraction.diagnostics.push(format!(
                    "kotlin declaration at bytes {}..{} not recovered: {why}",
                    region.range.start_byte, region.range.end_byte
                )),
            }
        }
    }
    let within = |inner: &TextRange, outer: &TextRange| {
        outer.start_byte <= inner.start_byte && inner.end_byte <= outer.end_byte
    };
    for region in &lost.regions {
        if recovered.iter().any(|done| within(&region.range, done)) {
            continue;
        }
        Staged::default().apply(extraction, region);
        extraction.decl_gaps.push(region.range.clone());
    }
    if lost.capped {
        // The scan stopped early: any declaration of the file may be missing.
        extraction.decl_gaps.push(TextRange {
            start_byte: 0,
            end_byte: source.len(),
        });
    }
    if let ParseOutcome::Partial {
        error_ranges,
        nameless_ranges,
    } = &mut extraction.parse_outcome
    {
        // The wrapper parse of a recovered range was clean, so a statement
        // charged inside it parsed as the declaration it is.
        error_ranges.retain(|range| !recovered.iter().any(|done| within(range, done)));
        nameless_ranges.retain(|range| !recovered.iter().any(|done| within(range, done)));
        if error_ranges.is_empty() {
            extraction.parse_outcome = ParseOutcome::Clean;
        }
    }
    extraction.recovered_decl_ranges.extend(recovered);
    extraction.recovered_decl_ranges.sort();
    extraction.recovered_decl_ranges.dedup();
    extraction.decl_gaps.sort();
    extraction.decl_gaps.dedup();
    extraction
        .symbols
        .sort_by_key(|symbol| symbol.span.start_byte);
    extraction.calls.sort_by_key(|call| call.span.start_byte);
    extraction
        .scope_receivers
        .sort_by_key(|scope| (scope.body_start, scope.body_end));
    extraction.jvm_decls.sort();
    extraction.jvm_decls.dedup();
    extraction.local_extents.sort();
    extraction.local_extents.dedup();
    // Derived afresh from the settled symbols, by the pass every file goes
    // through: the Kotlin walk publishes no export of its own, so this is the
    // whole of it, and a symbol that is not published cannot have held the slot
    // of one that is.
    extraction.exports.clear();
    crate::treesitter::publish_exports(&extraction.symbols, &mut extraction.exports);
    crate::embedded::reorder_after_merge(extraction);
}

/// Recover one region, or say why not and change nothing.
fn recover_region(
    extraction: &mut Extraction,
    source: &str,
    region: &LostRegion,
    deadline: Instant,
) -> Result<(), String> {
    let (start, end) = (region.range.start_byte, region.range.end_byte);
    let text = source
        .get(start..end)
        .ok_or_else(|| "the region is not on a character boundary".to_string())?;
    let wrapped = format!("{WRAPPER_OPEN}{text}{WRAPPER_CLOSE}");
    let prefix = WRAPPER_OPEN.len();
    let keywords: Vec<(usize, &str)> = region
        .keywords
        .iter()
        .map(|&(offset, word)| (offset - start + prefix, word))
        .collect();
    check_wrapper(&wrapped, &keywords, deadline)?;
    let remaining = deadline.saturating_duration_since(Instant::now());
    if remaining.is_zero() {
        return Err("the extraction budget was exhausted".to_string());
    }
    let inner = crate::treesitter::extract_treesitter_with_budget(
        &extraction.file_path,
        "kotlin",
        &wrapped,
        remaining,
    );
    if !matches!(inner.parse_outcome, ParseOutcome::Clean) {
        return Err(format!(
            "the wrapped text did not extract cleanly: {:?}",
            inner.parse_outcome
        ));
    }
    let staged = Staged::of(inner, &extraction.file_path, start, prefix, text.len())?;
    staged.apply(extraction, region);
    Ok(())
}

/// Parse the wrapped text and confirm it is what recovery assumes.
fn check_wrapper(
    wrapped: &str,
    keywords: &[(usize, &str)],
    deadline: Instant,
) -> Result<(), String> {
    let (_, language) = crate::treesitter::grammar_for("kotlin")
        .ok_or_else(|| "no Kotlin grammar is linked".to_string())?;
    let mut parser = tree_sitter::Parser::new();
    parser
        .set_language(&language)
        .map_err(|_| "the Kotlin grammar failed to load".to_string())?;
    let (tree, cancelled) = crate::treesitter::parse_until(&mut parser, wrapped, deadline);
    let tree = match tree {
        Some(tree) if !cancelled => tree,
        _ => return Err("the wrapped text did not parse within the budget".to_string()),
    };
    // A swallowed statement that is a one-line class (`@Ann(1) class A { val x
    // = 1 }`) reads as an error inside the wrapper's body, which the row 16
    // rewrite repairs the way it repairs the file's own. The inner extraction
    // below repairs the same text the same way, so what is checked here is
    // what it reads. Kept only when fully clean; otherwise the error stands.
    let tree = if tree.root_node().has_error() {
        match crate::kotlinsemi::repair(&mut parser, tree, wrapped, deadline) {
            crate::kotlinsemi::Repair::Repaired { tree, .. } => tree,
            crate::kotlinsemi::Repair::Unchanged { .. } => {
                return Err("the wrapped text has a syntax error".to_string());
            }
        }
    } else {
        tree
    };
    let root = tree.root_node();
    let mut cursor = root.walk();
    let mut top = root.named_children(&mut cursor);
    let (Some(class), None) = (top.next(), top.next()) else {
        return Err("the wrapped text is not exactly one class".to_string());
    };
    if !is_recovery_wrapper(class, wrapped) || class.end_byte() < wrapped.trim_end().len() {
        return Err("the wrapped text closed the wrapper early".to_string());
    }
    let mut inner = class.walk();
    let body = class
        .named_children(&mut inner)
        .find(|child| child.kind() == "class_body")
        .ok_or_else(|| "the wrapper has no body".to_string())?;
    let mut members = body.walk();
    let mut declarations: Vec<(usize, usize)> = Vec::new();
    for member in body.named_children(&mut members) {
        if DECLARATION_KINDS.contains(&member.kind()) {
            declarations.push((member.start_byte(), member.end_byte()));
        } else if !member.kind().ends_with("comment") {
            return Err(format!(
                "the wrapper holds a `{}`, not a declaration",
                member.kind()
            ));
        }
    }
    if declarations.is_empty() {
        return Err("the wrapper holds no declaration".to_string());
    }
    for &(at, word) in keywords {
        let token = root.descendant_for_byte_range(at, at + word.len());
        let is_the_keyword = !word.is_empty()
            && token.is_some_and(|token| {
                !token.is_named()
                    && token.kind() == word
                    && token.start_byte() == at
                    && token.end_byte() == at + word.len()
            });
        let owned = declarations
            .iter()
            .any(|&(from, to)| from <= at && at + word.len() <= to);
        if !is_the_keyword || !owned {
            return Err(format!(
                "the keyword `{word}` at wrapper byte {at} is not a declaration's own keyword"
            ));
        }
    }
    let scan = detect(root, wrapped);
    if scan.capped || scan.deadline_passed || !scan.regions.is_empty() {
        return Err("the wrapped text still holds a swallowed statement".to_string());
    }
    Ok(())
}

/// What one recovered region contributes, in the file's own byte coordinates.
/// The default is the empty replacement an unrecovered region gets.
#[derive(Default)]
struct Staged {
    symbols: Vec<ExtractedSymbol>,
    calls: Vec<ExtractedCall>,
    references: Vec<ExtractedReference>,
    wiring: Vec<WiringAnnotation>,
    scope_locals: Vec<(String, String)>,
    local_bindings: Vec<LocalBinding>,
    scope_receivers: Vec<ScopeReceiver>,
    jvm_decls: Vec<JvmDeclFact>,
    local_extents: Vec<LocalExtent>,
    diagnostics: Vec<String>,
}

impl Staged {
    /// Shift an inner extraction of `{WRAPPER_OPEN}{text}{WRAPPER_CLOSE}` back
    /// to the file. Any span outside the text, or any trace of the wrapper's
    /// name, means the re-parse said something about bytes that are not the
    /// region's and nothing is merged.
    fn of(
        inner: Extraction,
        file_path: &str,
        start: usize,
        prefix: usize,
        len: usize,
    ) -> Result<Self, String> {
        let shift = |byte: usize| -> Result<usize, String> {
            if byte >= prefix && byte <= prefix + len {
                Ok(byte - prefix + start)
            } else {
                Err(format!(
                    "an inner span at byte {byte} lies outside the region"
                ))
            }
        };
        let shift_span = |span: &Span| -> Result<Span, String> {
            Ok(Span {
                start_byte: shift(span.start_byte)?,
                end_byte: shift(span.end_byte)?,
            })
        };
        let wrapper_symbol = format!("{file_path}::{WRAPPER_NAME}");
        let mut staged = Staged {
            symbols: Vec::new(),
            calls: Vec::new(),
            references: Vec::new(),
            wiring: Vec::new(),
            scope_locals: inner.scope_locals,
            local_bindings: Vec::new(),
            scope_receivers: Vec::new(),
            jvm_decls: Vec::new(),
            local_extents: Vec::new(),
            diagnostics: inner.diagnostics,
        };
        for mut symbol in inner.symbols {
            if symbol.kind == SymbolKind::File {
                continue;
            }
            symbol.span = shift_span(&symbol.span)?;
            staged.symbols.push(symbol);
        }
        for mut call in inner.calls {
            call.span = shift_span(&call.span)?;
            staged.calls.push(call);
        }
        for mut reference in inner.references {
            // A property's declared type is filed under its owning class, and
            // the wrapper is a class the source does not have. A top-level
            // property is owned by nothing, and emits no such reference.
            if reference.enclosing_symbol.as_deref() == Some(wrapper_symbol.as_str()) {
                continue;
            }
            reference.span = shift_span(&reference.span)?;
            staged.references.push(reference);
        }
        staged.wiring = inner
            .wiring
            .into_iter()
            .filter(|annotation| annotation.target_symbol != file_path)
            .collect();
        for mut binding in inner.local_bindings {
            binding.start_byte = shift(binding.start_byte)?;
            staged.local_bindings.push(binding);
        }
        for mut receiver in inner.scope_receivers {
            receiver.body_start = shift(receiver.body_start)?;
            receiver.body_end = shift(receiver.body_end)?;
            staged.scope_receivers.push(receiver);
        }
        let shift_range = |range: &TextRange| -> Result<TextRange, String> {
            Ok(TextRange {
                start_byte: shift(range.start_byte)?,
                end_byte: shift(range.end_byte)?,
            })
        };
        for mut fact in inner.jvm_decls {
            fact.start_byte = shift(fact.start_byte)?;
            fact.header = fact.header.as_ref().map(shift_range).transpose()?;
            fact.body = fact.body.as_ref().map(shift_range).transpose()?;
            staged.jvm_decls.push(fact);
        }
        for mut extent in inner.local_extents {
            extent.extent = shift_range(&extent.extent)?;
            staged.local_extents.push(extent);
        }
        // Field by field, so a refusal names what leaked.
        fn leak<T: serde::Serialize>(label: &str, items: &[T]) -> Result<(), String> {
            for item in items {
                let text = serde_json::to_string(item).map_err(|error| {
                    format!("the recovered {label} could not be checked: {error}")
                })?;
                if text.contains(WRAPPER_NAME) {
                    return Err(format!(
                        "the recovered {label} still name the recovery wrapper: {text}"
                    ));
                }
            }
            Ok(())
        }
        leak("symbols", &staged.symbols)?;
        leak("calls", &staged.calls)?;
        leak("references", &staged.references)?;
        leak("wiring", &staged.wiring)?;
        leak("scope locals", &staged.scope_locals)?;
        leak("local bindings", &staged.local_bindings)?;
        leak("scope receivers", &staged.scope_receivers)?;
        leak("jvm decls", &staged.jvm_decls)?;
        leak("local extents", &staged.local_extents)?;
        Ok(staged)
    }

    /// Replace what the original tree produced inside `region` with these.
    ///
    /// Only by span, and only the rows that have one. What carries no span of
    /// its own is not patched here: the walks that derive it (declarations and
    /// their wiring, scope locals, scope receivers) already left the region
    /// out, so the outer tree published nothing under a name a phantom could
    /// share with a real symbol, and `exports` is derived afresh from the
    /// settled symbols by [`recover`]. Every table of an `Extraction` was
    /// considered:
    ///
    /// * `symbols`, `calls`, `references`, `local_bindings`, `scope_receivers`,
    ///   `jvm_decls` and `local_extents` have spans or start bytes and are
    ///   replaced by span;
    /// * `wiring` and `scope_locals` are keyed by names, never emitted inside a
    ///   region, and contributed by the re-parse for a recovered one;
    /// * `exports` is derived from `symbols`;
    /// * `imports`, `routes`, `literals`, `js_objects`, `rust_*`, `go_*` and
    ///   `dynamic_dispatch` belong to other languages or to nothing a region
    ///   holds (Kotlin imports are header lines, and Kotlin has no literal
    ///   decoder yet; the day it has, a literal inside an unrecovered region
    ///   must be left out by the walk that reads it);
    /// * `package_scopes` are file-level headers, not region contents;
    /// * `decl_gaps` and `recovered_decl_ranges` are written by [`recover`].
    fn apply(self, extraction: &mut Extraction, region: &LostRegion) {
        let (start, end) = (region.range.start_byte, region.range.end_byte);
        let inside = |span: &Span| span.start_byte >= start && span.end_byte <= end;
        extraction
            .symbols
            .retain(|symbol| symbol.kind == SymbolKind::File || !inside(&symbol.span));
        extraction.calls.retain(|call| !inside(&call.span));
        extraction
            .references
            .retain(|reference| !inside(&reference.span));
        extraction
            .local_bindings
            .retain(|binding| binding.start_byte < start || binding.start_byte >= end);
        extraction
            .scope_receivers
            .retain(|receiver| receiver.body_start < start || receiver.body_end > end);
        extraction
            .jvm_decls
            .retain(|fact| fact.start_byte < start || fact.start_byte >= end);
        extraction
            .local_extents
            .retain(|extent| extent.extent.start_byte < start || extent.extent.end_byte > end);
        extraction.symbols.extend(self.symbols);
        extraction.calls.extend(self.calls);
        extraction.references.extend(self.references);
        extraction.wiring.extend(self.wiring);
        extraction.scope_locals.extend(self.scope_locals);
        extraction.local_bindings.extend(self.local_bindings);
        extraction.scope_receivers.extend(self.scope_receivers);
        extraction.jvm_decls.extend(self.jvm_decls);
        extraction.local_extents.extend(self.local_extents);
        extraction.diagnostics.extend(self.diagnostics);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scan_with(source: &str, cap: Option<usize>) -> LostScan {
        let (_, language) = crate::treesitter::grammar_for("kotlin").expect("kotlin grammar");
        let mut parser = tree_sitter::Parser::new();
        parser.set_language(&language).expect("load grammar");
        let tree = parser.parse(source, None).expect("tree");
        match cap {
            Some(cap) => detect_capped(tree.root_node(), source, cap),
            None => detect(tree.root_node(), source),
        }
    }

    fn regions(source: &str) -> Vec<&str> {
        scan_with(source, None)
            .regions
            .iter()
            .map(|region| &source[region.range.start_byte..region.range.end_byte])
            .collect()
    }

    /// The two annotations that make the grammar read a declaration as an
    /// expression.
    const STACK: &str = "@Preview\n@Composable\n";

    #[test]
    fn every_declaration_kind_behind_a_stack_is_a_region() {
        for declaration in [
            "private fun f() {}",
            "private fun f() = 1",
            "private class Q {}",
            "private object O {}",
            "private interface I {}",
            "internal class Q",
            "private val y = 1",
            "private var y = 1",
            "private typealias T = Int",
            "open class O",
            "sealed interface S",
            "enum class E { X }",
            "abstract class AC {}",
            "class Plain {}",
        ] {
            let source = format!("{STACK}{declaration}\nfun after() {{}}\n");
            let found = regions(&source);
            assert_eq!(
                found,
                vec![format!("{STACK}{declaration}").as_str()],
                "{declaration}"
            );
        }
    }

    #[test]
    fn a_region_is_top_level_only_at_the_root() {
        let top = scan_with(
            &format!("{STACK}private fun f() {{}}\nfun after() {{}}\n"),
            None,
        );
        assert_eq!(top.regions.len(), 1);
        assert!(top.regions[0].top_level);
        let nested = scan_with(
            &format!("fun g() {{\n{STACK}private fun h() {{}}\n}}\n"),
            None,
        );
        assert_eq!(nested.regions.len(), 1);
        assert!(!nested.regions[0].top_level);
    }

    #[test]
    fn a_region_lists_the_start_of_each_keyword_it_holds() {
        let source = format!("{STACK}private fun f() {{}}\nfun after() {{}}\n");
        let scan = scan_with(&source, None);
        assert_eq!(
            scan.regions[0].keywords,
            vec![(source.find("fun").unwrap(), "fun")]
        );
    }

    #[test]
    fn what_parses_as_a_declaration_is_not_a_region() {
        for source in [
            "private val X = 1\n@Preview\nprivate fun f() {}\nfun after() {}\n",
            "@Preview\n@Composable\nfun f() {}\n",
            "@Preview\n@Composable\nprivate fun f(): Int = 1\n",
            "@Preview\n@Composable\nprivate suspend fun s() {}\n",
            "@A\n@B\ndata class D(val a: Int)\n",
            "class K {\n    @A\n    @B\n    private fun m() {}\n}\n",
            "fun `fun`() {}\nval `class` = 1\n",
            "val s = \"private fun f() {}\"\n// @A @B private fun f() {}\n/* class X */\n",
            "val k = String::class.java\nfun <T : Any> name(c: kotlin.reflect.KClass<T>) = T::class\n",
            "interface Cb {\n    fun call()\n}\nval o = object {\n    val x = 1\n}\n",
        ] {
            assert!(regions(source).is_empty(), "{source}");
        }
    }

    #[test]
    fn a_keyword_inside_an_error_is_left_to_the_error_range() {
        // `class` as an expression after a stray `)`; the ERROR node is charged
        // by `parse_outcome_of`, so the detector adds nothing for it.
        let source = "fun f() {\n  ) class X {}\n}\n";
        let scan = scan_with(source, None);
        assert!(scan.regions.is_empty(), "{:?}", scan.regions);
    }

    #[test]
    fn a_walk_that_hits_the_node_cap_is_reported_not_clean() {
        let source = format!("{STACK}private fun f() {{}}\nfun after() {{}}\n");
        let scan = scan_with(&source, Some(3));
        assert!(scan.capped);
        assert!(
            scan.regions.is_empty(),
            "a capped walk found nothing, and says it is capped"
        );
    }
}
