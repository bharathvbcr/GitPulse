//! Script regions embedded in a template language.
//!
//! **The problem.** A `.svelte`, `.vue`, `.astro` or `.liquid` file is parsed
//! by its own grammar, and every one of those grammars models the *template*.
//! The code lives inside a `<script>` element, and each grammar hands that back
//! as one opaque leaf — `raw_text` in Svelte, Vue and Astro,
//! `frontmatter_js_block` for an Astro frontmatter fence, `js_content` for a
//! Liquid `{% javascript %}` tag, and, in Liquid, not at all: everything that
//! is not a Liquid tag arrives as a single `template_content` node whose text
//! happens to contain HTML.
//!
//! Measured before this module existed, on fixtures whose script blocks each
//! declare two functions, import a name, and call one function from the other:
//!
//! | file | symbols | imports | calls | exports |
//! |------|---------|---------|-------|---------|
//! | `.svelte` | 1 (the `File` node) | 0 | 0 | 1 (the file itself) |
//! | `.vue`    | 1 | 0 | 0 | 1 |
//! | `.astro`  | 1 | 0 | 0 | 1 |
//! | `.liquid` | 1 | 0 | 0 | 1 |
//!
//! and each reported `ParseOutcome::Clean` — a complete-looking answer over a
//! file whose entire code half had never been read.
//!
//! **What this does.** It locates those regions in the outer parse tree, routes
//! each region's text back through [`crate::treesitter::extract_treesitter_with_budget`]
//! under the *outer file's own path*, and shifts every span the inner
//! extraction produced by the region's byte offset. Naming the region with the
//! real path is what keeps `qualified_name`, `parent_symbol` and
//! `caller_symbol` in one namespace: a symbol declared in a script block is
//! `src/Summary.svelte::renderSummary`, which is exactly the name the call to
//! it records, so the resolver joins them without a second naming rule.
//! Shifting the spans is what keeps them true — a span is a byte range into the
//! file on disk, and every consumer that renders a symbol slices the file with
//! it, so an unshifted span is not a coarser answer but a wrong one.
//!
//! **Which language a region is.** Read from the region's own evidence — a
//! `lang=` attribute, else a `type=` attribute, else the structure the outer
//! grammar imposes (an Astro frontmatter fence is TypeScript; a Liquid
//! `{% javascript %}` tag is JavaScript) — and then checked against
//! [`crate::languages::LanguageSpec::embedded`], which is the single authority
//! for what may appear inside a given outer language. The **default**, for a
//! region that names nothing, is the first entry of that list this build can
//! route to a grammar: `typescript` for Svelte, Vue and Astro, `javascript` for
//! Liquid, both read off the registry rather than restated here.
//!
//! **What it deliberately does not do.** `css` and `html` appear in those
//! permitted lists and no grammar for either is linked in this workspace, so
//! they resolve to nothing routable and `<style>` regions are not touched. That
//! is a gap, not a claim: nothing here reports anything about a stylesheet.
//!
//! **Honesty.** A located region that is *not extracted* — an unparseable
//! script, a language the registry does not permit there, a budget that ran out
//! — contributes its byte range to [`ParseOutcome::Partial`], so the file can
//! never report `Clean` over bytes nobody read. The range rides on the outcome
//! rather than on `diagnostics` because `Extraction::for_durable_store` clears
//! diagnostics before the payload reaches the store; the human-readable reason
//! is added there as well, but the durable record is the range. A region that
//! is *not script at all* — `type="application/json"`, `type="importmap"` —
//! contributes nothing and is not an error: recognising that it is not
//! JavaScript is a correct answer, not a failure to parse one.

use std::cell::Cell;
use std::time::Instant;

use tree_sitter::Node;

use crate::languages::LANGUAGE_SPECS;
use crate::model::{Extraction, ParseOutcome, Span, SymbolKind, TextRange};

/// Most embedded regions read from one file.
///
/// Far above any real component — the largest shape in the wild is a handful of
/// `<script>` elements per page — and low enough that a generated or hostile
/// template cannot turn one file into thousands of nested parses. Regions past
/// the cap are **not** silently dropped: their combined byte range is recorded
/// as unparsed, so the file reports `Partial` rather than a clean answer over
/// text nothing read.
const MAX_EMBEDDED_REGIONS: usize = 256;

thread_local! {
    /// Whether this thread is already inside an embedded extraction.
    ///
    /// A script region is not itself scanned for embedded regions. Today the
    /// registry makes that structurally impossible — `typescript` and
    /// `javascript` both declare `embedded: &[]` — but the guard is what stops
    /// a future registry edit from turning a one-line change into unbounded
    /// recursion, and it costs one thread-local read per file.
    static INSIDE_EMBEDDED: Cell<bool> = const { Cell::new(false) };
}

/// Sets [`INSIDE_EMBEDDED`] for its lifetime and clears it on drop, so a panic
/// inside an inner extraction cannot leave a rayon worker poisoned for every
/// file it handles afterwards.
struct DepthGuard;

impl DepthGuard {
    /// `None` when this thread is already inside an embedded extraction.
    fn enter() -> Option<Self> {
        INSIDE_EMBEDDED.with(|inside| {
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
        INSIDE_EMBEDDED.with(|inside| inside.set(false));
    }
}

/// The languages the registry permits inside `outer_grammar`, in declaration
/// order, narrowed to those this build can route to a linked grammar.
///
/// The registry is the authority twice over: it says *which* languages may be
/// embedded ([`crate::languages::LanguageSpec::embedded`]), and it is where the
/// name is resolved to a grammar. A name no `LanguageSpec` claims — `css`,
/// `html` — resolves to nothing and is dropped here rather than being restated
/// in a second list that could drift out of step.
///
/// Order is meaningful: the first entry is the default for a region that names
/// no language of its own.
pub(crate) fn permitted_embedded_languages(outer_grammar: &str) -> Vec<&'static str> {
    let mut permitted: Vec<&'static str> = Vec::new();
    for spec in LANGUAGE_SPECS
        .iter()
        .filter(|spec| spec.grammar == outer_grammar)
    {
        for name in spec.embedded {
            if !permitted.contains(name) && is_routable(name) {
                permitted.push(name);
            }
        }
    }
    permitted
}

/// Whether `name` is a registry grammar this build actually links.
fn is_routable(name: &str) -> bool {
    LANGUAGE_SPECS.iter().any(|spec| spec.grammar == name)
        && !crate::cache::base_grammar_identity(name).starts_with("unavailable:")
}

/// What a region's own evidence asks for.
#[derive(Debug, PartialEq, Eq)]
enum Requested {
    /// The region names no language; the outer language's default applies.
    Default,
    /// The region names this registry language.
    Named(&'static str),
    /// The region is not script: a `type` this build recognises as data or a
    /// template rather than code (`application/json`, `importmap`,
    /// `text/x-template`, …). Contributes nothing and is not an error.
    NotScript,
    /// The region names something no `LanguageSpec` claims — a `lang=` naming a
    /// preprocessor this build cannot parse. Reported, never guessed at.
    Unrecognised(String),
}

/// Map a region's `lang` / `type` evidence to a registry language name.
///
/// `lang` wins over `type`: it is the framework's own directive for what the
/// block is written in, while `type` is the HTML attribute browsers dispatch
/// on. Values are compared lowercased.
fn requested_language(attributes: &[(String, String)]) -> Requested {
    let value = |wanted: &str| {
        attributes
            .iter()
            .find(|(name, _)| name == wanted)
            .map(|(_, value)| value.trim().to_ascii_lowercase())
    };

    if let Some(lang) = value("lang").filter(|lang| !lang.is_empty()) {
        return match lang.as_str() {
            "ts" | "typescript" => Requested::Named("typescript"),
            // The registry files `.jsx` under the JavaScript spec, and
            // tree-sitter-javascript parses JSX, so `lang="jsx"` is JavaScript
            // by the registry's own account rather than by a rule invented here.
            "js" | "javascript" | "jsx" => Requested::Named("javascript"),
            "tsx" => Requested::Named("tsx"),
            other => Requested::Unrecognised(other.to_string()),
        };
    }

    if let Some(kind) = value("type").filter(|kind| !kind.is_empty()) {
        return match kind.as_str() {
            "module"
            | "text/javascript"
            | "application/javascript"
            | "text/ecmascript"
            | "application/ecmascript"
            | "text/babel" => Requested::Named("javascript"),
            "text/typescript" | "application/typescript" => Requested::Named("typescript"),
            // Anything else a `type` names is not executed as script by a
            // browser either — JSON payloads, import maps, client-side
            // templates. Treating an unknown `type` as JavaScript would parse a
            // JSON blob as code and publish whatever fell out.
            _ => Requested::NotScript,
        };
    }

    Requested::Default
}

/// One embedded region located in the outer file.
#[derive(Debug, Clone)]
struct Region {
    /// Byte range of the region's *content* in the outer file.
    content: Span,
    /// Byte range of the whole element, recorded when the region is located but
    /// not extracted so the unparsed range names the construct, not just its
    /// body.
    element: Span,
    /// The registry language the outer grammar's own structure fixes this
    /// region to, when it fixes one.
    fixed: Option<&'static str>,
    /// `lang` / `type` evidence read off the region's start tag.
    attributes: Vec<(String, String)>,
}

/// Everything [`collect_regions`] found, including what it refused to keep.
struct Located {
    regions: Vec<Region>,
    /// Regions past [`MAX_EMBEDDED_REGIONS`], and the byte range they span.
    /// Computed here, by the function that does the truncating, so the count
    /// can never disagree with the list it describes.
    dropped: usize,
    dropped_span: Option<Span>,
    /// `<script` openers in opaque template text with no matching `</script>`
    /// in the same run. See [`scan_script_elements`].
    unterminated: Vec<Span>,
}

/// Locate every embedded region in the outer parse tree, in document order.
///
/// Pre-order, with an explicit stack rather than recursion, for the reason
/// `walk_tree` uses one: adversarially deep syntax must not overflow the stack.
/// The node kinds are the ones the four linked template grammars actually
/// produce, measured against each grammar:
///
/// * `script_element` → `raw_text` (Svelte, Vue, Astro)
/// * `frontmatter` → `frontmatter_js_block` (Astro; always TypeScript)
/// * `javascript_statement` → `js_content` (Liquid `{% javascript %}`)
/// * `template_content` → text scan (Liquid, which does not model HTML)
fn collect_regions(root: Node, source: &str) -> Located {
    let mut regions: Vec<Region> = Vec::new();
    let mut unterminated: Vec<Span> = Vec::new();
    let mut stack: Vec<Node> = vec![root];

    while let Some(node) = stack.pop() {
        match node.kind() {
            "script_element" => {
                let attributes = child_of_kind(node, "start_tag")
                    .map(|tag| attributes_of_start_tag(&text_of(tag, source)))
                    .unwrap_or_default();
                // A `<script src="…" />` with no body has no `raw_text` child
                // and no bytes to read: nothing was skipped, so nothing is
                // reported.
                if let Some(body) = child_of_kind(node, "raw_text") {
                    regions.push(Region {
                        content: span_of(body),
                        element: span_of(node),
                        fixed: None,
                        attributes,
                    });
                }
                continue;
            }
            "frontmatter" => {
                if let Some(body) = child_of_kind(node, "frontmatter_js_block") {
                    regions.push(Region {
                        content: span_of(body),
                        element: span_of(node),
                        // Astro compiles the frontmatter fence as TypeScript;
                        // the fence carries no attribute to say so.
                        fixed: Some("typescript"),
                        attributes: Vec::new(),
                    });
                }
                continue;
            }
            "javascript_statement" => {
                if let Some(body) = child_of_kind(node, "js_content") {
                    regions.push(Region {
                        content: span_of(body),
                        element: span_of(node),
                        fixed: Some("javascript"),
                        attributes: Vec::new(),
                    });
                }
                continue;
            }
            "template_content" => {
                let span = span_of(node);
                if let Some(text) = source.get(span.start_byte..span.end_byte) {
                    scan_script_elements(text, span.start_byte, &mut regions, &mut unterminated);
                }
                continue;
            }
            _ => {}
        }

        // Reverse so the pop order is document order.
        let mut cursor = node.walk();
        let children: Vec<Node> = node.children(&mut cursor).collect();
        stack.extend(children.into_iter().rev());
    }

    // Document order regardless of how the walk reached them, so two builds of
    // the same file emit identical payloads (R4).
    regions.sort_by_key(|region| (region.content.start_byte, region.content.end_byte));
    unterminated.sort_by_key(|span| (span.start_byte, span.end_byte));

    let dropped = regions.len().saturating_sub(MAX_EMBEDDED_REGIONS);
    let dropped_span = (dropped > 0)
        .then(|| {
            let tail = regions.get(MAX_EMBEDDED_REGIONS..)?;
            let start = tail.first()?.element.start_byte;
            let end = tail.last()?.element.end_byte;
            Some(Span {
                start_byte: start,
                end_byte: end,
            })
        })
        .flatten();
    regions.truncate(MAX_EMBEDDED_REGIONS);

    Located {
        regions,
        dropped,
        dropped_span,
        unterminated,
    }
}

fn child_of_kind<'tree>(node: Node<'tree>, kind: &str) -> Option<Node<'tree>> {
    let mut cursor = node.walk();
    let found = node
        .children(&mut cursor)
        .find(|child| child.kind() == kind);
    found
}

fn span_of(node: Node) -> Span {
    Span {
        start_byte: node.start_byte(),
        end_byte: node.end_byte(),
    }
}

fn text_of(node: Node, source: &str) -> String {
    source
        .get(node.start_byte()..node.end_byte())
        .unwrap_or_default()
        .to_string()
}

/// Attributes of a start tag, read from the tag's own text.
///
/// One reader for both paths: Svelte, Vue and Astro hand over a `start_tag`
/// node whose text is exactly this, and the Liquid scan below has only text to
/// work with. Names are lowercased; values are returned unquoted and untrimmed
/// of nothing else.
fn attributes_of_start_tag(tag: &str) -> Vec<(String, String)> {
    let bytes = tag.as_bytes();
    let mut attributes = Vec::new();
    // Past `<script`; a tag that does not start that way contributes nothing.
    let mut at = match tag
        .get(..7)
        .filter(|head| head.eq_ignore_ascii_case("<script"))
    {
        Some(head) => head.len(),
        None => return attributes,
    };

    while at < bytes.len() {
        while at < bytes.len() && bytes[at].is_ascii_whitespace() {
            at += 1;
        }
        let name_start = at;
        while at < bytes.len()
            && (bytes[at].is_ascii_alphanumeric() || matches!(bytes[at], b'-' | b'_' | b':' | b'.'))
        {
            at += 1;
        }
        if at == name_start {
            // Not an attribute name: `>`, `/`, or something this reader does
            // not model. Step over it rather than spinning.
            at += 1;
            continue;
        }
        let name = tag
            .get(name_start..at)
            .unwrap_or_default()
            .to_ascii_lowercase();

        let mut after = at;
        while after < bytes.len() && bytes[after].is_ascii_whitespace() {
            after += 1;
        }
        if bytes.get(after) != Some(&b'=') {
            // A valueless attribute — `setup`, `defer`, `scoped`.
            attributes.push((name, String::new()));
            continue;
        }
        after += 1;
        while after < bytes.len() && bytes[after].is_ascii_whitespace() {
            after += 1;
        }
        let value_start;
        let value_end;
        match bytes.get(after) {
            Some(quote @ (b'"' | b'\'')) => {
                let quote = *quote;
                value_start = after + 1;
                let mut end = value_start;
                while end < bytes.len() && bytes[end] != quote {
                    end += 1;
                }
                value_end = end;
                at = (end + 1).min(bytes.len());
            }
            Some(_) => {
                value_start = after;
                let mut end = after;
                while end < bytes.len()
                    && !bytes[end].is_ascii_whitespace()
                    && !matches!(bytes[end], b'>' | b'/')
                {
                    end += 1;
                }
                value_end = end;
                at = end;
            }
            None => break,
        }
        attributes.push((
            name,
            tag.get(value_start..value_end)
                .unwrap_or_default()
                .to_string(),
        ));
    }

    attributes
}

/// Locate complete `<script …>…</script>` elements in a run of opaque template
/// text, appending them to `regions` with `base` added to every offset.
///
/// Needed because the Liquid grammar does not model HTML at all: a Shopify
/// section's markup, `<script>` included, arrives as one `template_content`
/// leaf. Without this pass a `.liquid` file recovers nothing but the
/// `{% javascript %}` tag, which almost no theme uses.
///
/// A `<script` with no `</script>` **in the same run of text** is recorded in
/// `unterminated` rather than guessed at. That case is not hypothetical: a
/// Liquid tag inside a script body — `var cart = {{ cart | json }};` — splits
/// the element across two `template_content` nodes, and the honest answer is
/// that this build cannot read that block, not a body that stops at the tag.
fn scan_script_elements(
    text: &str,
    base: usize,
    regions: &mut Vec<Region>,
    unterminated: &mut Vec<Span>,
) {
    const OPEN: &str = "<script";
    const CLOSE: &str = "</script";
    let bytes = text.as_bytes();
    let mut at = 0usize;

    while let Some(open) = find_ascii_ci(text, at, OPEN) {
        let after_name = open + OPEN.len();
        // `<scripting>` is not a script element.
        match bytes.get(after_name) {
            Some(byte) if byte.is_ascii_whitespace() || matches!(byte, b'>' | b'/') => {}
            _ => {
                at = after_name;
                continue;
            }
        }
        let Some(tag_end) = start_tag_end(text, after_name) else {
            unterminated.push(Span {
                start_byte: base + open,
                end_byte: base + text.len(),
            });
            break;
        };
        // `<script … />` closes itself and has no body.
        if tag_end > 0 && bytes.get(tag_end - 1) == Some(&b'/') {
            at = tag_end + 1;
            continue;
        }
        let content_start = tag_end + 1;
        let Some(close) = find_ascii_ci(text, content_start, CLOSE) else {
            unterminated.push(Span {
                start_byte: base + open,
                end_byte: base + text.len(),
            });
            break;
        };
        let Some(close_end) = text
            .get(close..)
            .and_then(|rest| rest.find('>'))
            .map(|offset| close + offset + 1)
        else {
            unterminated.push(Span {
                start_byte: base + open,
                end_byte: base + text.len(),
            });
            break;
        };

        regions.push(Region {
            content: Span {
                start_byte: base + content_start,
                end_byte: base + close,
            },
            element: Span {
                start_byte: base + open,
                end_byte: base + close_end,
            },
            fixed: None,
            attributes: attributes_of_start_tag(text.get(open..content_start).unwrap_or_default()),
        });
        at = close_end;
    }
}

/// Index of the start tag's closing `>`, starting from `from`, ignoring one
/// inside a quoted attribute value.
///
/// Byte-wise is safe: every byte of a multi-byte UTF-8 sequence is `>= 0x80`,
/// so it can never be mistaken for `"`, `'` or `>`, and every index this
/// returns is therefore a character boundary.
fn start_tag_end(text: &str, from: usize) -> Option<usize> {
    let bytes = text.as_bytes();
    let mut at = from;
    let mut quote: Option<u8> = None;
    while at < bytes.len() {
        let byte = bytes[at];
        match quote {
            Some(open) if byte == open => quote = None,
            Some(_) => {}
            None if byte == b'"' || byte == b'\'' => quote = Some(byte),
            None if byte == b'>' => return Some(at),
            None => {}
        }
        at += 1;
    }
    None
}

/// Case-insensitive ASCII substring search. `needle` must be lowercase ASCII.
fn find_ascii_ci(haystack: &str, from: usize, needle: &str) -> Option<usize> {
    let haystack = haystack.as_bytes();
    let needle = needle.as_bytes();
    if needle.is_empty() || from >= haystack.len() {
        return None;
    }
    let last = haystack.len().checked_sub(needle.len())?;
    (from..=last).find(|start| {
        haystack[*start..*start + needle.len()]
            .iter()
            .zip(needle)
            .all(|(found, wanted)| found.eq_ignore_ascii_case(wanted))
    })
}

/// What to do with one located region.
enum Choice {
    /// Extract it with this registry language.
    Route(&'static str),
    /// Not a script region. Contributes nothing, and is not an error.
    Skip,
    /// Located but not extracted, for this reason. Its byte range is recorded
    /// as unparsed.
    Unparsed(String),
}

fn choose_language(region: &Region, permitted: &[&'static str], default: &'static str) -> Choice {
    let requested = match region.fixed {
        Some(fixed) => Requested::Named(fixed),
        None => requested_language(&region.attributes),
    };
    match requested {
        Requested::Default => Choice::Route(default),
        Requested::Named(name) if permitted.contains(&name) => Choice::Route(name),
        Requested::Named(name) => Choice::Unparsed(format!(
            "embedded script declares language {name:?}, which the language registry does not \
             permit here (permitted: {permitted:?}); the region was not parsed"
        )),
        Requested::NotScript => Choice::Skip,
        Requested::Unrecognised(raw) => Choice::Unparsed(format!(
            "embedded script declares lang={raw:?}, which names no language in the registry; the \
             region was not parsed"
        )),
    }
}

/// Extract the script regions of `source` and merge them into `extraction`.
///
/// `root` is the outer file's parse tree, `outer_grammar` the grammar that
/// produced it, and `deadline` the instant the whole file's extraction must
/// stop by — shared with the outer walk, so a template with many script blocks
/// cannot spend more than one file's budget in total.
///
/// Called after the outer extraction is fully assembled, including
/// `collect_scope_locals`, because the inner parses clear the per-scope local
/// cache the outer walk warmed.
pub(crate) fn merge_embedded_scripts(
    extraction: &mut Extraction,
    root: Node,
    source: &str,
    outer_grammar: &str,
    deadline: Instant,
) {
    let permitted = permitted_embedded_languages(outer_grammar);
    let Some(default) = permitted.first().copied() else {
        return;
    };
    let Some(_guard) = DepthGuard::enter() else {
        return;
    };

    let located = collect_regions(root, source);
    let mut unparsed: Vec<TextRange> = Vec::new();
    let mut diagnostics: Vec<String> = Vec::new();

    if located.dropped > 0 {
        diagnostics.push(format!(
            "{} embedded script region(s) located, {MAX_EMBEDDED_REGIONS} extracted, {} past the \
             cap were not parsed",
            located.dropped + MAX_EMBEDDED_REGIONS,
            located.dropped
        ));
        if let Some(span) = located.dropped_span {
            unparsed.push(TextRange {
                start_byte: span.start_byte,
                end_byte: span.end_byte,
            });
        }
    }
    for span in &located.unterminated {
        diagnostics.push(format!(
            "unterminated <script> at byte {}: no </script> in the same run of template text, so \
             the block was not parsed",
            span.start_byte
        ));
        unparsed.push(TextRange {
            start_byte: span.start_byte,
            end_byte: span.end_byte,
        });
    }

    for region in &located.regions {
        let language = match choose_language(region, &permitted, default) {
            Choice::Route(language) => language,
            Choice::Skip => continue,
            Choice::Unparsed(reason) => {
                diagnostics.push(reason);
                unparsed.push(TextRange {
                    start_byte: region.element.start_byte,
                    end_byte: region.element.end_byte,
                });
                continue;
            }
        };

        let Some(text) = source.get(region.content.start_byte..region.content.end_byte) else {
            diagnostics.push(format!(
                "embedded script at bytes {}..{} is not a character boundary of the source and \
                 was not parsed",
                region.content.start_byte, region.content.end_byte
            ));
            unparsed.push(TextRange {
                start_byte: region.element.start_byte,
                end_byte: region.element.end_byte,
            });
            continue;
        };
        // An empty or whitespace-only block declares nothing. That is a
        // complete answer, not a failure to parse one.
        if text.trim().is_empty() {
            continue;
        }

        let remaining = deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            diagnostics.push(format!(
                "extraction budget was exhausted before the embedded script at bytes {}..{}; the \
                 region was not parsed",
                region.content.start_byte, region.content.end_byte
            ));
            unparsed.push(TextRange {
                start_byte: region.element.start_byte,
                end_byte: region.element.end_byte,
            });
            continue;
        }

        let inner = crate::treesitter::extract_treesitter_with_budget(
            &extraction.file_path,
            language,
            text,
            remaining,
        );

        match &inner.parse_outcome {
            ParseOutcome::Clean => {}
            ParseOutcome::Partial { error_ranges } => {
                for range in error_ranges {
                    unparsed.push(TextRange {
                        start_byte: region.content.start_byte.saturating_add(range.start_byte),
                        end_byte: region.content.start_byte.saturating_add(range.end_byte),
                    });
                }
            }
            ParseOutcome::Failed { reason } => {
                // Nothing is merged: a failed inner extraction carries only its
                // own `File` node, which is dropped anyway, and publishing a
                // clean-looking empty result over unread bytes is the shape
                // this codebase treats as worse than a visible failure.
                diagnostics.push(format!(
                    "embedded {language} at bytes {}..{} failed to parse: {reason}",
                    region.content.start_byte, region.content.end_byte
                ));
                unparsed.push(TextRange {
                    start_byte: region.element.start_byte,
                    end_byte: region.element.end_byte,
                });
                continue;
            }
            ParseOutcome::Fallback { reason } => {
                // Unreachable while every routable embedded language has a
                // linked grammar, and recorded rather than assumed away: these
                // symbols were matched by pattern, not parsed.
                diagnostics.push(format!(
                    "embedded {language} at bytes {}..{} was recovered by pattern: {reason}",
                    region.content.start_byte, region.content.end_byte
                ));
            }
            ParseOutcome::Skipped { reason } => {
                // Unreachable: the inner call is handed the *outer* file's
                // path, and a path the skip rule matches returns before any
                // embedded region is looked for. Handled as a real case anyway
                // — the region's bytes went unread either way, and the one
                // thing that must not happen is a skipped region leaving the
                // outer file reporting `Clean` over it.
                diagnostics.push(format!(
                    "embedded {language} at bytes {}..{} was not parsed: {reason}",
                    region.content.start_byte, region.content.end_byte
                ));
                unparsed.push(TextRange {
                    start_byte: region.element.start_byte,
                    end_byte: region.element.end_byte,
                });
                continue;
            }
        }

        absorb(extraction, inner, region.content.start_byte);
    }

    extraction.diagnostics.extend(diagnostics);
    fold_unparsed_ranges(&mut extraction.parse_outcome, unparsed);
    reorder_after_merge(extraction);
}

/// Merge one region's extraction into the outer one, shifting every span by
/// `offset` — the region's start byte in the outer file.
fn absorb(extraction: &mut Extraction, inner: Extraction, offset: usize) {
    let file_path = extraction.file_path.clone();

    for mut symbol in inner.symbols {
        // The region is a file to the extractor, so it emitted its own `File`
        // node spanning the region. The outer extraction already carries the
        // real one; letting this through would put two `File` nodes in the
        // graph for one file, the second spanning only a script block.
        if symbol.kind == SymbolKind::File {
            continue;
        }
        shift(&mut symbol.span, offset);
        extraction.symbols.push(symbol);
    }
    for mut import in inner.imports {
        shift(&mut import.span, offset);
        extraction.imports.push(import);
    }
    for mut call in inner.calls {
        shift(&mut call.span, offset);
        extraction.calls.push(call);
    }
    for mut reference in inner.references {
        shift(&mut reference.span, offset);
        extraction.references.push(reference);
    }
    for mut route in inner.routes {
        shift(&mut route.span, offset);
        extraction.routes.push(route);
    }
    for mut export in inner.exports {
        // The inner extraction synthesised an export for its own `File` node,
        // which is the same export the outer already holds. Deduplicated on the
        // key the outer's own synthesis uses, widened with the module
        // specifier so a genuine `export { a } from './x'` still stands beside
        // a local `a`.
        if extraction.exports.iter().any(|existing| {
            existing.exported_name == export.exported_name
                && existing.module_specifier == export.module_specifier
        }) {
            continue;
        }
        shift(&mut export.span, offset);
        extraction.exports.push(export);
    }
    // File-scoped wiring is dropped: the outer run computed it from the same
    // path and from a source that *contains* this region, so every file-level
    // annotation here is already present. Symbol-scoped ones are new.
    extraction.wiring.extend(
        inner
            .wiring
            .into_iter()
            .filter(|annotation| annotation.target_symbol != file_path),
    );
    extraction.scope_locals.extend(inner.scope_locals);
    extraction
        .local_bindings
        .extend(inner.local_bindings.into_iter().map(|mut binding| {
            binding.start_byte = binding.start_byte.saturating_add(offset);
            binding
        }));
    extraction.diagnostics.extend(inner.diagnostics);
}

fn shift(span: &mut Span, offset: usize) {
    span.start_byte = span.start_byte.saturating_add(offset);
    span.end_byte = span.end_byte.saturating_add(offset);
}

/// Record the byte ranges no extractor read, so the file cannot report `Clean`
/// over them.
fn fold_unparsed_ranges(outcome: &mut ParseOutcome, mut unparsed: Vec<TextRange>) {
    if unparsed.is_empty() {
        return;
    }
    unparsed.sort_by_key(|range| (range.start_byte, range.end_byte));
    unparsed.dedup();
    match outcome {
        ParseOutcome::Clean => {
            *outcome = ParseOutcome::Partial {
                error_ranges: unparsed,
            }
        }
        ParseOutcome::Partial { error_ranges } => {
            error_ranges.extend(unparsed);
            error_ranges.sort_by_key(|range| (range.start_byte, range.end_byte));
            error_ranges.dedup();
        }
        // A file the outer grammar could not read at all never reaches here —
        // the extractor returns before the merge — and if it ever did, an
        // unread region inside already-refused bytes says nothing new. A
        // skipped file is the same shape for the same reason: it claims no
        // coverage to qualify.
        ParseOutcome::Failed { .. }
        | ParseOutcome::Fallback { .. }
        | ParseOutcome::Skipped { .. } => {}
    }
}

/// Restore the orderings the outer extraction established before the merge.
///
/// `exports` and `references` are sorted at the end of `extract_treesitter`;
/// appending to them after the fact would leave the payload's order dependent
/// on where in the file a script block happened to sit, which the determinism
/// gate digests. `symbols`, `imports` and `calls` are emitted in walk order and
/// are left in it: the regions were merged in ascending byte order, so the
/// merged sequence is still document order.
fn reorder_after_merge(extraction: &mut Extraction) {
    extraction.exports.sort_by(|left, right| {
        (
            &left.exported_name,
            &left.module_specifier,
            left.span.start_byte,
        )
            .cmp(&(
                &right.exported_name,
                &right.module_specifier,
                right.span.start_byte,
            ))
    });
    extraction.references.sort_by(|left, right| {
        (left.span.start_byte, left.span.end_byte, &left.name).cmp(&(
            right.span.start_byte,
            right.span.end_byte,
            &right.name,
        ))
    });
    extraction.scope_locals.sort();
    extraction.scope_locals.dedup();
    extraction.local_bindings.sort();
    extraction.local_bindings.dedup();
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scan(text: &str) -> (Vec<Region>, Vec<Span>) {
        let mut regions = Vec::new();
        let mut unterminated = Vec::new();
        scan_script_elements(text, 0, &mut regions, &mut unterminated);
        (regions, unterminated)
    }

    fn attribute(tag: &str, name: &str) -> Option<String> {
        attributes_of_start_tag(tag)
            .into_iter()
            .find(|(found, _)| found == name)
            .map(|(_, value)| value)
    }

    #[test]
    fn start_tag_attributes_are_read_in_every_quoting_style() {
        assert_eq!(
            attribute("<script lang=\"ts\">", "lang").as_deref(),
            Some("ts")
        );
        assert_eq!(
            attribute("<script lang='ts'>", "lang").as_deref(),
            Some("ts")
        );
        assert_eq!(attribute("<script lang=ts>", "lang").as_deref(), Some("ts"));
        assert_eq!(
            attribute("<SCRIPT LANG=\"TS\">", "lang").as_deref(),
            Some("TS"),
            "the attribute name is lowercased; the value is left alone and compared lowercased \
             where it is read"
        );
        // A valueless attribute (`setup`, `defer`) must not swallow the one after it.
        let attributes = attributes_of_start_tag("<script setup lang=\"ts\">");
        assert_eq!(
            attributes,
            vec![
                ("setup".to_string(), String::new()),
                ("lang".to_string(), "ts".to_string())
            ]
        );
        assert!(attributes_of_start_tag("<script>").is_empty());
        assert!(
            attributes_of_start_tag("<div lang=\"ts\">").is_empty(),
            "a tag that is not <script> contributes nothing"
        );
    }

    #[test]
    fn a_closing_angle_inside_a_quoted_value_does_not_end_the_start_tag() {
        let tag = "<script data-tpl=\"a > b\" lang=\"ts\">rest";
        let end = start_tag_end(tag, "<script".len()).expect("the tag closes");
        assert_eq!(&tag[..=end], "<script data-tpl=\"a > b\" lang=\"ts\">");
        assert_eq!(attribute(&tag[..=end], "lang").as_deref(), Some("ts"));
        assert_eq!(
            start_tag_end("<script lang=\"ts\"", "<script".len()),
            None,
            "an unclosed start tag is reported, never assumed to end at the buffer's end"
        );
    }

    #[test]
    fn case_insensitive_search_finds_only_real_matches() {
        assert_eq!(find_ascii_ci("a <SCRIPT> b", 0, "<script"), Some(2));
        assert_eq!(find_ascii_ci("a <SCRIPT> b", 3, "<script"), None);
        assert_eq!(find_ascii_ci("", 0, "<script"), None);
        assert_eq!(find_ascii_ci("<scr", 0, "<script"), None);
        assert_eq!(find_ascii_ci("x", 0, ""), None);
    }

    #[test]
    fn the_text_scan_finds_complete_script_elements_and_nothing_else() {
        let (regions, unterminated) = scan("<p>a</p>\n<script>let x = 1;</script>\n<b/>");
        assert!(unterminated.is_empty());
        assert_eq!(regions.len(), 1);
        assert_eq!(regions[0].content.start_byte, 17);
        assert_eq!(regions[0].content.end_byte, 27);
        assert_eq!(regions[0].element.start_byte, 9);
        assert_eq!(regions[0].element.end_byte, 36);

        let (regions, _) = scan("<script>a</script><script lang=\"ts\">b</script>");
        assert_eq!(regions.len(), 2, "consecutive elements are both found");
        assert_eq!(
            attributes_of_start_tag("<script lang=\"ts\">"),
            regions[1].attributes
        );

        let (regions, unterminated) = scan("<scripting>not a script</scripting>");
        assert!(
            regions.is_empty() && unterminated.is_empty(),
            "`<scripting>` is not a script element"
        );

        let (regions, unterminated) = scan("<script src=\"/x.js\" />after");
        assert!(
            regions.is_empty() && unterminated.is_empty(),
            "a self-closing element has no body, and skipping it is not a failure"
        );

        let (regions, _) = scan("<SCRIPT>let x = 1;</SCRIPT>");
        assert_eq!(regions.len(), 1, "tag names are case-insensitive in HTML");
    }

    #[test]
    fn an_unterminated_script_is_recorded_not_guessed() {
        let (regions, unterminated) = scan("<p/>\n<script>\n  var cart = ");
        assert!(
            regions.is_empty(),
            "no </script> means no body this build can claim to have read"
        );
        assert_eq!(unterminated.len(), 1);
        assert_eq!(unterminated[0].start_byte, 5);

        let (regions, unterminated) = scan("<script lang=\"ts\"");
        assert!(regions.is_empty());
        assert_eq!(
            unterminated.len(),
            1,
            "a start tag that never closes is reported too"
        );
    }

    #[test]
    fn the_text_scan_offsets_are_relative_to_the_run_it_was_given() {
        let mut regions = Vec::new();
        let mut unterminated = Vec::new();
        scan_script_elements("<script>x</script>", 1_000, &mut regions, &mut unterminated);
        assert_eq!(regions[0].content.start_byte, 1_008);
        assert_eq!(regions[0].content.end_byte, 1_009);
        assert_eq!(regions[0].element.start_byte, 1_000);
        assert_eq!(regions[0].element.end_byte, 1_018);
    }

    #[test]
    fn language_evidence_maps_to_registry_names() {
        let attrs = |pairs: &[(&str, &str)]| -> Vec<(String, String)> {
            pairs
                .iter()
                .map(|(name, value)| (name.to_string(), value.to_string()))
                .collect()
        };

        assert_eq!(requested_language(&[]), Requested::Default);
        assert_eq!(
            requested_language(&attrs(&[("lang", "ts")])),
            Requested::Named("typescript")
        );
        assert_eq!(
            requested_language(&attrs(&[("lang", "TypeScript")])),
            Requested::Named("typescript")
        );
        assert_eq!(
            requested_language(&attrs(&[("lang", "jsx")])),
            Requested::Named("javascript"),
            "the registry files .jsx under the JavaScript spec"
        );
        assert_eq!(
            requested_language(&attrs(&[("lang", "tsx")])),
            Requested::Named("tsx")
        );
        assert_eq!(
            requested_language(&attrs(&[("type", "module")])),
            Requested::Named("javascript")
        );
        assert_eq!(
            requested_language(&attrs(&[("type", "text/typescript")])),
            Requested::Named("typescript")
        );
        assert_eq!(
            requested_language(&attrs(&[("type", "application/json")])),
            Requested::NotScript
        );
        assert_eq!(
            requested_language(&attrs(&[("type", "importmap")])),
            Requested::NotScript,
            "an unknown `type` is not executed as script by a browser either"
        );
        assert_eq!(
            requested_language(&attrs(&[("lang", "coffee")])),
            Requested::Unrecognised("coffee".to_string())
        );
        assert_eq!(
            requested_language(&attrs(&[("lang", "ts"), ("type", "module")])),
            Requested::Named("typescript"),
            "`lang` is the framework's own directive and wins over the HTML `type`"
        );
        assert_eq!(
            requested_language(&attrs(&[("setup", ""), ("type", "")])),
            Requested::Default,
            "an empty value is no evidence"
        );
    }

    /// The permitted set and the default are the registry's, not this module's.
    #[test]
    fn the_permitted_set_is_read_from_the_language_registry() {
        for template in ["svelte", "astro"] {
            assert_eq!(
                permitted_embedded_languages(template),
                vec!["typescript", "javascript"],
                "{template} declares typescript first, so typescript is its default"
            );
        }
        // Vue alone also permits `tsx`: its compiler accepts
        // `<script lang="tsx">`, where Svelte's template is not JSX and an
        // Astro `<script>` is plain JS/TS. `typescript` is still first, so the
        // *default* is unchanged — which is the half of this assertion that
        // would break silently if the registry order ever moved.
        assert_eq!(
            permitted_embedded_languages("vue"),
            vec!["typescript", "tsx", "javascript"],
            "vue permits tsx and still defaults to typescript"
        );
        assert_eq!(
            permitted_embedded_languages("liquid"),
            vec!["javascript"],
            "liquid declares html, javascript, css — and only javascript resolves to a linked \
             grammar, which makes it both the permitted set and the default"
        );
        assert!(
            permitted_embedded_languages("python").is_empty(),
            "a language that embeds nothing costs one registry lookup and returns"
        );
        assert!(
            permitted_embedded_languages("typescript").is_empty(),
            "the languages regions are routed to embed nothing, so a region is never rescanned"
        );
        assert!(
            !permitted_embedded_languages("svelte").contains(&"css"),
            "css and html are in the registry's list and no grammar for either is linked; they \
             are dropped here rather than restated in a second list"
        );
    }

    #[test]
    fn unparsed_ranges_are_folded_onto_the_outcome_not_lost() {
        let range = |start: usize, end: usize| TextRange {
            start_byte: start,
            end_byte: end,
        };

        let mut outcome = ParseOutcome::Clean;
        fold_unparsed_ranges(&mut outcome, vec![]);
        assert_eq!(
            outcome,
            ParseOutcome::Clean,
            "nothing unread leaves a clean file clean"
        );

        let mut outcome = ParseOutcome::Clean;
        fold_unparsed_ranges(&mut outcome, vec![range(9, 4), range(1, 2)]);
        assert_eq!(
            outcome,
            ParseOutcome::Partial {
                error_ranges: vec![range(1, 2), range(9, 4)]
            },
            "ranges are sorted so two builds of one file emit the same payload"
        );

        let mut outcome = ParseOutcome::Partial {
            error_ranges: vec![range(1, 2)],
        };
        fold_unparsed_ranges(&mut outcome, vec![range(1, 2), range(5, 6)]);
        assert_eq!(
            outcome,
            ParseOutcome::Partial {
                error_ranges: vec![range(1, 2), range(5, 6)]
            },
            "the grammar's own error ranges are kept and duplicates collapse"
        );

        let mut outcome = ParseOutcome::Failed {
            reason: "refused".to_string(),
        };
        fold_unparsed_ranges(&mut outcome, vec![range(1, 2)]);
        assert_eq!(
            outcome,
            ParseOutcome::Failed {
                reason: "refused".to_string()
            },
            "a file already refused whole says nothing new about a region inside it"
        );
    }

    #[test]
    fn the_depth_guard_refuses_reentry_and_releases_on_drop() {
        let outer = DepthGuard::enter().expect("a fresh thread is not inside an embedded parse");
        assert!(
            DepthGuard::enter().is_none(),
            "a script region is not itself scanned for embedded regions"
        );
        drop(outer);
        assert!(
            DepthGuard::enter().is_some(),
            "the flag is released even though the inner attempt was refused"
        );
    }
}
