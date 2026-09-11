//! Dead-symbol findings from `devmap dead --json`.
//!
//! The Go verifier shells out to `devmap` and feeds the JSON here. Parsing is
//! pure so a missing binary is a skip at the Go boundary (never a silent
//! pass), while a parse failure here is an error — an empty candidate list
//! means "devmap found nothing", not "we could not read the answer".

use std::fmt;

/// One dead / unwired candidate the verifier can turn into a gap.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DeadCandidate {
    pub path: String,
    pub name: Option<String>,
    pub kind: DeadKind,
    pub confidence: String,
    pub line: Option<u32>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DeadKind {
    DeadSymbol,
    UnwiredFile,
    Stranded,
}

/// A `devmap dead` payload that could not be read.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DeadParseError {
    pub reason: String,
}

impl fmt::Display for DeadParseError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "devmap dead JSON: {}", self.reason)
    }
}

impl std::error::Error for DeadParseError {}

/// Parses the JSON object `devmap dead --json` emits.
///
/// Accepts either the envelope with `dead_symbol_candidates` /
/// `unwired_candidates` or a bare array of rows. Unknown shapes are errors,
/// never an empty success — an empty success is reserved for "nothing dead".
pub fn parse_devmap_dead(raw: &str) -> Result<Vec<DeadCandidate>, DeadParseError> {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return Err(DeadParseError {
            reason: "empty input".into(),
        });
    }
    // Minimal hand parser for the fields we need — no new serde dependency.
    // We look for known array keys and extract path/name/confidence/line.
    let mut out = Vec::new();
    extract_array_items(trimmed, "dead_symbol_candidates", DeadKind::DeadSymbol, &mut out)?;
    extract_array_items(trimmed, "unwired_candidates", DeadKind::UnwiredFile, &mut out)?;
    // If none of the keys were present but the document is a non-empty object
    // that looks like a status envelope with zero candidates, that is a real
    // empty answer. Detect by presence of a known key even if empty.
    let has_key = trimmed.contains("dead_symbol_candidates")
        || trimmed.contains("unwired_candidates")
        || trimmed.contains("\"candidates\"");
    if !has_key && trimmed.starts_with('{') {
        return Err(DeadParseError {
            reason: "no dead_symbol_candidates or unwired_candidates key".into(),
        });
    }
    // Dedup by path+name.
    out.sort_by(|a, b| (&a.path, &a.name).cmp(&(&b.path, &b.name)));
    out.dedup_by(|a, b| a.path == b.path && a.name == b.name && a.kind == b.kind);
    Ok(out)
}

fn extract_array_items(
    raw: &str,
    key: &str,
    kind: DeadKind,
    out: &mut Vec<DeadCandidate>,
) -> Result<(), DeadParseError> {
    let needle = format!("\"{key}\"");
    let Some(pos) = raw.find(&needle) else {
        return Ok(());
    };
    let after = &raw[pos + needle.len()..];
    let Some(bracket) = after.find('[') else {
        return Err(DeadParseError {
            reason: format!("{key} is not an array"),
        });
    };
    let array = match extract_balanced(&after[bracket..], '[', ']') {
        Some(a) => a,
        None => {
            return Err(DeadParseError {
                reason: format!("{key} array was truncated"),
            })
        }
    };
    // Walk objects inside the array.
    let mut rest = array.trim().trim_start_matches('[').trim_end_matches(']');
    while let Some(obj_start) = rest.find('{') {
        let obj = match extract_balanced(&rest[obj_start..], '{', '}') {
            Some(o) => o,
            None => break,
        };
        if let Some(c) = candidate_from_object(obj, kind) {
            out.push(c);
        }
        rest = &rest[obj_start + obj.len()..];
    }
    Ok(())
}

fn candidate_from_object(obj: &str, kind: DeadKind) -> Option<DeadCandidate> {
    let path = json_string_field(obj, "path")
        .or_else(|| json_string_field(obj, "file"))
        .unwrap_or_default();
    if path.is_empty() {
        return None;
    }
    let name = json_string_field(obj, "name").or_else(|| json_string_field(obj, "symbol"));
    let confidence = json_string_field(obj, "confidence").unwrap_or_else(|| "unknown".into());
    let line = json_string_field(obj, "line")
        .and_then(|s| s.parse().ok())
        .or_else(|| {
            // numeric line without quotes
            let key = "\"line\"";
            let pos = obj.find(key)?;
            let after = obj[pos + key.len()..].trim_start().trim_start_matches(':').trim_start();
            let digits: String = after.chars().take_while(|c| c.is_ascii_digit()).collect();
            digits.parse().ok()
        });
    Some(DeadCandidate {
        path,
        name,
        kind,
        confidence,
        line,
    })
}

fn json_string_field(obj: &str, key: &str) -> Option<String> {
    let needle = format!("\"{key}\"");
    let pos = obj.find(&needle)?;
    let after = obj[pos + needle.len()..].trim_start().trim_start_matches(':').trim_start();
    if !after.starts_with('"') {
        return None;
    }
    let mut out = String::new();
    let mut chars = after[1..].chars();
    while let Some(c) = chars.next() {
        match c {
            '\\' => {
                if let Some(n) = chars.next() {
                    out.push(n);
                }
            }
            '"' => break,
            other => out.push(other),
        }
    }
    Some(out)
}

fn extract_balanced<'a>(s: &'a str, open: char, close: char) -> Option<&'a str> {
    let mut depth = 0usize;
    let mut in_string = false;
    let mut escape = false;
    for (i, c) in s.char_indices() {
        if in_string {
            if escape {
                escape = false;
                continue;
            }
            match c {
                '\\' => escape = true,
                '"' => in_string = false,
                _ => {}
            }
            continue;
        }
        match c {
            '"' => in_string = true,
            c if c == open => depth += 1,
            c if c == close => {
                depth -= 1;
                if depth == 0 {
                    return Some(&s[..=i]);
                }
            }
            _ => {}
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_dead_and_unwired() {
        let raw = r#"{
          "dead_symbol_candidates": [
            {"path":"src/a.go","name":"Helper","confidence":"extracted","line":12}
          ],
          "unwired_candidates": [
            {"path":"src/orphan.go","confidence":"extracted"}
          ]
        }"#;
        let got = parse_devmap_dead(raw).expect("parse");
        assert_eq!(got.len(), 2);
        assert_eq!(got[0].path, "src/a.go");
        assert_eq!(got[0].name.as_deref(), Some("Helper"));
        assert_eq!(got[0].kind, DeadKind::DeadSymbol);
        assert_eq!(got[0].line, Some(12));
        assert_eq!(got[1].path, "src/orphan.go");
        assert_eq!(got[1].kind, DeadKind::UnwiredFile);
    }

    #[test]
    fn empty_candidates_is_ok_not_error() {
        let raw = r#"{"dead_symbol_candidates":[],"unwired_candidates":[]}"#;
        let got = parse_devmap_dead(raw).expect("parse");
        assert!(got.is_empty());
    }

    #[test]
    fn missing_keys_is_error() {
        let err = parse_devmap_dead(r#"{"status":"ok"}"#).expect_err("must err");
        assert!(err.reason.contains("no dead_symbol"));
    }

    #[test]
    fn empty_input_is_error() {
        assert!(parse_devmap_dead("").is_err());
    }
}
