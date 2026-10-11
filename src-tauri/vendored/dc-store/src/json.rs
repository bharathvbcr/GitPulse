//! Enough of a JSON reader to prove a value is well formed.
//!
//! The store holds scope as JSON text and hands that text back to its callers
//! *raw* — embedded into the reply rather than quoted — because the consumer
//! expects an array and because the compare-and-swap on the appended-scope
//! column compares stored bytes. Re-encoding a value would change those bytes
//! and every widening would report as a conflict with itself.
//!
//! Embedding raw text is only safe if the text is provably one JSON value and
//! nothing else. It was not checked that way: the write accepted anything that
//! began with `[` and ended with `]`, so
//!
//! ```text
//! [{"path":"benign.go"}],"planned_files":[{"path":"**"}],"junk":[1]
//! ```
//!
//! passed, was stored verbatim, and was spliced into the `task` reply — where
//! it closed the array early and injected a second `planned_files` key. Go's
//! decoder takes the last occurrence of a duplicate key, so an executor's
//! self-widened `**` came back to the reporting side as scope the *planner*
//! authorised, which is precisely the confusion the planner/appended split
//! exists to prevent. A payload ending in `]` could also emit structurally
//! invalid JSON, denying every reader that task's scope until the column was
//! repaired out of band.
//!
//! So this validates rather than parses into values: a full scan of the
//! grammar, no allocation, no dependency, and — the point — no re-encoding, so
//! the byte-for-byte compare-and-swap contract survives. What the caller wrote
//! is what the store holds; the store just refuses to hold something that is
//! not a JSON array.

/// How deeply a value may nest before it is refused.
///
/// Bounded because the scan is recursive: scope entries are flat objects in a
/// flat array, and anything approaching this is a payload built to find the
/// stack rather than a plan. Refusing is the same answer as for any other
/// malformed input.
const MAX_DEPTH: usize = 32;

/// The whitespace JSON allows between tokens, and the only whitespace this
/// store trims anywhere — validation, storage and the compare-and-swap all use
/// this one definition. Rust's `str::trim` strips every Unicode space (a
/// non-breaking space, a form feed), SQLite's `trim()` only U+0020 and Go's
/// `bytes.TrimSpace` something in between; a value padded with a character one
/// of them strips and another does not was stored, then unreadable, or could
/// never be swapped.
pub const JSON_WS: [char; 4] = [' ', '\t', '\n', '\r'];

/// Trims JSON whitespace — and only that — from both ends.
pub fn trim_ws(text: &str) -> &str {
    text.trim_matches(JSON_WS)
}

/// Reports whether `text` is exactly one well-formed JSON array — no trailing
/// bytes, no second value, nothing after the closing bracket.
pub fn is_array(text: &str) -> bool {
    check_array(text).is_ok()
}

/// Like [`is_array`], but names what is wrong so a refusal can say why. The
/// bytes are validated exactly as given: padding is only legal where JSON
/// allows it, so a caller that trimmed first with a wider definition cannot
/// make an invalid column look valid.
pub fn check_array(text: &str) -> Result<(), String> {
    check_top(text, b'[')
}

/// Exactly one well-formed JSON object.
pub fn check_object(text: &str) -> Result<(), String> {
    check_top(text, b'{')
}

/// Exactly one well-formed JSON object or array.
pub fn check_container(text: &str) -> Result<(), String> {
    match text.trim_start_matches(JSON_WS).as_bytes().first() {
        Some(b'{') => check_top(text, b'{'),
        Some(b'[') => check_top(text, b'['),
        Some(c) => Err(format!("it starts with {:?}, not '{{' or '['", *c as char)),
        None => Err("it is empty".to_string()),
    }
}

/// The changes a planned file may declare, the same set the Go side's
/// `AllowedChange.Valid` accepts. An absent, null or empty value means
/// `modify`, as it does there.
pub const ALLOWED_CHANGES: [&str; 4] = ["create", "modify", "delete", "read_only"];

/// [`check_array`], and every entry is a planned file the Go decoder accepts:
/// a bare path string, or an object whose `allowed_change` is a string naming
/// one of [`ALLOWED_CHANGES`].
///
/// The raw text is stored and handed to Go, whose decoder refuses an unknown
/// change and so makes the whole task unreadable; refusing it at the write is
/// the same fix as for malformed JSON. Keys are matched the way Go matches
/// them — case-insensitively, after decoding escapes, every duplicate checked —
/// so none of those spellings reaches the field unchecked.
pub fn check_planned_files(text: &str) -> Result<(), String> {
    check_scan(text, b'[', true)
}

fn check_top(text: &str, open: u8) -> Result<(), String> {
    check_scan(text, open, false)
}

fn check_scan(text: &str, open: u8, planned: bool) -> Result<(), String> {
    let mut scanner = Scanner {
        bytes: text.as_bytes(),
        pos: 0,
        planned,
    };
    scanner.skip_ws();
    if scanner.peek() != Some(open) {
        return Err(match scanner.peek() {
            None => "it is empty".to_string(),
            Some(c) => format!("it starts with {:?}, not {:?}", c as char, open as char),
        });
    }
    scanner.value(0)?;
    scanner.skip_ws();
    if scanner.pos != scanner.bytes.len() {
        // The injection case: the value closed and something else followed.
        return Err(format!(
            "the value ends at byte {} and {} more byte(s) follow it",
            scanner.pos,
            scanner.bytes.len() - scanner.pos
        ));
    }
    Ok(())
}

struct Scanner<'a> {
    bytes: &'a [u8],
    pos: usize,
    /// Also hold the entries of the top-level array to the planned-file shape.
    planned: bool,
}

/// Decodes the escapes of a string body already proven well formed by
/// [`Scanner::string`].
fn unescape(raw: &str) -> String {
    let mut out = String::with_capacity(raw.len());
    let mut chars = raw.chars();
    while let Some(c) = chars.next() {
        if c != '\\' {
            out.push(c);
            continue;
        }
        match chars.next() {
            Some('b') => out.push('\u{8}'),
            Some('f') => out.push('\u{c}'),
            Some('n') => out.push('\n'),
            Some('r') => out.push('\r'),
            Some('t') => out.push('\t'),
            Some('u') => {
                let hex: String = chars.by_ref().take(4).collect();
                let unit = u32::from_str_radix(&hex, 16).unwrap_or(0xfffd);
                out.push(char::from_u32(unit).unwrap_or('\u{fffd}'));
            }
            Some(other) => out.push(other),
            None => {}
        }
    }
    out
}

impl Scanner<'_> {
    fn peek(&self) -> Option<u8> {
        self.bytes.get(self.pos).copied()
    }

    fn skip_ws(&mut self) {
        while matches!(self.peek(), Some(b' ' | b'\t' | b'\n' | b'\r')) {
            self.pos += 1;
        }
    }

    fn expect(&mut self, byte: u8) -> Result<(), String> {
        if self.peek() == Some(byte) {
            self.pos += 1;
            return Ok(());
        }
        Err(format!("expected {:?} at byte {}", byte as char, self.pos))
    }

    fn value(&mut self, depth: usize) -> Result<(), String> {
        if depth > MAX_DEPTH {
            return Err(format!("nested more than {MAX_DEPTH} levels deep"));
        }
        self.skip_ws();
        match self.peek() {
            Some(b'[') => self.array(depth),
            Some(b'{') => self.object(depth),
            Some(b'"') => self.string(),
            Some(b't') => self.literal("true"),
            Some(b'f') => self.literal("false"),
            Some(b'n') => self.literal("null"),
            Some(c) if c == b'-' || c.is_ascii_digit() => self.number(),
            Some(c) => Err(format!("unexpected {:?} at byte {}", c as char, self.pos)),
            None => Err("the value is unterminated".to_string()),
        }
    }

    fn array(&mut self, depth: usize) -> Result<(), String> {
        self.expect(b'[')?;
        self.skip_ws();
        if self.peek() == Some(b']') {
            self.pos += 1;
            return Ok(());
        }
        loop {
            if self.planned && depth == 0 {
                self.skip_ws();
                if !matches!(self.peek(), Some(b'{' | b'"')) {
                    return Err(format!(
                        "the entry at byte {} is neither a path string nor an object",
                        self.pos
                    ));
                }
            }
            self.value(depth + 1)?;
            self.skip_ws();
            match self.peek() {
                Some(b',') => self.pos += 1,
                Some(b']') => {
                    self.pos += 1;
                    return Ok(());
                }
                Some(c) => {
                    return Err(format!(
                        "expected ',' or ']' at byte {}, found {:?}",
                        self.pos, c as char
                    ));
                }
                None => return Err("the array is unterminated".to_string()),
            }
        }
    }

    fn object(&mut self, depth: usize) -> Result<(), String> {
        self.expect(b'{')?;
        self.skip_ws();
        if self.peek() == Some(b'}') {
            self.pos += 1;
            return Ok(());
        }
        loop {
            self.skip_ws();
            let key_at = self.pos;
            self.string()?;
            let key = unescape(&String::from_utf8_lossy(
                &self.bytes[key_at + 1..self.pos - 1],
            ));
            self.skip_ws();
            self.expect(b':')?;
            if self.planned && depth == 1 && key.eq_ignore_ascii_case("allowed_change") {
                self.allowed_change()?;
            } else {
                self.value(depth + 1)?;
            }
            self.skip_ws();
            match self.peek() {
                Some(b',') => self.pos += 1,
                Some(b'}') => {
                    self.pos += 1;
                    return Ok(());
                }
                Some(c) => {
                    return Err(format!(
                        "expected ',' or '}}' at byte {}, found {:?}",
                        self.pos, c as char
                    ));
                }
                None => return Err("the object is unterminated".to_string()),
            }
        }
    }

    /// The value of a planned file's `allowed_change`: null or a string, and a
    /// string must be empty or one of [`ALLOWED_CHANGES`].
    fn allowed_change(&mut self) -> Result<(), String> {
        self.skip_ws();
        match self.peek() {
            Some(b'n') => self.literal("null"),
            Some(b'"') => {
                let start = self.pos;
                self.string()?;
                let value = unescape(&String::from_utf8_lossy(
                    &self.bytes[start + 1..self.pos - 1],
                ));
                if value.is_empty() || ALLOWED_CHANGES.contains(&value.as_str()) {
                    Ok(())
                } else {
                    Err(format!(
                        "allowed_change {value:?} is not one of create, modify, delete, read_only"
                    ))
                }
            }
            _ => Err(format!(
                "allowed_change at byte {} is not a string",
                self.pos
            )),
        }
    }

    fn string(&mut self) -> Result<(), String> {
        self.expect(b'"')?;
        loop {
            match self.peek() {
                None => return Err("the string is unterminated".to_string()),
                Some(b'"') => {
                    self.pos += 1;
                    return Ok(());
                }
                Some(b'\\') => {
                    self.pos += 1;
                    match self.peek() {
                        Some(b'"' | b'\\' | b'/' | b'b' | b'f' | b'n' | b'r' | b't') => {
                            self.pos += 1
                        }
                        Some(b'u') => {
                            self.pos += 1;
                            for _ in 0..4 {
                                match self.peek() {
                                    Some(c) if c.is_ascii_hexdigit() => self.pos += 1,
                                    _ => {
                                        return Err(format!(
                                            "a \\u escape at byte {} is not four hex digits",
                                            self.pos
                                        ));
                                    }
                                }
                            }
                        }
                        _ => {
                            return Err(format!("an unknown escape at byte {}", self.pos));
                        }
                    }
                }
                // Raw control characters are not legal in a JSON string, and
                // letting one through here would produce a document the
                // consumer's decoder rejects — the failure this scan exists to
                // move to the write.
                Some(c) if c < 0x20 => {
                    return Err(format!(
                        "a raw control character (0x{c:02x}) at byte {}",
                        self.pos
                    ));
                }
                Some(_) => self.pos += 1,
            }
        }
    }

    fn literal(&mut self, word: &str) -> Result<(), String> {
        if self.bytes[self.pos..].starts_with(word.as_bytes()) {
            self.pos += word.len();
            return Ok(());
        }
        Err(format!("expected {word} at byte {}", self.pos))
    }

    fn number(&mut self) -> Result<(), String> {
        let start = self.pos;
        if self.peek() == Some(b'-') {
            self.pos += 1;
        }
        // A leading zero may not be followed by more digits: `01` is not JSON,
        // and accepting it here would mean the store holds text its consumer
        // refuses.
        match self.peek() {
            Some(b'0') => self.pos += 1,
            Some(c) if c.is_ascii_digit() => self.digits(),
            _ => return Err(format!("a number with no digits at byte {start}")),
        }
        if self.peek() == Some(b'.') {
            self.pos += 1;
            if !matches!(self.peek(), Some(c) if c.is_ascii_digit()) {
                return Err(format!("a fraction with no digits at byte {}", self.pos));
            }
            self.digits();
        }
        if matches!(self.peek(), Some(b'e' | b'E')) {
            self.pos += 1;
            if matches!(self.peek(), Some(b'+' | b'-')) {
                self.pos += 1;
            }
            if !matches!(self.peek(), Some(c) if c.is_ascii_digit()) {
                return Err(format!("an exponent with no digits at byte {}", self.pos));
            }
            self.digits();
        }
        Ok(())
    }

    fn digits(&mut self) {
        while matches!(self.peek(), Some(c) if c.is_ascii_digit()) {
            self.pos += 1;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The payload that made this module necessary. It passed the check it
    /// replaced — begins with `[`, ends with `]` — and closed the array early
    /// so a second `planned_files` key rode into the reply beside the real one.
    #[test]
    fn a_value_that_closes_its_array_and_keeps_going_is_not_an_array() {
        let injected = r#"[{"path":"benign.go","allowed_change":"modify"}],"planned_files":[{"path":"**","allowed_change":"modify"}],"junk":[1]"#;
        assert!(injected.starts_with('[') && injected.ends_with(']'));
        assert!(
            !is_array(injected),
            "the shape check's exact blind spot is still accepted"
        );
    }

    #[test]
    fn only_json_whitespace_is_trimmed_or_tolerated() {
        assert_eq!(trim_ws(" \t\r\n[]\n "), "[]");
        for padded in ["\u{a0}[]", "[]\u{a0}", "\x0c[]", "[]\x0c", "\u{2003}[]"] {
            assert_eq!(trim_ws(padded), padded, "{padded:?} was trimmed");
            assert!(check_array(padded).is_err(), "{padded:?} was accepted");
        }
    }

    #[test]
    fn object_and_container_checks_scan_the_whole_value() {
        assert!(check_object(r#"{"a":[1,{"b":null}]}"#).is_ok());
        assert!(check_object("{").is_err());
        assert!(check_object("[]").is_err());
        assert!(check_container("[]").is_ok() && check_container("{}").is_ok());
        assert!(check_container("{").is_err());
        assert!(check_container("1").is_err());
        assert!(check_container(r#"[]}],"gaps":[],"x":[{"q":0"#).is_err());
    }

    #[test]
    fn well_formed_arrays_are_accepted() {
        for good in [
            "[]",
            "  [ ]  ",
            r#"["src/a.go"]"#,
            r#"[{"path":"src/a.go","allowed_change":"modify"}]"#,
            r#"[1,-2.5,1e10,true,false,null,"x",[],{}]"#,
            r#"["a \"quoted\" path","tab\there","é"]"#,
        ] {
            assert!(
                is_array(good),
                "{good:?} was refused: {:?}",
                check_array(good)
            );
        }
    }

    #[test]
    fn malformed_values_are_refused_with_a_reason() {
        for bad in [
            "",
            "null",
            "{}",
            r#"{"path":"x"}"#,
            r#"["x""#,
            "[,]",
            "[1,]",
            "[01]",
            "[1.]",
            "[1e]",
            r#"["unterminated]"#,
            r#"["bad \q escape"]"#,
            r#"["bad \u00 escape"]"#,
            "[] []",
            "[]x",
        ] {
            assert!(check_array(bad).is_err(), "{bad:?} was accepted");
        }
    }

    /// A raw newline inside a string is not JSON, and letting it through here
    /// would mean the store holds text its own consumers reject.
    #[test]
    fn a_raw_control_character_in_a_string_is_refused() {
        assert!(check_array("[\"a\nb\"]").is_err());
    }

    /// The scan recurses, so nesting is bounded rather than left to the stack.
    #[test]
    fn nesting_past_the_bound_is_refused_rather_than_overflowing() {
        let deep = format!("{}{}", "[".repeat(2000), "]".repeat(2000));
        assert!(check_array(&deep).is_err());
        let shallow = format!("{}{}", "[".repeat(8), "]".repeat(8));
        assert!(is_array(&shallow), "an honest nested array was refused");
    }
}
