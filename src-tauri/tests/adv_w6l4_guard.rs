//! W6-L4 adversarial review of `tests/str_tail_slices.rs` (ft-181d43a9).
//!
//! The guard's own source is included, unmodified, so the attacks run against
//! its real `matches_in`, `SHAPES` and `is_test_file`. Tests named
//! `held_*` pass (attack failed). Tests named `BREAK_*` are EXPECTED TO FAIL on
//! the reviewed commit: each asserts what the guard's doc comment promises
//! ("fails when a production file gains a slice of that subtractive shape")
//! and shows a shape or file it silently misses. Tests named `gap_*` assert the
//! documented gap (offset computed into a variable first) and pass.
#![allow(non_snake_case)]

use regex::Regex;
use std::path::{Path, PathBuf};

mod guard {
    use super::*;

    // Mirror of the guard's `SHAPES`, `matches_in`, `is_test_file` and
    // `rust_files` (include! cannot be used: the guard starts with inner doc
    // comments). `the_mirror_matches_the_guard_source` pins that the guard's
    // own text still contains each load-bearing line, so drift fails loudly.
    pub const SHAPES: &[&str] = &[
        r"\[[^\[\]]*\.\.[^\[\]]*\.len\(\)\s*-[^\[\]]*\]",
        r"\[[^\[\]]*\.len\(\)\s*-[^\[\]]*\.\.[^\[\]]*\]",
    ];

    pub fn shapes() -> Vec<Regex> {
        SHAPES.iter().map(|s| Regex::new(s).unwrap()).collect()
    }

    pub fn rust_files(dir: &Path, out: &mut Vec<PathBuf>) {
        for entry in std::fs::read_dir(dir).unwrap() {
            let path = entry.unwrap().path();
            if path.is_dir() {
                rust_files(&path, out);
            } else if path.extension().is_some_and(|e| e == "rs") {
                out.push(path);
            }
        }
    }

    pub fn is_test_file(rel: &str) -> bool {
        let name = rel.rsplit('/').next().unwrap_or(rel);
        name.ends_with("tests.rs")
            || name.ends_with("_tests.rs")
            || name == "test_support.rs"
            || rel.starts_with("test_support/")
    }

    pub fn matches_in(text: &str, shapes: &[Regex]) -> usize {
        let mut count = 0;
        let lines: Vec<&str> = text.lines().collect();
        for (at, line) in lines.iter().enumerate() {
            let opens_module = |next: Option<&&str>| {
                next.is_some_and(|n| {
                    let n = n
                        .trim_start_matches("pub(crate) ")
                        .trim_start_matches("pub ");
                    n.starts_with("mod ")
                })
            };
            if line.starts_with("#[cfg(test)]") && opens_module(lines.get(at + 1)) {
                break;
            }
            if line.trim_start().starts_with("//") {
                continue;
            }
            if shapes.iter().any(|shape| shape.is_match(line)) {
                count += 1;
            }
        }
        count
    }

    #[test]
    fn the_mirror_matches_the_guard_source() {
        let guard = include_str!("str_tail_slices.rs");
        for must in [
            SHAPES[0],
            SHAPES[1],
            "if line.starts_with(\"#[cfg(test)]\") && opens_module(lines.get(at + 1)) {",
            "break;",
            "name.ends_with(\"tests.rs\")",
            "if line.trim_start().starts_with(\"//\") {",
            "let lines: Vec<&str> = text.lines().collect();",
            "rust_files(&src(), &mut files);",
        ] {
            assert!(
                guard.contains(must),
                "guard changed; refresh the mirror: {must}"
            );
        }
    }

    mod attacks {
        use super::*;

        fn caught(src: &str) -> bool {
            matches_in(src, &shapes()) > 0
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

        /// The walk covers `src-tauri/src` only. The vendored devmap/dc crates
        /// are linked into the shipped binary and hold unlisted tail slices.
        #[test]
        fn BREAK_vendored_crates_linked_into_the_binary_are_not_scanned() {
            let vendored = Path::new(env!("CARGO_MANIFEST_DIR")).join("vendored");
            let mut files = Vec::new();
            rust_files(&vendored, &mut files);
            let shapes = shapes();
            let mut hits = Vec::new();
            for path in files {
                let rel = path
                    .strip_prefix(&vendored)
                    .unwrap()
                    .to_string_lossy()
                    .replace('\\', "/");
                if rel.contains("/tests/") || is_test_file(&rel) {
                    continue;
                }
                let n = matches_in(&std::fs::read_to_string(&path).unwrap(), &shapes);
                if n > 0 {
                    hits.push(format!("{rel}: {n}"));
                }
            }
            assert!(hits.is_empty(), "unscanned vendored tail slices: {hits:?}");
        }
    }
}
