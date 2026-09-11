//! Does a stream hold *exactly one* JSON value, and nothing else?
//!
//! DevCouncil's `--json` contract is one JSON object on stdout with every
//! diagnostic on stderr. Two failure shapes break it and a naive check catches
//! neither reliably:
//!
//! * a human line printed *before* or *after* the payload, so stdout has one
//!   object plus garbage; and
//! * an early exit that prints a message and returns without a payload, so
//!   stdout has zero objects.
//!
//! "Find the first `{` and parse from there" — the workaround the Python tests
//! had grown — accepts both shapes silently, which is how the leak survived. So
//! this scanner starts at byte zero, consumes one complete value, and then
//! requires that only whitespace remains.
//!
//! It is a validator, not a parser: it reports where the value ends and whether
//! the whole stream is exactly that value, and never builds a tree. That keeps
//! it dependency-free, which is what the workspace asks of a crate that has not
//! earned an external dependency.

use std::fmt;

/// Why a stream failed the one-object contract.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ContractError {
    /// Nothing but whitespace — the early-exit shape, where a command printed a
    /// message and returned without a payload.
    Empty,
    /// The bytes at `offset` are not the start of a JSON value. A leading
    /// banner lands here, on byte zero.
    NotJson { offset: usize, found: String },
    /// A value parsed, but `trailing` non-whitespace bytes follow it. A banner
    /// printed after the payload lands here.
    Trailing { offset: usize, trailing: String },
    /// The value itself is malformed — truncated output, or an interleaved
    /// write that landed mid-object.
    Malformed { offset: usize, reason: String },
}

impl fmt::Display for ContractError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Empty => write!(f, "stdout held no JSON value at all (zero objects)"),
            Self::NotJson { offset, found } => write!(
                f,
                "stdout does not begin with a JSON value; at byte {offset} found {found:?}"
            ),
            Self::Trailing { offset, trailing } => write!(
                f,
                "stdout has trailing output after the JSON value; at byte {offset} found {trailing:?}"
            ),
            Self::Malformed { offset, reason } => {
                write!(f, "malformed JSON at byte {offset}: {reason}")
            }
        }
    }
}

/// Returns `Ok(())` when `stream` is exactly one JSON value plus optional
/// surrounding whitespace.
///
/// Whitespace-only is [`ContractError::Empty`] rather than "valid but nothing",
/// because a check that found no payload must never report what a check that
/// found a good one reports.
pub fn holds_exactly_one_value(stream: &str) -> Result<(), ContractError> {
    let bytes = stream.as_bytes();
    let start = skip_ws(bytes, 0);
    if start == bytes.len() {
        return Err(ContractError::Empty);
    }
    let end = scan_value(bytes, start)?;
    let after = skip_ws(bytes, end);
    if after != bytes.len() {
        return Err(ContractError::Trailing {
            offset: after,
            trailing: excerpt(stream, after),
        });
    }
    Ok(())
}

/// Returns `Ok(())` when every non-empty line of `stream` is its own JSON
/// value — the `--jsonl` contract, where stdout is a newline-delimited stream
/// and a human line at the end is just as unparseable as one in the middle.
pub fn holds_json_lines(stream: &str) -> Result<(), ContractError> {
    let mut saw_one = false;
    let mut offset = 0usize;
    for line in stream.split_inclusive('\n') {
        let trimmed = line.trim();
        if !trimmed.is_empty() {
            // Report positions against the whole stream, not the line, so a
            // failure points at something a reader can find in captured output.
            let lead = line.len() - line.trim_start().len();
            holds_exactly_one_value(trimmed).map_err(|err| shift(err, offset + lead))?;
            saw_one = true;
        }
        offset += line.len();
    }
    if saw_one {
        Ok(())
    } else {
        Err(ContractError::Empty)
    }
}

fn shift(err: ContractError, by: usize) -> ContractError {
    match err {
        ContractError::Empty => ContractError::Empty,
        ContractError::NotJson { offset, found } => ContractError::NotJson {
            offset: offset + by,
            found,
        },
        ContractError::Trailing { offset, trailing } => ContractError::Trailing {
            offset: offset + by,
            trailing,
        },
        ContractError::Malformed { offset, reason } => ContractError::Malformed {
            offset: offset + by,
            reason,
        },
    }
}

/// A short, single-line quote of what was found, for an error a human reads.
/// Bounded so a command that dumped a megabyte of help text does not put a
/// megabyte into the report.
fn excerpt(stream: &str, offset: usize) -> String {
    const LIMIT: usize = 60;
    let tail = &stream[offset..];
    let line = tail.lines().next().unwrap_or("");
    let mut out = String::new();
    for ch in line.chars() {
        if out.chars().count() >= LIMIT {
            out.push('…');
            break;
        }
        out.push(ch);
    }
    out
}

fn skip_ws(bytes: &[u8], mut at: usize) -> usize {
    // Exactly RFC 8259's four whitespace bytes. Notably not `char::is_whitespace`:
    // a U+00A0 in the output is garbage that must be reported, not skipped.
    while at < bytes.len() && matches!(bytes[at], b' ' | b'\t' | b'\n' | b'\r') {
        at += 1;
    }
    at
}

/// Consumes one JSON value starting at `at`; returns the byte offset just past it.
fn scan_value(bytes: &[u8], at: usize) -> Result<usize, ContractError> {
    // Depth is bounded so a pathological or truncated stream cannot recurse the
    // scanner into a stack overflow — an unbounded input must fail loudly, not
    // take the process down with it.
    scan_value_at_depth(bytes, at, 0)
}

const MAX_DEPTH: usize = 128;

fn scan_value_at_depth(bytes: &[u8], at: usize, depth: usize) -> Result<usize, ContractError> {
    if depth > MAX_DEPTH {
        return Err(ContractError::Malformed {
            offset: at,
            reason: format!("nesting deeper than {MAX_DEPTH} levels"),
        });
    }
    match bytes.get(at) {
        None => Err(ContractError::Malformed {
            offset: at,
            reason: "stream ended where a value was expected".into(),
        }),
        Some(b'{') => scan_object(bytes, at, depth),
        Some(b'[') => scan_array(bytes, at, depth),
        Some(b'"') => scan_string(bytes, at),
        Some(b't') => scan_literal(bytes, at, "true"),
        Some(b'f') => scan_literal(bytes, at, "false"),
        Some(b'n') => scan_literal(bytes, at, "null"),
        Some(c) if *c == b'-' || c.is_ascii_digit() => scan_number(bytes, at),
        Some(_) => Err(ContractError::NotJson {
            offset: at,
            found: excerpt(&String::from_utf8_lossy(bytes), at),
        }),
    }
}

fn scan_object(bytes: &[u8], at: usize, depth: usize) -> Result<usize, ContractError> {
    let mut cursor = skip_ws(bytes, at + 1);
    if bytes.get(cursor) == Some(&b'}') {
        return Ok(cursor + 1);
    }
    loop {
        cursor = skip_ws(bytes, cursor);
        if bytes.get(cursor) != Some(&b'"') {
            return Err(ContractError::Malformed {
                offset: cursor,
                reason: "expected a quoted object key".into(),
            });
        }
        cursor = scan_string(bytes, cursor)?;
        cursor = skip_ws(bytes, cursor);
        if bytes.get(cursor) != Some(&b':') {
            return Err(ContractError::Malformed {
                offset: cursor,
                reason: "expected ':' after an object key".into(),
            });
        }
        cursor = skip_ws(bytes, cursor + 1);
        cursor = scan_value_at_depth(bytes, cursor, depth + 1)?;
        cursor = skip_ws(bytes, cursor);
        match bytes.get(cursor) {
            Some(b',') => cursor += 1,
            Some(b'}') => return Ok(cursor + 1),
            _ => {
                return Err(ContractError::Malformed {
                    offset: cursor,
                    reason: "expected ',' or '}' in an object".into(),
                });
            }
        }
    }
}

fn scan_array(bytes: &[u8], at: usize, depth: usize) -> Result<usize, ContractError> {
    let mut cursor = skip_ws(bytes, at + 1);
    if bytes.get(cursor) == Some(&b']') {
        return Ok(cursor + 1);
    }
    loop {
        cursor = skip_ws(bytes, cursor);
        cursor = scan_value_at_depth(bytes, cursor, depth + 1)?;
        cursor = skip_ws(bytes, cursor);
        match bytes.get(cursor) {
            Some(b',') => cursor += 1,
            Some(b']') => return Ok(cursor + 1),
            _ => {
                return Err(ContractError::Malformed {
                    offset: cursor,
                    reason: "expected ',' or ']' in an array".into(),
                });
            }
        }
    }
}

fn scan_string(bytes: &[u8], at: usize) -> Result<usize, ContractError> {
    let mut cursor = at + 1;
    while let Some(byte) = bytes.get(cursor) {
        match byte {
            b'"' => return Ok(cursor + 1),
            b'\\' => {
                // Step over the escape's payload so an escaped quote does not
                // read as the closing quote. \u takes four more hex digits.
                match bytes.get(cursor + 1) {
                    Some(b'u') => cursor += 6,
                    Some(_) => cursor += 2,
                    None => break,
                }
            }
            _ => cursor += 1,
        }
    }
    Err(ContractError::Malformed {
        offset: at,
        reason: "unterminated string".into(),
    })
}

fn scan_literal(bytes: &[u8], at: usize, word: &str) -> Result<usize, ContractError> {
    let end = at + word.len();
    if bytes.get(at..end) == Some(word.as_bytes()) {
        Ok(end)
    } else {
        Err(ContractError::NotJson {
            offset: at,
            found: excerpt(&String::from_utf8_lossy(bytes), at),
        })
    }
}

fn scan_number(bytes: &[u8], at: usize) -> Result<usize, ContractError> {
    let mut cursor = at;
    if bytes.get(cursor) == Some(&b'-') {
        cursor += 1;
    }
    let digits_start = cursor;
    while matches!(bytes.get(cursor), Some(c) if c.is_ascii_digit()) {
        cursor += 1;
    }
    if cursor == digits_start {
        // A lone '-' with no digits behind it never started a value, so this is
        // "not JSON", not "broken JSON". The distinction is what a reader of the
        // report acts on: a banner beginning with a dash — "--set expects a
        // positive USD amount" — is a leaked diagnostic, and saying "expected
        // digits in a number" about it sends them looking for a malformed
        // payload that does not exist.
        return Err(ContractError::NotJson {
            offset: at,
            found: excerpt(&String::from_utf8_lossy(bytes), at),
        });
    }
    if bytes.get(cursor) == Some(&b'.') {
        cursor += 1;
        let frac_start = cursor;
        while matches!(bytes.get(cursor), Some(c) if c.is_ascii_digit()) {
            cursor += 1;
        }
        if cursor == frac_start {
            return Err(ContractError::Malformed {
                offset: at,
                reason: "expected digits after a decimal point".into(),
            });
        }
    }
    if matches!(bytes.get(cursor), Some(b'e' | b'E')) {
        cursor += 1;
        if matches!(bytes.get(cursor), Some(b'+' | b'-')) {
            cursor += 1;
        }
        let exp_start = cursor;
        while matches!(bytes.get(cursor), Some(c) if c.is_ascii_digit()) {
            cursor += 1;
        }
        if cursor == exp_start {
            return Err(ContractError::Malformed {
                offset: at,
                reason: "expected digits in an exponent".into(),
            });
        }
    }
    Ok(cursor)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn accepts_a_lone_object() {
        assert!(holds_exactly_one_value("{\"ok\": true}").is_ok());
        assert!(holds_exactly_one_value("\n  {\"ok\": true}\n\n").is_ok());
    }

    #[test]
    fn accepts_the_payload_shapes_the_cli_actually_emits() {
        for stream in [
            "{\n  \"budget_usd\": 5.0,\n  \"over_budget\": false\n}\n",
            "{\"ok\":false,\"error\":\"Config not found\"}\n",
            "{\"events\": [{\"path\": \"a.py\", \"allowed\": true}], \"next_cursor\": 42}",
            "{\"remaining_usd\": null, \"spend_usd\": -1.5e-3}",
        ] {
            assert!(
                holds_exactly_one_value(stream).is_ok(),
                "should accept: {stream}"
            );
        }
    }

    #[test]
    fn rejects_a_banner_in_front_of_the_payload() {
        // The `dev cost budget --json --set 5.00` shape.
        let err = holds_exactly_one_value("Set telemetry.cost_budget_usd = 5.00\n{\"ok\": true}\n")
            .unwrap_err();
        assert!(
            matches!(err, ContractError::NotJson { offset: 0, .. }),
            "got {err:?}"
        );
    }

    #[test]
    fn rejects_a_banner_after_the_payload() {
        // The `dev runs supervise --json` shape.
        let err =
            holds_exactly_one_value("{\"verdict\": \"revert\"}\nRun dev runs revert to apply.")
                .unwrap_err();
        match err {
            ContractError::Trailing { trailing, .. } => {
                assert!(
                    trailing.starts_with("Run dev runs revert"),
                    "got {trailing:?}"
                );
            }
            other => panic!("expected Trailing, got {other:?}"),
        }
    }

    #[test]
    fn rejects_zero_objects_distinctly_from_a_bad_one() {
        // The early-exit shape must not be reported as "parsed fine": a check
        // that found no payload has not passed.
        assert_eq!(
            holds_exactly_one_value("").unwrap_err(),
            ContractError::Empty
        );
        assert_eq!(
            holds_exactly_one_value("  \n\t ").unwrap_err(),
            ContractError::Empty
        );
        assert!(matches!(
            holds_exactly_one_value("Config not found at /x.\n").unwrap_err(),
            ContractError::NotJson { .. }
        ));
    }

    #[test]
    fn rejects_two_objects() {
        let err = holds_exactly_one_value("{\"a\": 1}{\"b\": 2}").unwrap_err();
        assert!(
            matches!(err, ContractError::Trailing { offset: 8, .. }),
            "got {err:?}"
        );
    }

    #[test]
    fn a_brace_inside_a_string_does_not_end_the_object() {
        assert!(holds_exactly_one_value(r#"{"error": "expected } here"}"#).is_ok());
        assert!(holds_exactly_one_value(r#"{"error": "a \"quoted\" word"}"#).is_ok());
        assert!(holds_exactly_one_value(r#"{"error": "} not a brace"}"#).is_ok());
    }

    #[test]
    fn a_leading_dash_is_a_leaked_banner_not_a_broken_number() {
        // The --set expects a positive USD amount (e.g. --set 5.00). and  shapes: the message happens to start with '-'.
        for banner in [
            "--set expects a positive USD amount (e.g. --set 5.00).
",
            "--stage must be before or after
",
        ] {
            match holds_exactly_one_value(banner).unwrap_err() {
                ContractError::NotJson { offset: 0, found } => {
                    assert!(found.starts_with("--"), "got {found:?}");
                }
                other => panic!("expected NotJson for {banner:?}, got {other:?}"),
            }
        }
    }

    #[test]
    fn a_number_that_really_did_start_is_still_malformed() {
        assert!(matches!(
            holds_exactly_one_value("1.").unwrap_err(),
            ContractError::Malformed { .. }
        ));
        assert!(matches!(
            holds_exactly_one_value("1e").unwrap_err(),
            ContractError::Malformed { .. }
        ));
        assert!(holds_exactly_one_value("-1.5e-3").is_ok());
    }

    #[test]
    fn rejects_truncated_output() {
        assert!(matches!(
            holds_exactly_one_value("{\"ok\": tru").unwrap_err(),
            ContractError::NotJson { .. }
        ));
        assert!(matches!(
            holds_exactly_one_value("{\"ok\": ").unwrap_err(),
            ContractError::Malformed { .. }
        ));
        assert!(matches!(
            holds_exactly_one_value("{\"unterminated: 1").unwrap_err(),
            ContractError::Malformed { .. }
        ));
    }

    #[test]
    fn bounded_nesting_fails_loudly_rather_than_overflowing_the_stack() {
        let deep = "[".repeat(MAX_DEPTH + 10);
        let err = holds_exactly_one_value(&deep).unwrap_err();
        assert!(
            matches!(err, ContractError::Malformed { .. }),
            "got {err:?}"
        );
    }

    #[test]
    fn non_ascii_whitespace_is_garbage_not_whitespace() {
        // U+00A0 between two values is output corruption, not formatting.
        let err = holds_exactly_one_value("{\"a\": 1}\u{00a0}").unwrap_err();
        assert!(matches!(err, ContractError::Trailing { .. }), "got {err:?}");
    }

    #[test]
    fn json_lines_accepts_a_record_per_line() {
        assert!(holds_json_lines("{\"a\": 1}\n{\"b\": 2}\n").is_ok());
        assert!(holds_json_lines("{\"a\": 1}\n\n{\"b\": 2}\n").is_ok());
    }

    #[test]
    fn json_lines_rejects_a_cursor_line_at_the_end() {
        // The `dev trace tail --since 0 --jsonl` shape.
        let err = holds_json_lines("{\"a\": 1}\n{\"b\": 2}\nnext_cursor: 42\n").unwrap_err();
        match err {
            ContractError::NotJson { offset, found } => {
                assert_eq!(offset, 18);
                assert!(found.starts_with("next_cursor"), "got {found:?}");
            }
            other => panic!("expected NotJson, got {other:?}"),
        }
    }

    #[test]
    fn json_lines_rejects_an_empty_stream() {
        assert_eq!(holds_json_lines("\n\n").unwrap_err(), ContractError::Empty);
    }
}
