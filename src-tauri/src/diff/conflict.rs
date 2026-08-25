use serde::{Deserialize, Serialize};

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
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum FileSegment {
    Normal(String),
    Conflict(ConflictChunk),
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ConflictDocument {
    pub file_path: String,
    pub segments: Vec<FileSegment>,
    pub total_conflicts: usize,
    /// The file used CRLF line endings; render_resolved restores them so a
    /// single conflict resolution never rewrites the whole file's EOLs.
    #[serde(default)]
    pub crlf: bool,
    /// The file ended with a newline; render_resolved preserves that.
    #[serde(default)]
    pub trailing_newline: bool,
}

pub struct ConflictResolver;

impl ConflictResolver {
    /// Parses a file containing standard Git conflict markers into structured editable chunks.
    /// Hardened against malformed, unclosed, or corrupt conflict sections.
    pub fn parse(file_path: &str, content: &str) -> ConflictDocument {
        let mut segments = Vec::new();
        let mut current_normal = Vec::new();
        let mut total_conflicts = 0;
        let crlf = content.contains("\r\n");
        let trailing_newline = content.ends_with('\n');

        let mut lines = content.lines().enumerate().peekable();
        let mut chunk_index = 0;

        while let Some((line_idx, line)) = lines.next() {
            if line.starts_with("<<<<<<<") {
                // Flush preceding normal lines
                if !current_normal.is_empty() {
                    segments.push(FileSegment::Normal(current_normal.join("\n")));
                    current_normal.clear();
                }

                let ours_label = line.trim_start_matches('<').trim().to_string();
                // Every scanned line is kept verbatim so the unclosed-marker
                // recovery below can replay the region without losing the
                // separator lines it consumed along the way.
                let mut raw_scanned = Vec::new();
                let mut ours_lines = Vec::new();
                let mut base_lines: Option<Vec<String>> = None;
                let mut theirs_lines = Vec::new();
                let mut theirs_label = String::new();
                let chunk_start = line_idx + 1;
                let mut chunk_end = chunk_start;

                let mut in_base = false;
                let mut in_theirs = false;
                let mut closed = false;
                for (inner_idx, inner_line) in lines.by_ref() {
                    chunk_end = inner_idx + 1;
                    raw_scanned.push(inner_line.to_string());
                    if inner_line.starts_with("|||||||") {
                        in_base = true;
                        base_lines = Some(Vec::new());
                    } else if inner_line.starts_with("=======") {
                        in_base = false;
                        in_theirs = true;
                    } else if inner_line.starts_with(">>>>>>>") {
                        theirs_label = inner_line.trim_start_matches('>').trim().to_string();
                        closed = true;
                        break;
                    } else if in_theirs {
                        theirs_lines.push(inner_line.to_string());
                    } else if in_base {
                        if let Some(ref mut b) = base_lines {
                            b.push(inner_line.to_string());
                        }
                    } else {
                        ours_lines.push(inner_line.to_string());
                    }
                }

                if closed {
                    total_conflicts += 1;
                    segments.push(FileSegment::Conflict(ConflictChunk {
                        chunk_index,
                        start_line: chunk_start,
                        end_line: chunk_end,
                        ours_label,
                        ours_content: ours_lines.join("\n"),
                        base_content: base_lines.map(|b| b.join("\n")),
                        theirs_label,
                        theirs_content: theirs_lines.join("\n"),
                        resolution: ConflictResolutionChoice::Unresolved,
                    }));
                    chunk_index += 1;
                } else {
                    // Unclosed marker recovery: replay every scanned line
                    // verbatim (separators included) after the opening marker
                    // so the "prevent data loss" path is a true passthrough.
                    let mut unclosed_text = vec![line.to_string()];
                    unclosed_text.append(&mut raw_scanned);
                    segments.push(FileSegment::Normal(unclosed_text.join("\n")));
                }
            } else {
                current_normal.push(line.to_string());
            }
        }

        if !current_normal.is_empty() {
            segments.push(FileSegment::Normal(current_normal.join("\n")));
        }

        ConflictDocument {
            file_path: file_path.to_string(),
            segments,
            total_conflicts,
            crlf,
            trailing_newline,
        }
    }

    /// Reassembles the resolved file based on selected chunk resolution choices.
    pub fn render_resolved(doc: &ConflictDocument) -> Result<String, &'static str> {
        let mut output = Vec::new();

        for seg in &doc.segments {
            match seg {
                FileSegment::Normal(text) => output.push(text.clone()),
                FileSegment::Conflict(chunk) => match &chunk.resolution {
                    ConflictResolutionChoice::Unresolved => {
                        return Err("Cannot render document with unresolved conflict chunks");
                    }
                    ConflictResolutionChoice::AcceptOurs => {
                        if !chunk.ours_content.is_empty() {
                            output.push(chunk.ours_content.clone());
                        }
                    }
                    ConflictResolutionChoice::AcceptTheirs => {
                        if !chunk.theirs_content.is_empty() {
                            output.push(chunk.theirs_content.clone());
                        }
                    }
                    ConflictResolutionChoice::AcceptBothOursFirst => {
                        let mut combined = chunk.ours_content.clone();
                        if !combined.is_empty() && !chunk.theirs_content.is_empty() {
                            combined.push('\n');
                        }
                        combined.push_str(&chunk.theirs_content);
                        output.push(combined);
                    }
                    ConflictResolutionChoice::AcceptBothTheirsFirst => {
                        let mut combined = chunk.theirs_content.clone();
                        if !combined.is_empty() && !chunk.ours_content.is_empty() {
                            combined.push('\n');
                        }
                        combined.push_str(&chunk.ours_content);
                        output.push(combined);
                    }
                    ConflictResolutionChoice::Custom(custom_text) => {
                        output.push(custom_text.clone());
                    }
                },
            }
        }

        let mut joined = output.join("\n");
        if doc.crlf {
            // lines() stripped \r on the way in; put CRLF back for every line
            // break, including between reassembled segments.
            joined = joined.replace("\r\n", "\n").replace('\n', "\r\n");
        }
        if doc.trailing_newline && !joined.ends_with("\r\n") && !joined.ends_with('\n') {
            joined.push_str(if doc.crlf { "\r\n" } else { "\n" });
        }
        Ok(joined)
    }
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
}
