//! Per-note metadata: title, headings, wikilinks, tags, aliases.

use pulldown_cmark::{Event, LinkType, MetadataBlockKind, Parser, Tag, TagEnd};
use serde::{Deserialize, Serialize};

use crate::md::parse::{options, scan_tags};

/// Markdown extensions the vault treats as notes — same set as the scanner.
pub const MARKDOWN_EXTENSIONS: &[&str] = &["md", "markdown", "mdown", "mdx", "mkd"];

/// Whether a path ends in a vault note extension.
pub fn has_markdown_extension(path: &str) -> bool {
    path.rsplit_once('.')
        .map(|(_, ext)| {
            MARKDOWN_EXTENSIONS
                .iter()
                .any(|known| ext.eq_ignore_ascii_case(known))
        })
        .unwrap_or(false)
}

/// Strips one trailing markdown extension, case-insensitively.
pub fn strip_markdown_extension(path: &str) -> &str {
    match path.rsplit_once('.') {
        Some((head, ext))
            if !head.is_empty()
                && MARKDOWN_EXTENSIONS
                    .iter()
                    .any(|known| ext.eq_ignore_ascii_case(known)) =>
        {
            head
        }
        _ => path,
    }
}

/// How a vault link was written.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum NoteLinkKind {
    #[default]
    Wiki,
    Markdown,
}

/// A heading, for the outline.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Heading {
    pub level: u8,
    pub text: String,
    /// UTF-16 offset of the heading in the note, so the editor can scroll to
    /// it without recomputing anything.
    pub offset: u32,
    pub line: u32,
}

/// A `[[wikilink]]` or local Markdown link occurrence.
///
/// Named for the wiki half of the vault graph; Markdown destinations that
/// point at notes share the same shape and carry [`NoteLinkKind::Markdown`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WikiLink {
    /// The target as written, without the `#anchor` or `|alias`.
    pub target: String,
    /// Heading anchor, when the link was `[[Note#Heading]]` or `[x](Note.md#Heading)`.
    pub anchor: Option<String>,
    /// What the reader sees — the alias when there is one.
    pub display: String,
    pub offset: u32,
    pub line: u32,
    /// Wiki vs ordinary Markdown — resolution rules differ.
    #[serde(default)]
    pub kind: NoteLinkKind,
}

/// Everything the vault knows about one note.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Note {
    /// Vault-relative path with forward slashes, e.g. `Projects/Roadmap.md`.
    pub path: String,
    /// First H1 if there is one, else the file name without its extension.
    pub title: String,
    pub headings: Vec<Heading>,
    pub links: Vec<WikiLink>,
    pub tags: Vec<String>,
    /// `aliases:` from frontmatter, so `[[Other Name]]` can resolve here.
    pub aliases: Vec<String>,
    /// The note's text.
    ///
    /// Kept in memory because unlinked mentions and search snippets both need
    /// to re-read it, and a personal vault is small enough that re-reading
    /// from disk per query would cost more than the memory does.
    pub text: String,
}

impl Note {
    /// Extracts metadata from a note's source.
    ///
    /// Walks the event stream directly rather than going through the editor's
    /// flat model: that model reports UTF-16 offsets for text styling, while
    /// indexing wants byte offsets and string contents. Converting between
    /// them would be work in service of nothing.
    pub fn parse(path: impl Into<String>, source: &str) -> Note {
        let path = path.into();
        let lines = LineTable::new(source);

        let mut headings: Vec<Heading> = Vec::new();
        let mut links: Vec<WikiLink> = Vec::new();
        let mut tags: Vec<String> = Vec::new();
        let mut aliases: Vec<String> = Vec::new();

        let mut heading_level: Option<(u8, usize)> = None;
        let mut heading_text = String::new();
        let mut link_open: Option<(String, usize, NoteLinkKind)> = None;
        let mut link_text = String::new();
        let mut in_frontmatter = false;
        let mut frontmatter = String::new();
        // Code and raw HTML are literal: a `#` there is not a tag.
        let mut verbatim = 0usize;

        for (event, range) in Parser::new_ext(source, options()).into_offset_iter() {
            match event {
                Event::Start(Tag::Heading { level, .. }) => {
                    heading_level = Some((level as u8, range.start));
                    heading_text.clear();
                }
                Event::End(TagEnd::Heading(_)) => {
                    if let Some((level, start)) = heading_level.take() {
                        let text = heading_text.trim().to_string();
                        if !text.is_empty() {
                            headings.push(Heading {
                                level,
                                text,
                                offset: utf16_offset(source, start),
                                line: lines.line(start),
                            });
                        }
                    }
                }

                Event::Start(Tag::Link {
                    link_type,
                    dest_url,
                    ..
                }) => {
                    let kind = match link_type {
                        LinkType::WikiLink { .. } => Some(NoteLinkKind::Wiki),
                        LinkType::Inline
                        | LinkType::Reference
                        | LinkType::Collapsed
                        | LinkType::Shortcut => Some(NoteLinkKind::Markdown),
                        // Autolink / email / unknown are not vault note links.
                        _ => None,
                    };
                    if let Some(kind) = kind {
                        link_open = Some((dest_url.to_string(), range.start, kind));
                        link_text.clear();
                    }
                }
                Event::End(TagEnd::Link) => {
                    if let Some((dest, start, kind)) = link_open.take() {
                        let Some(link) =
                            indexed_link(dest, &link_text, kind, source, start, &lines)
                        else {
                            continue;
                        };
                        links.push(link);
                    }
                }

                Event::Start(Tag::MetadataBlock(kind)) => {
                    in_frontmatter = matches!(
                        kind,
                        MetadataBlockKind::YamlStyle | MetadataBlockKind::PlusesStyle
                    );
                    frontmatter.clear();
                }
                Event::End(TagEnd::MetadataBlock(_)) => {
                    if in_frontmatter {
                        let (front_tags, front_aliases) = parse_frontmatter(&frontmatter);
                        tags.extend(front_tags);
                        aliases.extend(front_aliases);
                    }
                    in_frontmatter = false;
                }

                Event::Start(Tag::CodeBlock(_)) | Event::Start(Tag::HtmlBlock) => verbatim += 1,
                Event::End(TagEnd::CodeBlock) | Event::End(TagEnd::HtmlBlock) => {
                    verbatim = verbatim.saturating_sub(1)
                }

                Event::Text(text) => {
                    if in_frontmatter {
                        frontmatter.push_str(&text);
                    } else if verbatim == 0 {
                        if heading_level.is_some() {
                            heading_text.push_str(&text);
                        }
                        if link_open.is_some() {
                            link_text.push_str(&text);
                        }
                        for span in scan_tags(&text) {
                            // Stored without the leading `#`, which is
                            // presentation rather than identity.
                            tags.push(text[span.start + 1..span.end].to_string());
                        }
                    }
                }
                Event::Code(code) => {
                    if heading_level.is_some() {
                        heading_text.push_str(&code);
                    }
                    if link_open.is_some() {
                        link_text.push_str(&code);
                    }
                }
                _ => {}
            }
        }

        tags.sort();
        tags.dedup();

        let title = headings
            .iter()
            .find(|heading| heading.level == 1)
            .map(|heading| heading.text.clone())
            .unwrap_or_else(|| stem(&path));

        Note {
            path,
            title,
            headings,
            links,
            tags,
            aliases,
            text: source.to_string(),
        }
    }

    /// Names this note answers to when resolving a `[[wikilink]]`.
    pub fn names(&self) -> Vec<String> {
        let mut names = vec![stem(&self.path), self.title.clone()];
        names.extend(self.aliases.iter().cloned());
        names.retain(|name| !name.is_empty());
        names.sort();
        names.dedup();
        names
    }
}

/// File name without directories or extension.
pub fn stem(path: &str) -> String {
    let file = path.rsplit('/').next().unwrap_or(path);
    match file.rsplit_once('.') {
        Some((name, _)) if !name.is_empty() => name.to_string(),
        _ => file.to_string(),
    }
}

/// Builds a vault link row, or refuses destinations that are not notes.
fn indexed_link(
    dest: String,
    link_text: &str,
    kind: NoteLinkKind,
    source: &str,
    start: usize,
    lines: &LineTable,
) -> Option<WikiLink> {
    match kind {
        NoteLinkKind::Wiki => {
            // `dest` carries the anchor but never the alias.
            let (target, anchor) = split_target_anchor(&dest);
            let display = if link_text.is_empty() {
                target.clone()
            } else {
                link_text.to_string()
            };
            Some(WikiLink {
                target: target.trim().to_string(),
                anchor,
                display,
                offset: utf16_offset(source, start),
                line: lines.line(start),
                kind: NoteLinkKind::Wiki,
            })
        }
        NoteLinkKind::Markdown => {
            let decoded = percent_decode_once(&dest);
            let (target, anchor) = split_target_anchor(&decoded);
            let target = target.trim();
            if !is_indexable_markdown_destination(target) {
                return None;
            }
            let display = if link_text.is_empty() {
                target.to_string()
            } else {
                link_text.to_string()
            };
            Some(WikiLink {
                target: target.to_string(),
                anchor,
                display,
                offset: utf16_offset(source, start),
                line: lines.line(start),
                kind: NoteLinkKind::Markdown,
            })
        }
    }
}

fn split_target_anchor(dest: &str) -> (String, Option<String>) {
    match dest.split_once('#') {
        Some((target, anchor)) => (target.to_string(), Some(anchor.to_string())),
        None => (dest.to_string(), None),
    }
}

/// Destinations that become vault graph edges — notes only, never LICENSE or folders.
fn is_indexable_markdown_destination(target: &str) -> bool {
    if target.is_empty() || target.starts_with('#') || target.starts_with("//") {
        return false;
    }
    if has_url_scheme(target) {
        return false;
    }
    has_markdown_extension(target)
}

/// `http:`, `mailto:`, `file:` — a colon after an ASCII scheme name.
pub(crate) fn has_url_scheme(target: &str) -> bool {
    let Some(colon) = target.find(':') else {
        return false;
    };
    let scheme = &target[..colon];
    !scheme.is_empty() && scheme.bytes().all(|b| b.is_ascii_alphabetic())
}

/// Percent-decode once. Invalid `%` sequences stay literal.
pub(crate) fn percent_decode_once(input: &str) -> String {
    let bytes = input.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut index = 0;
    while index < bytes.len() {
        if bytes[index] == b'%' && index + 2 < bytes.len() {
            if let (Some(high), Some(low)) =
                (hex_nibble(bytes[index + 1]), hex_nibble(bytes[index + 2]))
            {
                out.push((high << 4) | low);
                index += 3;
                continue;
            }
        }
        out.push(bytes[index]);
        index += 1;
    }
    match String::from_utf8(out) {
        Ok(text) => text,
        Err(error) => String::from_utf8_lossy(error.as_bytes()).into_owned(),
    }
}

fn hex_nibble(byte: u8) -> Option<u8> {
    match byte {
        b'0'..=b'9' => Some(byte - b'0'),
        b'a'..=b'f' => Some(byte - b'a' + 10),
        b'A'..=b'F' => Some(byte - b'A' + 10),
        _ => None,
    }
}

/// UTF-16 offset of a byte offset.
pub(super) fn utf16_offset(source: &str, byte: usize) -> u32 {
    source[..byte.min(source.len())].encode_utf16().count() as u32
}

/// Maps byte offsets to 1-based line numbers.
struct LineTable {
    starts: Vec<usize>,
}

impl LineTable {
    fn new(source: &str) -> Self {
        let mut starts = vec![0usize];
        for (index, byte) in source.bytes().enumerate() {
            if byte == b'\n' {
                starts.push(index + 1);
            }
        }
        Self { starts }
    }

    fn line(&self, byte: usize) -> u32 {
        match self.starts.binary_search(&byte) {
            Ok(index) => index as u32 + 1,
            Err(index) => index as u32,
        }
    }
}

/// Reads `tags:` and `aliases:` from a frontmatter block.
///
/// A deliberately small YAML subset — inline `[a, b]` lists and `- item`
/// blocks — rather than a YAML dependency. Anything more elaborate is
/// ignored rather than guessed at, so a document with unusual frontmatter
/// still indexes instead of failing.
fn parse_frontmatter(text: &str) -> (Vec<String>, Vec<String>) {
    let mut tags = Vec::new();
    let mut aliases = Vec::new();
    let mut current: Option<&mut Vec<String>> = None;

    for line in text.lines() {
        let trimmed = line.trim();

        if let Some(rest) = trimmed.strip_prefix("- ") {
            if let Some(list) = current.as_deref_mut() {
                let value = clean_scalar(rest);
                if !value.is_empty() {
                    list.push(value);
                }
            }
            continue;
        }

        let Some((key, value)) = trimmed.split_once(':') else {
            current = None;
            continue;
        };

        let key = key.trim().to_ascii_lowercase();
        let value = value.trim();
        let target = match key.as_str() {
            "tags" | "tag" => Some(&mut tags),
            "aliases" | "alias" => Some(&mut aliases),
            _ => None,
        };

        let Some(target) = target else {
            current = None;
            continue;
        };

        if value.is_empty() {
            // A block list follows on the next lines.
            current = Some(target);
        } else {
            let inline = value.trim_start_matches('[').trim_end_matches(']');
            for item in inline.split(',') {
                let value = clean_scalar(item);
                if !value.is_empty() {
                    target.push(value);
                }
            }
            current = None;
        }
    }

    (tags, aliases)
}

fn clean_scalar(value: &str) -> String {
    value
        .trim()
        .trim_matches('"')
        .trim_matches('\'')
        .trim_start_matches('#')
        .trim()
        .to_string()
}
