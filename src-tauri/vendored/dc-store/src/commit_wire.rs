//! The framing `dcstore gaps-replace` reads a verification from.
//!
//! The store takes no JSON dependency (see [`crate::json`]), and a gap set is
//! too structured for flags, so a verification travels as one value in a
//! length-prefixed text form: no escaping, no ambiguity, and a reader that
//! cannot be talked into reading past what it was told.
//!
//! ```text
//! wire  := field("DCVC1") run gaps
//! run   := field("N") | field("R") field*7 opt        ; id task sandbox environment
//!                                                      ; commands status started_at, finished_at
//! gaps  := field(count) gap*
//! gap   := field(id) field(severity) field(gap_type) field(description)
//!          field(evidence_json) field(recommended_fix) field("1"|"0" blocking)
//!          opt(requirement_id) opt(file) opt(line) opt(suggested_command)
//!          opt(acceptance_criterion_id) opt(expected_verification_method)
//!          opt(stdout_path) opt(stderr_path)
//! field := <decimal byte length> ":" <that many bytes>
//! opt   := "-" | field
//! ```
//!
//! Nothing is trusted: every length is bounded, the count is bounded by
//! [`MAX_GAPS_PER_REPLACE`], a field must be valid UTF-8 and consume exactly
//! what it declared, and trailing bytes are an error.

use crate::records::{GapRow, MAX_GAPS_PER_REPLACE, VerificationRun};

/// The version tag every wire value starts with.
pub const WIRE_TAG: &str = "DCVC1";

/// The largest single field. Evidence and output tails are bounded well below.
const MAX_FIELD_BYTES: usize = 4 << 20;

struct Reader<'a> {
    rest: &'a [u8],
}

impl<'a> Reader<'a> {
    fn field(&mut self) -> Result<&'a str, String> {
        let colon = self
            .rest
            .iter()
            .take(9)
            .position(|b| *b == b':')
            .ok_or("field has no length prefix")?;
        let digits = std::str::from_utf8(&self.rest[..colon]).map_err(|_| "length is not text")?;
        if digits.is_empty() || !digits.bytes().all(|b| b.is_ascii_digit()) {
            return Err(format!("length {digits:?} is not a number"));
        }
        let len: usize = digits.parse().map_err(|_| "length overflows".to_string())?;
        if len > MAX_FIELD_BYTES {
            return Err(format!("field of {len} bytes exceeds {MAX_FIELD_BYTES}"));
        }
        let body = &self.rest[colon + 1..];
        if body.len() < len {
            return Err(format!(
                "field declares {len} bytes but {} remain",
                body.len()
            ));
        }
        self.rest = &body[len..];
        std::str::from_utf8(&body[..len]).map_err(|_| "field is not valid UTF-8".to_string())
    }

    fn opt(&mut self) -> Result<Option<&'a str>, String> {
        if self.rest.first() == Some(&b'-') {
            self.rest = &self.rest[1..];
            return Ok(None);
        }
        self.field().map(Some)
    }

    fn owned(&mut self) -> Result<String, String> {
        self.field().map(str::to_string)
    }

    fn owned_opt(&mut self) -> Result<Option<String>, String> {
        self.opt().map(|v| v.map(str::to_string))
    }
}

/// Decodes one verification: its run record (if sent) and its gaps.
pub fn decode(text: &str) -> Result<(Option<VerificationRun>, Vec<GapRow>), String> {
    let mut r = Reader {
        rest: text.as_bytes(),
    };
    let tag = r.field()?;
    if tag != WIRE_TAG {
        return Err(format!("unknown wire version {tag:?} (want {WIRE_TAG})"));
    }
    let run = match r.field()? {
        "N" => None,
        "R" => Some(VerificationRun {
            id: r.owned()?,
            task_id: r.owned()?,
            sandbox: r.owned()?,
            environment_json: r.owned()?,
            commands_json: r.owned()?,
            status: r.owned()?,
            started_at: r.owned()?,
            finished_at: r.owned_opt()?,
        }),
        other => return Err(format!("run marker {other:?} is neither N nor R")),
    };
    let count: usize = r
        .field()?
        .parse()
        .map_err(|_| "gap count is not a number".to_string())?;
    if count > MAX_GAPS_PER_REPLACE {
        return Err(format!(
            "{count} gaps in one replacement exceeds the limit of {MAX_GAPS_PER_REPLACE}"
        ));
    }
    let mut gaps = Vec::with_capacity(count);
    for _ in 0..count {
        let id = r.owned()?;
        let severity = r.owned()?;
        let gap_type = r.owned()?;
        let description = r.owned()?;
        let evidence_json = r.owned()?;
        let recommended_fix = r.owned()?;
        let blocking = match r.field()? {
            "1" => true,
            "0" => false,
            other => return Err(format!("blocking {other:?} is neither 1 nor 0")),
        };
        let requirement_id = r.owned_opt()?;
        let file = r.owned_opt()?;
        let line = match r.opt()? {
            Some(raw) => Some(
                raw.parse::<i64>()
                    .map_err(|_| format!("line {raw:?} is not a number"))?,
            ),
            None => None,
        };
        gaps.push(GapRow {
            id,
            severity,
            gap_type,
            requirement_id,
            task_id: None,
            description,
            evidence_json,
            recommended_fix,
            blocking,
            file,
            line,
            suggested_command: r.owned_opt()?,
            acceptance_criterion_id: r.owned_opt()?,
            expected_verification_method: r.owned_opt()?,
            stdout_path: r.owned_opt()?,
            stderr_path: r.owned_opt()?,
        });
    }
    if !r.rest.is_empty() {
        return Err(format!(
            "{} trailing bytes after the last gap",
            r.rest.len()
        ));
    }
    Ok((run, gaps))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn f(s: &str) -> String {
        format!("{}:{s}", s.len())
    }

    #[test]
    fn round_trips_a_run_and_a_gap_with_awkward_bytes() {
        let mut w = String::new();
        for p in [
            WIRE_TAG, "R", "run-1", "T1", "local", "{}", "[]", "failed", "t0",
        ] {
            w += &f(p);
        }
        w += "-";
        w += &f("1");
        for p in [
            "G1",
            "high",
            "test_failed",
            "line one\nline: two\u{1F600}",
            "[\"e\"]",
            "fix",
            "1",
        ] {
            w += &f(p);
        }
        w += "-";
        w += &f("a.go");
        w += &f("12");
        w += "-----";
        let (run, gaps) = decode(&w).expect("decodes");
        assert_eq!(run.expect("run").id, "run-1");
        assert_eq!(gaps.len(), 1);
        assert_eq!(gaps[0].description, "line one\nline: two\u{1F600}");
        assert_eq!(gaps[0].line, Some(12));
        assert!(gaps[0].blocking && gaps[0].requirement_id.is_none());
    }

    #[test]
    fn refuses_truncated_trailing_oversized_and_unknown() {
        let head = format!("{}{}{}", f(WIRE_TAG), f("N"), f("0"));
        assert!(decode(&head).expect("empty set").1.is_empty());
        assert!(
            decode(&format!("{head}x"))
                .unwrap_err()
                .contains("trailing")
        );
        assert!(decode(&format!("{}{}{}", f("DCVC9"), f("N"), f("0"))).is_err());
        assert!(
            decode(&format!("{}{}{}", f(WIRE_TAG), f("N"), f("501")))
                .unwrap_err()
                .contains("limit")
        );
        assert!(decode(&format!("{}{}9999999:", f(WIRE_TAG), f("N"))).is_err());
        assert!(decode(&format!("{}{}{}", f(WIRE_TAG), f("N"), f("1"))).is_err());
        assert!(decode("5:ab").is_err());
        assert!(decode("").is_err());
    }
}
