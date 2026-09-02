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

/// Reports whether `text` is exactly one well-formed JSON array — no trailing
/// bytes, no second value, nothing after the closing bracket.
pub fn is_array(text: &str) -> bool {
    check_array(text).is_ok()
}

/// Like [`is_array`], but names what is wrong so a refusal can say why.
pub fn check_array(text: &str) -> Result<(), String> {
    let mut scanner = Scanner {
        bytes: text.as_bytes(),
        pos: 0,
    };
    scanner.skip_ws();
    if scanner.peek() != Some(b'[') {
        return Err(match scanner.peek() {
            None => "it is empty".to_string(),
            Some(c) => format!("it starts with {:?}, not '['", c as char),
        });
    }
    scanner.value(0)?;
    scanner.skip_ws();
    if scanner.pos != scanner.bytes.len() {
        // The injection case: the array closed and something else followed.
        return Err(format!(
            "the array ends at byte {} and {} more byte(s) follow it",
            scanner.pos,
            scanner.bytes.len() - scanner.pos
        ));
    }
    Ok(())
}

struct Scanner<'a> {
    bytes: &'a [u8],
    pos: usize,
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
            self.string()?;
            self.skip_ws();
            self.expect(b':')?;
            self.value(depth + 1)?;
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
