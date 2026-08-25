use serde::{Deserialize, Serialize};
use std::borrow::Cow;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum ConflictResolutionChoice {
    Unresolved,
    AcceptOurs,
    AcceptTheirs,
    AcceptBothOursFirst,
    AcceptBothTheirsFirst,
    Custom(String),
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ConflictChunk {
    pub chunk_index: usize,
    pub start_line: usize,
    pub end_line: usize,
    pub ours_label: String,
    pub ours_content: String,
    pub base_content: Option<String>, // Optional for diff3 format
    pub theirs_label: String,
    pub theirs_content: String,
    pub resolution: ConflictResolutionChoice,
    /// Per-line CRLF flags parallel to `ours_content`'s lines: true means the
    /// original physical line ended with `\r\n`. Absent/missing entries mean
    /// LF. Contents themselves stay LF-normalized so the wire format is
    /// unchanged.
    #[serde(default)]
    pub ours_crlf: Vec<bool>,
    /// Per-line CRLF flags parallel to `theirs_content`'s lines.
    #[serde(default)]
    pub theirs_crlf: Vec<bool>,
    /// Per-line CRLF flags parallel to `base_content`'s lines.
    #[serde(default)]
    pub base_crlf: Option<Vec<bool>>,
    /// The file's local EOL convention around this conflict: the terminator
    /// kind of the last line preceding the conflict, falling back to the
    /// closing marker's own terminator, then to the document-wide hint.
    /// Used when this chunk must synthesize lines (Custom insertion, preview
    /// marker blocks).
    #[serde(default)]
    pub local_crlf: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum FileSegment {
    Normal(String),
    /// Boxed: ConflictChunk dwarfs the string variant, keeping `FileSegment`
    /// cheap to move through the parser's segment buffer.
    Conflict(Box<ConflictChunk>),
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ConflictDocument {
    pub file_path: String,
    pub segments: Vec<FileSegment>,
    pub total_conflicts: usize,
    /// Hint that the file contains CRLF somewhere. Rendering no longer uses
    /// this to rewrite every line — EOLs are tracked per line instead — but
    /// the flag stays part of the serialized shape.
    #[serde(default)]
    pub crlf: bool,
    /// The file ended with a newline; render_resolved preserves that.
    #[serde(default)]
    pub trailing_newline: bool,
    /// Terminator kind (`true` = CRLF) of the file's final line. Only used
    /// to synthesize a trailing newline when every resolved line vanished.
    #[serde(default)]
    pub final_crlf: bool,
    /// Per-Normal-segment per-line CRLF flags, parallel to the `Normal`
    /// variants in `segments`: entry k holds the flags for the k-th Normal
    /// segment's lines (true = that physical line ended with `\r\n`).
    #[serde(default)]
    pub normal_crlf_flags: Vec<Vec<bool>>,
}

/// Splits a raw physical line (possibly still carrying its `\r\n` or `\n`
/// terminator) into its display content and the terminator kind it carried.
/// `None` means the line had no terminator — only legal for the very last
/// physical line of a file that does not end with a newline.
fn split_line_eol(raw: &str) -> (&str, Option<bool>) {
    if let Some(body) = raw.strip_suffix("\r\n") {
        (body, Some(true))
    } else if let Some(body) = raw.strip_suffix('\n') {
        (body, Some(false))
    } else {
        (raw, None)
    }
}

pub struct ConflictResolver;

impl ConflictResolver {
    /// Parses a file containing standard Git conflict markers into structured editable chunks.
    /// Hardened against malformed, unclosed, or corrupt conflict sections.
    ///
    /// Line endings are recorded per physical line so resolution never has to
    /// guess a document-wide convention.
    pub fn parse(file_path: &str, content: &str) -> ConflictDocument {
        let mut segments = Vec::new();
        let mut normal_crlf_flags: Vec<Vec<bool>> = Vec::new();
        let mut current_normal: Vec<String> = Vec::new();
        let mut current_normal_flags: Vec<bool> = Vec::new();
        let mut total_conflicts = 0;
        let crlf_hint = content.contains("\r\n");
        let trailing_newline = content.ends_with('\n');

        // Physical lines, each still carrying its original terminator
        // (except a final unterminated line when the file lacks one).
        let physical: Vec<&str> = content.split_inclusive('\n').collect();
        // Terminator kind of the most recently consumed physical line.
        let mut last_term: Option<bool> = None;
        let mut chunk_index = 0;

        let mut lines = physical.iter().enumerate().peekable();

        while let Some((line_idx, raw)) = lines.next() {
            let (text, _term) = split_line_eol(raw);
            if text.starts_with("<<<<<<<") {
                // The reference EOL for synthesized lines is the terminator
                // of the line immediately preceding the conflict.
                let prev_term = last_term;
                // The marker line itself becomes the most recently consumed
                // line from here on.
                last_term = _term;

                // Flush preceding normal lines
                if !current_normal.is_empty() {
                    normal_crlf_flags.push(std::mem::take(&mut current_normal_flags));
                    segments.push(FileSegment::Normal(current_normal.join("\n")));
                    current_normal.clear();
                }

                let ours_label = text.trim_start_matches('<').trim().to_string();
                // Every scanned line is kept verbatim so the unclosed-marker
                // recovery below can replay the region without losing the
                // separator lines it consumed along the way.
                let mut raw_scanned: Vec<(&str, Option<bool>)> = Vec::new();
                let mut ours_lines: Vec<&str> = Vec::new();
                let mut ours_flags: Vec<bool> = Vec::new();
                let mut base_lines: Option<(Vec<&str>, Vec<bool>)> = None;
                let mut theirs_lines: Vec<&str> = Vec::new();
                let mut theirs_flags: Vec<bool> = Vec::new();
                let theirs_label_cell = std::cell::RefCell::new(String::new());
                let mut closed_raw: Option<&&str> = None;
                let chunk_start = line_idx + 1;
                let mut chunk_end = chunk_start;

                let mut in_base = false;
                let mut in_theirs = false;
                let mut closed = false;
                for (inner_idx, inner_raw) in lines.by_ref() {
                    chunk_end = inner_idx + 1;
                    let (inner_text, inner_term) = split_line_eol(inner_raw);
                    last_term = inner_term;
                    raw_scanned.push((inner_text, inner_term));
                    if inner_text.starts_with("|||||||") {
                        in_base = true;
                        base_lines = Some((Vec::new(), Vec::new()));
                    } else if inner_text.starts_with("=======") {
                        in_base = false;
                        in_theirs = true;
                    } else if inner_text.starts_with(">>>>>>>") {
                        theirs_label_cell
                            .borrow_mut()
                            .push_str(inner_text.trim_start_matches('>').trim());
                        closed_raw = Some(inner_raw);
                        closed = true;
                        break;
                    } else if in_theirs {
                        theirs_lines.push(inner_text);
                        if let Some(term) = inner_term {
                            theirs_flags.push(term);
                        }
                    } else if in_base {
                        if let Some((b, bf)) = base_lines.as_mut() {
                            b.push(inner_text);
                            if let Some(term) = inner_term {
                                bf.push(term);
                            }
                        }
                    } else {
                        ours_lines.push(inner_text);
                        if let Some(term) = inner_term {
                            ours_flags.push(term);
                        }
                    }
                }

                if closed {
                    total_conflicts += 1;
                    let theirs_label = theirs_label_cell.into_inner();
                    // Reference EOL: preceding line's terminator, else the
                    // closing marker's own terminator, else the doc hint.
                    let local_crlf = prev_term.unwrap_or_else(|| {
                        match closed_raw.and_then(|raw| split_line_eol(raw).1) {
                            Some(term) => term,
                            None => crlf_hint,
                        }
                    });
                    let (base_content, base_crlf) = match base_lines {
                        Some((b, f)) => (Some(b.join("\n")), Some(f)),
                        None => (None, None),
                    };
                    segments.push(FileSegment::Conflict(Box::new(ConflictChunk {
                        chunk_index,
                        start_line: chunk_start,
                        end_line: chunk_end,
                        ours_label,
                        ours_content: ours_lines.join("\n"),
                        ours_crlf: ours_flags,
                        base_content,
                        base_crlf,
                        theirs_label,
                        theirs_content: theirs_lines.join("\n"),
                        theirs_crlf: theirs_flags,
                        resolution: ConflictResolutionChoice::Unresolved,
                        local_crlf,
                    })));
                    chunk_index += 1;
                } else {
                    // Unclosed marker recovery: replay every scanned line
                    // verbatim (separators included) after the opening marker
                    // so the "prevent data loss" path is a true passthrough.
                    current_normal.push(text.to_string());
                    if let Some(term) = _term {
                        current_normal_flags.push(term);
                    }
                    for (scanned_text, scanned_term) in raw_scanned {
                        current_normal.push(scanned_text.to_string());
                        if let Some(term) = scanned_term {
                            current_normal_flags.push(term);
                        }
                    }
                    segments.push(FileSegment::Normal(current_normal.join("\n")));
                    normal_crlf_flags.push(std::mem::take(&mut current_normal_flags));
                    current_normal.clear();
                }
            } else {
                current_normal.push(text.to_string());
                if let Some(term) = _term {
                    current_normal_flags.push(term);
                }
                last_term = _term;
            }
        }

        if !current_normal.is_empty() {
            segments.push(FileSegment::Normal(current_normal.join("\n")));
            normal_crlf_flags.push(current_normal_flags);
        }

        let final_crlf = if trailing_newline {
            last_term.unwrap_or(crlf_hint)
        } else {
            false
        };

        ConflictDocument {
            file_path: file_path.to_string(),
            segments,
            total_conflicts,
            crlf: crlf_hint,
            trailing_newline,
            final_crlf,
            normal_crlf_flags,
        }
    }

    /// Reassembles the resolved file. Unresolved chunks are an error — saving
    /// a half-resolved file is how conflict markers leak into commits.
    ///
    /// EOL contract: every preserved line re-emits with its original line
    /// ending; selections reuse stored lines verbatim; Custom insertions use
    /// the conflict's `local_crlf` convention. The output ends with a newline
    /// iff the original file did.
    pub fn render_resolved(doc: &ConflictDocument) -> Result<String, &'static str> {
        Self::render_document(doc, false)
    }

    /// Preview: resolved chunks become their chosen content; unresolved chunks
    /// keep standard conflict markers so the editor can show a live file
    /// without failing the render.
    pub fn render_preview(doc: &ConflictDocument) -> String {
        Self::render_document(doc, true).unwrap_or_default()
    }

    fn render_document(
        doc: &ConflictDocument,
        allow_unresolved: bool,
    ) -> Result<String, &'static str> {
        // Each emitted line paired with its own EOL convention. Every line
        // gets a terminator except possibly the final one, which is
        // terminated iff the original file ended with a newline.
        let mut lines: Vec<(Cow<'_, str>, bool)> = Vec::new();
        let mut normal_idx = 0usize;

        for seg in &doc.segments {
            match seg {
                FileSegment::Normal(text) => {
                    let flags: &[bool] = doc
                        .normal_crlf_flags
                        .get(normal_idx)
                        .map(|v| v.as_slice())
                        .unwrap_or(&[]);
                    normal_idx += 1;
                    push_lf_block(&mut lines, text, flags);
                }
                FileSegment::Conflict(chunk) => match &chunk.resolution {
                    ConflictResolutionChoice::Unresolved => {
                        if !allow_unresolved {
                            return Err("Cannot render document with unresolved conflict chunks");
                        }
                        push_unresolved_markers(&mut lines, chunk);
                    }
                    ConflictResolutionChoice::AcceptOurs => {
                        push_lf_block(&mut lines, &chunk.ours_content, &chunk.ours_crlf);
                    }
                    ConflictResolutionChoice::AcceptTheirs => {
                        push_lf_block(&mut lines, &chunk.theirs_content, &chunk.theirs_crlf);
                    }
                    ConflictResolutionChoice::AcceptBothOursFirst => {
                        push_lf_block(&mut lines, &chunk.ours_content, &chunk.ours_crlf);
                        push_lf_block(&mut lines, &chunk.theirs_content, &chunk.theirs_crlf);
                    }
                    ConflictResolutionChoice::AcceptBothTheirsFirst => {
                        push_lf_block(&mut lines, &chunk.theirs_content, &chunk.theirs_crlf);
                        push_lf_block(&mut lines, &chunk.ours_content, &chunk.ours_crlf);
                    }
                    ConflictResolutionChoice::Custom(custom_text) => {
                        for line in normalize_custom_lines(custom_text) {
                            lines.push((Cow::Owned(line), chunk.local_crlf));
                        }
                    }
                },
            }
        }

        let mut out = String::new();
        let total = lines.len();
        for (i, (text, crlf)) in lines.iter().enumerate() {
            out.push_str(text);
            if i + 1 < total || doc.trailing_newline {
                out.push_str(if *crlf { "\r\n" } else { "\n" });
            }
        }
        // Degenerate case: the file ended with a newline but every resolved
        // line vanished. Restore the newline using the original final EOL.
        if doc.trailing_newline && out.is_empty() {
            out.push_str(if doc.final_crlf { "\r\n" } else { "\n" });
        }
        Ok(out)
    }
}

/// Appends an LF-normalized block (segment or selection content) to the line
/// list, pairing each line with its recorded per-line CRLF flag. Missing flag
/// entries mean LF.
fn push_lf_block<'a>(lines: &mut Vec<(Cow<'a, str>, bool)>, content: &'a str, flags: &[bool]) {
    if content.is_empty() {
        return;
    }
    let mut parts: Vec<&str> = content.split('\n').collect();
    // Texts written before per-line EOL tracking may carry their own final
    // terminator; its phantom empty piece must not become a blank line. Real
    // trailing blank lines are backed by a flag entry, which keeps them.
    if content.ends_with('\n') && flags.len() < parts.len() {
        parts.pop();
    }
    for (i, line) in parts.iter().enumerate() {
        let crlf = flags.get(i).copied().unwrap_or(false);
        lines.push((Cow::Borrowed(line), crlf));
    }
}

/// Rebuilds the standard conflict marker block for previews, using the
/// chunk's local EOL convention for synthesized lines.
fn push_unresolved_markers<'a>(lines: &mut Vec<(Cow<'a, str>, bool)>, chunk: &'a ConflictChunk) {
    let eol = chunk.local_crlf;
    lines.push((Cow::Owned(format!("<<<<<<< {}", chunk.ours_label)), eol));
    if let Some(base) = &chunk.base_content {
        lines.push((Cow::Borrowed("||||||| base"), eol));
        let flags = chunk.base_crlf.as_deref().unwrap_or(&[]);
        push_lf_block(lines, base, flags);
    }
    push_lf_block(lines, &chunk.ours_content, &chunk.ours_crlf);
    lines.push((Cow::Borrowed("======="), eol));
    push_lf_block(lines, &chunk.theirs_content, &chunk.theirs_crlf);
    lines.push((Cow::Owned(format!(">>>>>>> {}", chunk.theirs_label)), eol));
}

/// Normalizes user-provided Custom text into logical lines without
/// terminators: `\r\n` pairs collapse, and an explicit trailing newline ends
/// the last line instead of introducing a phantom blank line. Callers attach
/// the conflict's local EOL to each returned line.
fn normalize_custom_lines(text: &str) -> Vec<String> {
    if text.is_empty() {
        return Vec::new();
    }
    let mut pieces: Vec<&str> = text.split('\n').collect();
    if text.ends_with('\n') {
        pieces.pop();
    }
    pieces
        .into_iter()
        .map(|p| p.strip_suffix('\r').unwrap_or(p).to_string())
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_unclosed_conflict_marker_safety() {
        let unclosed = "fn test() {\n<<<<<<< HEAD\n    println!(\"Ours\");\n";
        let doc = ConflictResolver::parse("broken.rs", unclosed);
        assert_eq!(doc.total_conflicts, 0);
        assert_eq!(doc.segments.len(), 2);
        if let FileSegment::Normal(ref text) = doc.segments[1] {
            assert!(text.contains("<<<<<<< HEAD"));
        }
    }

    /// Regression (audit E1): the unclosed-recovery path consumed the
    /// `|||||||` and `=======` separator lines while scanning and dropped
    /// them from the recovered text, corrupting a file it promised to protect.
    #[test]
    fn unclosed_recovery_preserves_separator_lines_verbatim() {
        let unclosed = "<<<<<<< HEAD\nours line\n||||||| merged common ancestors\nbase line\n=======\ntheirs line\n";
        let doc = ConflictResolver::parse("broken.txt", unclosed);
        assert_eq!(doc.total_conflicts, 0);
        assert_eq!(doc.segments.len(), 1);
        if let FileSegment::Normal(ref text) = doc.segments[0] {
            for expected in [
                "<<<<<<< HEAD",
                "ours line",
                "||||||| merged common ancestors",
                "base line",
                "=======",
                "theirs line",
            ] {
                assert!(
                    text.contains(expected),
                    "recovered text lost {expected:?}:\n{text}"
                );
            }
        }
        // Round-trip must be byte-identical: recovery is a passthrough.
        if let FileSegment::Normal(ref text) = doc.segments[0] {
            assert_eq!(text.trim_end_matches('\n'), unclosed.trim_end_matches('\n'));
        }
    }

    /// Regression (audit E2): resolving one conflict in a CRLF file used to
    /// rewrite every line ending to LF; the document now records the file's
    /// convention and render_resolved restores it.
    #[test]
    fn render_resolved_preserves_crlf_and_trailing_newline() {
        let crlf =
            "head\r\n<<<<<<< HEAD\r\nours\r\n=======\r\ntheirs\r\n>>>>>>> branch\r\ntail\r\n";
        let mut doc = ConflictResolver::parse("f.txt", crlf);
        assert_eq!(doc.total_conflicts, 1);
        assert!(doc.crlf, "CRLF convention must be detected");
        assert!(doc.trailing_newline, "trailing newline flag must be set");
        if let Some(FileSegment::Conflict(ref mut chunk)) = doc.segments.get_mut(1) {
            chunk.resolution = ConflictResolutionChoice::AcceptOurs;
        } else {
            panic!("second segment should be the conflict chunk");
        }
        let out = ConflictResolver::render_resolved(&doc).unwrap();
        assert_eq!(
            out, "head\r\nours\r\ntail\r\n",
            "untouched lines keep CRLF and the final newline survives"
        );
    }

    /// A file with no trailing newline must not gain one just because it was
    /// parsed and reassembled.
    #[test]
    fn render_resolved_preserves_missing_final_newline() {
        let content = "a\n<<<<<<< HEAD\nours\n=======\ntheirs\n>>>>>>> b\nc";
        let mut doc = ConflictResolver::parse("f.txt", content);
        assert!(!doc.trailing_newline);
        if let Some(FileSegment::Conflict(ref mut chunk)) = doc.segments.get_mut(1) {
            chunk.resolution = ConflictResolutionChoice::AcceptTheirs;
        }
        let out = ConflictResolver::render_resolved(&doc).unwrap();
        assert_eq!(out, "a\ntheirs\nc");
    }

    /// Regression (audit C1): one stray CRLF line in a mostly-LF file used to
    /// flip the document-wide `crlf` flag and rewrite EVERY line ending when
    /// a single conflict was resolved. Resolution must be surgical: only the
    /// replaced span changes; every preserved line keeps its own original EOL.
    #[test]
    fn mixed_eol_resolution_rewrites_only_the_conflict_span() {
        let content = "head-one\nhead-two\r\ntail\n<<<<<<< HEAD\nours line\n=======\ntheirs line\n>>>>>>> feature\nend-one\nend-two\r\n";
        let mut doc = ConflictResolver::parse("mixed.txt", content);
        assert_eq!(doc.total_conflicts, 1);
        assert!(
            doc.crlf,
            "stray CRLF must still be reported by the hint flag"
        );
        if let Some(FileSegment::Conflict(ref mut chunk)) = doc.segments.get_mut(1) {
            chunk.resolution = ConflictResolutionChoice::AcceptTheirs;
        }
        let out = ConflictResolver::render_resolved(&doc).unwrap();
        assert_eq!(
            out,
            "head-one\nhead-two\r\ntail\ntheirs line\nend-one\nend-two\r\n",
            "lines outside the conflict span must be byte-identical, including the interior and trailing CRLF terminators"
        );
    }

    /// A CRLF-everywhere file must resolve exactly as it did under the old
    /// document-wide convention: untouched lines keep CRLF and the final
    /// newline survives.
    #[test]
    fn crlf_everywhere_resolution_semantics_unchanged() {
        let content = "a\r\n<<<<<<< HEAD\r\nours\r\n=======\r\ntheirs\r\n>>>>>>> b\r\nc\r\n";
        let mut doc = ConflictResolver::parse("f.txt", content);
        assert!(doc.crlf);
        if let Some(FileSegment::Conflict(ref mut chunk)) = doc.segments.get_mut(1) {
            chunk.resolution = ConflictResolutionChoice::AcceptBothOursFirst;
        }
        let out = ConflictResolver::render_resolved(&doc).unwrap();
        assert_eq!(out, "a\r\nours\r\ntheirs\r\nc\r\n");
    }

    #[test]
    fn render_preview_keeps_unresolved_markers_and_resolved_chunks() {
        let content = "head\n<<<<<<< HEAD\nours-a\n=======\ntheirs-a\n>>>>>>> branch\nmid\n<<<<<<< HEAD\nours-b\n=======\ntheirs-b\n>>>>>>> branch\ntail\n";
        let mut doc = ConflictResolver::parse("f.txt", content);
        assert_eq!(doc.total_conflicts, 2);
        if let Some(FileSegment::Conflict(ref mut chunk)) = doc.segments.get_mut(1) {
            chunk.resolution = ConflictResolutionChoice::AcceptOurs;
        }
        assert!(ConflictResolver::render_resolved(&doc).is_err());
        let preview = ConflictResolver::render_preview(&doc);
        assert!(preview.contains("ours-a"));
        assert!(!preview.contains("theirs-a"));
        assert!(preview.contains("<<<<<<<"));
        assert!(preview.contains("theirs-b"));
        assert!(preview.contains(">>>>>>>"));
        assert!(preview.contains("head"));
        assert!(preview.contains("tail"));
    }

    /// Mixed-EOL file that does not end with a newline must come back
    /// without one, even though it contains CRLF lines.
    #[test]
    fn mixed_eol_no_trailing_newline_preserved() {
        let content = "a\nb\r\n<<<<<<< HEAD\nours\n=======\ntheirs\n>>>>>>> b\nc";
        let mut doc = ConflictResolver::parse("f.txt", content);
        assert!(!doc.trailing_newline);
        if let Some(FileSegment::Conflict(ref mut chunk)) = doc.segments.get_mut(1) {
            chunk.resolution = ConflictResolutionChoice::AcceptOurs;
        }
        let out = ConflictResolver::render_resolved(&doc).unwrap();
        assert_eq!(
            out, "a\nb\r\nours\nc",
            "no trailing newline may appear and the CRLF line stays CRLF"
        );
    }

    /// Custom insertion adopts the EOL of the last line preceding the
    /// conflict (here CRLF), while surrounding lines keep their own endings.
    #[test]
    fn custom_insertion_adopts_preceding_line_eol() {
        let content = "a\nb\r\n<<<<<<< HEAD\nours\n=======\ntheirs\n>>>>>>> b\ntail\n";
        let mut doc = ConflictResolver::parse("f.txt", content);
        if let Some(FileSegment::Conflict(ref mut chunk)) = doc.segments.get_mut(1) {
            chunk.resolution = ConflictResolutionChoice::Custom("x\ny".to_string());
        }
        let out = ConflictResolver::render_resolved(&doc).unwrap();
        assert_eq!(
            out, "a\nb\r\nx\r\ny\r\ntail\n",
            "custom lines end with the preceding line's EOL; tail keeps its own LF"
        );
    }

    /// A custom value that already ends with a newline must not gain a
    /// second one.
    #[test]
    fn custom_trailing_newline_not_doubled() {
        let content = "p\n<<<<<<< HEAD\no\n=======\nt\n>>>>>>> b\nq\n";
        let mut doc = ConflictResolver::parse("f.txt", content);
        if let Some(FileSegment::Conflict(ref mut chunk)) = doc.segments.get_mut(1) {
            chunk.resolution = ConflictResolutionChoice::Custom("z\n".to_string());
        }
        let out = ConflictResolver::render_resolved(&doc).unwrap();
        assert_eq!(out, "p\nz\nq\n");
    }

    /// parse → resolve → render → parse → render must be a fixed point for
    /// mixed-EOL content: re-rendering an already-resolved file changes
    /// nothing (no EOL drift across saves).
    #[test]
    fn round_trip_parse_render_idempotent_mixed_eol() {
        let content = "head-one\nhead-two\r\ntail\n<<<<<<< HEAD\nours line\n=======\ntheirs line\n>>>>>>> feature\nend-one\nend-two\r\n";
        let mut doc = ConflictResolver::parse("mixed.txt", content);
        if let Some(FileSegment::Conflict(ref mut chunk)) = doc.segments.get_mut(1) {
            chunk.resolution = ConflictResolutionChoice::AcceptTheirs;
        }
        let first = ConflictResolver::render_resolved(&doc).unwrap();

        let doc2 = ConflictResolver::parse("mixed.txt", &first);
        assert_eq!(doc2.total_conflicts, 0);
        let second = ConflictResolver::render_resolved(&doc2).unwrap();
        assert_eq!(second, first, "second render must be byte-identical");
    }

    /// The frontend round-trips parsed documents through JSON. A document
    /// serialized before the per-line EOL fields existed (no `crlf` flag
    /// vectors, no `local_crlf`, no `final_crlf`) must still deserialize and
    /// render — legacy payloads behave as all-LF.
    #[test]
    fn legacy_payload_without_eol_fields_still_round_trips() {
        let legacy = serde_json::json!({
            "file_path": "f.txt",
            "segments": [
                { "Normal": "head\n" },
                {
                    "Conflict": {
                        "chunk_index": 0,
                        "start_line": 2,
                        "end_line": 5,
                        "ours_label": "HEAD",
                        "ours_content": "ours",
                        "base_content": null,
                        "theirs_label": "b",
                        "theirs_content": "theirs",
                        "resolution": "AcceptOurs"
                    }
                },
                { "Normal": "tail" }
            ],
            "total_conflicts": 1,
            "crlf": false,
            "trailing_newline": true
        });
        let doc: ConflictDocument =
            serde_json::from_value(legacy).expect("legacy payload must deserialize");
        assert_eq!(doc.total_conflicts, 1);
        let out = ConflictResolver::render_resolved(&doc).unwrap();
        assert_eq!(
            out, "head\nours\ntail\n",
            "all-LF legacy semantics preserved"
        );
    }
}
