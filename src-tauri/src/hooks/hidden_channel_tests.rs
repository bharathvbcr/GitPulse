//! Adversarial: channels in `hooks` that put untrusted text in front of a
//! reader without going through `alerts::text`, the one owner.

#[test]
fn session_brief_lines_drop_hidden_characters_like_the_one_owner() {
    for text in ["evil\u{202e}txt.exe", "a\u{200b}b", "\u{e000}x", "a\u{2066}b\u{2069}c"] {
        assert_eq!(
            super::one_bounded_line(text, 240),
            crate::alerts::text::sanitize(text),
            "{text:?}"
        );
    }
}
