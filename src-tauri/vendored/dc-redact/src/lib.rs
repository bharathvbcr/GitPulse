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
    SECRET_PATTERNS.iter().find_map(|pattern| {
        pattern.spans(line).next().map(|span| SecretMatch {
            name: pattern.name,
            redacted: redact(&line[span], pattern.prefix.len()),
        })
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
    let mut spans: Vec<_> = SECRET_PATTERNS
        .iter()
        .flat_map(|pattern| pattern.spans(text).map(|span| (span, pattern.prefix.len())))
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
    text.lines().any(|line| {
        SECRET_PATTERNS
            .iter()
            .any(|p| p.spans(line).next().is_some())
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

    #[test]
    fn redaction_output_carries_no_secret_of_its_own() {
        let key = "ghp_0123456789abcdefghijklmnopqrstuvwxyzA";
        let once = redact_secrets(key);
        assert_eq!(redact_secrets(&once), once);
        assert!(!contains_secret(&once));
    }
}
