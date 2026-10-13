//! Python `fnmatch` semantics, implemented with no dependencies.
//!
//! This is the Rust half of the matcher `backend/go_orchestrator/fnmatch` also
//! runs. Both read one fixture, `testdata/fnmatch-parity.tsv`, generated from
//! CPython's `fnmatchcase` by `scripts/gen-fnmatch-parity.py`. The failure mode
//! of two copies is not a crash — it is the two planes disagreeing about
//! whether a path is inside the write gate, in one language only, silently.
//!
//! The copies stay in-process on purpose. The write gate cannot call this crate
//! across a transport (a failure has no honest bool), and `dc-verify` cannot
//! call the Go package. The fixture is the lock.
//!
//! The rule that matters and that most glob crates get differently: `*` matches
//! path separators. `fnmatch("src/foo.py", "*.py")` is true in Python and false
//! under shell-style globbing. Every DevCouncil path rule is written in the
//! Python dialect.

mod secret_paths;
mod test_paths;

pub use secret_paths::{SECRET_PATH_PATTERNS, is_secret_path};
pub use test_paths::{in_test_directory, is_test_file_name, is_test_path, parse_test_path_rows};

/// Longest pattern or name, in Unicode scalar values, matched exactly.
///
/// Past this, [`matches`] returns false without walking. `dc-verify` uses that
/// as "not a planned file", which blocks the change. A real path is far below
/// the cap, and the cap is what keeps `O(pattern × name)` from being a hang.
const MAX_UNITS: usize = 16_384;

/// Returns true when `name` matches the fnmatch `pattern`.
///
/// Matching is case-sensitive, matching Python's `fnmatchcase`.
pub fn matches(pattern: &str, name: &str) -> bool {
    try_matches(pattern, name).unwrap_or(false)
}

/// [`matches`], saying when it could not decide.
///
/// `None` means the input was past [`MAX_UNITS`] or the walk exhausted its
/// step budget. [`matches`] reads that as false; Go's `fnmatch` reads it as
/// false for `Match` and as true for `MatchFold`, the deny-list entry point,
/// where a false would let a write through. A caller answering for either one
/// needs the distinction, which a bool erases.
pub fn try_matches(pattern: &str, name: &str) -> Option<bool> {
    let pattern = Pattern::compile(pattern)?;
    pattern.try_matches(&Name::new(name)?)
}

/// A name read once, for matching against many patterns.
///
/// [`Name::new`] is `None` past [`MAX_UNITS`], which every match reads as
/// undecided.
pub struct Name {
    text: Vec<char>,
}

impl Name {
    /// Reads `name`, or `None` when it is past [`MAX_UNITS`].
    pub fn new(name: &str) -> Option<Self> {
        // Reject on bytes before allocating a `Vec<char>` for a multi-megabyte input.
        if name.len() > MAX_UNITS * 4 {
            return None;
        }
        let text: Vec<char> = name.chars().collect();
        if text.len() > MAX_UNITS {
            return None;
        }
        Some(Self { text })
    }
}

/// A pattern parsed once, for matching against many names.
///
/// It is the only matcher: [`try_matches`] compiles and matches, so a
/// compiled pattern answers exactly what the one-shot call answers,
/// including where the step budget runs out.
pub struct Pattern {
    tokens: Vec<Token>,
    /// The pattern's length in Unicode scalar values. The step budget is
    /// computed from it, not from the token count, so a budget runs out at
    /// the same place it always has.
    units: usize,
}

enum Token {
    /// A run of one or more `*`.
    Star,
    /// `?`.
    Any,
    /// A terminated, non-empty `[...]`.
    Class(CharClass),
    /// Any other character, including a `[` that opens no class.
    Literal(char),
}

impl Pattern {
    /// Parses `pattern`, or `None` when it is past [`MAX_UNITS`].
    pub fn compile(pattern: &str) -> Option<Self> {
        if pattern.len() > MAX_UNITS * 4 {
            return None;
        }
        let pat: Vec<char> = pattern.chars().collect();
        if pat.len() > MAX_UNITS {
            return None;
        }
        let mut tokens = Vec::new();
        let mut pi = 0;
        while pi < pat.len() {
            match pat[pi] {
                '*' => {
                    while pi < pat.len() && pat[pi] == '*' {
                        pi += 1;
                    }
                    tokens.push(Token::Star);
                }
                '?' => {
                    tokens.push(Token::Any);
                    pi += 1;
                }
                '[' => match parse_class(&pat, pi) {
                    Some((class, close)) => {
                        tokens.push(Token::Class(class));
                        pi = close + 1;
                    }
                    None => {
                        // Unterminated '[' is a literal, never a wildcard.
                        tokens.push(Token::Literal('['));
                        pi += 1;
                    }
                },
                c => {
                    tokens.push(Token::Literal(c));
                    pi += 1;
                }
            }
        }
        Some(Self {
            tokens,
            units: pat.len(),
        })
    }

    /// [`try_matches`] for this pattern.
    pub fn try_matches(&self, name: &Name) -> Option<bool> {
        match_tokens(&self.tokens, self.units, &name.text)
    }
}

/// Returns true when `name` matches any of `patterns`.
pub fn matches_any<S: AsRef<str>>(patterns: &[S], name: &str) -> bool {
    patterns.iter().any(|p| matches(p.as_ref(), name))
}

/// Backtracking matcher over compiled tokens.
///
/// `*` is handled with a saved-position loop rather than recursion per
/// character, so a pattern of many stars against a long path stays linear in
/// the common case instead of exponential. Each loop turn is one step: one
/// token consumed, or the latest star given one more character. That is the
/// step the pattern-text walker counted, token for token, so the budget below
/// is the one it always was.
fn match_tokens(tokens: &[Token], units: usize, text: &[char]) -> Option<bool> {
    let (mut pi, mut ti) = (0usize, 0usize);
    // Saved backtrack point: the token after the star we most recently
    // expanded, and how far the text pointer had advanced when we chose that
    // expansion.
    let mut star: Option<(usize, usize)> = None;
    let mut steps = 0usize;
    // Four times the one-step-per-unit bound. Slack for star-collapse
    // iterations, not a second algorithm. Past it we fail closed.
    let limit = (units + 1)
        .saturating_mul(text.len() + 1)
        .saturating_mul(4)
        .saturating_add(8);

    loop {
        steps += 1;
        if steps > limit {
            return None;
        }
        if pi < tokens.len() {
            let advanced = match &tokens[pi] {
                Token::Star => {
                    // "**" is one run, which is why "**/.env" still
                    // requires a separator before ".env".
                    pi += 1;
                    star = Some((pi, ti));
                    continue;
                }
                Token::Any => ti < text.len(),
                Token::Class(class) => ti < text.len() && class.contains(text[ti]),
                Token::Literal(c) => ti < text.len() && text[ti] == *c,
            };
            if advanced {
                pi += 1;
                ti += 1;
                continue;
            }
        } else if ti == text.len() {
            return Some(true);
        }

        // Mismatch: give the last star one more character and retry.
        match star {
            Some((star_pi, star_ti)) if star_ti < text.len() => {
                pi = star_pi;
                ti = star_ti + 1;
                star = Some((star_pi, ti));
            }
            _ => return Some(false),
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

    if body.contains(&'-') {
        return Some((dashed_class(negated, body), i));
    }
    Some((
        CharClass {
            negated,
            singles: body.to_vec(),
            ranges: Vec::new(),
        },
        i,
    ))
}

/// Reads a set that holds a `-` the way CPython's `fnmatch.translate` does,
/// which is not the way a regex parser would read the text: it cuts the set into
/// chunks at each `-` that could be a range operator, drops every backwards
/// range by merging the two chunks around it — taking both endpoints — and only
/// then hands the joined chunks to the regex engine. `[]--!]` loses the range
/// `]`-`-` and the `!` after it, leaving a negation with no members, which
/// matches any one character. The Go twin is `dashedClass`.
fn dashed_class(negated: bool, body: &[char]) -> CharClass {
    let mut stuff: Vec<char> = Vec::with_capacity(body.len() + 1);
    if negated {
        stuff.push('!');
    }
    stuff.extend_from_slice(body);
    let mut k = if negated { 2 } else { 1 };
    let mut chunks: Vec<Vec<char>> = Vec::new();
    let mut start = 0;
    loop {
        let found = (k..stuff.len()).find(|&idx| stuff[idx] == '-');
        let Some(idx) = found else { break };
        chunks.push(stuff[start..idx].to_vec());
        start = idx + 1;
        k = idx + 3;
    }
    if start < stuff.len() {
        chunks.push(stuff[start..].to_vec());
    } else if let Some(last) = chunks.last_mut() {
        last.push('-');
    }
    let mut k = chunks.len();
    while k > 1 {
        k -= 1;
        // CPython raises IndexError when a merge empties a chunk that an
        // earlier pair then reads; such a pair is left as it is.
        let (Some(&end), Some(&first)) = (chunks[k - 1].last(), chunks[k].first()) else {
            continue;
        };
        if end > first {
            let mut merged = chunks[k - 1][..chunks[k - 1].len() - 1].to_vec();
            merged.extend_from_slice(&chunks[k][1..]);
            chunks[k - 1] = merged;
            chunks.remove(k);
        }
    }
    // (char, is_range_operator)
    let mut tokens: Vec<(char, bool)> = Vec::new();
    for (n, chunk) in chunks.iter().enumerate() {
        if n > 0 {
            tokens.push(('-', true));
        }
        tokens.extend(chunk.iter().map(|&c| (c, false)));
    }
    let mut class = CharClass {
        negated: false,
        singles: Vec::new(),
        ranges: Vec::new(),
    };
    let mut t = 0;
    if matches!(tokens.first(), Some(&('!', false))) {
        class.negated = true;
        t = 1;
    }
    while t < tokens.len() {
        if t + 2 < tokens.len() && tokens[t + 1].1 {
            class.ranges.push((tokens[t].0, tokens[t + 2].0));
            t += 3;
        } else {
            class.singles.push(tokens[t].0);
            t += 1;
        }
    }
    class
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The pattern-text walker the compiled matcher replaced, kept as the
    /// oracle it is held equal to, step budget included.
    ///
    /// `*` is handled with a saved-position loop rather than recursion per
    /// character, so a pattern of many stars against a long path stays linear in
    /// the common case instead of exponential.
    fn reference_match(pat: &[char], text: &[char]) -> Option<bool> {
        let (mut pi, mut ti) = (0usize, 0usize);
        // Saved backtrack point: the star we most recently expanded, and how far
        // the text pointer had advanced when we chose that expansion.
        let mut star: Option<(usize, usize)> = None;
        // Each step consumes one pattern unit or gives the latest star one more
        // character. Exceeding this is a loop bug; failing closed beats hanging.
        let mut steps = 0usize;
        // Four times the one-step-per-unit bound. Slack for star-collapse
        // iterations, not a second algorithm. Past it we fail closed.
        let limit = (pat.len() + 1)
            .saturating_mul(text.len() + 1)
            .saturating_mul(4)
            .saturating_add(8);

        loop {
            steps += 1;
            if steps > limit {
                return None;
            }
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
                return Some(true);
            }

            // Mismatch: give the last star one more character and retry.
            match star {
                Some((star_pi, star_ti)) if star_ti < text.len() => {
                    pi = star_pi;
                    ti = star_ti + 1;
                    star = Some((star_pi, ti));
                }
                _ => return Some(false),
            }
        }
    }

    fn reference(pattern: &str, name: &str) -> Option<bool> {
        if pattern.len() > MAX_UNITS * 4 || name.len() > MAX_UNITS * 4 {
            return None;
        }
        let pat: Vec<char> = pattern.chars().collect();
        let text: Vec<char> = name.chars().collect();
        if pat.len() > MAX_UNITS || text.len() > MAX_UNITS {
            return None;
        }
        reference_match(&pat, &text)
    }

    /// Every pattern over a small alphabet that exercises stars, classes,
    /// negation, ranges and unterminated brackets, against every short name,
    /// answers what the pattern-text walker answered.
    #[test]
    fn compiled_matches_the_reference_walker() {
        let alphabet = ['a', 'b', '*', '?', '[', ']', '!', '-', '/'];
        let names = [
            "", "a", "b", "ab", "ba", "a/b", "[", "]", "-", "!", "aab", "a[b", "abab/b",
        ];
        let mut patterns = vec![String::new()];
        let mut frontier = vec![String::new()];
        for _ in 0..4 {
            let mut next = Vec::new();
            for p in &frontier {
                for c in alphabet {
                    let mut q = p.clone();
                    q.push(c);
                    next.push(q);
                }
            }
            patterns.extend(next.iter().cloned());
            frontier = next;
        }
        let mut checked = 0usize;
        for p in &patterns {
            for n in names {
                assert_eq!(
                    try_matches(p, n),
                    reference(p, n),
                    "pattern {p:?} name {n:?}"
                );
                checked += 1;
            }
        }
        assert!(checked > 80_000, "only {checked} pairs compared");
    }

    /// Heavy backtracking agrees with the pattern-text walker. The step
    /// budget is a loop-bug guard, and a correct walk of either kind stays
    /// under it, so these exercise the star backtrack rather than the budget.
    #[test]
    fn compiled_backtracking_matches_the_reference() {
        for (pattern, name) in [
            (
                "*".to_string() + &"a".repeat(2000) + "b",
                "a".repeat(16_000),
            ),
            ("*a".repeat(40) + "*b", "a".repeat(3_000)),
            ("*[a]".repeat(20) + "*b", "a".repeat(2_000) + "b"),
            ("*?*?*[!a]".to_string(), "a".repeat(500) + "b"),
        ] {
            assert_eq!(
                try_matches(&pattern, &name),
                reference(&pattern, &name),
                "pattern {:.30?}",
                pattern
            );
        }
    }

    #[test]
    fn a_compiled_pattern_answers_many_names() {
        let p = Pattern::compile("src/*.[gr][os]").unwrap();
        for (name, want) in [
            ("src/a.go", true),
            ("src/b/c.rs", true),
            ("src/a.py", false),
        ] {
            assert_eq!(
                p.try_matches(&Name::new(name).unwrap()),
                Some(want),
                "{name}"
            );
        }
        assert!(Name::new(&"a".repeat(MAX_UNITS + 1)).is_none());
        assert!(Pattern::compile(&"a".repeat(MAX_UNITS + 1)).is_none());
    }

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
            fixture.contains("[c-a-e]\te\ttrue\n"),
            "parity fixture is missing the inverted-range row"
        );
        assert!(
            checked >= 800,
            "only {checked} cases loaded from the fixture"
        );
    }

    #[test]
    fn try_matches_says_when_it_could_not_decide() {
        assert_eq!(try_matches("*.py", "a.py"), Some(true));
        assert_eq!(try_matches("*.py", "a.rs"), Some(false));
        let long = "a".repeat(MAX_UNITS + 1);
        assert_eq!(try_matches(&long, "a"), None);
        assert_eq!(try_matches("a", &long), None);
        assert!(
            !matches(&long, "a"),
            "matches keeps reading undecided as false"
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
        let pattern = "*a".repeat(32) + "*b";
        let name = "a".repeat(2_000);
        let started = std::time::Instant::now();
        assert!(!matches(&pattern, &name));
        assert!(
            started.elapsed() < std::time::Duration::from_secs(2),
            "many-star match took {:?}; the backtrack is no longer bounded",
            started.elapsed()
        );
        assert!(matches(&pattern, &(name + "b")));
    }

    /// CPython keeps the members around an out-of-order range. A regexp
    /// translation of the same class fails to compile and matches nothing.
    #[test]
    fn inverted_ranges_keep_the_members_cpython_keeps() {
        assert!(matches("[c-a-e]", "e"));
        assert!(matches("[c-a-e]", "-"));
        assert!(!matches("[c-a-e]", "a"));
        assert!(matches("[a--c]", "c"));
        assert!(!matches("[a--c]", "b"));
        assert!(!matches("[z-a]", "z"));
        assert!(!matches("*[z-a]", "hello"));
    }

    #[test]
    fn oversize_inputs_return_false_without_walking() {
        let name = "a".repeat(MAX_UNITS + 1);
        assert!(!matches("*", &name));
        assert!(matches("*", &"a".repeat(MAX_UNITS)));
        let huge = "💩".repeat(MAX_UNITS + 1);
        assert!(!matches("*", &huge));
    }
}
