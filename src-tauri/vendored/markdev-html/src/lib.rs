//! Safe, standalone HTML export.
//!
//! Raw HTML is valid CommonMark, but an exported note is commonly opened in a
//! browser where passing it through would turn note text into executable code.
//! This renderer deliberately treats raw HTML as text, rejects active URL
//! schemes, and adds a closed Content Security Policy around the rendered
//! fragment.
//!
//! On top of CommonMark it renders MarkDev's Obsidian-flavoured dialect the
//! way Obsidian's reading view does: callouts (foldable ones as `<details>`,
//! so no script is needed), `==highlights==`, `#tags`, `%%comments%%`
//! removed, `^[inline footnotes]`, `^block-id` anchors, custom task statuses,
//! sized pictures, audio/video embeds, and `![[note]]` transclusion.

use std::borrow::Cow;
use std::collections::{HashMap, VecDeque};
use std::fs::{self, File};
use std::io::{self, Read, Write};
use std::ops::Range;
use std::path::{Component, Path, PathBuf};

use pulldown_cmark::{html, CodeBlockKind, CowStr, Event, LinkType, Parser, Tag, TagEnd};

use markdev_md::model::{BlockKind, CalloutKind, SpanKind, Utf16Mapper, CALLOUT_FOLD_COLLAPSED};
use markdev_md::obsidian;
use markdev_md::parse::{
    display_math_is_valid, inline_math_is_valid, options, parse_checked, scan_tags,
};

pub mod site;

/// Rendering is linear but necessarily allocates output proportional to the
/// document. Refuse pathological inputs before duplicating them across the
/// Rust/Swift boundary.
pub const MAX_SOURCE_BYTES: usize = 16 * 1_048_576;
/// Titles normally come from a filesystem component (at most a few hundred
/// bytes), but the renderer is a public FFI boundary and must defend itself.
pub const MAX_TITLE_BYTES: usize = 8 * 1024;
const MAX_OUTPUT_BYTES: usize = 64 * 1024 * 1024;
/// The largest single local picture copied into an export. Matches the order
/// of magnitude the native renderer accepts for raster input.
pub const MAX_EMBEDDED_IMAGE_BYTES: usize = 8 * 1_048_576;
/// Total raw picture bytes one export may copy. Base64 grows this by a third,
/// which still leaves the text of the note well inside the output ceiling.
pub const MAX_EMBEDDED_TOTAL_BYTES: usize = 32 * 1_048_576;
/// The largest note one `![[embed]]` transcludes.
pub const MAX_TRANSCLUDED_NOTE_BYTES: usize = 2 * 1_048_576;
/// Total note source all transclusions in one export may read, so a note
/// embedding the same large note a thousand times stays bounded.
pub const MAX_TRANSCLUDED_TOTAL_BYTES: usize = 8 * 1_048_576;
/// How deep `![[A]]` → `![[B]]` → … nests before later embeds become links.
pub const MAX_EMBED_DEPTH: usize = 3;
/// Files examined when searching a vault for an attachment or note by name.
const MAX_VAULT_SCAN_ENTRIES: usize = 50_000;
/// Folders walked upwards from a note looking for its vault root.
const MAX_VAULT_ROOT_ASCENT: usize = 8;

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum HTMLExportError {
    SourceTooLarge { actual: usize, maximum: usize },
    TitleTooLarge { actual: usize, maximum: usize },
    OutputTooLarge { actual: usize, maximum: usize },
}

/// Optional behaviour for [`render_document_with_options`].
#[derive(Clone, Debug, Default)]
pub struct ExportOptions<'a> {
    /// The folder the note lives in. When set, local images referenced with
    /// Markdown image syntax are read, identified by their bytes, and copied
    /// into the document as `data:` URIs so the export opens correctly in any
    /// browser, from any location, and `![[note]]` embeds are transcluded.
    /// Pictures that cannot be identified as a browser image format keep
    /// their original relative destination.
    pub asset_base: Option<&'a Path>,
    /// The vault the note belongs to. Obsidian resolves `![[picture.png]]`
    /// and `![[Note]]` by name anywhere in the vault; this bounds that search.
    /// When absent, the nearest folder above the note holding `.obsidian` or
    /// `.git` is used, else the note's own folder.
    pub vault_root: Option<&'a Path>,
    /// Where local links and media point from. Defaults to the note's
    /// folder, which is right when the page is saved beside the note.
    pub link_base: LinkBase<'a>,
    /// Set when the page is one of a whole-vault site: links to notes in the
    /// vault point at their `.html` pages in the site instead of `.md` files.
    pub site: Option<SiteLayout<'a>>,
    /// Which files the render may read — the pictures it embeds and the
    /// notes it transcludes. Unrestricted by default, which is right for a
    /// user's own notes; an embedder rendering Markdown nobody vetted, such
    /// as a cloned repository's README, should use [`FileAccess::Vault`].
    pub file_access: FileAccess,
    /// Whether pictures, audio and video may load from `http(s)` URLs. Off by
    /// default: a remote picture is a request the reader never made, which a
    /// shared note can use as a beacon. Links are unaffected — following one
    /// takes a click. Remote pictures are always sent without a referrer.
    ///
    /// Off, a remote embed renders with `src="#"` — the value a refused
    /// scheme also gets — plus `data-withheld-src` carrying its address, so
    /// an embedder can tell the two apart and say which picture was held
    /// back. A `data-` attribute is never fetched.
    pub remote_media: bool,
    /// Most raw picture bytes the render copies in, across every picture;
    /// `None` is [`MAX_EMBEDDED_TOTAL_BYTES`], sized for a one-off export. A
    /// viewer that re-renders as the note is edited wants far less: every
    /// byte embedded is read, base64-encoded and handed over on each render.
    /// Pictures past the budget keep their relative destination. Never more
    /// than [`MAX_EMBEDDED_TOTAL_BYTES`], whatever is asked.
    pub max_embedded_bytes: Option<usize>,
}

/// Which files a render may read.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum FileAccess {
    /// Any file the note names, including absolute paths and `file:` URLs.
    #[default]
    Unrestricted,
    /// Only regular files whose canonical path — every symlink resolved —
    /// lies inside the canonical [`ExportOptions::vault_root`]. Absolute
    /// paths, `file:` URLs, `../` and symlinks that leave the vault resolve
    /// to nothing, exactly as a missing file does. Without an explicit vault
    /// root nothing is read: the vault is never guessed from the folders
    /// above the note, because that guess could climb out of what was vetted.
    ///
    /// Confinement is of *contents*: resolving a name may ask whether a path
    /// outside the vault exists, but nothing outside is ever opened, and the
    /// answer never reaches the output — a refused file renders exactly as a
    /// missing one.
    Vault,
    /// No file is read: no picture is embedded and no note is transcluded.
    None,
}

/// A rendered note body, for embedding in an application's own page.
#[derive(Clone, Debug, Default, PartialEq, Eq, serde::Serialize)]
pub struct Fragment {
    /// Sanitized HTML for the body, without a document shell or stylesheet.
    ///
    /// It carries no script, no event-handler attribute and no author HTML
    /// (raw HTML is shown as text), and every URL has passed the same scheme
    /// filter as [`render_document`]. What it lacks is the export's closed
    /// Content-Security-Policy: the embedding page supplies its own.
    pub html: String,
    /// The note's own headings in document order, each with the `id` its
    /// element carries in [`Fragment::html`], so an outline built from this
    /// list always lands on the heading it names. Headings inside
    /// transcluded notes are not listed: they belong to another note.
    pub headings: Vec<FragmentHeading>,
    /// The text of the note's frontmatter block, between its fences, when it
    /// opens with one. Not rendered into [`Fragment::html`].
    pub frontmatter: Option<String>,
}

/// One heading of a [`Fragment`].
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize)]
pub struct FragmentHeading {
    /// 1 to 6.
    pub level: u8,
    /// The heading's text, without markup or a trailing `^block-id`.
    pub text: String,
    /// The `id` attribute of the heading element.
    pub id: String,
}

/// Where a whole-vault site mirrors the vault.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SiteLayout<'a> {
    pub vault_root: &'a Path,
    pub output_root: &'a Path,
}

/// How an exported page refers to local files it does not embed: other
/// notes, audio and video, PDFs, and pictures too large to copy in.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum LinkBase<'a> {
    /// Relative to the note's own folder.
    #[default]
    NoteFolder,
    /// Relative to the folder the page is written to.
    Directory(&'a Path),
    /// Absolute `file://` URLs, for a page in a temporary location such as
    /// a browser preview.
    FileUrl,
}

/// Renders a complete browser-ready document from MarkDev's Markdown dialect.
pub fn render_document(source: &str, title: &str) -> Result<String, HTMLExportError> {
    render_document_with_options(source, title, &ExportOptions::default())
}

/// [`render_document`] with explicit export options.
pub fn render_document_with_options(
    source: &str,
    title: &str,
    export: &ExportOptions<'_>,
) -> Result<String, HTMLExportError> {
    if source.len() > MAX_SOURCE_BYTES {
        return Err(HTMLExportError::SourceTooLarge {
            actual: source.len(),
            maximum: MAX_SOURCE_BYTES,
        });
    }
    if title.len() > MAX_TITLE_BYTES {
        return Err(HTMLExportError::TitleTooLarge {
            actual: title.len(),
            maximum: MAX_TITLE_BYTES,
        });
    }

    let clean_source = without_nul(source);
    let clean_title = without_nul(title);

    // Render straight into a writer that refuses the first byte beyond the
    // ceiling. Checking a String after `push_html` returns is too late: the
    // pathological allocation has already happened by then.
    let initial_capacity = clean_source
        .len()
        .saturating_add(clean_title.len())
        .saturating_add(16 * 1024)
        .min(MAX_OUTPUT_BYTES);
    let mut output = BoundedHTMLWriter::new(MAX_OUTPUT_BYTES, initial_capacity);
    output
        .write_all(DOCUMENT_PREFIX.as_bytes())
        .map_err(|_| output.error())?;
    html::write_html_io(
        &mut output,
        std::iter::once(Event::Text(CowStr::Borrowed(clean_title.as_ref()))),
    )
    .map_err(|_| output.error())?;
    output
        .write_all(DOCUMENT_MIDDLE.as_bytes())
        .map_err(|_| output.error())?;
    write_body(&mut output, clean_source.as_ref(), export)?;
    output
        .write_all(DOCUMENT_SUFFIX.as_bytes())
        .map_err(|_| output.error())?;

    // Every input to the writer is UTF-8 text generated by pulldown-cmark or
    // by this module's own markup.
    Ok(String::from_utf8(output.into_bytes()).expect("HTML writer emitted invalid UTF-8"))
}

/// Renders a note body for embedding in an application's own page.
///
/// The same pipeline as [`render_document_with_options`] — one dialect, one
/// sanitizer, one set of limits — without the document shell, plus the
/// heading outline and frontmatter an embedder would otherwise have to
/// re-derive (and would derive differently: a second slugger is how an
/// outline entry comes to point at a heading id that does not exist).
pub fn render_fragment(
    source: &str,
    export: &ExportOptions<'_>,
) -> Result<Fragment, HTMLExportError> {
    if source.len() > MAX_SOURCE_BYTES {
        return Err(HTMLExportError::SourceTooLarge {
            actual: source.len(),
            maximum: MAX_SOURCE_BYTES,
        });
    }
    let clean_source = without_nul(source);
    let initial_capacity = clean_source
        .len()
        .saturating_add(16 * 1024)
        .min(MAX_OUTPUT_BYTES);
    let mut output = BoundedHTMLWriter::new(MAX_OUTPUT_BYTES, initial_capacity);
    let body = write_body(&mut output, clean_source.as_ref(), export)?;
    Ok(Fragment {
        // As above: everything written is UTF-8 from pulldown-cmark or this
        // module's own markup.
        html: String::from_utf8(output.into_bytes()).expect("HTML writer emitted invalid UTF-8"),
        headings: body.headings,
        frontmatter: body.frontmatter,
    })
}

/// Keeps output NUL-free even though Rust and Swift strings allow it,
/// without copying the ordinary case. Callers bound the input first, so the
/// replacement allocation is bounded too.
fn without_nul(text: &str) -> Cow<'_, str> {
    if text.contains('\0') {
        Cow::Owned(text.replace('\0', "\u{FFFD}"))
    } else {
        Cow::Borrowed(text)
    }
}

/// What rendering a body learned besides its HTML.
struct Body {
    headings: Vec<FragmentHeading>,
    frontmatter: Option<String>,
}

/// Renders `source` into `output`: the one body pipeline both entry points
/// share.
fn write_body(
    output: &mut BoundedHTMLWriter,
    source: &str,
    export: &ExportOptions<'_>,
) -> Result<Body, HTMLExportError> {
    let mut context = ExportContext::new(export);
    let prepared = prepare_source(source);
    let mut events = ExportEvents::new(
        prepared.text.as_ref(),
        prepared.math,
        export.asset_base.map(Path::to_path_buf),
        &mut context,
    );
    html::write_html_io(&mut *output, &mut events).map_err(|_| output.error())?;
    Ok(Body {
        headings: std::mem::take(&mut events.outline),
        frontmatter: events.frontmatter.take(),
    })
}

/// Math pulldown-cmark does not see (`\(…\)`, `\[…\]`), lifted out of the
/// source before parsing and put back when its placeholder is reached.
#[derive(Clone, Debug)]
struct MathToken {
    latex: String,
    display: bool,
}

/// Opens and closes a math placeholder. Private-use characters, so they
/// cannot collide with anything pulldown-cmark treats as syntax; authored
/// occurrences are replaced before placeholders are inserted.
const MATH_OPEN: char = '\u{E000}';
const MATH_CLOSE: char = '\u{E001}';
/// Stand in for the `==` of a highlight the editor recognises, so export
/// highlights exactly what the editor does — including `==**bold**==`,
/// whose content pulldown-cmark splits across several events.
const MARK_OPEN: char = '\u{E002}';
const MARK_CLOSE: char = '\u{E003}';
const PLACEHOLDERS: [char; 4] = [MATH_OPEN, MATH_CLOSE, MARK_OPEN, MARK_CLOSE];

/// Source ready to render, and the math lifted out of it.
struct Prepared<'s> {
    text: Cow<'s, str>,
    math: Vec<MathToken>,
}

/// Rewrites syntax CommonMark would otherwise print literally:
/// `%%comments%%` are removed, `^[inline footnotes]` become ordinary
/// footnote references whose definitions are appended (so the browser gets
/// numbered notes at the foot of the page exactly as Obsidian shows them),
/// and the editor's `\(…\)` / `\[…\]` math becomes placeholders.
fn prepare_source(source: &str) -> Prepared<'_> {
    let has_comment = source.contains("%%");
    let has_inline_note = source.contains("^[");
    let has_delimited_math = source.contains("\\(") || source.contains("\\[");
    let has_highlight = source.contains("==");
    let has_placeholder_chars = source.contains(PLACEHOLDERS);
    if !has_comment
        && !has_inline_note
        && !has_delimited_math
        && !has_highlight
        && !has_placeholder_chars
    {
        return Prepared {
            text: Cow::Borrowed(source),
            math: Vec::new(),
        };
    }

    let mut text: Cow<'_, str> = if has_placeholder_chars {
        Cow::Owned(source.replace(PLACEHOLDERS, "\u{FFFD}"))
    } else {
        Cow::Borrowed(source)
    };
    if has_comment {
        let literal = obsidian::verbatim_ranges(&text);
        let comments = obsidian::comment_ranges(&text, &literal);
        if !comments.is_empty() {
            let mut stripped = String::with_capacity(text.len());
            let mut at = 0;
            for comment in comments {
                stripped.push_str(&text[at..comment.start]);
                at = comment.end;
            }
            stripped.push_str(&text[at..]);
            text = Cow::Owned(stripped);
        }
    }

    if has_inline_note {
        let literal = obsidian::verbatim_ranges(&text);
        let notes = obsidian::inline_footnotes(&text, &literal);
        if !notes.is_empty() {
            let mut rewritten = String::with_capacity(text.len() + notes.len() * 32);
            let mut definitions = String::new();
            let mut at = 0;
            for (index, note) in notes.iter().enumerate() {
                let label = format!("markdev-inline-{}", index + 1);
                rewritten.push_str(&text[at..note.full.start]);
                rewritten.push_str("[^");
                rewritten.push_str(&label);
                rewritten.push(']');
                at = note.full.end;
                let body: String = text[note.inner.clone()]
                    .chars()
                    .map(|c| if c == '\n' || c == '\r' { ' ' } else { c })
                    .collect();
                definitions.push_str("\n[^");
                definitions.push_str(&label);
                definitions.push_str("]: ");
                definitions.push_str(body.trim());
                definitions.push('\n');
            }
            rewritten.push_str(&text[at..]);
            rewritten.push_str("\n\n");
            rewritten.push_str(&definitions);
            text = Cow::Owned(rewritten);
        }
    }

    let mut math = Vec::new();
    if has_delimited_math || has_highlight {
        if let Ok(parsed) = parse_checked(&text) {
            // (range to replace, replacement): math placeholders and the
            // delimiters of every highlight the editor draws.
            let mut edits: Vec<(Range<usize>, String)> = Vec::new();
            if has_delimited_math {
                for (full, inner, display) in delimited_math_ranges(&text, &parsed) {
                    let token = format!("{MATH_OPEN}{}{MATH_CLOSE}", math.len());
                    math.push(MathToken {
                        latex: text[inner].to_owned(),
                        display,
                    });
                    edits.push((full, token));
                }
            }
            if has_highlight {
                let mapper = Utf16Mapper::new(&text);
                for span in &parsed.spans {
                    if span.kind != SpanKind::Highlight as u16 {
                        continue;
                    }
                    let inner = mapper.to_byte(span.start)..mapper.to_byte(span.end);
                    if inner.start < 2
                        || text.get(inner.start - 2..inner.start) != Some("==")
                        || text.get(inner.end..inner.end + 2) != Some("==")
                    {
                        continue;
                    }
                    edits.push((inner.start - 2..inner.start, MARK_OPEN.to_string()));
                    edits.push((inner.end..inner.end + 2, MARK_CLOSE.to_string()));
                }
            }
            edits.sort_by_key(|(range, _)| range.start);
            let mut rewritten = String::with_capacity(text.len());
            let mut at = 0;
            for (range, replacement) in edits {
                if range.start < at {
                    continue;
                }
                rewritten.push_str(&text[at..range.start]);
                rewritten.push_str(&replacement);
                at = range.end;
            }
            rewritten.push_str(&text[at..]);
            text = Cow::Owned(rewritten);
        }
    }
    Prepared { text, math }
}

/// `\(…\)`, `\[…\]` and their Markdown-escaped `\\(…\\)` forms, exactly as
/// the editor recognises them: full range, formula range, display.
fn delimited_math_ranges(
    text: &str,
    parsed: &markdev_md::ParseResult,
) -> Vec<(Range<usize>, Range<usize>, bool)> {
    let mapper = Utf16Mapper::new(text);
    let mut found: Vec<(Range<usize>, Range<usize>, bool)> = Vec::new();
    for span in &parsed.spans {
        if span.kind != SpanKind::InlineMath as u16 {
            continue;
        }
        let inner = mapper.to_byte(span.start)..mapper.to_byte(span.end);
        let before = &text[..inner.start];
        let after = &text[inner.end..];
        let open = if before.ends_with("\\\\(") {
            3
        } else if before.ends_with("\\(") {
            2
        } else {
            continue;
        };
        let close = if after.starts_with("\\\\)") {
            3
        } else if after.starts_with("\\)") {
            2
        } else {
            continue;
        };
        found.push((inner.start - open..inner.end + close, inner, false));
    }
    for block in &parsed.blocks {
        if block.kind != BlockKind::MathBlock as u16 {
            continue;
        }
        let full = mapper.to_byte(block.start)..mapper.to_byte(block.end);
        let body = &text[full.clone()];
        let open = if body.starts_with("\\\\[") {
            3
        } else if body.starts_with("\\[") {
            2
        } else {
            continue;
        };
        let trimmed = body.trim_end();
        let close = if trimmed.ends_with("\\\\]") {
            3
        } else if trimmed.ends_with("\\]") {
            2
        } else {
            continue;
        };
        let end = full.start + trimmed.len();
        if end < full.start + open + close {
            continue;
        }
        found.push((full.start..end, full.start + open..end - close, true));
    }
    found.sort_by_key(|(full, _, _)| full.start);
    let mut disjoint: Vec<(Range<usize>, Range<usize>, bool)> = Vec::new();
    for item in found {
        if disjoint
            .last()
            .is_some_and(|(last, _, _)| item.0.start < last.end)
        {
            continue;
        }
        disjoint.push(item);
    }
    disjoint
}

/// Largest formula typeset; longer ones stay as source.
#[cfg(feature = "mathml")]
const MAX_MATH_BYTES: usize = 16 * 1024;
/// Deepest `{…}` nesting typeset, so a hostile formula cannot recurse deep.
#[cfg(feature = "mathml")]
const MAX_MATH_NESTING: usize = 48;

/// A formula as browser-native MathML, or `None` when it is not something
/// the typesetter fully understands — the caller then shows the source, the
/// same bargain the editor makes.
#[cfg(feature = "mathml")]
fn render_math(latex: &str, display: bool) -> Option<String> {
    use pulldown_latex::config::DisplayMode;
    use pulldown_latex::{push_mathml, Parser as LatexParser, RenderConfig, Storage};

    if latex.trim().is_empty() || latex.len() > MAX_MATH_BYTES {
        return None;
    }
    let mut depth = 0usize;
    for byte in latex.bytes() {
        match byte {
            b'{' => {
                depth += 1;
                if depth > MAX_MATH_NESTING {
                    return None;
                }
            }
            b'}' => depth = depth.saturating_sub(1),
            _ => {}
        }
    }
    // A typesetting bug must cost one formula, not the export.
    let rendered = std::panic::catch_unwind(|| {
        let storage = Storage::new();
        let mut events: Vec<_> = LatexParser::new(latex, &storage).collect();
        if events.iter().any(Result::is_err) {
            return None;
        }
        for event in events.iter_mut().flatten() {
            bound_spacing(event);
        }
        let config = RenderConfig {
            display_mode: if display {
                DisplayMode::Block
            } else {
                DisplayMode::Inline
            },
            // The library writes annotations unescaped; never ask for one.
            annotation: None,
            ..RenderConfig::default()
        };
        let mut out = String::new();
        push_mathml(&mut out, events.into_iter(), config).ok()?;
        Some(out)
    })
    .ok()
    .flatten()?;
    sanitize_mathml(&rendered)
}

/// Most space one LaTeX spacing command may add or take away, in em.
///
/// pulldown-latex writes the author's `\hspace`, `\kern` and `\\[…]` amounts
/// straight into the page — `\hspace{-1000em}` becomes `margin-left: -1000em`
/// — so a formula could drag its neighbours, or itself, anywhere on the page
/// and lay text over text the reader trusts. Real typesetting stays well
/// inside this: `\qquad` is 2em and `\hspace{2cm}` under 6em.
#[cfg(feature = "mathml")]
const MAX_MATH_SPACE_EM: f32 = 8.0;

/// Bounds the author-chosen dimensions in one LaTeX event to
/// [`MAX_MATH_SPACE_EM`]; every other event is left as written.
#[cfg(feature = "mathml")]
fn bound_spacing(event: &mut pulldown_latex::Event<'_>) {
    use pulldown_latex::event::{EnvironmentFlow, Event};
    match event {
        Event::Space {
            width,
            height,
            depth,
        } => {
            for dimension in [width, height, depth] {
                bound_dimension(dimension);
            }
        }
        Event::EnvironmentFlow(EnvironmentFlow::NewLine { spacing, .. }) => {
            bound_dimension(spacing);
        }
        _ => {}
    }
}

#[cfg(feature = "mathml")]
fn bound_dimension(dimension: &mut Option<pulldown_latex::event::Dimension>) {
    use pulldown_latex::event::{Dimension, DimensionUnit};
    let Some(given) = *dimension else {
        return;
    };
    // TeX's own ratios at a 10pt em (the TeXbook, p. 57): 1em = 10pt = 18mu.
    // `ex` depends on the font; 0.431 is Computer Modern's.
    let em_per_unit = match given.unit {
        DimensionUnit::Em => 1.0,
        DimensionUnit::Mu => 1.0 / 18.0,
        DimensionUnit::Ex => 0.431,
        DimensionUnit::Pt => 0.1,
        DimensionUnit::Pc => 1.2,
        DimensionUnit::In => 7.227,
        DimensionUnit::Bp => 0.100_375,
        DimensionUnit::Cm => 2.845_276,
        DimensionUnit::Mm => 0.284_528,
        DimensionUnit::Dd => 0.107,
        DimensionUnit::Cc => 1.284,
        DimensionUnit::Sp => 0.1 / 65_536.0,
    };
    let em = given.value * em_per_unit;
    if em.is_finite() && em.abs() <= MAX_MATH_SPACE_EM {
        return;
    }
    let bounded = if em.is_finite() {
        em.clamp(-MAX_MATH_SPACE_EM, MAX_MATH_SPACE_EM)
    } else {
        0.0
    };
    *dimension = Some(Dimension::new(bounded, DimensionUnit::Em));
}

#[cfg(not(feature = "mathml"))]
fn render_math(_latex: &str, _display: bool) -> Option<String> {
    None
}

/// Elements a typeset formula may contain.
#[cfg(feature = "mathml")]
const MATHML_ELEMENTS: &[&str] = &[
    "math",
    "semantics",
    "mrow",
    "mi",
    "mn",
    "mo",
    "ms",
    "mtext",
    "mspace",
    "msup",
    "msub",
    "msubsup",
    "mfrac",
    "msqrt",
    "mroot",
    "munder",
    "mover",
    "munderover",
    "mtable",
    "mtr",
    "mtd",
    "mlabeledtr",
    "mstyle",
    "mpadded",
    "mphantom",
    "menclose",
    "mmultiscripts",
    "mprescripts",
    "none",
];

/// Re-validates typesetter output against an allowlist.
///
/// The generated markup is not trusted to be well-formed HTML: it writes
/// comparison operators as a bare `<mo><</mo>`. Tags must be allowlisted
/// MathML with plain attributes (no event handlers, links, or `url()`
/// styles); every other `<`, `>` or `&` is escaped. Anything that does not
/// fit returns `None`, and the caller shows the formula's source instead.
#[cfg(feature = "mathml")]
fn sanitize_mathml(markup: &str) -> Option<String> {
    let bytes = markup.as_bytes();
    let mut out = String::with_capacity(markup.len() + 32);
    let mut i = 0;
    while i < bytes.len() {
        match bytes[i] {
            b'<' => {
                let closing = bytes.get(i + 1) == Some(&b'/');
                let name_start = i + 1 + usize::from(closing);
                let mut j = name_start;
                while j < bytes.len() && bytes[j].is_ascii_alphabetic() {
                    j += 1;
                }
                let name = &markup[name_start..j];
                if name.is_empty() || !MATHML_ELEMENTS.contains(&name) {
                    out.push_str("&lt;");
                    i += 1;
                    continue;
                }
                let end = i + markup[i..].find('>')?;
                let attributes = markup[j..end].trim_end_matches('/').trim();
                if closing && !attributes.is_empty() {
                    return None;
                }
                if !attributes_are_safe(attributes) {
                    return None;
                }
                out.push_str(&markup[i..=end]);
                i = end + 1;
            }
            b'>' => {
                out.push_str("&gt;");
                i += 1;
            }
            b'&' => {
                let entity = markup[i + 1..].find(';').filter(|&n| {
                    n > 0
                        && n <= 10
                        && markup[i + 1..i + 1 + n]
                            .bytes()
                            .all(|b| b.is_ascii_alphanumeric() || b == b'#')
                });
                match entity {
                    Some(_) => out.push('&'),
                    None => out.push_str("&amp;"),
                }
                i += 1;
            }
            _ => {
                let next = markup[i..]
                    .find(['<', '>', '&'])
                    .map_or(markup.len(), |n| i + n);
                out.push_str(&markup[i..next]);
                i = next;
            }
        }
    }
    Some(out)
}

/// `name="value"` pairs with inert names and values.
#[cfg(feature = "mathml")]
fn attributes_are_safe(attributes: &str) -> bool {
    let mut rest = attributes;
    while !rest.is_empty() {
        let Some(eq) = rest.find('=') else {
            return false;
        };
        let name = rest[..eq].trim().to_ascii_lowercase();
        if name.is_empty()
            || !name.bytes().all(|b| b.is_ascii_alphabetic() || b == b'-')
            || name.starts_with("on")
            || matches!(name.as_str(), "href" | "src" | "xlink" | "xmlns")
        {
            return false;
        }
        let after = rest[eq + 1..].trim_start();
        let Some(value_and_rest) = after.strip_prefix('"') else {
            return false;
        };
        let Some(close) = value_and_rest.find('"') else {
            return false;
        };
        let value = &value_and_rest[..close];
        let lower = value.to_ascii_lowercase();
        if value.contains(['<', '>', '\\', '@'])
            || lower.contains("url")
            || lower.contains("expression")
            || lower.contains("javascript")
        {
            return false;
        }
        rest = value_and_rest[close + 1..].trim_start();
    }
    true
}

/// A formula for the page: MathML when it typesets, else its source.
fn math_html(latex: &str, display: bool) -> String {
    let class = if display {
        "math math-display"
    } else {
        "math math-inline"
    };
    match render_math(latex, display) {
        Some(mathml) => format!("<span class=\"{class}\">{mathml}</span>"),
        None => format!(
            "<span class=\"{class} math-source\">{}</span>",
            escape_html(latex)
        ),
    }
}

/// State shared by the top-level render and every transcluded note.
struct ExportContext {
    images: ImageEmbedder,
    transclusion_remaining: usize,
    depth: usize,
    /// Notes currently being rendered, so `A` embedding `B` embedding `A`
    /// becomes a link instead of a loop.
    visited: Vec<PathBuf>,
    vault_root: Option<PathBuf>,
    vault_files: Option<std::sync::Arc<Vec<PathBuf>>>,
    /// `(vault root, site root)` when rendering one page of a vault site.
    site: Option<(PathBuf, PathBuf)>,
    /// The folder links are written relative to, or `None` for `file://`.
    link_from: Option<PathBuf>,
    /// Whether links should be absolute `file://` URLs.
    file_urls: bool,
    access: FileAccess,
    /// The vault root with every symlink resolved, for [`FileAccess::Vault`].
    /// `None` there means no root was given or it cannot be resolved, and
    /// then nothing is read.
    vault_canonical: Option<PathBuf>,
    remote_media: bool,
}

impl ExportContext {
    fn new(export: &ExportOptions<'_>) -> Self {
        let base = export.asset_base;
        let (link_from, file_urls) = match export.link_base {
            LinkBase::NoteFolder => (base.map(absolute_path), false),
            LinkBase::Directory(dir) => (Some(absolute_path(dir)), false),
            LinkBase::FileUrl => (None, true),
        };
        let vault_root = match (export.vault_root, export.file_access) {
            (Some(root), FileAccess::Vault) => Some(absolute_path(root)),
            (Some(root), _) => Some(root.to_path_buf()),
            // Guessing the vault by walking up from the note is a convenience
            // for a user's own notes; under confinement it could choose a
            // folder above the one that was vetted.
            (None, FileAccess::Unrestricted) => base.and_then(find_vault_root),
            (None, _) => None,
        };
        let vault_canonical = match export.file_access {
            FileAccess::Vault => vault_root.as_deref().and_then(|r| fs::canonicalize(r).ok()),
            _ => None,
        };
        Self {
            images: ImageEmbedder::new(
                export
                    .max_embedded_bytes
                    .unwrap_or(MAX_EMBEDDED_TOTAL_BYTES)
                    .min(MAX_EMBEDDED_TOTAL_BYTES),
            ),
            transclusion_remaining: MAX_TRANSCLUDED_TOTAL_BYTES,
            depth: 0,
            visited: Vec::new(),
            vault_root,
            vault_files: None,
            site: export.site.map(|site| {
                (
                    absolute_path(site.vault_root),
                    absolute_path(site.output_root),
                )
            }),
            link_from,
            file_urls,
            access: export.file_access,
            vault_canonical,
            remote_media: export.remote_media,
        }
    }

    /// The one gate every file read passes: the path to read `path` through,
    /// or `None` when [`FileAccess`] forbids it.
    ///
    /// Under [`FileAccess::Vault`] the file is resolved through every
    /// symlink and must land inside the canonical vault root as a regular
    /// file. The path returned is that location re-expressed under the vault
    /// root as given, so hrefs computed from it agree with hrefs computed
    /// from the note's own folder, and opening it follows no symlink below
    /// the root.
    fn admit(&self, path: &Path) -> Option<PathBuf> {
        match self.access {
            FileAccess::Unrestricted => Some(path.to_path_buf()),
            FileAccess::None => None,
            FileAccess::Vault => {
                let root = self.vault_canonical.as_ref()?;
                let given = self.vault_root.as_ref()?;
                let canonical = fs::canonicalize(path).ok()?;
                let inside = canonical.strip_prefix(root).ok()?;
                if !fs::metadata(&canonical).ok()?.is_file() {
                    return None;
                }
                Some(given.join(inside))
            }
        }
    }

    /// How the page refers to a note: its page in the site when rendering a
    /// vault site, else the note's own file.
    fn note_href(&self, path: &Path) -> String {
        if let Some((vault, output)) = &self.site {
            let absolute = absolute_path(path);
            let is_note = absolute
                .extension()
                .map(|e| e.to_string_lossy().to_ascii_lowercase())
                .is_some_and(|e| e == "md" || e == "markdown");
            if let (true, Ok(relative)) = (is_note, absolute.strip_prefix(vault)) {
                return self.href_for(&output.join(relative).with_extension("html"));
            }
        }
        self.href_for(path)
    }

    /// How the page refers to a local file it does not embed.
    fn href_for(&self, path: &Path) -> String {
        let path = absolute_path(path);
        if self.file_urls {
            return file_url(&path);
        }
        match &self.link_from {
            Some(from) => relative_href(from, &path),
            None => file_url(&path),
        }
    }

    /// Finds a file the way Obsidian does: next to the note, in an
    /// attachments folder of the note or any folder above it inside the
    /// vault, and finally anywhere in the vault by name (the shortest path
    /// wins, as with Obsidian's "shortest path when possible"). Whatever is
    /// found must still pass [`Self::admit`]; a candidate it refuses is a
    /// miss, not a reason to keep searching for a permitted namesake.
    fn resolve(&mut self, base: &Path, name: &str) -> Option<PathBuf> {
        if self.access == FileAccess::None {
            return None;
        }
        let found = self.locate(base, name)?;
        self.admit(&found)
    }

    fn locate(&mut self, base: &Path, name: &str) -> Option<PathBuf> {
        let name = name.trim().trim_start_matches("./");
        if name.is_empty() || name.contains('\0') || name.ends_with('/') {
            return None;
        }
        let relative = Path::new(name);
        if relative.is_absolute() {
            return relative.is_file().then(|| relative.to_path_buf());
        }
        let direct = base.join(relative);
        if direct.is_file() {
            return Some(direct);
        }

        let root = self
            .vault_root
            .clone()
            .unwrap_or_else(|| base.to_path_buf());
        // The folder Obsidian's own "Default location for new attachments"
        // setting names, when the vault has one. Reading that setting is a
        // file read like any other, so it passes the same gate.
        let settings_readable = self.vault_root.as_deref().is_some_and(|root| {
            self.admit(&root.join(".obsidian").join("app.json"))
                .is_some()
        });
        if let Some(folder) = self
            .vault_root
            .as_deref()
            .filter(|_| settings_readable)
            .and_then(|root| attachment_folder(root, base))
        {
            let candidate = folder.join(relative);
            if candidate.is_file() {
                return Some(candidate);
            }
        }
        let mut folder = Some(base.to_path_buf());
        for _ in 0..=MAX_VAULT_ROOT_ASCENT {
            let Some(dir) = folder else { break };
            for attachments in [
                "",
                "attachments",
                "Attachments",
                "assets",
                "_attachments",
                "media",
            ] {
                let candidate = dir.join(attachments).join(relative);
                if candidate.is_file() {
                    return Some(candidate);
                }
            }
            if dir == root || !dir.starts_with(&root) {
                break;
            }
            folder = dir.parent().map(Path::to_path_buf);
        }

        // Only search by name inside a folder known to be a vault: walking a
        // home directory because a stray note sits in it would be slow and
        // surprising.
        self.vault_root.as_ref()?;
        let wanted: Vec<String> = relative
            .components()
            .filter_map(|c| match c {
                Component::Normal(part) => Some(part.to_string_lossy().to_lowercase()),
                _ => None,
            })
            .collect();
        if wanted.is_empty() {
            return None;
        }
        let files = self
            .vault_files
            .get_or_insert_with(|| cached_vault_scan(&root))
            .clone();
        files
            .iter()
            .filter(|path| {
                let parts: Vec<String> = path
                    .components()
                    .filter_map(|c| match c {
                        Component::Normal(part) => Some(part.to_string_lossy().to_lowercase()),
                        _ => None,
                    })
                    .collect();
                parts.ends_with(&wanted)
            })
            .min_by_key(|path| path.components().count())
            .cloned()
    }

    /// Resolves a note name (`Project Plan`, `folder/Plan`, `Plan.md`).
    fn resolve_note(&mut self, base: &Path, name: &str) -> Option<PathBuf> {
        let has_markdown = name.to_ascii_lowercase().ends_with(".md")
            || name.to_ascii_lowercase().ends_with(".markdown");
        let file = if has_markdown {
            name.to_owned()
        } else {
            format!("{name}.md")
        };
        let found = self.resolve(base, &file)?;
        let extension = found
            .extension()
            .map(|e| e.to_string_lossy().to_ascii_lowercase());
        matches!(extension.as_deref(), Some("md" | "markdown")).then_some(found)
    }
}

/// Where Obsidian's `attachmentFolderPath` setting (in the vault's
/// `.obsidian/app.json`) puts attachments for a note in `note_folder`:
/// `/` is the vault root, `./sub` is relative to the note, anything else is
/// relative to the vault root. `None` when unset, unreadable, or when the
/// setting would climb out of the vault.
pub fn attachment_folder(vault_root: &Path, note_folder: &Path) -> Option<PathBuf> {
    let config = read_bounded_utf8(&vault_root.join(".obsidian").join("app.json"), 256 * 1024)?;
    let value: serde_json::Value = serde_json::from_str(&config).ok()?;
    let setting = value.get("attachmentFolderPath")?.as_str()?.trim();
    if setting.is_empty()
        || setting.contains('\0')
        || Path::new(setting)
            .components()
            .any(|c| matches!(c, Component::ParentDir))
    {
        return None;
    }
    Some(if setting == "/" {
        vault_root.to_path_buf()
    } else if setting == "." || setting == "./" {
        note_folder.to_path_buf()
    } else if let Some(sub) = setting.strip_prefix("./") {
        note_folder.join(sub)
    } else {
        vault_root.join(setting.trim_start_matches('/'))
    })
}

/// The nearest folder at or above `base` that looks like a vault root.
fn find_vault_root(base: &Path) -> Option<PathBuf> {
    let mut folder = Some(base);
    for _ in 0..=MAX_VAULT_ROOT_ASCENT {
        let dir = folder?;
        if dir.join(".obsidian").is_dir() || dir.join(".git").exists() {
            return Some(dir.to_path_buf());
        }
        folder = dir.parent();
    }
    None
}

/// [`scan_vault`], shared for a short while: exporting a whole vault renders
/// every note, and each would otherwise walk the vault again.
fn cached_vault_scan(root: &Path) -> std::sync::Arc<Vec<PathBuf>> {
    use std::sync::{Arc, Mutex, OnceLock};
    use std::time::{Duration, Instant};
    type Cache = Mutex<HashMap<PathBuf, (Instant, Arc<Vec<PathBuf>>)>>;
    static CACHE: OnceLock<Cache> = OnceLock::new();
    let cache = CACHE.get_or_init(|| Mutex::new(HashMap::new()));
    if let Ok(entries) = cache.lock() {
        if let Some((built, files)) = entries.get(root) {
            if built.elapsed() < Duration::from_secs(30) {
                return files.clone();
            }
        }
    }
    let files = Arc::new(scan_vault(root));
    if let Ok(mut entries) = cache.lock() {
        if entries.len() > 16 {
            entries.clear();
        }
        entries.insert(root.to_path_buf(), (Instant::now(), files.clone()));
    }
    files
}

/// Every regular file under `root`, skipping hidden folders and
/// `node_modules`, bounded by [`MAX_VAULT_SCAN_ENTRIES`].
fn scan_vault(root: &Path) -> Vec<PathBuf> {
    let mut files = Vec::new();
    let mut queue = VecDeque::from([(root.to_path_buf(), 0usize)]);
    let mut seen = 0usize;
    while let Some((dir, depth)) = queue.pop_front() {
        let Ok(entries) = fs::read_dir(&dir) else {
            continue;
        };
        for entry in entries.flatten() {
            seen += 1;
            if seen > MAX_VAULT_SCAN_ENTRIES {
                return files;
            }
            let name = entry.file_name();
            let name = name.to_string_lossy();
            if name.starts_with('.') || name == "node_modules" {
                continue;
            }
            let Ok(kind) = entry.file_type() else {
                continue;
            };
            if kind.is_dir() {
                if depth < 16 {
                    queue.push_back((entry.path(), depth + 1));
                }
            } else if kind.is_file() {
                files.push(entry.path());
            }
        }
    }
    files
}

fn absolute_path(path: &Path) -> PathBuf {
    std::path::absolute(path).unwrap_or_else(|_| path.to_path_buf())
}

/// `file:///…` for an absolute path, percent-encoded per component.
fn file_url(path: &Path) -> String {
    let mut url = String::from("file://");
    for component in path.components() {
        if let Component::Normal(part) = component {
            url.push('/');
            url.push_str(&encode_path_component(&part.to_string_lossy()));
        }
    }
    if url.len() == "file://".len() {
        url.push('/');
    }
    url
}

/// A relative URL from `from_dir` to `to`, percent-encoded per component.
fn relative_href(from_dir: &Path, to: &Path) -> String {
    let from: Vec<Component<'_>> = from_dir.components().collect();
    let target: Vec<Component<'_>> = to.components().collect();
    let common = from.iter().zip(&target).take_while(|(a, b)| a == b).count();
    let mut parts: Vec<String> = Vec::new();
    for _ in common..from.len() {
        parts.push("..".to_owned());
    }
    for component in &target[common..] {
        if let Component::Normal(part) = component {
            parts.push(encode_path_component(&part.to_string_lossy()));
        }
    }
    parts.join("/")
}

pub(crate) fn encode_path_component(part: &str) -> String {
    let mut out = String::with_capacity(part.len());
    for character in part.chars() {
        match character {
            ' ' => out.push_str("%20"),
            '"' => out.push_str("%22"),
            '#' => out.push_str("%23"),
            '%' => out.push_str("%25"),
            '?' => out.push_str("%3F"),
            '<' => out.push_str("%3C"),
            '>' => out.push_str("%3E"),
            _ => out.push(character),
        }
    }
    out
}

/// Escapes text for an HTML text node or a double-quoted attribute.
fn escape_html(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for character in text.chars() {
        match character {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&#39;"),
            _ => out.push(character),
        }
    }
    out
}

/// A blockquote in flight: plain, or a callout that owes a closing tag.
enum Quote {
    Plain,
    Callout { foldable: bool },
}

/// Streams parser events through sanitization, heading anchors, and the
/// Obsidian dialect.
///
/// Headings are the only construct buffered ahead: their `id` must be known
/// when the opening tag is written, and it is derived from the text inside. A
/// heading is a single block, so the buffer is bounded by the longest heading
/// rather than by the document.
struct ExportEvents<'a, 'c> {
    inner: pulldown_cmark::OffsetIter<'a, pulldown_cmark::DefaultBrokenLinkCallback>,
    source: &'a str,
    /// Formulas lifted out by [`prepare_source`], by placeholder index.
    math: Vec<MathToken>,
    base: Option<PathBuf>,
    context: &'c mut ExportContext,
    /// Events already pulled from the parser, to be processed again.
    replay: VecDeque<(Event<'a>, Range<usize>)>,
    /// Events handled, awaiting [`Self::balance_marks`].
    pending: VecDeque<Event<'a>>,
    /// Events ready to hand to the HTML writer.
    ready: VecDeque<Event<'a>>,
    /// The events of a highlight in progress, held for [`place_marks`].
    mark: Option<Vec<Event<'a>>>,
    slugs: HashMap<String, usize>,
    heading_anchor: Option<String>,
    quotes: Vec<Quote>,
    /// Source bytes whose events are dropped: a callout's `[!type] Title`
    /// line, or the `[/] ` of a custom task.
    skip: Option<Range<usize>>,
    /// This note's headings as their elements are written, for
    /// [`Fragment::headings`].
    outline: Vec<FragmentHeading>,
    /// The text of the note's first frontmatter block.
    frontmatter: Option<String>,
}

impl<'a, 'c> ExportEvents<'a, 'c> {
    fn new(
        source: &'a str,
        math: Vec<MathToken>,
        base: Option<PathBuf>,
        context: &'c mut ExportContext,
    ) -> Self {
        Self {
            inner: Parser::new_ext(source, options()).into_offset_iter(),
            source,
            math,
            base,
            context,
            replay: VecDeque::new(),
            pending: VecDeque::new(),
            ready: VecDeque::new(),
            mark: None,
            slugs: HashMap::new(),
            heading_anchor: None,
            quotes: Vec::new(),
            skip: None,
            outline: Vec::new(),
            frontmatter: None,
        }
    }

    fn pull(&mut self) -> Option<(Event<'a>, Range<usize>)> {
        self.replay.pop_front().or_else(|| self.inner.next())
    }

    fn html(&mut self, markup: String) {
        self.pending
            .push_back(Event::Html(CowStr::Boxed(markup.into_boxed_str())));
    }

    fn unique_slug(&mut self, text: &str) -> String {
        let base = slugify(text);
        let base = if base.is_empty() {
            "section".to_owned()
        } else {
            base
        };
        let count = self.slugs.entry(base.clone()).or_insert(0);
        let slug = if *count == 0 {
            base.clone()
        } else {
            format!("{base}-{count}")
        };
        *count += 1;
        // Reserve the suffixed spelling too, so a later heading literally
        // titled "Intro 1" cannot collide with the second "Intro".
        if slug != base {
            self.slugs.entry(slug.clone()).or_insert(1);
        }
        slug
    }

    /// Drops or trims events inside [`Self::skip`].
    fn apply_skip(
        &mut self,
        event: Event<'a>,
        range: Range<usize>,
    ) -> Option<(Event<'a>, Range<usize>)> {
        let Some(skip) = self.skip.clone() else {
            return Some((event, range));
        };
        if range.start >= skip.end {
            self.skip = None;
            return Some((event, range));
        }
        if range.start < skip.start {
            return Some((event, range));
        }
        if range.end <= skip.end {
            return None;
        }
        // A text run that starts inside the skipped prefix and continues
        // past it keeps only its tail — but only when the event is a verbatim
        // copy of the source, so the cut lands where it should.
        if let Event::Text(text) = &event {
            let cut = skip.end - range.start;
            if text.len() == range.len() && text.is_char_boundary(cut) {
                let tail = text[cut..].to_owned();
                return Some((
                    Event::Text(CowStr::Boxed(tail.into_boxed_str())),
                    skip.end..range.end,
                ));
            }
            return None;
        }
        Some((event, range))
    }

    fn handle(&mut self, event: Event<'a>, range: Range<usize>) {
        match event {
            Event::Start(Tag::Heading {
                level,
                classes,
                attrs,
                ..
            }) => {
                let mut text = String::new();
                let mut body = Vec::new();
                while let Some((event, range)) = self.pull() {
                    let end = matches!(event, Event::End(TagEnd::Heading(_)));
                    if let Event::Text(value) | Event::Code(value) = &event {
                        text.push_str(value);
                    }
                    body.push((event, range));
                    if end {
                        break;
                    }
                }
                // A trailing `^block-id` is not part of the heading's name.
                let text = self.plain_text(&text);
                let name = strip_block_id(&text);
                // Heading attributes are not enabled, so the parser never
                // supplies an id; derive one so in-page links resolve.
                let slug = self.unique_slug(name);
                self.outline.push(FragmentHeading {
                    level: level as u8,
                    text: name.trim().to_owned(),
                    id: slug.clone(),
                });
                self.heading_anchor = Some(slug.clone());
                for item in body.into_iter().rev() {
                    self.replay.push_front(item);
                }
                self.pending.push_back(Event::Start(Tag::Heading {
                    level,
                    id: Some(CowStr::Boxed(slug.into_boxed_str())),
                    classes,
                    attrs,
                }));
            }
            Event::End(TagEnd::Heading(level)) => {
                if let Some(slug) = self.heading_anchor.take() {
                    // Slugs are built from alphanumerics, `-` and `_` only,
                    // so they need no escaping inside the attribute.
                    self.html(format!(
                        "<a class=\"anchor\" href=\"#{slug}\" aria-label=\"Link to this section\">#</a>"
                    ));
                }
                self.pending.push_back(Event::End(TagEnd::Heading(level)));
            }
            Event::Start(Tag::BlockQuote(kind)) => {
                match obsidian::callout_line(self.source, &range) {
                    Some(line) => {
                        let newline = match self.source.as_bytes().get(line.line_end) {
                            Some(b'\r')
                                if self.source.as_bytes().get(line.line_end + 1)
                                    == Some(&b'\n') =>
                            {
                                2
                            }
                            Some(b'\n' | b'\r') => 1,
                            _ => 0,
                        };
                        self.skip = Some(line.tag.start..line.line_end + newline);
                        let foldable = line.fold != 0;
                        let title = match line.title {
                            Some((title, _)) => self.render_inline(title),
                            None => escape_html(&obsidian::default_callout_title(line.type_name)),
                        };
                        let kind_name = callout_name(line.kind);
                        let type_name = escape_html(&line.type_name.to_ascii_lowercase());
                        let icon = callout_icon(line.kind);
                        let markup = if foldable {
                            let open = if line.fold == CALLOUT_FOLD_COLLAPSED {
                                ""
                            } else {
                                " open"
                            };
                            format!(
                                "<details class=\"callout\" data-callout=\"{kind_name}\" data-callout-type=\"{type_name}\"{open}>\n<summary class=\"callout-title\">{icon}<span class=\"callout-title-inner\">{title}</span></summary>\n<div class=\"callout-content\">\n"
                            )
                        } else {
                            format!(
                                "<div class=\"callout\" data-callout=\"{kind_name}\" data-callout-type=\"{type_name}\">\n<div class=\"callout-title\">{icon}<span class=\"callout-title-inner\">{title}</span></div>\n<div class=\"callout-content\">\n"
                            )
                        };
                        self.quotes.push(Quote::Callout { foldable });
                        self.html(markup);
                    }
                    None => {
                        self.quotes.push(Quote::Plain);
                        self.pending.push_back(Event::Start(Tag::BlockQuote(kind)));
                    }
                }
            }
            Event::End(TagEnd::BlockQuote(kind)) => match self.quotes.pop() {
                Some(Quote::Callout { foldable: true }) => {
                    self.html("</div>\n</details>\n".to_owned())
                }
                Some(Quote::Callout { foldable: false }) => {
                    self.html("</div>\n</div>\n".to_owned())
                }
                _ => self.pending.push_back(Event::End(TagEnd::BlockQuote(kind))),
            },
            Event::Start(Tag::Item) => {
                self.pending.push_back(Event::Start(Tag::Item));
                if let Some(task) = obsidian::custom_task_at(self.source, range.start) {
                    let bytes = self.source.as_bytes();
                    let mut end = task.marker.end;
                    while end < bytes.len() && matches!(bytes[end], b' ' | b'\t') {
                        end += 1;
                    }
                    self.skip = Some(task.marker.start..end);
                    let status = escape_html(&task.status.to_string());
                    self.html(format!(
                        "<input disabled=\"\" type=\"checkbox\" checked=\"\" data-task=\"{status}\"/>\n"
                    ));
                }
            }
            Event::Start(Tag::Image {
                link_type,
                dest_url,
                title,
                ..
            }) => {
                let mut alt = String::new();
                let mut depth = 1usize;
                while let Some((event, _)) = self.pull() {
                    match event {
                        Event::Start(Tag::Image { .. }) => depth += 1,
                        Event::End(TagEnd::Image) => {
                            depth -= 1;
                            if depth == 0 {
                                break;
                            }
                        }
                        Event::Text(value) | Event::Code(value) => alt.push_str(&value),
                        Event::SoftBreak | Event::HardBreak => alt.push(' '),
                        _ => {}
                    }
                }
                let alt = self.plain_text(&alt);
                let markup = self.render_embed(link_type, &dest_url, &title, &alt);
                self.html(markup);
            }
            Event::Start(Tag::Link {
                link_type,
                dest_url,
                title,
                id,
            }) => {
                let dest_url = if matches!(link_type, LinkType::WikiLink { .. }) {
                    CowStr::Boxed(self.wikilink_href(&dest_url).into_boxed_str())
                } else {
                    let safe = sanitize_destination(dest_url, true, link_type);
                    match self.rebase_relative(&safe) {
                        Some(rebased) => CowStr::Boxed(rebased.into_boxed_str()),
                        None => safe,
                    }
                };
                self.pending.push_back(Event::Start(Tag::Link {
                    dest_url,
                    link_type,
                    title,
                    id,
                }));
            }
            Event::InlineMath(latex) => {
                // The editor's currency rules decide, not pulldown's: `$5 and
                // $10` is prose on the page just as it is in the editor.
                if inline_math_is_valid(self.source, &range) {
                    self.html(math_html(&latex, false));
                } else {
                    self.push_literal(range);
                }
            }
            Event::DisplayMath(latex) => {
                if display_math_is_valid(self.source, &range) {
                    self.html(math_html(&latex, true));
                } else {
                    self.push_literal(range);
                }
            }
            Event::Start(Tag::CodeBlock(CodeBlockKind::Fenced(language)))
                if language.trim().eq_ignore_ascii_case("math") =>
            {
                let mut body: Vec<Event<'a>> = Vec::new();
                let mut latex = String::new();
                while let Some((event, _)) = self.pull() {
                    let end = matches!(event, Event::End(TagEnd::CodeBlock));
                    if let Event::Text(value) = &event {
                        latex.push_str(value);
                    }
                    body.push(event);
                    if end {
                        break;
                    }
                }
                match render_math(&latex, true) {
                    Some(mathml) => {
                        self.html(format!("<p class=\"math math-display\">{mathml}</p>\n"))
                    }
                    None => {
                        self.pending.push_back(Event::Start(Tag::CodeBlock(
                            CodeBlockKind::Fenced(language),
                        )));
                        self.pending.extend(body);
                    }
                }
            }
            Event::Start(Tag::MetadataBlock(kind)) => {
                // The writer prints nothing inside a metadata block; keep its
                // events so that stays its decision, and lift the text out.
                let mut text = String::new();
                self.pending
                    .push_back(Event::Start(Tag::MetadataBlock(kind)));
                while let Some((event, _)) = self.pull() {
                    let end = matches!(event, Event::End(TagEnd::MetadataBlock(_)));
                    if let Event::Text(value) = &event {
                        text.push_str(value);
                    }
                    self.pending.push_back(event);
                    if end {
                        break;
                    }
                }
                if self.frontmatter.is_none() {
                    self.frontmatter = Some(text);
                }
            }
            Event::Text(value) => self.push_text(value, range),
            // Preserve what the author typed, but never interpret it in the
            // browser. `push_html` escapes Text events for us.
            Event::Html(value) | Event::InlineHtml(value) => {
                self.pending.push_back(Event::Text(value))
            }
            other => self.pending.push_back(other),
        }
    }

    /// Plain text, with `==highlights==`, `#tags`, and a trailing
    /// `^block-id` turned into markup.
    fn push_text(&mut self, value: CowStr<'a>, range: Range<usize>) {
        let text: &str = value.as_ref();
        let at_line_end = matches!(
            self.source.as_bytes().get(range.end),
            None | Some(b'\n' | b'\r')
        );
        let (body, block_id) = if at_line_end && text.contains('^') {
            split_block_id(text)
        } else {
            (text, None)
        };
        let has_math = body.contains(MATH_OPEN);
        let has_highlight = body.contains([MARK_OPEN, MARK_CLOSE]);
        let has_tag = body.contains('#') && !scan_tags(body).is_empty();
        if !has_math && !has_highlight && !has_tag && block_id.is_none() {
            self.pending.push_back(Event::Text(value));
            return;
        }

        let mut pieces: Vec<Event<'a>> = Vec::new();
        let mut rest = body;
        while let Some(open) = rest.find(MATH_OPEN) {
            let after = &rest[open + MATH_OPEN.len_utf8()..];
            let Some(close) = after.find(MATH_CLOSE) else {
                break;
            };
            let token = after[..close]
                .parse::<usize>()
                .ok()
                .and_then(|index| self.math.get(index));
            push_rich(&rest[..open], &mut pieces);
            match token {
                Some(token) => pieces.push(Event::Html(CowStr::Boxed(
                    math_html(&token.latex, token.display).into_boxed_str(),
                ))),
                None => pieces.push(Event::Text(CowStr::Boxed(
                    rest[open..open + MATH_OPEN.len_utf8() + close + MATH_CLOSE.len_utf8()].into(),
                ))),
            }
            rest = &after[close + MATH_CLOSE.len_utf8()..];
        }
        push_rich(rest, &mut pieces);
        if let Some(id) = block_id {
            // Ids are `[A-Za-z0-9-]`, safe inside the attribute as written.
            pieces.push(Event::Html(CowStr::Boxed(
                format!("<span class=\"block-id\" id=\"^{id}\"></span>").into_boxed_str(),
            )));
        }
        self.pending.extend(pieces);
    }

    /// The source of `range`, shown as typed.
    fn push_literal(&mut self, range: Range<usize>) {
        let literal = self.source.get(range).unwrap_or_default().to_owned();
        self.pending
            .push_back(Event::Text(CowStr::Boxed(literal.into_boxed_str())));
    }

    /// A relative local destination written in this note (`other.md#x`,
    /// `media/clip.mp4`), rewritten to point there from the page. `None` for
    /// anything that is not a relative path: fragments, URLs, e-mail.
    fn rebase_relative(&self, destination: &str) -> Option<String> {
        let base = self.base.as_ref()?;
        let (path, fragment) = match destination.split_once('#') {
            Some((path, fragment)) => (path, Some(fragment)),
            None => (destination, None),
        };
        let (path, query) = match path.split_once('?') {
            Some((path, query)) => (path, Some(query)),
            None => (path, None),
        };
        let lower = path.to_ascii_lowercase();
        let has_scheme = lower.find(':').is_some_and(|colon| {
            let boundary = lower.find('/').unwrap_or(usize::MAX);
            colon < boundary
        });
        if path.is_empty() || has_scheme || path.starts_with("//") || path.starts_with('/') {
            return None;
        }
        let decoded = percent_decode(path)?;
        let mut href = self.context.note_href(&base.join(decoded));
        if path.ends_with('/') && !href.ends_with('/') {
            href.push('/');
        }
        if let Some(query) = query {
            href.push('?');
            href.push_str(query);
        }
        if let Some(fragment) = fragment {
            href.push('#');
            href.push_str(fragment);
        }
        Some(href)
    }

    /// Where a `[[wikilink]]` points in the browser: the note's file,
    /// relative to this page when the vault can find it, and `#^id` or a
    /// heading slug for the fragment.
    fn wikilink_href(&mut self, destination: &str) -> String {
        let (page, fragment) = match destination.split_once('#') {
            Some((page, fragment)) => (page.trim(), Some(fragment.trim())),
            None => (destination.trim(), None),
        };
        let mut href = String::new();
        if !page.is_empty() {
            let lower = page.to_ascii_lowercase();
            // A wikilink is always a note in the vault; never let it smuggle
            // in a scheme or a protocol-relative host.
            if lower.contains(':') || page.starts_with("//") {
                return "#".to_owned();
            }
            let resolved = self.base.clone().and_then(|base| {
                let found =
                    if obsidian::is_media_target(page) || Path::new(page).extension().is_some() {
                        self.context.resolve(&base, page)
                    } else {
                        self.context.resolve_note(&base, page)
                    }?;
                Some(self.context.note_href(&found))
            });
            let unresolved = || {
                let has_extension = Path::new(page)
                    .extension()
                    .is_some_and(|extension| !extension.is_empty());
                if has_extension {
                    page.to_owned()
                } else {
                    format!("{page}.md")
                }
            };
            let resolved = resolved.or_else(|| {
                // Not found: still point where it would be, from the page.
                self.base
                    .as_ref()
                    .map(|base| self.context.note_href(&base.join(unresolved())))
            });
            match resolved {
                Some(relative) => href.push_str(&relative),
                None => {
                    for part in page.split('/') {
                        if !href.is_empty() {
                            href.push('/');
                        }
                        href.push_str(&encode_path_component(part));
                    }
                    let has_extension = Path::new(page)
                        .extension()
                        .is_some_and(|extension| !extension.is_empty());
                    if !has_extension {
                        href.push_str(".md");
                    }
                }
            }
        }
        if let Some(fragment) = fragment {
            if let Some(id) = fragment.strip_prefix('^') {
                let id: String = id
                    .chars()
                    .filter(|c| c.is_ascii_alphanumeric() || *c == '-')
                    .collect();
                if !id.is_empty() {
                    href.push_str("#^");
                    href.push_str(&id);
                }
            } else {
                // `Heading#Subheading` targets the innermost heading.
                let innermost = fragment.rsplit('#').find(|s| !s.trim().is_empty());
                let slug = slugify(innermost.unwrap_or(fragment));
                if !slug.is_empty() {
                    href.push('#');
                    href.push_str(&slug);
                }
            }
        }
        if href.is_empty() {
            "#".to_owned()
        } else {
            href
        }
    }

    /// Renders a short run of Markdown (a callout title) as inline HTML.
    fn render_inline(&mut self, markdown: &str) -> String {
        let mut out = String::new();
        let events = ExportEvents::new(
            markdown,
            self.math.clone(),
            self.base.clone(),
            &mut *self.context,
        );
        html::push_html(&mut out, events);
        let trimmed = out.trim();
        match trimmed
            .strip_prefix("<p>")
            .and_then(|rest| rest.strip_suffix("</p>"))
        {
            Some(inline) if !inline.contains("<p>") => inline.to_owned(),
            _ => escape_html(markdown),
        }
    }

    /// `![](…)` and `![[…]]`: a picture, audio or video player, PDF link, or
    /// a transcluded note.
    fn render_embed(
        &mut self,
        link_type: LinkType,
        destination: &str,
        title: &str,
        alt: &str,
    ) -> String {
        let wiki = matches!(link_type, LinkType::WikiLink { .. });
        if wiki && !obsidian::is_media_target(destination) {
            return self.render_note_embed(destination, alt);
        }

        // Obsidian writes a display size where the alt text goes:
        // `![caption|300](pic.png)`, `![[pic.png|300x200]]`.
        let (alt_text, size) = obsidian::split_alt_size(alt);
        let alt_text = if wiki && alt_text == destination {
            ""
        } else {
            alt_text
        };
        let safe = sanitize_destination(
            CowStr::Borrowed(destination),
            self.context.remote_media,
            link_type,
        );
        let found = match (&self.base, safe.as_ref()) {
            (_, "#") | (None, _) => None,
            (Some(base), dest) => {
                let base = base.clone();
                local_image_path(dest, &base)
                    .filter(|path| path.is_file())
                    .and_then(|path| self.context.admit(&path))
                    .or_else(|| {
                        let decoded = percent_decode(dest)?;
                        let lower = decoded.to_ascii_lowercase();
                        (!lower.starts_with("file:") && !decoded.starts_with('/'))
                            .then(|| self.context.resolve(&base, &decoded))
                            .flatten()
                    })
            }
        };
        let href = match (&found, &self.base) {
            (Some(path), Some(_)) => self.context.href_for(path),
            (None, Some(base)) if wiki && safe.as_ref() != "#" => {
                self.context.href_for(&base.join(destination))
            }
            _ if wiki && safe.as_ref() != "#" => destination
                .split('/')
                .map(encode_path_component)
                .collect::<Vec<_>>()
                .join("/"),
            _ => self
                .rebase_relative(safe.as_ref())
                .unwrap_or_else(|| safe.to_string()),
        };
        let href = escape_html(&href);
        let size_attrs = match size {
            Some((width, Some(height))) => format!(" width=\"{width}\" height=\"{height}\""),
            Some((width, None)) => format!(" width=\"{width}\""),
            None => String::new(),
        };
        let title_attr = if title.is_empty() {
            String::new()
        } else {
            format!(" title=\"{}\"", escape_html(title))
        };
        // Withheld by `remote_media`, not refused for its scheme: say so, and
        // keep the address as inert data for the embedder to report.
        let withheld = if !self.context.remote_media && is_remote(destination) {
            format!(" data-withheld-src=\"{}\"", escape_html(destination))
        } else {
            String::new()
        };

        if obsidian::is_audio_target(destination) {
            return format!(
                "<audio class=\"media-embed\" controls src=\"{href}\"{withheld}{title_attr}></audio>"
            );
        }
        if obsidian::is_video_target(destination) {
            return format!(
                "<video class=\"media-embed\" controls src=\"{href}\"{withheld}{size_attrs}{title_attr}></video>"
            );
        }
        if destination
            .to_ascii_lowercase()
            .split('#')
            .next()
            .is_some_and(|d| d.ends_with(".pdf"))
        {
            let name = destination.rsplit('/').next().unwrap_or(destination);
            let label = if alt_text.is_empty() { name } else { alt_text };
            return format!(
                "<a class=\"embed-link pdf-embed\" href=\"{href}\"{title_attr}>{}</a>",
                escape_html(label)
            );
        }

        let src = found
            .as_deref()
            .and_then(|path| self.context.images.embed(path))
            .unwrap_or(href);
        // A remote picture is a request on the reader's behalf; it need not
        // also tell the server which note asked for it.
        let lower = src.to_ascii_lowercase();
        let referrer =
            if lower.starts_with("http:") || lower.starts_with("https:") || lower.starts_with("//")
            {
                " referrerpolicy=\"no-referrer\""
            } else {
                ""
            };
        format!(
            "<img src=\"{src}\"{withheld} alt=\"{}\"{title_attr}{size_attrs} loading=\"lazy\"{referrer} />",
            escape_html(alt_text)
        )
    }

    /// `![[Note]]`, `![[Note#Heading]]`, `![[Note#^block]]`: the note's
    /// content in a frame, or a link when it cannot be read.
    fn render_note_embed(&mut self, destination: &str, alt: &str) -> String {
        let href = escape_html(&self.wikilink_href(destination));
        let (page, anchor) = match destination.split_once('#') {
            Some((page, anchor)) => (page.trim(), Some(anchor.trim())),
            None => (destination.trim(), None),
        };
        let label = if !alt.trim().is_empty() && alt.trim() != destination.trim() {
            alt.trim().to_owned()
        } else if page.is_empty() {
            anchor.unwrap_or_default().to_owned()
        } else {
            page.rsplit('/').next().unwrap_or(page).to_owned()
        };
        let label = escape_html(&label);
        match self.transclude(page, anchor) {
            Some(content) => format!(
                "<div class=\"markdown-embed\"><div class=\"markdown-embed-title\"><a class=\"internal-link\" href=\"{href}\">{label}</a></div><div class=\"markdown-embed-content\">\n{content}</div></div>"
            ),
            None => format!("<a class=\"internal-link embed-link\" href=\"{href}\">{label}</a>"),
        }
    }

    fn transclude(&mut self, page: &str, anchor: Option<&str>) -> Option<String> {
        let base = self.base.clone()?;
        if page.is_empty() || self.context.depth >= MAX_EMBED_DEPTH {
            return None;
        }
        let path = self.context.resolve_note(&base, page)?;
        let canonical = fs::canonicalize(&path).unwrap_or_else(|_| path.clone());
        if self.context.visited.iter().any(|seen| seen == &canonical) {
            return None;
        }
        let limit = MAX_TRANSCLUDED_NOTE_BYTES.min(self.context.transclusion_remaining);
        let text = read_bounded_utf8(&path, limit)?;
        self.context.transclusion_remaining -= text.len();
        let section = select_section(&text, anchor)?;
        let prepared = prepare_source(section);
        let math = prepared.math;
        let prepared = prepared.text.into_owned();

        self.context.depth += 1;
        self.context.visited.push(canonical);
        let mut out = String::new();
        {
            let events = ExportEvents::new(
                &prepared,
                math,
                path.parent().map(Path::to_path_buf),
                &mut *self.context,
            );
            html::push_html(&mut out, events);
        }
        self.context.visited.pop();
        self.context.depth -= 1;
        Some(out)
    }
}

impl<'a> Iterator for ExportEvents<'a, '_> {
    type Item = Event<'a>;

    fn next(&mut self) -> Option<Event<'a>> {
        loop {
            if let Some(event) = self.ready.pop_front() {
                return Some(event);
            }
            if let Some(event) = self.pending.pop_front() {
                self.balance_marks(event);
                continue;
            }
            let Some((event, range)) = self.pull() else {
                // A highlight still open at the end of the note closes here.
                let run = self.mark.take()?;
                self.ready.extend(place_marks(run));
                continue;
            };
            let Some((event, range)) = self.apply_skip(event, range) else {
                continue;
            };
            self.handle(event, range);
        }
    }
}

impl<'a> ExportEvents<'a, '_> {
    /// Collects each highlight's events so [`place_marks`] can see its whole
    /// extent before any `<mark>` is written.
    fn balance_marks(&mut self, event: Event<'a>) {
        let opens = matches!(&event, Event::Html(m) if m.as_ref() == MARK_START_TAG);
        let closes = matches!(&event, Event::Html(m) if m.as_ref() == MARK_END_TAG);
        match &mut self.mark {
            None if opens => self.mark = Some(Vec::new()),
            // A close with nothing open marks nothing.
            None if closes => {}
            None => self.ready.push_back(event),
            // Already inside a highlight: a second opener changes nothing.
            Some(_) if opens => {}
            Some(_) if closes => {
                let run = self.mark.take().unwrap_or_default();
                self.ready.extend(place_marks(run));
            }
            Some(run) => {
                run.push(event);
                // Bounds what is held back. Placing the marks for a prefix and
                // carrying on with a fresh run is still well-nested — an
                // element left open by the prefix is just one more crossing.
                if run.len() >= MAX_HELD_MARK_EVENTS {
                    let prefix = std::mem::take(run);
                    self.ready.extend(place_marks(prefix));
                }
            }
        }
    }

    /// Text collected from events, with this module's placeholders turned
    /// back into what the author wrote: highlight markers dropped, and a
    /// formula's index replaced by its LaTeX. Without this a heading's outline
    /// entry carried private-use characters, and `# Euler \(e\)` slugged as
    /// the formula's position in the note rather than its text.
    fn plain_text(&self, text: &str) -> String {
        if !text.contains(PLACEHOLDERS) {
            return text.to_owned();
        }
        let mut out = String::with_capacity(text.len());
        let mut rest = text;
        while let Some(open) = rest.find(MATH_OPEN) {
            out.extend(rest[..open].chars().filter(|c| !PLACEHOLDERS.contains(c)));
            let after = &rest[open + MATH_OPEN.len_utf8()..];
            let Some(close) = after.find(MATH_CLOSE) else {
                rest = after;
                break;
            };
            if let Some(token) = after[..close]
                .parse::<usize>()
                .ok()
                .and_then(|index| self.math.get(index))
            {
                out.push_str(&token.latex);
            }
            rest = &after[close + MATH_CLOSE.len_utf8()..];
        }
        out.extend(rest.chars().filter(|c| !PLACEHOLDERS.contains(c)));
        out
    }
}

const MARK_START_TAG: &str = "<mark>";
const MARK_END_TAG: &str = "</mark>";
/// Most events one highlight holds back before its marks are placed.
const MAX_HELD_MARK_EVENTS: usize = 4096;

/// Writes `<mark>` around a highlight's events so it nests properly.
///
/// A highlight's `==` delimiters are found by the editor's rules, which do
/// not know CommonMark's: `_a==b_ c==` opens a highlight inside an emphasis
/// and closes it outside. Writing the tags where the delimiters fell
/// produced `<em>a<mark>b</em> c</mark>`, which a browser silently
/// restructures. So the mark is split exactly where an element crosses its
/// edge — one opened before the highlight began, or still open when it
/// ends — and at block boundaries. An inline element wholly inside stays
/// wrapped (`==**bold**==` is `<mark><strong>bold</strong></mark>`).
fn place_marks(run: Vec<Event<'_>>) -> Vec<Event<'_>> {
    // Which Start/End events pair up inside the run. Parser events nest, so
    // an End with no open Start here closes something opened before the
    // highlight, and a Start left on the stack is still open after it.
    let mut paired = vec![false; run.len()];
    let mut open = Vec::new();
    for (index, event) in run.iter().enumerate() {
        match event {
            Event::Start(_) => open.push(index),
            Event::End(_) => {
                if let Some(start) = open.pop() {
                    paired[start] = true;
                    paired[index] = true;
                }
            }
            _ => {}
        }
    }

    let mut out = Vec::with_capacity(run.len() + 4);
    let mut marked = false;
    let mark = |out: &mut Vec<Event<'_>>, marked: &mut bool, on: bool| {
        if *marked != on {
            out.push(Event::Html(CowStr::Borrowed(if on {
                MARK_START_TAG
            } else {
                MARK_END_TAG
            })));
            *marked = on;
        }
    };
    for (index, event) in run.into_iter().enumerate() {
        match &event {
            Event::Start(tag) if paired[index] && is_inline_tag(tag) => {
                mark(&mut out, &mut marked, true)
            }
            // Its Start opened the mark, which nothing inside could close.
            Event::End(end) if paired[index] && is_inline_end(end) => {}
            Event::Text(_)
            | Event::Code(_)
            | Event::FootnoteReference(_)
            | Event::InlineMath(_)
            | Event::DisplayMath(_) => mark(&mut out, &mut marked, true),
            Event::Html(markup) | Event::InlineHtml(markup) => {
                mark(&mut out, &mut marked, !is_block_markup(markup))
            }
            Event::SoftBreak | Event::HardBreak => {}
            // Every other Start or End — crossing, or block-level — and
            // rules and task boxes sit outside the mark.
            _ => mark(&mut out, &mut marked, false),
        }
        out.push(event);
    }
    mark(&mut out, &mut marked, false);
    out
}

fn is_inline_tag(tag: &Tag<'_>) -> bool {
    matches!(
        tag,
        Tag::Emphasis
            | Tag::Strong
            | Tag::Strikethrough
            | Tag::Superscript
            | Tag::Subscript
            | Tag::Link { .. }
    )
}

fn is_inline_end(end: &TagEnd) -> bool {
    matches!(
        end,
        TagEnd::Emphasis
            | TagEnd::Strong
            | TagEnd::Strikethrough
            | TagEnd::Superscript
            | TagEnd::Subscript
            | TagEnd::Link
    )
}

/// Whether markup this module wrote is block-level (a callout, an embed,
/// display math), and so cannot sit inside a `<mark>`.
fn is_block_markup(markup: &str) -> bool {
    let markup = markup.trim_start();
    [
        "<div",
        "</div",
        "<details",
        "</details",
        "<summary",
        "</summary",
        "<p>",
        "<p ",
        "</p>",
    ]
    .iter()
    .any(|prefix| markup.starts_with(prefix))
}

/// Text with highlight placeholders and `#tags` turned into markup.
fn push_rich<'a>(body: &str, pieces: &mut Vec<Event<'a>>) {
    let mut at = 0;
    for (index, character) in body.char_indices() {
        let markup = match character {
            MARK_OPEN => MARK_START_TAG,
            MARK_CLOSE => MARK_END_TAG,
            _ => continue,
        };
        push_tagged(&body[at..index], pieces);
        pieces.push(Event::Html(CowStr::Borrowed(markup)));
        at = index + character.len_utf8();
    }
    push_tagged(&body[at..], pieces);
}

/// Text with any `#tags` wrapped, appended to `pieces`.
fn push_tagged<'a>(text: &str, pieces: &mut Vec<Event<'a>>) {
    if text.is_empty() {
        return;
    }
    let mut at = 0;
    for tag in scan_tags(text) {
        if tag.start > at {
            pieces.push(Event::Text(CowStr::Boxed(text[at..tag.start].into())));
        }
        pieces.push(Event::Html(CowStr::Boxed(
            format!(
                "<span class=\"tag\">{}</span>",
                escape_html(&text[tag.clone()])
            )
            .into_boxed_str(),
        )));
        at = tag.end;
    }
    if at < text.len() {
        pieces.push(Event::Text(CowStr::Boxed(text[at..].into())));
    }
}

/// Splits a trailing `^block-id` off a line of text.
fn split_block_id(text: &str) -> (&str, Option<String>) {
    let trimmed_len = text.trim_end_matches([' ', '\t']).len();
    match obsidian::block_ids(text, &[]).into_iter().last() {
        Some(id) if id.id.end == trimmed_len => (
            &text[..id.marker.start],
            Some(text[id.id.clone()].to_owned()),
        ),
        _ => (text, None),
    }
}

fn strip_block_id(text: &str) -> &str {
    split_block_id(text).0.trim_end()
}

/// Reads a UTF-8 file no larger than `limit` bytes.
fn read_bounded_utf8(path: &Path, limit: usize) -> Option<String> {
    let file = File::open(path).ok()?;
    let metadata = file.metadata().ok()?;
    if !metadata.is_file() || metadata.len() > limit as u64 {
        return None;
    }
    let mut bytes = Vec::with_capacity(metadata.len() as usize);
    file.take(limit as u64 + 1).read_to_end(&mut bytes).ok()?;
    if bytes.len() > limit || bytes.contains(&0) {
        return None;
    }
    String::from_utf8(bytes).ok()
}

/// The part of a note an embed shows: everything but its frontmatter, the
/// section under a heading, or the block carrying a `^block-id`.
fn select_section<'t>(text: &'t str, anchor: Option<&str>) -> Option<&'t str> {
    let mut body_start = 0;
    let mut headings: Vec<(u8, String, usize)> = Vec::new();
    let mut open: Option<(u8, usize)> = None;
    let mut heading_text = String::new();
    for (event, range) in Parser::new_ext(text, options()).into_offset_iter() {
        match event {
            Event::End(TagEnd::MetadataBlock(_)) if range.start == 0 => body_start = range.end,
            Event::Start(Tag::Heading { level, .. }) => {
                open = Some((level as u8, range.start));
                heading_text.clear();
            }
            Event::Text(value) | Event::Code(value) if open.is_some() => {
                heading_text.push_str(&value)
            }
            Event::End(TagEnd::Heading(_)) => {
                if let Some((level, start)) = open.take() {
                    headings.push((level, strip_block_id(&heading_text).to_owned(), start));
                }
            }
            _ => {}
        }
    }

    let Some(anchor) = anchor.filter(|a| !a.is_empty()) else {
        return Some(&text[body_start..]);
    };
    if let Some(id) = anchor.strip_prefix('^') {
        let literal = obsidian::verbatim_ranges(text);
        let block = obsidian::block_ids(text, &literal)
            .into_iter()
            .find(|block| text[block.id.clone()].eq_ignore_ascii_case(id))?;
        let line_start = text[..block.marker.start].rfind('\n').map_or(0, |i| i + 1);
        let standalone = text[line_start..block.marker.start].trim().is_empty();
        let end = if standalone {
            line_start.saturating_sub(1)
        } else {
            block.marker.start
        };
        let mut start = text[..end].rfind("\n\n").map_or(0, |i| i + 2);
        if standalone {
            // `^id` after a blank line names the block above that blank line.
            let above = text[..end].trim_end_matches(['\n', '\r']);
            start = above.rfind("\n\n").map_or(0, |i| i + 2);
            return Some(&text[start.max(body_start)..above.len()]);
        }
        start = start.max(body_start);
        return Some(&text[start..end]);
    }
    let outline: Vec<(u8, &str)> = headings
        .iter()
        .map(|(level, name, _)| (*level, name.as_str()))
        .collect();
    let index = headings
        .iter()
        .position(|(_, name, _)| name.eq_ignore_ascii_case(anchor))
        .or_else(|| obsidian::heading_path_index(&outline, anchor))?;
    let (level, _, start) = headings[index];
    let end = headings[index + 1..]
        .iter()
        .find(|(other, _, _)| *other <= level)
        .map_or(text.len(), |(_, _, at)| *at);
    Some(&text[start..end])
}

/// The canonical name of a callout flavour, used for its colour and icon.
fn callout_name(kind: CalloutKind) -> &'static str {
    match kind {
        CalloutKind::Note => "note",
        CalloutKind::Tip => "tip",
        CalloutKind::Important => "important",
        CalloutKind::Warning => "warning",
        CalloutKind::Caution => "caution",
        CalloutKind::Abstract => "abstract",
        CalloutKind::Info => "info",
        CalloutKind::Todo => "todo",
        CalloutKind::Success => "success",
        CalloutKind::Question => "question",
        CalloutKind::Failure => "failure",
        CalloutKind::Danger => "danger",
        CalloutKind::Bug => "bug",
        CalloutKind::Example => "example",
        CalloutKind::Quote => "quote",
    }
}

/// A small stroked icon for a callout's title, drawn with `currentColor`.
fn callout_icon(kind: CalloutKind) -> String {
    let paths = match kind {
        CalloutKind::Note => {
            r#"<path d="M12 20h9"/><path d="M16.5 3.5a2.1 2.1 0 0 1 3 3L7 19l-4 1 1-4Z"/>"#
        }
        CalloutKind::Abstract => {
            r#"<rect x="8" y="2" width="8" height="4" rx="1"/><path d="M16 4h2a2 2 0 0 1 2 2v14a2 2 0 0 1-2 2H6a2 2 0 0 1-2-2V6a2 2 0 0 1 2-2h2M9 12h6M9 16h6"/>"#
        }
        CalloutKind::Info => r#"<circle cx="12" cy="12" r="10"/><path d="M12 16v-4M12 8h.01"/>"#,
        CalloutKind::Todo => r#"<circle cx="12" cy="12" r="10"/><path d="m9 12 2 2 4-4"/>"#,
        CalloutKind::Tip => {
            r#"<path d="M8.5 14.5A2.5 2.5 0 0 0 11 12c0-1.4-.5-2-1-3-1.1-2.1-.2-4 2-6 .5 2.5 2 4.9 4 6.5 2 1.6 3 3.5 3 5.5a7 7 0 1 1-14 0c0-1.2.4-2.3 1-3a2.5 2.5 0 0 0 2.5 2.5z"/>"#
        }
        CalloutKind::Important => {
            r#"<path d="M21 15a2 2 0 0 1-2 2H7l-4 4V5a2 2 0 0 1 2-2h14a2 2 0 0 1 2 2z"/><path d="M12 7v4M12 15h.01"/>"#
        }
        CalloutKind::Success => r#"<path d="M20 6 9 17l-5-5"/>"#,
        CalloutKind::Question => {
            r#"<circle cx="12" cy="12" r="10"/><path d="M9.1 9a3 3 0 0 1 5.8 1c0 2-3 3-3 3M12 17h.01"/>"#
        }
        CalloutKind::Warning => {
            r#"<path d="m21.7 18-8-14a2 2 0 0 0-3.4 0l-8 14A2 2 0 0 0 4 21h16a2 2 0 0 0 1.7-3Z"/><path d="M12 9v4M12 17h.01"/>"#
        }
        CalloutKind::Caution => {
            r#"<path d="M7.9 2h8.2L22 7.9v8.2L16.1 22H7.9L2 16.1V7.9Z"/><path d="M12 8v4M12 16h.01"/>"#
        }
        CalloutKind::Failure => r#"<path d="M18 6 6 18M6 6l12 12"/>"#,
        CalloutKind::Danger => r#"<path d="M13 2 3 14h9l-1 8 10-12h-9l1-8z"/>"#,
        CalloutKind::Bug => {
            r#"<rect x="8" y="6" width="8" height="14" rx="4"/><path d="M19 7l-3 2M5 7l3 2M19 19l-3-2M5 19l3-2M20 13h-4M4 13h4M10 4l1 2M14 4l-1 2"/>"#
        }
        CalloutKind::Example => r#"<path d="M8 6h13M8 12h13M8 18h13M3 6h.01M3 12h.01M3 18h.01"/>"#,
        CalloutKind::Quote => {
            r#"<path d="M3 21c3 0 7-1 7-8V5c0-1.3-.8-2-2-2H4c-1.3 0-2 .8-2 2v6c0 1.3.8 2 2 2 1 0 1 0 1 1v1c0 1-1 2-2 2s-1 0-1 1v3c0 1 0 1 1 1z"/><path d="M15 21c3 0 7-1 7-8V5c0-1.3-.8-2-2-2h-4c-1.3 0-2 .8-2 2v6c0 1.3.8 2 2 2h.8c0 2.3.2 4-2.8 4v3c0 1 0 1 1 1z"/>"#
        }
    };
    format!("<svg class=\"callout-icon\" viewBox=\"0 0 24 24\" aria-hidden=\"true\">{paths}</svg>")
}

/// A hard allocation boundary around pulldown-cmark's streaming renderer.
struct BoundedHTMLWriter {
    bytes: Vec<u8>,
    maximum: usize,
    attempted: usize,
}

impl BoundedHTMLWriter {
    fn new(maximum: usize, initial_capacity: usize) -> Self {
        Self {
            bytes: Vec::with_capacity(initial_capacity.min(maximum)),
            maximum,
            attempted: 0,
        }
    }

    fn error(&self) -> HTMLExportError {
        HTMLExportError::OutputTooLarge {
            actual: self.attempted.max(self.maximum.saturating_add(1)),
            maximum: self.maximum,
        }
    }

    fn into_bytes(self) -> Vec<u8> {
        self.bytes
    }
}

impl Write for BoundedHTMLWriter {
    fn write(&mut self, buffer: &[u8]) -> io::Result<usize> {
        self.attempted = self.bytes.len().saturating_add(buffer.len());
        if self.attempted > self.maximum {
            return Err(io::Error::new(
                io::ErrorKind::FileTooLarge,
                "rendered HTML exceeds its byte limit",
            ));
        }
        self.bytes.extend_from_slice(buffer);
        Ok(buffer.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

/// GitHub-style heading slug: lowercase letters and digits, spaces become
/// hyphens, everything else is dropped.
pub fn slugify(text: &str) -> String {
    let mut slug = String::with_capacity(text.len());
    for character in text.trim().chars() {
        if character.is_alphanumeric() {
            slug.extend(character.to_lowercase());
        } else if character == ' ' || character == '-' {
            slug.push('-');
        } else if character == '_' {
            slug.push('_');
        }
    }
    slug
}

fn sanitize_destination<'a>(
    destination: CowStr<'a>,
    allow_remote: bool,
    link_type: LinkType,
) -> CowStr<'a> {
    if link_type == LinkType::Email {
        return destination;
    }

    let lower = compact_lower(&destination);

    // Protocol-relative image URLs also perform a network request. Links may
    // navigate there only after an explicit click.
    if !allow_remote && lower.starts_with("//") {
        return CowStr::Borrowed("#");
    }

    let allowed = match scheme_of(&lower) {
        None => true,
        Some("http" | "https") => allow_remote,
        Some("mailto" | "file") => true,
        _ => false,
    };
    if allowed {
        destination
    } else {
        CowStr::Borrowed("#")
    }
}

/// Whether `destination` is a network address — `http(s):` or a
/// protocol-relative `//host` — read the way [`sanitize_destination`] reads
/// it, so the two can never disagree about what `remote_media` withheld.
fn is_remote(destination: &str) -> bool {
    let lower = compact_lower(destination);
    lower.starts_with("//") || matches!(scheme_of(&lower), Some("http" | "https"))
}

/// `destination` lowercased, without the ASCII controls and whitespace a
/// browser drops from a URL before reading its scheme.
fn compact_lower(destination: &str) -> String {
    destination
        .chars()
        .filter(|character| !character.is_ascii_control() && !character.is_ascii_whitespace())
        .collect::<String>()
        .to_ascii_lowercase()
}

/// The scheme of a [`compact_lower`]ed destination: what precedes the first
/// `:`, when that comes before any `/`, `?` or `#`.
fn scheme_of(lower: &str) -> Option<&str> {
    lower.find(':').and_then(|colon| {
        let boundary = lower.find(['/', '?', '#']).unwrap_or(usize::MAX);
        (colon < boundary).then_some(&lower[..colon])
    })
}

/// Copies local pictures into the export as `data:` URIs.
///
/// Only bytes that identify as a format browsers draw in `<img>` are copied,
/// and the MIME type comes from those bytes, never from the file name. An SVG
/// inside `<img>` cannot run script or load subresources, and the document's
/// Content-Security-Policy forbids both anyway.
struct ImageEmbedder {
    remaining: usize,
    cache: HashMap<PathBuf, Option<String>>,
}

impl ImageEmbedder {
    fn new(budget: usize) -> Self {
        Self {
            remaining: budget,
            cache: HashMap::new(),
        }
    }

    fn embed(&mut self, path: &Path) -> Option<String> {
        if let Some(cached) = self.cache.get(path) {
            return cached.clone();
        }
        let embedded = self.load(path);
        self.cache.insert(path.to_path_buf(), embedded.clone());
        embedded
    }

    fn load(&mut self, path: &Path) -> Option<String> {
        let limit = MAX_EMBEDDED_IMAGE_BYTES.min(self.remaining);
        let file = File::open(path).ok()?;
        let metadata = file.metadata().ok()?;
        if !metadata.is_file() || metadata.len() > limit as u64 {
            return None;
        }
        let mut bytes = Vec::with_capacity(metadata.len() as usize);
        // Read one byte past the limit so a file that grew after `metadata`
        // is refused rather than silently truncated.
        file.take(limit as u64 + 1).read_to_end(&mut bytes).ok()?;
        if bytes.len() > limit {
            return None;
        }
        let mime = sniff_image(&bytes)?;
        self.remaining -= bytes.len();
        let mut uri = String::with_capacity(bytes.len() / 3 * 4 + 64);
        uri.push_str("data:");
        uri.push_str(mime);
        uri.push_str(";base64,");
        base64_encode_into(&bytes, &mut uri);
        Some(uri)
    }
}

/// Resolves a Markdown image destination to a local file the way the native
/// renderer does: `file:` URLs, absolute paths, and paths relative to the
/// note's folder. Remote and directory destinations resolve to nothing.
fn local_image_path(destination: &str, base: &Path) -> Option<PathBuf> {
    let trimmed = destination.trim();
    if trimmed.is_empty() || trimmed == "#" {
        return None;
    }
    let decoded = percent_decode(trimmed)?;
    if decoded.starts_with("//") || decoded.ends_with('/') || decoded.contains('\0') {
        return None;
    }
    let lower = decoded.to_ascii_lowercase();
    if let Some(rest) = lower.strip_prefix("file:") {
        let offset = decoded.len() - rest.len();
        let rest = &decoded[offset..];
        let path = match rest.strip_prefix("//") {
            Some(authority_and_path) => {
                let slash = authority_and_path.find('/')?;
                let host = &authority_and_path[..slash];
                if !(host.is_empty() || host.eq_ignore_ascii_case("localhost")) {
                    return None;
                }
                &authority_and_path[slash..]
            }
            None => rest,
        };
        return Some(PathBuf::from(path));
    }
    let scheme_end = lower.find(':');
    let boundary = lower.find(['/', '?', '#']).unwrap_or(usize::MAX);
    if scheme_end.is_some_and(|colon| colon < boundary) {
        return None;
    }
    let path = Path::new(&decoded);
    if path.is_absolute() {
        Some(path.to_path_buf())
    } else {
        Some(base.join(path))
    }
}

fn percent_decode(value: &str) -> Option<String> {
    if !value.contains('%') {
        return Some(value.to_owned());
    }
    let bytes = value.as_bytes();
    let mut decoded = Vec::with_capacity(bytes.len());
    let mut index = 0;
    while index < bytes.len() {
        if bytes[index] == b'%' && index + 2 < bytes.len() {
            let hex = std::str::from_utf8(&bytes[index + 1..index + 3]).ok()?;
            if let Ok(byte) = u8::from_str_radix(hex, 16) {
                decoded.push(byte);
                index += 3;
                continue;
            }
        }
        decoded.push(bytes[index]);
        index += 1;
    }
    String::from_utf8(decoded).ok()
}

/// The browser image format `bytes` identify as, if any.
pub fn sniff_image(bytes: &[u8]) -> Option<&'static str> {
    if bytes.starts_with(b"\x89PNG\r\n\x1a\n") {
        return Some("image/png");
    }
    if bytes.starts_with(&[0xFF, 0xD8, 0xFF]) {
        return Some("image/jpeg");
    }
    if bytes.starts_with(b"GIF87a") || bytes.starts_with(b"GIF89a") {
        return Some("image/gif");
    }
    if bytes.len() >= 12 && &bytes[..4] == b"RIFF" && &bytes[8..12] == b"WEBP" {
        return Some("image/webp");
    }
    if bytes.len() >= 12 && &bytes[4..8] == b"ftyp" {
        match &bytes[8..12] {
            b"avif" | b"avis" => return Some("image/avif"),
            _ => {}
        }
    }
    if bytes.starts_with(b"BM") && bytes.len() >= 26 {
        return Some("image/bmp");
    }
    if bytes.starts_with(&[0, 0, 1, 0]) && bytes.len() >= 22 {
        return Some("image/x-icon");
    }
    if looks_like_svg(bytes) {
        return Some("image/svg+xml");
    }
    None
}

/// An SVG document is UTF-8 XML whose root element is `svg`. Skip the prolog
/// (BOM, XML declaration, comments, doctype) and require that root.
fn looks_like_svg(bytes: &[u8]) -> bool {
    let Ok(text) = std::str::from_utf8(bytes) else {
        return false;
    };
    let mut rest = text.strip_prefix('\u{FEFF}').unwrap_or(text);
    loop {
        rest = rest.trim_start();
        if let Some(after) = rest.strip_prefix("<?") {
            let Some(end) = after.find("?>") else {
                return false;
            };
            rest = &after[end + 2..];
        } else if let Some(after) = rest.strip_prefix("<!--") {
            let Some(end) = after.find("-->") else {
                return false;
            };
            rest = &after[end + 3..];
        } else if rest.len() >= 9 && rest[..9].eq_ignore_ascii_case("<!doctype") {
            let Some(end) = rest.find('>') else {
                return false;
            };
            rest = &rest[end + 1..];
        } else {
            break;
        }
    }
    let Some(after) = rest.strip_prefix('<') else {
        return false;
    };
    let name_end = after
        .find(|character: char| character.is_whitespace() || character == '>' || character == '/')
        .unwrap_or(after.len());
    let name = &after[..name_end];
    let local = name.rsplit(':').next().unwrap_or(name);
    local == "svg"
}

fn base64_encode_into(bytes: &[u8], output: &mut String) {
    const ALPHABET: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let (chunks, remainder) = bytes.as_chunks::<3>();
    for chunk in chunks {
        let n = (u32::from(chunk[0]) << 16) | (u32::from(chunk[1]) << 8) | u32::from(chunk[2]);
        for shift in [18, 12, 6, 0] {
            output.push(ALPHABET[((n >> shift) & 63) as usize] as char);
        }
    }
    match remainder.len() {
        1 => {
            let n = u32::from(remainder[0]) << 16;
            output.push(ALPHABET[((n >> 18) & 63) as usize] as char);
            output.push(ALPHABET[((n >> 12) & 63) as usize] as char);
            output.push_str("==");
        }
        2 => {
            let n = (u32::from(remainder[0]) << 16) | (u32::from(remainder[1]) << 8);
            output.push(ALPHABET[((n >> 18) & 63) as usize] as char);
            output.push(ALPHABET[((n >> 12) & 63) as usize] as char);
            output.push(ALPHABET[((n >> 6) & 63) as usize] as char);
            output.push('=');
        }
        _ => {}
    }
}

const DOCUMENT_PREFIX: &str = r#"<!doctype html>
<html lang="en">
<head>
<meta charset="utf-8">
<meta name="viewport" content="width=device-width, initial-scale=1">
<meta name="color-scheme" content="light dark">
<meta name="generator" content="MarkDev">
<meta name="referrer" content="no-referrer">
<meta http-equiv="Content-Security-Policy" content="default-src 'none'; img-src 'self' data: file:; media-src 'self' data: file:; style-src 'unsafe-inline'; font-src data:; object-src 'none'; base-uri 'none'; form-action 'none'">
<title>"#;

/// Plain CSS that works in every current browser engine (Safari, Chrome,
/// Edge, Firefox) without script, fonts, or network access. Colours are
/// custom properties so the dark palette and print styles only swap values.
const DOCUMENT_MIDDLE: &str = r#"</title>
<style>
:root {
  color-scheme: light dark;
  --text: #1f2328; --muted: #59636e; --bg: #ffffff; --subtle: #f6f8fa;
  --border: #d1d9e0; --link: #0969da; --mark: #fff8c5; --accent: #b91c1c;
  --note: #0969da; --tip: #1a7f37; --important: #8250df; --warning: #9a6700; --caution: #cf222e;
  --cyan: #0a8f8c; --orange: #c2410c; --gray: #6e7781;
  font: 17px/1.65 -apple-system, BlinkMacSystemFont, "Segoe UI", "Helvetica Neue", Helvetica, Arial, sans-serif;
  -webkit-text-size-adjust: 100%; text-size-adjust: 100%;
}
@media (prefers-color-scheme: dark) {
  :root {
    --text: #e6edf3; --muted: #9198a1; --bg: #0d1117; --subtle: #151b23;
    --border: #3d444d; --link: #4493f8; --mark: #bb800926; --accent: #f87171;
    --note: #4493f8; --tip: #3fb950; --important: #ab7df8; --warning: #d29922; --caution: #f85149;
    --cyan: #2cc7c3; --orange: #fb923c; --gray: #9198a1;
  }
}
*, *::before, *::after { box-sizing: border-box; }
html { background: var(--bg); }
body { max-width: 780px; margin: 0 auto; padding: 3rem 1.5rem 5rem; color: var(--text); background: var(--bg); overflow-wrap: break-word; }
h1, h2, h3, h4, h5, h6 { position: relative; margin: 1.8em 0 .6em; line-height: 1.25; font-weight: 650; scroll-margin-top: 1rem; }
h1 { font-size: 2em; padding-bottom: .3em; border-bottom: 1px solid var(--border); }
h2 { font-size: 1.5em; padding-bottom: .3em; border-bottom: 1px solid var(--border); }
h3 { font-size: 1.25em; } h4 { font-size: 1em; } h5 { font-size: .9em; } h6 { font-size: .85em; color: var(--muted); }
main > :first-child { margin-top: 0; }
.anchor { margin-inline-start: .35em; color: var(--muted); text-decoration: none; font-weight: 400; opacity: 0; transition: opacity .15s; }
h1:hover .anchor, h2:hover .anchor, h3:hover .anchor, h4:hover .anchor, h5:hover .anchor, h6:hover .anchor, .anchor:focus { opacity: 1; }
p, ul, ol, dl, table, pre, blockquote, figure { margin: 0 0 1em; }
a { color: var(--link); text-underline-offset: .15em; }
a:not(:hover) { text-decoration: none; }
strong { font-weight: 650; }
mark { background: var(--mark); color: inherit; padding: 0 .15em; border-radius: 3px; }
del { color: var(--muted); }
hr { height: 1px; margin: 2em 0; border: 0; background: var(--border); }
ul, ol { padding-inline-start: 1.8em; }
li + li { margin-top: .25em; }
li > ul, li > ol { margin-bottom: 0; }
li:has(> input[type="checkbox"]) { list-style: none; margin-inline-start: -1.4em; }
input[type="checkbox"] { width: 1em; height: 1em; margin: 0 .45em 0 0; vertical-align: -.1em; accent-color: var(--accent); }
dt { font-weight: 650; margin-top: .6em; }
dd { margin-inline-start: 1.5em; }
pre, code, kbd, samp { font-family: ui-monospace, SFMono-Regular, "SF Mono", Menlo, Consolas, "Liberation Mono", monospace; }
code { font-size: .875em; padding: .15em .35em; border-radius: 6px; background: var(--subtle); }
pre { overflow-x: auto; padding: 1rem 1.1rem; border: 1px solid var(--border); border-radius: 10px; background: var(--subtle); line-height: 1.5; tab-size: 4; }
pre code { padding: 0; background: none; font-size: .85em; }
pre:has(> code[class*="language-"]) { position: relative; }
pre > code[class*="language-"]::before { position: absolute; top: .35rem; right: .6rem; color: var(--muted); font: 600 .68rem/1 -apple-system, BlinkMacSystemFont, "Segoe UI", sans-serif; letter-spacing: .04em; text-transform: uppercase; }
pre > code.language-mermaid::before { content: "Mermaid"; }
pre > code.language-math::before, pre > code.language-latex::before, pre > code.language-tex::before { content: "Math"; }
kbd { font-size: .8em; padding: .1em .4em; border: 1px solid var(--border); border-bottom-width: 2px; border-radius: 5px; background: var(--subtle); }
.math { font-family: math, "STIX Two Math", "Cambria Math", "Latin Modern Math", "Times New Roman", serif; }
math { font-family: math, "STIX Two Math", "Cambria Math", "Latin Modern Math", serif; font-size: 1.08em; }
.math-source { font-style: italic; }
p.math-display { margin: 1em 0; }
.math-display { display: block; overflow-x: auto; margin: 1em 0; padding: .5em 0; text-align: center; font-size: 1.1em; }
blockquote { padding: .1em 1em; border-inline-start: 4px solid var(--border); color: var(--muted); }
blockquote > :last-child { margin-bottom: 0; }
.callout { --callout: var(--note); margin: 0 0 1em; padding: .7em 1em; border-inline-start: 4px solid var(--callout); border-radius: 8px; background: color-mix(in srgb, var(--callout) 9%, transparent); overflow: hidden; }
.callout-title { display: flex; align-items: center; gap: .5em; color: var(--callout); font-weight: 650; line-height: 1.35; }
.callout-title-inner { flex: 1; min-width: 0; }
.callout-icon { width: 1.1em; height: 1.1em; flex: none; fill: none; stroke: currentColor; stroke-width: 2; stroke-linecap: round; stroke-linejoin: round; }
.callout-content { margin-top: .55em; }
.callout-content > :last-child { margin-bottom: 0; }
.callout-content:empty, .callout-content > p:empty { display: none; }
details.callout > summary { cursor: pointer; list-style: none; }
details.callout > summary::-webkit-details-marker { display: none; }
details.callout > summary::after { content: ""; width: .5em; height: .5em; margin-inline: .25em; border: solid currentColor; border-width: 0 2px 2px 0; transform: rotate(-45deg); transition: transform .15s; }
details.callout[open] > summary::after { transform: rotate(45deg); }
details.callout:not([open]) { padding-bottom: .7em; }
.callout[data-callout="note"], .callout[data-callout="info"], .callout[data-callout="todo"] { --callout: var(--note); }
.callout[data-callout="abstract"], .callout[data-callout="tip"] { --callout: var(--cyan); }
.callout[data-callout="success"] { --callout: var(--tip); }
.callout[data-callout="question"], .callout[data-callout="warning"] { --callout: var(--orange); }
.callout[data-callout="failure"], .callout[data-callout="danger"], .callout[data-callout="bug"], .callout[data-callout="caution"] { --callout: var(--caution); }
.callout[data-callout="important"], .callout[data-callout="example"] { --callout: var(--important); }
.callout[data-callout="quote"] { --callout: var(--gray); }
.tag { display: inline-block; padding: 0 .55em; border-radius: 999px; background: color-mix(in srgb, var(--link) 12%, transparent); color: var(--link); font-size: .85em; line-height: 1.6; }
.markdown-embed { margin: 0 0 1em; padding: .1em 0 .1em 1em; border-inline-start: 3px solid var(--border); }
.markdown-embed-title { margin: .2em 0 .5em; font-size: .85em; font-weight: 650; }
.markdown-embed-title a { color: var(--muted); }
.markdown-embed-content > :first-child { margin-top: 0; }
.markdown-embed-content > :last-child { margin-bottom: 0; }
.embed-link::before { content: "↪ "; color: var(--muted); }
.pdf-embed::before { content: "PDF · "; color: var(--muted); font-size: .8em; font-weight: 650; }
.media-embed { display: block; max-width: 100%; margin: .5em 0; }
main p:empty { display: none; }
li:has(> input[type="checkbox"]:checked, > p > input[type="checkbox"]:checked) { color: var(--muted); text-decoration: line-through; }
li:has(> input[type="checkbox"]:checked) li { color: var(--text); text-decoration: none; }
.block-id { scroll-margin-top: 1rem; }
table { display: block; width: max-content; max-width: 100%; overflow-x: auto; border-collapse: collapse; font-variant-numeric: tabular-nums; }
th, td { padding: .45rem .8rem; border: 1px solid var(--border); text-align: start; vertical-align: top; }
th { font-weight: 650; background: var(--subtle); }
tbody tr:nth-child(even) { background: color-mix(in srgb, var(--subtle) 60%, transparent); }
img { max-width: 100%; height: auto; border-radius: 4px; }
img[src$=".svg"], img[src^="data:image/svg+xml"] { border-radius: 0; }
sup { line-height: 0; }
.footnote-reference a { text-decoration: none; }
.footnote-definition { display: flex; align-items: baseline; gap: .5em; margin-top: .5em; font-size: .9em; color: var(--muted); }
.footnote-definition:first-of-type { margin-top: 2.5em; padding-top: 1em; border-top: 1px solid var(--border); }
.footnote-definition p { margin: 0; }
.footnote-definition-label { font-weight: 650; }
:target { scroll-margin-top: 1rem; }
.footnote-definition:target, h1:target, h2:target, h3:target, h4:target { background: var(--mark); }
@media (max-width: 600px) {
  :root { font-size: 16px; }
  body { padding: 1.5rem 1rem 3rem; }
  pre { margin-inline: -1rem; border-radius: 0; border-inline: 0; }
}
@media print {
  :root { --text: #000; --muted: #444; --bg: #fff; --subtle: #f5f5f5; --border: #bbb; --link: #000; font-size: 11pt; }
  body { max-width: none; padding: 0; }
  .anchor { display: none; }
  a[href^="http"]::after { content: " (" attr(href) ")"; font-size: .85em; color: var(--muted); overflow-wrap: anywhere; }
  pre { white-space: pre-wrap; }
  pre, blockquote, table, img, figure { break-inside: avoid; }
  h1, h2, h3, h4, h5, h6 { break-after: avoid; }
}
</style>
</head>
<body>
<main>
"#;

const DOCUMENT_SUFFIX: &str = "</main>\n</body>\n</html>\n";

#[cfg(test)]
mod tests {
    use super::BoundedHTMLWriter;
    use std::io::Write;

    #[test]
    fn bounded_writer_refuses_before_allocating_the_first_excess_byte() {
        let mut writer = BoundedHTMLWriter::new(4, 4);
        writer.write_all(b"1234").unwrap();
        assert!(writer.write_all(b"5").is_err());
        assert_eq!(writer.bytes, b"1234");
        assert_eq!(writer.attempted, 5);
    }
}

#[cfg(all(test, feature = "mathml"))]
mod mathml_tests {
    use super::sanitize_mathml;

    #[test]
    fn sanitizer_escapes_stray_markup_and_refuses_active_attributes() {
        assert_eq!(
            sanitize_mathml("<math><mo><</mo><mi>x</mi><mo>></mo></math>").as_deref(),
            Some("<math><mo>&lt;</mo><mi>x</mi><mo>&gt;</mo></math>")
        );
        assert_eq!(
            sanitize_mathml("<math><script>x</script></math>").as_deref(),
            Some("<math>&lt;script&gt;x&lt;/script&gt;</math>")
        );
        assert_eq!(sanitize_mathml("<math onload=\"x()\"></math>"), None);
        assert_eq!(sanitize_mathml("<math href=\"javascript:x\"></math>"), None);
        assert_eq!(
            sanitize_mathml("<mrow style=\"background:url(x)\"></mrow>"),
            None
        );
        assert_eq!(
            sanitize_mathml("<mi>a &amp; b & c</mi>").as_deref(),
            Some("<mi>a &amp; b &amp; c</mi>")
        );
        assert!(sanitize_mathml("<mrow style=\"color: rgb(255 0 0)\"><mi>x</mi></mrow>").is_some());
    }
}
