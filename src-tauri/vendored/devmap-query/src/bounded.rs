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
        if is_agent_unsafe(ch) {
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

/// Bidirectional formatting characters (the Unicode `Bidi_Control` property:
/// marks, embeddings, overrides, isolates): not `is_control`, but able to
/// reorder the text around them on a terminal. This is the subset that
/// *validation* may reject (claude.rs); display sinks escape the wider
/// [`is_agent_unsafe`] set, so validation never narrows when the set grows.
pub fn is_bidi_control(c: char) -> bool {
    matches!(c, '\u{061c}' | '\u{200e}' | '\u{200f}' | '\u{202a}'..='\u{202e}' | '\u{2066}'..='\u{2069}')
}

/// Whether `c` is escaped wherever text from outside is shown to an agent or a
/// terminal. The one definition of the set; each sink keeps its own spelling.
///
/// - every control (Cc, tab and U+0085 included) and the line separators
///   U+2028/U+2029;
/// - [`is_bidi_control`];
/// - invisible format characters: U+200B-200D (zero-width space, ZWNJ, ZWJ),
///   U+2060, U+FEFF;
/// - the tag block U+E0000-U+E007F, which renders as nothing.
///
/// ZWNJ and ZWJ are escaped, not rejected: they are ordinary in Persian, Indic
/// text and emoji, and a display escape is visible and reversible.
pub fn is_agent_unsafe(c: char) -> bool {
    c.is_control()
        || is_bidi_control(c)
        || matches!(c, '\u{2028}' | '\u{2029}' | '\u{200b}'..='\u{200d}' | '\u{2060}' | '\u{feff}' | '\u{e0000}'..='\u{e007f}')
}

/// Right-to-left letters that no escaper may touch: Hebrew, Arabic, Persian
/// (with a ZWNJ, which is escaped, so it is not here), Devanagari and mixed
/// text. Every sink that escapes agent- or terminal-visible text asserts these
/// come through unchanged.
pub const RTL_LETTER_FIXTURES: &[&str] = &[
    "שלום עולם",
    "مرحبا بالعالم",
    "a שלום b مرحبا",
    "नमस्ते दुनिया",
    "فارسی",
];

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

    #[test]
    fn every_bidi_control_is_escaped() {
        let controls = [
            '\u{202a}', '\u{202b}', '\u{202c}', '\u{202d}', '\u{202e}', '\u{2066}', '\u{2067}',
            '\u{2068}', '\u{2069}', '\u{200e}', '\u{200f}', '\u{061c}',
        ];
        for ch in controls {
            let out = bounded_text(&format!("a{ch}b"), 64);
            assert_eq!(
                out,
                format!("a{}b", ch.escape_default()),
                "U+{:04X}",
                ch as u32
            );
            assert!(!out.contains(ch), "U+{:04X} survived", ch as u32);
        }
    }

    #[test]
    fn rtl_letters_pass_through_unchanged() {
        for text in ["שלום עולם", "مرحبا بالعالم", "a שלום b مرحبا"] {
            assert_eq!(bounded_text(text, 256), text);
        }
    }

    #[test]
    fn bidi_overrides_are_escaped() {
        assert_eq!(
            bounded_text("a\u{202e}b\u{2066}", 64),
            "a\\u{202e}b\\u{2066}"
        );
    }

    /// The escape set as ranges, written out independently of the owner
    /// function so a drift in either shows up as a difference.
    const ESCAPED_RANGES: &[(u32, u32)] = &[
        (0x0000, 0x001f),
        (0x007f, 0x009f),
        (0x061c, 0x061c),
        (0x200b, 0x200f),
        (0x2028, 0x202e),
        (0x2060, 0x2060),
        (0x2066, 0x2069),
        (0xfeff, 0xfeff),
        (0xe0000, 0xe007f),
    ];

    fn in_definition(cp: u32) -> bool {
        ESCAPED_RANGES
            .iter()
            .any(|&(lo, hi)| (lo..=hi).contains(&cp))
    }

    /// Walk every scalar value: the text changes iff the code point is in the
    /// definition, and then it is its `escape_default` spelling.
    #[test]
    fn bounded_text_escapes_exactly_the_defined_set_over_every_code_point() {
        for cp in 0..=0x10ffffu32 {
            let Some(c) = char::from_u32(cp) else {
                continue;
            };
            let text = format!("a{c}b");
            let out = bounded_text(&text, 64);
            if in_definition(cp) {
                assert_eq!(
                    out,
                    format!("a{}b", c.escape_default()),
                    "U+{cp:04X} not escaped"
                );
            } else {
                assert_eq!(out, text, "U+{cp:04X} escaped but outside the set");
            }
        }
    }

    /// The owner agrees with the independent definition over every scalar,
    /// and the validation subset is exactly the `Bidi_Control` code points.
    #[test]
    fn the_owner_matches_the_definition_over_every_code_point() {
        for cp in 0..=0x10ffffu32 {
            let Some(c) = char::from_u32(cp) else {
                continue;
            };
            assert_eq!(is_agent_unsafe(c), in_definition(cp), "U+{cp:04X}");
            let bidi = matches!(cp, 0x61c | 0x200e | 0x200f | 0x202a..=0x202e | 0x2066..=0x2069);
            assert_eq!(is_bidi_control(c), bidi, "U+{cp:04X}");
            assert!(!is_bidi_control(c) || is_agent_unsafe(c), "U+{cp:04X}");
        }
    }

    #[test]
    fn rtl_letters_and_a_persian_zwnj_word() {
        for text in RTL_LETTER_FIXTURES {
            assert_eq!(bounded_text(text, 256), *text);
        }
        assert_eq!(bounded_text("می\u{200c}خواهم", 64), "می\\u{200c}خواهم");
    }
}
