//! The markup and stylesheet halves of a template, HTML or CSS file.
//!
//! **The gap this closes.** [`crate::embedded`] routes a template file's
//! `<script>` blocks back through a real grammar, so the *code* half of a
//! `.svelte`, `.vue`, `.astro` or `.liquid` file is indexed. Everything else in
//! those files — the markup, and the `<style>` block — was indexed nowhere, and
//! `css` and `html` were listed in [`crate::fallback`]'s
//! `NON_DECLARATIVE_LANGUAGES`, so a standalone `.css` or `.html` file
//! contributed its `File` node and nothing more.
//!
//! Measured on a real Svelte application (GitPulse, 149 components) against a
//! current index:
//!
//! ```text
//! devmap search toggleAddMenu   -> 1 hit   (a <script> function)
//! devmap search data-add-repo   -> 0 hits, truncated=false
//! devmap search nav-heading     -> 0 hits, truncated=false
//! ```
//!
//! `data-add-repo` is declared on an element in `TaskBoard.svelte` and used by
//! a selector string in the same file's script; `nav-heading` is declared by
//! that file's `<style>` block and used by a `class` attribute. Both are
//! contracts between two halves of one file, and neither half was in the graph.
//!
//! **What this indexes.** Two declaration kinds and one reference kind:
//!
//! * [`SymbolKind::MarkupAnchor`] — an identity declared *in markup*: `id="x"`
//!   becomes `#x`, and a `data-*` attribute becomes `[data-x]`. Named in
//!   CSS-selector form because that is the form everything that targets the
//!   identity is written in, so it is the form a search for it uses.
//! * [`SymbolKind::StyleRule`] — a name declared *in a stylesheet*: each named
//!   component of each selector in a rule's selector list (`.nav-heading`,
//!   `#repo-heading`, `[data-add-repo]`), a `@keyframes` name, and a custom
//!   property declaration (`--gap-x`).
//! * [`ReferenceKind::Selector`] — a use: a class named in a `class` attribute
//!   or a Svelte `class:foo` directive, an id named by an IDREF attribute
//!   (`for`, `aria-controls`, `aria-labelledby`, …), a `var(--x)` read, and a
//!   selector string inside a script region.
//!
//! **Why an attribute *value* is not a name.** `data-tab="repos"` contributes
//! `[data-tab]` and not `[data-tab="repos"]`. The attribute name is the part a
//! selector must match; the value is content, and indexing content would put
//! one symbol per row of a rendered table into a graph of declarations. The
//! one exception is `id`, whose value *is* the identity.
//!
//! **Precision over recall, and never fabrication.** Three rules hold that
//! line, and each is the reason a whole class of input is skipped:
//!
//! 1. A selector string in a script region contributes a reference **only when
//!    the name it targets is declared in the same file**. `"[data-add-repo]"`
//!    in `TaskBoard.svelte` links to that file's own anchor; a selector naming
//!    something this file never declares contributes nothing, because the only
//!    other thing it could link to is a same-named class in an unrelated
//!    component.
//! 2. A [`ReferenceKind::Selector`] never enters the code resolution ladder.
//!    Class names and identifier names are different namespaces; a class called
//!    `menu` is not the function `menu`, and the ladder's unique-global rung
//!    would happily join them.
//! 3. The stylesheet reader is a **scanner, not a parser** — no CSS grammar is
//!    linked in this workspace — so it reads only what a scanner can read
//!    honestly: the text before a `{`, and declarations that begin `--`. It
//!    tracks comments, strings and brace depth so that neither a `{` in a
//!    string nor a selector in a comment is mistaken for structure.
//!
//! **Bounds.** Every scan is bounded by count *and* by bytes, and every bound
//! reports what it cut: [`MarkupScan::truncated_symbols`],
//! [`MarkupScan::truncated_references`] and [`MarkupScan::unread`]. A generated
//! stylesheet or a minified page must not be able to turn one file into tens of
//! thousands of speculative symbols, and it must not be able to look like a
//! complete read either.
//!
//! **What it deliberately does not do.** It does not evaluate expressions, so
//! `class={cls}` and Vue's `:class="…"` contribute nothing; it does not follow a
//! class applied by `classList.add(runtime)`. That is why liveness exempts both
//! new kinds instead of reporting an unreferenced rule as dead: this module can
//! prove a name is *used*, and cannot prove one is not.

use std::collections::HashSet;

use crate::model::{
    ExtractedReference, ExtractedSymbol, ReferenceKind, Span, SymbolKind, TextRange,
};

/// Most declarations one file may contribute here.
///
/// A bound, not a tuning knob, and the same one [`crate::fallback`] applies for
/// the same reason: a generated stylesheet would otherwise swamp the graph it is
/// meant to enrich. Measured on the 933-file frontend this was written for, the
/// busiest component declares 86 (`StatusPopover.svelte`) and the busiest file of
/// any kind 121 (`app.css`), so the cap is an order of magnitude above the real
/// distribution and only a generated file can reach it.
pub const MAX_MARKUP_SYMBOLS: usize = 2_000;

/// Most uses one file may contribute.
///
/// Higher than the declaration cap because uses are per *site*: one class
/// declared once is legitimately named by fifty elements, exactly as one
/// function is legitimately called fifty times.
pub const MAX_MARKUP_REFERENCES: usize = 8_000;

/// Most stylesheet bytes read in one file.
///
/// Past this the remaining bytes are recorded in [`MarkupScan::unread`] rather
/// than read, so a vendored `.css` bundle costs a bounded scan and the answer
/// says which bytes it does not cover.
pub const MAX_STYLESHEET_BYTES: usize = 512 * 1024;

/// Most markup bytes scanned in one text-scanned file (`.html`, Liquid).
pub const MAX_MARKUP_BYTES: usize = 512 * 1024;

/// Longest selector list this reads.
///
/// A rule prelude longer than this is not a selector list anyone wrote; it is a
/// minified file with no newlines, or a `{` inside something this scanner
/// mismodelled. Skipped rather than guessed at.
const MAX_SELECTOR_BYTES: usize = 1_024;

/// Longest string literal considered as a possible selector.
const MAX_SELECTOR_LITERAL_BYTES: usize = 512;

/// Longest single name this admits.
///
/// CSS identifiers have no length limit, but a 200-byte class name is a data
/// URI or a hash, not a name a person searches for.
const MAX_NAME_BYTES: usize = 200;

/// Attributes whose value is a *reference* to an `id` declared elsewhere.
///
/// Every entry is IDREF or IDREFS in the HTML and ARIA specifications, so this
/// is a spec-defined list and not a naming convention: `for="x"` names the
/// element whose id is `x`. The IDREFS ones carry a whitespace-separated list.
const IDREF_ATTRIBUTES: &[&str] = &[
    "for",
    "form",
    "list",
    "popovertarget",
    "aria-controls",
    "aria-labelledby",
    "aria-describedby",
    "aria-details",
    "aria-errormessage",
    "aria-flowto",
    "aria-owns",
    "headers",
    "itemref",
];

/// What one file's markup and stylesheet halves contributed.
#[derive(Debug, Default)]
pub struct MarkupScan {
    pub symbols: Vec<ExtractedSymbol>,
    pub references: Vec<ExtractedReference>,
    /// Script-region byte ranges located during the walk, so a caller can scan
    /// them for selector strings without locating them a second time.
    pub script_regions: Vec<Span>,
    /// Declarations found past [`MAX_MARKUP_SYMBOLS`] and therefore dropped.
    /// Non-zero means the symbol list is a prefix, not a set.
    pub truncated_symbols: usize,
    /// Uses found past [`MAX_MARKUP_REFERENCES`] and therefore dropped.
    pub truncated_references: usize,
    /// Byte ranges located and *not read*, because a byte cap was reached.
    /// A caller that reports coverage must count these as unread.
    pub unread: Vec<TextRange>,
}

impl MarkupScan {
    /// Whether any bound cut something. The one predicate callers use to decide
    /// whether the answer needs a disclosure, so no caller has to remember all
    /// three fields.
    pub fn is_truncated(&self) -> bool {
        self.truncated_symbols > 0 || self.truncated_references > 0 || !self.unread.is_empty()
    }

    /// The names this file declares, in selector form — the set rule 1 of this
    /// module's contract consults before admitting a selector string.
    pub fn declared_names(&self) -> HashSet<String> {
        self.symbols.iter().map(|sym| sym.name.clone()).collect()
    }
}

/// Accumulates one file's findings under this module's caps and dedup rules.
struct Collector<'a> {
    file_path: &'a str,
    scan: MarkupScan,
    /// One declaration per name per file. A class is routinely declared by
    /// several rules (`.a` and `.a:hover`), and the graph must carry one symbol
    /// for it, not one per rule — a second `.a` is the same declaration seen
    /// again, not a second thing to find.
    declared: HashSet<String>,
}

impl<'a> Collector<'a> {
    fn new(file_path: &'a str) -> Self {
        Self {
            file_path,
            scan: MarkupScan::default(),
            declared: HashSet::new(),
        }
    }

    fn add_symbol(&mut self, name: &str, kind: SymbolKind, span: Span) {
        if !is_admissible_name(name) {
            return;
        }
        if self.declared.contains(name) {
            return;
        }
        if self.scan.symbols.len() >= MAX_MARKUP_SYMBOLS {
            self.scan.truncated_symbols += 1;
            return;
        }
        self.declared.insert(name.to_string());
        self.scan.symbols.push(ExtractedSymbol {
            name: name.to_string(),
            qualified_name: format!("{}::{}", self.file_path, name),
            kind,
            span,
            // Exportedness is a claim about a module's public API. A DOM hook
            // has no such thing, and `true` would put every one of these into
            // any consumer that reads `is_exported` as "public surface".
            is_exported: false,
            docstring: None,
            signature: None,
            parent_symbol: None,
            body_signature: None,
            declaration_hash: None,
        });
    }

    fn add_reference(&mut self, name: &str, span: Span) {
        if !is_admissible_name(name) {
            return;
        }
        if self.scan.references.len() >= MAX_MARKUP_REFERENCES {
            self.scan.truncated_references += 1;
            return;
        }
        self.scan.references.push(ExtractedReference {
            name: name.to_string(),
            kind: ReferenceKind::Selector,
            span,
            // Filled in by the caller, which knows the file's symbol table and
            // can name the function a script-region use sits inside.
            enclosing_symbol: None,
            assigned_to: None,
            receiver_expr: None,
        });
    }
}

/// Whether `name` is a selector-shaped name worth putting in the graph.
///
/// Rejects the empty string, anything past [`MAX_NAME_BYTES`], and anything
/// whose body is not an identifier — so a mis-scanned fragment cannot enter the
/// graph as a declaration.
fn is_admissible_name(name: &str) -> bool {
    if name.is_empty() || name.len() > MAX_NAME_BYTES {
        return false;
    }
    let body = match name.as_bytes()[0] {
        b'.' | b'#' => &name[1..],
        b'[' => {
            if !name.ends_with(']') {
                return false;
            }
            &name[1..name.len() - 1]
        }
        _ => {
            // `--custom-property` and `@keyframes name` are the two remaining
            // shapes. Both are checked below by the same identifier rule, the
            // at-rule after its keyword.
            if let Some(rest) = name.strip_prefix("@keyframes ") {
                rest
            } else if let Some(rest) = name.strip_prefix("--") {
                rest
            } else {
                return false;
            }
        }
    };
    !body.is_empty() && body.bytes().all(is_name_byte)
}

/// Bytes a CSS identifier may contain, as this scanner models one.
///
/// ASCII alphanumerics, `-`, `_`, and every non-ASCII byte: CSS identifiers
/// admit non-ASCII directly, and a UTF-8 continuation byte is never one of the
/// delimiters this scanner stops on, so admitting the whole range keeps a
/// non-ASCII class name intact rather than truncating it mid-codepoint.
/// Escapes (`\.`) are **not** modelled — a name containing one is skipped by
/// [`is_admissible_name`] rather than half-read.
fn is_name_byte(byte: u8) -> bool {
    byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_') || !byte.is_ascii()
}

// ---------------------------------------------------------------------------
// Stylesheet scanning
// ---------------------------------------------------------------------------

/// Read a standalone stylesheet (`.css`, `.scss`, `.less`).
pub fn scan_stylesheet(file_path: &str, source: &str) -> MarkupScan {
    let mut collector = Collector::new(file_path);
    read_stylesheet(source, 0, &mut collector);
    collector.scan
}

/// Read one stylesheet region, appending to `collector`.
///
/// `base` is the region's byte offset in the file, added to every span, for the
/// reason [`crate::embedded`] shifts its spans: a span is a byte range into the
/// file on disk and every consumer slices the file with it, so an unshifted
/// span is not a coarser answer but a wrong one.
fn read_stylesheet(css: &str, base: usize, collector: &mut Collector) {
    let full_len = css.len();
    let (css, cut) = cap_region(css, MAX_STYLESHEET_BYTES);
    if let Some(cut_at) = cut {
        collector.scan.unread.push(TextRange {
            start_byte: base + cut_at,
            end_byte: base + full_len,
        });
    }
    let bytes = css.as_bytes();
    let mut at = 0usize;
    // Start of the text since the last structural token: a rule prelude before
    // a `{`, or a declaration before a `;` or `}`.
    let mut chunk_start = 0usize;
    // Open braces. Counted separately from `blocks` because `blocks` stops
    // growing at [`MAX_BLOCK_DEPTH`]: without the count, a `}` past the cap
    // would pop an entry describing a *shallower* block and every remaining
    // answer about "am I inside a rule body" would be off by one level.
    let mut depth = 0usize;
    // One entry per open brace, up to the cap: whether that block is a *rule*
    // body (so its `--x:` declarations are custom properties) rather than an
    // at-rule group.
    let mut blocks: Vec<bool> = Vec::new();

    while at < bytes.len() {
        match bytes[at] {
            b'/' if bytes.get(at + 1) == Some(&b'*') => {
                // A comment is skipped whole. It may contain `{`, `}`, `;` and
                // quotes, and a scanner that read those as structure would
                // report a commented-out rule as a declaration.
                at = match css[at + 2..].find("*/") {
                    Some(offset) => at + 2 + offset + 2,
                    None => bytes.len(),
                };
                continue;
            }
            quote @ (b'"' | b'\'') => {
                at = skip_string(bytes, at, quote);
                continue;
            }
            b'{' => {
                let prelude = &css[chunk_start..at];
                let is_rule = read_rule_prelude(prelude, base + chunk_start, collector);
                depth += 1;
                if blocks.len() < MAX_BLOCK_DEPTH {
                    blocks.push(is_rule);
                }
                chunk_start = at + 1;
            }
            b'}' => {
                // The last declaration in a block needs no `;`, so the text
                // before the closing brace is one.
                let in_rule = innermost_is_rule(depth, &blocks);
                read_declaration(
                    &css[chunk_start..at],
                    base + chunk_start,
                    in_rule,
                    collector,
                );
                if depth == blocks.len() {
                    blocks.pop();
                }
                depth = depth.saturating_sub(1);
                chunk_start = at + 1;
            }
            b';' => {
                let in_rule = innermost_is_rule(depth, &blocks);
                read_declaration(
                    &css[chunk_start..at],
                    base + chunk_start,
                    in_rule,
                    collector,
                );
                chunk_start = at + 1;
            }
            _ => {}
        }
        at += 1;
    }
    // Trailing text with no closing brace — a truncated file. `var(--x)` reads
    // in it are still reads, and the enclosing block's kind is unknown, so it
    // is treated as not-a-rule: custom properties are not claimed from it.
    read_declaration(&css[chunk_start..], base + chunk_start, false, collector);
}

/// Deepest nesting this tracks.
///
/// SCSS nests, `@media` nests, and a hostile file nests a million times. Past
/// this the depth stack stops growing; the scan continues, and the only cost is
/// that a custom property nested deeper than this is not claimed as one.
const MAX_BLOCK_DEPTH: usize = 64;

/// Whether the innermost open block is a rule body.
///
/// `false` whenever the answer is not *known*: past [`MAX_BLOCK_DEPTH`] the
/// stack no longer describes the innermost block, and at depth 0 there is no
/// block at all. Both are cases where claiming a `--x:` as a custom property
/// declaration would be a guess, and this reader does not guess.
fn innermost_is_rule(depth: usize, blocks: &[bool]) -> bool {
    if depth == 0 || depth != blocks.len() {
        return false;
    }
    blocks.last().copied().unwrap_or(false)
}

/// Read a rule prelude — the text before a `{`. Returns whether the block it
/// opens is a rule body.
///
/// An at-rule prelude is not a selector list: `@media (max-width: 40rem)` names
/// no element. `@keyframes pulse` declares the name `pulse`, and its body's
/// `from`/`to` blocks are keyframe selectors that name nothing, so they fall out
/// on their own — this scanner only ever claims `.`, `#`, `[` and `--` shapes.
fn read_rule_prelude(prelude: &str, base: usize, collector: &mut Collector) -> bool {
    let trimmed_start = prelude.len() - prelude.trim_start().len();
    let prelude_text = prelude.trim();
    if prelude_text.is_empty() || prelude_text.len() > MAX_SELECTOR_BYTES {
        return false;
    }
    if let Some(rest) = prelude_text.strip_prefix('@') {
        let (keyword, argument) = split_at_rule(rest);
        if keyword.eq_ignore_ascii_case("keyframes")
            || keyword.eq_ignore_ascii_case("-webkit-keyframes")
        {
            if let Some(name) = argument.split_whitespace().next() {
                collector.add_symbol(
                    &format!("@keyframes {name}"),
                    SymbolKind::StyleRule,
                    Span {
                        start_byte: base + trimmed_start,
                        end_byte: base + trimmed_start + prelude_text.len(),
                    },
                );
            }
        }
        // Every other at-rule — `@media`, `@supports`, `@layer`, `@container` —
        // opens a group whose children are the rules. Not a rule body itself,
        // so a `--x:` directly inside one is not claimed as a custom property.
        return false;
    }
    for (name, span) in selector_components(prelude, base) {
        collector.add_symbol(&name, SymbolKind::StyleRule, span);
    }
    true
}

/// Split `@keyword rest` into its two halves.
fn split_at_rule(rest: &str) -> (&str, &str) {
    let end = rest
        .find(|c: char| c.is_whitespace() || c == '(' || c == '{')
        .unwrap_or(rest.len());
    (&rest[..end], rest[end..].trim_start())
}

/// Read one declaration — the text between `{`/`;` and `;`/`}`.
///
/// Claims a custom property when the declaration begins `--`, and a
/// [`ReferenceKind::Selector`] use for every `var(--x)` in its value. `in_rule`
/// gates the declaration half: a `--x:` directly inside `@media` is not a
/// custom property declaration, while a `var(--x)` read is a read wherever it
/// appears.
fn read_declaration(declaration: &str, base: usize, in_rule: bool, collector: &mut Collector) {
    if declaration.len() > MAX_SELECTOR_BYTES {
        // Still scanned for `var()` — a long declaration is ordinary (a
        // `grid-template-areas`, a `box-shadow` list) and its reads are real.
        read_var_uses(declaration, base, collector);
        return;
    }
    let leading = declaration.len() - declaration.trim_start().len();
    let text = declaration.trim();
    if in_rule && text.starts_with("--") {
        if let Some((name, _)) = text.split_once(':') {
            let name = name.trim_end();
            collector.add_symbol(
                name,
                SymbolKind::StyleRule,
                Span {
                    start_byte: base + leading,
                    end_byte: base + leading + name.len(),
                },
            );
        }
    }
    read_var_uses(declaration, base, collector);
}

/// Every `var(--name)` in `text`, as a use of `--name`.
fn read_var_uses(text: &str, base: usize, collector: &mut Collector) {
    let bytes = text.as_bytes();
    let mut at = 0usize;
    while let Some(offset) = text[at..].find("var(") {
        let open = at + offset + 4;
        let mut cursor = open;
        while cursor < bytes.len() && bytes[cursor].is_ascii_whitespace() {
            cursor += 1;
        }
        let name_start = cursor;
        while cursor < bytes.len() && is_name_byte(bytes[cursor]) {
            cursor += 1;
        }
        // Only `var(--x)`; `var(x)` is invalid CSS and `var(` followed by
        // nothing is a truncated file.
        if cursor > name_start && text[name_start..].starts_with("--") {
            collector.add_reference(
                &text[name_start..cursor],
                Span {
                    start_byte: base + name_start,
                    end_byte: base + cursor,
                },
            );
        }
        at = open.min(bytes.len());
        if at >= bytes.len() {
            break;
        }
    }
}

/// The named components of a selector list, with their spans.
///
/// `.nav-heading > .icon:hover, #repo-heading` yields `.nav-heading`, `.icon`
/// and `#repo-heading`. Element names, combinators, `*` and `&` name nothing
/// this graph can hold and are skipped; a pseudo-class's argument **is** read,
/// because `:global(.x)` is how a Svelte component declares a global class and
/// skipping it would lose exactly the declarations that cross file boundaries.
///
/// An attribute selector contributes its attribute *name*: `[data-x="y"]`
/// yields `[data-x]`, matching the anchor the markup side declares for the same
/// attribute so that the two join.
fn selector_components(selector: &str, base: usize) -> Vec<(String, Span)> {
    let bytes = selector.as_bytes();
    let mut found = Vec::new();
    let mut at = 0usize;
    while at < bytes.len() {
        match bytes[at] {
            quote @ (b'"' | b'\'') => {
                at = skip_string(bytes, at, quote);
                continue;
            }
            b'.' | b'#' => {
                let sigil = bytes[at];
                let name_start = at + 1;
                let mut cursor = name_start;
                while cursor < bytes.len() && is_name_byte(bytes[cursor]) {
                    cursor += 1;
                }
                if cursor > name_start {
                    found.push((
                        format!("{}{}", sigil as char, &selector[name_start..cursor]),
                        Span {
                            start_byte: base + at,
                            end_byte: base + cursor,
                        },
                    ));
                }
                at = cursor.max(at + 1);
                continue;
            }
            b'[' => {
                let name_start = at + 1;
                let mut cursor = name_start;
                while cursor < bytes.len() && is_name_byte(bytes[cursor]) {
                    cursor += 1;
                }
                let close = selector[at..].find(']').map(|offset| at + offset);
                if cursor > name_start {
                    found.push((
                        // Folded to lowercase, like the markup side folds an
                        // attribute name, and for the same reason: HTML matches
                        // attribute names ASCII case-insensitively, so
                        // `[data-Foo]` in a rule and `data-foo` on an element are
                        // one name. Folded in **both** readers or in neither —
                        // one alone is worse than either, because the two halves
                        // of the contract stop joining while each looks right.
                        // Class and id keep their case above: those are
                        // case-sensitive, and folding them would join `.Card` to
                        // an unrelated `.card`.
                        format!("[{}]", selector[name_start..cursor].to_ascii_lowercase()),
                        Span {
                            start_byte: base + at,
                            end_byte: base + close.map(|end| end + 1).unwrap_or(cursor),
                        },
                    ));
                }
                // Past the whole bracket, so a quoted value inside it is never
                // read as a selector of its own.
                at = close.map(|end| end + 1).unwrap_or(cursor).max(at + 1);
                continue;
            }
            _ => {}
        }
        at += 1;
    }
    found
}

/// Step past a quoted run starting at `at`, honouring backslash escapes.
/// Returns the index just past the closing quote, or the end of input.
fn skip_string(bytes: &[u8], at: usize, quote: u8) -> usize {
    let mut cursor = at + 1;
    while cursor < bytes.len() {
        match bytes[cursor] {
            b'\\' => cursor += 2,
            byte if byte == quote => return cursor + 1,
            _ => cursor += 1,
        }
    }
    bytes.len()
}

/// Truncate `text` to `limit` bytes on a character boundary, reporting where it
/// was cut.
fn cap_region(text: &str, limit: usize) -> (&str, Option<usize>) {
    if text.len() <= limit {
        return (text, None);
    }
    let mut cut = limit;
    while cut > 0 && !text.is_char_boundary(cut) {
        cut -= 1;
    }
    (&text[..cut], Some(cut))
}

// ---------------------------------------------------------------------------
// Markup attributes
// ---------------------------------------------------------------------------

/// Read one attribute, given its name and the literal value the markup gives it.
///
/// `value` is `None` for a valueless attribute (`data-add-repo`) **and** for one
/// whose value is an expression (`class={cls}`): in both cases the markup states
/// no literal, and the difference does not change what can be indexed.
///
/// One reader for every caller — the tree walk over Svelte, Vue and Astro, and
/// the text scan over HTML and Liquid — so the rule for what an attribute
/// contributes has exactly one owner.
fn read_attribute(
    name: &str,
    value: Option<&str>,
    name_span: Span,
    value_span: Option<Span>,
    collector: &mut Collector,
) {
    let lowered = name.to_ascii_lowercase();
    if lowered.starts_with("class:") {
        // Svelte's `class:active={cond}` — the directive names the class, and
        // whether it is applied is the expression's business.
        //
        // The class name keeps its own case while the directive prefix does not,
        // because they are different kinds of name: `class:` is an attribute
        // name, which HTML matches ASCII case-insensitively, and what follows is
        // a class name, which is case-*sensitive*. Folding the whole thing turned
        // `class:isActive` into `.isactive`, which joins to no rule anyone wrote.
        let class = &name[lowered.find(':').map(|at| at + 1).unwrap_or(name.len())..];
        collector.add_reference(&format!(".{class}"), name_span);
        return;
    }
    if lowered.starts_with("data-") {
        collector.add_symbol(&format!("[{lowered}]"), SymbolKind::MarkupAnchor, name_span);
        return;
    }
    let Some(value) = value else { return };
    let value_span = value_span.unwrap_or(name_span);
    match lowered.as_str() {
        "id" => {
            let id = value.trim();
            collector.add_symbol(&format!("#{id}"), SymbolKind::MarkupAnchor, value_span);
        }
        "class" => {
            for token in value.split_whitespace() {
                collector.add_reference(&format!(".{token}"), value_span.clone());
            }
        }
        other if IDREF_ATTRIBUTES.contains(&other) => {
            for token in value.split_whitespace() {
                collector.add_reference(&format!("#{token}"), value_span.clone());
            }
        }
        _ => {}
    }
}

// ---------------------------------------------------------------------------
// Text-scanned markup (HTML, and Liquid's opaque template text)
// ---------------------------------------------------------------------------

/// Read markup from text, for the two inputs no linked grammar models: a
/// standalone `.html` file, and the single opaque `template_content` leaf the
/// Liquid grammar hands back for everything that is not a Liquid tag.
pub fn scan_markup_text(file_path: &str, source: &str) -> MarkupScan {
    let mut collector = Collector::new(file_path);
    read_markup_text(source, 0, &mut collector);
    collector.scan
}

/// Scan `text` for start tags, `<style>` blocks and `<script>` bodies.
///
/// A scanner and not a parser, so it is written to *stop* rather than guess: an
/// unterminated `<style>` contributes nothing, and a `<` that does not begin a
/// tag name is ordinary text. `<script>` bodies are located (so a caller can
/// scan them for selector strings) and never read as markup — a `<` inside a
/// JavaScript comparison is not a tag.
fn read_markup_text(text: &str, base: usize, collector: &mut Collector) {
    let full_len = text.len();
    let (text, cut) = cap_region(text, MAX_MARKUP_BYTES);
    if let Some(cut_at) = cut {
        collector.scan.unread.push(TextRange {
            start_byte: base + cut_at,
            end_byte: base + full_len,
        });
    }
    let bytes = text.as_bytes();
    let mut at = 0usize;
    while at < bytes.len() {
        if bytes[at] != b'<' {
            at += 1;
            continue;
        }
        if text[at..].starts_with("<!--") {
            at = match text[at + 4..].find("-->") {
                Some(offset) => at + 4 + offset + 3,
                None => bytes.len(),
            };
            continue;
        }
        let name_start = at + 1;
        let mut cursor = name_start;
        while cursor < bytes.len()
            && (bytes[cursor].is_ascii_alphanumeric()
                || matches!(bytes[cursor], b'-' | b'_' | b':'))
        {
            cursor += 1;
        }
        if cursor == name_start {
            at += 1;
            continue;
        }
        let tag = text[name_start..cursor].to_ascii_lowercase();
        let Some(tag_end) = find_tag_end(bytes, cursor) else {
            // No `>` in the rest of the file: a truncated tag. Nothing after it
            // can be read as markup either, so the scan stops.
            return;
        };
        read_tag_attributes(&text[cursor..tag_end], base + cursor, collector);
        at = tag_end + 1;

        // A raw-text element's body is not markup. `<style>` goes to the
        // stylesheet reader; `<script>` is recorded for the caller's selector
        // scan and otherwise skipped.
        if tag == "style" || tag == "script" {
            let close = format!("</{tag}");
            let Some(offset) = find_case_insensitive(&text[at..], &close) else {
                // Unterminated: the bytes exist and were not read.
                collector.scan.unread.push(TextRange {
                    start_byte: base + at,
                    end_byte: base + text.len(),
                });
                return;
            };
            let body_end = at + offset;
            if tag == "style" {
                read_stylesheet(&text[at..body_end], base + at, collector);
            } else {
                collector.scan.script_regions.push(Span {
                    start_byte: base + at,
                    end_byte: base + body_end,
                });
            }
            at = body_end;
        }
    }
}

/// Index of the `>` closing a start tag beginning at `from`, honouring quoted
/// attribute values so a `>` inside one does not end the tag.
fn find_tag_end(bytes: &[u8], from: usize) -> Option<usize> {
    let mut at = from;
    while at < bytes.len() {
        match bytes[at] {
            quote @ (b'"' | b'\'') => {
                at = skip_string(bytes, at, quote);
                continue;
            }
            b'>' => return Some(at),
            _ => at += 1,
        }
    }
    None
}

/// Case-insensitive `find` for a lowercase ASCII needle.
///
/// Written out rather than `haystack.to_ascii_lowercase().find(needle)`, which
/// is what this was first: that allocates a copy of everything left in the file
/// **per tag**, so a page with a thousand `<script>` elements lowercased the
/// remaining bytes a thousand times — O(n^2) time and O(n) allocation each. This
/// is the same search with neither.
///
/// The needle is `</style` or `</script`, both ASCII, and only the haystack may
/// hold multi-byte characters. A UTF-8 continuation byte is never an ASCII
/// letter, so comparing byte-wise under `to_ascii_lowercase` can neither match
/// inside a character nor return an index that is not a boundary.
fn find_case_insensitive(haystack: &str, needle: &str) -> Option<usize> {
    debug_assert!(
        needle.bytes().all(|byte| !byte.is_ascii_uppercase()),
        "the needle must already be lowercase for a byte-wise comparison to be symmetric"
    );
    let bytes = haystack.as_bytes();
    let needle = needle.as_bytes();
    if needle.is_empty() || needle.len() > bytes.len() {
        return None;
    }
    let first = needle[0];
    for start in 0..=(bytes.len() - needle.len()) {
        if bytes[start].to_ascii_lowercase() != first {
            continue;
        }
        if bytes[start..start + needle.len()]
            .iter()
            .zip(needle)
            .all(|(candidate, wanted)| candidate.to_ascii_lowercase() == *wanted)
        {
            return Some(start);
        }
    }
    None
}

/// Read the attributes in the text of a start tag, after the tag name.
///
/// `base` is the offset of `region`'s first byte in the file, so every span this
/// reports is a file offset.
fn read_tag_attributes(region: &str, base: usize, collector: &mut Collector) {
    let bytes = region.as_bytes();
    let mut at = 0usize;
    while at < bytes.len() {
        while at < bytes.len() && !is_attribute_name_start(bytes[at]) {
            if matches!(bytes[at], b'>' | b'/') && at + 1 >= bytes.len() {
                return;
            }
            at += 1;
        }
        if at >= bytes.len() {
            return;
        }
        let name_start = at;
        while at < bytes.len() && is_attribute_name_byte(bytes[at]) {
            at += 1;
        }
        let name = &region[name_start..at];
        let name_span = Span {
            start_byte: base + name_start,
            end_byte: base + at,
        };
        let mut cursor = at;
        while cursor < bytes.len() && bytes[cursor].is_ascii_whitespace() {
            cursor += 1;
        }
        if bytes.get(cursor) != Some(&b'=') {
            read_attribute(name, None, name_span, None, collector);
            continue;
        }
        cursor += 1;
        while cursor < bytes.len() && bytes[cursor].is_ascii_whitespace() {
            cursor += 1;
        }
        let (value, value_span, next) = match bytes.get(cursor) {
            Some(quote @ (b'"' | b'\'')) => {
                let quote = *quote;
                let value_start = cursor + 1;
                let mut end = value_start;
                while end < bytes.len() && bytes[end] != quote {
                    end += 1;
                }
                (
                    &region[value_start..end.min(region.len())],
                    Span {
                        start_byte: base + value_start,
                        end_byte: base + end,
                    },
                    (end + 1).min(bytes.len()),
                )
            }
            // `{expr}` — a framework expression, not a literal. Stepped over
            // whole, so the identifiers inside it are not read as class names.
            Some(b'{') => {
                let end = match region[cursor..].find('}') {
                    Some(offset) => cursor + offset + 1,
                    None => bytes.len(),
                };
                read_attribute(name, None, name_span, None, collector);
                at = end;
                continue;
            }
            Some(_) => {
                let value_start = cursor;
                let mut end = cursor;
                while end < bytes.len()
                    && !bytes[end].is_ascii_whitespace()
                    && !matches!(bytes[end], b'>' | b'/')
                {
                    end += 1;
                }
                (
                    &region[value_start..end],
                    Span {
                        start_byte: base + value_start,
                        end_byte: base + end,
                    },
                    end,
                )
            }
            None => return,
        };
        read_attribute(name, Some(value), name_span, Some(value_span), collector);
        at = next;
    }
}

fn is_attribute_name_start(byte: u8) -> bool {
    byte.is_ascii_alphabetic() || matches!(byte, b'_' | b':' | b'@')
}

fn is_attribute_name_byte(byte: u8) -> bool {
    byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b':' | b'.' | b'@')
}

// ---------------------------------------------------------------------------
// Selector strings in script regions
// ---------------------------------------------------------------------------

/// Selector uses named by string literals inside `regions` of `source`.
///
/// This is the half that closes the loop on a DOM hook: `TaskBoard.svelte`
/// declares `data-add-repo` on an element and its script names
/// `"[data-add-repo], [data-add-repo-popup]"`. Without this the two halves are
/// both in the graph and unconnected.
///
/// **Only names `declared` holds are admitted.** That is the whole precision
/// story: a literal is a selector *because the file it sits in declares what it
/// names*, and a literal naming something this file never declares contributes
/// nothing rather than linking to a same-named class in an unrelated component.
/// It also makes the pass incapable of inventing a symbol — every reference it
/// emits has a declaration, in this file, that it was matched against.
pub fn selector_references(
    source: &str,
    regions: &[Span],
    declared: &HashSet<String>,
    limit: usize,
) -> (Vec<ExtractedReference>, usize) {
    let mut references: Vec<ExtractedReference> = Vec::new();
    let mut truncated = 0usize;
    if declared.is_empty() {
        return (references, truncated);
    }
    for region in regions {
        let Some(text) = source.get(region.start_byte..region.end_byte) else {
            continue;
        };
        let bytes = text.as_bytes();
        let mut at = 0usize;
        while at < bytes.len() {
            let quote = match bytes[at] {
                byte @ (b'"' | b'\'' | b'`') => byte,
                _ => {
                    at += 1;
                    continue;
                }
            };
            let end = skip_string(bytes, at, quote);
            let literal_start = at + 1;
            // `end - 1` is the closing quote only when there *was* one. On an
            // unterminated literal `skip_string` returns the end of input, and
            // `end - 1` can land inside a multi-byte character — slicing there
            // panics. The closed case is the one that steps back.
            let closed = end > literal_start && bytes.get(end - 1) == Some(&quote);
            let literal_end = if closed { end - 1 } else { end };
            at = end;
            let literal = &text[literal_start.min(text.len())..literal_end.min(text.len())];
            if literal.is_empty() || literal.len() > MAX_SELECTOR_LITERAL_BYTES {
                continue;
            }
            for (name, span) in selector_components(literal, region.start_byte + literal_start) {
                if !declared.contains(&name) {
                    continue;
                }
                if references.len() >= limit {
                    truncated += 1;
                    continue;
                }
                references.push(ExtractedReference {
                    name,
                    kind: ReferenceKind::Selector,
                    span,
                    enclosing_symbol: None,
                    assigned_to: None,
                    receiver_expr: None,
                });
            }
        }
    }
    (references, truncated)
}

// ---------------------------------------------------------------------------
// Template parse trees
// ---------------------------------------------------------------------------

/// Read the markup and stylesheet halves of a template file from its own parse
/// tree.
///
/// Used for the three grammars that model HTML — Svelte, Vue and Astro — where
/// the tree gives `attribute` nodes directly and reading them is exact rather
/// than scanned. Liquid models no HTML at all: its markup arrives as one
/// `template_content` leaf, which is handed to [`read_markup_text`].
///
/// Pre-order with an explicit stack, for the reason every other walk in this
/// crate uses one: adversarially deep markup must not overflow the stack.
///
/// The node kinds are measured against each linked grammar, not assumed:
///
/// ```text
/// element / style_element -> start_tag | self_closing_tag -> attribute
///                            -> attribute_name, quoted_attribute_value -> attribute_value
/// style_element  -> raw_text        (the stylesheet)
/// script_element -> raw_text        (recorded for the selector-string pass)
/// frontmatter    -> frontmatter_js_block   (Astro; also a script region)
/// template_content                  (Liquid; text-scanned)
/// ```
#[cfg(feature = "parse")]
pub(crate) fn scan_template_tree(
    file_path: &str,
    root: tree_sitter::Node,
    source: &str,
) -> MarkupScan {
    use crate::embedded::{child_of_kind, span_of, text_slice};
    use tree_sitter::Node;

    /// Deepest markup nesting the walk descends.
    ///
    /// Not a stack bound — the walk is iterative — but a work bound: a
    /// generated page nested tens of thousands deep is not markup anyone wrote,
    /// and the bytes below the cut are reported unread rather than skipped
    /// quietly.
    const MAX_ELEMENT_DEPTH: usize = 512;

    let mut collector = Collector::new(file_path);
    let mut stack: Vec<(Node, usize)> = vec![(root, 0)];
    while let Some((node, depth)) = stack.pop() {
        match node.kind() {
            "style_element" => {
                if let Some(body) = child_of_kind(node, "raw_text") {
                    read_stylesheet(text_slice(body, source), body.start_byte(), &mut collector);
                }
                continue;
            }
            "script_element" => {
                if let Some(body) = child_of_kind(node, "raw_text") {
                    collector.scan.script_regions.push(span_of(body));
                }
                continue;
            }
            "frontmatter" => {
                if let Some(body) = child_of_kind(node, "frontmatter_js_block") {
                    collector.scan.script_regions.push(span_of(body));
                }
                continue;
            }
            "template_content" => {
                read_markup_text(text_slice(node, source), node.start_byte(), &mut collector);
                continue;
            }
            "start_tag" | "self_closing_tag" => {
                read_tree_attributes(node, source, &mut collector);
            }
            _ => {}
        }
        if depth >= MAX_ELEMENT_DEPTH {
            collector.scan.unread.push(TextRange {
                start_byte: node.start_byte(),
                end_byte: node.end_byte(),
            });
            continue;
        }
        let mut cursor = node.walk();
        let children: Vec<Node> = node.children(&mut cursor).collect();
        for child in children.into_iter().rev() {
            stack.push((child, depth + 1));
        }
    }
    collector.scan
}

/// Read every `attribute` child of a start tag from the tree.
///
/// Vue's `directive_attribute` (`:class="…"`, `@click="…"`) is deliberately not
/// read: its value is an expression in the component's own language, and the
/// identifiers in it are not class names.
#[cfg(feature = "parse")]
fn read_tree_attributes(tag: tree_sitter::Node, source: &str, collector: &mut Collector) {
    use crate::embedded::{child_of_kind, span_of, text_slice};
    let mut cursor = tag.walk();
    let children: Vec<tree_sitter::Node> = tag.children(&mut cursor).collect();
    for attribute in children {
        if attribute.kind() != "attribute" {
            continue;
        }
        let Some(name_node) = child_of_kind(attribute, "attribute_name") else {
            continue;
        };
        let name_span = span_of(name_node);
        let value = child_of_kind(attribute, "quoted_attribute_value")
            .and_then(|quoted| child_of_kind(quoted, "attribute_value"))
            .or_else(|| child_of_kind(attribute, "attribute_value"));
        match value {
            Some(value_node) => read_attribute(
                text_slice(name_node, source),
                Some(text_slice(value_node, source)),
                name_span,
                Some(span_of(value_node)),
                collector,
            ),
            None => read_attribute(
                text_slice(name_node, source),
                None,
                name_span,
                None,
                collector,
            ),
        }
    }
}

// `child_of_kind`, `span_of` and `text_slice` are [`crate::embedded`]'s: the two
// tree walks read the same grammars and must not hold two answers to "what is
// this node's text". Imported at each use site below rather than re-declared.

/// Whether `grammar`'s registry entry says a file in it can contain markup or a
/// stylesheet.
///
/// Read from [`crate::languages::LanguageSpec::embedded`] — the same single
/// authority [`crate::embedded`] consults for which *scripts* may appear — so
/// adding a template language to the registry enables this pass for it, and a
/// second list cannot drift out of step with the first.
#[cfg(feature = "parse")]
pub(crate) fn has_markup_half(grammar: &str) -> bool {
    crate::languages::LANGUAGE_SPECS
        .iter()
        .filter(|spec| spec.grammar == grammar)
        .any(|spec| {
            spec.embedded
                .iter()
                .any(|name| *name == "css" || *name == "html")
        })
}

/// Read a template file's markup and stylesheet halves and merge them into
/// `extraction`.
///
/// Runs **after** [`crate::embedded::merge_embedded_scripts`] and after the
/// duplicate-callee pass, for two reasons that both come from the symbol table:
/// a script-region selector use is attributed to the function it sits inside,
/// which needs that function to be in `extraction.symbols` already; and a
/// `ReferenceKind::Selector` must not be offered to a dedup pass written about
/// callee names, whose namespace it does not share.
///
/// Every bound that cut something is disclosed twice over — a sentence in
/// `diagnostics` for a human, and, for anything that leaves *bytes unread*, a
/// range folded into [`ParseOutcome::Partial`], because `for_durable_store`
/// clears diagnostics and the durable record has to be the range. That is the
/// same contract [`crate::embedded`] keeps, through the same function, so a file
/// can never report a clean read over text nothing read.
#[cfg(feature = "parse")]
pub(crate) fn merge_markup(
    extraction: &mut crate::model::Extraction,
    root: tree_sitter::Node,
    source: &str,
    grammar: &str,
) {
    if !has_markup_half(grammar) {
        return;
    }
    let mut scan = scan_template_tree(&extraction.file_path, root, source);
    let declared = scan.declared_names();
    let budget = MAX_MARKUP_REFERENCES.saturating_sub(scan.references.len());
    let (script_references, truncated) =
        selector_references(source, &scan.script_regions, &declared, budget);
    scan.references.extend(script_references);
    scan.truncated_references += truncated;
    absorb_scan(extraction, scan);
}

/// Merge a completed scan into an extraction, attributing every use and
/// disclosing every bound.
///
/// Behind `parse` because the two disclosure helpers it delegates to —
/// `fold_unparsed_ranges` and `reorder_after_merge` — live in
/// [`crate::embedded`], which is gated on the grammars. The whole-file paths do
/// not come through here: they are read before an `Extraction` exists and
/// disclose through [`crate::fallback`]'s own result type, so the feature-off
/// build that an embedder links needs neither this function nor those helpers.
/// Ungated, this compiled only by accident of `parse` being a default feature.
#[cfg(feature = "parse")]
pub(crate) fn absorb_scan(extraction: &mut crate::model::Extraction, scan: MarkupScan) {
    let mut diagnostics: Vec<String> = Vec::new();
    if scan.truncated_symbols > 0 {
        diagnostics.push(format!(
            "{} markup/stylesheet declaration(s) past the {MAX_MARKUP_SYMBOLS} cap were not \
             indexed; this file's list of them is a prefix, not a set",
            scan.truncated_symbols
        ));
    }
    if scan.truncated_references > 0 {
        diagnostics.push(format!(
            "{} markup/stylesheet use(s) past the {MAX_MARKUP_REFERENCES} cap were not indexed",
            scan.truncated_references
        ));
    }
    for range in &scan.unread {
        diagnostics.push(format!(
            "markup/stylesheet bytes {}..{} were not read: a byte cap in `markup` stopped the \
             scanner there",
            range.start_byte, range.end_byte
        ));
    }

    extraction.symbols.extend(scan.symbols);
    let mut references = scan.references;
    attribute_uses(&mut references, &extraction.symbols, &extraction.file_path);
    extraction.references.extend(references);
    extraction.diagnostics.extend(diagnostics);
    crate::embedded::fold_unparsed_ranges(&mut extraction.parse_outcome, scan.unread);
    crate::embedded::reorder_after_merge(extraction);
}

/// Give every use an owner: the innermost declaration containing it, else the
/// file.
///
/// The one owner of that rule, called from both paths that produce uses — the
/// template merge here, and the whole-file `.css`/`.html` reader in
/// [`crate::fallback`]. It was not, once: the whole-file path left
/// `enclosing_symbol` at `None` and relied on the resolver's own
/// `unwrap_or(file_path)` default. The answer was the same and the property was
/// not — a use with no owner is a use whose edge source is supplied by whichever
/// consumer reads it next, and the stress corpus caught it on the first
/// unterminated `<style>` in a `.css` file.
pub fn attribute_uses(
    references: &mut [ExtractedReference],
    symbols: &[ExtractedSymbol],
    file_path: &str,
) {
    for reference in references {
        reference.enclosing_symbol =
            Some(enclosing_symbol_for(&reference.span, symbols, file_path));
    }
}

/// The qualified name of the innermost declaration containing `span`.
///
/// A selector string inside `toggleAddMenu` is that function's use of the
/// anchor, not the file's, and an edge whose source is the file says only that
/// the name appears somewhere in it. Markup outside every declaration — which is
/// most markup — is attributed to the file, which is what
/// [`ExtractedReference::enclosing_symbol`]'s `None` already meant; it is set
/// explicitly so the value is a fact this pass computed rather than a default
/// another layer supplies.
///
/// `File` and the two kinds this module produces are excluded as candidates: a
/// `File` node spans the whole file and would win every comparison, and a
/// stylesheet rule containing a use of a custom property is not a *declaration
/// site* for that use in the sense an edge source means.
fn enclosing_symbol_for(span: &Span, symbols: &[ExtractedSymbol], file_path: &str) -> String {
    let mut best: Option<&ExtractedSymbol> = None;
    for symbol in symbols {
        if matches!(
            symbol.kind,
            SymbolKind::File | SymbolKind::MarkupAnchor | SymbolKind::StyleRule
        ) {
            continue;
        }
        if symbol.span.start_byte > span.start_byte || symbol.span.end_byte < span.end_byte {
            continue;
        }
        let width = symbol.span.end_byte.saturating_sub(symbol.span.start_byte);
        match best {
            Some(current)
                if current
                    .span
                    .end_byte
                    .saturating_sub(current.span.start_byte)
                    <= width => {}
            _ => best = Some(symbol),
        }
    }
    best.map(|symbol| symbol.qualified_name.clone())
        .unwrap_or_else(|| file_path.to_string())
}
