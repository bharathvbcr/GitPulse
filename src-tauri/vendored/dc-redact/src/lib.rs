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
        let mut token_end = 0;
        let mut consumed = 0;
        content
            .match_indices(self.prefix)
            .filter_map(move |(start, _)| {
                if start < consumed || !starts_a_token(content, start) {
                    return None;
                }
                let end = if self.prefix == "-----BEGIN" {
                    // Validate this header, not unrelated private-key words
                    // elsewhere on the line. Redact the complete private header.
                    let suffix = start + self.prefix.len();
                    let end = suffix + content[suffix..].find("-----")? + 5;
                    let header = &content[start..end];
                    if header.contains(['\n', '\r'])
                        || !header.to_ascii_lowercase().contains("private key")
                    {
                        return None;
                    }
                    end
                } else {
                    // Reuse the token boundary when rejected prefixes occur in
                    // one long token, avoiding repeated scans of the same suffix.
                    if start >= token_end {
                        token_end = start
                            + content[start..]
                                .find(|c: char| c.is_whitespace() || matches!(c, '"' | '\'' | ','))
                                .unwrap_or(content.len() - start);
                    }
                    let token = &content[start..token_end];
                    if token.len() < self.min_len
                        || (self.alphanumeric_body
                            && !token[self.prefix.len()..]
                                .bytes()
                                .all(|b| b.is_ascii_alphanumeric()))
                    {
                        return None;
                    }
                    token_end
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
/// A leading `-` is a boundary too, so a PEM header written with extra dashes
/// still matches.
fn starts_a_token(content: &str, at: usize) -> bool {
    match content[..at].chars().next_back() {
        None => true,
        Some(c) => !(c.is_ascii_alphanumeric() || c == '_'),
    }
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
///   - **A PEM body without its header.** The private-key pattern requires the
///     literal `private key` text on the same line, because matching the
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
fn jwt_spans(line: &str) -> Vec<std::ops::Range<usize>> {
    let is_b64url = |c: char| c.is_ascii_alphanumeric() || matches!(c, '-' | '_');
    line.match_indices("eyJ")
        .filter(|(at, _)| starts_a_token(line, *at))
        .filter_map(|(at, _)| {
            let len = line[at..]
                .find(|c: char| !(is_b64url(c) || c == '.'))
                .unwrap_or(line.len() - at);
            let token = &line[at..at + len];
            let parts: Vec<&str> = token.split('.').collect();
            (parts.len() == 3 && parts[1].starts_with("eyJ") && parts.iter().all(|p| p.len() >= 10))
                .then(|| at..at + len)
        })
        .collect()
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
    for (i, &b) in bytes.iter().enumerate() {
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
        if inside_template(&line[..i]) {
            continue;
        }
        let Some(key) = declared_name(line, i) else {
            continue;
        };
        if !names_a_credential(key) {
            continue;
        }
        if let Some(span) = literal_after(line, value_from) {
            spans.push(span);
        }
    }
    spans
}

/// Whether `head` ends inside an unclosed `${…}` or `{{…}}`: a separator
/// there is template syntax (`${API_KEY:?missing}`), not an assignment.
fn inside_template(head: &str) -> bool {
    let close = head.rfind('}');
    ["${", "{{"].iter().any(|open| {
        head.rfind(open)
            .is_some_and(|at| close.is_none_or(|c| c < at))
    })
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
    // Distinctive enough to match inside a fused word: `PGPASSWORD`,
    // `dbpasswd`. `token` is not — `tokenizer` contains it.
    const FUSED: &[&str] = &["password", "passwd", "passphrase"];
    let w = words(key);
    if w.last().is_some_and(|last| ABOUT.contains(&last.as_str())) {
        return false;
    }
    w.iter()
        .any(|x| SINGLE.contains(&x.as_str()) || FUSED.iter().any(|f| x.contains(f)))
        || w.windows(2)
            .any(|p| PAIRS.contains(&(p[0].as_str(), p[1].as_str())))
}

/// The span of the literal value starting at or after `from`, when it is one
/// a credential could be.
fn literal_after(line: &str, from: usize) -> Option<std::ops::Range<usize>> {
    let rest = &line[from..];
    let start = from + (rest.len() - rest.trim_start().len());
    let rest = &line[start..];
    let first = rest.chars().next()?;
    let (span, quoted) = if matches!(first, '"' | '\'' | '`') {
        let close = rest[1..].find(first)?;
        (start + 1..start + 1 + close, true)
    } else {
        let len = rest
            .find(|c: char| c.is_whitespace() || ",;)}]\"'".contains(c))
            .unwrap_or(rest.len());
        (start..start + len, false)
    };
    is_literal_secret(&line[span.clone()], quoted).then_some(span)
}

/// Whether `value` reads as a literal password or secret rather than a
/// reference to one. `quoted` relaxes the expression test: inside quotes a
/// `.` or `(` is a character, not member access or a call.
fn is_literal_secret(value: &str, quoted: bool) -> bool {
    let has_digit = value.bytes().any(|b| b.is_ascii_digit());
    // A name, not a value: `session_token`, `settings.api_token`, `apiToken`.
    let reads_as_identifier = !has_digit
        && (value.contains(['_', '.'])
            || value
                .as_bytes()
                .windows(2)
                .any(|p| p[0].is_ascii_lowercase() && p[1].is_ascii_uppercase()));
    // An expression rather than a literal: a call, an index, a member access.
    let expression = !quoted && (value.contains(['(', '[']) || value.contains('.') && !has_digit);
    let plain_word = value.bytes().all(|b| b.is_ascii_lowercase()) && value.len() < 12;
    value.chars().count() >= MIN_LITERAL_LEN
        && !value.contains(char::is_whitespace)
        && !value.contains("://")
        && !value.bytes().all(|b| b.is_ascii_digit())
        && !reads_as_identifier
        && !expression
        && !plain_word
        && !is_placeholder(value)
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
        let (start, value) = match client {
            Some("sshpass") if word == "-p" => match words.get(i + 1) {
                Some(&(next_at, next)) => (next_at, next),
                None => continue,
            },
            Some(c)
                if c != "sshpass"
                    && word.len() > 2
                    && word.starts_with("-p")
                    && !word.starts_with("--") =>
            {
                (at + 2, &word[2..])
            }
            _ => continue,
        };
        let quoted = value.len() >= 2
            && matches!(value.as_bytes()[0], b'"' | b'\'')
            && value.ends_with(value.as_bytes()[0] as char);
        let (start, value) = if quoted {
            (start + 1, &value[1..value.len() - 1])
        } else {
            (start, value)
        };
        if is_literal_secret(value, quoted) {
            spans.push(start..start + value.len());
        }
    }
    spans
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
    value.starts_with('$')
        || ["${", "{{", "$(", "%(", "<", ">"]
            .iter()
            .any(|t| value.contains(t))
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
    // Preserve byte offsets until every family has been checked. Sorting and
    // merging overlaps keeps the most specific prefix at a shared start and
    // prevents replacement text from being interpreted as another credential.
    let mut spans: Vec<_> = detections(text)
        .map(|(_, span, keep)| (span, keep))
        .collect();
    spans.sort_by_key(|(span, keep)| {
        (
            span.start,
            std::cmp::Reverse(span.end),
            std::cmp::Reverse(*keep),
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

    #[test]
    fn redacts_a_credential_in_an_argv_line() {
        let argv = r#"["git","push","https://x-access-token:ghp_0123456789abcdefghijklmnopqrstuvwxyzA@github.com/o/r"]"#;
        let out = redact_secrets(argv);
        assert!(
            !out.contains("ghp_0123456789abcdefghijklmnopqrstuvwxyzA"),
            "{out}"
        );
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
        let key = "ghp_0123456789abcdefghijklmnopqrstuvwxyzA";
        let text = format!("{key} and again {key}");
        let out = redact_secrets(&text);
        assert!(!out.contains(key), "a repeated key survived: {out}");
    }

    #[test]
    fn contains_secret_agrees_with_redaction() {
        let key = "ghp_0123456789abcdefghijklmnopqrstuvwxyzA";
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
    /// `AKIAIOSFODNN7EXAMPLE` as the canonical way to prove the scanner fires
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
            r#"aws_access_key_id = "AKIAIOSFODNN7EXAMPLE""#,
            r#"aws_secret_access_key = "wJalrXUtnFEMI/K7MDENG/bPxRfiCYEXAMPLEKEY""#,
            r#"WEBHOOK = "https://hooks.slack.com/services/T00000000/B00000000/XXXXXXXXXXXXXXXXXXXXXXXX""#,
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
            "-----BEGIN RSA PRIVATE KEY-----",
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

    #[test]
    fn redaction_output_carries_no_secret_of_its_own() {
        let key = "ghp_0123456789abcdefghijklmnopqrstuvwxyzA";
        let once = redact_secrets(key);
        assert_eq!(redact_secrets(&once), once);
        assert!(!contains_secret(&once));
    }
}
