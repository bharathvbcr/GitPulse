//! Which paths are test files: the one rule `protocols/test-paths/v1.json` pins.
//!
//! Go's verifier (`devcouncil/verify` `isTestPath`) reads the same fixture. A
//! directory must be *exactly* a test directory and a file must carry a test
//! affix — never a bare substring, so `contest`, `testing_utils.py` and
//! `lib/latest/mod.rs` are not tests. The camel-case arm reads the original
//! casing, so `FooTest.java` is a test and `Latest.java` is not.

/// Directory names that make everything beneath them a test path, compared
/// case-insensitively.
const TEST_DIRECTORIES: [&str; 8] = [
    "test",
    "tests",
    "spec",
    "specs",
    "__tests__",
    "testing",
    "e2e",
    "testdata",
];

/// Whether `path` names a test file. Either separator is accepted.
pub fn is_test_path(path: &str) -> bool {
    let normalized = path.replace('\\', "/");
    let file_name = normalized.rsplit('/').next().unwrap_or(&normalized);
    in_test_directory(&normalized) || is_test_file_name(file_name)
}

/// Whether a directory component of `path` (never its file name) is exactly a
/// test directory, compared case-insensitively. Either separator is accepted.
pub fn in_test_directory(path: &str) -> bool {
    let normalized = path.replace('\\', "/");
    normalized.rsplit_once('/').is_some_and(|(directories, _)| {
        directories
            .split('/')
            .any(|segment| TEST_DIRECTORIES.contains(&segment.to_lowercase().as_str()))
    })
}

/// Whether a bare file name carries a test affix.
pub fn is_test_file_name(file_name: &str) -> bool {
    let lowered = file_name.to_lowercase();
    let stem = file_name.split('.').next().unwrap_or(file_name);
    let lowered_stem = lowered.split('.').next().unwrap_or(&lowered);
    lowered.starts_with("test_")
        || lowered.starts_with("spec_")
        || [".test.", ".spec.", "_test.", "_spec.", "-test.", "-spec."]
            .iter()
            .any(|infix| lowered.contains(infix))
        || matches!(lowered_stem, "test" | "tests" | "spec" | "specs")
        || ["Test", "Tests", "Spec", "Specs"]
            .iter()
            .any(|suffix| stem.ends_with(suffix))
}

/// Reads the rows of `protocols/test-paths/v1.json` as `(path, is_test)`.
///
/// For the crates' conformance tests, so each holds its classifier to the one
/// fixture without carrying a JSON reader. It reads the fixture's own layout
/// (one `"path"` and one `"test"` member per row, in that order) and refuses
/// anything else rather than guessing, so a reformatted fixture fails loudly.
pub fn parse_test_path_rows(json: &str) -> Result<Vec<(String, bool)>, String> {
    let mut rows = Vec::new();
    let mut pending: Option<String> = None;
    for line in json.lines() {
        let line = line.trim().trim_end_matches(',');
        if let Some(raw) = line.strip_prefix("\"path\":") {
            if pending.is_some() {
                return Err("a row has a path and no test member".into());
            }
            pending = Some(unquote(raw.trim())?);
        } else if let Some(flag) = line.strip_prefix("\"test\":") {
            let Some(path) = pending.take() else {
                return Err("a test member with no path before it".into());
            };
            match flag.trim() {
                "true" => rows.push((path, true)),
                "false" => rows.push((path, false)),
                other => return Err(format!("test member {other:?} is not a boolean")),
            }
        }
    }
    if pending.is_some() {
        return Err("the last row has a path and no test member".into());
    }
    if rows.is_empty() {
        return Err("the fixture has no rows".into());
    }
    Ok(rows)
}

fn unquote(raw: &str) -> Result<String, String> {
    let body = raw
        .strip_prefix('"')
        .and_then(|rest| rest.strip_suffix('"'))
        .ok_or_else(|| format!("{raw:?} is not a JSON string"))?;
    let mut out = String::with_capacity(body.len());
    let mut chars = body.chars();
    while let Some(c) = chars.next() {
        if c != '\\' {
            out.push(c);
            continue;
        }
        match chars.next() {
            Some('\\') => out.push('\\'),
            Some('"') => out.push('"'),
            Some('/') => out.push('/'),
            other => return Err(format!("escape {other:?} in {raw:?} is not read")),
        }
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    const FIXTURE: &str = include_str!("../../../protocols/test-paths/v1.json");

    #[test]
    fn the_rule_agrees_with_every_row_of_the_fixture() {
        let rows = parse_test_path_rows(FIXTURE).expect("fixture");
        assert!(
            rows.len() >= 29,
            "the fixture was read short: {}",
            rows.len()
        );
        for (path, expected) in rows {
            assert_eq!(is_test_path(&path), expected, "{path}");
        }
    }

    #[test]
    fn a_reformatted_fixture_is_refused_not_misread() {
        assert!(parse_test_path_rows("{}").is_err());
        assert!(parse_test_path_rows("\"path\": \"a\"\n").is_err());
        assert!(parse_test_path_rows("\"test\": true\n").is_err());
        assert!(parse_test_path_rows("\"path\": \"a\"\n\"test\": 1\n").is_err());
    }
}
