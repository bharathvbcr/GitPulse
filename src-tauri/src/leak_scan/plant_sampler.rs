//! Credential plants drawn from a rule's own regex.
//!
//! The recall measurement (`recall_tests`) needs one plant per Kingfisher
//! rule, and the plant has to be what the rule describes, not what someone
//! remembers the vendor's format to be. This samples a string from the rule's
//! pattern. It understands the subset of regex syntax the Kingfisher rule set
//! uses — groups (capturing, named, non-capturing, inline flags), alternation,
//! greedy and lazy quantifiers, bracket classes with ranges, Perl and POSIX
//! classes, `\xHH` escapes, assertions, and `(?x)` whitespace and comments —
//! and refuses everything else (lookaround, back-references, `\p{..}`, class
//! set operations) with a reason, so an unsupported rule is counted, never
//! silently dropped.
//!
//! The sampler is not trusted. Every candidate is checked against the pattern
//! with the `regex` crate, and then by Kingfisher itself, before it counts as
//! a plant; a sampler mistake costs yield, which the report shows, and never
//! correctness.
//!
//! Characters are drawn from each class by a seeded generator, so plants are
//! varied enough to pass the oracles' entropy filters and are the same on
//! every run.

use super::corpus_tests::Seeded;

/// How many times a repeated piece is emitted.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Length {
    /// Every repetition at its minimum. This is the comparable measurement:
    /// the shortest string the rule accepts.
    Min,
    /// Repetitions inside the secret (a capture group, or the whole match
    /// when the rule has none) at the top of a bounded range, or 16 past
    /// the floor of an unbounded one; everything else at its minimum. Used to
    /// tell "dc-redact has no shape" from "the minimum plant is shorter than
    /// any real token".
    Realistic,
}

impl Length {
    pub(super) fn label(self) -> &'static str {
        match self {
            Self::Min => "min",
            Self::Realistic => "realistic",
        }
    }
}

/// Characters a class may be sampled from when the pattern does not narrow
/// it: printable ASCII without the space.
fn printable() -> impl Iterator<Item = char> {
    '!'..='~'
}

fn word_chars() -> Vec<char> {
    ('a'..='z')
        .chain('A'..='Z')
        .chain('0'..='9')
        .chain(['_'])
        .collect()
}

fn digit_chars() -> Vec<char> {
    ('0'..='9').collect()
}

/// `\s`. Whitespace is sampled as a space: a plant is one readable line
/// wherever the rule allows it.
fn space_chars() -> Vec<char> {
    vec![' ']
}

fn is_space(c: char) -> bool {
    matches!(c, ' ' | '\t' | '\n' | '\r' | '\x0B' | '\x0C')
}

fn complement(set: &[char]) -> Vec<char> {
    printable().filter(|c| !set.contains(c)).collect()
}

#[derive(Debug, Clone)]
pub(super) enum Node {
    Lit(char),
    /// The characters a class may produce; never empty.
    Set(Vec<char>),
    Group {
        alts: Vec<Vec<Node>>,
        capture: bool,
    },
    Repeat {
        node: Box<Node>,
        min: u32,
        max: Option<u32>,
    },
    /// An assertion or an inline flag group: matches without consuming.
    Empty,
}

#[derive(Debug, Clone)]
pub(super) struct Pattern {
    root: Vec<Vec<Node>>,
    has_capture: bool,
}

/// Parse `pattern`, or say which construct the sampler does not support.
pub(super) fn parse(pattern: &str) -> Result<Pattern, String> {
    let mut p = Parser {
        s: pattern.chars().collect(),
        i: 0,
        captures: 0,
    };
    let root = p.alternation(false, true)?;
    Ok(Pattern {
        root,
        has_capture: p.captures > 0,
    })
}

/// Draw one string from `pattern`. `rng` decides every alternative and
/// character; `length` decides repetition counts.
pub(super) fn sample(pattern: &Pattern, rng: &mut Seeded, length: Length) -> String {
    let mut out = String::new();
    let whole_is_secret = !pattern.has_capture;
    emit_alts(&pattern.root, rng, length, whole_is_secret, &mut out);
    out
}

fn pick<'a, T>(items: &'a [T], rng: &mut Seeded) -> &'a T {
    &items[(rng.next() % items.len() as u64) as usize]
}

fn emit_alts(alts: &[Vec<Node>], rng: &mut Seeded, length: Length, secret: bool, out: &mut String) {
    for node in pick(alts, rng) {
        emit(node, rng, length, secret, out);
    }
}

fn emit(node: &Node, rng: &mut Seeded, length: Length, secret: bool, out: &mut String) {
    match node {
        Node::Lit(c) => out.push(*c),
        Node::Set(chars) => out.push(*pick(chars, rng)),
        Node::Group { alts, capture } => emit_alts(alts, rng, length, secret || *capture, out),
        Node::Repeat { node, min, max } => {
            let count = match (length, secret, max) {
                (Length::Realistic, true, Some(max)) => (*max).min(min.saturating_add(256)),
                (Length::Realistic, true, None) => min.saturating_add(16),
                _ => *min,
            };
            for _ in 0..count {
                emit(node, rng, length, secret, out);
            }
        }
        Node::Empty => {}
    }
}

struct Parser {
    s: Vec<char>,
    i: usize,
    captures: usize,
}

impl Parser {
    fn peek(&self) -> Option<char> {
        self.s.get(self.i).copied()
    }

    fn peek_at(&self, ahead: usize) -> Option<char> {
        self.s.get(self.i + ahead).copied()
    }

    fn bump(&mut self) -> Option<char> {
        let c = self.peek();
        if c.is_some() {
            self.i += 1;
        }
        c
    }

    fn eat(&mut self, c: char) -> bool {
        if self.peek() == Some(c) {
            self.i += 1;
            true
        } else {
            false
        }
    }

    /// `(?x)`: whitespace is insignificant and `#` starts a comment, inside
    /// bracket classes too (as the `regex` crate reads it).
    fn skip_extended(&mut self, x: bool) {
        if !x {
            return;
        }
        while let Some(c) = self.peek() {
            if is_space(c) {
                self.i += 1;
            } else if c == '#' {
                while let Some(c) = self.bump() {
                    if c == '\n' {
                        break;
                    }
                }
            } else {
                break;
            }
        }
    }

    fn alternation(&mut self, mut x: bool, top: bool) -> Result<Vec<Vec<Node>>, String> {
        let mut alts: Vec<Vec<Node>> = vec![Vec::new()];
        loop {
            self.skip_extended(x);
            let Some(c) = self.peek() else {
                return if top {
                    Ok(alts)
                } else {
                    Err("unclosed group".into())
                };
            };
            match c {
                ')' => {
                    if top {
                        return Err("unbalanced `)`".into());
                    }
                    self.i += 1;
                    return Ok(alts);
                }
                '|' => {
                    self.i += 1;
                    alts.push(Vec::new());
                }
                '*' | '+' | '?' => {
                    self.i += 1;
                    let (min, max) = match c {
                        '*' => (0, None),
                        '+' => (1, None),
                        _ => (0, Some(1)),
                    };
                    self.quantify(alts.last_mut().expect("one alternative"), min, max)?;
                }
                '{' if self.counted_quantifier_ahead() => {
                    let (min, max) = self.counted_quantifier()?;
                    self.quantify(alts.last_mut().expect("one alternative"), min, max)?;
                }
                _ => {
                    let node = self.atom(&mut x)?;
                    alts.last_mut().expect("one alternative").push(node);
                }
            }
        }
    }

    fn quantify(&mut self, seq: &mut Vec<Node>, min: u32, max: Option<u32>) -> Result<(), String> {
        // Lazy (`?`) changes which match is found, not what can match.
        self.eat('?');
        if self.peek() == Some('+') {
            return Err("possessive quantifier".into());
        }
        match seq.pop() {
            None | Some(Node::Empty) => Err("quantifier with nothing to repeat".into()),
            Some(node) => {
                seq.push(Node::Repeat {
                    node: Box::new(node),
                    min,
                    max,
                });
                Ok(())
            }
        }
    }

    fn counted_quantifier_ahead(&self) -> bool {
        let mut j = self.i + 1;
        let mut digits = 0;
        while let Some(c) = self.s.get(j) {
            match c {
                '0'..='9' => digits += 1,
                ',' => {}
                '}' => return digits > 0,
                _ => return false,
            }
            j += 1;
        }
        false
    }

    fn counted_quantifier(&mut self) -> Result<(u32, Option<u32>), String> {
        self.i += 1; // `{`
        let mut text = String::new();
        while let Some(c) = self.bump() {
            if c == '}' {
                break;
            }
            text.push(c);
        }
        let number = |s: &str| {
            s.parse::<u32>()
                .map_err(|_| format!("bad repetition `{{{text}}}`"))
        };
        match text.split_once(',') {
            None => {
                let n = number(&text)?;
                Ok((n, Some(n)))
            }
            Some((lo, "")) => Ok((number(lo)?, None)),
            Some((lo, hi)) => {
                let (lo, hi) = (number(lo)?, number(hi)?);
                if hi < lo {
                    return Err(format!("bad repetition `{{{text}}}`"));
                }
                Ok((lo, Some(hi)))
            }
        }
    }

    fn atom(&mut self, x: &mut bool) -> Result<Node, String> {
        let c = self.bump().expect("caller peeked");
        match c {
            '(' => self.group(x),
            '[' => Ok(Node::Set(self.class(*x)?)),
            '.' => Ok(Node::Set(word_chars())),
            '^' | '$' => Ok(Node::Empty),
            '\\' => self.escape(),
            c => Ok(Node::Lit(c)),
        }
    }

    fn group(&mut self, x: &mut bool) -> Result<Node, String> {
        let mut capture = true;
        let mut inner_x = *x;
        if self.eat('?') {
            match self.peek() {
                Some(':') => {
                    self.i += 1;
                    capture = false;
                }
                Some('P') if self.peek_at(1) == Some('<') => {
                    self.i += 2;
                    self.group_name()?;
                }
                Some('<') if !matches!(self.peek_at(1), Some('=' | '!')) => {
                    self.i += 1;
                    self.group_name()?;
                }
                Some('=' | '!' | '<') => return Err("lookaround".into()),
                _ => {
                    // Inline flags: `(?flags)` for the rest of this group,
                    // or `(?flags:...)` for its own body.
                    let mut on = true;
                    let mut new_x = *x;
                    loop {
                        match self.bump() {
                            Some('-') => on = false,
                            Some('x') => new_x = on,
                            Some('i' | 's' | 'm' | 'U' | 'u' | 'R') => {}
                            Some(')') => {
                                *x = new_x;
                                return Ok(Node::Empty);
                            }
                            Some(':') => {
                                inner_x = new_x;
                                capture = false;
                                break;
                            }
                            other => return Err(format!("unsupported group flag {other:?}")),
                        }
                    }
                }
            }
        }
        if capture {
            self.captures += 1;
        }
        let alts = self.alternation(inner_x, false)?;
        Ok(Node::Group { alts, capture })
    }

    fn group_name(&mut self) -> Result<(), String> {
        while let Some(c) = self.bump() {
            if c == '>' {
                return Ok(());
            }
        }
        Err("unclosed group name".into())
    }

    fn hex_escape(&mut self) -> Result<char, String> {
        let digits: String = if self.eat('{') {
            let mut d = String::new();
            loop {
                match self.bump() {
                    Some('}') => break,
                    Some(c) => d.push(c),
                    None => return Err("unclosed `\\x{`".into()),
                }
            }
            d
        } else {
            let d: String = self.s[self.i..].iter().take(2).collect();
            self.i += d.chars().count();
            d
        };
        u32::from_str_radix(&digits, 16)
            .ok()
            .and_then(char::from_u32)
            .ok_or_else(|| format!("bad hex escape `{digits}`"))
    }

    /// An escape outside a class.
    fn escape(&mut self) -> Result<Node, String> {
        let c = self.bump().ok_or("trailing backslash")?;
        Ok(match c {
            'b' | 'B' | 'A' | 'z' => Node::Empty,
            _ => match self.class_escape(c)? {
                Escaped::Char(c) => Node::Lit(c),
                Escaped::Set(set) => Node::Set(set),
            },
        })
    }

    /// An escape that means the same inside and outside a class.
    fn class_escape(&mut self, c: char) -> Result<Escaped, String> {
        Ok(match c {
            'd' => Escaped::Set(digit_chars()),
            'w' => Escaped::Set(word_chars()),
            's' => Escaped::Set(space_chars()),
            'D' => Escaped::Set(complement(&digit_chars())),
            'W' => Escaped::Set(complement(&word_chars())),
            'S' => Escaped::Set(printable().collect()),
            't' => Escaped::Char('\t'),
            'n' => Escaped::Char('\n'),
            'r' => Escaped::Char('\r'),
            'f' => Escaped::Char('\x0C'),
            'v' => Escaped::Char('\x0B'),
            'x' => Escaped::Char(self.hex_escape()?),
            'p' | 'P' => return Err("unicode class".into()),
            '0'..='9' => return Err("back-reference or octal escape".into()),
            c if c.is_ascii_alphabetic() => return Err(format!("unsupported escape `\\{c}`")),
            c => Escaped::Char(c),
        })
    }

    /// A bracket class, after its `[`. Returns the characters it allows.
    fn class(&mut self, x: bool) -> Result<Vec<char>, String> {
        let negated = self.eat('^');
        let mut set: Vec<char> = Vec::new();
        let mut first = true;
        loop {
            self.skip_extended(x);
            let c = self.bump().ok_or("unclosed class")?;
            match c {
                ']' if !first => break,
                '[' if self.peek() == Some(':') => set.extend(self.posix_class()?),
                '[' => set.extend(self.class(x)?),
                '&' | '~' if self.peek() == Some(c) => return Err("class set operation".into()),
                '-' if self.peek() == Some('-') => return Err("class set operation".into()),
                _ => {
                    let lo = if c == '\\' {
                        let e = self.bump().ok_or("trailing backslash")?;
                        match self.class_escape(e)? {
                            Escaped::Set(s) => {
                                set.extend(s);
                                first = false;
                                continue;
                            }
                            Escaped::Char(c) => c,
                        }
                    } else {
                        c
                    };
                    // A range, unless the `-` is the class's last character.
                    if self.peek() == Some('-') && !matches!(self.peek_at(1), Some(']') | None) {
                        self.i += 1;
                        let hi = match self.bump().ok_or("unclosed class")? {
                            '\\' => {
                                let e = self.bump().ok_or("trailing backslash")?;
                                match self.class_escape(e)? {
                                    Escaped::Char(c) => c,
                                    Escaped::Set(_) => return Err("range to a class".into()),
                                }
                            }
                            c => c,
                        };
                        if hi < lo {
                            return Err("reversed class range".into());
                        }
                        set.extend(lo..=hi);
                    } else {
                        set.push(lo);
                    }
                }
            }
            first = false;
        }
        set.sort_unstable();
        set.dedup();
        let set = if negated {
            complement(&set)
        } else {
            // Whitespace other than a space only where the class insists.
            if set.contains(&' ') {
                set.retain(|c| !is_space(*c) || *c == ' ');
            }
            set
        };
        if set.is_empty() {
            return Err("class allows no printable character".into());
        }
        Ok(set)
    }

    fn posix_class(&mut self) -> Result<Vec<char>, String> {
        self.i += 1; // `:`
        let mut name = String::new();
        loop {
            match self.bump() {
                Some(':') if self.eat(']') => break,
                Some(c) => name.push(c),
                None => return Err("unclosed POSIX class".into()),
            }
        }
        Ok(match name.as_str() {
            "alnum" => ('a'..='z').chain('A'..='Z').chain('0'..='9').collect(),
            "alpha" => ('a'..='z').chain('A'..='Z').collect(),
            "digit" => digit_chars(),
            "xdigit" => ('0'..='9').chain('a'..='f').chain('A'..='F').collect(),
            "upper" => ('A'..='Z').collect(),
            "lower" => ('a'..='z').collect(),
            "word" => word_chars(),
            "space" => space_chars(),
            other => return Err(format!("POSIX class `{other}`")),
        })
    }
}

enum Escaped {
    Char(char),
    Set(Vec<char>),
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Sample `pattern` under both length policies and several seeds; every
    /// string must match the pattern as the `regex` crate reads it.
    fn assert_samples_match(pattern: &str) {
        let parsed = parse(pattern).unwrap_or_else(|e| panic!("{pattern}: {e}"));
        let re = regex::Regex::new(pattern).expect("test pattern compiles");
        for seed in 0..16u64 {
            for length in [Length::Min, Length::Realistic] {
                let s = sample(&parsed, &mut Seeded(seed), length);
                assert!(re.is_match(&s), "{pattern} under {length:?}, seed {seed}");
            }
        }
    }

    #[test]
    fn samples_match_the_constructs_the_rule_set_uses() {
        for pattern in [
            r"\bpre_[A-Za-z0-9]{36}\b",
            r"(?i)(?:vendor)(?:[ \t\w.-]{0,20})[\s'\x22]{0,3}(?:=|>|:{1,3}=|\|\||:|=>|\?=|,)[\x60'\x22\s=]{0,5}([a-z0-9_-]{32})(?:\\?['\x22\x60]|[\s;]|\\[nr]|$)",
            r"(?P<TOKEN>\bhttps://(?P<USER>[^:\s]+):(?P<PASS>[^\s@]+)@host\.example)",
            r"\b(tok[[:alnum:]]{14}\.[a-f0-9]{64})\b",
            r"(?s)(?i)-----BEGIN[ A-Z0-9_-]{0,100}PRIVATE KEY(?: BLOCK)?-----[\s\S-]*?-----END[ A-Z0-9_-]{0,100}PRIVATE KEY(?: BLOCK)?-----",
            r"pre-v[0-9]+-[A-Za-z0-9_-]{20,}",
            r"x{2}y{1,}z?",
            r"[\w${}@.-]+:\d{4}\z",
            "(?xi)\n  (?:^|[;\\s'\"=(])   # leading context\n  (?:server|host) [\\t\\x20]* = [\\t\\x20]*\n  ([a-z0-9]{12,16})\n",
            r#""(?i)(?:Authorization:[ \t]{0,5}(?:Bearer)[ \t]([\w=~@.+/-]{8,}))""#,
        ] {
            assert_samples_match(pattern);
        }
    }

    #[test]
    fn same_seed_same_plant() {
        let p = parse(r"pre_[A-Za-z0-9]{36}").unwrap();
        let a = sample(&p, &mut Seeded(7), Length::Min);
        let b = sample(&p, &mut Seeded(7), Length::Min);
        let c = sample(&p, &mut Seeded(8), Length::Min);
        assert_eq!(a, b);
        assert_ne!(a, c);
    }

    #[test]
    fn realistic_length_grows_only_the_secret() {
        let p = parse(r"key[ ]{0,8}=([a-f0-9]{16,40})").unwrap();
        let min = sample(&p, &mut Seeded(1), Length::Min);
        let real = sample(&p, &mut Seeded(1), Length::Realistic);
        assert_eq!(min.len(), "key=".len() + 16);
        assert_eq!(real.len(), "key=".len() + 40);

        let unbounded = parse(r"tok_[a-z]{20,}").unwrap();
        let real = sample(&unbounded, &mut Seeded(1), Length::Realistic);
        assert_eq!(real.len(), "tok_".len() + 36);
    }

    #[test]
    fn unsupported_syntax_is_refused_with_a_reason() {
        for (pattern, reason) in [
            (r"(?=abc)x", "lookaround"),
            (r"(?<!a)b", "lookaround"),
            (r"(a)\1", "back-reference"),
            (r"\p{Greek}", "unicode class"),
            (r"[a-z&&[^aeiou]]", "class set operation"),
            (r"(abc", "unclosed group"),
            (r"*a", "nothing to repeat"),
        ] {
            let err = parse(pattern).expect_err(pattern);
            assert!(err.contains(reason), "{pattern}: {err}");
        }
    }
}
