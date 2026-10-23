//! The one owner of "escape and bound text that came from outside, before it
//! goes into a sentence an answer carries".
//!
//! A degraded reason holds free text from the store, the analysis and the OS; a
//! `dependencies` note holds the caller's own query. Neither is a transport, and
//! neither may smuggle a newline or an escape sequence into a terminal or an
//! unbounded string into an envelope.

/// The most one component of a degraded reason may carry, in bytes of the
/// escaped text. A reason is a sentence for an operator, not a transport: the
/// components are free text from the store, the analysis and the OS.
pub const MAX_REASON_COMPONENT_BYTES: usize = 4096;

/// `text` made safe to put in a sentence: control characters (newline, ESC,
/// NUL, ...) are rendered as their visible escapes (`\n`, `\u{1b}`), and the
/// result is cut at `max_bytes` on a character boundary with a `…[N bytes]`
/// marker naming how much was cut, so a capped text is never mistaken for a
/// whole one.
pub fn bounded_text(text: &str, max_bytes: usize) -> String {
    let mut escaped = String::with_capacity(text.len().min(max_bytes + 8));
    for ch in text.chars() {
        if ch.is_control() {
            escaped.extend(ch.escape_default());
        } else {
            escaped.push(ch);
        }
    }
    if escaped.len() <= max_bytes {
        return escaped;
    }
    let mut cut = max_bytes;
    while !escaped.is_char_boundary(cut) {
        cut -= 1;
    }
    let omitted = escaped.len() - cut;
    escaped.truncate(cut);
    format!("{escaped}…[{omitted} bytes]")
}

/// A component reason made safe to put in a status sentence; see
/// [`bounded_text`] with [`MAX_REASON_COMPONENT_BYTES`].
pub fn bounded_reason(text: &str) -> String {
    bounded_text(text, MAX_REASON_COMPONENT_BYTES)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn controls_are_escaped_and_the_cut_is_named() {
        assert_eq!(bounded_text("a\nb\u{1b}", 64), "a\\nb\\u{1b}");
        let long = "é".repeat(200);
        let cut = bounded_text(&long, 256);
        assert!(cut.ends_with("bytes]") && cut.contains("…["), "{cut}");
        assert!(cut.len() < 256 + 16);
        assert_eq!(bounded_text("short", 256), "short");
    }
}
