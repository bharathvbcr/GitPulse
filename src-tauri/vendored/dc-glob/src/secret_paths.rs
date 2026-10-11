//! The paths whose contents are credentials, as the write and read gates name
//! them.
//!
//! `backend/go_orchestrator/policy.SecretPathPatterns` is the owner. This is
//! its Rust copy, for the analysis plane's readers that never pass through the
//! Go gate — DevMap indexes a repository's string literals and serves its
//! source spans straight to an agent, so without this list a value the write
//! gate refuses to let anyone touch came back from `devmap literals`. The copy
//! is pinned to the Go list by a test that reads `file.go`, because the failure
//! mode of two lists is one of them growing a pattern the other lacks.

use std::sync::OnceLock;

use crate::{Name, Pattern};

/// `policy.SecretPathPatterns`, entry for entry and in the same order.
///
/// The Go comment's rule holds here too: every `**/x` entry has a bare `x`
/// beside it, because `**/` matches one or more leading segments, never zero.
pub const SECRET_PATH_PATTERNS: &[&str] = &[
    ".env",
    ".env.*",
    "**/.env",
    "**/.env.*",
    "credentials/**",
    "**/credentials/**",
    "secrets/**",
    "**/secrets/**",
    "*.pem",
    "*.key",
    "**/id_rsa",
    "**/id_dsa",
    "**/id_ecdsa",
    "**/id_ed25519",
    "id_rsa",
    "id_dsa",
    "id_ecdsa",
    "id_ed25519",
    ".npmrc",
    "**/.npmrc",
    ".pypirc",
    "**/.pypirc",
    ".netrc",
    "**/.netrc",
];

/// Whether `path`, repository-relative, names a file whose contents are
/// credentials.
///
/// Answers as Go's `fnmatch.MatchAnyFold(SecretPathPatterns, path)` does:
/// case-folded, because `.ENV` and `.env` are one file on APFS and NTFS, and
/// an undecided match — a name past the matcher's size cap — counts as secret,
/// because this is a deny list and a false here would hand the value over.
/// Backslashes are read as separators and a leading `./` is dropped, so a
/// Windows-spelled or dot-prefixed path cannot step around a pattern.
pub fn is_secret_path(path: &str) -> bool {
    let slashed = path.replace('\\', "/");
    let mut relative = slashed.as_str();
    while let Some(rest) = relative.strip_prefix("./") {
        relative = rest;
    }
    let folded = fold(relative);
    let Some(name) = Name::new(&folded) else {
        return true;
    };
    compiled().iter().any(|pattern| {
        pattern
            .as_ref()
            .is_none_or(|compiled| compiled.try_matches(&name).unwrap_or(true))
    })
}

/// [`SECRET_PATH_PATTERNS`], compiled once. The indexer asks about every file
/// it extracts; compiling two dozen patterns per question was most of the
/// cost. A pattern that will not compile stays `None` and matches everything,
/// for the same deny-list reason an undecided match does.
fn compiled() -> &'static [Option<Pattern>] {
    static COMPILED: OnceLock<Vec<Option<Pattern>>> = OnceLock::new();
    COMPILED.get_or_init(|| {
        SECRET_PATH_PATTERNS
            .iter()
            .map(|pattern| Pattern::compile(pattern))
            .collect()
    })
}

/// Folds one scalar at a time the way Go's `fnmatch.foldRune` does: simple
/// case folding as a case-insensitive filesystem applies it.
///
/// Rust's `char::to_lowercase` can expand a scalar into several (`İ` becomes
/// `i` plus a combining dot); Go maps it to one. Keeping the first keeps the
/// two folds equal on every name the patterns can tell apart, all of which
/// are ASCII. `to_lowercase` leaves U+017F (long s) alone, and APFS folds it to
/// `s`; U+212A (Kelvin sign) it already maps to `k`. Those are the only two
/// scalars outside ASCII whose fold orbit holds an ASCII letter, and
/// `fold_covers_every_ascii_orbit` walks the whole range to keep it so.
fn fold(text: &str) -> String {
    text.chars().map(fold_char).collect()
}

fn fold_char(c: char) -> char {
    if c == '\u{17f}' {
        return 's';
    }
    c.to_lowercase().next().unwrap_or(c)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The Go list, read out of the file that owns it.
    fn go_secret_patterns() -> Vec<String> {
        let source = include_str!("../../../backend/go_orchestrator/policy/file.go");
        let start = source
            .find("var SecretPathPatterns = []string{")
            .expect("policy/file.go no longer declares SecretPathPatterns");
        let body = &source[start..];
        let end = body
            .find("\n}")
            .expect("SecretPathPatterns is unterminated");
        body[..end]
            .lines()
            .skip(1)
            .map(str::trim)
            .filter(|line| line.starts_with('"'))
            .map(|line| line.trim_end_matches(',').trim_matches('"').to_string())
            .collect()
    }

    #[test]
    fn the_list_is_the_go_policy_list() {
        let go = go_secret_patterns();
        assert!(go.len() >= 20, "read only {} Go patterns", go.len());
        let rust: Vec<String> = SECRET_PATH_PATTERNS.iter().map(|p| p.to_string()).collect();
        assert_eq!(
            rust, go,
            "dc_glob::SECRET_PATH_PATTERNS drifted from policy.SecretPathPatterns"
        );
    }

    #[test]
    fn secret_paths_are_recognised_at_the_root_and_nested() {
        for path in [
            ".env",
            ".env.production",
            "app/.env",
            "secrets/db.py",
            "deploy/secrets/prod.env",
            "credentials/aws.py",
            "server.key",
            "certs/tls.pem",
            "id_ed25519",
            "home/.ssh/id_rsa",
            ".npmrc",
            "pkg/.pypirc",
            ".netrc",
        ] {
            assert!(is_secret_path(path), "{path} should be secret");
        }
    }

    #[test]
    fn case_dot_prefix_and_backslashes_do_not_step_around_a_pattern() {
        for path in [
            ".ENV",
            "Secrets/db.py",
            "./secrets/db.py",
            r"credentials\aws.py",
            "SERVER.KEY",
        ] {
            assert!(is_secret_path(path), "{path} should be secret");
        }
    }

    #[test]
    fn ordinary_paths_are_not_secret() {
        for path in [
            "src/app.py",
            "config/settings.py",
            "docs/secrets.md",
            "src/credentials_test.go",
            "keys.rs",
            ".envrc",
        ] {
            assert!(!is_secret_path(path), "{path} should not be secret");
        }
    }

    #[test]
    fn an_undecidable_name_counts_as_secret() {
        assert!(is_secret_path(&"a/".repeat(crate::MAX_UNITS)));
    }

    /// A case-insensitive filesystem opens `id_rſa` as `id_rsa`. The rows are
    /// held by the Go gate too (policy/zfix_w6_fold_test.go reads the same file).
    #[test]
    fn folded_spellings_match_the_shared_fixture() {
        let fixture = include_str!("../../../testdata/secret-paths-fold.tsv");
        let mut checked = 0usize;
        for (lineno, line) in fixture.lines().enumerate() {
            if line.starts_with('#') || line.trim().is_empty() {
                continue;
            }
            let cols: Vec<&str> = line.split('\t').collect();
            assert_eq!(cols.len(), 2, "line {} is malformed: {line:?}", lineno + 1);
            assert_eq!(
                is_secret_path(cols[0]),
                cols[1] == "true",
                "is_secret_path({:?}) disagrees with the fixture (line {})",
                cols[0],
                lineno + 1
            );
            checked += 1;
        }
        assert!(checked >= 20, "only {checked} rows loaded from the fixture");
    }

    /// Every scalar outside ASCII that a case-insensitive filesystem folds to an
    /// ASCII letter must fold to it here: the long s and the Kelvin sign today.
    #[test]
    fn fold_covers_every_ascii_orbit() {
        let mut found = Vec::new();
        for c in (0x80u32..=0x10FFFF).filter_map(char::from_u32) {
            // Simple folding only: a scalar that case-maps to several (ß to SS)
            // has no single-scalar fold.
            let single = |mut it: Box<dyn Iterator<Item = char>>| match (it.next(), it.next()) {
                (Some(one), None) => Some(one),
                _ => None,
            };
            // U+0131 uppercases to I but CaseFolding.txt folds it only under the
            // Turkic (T) status, which filesystems do not apply.
            if c == '\u{131}' {
                continue;
            }
            let lower = single(Box::new(c.to_lowercase()));
            let upper = single(Box::new(c.to_uppercase()));
            if let Some(ascii) = [lower, upper]
                .into_iter()
                .flatten()
                .find(|o| o.is_ascii_alphabetic())
            {
                found.push(c);
                assert_eq!(
                    fold_char(c),
                    ascii.to_ascii_lowercase(),
                    "{c:?} folds to an ASCII letter on APFS but not here"
                );
            }
        }
        assert!(
            found.contains(&'\u{17f}') && found.contains(&'\u{212a}'),
            "walk found {found:?}"
        );
    }
}
