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
//!
//! # What a value is judged on
//!
//! A credential-named key (`password=`, `"token": `, `--password`) is followed
//! by a value that may be a secret or may be a reference to one, and the
//! detectors decide which by reading the value: its length, whether it reads as
//! an identifier or as words, whether it is a placeholder. That judgement reads
//! at most the first `MAX_JUDGED_VALUE` (512) bytes of the value, cut at a
//! character boundary. The detectors ask at every separator of a line, and the
//! value after a separator can run to the end of it, so an unbounded read made
//! a long line of repeated `password=` quadratic; the bound makes a line cost
//! time proportional to its length (`tests/scaling.rs` measures 100 KB against
//! 400 KB). A real password, key or token is far shorter than 512 bytes. The
//! bound is on the judgement only: a value judged a secret is redacted whole,
//! however long, and a value whose first 512 bytes read as words or an
//! identifier is not judged by anything past them.
//!
//! # Same-line joins
//!
//! The scan reads one line at a time. On one line, a run of two or more quoted
//! literals separated only by whitespace or one `+` is also scanned once as one
//! joined literal, so a key written as two quoted halves is the key it reads as. The
//! line as written is always scanned first, and a match in the joined copy
//! counts only when it starts in one literal and ends in a later one. A split
//! across lines and a headerless PEM body stay misses.

/// A credential shape worth stopping a commit for.
struct SecretPattern {
    name: &'static str,
    /// Literal prefix the value starts with.
    prefix: &'static str,
    /// Minimum length of the whole token, including the prefix.
    min_len: usize,
    /// Which characters everything after the prefix must be written in.
    body: Body,
}

/// The characters a key's body is written in, after its prefix.
///
/// It exists for the short, ambiguous prefixes. `sk-` occurs inside ordinary
/// English and ordinary identifiers — `task-`, `disk-`, `risk-` all contain it
/// — and a length floor alone would turn a long enough kebab-case name into a
/// reported credential. The vendors whose keys are a narrower alphabet than a
/// token run can say so, and then the shape does the work the prefix cannot.
#[derive(Clone, Copy)]
enum Body {
    /// Any key character ([`is_key_char`]).
    Any,
    /// Letters and digits only.
    Alphanumeric,
    /// Upper-case letters and digits only: how AWS writes every key id. For
    /// a prefix as short as `A3T`, the case is what keeps the start of a
    /// mixed-case base64 run from reading as a key.
    UpperAlphanumeric,
    /// Letters, digits, `-` and `_`: the URL-safe base64 alphabet without
    /// padding. A `/` or a `.` in the run means a path or a URL, not a key.
    UrlSafe,
    /// Any key character, and `%` and `\`: a browser cookie copied from a
    /// header or a JSON export keeps its percent-encoding and its escapes, and
    /// a run that ended at the first `%` left the rest of the cookie visible.
    Cookie,
    /// A structure the vendor's key has, checked by the function: numeric
    /// parts, a fixed encoded host, a digit a short key always carries.
    Shape(fn(&str) -> bool),
}

impl Body {
    /// Whether `c` continues a key's run past the prefix.
    fn continues(self, c: char) -> bool {
        is_key_char(c) || matches!(self, Body::Cookie) && matches!(c, '%' | '\\')
    }

    fn admits(self, body: &str) -> bool {
        match self {
            Body::Any | Body::Cookie => true,
            Body::Shape(shape) => shape(body),
            Body::Alphanumeric => body.bytes().all(|b| b.is_ascii_alphanumeric()),
            Body::UpperAlphanumeric => body
                .bytes()
                .all(|b| b.is_ascii_uppercase() || b.is_ascii_digit()),
            Body::UrlSafe => body
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_'),
        }
    }
}

impl SecretPattern {
    /// Validated UTF-8 byte spans. Rejected prefixes do not hide subsequent
    /// candidates; a matched token is consumed once, including nested prefixes.
    fn spans<'a>(
        &'a self,
        content: &'a str,
        boundary: &'a dyn Fn(usize) -> bool,
    ) -> impl Iterator<Item = std::ops::Range<usize>> + 'a {
        let pem = self.prefix == "-----BEGIN";
        let mut token_end = 0;
        let mut consumed = 0;
        content
            .match_indices(self.prefix)
            .filter_map(move |(start, _)| {
                // A PEM header opens with its own dashes, which are the
                // boundary; one glued to the word before it is still a header.
                if start < consumed || !(pem || starts_a_token(content, start) || boundary(start)) {
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
                                .find(|c: char| !self.body.continues(c))
                                .unwrap_or(content.len() - body);
                    }
                    // A period closing a sentence is not part of the key.
                    let end = start + content[start..token_end].trim_end_matches('.').len();
                    let token = &content[start..end];
                    if token.len() < self.min_len || !self.body.admits(&token[self.prefix.len()..])
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
///   - **A key split across lines.** `const k = "sk-ant-" +` on one line and
///     `"api03-…"` on the next is not detected; the scan reads one line at a
///     time. On one line, a run of quoted literals separated only by
///     whitespace or by one `+` is joined and scanned as one token
///     ([`join_quoted_literals`]), but only a match that starts in one literal
///     and ends in a later one is taken from that join. A secret wholly inside
///     one literal is read on the original line, so the literal before it
///     cannot hide the token start. A Rust raw string (`r`, `br`, `cr`, or
///     `bcr`, any ASCII case, then one or more hashes) is not a join. Joining
///     lines first would mean deciding
///     where a logical line ends in every language a repository contains, and
///     getting that wrong brings back the false positives this design exists
///     to avoid.
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
///
/// Formats the vendor does not publish are taken from Kingfisher's bundled
/// rules (MongoDB's Kingfisher, Apache-2.0, 2.16.0), and say so where they
/// appear: a prefix, an alphabet and a length are facts about a key, not
/// Kingfisher's expressions, and none of its patterns is copied here.
const SECRET_PATTERNS: &[SecretPattern] = &[
    SecretPattern {
        name: "anthropic api key",
        prefix: "sk-ant-",
        min_len: 24,
        body: Body::Any,
    },
    SecretPattern {
        name: "openai project api key",
        prefix: "sk-proj-",
        min_len: 24,
        body: Body::Any,
    },
    // Stripe: `sk_`/`rk_`, the mode, and ten or more letters and digits —
    // Kingfisher's stripe-access-token rule, which also admits a `prod` mode
    // that Stripe's docs do not show. The floor was 24, which passed a short
    // key. The body is checked by `stripe_body`: base64's alphabet, so an
    // `sk_test_fixture_…` identifier is not a key while the base64 bodies
    // other vendors issue under `sk_live_` still are, and a body with no
    // digit must still be the 16 characters the old floor asked.
    SecretPattern {
        name: "stripe live key",
        prefix: "sk_live_",
        min_len: 18,
        body: Body::Shape(stripe_body),
    },
    SecretPattern {
        name: "stripe test key",
        prefix: "sk_test_",
        min_len: 18,
        body: Body::Shape(stripe_body),
    },
    SecretPattern {
        name: "stripe prod key",
        prefix: "sk_prod_",
        min_len: 18,
        body: Body::Shape(stripe_body),
    },
    SecretPattern {
        name: "stripe restricted test key",
        prefix: "rk_test_",
        min_len: 18,
        body: Body::Shape(stripe_body),
    },
    SecretPattern {
        name: "stripe restricted live key",
        prefix: "rk_live_",
        min_len: 18,
        body: Body::Shape(stripe_body),
    },
    SecretPattern {
        name: "stripe restricted prod key",
        prefix: "rk_prod_",
        min_len: 18,
        body: Body::Shape(stripe_body),
    },
    // OpenAI admin keys: the Admin API Keys reference's create example returns
    // a value beginning `sk-admin-`
    // (https://platform.openai.com/docs/api-reference/admin-api-keys/create).
    // The docs state no length; the floor is this table's usual one for a
    // vendor prefix long enough not to occur in prose.
    SecretPattern {
        name: "openai admin api key",
        prefix: "sk-admin-",
        min_len: 24,
        body: Body::Any,
    },
    // OpenAI service-account keys: `sk-svcacct-`, then a body written in the
    // URL-safe alphabet with `-` and `_`, which is why the legacy `sk-` rule
    // below (alphanumeric bodies only) never saw them. Gitleaks'
    // `openai-api-key` rule names `sk-(?:proj|svcacct|admin)-` as one family
    // (https://github.com/gitleaks/gitleaks, config/gitleaks.toml); OpenAI
    // states no length, so the floor matches the sibling prefixes above.
    SecretPattern {
        name: "openai service account api key",
        prefix: "sk-svcacct-",
        min_len: 24,
        body: Body::Any,
    },
    // OpenRouter: `sk-or-v1-` plus 64 hex characters, 73 in all. The format is
    // read off the key examples on
    // https://openrouter.ai/docs/api-reference/api-keys/create-api-key and is
    // not stated there as a rule, so the floor sits well below 73.
    SecretPattern {
        name: "openrouter api key",
        prefix: "sk-or-v1-",
        min_len: 40,
        body: Body::Any,
    },
    // Last of the `sk` family, because it is a prefix of the ones above and the
    // scan stops at its first match. The floor is high — a legacy OpenAI key is
    // `sk-` plus 48 characters — so `sk-test`, an `sk-` in prose, and this
    // file's own pattern literals stay below it.
    SecretPattern {
        name: "openai api key",
        prefix: "sk-",
        min_len: 45,
        body: Body::Alphanumeric,
    },
    // Stripe endpoint signing secret: "a signing secret beginning with
    // `whsec_`" (https://docs.stripe.com/webhooks). No length is stated.
    SecretPattern {
        name: "stripe webhook signing secret",
        prefix: "whsec_",
        min_len: 24,
        body: Body::Any,
    },
    SecretPattern {
        name: "xai api key",
        prefix: "xai-",
        min_len: 20,
        body: Body::Any,
    },
    SecretPattern {
        name: "google api key",
        prefix: "AIza",
        min_len: 30,
        body: Body::Any,
    },
    // Gemini's `AQ.` key: `AQ.Ab8RN6` and 44 URL-safe characters. This is
    // Kingfisher's rule. Google's API-key pages (AI Studio's, which makes
    // every new key an auth key from 2026-05-28, and Cloud's "Manage API
    // keys") describe auth keys but state no format.
    SecretPattern {
        name: "google gemini api key",
        prefix: "AQ.Ab8RN6",
        min_len: 53,
        body: Body::UrlSafe,
    },
    // Google OAuth 2.0 access token. Google's Compute Engine guide prints one
    // as `ya29.AHES6ZRN3-…` (59 characters) and its IAM downscoping guide as
    // `ya29.dr.AbCDeFg-…`, so the body carries dots and is left open; the
    // prefix is Google's alone:
    // https://docs.cloud.google.com/compute/docs/access/authenticate-workloads
    // https://docs.cloud.google.com/iam/docs/create-downscoped-short-lived-credentials
    // Google states no alphabet, only that an access token is at most 2048
    // bytes (https://developers.google.com/identity/protocols/oauth2), so a
    // narrower body would risk refusing a real token and passing it whole.
    SecretPattern {
        name: "google oauth access token",
        prefix: "ya29.",
        min_len: 30,
        body: Body::Any,
    },
    // Google OAuth 2.0 refresh token, printed as `1//xEoDL4iW3cxlI7yDbSRFYNG01k…`
    // (46 characters) in https://developers.google.com/identity/protocols/oauth2/native-app.
    // `1//` is also a URL's or a path's `…1//…` and Python's floor division,
    // so the body must be URL-safe (no `/` or `.`) and the floor sits above a
    // `1//`-prefixed UUID.
    SecretPattern {
        name: "google oauth refresh token",
        prefix: "1//",
        min_len: 40,
        body: Body::UrlSafe,
    },
    // Google OAuth 2.0 client secret: `GOCSPX-` and a URL-safe body. Google
    // publishes the prefix only by example and states no length: its codelab
    // shows `"client_secret": "GOCSPX-abcd1234-…"`, a 26-character body, in
    // https://codelabs.developers.google.com/generate_creatives_google_ads.
    // Secret scanners expect a 28-character body, 35 characters in all
    // (GitLab's secret-detection rule is `GOCSPX-[a-zA-Z0-9_-]{28}`). The floor
    // of 30, a 23-character body, sits under both, and the body is left open,
    // because a real secret refused by either check is a secret passed whole.
    SecretPattern {
        name: "google oauth client secret",
        prefix: "GOCSPX-",
        min_len: 30,
        body: Body::Any,
    },
    SecretPattern {
        name: "github personal access token",
        prefix: "ghp_",
        min_len: 30,
        body: Body::Any,
    },
    SecretPattern {
        name: "github oauth token",
        prefix: "gho_",
        min_len: 30,
        body: Body::Any,
    },
    SecretPattern {
        name: "github app server token",
        prefix: "ghs_",
        min_len: 30,
        body: Body::Any,
    },
    SecretPattern {
        name: "github app user token",
        prefix: "ghu_",
        min_len: 30,
        body: Body::Any,
    },
    SecretPattern {
        name: "github refresh token",
        prefix: "ghr_",
        min_len: 30,
        body: Body::Any,
    },
    SecretPattern {
        name: "github pat",
        prefix: "github_pat_",
        min_len: 40,
        body: Body::Any,
    },
    SecretPattern {
        name: "gitlab personal access token",
        prefix: "glpat-",
        min_len: 26,
        body: Body::Any,
    },
    // GitLab token prefixes, from the table at
    // https://docs.gitlab.com/security/tokens/ , which states no length or
    // character set. GitLab's own secret-detection rules do
    // (gitlab-org/security-products/secret-detection/secret-detection-rules,
    // rules/mit/gitlab/gitlab.toml): every classic token is its prefix and a
    // 20-character body of `[0-9a-zA-Z_-]`, and the routable, partitioned and
    // versioned forms are longer. So a floor is the prefix plus 20 at most:
    // `gldt-`, `glrt-` and `glft-` tokens are 25 characters, and the floor of
    // 26 they shared with `glpat-` passed every one of them whole. `glwt-` is
    // listed there only in its routable form (45 or more); its floor is still
    // prefix plus 20, because a floor above a real token is a token passed,
    // and one below it costs nothing for a prefix this distinctive. No floor
    // is raised to a family's longer documented body: that would trade recall
    // for nothing measured.
    SecretPattern {
        name: "gitlab deploy token",
        prefix: "gldt-",
        min_len: 25,
        body: Body::Any,
    },
    SecretPattern {
        name: "gitlab runner authentication token",
        prefix: "glrt-",
        min_len: 25,
        body: Body::Any,
    },
    SecretPattern {
        name: "gitlab runner registration-created token",
        prefix: "glrtr-",
        min_len: 26,
        body: Body::Any,
    },
    SecretPattern {
        name: "gitlab oauth application secret",
        prefix: "gloas-",
        min_len: 26,
        body: Body::Any,
    },
    SecretPattern {
        name: "gitlab ci job token",
        prefix: "glcbt-",
        min_len: 26,
        body: Body::Any,
    },
    SecretPattern {
        name: "gitlab trigger token",
        prefix: "glptt-",
        min_len: 26,
        body: Body::Any,
    },
    SecretPattern {
        name: "gitlab feed token",
        prefix: "glft-",
        min_len: 25,
        body: Body::Any,
    },
    SecretPattern {
        name: "gitlab incoming mail token",
        prefix: "glimt-",
        min_len: 26,
        body: Body::Any,
    },
    SecretPattern {
        name: "gitlab agent for kubernetes token",
        prefix: "glagent-",
        min_len: 26,
        body: Body::Any,
    },
    SecretPattern {
        name: "gitlab workspace token",
        prefix: "glwt-",
        min_len: 25,
        body: Body::Any,
    },
    SecretPattern {
        name: "gitlab scim token",
        prefix: "glsoat-",
        min_len: 26,
        body: Body::Any,
    },
    SecretPattern {
        name: "gitlab feature flags client token",
        prefix: "glffct-",
        min_len: 26,
        body: Body::Any,
    },
    // GitLab's runner registration token, deprecated but still accepted:
    // `GR1348941` and 20 characters (GitLab's secret-detection rules,
    // gitlab_runner_registration_token, and Kingfisher's).
    SecretPattern {
        name: "gitlab runner registration token",
        prefix: "GR1348941",
        min_len: 29,
        body: Body::UrlSafe,
    },
    // GitLab's session cookie: `_gitlab_session=` and 32 characters
    // (Kingfisher's rule), or the cell-routed `cell-<n>-` and 64 hex
    // (GitLab's gitlab_session_cookie rule). A session cookie is a signed-in
    // browser: whoever holds it is that user until it expires.
    SecretPattern {
        name: "gitlab session cookie",
        prefix: "_gitlab_session=",
        min_len: 48,
        body: Body::UrlSafe,
    },
    // Slack token rotation: an app that opts in gets access tokens
    // `xoxe.xoxp-…`, `xoxe.xoxb-…` and `xoxe.xapp-…` and refresh tokens
    // `xoxe-1-…` (https://docs.slack.dev/authentication/using-token-rotation;
    // https://docs.slack.dev/authentication/tokens names `xoxe.xapp-`). No
    // length is stated, so the floor is the other Slack tokens'. The rotated
    // forms come before `xoxp-`, `xoxb-` and `xapp-`: a `.` is a token
    // boundary, and the inner prefix would otherwise name the token.
    SecretPattern {
        name: "slack rotated user token",
        prefix: "xoxe.xoxp-",
        min_len: 30,
        body: Body::Any,
    },
    SecretPattern {
        name: "slack rotated bot token",
        prefix: "xoxe.xoxb-",
        min_len: 30,
        body: Body::Any,
    },
    SecretPattern {
        name: "slack rotated app-level token",
        prefix: "xoxe.xapp-",
        min_len: 30,
        body: Body::Any,
    },
    SecretPattern {
        name: "slack refresh token",
        prefix: "xoxe-",
        min_len: 30,
        body: Body::Any,
    },
    SecretPattern {
        name: "slack bot token",
        prefix: "xoxb-",
        min_len: 30,
        body: Body::Any,
    },
    SecretPattern {
        name: "slack user token",
        prefix: "xoxp-",
        min_len: 30,
        body: Body::Any,
    },
    SecretPattern {
        name: "slack workspace token",
        prefix: "xoxa-",
        min_len: 30,
        body: Body::Any,
    },
    SecretPattern {
        name: "slack app-level token",
        prefix: "xapp-",
        min_len: 30,
        body: Body::Any,
    },
    // Slack's older and browser-session tokens, none of which Slack's token
    // page names; the shapes are Kingfisher's rules. Each short one carries
    // a structure the body must have, which is what lets its floor sit low.
    //   - `xoxo-`/`xoxs-` legacy tokens: three numeric parts and a hex secret.
    //   - `xoxa-`/`xoxr-` legacy workspace tokens: an optional `<digit>-`,
    //     then eight or more letters and digits. A long `xoxa-` token is
    //     already the workspace token above.
    //   - `xoxc-` session token: three numeric parts and 64 hex, 99 at least.
    //   - `xoxd-` session cookie: 100 or more characters, URL-encoded.
    SecretPattern {
        name: "slack legacy token",
        prefix: "xoxo-",
        min_len: 12,
        body: Body::Shape(slack_legacy_body),
    },
    SecretPattern {
        name: "slack legacy token",
        prefix: "xoxs-",
        min_len: 12,
        body: Body::Shape(slack_legacy_body),
    },
    SecretPattern {
        name: "slack workspace token",
        prefix: "xoxa-",
        min_len: 13,
        body: Body::Shape(slack_legacy_workspace_body),
    },
    SecretPattern {
        name: "slack legacy workspace token",
        prefix: "xoxr-",
        min_len: 13,
        body: Body::Shape(slack_legacy_workspace_body),
    },
    SecretPattern {
        name: "slack session token",
        prefix: "xoxc-",
        min_len: 99,
        body: Body::Any,
    },
    SecretPattern {
        name: "slack session cookie",
        prefix: "xoxd-",
        min_len: 105,
        body: Body::Cookie,
    },
    // The URL is the credential: whoever has it can post to the channel.
    SecretPattern {
        name: "slack incoming webhook",
        prefix: "https://hooks.slack.com/services/",
        min_len: 60,
        body: Body::Any,
    },
    // `SG.` + 22 + `.` + 43. The floor is the whole shape, so `SG.` in prose
    // never reaches it.
    SecretPattern {
        name: "sendgrid api key",
        prefix: "SG.",
        min_len: 69,
        body: Body::Any,
    },
    SecretPattern {
        name: "shopify access token",
        prefix: "shpat_",
        min_len: 38,
        body: Body::Alphanumeric,
    },
    SecretPattern {
        name: "shopify custom app token",
        prefix: "shpca_",
        min_len: 38,
        body: Body::Alphanumeric,
    },
    SecretPattern {
        name: "shopify private app token",
        prefix: "shppa_",
        min_len: 38,
        body: Body::Alphanumeric,
    },
    SecretPattern {
        name: "shopify shared secret",
        prefix: "shpss_",
        min_len: 38,
        body: Body::Alphanumeric,
    },
    SecretPattern {
        name: "aws access key id",
        prefix: "AKIA",
        min_len: 20,
        body: Body::Alphanumeric,
    },
    // Temporary credentials, and no less dangerous for it: an ASIA key plus its
    // session token is the same access as a long-lived one for as long as it
    // lasts, and it was the shape a leaked assume-role snippet carried.
    SecretPattern {
        name: "aws temporary access key id",
        prefix: "ASIA",
        min_len: 20,
        body: Body::Alphanumeric,
    },
    // The other credential ids in AWS's unique-ID prefix table
    // (https://docs.aws.amazon.com/IAM/latest/UserGuide/reference_identifiers.html#identifiers-prefixes):
    // `ABIA`, an AWS STS service bearer token, and `ACCA`, a context-specific
    // credential. Like `AKIA` they are 20 characters.
    SecretPattern {
        name: "aws sts service bearer token id",
        prefix: "ABIA",
        min_len: 20,
        body: Body::UpperAlphanumeric,
    },
    SecretPattern {
        name: "aws context-specific credential id",
        prefix: "ACCA",
        min_len: 20,
        body: Body::UpperAlphanumeric,
    },
    // `A3T` and one more character: an access key id prefix that secret
    // scanners match beside `AKIA` (gitleaks' and Betterleaks'
    // `A3T[A-Z0-9]`). AWS's prefix table does not list it, so this is a
    // scanner convention, not a documented format. Three characters is a
    // short prefix, so the body must be upper case and digits, as every AWS
    // key id is, and a mixed-case base64 run that starts `A3T` is refused.
    SecretPattern {
        name: "aws access key id",
        prefix: "A3T",
        min_len: 20,
        body: Body::UpperAlphanumeric,
    },
    // Amazon Bedrock short-term API key: `bedrock-api-key-`, the base64 of a
    // presigned URL with embedded credentials, then `&Version=1` (AWS's
    // aws-bedrock-token-generator README, "Token Format"). The URL is on
    // `bedrock.amazonaws.com`, 21 bytes, so its base64 is a fixed 28
    // characters the body must open with; `&` ends the run, so the suffix
    // stays visible and the credential does not.
    SecretPattern {
        name: "aws bedrock short-term api key",
        prefix: "bedrock-api-key-",
        min_len: 44,
        body: Body::Shape(bedrock_presigned_body),
    },
    // Amazon Bedrock long-term API key: `ABSK` and 109 to 269 base64
    // characters. Kingfisher's rule; AWS's user guide describes the key, the
    // `ServiceApiKeyValue` of CreateServiceSpecificCredential, but not its
    // format. The base64 alphabet and the length keep `ABSK…` names out.
    SecretPattern {
        name: "aws bedrock long-term api key",
        prefix: "ABSK",
        min_len: 113,
        body: Body::Shape(base64_body),
    },
    SecretPattern {
        name: "huggingface token",
        prefix: "hf_",
        min_len: 30,
        body: Body::Any,
    },
    // HuggingFace organisation API token, deprecated but still in old
    // organisations' hands: `huggingface_hub`'s login refuses a token that
    // `startswith("api_org")` (src/huggingface_hub/_login.py). HuggingFace
    // documents no length; scanners give the body as 34 letters, 42 in all.
    // The body is letters and digits, so an `api_org_…` identifier is not one.
    SecretPattern {
        name: "huggingface organization token",
        prefix: "api_org_",
        min_len: 40,
        body: Body::Alphanumeric,
    },
    // npm: `npm_` and 36 letters and digits, 40 in all. With any key
    // character admitted, every `fn npm_…()` test name was a token. The floor
    // stays where it was, below the real length.
    SecretPattern {
        name: "npm access token",
        prefix: "npm_",
        min_len: 36,
        body: Body::Alphanumeric,
    },
    // PyPI API token: `pypi-` and a base64 macaroon; the secret-reporting
    // docs give the pattern as `pypi-[A-Za-z0-9-_]{85,}`, 90 bytes with the
    // prefix (https://docs.pypi.org/api/secrets/).
    SecretPattern {
        name: "pypi api token",
        prefix: "pypi-",
        min_len: 90,
        body: Body::Any,
    },
    // DigitalOcean: `dop_v1_` personal access token, `doo_v1_` OAuth access
    // token, `dor_v1_` OAuth refresh token
    // (https://www.digitalocean.com/blog/updated-api-tokens-new-management-features,
    // repeated in https://docs.digitalocean.com/reference/api/reference/security/).
    // No length is stated.
    SecretPattern {
        name: "digitalocean personal access token",
        prefix: "dop_v1_",
        min_len: 32,
        body: Body::Any,
    },
    SecretPattern {
        name: "digitalocean oauth access token",
        prefix: "doo_v1_",
        min_len: 32,
        body: Body::Any,
    },
    SecretPattern {
        name: "digitalocean oauth refresh token",
        prefix: "dor_v1_",
        min_len: 32,
        body: Body::Any,
    },
    // Replicate: "40-character strings that always start with `r8_`"
    // (https://replicate.com/docs/topics/security/api-tokens).
    SecretPattern {
        name: "replicate api token",
        prefix: "r8_",
        min_len: 40,
        body: Body::Any,
    },
    // Linear: personal API keys `lin_api_` and OAuth tokens `lin_oauth_`, from
    // the changelog entry that introduced the prefixes so GitHub could scan for
    // them (https://linear.app/changelog). No length is stated.
    SecretPattern {
        name: "linear api key",
        prefix: "lin_api_",
        min_len: 32,
        body: Body::Any,
    },
    SecretPattern {
        name: "linear oauth token",
        prefix: "lin_oauth_",
        min_len: 32,
        body: Body::Any,
    },
    // Notion: new Public API tokens begin `ntn_` in place of `secret_` since
    // 2024-09-25 (https://developers.notion.com/page/changelog). Notion advises
    // against validating tokens by pattern; this is a leak detector, which
    // errs the other way, and the old `secret_` prefix is left out as too
    // common a word to stop a commit for.
    SecretPattern {
        name: "notion api token",
        prefix: "ntn_",
        min_len: 30,
        body: Body::Any,
    },
    // Groq: `gsk_` and 52 letters and digits, TruffleHog's `groq` detector
    // (`gsk_[a-zA-Z0-9]{52}`, https://github.com/trufflesecurity/trufflehog,
    // pkg/detectors/groq). Groq's documentation does not state the format. The
    // floor sits below that length, and the alphanumeric body keeps a
    // `gsk_config_value` identifier from reading as a key.
    SecretPattern {
        name: "groq api key",
        prefix: "gsk_",
        min_len: 48,
        body: Body::Alphanumeric,
    },
    // Atlassian API tokens: `ATATT3` and a long run of URL-safe base64 with
    // `=`, 186 characters after the prefix in Gitleaks'
    // `atlassian-api-token` rule (config/gitleaks.toml). Atlassian documents
    // no length, and other scanners anchor on the prefix alone, so the floor is
    // half of that.
    SecretPattern {
        name: "atlassian api token",
        prefix: "ATATT3",
        min_len: 100,
        body: Body::Shape(atlassian_body),
    },
    // Databricks personal access token: `dapi`, 32 hex digits and an optional
    // `-` and digit (Kingfisher's imported Betterleaks rule
    // `dapi[a-f0-9]{32}(?:-\d)?`; AWS Secrets Manager's partner documentation
    // describes it as "dapi followed by 32 hexadecimal characters",
    // https://docs.aws.amazon.com/secretsmanager/latest/userguide/mes-partner-DatabricksPAT.html).
    // `dapi` begins ordinary words, so only the hex body makes it a key.
    SecretPattern {
        name: "databricks personal access token",
        prefix: "dapi",
        min_len: 36,
        body: Body::Shape(databricks_body),
    },
    SecretPattern {
        name: "private key block",
        prefix: "-----BEGIN",
        min_len: 20,
        body: Body::Any,
    },
];

/// Whether every byte of `body` is in standard base64's alphabet, padding
/// included.
fn base64_body(body: &str) -> bool {
    body.bytes()
        .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'+' | b'/' | b'='))
}

/// Atlassian's body: the URL-safe base64 alphabet, and `=`.
fn atlassian_body(body: &str) -> bool {
    body.bytes()
        .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'='))
}

/// Databricks' body: at least 32 hex digits, then optionally `-` and digits.
fn databricks_body(body: &str) -> bool {
    let hex = match body.rsplit_once('-') {
        Some((hex, suffix)) if !suffix.is_empty() && suffix.bytes().all(|b| b.is_ascii_digit()) => {
            hex
        }
        _ => body,
    };
    hex.len() >= 32 && hex.bytes().all(|b| b.is_ascii_hexdigit())
}

/// Stripe's body: base64's alphabet — letters and digits for Stripe's own
/// keys, `+`, `/` and `=` for the base64 keys other vendors issue under the
/// same prefixes — so an `sk_test_fixture_loader` identifier is refused. A
/// body under 16 characters must hold a digit: a random key's does, and a
/// short camelCase name (`sk_live_dashboardView`) does not.
fn stripe_body(body: &str) -> bool {
    base64_body(body) && (body.len() >= 16 || body.bytes().any(|b| b.is_ascii_digit()))
}

/// The base64 of `bedrock.amazonaws.com`, where every presigned URL a
/// Bedrock short-term key encodes begins. It is 21 bytes, a multiple of
/// three, so the encoding of what follows starts on a fresh quantum and these
/// 28 characters open every such key unchanged.
const BEDROCK_HOST_BASE64: &str = "YmVkcm9jay5hbWF6b25hd3MuY29t";

fn bedrock_presigned_body(body: &str) -> bool {
    body.starts_with(BEDROCK_HOST_BASE64) && base64_body(body)
}

/// A Slack legacy token's body: three numeric parts, then a hex secret.
fn slack_legacy_body(body: &str) -> bool {
    let mut parts = body.splitn(4, '-');
    let numeric =
        |p: Option<&str>| p.is_some_and(|p| !p.is_empty() && p.bytes().all(|b| b.is_ascii_digit()));
    numeric(parts.next())
        && numeric(parts.next())
        && numeric(parts.next())
        && parts
            .next()
            .is_some_and(|p| !p.is_empty() && p.bytes().all(|b| b.is_ascii_hexdigit()))
}

/// A Slack legacy workspace token's body: an optional `<digit>-`, then eight
/// or more letters and digits before anything else.
fn slack_legacy_workspace_body(body: &str) -> bool {
    let rest = match body.as_bytes() {
        [d, b'-', ..] if d.is_ascii_digit() => &body[2..],
        _ => body,
    };
    rest.bytes().take_while(u8::is_ascii_alphanumeric).count() >= 8
}

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
        name: "gitlab incoming email token",
        keep: 0,
        spans: gitlab_incoming_email_spans,
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

/// One quoted literal on `line`.
struct Lit {
    /// First byte of the literal, including a string prefix.
    start: usize,
    /// The opening quote. A triple quote still records its first byte.
    quote_at: usize,
    /// First byte of the raw interior.
    content: usize,
    /// The closing quote, or `line.len()` when the literal never closes.
    content_end: usize,
    /// One past the closing delimiter, or `line.len()` when it never closes.
    end: usize,
    /// Closed, and not a format string, a verbatim string, or an identifier
    /// glued to the quote. Only joinable literals are concatenated.
    joinable: bool,
    /// A plain quote that never closed. The scan treats the quote as an
    /// ordinary character and goes on at the next one, so an apostrophe in a
    /// comment does not swallow the well-formed literals after it.
    resync: bool,
}

fn is_prefix_letter(b: u8) -> bool {
    matches!(
        b,
        b'r' | b'R' | b'u' | b'U' | b'b' | b'B' | b'f' | b'F' | b'c' | b'C'
    )
}

fn is_ident_byte(b: u8) -> bool {
    b.is_ascii_alphanumeric() || b == b'_'
}

/// A hashed Rust raw string: `r#"..."#`, `br#"..."#`, `cr#"..."#`, or
/// `bcr#"..."#`, with the prefix in any ASCII case and one or more hashes.
/// The hashes are part of the delimiter, so quotes inside it are not string
/// boundaries and the span is not joinable. The same prefixes with no hash
/// (`r"..."`, `br"..."`, `cr"..."`) stay ordinary prefixed literals and stay
/// joinable. An identifier glued on the front (`xr#"..."#`) is not a raw
/// string. An unclosed or mismatched closer runs to the end of the line and
/// is not joinable.
fn try_raw_hash(line: &str, i: usize) -> Option<Lit> {
    let b = line.as_bytes();
    if i >= b.len() || !matches!(b[i].to_ascii_lowercase(), b'b' | b'c' | b'r') {
        return None;
    }
    // Longest first, so `bcr` is not read as `br` plus a stray `c`.
    const PREFIXES: [&[u8]; 4] = [b"bcr", b"br", b"cr", b"r"];
    let prefix_len = PREFIXES.into_iter().find_map(|prefix| {
        let end = i.checked_add(prefix.len())?;
        let got = b.get(i..end)?;
        got.iter()
            .zip(prefix)
            .all(|(got, want)| got.eq_ignore_ascii_case(want))
            .then_some(prefix.len())
    })?;
    // `foo_r#"..."#` and `xr#"..."#` are identifiers, not raw strings.
    if i > 0 && is_ident_byte(b[i - 1]) {
        return None;
    }
    let mut hash_end = i + prefix_len;
    if hash_end >= b.len() || b[hash_end] != b'#' {
        return None;
    }
    while hash_end < b.len() && b[hash_end] == b'#' {
        hash_end += 1;
    }
    let hashes = hash_end - (i + prefix_len);
    if hash_end >= b.len() || b[hash_end] != b'"' {
        return None;
    }
    let quote_at = hash_end;
    let content = quote_at + 1;
    let mut k = content;
    while k < b.len() {
        if b[k] == b'"'
            && let Some(end) = k.checked_add(1 + hashes)
            && end <= b.len()
            && b[k + 1..end].iter().all(|c| *c == b'#')
        {
            return Some(Lit {
                start: i,
                quote_at,
                content,
                content_end: k,
                end,
                joinable: false,
                resync: false,
            });
        }
        k += 1;
    }
    Some(Lit {
        start: i,
        quote_at,
        content,
        content_end: line.len(),
        end: line.len(),
        joinable: false,
        resync: false,
    })
}

/// The literal at `i`: a hashed Rust raw string, or an ordinary quote.
fn at_literal(line: &str, i: usize, dead: &Dead) -> Option<Lit> {
    try_raw_hash(line, i).or_else(|| try_literal(line, i, dead))
}

/// The ordinary literal that starts at `i`, if one does.
///
/// An unclosed quote is a literal through the end of the line and is not
/// joinable, so a `+` inside it stays text: the bytes are copied unchanged
/// and a secret written there is still scanned. Format strings (`f`, `fr`,
/// `rf`) and C# verbatim `@"..."` are recognised so they are not joined, and
/// then skipped the same way. Backticks and macro calls are not literals.
/// Hashed Rust raw strings are handled by [`try_raw_hash`] before this runs.
fn try_literal(line: &str, i: usize, dead: &Dead) -> Option<Lit> {
    let b = line.as_bytes();
    if i >= b.len() {
        return None;
    }
    let prev_ident = i > 0 && is_ident_byte(b[i - 1]);
    let (quote_at, format_prefix) = if b[i] == b'"' || b[i] == b'\'' {
        (i, false)
    } else if prev_ident || !is_prefix_letter(b[i]) {
        return None;
    } else {
        let mut j = i;
        while j < b.len() && j - i < 2 && is_prefix_letter(b[j]) {
            j += 1;
        }
        if j < b.len() && is_prefix_letter(b[j]) {
            return None;
        }
        if j >= b.len() || (b[j] != b'"' && b[j] != b'\'') {
            return None;
        }
        let format = b[i..j].contains(&b'f') || b[i..j].contains(&b'F');
        (j, format)
    };
    let quote = b[quote_at];
    let verbatim = quote == b'"' && quote_at > 0 && b[quote_at - 1] == b'@';
    let glued = quote_at == i && prev_ident;
    let triple = quote_at + 2 < b.len() && b[quote_at + 1] == quote && b[quote_at + 2] == quote;
    let single = usize::from(quote == b'\'');
    let kind = if triple {
        2 + single
    } else if verbatim {
        4
    } else {
        single
    };
    let closer = if dead.0[kind].get() {
        None
    } else if triple {
        close_triple(b, quote_at, quote)
    } else if verbatim {
        close_verbatim(b, quote_at)
    } else {
        close_quoted(line, quote_at, quote)
    };
    // A scan of this kind that ran off the end means no later opener of the
    // same kind closes either (an escaped quote is skipped the same way from
    // wherever it is reached), so the answer is remembered rather than found
    // again at every later opener: that is what keeps `"\"\"\"…` linear.
    if closer.is_none() {
        dead.0[kind].set(true);
    }
    let unclosed_open = if triple { quote_at + 3 } else { quote_at + 1 };
    let (content, content_end, end) = closer.unwrap_or((unclosed_open, line.len(), line.len()));
    let closed = content_end < line.len();
    // `"$URL"` and `"Bearer ${TOKEN}"` are references written as literals.
    // Joining one onto the string beside it pulls the reference into a value
    // the scanners already found, and the redaction of that line changes.
    let reference = closed && interior_is_reference(line, content, content_end);
    Some(Lit {
        start: i,
        quote_at,
        content,
        content_end,
        end,
        joinable: closed && !format_prefix && !verbatim && !glued && !reference,
        resync: !closed,
    })
}

/// A quoted interior that names a value rather than being one: a shell
/// `$var` (or `\$var`, as it is written inside an eval or a heredoc), a
/// template, or a printf format with a conversion in it (`'KEY=%s\n'`). A `$`
/// later in a password is still a character.
fn interior_is_reference(line: &str, content: usize, content_end: usize) -> bool {
    let v = &line[content..content_end];
    v.starts_with('$')
        || v.starts_with("\\$")
        || ["${", "$(", "{{", "%("].iter().any(|t| v.contains(t))
        || has_printf_conversion(v)
}

/// Whether `v` holds a printf conversion: `%`, optional flags, width and
/// precision, then a conversion letter (`%s`, `%q`, `%v`, `%-8.3f`). `%%` is
/// a literal percent sign and is not one.
fn has_printf_conversion(v: &str) -> bool {
    let b = v.as_bytes();
    let mut i = 0;
    while i < b.len() {
        if b[i] != b'%' {
            i += 1;
            continue;
        }
        if b.get(i + 1) == Some(&b'%') {
            i += 2;
            continue;
        }
        let mut j = i + 1;
        while j < b.len() && matches!(b[j], b'-' | b'+' | b'#' | b' ' | b'0'..=b'9' | b'.' | b'*') {
            j += 1;
        }
        if j < b.len() && b"sdqvxXfFeEgGcTtpbouUi".contains(&b[j]) {
            return true;
        }
        i = j.max(i + 1);
    }
    false
}

/// `(content_end, end)` of a single-quoted literal, honouring backslash escapes.
fn close_quoted(line: &str, quote_at: usize, quote: u8) -> Option<(usize, usize, usize)> {
    let b = line.as_bytes();
    let mut i = quote_at + 1;
    while i < b.len() {
        if b[i] == b'\\' {
            i += 1;
            if i >= b.len() {
                break;
            }
            i += line[i..].chars().next()?.len_utf8();
            continue;
        }
        if b[i] == quote {
            return Some((quote_at + 1, i, i + 1));
        }
        i += line[i..].chars().next()?.len_utf8();
    }
    None
}

fn close_triple(b: &[u8], quote_at: usize, quote: u8) -> Option<(usize, usize, usize)> {
    let mut i = quote_at + 3;
    while i + 2 < b.len() {
        if b[i] == quote && b[i + 1] == quote && b[i + 2] == quote {
            return Some((quote_at + 3, i, i + 3));
        }
        i += 1;
    }
    None
}

fn close_verbatim(b: &[u8], quote_at: usize) -> Option<(usize, usize, usize)> {
    let mut i = quote_at + 1;
    while i < b.len() {
        if b[i] == b'"' {
            if i + 1 < b.len() && b[i + 1] == b'"' {
                i += 2;
                continue;
            }
            return Some((quote_at + 1, i, i + 1));
        }
        i += 1;
    }
    None
}

fn skip_hspace(b: &[u8], mut j: usize) -> usize {
    while j < b.len() && matches!(b[j], b' ' | b'\t' | 0x0c) {
        j += 1;
    }
    j
}

/// The next joinable literal after `i` when only whitespace, or whitespace
/// around one `+`, separates them.
fn join_sep(line: &str, i: usize, dead: &Dead) -> Option<Lit> {
    let b = line.as_bytes();
    let j = skip_hspace(b, i);
    if let Some(lit) = at_literal(line, j, dead)
        && lit.joinable
    {
        return Some(lit);
    }
    if j < b.len() && b[j] == b'+' && !matches!(b.get(j + 1).copied(), Some(b'+' | b'=')) {
        let k = skip_hspace(b, j + 1);
        if let Some(lit) = at_literal(line, k, dead)
            && lit.joinable
        {
            return Some(lit);
        }
    }
    None
}

/// Quote kinds whose closer was looked for and not found, one flag each: plain
/// `"`, plain `'`, triple `"`, triple `'`, and C# verbatim. See `try_literal`.
#[derive(Default)]
struct Dead([std::cell::Cell<bool>; 5]);

/// Appends `range` of `line` to `out`.
fn push_slice(out: &mut String, line: &str, range: std::ops::Range<usize>) {
    out.push_str(&line[range]);
}

/// Whether `line` holds a run of two or more joinable literals. Stops at the
/// second literal of the first run, so a line with no join costs one pass and
/// a line with one costs the distance to it.
fn has_quoted_join(line: &str, dead: &Dead) -> bool {
    let mut i = 0;
    while i < line.len() {
        match at_literal(line, i, dead) {
            Some(first) if !first.resync => {
                if first.joinable && join_sep(line, first.end, dead).is_some() {
                    return true;
                }
                i = first.end;
            }
            _ => i += line[i..].chars().next().map_or(1, char::len_utf8),
        }
    }
    false
}

/// The set bit that marks a [`Cut`] as the start of a joined piece.
const PIECE: usize = 1 << (usize::BITS - 1);

/// A point where the joined copy stops being a byte-for-byte copy of the line.
///
/// From shadow position `at()` on, shadow position `p` is line position
/// `orig + (p - at())`, until the next cut. There is one cut per joined piece
/// (flagged) and one after each run of pieces, so the map is sixteen bytes per
/// literal rather than eight per byte of the line.
struct Cut {
    at_and_flag: usize,
    orig: usize,
}

impl Cut {
    fn piece(at: usize, orig: usize) -> Cut {
        Cut {
            at_and_flag: at | PIECE,
            orig,
        }
    }

    fn after(at: usize, orig: usize) -> Cut {
        Cut {
            at_and_flag: at,
            orig,
        }
    }

    fn at(&self) -> usize {
        self.at_and_flag & !PIECE
    }

    fn is_piece(&self) -> bool {
        self.at_and_flag & PIECE != 0
    }
}

/// The joined copy of a line: every run of two or more joinable literals is
/// one literal (one opening quote, the concatenated interiors, one closing
/// quote), everything else is as written. `cuts` maps it back to the line.
struct Joined {
    shadow: String,
    cuts: Vec<Cut>,
}

impl Joined {
    /// Line position of shadow position `p`.
    fn orig_of(&self, p: usize) -> usize {
        match self.cuts.partition_point(|c| c.at() <= p) {
            0 => p,
            n => self.cuts[n - 1].orig + (p - self.cuts[n - 1].at()),
        }
    }

    /// The line bytes a shadow span was joined from; `None` for an empty span
    /// or one off the end of the shadow.
    fn orig_range(&self, span: &std::ops::Range<usize>) -> Option<std::ops::Range<usize>> {
        if span.start >= span.end || span.end > self.shadow.len() {
            return None;
        }
        Some(self.orig_of(span.start)..self.orig_of(span.end - 1) + 1)
    }

    /// The cut that starts the piece holding shadow byte `p`, if one does. A
    /// piece ends where the next cut begins, less the closing quote the copy
    /// keeps after the last piece of a run.
    fn piece_at(&self, p: usize) -> Option<usize> {
        let idx = self.cuts.partition_point(|c| c.at() <= p).checked_sub(1)?;
        if !self.cuts[idx].is_piece() {
            return None;
        }
        let next = self.cuts.get(idx + 1)?;
        let end = if next.is_piece() {
            next.at()
        } else {
            next.at() - 1
        };
        (p < end).then_some(idx)
    }

    /// Whether `span` starts in one joined piece and ends in a later one. A
    /// match wholly inside one piece is visible on the line as written, and
    /// taking it from the copy too would hide the quote that bounds it.
    fn crosses(&self, span: &std::ops::Range<usize>) -> bool {
        if span.start >= span.end {
            return false;
        }
        match (self.piece_at(span.start), self.piece_at(span.end - 1)) {
            (Some(first), Some(last)) => last > first,
            _ => false,
        }
    }

    /// Whether a piece begins at shadow position `p`. A piece begins at a
    /// quote, so a key that starts there starts a token whatever the piece
    /// before it ended in.
    fn is_piece_start(&self, p: usize) -> bool {
        let mut k = self.cuts.partition_point(|c| c.at() < p);
        while let Some(cut) = self.cuts.get(k)
            && cut.at() == p
        {
            if cut.is_piece() {
                return true;
            }
            k += 1;
        }
        false
    }

    /// The joined text of line bytes `a..b`: those bytes, less the quotes and
    /// separators the copy took out between pieces. It is the secret as a
    /// reader of the line sees it, and what a marker's length counts.
    fn joined_text(&self, a: usize, b: usize) -> String {
        let mut out = String::new();
        // Segment i is the shadow bytes from cut i to cut i+1, which are the
        // line bytes from cut i's `orig` on; segment -1 runs from 0 to cut 0.
        let first = self.cuts.partition_point(|c| c.orig <= a);
        let mut seg = first.checked_sub(1);
        loop {
            let (at, orig) = seg.map_or((0, 0), |k| (self.cuts[k].at(), self.cuts[k].orig));
            if orig >= b {
                break;
            }
            let next = seg.map_or(0, |k| k + 1);
            let end = self.cuts.get(next).map_or(self.shadow.len(), |c| c.at());
            let len = end - at;
            let (lo, hi) = (a.max(orig), b.min(orig + len));
            if lo < hi {
                out.push_str(&self.shadow[at + (lo - orig)..at + (hi - orig)]);
            }
            if next >= self.cuts.len() {
                break;
            }
            seg = Some(next);
        }
        out
    }
}

/// A line in which every same-line run of two or more quoted literals is one
/// literal, with the map back to `line`.
///
/// `None` when there is no such run, so the caller scans `line` itself and
/// redaction of every other line stays byte-identical. The join is lexical,
/// not a parse: `'` and `"` (including a Python `r`/`u`/`b` prefix and a
/// triple quote), separated by whitespace or one `+`. Not joined: a format
/// string, a quoted reference (`"$URL"`, `"${TOKEN}"`, `'KEY=%s'`), a backtick
/// template, a C# verbatim string, a hashed Rust raw string, a call or macro
/// (`concat!("A", "B")` is an accepted miss), or anything on the next line. A
/// quote that never closes is an ordinary character, and the scan goes on at
/// the next one. Interiors are copied raw, escapes and all; they are not
/// decoded.
fn join_quoted_literals(line: &str) -> Option<Joined> {
    let b = line.as_bytes();
    if !b.contains(&b'"') && !b.contains(&b'\'') {
        return None;
    }
    let dead = Dead::default();
    if !has_quoted_join(line, &dead) {
        return None;
    }
    let mut shadow = String::with_capacity(line.len());
    let mut cuts = Vec::new();
    let mut i = 0;
    while i < line.len() {
        match at_literal(line, i, &dead) {
            Some(first) if !first.resync => {
                let second = if first.joinable {
                    join_sep(line, first.end, &dead)
                } else {
                    None
                };
                let Some(second) = second else {
                    push_slice(&mut shadow, line, first.start..first.end);
                    i = first.end;
                    continue;
                };
                // One opening quote, so the scanners that already understand a
                // quoted literal see the concatenated interior. Extra quotes of
                // a triple-quoted piece are left out of the copy.
                push_slice(&mut shadow, line, first.start..first.quote_at + 1);
                let mut lit = first;
                let mut next = Some(second);
                loop {
                    cuts.push(Cut::piece(shadow.len(), lit.content));
                    push_slice(&mut shadow, line, lit.content..lit.content_end);
                    let Some(n) = next.take() else { break };
                    lit = n;
                    next = join_sep(line, lit.end, &dead);
                }
                push_slice(&mut shadow, line, lit.content_end..lit.content_end + 1);
                i = lit.end;
                cuts.push(Cut::after(shadow.len(), i));
            }
            _ => {
                let n = line[i..].chars().next().map_or(1, char::len_utf8);
                push_slice(&mut shadow, line, i..i + n);
                i += n;
            }
        }
    }
    Some(Joined { shadow, cuts })
}

/// Every credential on `line`, from both tables, as (name, span, bytes kept).
///
/// Spans refer to `line` as passed. A joined shadow is a different line; only
/// a span that crosses its pieces is mapped back onto the original.
fn detections(
    line: &str,
) -> impl Iterator<Item = (&'static str, std::ops::Range<usize>, usize)> + '_ {
    detections_at(line, &|_| false)
}

/// [`detections`] where `boundary(p)` also says that a token may start at byte
/// `p`: the start of a joined piece, which is the quote that opens a literal.
fn detections_at<'a>(
    line: &'a str,
    boundary: &'a dyn Fn(usize) -> bool,
) -> impl Iterator<Item = (&'static str, std::ops::Range<usize>, usize)> + 'a {
    let prefixed = SECRET_PATTERNS.iter().flat_map(move |p| {
        p.spans(line, boundary)
            .map(move |s| (p.name, s, p.prefix.len()))
    });
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

/// The token in a GitLab incoming-email address:
/// `incoming+<project path>-<project id>-<token>-issue@<host>`, and the
/// `-issue-<n>` and `-merge-request` forms (Kingfisher's
/// gitlab-incoming-mail-address-token rule). Whoever has the address can open
/// issues and merge requests as the user it belongs to.
///
/// The token is what follows the last `-<digits>-` before the suffix — the
/// project id, which the path before it may also contain digits beside —
/// and it is judged as every literal is, so a `<your-token>` slot or a short
/// word is not one.
fn gitlab_incoming_email_spans(line: &str) -> Vec<std::ops::Range<usize>> {
    /// Longest local part read. RFC 5321 allows 64 octets; a long project
    /// path can pass that, and the bound keeps a line of `incoming+` from
    /// being rescanned to its end from every one.
    const MAX_LOCAL: usize = 256;
    const LEAD: &str = "incoming+";
    let mut spans = Vec::new();
    for (at, _) in line.match_indices(LEAD) {
        if !starts_a_token(line, at) {
            continue;
        }
        let from = at + LEAD.len();
        let rest = &line.as_bytes()[from..];
        let window = &rest[..rest.len().min(MAX_LOCAL + 1)];
        let Some(len) = window
            .iter()
            .position(|&b| !(b.is_ascii_alphanumeric() || matches!(b, b'.' | b'_' | b'-')))
        else {
            continue;
        };
        if window[len] != b'@' {
            continue;
        }
        let local = &line[from..from + len];
        let Some(body) = local.strip_suffix("-merge-request").or_else(|| {
            let issue = local.rfind("-issue")?;
            let after = &local[issue + "-issue".len()..];
            let numbered = after
                .strip_prefix('-')
                .is_some_and(|n| !n.is_empty() && n.bytes().all(|b| b.is_ascii_digit()));
            (after.is_empty() || numbered).then(|| &local[..issue])
        }) else {
            continue;
        };
        // The token starts after the last `-<digits>-`, or `<digits>-` at the
        // start of the local part.
        let bytes = body.as_bytes();
        let token_start = (0..bytes.len()).rev().find(|&dash| {
            bytes[dash] == b'-' && dash + 1 < bytes.len() && {
                let digits = bytes[..dash]
                    .iter()
                    .rev()
                    .take_while(|b| b.is_ascii_digit())
                    .count();
                digits > 0 && (digits == dash || bytes[dash - digits - 1] == b'-')
            }
        });
        let Some(dash) = token_start else { continue };
        let token = &body[dash + 1..];
        let token_ok = token
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'_' | b'-'));
        if token_ok && token.len() >= MIN_LITERAL_LEN && !is_placeholder(token) {
            let start = from + dash + 1;
            spans.push(start..start + token.len());
        }
    }
    spans
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
        if b == b':' && value_from == i + 1 && annotates_a_type(line, i) {
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
    let head = head.trim_suffix(':').trim_end();
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
    /// (where the last comma chain began, where it ended).
    chain: Option<(usize, usize)>,
}

impl ValueEnds {
    /// Where an unquoted value ending at `end` is redacted to: on past each
    /// comma that is not followed by whitespace, to the next stop.
    ///
    /// The hop from a comma depends only on where it is, so every comma inside
    /// a chain already walked ends at the chain's end. Walking it again from
    /// each one made a line of `password=` and a comma-joined value, repeated,
    /// quadratic — one chain, as long as the line, started at every separator.
    fn reach_past_commas(&mut self, line: &str, end: usize) -> usize {
        if !line[end..].starts_with(',') {
            return end;
        }
        if let Some((from, last)) = self.chain
            && from <= end
            && end <= last
        {
            return last;
        }
        let mut whole = end;
        while line[whole..].starts_with(',') {
            let next = whole + 1;
            if next == line.len() || line[next..].starts_with(char::is_whitespace) {
                break;
            }
            whole = self
                .continued
                .at_or_after(line, next, |c| c.is_whitespace() || ",;)}]\"'".contains(c));
        }
        self.chain = Some((end, whole));
        whole
    }
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
    Some(start..ends.reach_past_commas(line, end))
}

/// Whether `value` reads as a literal password or secret rather than a
/// reference to one. `quoted` relaxes the expression test: inside quotes a
/// `.` or `(` is a character, not member access or a call.
fn is_literal_secret(value: &str, quoted: bool) -> bool {
    // Judged on its first `MAX_JUDGED_VALUE` bytes. The detectors ask this at
    // every separator, and the value after a separator can run to the end of
    // the line: `password=` repeated made each ask read the whole rest, so a
    // 400 KB line cost forty seconds. Every property read below is settled
    // within a credential's first kilobyte, and the redaction span is
    // unaffected: it still covers the whole value.
    let value = judged_prefix(value);
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
    // An increment or decrement of a bare name: `++latest`, `--remaining`.
    // Only those two: a password may start with one `!`, `-`, `+` or `*`
    // (`!Welcomeback`), and a single operator says too little to excuse it.
    // The operand is letters and `_` alone — no digit — as a name is.
    let operand = value
        .strip_prefix("++")
        .or_else(|| value.strip_prefix("--"))
        .unwrap_or(value);
    let operated_name = operand.len() < value.len()
        && !operand.is_empty()
        && operand
            .bytes()
            .all(|b| b.is_ascii_alphabetic() || b == b'_');
    // An expression rather than a literal: a call, an index, a member access
    // or path (`Duration::ZERO`), a block or object literal or format slot
    // (`{scrubbed}`, which the unquoted value's end at `}` cuts to
    // `{scrubbed`), an operator applied.
    let expression = !quoted
        && (value.contains(['(', '['])
            || value.starts_with('{')
            || value.contains("::")
            || value.contains('.') && !has_digit
            || qualified_name
            || operated_name);
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

/// Most bytes of a value [`is_literal_secret`] reads. A real password, key or
/// token is far shorter; the bound is what keeps judging a value at every
/// separator of a long line linear.
const MAX_JUDGED_VALUE: usize = 512;

/// `value` cut to at most [`MAX_JUDGED_VALUE`] bytes at a character boundary.
fn judged_prefix(value: &str) -> &str {
    if value.len() <= MAX_JUDGED_VALUE {
        return value;
    }
    let mut end = MAX_JUDGED_VALUE;
    while !value.is_char_boundary(end) {
        end -= 1;
    }
    &value[..end]
}

/// Whether `text` is at least `min` and at most four words of three or more
/// letters each — how an identifier reads, and how a random token does not.
fn reads_as_words(text: &str, min: usize) -> bool {
    let w = words(text);
    (min..=4).contains(&w.len())
        && w.iter()
            .all(|x| x.len() >= 3 && x.bytes().all(|b| b.is_ascii_alphabetic()))
}

/// A password given to a command-line client, in the spellings each client
/// documents:
///
///   - the MySQL family's attached `-p<password>` (a detached `-p` prompts, and
///     is not one) and `sshpass -p <password>`;
///   - `curl -u user:password`, `--user user:password`, `-uuser:password`
///     (curl manual, <https://curl.se/docs/manpage.html>: `-u name:password`);
///   - `docker login -p <password>`, `--password <password>`, `-p<password>`
///     (<https://docs.docker.com/reference/cli/docker/login/>: `-p, --password`,
///     where `-` reads standard input and is no credential);
///   - any `--password <value>`, `--passwd <value>` or `--passphrase <value>`;
///   - a `.netrc` entry's `password <value>`, alone on a line or after
///     `machine`/`login`/`default` on one
///     (<https://everything.curl.dev/usingcurl/netrc.html>: entries may span
///     lines or sit on one).
///
/// Only after the client's own name, because `-p` and `-u` mean something else
/// to nearly every other command (`mkdir -p`, `ssh -u`). Every value is judged
/// by the rule the other detectors use: a literal, not a reference to one.
fn cli_password_spans(line: &str) -> Vec<std::ops::Range<usize>> {
    const ATTACHED: &[&str] = &[
        "mysql",
        "mysqldump",
        "mysqladmin",
        "mysqlimport",
        "mariadb",
        "mariadb-dump",
    ];
    const LONG_FLAGS: &[&str] = &["--password", "--passwd", "--passphrase"];
    let words: Vec<(usize, &str)> = line
        .split_whitespace()
        .map(|w| (w.as_ptr() as usize - line.as_ptr() as usize, w))
        .collect();
    let mut spans = Vec::new();
    let mut client: Option<&str> = None;
    // Whether this command is a `.netrc` entry: it names a machine, a login or
    // the default, or the line starts with `password`.
    let netrc_entry = words.first().is_some_and(|&(_, w)| w == "password")
        || words
            .iter()
            .any(|&(_, w)| matches!(w, "machine" | "login" | "default"));
    let next_word_at = |i: usize| words.get(i + 1).map(|&(at, _)| at);
    for (i, &(at, word)) in words.iter().enumerate() {
        // The client's name, with what is glued to its front taken off: a
        // directory, and the `=`, `(`, backtick or quote that a command
        // recorded as `command=curl`, run as `$(curl …)` or written inside a
        // string literal puts there. Reading only a bare `curl` passed the
        // password after every one of those.
        let program = word
            .rsplit(['/', '=', '(', '`', '"', '\''])
            .next()
            .unwrap_or(word);
        if ATTACHED.contains(&program) || matches!(program, "sshpass" | "curl") {
            client = Some(program);
            continue;
        }
        if program == "docker" && words.get(i + 1).is_some_and(|&(_, w)| w == "login") {
            client = Some("docker login");
            continue;
        }
        if matches!(word, "|" | "&&" | "||" | ";") {
            client = None;
            continue;
        }
        // A flag or keyword whose value is the next word.
        let detached = LONG_FLAGS.contains(&word) || (netrc_entry && word == "password");
        let start = if detached {
            next_word_at(i)
        } else {
            match client {
                Some("sshpass" | "docker login") if word == "-p" => next_word_at(i),
                Some("docker login")
                    if word.len() > 2 && word.starts_with("-p") && !word.starts_with("--") =>
                {
                    Some(at + 2)
                }
                Some("curl") if matches!(word, "-u" | "--user") => next_word_at(i),
                Some("curl") if word.starts_with("--user=") => Some(at + "--user=".len()),
                Some("curl")
                    if word.len() > 2 && word.starts_with("-u") && !word.starts_with("--") =>
                {
                    Some(at + 2)
                }
                Some(c)
                    if ATTACHED.contains(&c)
                        && word.len() > 2
                        && word.starts_with("-p")
                        && !word.starts_with("--") =>
                {
                    Some(at + 2)
                }
                _ => None,
            }
        };
        let Some(mut start) = start else { continue };
        // The whole shell word, which a quoted space does not end:
        // `-pS3cr3t"x y"` is one argument, and redacting only the part before
        // the space left the rest of the password visible.
        let end = shell_word_end(line, start);
        // curl's value is `user:password`; the credential is what follows the
        // first colon, and a user with no colon makes curl prompt.
        if client == Some("curl") && !detached {
            match line[start..end].find(':') {
                Some(colon) => start += colon + 1,
                None => continue,
            }
        }
        let raw = &line[start..end];
        // A flag after `--password` is the next option, not its value.
        if raw.is_empty() || (LONG_FLAGS.contains(&word) && raw.starts_with('-')) {
            continue;
        }
        let quoted = raw.contains(['"', '\'']);
        // Judged without its quotes and its quoted spaces: after `-p` the
        // context already says this is a password, so a space in it is a
        // character of the password, not a sign of prose.
        let value: String = raw
            .chars()
            .filter(|c| !matches!(c, '"' | '\'') && !c.is_whitespace())
            .collect();
        // A command read inside a backtick or a `$(…)` ends at the closer,
        // which a value glued to it — ``…`curl -u user:password`, …`` in
        // prose — would otherwise carry into its judging, hiding the generic
        // word it is. Only the judging drops it; the redaction keeps the word
        // whole, and a value with no closer is judged as written.
        let unclosed = value.trim_end_matches([',', ';', '.', ':']);
        let judged = if unclosed.ends_with(['`', ')']) {
            unclosed.trim_end_matches(['`', ')'])
        } else {
            value.as_str()
        };
        if is_literal_secret(judged, quoted) {
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
        || has_brace_slot(value)
        || ["${", "{{", "$(", "%("].iter().any(|t| value.contains(t))
        || WORDS.iter().any(|w| lowered.contains(w))
        || value
            .chars()
            .all(|c| c == value.chars().next().unwrap_or(c))
        || value.contains('…')
}

/// Whether `value` holds a single-brace template slot — `{}`, `{token}`,
/// `{pw:?}`, `{0}` — the interpolation of a Rust or Python format string,
/// which a program fills in. Like the angle-bracket slot, it needs its closing
/// brace, and its inside must read as a name or a format spec: braces around
/// anything else are characters of a literal. One pass, so a value of
/// unclosed braces costs no more than its length.
fn has_brace_slot(value: &str) -> bool {
    let mut open = false;
    for b in value.bytes() {
        match b {
            b'{' => open = true,
            b'}' if open => return true,
            _ if open
                && !(b.is_ascii_alphanumeric()
                    || matches!(b, b'_' | b'.' | b':' | b'?' | b'#')) =>
            {
                open = false;
            }
            _ => {}
        }
    }
    false
}

/// Whether the `:` at `colon` opens a type annotation rather than a value:
/// `MIN_TOKEN_INTERVAL: Duration = …`, `let token: String = …`, a parameter
/// `token_delay: Duration)`, a field `password: Password,`. The name before
/// the colon is a credential's; what follows it is a type, and reading that
/// type as the literal assigned flagged `Duration` as a token.
///
/// A type followed by `=` is annotated whatever it looks like. One followed by
/// `)`, `,`, `;` or `{` is only when it reads as a capitalised name of words,
/// because a YAML flow mapping writes a password the same way.
fn annotates_a_type(line: &str, colon: usize) -> bool {
    /// Longest type read. A type is short; the bound keeps a line of `:`s
    /// from being rescanned to its end at every one of them.
    const MAX_TYPE_LEN: usize = 128;
    let rest = line[colon + 1..].trim_start();
    let scan = &rest.as_bytes()[..rest.len().min(MAX_TYPE_LEN + 1)];
    let len = scan
        .iter()
        .position(|&b| {
            !(b.is_ascii_alphanumeric()
                || matches!(b, b'_' | b':' | b'<' | b'>' | b'&' | b'[' | b']' | b'*'))
        })
        .unwrap_or(scan.len());
    if len == 0 || len > MAX_TYPE_LEN {
        return false;
    }
    let ty = &rest[..len];
    let after = rest[len..].trim_start();
    // Spaced: `QWxhZGRpbg=`, a base64 value's own padding, is glued to it.
    let spaced = after.len() < rest.len() - len;
    let assigned = spaced && after.starts_with('=') && !after[1..].starts_with(['=', '>']);
    let named_type = after.starts_with([')', ',', ';', '{'])
        && ty.starts_with(|c: char| c.is_ascii_uppercase())
        && reads_as_words(ty, 1);
    assigned || named_type
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
    if let Some((name, span, keep)) = detections(line).next() {
        return Some(SecretMatch {
            name,
            redacted: redact(&line[span], keep),
        });
    }
    // Nothing on the original line. A secret split across literals exists
    // only in the joined copy, and only when the match itself crosses.
    let joined = join_quoted_literals(line)?;
    let boundary = |p: usize| joined.is_piece_start(p);
    detections_at(&joined.shadow, &boundary)
        .find(|(_, span, _)| joined.crosses(span))
        .map(|(name, span, keep)| SecretMatch {
            name,
            redacted: redact(&joined.shadow[span], keep),
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
    redact_text(text, None)
}

/// What [`redact_secrets_report`] answers: the redacted text and an exact
/// account of what was replaced in it.
///
/// `non_exhaustive`: fields may be added later without breaking a vendored copy.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct RedactionReport {
    /// Exactly [`redact_secrets`]`(text)`, byte for byte.
    pub text: String,
    /// Replacements made, counting every merged span of every pass. A line
    /// that never converged and was redacted whole counts that whole-line
    /// replacement as one more span.
    pub spans: usize,
    /// Every vendor shape a detection matched, sorted and de-duplicated. A
    /// shape whose match overlapped another's, and so shared its span, is
    /// still named.
    pub kinds: Vec<&'static str>,
}

/// [`redact_secrets`] that also reports how many spans it replaced and which
/// shapes it matched, where [`find_secret`] names only the first shape on a
/// line. Additive: the output text is identical to `redact_secrets`.
pub fn redact_secrets_report(text: &str) -> RedactionReport {
    let mut tally = Tally::default();
    let text = redact_text(text, Some(&mut tally));
    RedactionReport {
        text,
        spans: tally.spans,
        kinds: tally.kinds.into_iter().collect(),
    }
}

/// What a report accumulates while a redaction runs.
#[derive(Default)]
struct Tally {
    spans: usize,
    kinds: std::collections::BTreeSet<&'static str>,
}

fn redact_text(text: &str, mut tally: Option<&mut Tally>) -> String {
    // One line at a time, exactly as `contains_secret` reads. Every detector
    // is a one-line rule; run over a whole text, one line's `${` made the next
    // line's password look like template syntax, so the password was found by
    // `contains_secret` and left in place here.
    let mut out = String::with_capacity(text.len());
    for segment in text.split_inclusive('\n') {
        let line = segment.trim_suffix('\n');
        let line = line.trim_suffix('\r');
        out.push_str(&redact_line(line, tally.as_deref_mut()));
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
fn redact_line(line: &str, mut tally: Option<&mut Tally>) -> String {
    let mut text = redact_pass(line, tally.as_deref_mut());
    if text == line {
        return text;
    }
    for _ in 1..MAX_REDACTION_PASSES {
        let next = redact_pass(&text, tally.as_deref_mut());
        if next == text {
            return text;
        }
        text = next;
    }
    if let Some(tally) = tally {
        tally.spans += 1;
    }
    redact(line, 0)
}

/// One redaction pass over a line: every detection, overlaps merged.
///
/// Detections are found on the line as written and, when the line holds a
/// same-line join, on the joined copy (only matches that cross pieces). They
/// are merged in line coordinates exactly as a line with no join is, and a
/// merged span's marker is rendered from the whole merged text, so a line
/// with no join comes out byte for byte as it always did.
fn redact_pass(text: &str, mut tally: Option<&mut Tally>) -> String {
    // (span in `text`, bytes kept, found on the joined copy)
    let mut hits: Vec<(std::ops::Range<usize>, usize, bool)> = Vec::new();
    for (name, span, keep) in detections(text) {
        if let Some(tally) = tally.as_deref_mut() {
            tally.kinds.insert(name);
        }
        hits.push((span, keep, false));
    }
    let joined = join_quoted_literals(text);
    if let Some(joined) = &joined {
        let boundary = |p: usize| joined.is_piece_start(p);
        for (name, span, keep) in detections_at(&joined.shadow, &boundary) {
            if !joined.crosses(&span) {
                continue;
            }
            if let Some(tally) = tally.as_deref_mut() {
                tally.kinds.insert(name);
            }
            match joined.orig_range(&span) {
                Some(orig) if orig.end <= text.len() => hits.push((orig, keep, true)),
                // A span that cannot be placed on the line is still a
                // credential: redact the line whole rather than leave it.
                _ => {
                    if let Some(tally) = tally {
                        tally.spans += 1;
                    }
                    return redact(text, 0);
                }
            }
        }
    }
    // Preserve byte offsets until every family has been checked. Sorting and
    // merging overlaps keeps the most specific prefix at a shared start and
    // prevents replacement text from being interpreted as another credential.
    // The prefix is chosen before the length: a vendor key and a context match
    // that start together merge into the longer span, and that span still
    // shows the vendor's prefix, which says what leaked and nothing more.
    hits.sort_by_key(|(span, keep, _)| {
        (
            span.start,
            std::cmp::Reverse(*keep),
            std::cmp::Reverse(span.end),
        )
    });
    let mut merged: Vec<(std::ops::Range<usize>, usize, bool)> = Vec::new();
    for (span, keep, on_copy) in hits {
        if let Some((previous, _, previous_on_copy)) = merged.last_mut()
            && span.start < previous.end
        {
            previous.end = previous.end.max(span.end);
            *previous_on_copy |= on_copy;
            continue;
        }
        merged.push((span, keep, on_copy));
    }
    if let Some(tally) = tally {
        tally.spans += merged.len();
    }
    let mut out = String::with_capacity(text.len());
    let mut copied = 0;
    for (span, keep, on_copy) in merged {
        // Widened, never narrowed, to character boundaries: a bad span may
        // redact too much of a line, and cannot leave part of one in the clear.
        let mut start = span.start.min(text.len());
        while !text.is_char_boundary(start) {
            start -= 1;
        }
        let mut end = span.end.min(text.len());
        while !text.is_char_boundary(end) {
            end += 1;
        }
        let start = start.max(copied);
        out.push_str(&text[copied..start]);
        match &joined {
            Some(joined) if on_copy => out.push_str(&redact(&joined.joined_text(start, end), keep)),
            _ => out.push_str(&redact(&text[start..end], keep)),
        }
        copied = end;
    }
    out.push_str(&text[copied..]);
    out
}

/// Reports whether `text` carries anything the secret gate would stop.
///
/// Callers that must *refuse* rather than redact use this: a value that cannot
/// be safely stored is not the same as one that was stored redacted.
pub fn contains_secret(text: &str) -> bool {
    text.lines().any(|line| {
        detections(line).next().is_some()
            || join_quoted_literals(line).is_some_and(|joined| {
                let boundary = |p: usize| joined.is_piece_start(p);
                detections_at(&joined.shadow, &boundary).any(|(_, span, _)| joined.crosses(&span))
            })
    })
}

/// redact keeps the identifying prefix and hides the rest.
fn redact(token: &str, keep: usize) -> String {
    let keep = keep.min(token.len());
    format!("{}… ({} chars)", &token[..keep], token.len())
}

#[cfg(test)]
mod redaction_tests {
    use super::*;

    /// A fixture written so that its source line is not a credential:
    /// `masked` is the value with a `¦` placed where it breaks the shape a
    /// detector reads — a key's separator, a URL's `://`, a client's name, a
    /// JWT's `eyJ` — and the value under test is `masked` without it. See
    /// `no_line_of_src_is_flagged_by_find_secret`. The string is leaked so a
    /// fixture is a `&'static str` like the literal it stands for.
    fn fixture(masked: &str) -> &'static str {
        assert!(
            masked.contains('¦'),
            "a fixture must carry its mask: {masked}"
        );
        Box::leak(masked.replace('¦', "").into_boxed_str())
    }

    /// A GitHub classic token shape, split so that no source line is one (see
    /// `no_line_of_this_file_is_itself_a_vendor_token`).
    const GITHUB_TOKEN: &str = concat!("ghp_", "0123456789abcdefghijklmnopqrstuvwxyzA");

    #[test]
    fn report_counts_every_span_and_names_every_shape_on_one_line() {
        let aws = concat!("AK", "IAQYPD7W3KXN2MZB5T");
        let text = format!("A={GITHUB_TOKEN} B={aws}\nplain\nC={GITHUB_TOKEN}\n");
        let report = redact_secrets_report(&text);
        assert_eq!(report.text, redact_secrets(&text));
        assert_eq!(report.spans, 3, "{}", report.text);
        assert_eq!(
            report.kinds,
            vec!["aws access key id", "github personal access token"]
        );
        let clean = redact_secrets_report("nothing here\n");
        assert_eq!((clean.spans, clean.kinds.len()), (0, 0));
    }

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

    /// Google's OAuth tokens, in the shapes Google's own documentation prints
    /// them, with `FAKE` bodies of the same length and alphabet. A CLI that
    /// talks to Google — `gcloud`, `firebase` — can echo one in an error or a
    /// debug line, and with no JSON key beside it nothing named it a
    /// credential.
    #[test]
    fn google_oauth_tokens_are_found_and_redacted() {
        let access = concat!(
            "ya",
            "29.FAKE6fakeT-okenNotReal0123_abcDEF_ghIJ01k85nHq39HE3C2LTrFAKE"
        );
        let downscoped = concat!("ya", "29.dr.FAKEfak-123456789abcdefghijKLMNOP");
        let refresh = concat!("1/", "/FAKEfakeRefreshTokenNotReal01k-259FAKEfake");
        for (token, name) in [
            (access, "google oauth access token"),
            (downscoped, "google oauth access token"),
            (refresh, "google oauth refresh token"),
        ] {
            for line in [
                token.to_string(),
                format!("Authorization: Bearer {token}"),
                format!("refresh={token}; next"),
                format!("token ({token})."),
            ] {
                assert_eq!(find_secret(&line).map(|m| m.name), Some(name), "{line}");
                let out = redact_secrets(&line);
                assert!(!out.contains(token), "{out}");
            }
        }
    }

    /// `1//` is a short prefix, and a path or URL contains it: the refresh
    /// token's body is letters, digits, `-` and `_`, so a run with a `/` or a
    /// `.` in it is not one, and neither is anything under the floor.
    #[test]
    fn a_double_slash_path_is_not_a_google_refresh_token() {
        for ordinary in [
            "curl http://127.0.0.1//api/v1/projects/scholarlm/backends/list?page=2",
            "gs://bucket-1//some/long/object/key/that/keeps/going/for/a/while",
            "x = n1//2 + 1//3",
            "print(total // 1//len(items_in_the_very_long_named_collection))",
            concat!("id 1/", "/0f8fad5b-d9cb-469f-a165-70867728950e"),
            "file:///Users/me/1//Library/Application Support/Something/Longer",
        ] {
            assert_eq!(find_secret(ordinary), None, "{ordinary}");
            assert_eq!(redact_secrets(ordinary), ordinary, "{ordinary}");
        }
    }

    /// Each context detector on the literal it exists for, and on the
    /// reference to a credential that must not be mistaken for one.
    #[test]
    fn context_detectors_find_literals_and_leave_references_alone() {
        for (line, name) in [
            (
                fixture(r#"password¦ = "Tr0ub4dor&3xample""#),
                "credential assigned to a named key",
            ),
            (
                fixture(r#"password¦ = "pässwörd""#),
                "credential assigned to a named key",
            ),
            (
                fixture("PGPASSWORD¦=xv9Lq2Rt7pW3 psql -h db"),
                "credential assigned to a named key",
            ),
            (
                fixture(r#"const ADMIN_PASSWORD: &str ¦= "Wint3r!sComing2026";"#),
                "credential assigned to a named key",
            ),
            (
                fixture(r#"val apiToken: String ¦= "a7Hk29LmQp3ZxW8vRt5N""#),
                "credential assigned to a named key",
            ),
            (
                fixture(r#"  "apiKey"¦: "AbC123dEf456GhI789jKl0","#),
                "credential assigned to a named key",
            ),
            (
                fixture("discord_token¦: MTIzNDU2Nzg5MDEy.GhIjKl.MnOpQrStUvWxYz012345"),
                "credential assigned to a named key",
            ),
            (
                fixture("DATABASE_URL=postgres:¦//app:s3cr3tPassw0rd@db:5432/app"),
                "password in a url",
            ),
            (
                fixture(r#"{"Authorization": "Bear¦er 9f8e7d6c5b4a39281706f5e4d3c2b1a0"}"#),
                "http authorization credential",
            ),
            (
                fixture(r#"auth = "Bas¦ic dXNlcjpwYXNzd29yZA==""#),
                "http authorization credential",
            ),
            (
                fixture("eyJhbGciOiJIUzI1NiJ9.ey¦JzdWIiOiIxMjM0NTY3ODkwIn0.dozjgNryP4J3jVmNHl0w"),
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
            fixture("mys¦ql -u root -pS3cretRootPw appdb"),
            fixture("mysqld¦ump --user=app -pDumpP4ss2026 shop > shop.sql"),
            fixture("/usr/bin/mari¦adb -h db -pM4riaPassw0rd"),
            fixture("sshp¦ass -p Tr0ub4dor3x ssh deploy@host"),
            fixture("psql --password¦=xv9Lq2Rt7pW3 -h db"),
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
            fixture(r#"aws_secret_access_key¦ = "wJalrXUtnFEMI/K7MDENG/bPxRfiCYEXAMPLEKEY""#),
            concat!(
                r#"WEBHOOK = "https://hooks.slack.com/services/"#,
                r#"T00000000/B00000000/XXXXXXXXXXXXXXXXXXXXXXXX""#
            ),
            fixture("postgres:¦//app:hunter2@prod-db.internal:5432/app"),
            fixture("amqp:¦//app:z9@mq:5672/"),
        ] {
            assert!(find_secret(line).is_some(), "{line}");
        }
    }

    #[test]
    fn context_matches_redact_their_value_and_redaction_is_idempotent() {
        for (line, value) in [
            (
                fixture(r#"password¦ = "Tr0ub4dor&3xample""#),
                "Tr0ub4dor&3xample",
            ),
            (
                fixture("DB_PASSWORD¦=correcthorsebatterystaple"),
                "correcthorsebatterystaple",
            ),
            (
                fixture("redis:¦//:9xYzQ8wVu7tS6rQ5@cache:6379/0"),
                "9xYzQ8wVu7tS6rQ5",
            ),
            (
                fixture("Authorization: Tok¦en 9944b09199c62bcf9418ad846dd0e4bbdfc6ee4b"),
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
            fixture(r#"password¦ = "Tr0ub4dor&3xample""#),
            fixture("PGPASSWORD¦=xv9Lq2Rt7pW3 psql"),
            r#"const K: &str = "Wint3r!sComing2026";"#,
            fixture("redis:¦//:9xYzQ8wVu7tS6rQ5@cache:6379/0"),
            fixture("Authorization: Bas¦ic dXNlcjpwYXNzd29yZA=="),
            fixture("eyJhbGciOiJIUzI1NiJ9.ey¦JzdWIiOiIxMjM0NTY3ODkwIn0.dozjgNryP4J3jVmNHl0w"),
            "api_key: ${API_KEY:?missing}",
            concat!("-----BEGIN RSA PRIVATE", " KEY-----"),
            fixture("mys¦ql -u root -p\"S3cretRootPw\" db | sshp¦ass -p Tr0ub4dor3x ssh h"),
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
            fixture("run:\n  - echo ${\n  - PGPASSWORD¦=xv9Lq2Rt7pW3 psql"),
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
            (
                fixture("api_key¦: Ab1234567890Xy,zzzz"),
                vec!["Ab12", "zzzz"],
                "",
            ),
            (
                fixture("{api_key¦: Ab1234567890Xy, next: 1}"),
                vec!["Ab1234567890Xy"],
                ", next: 1}",
            ),
            (
                fixture("mys¦ql -u root -pS3cr3t\"x y\" appdb"),
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
    /// The textbook bearer example, `abc123def456ghi789` after the scheme, in a comment is *not* here: the
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
            fixture("secret¦: Tr0ub4dor-x9q"),
            fixture("token¦: v1.Xk29LmQp3ZxW8vRt5N"),
            fixture("password¦: Summer2024!x"),
            fixture("auth¦: dXNlcjpwYXNzd29yZDEyMw=="),
            fixture("DB_PASSWORD¦=correcthorsebatterystaple"),
            fixture("// Bearer token format: Bear¦er abc123def456ghi789"),
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
                    .any(|p| p.spans(line, &|_| false).next().is_some())
            })
            .map(|(i, _)| i + 1)
            .collect();
        assert!(
            offenders.is_empty(),
            "rust/dc-redact/src/lib.rs lines {offenders:?} carry a vendor token as written; \
             split each fixture with concat! so the source line does not"
        );
    }

    /// No line of this crate's source is a credential by its own rules — not
    /// only no vendor token, which the test above holds, but no assignment,
    /// URL password, authorization header, JWT or command-line password
    /// either. GitPulse vendors `src/` and its pre-push scan runs
    /// [`find_secret`] on every line, so a fixture written as a literal made
    /// it refuse its own branch. Every `src/` file is read, so a new module is
    /// held to this too. Only line numbers are reported, never the line.
    #[test]
    fn no_line_of_src_is_flagged_by_find_secret() {
        fn walk(dir: &std::path::Path, out: &mut Vec<std::path::PathBuf>) {
            for entry in std::fs::read_dir(dir).expect("read src") {
                let path = entry.expect("dir entry").path();
                if path.is_dir() {
                    walk(&path, out);
                } else if path.extension().is_some_and(|e| e == "rs") {
                    out.push(path);
                }
            }
        }
        let src = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
        let mut files = Vec::new();
        walk(&src, &mut files);
        assert!(!files.is_empty(), "no source files under {}", src.display());
        let mut offenders = Vec::new();
        for file in &files {
            let text = std::fs::read_to_string(file).expect("read source file");
            for (i, line) in text.lines().enumerate() {
                if find_secret(line).is_some() {
                    offenders.push(format!(
                        "{}:{}",
                        file.strip_prefix(&src).unwrap_or(file).display(),
                        i + 1
                    ));
                }
            }
        }
        assert!(
            offenders.is_empty(),
            "{} line(s) of dc-redact/src are flagged by find_secret: {offenders:?}; \
             write each fixture through `fixture(...)` with a `¦` breaking its shape",
            offenders.len()
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
        let shapes: [(&str, &str); 23] = [
            ("type annotation", "fn f(token: Duration) "),
            ("incoming email", "incoming+a-1-"),
            ("incoming email at", "incoming+a-1-b@"),
            ("slack legacy", "xoxo-1-"),
            ("slack cookie", "xoxd-%"),
            ("stripe", "sk_live_"),
            ("bedrock short", fixture("bedrock-api¦-key-")),
            ("bedrock long", "ABSK"),
            ("brace slots", "https://u:{a"),
            ("glued client", fixture("x=cu¦rl -u a:Xk29LmQp3Zw ")),
            ("prefix operator", "token=++latest "),
            ("jwt dots", "eyJ."),
            ("jwt dashes", "-eyJ"),
            ("url userinfo", "a://"),
            ("auth scheme", "Bearer "),
            ("pem header", "-----BEGIN "),
            ("nested prefix", "ghp_"),
            ("cli password", "mysql -px "),
            ("template", fixture("${password¦=Ab12345678 ")),
            ("quoted keys", "\"a\":\""),
            ("assignment", "token=\""),
            ("separators", "a:1,"),
            ("commas", fixture("api_key¦: Ab1234567890Xy,")),
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
        let first = fixture("eyJhbGciOiJIUzI1.ey¦JzdWIiOiIxMjM0.c2lnbmF0dXJlMQ");
        let second = fixture("eyJ0eXAiOiJKV1Qi.ey¦Jpc3MiOiJhY21l.c2lnbmF0dXJlMg");
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
            fixture(r#"const open = "{{"; const password¦ = "Xq9vB2mL7pZ4";"#),
            fixture(r#"echo "${" ; DB_PASSWORD¦=Xq9vB2mL7pZ4"#),
            fixture(r#"tpl = "${x"; api_key¦ = "Xq9vB2mL7pZ4""#),
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
        for line in [
            fixture(r#"password¦ = "x>y2Zq81!mN""#),
            fixture(r#"password¦ = "Zq81!mN<y2x""#),
        ] {
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

    /// `prefix` and `body` joined at run time, so no source line of this file
    /// is itself a vendor token.
    fn token(prefix: &[&str], body: &str) -> String {
        let mut t = prefix.concat();
        t.push_str(body);
        t
    }

    /// GitLab's own secret-detection rules give every classic token a
    /// 20-character body, so `gldt-`, `glft-` and `glrt-` tokens are 25
    /// characters long. The table's floor of 26 passed each of them whole.
    #[test]
    fn gitlab_tokens_with_a_twenty_character_body_are_found() {
        const BODY: &str = "Xk29LmQp3ZwR7tY1uV5n";
        for (prefix, name) in [
            (["gl", "dt-"], "gitlab deploy token"),
            (["gl", "ft-"], "gitlab feed token"),
            (["gl", "rt-"], "gitlab runner authentication token"),
            (["gl", "wt-"], "gitlab workspace token"),
            (["gl", "pat-"], "gitlab personal access token"),
        ] {
            let t = token(&prefix, BODY);
            assert_eq!(find_secret(&t).map(|m| m.name), Some(name), "{name}");
            let line = format!("export TOKEN={t}");
            assert!(!redact_secrets(&line).contains(BODY), "{name}");
            // One character short of GitLab's shortest body is not a token.
            assert_eq!(find_secret(&t[..t.len() - 1]), None, "{name}");
        }
        // The partitioned runner token carries `t1_`..`t3_` before its body.
        let t = token(&["gl", "rt-", "t3_"], BODY);
        assert_eq!(
            find_secret(&t).map(|m| m.name),
            Some("gitlab runner authentication token")
        );
    }

    /// Slack's token rotation issues refresh tokens `xoxe-1-…` and rotated
    /// access tokens `xoxe.xoxp-…`, `xoxe.xoxb-…` and `xoxe.xapp-…`. A refresh
    /// token passed whole; a rotated token was named by the `xoxp-` inside it.
    #[test]
    fn slack_rotation_tokens_are_found_and_named() {
        const BODY: &str = "1-A1b2C3d4E5f6G7h8I9j0K1l2M3n4O5p6Q7r8S9t0";
        for (prefix, name, shown) in [
            (["xo", "xe-"].as_slice(), "slack refresh token", "xoxe-"),
            (
                ["xo", "xe.xo", "xp-"].as_slice(),
                "slack rotated user token",
                "xoxe.xoxp-",
            ),
            (
                ["xo", "xe.xo", "xb-"].as_slice(),
                "slack rotated bot token",
                "xoxe.xoxb-",
            ),
            (
                ["xo", "xe.xa", "pp-"].as_slice(),
                "slack rotated app-level token",
                "xoxe.xapp-",
            ),
        ] {
            let t = token(prefix, BODY);
            let line = format!("refresh_token={t}");
            let found = find_secret(&line).expect(name);
            assert_eq!(found.name, name);
            assert!(found.redacted.starts_with(shown), "{name}");
            assert!(!redact_secrets(&line).contains(&BODY[10..]), "{name}");
        }
    }

    /// AWS's unique-ID prefixes for an STS service bearer token (`ABIA`) and
    /// a context-specific credential (`ACCA`), and the `A3T` key-id prefix
    /// secret scanners carry, beside `AKIA` and `ASIA`.
    #[test]
    fn aws_bearer_context_and_a3t_key_ids_are_found() {
        const BODY: &str = "Q3EGRTWQKYZXN7M2";
        for (prefix, name) in [
            (["AB", "IA"].as_slice(), "aws sts service bearer token id"),
            (
                ["AC", "CA"].as_slice(),
                "aws context-specific credential id",
            ),
            (["A3", "TX"].as_slice(), "aws access key id"),
            (["A3", "T7"].as_slice(), "aws access key id"),
        ] {
            let t = token(prefix, BODY);
            assert_eq!(t.len(), 20);
            assert_eq!(
                find_secret(&format!("id: {t}")).map(|m| m.name),
                Some(name),
                "{t}"
            );
        }
        // The body is upper-case letters and digits, as AWS writes every
        // key id: a mixed-case run that starts `A3T` is base64, not a key.
        for line in [
            token(&["A3", "T"], "xq9vB2mL7pZ4kR8sT1u"),
            token(&["AB", "IA"], "q3egrtwqkyzxn7m2"),
            token(&["ac", "ca"], BODY),
        ] {
            assert_eq!(find_secret(&line), None, "{line}");
        }
    }

    /// HuggingFace organisation tokens begin `api_org_`; `huggingface_hub`'s
    /// own login refuses a token with that prefix. The body is letters, so an
    /// identifier that starts the same way is not one.
    #[test]
    fn huggingface_organisation_tokens_are_found() {
        let t = token(&["api_", "org_"], "AbCdEfGhIjKlMnOpQrStUvWxYzAbCdEfGh");
        assert_eq!(
            find_secret(&format!("HF_TOKEN={t}")).map(|m| m.name),
            Some("huggingface organization token")
        );
        for line in [
            "fn api_org_settings_handler_for_the_tenant_admin_page() {",
            "let x = api_org_membership_lookup_table_v2.get(id);",
        ] {
            assert_eq!(find_secret(line), None, "{line}");
        }
    }

    /// `curl -u user:password` was already a detected shape, but only when
    /// the word was `curl` itself. A command recorded as `command=curl …`, a
    /// command substitution `$(curl …)`, or one inside a string literal hid
    /// the client's name, and the password after it passed.
    #[test]
    fn a_client_name_glued_to_punctuation_is_still_the_client() {
        for line in [
            fixture("command=cu¦rl --user alice:Xk29LmQp3Zw https://example.test"),
            fixture("out=$(cu¦rl -u alice:Xk29LmQp3Zw https://example.test)"),
            fixture(r#"run("cu¦rl -u alice:Xk29LmQp3Zw https://example.test")"#),
            fixture(r#"let cmd = "cu¦rl --user alice:Xk29LmQp3Zw";"#),
            fixture("`cu¦rl -u alice:Xk29LmQp3Zw https://example.test`"),
            fixture(r#"sh -c "mys¦ql -u root -pS3cretRootPw appdb""#),
            fixture("cmd=sshp¦ass -p Tr0ub4dor3x ssh deploy@host"),
        ] {
            assert_eq!(
                find_secret(line).map(|m| m.name),
                Some("password on a command line"),
                "{line}"
            );
            let out = redact_secrets(line);
            assert!(!out.contains("Xk29LmQp3Zw"), "{line} -> {out}");
            assert!(!out.contains("S3cretRootPw"), "{line} -> {out}");
            assert!(!out.contains("Tr0ub4dor3x"), "{line} -> {out}");
            assert_eq!(redact_secrets(&out), out, "not idempotent: {out}");
        }
        // The same rules as every other client: a reference, a prompt, a
        // value under the length floor and a placeholder are not passwords.
        for line in [
            r#"command=curl -u "$USER:$PASS" https://example.test"#,
            "command=curl -u alice https://example.test",
            "command=curl -u alice:opaque https://example.test",
            "out=$(curl -u alice:${TOKEN} https://example.test)",
            "command=curl -u alice:<your-password> https://example.test",
            "mode=curl-like retry -u 5",
            // This crate's own doc comment: the backtick ends the command.
            "///   - `curl -u user:password`, `--user user:password`, `-uuser:password`",
            "see `$(curl -u user:password)` and `mysql -ppassword`;",
        ] {
            assert_eq!(find_secret(line), None, "{line}");
        }
        // Shell punctuation inside a value is a character of the password:
        // a `.netrc` is not shell, and the rest is not the end of a command
        // until whitespace. Each is found and redacted whole.
        for (line, tail) in [
            (
                fixture("machine m login l pass¦word Ab1&2345678x"),
                "2345678x",
            ),
            (fixture("sshp¦ass -p Tr0ub4dor&3xample ssh host"), "3xample"),
            (fixture("mys¦ql -u root -pS3cr3t;Pw99 db"), "Pw99"),
            (fixture("dock¦er login -p Xk29Lm|Qp3Zw registry"), "Qp3Zw"),
            (fixture("psql --pass¦word Xk29Lm<Qp3Zw -h db"), "Qp3Zw"),
        ] {
            assert!(find_secret(line).is_some(), "{line}");
            assert!(!redact_secrets(line).contains(tail), "{line}");
        }
    }

    /// Real npm tokens are `npm_` and 36 letters and digits. With any key
    /// character admitted, a test function named `npm_…` was a token.
    #[test]
    fn an_npm_prefixed_identifier_is_not_an_npm_token() {
        for line in [
            "fn npm_scan_package_manifest_handles_scoped_names() {",
            "fn npm_scan_child_sees_no_tokens_and_a_pinned_config() {",
        ] {
            assert_eq!(find_secret(line), None, "{line}");
        }
        let t = token(&["np", "m_"], "A1b2C3d4E5f6G7h8I9j0K1l2M3n4O5p6Q7r8");
        assert_eq!(
            find_secret(&format!("//registry.npmjs.org/:_authToken={t}")).map(|m| m.name),
            Some("npm access token")
        );
    }

    /// A type annotation names a type, not a value: `MIN_TOKEN_INTERVAL:
    /// Duration =` read `Duration` as the literal assigned to a token.
    #[test]
    fn a_type_annotation_is_not_a_credential_value() {
        for line in [
            "const MIN_TOKEN_INTERVAL: Duration = Duration::from_millis(1);",
            "let token_ttl_floor: Duration = Duration::ZERO;",
            "fn wait(token_delay: Duration) {",
            "    password: Password,",
            "pub fn new(secret: Zeroizing, other: u8) -> Self {",
        ] {
            assert_eq!(find_secret(line), None, "{line}");
        }
        // A typed declaration with a literal still assigns one, a YAML value
        // that is a capitalised word is still a value, and so is a base64
        // value whose padding `=` is glued to it.
        for line in [
            fixture(r#"const ADMIN_PASSWORD: &str ¦= "Wint3r!sComing2026";"#),
            fixture(r#"let token: String ¦= "a7Hk29LmQp3ZxW8vRt5N".into();"#),
            fixture("password¦: Monkeyshines"),
            fixture("token¦: QWxhZGRpbjpvcGVuIHNlc2FtZQ="),
        ] {
            assert_eq!(
                find_secret(line).map(|m| m.name),
                Some("credential assigned to a named key"),
                "{line}"
            );
        }
    }

    /// A prefix operator applied to a name is an expression: `const token =
    /// ++latest;` assigns a counter, not a password.
    #[test]
    fn a_prefix_operator_on_a_name_is_an_expression() {
        for line in [
            "const token = ++latest;",
            "let token = --remaining;",
            "token = !isStale",
            "auth = &mutableConfig;",
            "secret = *borrowedSecret",
        ] {
            assert_eq!(find_secret(line), None, "{line}");
        }
        // Leading punctuation in a literal password is still a password, a
        // letters-only one included: only `++` and `--` make an expression.
        for line in [
            fixture("DB_PASSWORD¦=!Welcomeback"),
            fixture("sshp¦ass -p !Welcomeback ssh h"),
            fixture("token¦=+Correcthorse"),
            fixture("password¦=!Xk29LmQp3Z"),
            fixture("token¦ = -A1b2C3d4E5f6"),
            fixture("secret¦: +Zq81mNy2x7"),
        ] {
            assert!(find_secret(line).is_some(), "{line}");
        }
    }

    /// A single-brace template — Rust's and Python's `{}`, `{name}`,
    /// `{name:?}` — stands in for a value the program supplies, as `${…}` and
    /// `<…>` already did. In a URL's userinfo and after a credential-named key
    /// it was reported as the credential itself.
    #[test]
    fn a_brace_template_is_a_placeholder() {
        for line in [
            r#""leaked bearer token: {scrubbed}""#,
            r#"format!("password={password}")"#,
            r#"let url = format!("https://x:{token}@example.com/r");"#,
            r#"write!(f, "https://user:{}@host/path", self.user)"#,
            r#"f"https://{user}:{password}@db.internal:5432/app""#,
            r#"format!("https://user:{pw:?}@host")"#,
        ] {
            assert_eq!(find_secret(line), None, "{line}");
        }
        // Braces that do not form a slot are characters of the literal.
        for line in [
            fixture("postgres:¦//app:s3cr}t{Passw0rd@db:5432/app"),
            fixture("postgres:¦//app:{s3cr3t-Passw0rd!}@db:5432/app"),
            fixture(r#"password¦ = "Xq9{vB2mL7pZ4""#),
        ] {
            assert!(find_secret(line).is_some(), "{line}");
        }
    }

    /// `unit` repeated to exactly `len` characters: a long plant body.
    fn body_of(unit: &str, len: usize) -> String {
        unit.chars().cycle().take(len).collect()
    }

    /// Amazon Bedrock API keys. A short-term key is `bedrock-api-key-` and the
    /// base64 of a presigned URL on `bedrock.amazonaws.com`, then `&Version=1`
    /// (AWS's aws-bedrock-token-generator README). A long-term key is `ABSK`
    /// and 109 to 269 base64 characters (Kingfisher's rule; AWS's user guide
    /// states no format).
    #[test]
    fn aws_bedrock_api_keys_are_found() {
        let host = "YmVkcm9jay5hbWF6b25hd3MuY29t";
        let short = token(
            &["bedrock-", "api-key-", host],
            "Lz9BY3Rpb249Q2FsbFdpdGhCZWFyZXJUb2tlbiZYLUFtei1",
        );
        let long = token(
            &["AB", "SK"],
            &body_of("QmVkcm9ja0FQSUtleS1+Xk29Lm/Qp3Zw", 120),
        );
        for (line, name, tail) in [
            (
                format!("export AWS_BEARER_TOKEN_BEDROCK={short}&Version=1"),
                "aws bedrock short-term api key",
                "ZYLUFtei1",
            ),
            (
                format!("key: {}", token(&["bedrock-", "api-key-"], host)),
                "aws bedrock short-term api key",
                "hd3MuY29t",
            ),
            (
                format!("bedrock_key = \"{long}\""),
                "aws bedrock long-term api key",
                "Qp3Zw",
            ),
        ] {
            assert_eq!(find_secret(&line).map(|m| m.name), Some(name), "{name}");
            assert!(!redact_secrets(&line).contains(tail), "{name}");
        }
        for line in [
            "fn rotate_bedrock_api_key_for_the_staging_account() {",
            "let prefix = \"bedrock-api-key-\";",
            "bedrock-api-key-rotation-handler-for-the-staging-account-and-more",
            "const ABSKIP_MASK: u32 = 0x1;",
        ] {
            assert_eq!(find_secret(line), None, "{line}");
        }
    }

    /// Gemini's `AQ.` auth key, `AQ.Ab8RN6` and 44 URL-safe characters. This
    /// is Kingfisher's rule: Google's API-key pages state no format.
    #[test]
    fn google_gemini_auth_keys_are_found() {
        let key = token(&["AQ.", "Ab8RN6"], &body_of("Xk29LmQp3Zw_R7tY-1uV5n", 44));
        assert_eq!(key.len(), 53);
        let line = format!("GEMINI_API_KEY={key}");
        assert_eq!(
            find_secret(&line).map(|m| m.name),
            Some("google gemini api key")
        );
        assert!(!redact_secrets(&line).contains("R7tY-1uV5n"));
        for line in [
            concat!("AQ.Ab8", "RN6 is the prefix"),
            concat!(
                "see AQ.Ab8",
                "RN6/docs/path/that/runs/well/past/the/length/floor/here"
            ),
        ] {
            assert_eq!(find_secret(line), None, "{line}");
        }
    }

    /// GitLab's runner registration token (`GR1348941` and 20 characters, in
    /// GitLab's own secret-detection rules and Kingfisher's) and its session
    /// cookie (`_gitlab_session=` and 32 characters, Kingfisher's rule; the
    /// cell-routed form `cell-<n>-` and 64 hex is GitLab's).
    #[test]
    fn gitlab_registration_tokens_and_session_cookies_are_found() {
        let rrt = token(&["GR134", "8941"], "Xk29LmQp3Zw_R7tY-1uV");
        let cookie = token(
            &["_gitlab_", "session="],
            "a1b2c3d4e5f6a7b8c9d0e1f2a3b4c5d6",
        );
        let cell = token(
            &["_gitlab_", "session=", "cell-1-"],
            &body_of("0123456789abcdef", 64),
        );
        for (line, name, tail) in [
            (
                format!("REGISTRATION_TOKEN={rrt}"),
                "gitlab runner registration token",
                "R7tY-1uV",
            ),
            (
                format!("Cookie: {cookie}; path=/"),
                "gitlab session cookie",
                "c4d5e6a7b8",
            ),
            (
                format!("Cookie: {cell}"),
                "gitlab session cookie",
                "89abcdef0123",
            ),
        ] {
            assert_eq!(find_secret(&line).map(|m| m.name), Some(name), "{line}");
            assert!(!redact_secrets(&line).contains(tail), "{name}");
        }
        for line in [
            concat!("GR134", "8941 is the registration token prefix"),
            concat!("Cookie: _gitlab_", "session=${GITLAB_SESSION}"),
            concat!("x_gitlab_", "session=a1b2c3d4e5f6a7b8c9d0e1f2a3b4c5d6"),
        ] {
            assert_eq!(find_secret(line), None, "{line}");
        }
    }

    /// GitLab's incoming-email address carries a user's incoming-email token:
    /// `incoming+<project path>-<project id>-<token>-issue@…`, and the
    /// `-issue-<n>` and `-merge-request` forms (Kingfisher's rule).
    #[test]
    fn gitlab_incoming_email_address_tokens_are_found() {
        let tok = "a1b2c3d4e5f6g7h8i9j0k1l2m";
        for local in [
            format!("acme-web-42-{tok}-issue"),
            format!("42-{tok}-issue-7"),
            format!("acme.group-web-2024-42-{tok}-merge-request"),
        ] {
            let line = format!(
                "Reply-To: {}{local}@gitlab.example.com",
                ["incom", "ing+"].concat()
            );
            assert_eq!(
                find_secret(&line).map(|m| m.name),
                Some("gitlab incoming email token"),
                "{line}"
            );
            let out = redact_secrets(&line);
            assert!(!out.contains(tok), "{out}");
            assert!(out.ends_with("@gitlab.example.com"), "{out}");
        }
        for line in [
            "write to incoming+support@example.com for help",
            "incoming+acme-42-<your-token>-issue@gitlab.example.com",
            "incoming+acme-42-short-issue@gitlab.example.com",
            "the incoming+ address is shown in the project's issue list",
            "incoming+acme-web-42-a1b2c3d4e5f6g7h8i9j0-comment@example.com",
        ] {
            assert_eq!(find_secret(line), None, "{line}");
        }
    }

    /// Stripe's keys at any length Kingfisher's access-token rule admits:
    /// `sk_`/`rk_`, `test`/`live`/`prod`, and ten or more letters and digits.
    /// The floor of 24 passed a ten-character key, and `_prod_` had no entry.
    /// The body is base64's alphabet, so an `sk_test_…` identifier is not a
    /// key, and a key whose body has no digit still needs the old length.
    #[test]
    fn stripe_keys_of_any_admitted_length_are_found() {
        for (prefix, name) in [
            (["sk_", "test_"], "stripe test key"),
            (["sk_", "live_"], "stripe live key"),
            (["rk_", "test_"], "stripe restricted test key"),
            (["rk_", "live_"], "stripe restricted live key"),
            (["sk_", "prod_"], "stripe prod key"),
            (["rk_", "prod_"], "stripe restricted prod key"),
        ] {
            let line = format!("STRIPE_KEY={}", token(&prefix, "Xk29LmQp3Z"));
            assert_eq!(find_secret(&line).map(|m| m.name), Some(name), "{name}");
            assert!(!redact_secrets(&line).contains("LmQp3Z"), "{name}");
        }
        // A base64 body, as other vendors issue under `sk_live_`, still is one.
        let b64 = token(&["sk_", "live_"], &body_of("a2V5Xz+Xk29Lm/Qp3Zw=", 75));
        assert!(find_secret(&b64).is_some());
        for line in [
            "fn sk_test_helpers_module() {}",
            "let rk_live_dashboard_url = base();",
            "use crate::sk_prod_config::load;",
            "sk_test_fixture_loader_for_payments()",
            "sk_live_dashboardView",
        ] {
            assert_eq!(find_secret(line), None, "{line}");
        }
    }

    /// Slack's legacy tokens (`xoxo-`/`xoxs-` with three numeric parts and a
    /// hex secret; `xoxa-`/`xoxr-` workspace tokens), the browser session
    /// token `xoxc-` and the session cookie `xoxd-`, which is URL-encoded and
    /// carries `%` and `\`. All from Kingfisher's rules; Slack's token page
    /// names none of them.
    #[test]
    fn slack_legacy_and_session_tokens_are_found() {
        let hex64 = body_of("0123456789abcdef", 64);
        let cookie = token(&["xo", "xd-"], &body_of("Xk29Lm%2FQp3Zw\\R7tY-1uV5n+", 140));
        for (line, name, tail) in [
            (
                token(&["xo", "xs-"], "12345-67890-13579-0a1b2c3d4e5f"),
                "slack legacy token",
                "0a1b2c3d4e5f",
            ),
            (
                token(&["xo", "xo-"], "1-2-3-abcdef"),
                "slack legacy token",
                "-abcdef",
            ),
            (
                token(&["xo", "xr-"], "2-AbCd1234Ef"),
                "slack legacy workspace token",
                "1234Ef",
            ),
            (
                token(&["xo", "xa-"], "AbCd1234Ef"),
                "slack workspace token",
                "1234Ef",
            ),
            (
                token(
                    &["xo", "xc-"],
                    &format!("123456789-234567890-345678901-{hex64}"),
                ),
                "slack session token",
                "89abcdef0123",
            ),
            (
                format!("d={cookie}; Path=/"),
                "slack session cookie",
                "R7tY-1uV5n+",
            ),
        ] {
            assert_eq!(find_secret(&line).map(|m| m.name), Some(name), "{line}");
            assert!(!redact_secrets(&line).contains(tail), "{name}: {line}");
        }
        for line in [
            concat!("xo", "xo-hugs-from-the-whole-team-at-the-office"),
            concat!("XO", "XO-gossip-girl-1-2-3-abcdef"),
            concat!("the xo", "xs- prefix was retired"),
            concat!("xo", "xr-abc"),
            concat!("xo", "xc-12-34-56-abcdef"),
            concat!("xo", "xd-short%2Fcookie"),
        ] {
            assert_eq!(find_secret(line), None, "{line}");
        }
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
