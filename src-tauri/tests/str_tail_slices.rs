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
//! It matches the shape, not the type: a `Vec`/slice index of the same shape is
//! listed too. It does not see an offset computed into a variable first; those
//! sites were swept by hand against `clippy::string_slice` (147 sites crate
//! wide, too many to allow-list), so a new such site is for review.

mod common;
use regex::Regex;
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

/// file (relative to `src/`) -> (matches allowed, why each is safe).
const ALLOWED: &[(&str, usize, &str)] = &[
    ("ai/prompt.rs", 1, "both ends were just checked to be the ASCII bytes b'\"' or b'\\'', so `[1..len - 1]` cuts on boundaries"),
    ("ledger/mod.rs", 1, "`serde_json::json!({..}).to_string()` always starts with `{` and ends with `}`"),
    ("engine/worktree_hooks.rs", 1, "guarded by starts_with('[') and ends_with(']'), both ASCII"),
    ("analyzer/deps.rs", 1, "cfg(windows) only: guarded by ends_with(\".exe\") on an ASCII-lowercased copy, so the tail is ASCII"),
    ("analyzer/language.rs", 1, "slice of a Vec<&str>, not a str"),
    ("workspace_registry.rs", 1, "slice of a Vec<&str>, not a str"),
    ("analyzer/cargo_supply.rs", 1, "slice of a Vec<u8> read window, not a str"),
    ("engine/git_writer.rs", 1, "slice of a Vec<String> (`prefix[1..prefix.len() - 1].iter()`), not a str"),
];

const SHAPES: &[&str] = &[
    // `[.. x.len() - n]`, `[a..x.len() - n]`
    r"\[[^\[\]]*\.\.[^\[\]]*\.len\(\)\s*-[^\[\]]*\]",
    // `[x.len() - n..]`, `[x.len() - n..m]`
    r"\[[^\[\]]*\.len\(\)\s*-[^\[\]]*\.\.[^\[\]]*\]",
];

fn src() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("src")
}

fn rust_files(dir: &Path, out: &mut Vec<PathBuf>) {
    for entry in std::fs::read_dir(dir).expect("read src") {
        let path = entry.expect("entry").path();
        if path.is_dir() {
            rust_files(&path, out);
        } else if path.extension().is_some_and(|e| e == "rs") {
            out.push(path);
        }
    }
}

fn is_test_file(rel: &str) -> bool {
    let name = rel.rsplit('/').next().unwrap_or(rel);
    name.ends_with("tests.rs")
        || name.ends_with("_tests.rs")
        || name == "test_support.rs"
        || rel.starts_with("test_support/")
}

/// Matches in the production part of a file: everything before its first
/// top-level `#[cfg(test)]` that opens a module (files keep their test modules
/// last), minus comment lines.
fn matches_in(text: &str, shapes: &[Regex]) -> usize {
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

fn shapes() -> Vec<Regex> {
    SHAPES
        .iter()
        .map(|s| Regex::new(s).expect("shape"))
        .collect()
}

#[test]
fn no_production_str_is_sliced_by_len_minus_n_unlisted() {
    let shapes = shapes();
    let mut files = Vec::new();
    rust_files(&src(), &mut files);
    assert!(files.len() > 100, "the scan found {} files", files.len());
    let allowed: BTreeMap<&str, usize> = ALLOWED.iter().map(|(f, n, _)| (*f, *n)).collect();
    let mut found: BTreeMap<String, usize> = BTreeMap::new();
    for path in files {
        let rel = path
            .strip_prefix(src())
            .unwrap()
            .to_string_lossy()
            .replace('\\', "/");
        if is_test_file(&rel) {
            continue;
        }
        let n = matches_in(&std::fs::read_to_string(&path).expect("read"), &shapes);
        if n > 0 {
            found.insert(rel, n);
        }
    }
    let mut problems = Vec::new();
    for (file, n) in &found {
        match allowed.get(file.as_str()) {
            None => problems.push(format!(
                "{file}: {n} slice(s) cut by len() - N; use fs_entry::strip_suffix_ascii_ci or prove the cut is a boundary and list it"
            )),
            Some(max) if n > max => problems.push(format!("{file}: {n} slices, {max} allowed")),
            Some(max) if n < max => problems.push(format!(
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
    assert!(problems.is_empty(), "{}", problems.join("\n"));
}

/// The scan must be able to fail: the two shipped bugs and their mirror images
/// are flagged, safe shapes and test code are not.
#[test]
fn the_scan_flags_the_shapes_that_panicked_and_skips_test_code() {
    let shapes = shapes();
    for bad in [
        "if path[path.len() - 4..].eq_ignore_ascii_case(\".git\") {",
        "let stem = &name[..name.len() - 3];",
        "name[name.len() - 3..].eq_ignore_ascii_case(\".md\")",
        "let t = &s[1..s.len() - 1];",
    ] {
        assert_eq!(matches_in(bad, &shapes), 1, "{bad}");
    }
    for fine in [
        "let head = &text[..text.floor_char_boundary(cap)];",
        "&sha[..sha.len().min(12)]",
        "// name[name.len() - 3..] is the shape this guards against",
        "let n = v.len() - 3;",
    ] {
        assert_eq!(matches_in(fine, &shapes), 0, "{fine}");
    }
    let after_tests =
        "fn a() {}\n#[cfg(test)]\nmod tests { fn t(s: &str) { &s[s.len() - 1..]; } }\n";
    assert_eq!(matches_in(after_tests, &shapes), 0);
}
