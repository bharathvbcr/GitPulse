//! Obsidian-flavoured syntax that pulldown-cmark does not model.
//!
//! One scanner per construct, shared by the editor model, the vault indexer,
//! and HTML export, so a comment or a block id means the same thing in all
//! three. Every function works in **byte** offsets into the source and takes
//! the ranges that must stay literal (code, math, frontmatter, raw HTML) as
//! input, because "a `%%` inside a code span is code" is a rule each caller
//! would otherwise re-derive differently.

use std::ops::Range;

use pulldown_cmark::{Event, Parser, Tag, TagEnd};

use super::model::{CalloutKind, CALLOUT_FOLD_COLLAPSED, CALLOUT_FOLD_EXPANDED};
use super::parse::options;

/// File extensions an `![[embed]]` draws as a picture or media player rather
/// than transcluding as a note.
const MEDIA_EXTENSIONS: &[&str] = &[
    "png", "jpg", "jpeg", "gif", "bmp", "svg", "webp", "avif", "heic", "heif", "tif", "tiff",
    "ico", "pdf", "mp3", "wav", "m4a", "ogg", "flac", "3gp", "webm", "mp4", "mov", "mkv", "ogv",
    "m4v",
];

/// Picture formats among [`MEDIA_EXTENSIONS`].
const IMAGE_EXTENSIONS: &[&str] = &[
    "png", "jpg", "jpeg", "gif", "bmp", "svg", "webp", "avif", "heic", "heif", "tif", "tiff", "ico",
];

fn extension(target: &str) -> Option<String> {
    let path = target.split(['#', '?']).next().unwrap_or(target);
    let name = path.rsplit('/').next().unwrap_or(path);
    let (stem, ext) = name.rsplit_once('.')?;
    (!stem.is_empty() && !ext.is_empty()).then(|| ext.to_ascii_lowercase())
}

/// Whether an embed target names a media file (picture, PDF, audio, video).
pub fn is_media_target(target: &str) -> bool {
    extension(target).is_some_and(|ext| MEDIA_EXTENSIONS.contains(&ext.as_str()))
}

/// Whether an embed target names a picture.
pub fn is_image_target(target: &str) -> bool {
    extension(target).is_some_and(|ext| IMAGE_EXTENSIONS.contains(&ext.as_str()))
}

/// Whether an embed target names audio.
pub fn is_audio_target(target: &str) -> bool {
    extension(target)
        .is_some_and(|ext| matches!(ext.as_str(), "mp3" | "wav" | "m4a" | "ogg" | "flac" | "3gp"))
}

/// Whether an embed target names video.
pub fn is_video_target(target: &str) -> bool {
    extension(target)
        .is_some_and(|ext| matches!(ext.as_str(), "webm" | "mp4" | "mov" | "mkv" | "ogv" | "m4v"))
}

/// Obsidian's picture size suffix: `300` (width) or `300x200`.
///
/// Returns `None` for anything else, so `![[pic.png|A caption]]` keeps its
/// text as alt. Sizes are capped at 10,000 so a typo cannot ask for a
/// picture the size of a building.
pub fn parse_size(spec: &str) -> Option<(u32, Option<u32>)> {
    let spec = spec.trim();
    let (width, height) = match spec.split_once(['x', 'X']) {
        Some((w, h)) => (w.trim(), Some(h.trim())),
        None => (spec, None),
    };
    let parse = |value: &str| -> Option<u32> {
        if value.is_empty() || value.len() > 5 || !value.bytes().all(|b| b.is_ascii_digit()) {
            return None;
        }
        value
            .parse::<u32>()
            .ok()
            .filter(|v| (1..=10_000).contains(v))
    };
    let width = parse(width)?;
    let height = match height {
        Some(h) => Some(parse(h)?),
        None => None,
    };
    Some((width, height))
}

/// Splits `alt|300` / `alt|300x200` into the alt text and a size, the way
/// Obsidian reads `![alt|300](pic.png)` and `![[pic.png|300]]`.
pub fn split_alt_size(alt: &str) -> (&str, Option<(u32, Option<u32>)>) {
    if let Some((text, spec)) = alt.rsplit_once('|') {
        if let Some(size) = parse_size(spec) {
            return (text.trim_end(), Some(size));
        }
    } else if let Some(size) = parse_size(alt) {
        return ("", Some(size));
    }
    (alt, None)
}

/// Byte ranges pulldown-cmark treats literally: code, math, frontmatter and
/// raw HTML. Obsidian's own syntax is never recognised inside them.
pub fn verbatim_ranges(source: &str) -> Vec<Range<usize>> {
    let mut out: Vec<Range<usize>> = Vec::new();
    let mut open: Option<usize> = None;
    let mut depth = 0usize;
    for (event, range) in Parser::new_ext(source, options()).into_offset_iter() {
        match event {
            Event::Start(Tag::CodeBlock(_) | Tag::MetadataBlock(_) | Tag::HtmlBlock) => {
                if depth == 0 {
                    open = Some(range.start);
                }
                depth += 1;
            }
            Event::End(TagEnd::CodeBlock | TagEnd::MetadataBlock(_) | TagEnd::HtmlBlock) => {
                depth = depth.saturating_sub(1);
                if depth == 0 {
                    if let Some(start) = open.take() {
                        out.push(start..range.end);
                    }
                }
            }
            Event::Code(_)
            | Event::InlineMath(_)
            | Event::DisplayMath(_)
            | Event::InlineHtml(_)
                if depth == 0 =>
            {
                out.push(range)
            }
            _ => {}
        }
    }
    normalise(out)
}

/// Sorts and merges ranges so membership tests can binary search.
pub fn normalise(mut ranges: Vec<Range<usize>>) -> Vec<Range<usize>> {
    ranges.retain(|r| r.start < r.end);
    ranges.sort_by_key(|r| (r.start, r.end));
    let mut out: Vec<Range<usize>> = Vec::with_capacity(ranges.len());
    for range in ranges {
        match out.last_mut() {
            Some(last) if range.start <= last.end => last.end = last.end.max(range.end),
            _ => out.push(range),
        }
    }
    out
}

/// Whether `pos` falls inside one of `ranges` (sorted and merged).
pub fn contains(ranges: &[Range<usize>], pos: usize) -> bool {
    let index = ranges.partition_point(|r| r.end <= pos);
    ranges.get(index).is_some_and(|r| r.start <= pos)
}

/// Whether `start..end` overlaps one of `ranges` (sorted and merged).
pub fn overlaps(ranges: &[Range<usize>], start: usize, end: usize) -> bool {
    let index = ranges.partition_point(|r| r.end <= start);
    ranges.get(index).is_some_and(|r| r.start < end)
}

/// `%%comment%%` ranges, delimiters included.
///
/// A comment may span lines and paragraphs. An opener with no closer is left
/// as text: Obsidian hides the rest of the note in that case, but a stray
/// `%%` silently swallowing everything after it reads as data loss.
pub fn comment_ranges(source: &str, verbatim: &[Range<usize>]) -> Vec<Range<usize>> {
    let bytes = source.as_bytes();
    let mut out = Vec::new();
    if !source.contains("%%") {
        return out;
    }
    let mut i = 0;
    while i + 1 < bytes.len() {
        if bytes[i] == b'%' && bytes[i + 1] == b'%' && !contains(verbatim, i) && !escaped(bytes, i)
        {
            let mut j = i + 2;
            let mut close = None;
            while j + 1 < bytes.len() {
                if bytes[j] == b'%' && bytes[j + 1] == b'%' && !contains(verbatim, j) {
                    close = Some(j);
                    break;
                }
                j += 1;
            }
            let Some(close) = close else { break };
            out.push(i..close + 2);
            i = close + 2;
            continue;
        }
        i += 1;
    }
    out
}

fn escaped(bytes: &[u8], i: usize) -> bool {
    let mut count = 0;
    let mut k = i;
    while k > 0 && bytes[k - 1] == b'\\' {
        count += 1;
        k -= 1;
    }
    count % 2 == 1
}

/// A block id at the end of a line: `text ^my-id`, or `^my-id` on its own.
pub struct BlockId {
    /// What to collapse: the id and the whitespace before it.
    pub marker: Range<usize>,
    /// The id itself, without `^`.
    pub id: Range<usize>,
}

/// Obsidian block ids (`^[A-Za-z0-9-]+` ending a line).
pub fn block_ids(source: &str, excluded: &[Range<usize>]) -> Vec<BlockId> {
    let bytes = source.as_bytes();
    let mut out = Vec::new();
    if !source.contains('^') {
        return out;
    }
    let mut line_start = 0;
    while line_start < bytes.len() {
        let mut line_end = line_start;
        while line_end < bytes.len() && bytes[line_end] != b'\n' {
            line_end += 1;
        }
        let mut end = line_end;
        while end > line_start && matches!(bytes[end - 1], b' ' | b'\t' | b'\r') {
            end -= 1;
        }
        let mut id_start = end;
        while id_start > line_start
            && (bytes[id_start - 1].is_ascii_alphanumeric() || bytes[id_start - 1] == b'-')
        {
            id_start -= 1;
        }
        if id_start < end && id_start > line_start && bytes[id_start - 1] == b'^' {
            let caret = id_start - 1;
            let mut marker_start = caret;
            while marker_start > line_start && matches!(bytes[marker_start - 1], b' ' | b'\t') {
                marker_start -= 1;
            }
            // `^id` must stand alone or follow whitespace: `x^2` is not an id,
            // and neither is the `^1` of a footnote-looking `[^1`.
            let standalone = source[line_start..caret].trim().is_empty()
                || source[line_start..caret]
                    .trim_start()
                    .starts_with(['>', '-', '*', '+'])
                    && source[line_start..caret].trim_start()[1..]
                        .trim()
                        .is_empty();
            let spaced = marker_start < caret;
            if (standalone || spaced) && !overlaps(excluded, caret, end) {
                let marker_start = if standalone { caret } else { marker_start };
                out.push(BlockId {
                    marker: marker_start..end,
                    id: id_start..end,
                });
            }
        }
        line_start = line_end + 1;
    }
    out
}

/// An inline footnote: `^[text]`.
pub struct InlineFootnote {
    pub full: Range<usize>,
    pub inner: Range<usize>,
}

/// Obsidian inline footnotes. The note may contain nested brackets but not a
/// blank line, and never starts a `^[[wikilink]]`.
pub fn inline_footnotes(source: &str, excluded: &[Range<usize>]) -> Vec<InlineFootnote> {
    let bytes = source.as_bytes();
    let mut out = Vec::new();
    if !source.contains("^[") {
        return out;
    }
    let mut i = 0;
    while i + 1 < bytes.len() {
        if bytes[i] == b'^'
            && bytes[i + 1] == b'['
            && bytes.get(i + 2) != Some(&b'[')
            && bytes.get(i + 2) != Some(&b']')
            && !escaped(bytes, i)
            && !contains(excluded, i)
        {
            let mut depth = 0usize;
            let mut j = i + 1;
            let mut close = None;
            while j < bytes.len() {
                match bytes[j] {
                    b'\\' => {
                        j += 2;
                        continue;
                    }
                    b'[' => depth += 1,
                    b']' => {
                        depth -= 1;
                        if depth == 0 {
                            close = Some(j);
                            break;
                        }
                    }
                    b'\n'
                        if bytes.get(j + 1) == Some(&b'\n')
                            || bytes.get(j + 1..j + 3) == Some(b"\r\n") =>
                    {
                        break
                    }
                    _ => {}
                }
                j += 1;
            }
            if let Some(close) = close {
                if close > i + 2 && !contains(excluded, close) {
                    out.push(InlineFootnote {
                        full: i..close + 1,
                        inner: i + 2..close,
                    });
                    i = close + 1;
                    continue;
                }
            }
        }
        i += 1;
    }
    out
}

/// A task marker pulldown-cmark does not recognise: `- [/]`, `- [-]`,
/// `- [>]`, `- [?]` and the other single-character statuses community themes
/// draw. Obsidian treats any non-space status as done.
pub struct CustomTask {
    /// `[c]`.
    pub marker: Range<usize>,
    pub status: char,
}

/// Reads a custom task status at the start of a list item.
///
/// `item_start` is where the list item's bullet begins.
pub fn custom_task_at(source: &str, item_start: usize) -> Option<CustomTask> {
    let bytes = source.as_bytes();
    let mut i = item_start;
    while i < bytes.len() && matches!(bytes[i], b' ' | b'\t') {
        i += 1;
    }
    match bytes.get(i)? {
        b'-' | b'*' | b'+' => i += 1,
        b'0'..=b'9' => {
            let digits = i;
            while i < bytes.len() && bytes[i].is_ascii_digit() && i - digits < 9 {
                i += 1;
            }
            if !matches!(bytes.get(i), Some(b'.' | b')')) {
                return None;
            }
            i += 1;
        }
        _ => return None,
    }
    let spaces = i;
    while i < bytes.len() && matches!(bytes[i], b' ' | b'\t') && i - spaces < 4 {
        i += 1;
    }
    if i == spaces || bytes.get(i) != Some(&b'[') {
        return None;
    }
    let status = source[i + 1..].chars().next()?;
    let close = i + 1 + status.len_utf8();
    if status.is_whitespace()
        || matches!(status, 'x' | 'X' | '[' | ']' | '\\')
        || bytes.get(close) != Some(&b']')
        || !matches!(bytes.get(close + 1), Some(b' ' | b'\t'))
    {
        return None;
    }
    Some(CustomTask {
        marker: i..close + 1,
        status,
    })
}

/// The header line of a callout: `> [!type]±  Title`.
pub struct CalloutLine<'a> {
    pub kind: CalloutKind,
    /// The type as written, e.g. `info` or a custom `recipe`.
    pub type_name: &'a str,
    /// `0`, [`CALLOUT_FOLD_EXPANDED`], or [`CALLOUT_FOLD_COLLAPSED`].
    pub fold: u32,
    /// `[!type]` and its fold sign.
    pub tag: Range<usize>,
    /// The authored title and its range, if the line has one.
    pub title: Option<(&'a str, Range<usize>)>,
    /// End of the header line (before its newline).
    pub line_end: usize,
}

/// Parses the first line of a blockquote as a callout header.
pub fn callout_line<'a>(source: &'a str, range: &Range<usize>) -> Option<CalloutLine<'a>> {
    let bytes = source.as_bytes();
    let end = range.end.min(bytes.len());
    let mut i = range.start.min(end);
    while i < end && (bytes[i] == b' ' || bytes[i] == b'\t') {
        i += 1;
    }
    if i < end && bytes[i] == b'>' {
        i += 1;
    }
    if i < end && (bytes[i] == b' ' || bytes[i] == b'\t') {
        i += 1;
    }
    if i + 1 >= end || bytes[i] != b'[' || bytes[i + 1] != b'!' {
        return None;
    }
    let tag_open = i;
    i += 2;
    let name_start = i;
    while i < end && (bytes[i].is_ascii_alphanumeric() || bytes[i] == b'-' || bytes[i] == b'_') {
        i += 1;
    }
    if i >= end || bytes[i] != b']' || i == name_start || i - name_start > 64 {
        return None;
    }
    let type_name = &source[name_start..i];
    let kind = CalloutKind::from_type_name(type_name);
    i += 1;
    let mut fold = 0;
    if i < end && bytes[i] == b'+' {
        fold = CALLOUT_FOLD_EXPANDED;
        i += 1;
    } else if i < end && bytes[i] == b'-' {
        fold = CALLOUT_FOLD_COLLAPSED;
        i += 1;
    }
    let tag = tag_open..i;
    let mut line_end = i;
    while line_end < end && bytes[line_end] != b'\n' && bytes[line_end] != b'\r' {
        line_end += 1;
    }
    let title_text = source[i..line_end].trim();
    let title = if title_text.is_empty() {
        None
    } else {
        Some((title_text, i..line_end))
    };
    Some(CalloutLine {
        kind,
        type_name,
        fold,
        tag,
        title,
        line_end,
    })
}

/// A readable default title for a callout type: `info` → `Info`,
/// `my-recipe` → `My recipe`.
pub fn default_callout_title(type_name: &str) -> String {
    let words = type_name.replace(['-', '_'], " ");
    let mut chars = words.chars();
    match chars.next() {
        Some(first) => first
            .to_uppercase()
            .chain(chars.map(|c| c.to_ascii_lowercase()))
            .collect(),
        None => String::new(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sizes_parse_like_obsidian() {
        assert_eq!(parse_size("300"), Some((300, None)));
        assert_eq!(parse_size("300x200"), Some((300, Some(200))));
        assert_eq!(parse_size(" 40 X 20 "), Some((40, Some(20))));
        assert_eq!(parse_size("caption"), None);
        assert_eq!(parse_size("0"), None);
        assert_eq!(parse_size("99999"), None);
        assert_eq!(split_alt_size("A cat|250"), ("A cat", Some((250, None))));
        assert_eq!(split_alt_size("250"), ("", Some((250, None))));
        assert_eq!(split_alt_size("a|b"), ("a|b", None));
    }

    #[test]
    fn media_targets_are_recognised_by_extension() {
        assert!(is_media_target("Pasted image 2024.png"));
        assert!(is_media_target("docs/Spec.PDF"));
        assert!(is_media_target("clip.mp4#t=3"));
        assert!(!is_media_target("Project Plan"));
        assert!(!is_media_target("Plan.md"));
        assert!(!is_media_target(".png"));
        assert!(is_image_target("a.svg") && !is_image_target("a.pdf"));
        assert!(is_audio_target("a.mp3") && is_video_target("a.webm"));
    }

    #[test]
    fn comments_pair_across_lines_and_skip_code() {
        let source = "a %%x%% b `%%` c %%\nmulti\n\nline%% d %%open";
        let verbatim = verbatim_ranges(source);
        let comments = comment_ranges(source, &verbatim);
        let texts: Vec<&str> = comments.iter().map(|r| &source[r.clone()]).collect();
        assert_eq!(texts, ["%%x%%", "%%\nmulti\n\nline%%"]);
    }

    #[test]
    fn block_ids_need_whitespace_or_their_own_line() {
        let source = "para ^abc-1\nx^2\n^solo\n- item ^li\n> quote ^q";
        let ids: Vec<&str> = block_ids(source, &[])
            .iter()
            .map(|b| &source[b.id.clone()])
            .collect();
        assert_eq!(ids, ["abc-1", "solo", "li", "q"]);
    }

    #[test]
    fn inline_footnotes_nest_brackets_and_stop_at_blank_lines() {
        let source = "a^[note [x] here] b^[[wiki]] c^[\n\nopen]";
        let notes: Vec<&str> = inline_footnotes(source, &[])
            .iter()
            .map(|f| &source[f.inner.clone()])
            .collect();
        assert_eq!(notes, ["note [x] here"]);
    }

    #[test]
    fn custom_task_statuses_are_read_after_the_bullet() {
        let source = "- [/] half\n1. [-] gone\n- [x] done\n- [ ] open\n- [?]no-space";
        let statuses: Vec<char> = [0, 11, 23, 34, 45]
            .iter()
            .filter_map(|&start| custom_task_at(source, start).map(|t| t.status))
            .collect();
        assert_eq!(statuses, ['/', '-']);
    }

    #[test]
    fn callout_lines_read_type_fold_and_title() {
        let source = "> [!FAQ]- Why?\n> body";
        let line = callout_line(source, &(0..source.len())).unwrap();
        assert_eq!(line.kind, CalloutKind::Question);
        assert_eq!(line.type_name, "FAQ");
        assert_eq!(line.fold, CALLOUT_FOLD_COLLAPSED);
        assert_eq!(line.title.unwrap().0, "Why?");
        let custom = callout_line("> [!my-recipe]", &(0..14)).unwrap();
        assert_eq!(custom.kind, CalloutKind::Note);
        assert_eq!(default_callout_title(custom.type_name), "My recipe");
    }
}

/// Resolves a heading path like `Setup#Install#macOS` (Obsidian's nested
/// heading link) against a note's headings, given as `(level, text)` in
/// document order. Each segment must be found inside the section of the one
/// before it. A single segment matches any heading. Comparison ignores
/// case and punctuation, so `Q3 Goals`, `q3-goals` and `Q3: Goals` agree.
pub fn heading_path_index(headings: &[(u8, &str)], anchor: &str) -> Option<usize> {
    let segments: Vec<&str> = anchor
        .split('#')
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .collect();
    if segments.is_empty() {
        return None;
    }
    let mut start = 0;
    let mut end = headings.len();
    let mut parent_level = 0u8;
    let mut found = None;
    for segment in segments {
        let wanted = heading_key(segment);
        let index = (start..end).find(|&i| {
            let (level, text) = headings[i];
            level > parent_level && heading_key(text) == wanted
        })?;
        let level = headings[index].0;
        found = Some(index);
        parent_level = level;
        start = index + 1;
        end = (start..end)
            .find(|&i| headings[i].0 <= level)
            .unwrap_or(end);
    }
    found
}

fn heading_key(text: &str) -> String {
    text.chars()
        .filter(|c| c.is_alphanumeric())
        .flat_map(char::to_lowercase)
        .collect()
}

#[cfg(test)]
mod heading_path_tests {
    use super::heading_path_index;

    #[test]
    fn nested_paths_search_inside_their_parent_section() {
        let headings = [
            (1, "Guide"),
            (2, "Setup"),
            (3, "macOS"),
            (2, "Usage"),
            (3, "macOS"),
            (3, "Q3: Goals"),
        ];
        assert_eq!(heading_path_index(&headings, "Usage#macOS"), Some(4));
        assert_eq!(heading_path_index(&headings, "Setup#macOS"), Some(2));
        assert_eq!(heading_path_index(&headings, "macOS"), Some(2));
        assert_eq!(heading_path_index(&headings, "q3-goals"), Some(5));
        assert_eq!(heading_path_index(&headings, "Setup#Q3 Goals"), None);
        assert_eq!(heading_path_index(&headings, ""), None);
    }
}
