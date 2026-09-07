//! MarkDev flat-model parse + safe HTML render for GitPulse surfaces.
//!
//! Offsets in the model are UTF-16 (JavaScript string indices). The renderer
//! escapes every text run and refuses active URL schemes — repository
//! markdown is untrusted.

use markdev::md::model::{
    BlockDescriptor, BlockKind, CalloutKind, ParseResult, SpanKind, StyleSpan, NO_INFO,
};
use markdev::{parse_checked, ParseError};
use serde::{Deserialize, Serialize};

/// Ceiling shared with the TypeScript client. Enforced before IPC so a
/// megabyte CHANGELOG cannot freeze the UI or the backend.
pub const MAX_RENDER_BYTES: usize = 128 * 1024;

/// Enriched parse handed to the frontend: flat arrays plus resolved
/// string-table lookups so the client never indexes `strings` itself.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct ParsedMarkdown {
    pub source: String,
    pub spans: Vec<ResolvedSpan>,
    pub markers: Vec<ResolvedMarker>,
    pub blocks: Vec<ResolvedBlock>,
    pub truncated: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct ResolvedSpan {
    pub start: u32,
    pub end: u32,
    pub kind: String,
    pub depth: u16,
    pub data: u32,
    /// Resolved string-table entry when the span carries one (link href, etc.).
    pub text: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct ResolvedMarker {
    pub start: u32,
    pub end: u32,
    pub block: u32,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct ResolvedBlock {
    pub start: u32,
    pub end: u32,
    pub kind: String,
    pub depth: u16,
    pub data: u32,
    pub info: Option<String>,
}

/// Parses `text`, capping at [`MAX_RENDER_BYTES`] and saying so.
pub fn parse(text: &str) -> Result<ParsedMarkdown, String> {
    let (slice, truncated) = cap(text);
    let parsed = parse_checked(slice).map_err(describe_parse_error)?;
    Ok(enrich(slice, parsed, truncated))
}

/// Parses and renders safe HTML for embedding in the app.
pub fn render(text: &str) -> Result<String, String> {
    let (slice, truncated) = cap(text);
    if slice.is_empty() {
        return Ok(String::new());
    }
    let parsed = parse_checked(slice).map_err(describe_parse_error)?;
    let mut html = render_from_parse(slice, &parsed);
    if truncated {
        let remaining = text.len().saturating_sub(MAX_RENDER_BYTES);
        html.push_str(&truncation_notice(remaining));
    }
    Ok(html)
}

fn cap(text: &str) -> (&str, bool) {
    if text.len() <= MAX_RENDER_BYTES {
        return (text, false);
    }
    // Prefer a char boundary so we never split a codepoint.
    let mut end = MAX_RENDER_BYTES;
    while end > 0 && !text.is_char_boundary(end) {
        end -= 1;
    }
    (&text[..end], true)
}

fn enrich(source: &str, parsed: ParseResult, truncated: bool) -> ParsedMarkdown {
    let spans = parsed
        .spans
        .iter()
        .map(|span| ResolvedSpan {
            start: span.start,
            end: span.end,
            kind: span_kind_name(span.kind).to_string(),
            depth: span.depth,
            data: span.data,
            text: span_string(&parsed, span),
        })
        .collect();
    let markers = parsed
        .markers
        .iter()
        .map(|m| ResolvedMarker {
            start: m.start,
            end: m.end,
            block: m.block,
        })
        .collect();
    let blocks = parsed
        .blocks
        .iter()
        .map(|b| ResolvedBlock {
            start: b.start,
            end: b.end,
            kind: block_kind_name(b.kind).to_string(),
            depth: b.depth,
            data: b.data,
            info: info_string(&parsed, b),
        })
        .collect();
    ParsedMarkdown {
        source: source.to_string(),
        spans,
        markers,
        blocks,
        truncated,
    }
}

fn span_string(parsed: &ParseResult, span: &StyleSpan) -> Option<String> {
    match span.kind {
        k if k == SpanKind::Link as u16
            || k == SpanKind::WikiLink as u16
            || k == SpanKind::Image as u16
            || k == SpanKind::FootnoteReference as u16 =>
        {
            parsed.strings.get(span.data as usize).cloned()
        }
        _ => None,
    }
}

fn info_string(parsed: &ParseResult, block: &BlockDescriptor) -> Option<String> {
    if block.info == NO_INFO {
        None
    } else {
        parsed.strings.get(block.info as usize).cloned()
    }
}

fn span_kind_name(kind: u16) -> &'static str {
    match kind {
        k if k == SpanKind::Emphasis as u16 => "emphasis",
        k if k == SpanKind::Strong as u16 => "strong",
        k if k == SpanKind::Strikethrough as u16 => "strikethrough",
        k if k == SpanKind::Superscript as u16 => "superscript",
        k if k == SpanKind::Subscript as u16 => "subscript",
        k if k == SpanKind::InlineCode as u16 => "inlineCode",
        k if k == SpanKind::Link as u16 => "link",
        k if k == SpanKind::WikiLink as u16 => "wikiLink",
        k if k == SpanKind::Image as u16 => "image",
        k if k == SpanKind::InlineMath as u16 => "inlineMath",
        k if k == SpanKind::Heading as u16 => "heading",
        k if k == SpanKind::TaskMarker as u16 => "taskMarker",
        k if k == SpanKind::FootnoteReference as u16 => "footnoteReference",
        k if k == SpanKind::Tag as u16 => "tag",
        k if k == SpanKind::Highlight as u16 => "highlight",
        k if k == SpanKind::InlineHtml as u16 => "inlineHtml",
        _ => "unknown",
    }
}

fn block_kind_name(kind: u16) -> &'static str {
    match kind {
        k if k == BlockKind::Paragraph as u16 => "paragraph",
        k if k == BlockKind::Heading as u16 => "heading",
        k if k == BlockKind::CodeBlock as u16 => "codeBlock",
        k if k == BlockKind::MermaidBlock as u16 => "mermaidBlock",
        k if k == BlockKind::MathBlock as u16 => "mathBlock",
        k if k == BlockKind::Table as u16 => "table",
        k if k == BlockKind::TableHead as u16 => "tableHead",
        k if k == BlockKind::TableRow as u16 => "tableRow",
        k if k == BlockKind::TableCell as u16 => "tableCell",
        k if k == BlockKind::BlockQuote as u16 => "blockQuote",
        k if k == BlockKind::Callout as u16 => "callout",
        k if k == BlockKind::List as u16 => "list",
        k if k == BlockKind::ListItem as u16 => "listItem",
        k if k == BlockKind::Rule as u16 => "rule",
        k if k == BlockKind::Frontmatter as u16 => "frontmatter",
        k if k == BlockKind::FootnoteDefinition as u16 => "footnoteDefinition",
        k if k == BlockKind::HtmlBlock as u16 => "htmlBlock",
        k if k == BlockKind::DefinitionList as u16 => "definitionList",
        k if k == BlockKind::DefinitionListTitle as u16 => "definitionListTitle",
        k if k == BlockKind::DefinitionListDefinition as u16 => "definitionListDefinition",
        k if k == BlockKind::LinkReferenceDefinition as u16 => "linkReferenceDefinition",
        _ => "unknown",
    }
}

fn describe_parse_error(err: ParseError) -> String {
    match err {
        ParseError::SourceTooLarge => "markdown exceeds the parse size limit".into(),
        ParseError::InteriorNul => "markdown contains an interior NUL".into(),
        ParseError::TooManyEvents => "markdown produced too many parse events".into(),
        ParseError::TooDeep => "markdown nesting exceeded the depth limit".into(),
        ParseError::TooManyRecords => "markdown produced too many structural records".into(),
        ParseError::TooManyStrings => "markdown interned too many strings".into(),
        ParseError::StringTooLong => "an interned markdown string is too large".into(),
        ParseError::TooManyStringBytes => "markdown string table exceeds the size limit".into(),
    }
}

fn truncation_notice(remaining: usize) -> String {
    let remaining_label = format_grouped(remaining);
    format!(
        "<div class=\"my-3 rounded-xl border border-amber-500/40 bg-amber-500/10 px-3.5 py-2 text-[11px] text-amber-300\">\
         Rendered the first {} KB of this document. \
         {remaining_label} more characters are not shown — rendering the whole file \
         would take long enough to freeze the view. Open it in the code viewer to read all of it.\
         </div>",
        MAX_RENDER_BYTES / 1024,
    )
}

fn format_grouped(n: usize) -> String {
    let digits: Vec<char> = n.to_string().chars().collect();
    let mut out = String::new();
    for (i, ch) in digits.iter().enumerate() {
        if i > 0 && (digits.len() - i).is_multiple_of(3) {
            out.push(',');
        }
        out.push(*ch);
    }
    out
}

/// Renders HTML from the flat model. Text is always escaped; links pass
/// [`safe_url`].
pub fn render_from_parse(source: &str, parsed: &ParseResult) -> String {
    let units: Vec<u16> = source.encode_utf16().collect();
    let top: Vec<&BlockDescriptor> = parsed.blocks.iter().filter(|b| b.depth == 0).collect();

    let mut out = String::with_capacity(source.len().saturating_mul(2));
    let mut cursor = 0u32;

    for block in top {
        if block.start > cursor {
            let gap = slice_utf16(&units, cursor, block.start);
            if !gap.trim().is_empty() {
                out.push_str(&escape_html(gap));
            }
        }
        render_block(source, &units, parsed, block, &mut out);
        cursor = block.end;
    }
    if (cursor as usize) < units.len() {
        let tail = slice_utf16(&units, cursor, units.len() as u32);
        if !tail.trim().is_empty() {
            out.push_str(&escape_html(tail));
        }
    }
    out
}

fn render_block(
    source: &str,
    units: &[u16],
    parsed: &ParseResult,
    block: &BlockDescriptor,
    out: &mut String,
) {
    let kind = block.kind;
    if kind == BlockKind::Heading as u16 {
        let level = block.data.clamp(1, 6);
        let inner = inline_html(source, units, parsed, block.start, block.end);
        let plain = strip_tags_approx(&inner);
        let id = slugify(&plain);
        out.push_str(&format!(
            "<h{level} id=\"{id}\" class=\"scroll-mt-4\">{inner}</h{level}>"
        ));
        return;
    }
    if kind == BlockKind::Paragraph as u16 {
        let inner = inline_html(source, units, parsed, block.start, block.end);
        out.push_str(&format!(
            "<p class=\"my-2 leading-relaxed text-textPrimary/90\">{inner}</p>"
        ));
        return;
    }
    if kind == BlockKind::CodeBlock as u16 || kind == BlockKind::MermaidBlock as u16 {
        let lang = info_string(parsed, block).unwrap_or_else(|| "plaintext".into());
        let body = code_body(source, units, parsed, block);
        if kind == BlockKind::MermaidBlock as u16 || lang.eq_ignore_ascii_case("mermaid") {
            out.push_str(&format!(
                "<div class=\"my-4 rounded-xl border border-border/80 bg-surface overflow-hidden shadow-card\">\
                 <div class=\"flex items-center justify-between px-3 py-1.5 border-b border-border/60 bg-surfaceHover/50 select-none\">\
                 <div class=\"flex items-center gap-2\">\
                 <span class=\"w-2 h-2 rounded-full bg-purple-400\"></span>\
                 <span class=\"font-mono text-[10px] font-bold text-purple-400 uppercase\">MERMAID DIAGRAM</span>\
                 </div>\
                 <button type=\"button\" class=\"gp-btn py-0.5! px-2! text-[10px] copy-code-btn\" data-code=\"{escaped}\" title=\"Copy diagram source\">Copy</button>\
                 </div>\
                 <div class=\"p-4 overflow-x-auto\"><pre class=\"leading-relaxed\"><code>{escaped}</code></pre></div></div>",
                escaped = escape_html(&body)
            ));
            return;
        }
        out.push_str(&format!(
            "<div class=\"my-4 rounded-xl border border-border/80 bg-surface overflow-hidden shadow-card\">\
             <div class=\"flex items-center justify-between px-3 py-1.5 border-b border-border/60 bg-surfaceHover/50 select-none\">\
             <span class=\"font-mono text-[10px] font-bold text-accent uppercase\">{lang_esc}</span>\
             <button type=\"button\" class=\"gp-btn py-0.5! px-2! text-[10px] copy-code-btn\" data-code=\"{escaped}\" title=\"Copy code\">Copy</button>\
             </div>\
             <pre class=\"p-3 overflow-x-auto text-xs leading-relaxed\"><code>{escaped}</code></pre></div>",
            lang_esc = escape_html(lang.to_uppercase()),
            escaped = escape_html(&body)
        ));
        return;
    }
    if kind == BlockKind::MathBlock as u16 {
        let body = code_body(source, units, parsed, block);
        out.push_str(&format!(
            "<div class=\"my-4 rounded-xl border border-cyan-500/40 bg-cyan-500/5 px-4 py-3\">\
             <div class=\"text-[10px] font-mono font-bold text-cyan-400 mb-1\">FORMULA</div>\
             <div class=\"text-sm font-semibold tracking-wide text-cyan-300\">{esc}</div></div>",
            esc = escape_html(body.trim())
        ));
        return;
    }
    if kind == BlockKind::Callout as u16 {
        let (label, border) = callout_style(block.data);
        let inner = inline_html(source, units, parsed, block.start, block.end);
        out.push_str(&format!(
            "<div class=\"my-3 rounded-xl border {border} bg-surface/60 px-3.5 py-2.5\">\
             <div class=\"text-[11px] font-bold mb-1\">{label}</div>\
             <div class=\"text-[12px] text-textPrimary/90\">{inner}</div></div>"
        ));
        return;
    }
    if kind == BlockKind::BlockQuote as u16 {
        let inner = inline_html(source, units, parsed, block.start, block.end);
        out.push_str(&format!(
            "<blockquote class=\"border-l-2 border-accent/60 pl-3 py-1 my-2 text-textMuted italic bg-accent/5 rounded-r-lg\">{inner}</blockquote>"
        ));
        return;
    }
    if kind == BlockKind::List as u16 {
        let ordered = block.data == 1;
        let tag = if ordered { "ol" } else { "ul" };
        out.push_str(&format!("<{tag} class=\"my-2 ml-4 list-outside\">"));
        for child in children(parsed, block) {
            if child.kind == BlockKind::ListItem as u16 {
                let item = inline_html(source, units, parsed, child.start, child.end);
                let task = parsed.spans.iter().find(|s| {
                    s.kind == SpanKind::TaskMarker as u16
                        && s.start >= child.start
                        && s.end <= child.end
                });
                if let Some(marker) = task {
                    let checked = marker.data == 1;
                    let cls = if checked {
                        "line-through text-textMuted"
                    } else {
                        "text-textPrimary"
                    };
                    let mark = if checked { "✓" } else { "☐" };
                    out.push_str(&format!(
                        "<li class=\"my-0.5 flex items-start gap-2 {cls}\"><span>{mark}</span><span>{item}</span></li>"
                    ));
                } else {
                    out.push_str(&format!(
                        "<li class=\"my-0.5 text-textPrimary/90\">{item}</li>"
                    ));
                }
            }
        }
        out.push_str(&format!("</{tag}>"));
        return;
    }
    if kind == BlockKind::Table as u16 {
        out.push_str("<div class=\"my-3 overflow-x-auto\"><table class=\"w-full text-[12px] border-collapse\">");
        for child in children(parsed, block) {
            render_table_part(source, units, parsed, child, out);
        }
        out.push_str("</table></div>");
        return;
    }
    if kind == BlockKind::Rule as u16 {
        out.push_str("<hr class=\"border-border/60 my-4\" />");
        return;
    }
    if kind == BlockKind::Frontmatter as u16 {
        let body = slice_utf16(units, block.start, block.end);
        out.push_str(&render_frontmatter(&body));
        return;
    }
    if kind == BlockKind::HtmlBlock as u16 {
        // Raw HTML is text — never executed.
        let body = slice_utf16(units, block.start, block.end);
        out.push_str(&format!(
            "<pre class=\"my-2 text-[11px] text-textMuted\">{esc}</pre>",
            esc = escape_html(body)
        ));
        return;
    }
    // Fallback: escaped source slice.
    let body = slice_utf16(units, block.start, block.end);
    out.push_str(&format!(
        "<div class=\"my-2\">{esc}</div>",
        esc = escape_html(body)
    ));
}

fn render_table_part(
    source: &str,
    units: &[u16],
    parsed: &ParseResult,
    block: &BlockDescriptor,
    out: &mut String,
) {
    if block.kind == BlockKind::TableHead as u16 {
        out.push_str("<thead>");
        for row in children(parsed, block) {
            render_table_row(source, units, parsed, row, out, true);
        }
        out.push_str("</thead>");
    } else if block.kind == BlockKind::TableRow as u16 {
        render_table_row(source, units, parsed, block, out, false);
    } else {
        for row in children(parsed, block) {
            render_table_row(source, units, parsed, row, out, false);
        }
    }
}

fn render_table_row(
    source: &str,
    units: &[u16],
    parsed: &ParseResult,
    row: &BlockDescriptor,
    out: &mut String,
    header: bool,
) {
    if row.kind != BlockKind::TableRow as u16 && row.kind != BlockKind::TableHead as u16 {
        // Walk cells directly when the row wrapper is the head itself.
    }
    out.push_str("<tr>");
    let cells: Vec<&BlockDescriptor> =
        if row.kind == BlockKind::TableRow as u16 || row.kind == BlockKind::TableHead as u16 {
            children(parsed, row)
        } else {
            vec![row]
        };
    for cell in cells {
        if cell.kind != BlockKind::TableCell as u16 {
            continue;
        }
        let align = cell.data & 0b11;
        let align_class = match align {
            2 => "text-center",
            3 => "text-right",
            _ => "text-left",
        };
        let tag = if header { "th" } else { "td" };
        let inner = inline_html(source, units, parsed, cell.start, cell.end);
        out.push_str(&format!(
            "<{tag} class=\"px-3.5 py-2 {align_class} border-b border-border/50\">{inner}</{tag}>"
        ));
    }
    out.push_str("</tr>");
}

fn children<'a>(parsed: &'a ParseResult, parent: &BlockDescriptor) -> Vec<&'a BlockDescriptor> {
    parsed
        .blocks
        .iter()
        .filter(|b| b.depth == parent.depth + 1 && b.start >= parent.start && b.end <= parent.end)
        .collect()
}

fn code_body(
    _source: &str,
    units: &[u16],
    parsed: &ParseResult,
    block: &BlockDescriptor,
) -> String {
    // Prefer content between markers (fence lines), else the whole block.
    let markers: Vec<_> = parsed
        .markers
        .iter()
        .filter(|m| {
            (m.block as usize) < parsed.blocks.len()
                && parsed.blocks[m.block as usize].start == block.start
                && parsed.blocks[m.block as usize].end == block.end
        })
        .collect();
    if markers.len() >= 2 {
        let start = markers.first().map(|m| m.end).unwrap_or(block.start);
        let end = markers.last().map(|m| m.start).unwrap_or(block.end);
        if end > start {
            return slice_utf16(units, start, end).to_string();
        }
    }
    slice_utf16(units, block.start, block.end).to_string()
}

fn inline_html(_source: &str, units: &[u16], parsed: &ParseResult, start: u32, end: u32) -> String {
    let spans: Vec<&StyleSpan> = parsed
        .spans
        .iter()
        .filter(|s| s.start >= start && s.end <= end && s.end > s.start)
        .collect();
    if spans.is_empty() {
        return escape_html(slice_utf16(units, start, end));
    }

    // Emit text with the deepest overlapping span applied greedily left-to-right.
    let mut out = String::new();
    let mut cursor = start;
    let mut ordered = spans;
    ordered.sort_by_key(|s| (s.start, std::cmp::Reverse(s.end - s.start)));

    while cursor < end {
        let next = ordered.iter().find(|s| s.start >= cursor && s.start < end);
        let Some(span) = next else {
            out.push_str(&escape_html(slice_utf16(units, cursor, end)));
            break;
        };
        if span.start > cursor {
            out.push_str(&escape_html(slice_utf16(units, cursor, span.start)));
        }
        let content = slice_utf16(units, span.start, span.end);
        out.push_str(&wrap_span(parsed, span, content));
        cursor = span.end;
    }
    out
}

fn wrap_span(parsed: &ParseResult, span: &StyleSpan, content: impl AsRef<str>) -> String {
    let content = content.as_ref();
    let escaped = escape_html(content);
    match span.kind {
        k if k == SpanKind::Strong as u16 => {
            format!("<strong class=\"font-bold text-textPrimary\">{escaped}</strong>")
        }
        k if k == SpanKind::Emphasis as u16 => {
            format!("<em class=\"italic text-textPrimary/90\">{escaped}</em>")
        }
        k if k == SpanKind::Strikethrough as u16 => {
            format!("<del class=\"line-through text-textMuted\">{escaped}</del>")
        }
        k if k == SpanKind::InlineCode as u16 => format!(
            "<code class=\"bg-surface px-1.5 py-0.5 rounded text-amber-300 font-mono text-[11px] border border-border/60\">{escaped}</code>"
        ),
        k if k == SpanKind::Highlight as u16 => {
            format!("<mark class=\"bg-amber-400/25 text-textPrimary rounded px-0.5\">{escaped}</mark>")
        }
        k if k == SpanKind::InlineMath as u16 => format!(
            "<span class=\"inline-flex items-center px-1.5 py-0.5 rounded bg-cyan-500/15 text-cyan-300 font-mono text-[11px] border border-cyan-500/30\">{escaped}</span>"
        ),
        k if k == SpanKind::WikiLink as u16 => {
            let label = span_string(parsed, span).unwrap_or_else(|| content.to_string());
            format!(
                "<span class=\"inline-flex items-center gap-1 px-1.5 py-0.5 rounded bg-purple-500/15 text-purple-300 font-mono text-[11px] border border-purple-500/30 font-medium\"><span>[[</span><span class=\"text-textPrimary font-normal\">{esc}</span><span>]]</span></span>",
                esc = escape_html(&label)
            )
        }
        k if k == SpanKind::Link as u16 => {
            let href = span_string(parsed, span).unwrap_or_default();
            match safe_url(&href) {
                Some(url) => format!(
                    "<a href=\"{url}\" class=\"text-accent underline underline-offset-2\">{escaped}</a>",
                    url = escape_html(&url)
                ),
                None => escaped,
            }
        }
        k if k == SpanKind::Image as u16 => {
            let src = span_string(parsed, span).unwrap_or_default();
            match safe_url(&src) {
                Some(url) => format!(
                    "<img src=\"{url}\" alt=\"{escaped}\" class=\"max-w-full rounded-lg my-2\" />",
                    url = escape_html(&url)
                ),
                None => escaped,
            }
        }
        k if k == SpanKind::Tag as u16 => format!(
            "<span class=\"text-accent font-mono text-[11px]\">{escaped}</span>"
        ),
        k if k == SpanKind::InlineHtml as u16 => escaped, // never raw
        k if k == SpanKind::TaskMarker as u16 => String::new(), // drawn by list item
        _ => escaped,
    }
}

fn callout_style(data: u32) -> (&'static str, &'static str) {
    match data {
        d if d == CalloutKind::Tip as u32 => ("Tip", "border-emerald-500/50"),
        d if d == CalloutKind::Important as u32 => ("Important", "border-violet-500/50"),
        d if d == CalloutKind::Warning as u32 => ("Warning", "border-amber-500/50"),
        d if d == CalloutKind::Caution as u32 => ("Caution", "border-rose-500/50"),
        _ => ("Note", "border-sky-500/50"),
    }
}

fn render_frontmatter(body: &str) -> String {
    let mut rows = String::new();
    for line in body.lines() {
        let line = line.trim();
        if line.is_empty() || line == "---" || line == "+++" {
            continue;
        }
        if let Some((key, value)) = line.split_once(':') {
            rows.push_str(&format!(
                "<span class=\"inline-flex items-center gap-1 px-2 py-0.5 rounded-full bg-surface border border-border/70 text-[11px] font-mono\"><strong class=\"text-accent font-semibold\">{k}:</strong> <span class=\"text-textPrimary\">{v}</span></span> ",
                k = escape_html(key.trim()),
                v = escape_html(value.trim())
            ));
        }
    }
    format!(
        "<div class=\"mb-6 p-3 rounded-xl border border-border/70 bg-surface/50 shadow-xs select-none\">\
         <div class=\"text-[10px] font-mono uppercase tracking-wider text-textMuted mb-2 flex items-center gap-1.5 font-bold\">\
         <span class=\"w-1.5 h-1.5 rounded-full bg-accent\"></span><span>Metadata / Frontmatter</span></div>\
         <div class=\"flex flex-wrap gap-2\">{rows}</div></div>"
    )
}

fn slice_utf16(units: &[u16], start: u32, end: u32) -> String {
    let start = start as usize;
    let end = (end as usize).min(units.len());
    if start >= end || start >= units.len() {
        return String::new();
    }
    String::from_utf16_lossy(&units[start..end])
}

fn escape_html(text: impl AsRef<str>) -> String {
    let text = text.as_ref();
    let mut out = String::with_capacity(text.len());
    for ch in text.chars() {
        match ch {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&#39;"),
            _ => out.push(ch),
        }
    }
    out
}

/// Active schemes that must never reach an href/src attribute.
pub fn safe_url(raw: &str) -> Option<String> {
    let url = raw.trim();
    if url.is_empty() {
        return None;
    }
    let bare: String = url
        .chars()
        .filter(|c| !matches!(c, '\u{0000}'..='\u{001f}' | '\u{007f}'))
        .collect();
    if let Some(rest) = bare.split_once(':') {
        let scheme = rest.0;
        if scheme
            .chars()
            .next()
            .is_some_and(|c| c.is_ascii_alphabetic())
            && scheme
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || matches!(c, '+' | '.' | '-'))
        {
            if matches!(
                scheme.to_ascii_lowercase().as_str(),
                "http" | "https" | "mailto" | "tel"
            ) {
                return Some(bare);
            }
            return None;
        }
    }
    Some(bare)
}

fn slugify(text: &str) -> String {
    let mut out = String::new();
    let mut prev_dash = false;
    for ch in text.chars() {
        if ch.is_ascii_alphanumeric() {
            out.push(ch.to_ascii_lowercase());
            prev_dash = false;
        } else if !prev_dash && !out.is_empty() {
            out.push('-');
            prev_dash = true;
        }
    }
    while out.ends_with('-') {
        out.pop();
    }
    if out.is_empty() {
        "section".into()
    } else {
        out
    }
}

fn strip_tags_approx(html: &str) -> String {
    let mut out = String::new();
    let mut in_tag = false;
    for ch in html.chars() {
        match ch {
            '<' => in_tag = true,
            '>' => in_tag = false,
            _ if !in_tag => out.push(ch),
            _ => {}
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn headings_get_anchors() {
        let html = render("# Hello World\n## Next Level").unwrap();
        assert!(html.contains("id=\"hello-world\""), "{html}");
        assert!(html.contains("Hello World"), "{html}");
        assert!(html.contains("id=\"next-level\""), "{html}");
    }

    #[test]
    fn callouts_render_labels() {
        let note = render("> [!NOTE]\n> This is an important note with details.").unwrap();
        assert!(note.contains("Note"), "{note}");
        assert!(note.contains("border-sky-500/50"), "{note}");
        let warn = render("> [!WARNING]\n> This is a warning.").unwrap();
        assert!(warn.contains("Warning"), "{warn}");
        assert!(warn.contains("border-amber-500/50"), "{warn}");
    }

    #[test]
    fn refuses_javascript_urls() {
        for hostile in [
            "[c](javascript:alert(1))",
            "[c](JaVaScRiPt:alert(1))",
            "![i](javascript:alert(1))",
        ] {
            let html = render(hostile).unwrap();
            assert!(
                !html.to_lowercase().contains("href=\"javascript:")
                    && !html.to_lowercase().contains("src=\"javascript:"),
                "{hostile} -> {html}"
            );
        }
        let kept = render("[click me](javascript:alert(1))").unwrap();
        assert!(kept.contains("click me"), "{kept}");
    }

    #[test]
    fn keeps_safe_schemes() {
        let html = render("[c](https://example.com)").unwrap();
        assert!(html.contains("https://example.com"), "{html}");
        let rel = render("[c](./relative.md)").unwrap();
        assert!(rel.contains("./relative.md"), "{rel}");
    }

    #[test]
    fn escapes_raw_html() {
        let html = render("<script>alert(\"xss\")</script>").unwrap();
        assert!(!html.contains("<script>"), "{html}");
        assert!(html.contains("&lt;script&gt;"), "{html}");
    }

    #[test]
    fn caps_oversized_input_and_says_so() {
        let big = "# H\n\n".repeat(MAX_RENDER_BYTES);
        let html = render(&big).unwrap();
        assert!(html.contains("not shown"), "{html}");
        assert!(html.contains("Rendered the first"), "{html}");
    }

    #[test]
    fn parse_resolves_string_table() {
        let parsed = parse("[hi](https://example.com)").unwrap();
        assert!(
            parsed
                .spans
                .iter()
                .any(|s| s.kind == "link" && s.text.as_deref() == Some("https://example.com")),
            "{parsed:?}"
        );
    }

    #[test]
    fn code_fence_and_mermaid() {
        let code = render("```rust\nfn main() {}\n```").unwrap();
        assert!(code.contains("RUST") || code.contains("rust"), "{code}");
        assert!(code.contains("copy-code-btn"), "{code}");
        let mermaid = render("```mermaid\ngraph TD;\nA-->B;\n```").unwrap();
        assert!(mermaid.contains("MERMAID DIAGRAM"), "{mermaid}");
    }

    #[test]
    fn safe_url_strips_control_chars_in_scheme() {
        assert!(safe_url("java\tscript:alert(1)").is_none());
        assert!(safe_url("https://ok").is_some());
    }
}
