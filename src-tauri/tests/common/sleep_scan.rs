//! Source scan for short sleeps in test code.
//!
//! A host under load stretches a 10 ms sleep to tens of milliseconds, so a test
//! that paces behaviour or asserts timing around a short sleep goes red with no
//! change to the code under test. This module finds every `sleep(..)` in test
//! code whose duration is below [`THRESHOLD_NANOS`], or that it cannot prove is
//! at or above it, and gives each one a stable anchor so
//! `tests/hermetic_contract.rs` can hold the list against a committed allowlist.
//!
//! Included with `#[path = "common/sleep_scan.rs"] mod sleep_scan;` by the one
//! crate that uses it, the way `slice_guard` and `nextest_list` are.
//!
//! # What counts as test code
//!
//! * every file under `tests/`, `src/test_support*`, and files named `tests.rs`,
//!   `*_tests.rs` or `*_test.rs`;
//! * a body that follows a test-only attribute: `#[test]`, `#[tokio::test]`, or
//!   a `#[cfg(..)]` whose predicate requires `test` (`test`, `all(test, ..)`,
//!   `any(test, ..)` only when every arm needs it; never under `not(..)`);
//! * a `mod name;` declared under such an attribute, inside such a body, or in
//!   a file already known to be test code, resolved the way the compiler does.
//!
//! # What counts as a sleep
//!
//! The identifier `sleep` followed by `(` that is not the name of a function
//! being declared: `thread::sleep`, `std::thread::sleep`, a bare imported
//! `sleep`, `tokio::time::sleep(..).await`, and a `.sleep(..)` method on an
//! injected clock (flagged on purpose: a clock that really sleeps is as exposed
//! as the thread, and one that does not is cheap to allowlist). Comments,
//! string literals and character literals are blanked first, so a snippet in a
//! string never counts.
//!
//! # How a duration is judged
//!
//! The argument is evaluated to nanoseconds when it is built from literals:
//! `Duration::from_{millis,micros,nanos,secs,secs_f32,secs_f64,mins}(<number>)`,
//! sums and integer products of those, `Duration::ZERO`, a
//! `coverage_relaxed(..)` wrapper (its nominal value is what the author wrote),
//! and a name defined once in the same file as `const`/`static`/`let` with such
//! a value (`const SETTLE: Duration = ..`, `const POLL_MS: u64 = 5` used as
//! `from_millis(POLL_MS)`). Anything else (a parameter, a computed value, a
//! method on a duration) is *unresolved*, and unresolved is flagged exactly like
//! a short sleep: a check that could not run must not report what a check that
//! passed would.
//!
//! # The anchor
//!
//! `path|function|ordinal`. The function is the enclosing `fn`, qualified by the
//! enclosing `impl` target when there is one (`Waiter::drop`); the ordinal
//! counts every sleep in that function of that file, whatever its duration, so
//! changing a duration or a line number never moves an anchor.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

/// A sleep shorter than this is flagged.
pub const THRESHOLD_NANOS: u128 = 100_000_000;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Site {
    /// Path relative to the crate root, `/`-separated.
    pub file: String,
    /// `fn` or `Type::fn`; `<item>` outside any function.
    pub func: String,
    /// Index of this sleep among all sleeps of `func` in `file`.
    pub ordinal: usize,
    /// 1-based line, for the failure message only (never part of the anchor).
    pub line: usize,
    /// The argument text, whitespace collapsed.
    pub arg: String,
    /// The duration when the argument is built from literals.
    pub nanos: Option<u128>,
    /// Whether the sleep sits inside a `loop`, `while` or `for` body.
    pub in_loop: bool,
}

impl Site {
    pub fn key(&self) -> String {
        format!("{}|{}|{}", self.file, self.func, self.ordinal)
    }

    pub fn flagged(&self) -> bool {
        self.nanos.is_none_or(|nanos| nanos < THRESHOLD_NANOS)
    }
}

/// Every sleep in the test code of the given files, flagged or not.
pub fn scan(root: &Path, files: &[PathBuf]) -> Vec<Site> {
    let mut texts = BTreeMap::new();
    for path in files {
        let raw = std::fs::read_to_string(path).unwrap_or_else(|e| panic!("{path:?}: {e}"));
        texts.insert(path.clone(), blank(&raw));
    }
    let test_files = test_file_set(root, &texts);
    let mut sites = Vec::new();
    for (path, text) in &texts {
        let rel = relative(root, path);
        let regions = if test_files.contains(path) {
            vec![(0, text.len())]
        } else {
            test_regions(text).0
        };
        if regions.is_empty() {
            continue;
        }
        sites.extend(sites_in(&rel, text, &regions));
    }
    sites
}

/// Sleeps of one file inside `regions`, with anchors.
fn sites_in(rel: &str, text: &str, regions: &[(usize, usize)]) -> Vec<Site> {
    let bytes = text.as_bytes();
    let consts = Consts::of(text);
    let mut sites = Vec::new();
    let mut ordinals: BTreeMap<String, usize> = BTreeMap::new();
    // Each open block: its header (the text since the last `{`, `}` or `;`) and
    // where that header started.
    let mut stack: Vec<(&str, usize)> = Vec::new();
    let mut header_start = 0;
    let mut previous_word = "";
    let mut line = 1;
    let mut i = 0;
    while i < bytes.len() {
        let b = bytes[i];
        match b {
            b'\n' => line += 1,
            b'{' => {
                stack.push((&text[header_start..i], header_start));
                header_start = i + 1;
            }
            b'}' => {
                let (header, outer_start) = stack.pop().unwrap_or(("", 0));
                // `while unsafe { probe() } != pid && .. {`: an `unsafe` block is
                // an expression inside its statement's header, not the end of it.
                header_start = if header.trim_end().ends_with("unsafe") {
                    outer_start
                } else {
                    i + 1
                };
            }
            b';' => header_start = i + 1,
            _ => {}
        }
        if is_ident_byte(b) && (i == 0 || !is_ident_byte(bytes[i - 1])) {
            let mut end = i;
            while end < bytes.len() && is_ident_byte(bytes[end]) {
                end += 1;
            }
            let word = &text[i..end];
            if word == "sleep" && previous_word != "fn" {
                let after = text[end..].trim_start();
                if after.starts_with('(') && regions.iter().any(|(a, z)| *a <= i && i <= *z) {
                    let open = end + (text[end..].len() - after.len());
                    if let Some(close) = matching(bytes, open, b'(', b')') {
                        let arg = collapse(&text[open + 1..close]);
                        let headers: Vec<&str> = stack.iter().map(|(header, _)| *header).collect();
                        let func = enclosing_function(&headers);
                        let counter = ordinals.entry(func.clone()).or_insert(0);
                        let ordinal = *counter;
                        *counter += 1;
                        sites.push(Site {
                            file: rel.to_string(),
                            func,
                            ordinal,
                            line,
                            nanos: eval_duration(&arg, &consts),
                            arg,
                            in_loop: headers.iter().any(|header| is_loop_header(header)),
                        });
                    }
                }
            }
            previous_word = word;
            // Newlines inside the word cannot occur; skip past it.
            i = end;
            continue;
        }
        i += 1;
    }
    sites
}

fn relative(root: &Path, path: &Path) -> String {
    path.strip_prefix(root)
        .unwrap_or(path)
        .components()
        .map(|c| c.as_os_str().to_string_lossy().into_owned())
        .collect::<Vec<_>>()
        .join("/")
}

fn is_ident_byte(b: u8) -> bool {
    b.is_ascii_alphanumeric() || b == b'_'
}

fn collapse(text: &str) -> String {
    text.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// The index of the delimiter closing the one at `open`.
fn matching(bytes: &[u8], open: usize, up: u8, down: u8) -> Option<usize> {
    let mut depth = 0usize;
    for (offset, b) in bytes[open..].iter().enumerate() {
        if *b == up {
            depth += 1;
        } else if *b == down {
            depth -= 1;
            if depth == 0 {
                return Some(open + offset);
            }
        }
    }
    None
}

// ---------------------------------------------------------------------------
// Blanking: comments, strings and character literals become spaces.
// ---------------------------------------------------------------------------

/// `text` with every comment, string literal and character literal replaced by
/// spaces (newlines kept), and any remaining non-ASCII character by `_`, so byte
/// offsets in the result are character offsets and line numbers still match.
pub fn blank(text: &str) -> String {
    let chars: Vec<char> = text.chars().collect();
    let n = chars.len();
    let mut out = chars.clone();
    let wipe = |out: &mut Vec<char>, from: usize, to: usize| {
        for slot in &mut out[from..to.min(n)] {
            if *slot != '\n' {
                *slot = ' ';
            }
        }
    };
    let ident = |c: char| c.is_ascii_alphanumeric() || c == '_';
    let mut i = 0;
    while i < n {
        let c = chars[i];
        let next = chars.get(i + 1).copied();
        if c == '/' && next == Some('/') {
            let mut j = i;
            while j < n && chars[j] != '\n' {
                j += 1;
            }
            wipe(&mut out, i, j);
            i = j;
        } else if c == '/' && next == Some('*') {
            let mut depth = 1;
            let mut j = i + 2;
            while j < n && depth > 0 {
                if chars[j] == '/' && chars.get(j + 1) == Some(&'*') {
                    depth += 1;
                    j += 2;
                } else if chars[j] == '*' && chars.get(j + 1) == Some(&'/') {
                    depth -= 1;
                    j += 2;
                } else {
                    j += 1;
                }
            }
            wipe(&mut out, i, j);
            i = j;
        } else if c == 'r'
            && (i == 0
                || !ident(chars[i - 1])
                || (chars[i - 1] == 'b' && (i == 1 || !ident(chars[i - 2]))))
            && raw_string_end(&chars, i).is_some()
        {
            let end = raw_string_end(&chars, i).unwrap();
            wipe(&mut out, i, end);
            i = end;
        } else if c == '"' {
            let mut j = i + 1;
            while j < n && chars[j] != '"' {
                j += if chars[j] == '\\' { 2 } else { 1 };
            }
            let end = (j + 1).min(n);
            wipe(&mut out, i, end);
            i = end;
        } else if c == '\'' {
            if next == Some('\\') {
                let mut j = i + 3;
                while j < n && chars[j] != '\'' && j < i + 14 {
                    j += 1;
                }
                let end = (j + 1).min(n);
                wipe(&mut out, i, end);
                i = end;
            } else if chars.get(i + 2) == Some(&'\'') && next != Some('\'') {
                wipe(&mut out, i, i + 3);
                i += 3;
            } else {
                i += 1;
            }
        } else {
            i += 1;
        }
    }
    out.into_iter()
        .map(|c| if c.is_ascii() { c } else { '_' })
        .collect()
}

/// The end (exclusive) of the raw string starting at `chars[start] == 'r'`, if
/// that `r` opens one.
fn raw_string_end(chars: &[char], start: usize) -> Option<usize> {
    let mut hashes = 0;
    let mut j = start + 1;
    while chars.get(j) == Some(&'#') {
        hashes += 1;
        j += 1;
    }
    if chars.get(j) != Some(&'"') {
        return None;
    }
    j += 1;
    while j < chars.len() {
        if chars[j] == '"' && (0..hashes).all(|k| chars.get(j + 1 + k) == Some(&'#')) {
            return Some(j + 1 + hashes);
        }
        j += 1;
    }
    Some(chars.len())
}

// ---------------------------------------------------------------------------
// Test regions and test files.
// ---------------------------------------------------------------------------

/// Whether a `cfg` predicate is true only when `test` is set.
pub fn cfg_requires_test(predicate: &str) -> bool {
    let predicate = predicate.trim();
    if predicate == "test" {
        return true;
    }
    let Some(open) = predicate.find('(') else {
        return false;
    };
    if !predicate.ends_with(')') {
        return false;
    }
    let (head, inner) = (
        predicate[..open].trim(),
        &predicate[open + 1..predicate.len() - 1],
    );
    let arms = split_top(inner, ',');
    match head {
        "all" => arms.iter().any(|arm| cfg_requires_test(arm)),
        "any" => !arms.is_empty() && arms.iter().all(|arm| cfg_requires_test(arm)),
        _ => false,
    }
}

/// Whether an attribute's text (between `#[` and `]`) marks test-only code.
fn attribute_marks_test(attribute: &str) -> bool {
    let attribute = attribute.trim();
    if let Some(rest) = attribute.strip_prefix("cfg") {
        let rest = rest.trim_start();
        if let Some(inner) = rest.strip_prefix('(').and_then(|r| r.strip_suffix(')')) {
            return cfg_requires_test(inner);
        }
        return false;
    }
    let path = attribute.split('(').next().unwrap_or("").trim();
    path == "test" || path.ends_with("::test")
}

/// The byte ranges of items under a test-only attribute (attribute through the
/// closing brace), and the `mod name;` declarations under one.
pub fn test_regions(text: &str) -> (Vec<(usize, usize)>, Vec<String>) {
    let bytes = text.as_bytes();
    let mut regions = Vec::new();
    let mut modules = Vec::new();
    let mut i = 0;
    while i + 1 < bytes.len() {
        if bytes[i] != b'#' {
            i += 1;
            continue;
        }
        let inner = bytes.get(i + 1) == Some(&b'!');
        let open = if inner { i + 2 } else { i + 1 };
        if bytes.get(open) != Some(&b'[') {
            i += 1;
            continue;
        }
        let Some(close) = matching(bytes, open, b'[', b']') else {
            break;
        };
        let attribute = &text[open + 1..close];
        if !attribute_marks_test(attribute) {
            i = close + 1;
            continue;
        }
        if inner {
            return (vec![(0, bytes.len())], modules);
        }
        // Skip any further attributes, then find the item's body or its `;`.
        let mut j = close + 1;
        let mut depth = 0i32;
        let mut end = None;
        while j < bytes.len() {
            match bytes[j] {
                b'(' | b'[' => depth += 1,
                b')' | b']' => depth -= 1,
                b'{' if depth == 0 => {
                    end = matching(bytes, j, b'{', b'}').map(|e| (j, e));
                    break;
                }
                b';' if depth == 0 => {
                    let head = text[close + 1..j].trim();
                    if let Some(name) = declared_module(head) {
                        modules.push(name);
                    }
                    break;
                }
                _ => {}
            }
            j += 1;
        }
        match end {
            Some((_, e)) => {
                regions.push((i, e));
                i = e + 1;
            }
            None => i = close + 1,
        }
    }
    (regions, modules)
}

/// `mod name` (with any visibility and further attributes before it) -> `name`.
fn declared_module(head: &str) -> Option<String> {
    let words: Vec<&str> = head
        .split(|c: char| !(c.is_ascii_alphanumeric() || c == '_'))
        .filter(|w| !w.is_empty())
        .collect();
    let at = words.iter().rposition(|w| *w == "mod")?;
    (at + 1 == words.len() - 1).then(|| words[at + 1].to_string())
}

/// Every `mod name;` in the text, with where it sits.
fn module_declarations(text: &str) -> Vec<(usize, String)> {
    let bytes = text.as_bytes();
    let mut found = Vec::new();
    let mut i = 0;
    while i + 4 < bytes.len() {
        if &bytes[i..i + 3] == b"mod"
            && !is_ident_byte(bytes[i + 3])
            && (i == 0 || !is_ident_byte(bytes[i - 1]))
        {
            let rest = text[i + 3..].trim_start();
            let name: String = rest
                .chars()
                .take_while(|c| c.is_ascii_alphanumeric() || *c == '_')
                .collect();
            let after = rest[name.len()..].trim_start();
            if !name.is_empty() && after.starts_with(';') {
                found.push((i, name));
            }
        }
        i += 1;
    }
    found
}

/// Files that are test code as a whole: by name, or declared only for tests.
fn test_file_set(root: &Path, texts: &BTreeMap<PathBuf, String>) -> BTreeSet<PathBuf> {
    let mut set: BTreeSet<PathBuf> = texts
        .keys()
        .filter(|path| is_test_path(&relative(root, path)))
        .cloned()
        .collect();
    // Seeds: modules declared under a test attribute, inside a test body, or in
    // a file that is test code by name.
    let mut queue: Vec<(PathBuf, String)> = Vec::new();
    for (path, text) in texts {
        let (regions, attributed) = test_regions(text);
        queue.extend(attributed.into_iter().map(|name| (path.clone(), name)));
        for (at, name) in module_declarations(text) {
            let in_body = regions.iter().any(|(a, z)| *a <= at && at <= *z);
            if in_body || set.contains(path) {
                queue.push((path.clone(), name));
            }
        }
    }
    while let Some((parent, name)) = queue.pop() {
        for file in module_files(&parent, &name) {
            if texts.contains_key(&file) && set.insert(file.clone()) {
                // A test file's own `mod x;` declarations are test files too.
                for (_, child) in module_declarations(&texts[&file]) {
                    queue.push((file.clone(), child));
                }
            }
        }
    }
    set
}

/// Whether a crate-relative path names test code.
pub fn is_test_path(rel: &str) -> bool {
    let name = rel.rsplit('/').next().unwrap_or(rel);
    rel.starts_with("tests/")
        || rel.contains("/tests/")
        || rel.starts_with("src/test_support")
        || name == "tests.rs"
        || name.ends_with("_tests.rs")
        || name.ends_with("_test.rs")
}

/// The files `mod name;` can mean when written in `parent`.
fn module_files(parent: &Path, name: &str) -> Vec<PathBuf> {
    let dir = parent.parent().unwrap_or_else(|| Path::new("."));
    let stem = parent.file_stem().and_then(|s| s.to_str()).unwrap_or("");
    let mut bases = vec![dir.to_path_buf()];
    if !matches!(stem, "mod" | "lib" | "main") {
        bases.push(dir.join(stem));
    }
    bases
        .into_iter()
        .flat_map(|base| {
            [
                base.join(format!("{name}.rs")),
                base.join(name).join("mod.rs"),
            ]
        })
        .collect()
}

// ---------------------------------------------------------------------------
// Anchors: enclosing function, enclosing impl, loops.
// ---------------------------------------------------------------------------

fn words(text: &str) -> Vec<&str> {
    word_positions(text).into_iter().map(|(_, w)| w).collect()
}

/// The identifier-like words of `text` with the byte offset each starts at.
fn word_positions(text: &str) -> Vec<(usize, &str)> {
    let bytes = text.as_bytes();
    let mut found = Vec::new();
    let mut i = 0;
    while i < bytes.len() {
        if is_ident_byte(bytes[i]) {
            let start = i;
            while i < bytes.len() && is_ident_byte(bytes[i]) {
                i += 1;
            }
            found.push((start, &text[start..i]));
        } else {
            i += 1;
        }
    }
    found
}

/// `fn name` or `Type::fn name` for the innermost function among `stack`'s block
/// headers.
fn enclosing_function(stack: &[&str]) -> String {
    let function = stack.iter().rev().find_map(|header| {
        let all = words(header);
        let at = all.iter().position(|w| *w == "fn")?;
        all.get(at + 1).map(|name| name.to_string())
    });
    let Some(function) = function else {
        return "<item>".to_string();
    };
    let target = stack.iter().rev().find_map(|header| impl_target(header));
    match target {
        Some(target) => format!("{target}::{function}"),
        None => function,
    }
}

/// The type an `impl` header is for: `impl<T> Trait for Foo<T> where ..` -> `Foo`.
fn impl_target(header: &str) -> Option<String> {
    let mut flat = String::new();
    let mut depth = 0;
    for c in header.chars() {
        match c {
            '<' => depth += 1,
            '>' if depth > 0 => depth -= 1,
            _ if depth == 0 => flat.push(c),
            _ => {}
        }
    }
    let all = words(&flat);
    let at = all.iter().position(|w| *w == "impl")?;
    // `fn f(x: impl Trait)` is a function header, not an impl block.
    if all.iter().position(|w| *w == "fn").is_some_and(|f| f < at) {
        return None;
    }
    let rest = &all[at + 1..];
    let rest = match rest.iter().position(|w| *w == "for") {
        Some(for_at) => &rest[for_at + 1..],
        None => rest,
    };
    let rest = match rest.iter().position(|w| *w == "where") {
        Some(where_at) => &rest[..where_at],
        None => rest,
    };
    rest.last().map(|w| w.to_string())
}

fn is_loop_header(header: &str) -> bool {
    let mut header = header.trim_start();
    // A loop label: `'outer: loop`.
    if header.starts_with('\'') {
        header = header
            .split_once(':')
            .map_or(header, |(_, rest)| rest.trim_start());
    }
    let all = words(header);
    // `(0..100).find_map(|_| { .. sleep .. })` retries as a loop does.
    let iterated = header.contains("(0..") && header.contains('|');
    matches!(all.first().copied(), Some("while" | "for" | "loop"))
        || all.contains(&"loop")
        || iterated
}

// ---------------------------------------------------------------------------
// Duration evaluation.
// ---------------------------------------------------------------------------

/// Names a file defines with a literal-built value, every definition of which
/// agrees. A name defined twice with different values stays unresolved.
pub struct Consts {
    durations: BTreeMap<String, u128>,
    numbers: BTreeMap<String, f64>,
}

impl Consts {
    fn of(text: &str) -> Self {
        let mut found = Self {
            durations: BTreeMap::new(),
            numbers: BTreeMap::new(),
        };
        let mut definitions: BTreeMap<String, Vec<String>> = BTreeMap::new();
        for statement in text.split(';') {
            let positions = word_positions(statement);
            let Some((at, keyword)) = positions
                .iter()
                .find(|(_, w)| matches!(*w, "const" | "static" | "let"))
            else {
                continue;
            };
            let tail = statement[at + keyword.len()..].trim_start();
            let tail = tail.strip_prefix("mut ").unwrap_or(tail).trim_start();
            let name: String = tail
                .chars()
                .take_while(|c| c.is_ascii_alphanumeric() || *c == '_')
                .collect();
            let Some((_, value)) = tail.split_once('=') else {
                continue;
            };
            if !name.is_empty() && !value.starts_with('=') {
                definitions.entry(name).or_default().push(collapse(value));
            }
        }
        // Two passes so a name defined in terms of one defined later resolves.
        for _ in 0..2 {
            for (name, values) in &definitions {
                let durations: Vec<Option<u128>> =
                    values.iter().map(|v| eval_duration(v, &found)).collect();
                if durations.iter().all(Option::is_some)
                    && durations.windows(2).all(|w| w[0] == w[1])
                {
                    found.durations.insert(name.clone(), durations[0].unwrap());
                    continue;
                }
                let numbers: Vec<Option<f64>> =
                    values.iter().map(|v| eval_number(v, &found)).collect();
                if numbers.iter().all(Option::is_some) && numbers.windows(2).all(|w| w[0] == w[1]) {
                    found.numbers.insert(name.clone(), numbers[0].unwrap());
                }
            }
        }
        found
    }
}

/// Splits on `separator` outside any bracket.
fn split_top(text: &str, separator: char) -> Vec<String> {
    let mut parts = Vec::new();
    let mut depth = 0i32;
    let mut current = String::new();
    for c in text.chars() {
        match c {
            '(' | '[' | '{' => depth += 1,
            ')' | ']' | '}' => depth -= 1,
            _ => {}
        }
        if c == separator && depth == 0 {
            parts.push(current.trim().to_string());
            current.clear();
        } else {
            current.push(c);
        }
    }
    if !current.trim().is_empty() {
        parts.push(current.trim().to_string());
    }
    parts
}

/// `text` without one pair of parentheses wrapping all of it.
fn unwrap_parens(text: &str) -> &str {
    let text = text.trim().trim_end_matches(',').trim();
    if text.starts_with('(') && matching(text.as_bytes(), 0, b'(', b')') == Some(text.len() - 1) {
        return unwrap_parens(&text[1..text.len() - 1]);
    }
    text
}

/// A call `path::name(inner)` spanning all of `text`: (name, inner).
fn whole_call(text: &str) -> Option<(&str, &str)> {
    let open = text.find('(')?;
    let head = text[..open].trim();
    if head.is_empty()
        || !head
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == ':')
    {
        return None;
    }
    (matching(text.as_bytes(), open, b'(', b')')? == text.len() - 1).then(|| {
        (
            head.rsplit("::").next().unwrap_or(head),
            &text[open + 1..text.len() - 1],
        )
    })
}

fn eval_number(text: &str, consts: &Consts) -> Option<f64> {
    let text = unwrap_parens(text);
    let sums = split_top(text, '+');
    if sums.len() > 1 {
        return sums
            .iter()
            .map(|part| eval_number(part, consts))
            .sum::<Option<f64>>();
    }
    let products = split_top(text, '*');
    if products.len() > 1 {
        return products
            .iter()
            .map(|part| eval_number(part, consts))
            .try_fold(1.0, |acc, part| part.map(|p| acc * p));
    }
    let bare = text.replace('_', "");
    let digits: String = bare
        .chars()
        .take_while(|c| c.is_ascii_digit() || *c == '.')
        .collect();
    if !digits.is_empty() {
        let suffix = &bare[digits.len()..];
        if suffix.is_empty()
            || matches!(
                suffix,
                "u8" | "u16" | "u32" | "u64" | "u128" | "usize" | "i32" | "i64" | "f32" | "f64"
            )
        {
            return digits.parse().ok();
        }
        return None;
    }
    let name = text.rsplit("::").next().unwrap_or(text);
    consts.numbers.get(name).copied()
}

/// The duration `text` evaluates to, in nanoseconds, when it is built from
/// literals and same-file names.
pub fn eval_duration(text: &str, consts: &Consts) -> Option<u128> {
    let text = unwrap_parens(text);
    if let Some((name, inner)) = whole_call(text) {
        if name == "coverage_relaxed" {
            return eval_duration(inner, consts);
        }
        let unit = match name {
            "from_nanos" => 1.0,
            "from_micros" => 1e3,
            "from_millis" => 1e6,
            "from_secs" | "from_secs_f32" | "from_secs_f64" => 1e9,
            "from_mins" => 60e9,
            _ => return None,
        };
        let value = eval_number(inner, consts)?;
        return (value >= 0.0).then(|| (value * unit).round() as u128);
    }
    let sums = split_top(text, '+');
    if sums.len() > 1 {
        return sums
            .iter()
            .map(|part| eval_duration(part, consts))
            .sum::<Option<u128>>();
    }
    let products = split_top(text, '*');
    if products.len() == 2 {
        let (a, b) = (&products[0], &products[1]);
        if let (Some(d), Some(k)) = (eval_duration(a, consts), eval_number(b, consts)) {
            return Some((d as f64 * k).round() as u128);
        }
        if let (Some(k), Some(d)) = (eval_number(a, consts), eval_duration(b, consts)) {
            return Some((d as f64 * k).round() as u128);
        }
        return None;
    }
    let last = text.rsplit("::").next().unwrap_or(text);
    if text.ends_with("Duration::ZERO") {
        return Some(0);
    }
    if last.chars().all(|c| c.is_ascii_alphanumeric() || c == '_') && !last.is_empty() {
        return consts.durations.get(last).copied();
    }
    None
}

/// For the matcher pins: the sleeps of a snippet treated as test code.
pub fn scan_snippet(snippet: &str) -> Vec<Site> {
    let text = blank(snippet);
    sites_in("snippet.rs", &text, &[(0, text.len())])
}
