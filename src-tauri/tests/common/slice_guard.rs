//! The scanner behind `tests/str_tail_slices.rs`: finds index expressions that
//! cut a value by `len() - N`, the shape that panics on a multi-byte character.
//!
//! It works on a cleaned copy of the source in which comments and string/char
//! literal contents are blanked and the bodies of inline `#[cfg(test)] mod x {}`
//! are removed (an out-of-line `#[cfg(test)] mod x;` removes nothing, so the
//! production code after it is still scanned). Blanking keeps every newline, so
//! reported line numbers are the file's. Each index expression `[` .. `]` is
//! then read across lines, with nested indexes collapsed to `[]`, and matched.
#![allow(dead_code)]

use regex::Regex;
use std::path::{Path, PathBuf};

/// `..` somewhere in the index, and a `.len()` that is subtracted from, either
/// inline (`- n`) or as `saturating_sub(n)` / `checked_sub(n)`.
fn subtractive_len() -> Regex {
    Regex::new(r"\.len\(\)\s*(?:-|\.\s*(?:saturating_sub|checked_sub)\s*\()").expect("shape")
}

pub fn rust_files(dir: &Path, out: &mut Vec<PathBuf>) {
    for entry in std::fs::read_dir(dir).expect("read dir") {
        let path = entry.expect("entry").path();
        if path.is_dir() {
            rust_files(&path, out);
        } else if path.extension().is_some_and(|e| e == "rs") {
            out.push(path);
        }
    }
}

/// Exactly `tests.rs`, `*_tests.rs` and `test_support*` (file or directory):
/// a production file that merely ends in "tests.rs" (`contests.rs`) is scanned.
pub fn is_test_file(rel: &str) -> bool {
    let name = rel.rsplit('/').next().unwrap_or(rel);
    name == "tests.rs"
        || name.ends_with("_tests.rs")
        || name.starts_with("test_support")
        || rel.split('/').any(|part| part.starts_with("test_support"))
}

/// `text` with comments and literal contents replaced by spaces (newlines
/// kept), so braces and brackets left are code.
fn blank_non_code(text: &str) -> Vec<char> {
    let c: Vec<char> = text.chars().collect();
    let mut out = c.clone();
    let blank = |out: &mut Vec<char>, from: usize, to: usize| {
        for slot in out.iter_mut().take(to).skip(from) {
            if *slot != '\n' {
                *slot = ' ';
            }
        }
    };
    let mut i = 0;
    while i < c.len() {
        match c[i] {
            '/' if c.get(i + 1) == Some(&'/') => {
                let end = (i..c.len()).find(|&j| c[j] == '\n').unwrap_or(c.len());
                blank(&mut out, i, end);
                i = end;
            }
            '/' if c.get(i + 1) == Some(&'*') => {
                let mut depth = 1;
                let mut j = i + 2;
                while j < c.len() && depth > 0 {
                    if c[j] == '/' && c.get(j + 1) == Some(&'*') {
                        depth += 1;
                        j += 2;
                    } else if c[j] == '*' && c.get(j + 1) == Some(&'/') {
                        depth -= 1;
                        j += 2;
                    } else {
                        j += 1;
                    }
                }
                blank(&mut out, i, j);
                i = j;
            }
            'r' | 'b' if raw_string_open(&c, i).is_some() => {
                let (body, hashes) = raw_string_open(&c, i).expect("checked");
                let mut j = body;
                while j < c.len() {
                    if c[j] == '"' && (0..hashes).all(|k| c.get(j + 1 + k) == Some(&'#')) {
                        break;
                    }
                    j += 1;
                }
                blank(&mut out, body, j);
                i = (j + 1 + hashes).min(c.len());
            }
            '"' => {
                let mut j = i + 1;
                while j < c.len() && c[j] != '"' {
                    j += if c[j] == '\\' { 2 } else { 1 };
                }
                blank(&mut out, i + 1, j.min(c.len()));
                i = j + 1;
            }
            '\'' => {
                // A char literal (`'['`, `'\n'`) or a lifetime (`'a`).
                if c.get(i + 1) == Some(&'\\') {
                    let end = (i + 2..c.len()).find(|&j| c[j] == '\'').unwrap_or(c.len());
                    blank(&mut out, i + 1, end);
                    i = end + 1;
                } else if c.get(i + 2) == Some(&'\'') {
                    blank(&mut out, i + 1, i + 2);
                    i += 3;
                } else {
                    i += 1;
                }
            }
            _ => i += 1,
        }
    }
    out
}

/// At `i`, the start of a raw string's body and its `#` count, when one opens
/// there (`r"`, `r#"`, `br##"`) and the `r` is not the tail of an identifier.
fn raw_string_open(c: &[char], i: usize) -> Option<(usize, usize)> {
    if i > 0 && (c[i - 1].is_alphanumeric() || c[i - 1] == '_') {
        return None;
    }
    let mut j = i;
    if c[j] == 'b' {
        j += 1;
    }
    if c.get(j) != Some(&'r') {
        return None;
    }
    j += 1;
    let hashes = (j..c.len()).take_while(|&k| c[k] == '#').count();
    (c.get(j + hashes) == Some(&'"')).then_some((j + hashes + 1, hashes))
}

/// Matching close for the open bracket at `open`, over `[]`/`()`/`{}` of one kind.
fn matching(c: &[char], open: usize, lo: char, hi: char) -> Option<usize> {
    let mut depth = 0usize;
    for (j, &ch) in c.iter().enumerate().skip(open) {
        if ch == lo {
            depth += 1;
        } else if ch == hi {
            depth -= 1;
            if depth == 0 {
                return Some(j);
            }
        }
    }
    None
}

/// Removes every inline `#[cfg(test)] ... mod x { ... }` body.
fn drop_inline_test_modules(c: &mut [char]) {
    let head = Regex::new(
        r"#\[cfg\(test\)\]\s*(?:#\[[^\]]*\]\s*)*(?:pub(?:\([^)]*\))?\s+)?mod\s+\w+\s*\{",
    )
    .expect("head");
    loop {
        let text: String = c.iter().collect();
        let Some(m) = head.find_iter(&text).find(|m| {
            let start = text[..m.start()].chars().count();
            c[start] == '#'
        }) else {
            return;
        };
        let start = text[..m.start()].chars().count();
        let open = text[..m.end()].chars().count() - 1;
        let Some(close) = matching(c, open, '{', '}') else {
            return;
        };
        for slot in c.iter_mut().take(close + 1).skip(start) {
            if *slot != '\n' {
                *slot = ' ';
            }
        }
    }
}

/// 1-based lines of the production index expressions that cut by `len() - N`.
pub fn hits(text: &str) -> Vec<usize> {
    let mut c = blank_non_code(text);
    drop_inline_test_modules(&mut c);
    let shape = subtractive_len();
    let mut lines = Vec::new();
    for (open, &ch) in c.iter().enumerate() {
        if ch != '[' {
            continue;
        }
        let Some(close) = matching(&c, open, '[', ']') else {
            continue;
        };
        // The index with nested indexes collapsed to `[]`.
        let mut inner = String::new();
        let mut j = open + 1;
        while j < close {
            if c[j] == '[' {
                inner.push_str("[]");
                j = matching(&c, j, '[', ']').unwrap_or(close) + 1;
            } else {
                inner.push(c[j]);
                j += 1;
            }
        }
        if inner.contains("..") && shape.is_match(&inner) {
            lines.push(1 + c[..open].iter().filter(|&&x| x == '\n').count());
        }
    }
    lines
}

/// `(relative path, hit lines)` for every non-test file under `root`, and the
/// number of files walked (so a caller can refuse a scan that saw nothing).
pub fn scan_tree(root: &Path) -> (Vec<(String, Vec<usize>)>, usize) {
    let mut files = Vec::new();
    rust_files(root, &mut files);
    let walked = files.len();
    let mut found = Vec::new();
    for path in files {
        let rel = path
            .strip_prefix(root)
            .expect("under root")
            .to_string_lossy()
            .replace('\\', "/");
        if is_test_file(&rel)
            || rel
                .split('/')
                .any(|part| part == "tests" || part == "benches")
        {
            continue;
        }
        let lines = hits(&std::fs::read_to_string(&path).expect("read"));
        if !lines.is_empty() {
            found.push((rel, lines));
        }
    }
    (found, walked)
}
