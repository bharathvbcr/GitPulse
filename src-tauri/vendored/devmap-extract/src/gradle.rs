//! `settings.gradle` / `settings.gradle.kts`: which directories are modules.
//!
//! `include(":app", ":core")` names two Gradle modules, each a directory under
//! the settings file's own, and a module's sources live under
//! `<module>/src/{main,test,androidTest}/{java,kotlin}` — a root no fixed
//! repository-level list can know. Only the `include` statement is read; the
//! rest of a build script is code (`project(":x").projectDir = file(..)`, source
//! sets in `build.gradle.kts`) and is deliberately left unread: a module a
//! script relocates is simply not found, which the import falls back from
//! rather than answers wrongly.
//!
//! Strict about what it cannot read: an `include` whose argument list is not
//! closed or not a string is an `Err`, so the caller records a coverage gap
//! instead of treating the repository as having no modules.

/// More modules than any real build declares.
const MAX_MODULES: usize = 2048;

/// Whether `path` names a Gradle settings script.
pub fn is_gradle_settings(path: &str) -> bool {
    path.replace('\\', "/")
        .rsplit('/')
        .next()
        .is_some_and(|name| name == "settings.gradle" || name == "settings.gradle.kts")
}

/// The module directories (relative to the settings file's directory,
/// `/`-separated) its `include` statements name, or why they could not be read.
pub fn parse_modules(source: &str) -> Result<Vec<String>, String> {
    let bytes = source.as_bytes();
    let mut modules = Vec::new();
    let mut at = 0usize;
    while let Some(offset) = source[at..].find("include") {
        let start = at + offset;
        at = start + "include".len();
        let boundary_before = start == 0
            || !(bytes[start - 1].is_ascii_alphanumeric()
                || matches!(bytes[start - 1], b'_' | b'.' | b'"' | b'\''));
        let after = bytes.get(at).copied();
        if !boundary_before || !matches!(after, Some(b'(' | b' ' | b'\t' | b'\n' | b'\r')) {
            continue;
        }
        // Only a call: `include(` or `include ` followed by a string.
        let mut cursor = at;
        while bytes.get(cursor).is_some_and(|b| b.is_ascii_whitespace()) {
            cursor += 1;
        }
        let parenthesised = bytes.get(cursor) == Some(&b'(');
        if parenthesised {
            cursor += 1;
        }
        loop {
            while bytes.get(cursor).is_some_and(|b| b.is_ascii_whitespace()) {
                cursor += 1;
            }
            let Some(&quote @ (b'"' | b'\'')) = bytes.get(cursor) else {
                break;
            };
            cursor += 1;
            let value_start = cursor;
            while cursor < bytes.len() && bytes[cursor] != quote && bytes[cursor] != b'\n' {
                cursor += 1;
            }
            if bytes.get(cursor) != Some(&quote) {
                return Err("an `include` argument is not a terminated string".to_string());
            }
            let path = source[value_start..cursor].trim().trim_start_matches(':');
            cursor += 1;
            if !path.is_empty() {
                if modules.len() >= MAX_MODULES {
                    return Err(format!(
                        "more than {MAX_MODULES} modules; the rest were not read"
                    ));
                }
                modules.push(path.replace(':', "/"));
            }
            while bytes.get(cursor).is_some_and(|b| b.is_ascii_whitespace()) {
                cursor += 1;
            }
            if bytes.get(cursor) == Some(&b',') {
                cursor += 1;
            } else {
                break;
            }
        }
        if parenthesised && bytes.get(cursor) != Some(&b')') {
            // `include(` named nothing readable: a variable, a list expression.
            // Not an error — the statement is real, just not data — unless it
            // swallowed strings and then stopped mid-list.
            if source[at..cursor].contains(['"', '\'']) {
                return Err("an `include(` argument list is not closed".to_string());
            }
        }
        at = cursor;
    }
    modules.sort();
    modules.dedup();
    Ok(modules)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn both_dsls_and_nested_modules_are_read() {
        assert_eq!(
            parse_modules("include(\":app\", \":core\")\ninclude ':libs:util', ':wear'\n"),
            Ok(vec![
                "app".to_string(),
                "core".to_string(),
                "libs/util".to_string(),
                "wear".to_string()
            ])
        );
        assert_eq!(
            parse_modules("include(\n  \":a\",\n  \":b\"\n)\n"),
            Ok(vec!["a".to_string(), "b".to_string()])
        );
    }

    #[test]
    fn a_word_that_merely_contains_include_is_not_a_statement() {
        assert_eq!(
            parse_modules("val includeAll = true\nincludeBuild(\"../x\")\nfoo.include(\":no\")\n"),
            Ok(Vec::new())
        );
    }

    #[test]
    fn an_include_that_cannot_be_read_is_an_error_not_no_modules() {
        for broken in [
            "include(\":app\", \":core)\n",
            "include(\":app\", \":core\"\n",
        ] {
            assert!(
                parse_modules(broken).is_err(),
                "{broken:?} must be refused, not read as naming no modules"
            );
        }
    }

    #[test]
    fn the_module_list_is_bounded() {
        let source: String = (0..=MAX_MODULES)
            .map(|index| format!("include(\":m{index}\")\n"))
            .collect();
        assert!(parse_modules(&source).is_err());
    }
}
