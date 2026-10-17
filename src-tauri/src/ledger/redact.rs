//! Credential redaction on the ledger's write path.
//!
//! Vendor-shaped tokens delegate to `dc_redact`, whose `SECRET_PATTERNS` table
//! — the one dc-verify's secret gate also reads — is the single owner of those
//! signatures. This module additionally owns context-shaped credentials
//! (authorization/cookie headers,
//! URL passwords, named secret fields, and private-key blocks) that cannot be
//! identified safely by a token prefix alone.
//!
//! The alternative was to copy the pattern table into GitPulse. That is how a
//! key redacted by one gate leaks out of another: two tables drift, and the
//! one that drifts is discovered by the leak.

use regex::{Captures, Regex, RegexSet, RegexSetBuilder, Replacer};
use std::borrow::Cow;
use std::collections::HashMap;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::OnceLock;

/// The contextual stage's patterns, in the order the chain applies them.
///
/// The one owner: the compiled `Regex` slots and the `RegexSet` that gates the
/// chain are both built from this table, so the gate cannot describe a pattern
/// the chain does not run. `{names}` is replaced by the canonical credential
/// names (see [`resolved_pattern`]).
const CONTEXTUAL_PATTERNS: [&str; 15] = [
    // Private-key blocks go first. dc-redact deliberately recognizes the PEM
    // prefix as a secret token, but token-oriented matching stops at the first
    // space and cannot remove the body after it has rewritten that prefix.
    r"(?is)-----BEGIN[^\r\n]*?PRIVATE KEY(?: BLOCK)?-----.*?(?:-----END[^\r\n]*?PRIVATE KEY(?: BLOCK)?-----|$)",
    r#"(?i)(["']?authorization["']?\s*[:=]\s*)()("[^"\r\n]*"|'[^'\r\n]*')"#,
    r#"(?i)(authorization\s*[:=]\s*)(?:(basic|bearer|digest|token|negotiate|aws4-hmac-sha256)\s+)?((?:\\.|[^"\\\r\n])+?)(")"#,
    r#"(?i)(authorization\s*[:=]\s*)(?:(basic|bearer|digest|token|negotiate|aws4-hmac-sha256)\s+)?([^"'\\\r\n]+?)(\\?["'])"#,
    r#"(?im)(["']?authorization["']?\s*[:=]\s*)(?:(basic|bearer|digest|token|negotiate|aws4-hmac-sha256)\s+)?([^"'\\\r\n]*?)(\s+[a-z][a-z0-9+.-]*://[^\\\r\n]*)?(?:\\.*)?$"#,
    r#"(?i)(["']?(?:set-)?cookie["']?\s*[:=]\s*)("[^"\r\n]*"|'[^'\r\n]*')"#,
    r#"(?i)((?:set-)?cookie\s*[:=]\s*)((?:\\.|[^"\\\r\n])+?)(")"#,
    r#"(?i)((?:set-)?cookie\s*[:=]\s*)([^"'\\\r\n]+?)(\\?["'])"#,
    r#"(?im)(["']?(?:set-)?cookie["']?\s*[:=]\s*)([^"'\\\r\n]+?)(?:\\.*)?$"#,
    // The password runs to the LAST `@` of the whitespace-delimited word, so a
    // password containing `/` or `@` is removed whole. Over-redacting the tail
    // of a path that also holds an `@` is the safe direction. `<` and `>` end
    // it so an already-redacted `<redacted>` is not matched twice.
    r#"(?i)([a-z][a-z0-9+.-]*://[^/\s:@]*:)([^\s'"<>,]*)@"#,
    r#"(?i)((?:{names})\s*[:=]\s*)((?:\\.|[^"\\\r\n])+?)(")"#,
    r#"(?i)((?:{names})\s*[:=]\s*)([^"'\\\r\n]+?)(\\?["'])"#,
    r#"(?i)(["']?(?:{names})["']?\s*[:=]\s*)("[^"\r\n]*"|'[^'\r\n]*'|[^\s,;&\\"'\]]+)"#,
    r#"(?i)((?:--password|--passwd|--api-key|--apikey|--access-token|--refresh-token|--client-secret|--secret|--token|--auth-token|--oauth-token|--oauth2-bearer|--aws-secret-access-key|--aws-session-token|--aws-access-key-id)\s+)("[^"\r\n]*"|'[^'\r\n]*'|[^\s"'\\]+)"#,
    r#"(?i)((?:--user|--userpass|--proxy-user|-u)\s+)("[^"\r\n]*:[^"\r\n]*"|'[^'\r\n]*:[^'\r\n]*'|[^\s"'\\:]+:[^\s"'\\]+)"#,
];

// Positions in `CONTEXTUAL_PATTERNS`.
const PRIVATE_KEY: usize = 0;
const QUOTED_AUTHORIZATION: usize = 1;
const DOUBLE_QUOTED_AUTHORIZATION: usize = 2;
const EMBEDDED_AUTHORIZATION: usize = 3;
const AUTHORIZATION: usize = 4;
const QUOTED_COOKIE: usize = 5;
const DOUBLE_QUOTED_COOKIE: usize = 6;
const EMBEDDED_COOKIE: usize = 7;
const COOKIE: usize = 8;
const URL_PASSWORD: usize = 9;
const DOUBLE_QUOTED_NAMED_SECRET: usize = 10;
const EMBEDDED_NAMED_SECRET: usize = 11;
const NAMED_SECRET: usize = 12;
const SHELL_SECRET_FLAG: usize = 13;
const SHELL_USERINFO_FLAG: usize = 14;

const URL_TOKEN_USERINFO_PATTERN: &str = r"(?i)([a-z][a-z0-9+.-]*://)([a-z0-9_.~%-]{20,})@";

/// How many `redact_contextual` calls a process runs the full chain without
/// the gate before it builds and uses the `RegexSet`.
///
/// Derivation (release build, Apple silicon, min of 25 fresh processes):
/// - Building the set costs about 1.45 ms: the first `text` call is 4.16 ms
///   with the set built eagerly against 2.72 ms with only the chain.
/// - Consulting it saves a few microseconds per call on a leaf no pattern
///   matches: 9.3 us on a 4-field JSON detail, 1.7 us on a path, 6.2 us on a
///   hook session marker (steady-state `text`, set gated vs chain only), about
///   5.6 us on average.
/// - Break-even is therefore about 1450 / 5.6 = 260 calls (160 to 850 across
///   those leaves). Switching at the break-even bounds the regret of either
///   choice to 2x the better one (ski rental), so N is the nearest power of
///   two, 256.
/// - A `gitpulse-hook` invocation makes 0 to 2 contextual calls on its normal
///   paths (counted per subcommand), 100x below N, so it never builds the
///   set. A long-lived writer (the ledger, the daemon) passes N within its
///   first rows and then keeps the gate for the rest of its life.
const UNGATED_CALLS: usize = 256;

/// The contextual stage. The chain and the gate are built from the same
/// `CONTEXTUAL_PATTERNS` table but in separate slots, so a process that only
/// ever runs the chain never pays for the set.
///
/// A text that matches no pattern of the set is unchanged by the whole chain,
/// because a replacement needs a match; running the chain without consulting
/// the set therefore yields the same output, only slower on a clean leaf.
struct ContextualStage {
    chain: OnceLock<Vec<Regex>>,
    gate: OnceLock<RegexSet>,
    calls: AtomicUsize,
}

impl ContextualStage {
    const fn new() -> Self {
        Self {
            chain: OnceLock::new(),
            gate: OnceLock::new(),
            calls: AtomicUsize::new(0),
        }
    }

    fn patterns() -> Vec<String> {
        let names = credential_name_alternation();
        CONTEXTUAL_PATTERNS
            .iter()
            .map(|pattern| resolved_pattern(pattern, &names))
            .collect()
    }

    fn chain(&self) -> &[Regex] {
        self.chain.get_or_init(|| {
            Self::patterns()
                .iter()
                .map(|pattern| Regex::new(pattern).expect("static credential regex must compile"))
                .collect()
        })
    }

    fn gate(&self) -> &RegexSet {
        self.gate.get_or_init(|| {
            // The set is the union of the chain, so it may need more than the
            // default 10 MiB that one pattern gets.
            RegexSetBuilder::new(Self::patterns())
                .size_limit(64 << 20)
                .build()
                .expect("credential regex set must compile")
        })
    }

    /// True when this call should consult the gate. The first `UNGATED_CALLS`
    /// calls do not. Relaxed is enough: a race only shifts which call builds
    /// the set, never the output.
    fn use_gate(&self) -> bool {
        if self.calls.load(Ordering::Relaxed) < UNGATED_CALLS {
            self.calls.fetch_add(1, Ordering::Relaxed);
            false
        } else {
            true
        }
    }
}

static CONTEXTUAL: ContextualStage = ContextualStage::new();
static URL_TOKEN_USERINFO: OnceLock<Regex> = OnceLock::new();

/// The credential names `{names}` stands for.
///
/// These three names have distinct header value grammars above. Every ordinary
/// assignment uses the same credential-name owner as JSON fields and CLI flags,
/// including vendor-prefixed suffixes.
fn credential_name_alternation() -> String {
    let mut names: Vec<String> = SECRET_FIELD_NAMES
        .iter()
        .chain(SECRET_FIELD_SUFFIXES.iter())
        .filter(|name| !matches!(**name, "authorization" | "cookie" | "set_cookie"))
        .map(|name| regex::escape(name).replace('_', "[_. -]?"))
        .collect();
    names.sort();
    names.dedup();
    names.join("|")
}

fn resolved_pattern(pattern: &str, names: &str) -> String {
    pattern.replace("{names}", names)
}

fn url_token_userinfo() -> &'static Regex {
    URL_TOKEN_USERINFO.get_or_init(|| {
        Regex::new(URL_TOKEN_USERINFO_PATTERN).expect("static credential regex must compile")
    })
}

/// `replace_all` that hands the input back, not a copy, when nothing matched.
fn replaced<'a>(regex: &Regex, text: Cow<'a, str>, replacement: impl Replacer) -> Cow<'a, str> {
    let changed = match regex.replace_all(&text, replacement) {
        Cow::Owned(out) => Some(out),
        Cow::Borrowed(_) => None,
    };
    changed.map_or(text, Cow::Owned)
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

fn redact_contextual(value: Cow<'_, str>) -> Cow<'_, str> {
    redact_contextual_with(&CONTEXTUAL, value)
}

fn redact_contextual_with<'a>(stage: &ContextualStage, value: Cow<'a, str>) -> Cow<'a, str> {
    // Every replacement needs a match, and the set matches the input when any
    // chain pattern does, so no match here means the chain is the identity.
    // When one does match, the full chain runs in order: an earlier
    // replacement can create or destroy a later match, so only running the
    // matching patterns would not be the same function.
    if stage.use_gate() && !stage.gate().is_match(&value) {
        return value;
    }
    let chain = stage.chain();
    let out = value;
    let out = replaced(&chain[PRIVATE_KEY], out, "<private key redacted>");
    let out = replaced(&chain[QUOTED_AUTHORIZATION], out, redacted_authorization);
    let out = replaced(
        &chain[DOUBLE_QUOTED_AUTHORIZATION],
        out,
        redacted_authorization,
    );
    let out = replaced(&chain[EMBEDDED_AUTHORIZATION], out, redacted_authorization);
    let out = replaced(&chain[AUTHORIZATION], out, redacted_authorization);
    let out = replaced(&chain[QUOTED_COOKIE], out, redacted_assignment);
    let out = replaced(
        &chain[DOUBLE_QUOTED_COOKIE],
        out,
        redacted_embedded_assignment,
    );
    let out = replaced(&chain[EMBEDDED_COOKIE], out, redacted_embedded_assignment);
    let out = replaced(&chain[COOKIE], out, redacted_assignment);
    let out = replaced(&chain[URL_PASSWORD], out, |caps: &Captures<'_>| {
        // `host:8443/@scope/pkg` is a port and a path, not a password.
        let looks_like_port = caps[2]
            .split_once('/')
            .is_some_and(|(head, _)| !head.is_empty() && head.bytes().all(|b| b.is_ascii_digit()));
        if looks_like_port {
            caps[0].to_string()
        } else {
            format!("{}<redacted>@", &caps[1])
        }
    });
    let out = replaced(
        &chain[DOUBLE_QUOTED_NAMED_SECRET],
        out,
        redacted_embedded_assignment,
    );
    let out = replaced(
        &chain[EMBEDDED_NAMED_SECRET],
        out,
        redacted_embedded_assignment,
    );
    let out = replaced(&chain[NAMED_SECRET], out, redacted_assignment);
    let out = replaced(&chain[SHELL_SECRET_FLAG], out, redacted_assignment);
    replaced(&chain[SHELL_USERINFO_FLAG], out, redacted_assignment)
}

/// A token used as the whole URL userinfo (`https://<token>@host`). Runs after
/// the vendor patterns, which keep a recognisable prefix for the tokens they
/// know; this catches the rest. Short names such as `git` are usernames, not
/// secrets, so only a long credential-shaped run containing a digit qualifies.
fn redact_userinfo_tokens(value: Cow<'_, str>) -> String {
    let out = replaced(url_token_userinfo(), value, |caps: &Captures<'_>| {
        if caps[2].bytes().any(|b| b.is_ascii_digit()) {
            format!("{}<redacted>@", &caps[1])
        } else {
            caps[0].to_string()
        }
    });
    out.into_owned()
}

fn redact_vendor_tokens(value: Cow<'_, str>) -> Cow<'_, str> {
    // dc-redact defines a token boundary as whitespace, quote, or comma, but
    // its public helper returns only the first distinct value for one vendor
    // on a line. Apply that same owner independently to every such segment so
    // a short/previously-redacted candidate cannot hide a later credential.
    // Delimiters are copied byte-for-byte; no credential pattern is duplicated
    // here.
    //
    // A segment holds no whitespace, so it is one line, and for one line
    // `!contains_secret(line)` means `detections(line)` is empty, which makes
    // `redact_secrets(line)` return the line unchanged (dc-redact lib.rs:
    // `contains_secret` and `redact_pass`). Such a segment is copied without
    // asking `redact_secrets` for a rewrite. Output is built lazily, so a
    // value with no credential segment is returned as it came.
    let text: &str = &value;
    let mut rewritten = VendorRewrite {
        out: None,
        copied: 0,
    };
    let mut start = 0;
    for (index, ch) in text.char_indices() {
        if !(ch.is_whitespace() || matches!(ch, '"' | '\'' | ',')) {
            continue;
        }
        if start < index {
            rewritten.segment(text, start, index);
        }
        start = index + ch.len_utf8();
    }
    if start < text.len() {
        rewritten.segment(text, start, text.len());
    }
    match rewritten.out {
        None => value,
        Some(mut out) => {
            out.push_str(&text[rewritten.copied..]);
            Cow::Owned(out)
        }
    }
}

/// The output of [`redact_vendor_tokens`] so far: nothing until a segment
/// actually carries a credential, then everything up to `copied` with that
/// segment rewritten.
struct VendorRewrite {
    out: Option<String>,
    copied: usize,
}

impl VendorRewrite {
    fn segment(&mut self, text: &str, from: usize, to: usize) {
        let segment = &text[from..to];
        if !dc_redact::contains_secret(segment) {
            return;
        }
        let out = self
            .out
            .get_or_insert_with(|| String::with_capacity(text.len()));
        out.push_str(&text[self.copied..from]);
        out.push_str(&dc_redact::redact_secrets(segment));
        self.copied = to;
    }
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

/// Redacts an already-parsed JSON document, handing back `value` itself (the
/// text it was parsed from) when nothing in it needed to change, so an
/// unchanged document is never re-serialized.
fn redact_parsed_json(value: &str, mut parsed: serde_json::Value, depth: usize) -> String {
    if !redact_cli_json_value(&mut parsed, depth) {
        return value.to_string();
    }
    serde_json::to_string(&parsed).unwrap_or_else(|_| value.to_string())
}

fn redact_serialized_cli_values_at_depth(value: &str, depth: usize) -> String {
    if depth >= MAX_SERIALIZED_NESTING {
        return value.to_string();
    }
    let Ok(parsed) = serde_json::from_str::<serde_json::Value>(value) else {
        return value.to_string();
    };
    redact_parsed_json(value, parsed, depth)
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

fn redact_embedded_cli_arrays_at_depth(value: &str, depth: usize) -> Cow<'_, str> {
    // No bracket, no array: the scan below would copy the value unchanged.
    if !value.contains('[') {
        return Cow::Borrowed(value);
    }
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
    if out == value {
        Cow::Borrowed(value)
    } else {
        Cow::Owned(out)
    }
}

fn redact_value_at_depth(value: &str, depth: usize) -> String {
    // One parse per string. A JSON value was traversed structurally, including
    // recursively encoded strings, and is returned as that pass left it:
    // regexing its escaped serialization again can consume an inner closing
    // quote and corrupt a still-valid outer document. A string at or past the
    // nesting cap is never parsed, and is treated as plain text.
    if depth < MAX_SERIALIZED_NESTING {
        if let Ok(parsed) = serde_json::from_str::<serde_json::Value>(value) {
            return redact_parsed_json(value, parsed, depth);
        }
    }
    let arrays = redact_embedded_cli_arrays_at_depth(value, depth);
    redact_userinfo_tokens(redact_vendor_tokens(redact_contextual(arrays)))
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

/// A remote URL as it may be shown, and whether the original embedded a secret.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UrlDisplay {
    pub display: String,
    pub has_credentials: bool,
}

/// The display form of a remote URL: the userinfo's password is dropped and
/// the username kept, so the configured account stays visible without the
/// token.
///
/// - `https://user:tok@host/p` becomes `https://user@host/p`.
/// - A lone userinfo on a non-ssh scheme (`https://ghp_xxx@host`) is dropped
///   when [`text`] judges it a token; a plain name (`https://alice@host`) is
///   kept. A lone `ssh://` / `git+ssh://` userinfo is an account name, kept.
/// - scp-like `git@host:path`, local paths and URLs without userinfo are
///   returned unchanged: they carry no password component.
///
/// The authority is read as git does (up to the first `/`, `?` or `#`) and its
/// userinfo ends at the last `@`, so percent-encoded userinfo and bracketed
/// IPv6 hosts survive. The result is also passed through [`text`], so any
/// other secret shape in the string is removed too.
pub fn url_for_display(url: &str) -> UrlDisplay {
    let unchanged = |url: &str| {
        let display = text(url);
        UrlDisplay {
            has_credentials: display != url,
            display,
        }
    };
    let Some((scheme, after)) = url.split_once("://") else {
        return unchanged(url);
    };
    let scheme_ok = scheme
        .chars()
        .next()
        .is_some_and(|c| c.is_ascii_alphabetic())
        && scheme
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '+' | '.' | '-'));
    let authority_end = after.find(['/', '?', '#']).unwrap_or(after.len());
    let authority = &after[..authority_end];
    let Some(at) = authority.rfind('@').filter(|_| scheme_ok) else {
        return unchanged(url);
    };
    let (userinfo, host) = (&authority[..at], &authority[at + 1..]);
    let tail = &after[authority_end..];
    let (user, password) = match userinfo.split_once(':') {
        Some((user, password)) => (user, Some(password)),
        None => (userinfo, None),
    };
    let is_ssh = scheme.to_ascii_lowercase().ends_with("ssh");
    let user_is_secret = !is_ssh && {
        let probe = format!("{scheme}://{user}@{host}");
        text(&probe) != probe
    };
    let keep_user = !user.is_empty() && !user_is_secret;
    let has_credentials = password.is_some_and(|p| !p.is_empty()) || user_is_secret;
    let display = if keep_user {
        format!("{scheme}://{user}@{host}{tail}")
    } else {
        format!("{scheme}://{host}{tail}")
    };
    UrlDisplay {
        display: text(&display),
        has_credentials,
    }
}

/// The redactor as it was before the single-parse, gated, copy-on-write
/// rewrite: the oracle every equivalence test compares against.
///
/// The bodies below are the previous implementation copied verbatim from
/// `0f0e0b4b` (statics and compilers, `redact_contextual`,
/// `redact_userinfo_tokens`, `redact_vendor_tokens`, the key and value
/// recursion, and `text`). Only the helpers whose bodies did not change
/// (replacement closures, credential-name table, argv rewriting, bracket
/// scanning) are shared with the production module. Do not edit these bodies to
/// follow a production change: a change that makes them differ is exactly what
/// the equivalence tests exist to report.
#[cfg(test)]
mod reference {
    use super::{
        is_secret_field_name, json_array_end, redact_cli_array, redacted_assignment,
        redacted_authorization, redacted_embedded_assignment, SECRET_FIELD_NAMES,
        SECRET_FIELD_SUFFIXES,
    };
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
    static URL_TOKEN_USERINFO: OnceLock<Regex> = OnceLock::new();
    static DOUBLE_QUOTED_NAMED_SECRET: OnceLock<Regex> = OnceLock::new();
    static EMBEDDED_NAMED_SECRET: OnceLock<Regex> = OnceLock::new();
    static NAMED_SECRET: OnceLock<Regex> = OnceLock::new();
    static SHELL_SECRET_FLAG: OnceLock<Regex> = OnceLock::new();
    static SHELL_USERINFO_FLAG: OnceLock<Regex> = OnceLock::new();

    fn compiled(slot: &'static OnceLock<Regex>, pattern: &str) -> &'static Regex {
        slot.get_or_init(|| Regex::new(pattern).expect("static credential regex must compile"))
    }

    fn compiled_named(slot: &'static OnceLock<Regex>, pattern: &str) -> &'static Regex {
        slot.get_or_init(|| {
            // These three names have distinct header value grammars above.
            // Every ordinary assignment uses the same credential-name owner as
            // JSON fields and CLI flags, including vendor-prefixed suffixes.
            let mut names: Vec<String> = SECRET_FIELD_NAMES
                .iter()
                .chain(SECRET_FIELD_SUFFIXES.iter())
                .filter(|name| !matches!(**name, "authorization" | "cookie" | "set_cookie"))
                .map(|name| regex::escape(name).replace('_', "[_. -]?"))
                .collect();
            names.sort();
            names.dedup();
            Regex::new(&pattern.replace("{names}", &names.join("|")))
                .expect("canonical credential-name regex must compile")
        })
    }
    pub(super) fn redact_contextual(value: &str) -> String {
        // Private-key blocks go first. dc-redact deliberately recognizes the PEM
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
        // The password runs to the LAST `@` of the whitespace-delimited word, so a
        // password containing `/` or `@` is removed whole. Over-redacting the tail
        // of a path that also holds an `@` is the safe direction. `<` and `>` end
        // it so an already-redacted `<redacted>` is not matched twice.
        let url_password = compiled(
            &URL_PASSWORD,
            r#"(?i)([a-z][a-z0-9+.-]*://[^/\s:@]*:)([^\s'"<>,]*)@"#,
        );
        let named_secret = compiled_named(
            &NAMED_SECRET,
            r#"(?i)(["']?(?:{names})["']?\s*[:=]\s*)("[^"\r\n]*"|'[^'\r\n]*'|[^\s,;&\\"'\]]+)"#,
        );
        let embedded_named_secret = compiled_named(
            &EMBEDDED_NAMED_SECRET,
            r#"(?i)((?:{names})\s*[:=]\s*)([^"'\\\r\n]+?)(\\?["'])"#,
        );
        let double_quoted_named_secret = compiled_named(
            &DOUBLE_QUOTED_NAMED_SECRET,
            r#"(?i)((?:{names})\s*[:=]\s*)((?:\\.|[^"\\\r\n])+?)(")"#,
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
        let out = url_password
            .replace_all(&out, |caps: &Captures<'_>| {
                // `host:8443/@scope/pkg` is a port and a path, not a password.
                let looks_like_port = caps[2].split_once('/').is_some_and(|(head, _)| {
                    !head.is_empty() && head.bytes().all(|b| b.is_ascii_digit())
                });
                if looks_like_port {
                    caps[0].to_string()
                } else {
                    format!("{}<redacted>@", &caps[1])
                }
            })
            .into_owned();
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

    /// A token used as the whole URL userinfo (`https://<token>@host`). Runs after
    /// the vendor patterns, which keep a recognisable prefix for the tokens they
    /// know; this catches the rest. Short names such as `git` are usernames, not
    /// secrets, so only a long credential-shaped run containing a digit qualifies.
    fn redact_userinfo_tokens(value: &str) -> String {
        let url_token_userinfo = compiled(
            &URL_TOKEN_USERINFO,
            r"(?i)([a-z][a-z0-9+.-]*://)([a-z0-9_.~%-]{20,})@",
        );
        url_token_userinfo
            .replace_all(value, |caps: &Captures<'_>| {
                if caps[2].bytes().any(|b| b.is_ascii_digit()) {
                    format!("{}<redacted>@", &caps[1])
                } else {
                    caps[0].to_string()
                }
            })
            .into_owned()
    }

    fn redact_vendor_tokens(value: &str) -> String {
        // dc-redact defines a token boundary as whitespace, quote, or comma, but
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
                out.push_str(&dc_redact::redact_secrets(&value[start..index]));
            }
            out.push(ch);
            start = index + ch.len_utf8();
        }
        if start < value.len() {
            out.push_str(&dc_redact::redact_secrets(&value[start..]));
        }
        out
    }
    const MAX_SERIALIZED_NESTING: usize = 32;
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
        let is_json = depth < MAX_SERIALIZED_NESTING
            && serde_json::from_str::<serde_json::Value>(value).is_ok();
        let value = redact_serialized_cli_values_at_depth(value, depth);
        // JSON values were traversed structurally, including recursively encoded
        // strings. Regexing their escaped serialization again can consume an
        // inner closing quote and corrupt a still-valid outer document.
        if is_json {
            return value;
        }
        let value = redact_embedded_cli_arrays_at_depth(&value, depth);
        redact_userinfo_tokens(&redact_vendor_tokens(&redact_contextual(&value)))
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

    pub(super) fn reference_text(value: &str) -> String {
        text(value)
    }

    pub(super) fn reference_carries_secret(value: &str) -> bool {
        text(value) != value
    }
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
            elapsed < crate::test_support::coverage_relaxed(std::time::Duration::from_secs(30)),
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
    fn plain_assignments_use_the_same_credential_names_as_structured_data() {
        for name in SECRET_FIELD_NAMES
            .iter()
            .chain(SECRET_FIELD_SUFFIXES.iter())
        {
            // Headers have their own value syntax, covered by the existing
            // header tests rather than assignment matching.
            if matches!(*name, "authorization" | "cookie" | "set_cookie") {
                continue;
            }
            for input in [
                format!("{name}=synthetic-credential-value"),
                format!("request failed: {name}='synthetic-credential-value' next=keep"),
                format!("message=\"{name}=synthetic-credential-value\" next=keep"),
            ] {
                let output = text(&input);
                assert!(
                    !output.contains("synthetic-credential-value"),
                    "{name}: {output}"
                );
                assert_eq!(text(&output), output);
                if input.contains("next=keep") {
                    assert!(output.contains("next=keep"));
                }
            }
        }
        assert_eq!(
            text("token_count=5 tokenizer=word public_key=visible"),
            "token_count=5 tokenizer=word public_key=visible"
        );
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

#[cfg(test)]
mod url_userinfo_tests {
    use super::*;

    /// No vendor prefix, so only the URL-userinfo rule can catch it.
    const PAT: &str = "q7Zp4Lw9Xv2Rk8Tn5Jm3Hs6Bd1Fg0Ca";

    fn leaks(input: &str) -> Option<String> {
        let out = text(input);
        out.contains(PAT).then_some(out)
    }

    #[test]
    fn empty_username_url_password_is_redacted() {
        let input = format!("git push https://:{PAT}@dev.azure.com/org/p/_git/r main");
        assert_eq!(
            leaks(&input),
            None,
            "carries_secret={}",
            carries_secret(&input)
        );
    }

    #[test]
    fn token_only_userinfo_is_redacted() {
        let input = format!("git clone https://{PAT}@github.com/o/r.git");
        assert_eq!(
            leaks(&input),
            None,
            "carries_secret={}",
            carries_secret(&input)
        );
        assert!(carries_secret(&input));
    }

    #[test]
    fn password_containing_slash_is_redacted() {
        let input = format!("git fetch https://user:ab/{PAT}@host.example/r.git");
        assert_eq!(
            leaks(&input),
            None,
            "carries_secret={}",
            carries_secret(&input)
        );
    }

    #[test]
    fn password_containing_at_is_redacted() {
        let input = format!("git fetch https://user:p@{PAT}@host.example/r.git");
        assert_eq!(
            leaks(&input),
            None,
            "carries_secret={}",
            carries_secret(&input)
        );
    }

    #[test]
    fn plain_usernames_and_paths_are_kept() {
        for input in [
            "git clone ssh://git@github.com/o/r.git",
            "git clone https://github.com/o/r.git@v1",
            "npm i git+https://github.com/o/r#semver:^1",
            "mailto://someone@example.com",
            "npm i https://registry.example.com:8443/@scope/pkg",
        ] {
            assert_eq!(text(input), input, "{input}");
        }
    }

    #[test]
    fn redaction_is_idempotent_for_userinfo() {
        let input = format!("https://user:p@{PAT}@host.example/r.git https://{PAT}@h.example/x");
        let once = text(&input);
        assert_eq!(text(&once), once);
    }
}

/// The rewritten redactor is the reference redactor, byte for byte.
///
/// Three layers: a corpus (every input the module's own tests use, plus the
/// edge cases the rewrite could plausibly break), a pairwise sweep of every
/// pattern trigger against every other, and a generator over secret-shaped
/// fragments with a few hundred thousand cases.
#[cfg(test)]
mod equivalence_tests {
    use super::reference::{reference_carries_secret, reference_text};
    use super::*;
    use proptest::prelude::*;

    pub(super) const KEY: &str = "ghp_0123456789abcdefghijklmnopqrstuvwxyzA";
    const KEY2: &str = "ghp_ZYXWVUTSRQPONMLKJIHGFEDCBA9876543210B";
    const PAT: &str = "q7Zp4Lw9Xv2Rk8Tn5Jm3Hs6Bd1Fg0Ca";

    fn assert_same(input: &str) {
        let expected = reference_text(input);
        let actual = text(input);
        assert_eq!(actual, expected, "text diverged on {input:?}");
        assert_eq!(
            carries_secret(input),
            reference_carries_secret(input),
            "carries_secret diverged on {input:?}"
        );
    }

    fn nested(payload: &str, layers: usize) -> String {
        let mut value = serde_json::Value::String(payload.to_string());
        for _ in 0..layers {
            value = serde_json::json!({ "message": value.to_string() });
        }
        value.to_string()
    }

    fn deep_object(payload: &str, depth: usize) -> String {
        let mut value = serde_json::Value::String(payload.to_string());
        for _ in 0..depth {
            value = serde_json::json!({ "message": value });
        }
        serde_json::to_string(&value).expect("serialize")
    }

    /// Single-pattern triggers: each makes exactly one chain pattern match.
    const TRIGGERS: &[&str] = &[
        "-----BEGIN PRIVATE KEY-----\nopaque-body\n-----END PRIVATE KEY-----",
        "-----BEGIN RSA PRIVATE KEY-----\nopaque-body without an end marker",
        "-----BEGIN PGP PRIVATE KEY BLOCK-----\\nopaque\\n-----END PGP PRIVATE KEY BLOCK-----",
        r#""authorization": "Bearer opaque-auth""#,
        r#"authorization='Basic opaque-auth'"#,
        r#"{\"authorization\":\"Token opaque-auth\"}"#,
        "Authorization: Digest opaque-auth",
        "authorization: Bearer opaque-auth https://host.example/x",
        "Authorization: Bearer opaque-auth\\nnext",
        r#""cookie": "session=opaque-cookie""#,
        "Set-Cookie: sid=opaque-cookie; Path=/",
        r#"{\"cookie\":\"a=b\"}"#,
        "cookie='a=b'",
        "cookie=a=b\nnext=1",
        "https://alice:opaque-password@example.test/repo",
        "https://user:ab/opaque@host.example/r.git",
        "https://user:p@opaque@host.example/r.git",
        "https://host:8443/@scope/pkg",
        "https://:opaque@host.example",
        "https://q7Zp4Lw9Xv2Rk8Tn5Jm3Hs6Bd1Fg0Ca@github.com/o/r.git",
        "https://abcdefghijklmnopqrstuvwxyz@github.com/o/r.git",
        "ssh://git@github.com/o/r.git",
        "api_key=opaque-query",
        "API-KEY: opaque",
        "x_token='opaque'",
        r#"{"password":"opaque"}"#,
        r#"{\"client_secret\":\"opaque\"}"#,
        r#"msg=\"password=opaque\" next"#,
        "aws_secret_access_key=opaque",
        "token_count=5 tokenizer=word public_key=visible",
        "tool --password opaque-pw next",
        "tool --api-key 'opaque key' next",
        "tool --token \"opaque\" next",
        "curl --user alice:opaque https://example.test",
        "curl -u alice:opaque https://example.test",
        "--proxy-user 'a:b c' x",
        r#"["tool","--password","opaque","next"]"#,
        r#"["curl","-u","alice:opaque","x"]"#,
        r#"["tool","--token=opaque"]"#,
        r#"["tool","--user=alice:opaque"]"#,
        r#"["git","-c","http.extraHeader=Authorization: Bearer opaque","fetch"]"#,
        r#"phase [broken argv=["tool","--password","opaque","next"]"#,
        KEY,
        "AKIAIOSFODNN7EXAMPLE",
        "sk-ant-api03-0123456789abcdefghijklmnopqrstuvwxyz0123456789",
        "xoxb-0123456789-0123456789012-abcdefghijklmnopqrstuvwx",
        "eyJhbGciOiJIUzI1NiJ9.eyJzdWIiOiIxMjM0NTY3ODkwIn0.dBjftJeZ4CVPmB92K27uhbUJU1p1r_wW1gFWFOEjXk",
        "<redacted>",
        "password=<redacted>",
        "Authorization: Basic <redacted>\\nCookie: <redacted>\\nhttps://example.test/repo",
    ];

    pub(super) fn corpus() -> Vec<String> {
        let mut inputs: Vec<String> = vec![
            // Valid JSON scalars and near-scalars.
            "123".into(),
            "true".into(),
            "null".into(),
            "\"x\"".into(),
            "\"password=opaque\"".into(),
            "-0.5e3".into(),
            " \t\n[1,2] ".into(),
            "{}".into(),
            "[]".into(),
            "[".into(),
            "]".into(),
            "][".into(),
            "[[[[".into(),
            "{\"a\":".into(),
            // Whitespace, line endings, unicode, empty.
            "".into(),
            " ".into(),
            "\r\n".into(),
            "a\r\nb\r\npassword=opaque\r\nc".into(),
            "Authorization: Bearer opaque\r\nHost: x".into(),
            "cookie: a=b\r\nx".into(),
            "\u{202e}password=opaque\u{202c}".into(),
            "pa\u{0301}ssword=e\u{0301}".into(),
            "token=\u{1f512}\u{1f511} next".into(),
            "\u{1f511}ghp_0123456789abcdefghijklmnopqrstuvwxyzA\u{1f511}".into(),
            "line1\nline2 password=opaque\nline3 cookie=x\n".into(),
            "multi\nline\nauthorization: Bearer a\nend".into(),
            // Already redacted, and redacting twice.
            "<redacted>".into(),
            "api_key=<redacted>".into(),
            "\"api_key\":\"<redacted>\"".into(),
            "Authorization: Bearer <redacted>".into(),
            r#"{"access_token":"<redacted>"}"#.into(),
            // PEM with no END marker, at a line end, mid-line.
            "-----BEGIN PRIVATE KEY-----".into(),
            "x -----BEGIN OPENSSH PRIVATE KEY-----\nbody\nmore".into(),
            // Port exemption and userinfo.
            "host:8443/@scope/pkg".into(),
            "https://host:8443/@scope/pkg".into(),
            "https://host:8443@x".into(),
            "https://user:8443/@x/y z https://u:p@h".into(),
            format!("https://{PAT}@github.com/o/r.git"),
            format!("git push https://:{PAT}@dev.azure.com/org/p/_git/r main"),
            format!("git fetch https://user:ab/{PAT}@host.example/r.git"),
            format!("git fetch https://user:p@{PAT}@host.example/r.git"),
            format!("https://user:p@{PAT}@host.example/r.git https://{PAT}@h.example/x"),
            "git clone ssh://git@github.com/o/r.git".into(),
            "git clone https://github.com/o/r.git@v1".into(),
            "npm i git+https://github.com/o/r#semver:^1".into(),
            "mailto://someone@example.com".into(),
            "npm i https://registry.example.com:8443/@scope/pkg".into(),
            // Vendor tokens.
            format!("git push https://{KEY}@github.com/o/r"),
            format!("target={KEY} detail={KEY2}"),
            format!("{KEY} and again {KEY}"),
            format!("a,{KEY},b\"{KEY2}\"'c'"),
            format!("{KEY}\n{KEY2}\n"),
            format!("{KEY}\r\n{KEY2}\r"),
            format!("x\u{a0}{KEY}\u{2003}{KEY2}"),
            // Object keys that are tokens, with collision ordinals.
            format!("{{\"{KEY}\":\"x\"}}"),
            format!("{{\"cache\":{{\"{KEY}\":1}}}}"),
            format!("{{\"{KEY}\":1,\"{KEY2}\":2}}"),
            format!("{{\"{KEY}\":1,\"{KEY2}\":2,\"{KEY2} #2\":3,\"<redacted>\":4}}"),
            r#"{"client_secret":"SUPERSECRETVALUE1234567890"}"#.into(),
            r#"{"access_token":"ya29.OPAQUEVALUE1234567890abcdef"}"#.into(),
            r#"{"clientSecret":"x","x-api-key":"y","Authorization":"Bearer z"}"#.into(),
            r#"{"cfg":{"password":"x"},"github_token":"y"}"#.into(),
            r#"{"password":1234567890}"#.into(),
            r#"{"token":["a","b"]}"#.into(),
            r#"{"secret":{"inner":"value"}}"#.into(),
            r#"{"public_key":"ssh-ed25519 AAAA","cache_key":"abc","primary_key":"id"}"#.into(),
            // Serialized argv and embedded argv.
            r#"["git","commit","-m","fix the task-runner"]"#.into(),
            r#"argv=["tool","--password","opaque","next"]"#.into(),
            r#"[unclosed prefix ["curl","--user","alice:opaque","https://example.test"] suffix"#
                .into(),
            r#"{"message":"argv=[\"tool\",\"--password\",\"opaque\",\"next\"]"}"#.into(),
            r#"x [1,2] y ["--token","a"] z [[["-u","a:b"]]]"#.into(),
            "[\"unterminated string ".into(),
            r#"["a\\","--password","b"]"#.into(),
            "command=tool --access-token opaque-shell-token next".into(),
            "command=curl --user alice:opaque-shell-password https://example.test".into(),
            // The existing tests' multi-pattern inputs.
            "Authorization: Basic dXNlcjpwYXNz\nAuthorization: Digest opaque-digest-secret\nCookie: session=opaque-cookie\nurl=https://alice:opaque-password@example.test/repo\ndb=postgres://dbuser:opaque-database-password@example.test/app\n{\"Authorization\":\"Token opaque-json-auth\"}\n{\"Cookie\":\"session=opaque-json-cookie\"}\nAuthorization: Bearer opaque-inline-auth postgres://inline:opaque-inline-password@example.test/app\napi_key=opaque-query-secret\n-----BEGIN PRIVATE KEY-----\nopaque-key-body\n-----END PRIVATE KEY-----".into(),
            r#"["tool","Authorization: Bearer opaque-auth\\nnext","tail"]"#.into(),
            r#"["tool","Cookie: session=opaque-cookie\\nnext","tail"]"#.into(),
            r#"["tool","-----BEGIN PGP PRIVATE KEY BLOCK-----\\nopaque-key-body\\n-----END PGP PRIVATE KEY BLOCK-----","next"]"#.into(),
            "git -c 'http.extraHeader=Authorization: Basic opaque-basic' fetch".into(),
            "git commit -m 'fix the task-runner and disk-cache'".into(),
            "cargo test --workspace".into(),
            "no credentials here at all".into(),
        ];
        for name in SECRET_FIELD_NAMES
            .iter()
            .chain(SECRET_FIELD_SUFFIXES.iter())
        {
            inputs.push(format!("{name}=synthetic-credential-value"));
            inputs.push(format!(
                "request failed: {name}='synthetic-credential-value' next=keep"
            ));
            inputs.push(format!(
                "message=\"{name}=synthetic-credential-value\" next=keep"
            ));
            inputs.push(format!("{{\"{name}\":\"synthetic\",\"n\":1}}"));
        }
        // Serialized-nesting cap at 31, 32 and 33 levels as nested objects.
        // Nested *strings* double their escapes at every layer, so they stop at
        // a depth whose text is still small.
        for layers in [0, 1, 2, 30, 31, 32, 33, 34, 40] {
            for payload in [
                "Authorization: Bearer opaque-boundary-auth",
                "api_key=opaque",
                r#"["tool","--password","opaque"]"#,
                KEY,
                "plain",
            ] {
                inputs.push(deep_object(payload, layers));
                if layers <= 9 {
                    inputs.push(nested(payload, layers));
                }
            }
        }
        // Every flag the shell-flag patterns name, in each value shape.
        for flag in [
            "--password",
            "--passwd",
            "--api-key",
            "--apikey",
            "--access-token",
            "--refresh-token",
            "--client-secret",
            "--secret",
            "--token",
            "--auth-token",
            "--oauth-token",
            "--oauth2-bearer",
            "--aws-secret-access-key",
            "--aws-session-token",
            "--aws-access-key-id",
            "--user",
            "--userpass",
            "--proxy-user",
            "-u",
            "--PASSWORD",
            "--Proxy-User",
        ] {
            for value in [
                "opaque-value",
                "alice:opaque-value",
                "\"alice:opaque value\"",
                "'opaque value'",
                "<redacted>",
                "",
            ] {
                inputs.push(format!("tool {flag} {value} next"));
                inputs.push(format!("{flag}\t{value}"));
                inputs.push(serde_json::json!(["tool", flag, value, "next"]).to_string());
                inputs.push(serde_json::json!(["tool", format!("{flag}={value}")]).to_string());
            }
        }
        // Many equal-length token keys.
        let mut hostile = serde_json::Map::new();
        for index in 0..300u32 {
            hostile.insert(format!("ghp_{index:036}"), serde_json::Value::from(index));
        }
        inputs.push(serde_json::Value::Object(hostile).to_string());
        for trigger in TRIGGERS {
            inputs.push((*trigger).to_string());
        }
        inputs
    }

    #[test]
    fn corpus_matches_the_reference() {
        for input in corpus() {
            assert_same(&input);
        }
    }

    #[test]
    fn every_trigger_alone_and_in_pairs_matches_the_reference() {
        for first in TRIGGERS {
            assert_same(first);
            for second in TRIGGERS {
                // Adjacent, so one's replacement sits next to the other's
                // match; separated by a line; and one inside the other's
                // JSON string.
                for joiner in ["", " ", "\n", "\r\n", ",", "\"", "'", "\\n", "=", ":", "@"] {
                    assert_same(&format!("{first}{joiner}{second}"));
                }
                assert_same(&serde_json::json!({ "m": format!("{first} {second}") }).to_string());
                assert_same(&serde_json::json!([first, second]).to_string());
            }
        }
    }

    #[test]
    fn the_gate_table_and_the_chain_are_the_same_patterns() {
        let stage = &CONTEXTUAL;
        assert_eq!(stage.chain().len(), CONTEXTUAL_PATTERNS.len());
        assert_eq!(stage.gate().len(), CONTEXTUAL_PATTERNS.len());
        for (position, pattern) in stage.gate().patterns().iter().enumerate() {
            assert_eq!(pattern, stage.chain()[position].as_str(), "slot {position}");
        }
    }

    #[test]
    fn an_input_no_pattern_matches_comes_back_borrowed() {
        for input in [
            "",
            "src-tauri/src/ledger/redact.rs",
            "2026-09-01T12:00:00.000Z",
            "Edit",
        ] {
            assert!(!CONTEXTUAL.gate().is_match(input), "{input}");
            assert!(
                matches!(redact_contextual(Cow::Borrowed(input)), Cow::Borrowed(_)),
                "{input}"
            );
        }
    }

    const FRAGMENTS: &[&str] = &[
        "password",
        "api_key",
        "access_token",
        "client-secret",
        "Authorization",
        "authorization:",
        "cookie=",
        "Set-Cookie: ",
        "token",
        "secret",
        "x-api-key",
        "github_token",
        "://u:p@",
        "://",
        "https://",
        "host:8443/@scope/pkg",
        "@",
        ":",
        "=",
        "/",
        "Bearer ",
        "Basic ",
        "Digest ",
        "-----BEGIN PRIVATE KEY-----",
        "-----END PRIVATE KEY-----",
        "-----BEGIN PGP PRIVATE KEY BLOCK-----",
        "--token ",
        "--userpass ",
        "--proxy-user ",
        "--client-secret ",
        "--aws-session-token ",
        "--password ",
        "--user ",
        "-u ",
        "--api-key=",
        "ghp_0123456789abcdefghijklmnopqrstuvwxyzA",
        "ghp_ZYXWVUTSRQPONMLKJIHGFEDCBA9876543210B",
        "AKIAIOSFODNN7EXAMPLE",
        "sk-ant-api03-0123456789abcdefghijklmnopqrstuvwxyz0123456789",
        "eyJhbGciOiJIUzI1NiJ9.eyJzdWIiOiIxMjM0NTY3ODkwIn0.dBjftJeZ4CVPmB92K27uhbUJU1p1r_wW1gFWFOEjXk",
        "q7Zp4Lw9Xv2Rk8Tn5Jm3Hs6Bd1Fg0Ca",
        "<redacted>",
        "opaque",
        "value1234567890",
        "[",
        "]",
        "[\"",
        "\",\"",
        "\"]",
        "{",
        "}",
        "{\"",
        "\":\"",
        "\":",
        "\"",
        "'",
        "\\",
        "\\\"",
        "\\n",
        "\\\\",
        ",",
        ";",
        "&",
        " ",
        "  ",
        "\t",
        "\n",
        "\r\n",
        "\r",
        "\u{a0}",
        "\u{202e}",
        "\u{0301}",
        "\u{1f511}",
        "é",
        "#2",
        "1",
        "a",
    ];

    pub(super) fn fragments() -> &'static [&'static str] {
        FRAGMENTS
    }

    fn fragment_text() -> impl Strategy<Value = String> {
        proptest::collection::vec(0..FRAGMENTS.len(), 0..14)
            .prop_map(|picks| picks.into_iter().map(|index| FRAGMENTS[index]).collect())
    }

    /// A fragment string, bare or wrapped in the JSON shapes the ledger stores.
    pub(super) fn candidate() -> impl Strategy<Value = String> {
        (fragment_text(), fragment_text(), 0u8..9).prop_map(|(a, b, shape)| match shape {
            0..=3 => a,
            4 => serde_json::json!({ "m": a }).to_string(),
            5 => {
                let mut keyed = serde_json::Map::new();
                keyed.insert(a, serde_json::Value::String(b));
                serde_json::Value::Object(keyed).to_string()
            }
            6 => serde_json::json!([a, b]).to_string(),
            7 => serde_json::json!({ "message": serde_json::json!({ "m": a }).to_string() })
                .to_string(),
            _ => format!("{a} {} {b}", serde_json::json!([a, b])),
        })
    }

    /// Cases per generated property. Optimized builds run the full 220k
    /// the equivalence claim is stated for; an unoptimized build (CI's
    /// `cargo test`, coverage instrumented) runs a seeded-size sample, since
    /// the same loop is several hundred times slower there.
    pub(super) fn generated_cases() -> u32 {
        if cfg!(debug_assertions) {
            GENERATED_CASES_DEBUG
        } else {
            GENERATED_CASES_RELEASE
        }
    }

    const GENERATED_CASES_RELEASE: u32 = 220_000;
    const GENERATED_CASES_DEBUG: u32 = 10_000;

    proptest! {
        #![proptest_config(ProptestConfig {
            cases: generated_cases(),
            failure_persistence: None,
            ..ProptestConfig::default()
        })]

        #[test]
        fn generated_text_matches_the_reference(input in candidate()) {
            let expected = reference_text(&input);
            prop_assert_eq!(text(&input), expected, "text diverged on {:?}", input);
            prop_assert_eq!(
                carries_secret(&input),
                reference_carries_secret(&input),
                "carries_secret diverged on {:?}",
                input
            );
        }

        /// The identity the vendor-token skip rests on, on the exact inputs it
        /// is applied to: a segment holds no whitespace, quote or comma, and
        /// when dc-redact's detector finds nothing in it, its redactor
        /// returns it unchanged.
        #[test]
        fn a_segment_dc_redact_does_not_detect_is_returned_unchanged(input in candidate()) {
            for segment in input.split(|ch: char| ch.is_whitespace() || matches!(ch, '"' | '\'' | ',')) {
                if segment.is_empty() {
                    continue;
                }
                if !dc_redact::contains_secret(segment) {
                    prop_assert_eq!(dc_redact::redact_secrets(segment), segment);
                    prop_assert!(dc_redact::find_secret(segment).is_none());
                }
            }
        }
    }
}

/// Adversarial checks on the one claim the rewrite stands on: the `RegexSet`
/// gate says "no match" only when no chain pattern matches. Plus a count of
/// what the generator reaches, so the equivalence suite cannot pass vacuously.
#[cfg(test)]
mod gate_tests {
    use super::*;
    use proptest::prelude::*;
    use proptest::strategy::ValueTree;
    use proptest::test_runner::{Config, RngAlgorithm, TestRng, TestRunner};

    /// Pieces that stress the regex flags: case folding that is not ASCII
    /// (Kelvin sign, long s, sharp s, dotless/dotted i), line terminators
    /// `(?m)$` and `(?s)` care about, and the empty and very long cases.
    const ADVERSARIAL: &[&str] = &[
        "\u{212a}ey",
        "api_\u{212a}ey=opaque",
        "pa\u{17f}\u{17f}word=opaque",
        "\u{17f}ecret: opaque",
        "to\u{212a}en=opaque",
        "A\u{17f}\u{17f}ociation",
        "stra\u{df}e",
        "\u{131}",
        "\u{130}",
        "authorization:\u{212a}",
        "AUTHORIZATION: Bearer x",
        "co\u{212a}\u{212a}ie=x",
        "cookie: a=b\r\n",
        "cookie: a=b\r",
        "cookie: a=b\n",
        "cookie: a=b\u{2028}c",
        "authorization: Bearer x\u{85}y",
        "authorization: Bearer x\r\nHost: h\r\n",
        "-----BEGIN PRIVATE KEY-----\r\nbody\r\n",
        "-----BEGIN PRIVATE KEY-----\r\nbody\r\n-----END PRIVATE KEY-----\r\n",
        "-----BEGIN \u{212a}EY PRIVATE KEY-----",
        "-----begin private key-----",
        "\r\n",
        "\r",
        "\n",
        "\u{0}",
        "",
        "password",
        "password=",
        "password: ",
        "--password",
        "--password ",
        "-u ",
        "://",
        "a://:@",
        "x://@",
        "a:b@",
        ":@",
        "\\",
        "\\\"",
        "\"",
        "'",
    ];

    fn any_piece() -> impl Strategy<Value = String> {
        prop_oneof![
            3 => proptest::sample::select(ADVERSARIAL).prop_map(str::to_string),
            3 => proptest::sample::select(super::equivalence_tests::fragments()).prop_map(str::to_string),
            1 => any::<String>(),
            1 => "[ -~\u{80}-\u{2fff}\r\n]{0,12}".prop_map(|s: String| s),
        ]
    }

    fn adversarial_text() -> impl Strategy<Value = String> {
        proptest::collection::vec(any_piece(), 0..24).prop_map(|parts| parts.concat())
    }

    /// Every set index must agree with its individual regex, and the set's
    /// `is_match` with "some chain regex matches".
    fn assert_gate_agrees(input: &str) {
        let stage = &CONTEXTUAL;
        let set_hit = stage.gate().is_match(input);
        let chain_hit = stage.chain().iter().any(|regex| regex.is_match(input));
        assert_eq!(set_hit, chain_hit, "gate vs chain on {input:?}");
        let matched: Vec<usize> = stage.gate().matches(input).into_iter().collect();
        let individually: Vec<usize> = stage
            .chain()
            .iter()
            .enumerate()
            .filter(|(_, regex)| regex.is_match(input))
            .map(|(index, _)| index)
            .collect();
        assert_eq!(matched, individually, "set indices vs regexes on {input:?}");
        assert_eq!(set_hit, !matched.is_empty());
    }

    proptest! {
        #![proptest_config(ProptestConfig {
            cases: super::equivalence_tests::generated_cases(),
            failure_persistence: None,
            ..ProptestConfig::default()
        })]

        #[test]
        fn the_gate_matches_exactly_when_some_chain_pattern_does(input in adversarial_text()) {
            assert_gate_agrees(&input);
            prop_assert_eq!(text(&input), super::reference::text(&input));
        }
    }

    #[test]
    fn the_gate_agrees_with_the_chain_on_line_endings_and_long_inputs() {
        let mut inputs: Vec<String> = ADVERSARIAL.iter().map(|s| (*s).to_string()).collect();
        for piece in ADVERSARIAL {
            for suffix in ["", "\r\n", "\r", "\n", "\u{2028}", "\u{85}"] {
                for prefix in ["", "x", "\r\n", "\n\n"] {
                    inputs.push(format!("{prefix}{piece}{suffix}"));
                    inputs.push(format!("{prefix}{piece}{suffix}{piece}"));
                }
            }
        }
        // Past the lazy DFA's cache budget, where the engine may fall back to
        // another strategy: the answer must not change with the strategy.
        let filler = "x".repeat(300_000);
        inputs.push(format!("{filler}password=opaque"));
        inputs.push(format!("password=opaque{filler}"));
        inputs.push(format!("{filler}\r\n{filler}"));
        inputs.push(format!("{filler}{}", "authorization: ".repeat(2000)));
        inputs.push("-----BEGIN PRIVATE KEY-----".to_string() + &filler);
        inputs.push("\u{212a}".repeat(50_000) + "ey=opaque");
        inputs.push("api_\u{212a}ey=".to_string() + &"\u{17f}".repeat(100_000));
        inputs.push(filler);
        for input in &inputs {
            assert_gate_agrees(input);
            assert_eq!(text(input), super::reference::text(input));
        }
    }

    #[test]
    fn a_non_ascii_case_fold_reaches_the_chain_and_the_set_alike() {
        // Pins that the folds are exercised at all: if the gate said "no
        // match" here, a Kelvin-sign key would reach the ledger in full.
        let stage = &CONTEXTUAL;
        for input in [
            "api_\u{212a}ey=opaque-value",
            "pa\u{17f}\u{17f}word=opaque-value",
            "\u{17f}ecret=opaque-value",
        ] {
            assert!(stage.gate().is_match(input), "{input:?}");
            assert!(
                stage.chain().iter().any(|regex| regex.is_match(input)),
                "{input:?}"
            );
            assert!(!text(input).contains("opaque-value"), "{input:?}");
        }
    }

    /// What one generated case exercises.
    #[derive(Default, Debug)]
    struct Reach {
        total: usize,
        pattern: [usize; 15],
        port_exemption: usize,
        line_end_anchor_mid_text: usize,
        pem_without_end: usize,
        json_scalar_string: usize,
        changed: usize,
        deepest: usize,
    }

    fn json_depth(value: &serde_json::Value) -> usize {
        match value {
            serde_json::Value::Array(items) => 1 + items.iter().map(json_depth).max().unwrap_or(0),
            serde_json::Value::Object(map) => 1 + map.values().map(json_depth).max().unwrap_or(0),
            _ => 0,
        }
    }

    fn observe(reach: &mut Reach, input: &str) {
        let stage = &CONTEXTUAL;
        reach.total += 1;
        for (index, regex) in stage.chain().iter().enumerate() {
            if regex.is_match(input) {
                reach.pattern[index] += 1;
            }
        }
        if stage.chain()[URL_PASSWORD]
            .captures_iter(input)
            .any(|caps| {
                caps[2].split_once('/').is_some_and(|(head, _)| {
                    !head.is_empty() && head.bytes().all(|b| b.is_ascii_digit())
                })
            })
        {
            reach.port_exemption += 1;
        }
        if [AUTHORIZATION, COOKIE].iter().any(|&slot| {
            stage.chain()[slot]
                .find_iter(input)
                .any(|found| matches!(input.as_bytes().get(found.end()), Some(b'\n' | b'\r')))
        }) {
            reach.line_end_anchor_mid_text += 1;
        }
        if stage.chain()[PRIVATE_KEY]
            .find_iter(input)
            .any(|found| !found.as_str().contains("-----END"))
        {
            reach.pem_without_end += 1;
        }
        if let Ok(parsed) = serde_json::from_str::<serde_json::Value>(input) {
            reach.deepest = reach.deepest.max(json_depth(&parsed));
            if !parsed.is_array() && !parsed.is_object() {
                reach.json_scalar_string += 1;
            }
        }
        let out = text(input);
        if out != input {
            reach.changed += 1;
        }
    }

    #[test]
    fn the_generator_reaches_every_branch_the_rewrite_touched() {
        let mut runner = TestRunner::new_with_rng(
            Config::default(),
            TestRng::from_seed(RngAlgorithm::ChaCha, &[7; 32]),
        );
        let strategy = super::equivalence_tests::candidate();
        let mut reach = Reach::default();
        for _ in 0..40_000 {
            let input = strategy.new_tree(&mut runner).expect("tree").current();
            observe(&mut reach, &input);
        }
        eprintln!("generator reach: {reach:#?}");
        for (index, hits) in reach.pattern.iter().enumerate() {
            assert!(*hits >= 50, "pattern {index} reached by only {hits} inputs");
        }
        assert!(reach.port_exemption >= 20, "{reach:?}");
        assert!(reach.line_end_anchor_mid_text >= 50, "{reach:?}");
        assert!(reach.pem_without_end >= 50, "{reach:?}");
        assert!(reach.json_scalar_string >= 20, "{reach:?}");
        assert!(reach.changed >= 4_000, "{reach:?}");
    }

    /// Objects whose keys are equal-length tokens (so they redact to the same
    /// text and take collision ordinals), literal `<redacted>` / `<redacted> #N`
    /// keys the ordinals must step around, and fragment keys, over fragment
    /// values. The fragment generator almost never builds two such keys.
    fn colliding_key_document() -> impl Strategy<Value = String> {
        let key = prop_oneof![
            4 => "ghp_[A-Za-z0-9]{36}",
            2 => Just("<redacted>".to_string()),
            2 => (2usize..6).prop_map(|n| format!("<redacted> #{n}")),
            2 => proptest::sample::select(super::equivalence_tests::fragments()).prop_map(str::to_string),
        ];
        let value = prop_oneof![
            2 => proptest::sample::select(super::equivalence_tests::fragments()).prop_map(|s| serde_json::Value::String(s.to_string())),
            1 => (0u32..100).prop_map(serde_json::Value::from),
            1 => Just(serde_json::json!({"ghp_0123456789abcdefghijklmnopqrstuvwxyzA": "x"})),
        ];
        proptest::collection::vec((key, value), 1..9).prop_map(|entries| {
            let mut map = serde_json::Map::new();
            for (k, v) in entries {
                map.insert(k, v);
            }
            serde_json::Value::Object(map).to_string()
        })
    }

    proptest! {
        #![proptest_config(ProptestConfig {
            cases: super::equivalence_tests::generated_cases(),
            failure_persistence: None,
            ..ProptestConfig::default()
        })]

        #[test]
        fn colliding_token_keys_match_the_reference(input in colliding_key_document()) {
            prop_assert_eq!(text(&input), super::reference::text(&input), "{:?}", input);
            prop_assert_eq!(carries_secret(&input), super::reference::reference_carries_secret(&input));
        }
    }

    #[test]
    fn the_collision_generator_takes_ordinals() {
        let mut runner = TestRunner::new_with_rng(
            Config::default(),
            TestRng::from_seed(RngAlgorithm::ChaCha, &[11; 32]),
        );
        let strategy = colliding_key_document();
        let mut ordinals = 0usize;
        for _ in 0..5_000 {
            let input = strategy.new_tree(&mut runner).expect("tree").current();
            let out = text(&input);
            if out.contains(" #2") && !input.contains(" #2") {
                ordinals += 1;
            }
        }
        assert!(
            ordinals >= 500,
            "only {ordinals} documents took a #2 ordinal"
        );
    }

    /// The generator's strings are at most a dozen fragments, so they cannot
    /// nest to the serialized-nesting cap. These do, with the payload as an
    /// object chain, as a serialized document one level in, and as a leaf
    /// string, around levels 31, 32 and 33.
    #[test]
    fn nesting_around_the_cap_matches_the_reference() {
        let leaves = [
            "Authorization: Bearer opaque-boundary-auth".to_string(),
            "api_key=opaque".to_string(),
            r#"{"p":"api_key=opaque","password":"x"}"#.to_string(),
            r#"["tool","--password","opaque"]"#.to_string(),
            "123".to_string(),
            "true".to_string(),
            "null".to_string(),
            r#""x""#.to_string(),
            super::equivalence_tests::KEY.to_string(),
        ];
        let mut changed_somewhere = [false; 4];
        for depth in 28..=36usize {
            for leaf in &leaves {
                for (wrap, changed) in changed_somewhere.iter_mut().enumerate() {
                    let mut value = serde_json::Value::String(leaf.clone());
                    for level in 0..depth {
                        value = match wrap {
                            0 => serde_json::json!({ "message": value }),
                            1 => serde_json::json!([value]),
                            2 if level % 2 == 0 => serde_json::json!({ "k": value }),
                            2 => serde_json::json!([value, "tail"]),
                            _ => {
                                serde_json::json!({ "a": value, "ghp_0123456789abcdefghijklmnopqrstuvwxyzA": 1 })
                            }
                        };
                    }
                    let input = value.to_string();
                    let expected = super::reference::text(&input);
                    assert_eq!(
                        text(&input),
                        expected,
                        "depth {depth} wrap {wrap} leaf {leaf:?}"
                    );
                    *changed |= expected != input;
                }
            }
        }
        assert!(
            changed_somewhere.iter().all(|c| *c),
            "{changed_somewhere:?}"
        );
    }

    #[test]
    fn twenty_thousand_equal_length_token_keys_match_the_reference() {
        let mut hostile = serde_json::Map::new();
        for index in 0..20_000u32 {
            hostile.insert(format!("ghp_{index:036}"), serde_json::Value::from(index));
        }
        hostile.insert("<redacted> #2".to_string(), serde_json::Value::from(1));
        let document = serde_json::Value::Object(hostile).to_string();
        let out = text(&document);
        assert_eq!(out, super::reference::text(&document));
        assert!(out.contains("#2"));
        let parsed: serde_json::Value = serde_json::from_str(&out).expect("valid JSON");
        assert_eq!(parsed.as_object().map(serde_json::Map::len), Some(20_001));
    }

    #[test]
    fn json_scalar_strings_and_a_string_at_each_depth_match_the_reference() {
        for scalar in [
            "123",
            "true",
            "null",
            "\"x\"",
            "\"123\"",
            "\"true\"",
            "\"\\\"x\\\"\"",
            "1e999",
            "-0",
            "[]",
            "{}",
        ] {
            for depth in [0usize, 1, 30, 31, 32, 33] {
                let mut value = serde_json::Value::String(scalar.to_string());
                for _ in 0..depth {
                    value = serde_json::json!({ "message": value });
                }
                let input = value.to_string();
                assert_eq!(text(&input), super::reference::text(&input), "{input:?}");
            }
            assert_eq!(text(scalar), super::reference::text(scalar), "{scalar:?}");
        }
    }
}

/// The deferred gate: the first `UNGATED_CALLS` calls of a process run the
/// bare chain, later calls consult the set. Each test owns a fresh
/// `ContextualStage`, so none of them depends on how many calls other tests
/// in this binary already made through the shared static.
#[cfg(test)]
mod deferred_gate_tests {
    use super::equivalence_tests::{corpus, KEY};
    use super::reference::redact_contextual as reference_contextual;
    use super::*;

    fn inputs() -> Vec<String> {
        let mut inputs = corpus();
        inputs.push(String::new());
        inputs.push("src-tauri/src/ledger/redact.rs".to_string());
        inputs.push(format!("Authorization: Bearer {KEY}"));
        inputs
    }

    fn run(stage: &ContextualStage, input: &str) -> String {
        redact_contextual_with(stage, Cow::Borrowed(input)).into_owned()
    }

    #[test]
    fn both_regimes_in_one_process_match_the_reference() {
        let stage = ContextualStage::new();
        let inputs = inputs();
        let mut ungated = 0;
        let mut gated = 0;
        for round in 0..UNGATED_CALLS / inputs.len() + 3 {
            for input in &inputs {
                let regime_gated = stage.calls.load(Ordering::Relaxed) >= UNGATED_CALLS;
                assert_eq!(
                    run(&stage, input),
                    reference_contextual(input),
                    "round {round} (gated: {regime_gated}) diverged on {input:?}"
                );
                if regime_gated {
                    gated += 1;
                } else {
                    ungated += 1;
                }
            }
        }
        assert_eq!(ungated, UNGATED_CALLS, "the ungated regime ran N calls");
        assert!(
            gated >= 2 * inputs.len(),
            "the gated regime ran too: {gated}"
        );
    }

    #[test]
    fn the_gate_is_not_built_before_the_threshold_call() {
        let stage = ContextualStage::new();
        assert!(stage.gate.get().is_none());
        for call in 0..UNGATED_CALLS {
            assert!(stage.gate.get().is_none(), "built before call {call}");
            run(&stage, "plain text with no credential");
        }
        assert!(stage.chain.get().is_some(), "the chain is built by call 1");
        assert!(
            stage.gate.get().is_none(),
            "{UNGATED_CALLS} calls must not build the set"
        );
        run(&stage, "plain text with no credential");
        assert!(stage.gate.get().is_some(), "call N+1 builds the set");
    }

    #[test]
    fn the_threshold_is_a_small_bounded_count() {
        assert!((1..=1024).contains(&UNGATED_CALLS));
    }
}
