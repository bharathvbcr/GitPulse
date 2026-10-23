//! W6-L4 adversarial review of `tests/str_tail_slices.rs` (ft-181d43a9).
//!
//! The attacks run against the guard's real scanner (`tests/common/slice_guard.rs`). Tests named
//! `held_*` pass (attack failed). Tests named `BREAK_*` are EXPECTED TO FAIL on
//! the reviewed commit: each asserts what the guard's doc comment promises
//! ("fails when a production file gains a slice of that subtractive shape")
//! and shows a shape or file it silently misses. Tests named `gap_*` assert the
//! documented gap (offset computed into a variable first) and pass.
#![allow(non_snake_case)]

mod common;
use std::path::Path;

#[path = "common/slice_guard.rs"]
mod slice_guard;

mod guard {
    use super::*;

    // The guard's own scanner, shared from tests/common (no mirror to drift).
    pub use super::slice_guard::{hits, is_test_file, scan_tree};

    mod attacks {
        use super::*;

        fn caught(src: &str) -> bool {
            !hits(src).is_empty()
        }

        #[test]
        fn held_spacing_variants_and_macros() {
            for src in [
                "&s[s.len()-3..]",
                "&s[s.len() -3..]",
                "&s[s.len()- 3..]",
                "s[..s.len()-N]",
                "s[..s.len()  -  N]",
                "let t = &s[a..s.len()-2];",
                "&name.as_str()[name.len() - 3..]",
                "&bytes.as_bytes()[bytes.as_bytes().len() - 4..]",
                r#"format!("{}", &s[s.len() - 3..])"#,
                "macro_rules! m { ($s:expr) => { &$s[$s.len() - 3..] }; }",
                "vec![&s[..s.len()-1], &t[..t.len()-1]]",
            ] {
                assert!(caught(src), "{src}");
            }
        }

        /// Documented gap: "does not see an offset computed into a variable".
        #[test]
        fn gap_offset_in_a_variable_is_documented() {
            for src in [
                "let n = s.len() - 3; &s[n..]",
                "let n = s.len() - 3;\n&s[n..]",
                "&s[..len - 4]",
            ] {
                assert!(!caught(src), "documented gap now caught? {src}");
            }
        }

        /// `get_unchecked` is not a slice-index panic and is not claimed; it is
        /// worse (UB) but is the `unsafe` policy's business. Recorded as held.
        #[test]
        fn held_get_unchecked_is_out_of_the_guards_claim() {
            assert!(!caught("unsafe { s.get_unchecked(s.len() - 3..) }"));
        }

        // ---- shapes the guard claims ("len() - N", "subtractive") but misses

        /// rustfmt wraps long indexes; the scan is line-by-line.
        #[test]
        fn BREAK_multi_line_index_is_missed() {
            let src = "let t = &text[\n    text.len() - 3..\n];";
            assert!(caught(src), "multi-line index slice not flagged: {src}");
        }

        #[test]
        fn BREAK_method_chain_split_across_lines_is_missed() {
            let src = "let t = &text[text\n    .len()\n    - 3..];";
            assert!(caught(src), "{src}");
        }

        /// Same panic, written as an inline subtraction the regex does not know.
        #[test]
        fn BREAK_saturating_and_checked_sub_inline_are_missed() {
            for src in [
                "&s[s.len().saturating_sub(3)..]",
                "&s[..s.len().saturating_sub(3)]",
                "&s[s.len().checked_sub(3).unwrap()..]",
            ] {
                assert!(caught(src), "{src}");
            }
        }

        /// `[^\[\]]*` cannot cross a nested index, so any inner `[..]` hides it.
        #[test]
        fn BREAK_nested_index_inside_the_slice_is_missed() {
            for src in [
                "&s[cuts[0]..s.len() - 1]",
                "&s[s.len() - w[0]..]",
                "&s[..s.len() - lens[i]]",
            ] {
                assert!(caught(src), "{src}");
            }
        }

        // ---- files and regions the scan silently skips

        /// The scan stops at the FIRST `#[cfg(test)]` followed by `mod`, and an
        /// out-of-line `#[cfg(test)] mod tests;` declaration at the top of a
        /// file (64 files do this) ends the production region before any
        /// production code. Everything after it is never scanned.
        #[test]
        fn BREAK_out_of_line_test_mod_declared_first_hides_all_later_code() {
            let src = "#[cfg(test)]\nmod tests;\n\nfn prod(s: &str) -> &str {\n    &s[s.len() - 3..]\n}\n";
            assert!(
                caught(src),
                "production code after `#[cfg(test)] mod tests;` is unscanned"
            );
        }

        /// Same for an inline test module that is not last.
        #[test]
        fn BREAK_production_code_after_an_inline_test_module_is_unscanned() {
            let src = "#[cfg(test)]\nmod tests { fn t() {} }\nfn prod(s: &str) -> &str { &s[s.len() - 3..] }\n";
            assert!(caught(src));
        }

        /// A production file whose name merely ends in `tests.rs` is skipped.
        #[test]
        fn BREAK_production_file_named_like_contests_is_skipped() {
            for rel in ["net/contests.rs", "a/attests.rs", "x/latests.rs"] {
                assert!(
                    !is_test_file(rel),
                    "{rel} is production code but is skipped as a test file"
                );
            }
        }

        /// The walk covers the vendored devmap/dc crates too (report mode; the
        /// verdicts live in `str_tail_slices.rs`'s `VENDORED_ALLOWED`).
        #[test]
        fn BREAK_vendored_crates_linked_into_the_binary_are_not_scanned() {
            let vendored = Path::new(env!("CARGO_MANIFEST_DIR")).join("vendored");
            let (found, walked) = scan_tree(&vendored);
            assert!(walked > 50, "walked {walked} vendored files");
            assert!(!found.is_empty(), "the vendored scan reaches its sites");
        }
    }
}
