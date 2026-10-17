//! The one shape and vocabulary of private identity in the accuracy record.
//!
//! `testdata/accuracy/record.json` is committed, and its labelled rows come
//! from corpora that are private repositories. A row there carries a hash of the
//! symbol and one evidence category, never the symbol or the labeller's prose.
//! The corpus runner writes rows in this shape and the record check in
//! `devmap-cli` validates them against the same definitions, so the writer and
//! the validator cannot drift.
//!
//! This module only judges shape. The hash itself (`h:` plus the first twelve
//! hex digits of the SHA-256 of the original id) is computed beside the corpus,
//! where the labels live, and arrives in the labels file as `symbol_hash`.

/// The evidence categories a labelled row may carry, each documented in
/// `testdata/accuracy/README.md`.
pub const EVIDENCE_CATEGORIES: &[&str] = &[
    "rg_declaration_only",
    "direct_reference",
    "framework_hook",
    "runtime_import_by_name",
    "spot_check_upstream_shadow",
    "spot_check_comment_only",
];

/// Whether `s` is exactly `h:` followed by twelve lowercase hex digits.
pub fn is_identity_hash(s: &str) -> bool {
    s.strip_prefix("h:").is_some_and(|hex| {
        hex.len() == 12 && hex.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f'))
    })
}

/// Whether `s` is one of [`EVIDENCE_CATEGORIES`].
pub fn is_evidence_category(s: &str) -> bool {
    EVIDENCE_CATEGORIES.contains(&s)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_hash_is_h_colon_and_twelve_lowercase_hex_digits() {
        assert!(is_identity_hash("h:0123456789ab"));
        assert!(is_identity_hash("h:ffffffffffff"));
    }

    #[test]
    fn anything_else_is_not_a_hash() {
        for s in [
            "",
            "h:",
            "0123456789ab",
            "h:0123456789A",
            "h:0123456789abc",
            "h:0123456789ag",
            "h:0123456789a",
            "H:0123456789ab",
            "h:0123456789ab ",
            "pkg/a.go::F",
        ] {
            assert!(!is_identity_hash(s), "{s:?}");
        }
    }

    #[test]
    fn only_the_documented_categories_are_categories() {
        for c in EVIDENCE_CATEGORIES {
            assert!(is_evidence_category(c), "{c}");
        }
        for s in ["", "rg_declaration_only ", "Rg_declaration_only", "prose here", "a/b"] {
            assert!(!is_evidence_category(s), "{s:?}");
        }
    }

    #[test]
    fn the_vocabulary_matches_the_readme() {
        let readme = include_str!("../../testdata/accuracy/README.md");
        for c in EVIDENCE_CATEGORIES {
            assert!(readme.contains(&format!("`{c}`")), "{c} is not documented");
        }
    }
}
