//! Python `fnmatch` semantics, implemented with no dependencies.
//!
//! This is the Rust half of a contract the Go half also implements
//! (`manvi/internal/fnmatch`). Both are tested against one fixture generated
//! from CPython itself, `testdata/fnmatch-parity.tsv`, because the failure mode
//! of a private reimplementation is not a crash — it is the two planes
//! disagreeing about whether a path is inside the write gate, in one language
//! only, silently.
//!
//! The rule that matters and that most glob crates get differently: `*` matches
//! path separators. `fnmatch("src/foo.py", "*.py")` is true in Python and false
//! under shell-style globbing. Every DevCouncil path rule is written in the
//! Python dialect.

/// Returns true when `name` matches the fnmatch `pattern`.
///
/// Matching is case-sensitive, matching Python on POSIX where
/// `os.path.normcase` is the identity.
pub fn matches(pattern: &str, name: &str) -> bool {
    let pat: Vec<char> = pattern.chars().collect();
    let text: Vec<char> = name.chars().collect();
    match_from(&pat, 0, &text, 0)
}

/// Returns true when `name` matches any of `patterns`.
pub fn matches_any<S: AsRef<str>>(patterns: &[S], name: &str) -> bool {
    patterns.iter().any(|p| matches(p.as_ref(), name))
}

/// Backtracking matcher.
///
/// `*` is handled with a saved-position loop rather than recursion per
/// character, so a pattern of many stars against a long path stays linear in
/// the common case instead of exponential.
fn match_from(pat: &[char], mut pi: usize, text: &[char], mut ti: usize) -> bool {
    // Saved backtrack point: the star we most recently expanded, and how far
    // the text pointer had advanced when we chose that expansion.
    let mut star: Option<(usize, usize)> = None;

    loop {
        if pi < pat.len() {
            match pat[pi] {
                '*' => {
                    // Collapse runs of stars: "**" is "*" repeated, which is why
                    // "**/.env" still requires a separator before ".env".
                    while pi < pat.len() && pat[pi] == '*' {
                        pi += 1;
                    }
                    star = Some((pi, ti));
                    continue;
                }
                '?' if ti < text.len() => {
                    pi += 1;
                    ti += 1;
                    continue;
                }
                '[' => {
                    if let Some((class, next)) = parse_class(pat, pi) {
                        if ti < text.len() && class.contains(text[ti]) {
                            pi = next + 1;
                            ti += 1;
                            continue;
                        }
                    } else if ti < text.len() && text[ti] == '[' {
                        // Unterminated '[' is a literal, never a wildcard.
                        pi += 1;
                        ti += 1;
                        continue;
                    }
                }
                c if ti < text.len() && text[ti] == c => {
                    pi += 1;
                    ti += 1;
                    continue;
                }
                _ => {}
            }
        } else if ti == text.len() {
            return true;
        }

        // Mismatch: give the last star one more character and retry.
        match star {
            Some((star_pi, star_ti)) if star_ti < text.len() => {
                pi = star_pi;
                ti = star_ti + 1;
                star = Some((star_pi, ti));
            }
            _ => return false,
        }
    }
}

/// A parsed `[...]` set.
struct CharClass {
    negated: bool,
    singles: Vec<char>,
    ranges: Vec<(char, char)>,
}

impl CharClass {
    fn contains(&self, c: char) -> bool {
        let hit =
            self.singles.contains(&c) || self.ranges.iter().any(|&(lo, hi)| c >= lo && c <= hi);
        hit != self.negated
    }
}

/// Parses the class starting at `open`, returning it and the index of the
/// closing bracket. Returns None for an unterminated or empty class, which the
/// caller then treats as a literal `[`.
///
/// Only a leading '!' negates, matching CPython's fnmatch: `[^a]` is a
/// literal set {^, a}, not a negation. Both planes are pinned to the
/// incumbent's behaviour — treating '^' as negation made them deny and allow
/// exactly inverted path sets against it.
fn parse_class(pat: &[char], open: usize) -> Option<(CharClass, usize)> {
    let mut i = open + 1;
    let negated = pat.get(i) == Some(&'!');
    if negated {
        i += 1;
    }
    let body_start = i;
    // A ']' immediately after the opening (or after the negation) is a literal.
    if pat.get(i) == Some(&']') {
        i += 1;
    }
    while i < pat.len() && pat[i] != ']' {
        i += 1;
    }
    if i >= pat.len() {
        return None; // unterminated
    }
    let body = &pat[body_start..i];
    if body.is_empty() {
        return None;
    }

    let mut singles = Vec::new();
    let mut ranges = Vec::new();
    let mut k = 0;
    while k < body.len() {
        if k + 2 < body.len() && body[k + 1] == '-' {
            ranges.push((body[k], body[k + 2]));
            k += 3;
        } else {
            singles.push(body[k]);
            k += 1;
        }
    }
    Some((
        CharClass {
            negated,
            singles,
            ranges,
        },
        i,
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The cross-language parity gate. Both this crate and the Go matcher read
    /// the same CPython-generated fixture; if they drift, one of them fails.
    #[test]
    fn matches_cpython_fixture() {
        let fixture = include_str!("../../../testdata/fnmatch-parity.tsv");
        let mut checked = 0usize;
        for (lineno, line) in fixture.lines().enumerate() {
            if line.starts_with('#') || line.trim().is_empty() {
                continue;
            }
            let cols: Vec<&str> = line.split('\t').collect();
            assert_eq!(cols.len(), 3, "line {} is malformed: {line:?}", lineno + 1);
            let (pattern, name, want) = (cols[0], cols[1], cols[2] == "true");
            assert_eq!(
                matches(pattern, name),
                want,
                "matches({pattern:?}, {name:?}) disagrees with CPython (line {})",
                lineno + 1
            );
            checked += 1;
        }
        // A fixture that silently failed to load would make this test pass by
        // checking nothing.
        assert!(
            checked > 500,
            "only {checked} cases loaded from the fixture"
        );
    }

    #[test]
    fn star_crosses_separator() {
        assert!(matches("*.py", "src/deep/foo.py"));
        assert!(matches(".claude/*", ".claude/agents/x.md"));
    }

    #[test]
    fn double_star_requires_a_separator() {
        assert!(!matches("**/.env", ".env"));
        assert!(matches("**/.env", "a/b/.env"));
    }

    #[test]
    fn unterminated_class_is_a_literal_not_a_wildcard() {
        assert!(!matches("a[bc", "anything at all"));
        assert!(matches("a[bc", "a[bc"));
    }

    #[test]
    fn ranges_and_negation() {
        assert!(matches("f[a-c]o", "fbo"));
        assert!(!matches("f[a-c]o", "fzo"));
        assert!(matches("f[!a-c]o", "fzo"));
    }

    /// CPython's fnmatch negates only on '!': translate('[^a]') is the
    /// literal set {^, a}. Negating on '^' made this plane and the Go plane
    /// disagree with the incumbent about which paths a [^…] pattern named.
    #[test]
    fn caret_in_class_is_literal_not_negation() {
        assert!(matches("[^a]", "^"));
        assert!(matches("[^a]", "a"));
        assert!(!matches("[^a]", "b"));
        assert!(!matches(".env[^1]", ".env2"));
        assert!(matches("[!a]", "b"));
        assert!(!matches("[!a]", "a"));
    }

    /// Pathological star patterns must not blow up.
    #[test]
    fn many_stars_terminate() {
        let pattern = "*a*a*a*a*a*a*b";
        let name = "a".repeat(64);
        assert!(!matches(pattern, &name));
    }
}
