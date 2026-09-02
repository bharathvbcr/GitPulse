//! Rigor gates: the checks that read the *content* of a diff rather than its
//! shape.
//!
//! Scope classification answers "did this task change files it was allowed to
//! change". These answer the harder question: "is what it wrote actually the
//! work". They are the three gates the Go verifier previously listed in its
//! degraded set, which is why a passing verification could not be trusted to
//! mean very much.
//!
//! Every finding carries a file, a line, and the text that triggered it, so a
//! report is actionable rather than a verdict. And every gate is deliberately
//! conservative about what it flags: a rigor check with a high false-positive
//! rate gets turned off, and a gate nobody runs protects nothing.

use crate::FileDiff;

/// How serious a finding is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Severity {
    /// Blocks the task. Reserved for findings where shipping is clearly wrong.
    Blocking,
    /// Reported and does not block.
    Advisory,
}

/// One thing a gate found.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Finding {
    pub gate: &'static str,
    pub severity: Severity,
    pub path: String,
    pub line: u32,
    /// What triggered it. Truncated, and never the full line for a secret
    /// finding — see `redact`.
    pub evidence: String,
    pub message: String,
}

/// Placeholder markers. Matched case-insensitively against added lines only:
/// an existing TODO in untouched code is somebody else's decision, and
/// flagging it would make every diff in a legacy file fail.
const STUB_MARKERS: &[&str] = &[
    "todo",
    "fixme",
    "xxx:",
    "hack:",
    "not implemented",
    "notimplemented",
    "unimplemented",
    "placeholder",
    "for now",
    "stub",
];

/// Bodies that do nothing, in the languages this repo builds.
const EMPTY_BODIES: &[&str] = &[
    "todo!()",
    "unimplemented!()",
    "panic!(\"todo\")",
    "raise notimplementederror",
    "pass  # todo",
];

/// Runs the stub and effort heuristics over a diff.
///
/// Only added lines are read. A gate that also read removed lines would flag a
/// diff for *deleting* a TODO, which is the opposite of what it is for.
pub fn detect_stubs(files: &[FileDiff]) -> Vec<Finding> {
    let mut findings = Vec::new();
    for file in files {
        for (line_no, content) in &file.added_lines {
            let lowered = content.to_ascii_lowercase();
            let trimmed = lowered.trim();

            if let Some(marker) = EMPTY_BODIES.iter().find(|m| trimmed.contains(**m)) {
                findings.push(Finding {
                    gate: "stub_detection",
                    severity: Severity::Blocking,
                    path: file.path.clone(),
                    line: *line_no,
                    evidence: safe_evidence(content),
                    message: format!(
                        "added code whose body is `{marker}`; the task is not implemented"
                    ),
                });
                continue;
            }

            // Comment markers are advisory. A TODO in a new file is often a
            // legitimate note about future work, and blocking on it would make
            // the gate something people route around.
            if let Some(marker) = STUB_MARKERS.iter().find(|m| trimmed.contains(**m)) {
                if !is_comment_or_string(trimmed) {
                    continue;
                }
                findings.push(Finding {
                    gate: "stub_detection",
                    severity: Severity::Advisory,
                    path: file.path.clone(),
                    line: *line_no,
                    evidence: safe_evidence(content),
                    message: format!("added a `{marker}` marker"),
                });
            }
        }
    }
    findings
}

/// Reports whether a line looks like a comment or a string literal, which is
/// where a placeholder marker means something.
///
/// The alternative — flagging the marker anywhere — makes an identifier such as
/// `todoItems` or a legitimate `stubServer` into a finding, and a gate that
/// fires on ordinary names is a gate that gets disabled.
fn is_comment_or_string(trimmed: &str) -> bool {
    trimmed.starts_with("//")
        || trimmed.starts_with('#')
        || trimmed.starts_with("/*")
        || trimmed.starts_with('*')
        || trimmed.starts_with("--")
        || trimmed.starts_with("<!--")
        || trimmed.contains("\" todo")
        || trimmed.contains("// todo")
}

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
    /// Returns the token this pattern matches in `content`, if it does.
    ///
    /// The single seam through which every consumer — the secret gate, and
    /// the evidence builder every other gate routes its quotes through —
    /// decides what counts as a credential. Two shapes share one rule set or
    /// they disagree about where secrets are, which is how a key redacted by
    /// one gate leaks out of another's evidence field.
    fn matches(&self, content: &str) -> Option<String> {
        // The prefix has to begin a token, not merely appear inside one. Without
        // this, `sk-` matched the middle of `task-`, `disk-` and `risk-`, and a
        // long enough kebab-case identifier would have been reported as an
        // OpenAI key — the false positive that gets a secret gate switched off.
        let start = content
            .match_indices(self.prefix)
            .map(|(at, _)| at)
            .find(|at| starts_a_token(content, *at))?;
        let token: String = content[start..]
            .chars()
            .take_while(|c| !c.is_whitespace() && *c != '"' && *c != '\'' && *c != ',')
            .collect();
        if self.alphanumeric_body
            && !token[self.prefix.len()..]
                .chars()
                .all(|c| c.is_ascii_alphanumeric())
        {
            return None;
        }
        // A PEM block is identified by its full header, not by the dashes
        // alone: certificates and public keys are ordinary trust-store
        // content, and blocking them taught operators to wave findings
        // through.
        if self.prefix == "-----BEGIN" {
            let upper = content.to_ascii_lowercase();
            if !upper.contains("private key") {
                return None;
            }
        }
        if token.len() < self.min_len && !self.prefix.starts_with("-----") {
            return None;
        }
        Some(token)
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

/// Scans added lines for credential shapes.
///
/// This is the gate whose *absence* mattered most. The write gate refuses to
/// write to `.env`, but nothing stopped a key from being pasted into an
/// ordinary source file — and a credential committed to history is compromised
/// even after it is deleted, because the object stays in the repository.
pub fn scan_secrets(files: &[FileDiff]) -> Vec<Finding> {
    let mut findings = Vec::new();
    for file in files {
        for (line_no, content) in &file.added_lines {
            for pattern in SECRET_PATTERNS {
                let Some(token) = pattern.matches(content) else {
                    continue;
                };
                findings.push(Finding {
                    gate: "secret_scan",
                    severity: Severity::Blocking,
                    path: file.path.clone(),
                    line: *line_no,
                    // The finding names the shape and shows only the prefix.
                    // A report that quotes the key in full copies the secret
                    // into the evidence trail, the terminal, and the session
                    // log — which is the leak this gate exists to prevent.
                    evidence: redact(&token, pattern.prefix.len()),
                    message: format!(
                        "added line contains what looks like a {}; a credential in a commit is \
                         compromised even after it is deleted, because the object stays in history",
                        pattern.name
                    ),
                });
                break;
            }
        }
    }
    findings
}

/// Builds the evidence text a non-secret finding may quote from an added line.
///
/// Every evidence field outside `scan_secrets` is built through here, so a
/// line carrying both a stub marker and a credential cannot leak the
/// credential through a gate that does not itself look for credentials — which
/// is exactly how `// TODO remove before merge sk-ant-…` once reached the
/// report verbatim while the secret gate beside it showed only `sk-ant-…`.
fn safe_evidence(line: &str) -> String {
    let trimmed = line.trim();
    for pattern in SECRET_PATTERNS {
        if pattern.matches(trimmed).is_some() {
            return format!("<contains a {}; quoted text withheld>", pattern.name);
        }
    }
    truncate(trimmed, 120)
}

/// Redacts every credential-shaped token in arbitrary text.
///
/// `scan_secrets` and `safe_evidence` both answer "is there a credential in
/// this *diff line*". This answers the same question for text that never came
/// from a diff at all — a command line, a log payload, a ledger event — and it
/// answers it through `SECRET_PATTERNS`, deliberately, because that table is
/// documented as the single seam through which every consumer decides what
/// counts as a credential.
///
/// GitPulse's action ledger is the caller this exists for. It redacts at write
/// time rather than at display time: display-time redaction protects the screen
/// and nothing else, because the secret is already on disk in a file that gets
/// backed up, synced, and read by every later consumer including ones that do
/// not know to redact.
///
/// Each match is replaced with the same prefix-preserving rendering the secret
/// gate reports, so an operator comparing a finding to a ledger row sees the
/// same string in both.
///
/// A token is replaced wherever it appears, not only at its first occurrence:
/// an argv that repeats a key twice must not have one copy survive.
pub fn redact_secrets(text: &str) -> String {
    // Collect first, mutate after. Replacing while scanning would shift the
    // offsets `matches` just computed, and a pattern matching inside the
    // replacement text would then redact the redaction.
    let mut tokens: Vec<(String, usize)> = Vec::new();
    for line in text.lines() {
        for pattern in SECRET_PATTERNS {
            if let Some(token) = pattern.matches(line) {
                tokens.push((token, pattern.prefix.len()));
            }
        }
    }
    if tokens.is_empty() {
        return text.to_string();
    }
    // Longest first, so a short prefix cannot partially rewrite a longer token
    // that contains it and leave the tail in place.
    tokens.sort_by(|a, b| b.0.len().cmp(&a.0.len()));

    let mut out = text.to_string();
    for (token, keep) in tokens {
        if !out.contains(&token) {
            continue;
        }
        out = out.replace(&token, &redact(&token, keep));
    }
    out
}

/// Reports whether `text` carries anything the secret gate would stop.
///
/// Callers that must *refuse* rather than redact use this: a value that cannot
/// be safely stored is not the same as one that was stored redacted.
pub fn contains_secret(text: &str) -> bool {
    text.lines()
        .any(|line| SECRET_PATTERNS.iter().any(|p| p.matches(line).is_some()))
}

/// redact keeps the identifying prefix and hides the rest.
fn redact(token: &str, keep: usize) -> String {
    let keep = keep.min(token.len());
    format!("{}… ({} chars)", &token[..keep], token.len())
}

/// A file's test coverage, as a set of covered line numbers.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FileCoverage {
    pub path: String,
    pub covered_lines: Vec<u32>,
}

/// What the coverage intersection found for one file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CoverageGap {
    pub path: String,
    /// Added lines no test executed.
    pub uncovered_lines: Vec<u32>,
    pub added_lines: usize,
}

/// The result of intersecting a diff with coverage data.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct CoverageReport {
    pub gaps: Vec<CoverageGap>,
    /// Files that changed but for which no coverage data was supplied at all.
    ///
    /// This list is the reason the whole report exists in this shape. A file
    /// with no coverage data and a file with full coverage both produce zero
    /// gaps, and reporting them the same way is how "diff coverage passed"
    /// comes to mean "coverage was never measured".
    pub unmeasured: Vec<String>,
    /// Files the intersection did not ask a coverage question about, because
    /// their extension is not one this gate knows how to measure.
    ///
    /// The third bucket exists for the same reason as the second. A skipped
    /// file used to leave no trace at all — not a gap, not unmeasured — so a
    /// diff that touched only files outside the allowlist produced an
    /// all-clear that was indistinguishable from one whose every line was
    /// executed. It is reported rather than blocking: "coverage is not a
    /// question about this file" is a real answer, and it is only a safe one
    /// while it is written down.
    pub skipped_by_type: Vec<String>,
}

impl CoverageReport {
    /// Reports whether every added line was measured and executed.
    ///
    /// `skipped_by_type` is deliberately not part of this. A skipped file is
    /// not a failure of the change; it is a limit of the gate, and the report
    /// carries it so a reader can see which one they are looking at.
    pub fn is_clean(&self) -> bool {
        self.gaps.is_empty() && self.unmeasured.is_empty()
    }
}

/// Intersects added lines with covered lines.
///
/// Files whose extension marks them as non-executable — documentation, data,
/// configuration — are not asked the coverage question, because "coverage" is
/// not a question about a Markdown file. They are *recorded* in
/// `skipped_by_type` rather than dropped: a file that left the report with no
/// trace at all was reported exactly like one that was measured and clean.
pub fn intersect_coverage(files: &[FileDiff], coverage: &[FileCoverage]) -> CoverageReport {
    let mut report = CoverageReport::default();
    for file in files {
        if file.status == crate::ChangeStatus::Deleted {
            continue;
        }
        let added = file.added_line_numbers();
        if added.is_empty() {
            continue;
        }
        if !is_executable_source(&file.path) {
            report.skipped_by_type.push(file.path.clone());
            continue;
        }
        let Some(entry) = coverage.iter().find(|c| c.path == file.path) else {
            report.unmeasured.push(file.path.clone());
            continue;
        };
        // Binary search rather than a linear scan of every covered line for
        // every added line: `finish` in the coverage parser hands back sorted,
        // deduplicated lines, and the quadratic version turned a large profile
        // into a second source of the wall-clock blowup the parser's bounds
        // now prevent. A caller that built a FileCoverage by hand may not have
        // sorted it, so that is checked rather than assumed — a wrong answer
        // from a binary search over unsorted data would be a silently missed
        // coverage gap.
        let sorted: std::borrow::Cow<'_, [u32]> = if entry.covered_lines.is_sorted() {
            std::borrow::Cow::Borrowed(&entry.covered_lines)
        } else {
            let mut owned = entry.covered_lines.clone();
            owned.sort_unstable();
            std::borrow::Cow::Owned(owned)
        };
        let uncovered: Vec<u32> = added
            .iter()
            .copied()
            .filter(|line| sorted.binary_search(line).is_err())
            .collect();
        if !uncovered.is_empty() {
            report.gaps.push(CoverageGap {
                path: file.path.clone(),
                uncovered_lines: uncovered,
                added_lines: added.len(),
            });
        }
    }
    report.unmeasured.sort();
    report.skipped_by_type.sort();
    report.gaps.sort_by(|a, b| a.path.cmp(&b.path));
    report
}

/// Extensions whose files execute, and therefore can be covered.
///
/// The list is an allowlist and it is kept long on purpose. Every extension
/// missing from it used to be dropped from the report entirely, so a diff
/// adding `rm -rf "$TARGET"` to a `.sh` file, or an ES module to a `.mjs` one,
/// passed the coverage gate without being counted — while the identical
/// CommonJS `.js` file was measured. Shell, Kotlin, Swift, PHP and C# were in
/// the same hole. Nothing here is measured by every ecosystem's tooling, and a
/// file with no coverage data lands in `unmeasured`, which is the honest
/// answer: it executed, and nobody showed evidence that it ran.
fn is_executable_source(path: &str) -> bool {
    const EXECUTABLE: &[&str] = &[
        ".go", ".rs", ".py", ".ts", ".tsx", ".js", ".jsx", ".mjs", ".cjs", ".mts", ".cts", ".java",
        ".kt", ".kts", ".scala", ".swift", ".rb", ".php", ".cs", ".c", ".cc", ".cpp", ".h", ".hpp",
        ".sh", ".bash", ".zsh", ".ps1", ".pl", ".lua",
    ];
    // A test file's own lines are not the thing coverage is asking about.
    if path.ends_with("_test.go") || path.ends_with("_test.py") || path.contains("/tests/") {
        return false;
    }
    EXECUTABLE.iter().any(|ext| path.ends_with(ext))
}

fn truncate(text: &str, n: usize) -> String {
    if text.chars().count() <= n {
        return text.to_string();
    }
    let cut: String = text.chars().take(n).collect();
    format!("{cut}…")
}

#[cfg(test)]
mod redaction_tests {
    use super::*;

    /// The ledger's requirement: a credential pasted into a command line is
    /// never written to disk in full.
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
        // A redactor that rewrites innocent text gets switched off, which is
        // how the secret gate stops running at all.
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
        // The two must not disagree: anything reported as carrying a secret
        // must actually be changed by redaction, or a caller that trusts
        // `contains_secret` to gate storage would store it unchanged.
        assert_ne!(redact_secrets(key), key);
        assert_eq!(redact_secrets("cargo build"), "cargo build");
    }

    #[test]
    fn redaction_output_carries_no_secret_of_its_own() {
        // Idempotence. If redacting a redaction found something, the first
        // pass left a credential behind.
        let key = "ghp_0123456789abcdefghijklmnopqrstuvwxyzA";
        let once = redact_secrets(key);
        assert_eq!(redact_secrets(&once), once);
        assert!(!contains_secret(&once));
    }
}
