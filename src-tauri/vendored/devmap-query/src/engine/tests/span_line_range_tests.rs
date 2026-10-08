use super::byte_span_to_line_range;
use devmap_extract::model::Span;

/// Multi-byte source must not abort the process.
///
/// This function counted newlines with `source[..start]` — slicing a `&str`
/// at an index that is not a character boundary, which panics. Spans are
/// byte offsets recorded at extraction time while the source is re-read
/// from disk when the graph is exported, so an offset lands mid-character
/// whenever a multi-byte character was inserted before it. One emoji added
/// to a file aborted `dev map manifest`, and the release profile is
/// `panic = "abort"`, so nothing recovered.
///
/// Exhaustive over every offset pair rather than sampled: the failure is
/// per-offset, and testing only the boundaries would pass against exactly
/// the code that panicked, because boundaries were always the safe case.
#[test]
fn every_offset_into_multibyte_source_is_answered_rather_than_panicked_on() {
    let source = "fn a() {}\n// \u{1F980} ferris r\u{e9}\nfn b() {}\n";
    for start in 0..=source.len() {
        for end in 0..=source.len() {
            let span = Span {
                start_byte: start,
                end_byte: end,
            };
            let (first, last) = byte_span_to_line_range(source, &span);
            assert!(first >= 1, "lines are one-based, got {first}");
            assert!(
                last >= first,
                "end line {last} precedes start line {first} for {start}..{end}"
            );
        }
    }
}

/// The line numbers must be right, not merely non-panicking.
///
/// A fix that clamped every offset to zero would satisfy the test above.
#[test]
fn line_numbers_are_correct_across_a_multibyte_character() {
    let source = "alpha\n\u{1F980}beta\ngamma\n";
    let crab = source
        .find('\u{1F980}')
        .expect("fixture contains the emoji");

    let at_emoji = Span {
        start_byte: crab,
        end_byte: crab,
    };
    assert_eq!(byte_span_to_line_range(source, &at_emoji), (2, 2));

    // Strictly inside the four-byte emoji — the exact index that panicked.
    let inside = Span {
        start_byte: crab + 1,
        end_byte: crab + 2,
    };
    assert_eq!(byte_span_to_line_range(source, &inside), (2, 2));

    let whole = Span {
        start_byte: 0,
        end_byte: source.len(),
    };
    assert_eq!(byte_span_to_line_range(source, &whole), (1, 4));
}

/// A stored span outliving the file it points into is the everyday case
/// after an edit, not a hostile one.
#[test]
fn offsets_beyond_the_source_are_clamped() {
    let source = "one\ntwo\n";
    let span = Span {
        start_byte: 10_000,
        end_byte: 20_000,
    };
    assert_eq!(byte_span_to_line_range(source, &span), (3, 3));
}
