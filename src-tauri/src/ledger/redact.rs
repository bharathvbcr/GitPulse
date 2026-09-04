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

fn is_separate_secret_flag(value: &str) -> bool {
    matches!(
        normalized_cli_flag(value).as_deref(),
        Some(
            "password"
                | "passwd"
                | "api_key"
                | "apikey"
                | "access_token"
                | "refresh_token"
                | "client_secret"
                | "secret"
                | "token"
                | "auth_token"
                | "oauth_token"
                | "oauth2_bearer"
                | "aws_secret_access_key"
                | "aws_session_token"
                | "aws_access_key_id"
        )
    )
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
            let mut changed = false;
            for value in values.values_mut() {
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
