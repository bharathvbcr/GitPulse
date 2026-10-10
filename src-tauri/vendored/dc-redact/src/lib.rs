//! The credential table, and the one rule for finding and redacting what it
//! names.
//!
//! `dc-verify`'s secret gate blocks a commit on it, every other gate there
//! routes its evidence through it, and GitPulse's action ledger redacts with
//! it before anything reaches disk. Those must agree about where secrets are,
//! so there is one table and they all read it.
//!
//! It is a crate of its own, with no dependencies, so that a consumer which
//! needs only redaction can link it without the rest of `dc-verify`. That is
//! not tidiness: `dc-verify` compiles tree-sitter for its stub detection, and
//! cargo admits one package per `links = "tree-sitter"`, so GitPulse — which
//! also links MarkDev's highlighter on a different tree-sitter — could not
//! take the table at all while it lived there. A second copy of the table
//! would have been the alternative, and a second table is how a key redacted
//! by one consumer leaks out of another.

/// A credential shape worth stopping a commit for.
struct SecretPattern {
    name: &'static str,
    /// Literal prefix the value starts with.
    prefix: &'static str,
    /// Minimum length of the whole token, including the prefix.
    min_len: usize,
    /// Whether everything after the prefix must be letters and digits.
    ///
    /// It exists for the short, ambiguous prefixes. `sk-` occurs inside
    /// ordinary English and ordinary identifiers — `task-`, `disk-`, `risk-`
    /// all contain it — and a length floor alone would turn a long enough
    /// kebab-case name into a reported credential. The vendors whose keys are
    /// a flat alphanumeric body can say so, and then the shape does the work
    /// the prefix cannot.
    alphanumeric_body: bool,
}

impl SecretPattern {
    /// Validated UTF-8 byte spans. Rejected prefixes do not hide subsequent
    /// candidates; a matched token is consumed once, including nested prefixes.
    fn spans<'a>(&'a self, content: &'a str) -> impl Iterator<Item = std::ops::Range<usize>> + 'a {
        let pem = self.prefix == "-----BEGIN";
        let mut token_end = 0;
        let mut consumed = 0;
        content
            .match_indices(self.prefix)
            .filter_map(move |(start, _)| {
                // A PEM header opens with its own dashes, which are the
                // boundary; one glued to the word before it is still a header.
                if start < consumed || !(pem || starts_a_token(content, start)) {
                    return None;
                }
                let end = if pem {
                    pem_private_header_end(content, start + self.prefix.len())?
                } else {
                    // Reuse the token boundary when rejected prefixes occur in
                    // one long token, avoiding repeated scans of the same suffix.
                    // The run is measured after the prefix: the webhook's
                    // prefix carries `:`, which no key's body does.
                    let body = start + self.prefix.len();
                    if start >= token_end {
                        token_end = body
                            + content[body..]
                                .find(|c: char| !is_key_char(c))
                                .unwrap_or(content.len() - body);
                    }
                    // A period closing a sentence is not part of the key.
                    let end = start + content[start..token_end].trim_end_matches('.').len();
                    let token = &content[start..end];
                    if token.len() < self.min_len
                        || (self.alphanumeric_body
                            && !token[self.prefix.len()..]
                                .bytes()
                                .all(|b| b.is_ascii_alphanumeric()))
                    {
                        return None;
                    }
                    end
                };
                consumed = end;
                Some(start..end)
            })
    }
}

/// Reports whether `at` begins a token rather than landing inside a word.
///
/// Punctuation and quotes count as boundaries — a key is nearly always
/// preceded by `=`, `:`, `"` or a space — while a letter, digit or underscore
/// means the prefix is part of a longer identifier and not a credential.
fn starts_a_token(content: &str, at: usize) -> bool {
    match content[..at].chars().next_back() {
        None => true,
        Some(c) => !(c.is_ascii_alphanumeric() || c == '_'),
    }
}

/// Whether `c` can be part of a prefixed key's body.
///
/// Every key in the table is written in letters, digits and `-_./`; `+` and
/// `=` are here too because a key-shaped run inside a base64 blob continues
/// through them, and reading the run whole is what lets the letters-and-digits
/// families refuse it. Anything else — a bracket, a backtick, `;`, `:`, `|`,
/// `@` — ends the key, where a space, a quote and a comma once were the only
/// characters that did, and a key in parentheses passed the gate whole.
fn is_key_char(c: char) -> bool {
    c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.' | '/' | '+' | '=')
}

/// The end of a private-key PEM header whose `-----BEGIN` ends at `label_at`,
/// in RFC 7468's shape: one space, a label of printable ASCII other than `-`,
/// then `-----`. The label must name a private key.
///
/// Accepting any text up to the next `-----` let a header span other content.
/// A redacted header keeps its `-----BEGIN`, so the redactor's own output then
/// joined a later `… PRIVATE KEY-----` into a new match and was flagged again.
fn pem_private_header_end(content: &str, label_at: usize) -> Option<usize> {
    let label_start = label_at + content[label_at..].strip_prefix(' ').map(|_| 1)?;
    let label_len = content[label_start..]
        .find(|c: char| c == '-' || !(c == ' ' || c.is_ascii_graphic()))
        .unwrap_or(content.len() - label_start);
    let label = &content[label_start..label_start + label_len];
    let end = label_start + label_len;
    (content[end..].starts_with("-----") && label.to_ascii_lowercase().contains("private key"))
        .then_some(end + 5)
}

/// Vendor-prefixed key shapes.
///
/// Matching on a documented prefix plus a length floor, rather than on entropy,
/// is the deliberate choice here. Entropy scoring flags base64 blobs, minified
/// assets, test fixtures, and hashes — and a secret scanner that cries wolf is
/// one whose findings get waved through, which is strictly worse than not
/// having it. These prefixes are published by their vendors and do not occur by
/// accident.
///
/// What that design does not excuse is being incomplete inside a family it
/// already claims. `ghp_` was listed and `gho_`/`ghs_`/`ghu_`/`ghr_` were not;
/// `xoxb-` was listed and `xoxp-`/`xoxa-`/`xapp-` were not; `AKIA` was listed
/// and `ASIA` — the temporary credential that grants the same access — was not;
/// `sk-proj-` was listed and the legacy `sk-` OpenAI key was not; GitLab,
/// HuggingFace and npm had no entry at all. Every one of those passed the gate
/// clean. Order matters as much as membership: the scan stops at its first
/// match, so a longer prefix must be listed before any shorter one it starts
/// with, or the finding would name the wrong vendor.
///
/// Two evasions remain, and they are written down rather than left implied,
/// because a gate is only safe to rely on while what it cannot see is known:
///
///   - **A key split across added lines.** `const k = "sk-ant-" +` on one line
///     and `"api03-…"` on the next is not detected; the scan reads one line at
///     a time. Joining lines first would mean deciding where a logical line
///     ends in every language a repository contains, and getting that wrong
///     brings back the false positives this design exists to avoid.
///   - **A PEM body without its header.** The private-key pattern requires a
///     header whose label says `private key`, because matching the
///     dashes alone flagged certificates and public keys — ordinary
///     trust-store content — and taught operators to wave findings through.
///
/// Both are accepted. This gate catches a credential pasted into a file, which
/// is how credentials reach commits; neither evasion is a reason to widen the
/// match into a shape that fires on ordinary code.
///
/// Credentials with no vendor prefix — a password, an AWS secret key, a
/// bearer token, a JWT, a URL's inline password — are not here; they are
/// found by their context, in [`CONTEXT_PATTERNS`].
const SECRET_PATTERNS: &[SecretPattern] = &[
    SecretPattern {
        name: "anthropic api key",
        prefix: "sk-ant-",
        min_len: 24,
        alphanumeric_body: false,
    },
    SecretPattern {
        name: "openai project api key",
        prefix: "sk-proj-",
        min_len: 24,
        alphanumeric_body: false,
    },
    SecretPattern {
        name: "stripe live key",
        prefix: "sk_live_",
        min_len: 24,
        alphanumeric_body: false,
    },
    SecretPattern {
        name: "stripe test key",
        prefix: "sk_test_",
        min_len: 24,
        alphanumeric_body: false,
    },
    SecretPattern {
        name: "stripe restricted test key",
        prefix: "rk_test_",
        min_len: 24,
        alphanumeric_body: false,
    },
    SecretPattern {
        name: "stripe restricted live key",
        prefix: "rk_live_",
        min_len: 24,
        alphanumeric_body: false,
    },
    // Last of the `sk` family, because it is a prefix of the ones above and the
    // scan stops at its first match. The floor is high — a legacy OpenAI key is
    // `sk-` plus 48 characters — so `sk-test`, an `sk-` in prose, and this
    // file's own pattern literals stay below it.
    SecretPattern {
        name: "openai api key",
        prefix: "sk-",
        min_len: 45,
        alphanumeric_body: true,
    },
    SecretPattern {
        name: "xai api key",
        prefix: "xai-",
        min_len: 20,
        alphanumeric_body: false,
    },
    SecretPattern {
        name: "google api key",
        prefix: "AIza",
        min_len: 30,
        alphanumeric_body: false,
    },
    SecretPattern {
        name: "github personal access token",
        prefix: "ghp_",
        min_len: 30,
        alphanumeric_body: false,
    },
    SecretPattern {
        name: "github oauth token",
        prefix: "gho_",
        min_len: 30,
        alphanumeric_body: false,
    },
    SecretPattern {
        name: "github app server token",
        prefix: "ghs_",
        min_len: 30,
        alphanumeric_body: false,
    },
    SecretPattern {
        name: "github app user token",
        prefix: "ghu_",
        min_len: 30,
        alphanumeric_body: false,
    },
    SecretPattern {
        name: "github refresh token",
        prefix: "ghr_",
        min_len: 30,
        alphanumeric_body: false,
    },
    SecretPattern {
        name: "github pat",
        prefix: "github_pat_",
        min_len: 40,
        alphanumeric_body: false,
    },
    SecretPattern {
        name: "gitlab personal access token",
        prefix: "glpat-",
        min_len: 26,
        alphanumeric_body: false,
    },
    SecretPattern {
        name: "slack bot token",
        prefix: "xoxb-",
        min_len: 30,
        alphanumeric_body: false,
    },
    SecretPattern {
        name: "slack user token",
        prefix: "xoxp-",
        min_len: 30,
        alphanumeric_body: false,
    },
    SecretPattern {
        name: "slack workspace token",
        prefix: "xoxa-",
        min_len: 30,
        alphanumeric_body: false,
    },
    SecretPattern {
        name: "slack app-level token",
        prefix: "xapp-",
        min_len: 30,
        alphanumeric_body: false,
    },
    // The URL is the credential: whoever has it can post to the channel.
    SecretPattern {
        name: "slack incoming webhook",
        prefix: "https://hooks.slack.com/services/",
        min_len: 60,
        alphanumeric_body: false,
    },
    // `SG.` + 22 + `.` + 43. The floor is the whole shape, so `SG.` in prose
    // never reaches it.
    SecretPattern {
        name: "sendgrid api key",
        prefix: "SG.",
        min_len: 69,
        alphanumeric_body: false,
    },
    SecretPattern {
        name: "shopify access token",
        prefix: "shpat_",
        min_len: 38,
        alphanumeric_body: true,
    },
    SecretPattern {
        name: "shopify custom app token",
        prefix: "shpca_",
        min_len: 38,
        alphanumeric_body: true,
    },
    SecretPattern {
        name: "shopify private app token",
        prefix: "shppa_",
        min_len: 38,
        alphanumeric_body: true,
    },
    SecretPattern {
        name: "shopify shared secret",
        prefix: "shpss_",
        min_len: 38,
        alphanumeric_body: true,
    },
    SecretPattern {
        name: "aws access key id",
        prefix: "AKIA",
        min_len: 20,
        alphanumeric_body: true,
    },
    // Temporary credentials, and no less dangerous for it: an ASIA key plus its
    // session token is the same access as a long-lived one for as long as it
    // lasts, and it was the shape a leaked assume-role snippet carried.
    SecretPattern {
        name: "aws temporary access key id",
        prefix: "ASIA",
        min_len: 20,
        alphanumeric_body: true,
    },
    SecretPattern {
        name: "huggingface token",
        prefix: "hf_",
        min_len: 30,
        alphanumeric_body: false,
    },
    SecretPattern {
        name: "npm access token",
        prefix: "npm_",
        min_len: 36,
        alphanumeric_body: false,
    },
    SecretPattern {
        name: "private key block",
        prefix: "-----BEGIN",
        min_len: 20,
        alphanumeric_body: false,
    },
];

/// A credential with no vendor prefix, recognised by what surrounds it.
///
/// The prefix table alone measured recall 0.500 on the hand-labelled corpus in
/// `dc-verify/tests/corpus/secrets.tsv`: every password, AWS secret key, JWT,
/// bearer token and URL with a password in it passed clean. Each detector here
/// is as narrow as the prefix rule's reason demands — a scanner that cries
/// wolf gets waved through — so each requires the *literal* value and refuses
/// a reference to one: an environment lookup, a template, a variable, a
/// placeholder.
struct ContextPattern {
    name: &'static str,
    /// Bytes of a match the redaction keeps visible.
    keep: usize,
    spans: fn(&str) -> Vec<std::ops::Range<usize>>,
}

/// Searched after [`SECRET_PATTERNS`], so a vendor-shaped value is named by its
/// vendor even where it is also assigned to a credential-named key.
const CONTEXT_PATTERNS: &[ContextPattern] = &[
    ContextPattern {
        name: "json web token",
        keep: 3,
        spans: jwt_spans,
    },
    ContextPattern {
        name: "http authorization credential",
        keep: 0,
        spans: auth_scheme_spans,
    },
    ContextPattern {
        name: "password in a url",
        keep: 0,
        spans: url_password_spans,
    },
    ContextPattern {
        name: "password on a command line",
        keep: 0,
        spans: cli_password_spans,
    },
    ContextPattern {
        name: "credential assigned to a named key",
        keep: 0,
        spans: assignment_spans,
    },
];

/// Shortest literal password or secret the context detectors report. Below it
/// sit fixtures (`"test"`), flags and enum values.
const MIN_LITERAL_LEN: usize = 8;

/// Every credential on `line`, from both tables, as (name, span, bytes kept).
fn detections(
    line: &str,
) -> impl Iterator<Item = (&'static str, std::ops::Range<usize>, usize)> + '_ {
    let prefixed = SECRET_PATTERNS
        .iter()
        .flat_map(move |p| p.spans(line).map(move |s| (p.name, s, p.prefix.len())));
    let contextual = CONTEXT_PATTERNS.iter().flat_map(move |p| {
        (p.spans)(line)
            .into_iter()
            .map(move |s| (p.name, s, p.keep))
    });
    prefixed.chain(contextual)
}

/// A JWT: three base64url segments, the first two JSON objects (`eyJ` is
/// `{"` encoded). A signed token is a bearer credential until it expires.
///
/// A token runs from an `eyJ` to the end of its run of base64url characters
/// and dots, so every candidate in one run shares that end, and only one
/// segment of the run can hold a start with exactly two dots after it. The
/// run is read once, from its last two dots. Rescanning it from every `eyJ`
/// made a line of `eyJ.` quadratic: seconds per hundred kilobytes.
fn jwt_spans(line: &str) -> Vec<std::ops::Range<usize>> {
    let in_run = |b: u8| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.');
    let bytes = line.as_bytes();
    let mut spans = Vec::new();
    let mut at = 0;
    while at < bytes.len() {
        if !in_run(bytes[at]) {
            at += 1;
            continue;
        }
        let end = at
            + bytes[at..]
                .iter()
                .position(|&b| !in_run(b))
                .unwrap_or(bytes.len() - at);
        jwt_in_run(line, at, end, &mut spans);
        at = end;
    }
    spans
}

/// The JWTs in the run `line[start..end]`: each `eyJ` at a token start with
/// exactly two dots after it, three segments of ten or more, the second also
/// a JSON object.
fn jwt_in_run(line: &str, start: usize, end: usize, spans: &mut Vec<std::ops::Range<usize>>) {
    let run = &line[start..end];
    let Some(last) = run.rfind('.') else { return };
    let Some(middle) = run[..last].rfind('.') else {
        return;
    };
    let first = run[..middle].rfind('.').map_or(0, |d| d + 1);
    let (payload, signature) = (&run[middle + 1..last], &run[last + 1..]);
    if !payload.starts_with("eyJ") || payload.len() < 10 || signature.len() < 10 {
        return;
    }
    for (offset, _) in run[first..middle].match_indices("eyJ") {
        let at = start + first + offset;
        if start + middle - at >= 10 && starts_a_token(line, at) {
            spans.push(at..end);
        }
    }
}

/// An HTTP authorization credential: `Bearer <token>` or `Token <token>`
/// whose token is a literal — long, with both a letter and a digit, so
/// `Bearer ${TOKEN}` and prose do not match — or `Basic <base64>` whose value
/// decodes to `user:password`.
fn auth_scheme_spans(line: &str) -> Vec<std::ops::Range<usize>> {
    let lowered = line.to_ascii_lowercase();
    let mut spans = Vec::new();
    for scheme in ["bearer ", "token ", "basic "] {
        for (at, word) in lowered.match_indices(scheme) {
            if !starts_a_token(line, at) {
                continue;
            }
            let start = at + word.len();
            let start = start + (line.len() - start - line[start..].trim_start().len());
            let len = line[start..]
                .find(|c: char| !(c.is_ascii_alphanumeric() || "-._~+/=".contains(c)))
                .unwrap_or(line.len() - start);
            let token = &line[start..start + len];
            let literal = if scheme == "basic " {
                decode_base64(token).is_some_and(|d| d.len() >= 3 && d.contains(&b':'))
            } else {
                token.len() >= 16
                    && token.bytes().any(|b| b.is_ascii_digit())
                    && token.bytes().any(|b| b.is_ascii_alphabetic())
            };
            if literal {
                spans.push(start..start + len);
            }
        }
    }
    spans
}

/// Standard base64, padded or not; `None` for anything that is not.
fn decode_base64(text: &str) -> Option<Vec<u8>> {
    let body = text.trim_end_matches('=');
    if body.len() < 4 || text.len() - body.len() > 2 {
        return None;
    }
    let mut out = Vec::with_capacity(body.len() * 3 / 4);
    let (mut acc, mut bits) = (0u32, 0u32);
    for b in body.bytes() {
        let v = match b {
            b'A'..=b'Z' => b - b'A',
            b'a'..=b'z' => b - b'a' + 26,
            b'0'..=b'9' => b - b'0' + 52,
            b'+' => 62,
            b'/' => 63,
            _ => return None,
        };
        acc = (acc << 6) | u32::from(v);
        bits += 6;
        if bits >= 8 {
            bits -= 8;
            out.push((acc >> bits) as u8);
            acc &= (1 << bits) - 1;
        }
    }
    Some(out)
}

/// `scheme://user:password@host`: the password, when it is a literal.
fn url_password_spans(line: &str) -> Vec<std::ops::Range<usize>> {
    line.match_indices("://")
        .filter_map(|(at, sep)| {
            let authority_start = at + sep.len();
            let rest = &line[authority_start..];
            let authority = &rest[..rest
                .find(|c: char| c.is_whitespace() || "/?#\"'`".contains(c))
                .unwrap_or(rest.len())];
            let userinfo = &authority[..authority.rfind('@')?];
            let colon = userinfo.find(':')?;
            let (user, password) = (&userinfo[..colon], &userinfo[colon + 1..]);
            let start = authority_start + colon + 1;
            // No length floor: in a URL's userinfo the context says what the
            // value is, and a short password on a real host is still one. What
            // is excused is a generic word and a password that repeats the
            // user name — a documented default like RabbitMQ's `guest:guest`.
            const GENERIC: &[&str] = &["pass", "password", "passwd", "pwd", "secret", "test"];
            let generic = GENERIC.contains(&password.to_ascii_lowercase().as_str());
            (!password.is_empty() && !generic && password != user && !is_placeholder(password))
                .then(|| start..start + password.len())
        })
        .collect()
}

/// `<credential-named key> = <literal>`, in the spellings source, config and
/// shell files use: `k = "v"`, `k := "v"`, `K=v`, `k: v`, `"k": "v"`.
///
/// The key decides whether the value is asked about at all, so a name is read
/// as words (`awsSecretAccessKey`, `DB_PASSWORD`, `_authToken`) and must hold
/// one that names a credential and not end in one that names something *about*
/// a credential — `token_ttl`, `secret_name`, `PASSWORD_MIN_LENGTH`. The value
/// must then be a literal: no template, environment lookup, call, member
/// access or variable name.
fn assignment_spans(line: &str) -> Vec<std::ops::Range<usize>> {
    let bytes = line.as_bytes();
    let mut spans = Vec::new();
    // Where the last `}` and the last `${` or `{{` before `i` are, carried
    // forward rather than searched back for at every separator: a minified
    // line is all separators, and the backward search made it quadratic.
    let (mut last_close, mut last_open) = (None::<usize>, None::<usize>);
    // An opener is a template only when it closes: with no `}` after a
    // separator, nothing before it is open, and a stray `{{` in a string
    // literal must not hide every assignment after it.
    let final_close = line.rfind('}');
    let mut ends = ValueEnds::default();
    for (i, &b) in bytes.iter().enumerate() {
        if i >= 1 && bytes[i - 1] == b'}' {
            last_close = Some(i - 1);
        }
        if i >= 2 && matches!(&bytes[i - 2..i], b"${" | b"{{") {
            last_open = Some(i - 2);
        }
        let prev = i.checked_sub(1).map(|j| bytes[j]);
        let next = bytes.get(i + 1).copied();
        let value_from = match b {
            b'=' if matches!(next, Some(b'=' | b'>'))
                || matches!(prev, Some(b'=' | b'!' | b'<' | b'>' | b':')) =>
            {
                continue;
            }
            b'=' => i + 1,
            b':' if next == Some(b'=') => i + 2,
            b':' if matches!(next, Some(b':' | b'/')) || prev == Some(b':') => continue,
            b':' => i + 1,
            _ => continue,
        };
        // Inside an unclosed `${…}` or `{{…}}` a separator is template
        // syntax (`${API_KEY:?missing}`), not an assignment.
        if last_open.is_some_and(|open| last_close.is_none_or(|close| close < open))
            && final_close.is_some_and(|close| close > i)
        {
            continue;
        }
        let Some(key) = declared_name(line, i) else {
            continue;
        };
        if !names_a_credential(key) {
            continue;
        }
        if let Some(span) = literal_after(line, value_from, &mut ends) {
            spans.push(span);
        }
    }
    spans
}

/// The name a separator at `sep` assigns to. Usually [`key_before`]; for a
/// typed declaration — `const ADMIN_PASSWORD: &str =`, `val token: String =`,
/// `var secret string =` — the word before `=` is the type, and the name is
/// the one before that.
fn declared_name(line: &str, sep: usize) -> Option<&str> {
    let key = key_before(line, sep)?;
    if !line[sep..].starts_with('=') || !looks_like_a_type(key) {
        return Some(key);
    }
    let key_start = line[..sep].trim_end().len() - key.len();
    let head = line[..key_start].trim_end();
    let head = head.trim_end_matches(['&', '*', '[', ']']).trim_end();
    let head = head.strip_suffix(':').unwrap_or(head).trim_end();
    key_before(line, head.len()).or(Some(key))
}

fn looks_like_a_type(word: &str) -> bool {
    matches!(word, "str" | "string" | "bytes" | "byte" | "char")
        || word.starts_with(|c: char| c.is_ascii_uppercase())
            && word.chars().any(|c| c.is_ascii_lowercase())
}

/// The key a separator at `sep` assigns to: a quoted string, or the run of
/// identifier characters, directly before it.
fn key_before(line: &str, sep: usize) -> Option<&str> {
    let head = line[..sep].trim_end();
    if let Some(quote) = head.chars().next_back().filter(|c| matches!(c, '"' | '\'')) {
        let inner = &head[..head.len() - 1];
        let open = inner.rfind(quote)?;
        return Some(&inner[open + 1..]);
    }
    // The boundary character's own width, not 1: a non-ASCII character before
    // the key would otherwise put the slice inside it, and a panic here takes
    // down the secret gate and GitPulse's ledger writer with it.
    let start = head
        .char_indices()
        .rev()
        .find(|(_, c)| !(c.is_ascii_alphanumeric() || matches!(c, '_' | '-' | '.')))
        .map_or(0, |(at, c)| at + c.len_utf8());
    let key = &head[start..];
    (!key.is_empty()).then_some(key)
}

/// Splits an identifier into lowercase words at `_`, `-`, `.` and camelCase
/// boundaries, so `APIKey`, `api_key` and `apiKey` all read `api`, `key`.
fn words(identifier: &str) -> Vec<String> {
    let mut out = Vec::new();
    for part in identifier.split(|c: char| !c.is_ascii_alphanumeric()) {
        let chars: Vec<char> = part.chars().collect();
        let mut word = String::new();
        for (i, &c) in chars.iter().enumerate() {
            let boundary = i > 0
                && c.is_ascii_uppercase()
                && (chars[i - 1].is_ascii_lowercase()
                    || chars.get(i + 1).is_some_and(char::is_ascii_lowercase)
                        && chars[i - 1].is_ascii_uppercase());
            if boundary && !word.is_empty() {
                out.push(std::mem::take(&mut word));
            }
            word.push(c.to_ascii_lowercase());
        }
        if !word.is_empty() {
            out.push(word);
        }
    }
    out
}

fn names_a_credential(key: &str) -> bool {
    const SINGLE: &[&str] = &[
        "password",
        "passwd",
        "passphrase",
        "secret",
        "token",
        "apikey",
        "credential",
        "credentials",
        "auth",
    ];
    const PAIRS: &[(&str, &str)] = &[
        ("api", "key"),
        ("access", "key"),
        ("private", "key"),
        ("account", "key"),
        ("signing", "key"),
        ("encryption", "key"),
    ];
    // A last word that makes the key a fact *about* a credential.
    const ABOUT: &[&str] = &[
        "name",
        "names",
        "id",
        "ids",
        "endpoint",
        "url",
        "uri",
        "path",
        "file",
        "dir",
        "ttl",
        "len",
        "length",
        "size",
        "count",
        "type",
        "header",
        "prefix",
        "suffix",
        "field",
        "ref",
        "env",
        "var",
        "hint",
        "policy",
        "min",
        "max",
        "regex",
        "pattern",
        "label",
        "format",
        "kind",
        "mode",
        "version",
        "enabled",
        "required",
        "timeout",
        "expiry",
        "expires",
        "hash",
        "digest",
        "source",
        "provider",
        "method",
        "scheme",
        "strategy",
        "flow",
        "mechanism",
        "backend",
        "helper",
        "store",
        "manager",
        "handler",
        "class",
    ];
    // Distinctive enough to match at the end of a fused word: `PGPASSWORD`,
    // `dbpasswd`, `password2`. Only at the end: `passwordless` names a way of
    // not having one. `token` is not fused at all — `tokenizer` contains it.
    const FUSED: &[&str] = &["password", "passwd", "passphrase"];
    let w = words(key);
    if w.last().is_some_and(|last| ABOUT.contains(&last.as_str())) {
        return false;
    }
    w.iter().any(|x| {
        let x = x.trim_end_matches(|c: char| c.is_ascii_digit());
        SINGLE.contains(&x) || FUSED.iter().any(|f| x.ends_with(f))
    }) || w
        .windows(2)
        .any(|p| PAIRS.contains(&(p[0].as_str(), p[1].as_str())))
}

/// The first position at or after a query where a scan stops, remembered.
///
/// [`assignment_spans`] asks where a value ends at every separator of a line,
/// and its queries only move forward. Without the memory each one rescans to
/// the same stop, which on a line of separators is quadratic; with it, a stop
/// found once answers every later query that starts at or before it.
#[derive(Default)]
struct NextStop {
    /// (where the scan that found it started, the stop), the stop being the
    /// line's length when there is none.
    found: Option<(usize, usize)>,
}

impl NextStop {
    fn at_or_after(&mut self, line: &str, from: usize, stop: impl Fn(char) -> bool) -> usize {
        if let Some((started, at)) = self.found
            && started <= from
            && from <= at
        {
            return at;
        }
        let at = from + line[from..].find(stop).unwrap_or(line.len() - from);
        self.found = Some((from, at));
        at
    }
}

/// The scans [`literal_after`] makes, one memory per kind of stop.
#[derive(Default)]
struct ValueEnds {
    /// The end of an unquoted value: whitespace or a delimiter.
    unquoted: NextStop,
    /// How far an unquoted secret's redaction reaches past a comma.
    continued: NextStop,
    /// The closing quote, per quote character.
    quotes: [NextStop; 3],
}

/// The span of the literal value starting at or after `from`, when it is one
/// a credential could be.
///
/// An unquoted value is *judged* up to its first delimiter, so `token:someVar,`
/// in minified code reads `someVar`. A value that is a secret is *redacted* up
/// to whitespace, a closing bracket or a `, ` — a comma inside it is part of
/// it (`api_key: Ab12…Xy,zzzz` is one YAML value), and stopping at that comma
/// left the rest of the secret on display.
fn literal_after(line: &str, from: usize, ends: &mut ValueEnds) -> Option<std::ops::Range<usize>> {
    let rest = &line[from..];
    let start = from + (rest.len() - rest.trim_start().len());
    let rest = &line[start..];
    let first = rest.chars().next()?;
    if let Some(q) = ['"', '\'', '`'].iter().position(|c| *c == first) {
        let close = ends.quotes[q].at_or_after(line, start + 1, |c| c == first);
        if close == line.len() {
            return None;
        }
        let span = start + 1..close;
        return is_literal_secret(&line[span.clone()], true).then_some(span);
    }
    let end = ends
        .unquoted
        .at_or_after(line, start, |c| c.is_whitespace() || ",;)}]\"'".contains(c));
    if !is_literal_secret(&line[start..end], false) {
        return None;
    }
    let mut whole = end;
    while line[whole..].starts_with(',') {
        let next = whole + 1;
        if next == line.len() || line[next..].starts_with(char::is_whitespace) {
            break;
        }
        whole = ends
            .continued
            .at_or_after(line, next, |c| c.is_whitespace() || ",;)}]\"'".contains(c));
    }
    Some(start..whole)
}

/// Whether `value` reads as a literal password or secret rather than a
/// reference to one. `quoted` relaxes the expression test: inside quotes a
/// `.` or `(` is a character, not member access or a call.
fn is_literal_secret(value: &str, quoted: bool) -> bool {
    let has_digit = value.bytes().any(|b| b.is_ascii_digit());
    // A name, not a value: `session_token`, `settings.api_token`, `apiToken`,
    // and the kebab-case settings a credential-ish key often holds —
    // `same-origin`, `oidc-provider`, `refresh-token`.
    let reads_as_identifier = !has_digit
        && (value.contains(['_', '.', '-'])
            || value
                .as_bytes()
                .windows(2)
                .any(|p| p[0].is_ascii_lowercase() && p[1].is_ascii_uppercase()));
    // A name with a number on the end: `hashedPassword1`,
    // `hashed_password_2024`. The rest must be words, not a random run with a
    // digit at the end — `Summer2024` is one word, and stays a password.
    let stem = value
        .trim_end_matches(|c: char| c.is_ascii_digit())
        .trim_end_matches(['_', '-']);
    let numbered_name = stem.len() < value.len()
        && !stem.bytes().any(|b| b.is_ascii_digit())
        && reads_as_words(stem, 2);
    // A qualified name: `v1.Token`, `config.Secrets`. Its last part is a
    // word, which a dotted token's random tail is not.
    let qualified_name = !quoted
        && value.contains('.')
        && value.split('.').all(|part| {
            part.starts_with(|c: char| c.is_ascii_alphabetic())
                && part.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_')
        })
        && value
            .rsplit('.')
            .next()
            .is_some_and(|last| reads_as_words(last, 1));
    // An expression rather than a literal: a call, an index, a member access.
    let expression = !quoted
        && (value.contains(['(', '[']) || value.contains('.') && !has_digit || qualified_name);
    let plain_word = value.bytes().all(|b| b.is_ascii_lowercase()) && value.len() < 12;
    value.chars().count() >= MIN_LITERAL_LEN
        && !value.contains(char::is_whitespace)
        && !value.contains("://")
        && !value.bytes().all(|b| b.is_ascii_digit())
        && !reads_as_identifier
        && !numbered_name
        && !expression
        && !plain_word
        && !is_placeholder(value)
}

/// Whether `text` is at least `min` and at most four words of three or more
/// letters each — how an identifier reads, and how a random token does not.
fn reads_as_words(text: &str, min: usize) -> bool {
    let w = words(text);
    (min..=4).contains(&w.len())
        && w.iter()
            .all(|x| x.len() >= 3 && x.bytes().all(|b| b.is_ascii_alphabetic()))
}

/// A password given to a command-line client: the MySQL family's attached
/// `-p<password>` (a detached `-p` prompts, and is not one), and
/// `sshpass -p <password>`. Only after the client's own name, because `-p`
/// means something else to nearly every other command (`mkdir -p`).
fn cli_password_spans(line: &str) -> Vec<std::ops::Range<usize>> {
    const ATTACHED: &[&str] = &[
        "mysql",
        "mysqldump",
        "mysqladmin",
        "mysqlimport",
        "mariadb",
        "mariadb-dump",
    ];
    let words: Vec<(usize, &str)> = line
        .split_whitespace()
        .map(|w| (w.as_ptr() as usize - line.as_ptr() as usize, w))
        .collect();
    let mut spans = Vec::new();
    let mut client: Option<&str> = None;
    for (i, &(at, word)) in words.iter().enumerate() {
        let program = word.rsplit('/').next().unwrap_or(word);
        if ATTACHED.contains(&program) || program == "sshpass" {
            client = Some(program);
            continue;
        }
        if matches!(word, "|" | "&&" | "||" | ";") {
            client = None;
            continue;
        }
        let start = match client {
            Some("sshpass") if word == "-p" => match words.get(i + 1) {
                Some(&(next_at, _)) => next_at,
                None => continue,
            },
            Some(c)
                if c != "sshpass"
                    && word.len() > 2
                    && word.starts_with("-p")
                    && !word.starts_with("--") =>
            {
                at + 2
            }
            _ => continue,
        };
        // The whole shell word, which a quoted space does not end:
        // `-pS3cr3t"x y"` is one argument, and redacting only the part before
        // the space left the rest of the password visible.
        let end = shell_word_end(line, start);
        let raw = &line[start..end];
        let quoted = raw.contains(['"', '\'']);
        // Judged without its quotes and its quoted spaces: after `-p` the
        // context already says this is a password, so a space in it is a
        // character of the password, not a sign of prose.
        let value: String = raw
            .chars()
            .filter(|c| !matches!(c, '"' | '\'') && !c.is_whitespace())
            .collect();
        if is_literal_secret(&value, quoted) {
            spans.push(start..end);
        }
    }
    spans
}

/// Where the shell word starting at `start` ends: the first whitespace outside
/// quotes, or the end of the line when a quote is never closed.
fn shell_word_end(line: &str, start: usize) -> usize {
    let mut quote: Option<char> = None;
    for (i, c) in line[start..].char_indices() {
        match quote {
            Some(q) if c == q => quote = None,
            Some(_) => {}
            None if c == '"' || c == '\'' => quote = Some(c),
            None if c.is_whitespace() => return start + i,
            None => {}
        }
    }
    line.len()
}

/// A value standing in for a credential rather than being one: a template, a
/// shell expansion, a documented placeholder, a masked or fake value.
fn is_placeholder(value: &str) -> bool {
    const WORDS: &[&str] = &[
        "fake",
        "dummy",
        "placeholder",
        "changeme",
        "change_me",
        "your",
        "redacted",
        "sample",
    ];
    let lowered = value.to_ascii_lowercase();
    // An angle-bracket slot, `<your-token>`: a `<` with a `>` after it. A lone
    // bracket is a character a password may contain.
    let slot = value
        .find('<')
        .is_some_and(|open| value[open..].contains('>'));
    value.starts_with('$')
        || slot
        || ["${", "{{", "$(", "%("].iter().any(|t| value.contains(t))
        || WORDS.iter().any(|w| lowered.contains(w))
        || value
            .chars()
            .all(|c| c == value.chars().next().unwrap_or(c))
        || value.contains('…')
}

/// A credential found on a line: which shape it has, and the token rendered
/// the way every report shows it — the identifying prefix, then its length.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SecretMatch {
    /// The vendor shape, e.g. `"github personal access token"`.
    pub name: &'static str,
    /// The token with everything after its prefix withheld.
    pub redacted: String,
}

/// Returns the first credential shape, in table order, that `line` carries.
///
/// The single seam through which every consumer — the secret gate, and the
/// evidence builder every other gate routes its quotes through — decides what
/// counts as a credential. Two shapes share one rule set or they disagree
/// about where secrets are, which is how a key redacted by one gate leaks out
/// of another's evidence field. Table order is what makes the most specific
/// vendor win when one prefix starts another.
pub fn find_secret(line: &str) -> Option<SecretMatch> {
    detections(line)
        .next()
        .map(|(name, span, keep)| SecretMatch {
            name,
            redacted: redact(&line[span], keep),
        })
}

/// Redacts vendor-shaped credentials in an arbitrary string.
///
/// GitPulse's action ledger is the caller this exists for. It redacts at write
/// time rather than at display time: display-time redaction protects the screen
/// and nothing else, because the secret is already on disk.
///
/// Each match is replaced with the same prefix-preserving rendering the secret
/// gate reports. A token is replaced wherever it appears, not only at its
/// first occurrence.
pub fn redact_secrets(text: &str) -> String {
    // One line at a time, exactly as `contains_secret` reads. Every detector
    // is a one-line rule; run over a whole text, one line's `${` made the next
    // line's password look like template syntax, so the password was found by
    // `contains_secret` and left in place here.
    let mut out = String::with_capacity(text.len());
    for segment in text.split_inclusive('\n') {
        let line = segment.strip_suffix('\n').unwrap_or(segment);
        let line = line.strip_suffix('\r').unwrap_or(line);
        out.push_str(&redact_line(line));
        out.push_str(&segment[line.len()..]);
    }
    out
}

/// Passes [`redact_line`] makes before it stops trusting the detectors to
/// converge. Each pass replaces original text with a marker no detector reads,
/// so a line converges in at most as many passes as it has nested credentials;
/// the cap bounds the work on a line built to defeat that.
const MAX_REDACTION_PASSES: usize = 16;

/// [`redact_secrets`] for one line, repeated until a pass finds nothing.
///
/// One pass is not enough. A credential can hide a neighbour: two JWTs joined
/// by `-` are one dotted run, only the second has exactly two dots after it,
/// and once it is redacted the first is a JWT on its own — left in the clear by
/// a single pass. Redaction must leave nothing a scan would still stop, so it
/// runs until a scan stops nothing. A line that has not converged at the cap
/// is redacted whole: fail closed, never return a line still carrying one.
fn redact_line(line: &str) -> String {
    let mut text = redact_pass(line);
    if text == line {
        return text;
    }
    for _ in 1..MAX_REDACTION_PASSES {
        let next = redact_pass(&text);
        if next == text {
            return text;
        }
        text = next;
    }
    redact(line, 0)
}

/// One redaction pass over a line: every detection, overlaps merged.
fn redact_pass(text: &str) -> String {
    // Preserve byte offsets until every family has been checked. Sorting and
    // merging overlaps keeps the most specific prefix at a shared start and
    // prevents replacement text from being interpreted as another credential.
    // The prefix is chosen before the length: a vendor key and a context match
    // that start together merge into the longer span, and that span still
    // shows the vendor's prefix, which says what leaked and nothing more.
    let mut spans: Vec<_> = detections(text)
        .map(|(_, span, keep)| (span, keep))
        .collect();
    spans.sort_by_key(|(span, keep)| {
        (
            span.start,
            std::cmp::Reverse(*keep),
            std::cmp::Reverse(span.end),
        )
    });
    let mut merged: Vec<(std::ops::Range<usize>, usize)> = Vec::new();
    for (span, keep) in spans {
        if let Some((previous, _)) = merged.last_mut()
            && span.start < previous.end
        {
            previous.end = previous.end.max(span.end);
            continue;
        }
        merged.push((span, keep));
    }
    let mut out = String::with_capacity(text.len());
    let mut copied = 0;
    for (span, keep) in merged {
        out.push_str(&text[copied..span.start]);
        out.push_str(&redact(&text[span.clone()], keep));
        copied = span.end;
    }
    out.push_str(&text[copied..]);
    out
}

/// Reports whether `text` carries anything the secret gate would stop.
///
/// Callers that must *refuse* rather than redact use this: a value that cannot
/// be safely stored is not the same as one that was stored redacted.
pub fn contains_secret(text: &str) -> bool {
    text.lines().any(|line| detections(line).next().is_some())
}

/// redact keeps the identifying prefix and hides the rest.
fn redact(token: &str, keep: usize) -> String {
    let keep = keep.min(token.len());
    format!("{}… ({} chars)", &token[..keep], token.len())
}

#[cfg(test)]
mod redaction_tests {
    use super::*;

    /// A GitHub classic token shape, split so that no source line is one (see
    /// `no_line_of_this_file_is_itself_a_vendor_token`).
    const GITHUB_TOKEN: &str = concat!("ghp_", "0123456789abcdefghijklmnopqrstuvwxyzA");

    #[test]
    fn redacts_a_credential_in_an_argv_line() {
        let argv =
            format!(r#"["git","push","https://x-access-token:{GITHUB_TOKEN}@github.com/o/r"]"#);
        let out = redact_secrets(&argv);
        assert!(!out.contains(GITHUB_TOKEN), "{out}");
        assert!(
            out.contains("ghp_"),
            "the shape is still identifiable: {out}"
        );
        assert!(out.contains("chars)"), "the length is reported: {out}");
    }

    #[test]
    fn leaves_ordinary_text_byte_for_byte_alone() {
        for ordinary in [
            "git commit -m 'fix the task-runner and disk-cache'",
            "cargo test --workspace",
            "",
            "no credentials here at all",
        ] {
            assert_eq!(redact_secrets(ordinary), ordinary, "rewrote {ordinary:?}");
        }
    }

    #[test]
    fn redacts_every_occurrence_not_just_the_first() {
        let key = GITHUB_TOKEN;
        let text = format!("{key} and again {key}");
        let out = redact_secrets(&text);
        assert!(!out.contains(key), "a repeated key survived: {out}");
    }

    #[test]
    fn contains_secret_agrees_with_redaction() {
        let key = GITHUB_TOKEN;
        assert!(contains_secret(key));
        assert!(!contains_secret("cargo build --release"));
        assert_ne!(redact_secrets(key), key);
        assert_eq!(redact_secrets("cargo build"), "cargo build");
    }

    /// Each context detector on the literal it exists for, and on the
    /// reference to a credential that must not be mistaken for one.
    #[test]
    fn context_detectors_find_literals_and_leave_references_alone() {
        for (line, name) in [
            (
                r#"password = "Tr0ub4dor&3xample""#,
                "credential assigned to a named key",
            ),
            (
                r#"password = "pässwörd""#,
                "credential assigned to a named key",
            ),
            (
                "PGPASSWORD=xv9Lq2Rt7pW3 psql -h db",
                "credential assigned to a named key",
            ),
            (
                r#"const ADMIN_PASSWORD: &str = "Wint3r!sComing2026";"#,
                "credential assigned to a named key",
            ),
            (
                r#"val apiToken: String = "a7Hk29LmQp3ZxW8vRt5N""#,
                "credential assigned to a named key",
            ),
            (
                r#"  "apiKey": "AbC123dEf456GhI789jKl0","#,
                "credential assigned to a named key",
            ),
            (
                "discord_token: MTIzNDU2Nzg5MDEy.GhIjKl.MnOpQrStUvWxYz012345",
                "credential assigned to a named key",
            ),
            (
                "DATABASE_URL=postgres://app:s3cr3tPassw0rd@db:5432/app",
                "password in a url",
            ),
            (
                r#"{"Authorization": "Bearer 9f8e7d6c5b4a39281706f5e4d3c2b1a0"}"#,
                "http authorization credential",
            ),
            (
                r#"auth = "Basic dXNlcjpwYXNzd29yZA==""#,
                "http authorization credential",
            ),
            (
                "eyJhbGciOiJIUzI1NiJ9.eyJzdWIiOiIxMjM0NTY3ODkwIn0.dozjgNryP4J3jVmNHl0w",
                "json web token",
            ),
        ] {
            assert_eq!(find_secret(line).map(|m| m.name), Some(name), "{line}");
            assert!(contains_secret(line), "{line}");
        }
        for line in [
            r#"api_key = os.environ["OPENAI_API_KEY"]"#,
            r#"token := os.Getenv("GITHUB_TOKEN")"#,
            "api_key: ${API_KEY:?missing}",
            "auth_method: oauth2_client_credentials",
            "auth = BearerAuth(token=settings.api_token)",
            r#"token_type = "Bearer""#,
            "password_reset_token_ttl = 3600",
            r#"secret_name = "prod/db/credentials""#,
            r#"token = "fake-token-for-tests""#,
            r#"  "tokenizer": "cl100k_base","#,
            "TOKENIZERS_PARALLELISM=false",
            r#"curl -H "Authorization: Bearer ${TOKEN}""#,
            "DATABASE_URL=postgres://${DB_USER}:${DB_PASS}@db:5432/app",
            "if password == stored_password:",
            "Basic auth is configured in settings.py",
            r#"pub const ALLOW_STUB_MARKER: &str = "allow-stub";"#,
            r#"пароль = "значение""#,
        ] {
            assert_eq!(find_secret(line), None, "{line}");
        }
    }

    /// A password on a command line, in the spellings the clients accept: the
    /// attached `-p<pw>` of the MySQL family, `sshpass -p <pw>`, and
    /// `--password=<pw>` (which the named-key detector reads).
    #[test]
    fn command_line_passwords_are_found_and_prompts_are_not() {
        for line in [
            "mysql -u root -pS3cretRootPw appdb",
            "mysqldump --user=app -pDumpP4ss2026 shop > shop.sql",
            "/usr/bin/mariadb -h db -pM4riaPassw0rd",
            "sshpass -p Tr0ub4dor3x ssh deploy@host",
            "psql --password=xv9Lq2Rt7pW3 -h db",
        ] {
            assert!(contains_secret(line), "{line}");
            let once = redact_secrets(line);
            assert_eq!(redact_secrets(&once), once, "not idempotent: {once}");
        }
        for line in [
            "mysql -u root -p appdb",
            "mysql -u root -p\"$MYSQL_PWD\" appdb",
            "mysql -u root -p${DB_PASS}",
            "grep -pattern file.txt",
            "mkdir -p build/output/dir",
            "cp -preserve a b",
            "sshpass -p \"$PASS\" ssh host",
            "sshpass -f ~/.pw ssh host",
        ] {
            assert!(!contains_secret(line), "{line}");
        }
    }

    /// A URL password is a secret at any length, unless it is a generic word
    /// or repeats the user name — a documented default such as RabbitMQ's
    /// `guest:guest`.
    ///
    /// Vendors' published example credentials are *not* excused, and that is
    /// deliberate: this repository's own gate tests plant AWS's
    /// documented example key ID (the one ending `EXAMPLE`) as the canonical
    /// way to prove the scanner fires
    /// without committing a real key (dc/dcverify/client_interop_test.go,
    /// devcouncil/stopgate/rigor_end_to_end_test.go). Judged by shape, they
    /// are what a credential looks like.
    #[test]
    fn default_logins_are_not_credentials_and_published_examples_are() {
        for line in [
            r#"rabbit = "amqp://guest:guest@localhost:5672/""#,
            "postgres://user:pass@localhost:5432/app",
            "redis://default:password@cache:6379",
        ] {
            assert_eq!(find_secret(line), None, "{line}");
        }
        for line in [
            concat!(r#"aws_access_key_id = "AKIA"#, r#"IOSFODNN7EXAMPLE""#),
            r#"aws_secret_access_key = "wJalrXUtnFEMI/K7MDENG/bPxRfiCYEXAMPLEKEY""#,
            concat!(
                r#"WEBHOOK = "https://hooks.slack.com/services/"#,
                r#"T00000000/B00000000/XXXXXXXXXXXXXXXXXXXXXXXX""#
            ),
            "postgres://app:hunter2@prod-db.internal:5432/app",
            "amqp://app:z9@mq:5672/",
        ] {
            assert!(find_secret(line).is_some(), "{line}");
        }
    }

    #[test]
    fn context_matches_redact_their_value_and_redaction_is_idempotent() {
        for (line, value) in [
            (r#"password = "Tr0ub4dor&3xample""#, "Tr0ub4dor&3xample"),
            (
                "DB_PASSWORD=correcthorsebatterystaple",
                "correcthorsebatterystaple",
            ),
            ("redis://:9xYzQ8wVu7tS6rQ5@cache:6379/0", "9xYzQ8wVu7tS6rQ5"),
            (
                "Authorization: Token 9944b09199c62bcf9418ad846dd0e4bbdfc6ee4b",
                "9944b09199c62bcf9418ad846dd0e4bbdfc6ee4b",
            ),
        ] {
            let once = redact_secrets(line);
            assert!(!once.contains(value), "{line} -> {once}");
            assert_eq!(redact_secrets(&once), once, "not idempotent: {once}");
            assert!(
                !contains_secret(&once),
                "redaction output reads as a secret: {once}"
            );
        }
    }

    /// Every detector slices by byte offset. A multi-byte character at any
    /// position of a line that reaches them must not land a slice inside it:
    /// a panic here is a crashed gate and a crashed ledger writer.
    #[test]
    fn no_character_at_any_position_panics_a_detector() {
        let lines = [
            r#"password = "Tr0ub4dor&3xample""#,
            "PGPASSWORD=xv9Lq2Rt7pW3 psql",
            r#"const K: &str = "Wint3r!sComing2026";"#,
            "redis://:9xYzQ8wVu7tS6rQ5@cache:6379/0",
            "Authorization: Basic dXNlcjpwYXNzd29yZA==",
            "eyJhbGciOiJIUzI1NiJ9.eyJzdWIiOiIxMjM0NTY3ODkwIn0.dozjgNryP4J3jVmNHl0w",
            "api_key: ${API_KEY:?missing}",
            concat!("-----BEGIN RSA PRIVATE", " KEY-----"),
            "mysql -u root -p\"S3cretRootPw\" db | sshpass -p Tr0ub4dor3x ssh h",
        ];
        for line in lines {
            let boundaries: Vec<usize> = line
                .char_indices()
                .map(|(i, _)| i)
                .chain([line.len()])
                .collect();
            for at in boundaries {
                for ch in ['ь', '…', '😀', '\u{a0}'] {
                    let mut probe = String::with_capacity(line.len() + 4);
                    probe.push_str(&line[..at]);
                    probe.push(ch);
                    probe.push_str(&line[at..]);
                    let _ = find_secret(&probe);
                    let _ = contains_secret(&probe);
                    let _ = redact_secrets(&probe);
                }
            }
        }
    }

    /// Every detector reads one line, and so does redaction. When
    /// `redact_secrets` read a whole multi-line text as one line, a `${` on an
    /// earlier line made a later `password = "…"` look like template syntax:
    /// `contains_secret` saw the password and the redacted text still held it.
    #[test]
    fn redaction_judges_each_line_on_its_own() {
        for text in [
            "echo \"${\"\npassword = \"Tr0ub4dor3xample\"\n",
            "{{ header }}\r\napi_key: Xk29LmQp3ZxW8vRt5N\r\n",
            "run:\n  - echo ${\n  - PGPASSWORD=xv9Lq2Rt7pW3 psql",
        ] {
            assert!(contains_secret(text), "{text:?}");
            let out = redact_secrets(text);
            assert!(!contains_secret(&out), "{text:?} -> {out:?}");
            assert_eq!(
                out.matches('\n').count(),
                text.matches('\n').count(),
                "line structure changed: {out:?}"
            );
        }
        let clean = "a: ${\nb: }\r\nc = 1\n";
        assert_eq!(redact_secrets(clean), clean);
    }

    /// One added line can be a minified bundle. The named-key detector once
    /// scanned back over the whole line at every `:` and `=`, so a 400 KB line
    /// took tens of seconds and stalled the gate. Linear work finishes this in
    /// milliseconds; the bound is generous so a slow machine cannot flake it.
    #[test]
    fn a_long_line_is_scanned_in_linear_time() {
        let line = "a:1,".repeat(100_000);
        let started = std::time::Instant::now();
        assert!(!contains_secret(&line));
        assert_eq!(redact_secrets(&line), line);
        let elapsed = started.elapsed();
        assert!(
            elapsed < std::time::Duration::from_secs(2),
            "400 KB of separators took {elapsed:?}"
        );
    }

    /// A value is redacted whole. An unquoted value used to end at any comma,
    /// and `-p` took its value up to whitespace inside shell quotes, so the
    /// redaction left the tail of the secret visible.
    #[test]
    fn a_secret_value_is_redacted_whole() {
        for (line, hidden, kept) in [
            ("api_key: Ab1234567890Xy,zzzz", vec!["Ab12", "zzzz"], ""),
            (
                "{api_key: Ab1234567890Xy, next: 1}",
                vec!["Ab1234567890Xy"],
                ", next: 1}",
            ),
            (
                "mysql -u root -pS3cr3t\"x y\" appdb",
                vec!["S3cr3t", "x y"],
                " appdb",
            ),
        ] {
            assert!(contains_secret(line), "{line}");
            let out = redact_secrets(line);
            for h in hidden {
                assert!(!out.contains(h), "{line} -> {out}");
            }
            assert!(out.ends_with(kept), "{line} -> {out}");
        }
    }

    /// Settings and names that sit under a credential-ish key and are not
    /// credentials. Each of these blocked a commit: a kebab-case setting, an
    /// identifier with a number on the end, a qualified type name, and a key
    /// that only begins with `password`.
    ///
    /// The textbook `Bearer abc123def456ghi789` in a comment is *not* here: the
    /// corpus labels credentials by shape, published examples included, and
    /// its held-out set labels `AbC123dEf456GhI789jKl0` a secret.
    #[test]
    fn names_settings_and_examples_under_credential_keys_are_not_secrets() {
        for line in [
            "fetch(url, { credentials: 'same-origin' })",
            r#"  "auth": "github-actions","#,
            "auth: oidc-provider",
            r#"token: "refresh-token""#,
            "secret: my-app-secret",
            "passwordless: webauthn-v2",
            "token: v1.Token",
            "password: hashedPassword1",
            "user.password = hashed_password_2024",
        ] {
            assert_eq!(find_secret(line), None, "{line}");
        }
        // The same keys with literal values still are.
        for line in [
            "secret: Tr0ub4dor-x9q",
            "token: v1.Xk29LmQp3ZxW8vRt5N",
            "password: Summer2024!x",
            "auth: dXNlcjpwYXNzd29yZDEyMw==",
            "DB_PASSWORD=correcthorsebatterystaple",
            "// Bearer token format: Bearer abc123def456ghi789",
        ] {
            assert!(find_secret(line).is_some(), "{line}");
        }
    }

    #[test]
    fn redaction_output_carries_no_secret_of_its_own() {
        let key = GITHUB_TOKEN;
        let once = redact_secrets(key);
        assert_eq!(redact_secrets(&once), once);
        assert!(!contains_secret(&once));
    }

    /// This file is pushed to GitHub, whose push protection refuses a commit
    /// that carries a vendor-issued token shape, a vendor's documented
    /// placeholder included: it blocked Slack's `T00000000/B00000000/XXXX…`
    /// example in the test above, and every repository that vendors this
    /// crate inherits the block. So a vendor-prefixed fixture is written split
    /// with `concat!`, and no single source line matches [`SECRET_PATTERNS`]:
    /// the value under test is still a token, the text of the file is not.
    ///
    /// The context patterns (a password assigned to a named key, a URL's
    /// inline password) are not held to this; push protection matches vendor
    /// formats, and those fixtures read best whole.
    ///
    /// Only line numbers are reported, never the line, so a failure cannot
    /// itself print a token.
    #[test]
    fn no_line_of_this_file_is_itself_a_vendor_token() {
        let offenders: Vec<usize> = include_str!("lib.rs")
            .lines()
            .enumerate()
            .filter(|(_, line)| {
                SECRET_PATTERNS
                    .iter()
                    .any(|p| p.spans(line).next().is_some())
            })
            .map(|(i, _)| i + 1)
            .collect();
        assert!(
            offenders.is_empty(),
            "rust/dc-redact/src/lib.rs lines {offenders:?} carry a vendor token as written; \
             split each fixture with concat! so the source line does not"
        );
    }

    /// A key ends where a key's characters end, not only at a space, a quote
    /// or a comma. The families whose body must be letters and digits — AWS,
    /// the legacy OpenAI key, Shopify — read `(AKIA…)`, `` `AKIA…` ``,
    /// `AKIA…;` and a key closing a sentence as one token with the punctuation
    /// in it, failed the body check, and passed the gate whole.
    #[test]
    fn a_prefixed_key_ends_at_punctuation_not_only_at_whitespace() {
        let keys = [
            concat!("AKIA", "Q3EGRTWQKYZXN7M2PD"),
            concat!("ASIA", "Q3EGRTWQKYZXN7M2PD"),
            concat!("sk-", "A1b2C3d4E5f6G7h8I9j0K1l2M3n4O5p6Q7r8S9t0U1v2W3x4"),
            concat!("shpat_", "4f3a2b1c0d9e8f7a6b5c4d3e2f1a0b9c"),
            concat!("ghp_", "Fx7Qa9Lm2Rt8Vw3Yz6Bc1Dn4Ej5Hk0Pq7Su"),
        ];
        let wrappers = [
            "({k})",
            "`{k}`",
            "{k};",
            "[{k}]",
            "{{{k}}}",
            "<{k}>",
            "the key is {k}.",
            "id: {k}:",
            "{k}|tee",
            "f({k}, x)",
            "{k}@host",
        ];
        for key in keys {
            for wrapper in wrappers {
                let line = wrapper
                    .replace("{k}", key)
                    .replace("{{", "{")
                    .replace("}}", "}");
                let line = if wrapper == "{{{k}}}" {
                    format!("{{{key}}}")
                } else {
                    line
                };
                assert!(contains_secret(&line), "missed: {wrapper}");
                let out = redact_secrets(&line);
                assert!(!out.contains(&key[key.len() - 12..]), "{wrapper} -> {out}");
            }
        }
        // What the body rule exists to refuse still fails it: a prefix inside
        // a base64 blob, and a long kebab-case name that starts like a key.
        for line in [
            concat!(
                "data:image/png;base64,iVBOR/AKIA",
                "Q3EGRTWQKYZXN7M2PD+w0KGgo="
            ),
            concat!("blob=ab+AKIA", "Q3EGRTWQKYZXN7M2PD/cd"),
            concat!("sk-", "this-is-a-very-long-kebab-case-identifier-name-here"),
            concat!("AKIA", "Q3EGRTWQKYZXN7M2PD-suffix"),
        ] {
            assert!(!contains_secret(line), "flagged: {line}");
        }
    }

    /// A private-key header is RFC 7468's shape: `-----BEGIN `, a label of
    /// printable ASCII other than `-`, then `-----`. Any text up to the next
    /// dashes used to pass, so a redacted header — which keeps `-----BEGIN` —
    /// joined a later `… PRIVATE KEY-----` into a new match, and the redacted
    /// text was flagged again. A header glued to the word before it was not
    /// read at all, and that unread header was what the redaction left behind.
    #[test]
    fn a_private_key_header_is_the_rfc_7468_shape() {
        let header = concat!("-----BEGIN RSA PRIVATE", " KEY-----");
        for line in [
            format!("{header}abc{header}"),
            format!("key0{header}"),
            format!("x {header} y {header}"),
        ] {
            assert!(contains_secret(&line), "missed: {line}");
            let out = redact_secrets(&line);
            assert!(!contains_secret(&out), "{line} -> {out} is still flagged");
            assert_eq!(redact_secrets(&out), out, "not idempotent: {line}");
        }
        for line in [
            concat!("-----BEGIN… (31 chars) RSA PRIVATE", " KEY-----"),
            concat!("-----BEGIN ", "CERTIFICATE-----"),
            concat!("-----BEGIN PUBLIC KEY----- not a ", "private key -----"),
        ] {
            assert!(!contains_secret(line), "flagged: {line}");
        }
    }

    /// Every detector's worst line, at 400 KB, in linear time. The JWT
    /// detector rescanned to the end of a dotted run from every `eyJ` in it,
    /// so a line of `eyJ.` took seconds per hundred kilobytes and grew with
    /// its square — and this crate runs inside the secret gate and GitPulse's
    /// ledger writer, where a stalled scan is a stalled commit.
    #[test]
    fn every_detector_scans_its_worst_line_in_linear_time() {
        let shapes: [(&str, &str); 12] = [
            ("jwt dots", "eyJ."),
            ("jwt dashes", "-eyJ"),
            ("url userinfo", "a://"),
            ("auth scheme", "Bearer "),
            ("pem header", "-----BEGIN "),
            ("nested prefix", "ghp_"),
            ("cli password", "mysql -px "),
            ("template", "${password=Ab12345678 "),
            ("quoted keys", "\"a\":\""),
            ("assignment", "token=\""),
            ("separators", "a:1,"),
            ("commas", "api_key: Ab1234567890Xy,"),
        ];
        for (name, unit) in shapes {
            let line = unit.repeat(400_000 / unit.len());
            let started = std::time::Instant::now();
            let _ = contains_secret(&line);
            let _ = redact_secrets(&line);
            let elapsed = started.elapsed();
            assert!(
                elapsed < std::time::Duration::from_secs(2),
                "{name}: 400 KB took {elapsed:?}"
            );
        }
    }

    /// Redaction repeats until a pass finds nothing. Two JWTs joined by `-`
    /// are one dotted run in which only the second has exactly two dots after
    /// it; a single pass redacted that one and left the first, now a JWT on
    /// its own, in the clear.
    #[test]
    fn redaction_repeats_until_nothing_is_left() {
        let first = "eyJhbGciOiJIUzI1.eyJzdWIiOiIxMjM0.c2lnbmF0dXJlMQ";
        let second = "eyJ0eXAiOiJKV1Qi.eyJpc3MiOiJhY21l.c2lnbmF0dXJlMg";
        // No credential-named key: an assignment detector would redact the
        // whole value in one pass and hide what this is about.
        let line = format!("seen {first}-{second} next");
        let out = redact_secrets(&line);
        assert!(!out.contains("c2lnbmF0dXJlMQ"), "{out}");
        assert!(!out.contains("c2lnbmF0dXJlMg"), "{out}");
        assert!(!contains_secret(&out), "{out}");
        assert_eq!(redact_secrets(&out), out);
        assert!(out.ends_with(" next"), "{out}");
    }

    /// A template opener hides the separators inside its template, not the
    /// rest of the line. An unclosed `${` or `{{` — in a string literal, an
    /// `echo` — was read as open to the end of the line, and every password
    /// assigned after it passed the gate.
    #[test]
    fn an_unclosed_template_opener_hides_nothing_after_it() {
        for line in [
            r#"const open = "{{"; const password = "Xq9vB2mL7pZ4";"#,
            r#"echo "${" ; DB_PASSWORD=Xq9vB2mL7pZ4"#,
            r#"tpl = "${x"; api_key = "Xq9vB2mL7pZ4""#,
        ] {
            assert!(contains_secret(line), "missed: {line}");
            assert!(!redact_secrets(line).contains("Xq9vB2mL7pZ4"), "{line}");
        }
        // A template that closes still hides its own separators.
        for line in [
            "api_key: ${API_KEY:?missing}",
            "token={{ .Values.token | default \"x\" }}",
        ] {
            assert!(!contains_secret(line), "flagged: {line}");
        }
    }

    /// A placeholder is an angle-bracket slot, `<your-token>`; a lone `<` or
    /// `>` is a character a password may contain, and it hid the password.
    #[test]
    fn only_an_angle_bracket_slot_is_a_placeholder() {
        for line in [r#"password = "x>y2Zq81!mN""#, r#"password = "Zq81!mN<y2x""#] {
            assert!(contains_secret(line), "missed: {line}");
        }
        for line in [
            r#"password = "<your-password>""#,
            r#"api_key = "key-<ID>-9x7Qa2""#,
        ] {
            assert!(!contains_secret(line), "flagged: {line}");
        }
    }

    /// The linear JWT scan finds exactly what the quadratic one it replaced
    /// found. The reference is the old rule verbatim — every `eyJ` at a token
    /// start, read to the end of its run, three segments of ten or more, the
    /// second a JSON object — and short random lines over the characters that
    /// matter reach every branch of both.
    #[test]
    fn jwt_spans_agree_with_the_quadratic_reference() {
        fn reference(line: &str) -> Vec<std::ops::Range<usize>> {
            let is_b64url = |c: char| c.is_ascii_alphanumeric() || matches!(c, '-' | '_');
            line.match_indices("eyJ")
                .filter(|(at, _)| starts_a_token(line, *at))
                .filter_map(|(at, _)| {
                    let len = line[at..]
                        .find(|c: char| !(is_b64url(c) || c == '.'))
                        .unwrap_or(line.len() - at);
                    let parts: Vec<&str> = line[at..at + len].split('.').collect();
                    (parts.len() == 3
                        && parts[1].starts_with("eyJ")
                        && parts.iter().all(|p| p.len() >= 10))
                    .then(|| at..at + len)
                })
                .collect()
        }
        // Whole segments, so valid tokens are common, and the single
        // characters that split, join or bound them.
        const PIECES: &[&str] = &[
            "eyJhbGciOi",
            "eyJzdWIiOi",
            ".eyJpc3MiOi",
            ".c2lnbmF0dX",
            "eyJ",
            ".",
            ".",
            "-",
            "_",
            "a",
            "7",
            " ",
            "é",
            "\"",
            "Q2xvc2U",
        ];
        let mut state = 0x2545_F491_4F6C_DD1Du64;
        let mut hits = 0usize;
        for _ in 0..200_000 {
            let mut line = String::new();
            state ^= state << 13;
            state ^= state >> 7;
            state ^= state << 17;
            for _ in 0..(state % 12) {
                state ^= state << 13;
                state ^= state >> 7;
                state ^= state << 17;
                line.push_str(PIECES[(state >> 32) as usize % PIECES.len()]);
            }
            let mut got = jwt_spans(&line);
            got.sort_by_key(|r| r.start);
            assert_eq!(got, reference(&line), "{line:?}");
            hits += usize::from(!got.is_empty());
        }
        // Agreement on lines with no JWT in them would prove nothing.
        assert!(hits >= 1_000, "only {hits} generated lines held a JWT");
    }

    /// The table's own invariant: a key is longer than its prefix, so the
    /// body check never slices a token shorter than the prefix it starts with.
    #[test]
    fn every_floor_is_longer_than_its_prefix() {
        for p in SECRET_PATTERNS {
            assert!(p.min_len > p.prefix.len(), "{}", p.name);
        }
    }
}
