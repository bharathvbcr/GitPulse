use super::workspace::specifier_matches;

/// A module prefix matches its own path and anything beneath it — and
/// nothing that merely starts with the same letters. `manvibench` sharing a
/// prefix with `manvi` is not an import of it, and a bare `starts_with`
/// would claim it is.
#[test]
fn a_prefix_matches_only_on_a_segment_boundary() {
    assert!(specifier_matches("example.com/libb", "example.com/libb"));
    assert!(specifier_matches(
        "example.com/libb/store",
        "example.com/libb"
    ));
    assert!(specifier_matches("devcouncil.app.config", "devcouncil"));

    assert!(!specifier_matches(
        "example.com/libbeta",
        "example.com/libb"
    ));
    assert!(!specifier_matches("manvibench", "manvi"));
    assert!(!specifier_matches("libb", "example.com/libb"));
}
