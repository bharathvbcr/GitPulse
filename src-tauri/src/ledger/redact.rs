//! Credential redaction on the ledger's write path.
//!
//! Vendor-shaped tokens delegate to `dc_verify::rigor`, whose
//! `SECRET_PATTERNS` table is the single owner of those signatures. This module
//! additionally owns context-shaped credentials (authorization/cookie headers,
//! URL passwords, named secret fields, and private-key blocks) that cannot be
//! identified safely by a token prefix alone.
//!
//! The alternative was to copy the pattern table into GitPulse. That is how a
//! key redacted by one gate leaks out of another: two tables drift, and the
//! one that drifts is discovered by the leak.

use regex::{Captures, Regex};
use std::collections::HashMap;
use std::sync::OnceLock;

static PRIVATE_KEY: OnceLock<Regex> = OnceLock::new();
static QUOTED_AUTHORIZATION: OnceLock<Regex> = OnceLock::new();
static DOUBLE_QUOTED_AUTHORIZATION: OnceLock<Regex> = OnceLock::new();
static EMBEDDED_AUTHORIZATION: OnceLock<Regex> = OnceLock::new();
static AUTHORIZATION: OnceLock<Regex> = OnceLock::new();
static QUOTED_COOKIE: OnceLock<Regex> = OnceLock::new();
static DOUBLE_QUOTED_COOKIE: OnceLock<Regex> = OnceLock::new();
static EMBEDDED_COOKIE: OnceLock<Regex> = OnceLock::new();
static COOKIE: OnceLock<Regex> = OnceLock::new();
static URL_PASSWORD: OnceLock<Regex> = OnceLock::new();
static DOUBLE_QUOTED_NAMED_SECRET: OnceLock<Regex> = OnceLock::new();
static EMBEDDED_NAMED_SECRET: OnceLock<Regex> = OnceLock::new();
static NAMED_SECRET: OnceLock<Regex> = OnceLock::new();
static SHELL_SECRET_FLAG: OnceLock<Regex> = OnceLock::new();
static SHELL_USERINFO_FLAG: OnceLock<Regex> = OnceLock::new();

fn compiled(slot: &'static OnceLock<Regex>, pattern: &str) -> &'static Regex {
    slot.get_or_init(|| Regex::new(pattern).expect("static credential regex must compile"))
}

fn redacted_assignment(captures: &Captures<'_>) -> String {
    let prefix = captures.get(1).map_or("", |value| value.as_str());
    let raw = captures.get(2).map_or("", |value| value.as_str());
    if already_redacted_value(raw) {
        return captures
            .get(0)
            .map_or_else(String::new, |value| value.as_str().to_string());
    }
    let replacement = match (raw.chars().next(), raw.chars().last()) {
        (Some('"'), Some('"')) => "\"<redacted>\"",
        (Some('\''), Some('\'')) => "'<redacted>'",
        _ => "<redacted>",
    };
    format!("{prefix}{replacement}")
}

fn already_redacted_value(raw: &str) -> bool {
    let unquoted = raw
        .strip_prefix('"')
        .and_then(|value| value.strip_suffix('"'))
        .or_else(|| {
            raw.strip_prefix('\'')
                .and_then(|value| value.strip_suffix('\''))
        })
        .unwrap_or(raw);
    unquoted == "<redacted>"
        || unquoted.starts_with("<redacted>\\n")
        || unquoted.starts_with("<redacted>\\r")
        || unquoted.starts_with("<redacted>\\t")
}

fn redacted_authorization(captures: &Captures<'_>) -> String {
    let prefix = captures.get(1).map_or("", |value| value.as_str());
    let scheme = captures
        .get(2)
        .filter(|value| !value.as_str().is_empty())
        .map(|value| format!("{} ", value.as_str()))
        .unwrap_or_default();
    let raw = captures.get(3).map_or("", |value| value.as_str());
    if already_redacted_value(raw) {
        return captures
            .get(0)
            .map_or_else(String::new, |value| value.as_str().to_string());
    }
    let replacement = match (raw.chars().next(), raw.chars().last()) {
        (Some('"'), Some('"')) => "\"<redacted>\"",
        (Some('\''), Some('\'')) => "'<redacted>'",
        _ => "<redacted>",
    };
    let suffix = captures.get(4).map_or("", |value| value.as_str());
    format!("{prefix}{scheme}{replacement}{suffix}")
}

fn redacted_embedded_assignment(captures: &Captures<'_>) -> String {
    let prefix = captures.get(1).map_or("", |value| value.as_str());
    let raw = captures.get(2).map_or("", |value| value.as_str());
    if already_redacted_value(raw) {
        return captures
            .get(0)
            .map_or_else(String::new, |value| value.as_str().to_string());
    }
    let suffix = captures.get(3).map_or("", |value| value.as_str());
    format!("{prefix}<redacted>{suffix}")
}

fn redact_contextual(value: &str) -> String {
    // Private-key blocks go first. dc-verify deliberately recognizes the PEM
    // prefix as a secret token, but token-oriented matching stops at the first
    // space and cannot remove the body after it has rewritten that prefix.
    let private_key = compiled(
        &PRIVATE_KEY,
        r"(?is)-----BEGIN[^\r\n]*?PRIVATE KEY(?: BLOCK)?-----.*?(?:-----END[^\r\n]*?PRIVATE KEY(?: BLOCK)?-----|$)",
    );
    let quoted_authorization = compiled(
        &QUOTED_AUTHORIZATION,
        r#"(?i)(["']?authorization["']?\s*[:=]\s*)()("[^"\r\n]*"|'[^'\r\n]*')"#,
    );
    let embedded_authorization = compiled(
        &EMBEDDED_AUTHORIZATION,
        r#"(?i)(authorization\s*[:=]\s*)(?:(basic|bearer|digest|token|negotiate|aws4-hmac-sha256)\s+)?([^"'\\\r\n]+?)(\\?["'])"#,
    );
    let double_quoted_authorization = compiled(
        &DOUBLE_QUOTED_AUTHORIZATION,
        r#"(?i)(authorization\s*[:=]\s*)(?:(basic|bearer|digest|token|negotiate|aws4-hmac-sha256)\s+)?((?:\\.|[^"\\\r\n])+?)(")"#,
    );
    let authorization = compiled(
        &AUTHORIZATION,
        r#"(?im)(["']?authorization["']?\s*[:=]\s*)(?:(basic|bearer|digest|token|negotiate|aws4-hmac-sha256)\s+)?([^"'\\\r\n]*?)(\s+[a-z][a-z0-9+.-]*://[^\\\r\n]*)?(?:\\.*)?$"#,
    );
    let quoted_cookie = compiled(
        &QUOTED_COOKIE,
        r#"(?i)(["']?(?:set-)?cookie["']?\s*[:=]\s*)("[^"\r\n]*"|'[^'\r\n]*')"#,
    );
    let embedded_cookie = compiled(
        &EMBEDDED_COOKIE,
        r#"(?i)((?:set-)?cookie\s*[:=]\s*)([^"'\\\r\n]+?)(\\?["'])"#,
    );
    let double_quoted_cookie = compiled(
        &DOUBLE_QUOTED_COOKIE,
        r#"(?i)((?:set-)?cookie\s*[:=]\s*)((?:\\.|[^"\\\r\n])+?)(")"#,
    );
    let cookie = compiled(
        &COOKIE,
        r#"(?im)(["']?(?:set-)?cookie["']?\s*[:=]\s*)([^"'\\\r\n]+?)(?:\\.*)?$"#,
    );
    let url_password = compiled(
        &URL_PASSWORD,
        r"(?i)([a-z][a-z0-9+.-]*://[^/\s:@]+:)[^@\s/?#]+@",
    );
    let named_secret = compiled(
        &NAMED_SECRET,
        r#"(?i)(["']?(?:password|passwd|api[_-]?key|access[_-]?token|refresh[_-]?token|client[_-]?secret|secret|aws[_-]?secret[_-]?access[_-]?key|aws[_-]?session[_-]?token|aws[_-]?access[_-]?key[_-]?id)["']?\s*[:=]\s*)("[^"\r\n]*"|'[^'\r\n]*'|[^\s,;&\\"'\]]+)"#,
    );
    let embedded_named_secret = compiled(
        &EMBEDDED_NAMED_SECRET,
        r#"(?i)((?:password|passwd|api[_-]?key|access[_-]?token|refresh[_-]?token|client[_-]?secret|secret|aws[_-]?secret[_-]?access[_-]?key|aws[_-]?session[_-]?token|aws[_-]?access[_-]?key[_-]?id)\s*[:=]\s*)([^"'\\\r\n]+?)(\\?["'])"#,
    );
    let double_quoted_named_secret = compiled(
        &DOUBLE_QUOTED_NAMED_SECRET,
        r#"(?i)((?:password|passwd|api[_-]?key|access[_-]?token|refresh[_-]?token|client[_-]?secret|secret|aws[_-]?secret[_-]?access[_-]?key|aws[_-]?session[_-]?token|aws[_-]?access[_-]?key[_-]?id)\s*[:=]\s*)((?:\\.|[^"\\\r\n])+?)(")"#,
    );
    let shell_secret_flag = compiled(
        &SHELL_SECRET_FLAG,
        r#"(?i)((?:--password|--passwd|--api-key|--apikey|--access-token|--refresh-token|--client-secret|--secret|--token|--auth-token|--oauth-token|--oauth2-bearer|--aws-secret-access-key|--aws-session-token|--aws-access-key-id)\s+)("[^"\r\n]*"|'[^'\r\n]*'|[^\s"'\\]+)"#,
    );
    let shell_userinfo_flag = compiled(
        &SHELL_USERINFO_FLAG,
        r#"(?i)((?:--user|--userpass|--proxy-user|-u)\s+)("[^"\r\n]*:[^"\r\n]*"|'[^'\r\n]*:[^'\r\n]*'|[^\s"'\\:]+:[^\s"'\\]+)"#,
    );

    let out = private_key
        .replace_all(value, "<private key redacted>")
        .into_owned();
    let out = quoted_authorization
        .replace_all(&out, redacted_authorization)
        .into_owned();
    let out = double_quoted_authorization
        .replace_all(&out, redacted_authorization)
        .into_owned();
    let out = embedded_authorization
        .replace_all(&out, redacted_authorization)
        .into_owned();
    let out = authorization
        .replace_all(&out, redacted_authorization)
        .into_owned();
    let out = quoted_cookie
        .replace_all(&out, redacted_assignment)
        .into_owned();
    let out = double_quoted_cookie
        .replace_all(&out, redacted_embedded_assignment)
        .into_owned();
    let out = embedded_cookie
        .replace_all(&out, redacted_embedded_assignment)
        .into_owned();
    let out = cookie.replace_all(&out, redacted_assignment).into_owned();
    let out = url_password.replace_all(&out, "$1<redacted>@").into_owned();
    let out = double_quoted_named_secret
        .replace_all(&out, redacted_embedded_assignment)
        .into_owned();
    let out = embedded_named_secret
        .replace_all(&out, redacted_embedded_assignment)
        .into_owned();
    let out = named_secret
        .replace_all(&out, redacted_assignment)
        .into_owned();
    let out = shell_secret_flag
        .replace_all(&out, redacted_assignment)
        .into_owned();
    shell_userinfo_flag
        .replace_all(&out, redacted_assignment)
        .into_owned()
}

fn redact_vendor_tokens(value: &str) -> String {
    // dc-verify defines a token boundary as whitespace, quote, or comma, but
    // its public helper returns only the first distinct value for one vendor
    // on a line. Apply that same owner independently to every such segment so
    // a short/previously-redacted candidate cannot hide a later credential.
    // Delimiters are copied byte-for-byte; no credential pattern is duplicated
    // here.
    let mut out = String::with_capacity(value.len());
    let mut start = 0;
    for (index, ch) in value.char_indices() {
        if !(ch.is_whitespace() || matches!(ch, '"' | '\'' | ',')) {
            continue;
        }
        if start < index {
            out.push_str(&dc_verify::rigor::redact_secrets(&value[start..index]));
        }
        out.push(ch);
        start = index + ch.len_utf8();
    }
    if start < value.len() {
        out.push_str(&dc_verify::rigor::redact_secrets(&value[start..]));
    }
    out
}

fn normalized_cli_flag(value: &str) -> Option<String> {
    value.starts_with('-').then(|| {
        value
            .trim_start_matches('-')
            .replace('-', "_")
            .to_ascii_lowercase()
    })
}

/// Field names whose *value* is a credential, whatever syntax carries them.
///
/// One table, three call sites: a CLI flag (`--client-secret X`), a JSON object
/// key (`{"client_secret": "X"}`) and the contextual `name=value` regexes below
/// must agree, because a name treated as secret in one syntax and ignored in
/// another is precisely how a credential redacted at one gate walks out of the
/// next. `authorization`, `cookie` and `private_key` are here because the
/// contextual stage already treats them as secret-bearing in prose; a JSON key
/// of the same name must not be weaker than the same name in a log line.
///
/// Mirrored by `SECRET_FIELD_NAMES` in `src/lib/diagnostics/diagnostics.ts`,
/// and bound to it by `scripts/diagnostics-contract.test.ts` so the two cannot
/// drift apart unnoticed.
pub(crate) const SECRET_FIELD_NAMES: [&str; 19] = [
    "password",
    "passwd",
    "api_key",
    "apikey",
    "access_token",
    "refresh_token",
    "client_secret",
    "secret",
    "token",
    "auth_token",
    "oauth_token",
    "oauth2_bearer",
    "aws_secret_access_key",
    "aws_session_token",
    "aws_access_key_id",
    "authorization",
    "cookie",
    "set_cookie",
    "private_key",
];

/// Trailing words that make a compound name a credential name.
///
/// `github_token` and `webhook_secret` are credentials for the same reason
/// `token` and `secret` are, and enumerating every vendor prefix is the losing
/// half of that race. Deliberately excludes the bare word `key`, which would
/// swallow `public_key`, `cache_key` and `primary_key` and gut the diagnostic
/// value of the report to redact nothing that is actually secret.
pub(crate) const SECRET_FIELD_SUFFIXES: [&str; 8] = [
    "password",
    "passwd",
    "secret",
    "token",
    "api_key",
    "apikey",
    "credential",
    "credentials",
];

/// A field name in comparable form: `X-Api-Key`, `clientSecret` and
/// `client-secret` all have to reach the table as `x_api_key`,
/// `client_secret` and `client_secret`.
pub(crate) fn normalize_field_name(key: &str) -> String {
    let mut out = String::with_capacity(key.len() + 4);
    let mut prev_was_lower_or_digit = false;
    for character in key.chars() {
        if character == '-' || character == '_' || character == '.' || character == ' ' {
            if !out.is_empty() && !out.ends_with('_') {
                out.push('_');
            }
            prev_was_lower_or_digit = false;
            continue;
        }
        if character.is_ascii_uppercase() {
            // camelCase boundary: `accessToken` must not normalize to one word.
            if prev_was_lower_or_digit && !out.ends_with('_') {
                out.push('_');
            }
            out.push(character.to_ascii_lowercase());
            prev_was_lower_or_digit = false;
        } else {
            out.push(character.to_ascii_lowercase());
            prev_was_lower_or_digit = character.is_ascii_lowercase() || character.is_ascii_digit();
        }
    }
    out.trim_matches('_').to_string()
}

/// Whether a normalized name is one the table calls a credential.
fn is_secret_name(normalized: &str) -> bool {
    if SECRET_FIELD_NAMES.contains(&normalized) {
        return true;
    }
    SECRET_FIELD_SUFFIXES.iter().any(|suffix| {
        normalized
            .strip_suffix(suffix)
            .is_some_and(|head| head.ends_with('_'))
    })
}

/// Whether a JSON object key declares its value to be a credential.
pub(crate) fn is_secret_field_name(key: &str) -> bool {
    is_secret_name(&normalize_field_name(key))
}

fn is_separate_secret_flag(value: &str) -> bool {
    normalized_cli_flag(value).is_some_and(|flag| is_secret_name(&flag))
}

fn is_userinfo_flag(value: &str) -> bool {
    matches!(
        normalized_cli_flag(value).as_deref(),
        Some("user" | "userpass" | "proxy_user")
    ) || value == "-u"
}

fn redact_userinfo(value: &str) -> Option<String> {
    let (username, password) = value.split_once(':')?;
    if password.is_empty() || password == "<redacted>" {
        return None;
    }
    Some(format!("{username}:<redacted>"))
}

fn redact_cli_array(values: &mut [serde_json::Value]) -> bool {
    let mut changed = false;
    let mut index = 0;
    while index < values.len() {
        let Some(argument) = values[index].as_str().map(str::to_owned) else {
            index += 1;
            continue;
        };

        if let Some((flag, inline_value)) = argument.split_once('=') {
            let replacement = if is_separate_secret_flag(flag) {
                (inline_value != "<redacted>").then(|| format!("{flag}=<redacted>"))
            } else if is_userinfo_flag(flag) {
                redact_userinfo(inline_value).map(|value| format!("{flag}={value}"))
            } else {
                None
            };
            if let Some(replacement) = replacement {
                values[index] = serde_json::Value::String(replacement);
                changed = true;
            }
        } else if index + 1 < values.len() && is_separate_secret_flag(&argument) {
            if values[index + 1]
                .as_str()
                .is_some_and(|value| value != "<redacted>")
            {
                values[index + 1] = serde_json::Value::String("<redacted>".to_string());
                changed = true;
            }
            index += 1;
        } else if index + 1 < values.len() && is_userinfo_flag(&argument) {
            if let Some(replacement) = values[index + 1].as_str().and_then(redact_userinfo) {
                values[index + 1] = serde_json::Value::String(replacement);
                changed = true;
            }
            index += 1;
        }
        index += 1;
    }
    changed
}

const MAX_SERIALIZED_NESTING: usize = 32;

/// Redacts credentials that appear as object *keys* rather than values.
///
/// The value side of this seam was blind to its keys; the key side was blind
/// to its own contents. A token is a token wherever it sits, and
/// `{"ghp_…": {…}}` — a cache or rate-limit map keyed by the credential — put
/// one in the ledger and the durable log in full while the identical token one
/// character to the right was redacted.
///
/// Renaming is done by rebuilding the map, and a rename that would collide
/// with a key already present is disambiguated rather than allowed to
/// overwrite: two distinct tokens of the same length redact to the same text,
/// and silently dropping one entry would make the document claim there was
/// only ever one.
fn redact_object_keys(
    values: &mut serde_json::Map<String, serde_json::Value>,
    depth: usize,
) -> bool {
    // Computed once per key: `redact_value_at_depth` runs the whole boundary,
    // and calling it again during the rebuild doubled the cost of every
    // document with a wide object in it.
    let renames: Vec<Option<String>> = values
        .keys()
        .map(|key| {
            let redacted = redact_value_at_depth(key, depth + 1);
            (redacted != *key).then_some(redacted)
        })
        .collect();
    if renames.iter().all(Option::is_none) {
        return false;
    }
    let mut rebuilt = serde_json::Map::with_capacity(values.len());
    // Next ordinal per colliding base name. A linear probe from 2 on every
    // insertion is quadratic exactly when the attack is cheapest to mount: N
    // distinct tokens of equal length all redact to the SAME text, so key n
    // pays n probes. Measured at 20k such keys: 31s probing, 24ms with this.
    let mut next_ordinal: HashMap<String, usize> = HashMap::new();
    for ((key, value), rename) in std::mem::take(values).into_iter().zip(renames) {
        let replacement = rename.unwrap_or(key);
        let mut candidate = replacement.clone();
        let mut ordinal = next_ordinal.get(&replacement).copied().unwrap_or(2);
        // The counter alone can still land on a literal key already present
        // (a document containing both `x` and `x #2`), so confirm before use.
        while rebuilt.contains_key(&candidate) {
            candidate = format!("{replacement} #{ordinal}");
            ordinal += 1;
        }
        next_ordinal.insert(replacement, ordinal);
        rebuilt.insert(candidate, value);
    }
    *values = rebuilt;
    true
}

fn redact_cli_json_value(value: &mut serde_json::Value, depth: usize) -> bool {
    if depth >= MAX_SERIALIZED_NESTING {
        // The remaining subtree was not inspected, so it cannot be treated as
        // safe. Replace it inside the parsed value to preserve every enclosing
        // JSON layer while failing closed at the work bound.
        if value.as_str() == Some("<redacted>") {
            return false;
        }
        *value = serde_json::Value::String("<redacted>".to_string());
        return true;
    }
    match value {
        serde_json::Value::Array(values) => {
            let mut changed = redact_cli_array(values);
            for value in values {
                changed |= redact_cli_json_value(value, depth + 1);
            }
            changed
        }
        serde_json::Value::Object(values) => {
            let mut changed = redact_object_keys(values, depth);
            for (key, value) in values.iter_mut() {
                // A key that names a credential says what its value is, and it
                // says so for every shape the value can take. Recursing instead
                // would hand the value to the contextual stage stripped of the
                // only context that identified it — which is how
                // `{"access_token": "<opaque>"}`, the shape every OAuth
                // response has, reached the ledger and the diagnostics report
                // in full.
                if is_secret_field_name(key) {
                    if value.as_str() != Some("<redacted>") {
                        *value = serde_json::Value::String("<redacted>".to_string());
                        changed = true;
                    }
                    continue;
                }
                changed |= redact_cli_json_value(value, depth + 1);
            }
            changed
        }
        serde_json::Value::String(value) => {
            let redacted = redact_value_at_depth(value, depth + 1);
            if redacted == *value {
                false
            } else {
                *value = redacted;
                true
            }
        }
        _ => false,
    }
}

fn redact_serialized_cli_values_at_depth(value: &str, depth: usize) -> String {
    if depth >= MAX_SERIALIZED_NESTING {
        return value.to_string();
    }
    let Ok(mut parsed) = serde_json::from_str::<serde_json::Value>(value) else {
        return value.to_string();
    };
    if !redact_cli_json_value(&mut parsed, depth) {
        return value.to_string();
    }
    serde_json::to_string(&parsed).unwrap_or_else(|_| value.to_string())
}

fn json_array_end(value: &str, start: usize) -> Option<usize> {
    let bytes = value.as_bytes();
    let mut depth = 0usize;
    let mut in_string = false;
    let mut escaped = false;
    for (offset, byte) in bytes[start..].iter().copied().enumerate() {
        if in_string {
            if escaped {
                escaped = false;
            } else if byte == b'\\' {
                escaped = true;
            } else if byte == b'"' {
                in_string = false;
            }
            continue;
        }
        match byte {
            b'"' => in_string = true,
            b'[' => depth += 1,
            b']' => {
                depth = depth.checked_sub(1)?;
                if depth == 0 {
                    return Some(start + offset + 1);
                }
            }
            _ => {}
        }
    }
    None
}

fn redact_embedded_cli_arrays_at_depth(value: &str, depth: usize) -> String {
    let mut out = String::with_capacity(value.len());
    let mut cursor = 0usize;
    while let Some(relative_start) = value[cursor..].find('[') {
        let start = cursor + relative_start;
        let Some(end) = json_array_end(value, start) else {
            // A malformed earlier bracket must not blind the scanner to a
            // later complete argv. Copy it and resume at the next byte.
            out.push_str(&value[cursor..=start]);
            cursor = start + 1;
            continue;
        };
        out.push_str(&value[cursor..start]);
        out.push_str(&redact_serialized_cli_values_at_depth(
            &value[start..end],
            depth,
        ));
        cursor = end;
    }
    out.push_str(&value[cursor..]);
    out
}

fn redact_value_at_depth(value: &str, depth: usize) -> String {
    let is_json =
        depth < MAX_SERIALIZED_NESTING && serde_json::from_str::<serde_json::Value>(value).is_ok();
    let value = redact_serialized_cli_values_at_depth(value, depth);
    // JSON values were traversed structurally, including recursively encoded
    // strings. Regexing their escaped serialization again can consume an
    // inner closing quote and corrupt a still-valid outer document.
    if is_json {
        return value;
    }
    let value = redact_embedded_cli_arrays_at_depth(&value, depth);
    redact_vendor_tokens(&redact_contextual(&value))
}

/// Redacts every credential-shaped token or contextual secret in `text`.
///
/// Applied to `argv_json`, `detail_json` and `object` before insert. The
/// prefix survives so a row stays identifiable — an operator can still see
/// that a GitHub token was involved — while the secret itself does not reach
/// the disk.
pub fn text(value: &str) -> String {
    redact_value_at_depth(value, 0)
}

/// Whether `text` carries anything the secret gate would stop.
///
/// Used where a value must be *refused* rather than stored redacted: a field
/// that cannot be safely stored is not the same as one that was stored safely.
pub fn carries_secret(value: &str) -> bool {
    text(value) != value
}

#[cfg(test)]
mod tests {
    use super::*;

    const KEY: &str = "ghp_0123456789abcdefghijklmnopqrstuvwxyzA";

    #[test]
    fn key_redaction_cost_stays_linear_in_the_number_of_keys() {
        // N distinct tokens of equal length all redact to identical text, so a
        // rename that probes linearly for a free name is quadratic on exactly
        // the input an attacker finds cheapest. Correctness is the assertion
        // that matters here: every entry must survive the disambiguation.
        let mut hostile = serde_json::Map::new();
        for index in 0..5000u32 {
            hostile.insert(format!("ghp_{index:036}"), serde_json::Value::from(index));
        }
        let document = serde_json::Value::Object(hostile).to_string();
        let started = std::time::Instant::now();
        let out = text(&document);
        let elapsed = started.elapsed();

        assert!(
            !out.contains("ghp_000000000000000000000000000000000001"),
            "leaked"
        );
        let parsed: serde_json::Value = serde_json::from_str(&out).expect("valid JSON");
        assert_eq!(
            parsed.as_object().map(serde_json::Map::len),
            Some(5000),
            "entries were dropped during collision disambiguation"
        );
        assert!(
            elapsed < std::time::Duration::from_secs(30),
            "took {elapsed:?}"
        );
    }

    #[test]
    fn redacts_a_credential_that_appears_as_an_object_key() {
        // A cache or rate-limit map keyed by the token. The value side was
        // already scanned; the key side was not scanned at all.
        for document in [
            r#"{"ghp_0123456789abcdefghijklmnopqrstuvwxyzA":"x"}"#,
            r#"{"cache":{"ghp_0123456789abcdefghijklmnopqrstuvwxyzA":1}}"#,
        ] {
            let out = text(document);
            assert!(
                !out.contains("ghp_0123456789abcdefghijklmnopqrstuvwxyzA"),
                "leaked from {document}: {out}"
            );
            assert!(
                serde_json::from_str::<serde_json::Value>(&out).is_ok(),
                "redaction produced invalid JSON: {out}"
            );
        }
    }

    #[test]
    fn keeps_both_entries_when_two_keys_redact_to_the_same_text() {
        // Two distinct tokens of equal length redact to identical text.
        // Overwriting would make the document claim there was only ever one.
        let document = r#"{"ghp_0123456789abcdefghijklmnopqrstuvwxyzA":1,"ghp_ZYXWVUTSRQPONMLKJIHGFEDCBA9876543210B":2}"#;
        let out = text(document);
        let parsed: serde_json::Value = serde_json::from_str(&out).expect("valid JSON");
        assert_eq!(
            parsed.as_object().map(serde_json::Map::len),
            Some(2),
            "an entry was dropped: {out}"
        );
    }

    #[test]
    fn key_redaction_is_idempotent() {
        let once = text(r#"{"ghp_0123456789abcdefghijklmnopqrstuvwxyzA":"x"}"#);
        assert_eq!(text(&once), once, "second pass changed {once}");
    }

    #[test]
    fn redacts_a_credential_named_by_its_json_key() {
        // The shape every OAuth response and most API error bodies have. Before
        // this, the object traversal handed the value to the contextual stage
        // with the key discarded, so an opaque token matched nothing and the
        // whole document was written through unchanged.
        for document in [
            r#"{"client_secret":"SUPERSECRETVALUE1234567890"}"#,
            r#"{"access_token":"ya29.OPAQUEVALUE1234567890abcdef"}"#,
            r#"{"clientSecret":"SUPERSECRETVALUE1234567890"}"#,
            r#"{"x-api-key":"SUPERSECRETVALUE1234567890"}"#,
            r#"{"Authorization":"Bearer SUPERSECRETVALUE1234567890"}"#,
            r#"{"cfg":{"password":"SUPERSECRETVALUE1234567890"}}"#,
            r#"{"github_token":"SUPERSECRETVALUE1234567890"}"#,
        ] {
            let out = text(document);
            assert!(
                !out.contains("SUPERSECRETVALUE1234567890")
                    && !out.contains("ya29.OPAQUEVALUE1234567890abcdef"),
                "leaked from {document}: {out}"
            );
        }
    }

    #[test]
    fn fails_closed_on_a_non_string_secret_value() {
        // A number, array or object under a credential key is still the
        // credential. Recursing into it would leave it whole.
        for document in [
            r#"{"password":1234567890}"#,
            r#"{"token":["a","b"]}"#,
            r#"{"secret":{"inner":"value"}}"#,
        ] {
            let out = text(document);
            assert!(out.contains("<redacted>"), "not redacted: {out}");
        }
    }

    #[test]
    fn redaction_by_key_is_idempotent() {
        // `carries_secret` is `text(v) != v`, so a second pass that keeps
        // rewriting would report an already-clean value as still carrying one.
        let once = text(r#"{"access_token":"SUPERSECRETVALUE1234567890"}"#);
        assert_eq!(text(&once), once, "second pass changed {once}");
        assert!(
            !carries_secret(&once),
            "clean value reported as secret-bearing"
        );
    }

    #[test]
    fn leaves_benign_key_shaped_names_alone() {
        // The suffix rule deliberately excludes the bare word `key`: redacting
        // these would strip the report of the facts it exists to carry.
        let document = r#"{"public_key":"ssh-ed25519 AAAA","cache_key":"abc","primary_key":"id"}"#;
        let out = text(document);
        assert!(out.contains("cache_key"), "{out}");
        assert!(out.contains("abc"), "over-redacted a benign key: {out}");
        assert!(out.contains("id"), "over-redacted a benign key: {out}");
    }

    #[test]
    fn normalizes_field_names_across_naming_styles() {
        assert_eq!(normalize_field_name("clientSecret"), "client_secret");
        assert_eq!(normalize_field_name("X-Api-Key"), "x_api_key");
        assert_eq!(normalize_field_name("ACCESS_TOKEN"), "access_token");
        assert_eq!(normalize_field_name("accessToken"), "access_token");
        assert!(is_secret_field_name("APIKey"));
        assert!(!is_secret_field_name("public_key"));
    }

    #[test]
    fn redacts_through_the_harness_pattern_table() {
        let out = text(&format!("git push https://{KEY}@github.com/o/r"));
        assert!(!out.contains(KEY));
        assert!(out.contains("ghp_"), "the shape stays identifiable: {out}");
    }

    #[test]
    fn redacts_distinct_vendor_tokens_on_one_physical_line() {
        let first = "ghp_0123456789abcdefghijklmnopqrstuvwxyzA";
        let second = "ghp_ZYXWVUTSRQPONMLKJIHGFEDCBA9876543210B";
        let out = text(&format!("target={first} detail={second}"));
        assert!(!out.contains(first), "the first credential survived: {out}");
        assert!(
            !out.contains(second),
            "the second credential survived: {out}"
        );
    }

    #[test]
    fn redacts_contextual_secrets_that_have_no_vendor_prefix() {
        let private_key = "-----BEGIN PRIVATE KEY-----\nopaque-key-body\n-----END PRIVATE KEY-----";
        let input = format!(
            "Authorization: Basic dXNlcjpwYXNz\n\
             Authorization: Digest opaque-digest-secret\n\
             Cookie: session=opaque-cookie\n\
             url=https://alice:opaque-password@example.test/repo\n\
             db=postgres://dbuser:opaque-database-password@example.test/app\n\
             {{\"Authorization\":\"Token opaque-json-auth\"}}\n\
             {{\"Cookie\":\"session=opaque-json-cookie\"}}\n\
             Authorization: Bearer opaque-inline-auth postgres://inline:opaque-inline-password@example.test/app\n\
             api_key=opaque-query-secret\n\
             {private_key}"
        );

        let out = text(&input);

        for secret in [
            "dXNlcjpwYXNz",
            "opaque-digest-secret",
            "session=opaque-cookie",
            "opaque-password",
            "opaque-database-password",
            "opaque-json-auth",
            "opaque-json-cookie",
            "opaque-inline-auth",
            "opaque-inline-password",
            "opaque-query-secret",
            "opaque-key-body",
        ] {
            assert!(!out.contains(secret), "contextual secret survived: {out}");
        }
        assert!(out.contains("Authorization: Basic <redacted>"));
        assert!(out.contains("Authorization: Digest <redacted>"));
        assert!(out.contains("Cookie: <redacted>"));
        assert!(out.contains("https://alice:<redacted>@example.test/repo"));
        assert!(out.contains("postgres://dbuser:<redacted>@example.test/app"));
        assert!(out.contains("{\"Authorization\":\"<redacted>\"}"));
        assert!(out.contains("{\"Cookie\":\"<redacted>\"}"));
        assert!(out.contains(
            "Authorization: Bearer <redacted> postgres://inline:<redacted>@example.test/app"
        ));
        assert!(out.contains("api_key=<redacted>"));
        assert!(out.contains("<private key redacted>"));
        assert_eq!(text(&out), out, "contextual redaction must be idempotent");
    }

    #[test]
    fn redacts_contextual_secrets_inside_serialized_argv_without_corrupting_neighbors() {
        let cases = [
            (
                r#"["git","-c","http.extraHeader=Authorization: Bearer opaque-auth","fetch"]"#,
                r#"["git","-c","http.extraHeader=Authorization: Bearer <redacted>","fetch"]"#,
                "opaque-auth",
            ),
            (
                "git -c 'http.extraHeader=Authorization: Basic opaque-basic' fetch",
                "git -c 'http.extraHeader=Authorization: Basic <redacted>' fetch",
                "opaque-basic",
            ),
            (
                r#"["git","-c","http.extraHeader=Cookie: session=opaque-cookie","fetch"]"#,
                r#"["git","-c","http.extraHeader=Cookie: <redacted>","fetch"]"#,
                "opaque-cookie",
            ),
            (
                r#"["tool","--config","api_key=opaque-key","next"]"#,
                r#"["tool","--config","api_key=<redacted>","next"]"#,
                "opaque-key",
            ),
        ];

        for (input, expected, secret) in cases {
            let out = text(input);
            assert!(!out.contains(secret), "serialized secret survived: {out}");
            assert_eq!(
                out, expected,
                "argv structure or neighboring values changed"
            );
            assert_eq!(text(&out), out, "serialized redaction must be idempotent");
        }
    }

    #[test]
    fn redacts_escaped_and_ecosystem_credentials_inside_serialized_payloads() {
        let cases = [
            (
                r#"["tool","Authorization: Bearer opaque-auth\\nnext","tail"]"#,
                r#"["tool","Authorization: Bearer <redacted>","tail"]"#,
                "opaque-auth",
            ),
            (
                r#"["tool","Cookie: session=opaque-cookie\\nnext","tail"]"#,
                r#"["tool","Cookie: <redacted>","tail"]"#,
                "opaque-cookie",
            ),
            (
                r#"["tool","AWS_SECRET_ACCESS_KEY=opaque-aws","next"]"#,
                r#"["tool","AWS_SECRET_ACCESS_KEY=<redacted>","next"]"#,
                "opaque-aws",
            ),
            (
                r#"["tool","-----BEGIN PGP PRIVATE KEY BLOCK-----\\nopaque-key-body\\n-----END PGP PRIVATE KEY BLOCK-----","next"]"#,
                r#"["tool","<private key redacted>","next"]"#,
                "opaque-key-body",
            ),
            (
                r#"["tool","-----BEGIN PRIVATE KEY-----\\nopaque-pem-body\\n-----END PRIVATE KEY-----","next"]"#,
                r#"["tool","<private key redacted>","next"]"#,
                "opaque-pem-body",
            ),
        ];

        for (input, expected, secret) in cases {
            let out = text(input);
            assert!(!out.contains(secret), "serialized secret survived: {out}");
            assert_eq!(out, expected, "serialized payload structure changed");
            assert_eq!(text(&out), out, "serialized redaction must be idempotent");
        }
    }

    #[test]
    fn preserves_the_remainder_after_already_redacted_escaped_headers() {
        let input =
            r#"Authorization: Basic <redacted>\nCookie: <redacted>\nhttps://example.test/repo"#;

        assert_eq!(text(input), input);
    }

    #[test]
    fn redacts_separate_value_cli_credentials_without_losing_argv_shape() {
        let cases = [
            (
                r#"["tool","--password","opaque-password","next"]"#,
                r#"["tool","--password","<redacted>","next"]"#,
                "opaque-password",
            ),
            (
                r#"["tool","--access-token","opaque-access-token","next"]"#,
                r#"["tool","--access-token","<redacted>","next"]"#,
                "opaque-access-token",
            ),
            (
                r#"["curl","--user","alice:opaque-basic-password","https://example.test"]"#,
                r#"["curl","--user","alice:<redacted>","https://example.test"]"#,
                "opaque-basic-password",
            ),
            (
                r#"argv=["tool","--password","opaque-wrapped-password","next"]"#,
                r#"argv=["tool","--password","<redacted>","next"]"#,
                "opaque-wrapped-password",
            ),
            (
                "command=tool --access-token opaque-shell-token next",
                "command=tool --access-token <redacted> next",
                "opaque-shell-token",
            ),
            (
                "command=curl --user alice:opaque-shell-password https://example.test",
                "command=curl --user <redacted> https://example.test",
                "opaque-shell-password",
            ),
            (
                r#"phase [broken argv=["tool","--password","opaque-after-broken-bracket","next"]"#,
                r#"phase [broken argv=["tool","--password","<redacted>","next"]"#,
                "opaque-after-broken-bracket",
            ),
            (
                r#"[unclosed prefix ["curl","--user","alice:opaque-after-unclosed","https://example.test"] suffix"#,
                r#"[unclosed prefix ["curl","--user","alice:<redacted>","https://example.test"] suffix"#,
                "opaque-after-unclosed",
            ),
            (
                r#"{"message":"argv=[\"tool\",\"--password\",\"opaque-nested-password\",\"next\"]"}"#,
                r#"{"message":"argv=[\"tool\",\"--password\",\"<redacted>\",\"next\"]"}"#,
                "opaque-nested-password",
            ),
        ];

        for (input, expected, secret) in cases {
            let out = text(input);
            assert!(!out.contains(secret), "separate CLI secret survived: {out}");
            assert_eq!(out, expected, "serialized argv structure changed");
            if serde_json::from_str::<serde_json::Value>(input).is_ok() {
                serde_json::from_str::<serde_json::Value>(&out).unwrap_or_else(|error| {
                    panic!("redacted payload is invalid JSON: {error}: {out}")
                });
            }
            assert_eq!(text(&out), out, "CLI redaction must be idempotent");
        }
    }

    #[test]
    fn redacts_credentials_in_recursively_serialized_strings_without_corrupting_json() {
        let cli_secret = "opaque-depth-cli";
        let argv = serde_json::json!(["tool", "--password", cli_secret, "next"]).to_string();
        let nested = serde_json::json!({ "message": argv }).to_string();
        let outer = serde_json::json!({ "message": nested }).to_string();

        let cli_out = text(&outer);
        assert!(
            !cli_out.contains(cli_secret),
            "nested CLI secret survived: {cli_out}"
        );
        let cli_outer =
            serde_json::from_str::<serde_json::Value>(&cli_out).unwrap_or_else(|error| {
                panic!("nested CLI redaction corrupted JSON: {error}: {cli_out}")
            });
        let cli_nested = cli_outer["message"]
            .as_str()
            .expect("nested CLI JSON string");
        let cli_nested = serde_json::from_str::<serde_json::Value>(cli_nested)
            .expect("first nested CLI JSON must remain valid");
        serde_json::from_str::<serde_json::Value>(
            cli_nested["message"]
                .as_str()
                .expect("serialized argv string"),
        )
        .expect("serialized argv must remain valid");

        for (name, secret, payload) in [
            (
                "authorization",
                "opaque-depth-auth",
                "Authorization: Bearer opaque-depth-auth",
            ),
            (
                "cookie",
                "opaque-depth-cookie",
                "Cookie: session=opaque-depth-cookie",
            ),
            ("named", "opaque-depth-key", "api_key=opaque-depth-key"),
        ] {
            let nested = serde_json::json!({ "message": payload }).to_string();
            let outer = serde_json::json!({ "message": nested }).to_string();
            let out = text(&outer);
            assert!(
                !out.contains(secret),
                "nested {name} secret survived: {out}"
            );
            let parsed = serde_json::from_str::<serde_json::Value>(&out).unwrap_or_else(|error| {
                panic!("nested {name} redaction corrupted JSON: {error}: {out}")
            });
            serde_json::from_str::<serde_json::Value>(
                parsed["message"]
                    .as_str()
                    .expect("nested diagnostic JSON string"),
            )
            .unwrap_or_else(|error| {
                panic!("nested {name} JSON string was corrupted: {error}: {out}")
            });
            assert_eq!(text(&out), out, "nested {name} redaction is not idempotent");
        }
    }

    #[test]
    fn fails_closed_when_structured_json_reaches_the_serialized_nesting_cap() {
        let secret = "opaque-boundary-auth";
        let mut payload = serde_json::Value::String(format!("Authorization: Bearer {secret}"));
        for _ in 0..40 {
            payload = serde_json::json!({ "message": payload });
        }
        let input = serde_json::to_string(&payload).expect("serialize nested diagnostic");

        let out = text(&input);

        assert!(!out.contains(secret), "nesting-cap secret survived: {out}");
        serde_json::from_str::<serde_json::Value>(&out)
            .unwrap_or_else(|error| panic!("nesting-cap redaction corrupted JSON: {error}: {out}"));
        assert_eq!(text(&out), out, "nesting-cap redaction is not idempotent");
    }

    #[test]
    fn ordinary_command_lines_are_returned_unchanged() {
        // The property that keeps this on the write path: a redactor that
        // mangles innocent argv gets turned off, and then nothing is redacted.
        for ordinary in [
            r#"["git","commit","-m","fix the task-runner"]"#,
            r#"["git","rebase","--continue"]"#,
            r#"["cargo","test","--workspace"]"#,
        ] {
            assert_eq!(text(ordinary), ordinary);
        }
    }

    #[test]
    fn detection_and_redaction_agree() {
        assert!(carries_secret(KEY));
        assert!(!carries_secret("git status"));
        // Anything reported as carrying a secret must actually change.
        assert_ne!(text(KEY), KEY);
    }

    #[test]
    fn redacting_twice_changes_nothing_the_second_time() {
        let once = text(KEY);
        assert_eq!(text(&once), once, "a credential survived the first pass");
        assert!(!carries_secret(&once));
    }
}
