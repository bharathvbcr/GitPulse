//! Markdown source → the flat [`ParseResult`] the editor renders from.
//!
//! # How syntax markers are found
//!
//! Live preview hides the literal syntax (`**`, `` ` ``, `# `, `](url)`) when
//! the caret is elsewhere. Rather than pattern-match each construct, markers
//! are derived structurally:
//!
//! > A marker is any part of a construct's source range that none of its
//! > children cover.
//!
//! `pulldown-cmark`'s [`OffsetIter`] gives a source range for every event, so
//! for `**bold**` the `Strong` range is `0..8` and its `Text` child is `2..6`,
//! leaving `0..2` and `6..8` — exactly the asterisks. The same single rule
//! yields `# ` on headings, the fence lines on code blocks, `[[`/`]]` on
//! wikilinks, and `](url)` on inline links, with no per-construct code.
//!
//! Two things this rule cannot see, handled separately below:
//! - Delimiters of *leaf* events (`` `code` ``, `$math$`) — these have no
//!   child events, so their delimiter width is computed directly.
//! - Blockquote `>` continuation markers on lines after the first, which sit
//!   *inside* the child paragraph's range rather than outside it.
//!
//! [`OffsetIter`]: pulldown_cmark::OffsetIter

use pulldown_cmark::{
    Alignment, BlockQuoteKind, CodeBlockKind, Event, LinkType, MetadataBlockKind, Options, Parser,
    Tag, TagEnd,
};
use std::collections::HashMap;
use std::ops::{Deref, DerefMut, Range};

use super::model::{
    BlockDescriptor, BlockKind, CalloutKind, ParseResult, SpanKind, StyleSpan, SyntaxMarker,
    TableAlignment, Utf16Mapper, CALLOUT_FOLD_SHIFT, MAX_DOCUMENT_BYTES, MAX_INTERNED_STRINGS,
    MAX_INTERNED_STRING_BYTES, MAX_PARSE_EVENTS, MAX_PARSE_NESTING, MAX_STRUCTURAL_RECORDS,
    MAX_TOTAL_STRING_BYTES, NO_INFO, TABLE_ALIGNMENT_BITS, TASK_STATUS_SHIFT,
};
use super::obsidian;

/// Why a Markdown input was refused before a partial model could escape.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ParseError {
    SourceTooLarge,
    TooManyEvents,
    TooDeep,
    TooManyRecords,
    TooManyStrings,
    StringTooLong,
    TooManyStringBytes,
    InteriorNul,
}

/// Private bounded owner used while a parse is under construction.
///
/// Every output allocation passes through this type. Once any limit would be
/// crossed the parse returns `Err`; callers never receive a prefix that could
/// be mistaken for a complete document model.
struct ParseAccumulator {
    result: ParseResult,
    string_index: HashMap<String, u32>,
    string_bytes: usize,
    structural_records: usize,
}

impl ParseAccumulator {
    fn new() -> Self {
        Self {
            result: ParseResult::default(),
            string_index: HashMap::new(),
            string_bytes: 0,
            structural_records: 0,
        }
    }

    fn charge_record(&mut self) -> Result<(), ParseError> {
        if self.structural_records >= MAX_STRUCTURAL_RECORDS {
            return Err(ParseError::TooManyRecords);
        }
        self.structural_records += 1;
        Ok(())
    }

    fn push_span(&mut self, span: StyleSpan) -> Result<(), ParseError> {
        self.charge_record()?;
        self.result.spans.push(span);
        Ok(())
    }

    fn push_marker(&mut self, marker: SyntaxMarker) -> Result<(), ParseError> {
        self.charge_record()?;
        self.result.markers.push(marker);
        Ok(())
    }

    fn push_block(&mut self, block: BlockDescriptor) -> Result<u32, ParseError> {
        self.charge_record()?;
        let index =
            u32::try_from(self.result.blocks.len()).map_err(|_| ParseError::TooManyRecords)?;
        self.result.blocks.push(block);
        Ok(index)
    }

    fn push_top_level(&mut self, range: Range<usize>) -> Result<(), ParseError> {
        // Every top-level range owns a block, but retain an independent guard
        // so a future parser change cannot turn this auxiliary index into an
        // unbounded allocation.
        if self.result.top_level.len() >= MAX_STRUCTURAL_RECORDS {
            return Err(ParseError::TooManyRecords);
        }
        self.result.top_level.push(range);
        Ok(())
    }

    fn intern(&mut self, value: &str) -> Result<u32, ParseError> {
        if let Some(&index) = self.string_index.get(value) {
            return Ok(index);
        }
        if value.len() > MAX_INTERNED_STRING_BYTES {
            return Err(ParseError::StringTooLong);
        }
        if self.result.strings.len() >= MAX_INTERNED_STRINGS {
            return Err(ParseError::TooManyStrings);
        }
        let next_bytes = self
            .string_bytes
            .checked_add(value.len())
            .ok_or(ParseError::TooManyStringBytes)?;
        if next_bytes > MAX_TOTAL_STRING_BYTES {
            return Err(ParseError::TooManyStringBytes);
        }
        let index =
            u32::try_from(self.result.strings.len()).map_err(|_| ParseError::TooManyStrings)?;
        let owned = value.to_owned();
        self.result.strings.push(owned.clone());
        self.string_index.insert(owned, index);
        self.string_bytes = next_bytes;
        Ok(index)
    }

    fn finish(self) -> ParseResult {
        self.result
    }
}

impl Deref for ParseAccumulator {
    type Target = ParseResult;

    fn deref(&self) -> &Self::Target {
        &self.result
    }
}

impl DerefMut for ParseAccumulator {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.result
    }
}

/// Parser options MarkDev renders with.
///
/// Deliberately excludes `ENABLE_SUBSCRIPT`/`ENABLE_SUPERSCRIPT`: subscript
/// re-reads `~x~` as subscript rather than strikethrough, which would quietly
/// change how existing notes render. Also excludes `ENABLE_SMART_PUNCTUATION`,
/// which rewrites text content and would desynchronise offsets from the
/// characters actually in the buffer.
pub fn options() -> Options {
    Options::ENABLE_TABLES
        | Options::ENABLE_FOOTNOTES
        | Options::ENABLE_STRIKETHROUGH
        | Options::ENABLE_TASKLISTS
        | Options::ENABLE_MATH
        | Options::ENABLE_GFM
        | Options::ENABLE_WIKILINKS
        | Options::ENABLE_DEFINITION_LIST
        | Options::ENABLE_YAML_STYLE_METADATA_BLOCKS
        | Options::ENABLE_PLUSES_DELIMITED_METADATA_BLOCKS
}

/// Column alignment for the table currently being walked.
///
/// `pulldown-cmark` reports alignment once, on the table's `Start` tag, but a
/// renderer needs it per *cell* — the cell is what gets padded left, centred,
/// or right. Rather than make every consumer re-find the owning table and
/// count columns, the alignments are stashed here and stamped onto each cell
/// as it opens.
///
/// A stack, not a single value: a table can sit inside a blockquote inside a
/// list, and while GFM tables cannot nest directly, a malformed document can
/// still open a second table before the first closes. Popping the wrong
/// alignment vector would silently right-align an unrelated column.
#[derive(Default)]
struct TableState {
    /// Alignment per column, innermost table last.
    tables: Vec<Vec<TableAlignment>>,
    /// Which column the next cell in the current row is.
    column: usize,
}

impl TableState {
    fn open(&mut self, alignments: &[Alignment]) -> Result<(), ParseError> {
        if alignments.len() > MAX_STRUCTURAL_RECORDS {
            return Err(ParseError::TooManyRecords);
        }
        self.tables
            .push(alignments.iter().copied().map(alignment).collect());
        self.column = 0;
        Ok(())
    }

    fn close(&mut self) {
        self.tables.pop();
        self.column = 0;
    }

    /// Starts a new row. Header and body rows both restart at column zero.
    fn new_row(&mut self) {
        self.column = 0;
    }

    /// Alignment for the next cell, consuming one column.
    ///
    /// A row with more cells than the delimiter row declared falls back to
    /// `Auto` rather than panicking: GFM tolerates ragged rows, and a parse
    /// must never be the thing that crashes an edit.
    fn next_cell(&mut self) -> TableAlignment {
        let alignment = self
            .tables
            .last()
            .and_then(|columns| columns.get(self.column))
            .copied()
            .unwrap_or(TableAlignment::Auto);
        self.column += 1;
        alignment
    }
}

fn alignment(a: Alignment) -> TableAlignment {
    match a {
        Alignment::None => TableAlignment::Auto,
        Alignment::Left => TableAlignment::Left,
        Alignment::Center => TableAlignment::Center,
        Alignment::Right => TableAlignment::Right,
    }
}

/// One open construct while walking the event stream.
struct Frame {
    range: Range<usize>,
    /// Byte ranges covered by children, in document order.
    covered: Vec<Range<usize>>,
    /// Index into `result.blocks` when this frame is a block, else `None`.
    block: Option<usize>,
    /// Span to emit on close, if this frame is an inline construct.
    span: Option<(SpanKind, u32)>,
    /// True for blockquote frames, which need the continuation-marker pass.
    is_block_quote: bool,
    /// Indented code: the four-space (or tab) prefix is syntax, like a fence.
    indented_code: bool,
    /// Extra syntax on a GFM alert's first line (`[!NOTE]`, a custom title).
    extra_markers: Vec<Range<usize>>,
}

/// Parses `source` into the flat model the editor renders from.
///
/// The result is atomic: every source, event, nesting, record, and string cap
/// is enforced while building, and no partial [`ParseResult`] is returned.
pub fn parse_checked(source: &str) -> Result<ParseResult, ParseError> {
    if source.len() > MAX_DOCUMENT_BYTES {
        return Err(ParseError::SourceTooLarge);
    }
    // C ABI v3 is length-delimited, but pulldown-cmark deliberately treats a
    // NUL as invalid Markdown preprocessing and may stop recognising the
    // construct that contains it. Rejecting the whole document is explicit
    // and lossless; accepting a model with a silently missing destination or
    // info string is not.
    if source.as_bytes().contains(&0) {
        return Err(ParseError::InteriorNul);
    }
    let mapper = Utf16Mapper::new(source);
    let mut result = ParseAccumulator::new();
    let mut stack: Vec<Frame> = Vec::new();
    // Innermost open block, so markers can be attributed to their block.
    let mut block_stack: Vec<usize> = Vec::new();
    let mut inline_depth: u16 = 0;
    // Depth of enclosing verbatim blocks (code, frontmatter, raw HTML). Their
    // contents arrive as `Text` events but must not be scanned for `#tag` or
    // `==highlight==` — a `#` inside a code fence is code, not a tag.
    let mut verbatim: usize = 0;
    let mut tables = TableState::default();
    let mut event_count = 0usize;

    for (event, range) in Parser::new_ext(source, options()).into_offset_iter() {
        charge_event(&mut event_count)?;
        match event {
            Event::Start(tag) => {
                if stack.len() >= MAX_PARSE_NESTING {
                    return Err(ParseError::TooDeep);
                }
                if is_verbatim_tag(&tag) {
                    verbatim += 1;
                }
                let frame = open_frame(
                    &tag,
                    range.clone(),
                    source,
                    &mut result,
                    &mut block_stack,
                    inline_depth,
                    &mut tables,
                )?;
                if frame.span.is_some() {
                    inline_depth += 1;
                }
                stack.push(frame);
                // A footnote definition's `[^label]:` prefix is a gap the
                // child paragraph does not cover. Covering the label leaves
                // only the brackets and colon as markers, so the superscript
                // has something to sit on — hiding the whole prefix was a
                // hole where the mark should be.
                if let Tag::FootnoteDefinition(label) = &tag {
                    if let Some(label_range) = footnote_label_in_definition(source, &range, label) {
                        cover(&mut stack, label_range.clone());
                        let dest = result.intern(label)?;
                        push_span(
                            &mut result,
                            &mapper,
                            &label_range,
                            SpanKind::FootnoteReference,
                            inline_depth,
                            dest,
                        )?;
                    }
                }
            }

            Event::End(end) => {
                if is_verbatim_end(&end) {
                    verbatim = verbatim.saturating_sub(1);
                }
                if end == TagEnd::Table {
                    tables.close();
                }
                let Some(frame) = stack.pop() else { continue };
                if frame.span.is_some() {
                    inline_depth = inline_depth.saturating_sub(1);
                }
                close_frame(frame, &end, source, &mapper, &mut result, &mut block_stack)?;
                // The closed construct is covered ground for its parent.
                cover(&mut stack, range);
            }

            // Leaf events whose range includes delimiters we must hide.
            Event::Code(_) => {
                emit_delimited_leaf(
                    &range,
                    source,
                    SpanKind::InlineCode,
                    delimiter_run(source, &range, b'`'),
                    &mapper,
                    &mut result,
                    &block_stack,
                    inline_depth,
                )?;
                cover(&mut stack, range);
            }
            Event::InlineMath(_) => {
                if inline_math_is_valid(source, &range) {
                    emit_delimited_leaf(
                        &range,
                        source,
                        SpanKind::InlineMath,
                        1,
                        &mapper,
                        &mut result,
                        &block_stack,
                        inline_depth,
                    )?;
                }
                cover(&mut stack, range);
            }
            Event::DisplayMath(_) => {
                if display_math_is_valid(source, &range) {
                    let block = push_block(
                        &mut result,
                        &mapper,
                        &range,
                        BlockKind::MathBlock,
                        block_stack.len() as u16,
                        0,
                        NO_INFO,
                    )?;
                    if block_stack.is_empty() {
                        result.push_top_level(range.clone())?;
                    }
                    // `$$` on both sides is syntax, the formula between is content.
                    mark(&mut result, &mapper, range.start..range.start + 2, block)?;
                    if range.end >= range.start + 2 {
                        mark(&mut result, &mapper, range.end - 2..range.end, block)?;
                    }
                }
                cover(&mut stack, range);
            }
            Event::FootnoteReference(label) => {
                // `[^label]` is brackets around a label. Hiding the whole run
                // leaves a hole; hiding `[^` and `]` leaves the label for the
                // superscript the styler draws.
                let dest = result.intern(&label)?;
                let bytes = source.as_bytes();
                if range.len() >= 3
                    && bytes.get(range.start) == Some(&b'[')
                    && bytes.get(range.start + 1) == Some(&b'^')
                    && bytes.get(range.end - 1) == Some(&b']')
                {
                    let inner = range.start + 2..range.end - 1;
                    push_span(
                        &mut result,
                        &mapper,
                        &inner,
                        SpanKind::FootnoteReference,
                        inline_depth,
                        dest,
                    )?;
                    mark_current(
                        &mut result,
                        &mapper,
                        range.start..range.start + 2,
                        &block_stack,
                    )?;
                    mark_current(&mut result, &mapper, range.end - 1..range.end, &block_stack)?;
                } else {
                    push_span(
                        &mut result,
                        &mapper,
                        &range,
                        SpanKind::FootnoteReference,
                        inline_depth,
                        dest,
                    )?;
                }
                cover(&mut stack, range);
            }
            Event::TaskListMarker(checked) => {
                push_span(
                    &mut result,
                    &mapper,
                    &range,
                    SpanKind::TaskMarker,
                    inline_depth,
                    u32::from(checked),
                )?;
                // The literal `[ ]` is replaced by a drawn checkbox.
                mark_current(&mut result, &mapper, range.clone(), &block_stack)?;
                cover(&mut stack, range);
            }
            Event::Rule => {
                let block = push_block(
                    &mut result,
                    &mapper,
                    &range,
                    BlockKind::Rule,
                    block_stack.len() as u16,
                    0,
                    NO_INFO,
                )?;
                if block_stack.is_empty() {
                    result.push_top_level(range.clone())?;
                }
                // The `---` is replaced by a drawn line.
                mark(&mut result, &mapper, range.clone(), block)?;
                cover(&mut stack, range);
            }
            Event::InlineHtml(_) => {
                push_span(
                    &mut result,
                    &mapper,
                    &range,
                    SpanKind::InlineHtml,
                    inline_depth,
                    0,
                )?;
                cover(&mut stack, range);
            }

            // Plain content: covered, never a marker.
            Event::Text(_) => {
                if verbatim == 0 {
                    scan_text_extensions(source, &range, &mapper, &mut result, &block_stack)?;
                }
                cover(&mut stack, range);
            }
            Event::Html(_) | Event::SoftBreak => {
                cover(&mut stack, range);
            }
            Event::HardBreak => {
                // Two trailing spaces, or a backslash, are the hard-break
                // marker. The newline is the break itself and must stay, or
                // the two lines join.
                let bytes = source.as_bytes();
                let mut end = range.end;
                while end > range.start && (bytes[end - 1] == b'\n' || bytes[end - 1] == b'\r') {
                    end -= 1;
                }
                if end > range.start {
                    mark_current(&mut result, &mapper, range.start..end, &block_stack)?;
                }
                cover(&mut stack, range);
            }
        }
    }

    collect_delimited_math(source, &mapper, &mut result)?;
    collect_link_reference_definitions(source, &mapper, &mut result)?;
    collect_obsidian_extensions(source, &mapper, &mut result)?;
    collect_cross_run_highlights(source, &mapper, &mut result)?;

    result.spans.sort_by_key(|s| (s.start, s.end));
    result.markers.sort_by_key(|m| (m.start, m.end));
    result.top_level.sort_by_key(|r| (r.start, r.end));
    Ok(result.finish())
}

fn charge_event(event_count: &mut usize) -> Result<(), ParseError> {
    *event_count = event_count
        .checked_add(1)
        .ok_or(ParseError::TooManyEvents)?;
    if *event_count > MAX_PARSE_EVENTS {
        return Err(ParseError::TooManyEvents);
    }
    Ok(())
}

/// Opens a frame for a `Start` tag, reserving a block slot where applicable.
fn open_frame(
    tag: &Tag,
    range: Range<usize>,
    source: &str,
    result: &mut ParseAccumulator,
    block_stack: &mut Vec<usize>,
    inline_depth: u16,
    tables: &mut TableState,
) -> Result<Frame, ParseError> {
    let depth = block_stack.len() as u16;
    let mut frame = Frame {
        range: range.clone(),
        covered: Vec::new(),
        block: None,
        span: None,
        is_block_quote: false,
        indented_code: false,
        extra_markers: Vec::new(),
    };

    // Block slots are reserved on open so markers can reference them, and
    // filled in on close once the full range is known.
    let reserve = |result: &mut ParseAccumulator,
                   kind: BlockKind,
                   data: u32,
                   info: u32|
     -> Result<usize, ParseError> {
        let index = result.push_block(BlockDescriptor {
            start: 0,
            end: 0,
            kind: kind as u16,
            depth,
            data,
            info,
        })?;
        Ok(index as usize)
    };

    match tag {
        Tag::Paragraph => frame.block = Some(reserve(result, BlockKind::Paragraph, 0, NO_INFO)?),
        Tag::Heading { level, .. } => {
            let lvl = *level as u32;
            frame.block = Some(reserve(result, BlockKind::Heading, lvl, NO_INFO)?);
            frame.span = Some((SpanKind::Heading, lvl));
        }
        Tag::BlockQuote(kind) => {
            frame.is_block_quote = true;
            let alert = gfm_alert_line(source, &range);
            match (kind, alert) {
                (Some(k), alert) => {
                    let idx =
                        reserve(result, BlockKind::Callout, callout_kind(*k) as u32, NO_INFO)?;
                    if let Some(alert) = alert {
                        apply_alert_title(result, &mut frame, idx, alert)?;
                    }
                    frame.block = Some(idx);
                }
                (None, Some(alert)) => {
                    // pulldown only names a flavour when `[!NOTE]` is the whole
                    // line. `> [!NOTE] Custom` is a BlockQuote to it; we still
                    // owe the reader a callout whose strip can show the title.
                    let idx = reserve(result, BlockKind::Callout, alert.kind as u32, NO_INFO)?;
                    frame.extra_markers.push(alert.tag.clone());
                    apply_alert_title(result, &mut frame, idx, alert)?;
                    frame.block = Some(idx);
                }
                (None, None) => {
                    frame.block = Some(reserve(result, BlockKind::BlockQuote, 0, NO_INFO)?);
                }
            }
        }
        Tag::CodeBlock(kind) => {
            let (block_kind, info) = match kind {
                CodeBlockKind::Fenced(lang) => {
                    let lang = lang.split_whitespace().next().unwrap_or("");
                    let info = if lang.is_empty() {
                        NO_INFO
                    } else {
                        result.intern(lang)?
                    };
                    // Routed by kind so the editor never string-compares.
                    if lang.eq_ignore_ascii_case("mermaid") {
                        (BlockKind::MermaidBlock, info)
                    } else if lang.eq_ignore_ascii_case("math") {
                        (BlockKind::MathBlock, info)
                    } else {
                        (BlockKind::CodeBlock, info)
                    }
                }
                CodeBlockKind::Indented => {
                    frame.indented_code = true;
                    (BlockKind::CodeBlock, NO_INFO)
                }
            };
            frame.block = Some(reserve(result, block_kind, 0, info)?);
        }
        Tag::List(first) => {
            frame.block = Some(reserve(
                result,
                BlockKind::List,
                u32::from(first.is_some()),
                NO_INFO,
            )?);
        }
        Tag::Item => frame.block = Some(reserve(result, BlockKind::ListItem, 0, NO_INFO)?),
        Tag::Table(alignments) => {
            tables.open(alignments)?;
            // The column count rides on the table so a renderer can size the
            // grid without walking every row first.
            let columns = alignments.len() as u32;
            frame.block = Some(reserve(result, BlockKind::Table, columns, NO_INFO)?);
        }
        Tag::TableHead => {
            tables.new_row();
            frame.block = Some(reserve(result, BlockKind::TableHead, 0, NO_INFO)?);
        }
        Tag::TableRow => {
            tables.new_row();
            frame.block = Some(reserve(result, BlockKind::TableRow, 0, NO_INFO)?);
        }
        Tag::TableCell => {
            // The cell's column index and alignment, packed so one `data`
            // field answers both "which column am I" and "how do I sit in it".
            let column = tables.column as u32;
            let alignment = tables.next_cell() as u32;
            frame.block = Some(reserve(
                result,
                BlockKind::TableCell,
                (column << TABLE_ALIGNMENT_BITS) | alignment,
                NO_INFO,
            )?);
        }
        Tag::HtmlBlock => frame.block = Some(reserve(result, BlockKind::HtmlBlock, 0, NO_INFO)?),
        Tag::FootnoteDefinition(label) => {
            let info = result.intern(label)?;
            frame.block = Some(reserve(result, BlockKind::FootnoteDefinition, 0, info)?)
        }
        Tag::DefinitionList => {
            frame.block = Some(reserve(result, BlockKind::DefinitionList, 0, NO_INFO)?)
        }
        Tag::DefinitionListTitle => {
            frame.block = Some(reserve(result, BlockKind::DefinitionListTitle, 0, NO_INFO)?)
        }
        Tag::DefinitionListDefinition => {
            frame.block = Some(reserve(
                result,
                BlockKind::DefinitionListDefinition,
                0,
                NO_INFO,
            )?)
        }
        Tag::MetadataBlock(kind) => {
            let data = match kind {
                MetadataBlockKind::YamlStyle => 0,
                MetadataBlockKind::PlusesStyle => 1,
            };
            frame.block = Some(reserve(result, BlockKind::Frontmatter, data, NO_INFO)?);
        }

        // Inline constructs carry a span rather than a block.
        Tag::Emphasis => frame.span = Some((SpanKind::Emphasis, 0)),
        Tag::Strong => frame.span = Some((SpanKind::Strong, 0)),
        Tag::Strikethrough => frame.span = Some((SpanKind::Strikethrough, 0)),
        Tag::Superscript => frame.span = Some((SpanKind::Superscript, 0)),
        Tag::Subscript => frame.span = Some((SpanKind::Subscript, 0)),
        Tag::Link {
            link_type,
            dest_url,
            ..
        } => {
            let dest = result.intern(dest_url)?;
            let kind = if matches!(link_type, LinkType::WikiLink { .. }) {
                SpanKind::WikiLink
            } else {
                SpanKind::Link
            };
            frame.span = Some((kind, dest));
        }
        Tag::Image {
            link_type,
            dest_url,
            ..
        } => {
            let dest = result.intern(dest_url)?;
            // `![[Note]]` transcludes a note in Obsidian. It is a link to that
            // note, not a picture: drawn as one it is a broken image, and the
            // vault would miss the connection it makes.
            let kind = if matches!(link_type, LinkType::WikiLink { .. })
                && !obsidian::is_media_target(dest_url)
            {
                SpanKind::WikiLink
            } else if obsidian::is_audio_target(dest_url) || obsidian::is_video_target(dest_url) {
                // The editor draws pictures, not players: an audio or video
                // embed is a link that opens the file in the system player.
                SpanKind::Link
            } else {
                SpanKind::Image
            };
            frame.span = Some((kind, dest));
        }
    }

    let _ = (source, inline_depth);
    if let Some(idx) = frame.block {
        block_stack.push(idx);
    }
    Ok(frame)
}

/// Closes a frame: emits its span, finalises its block, and derives markers
/// from the gaps its children left uncovered.
fn close_frame(
    frame: Frame,
    end: &TagEnd,
    source: &str,
    mapper: &Utf16Mapper,
    result: &mut ParseAccumulator,
    block_stack: &mut Vec<usize>,
) -> Result<(), ParseError> {
    let mut range = frame.range.clone();
    if frame.indented_code {
        range.start = indented_code_line_start(source, range.start);
    }

    if let Some(idx) = frame.block {
        result.blocks[idx].start = mapper.to_utf16(range.start);
        result.blocks[idx].end = mapper.to_utf16(range.end);
        block_stack.pop();
        // Emptying the stack means this block was top-level: a boundary the
        // incremental parser can safely cut on.
        if block_stack.is_empty() {
            result.push_top_level(range.clone())?;
        }
    }

    // Attribute markers to the innermost enclosing block: this frame if it is
    // one, otherwise whatever block still encloses it.
    let owner = frame
        .block
        .or_else(|| block_stack.last().copied())
        .unwrap_or(0) as u32;

    if let Some((kind, data)) = frame.span {
        // The span covers only the content, not the surrounding delimiters,
        // so styling never bleeds onto hidden syntax.
        let (content_start, content_end) = content_bounds(&frame.covered, &range);
        result.push_span(StyleSpan {
            start: mapper.to_utf16(content_start),
            end: mapper.to_utf16(content_end),
            kind: kind as u16,
            depth: 0,
            data,
        })?;
    }

    let mut cursor = range.start;
    for child in &frame.covered {
        if child.start > cursor {
            mark(result, mapper, cursor..child.start, owner)?;
        }
        cursor = cursor.max(child.end);
    }
    if cursor < range.end {
        mark(result, mapper, cursor..range.end, owner)?;
    }

    // A blockquote's `>` on continuation lines sits inside the child
    // paragraph's range, so the gap rule cannot see it.
    if frame.is_block_quote {
        mark_quote_prefixes(source, &range, mapper, result, owner)?;
    }
    if frame.indented_code {
        mark_indented_code_prefixes(source, &range, mapper, result, owner)?;
    }
    for extra in frame.extra_markers {
        mark(result, mapper, extra, owner)?;
    }

    let _ = end;
    Ok(())
}

/// Byte range spanned by a frame's children, falling back to the frame itself.
fn content_bounds(covered: &[Range<usize>], range: &Range<usize>) -> (usize, usize) {
    match (covered.first(), covered.last()) {
        (Some(first), Some(last)) => (first.start, last.end),
        _ => (range.start, range.end),
    }
}

/// Records `range` as covered by the innermost open frame.
fn cover(stack: &mut [Frame], range: Range<usize>) {
    if let Some(frame) = stack.last_mut() {
        frame.covered.push(range);
    }
}

/// Width of the delimiter run of `byte` at the start of `range`.
fn delimiter_run(source: &str, range: &Range<usize>, byte: u8) -> usize {
    source.as_bytes()[range.start..range.end]
        .iter()
        .take_while(|&&b| b == byte)
        .count()
}
/// Whether an inline `$…$` event should render as math rather than as the
/// literal text the reader typed.
///
/// `pulldown-cmark` pairs two `$`s whenever the first has a non-space after
/// it and the second a non-space before it — pandoc's rule, written for
/// documents that are mostly mathematics. In notes that are mostly prose it
/// eats currency: `$50-$100` pairs on the hyphen, hiding both dollars and
/// styling `50-` as math. Three refusals, each named for the prose it
/// protects:
///
/// - **Letter before plus digit after is a currency mark.** `US$5`,
///   `A$10`, `price$5` — an ASCII alphanumeric welded to the left of the
///   opener with a digit on its right is money, not LaTeX. The check is
///   ASCII-only deliberately: Chinese and Japanese are written without
///   spaces, so `其中$x$是变量` is the *normal* spelling of glued math, and
///   a Unicode-letter rule would refuse every one of those.
/// - **Nothing follows the closer but a digit.** Pandoc's own currency
///   rule: `$50-$100` closes before the second `0`, `$5 and$6` before the
///   `6`. A letter suffix is a word, so `$n$th` survives.
/// - **The pair never crosses from a link label into its destination.** A `$`
///   pair in `![chart $5](pic$a.png)` reaches across `](` because pulldown's
///   math scanner runs before images resolve. `](` alone is not enough to
///   decide that: real scientific notation routinely contains `$v[i](t)$`,
///   whose `]` matches a `[` inside the pair. A separator the pair cannot
///   account for is what refuses.
///   The same refusal guards `$$…$$`; see [`crosses_link_destination`].
///
/// The two pulldown rules — non-space after the opener, non-space before
/// the closer — are re-checked rather than trusted, so this function stays
/// correct even if the upstream pairing ever loosens.
pub fn inline_math_is_valid(source: &str, range: &Range<usize>) -> bool {
    let bytes = source.as_bytes();
    // Structural sanity: `$` at both ends with something between, and both
    // ends on character boundaries so the adjacency reads below cannot slice
    // mid-character however odd the caller.
    if range.len() < 3
        || range.end > bytes.len()
        || !source.is_char_boundary(range.start)
        || !source.is_char_boundary(range.end)
    {
        return false;
    }
    let (opener, closer) = (range.start, range.end - 1);
    if bytes[opener] != b'$' || bytes[closer] != b'$' {
        return false;
    }
    if bytes[opener + 1].is_ascii_whitespace() || bytes[closer - 1].is_ascii_whitespace() {
        return false;
    }
    // Currency signature: letter welded to the left, digit straight after.
    // Both halves must hold — `the$x$axis` has a digit nowhere in sight.
    if opener > 0 && bytes[opener - 1].is_ascii_alphanumeric() && bytes[opener + 1].is_ascii_digit()
    {
        return false;
    }
    if source[range.end..]
        .chars()
        .next()
        .is_some_and(|c| c.is_ascii_digit())
    {
        return false;
    }
    if crosses_link_destination(bytes, opener + 1..closer) {
        return false;
    }
    true
}

/// Whether a `$`/`$$` pair spans from a Markdown label into its destination.
///
/// Pulldown-cmark pairs dollars before images and links resolve, so a pair
/// that reaches across `](pic` has eaten an image separator — drawing it as
/// math puts a formula where the reader's sentence was. A pair is refused
/// when it cannot account for its own `](`. A `]` whose matching `[` lies
/// inside the pair is subscript-then-call notation —
/// `$v_{\text{dend}}[i](t)$`, ordinary scientific spelling. A `]` that
/// matches nothing inside the pair is debris of a construct the pairing ran
/// through, even when the surrounding brackets happen to balance
/// (`![img [inner] $5](pic$a.png)`).
///
/// `content` excludes the delimiters themselves.
fn crosses_link_destination(bytes: &[u8], content: Range<usize>) -> bool {
    let mut depth = 0usize;
    for i in content {
        match bytes[i] {
            b'[' => depth += 1,
            b']' => {
                if bytes.get(i + 1) == Some(&b'(') && depth == 0 {
                    // The `]` matches nothing inside the pair: this
                    // separator closes a bracket opened before the math.
                    return true;
                }
                depth = depth.saturating_sub(1);
            }
            _ => {}
        }
    }
    false
}

/// The same question for `$$…$$`.
///
/// Display pairing ignores whitespace entirely — pulldown-cmark only asks
/// that both delimiters be doubled — so `He gave me $$5 and I gave him $$10`
/// becomes a formula block *inside the sentence*, which the editor replaces
/// with a typeset bitmap: a hole where the sentence was. The currency rules
/// refuse it exactly as [`inline_math_is_valid`] does, and so does the
/// label-to-destination crossing: `![chart $$5](pic$$a.png)` would otherwise
/// draw its formula over the mangled image's debris, the one gap the inline
/// check's `](` refusal covered and this function lacked. Block math on its
/// own lines ends at a newline or the end of the document and passes
/// untouched; so does inline display math between words (`text $$x$$ more`),
/// which notes in the wild rely on.
pub fn display_math_is_valid(source: &str, range: &Range<usize>) -> bool {
    let bytes = source.as_bytes();
    // Structural sanity: `$$` at both ends with something between, on
    // character boundaries, for the same reason the inline check insists.
    if range.len() < 5
        || range.end > bytes.len()
        || !source.is_char_boundary(range.start)
        || !source.is_char_boundary(range.end)
    {
        return false;
    }
    if &bytes[range.start..range.start + 2] != b"$$" || &bytes[range.end - 2..range.end] != b"$$" {
        return false;
    }
    // Currency signature, same as inline: "word$$5 …" is slang for money
    // (`gave him $$10`), not display math.
    if range.start > 0
        && bytes[range.start - 1].is_ascii_alphanumeric()
        && bytes[range.start + 2].is_ascii_digit()
    {
        return false;
    }
    if source[range.end..]
        .chars()
        .next()
        .is_some_and(|c| c.is_ascii_digit())
    {
        return false;
    }
    if crosses_link_destination(bytes, range.start + 2..range.end - 2) {
        return false;
    }
    true
}

/// Emits a span for a leaf construct and hides its delimiters.
#[allow(clippy::too_many_arguments)]
fn emit_delimited_leaf(
    range: &Range<usize>,
    source: &str,
    kind: SpanKind,
    delim: usize,
    mapper: &Utf16Mapper,
    result: &mut ParseAccumulator,
    block_stack: &[usize],
    inline_depth: u16,
) -> Result<(), ParseError> {
    let inner = range.start + delim..range.end.saturating_sub(delim);
    if inner.start <= inner.end {
        push_span(result, mapper, &inner, kind, inline_depth, 0)?;
    }
    if delim > 0 {
        mark_current(
            result,
            mapper,
            range.start..range.start + delim,
            block_stack,
        )?;
        mark_current(result, mapper, range.end - delim..range.end, block_stack)?;
    }
    let _ = source;
    Ok(())
}

fn push_span(
    result: &mut ParseAccumulator,
    mapper: &Utf16Mapper,
    range: &Range<usize>,
    kind: SpanKind,
    depth: u16,
    data: u32,
) -> Result<(), ParseError> {
    result.push_span(StyleSpan {
        start: mapper.to_utf16(range.start),
        end: mapper.to_utf16(range.end),
        kind: kind as u16,
        depth,
        data,
    })
}

fn push_block(
    result: &mut ParseAccumulator,
    mapper: &Utf16Mapper,
    range: &Range<usize>,
    kind: BlockKind,
    depth: u16,
    data: u32,
    info: u32,
) -> Result<u32, ParseError> {
    result.push_block(BlockDescriptor {
        start: mapper.to_utf16(range.start),
        end: mapper.to_utf16(range.end),
        kind: kind as u16,
        depth,
        data,
        info,
    })
}

fn mark(
    result: &mut ParseAccumulator,
    mapper: &Utf16Mapper,
    range: Range<usize>,
    block: u32,
) -> Result<(), ParseError> {
    if range.start >= range.end {
        return Ok(());
    }
    result.push_marker(SyntaxMarker {
        start: mapper.to_utf16(range.start),
        end: mapper.to_utf16(range.end),
        block,
    })
}

fn mark_current(
    result: &mut ParseAccumulator,
    mapper: &Utf16Mapper,
    range: Range<usize>,
    block_stack: &[usize],
) -> Result<(), ParseError> {
    let owner = block_stack.last().copied().unwrap_or(0) as u32;
    mark(result, mapper, range, owner)
}

/// Byte offset of the label inside a footnote definition's `[^label]:` prefix.
fn footnote_label_in_definition(
    source: &str,
    range: &Range<usize>,
    label: &str,
) -> Option<Range<usize>> {
    let text = source.get(range.clone())?;
    let bytes = text.as_bytes();
    let label_end = 2usize.checked_add(label.len())?;
    if bytes.get(0..2) == Some(b"[^")
        && bytes.get(2..label_end) == Some(label.as_bytes())
        && bytes.get(label_end) == Some(&b']')
    {
        Some(range.start + 2..range.start + label_end)
    } else {
        None
    }
}

/// Start of the line holding `content_start`, when the prefix is indent.
fn indented_code_line_start(source: &str, content_start: usize) -> usize {
    let bytes = source.as_bytes();
    let mut i = content_start.min(bytes.len());
    while i > 0 && bytes[i - 1] != b'\n' && bytes[i - 1] != b'\r' {
        i -= 1;
    }
    if bytes[i..content_start]
        .iter()
        .all(|&b| b == b' ' || b == b'\t')
    {
        i
    } else {
        content_start
    }
}

/// Hides the four-space (or tab) indent that opens each line of an indented
/// code block. Continuation lines are usually already a gap; the first line's
/// indent sits *before* pulldown's range and would otherwise stay visible.
fn mark_indented_code_prefixes(
    source: &str,
    range: &Range<usize>,
    mapper: &Utf16Mapper,
    result: &mut ParseAccumulator,
    block: u32,
) -> Result<(), ParseError> {
    let bytes = source.as_bytes();
    let mut i = range.start;
    let mut at_line_start = true;
    while i < range.end.min(bytes.len()) {
        if at_line_start {
            let mut j = i;
            let mut spaces = 0u8;
            while j < range.end && spaces < 4 {
                match bytes[j] {
                    b' ' => {
                        spaces += 1;
                        j += 1;
                    }
                    b'\t' => {
                        j += 1;
                        break;
                    }
                    _ => break,
                }
            }
            if j > i {
                mark(result, mapper, i..j, block)?;
                i = j;
                at_line_start = false;
                continue;
            }
        }
        at_line_start = bytes[i] == b'\n';
        i += 1;
    }
    Ok(())
}

struct GfmAlertLine<'a> {
    kind: CalloutKind,
    /// `CALLOUT_FOLD_*` from a trailing `+` / `-`, or 0.
    fold: u32,
    /// `[!NOTE]` (and an optional trailing `+` / `-`).
    tag: Range<usize>,
    title: Option<(&'a str, Range<usize>)>,
    /// Shown when the author wrote no title but used a custom type such as
    /// `[!recipe]`: the type name is the only thing that tells it apart
    /// from a plain note.
    custom_title: Option<String>,
}

fn apply_alert_title(
    result: &mut ParseAccumulator,
    frame: &mut Frame,
    idx: usize,
    alert: GfmAlertLine<'_>,
) -> Result<(), ParseError> {
    result.blocks[idx].data |= alert.fold << CALLOUT_FOLD_SHIFT;
    if let Some((title, title_range)) = alert.title {
        result.blocks[idx].info = result.intern(title)?;
        frame.extra_markers.push(title_range);
    } else if let Some(custom) = alert.custom_title {
        result.blocks[idx].info = result.intern(&custom)?;
    }
    Ok(())
}

/// First line of a blockquote that is a callout: a GitHub alert, including
/// the custom-title form pulldown-cmark leaves as a plain quote, or any
/// Obsidian callout type with its optional fold sign and title.
fn gfm_alert_line<'a>(source: &'a str, range: &Range<usize>) -> Option<GfmAlertLine<'a>> {
    let line = obsidian::callout_line(source, range)?;
    let custom_title = (line.title.is_none() && !CalloutKind::is_known_type_name(line.type_name))
        .then(|| obsidian::default_callout_title(line.type_name));
    Some(GfmAlertLine {
        kind: line.kind,
        fold: line.fold,
        tag: line.tag,
        title: line.title,
        custom_title,
    })
}

/// `\(...\)`, `\[...\]`, and their Markdown-escaped `\\(…\\)` / `\\[…\\]` forms.
///
/// pulldown-cmark's math scanner only pairs `$` / `$$`. These delimiters are
/// recognised against the source so a note written for KaTeX still typesets,
/// while code, existing math, and link labels stay literal. The `$` currency
/// and adjacency refusals are not reimplemented here — they already ran on
/// the dollar events; this pass only claims constructs that scanner never sees.
fn collect_delimited_math(
    source: &str,
    mapper: &Utf16Mapper,
    result: &mut ParseAccumulator,
) -> Result<(), ParseError> {
    let bytes = source.as_bytes();
    if !bytes.contains(&b'\\') {
        return Ok(());
    }
    let occupied = occupied_byte_ranges(mapper, result);
    let mut innermost = InnermostCursor::new(result);
    let mut i = 0;
    let mut added_block = false;
    while i < bytes.len() {
        if bytes[i] != b'\\' || range_is_occupied(&occupied, i) {
            i += 1;
            continue;
        }
        let preceded = i > 0 && bytes[i - 1] == b'\\';
        if preceded {
            i += 1;
            continue;
        }

        let taken = take_delimited_math(
            source,
            mapper,
            result,
            &occupied,
            &mut innermost,
            i,
            &mut added_block,
        )?;
        i = if taken > i { taken } else { i + 1 };
    }
    if added_block {
        resort_blocks(result);
    }
    Ok(())
}

fn take_delimited_math(
    source: &str,
    mapper: &Utf16Mapper,
    result: &mut ParseAccumulator,
    occupied: &[(usize, usize)],
    innermost: &mut InnermostCursor,
    i: usize,
    added_block: &mut bool,
) -> Result<usize, ParseError> {
    let bytes = source.as_bytes();
    // Longer openers first so `\\[` is not eaten as `\[` starting one later.
    let candidates: [(&[u8], &[u8], bool); 4] = [
        (b"\\\\[", b"\\\\]", true),
        (b"\\\\(", b"\\\\)", false),
        (b"\\[", b"\\]", true),
        (b"\\(", b"\\)", false),
    ];
    for &(opener, closer, display) in &candidates {
        if !bytes[i..].starts_with(opener) {
            continue;
        }
        let inner_start = i + opener.len();
        let Some(close) = find_math_closer(bytes, inner_start, closer, display) else {
            continue;
        };
        let inner_end = close;
        let full_end = close + closer.len();
        if inner_end <= inner_start {
            continue;
        }
        if range_is_occupied(occupied, i) || range_overlaps(occupied, i, full_end) {
            continue;
        }
        if crosses_link_destination(bytes, inner_start..inner_end) {
            continue;
        }
        // A formula lives inside one block. `\[` in a paragraph and `\]`
        // inside the quote below it claimed a range that overlapped the
        // quote without containing it, and the block list stopped nesting —
        // which everything reading it assumes.
        if innermost.crosses_boundary(result, mapper.to_utf16(i), mapper.to_utf16(full_end)) {
            continue;
        }
        if display {
            emit_delimited_math_block(
                mapper,
                result,
                innermost,
                i..full_end,
                opener.len(),
                closer.len(),
            )?;
            *added_block = true;
        } else {
            emit_delimited_math_span(
                mapper,
                result,
                innermost,
                i..full_end,
                opener.len(),
                closer.len(),
            )?;
        }
        return Ok(full_end);
    }
    Ok(i)
}

fn find_math_closer(bytes: &[u8], from: usize, closer: &[u8], display: bool) -> Option<usize> {
    let mut i = from;
    while i + closer.len() <= bytes.len() {
        if !display && (bytes[i] == b'\n' || bytes[i] == b'\r') {
            return None;
        }
        if bytes[i..].starts_with(closer) {
            return Some(i);
        }
        i += 1;
    }
    None
}

fn emit_delimited_math_span(
    mapper: &Utf16Mapper,
    result: &mut ParseAccumulator,
    innermost: &mut InnermostCursor,
    range: Range<usize>,
    opener_len: usize,
    closer_len: usize,
) -> Result<(), ParseError> {
    let inner = range.start + opener_len..range.end - closer_len;
    let utf = mapper.to_utf16(range.start);
    let owner = innermost.at(result, utf).unwrap_or(0) as u32;
    push_span(result, mapper, &inner, SpanKind::InlineMath, 0, 0)?;
    mark(result, mapper, range.start..range.start + opener_len, owner)?;
    mark(result, mapper, range.end - closer_len..range.end, owner)
}

fn emit_delimited_math_block(
    mapper: &Utf16Mapper,
    result: &mut ParseAccumulator,
    innermost: &mut InnermostCursor,
    range: Range<usize>,
    opener_len: usize,
    closer_len: usize,
) -> Result<(), ParseError> {
    let utf_start = mapper.to_utf16(range.start);
    let enclosing = innermost.at(result, utf_start);
    let depth = enclosing
        .map(|i| result.blocks[i].depth.saturating_add(1))
        .unwrap_or(0);
    let block = push_block(
        result,
        mapper,
        &range,
        BlockKind::MathBlock,
        depth,
        0,
        NO_INFO,
    )?;
    if enclosing.is_none() {
        result.push_top_level(range.clone())?;
    }
    mark(result, mapper, range.start..range.start + opener_len, block)?;
    if range.end >= range.start + opener_len + closer_len {
        mark(result, mapper, range.end - closer_len..range.end, block)?;
    }
    Ok(())
}

/// Obsidian syntax pulldown-cmark does not model: `%%comments%%`,
/// `^[inline footnotes]`, `^block-ids`, and `- [/]` task statuses.
///
/// Runs after every other pass so the literal regions it must respect —
/// code, math, frontmatter, raw HTML — are already known.
fn collect_obsidian_extensions(
    source: &str,
    mapper: &Utf16Mapper,
    result: &mut ParseAccumulator,
) -> Result<(), ParseError> {
    let has_comment = source.contains("%%");
    let has_caret = source.contains('^');
    let has_task = source.contains('[');
    if result.blocks.is_empty() || !(has_comment || has_caret || has_task) {
        return Ok(());
    }

    let verbatim = verbatim_byte_ranges(mapper, result);
    let comments = if has_comment {
        obsidian::comment_ranges(source, &verbatim)
    } else {
        Vec::new()
    };
    let mut excluded = verbatim.clone();
    excluded.extend(comments.iter().cloned());
    let excluded = obsidian::normalise(excluded);

    let footnotes = if has_caret {
        obsidian::inline_footnotes(source, &excluded)
    } else {
        Vec::new()
    };
    let block_ids = if has_caret {
        obsidian::block_ids(source, &excluded)
    } else {
        Vec::new()
    };

    // Every marker needs the block that owns it. Resolve all of them in one
    // sweep: asking per construct is quadratic in a note full of comments.
    let mut queries: Vec<usize> = Vec::new();
    let mut comment_segments: Vec<Range<usize>> = Vec::new();
    for comment in &comments {
        let mut start = comment.start;
        while start < comment.end {
            let end = source[start..comment.end]
                .find('\n')
                .map_or(comment.end, |nl| (start + nl + 1).min(comment.end));
            comment_segments.push(start..end);
            queries.push(start);
            start = end;
        }
    }
    for note in &footnotes {
        queries.push(note.full.start);
    }
    for id in &block_ids {
        queries.push(id.marker.start);
    }
    let owners = block_owners(mapper, result, &queries);
    let mut owner_at = owners.into_iter();

    let mut previous_owner = 0u32;
    for segment in &comment_segments {
        let owner = owner_at
            .next()
            .flatten()
            .map_or(previous_owner, |o| o as u32);
        previous_owner = owner;
        mark(result, mapper, segment.clone(), owner)?;
    }
    for comment in &comments {
        push_span(result, mapper, comment, SpanKind::Comment, 0, 0)?;
    }
    for note in &footnotes {
        let owner = owner_at.next().flatten().unwrap_or(0) as u32;
        push_span(result, mapper, &note.inner, SpanKind::InlineFootnote, 0, 0)?;
        mark(result, mapper, note.full.start..note.inner.start, owner)?;
        mark(result, mapper, note.inner.end..note.full.end, owner)?;
    }
    for id in &block_ids {
        let owner = owner_at.next().flatten().unwrap_or(0) as u32;
        mark(result, mapper, id.marker.clone(), owner)?;
    }

    // What a comment holds is not part of the note: a tag, link or picture
    // written inside one must not style, navigate, or render.
    if !comments.is_empty() {
        let hidden: Vec<(u32, u32)> = comments
            .iter()
            .map(|c| (mapper.to_utf16(c.start), mapper.to_utf16(c.end)))
            .collect();
        result.spans.retain(|span| {
            if span.kind == SpanKind::Comment as u16 {
                return true;
            }
            let index = hidden.partition_point(|&(_, end)| end <= span.start);
            !hidden
                .get(index)
                .is_some_and(|&(start, end)| start <= span.start && span.end <= end)
        });
    }

    if has_task {
        collect_custom_tasks(source, mapper, result, &excluded)?;
    }
    Ok(())
}

/// `==highlights==` whose content is not one plain text run — `==**bold**==`,
/// `==[[link]]==`, `==a *b* c==` — which the per-run scan cannot pair.
///
/// Uses the same adjacency rules as the per-run scan, pairs only within one
/// block and never across a blank line, and refuses a pair that would cut
/// through another construct (`**a ==b** c==`), so a highlight always nests.
fn collect_cross_run_highlights(
    source: &str,
    mapper: &Utf16Mapper,
    result: &mut ParseAccumulator,
) -> Result<(), ParseError> {
    if result.blocks.is_empty() || !source.contains("==") {
        return Ok(());
    }
    // A delimiter cannot sit inside existing syntax (which already covers the
    // `==` of every per-run highlight), code, math, or a comment.
    let mut blocked = verbatim_byte_ranges(mapper, result);
    blocked.extend(
        result
            .markers
            .iter()
            .map(|m| mapper.to_byte(m.start)..mapper.to_byte(m.end)),
    );
    blocked.extend(
        result
            .spans
            .iter()
            .filter(|s| s.kind == SpanKind::Comment as u16)
            .map(|s| mapper.to_byte(s.start)..mapper.to_byte(s.end)),
    );
    let blocked = obsidian::normalise(blocked);

    let bytes = source.as_bytes();
    let mut candidates = Vec::new();
    let mut i = 0;
    while i + 1 < bytes.len() {
        if bytes[i] == b'=' && bytes[i + 1] == b'=' && !obsidian::overlaps(&blocked, i, i + 2) {
            candidates.push(i);
            i += 2;
        } else {
            i += 1;
        }
    }
    if candidates.len() < 2 {
        return Ok(());
    }
    let owners = block_owners(mapper, result, &candidates);

    // Constructs a highlight must not cut through, sorted by start.
    let mut spans: Vec<(usize, usize)> = result
        .spans
        .iter()
        .filter(|s| s.kind != SpanKind::Heading as u16)
        .map(|s| (mapper.to_byte(s.start), mapper.to_byte(s.end)))
        .collect();
    spans.sort_unstable();

    let mut found: Vec<(usize, usize, usize)> = Vec::new();
    let mut k = 0;
    while k + 1 < candidates.len() {
        let (open, close) = (candidates[k], candidates[k + 1]);
        let owner = owners[k];
        let valid = owner.is_some()
            && owners[k + 1] == owner
            && highlight_is_valid(source, open, close)
            && !source[open..close].contains("\n\n")
            && !source[open..close].contains("\r\n\r\n")
            && {
                let block_start = owner.map_or(0, |o| mapper.to_byte(result.blocks[o].start));
                let (inner_start, inner_end) = (open + 2, close);
                let from = spans.partition_point(|&(s, _)| s < block_start);
                let to = spans.partition_point(|&(s, _)| s < inner_end);
                spans[from..to].iter().all(|&(s, e)| {
                    let inside = s >= inner_start && e <= inner_end;
                    let around = s <= open && e >= close + 2;
                    let apart = e <= inner_start || s >= inner_end;
                    inside || around || apart
                })
            };
        if valid {
            found.push((open, close, owner.unwrap_or(0)));
            k += 2;
        } else {
            k += 1;
        }
    }
    for (open, close, owner) in found {
        push_span(
            result,
            mapper,
            &(open + 2..close),
            SpanKind::Highlight,
            0,
            0,
        )?;
        mark(result, mapper, open..open + 2, owner as u32)?;
        mark(result, mapper, close..close + 2, owner as u32)?;
    }
    Ok(())
}

/// `- [/]`-style list items: a checked task marker pulldown does not see.
fn collect_custom_tasks(
    source: &str,
    mapper: &Utf16Mapper,
    result: &mut ParseAccumulator,
    excluded: &[Range<usize>],
) -> Result<(), ParseError> {
    let items: Vec<(usize, u32)> = result
        .blocks
        .iter()
        .enumerate()
        .filter(|(_, b)| b.kind == BlockKind::ListItem as u16)
        .map(|(i, b)| (i, b.start))
        .collect();
    if items.is_empty() {
        return Ok(());
    }
    let mut existing: Vec<u32> = result
        .spans
        .iter()
        .filter(|s| s.kind == SpanKind::TaskMarker as u16)
        .map(|s| s.start)
        .collect();
    existing.sort_unstable();
    for (index, start) in items {
        let byte = mapper.to_byte(start);
        let Some(task) = obsidian::custom_task_at(source, byte) else {
            continue;
        };
        if obsidian::overlaps(excluded, task.marker.start, task.marker.end) {
            continue;
        }
        let at = mapper.to_utf16(task.marker.start);
        if existing.binary_search(&at).is_ok() {
            continue;
        }
        // Bit 0 is "done", as for `[x]`; the status character rides above it
        // so the editor can draw `[/]`, `[-]`, `[>]` distinctly.
        let data = 1 | (u32::from(task.status) << TASK_STATUS_SHIFT);
        push_span(result, mapper, &task.marker, SpanKind::TaskMarker, 0, data)?;
        // The box and the space after it, exactly as pulldown's own `[x]`.
        let bytes = source.as_bytes();
        let mut end = task.marker.end;
        while end < bytes.len() && matches!(bytes[end], b' ' | b'\t') {
            end += 1;
        }
        mark(result, mapper, task.marker.start..end, index as u32)?;
    }
    Ok(())
}

/// Byte ranges whose text is literal: code, math, frontmatter, raw HTML.
fn verbatim_byte_ranges(mapper: &Utf16Mapper, result: &ParseResult) -> Vec<Range<usize>> {
    let mut ranges = Vec::new();
    for b in &result.blocks {
        if b.kind == BlockKind::CodeBlock as u16
            || b.kind == BlockKind::MermaidBlock as u16
            || b.kind == BlockKind::MathBlock as u16
            || b.kind == BlockKind::Frontmatter as u16
            || b.kind == BlockKind::HtmlBlock as u16
        {
            ranges.push(mapper.to_byte(b.start)..mapper.to_byte(b.end));
        }
    }
    for s in &result.spans {
        if s.kind == SpanKind::InlineCode as u16
            || s.kind == SpanKind::InlineMath as u16
            || s.kind == SpanKind::InlineHtml as u16
        {
            // Inline spans cover only their content; widen by the
            // delimiter so a backtick-wrapped `%%` is caught at its edge.
            let start = mapper.to_byte(s.start).saturating_sub(1);
            let end = (mapper.to_byte(s.end) + 1).min(mapper.to_byte(u32::MAX));
            ranges.push(start..end);
        }
    }
    obsidian::normalise(ranges)
}

/// The innermost block containing each byte position, in one sweep.
fn block_owners(
    mapper: &Utf16Mapper,
    result: &ParseResult,
    positions: &[usize],
) -> Vec<Option<usize>> {
    let mut queries: Vec<(u32, usize)> = positions
        .iter()
        .enumerate()
        .map(|(q, &byte)| (mapper.to_utf16(byte), q))
        .collect();
    queries.sort_unstable();
    let mut owners = vec![None; positions.len()];
    let mut innermost = InnermostCursor::new(result);
    for (at, q) in queries {
        owners[q] = innermost.at(result, at);
    }
    owners
}

/// The innermost block containing a UTF-16 offset, by scanning every block.
/// The specification [`InnermostCursor`] is tested against: smallest
/// containing range, and among identical ranges the deepest — the last in
/// the parser's pre-order.
#[cfg(test)]
fn innermost_block(result: &ParseResult, utf16: u32) -> Option<usize> {
    result
        .blocks
        .iter()
        .enumerate()
        .filter(|(_, b)| b.start <= utf16 && utf16 < b.end)
        .min_by_key(|&(i, b)| (b.end.saturating_sub(b.start), std::cmp::Reverse(i)))
        .map(|(i, _)| i)
}

fn occupied_byte_ranges(mapper: &Utf16Mapper, result: &ParseResult) -> Vec<(usize, usize)> {
    let mut v: Vec<(usize, usize)> = Vec::new();
    for b in &result.blocks {
        if b.kind == BlockKind::CodeBlock as u16
            || b.kind == BlockKind::MermaidBlock as u16
            || b.kind == BlockKind::MathBlock as u16
            || b.kind == BlockKind::Frontmatter as u16
            || b.kind == BlockKind::HtmlBlock as u16
        {
            let start = mapper.to_byte(b.start);
            let end = mapper.to_byte(b.end);
            if end > start {
                v.push((start, end));
            }
        }
    }
    for s in &result.spans {
        if s.kind == SpanKind::InlineCode as u16
            || s.kind == SpanKind::InlineMath as u16
            || s.kind == SpanKind::Link as u16
            || s.kind == SpanKind::WikiLink as u16
            || s.kind == SpanKind::Image as u16
            || s.kind == SpanKind::InlineHtml as u16
        {
            let start = mapper.to_byte(s.start);
            let end = mapper.to_byte(s.end);
            if end > start {
                v.push((start, end));
            }
        }
    }
    if v.len() < 2 {
        return v;
    }
    v.sort_unstable();
    let mut out = Vec::with_capacity(v.len());
    let mut cur = v[0];
    for next in v.into_iter().skip(1) {
        if next.0 <= cur.1 {
            cur.1 = cur.1.max(next.1);
        } else {
            out.push(cur);
            cur = next;
        }
    }
    out.push(cur);
    out
}

/// Whether `pos` lies in one of `occupied`'s intervals.
///
/// `occupied` is [`occupied_byte_ranges`]'s output: sorted, merged, and so
/// disjoint, which is what makes a binary search exact. A linear scan here
/// ran once per backslash in the document, so a note with many code spans
/// and many backslashes went quadratic.
fn range_is_occupied(occupied: &[(usize, usize)], pos: usize) -> bool {
    let after = occupied.partition_point(|&(s, _)| s <= pos);
    after > 0 && pos < occupied[after - 1].1
}

/// Whether `start..end` overlaps one of `occupied`'s disjoint, sorted
/// intervals. Only the last interval starting before `end` can: any earlier
/// one ends before that one starts.
fn range_overlaps(occupied: &[(usize, usize)], start: usize, end: usize) -> bool {
    let before_end = occupied.partition_point(|&(s, _)| s < end);
    before_end > 0 && occupied[before_end - 1].1 > start
}

/// The innermost block containing a position, for positions visited in
/// increasing order — the one sweep both [`block_owners`] and the
/// delimited-math pass use.
///
/// Blocks nest, so after sorting by start a stack of open blocks always has
/// the innermost one on top: amortised O(1) per query. The math pass used to
/// scan every block once per formula instead, and since each `\[…\]` adds a
/// block, a note of N display formulas cost O(N²) — 1.1 s for 128 KiB of
/// them where pulldown-cmark took 2 ms.
///
/// Blocks pushed after the cursor was built (the math blocks that pass adds)
/// are not seen, which is exact there: each ends where the scan resumes, so
/// it contains no later position. A query that moves backwards restarts the
/// sweep rather than answering from a stack that has moved past it.
struct InnermostCursor {
    /// Block indices by start, outer before inner; among identical ranges
    /// the later (deeper, in pre-order) one is pushed last, so it is on top.
    order: Vec<usize>,
    next: usize,
    stack: Vec<usize>,
    last: u32,
}

impl InnermostCursor {
    fn new(result: &ParseResult) -> Self {
        let mut order: Vec<usize> = (0..result.blocks.len()).collect();
        order.sort_by_key(|&i| {
            let b = &result.blocks[i];
            (b.start, std::cmp::Reverse(b.end), i)
        });
        Self {
            order,
            next: 0,
            stack: Vec::new(),
            last: 0,
        }
    }

    fn at(&mut self, result: &ParseResult, utf16: u32) -> Option<usize> {
        if utf16 < self.last {
            self.next = 0;
            self.stack.clear();
        }
        self.last = utf16;
        let blocks = &result.blocks;
        while let Some(&index) = self
            .order
            .get(self.next)
            .filter(|&&i| blocks[i].start <= utf16)
        {
            self.next += 1;
            while self
                .stack
                .last()
                .is_some_and(|&top| blocks[top].end <= blocks[index].start)
            {
                self.stack.pop();
            }
            self.stack.push(index);
        }
        while self
            .stack
            .last()
            .is_some_and(|&top| blocks[top].end <= utf16)
        {
            self.stack.pop();
        }
        // The top contains `utf16` whenever blocks nest; searching down
        // keeps the answer right even for a pair that merely overlaps.
        self.stack
            .iter()
            .rev()
            .find(|&&i| blocks[i].start <= utf16 && utf16 < blocks[i].end)
            .copied()
    }

    /// Whether a construct over `start..end` would cross a block boundary:
    /// run past the end of the innermost block holding `start`, or contain
    /// the start of another block.
    fn crosses_boundary(&mut self, result: &ParseResult, start: u32, end: u32) -> bool {
        let blocks = &result.blocks;
        if let Some(owner) = self.at(result, start) {
            if end > blocks[owner].end {
                return true;
            }
        }
        let after = self.order.partition_point(|&i| blocks[i].start <= start);
        self.order
            .get(after)
            .is_some_and(|&i| blocks[i].start < end)
    }
}

/// pulldown-cmark consumes link reference definitions and emits no event for
/// them, so they would otherwise sit in the document as leftover source.
///
/// Walks only the gaps between existing blocks. Scanning every line against
/// every block is quadratic and was measured at 14× for 4× the text.
fn collect_link_reference_definitions(
    source: &str,
    mapper: &Utf16Mapper,
    result: &mut ParseAccumulator,
) -> Result<(), ParseError> {
    let occupied = merge_utf16_intervals(
        result
            .blocks
            .iter()
            .filter(|b| b.end > b.start)
            .map(|b| (b.start, b.end)),
    );
    let doc_end = mapper.len_utf16();
    let mut cursor = 0u32;
    let mut found = false;
    for &(start, end) in &occupied {
        if start > cursor {
            found |= emit_link_defs_in_span(source, mapper, result, cursor, start)?;
        }
        if end > cursor {
            cursor = end;
        }
    }
    if cursor < doc_end {
        found |= emit_link_defs_in_span(source, mapper, result, cursor, doc_end)?;
    }
    if found {
        resort_blocks(result);
    }
    Ok(())
}

fn merge_utf16_intervals(intervals: impl Iterator<Item = (u32, u32)>) -> Vec<(u32, u32)> {
    let mut v: Vec<(u32, u32)> = intervals.collect();
    if v.len() < 2 {
        return v;
    }
    v.sort_unstable();
    let mut out = Vec::with_capacity(v.len());
    let mut cur = v[0];
    for next in v.into_iter().skip(1) {
        if next.0 <= cur.1 {
            cur.1 = cur.1.max(next.1);
        } else {
            out.push(cur);
            cur = next;
        }
    }
    out.push(cur);
    out
}

fn emit_link_defs_in_span(
    source: &str,
    mapper: &Utf16Mapper,
    result: &mut ParseAccumulator,
    utf_start: u32,
    utf_end: u32,
) -> Result<bool, ParseError> {
    if utf_end <= utf_start {
        return Ok(false);
    }
    let bytes = source.as_bytes();
    let span_end = mapper.to_byte(utf_end);
    let mut i = mapper.to_byte(utf_start);
    if i > 0 && i < bytes.len() && bytes[i - 1] != b'\n' && bytes[i - 1] != b'\r' {
        i = next_line_start(source, i);
    }
    let mut found = false;
    while i < span_end {
        if let Some(def_end) = scan_link_reference_definition(source, i) {
            let def_utf_end = mapper.to_utf16(def_end);
            if def_utf_end > utf_end {
                break;
            }
            let utf_s = mapper.to_utf16(i);
            let idx = result.blocks.len() as u32;
            result.push_block(BlockDescriptor {
                start: utf_s,
                end: def_utf_end,
                kind: BlockKind::LinkReferenceDefinition as u16,
                depth: 0,
                data: 0,
                info: NO_INFO,
            })?;
            mark(result, mapper, i..def_end, idx)?;
            result.push_top_level(i..def_end)?;
            found = true;
            i = def_end;
            continue;
        }
        let next = next_line_start(source, i);
        if next <= i {
            break;
        }
        i = next;
    }
    Ok(found)
}

fn next_line_start(source: &str, from: usize) -> usize {
    match source.get(from..).and_then(|s| s.find('\n')) {
        Some(n) => from + n + 1,
        None => source.len(),
    }
}

/// CommonMark link reference definition starting at `start`, or `None`.
///
/// CommonMark: optional whitespace after the colon includes one newline, so
/// the destination may sit on the next line, and a quoted title may follow
/// on the destination line or the line after that.
fn scan_link_reference_definition(source: &str, start: usize) -> Option<usize> {
    let bytes = source.as_bytes();
    let mut i = start;
    let mut spaces = 0;
    while i < bytes.len() && bytes[i] == b' ' && spaces < 3 {
        i += 1;
        spaces += 1;
    }
    if i < bytes.len() && (bytes[i] == b' ' || bytes[i] == b'\t') {
        return None;
    }
    if i >= bytes.len() || bytes[i] != b'[' {
        return None;
    }
    i += 1;
    let label_start = i;
    let mut escaped = false;
    while i < bytes.len() {
        let b = bytes[i];
        if b == b'\n' || b == b'\r' {
            return None;
        }
        if escaped {
            escaped = false;
            i += 1;
            continue;
        }
        if b == b'\\' {
            escaped = true;
            i += 1;
            continue;
        }
        if b == b'[' {
            return None;
        }
        if b == b']' {
            break;
        }
        i += 1;
    }
    if i >= bytes.len() || bytes[i] != b']' || i == label_start {
        return None;
    }
    i += 1;
    if i >= bytes.len() || bytes[i] != b':' {
        return None;
    }
    i += 1;
    i = skip_space_and_one_newline(bytes, i);
    if i >= bytes.len() || bytes[i] == b'\n' || bytes[i] == b'\r' {
        return None;
    }
    // Destination: <...> or a run of non-space.
    if bytes[i] == b'<' {
        i += 1;
        while i < bytes.len() && bytes[i] != b'>' && bytes[i] != b'\n' {
            i += 1;
        }
        if i >= bytes.len() || bytes[i] != b'>' {
            return None;
        }
        i += 1;
    } else {
        let dest_start = i;
        while i < bytes.len() && !bytes[i].is_ascii_whitespace() {
            i += 1;
        }
        if i == dest_start {
            return None;
        }
    }
    while i < bytes.len() && (bytes[i] == b' ' || bytes[i] == b'\t') {
        i += 1;
    }
    if let Some(after_title) = scan_link_title(bytes, i) {
        return finish_definition_line(bytes, after_title);
    }
    // Optional title on the next line. If that line is not a title, the
    // definition ended on the destination line — do not swallow the paragraph
    // that follows.
    if i < bytes.len() && (bytes[i] == b'\n' || bytes[i] == b'\r') {
        let dest_line_end = skip_one_newline(bytes, i);
        let after = skip_spaces(bytes, dest_line_end);
        if let Some(after_title) = scan_link_title(bytes, after) {
            return finish_definition_line(bytes, after_title);
        }
        return Some(dest_line_end);
    }
    finish_definition_line(bytes, i)
}

fn skip_spaces(bytes: &[u8], mut i: usize) -> usize {
    while i < bytes.len() && (bytes[i] == b' ' || bytes[i] == b'\t') {
        i += 1;
    }
    i
}

fn skip_one_newline(bytes: &[u8], mut i: usize) -> usize {
    if i < bytes.len() && bytes[i] == b'\r' {
        i += 1;
    }
    if i < bytes.len() && bytes[i] == b'\n' {
        i += 1;
    }
    i
}

/// Spaces, then at most one line ending, then spaces — CommonMark's
/// "optional whitespace including up to one line ending".
fn skip_space_and_one_newline(bytes: &[u8], i: usize) -> usize {
    skip_spaces(bytes, skip_one_newline(bytes, skip_spaces(bytes, i)))
}

fn scan_link_title(bytes: &[u8], mut i: usize) -> Option<usize> {
    if i >= bytes.len() {
        return None;
    }
    let closer = match bytes[i] {
        b'"' | b'\'' => bytes[i],
        b'(' => b')',
        _ => return None,
    };
    i += 1;
    while i < bytes.len() && bytes[i] != closer && bytes[i] != b'\n' && bytes[i] != b'\r' {
        i += 1;
    }
    if i >= bytes.len() || bytes[i] != closer {
        return None;
    }
    Some(skip_spaces(bytes, i + 1))
}

fn finish_definition_line(bytes: &[u8], i: usize) -> Option<usize> {
    if i < bytes.len() && bytes[i] != b'\n' && bytes[i] != b'\r' {
        return None;
    }
    Some(skip_one_newline(bytes, i))
}

fn resort_blocks(result: &mut ParseResult) {
    let n = result.blocks.len();
    if n < 2 {
        return;
    }
    let mut order: Vec<usize> = (0..n).collect();
    // Parents before children at the same start: a List and its first
    // ListItem share an offset, and `MarkdownStyler.topLevel` takes the first
    // block whose start is free. Sorting by `(start, end)` put the shorter
    // child first, so every item looked top-level the moment a link
    // definition forced this rematerialisation.
    order.sort_by(|&a, &b| {
        let x = &result.blocks[a];
        let y = &result.blocks[b];
        x.start
            .cmp(&y.start)
            .then(y.end.cmp(&x.end))
            .then(a.cmp(&b))
    });
    if order.iter().copied().eq(0..n) {
        return;
    }
    let mut new_index = vec![0u32; n];
    for (new_i, &old_i) in order.iter().enumerate() {
        new_index[old_i] = new_i as u32;
    }
    let old = std::mem::take(&mut result.blocks);
    result.blocks = order.into_iter().map(|i| old[i]).collect();
    for m in &mut result.markers {
        if (m.block as usize) < new_index.len() {
            m.block = new_index[m.block as usize];
        }
    }
}

/// Hides `>` (and one following space) at the start of every line inside a
/// blockquote. The first one is already caught by the gap rule; re-marking it
/// is harmless because markers are deduplicated by the editor's range set.
fn mark_quote_prefixes(
    source: &str,
    range: &Range<usize>,
    mapper: &Utf16Mapper,
    result: &mut ParseAccumulator,
    block: u32,
) -> Result<(), ParseError> {
    let bytes = source.as_bytes();
    let mut i = range.start;
    let mut at_line_start = true;
    while i < range.end.min(bytes.len()) {
        if at_line_start {
            let mut j = i;
            // Leading indentation is layout, not syntax.
            while j < range.end && (bytes[j] == b' ' || bytes[j] == b'\t') {
                j += 1;
            }
            if j < range.end && bytes[j] == b'>' {
                let mut k = j + 1;
                if k < range.end && bytes[k] == b' ' {
                    k += 1;
                }
                mark(result, mapper, j..k, block)?;
                i = k;
                at_line_start = false;
                continue;
            }
        }
        at_line_start = bytes[i] == b'\n';
        i += 1;
    }
    Ok(())
}

/// Finds constructs `pulldown-cmark` does not model: `#tag` and `==highlight==`.
///
/// Scanning only inside `Text` event ranges is what keeps a `#` in a code
/// fence or a URL fragment from being mistaken for a tag.
fn scan_text_extensions(
    source: &str,
    range: &Range<usize>,
    mapper: &Utf16Mapper,
    result: &mut ParseAccumulator,
    block_stack: &[usize],
) -> Result<(), ParseError> {
    let text = &source[range.clone()];
    let bytes = text.as_bytes();
    let base = range.start;

    // ==highlight==
    //
    // Pairing is MarkDev's own, so the adjacency rules live here rather
    // than in a vendored parser: the opener must sit outside an ASCII word,
    // both inner flanks must be tight (`== spaced ==` is prose), nothing
    // may follow the closer but a digit, and an empty pair hides nothing.
    // Without them `x == y == z` swallowed the words between, base64 URL
    // padding paired across two URLs, and `a ==== b` hid four characters
    // and drew nothing — the pure gap this rule exists to refuse.
    for inner in scan_highlights(text) {
        push_span(
            result,
            mapper,
            &(base + inner.start..base + inner.end),
            SpanKind::Highlight,
            0,
            0,
        )?;
        mark_current(
            result,
            mapper,
            base + inner.start - 2..base + inner.start,
            block_stack,
        )?;
        mark_current(
            result,
            mapper,
            base + inner.end..base + inner.end + 2,
            block_stack,
        )?;
    }

    // #tag — must start a word, and needs at least one non-digit so that
    // "#1" reads as a number rather than a tag.
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'#' && (i == 0 || is_boundary(bytes[i - 1])) {
            let start = i + 1;
            let mut j = start;
            while j < bytes.len() && is_tag_byte(bytes[j]) {
                j += 1;
            }
            if j > start && text[start..j].bytes().any(|b| !b.is_ascii_digit()) {
                push_span(result, mapper, &(base + i..base + j), SpanKind::Tag, 0, 0)?;
                i = j;
                continue;
            }
        }
        i += 1;
    }
    Ok(())
}

/// Byte ranges of `==highlight==` *contents* in `text` (delimiters excluded).
///
/// Shared with HTML export so a highlight means the same thing on the page
/// and in the editor.
pub fn scan_highlights(text: &str) -> Vec<Range<usize>> {
    let bytes = text.as_bytes();
    let mut out = Vec::new();
    let mut i = 0;
    while i + 1 < bytes.len() {
        if bytes[i] == b'=' && bytes[i + 1] == b'=' {
            if let Some(close) = find_pair(bytes, i + 2, b'=') {
                if highlight_is_valid(text, i, close) {
                    out.push(i + 2..close);
                    i = close + 2;
                    continue;
                }
            }
        }
        i += 1;
    }
    out
}

fn find_pair(bytes: &[u8], from: usize, delim: u8) -> Option<usize> {
    let mut i = from;
    while i + 1 < bytes.len() {
        if bytes[i] == delim && bytes[i + 1] == delim {
            return Some(i);
        }
        i += 1;
    }
    None
}

/// Whether the `==` run at `open` may highlight up to the one at `close`.
///
/// Both are offsets into `text`, the `Text` event being scanned; `close`
/// is the start of the closing run, so the content is `open+2..close`.
/// The rules mirror [`inline_math_is_valid`] and exist for the same
/// reason: a pair that forms across ordinary prose hides its delimiters
/// and styles everything between, which reads as data loss.
///
/// The boundary check is ASCII-only for the reason its sibling's is:
/// Chinese and Japanese carry no spaces, so `这是==重点==内容` is normal
/// spelling, not an equals sign welded into a word.
fn highlight_is_valid(text: &str, open: usize, close: usize) -> bool {
    let bytes = text.as_bytes();
    // An empty or absent body highlights nothing — `a ==== b` must keep
    // every character it typed.
    if close <= open + 2 {
        return false;
    }
    // Tight inner flanks: `== spaced ==` is arithmetic, not emphasis.
    if bytes[open + 2].is_ascii_whitespace() || bytes[close - 1].is_ascii_whitespace() {
        return false;
    }
    // Outside an ASCII word on the left: base64 padding (`dGVzdA==`) and
    // glued comparisons (`a=b==c`) both put `==` inside a token.
    if open > 0 && bytes[open - 1].is_ascii_alphanumeric() {
        return false;
    }
    // Nothing follows the closer but a digit — the currency rule again,
    // because `==5==6` is arithmetic.
    if bytes.get(close + 2).is_some_and(|b| b.is_ascii_digit()) {
        return false;
    }
    true
}

/// Byte ranges of `#tag` occurrences in `text`.
///
/// Shared with the vault indexer so a tag means the same thing in the editor
/// and in the tag browser. Two implementations would drift the moment either
/// gained a rule.
pub fn scan_tags(text: &str) -> Vec<Range<usize>> {
    let bytes = text.as_bytes();
    let mut out = Vec::new();
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'#' && (i == 0 || is_boundary(bytes[i - 1])) {
            let start = i + 1;
            let mut j = start;
            while j < bytes.len() && is_tag_byte(bytes[j]) {
                j += 1;
            }
            if j > start && text[start..j].bytes().any(|b| !b.is_ascii_digit()) {
                out.push(i..j);
                i = j;
                continue;
            }
        }
        i += 1;
    }
    out
}

fn is_boundary(b: u8) -> bool {
    b.is_ascii_whitespace() || b == b'(' || b == b'[' || b == b'{' || b == b','
}

fn is_tag_byte(b: u8) -> bool {
    b.is_ascii_alphanumeric() || b == b'-' || b == b'_' || b == b'/' || !b.is_ascii()
}

/// Blocks whose text content is literal and must not be scanned for MarkDev's
/// own inline extensions.
fn is_verbatim_tag(tag: &Tag) -> bool {
    matches!(
        tag,
        Tag::CodeBlock(_) | Tag::HtmlBlock | Tag::MetadataBlock(_)
    )
}

fn is_verbatim_end(end: &TagEnd) -> bool {
    matches!(
        end,
        TagEnd::CodeBlock | TagEnd::HtmlBlock | TagEnd::MetadataBlock(_)
    )
}

fn callout_kind(k: BlockQuoteKind) -> CalloutKind {
    match k {
        BlockQuoteKind::Note => CalloutKind::Note,
        BlockQuoteKind::Tip => CalloutKind::Tip,
        BlockQuoteKind::Important => CalloutKind::Important,
        BlockQuoteKind::Warning => CalloutKind::Warning,
        BlockQuoteKind::Caution => CalloutKind::Caution,
    }
}

#[cfg(test)]
mod limit_tests {
    use super::*;

    #[test]
    fn event_budget_accepts_the_exact_boundary_and_rejects_plus_one() {
        let mut count = 0;
        for _ in 0..MAX_PARSE_EVENTS {
            charge_event(&mut count).expect("exact event budget");
        }
        assert_eq!(count, MAX_PARSE_EVENTS);
        assert_eq!(charge_event(&mut count), Err(ParseError::TooManyEvents));
    }
}

#[cfg(test)]
mod sweep_tests {
    use super::*;

    /// Deterministic documents over a construct-heavy alphabet, so a failure
    /// reproduces from its seed alone.
    fn documents(count: usize) -> Vec<String> {
        const TOKENS: &[&str] = &[
            "# ",
            "> ",
            ">",
            "- ",
            "  - ",
            "1. ",
            "\\[x\\]",
            "\\(y\\)",
            "\\\\[z\\\\]",
            "$$w$$",
            "$v$",
            "```\n",
            "~~~\n",
            "`c`",
            "| a | b |\n|-|-|\n| 1 | 2 |\n",
            "[!NOTE]",
            "**",
            "_",
            "[l](u)",
            "[[w]]",
            "<div>",
            "---\n",
            "\n",
            "\n\n",
            " ",
            "text",
            "\\",
            "é",
            "😀",
            "[^1]",
            "[^1]: n\n",
            "[r]: /x\n",
            "%%c%%",
            "^[n]",
            "- [ ] ",
            "==h==",
            "\r\n",
        ];
        let mut state = 0x9E37_79B9_7F4A_7C15_u64;
        let mut next = move || {
            state ^= state << 13;
            state ^= state >> 7;
            state ^= state << 17;
            state
        };
        (0..count)
            .map(|_| {
                let len = (next() % 120) as usize;
                (0..len)
                    .map(|_| TOKENS[(next() % TOKENS.len() as u64) as usize])
                    .collect()
            })
            .collect()
    }

    #[test]
    fn the_cursor_answers_exactly_what_a_full_scan_does() {
        let mut checked = 0usize;
        for (seed, source) in documents(1_500).iter().enumerate() {
            let Ok(parsed) = parse_checked(source) else {
                continue;
            };
            let len = Utf16Mapper::new(source).len_utf16();
            let mut cursor = InnermostCursor::new(&parsed);
            for at in 0..=len {
                let (swept, scanned) = (cursor.at(&parsed, at), innermost_block(&parsed, at));
                assert_eq!(
                    swept,
                    scanned,
                    "seed {seed}, offset {at}: swept {:?}, scanned {:?}",
                    swept.map(|i| parsed.blocks[i]),
                    scanned.map(|i| parsed.blocks[i]),
                );
                checked += 1;
            }
            // Backwards and repeated queries restart rather than misanswer.
            for at in (0..=len).rev().step_by(3) {
                assert_eq!(cursor.at(&parsed, at), innermost_block(&parsed, at));
            }
        }
        // Guard against a generator that stopped producing parseable input:
        // a differential test over nothing passes vacuously.
        assert!(checked > 50_000, "only {checked} positions compared");
    }

    #[test]
    fn blocks_always_nest() {
        // The contract every reader of the block list assumes. Found broken
        // by the cursor's differential test: a `\[…\]` formula claimed across
        // a quote's opening line overlapped the quote without containing it.
        let mut compared = 0usize;
        for (seed, source) in documents(1_500).iter().enumerate() {
            let Ok(parsed) = parse_checked(source) else {
                continue;
            };
            for (index, x) in parsed.blocks.iter().enumerate() {
                for y in &parsed.blocks[index + 1..] {
                    let disjoint = x.end <= y.start || y.end <= x.start;
                    let nested = (x.start <= y.start && y.end <= x.end)
                        || (y.start <= x.start && x.end <= y.end);
                    assert!(
                        disjoint || nested,
                        "seed {seed}: {x:?} overlaps {y:?} in {source:?}"
                    );
                    compared += 1;
                }
            }
        }
        assert!(compared > 10_000, "only {compared} block pairs compared");
    }

    #[test]
    fn a_delimited_formula_stays_inside_one_block() {
        let kinds = |source: &str| {
            let parsed = parse_checked(source).unwrap();
            (
                parsed
                    .blocks
                    .iter()
                    .any(|b| b.kind == BlockKind::MathBlock as u16),
                parsed
                    .spans
                    .iter()
                    .any(|s| s.kind == SpanKind::InlineMath as u16),
            )
        };
        // Across a quote's opening, a list item, a blank line, table cells.
        assert_eq!(kinds("\\[a\n> b\\]"), (false, false));
        assert_eq!(kinds("x \\[a\n- b\\]"), (false, false));
        assert_eq!(kinds("\\[a\n\nb\\]"), (false, false));
        assert_eq!(kinds("| \\(a | b\\) |\n|-|-|\n"), (false, false));
        // Within one block it is still a formula.
        assert_eq!(kinds("\\[a\nb\\]"), (true, false));
        assert_eq!(kinds("> \\[a\n> b\\]"), (true, false));
        assert_eq!(kinds("x \\(y\\) z"), (false, true));
        assert_eq!(kinds("| \\(a\\) | b |\n|-|-|\n"), (false, true));
    }

    #[test]
    fn binary_searches_answer_what_linear_scans_do() {
        let mut nonempty = 0usize;
        for source in documents(400) {
            let Ok(parsed) = parse_checked(&source) else {
                continue;
            };
            let mapper = Utf16Mapper::new(&source);
            let occupied = occupied_byte_ranges(&mapper, &parsed);
            nonempty += usize::from(!occupied.is_empty());
            let inside = |p: usize| occupied.iter().any(|&(s, e)| p >= s && p < e);
            let overlaps = |a: usize, b: usize| occupied.iter().any(|&(s, e)| a < e && b > s);
            for start in 0..=source.len() {
                assert_eq!(
                    range_is_occupied(&occupied, start),
                    inside(start),
                    "{source:?} @{start}"
                );
                for end in start..=(start + 6).min(source.len()) {
                    assert_eq!(
                        range_overlaps(&occupied, start, end),
                        overlaps(start, end),
                        "{source:?} {start}..{end}"
                    );
                }
            }
        }
        assert!(
            nonempty > 200,
            "only {nonempty} documents had occupied ranges"
        );
    }
}
