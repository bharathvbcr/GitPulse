//! No production code slices by `len() - N` on text it did not write.
//!
//! `text[text.len() - 4..]` panics when the cut lands inside a multi-byte
//! character, and the text that reaches those slices (remote URLs from
//! `.git/config`, file names in `tasks/`, `.git/HEAD`) is not ours. The fix is
//! `fs_entry::strip_suffix_ascii_ci`, which compares bytes and returns a cut
//! that is a boundary by construction. This test fails when a production file
//! gains a slice of that subtractive shape that is not listed below with the
//! reason it is safe, and when a listed entry has gone stale.
//!
//! The scanner is `tests/common/slice_guard.rs` (shared with the adversarial
//! tests, so there is one copy). It matches the shape, not the type: a
//! `Vec`/slice index of the same shape is listed too. It reads multi-line
//! indexes, nested indexes and `saturating_sub`/`checked_sub`, but not an offset
//! computed into a variable first; those sites were swept by hand against
//! `clippy::string_slice` (too many sites to allow-list), so a new such site is
//! for review.
//!
//! Vendored DevCouncil crates are scanned in report mode: they are not ours to
//! edit, so each hit is listed in `VENDORED_ALLOWED` with a verdict until
//! DevCouncil fixes it upstream.

mod common;
#[path = "common/slice_guard.rs"]
mod slice_guard;

use std::collections::BTreeMap;
use std::path::Path;

/// file (relative to `src/`) -> (matches allowed, why each is safe).
const ALLOWED: &[(&str, usize, &str)] = &[
    ("ai/prompt.rs", 1, "both ends were just checked to be the ASCII bytes b'\"' or b'\\'', so `[1..len - 1]` cuts on boundaries"),
    ("ledger/mod.rs", 1, "`serde_json::json!({..}).to_string()` always starts with `{` and ends with `}`"),
    ("engine/worktree_hooks.rs", 1, "guarded by starts_with('[') and ends_with(']'), both ASCII"),
    ("analyzer/deps.rs", 1, "cfg(windows) only: guarded by ends_with(\".exe\") on an ASCII-lowercased copy, so the tail is ASCII"),
    ("analyzer/language.rs", 1, "slice of a Vec<&str>, not a str"),
    ("workspace_registry.rs", 1, "slice of a Vec<&str>, not a str"),
    ("analyzer/cargo_supply.rs", 1, "slice of a Vec<u8> read window, not a str"),
    ("engine/git_writer.rs", 2, "both slice a Vec: `lines[lines.len().saturating_sub(20)..]` (Vec<&str>) and `prefix[1..prefix.len() - 1]` (Vec<String>)"),
    ("secrets/report.rs", 1, "slice of a Vec<&str> (`parts`), not a str"),
    ("terminal/ring.rs", 1, "slice of a `&[u8]` ring chunk, not a str"),
];

/// file (relative to `vendored/`) -> (matches, "upstream DevCouncil" verdict).
/// Vendored code is DevCouncil's and is not edited here; every site below was
/// read and none is both unguarded and reachable with untrusted input.
const VENDORED_ALLOWED: &[(&str, usize, &str)] = &[
    ("dc-glob/src/lib.rs", 1, "upstream DevCouncil: slice of a Vec of chunks (`chunks[k - 1][..len - 1]`), not a str"),
    ("dc-redact/src/lib.rs", 1, "upstream DevCouncil: guarded, `head` ends in a matched ASCII quote char so `len - 1` is a boundary"),
    ("devmap-extract/src/frameworks.rs", 2, "upstream DevCouncil: guarded, `[1..len - 1]` after the first char is an ASCII quote and ends_with(open); and a slice of a Vec<(usize, &str)>"),
    ("devmap-extract/src/languages.rs", 1, "upstream DevCouncil: slice of a Vec<&str>, not a str"),
    ("devmap-extract/src/markup.rs", 1, "upstream DevCouncil: guarded, first byte b'[' and ends_with(']')"),
    ("devmap-extract/src/rustsig.rs", 1, "upstream DevCouncil: guarded, `matching_close` returned the index of an ASCII close bracket equal to len - 1"),
    ("devmap-extract/src/wiring.rs", 1, "upstream DevCouncil: slice of a Vec<&str>, not a str"),
    ("devmap-query/src/cypher.rs", 1, "upstream DevCouncil: guarded, `ends_with(open)` so the `len - open.len_utf8()` tail is exactly one char"),
];

fn check(
    root: &Path,
    allowed: &[(&str, usize, &str)],
    min_files: usize,
    unlisted: &str,
) -> Vec<String> {
    let (found, walked) = slice_guard::scan_tree(root);
    assert!(
        walked > min_files,
        "the scan of {} found {walked} files",
        root.display()
    );
    let allowed: BTreeMap<&str, usize> = allowed.iter().map(|(f, n, _)| (*f, *n)).collect();
    let found: BTreeMap<String, Vec<usize>> = found.into_iter().collect();
    let mut problems = Vec::new();
    for (file, lines) in &found {
        let n = lines.len();
        match allowed.get(file.as_str()) {
            None => problems.push(format!("{file}:{lines:?}: {unlisted}")),
            Some(max) if n > *max => {
                problems.push(format!("{file}:{lines:?}: {n} slices, {max} allowed"))
            }
            Some(max) if n < *max => problems.push(format!(
                "{file}: only {n} slices; lower its allowance from {max}"
            )),
            Some(_) => {}
        }
    }
    for file in allowed.keys() {
        if !found.contains_key(*file) {
            problems.push(format!(
                "{file}: listed as safe but has no such slice; remove it"
            ));
        }
    }
    problems
}

#[test]
fn no_production_str_is_sliced_by_len_minus_n_unlisted() {
    let src = Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
    let problems = check(
        &src,
        ALLOWED,
        100,
        "slice cut by len() - N; use fs_entry::strip_suffix_ascii_ci or prove the cut is a boundary and list it",
    );
    assert!(problems.is_empty(), "{}", problems.join("\n"));
}

#[test]
fn vendored_devcouncil_slices_are_all_reported_with_a_verdict() {
    let vendored = Path::new(env!("CARGO_MANIFEST_DIR")).join("vendored");
    let problems = check(
        &vendored,
        VENDORED_ALLOWED,
        50,
        "new vendored slice cut by len() - N; give it a verdict (upstream DevCouncil)",
    );
    assert!(problems.is_empty(), "{}", problems.join("\n"));
}

/// The scan must be able to fail: the two shipped bugs and their mirror images
/// are flagged, safe shapes and test code are not.
#[test]
fn the_scan_flags_the_shapes_that_panicked_and_skips_test_code() {
    for bad in [
        "if path[path.len() - 4..].eq_ignore_ascii_case(\".git\") {",
        "let stem = &name[..name.len() - 3];",
        "name[name.len() - 3..].eq_ignore_ascii_case(\".md\")",
        "let t = &s[1..s.len() - 1];",
    ] {
        assert_eq!(slice_guard::hits(bad).len(), 1, "{bad}");
    }
    for fine in [
        "let head = &text[..text.floor_char_boundary(cap)];",
        "&sha[..sha.len().min(12)]",
        "// name[name.len() - 3..] is the shape this guards against",
        "let n = v.len() - 3;",
        "let m = \"s[s.len() - 3..]\";",
    ] {
        assert!(slice_guard::hits(fine).is_empty(), "{fine}");
    }
    let inline = "fn a() {}\n#[cfg(test)]\nmod tests { fn t(s: &str) { &s[s.len() - 1..]; } }\n";
    assert!(slice_guard::hits(inline).is_empty());
    let out_of_line = "#[cfg(test)]\nmod tests;\nfn p(s: &str) -> &str { &s[s.len() - 1..] }\n";
    assert_eq!(slice_guard::hits(out_of_line), [3]);
}
