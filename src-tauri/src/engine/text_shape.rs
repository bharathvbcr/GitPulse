//! What a text file's bytes look like beyond the characters the editor shows.
//!
//! The file editor round-trips text through a `<textarea>`, whose value turns
//! every CRLF and lone CR into LF, and the reader decodes with
//! `from_utf8_lossy`, which turns undecodable bytes into U+FFFD. Neither change
//! is visible in the text itself, so the reader reports both here and the
//! editor's write path consults the same answers before it touches the disk.

use serde::{Deserialize, Serialize};

/// How a text file ends its lines. A file with no line break is `Lf`: writing
/// it back verbatim is exact either way.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum LineEnding {
    Lf,
    Crlf,
    /// More than one style, or any lone CR. The editor cannot keep these per
    /// line, because the textarea has already normalised all of them to LF.
    Mixed,
}

pub fn line_ending(bytes: &[u8]) -> LineEnding {
    let (mut lf, mut crlf, mut lone_cr) = (0usize, 0usize, 0usize);
    let mut i = 0;
    while i < bytes.len() {
        match bytes[i] {
            b'\r' if bytes.get(i + 1) == Some(&b'\n') => {
                crlf += 1;
                i += 1;
            }
            b'\r' => lone_cr += 1,
            b'\n' => lf += 1,
            _ => {}
        }
        i += 1;
    }
    match (lf, crlf, lone_cr) {
        (_, 0, 0) => LineEnding::Lf,
        (0, _, 0) => LineEnding::Crlf,
        _ => LineEnding::Mixed,
    }
}

/// Bytes a lossy UTF-8 decode replaced with U+FFFD. Zero means the decoded
/// text is a faithful copy of the file.
pub fn invalid_utf8_bytes(bytes: &[u8]) -> usize {
    bytes.utf8_chunks().map(|chunk| chunk.invalid().len()).sum()
}

/// Encodes editor text for a file that currently holds `existing` (`None`
/// when the file does not exist yet), or refuses when the write would destroy
/// bytes the editor never showed faithfully.
pub fn encode_edited_text(
    file_path: &str,
    existing: Option<&[u8]>,
    content: &str,
) -> Result<Vec<u8>, String> {
    let Some(existing) = existing else {
        return Ok(content.as_bytes().to_vec());
    };
    let invalid = invalid_utf8_bytes(existing);
    if invalid > 0 {
        return Err(format!(
            "Refusing to save {file_path}: {invalid} byte(s) in it are not valid UTF-8 and \
             were shown as U+FFFD, so saving would replace them permanently"
        ));
    }
    match line_ending(existing) {
        LineEnding::Lf => Ok(content.as_bytes().to_vec()),
        LineEnding::Crlf => Ok(to_crlf(content).into_bytes()),
        LineEnding::Mixed => Err(format!(
            "Refusing to save {file_path}: it mixes line endings, and the editor cannot keep \
             each line's ending, so saving would rewrite every one of them"
        )),
    }
}

/// Every bare LF becomes CRLF; existing CRLF pairs are left alone.
fn to_crlf(content: &str) -> String {
    let mut out = String::with_capacity(content.len() + content.len() / 32);
    let mut previous = '\0';
    for ch in content.chars() {
        if ch == '\n' && previous != '\r' {
            out.push('\r');
        }
        out.push(ch);
        previous = ch;
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn line_ending_classifies_each_style() {
        assert_eq!(line_ending(b""), LineEnding::Lf);
        assert_eq!(line_ending(b"one line"), LineEnding::Lf);
        assert_eq!(line_ending(b"a\nb\n"), LineEnding::Lf);
        assert_eq!(line_ending(b"a\r\nb\r\n"), LineEnding::Crlf);
        assert_eq!(line_ending(b"a\r\nb\n"), LineEnding::Mixed);
        assert_eq!(
            line_ending(b"a\rb\r"),
            LineEnding::Mixed,
            "lone CR is lost by the textarea too"
        );
        assert_eq!(line_ending(b"a\r\nb\rc\r\n"), LineEnding::Mixed);
        assert_eq!(line_ending(b"a\r"), LineEnding::Mixed);
    }

    #[test]
    fn invalid_utf8_bytes_counts_every_replaced_byte() {
        assert_eq!(invalid_utf8_bytes("café".as_bytes()), 0);
        assert_eq!(invalid_utf8_bytes(b"caf\xe9 na\xefve"), 2);
        // A truncated multi-byte sequence at the end is lost too.
        assert_eq!(invalid_utf8_bytes(b"ok\xe2\x82"), 2);
    }

    #[test]
    fn encode_restores_crlf_and_is_idempotent_on_existing_pairs() {
        let out = encode_edited_text("f", Some(b"a\r\nb\r\n"), "a\nB\nc\n").unwrap();
        assert_eq!(out, b"a\r\nB\r\nc\r\n");
        let out = encode_edited_text("f", Some(b"a\r\n"), "a\r\nb\n").unwrap();
        assert_eq!(out, b"a\r\nb\r\n");
    }

    #[test]
    fn encode_writes_lf_and_new_files_verbatim() {
        assert_eq!(
            encode_edited_text("f", Some(b"a\n"), "a\nb\n").unwrap(),
            b"a\nb\n"
        );
        assert_eq!(encode_edited_text("f", None, "a\nb\n").unwrap(), b"a\nb\n");
        assert_eq!(encode_edited_text("f", None, "").unwrap(), b"");
    }

    #[test]
    fn encode_refuses_lossy_and_mixed_files_naming_the_loss() {
        let err = encode_edited_text("latin1.txt", Some(b"caf\xe9\n"), "cafe\n").unwrap_err();
        assert!(
            err.contains("latin1.txt") && err.contains("1 byte(s)"),
            "got: {err}"
        );
        assert!(err.contains("not valid UTF-8"), "got: {err}");
        let err = encode_edited_text("mixed.txt", Some(b"a\r\nb\n"), "a\nb\n").unwrap_err();
        assert!(err.contains("mixes line endings"), "got: {err}");
    }
}
