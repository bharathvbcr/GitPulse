//! Deterministic canonical JSON serialization and approval-binding digests.
//!
//! All platforms (Rust desktop/daemon, Go relay, iOS Swift/UniFFI, Android Kotlin/UniFFI)
//! compute identical byte sequences for `input_sha256` and approval signing payloads.
//!
//! Canonicalization rules:
//! - Object keys are sorted lexicographically by UTF-8 bytes (self-consistent across
//!   all GitPulse components; note this is UTF-8 byte order, NOT RFC 8785 JCS UTF-16
//!   code-unit order).
//! - Zero insignificant whitespace.
//! - Numbers that cannot be represented losslessly (integers outside `i64`/`u64`,
//!   non-round-trippable floats, `-0.0`, integer-valued floats like `1.0`, `NaN`/`Infinity`)
//!   fail closed with [`ProtocolError::NonCanonicalNumber`] so the approval ticket
//!   falls back to "approve at desk only" rather than rendering a different value on
//!   mobile than the harness executes.

use crate::error::{ProtocolError, Result};
use crate::wire::ApprovalDecisionKind;
use serde_json::Value;
use sha2::{Digest, Sha256};

/// Domain prefix prepended to canonical JSON before P-256 signing/verification of
/// every `SignedApprovalDecision`.
pub const APPROVAL_SIGNING_DOMAIN: &[u8] = b"gitpulse-approval/v1\n";

/// Maximum nesting depth accepted during canonicalization to prevent stack abuse.
const MAX_JSON_DEPTH: usize = 64;

/// Parses a raw JSON text string, rejects any lossy/non-canonical number literals
/// BEFORE `serde_json::Value` parsing can silently coerce them into `f64`, and
/// returns the parsed [`Value`].
pub fn parse_canonical_json_value(raw_json: &str) -> Result<Value> {
    validate_raw_json_numbers(raw_json)?;
    let value: Value =
        serde_json::from_str(raw_json).map_err(|e| ProtocolError::Json(e.to_string()))?;
    // Verify canonical serialization succeeds as well (catches depth/number invariants).
    let _ = canonical_json_bytes(&value)?;
    Ok(value)
}

/// Parses and canonicalizes a raw JSON string to deterministic UTF-8 bytes, failing
/// closed with [`ProtocolError::NonCanonicalNumber`] on any lossy number literal.
pub fn canonical_json_from_str(raw_json: &str) -> Result<Vec<u8>> {
    let value = parse_canonical_json_value(raw_json)?;
    canonical_json_bytes(&value)
}

/// Serializes a `serde_json::Value` to deterministic canonical JSON UTF-8 bytes:
/// - Object keys sorted lexicographically by UTF-8 byte order.
/// - Zero insignificant whitespace.
/// - Rejects non-finite floats, `-0.0`, integer-valued floats (which include integers
///   above `u64::MAX` or below `i64::MIN` coerced to `f64` by `serde_json`), and
///   excessive nesting depth.
pub fn canonical_json_bytes(value: &Value) -> Result<Vec<u8>> {
    let mut out = Vec::with_capacity(256);
    write_canonical(value, &mut out, 0)?;
    Ok(out)
}

/// Convenience UTF-8 wrapper around [`canonical_json_bytes`].
pub fn canonical_json_string(value: &Value) -> Result<String> {
    let bytes = canonical_json_bytes(value)?;
    String::from_utf8(bytes).map_err(|e| ProtocolError::Json(e.to_string()))
}

fn write_canonical(value: &Value, out: &mut Vec<u8>, depth: usize) -> Result<()> {
    if depth > MAX_JSON_DEPTH {
        return Err(ProtocolError::TooLarge(
            "json nesting depth exceeds limit".to_string(),
        ));
    }
    match value {
        Value::Null => out.extend_from_slice(b"null"),
        Value::Bool(true) => out.extend_from_slice(b"true"),
        Value::Bool(false) => out.extend_from_slice(b"false"),
        Value::Number(num) => {
            if let Some(i) = num.as_i64() {
                out.extend_from_slice(i.to_string().as_bytes());
            } else if let Some(u) = num.as_u64() {
                out.extend_from_slice(u.to_string().as_bytes());
            } else if let Some(f) = num.as_f64() {
                if !f.is_finite() {
                    return Err(ProtocolError::NonCanonicalNumber(
                        "non-finite float".to_string(),
                    ));
                }
                if f == 0.0 && f.is_sign_negative() {
                    return Err(ProtocolError::NonCanonicalNumber(
                        "negative zero float".to_string(),
                    ));
                }
                // Any integer outside i64/u64 is coerced to f64 with fract() == 0.0,
                // and float-formatted integers like `1.0` are ambiguous across JSON engines.
                if f.fract() == 0.0 {
                    return Err(ProtocolError::NonCanonicalNumber(format!(
                        "integer-valued or out-of-range float: {num}"
                    )));
                }
                let formatted =
                    serde_json::to_string(&f).map_err(|e| ProtocolError::Json(e.to_string()))?;
                let reparsed: f64 = formatted.parse().map_err(|_| {
                    ProtocolError::NonCanonicalNumber(format!(
                        "float failed round-trip parse: {num}"
                    ))
                })?;
                if reparsed.to_bits() != f.to_bits() {
                    return Err(ProtocolError::NonCanonicalNumber(format!(
                        "non-round-trippable float: {num}"
                    )));
                }
                out.extend_from_slice(formatted.as_bytes());
            } else {
                return Err(ProtocolError::NonCanonicalNumber(format!(
                    "unsupported JSON number: {num}"
                )));
            }
        }
        Value::String(s) => write_json_string(s, out),
        Value::Array(items) => {
            out.push(b'[');
            for (idx, item) in items.iter().enumerate() {
                if idx > 0 {
                    out.push(b',');
                }
                write_canonical(item, out, depth + 1)?;
            }
            out.push(b']');
        }
        Value::Object(map) => {
            let mut entries: Vec<(&String, &Value)> = map.iter().collect();
            entries.sort_by(|(a, _), (b, _)| a.as_bytes().cmp(b.as_bytes()));
            out.push(b'{');
            for (idx, (k, v)) in entries.into_iter().enumerate() {
                if idx > 0 {
                    out.push(b',');
                }
                write_json_string(k, out);
                out.push(b':');
                write_canonical(v, out, depth + 1)?;
            }
            out.push(b'}');
        }
    }
    Ok(())
}

/// Scans raw JSON text and rejects any number token that would lose precision or
/// change representation when parsed into `serde_json::Value`:
/// - Integers outside `i64`/`u64` (e.g. `18446744073709551616`, `-9223372036854775809`).
/// - Negative zero (`-0`, `-0.0`).
/// - Floats whose parsed `f64` does not serialize identically back to the input token
///   (catches excessive decimal digits like `0.10000000000000000555`, exponent overflow,
///   and `1.0`-style integer-valued floats).
fn validate_raw_json_numbers(raw: &str) -> Result<()> {
    let bytes = raw.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        match bytes[i] {
            b'"' => {
                // Skip string literal respecting escapes.
                i += 1;
                while i < bytes.len() {
                    if bytes[i] == b'\\' {
                        i += 2;
                        continue;
                    }
                    if bytes[i] == b'"' {
                        i += 1;
                        break;
                    }
                    i += 1;
                }
            }
            b'-' | b'0'..=b'9' => {
                let start = i;
                while i < bytes.len()
                    && matches!(bytes[i], b'0'..=b'9' | b'-' | b'+' | b'.' | b'e' | b'E')
                {
                    i += 1;
                }
                let token = &raw[start..i];
                validate_number_token(token)?;
            }
            _ => {
                i += 1;
            }
        }
    }
    Ok(())
}

fn validate_number_token(token: &str) -> Result<()> {
    if token == "-0"
        || token.starts_with("-0.")
        || token.starts_with("-0e")
        || token.starts_with("-0E")
    {
        // Distinguish `-0.5` (valid negative non-zero float) from `-0` / `-0.0`.
        if let Ok(f) = token.parse::<f64>() {
            if f == 0.0 {
                return Err(ProtocolError::NonCanonicalNumber(format!(
                    "negative zero: {token}"
                )));
            }
        }
    }

    let is_float_syntax = token.contains('.') || token.contains('e') || token.contains('E');
    if !is_float_syntax {
        if token.starts_with('-') {
            if token.parse::<i64>().is_err() {
                return Err(ProtocolError::NonCanonicalNumber(format!(
                    "signed integer out of i64 range: {token}"
                )));
            }
        } else if token.parse::<u64>().is_err() {
            return Err(ProtocolError::NonCanonicalNumber(format!(
                "unsigned integer out of u64 range: {token}"
            )));
        }
        return Ok(());
    }

    let f: f64 = token
        .parse()
        .map_err(|_| ProtocolError::NonCanonicalNumber(format!("invalid float: {token}")))?;
    if !f.is_finite() {
        return Err(ProtocolError::NonCanonicalNumber(format!(
            "non-finite float: {token}"
        )));
    }
    if f.fract() == 0.0 {
        return Err(ProtocolError::NonCanonicalNumber(format!(
            "integer-valued float is non-canonical: {token}"
        )));
    }
    let formatted = serde_json::to_string(&f).map_err(|e| ProtocolError::Json(e.to_string()))?;
    if formatted != token {
        return Err(ProtocolError::NonCanonicalNumber(format!(
            "float does not round-trip losslessly: input={token}, canonical={formatted}"
        )));
    }
    Ok(())
}

/// Writes a JSON string using minimal RFC 8259 escaping.
fn write_json_string(s: &str, out: &mut Vec<u8>) {
    out.push(b'"');
    for ch in s.chars() {
        match ch {
            '"' => out.extend_from_slice(b"\\\""),
            '\\' => out.extend_from_slice(b"\\\\"),
            '\u{0008}' => out.extend_from_slice(b"\\b"),
            '\u{000C}' => out.extend_from_slice(b"\\f"),
            '\n' => out.extend_from_slice(b"\\n"),
            '\r' => out.extend_from_slice(b"\\r"),
            '\t' => out.extend_from_slice(b"\\t"),
            c if (c as u32) < 0x20 => {
                let code = c as u32;
                let hex = format!("\\u{code:04x}");
                out.extend_from_slice(hex.as_bytes());
            }
            c => {
                let mut buf = [0u8; 4];
                out.extend_from_slice(c.encode_utf8(&mut buf).as_bytes());
            }
        }
    }
    out.push(b'"');
}

/// Builds the exact canonical JSON bytes representing a tool approval request:
/// `{"cwd":<cwd>,"tool_input":<canonical_tool_input>,"tool_name":<tool_name>}`.
///
/// The mobile native confirmation sheet renders these exact bytes and computes
/// `SHA-256` locally in Rust so the user signs what was rendered (Opus P0-5/M1/M3).
pub fn canonical_tool_input_bytes(
    tool_name: &str,
    tool_input: &Value,
    cwd: &str,
) -> Result<Vec<u8>> {
    if tool_name.is_empty() || tool_name.len() > 256 {
        return Err(ProtocolError::InvalidInput(
            "tool_name must be 1..=256 bytes".to_string(),
        ));
    }
    if cwd.len() > 4096 {
        return Err(ProtocolError::InvalidInput(
            "cwd exceeds 4096 bytes".to_string(),
        ));
    }
    let mut map = serde_json::Map::with_capacity(3);
    map.insert("cwd".to_string(), Value::String(cwd.to_string()));
    map.insert("tool_input".to_string(), tool_input.clone());
    map.insert(
        "tool_name".to_string(),
        Value::String(tool_name.to_string()),
    );
    canonical_json_bytes(&Value::Object(map))
}

/// Computes `SHA-256(canonical_tool_input_bytes(tool_name, tool_input, cwd))`.
pub fn input_sha256(tool_name: &str, tool_input: &Value, cwd: &str) -> Result<[u8; 32]> {
    let bytes = canonical_tool_input_bytes(tool_name, tool_input, cwd)?;
    Ok(Sha256::digest(&bytes).into())
}

/// Computes lowercase hex-encoded `input_sha256` (64 hex chars).
pub fn input_sha256_hex(tool_name: &str, tool_input: &Value, cwd: &str) -> Result<String> {
    Ok(hex_encode(&input_sha256(tool_name, tool_input, cwd)?))
}

/// Builds the exact domain-separated bytes signed by the mobile hardware P-256 key
/// for a [`crate::wire::SignedApprovalDecision`]:
///
/// ```text
/// gitpulse-approval/v1\n{"decision":...,"expires_at":...,"host_id":...,"input_sha256":...,"issued_at":...,"nonce":...,"ticket_id":...}
/// ```
#[allow(clippy::too_many_arguments)]
pub fn approval_signing_bytes(
    host_id: &str,
    ticket_id: &str,
    input_sha256_hex_str: &str,
    decision: ApprovalDecisionKind,
    nonce: &str,
    issued_at: i64,
    expires_at: i64,
) -> Result<Vec<u8>> {
    validate_non_empty_id(host_id, "host_id", 128)?;
    validate_non_empty_id(ticket_id, "ticket_id", 128)?;
    validate_sha256_hex(input_sha256_hex_str)?;
    validate_non_empty_id(nonce, "nonce", 128)?;
    if expires_at <= issued_at {
        return Err(ProtocolError::InvalidInput(
            "expires_at must be strictly greater than issued_at".to_string(),
        ));
    }

    let mut map = serde_json::Map::with_capacity(7);
    map.insert(
        "decision".to_string(),
        Value::String(decision.as_wire_str().to_string()),
    );
    map.insert("expires_at".to_string(), Value::from(expires_at));
    map.insert("host_id".to_string(), Value::String(host_id.to_string()));
    map.insert(
        "input_sha256".to_string(),
        Value::String(input_sha256_hex_str.to_string()),
    );
    map.insert("issued_at".to_string(), Value::from(issued_at));
    map.insert("nonce".to_string(), Value::String(nonce.to_string()));
    map.insert(
        "ticket_id".to_string(),
        Value::String(ticket_id.to_string()),
    );

    let canonical = canonical_json_bytes(&Value::Object(map))?;
    let mut out = Vec::with_capacity(APPROVAL_SIGNING_DOMAIN.len() + canonical.len());
    out.extend_from_slice(APPROVAL_SIGNING_DOMAIN);
    out.extend_from_slice(&canonical);
    Ok(out)
}

/// Validates a lowercase 64-char hex SHA-256 digest.
pub(crate) fn validate_sha256_hex(s: &str) -> Result<()> {
    if s.len() == 64 && s.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f')) {
        Ok(())
    } else {
        Err(ProtocolError::InvalidInput(
            "input_sha256 must be 64 lowercase hex characters".to_string(),
        ))
    }
}

/// Validates an identifier string is non-empty, ASCII printable without whitespace/control, and bounded.
pub(crate) fn validate_non_empty_id(value: &str, field: &str, max_len: usize) -> Result<()> {
    if value.is_empty()
        || value.len() > max_len
        || value
            .bytes()
            .any(|b| !(b.is_ascii_alphanumeric() || matches!(b, b'.' | b'_' | b'-' | b':')))
    {
        return Err(ProtocolError::InvalidInput(format!(
            "{field} must be 1..={max_len} safe identifier characters"
        )));
    }
    Ok(())
}

/// Encodes bytes as lowercase hexadecimal.
pub fn hex_encode(bytes: &[u8]) -> String {
    const DIGITS: &[u8; 16] = b"0123456789abcdef";
    let mut out = String::with_capacity(bytes.len() * 2);
    for &b in bytes {
        out.push(char::from(DIGITS[usize::from(b >> 4)]));
        out.push(char::from(DIGITS[usize::from(b & 0x0f)]));
    }
    out
}

/// Decodes a fixed-length hex string.
pub fn hex_decode<const N: usize>(text: &str) -> Result<[u8; N]> {
    let raw = text.as_bytes();
    if raw.len() != N * 2 {
        return Err(ProtocolError::Malformed(format!(
            "expected {} hex chars, got {}",
            N * 2,
            raw.len()
        )));
    }
    let nibble = |c: u8| match c {
        b'0'..=b'9' => Some(c - b'0'),
        b'a'..=b'f' => Some(c - b'a' + 10),
        b'A'..=b'F' => Some(c - b'A' + 10),
        _ => None,
    };
    let mut out = [0u8; N];
    for (idx, slot) in out.iter_mut().enumerate() {
        let hi = nibble(raw[idx * 2])
            .ok_or_else(|| ProtocolError::Malformed("non-hex character".to_string()))?;
        let lo = nibble(raw[idx * 2 + 1])
            .ok_or_else(|| ProtocolError::Malformed("non-hex character".to_string()))?;
        *slot = (hi << 4) | lo;
    }
    Ok(out)
}
