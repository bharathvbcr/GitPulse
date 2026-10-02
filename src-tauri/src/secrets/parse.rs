//! Allowlist parse of Kingfisher JSON so secret fields never enter the report.
//!
//! Two layers. [`parse_kingfisher_json`] reads Kingfisher's envelope into
//! [`Envelope`], which still carries Kingfisher's absolute paths and value
//! fingerprints and never crosses IPC. [`super::report`] turns that into the
//! [`SecretsReport`] the webview sees: repo-relative paths, a Git location per
//! finding, and an opaque group ordinal in place of the fingerprint.

use serde::{Deserialize, Serialize};

/// Where a finding sits relative to the scanned repository's Git state.
///
/// This is the question a reader actually has about a secret in the working
/// tree — "will this be committed?" — and Kingfisher cannot answer it: it
/// walks the filesystem, so committed source, build output and the
/// repository's own `.git/config` arrive as identical rows.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[serde(rename_all = "snake_case")]
pub enum SecretLocation {
    /// Inside a `.git` directory: remote URLs, credential helpers, hooks.
    /// Never committed, but read by every Git command and easy to forget.
    GitMetadata,
    /// In the index. Committed already, or will be with the next commit.
    Tracked,
    /// Not in the index and not ignored: the next `git add -A` commits it.
    Untracked,
    /// Under a nested repository or submodule, which the outer repository's
    /// index cannot speak for.
    NestedRepo,
    /// Matched by an ignore rule: build output, local env files, caches.
    Ignored,
    /// Kingfisher reported a path outside the scanned root.
    Outside,
    /// The Git listings could not be read, so the location was not decided.
    Unknown,
}

/// Whether Kingfisher said it read every input it found.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ScanCompleteness {
    /// Every repository entry in Kingfisher's audit reports `completed`.
    Complete,
    /// Kingfisher marked the scan partial or left inputs pending: at least
    /// one file could not be read. The findings are a floor.
    Partial,
    /// Kingfisher's output carried no audit Kingfisher could be held to
    /// (older releases), so completeness is unknown — never assumed.
    Unverified,
}

/// One allowlisted finding row for the Insights Secrets panel.
///
/// Nothing here is derived from the secret's value except `secret_group`,
/// an ordinal assigned in this process: equal ordinals mean Kingfisher's
/// redaction hash of the two matched values was equal in this run. Neither
/// that hash nor Kingfisher's fingerprint leaves Rust.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct SecretFinding {
    pub rule_id: String,
    /// Kingfisher's short rule name (`github-pat`); empty when absent.
    pub rule_name: String,
    /// Repo-relative with `/` separators, except for `outside` rows, which
    /// keep the path Kingfisher printed.
    pub path: String,
    /// 1-based; 0 when Kingfisher reported none.
    pub line: u32,
    /// `low`, `medium` or `high`; empty when Kingfisher reported none.
    pub confidence: String,
    pub location: SecretLocation,
    /// 1-based ordinal shared by findings of the same value (scoped to this
    /// scan and rule); 0 when Kingfisher gave no redaction hash.
    pub secret_group: u32,
}

/// Result of an on-demand Kingfisher secrets scan.
///
/// `ok: false` means the scan did not complete successfully — missing binary,
/// truncated stdout, bad JSON, timeout, a non-success exit, or a scan
/// Kingfisher itself reported as failed. That must never render as a clean
/// repository. `ok: true` with `completeness != complete` is a scan that ran
/// and missed inputs: its findings are real, and they are a floor.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct SecretsReport {
    pub ok: bool,
    /// Fixed reason string only. Never carries Kingfisher stdout or stderr.
    pub error: Option<String>,
    /// Copyable facts about a failed run: version, binary, jobs, deadline,
    /// elapsed, and how many bytes were captured. Never Kingfisher's stdout
    /// or stderr — those can contain secrets. Absent when the scan is fine
    /// or was only superseded.
    pub diagnostic: Option<String>,
    pub kingfisher_present: bool,
    pub kingfisher_version: Option<String>,
    /// Kingfisher defaults nested-repo scanning on, and its CLI flag cannot
    /// take `false` (no `ArgAction::Set`). Always true for our invocation.
    pub nested_repos_scanned: bool,
    pub completeness: ScanCompleteness,
    /// Sorted most-actionable first, at most [`super::MAX_FINDINGS`] rows.
    pub findings: Vec<SecretFinding>,
    /// Findings read from Kingfisher's output before the display cap, so
    /// `findings.len() < findings_total` says the list was cut here.
    pub findings_total: u32,
    /// Entries in Kingfisher's findings array this parser could not read.
    /// They are counted, never silently dropped.
    pub findings_unreadable: u32,
    /// True when Kingfisher itself omitted findings past its own report cap.
    pub findings_truncated: bool,
    /// False when the Git listings behind `location` could not be read; every
    /// row that needed them is then `unknown`.
    pub git_status_known: bool,
    /// Files larger than this were skipped by Kingfisher without a trace in
    /// its audit, so the panel has to say so itself.
    pub max_file_size_mb: u32,
    /// Milliseconds since the Unix epoch when the scan finished; 0 when no
    /// scan ran.
    pub scanned_at_ms: u64,
    pub duration_ms: u64,
}

impl SecretsReport {
    fn not_ok(error: String, present: bool, version: Option<String>) -> Self {
        Self {
            ok: false,
            error: Some(error),
            diagnostic: None,
            kingfisher_present: present,
            kingfisher_version: version,
            nested_repos_scanned: true,
            completeness: ScanCompleteness::Unverified,
            findings: Vec::new(),
            findings_total: 0,
            findings_unreadable: 0,
            findings_truncated: false,
            git_status_known: false,
            max_file_size_mb: super::run::MAX_FILE_SIZE_MB,
            scanned_at_ms: 0,
            duration_ms: 0,
        }
    }

    pub fn unavailable(error: impl Into<String>) -> Self {
        Self::not_ok(error.into(), false, None)
    }

    pub fn failed(error: impl Into<String>, version: Option<String>) -> Self {
        Self::not_ok(error.into(), true, version)
    }
}

/// One finding as Kingfisher printed it. Internal: the path is whatever
/// Kingfisher emitted (absolute, under the scan root) and `value_key` is a
/// hash of the secret's value, so neither may reach the webview as-is.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct RawFinding {
    pub rule_id: String,
    pub rule_name: String,
    pub path: String,
    pub line: u32,
    pub confidence: String,
    /// The `--redact` hash of the matched value. Measured on Kingfisher
    /// 2.7.0: equal for equal values within one run whatever the surrounding
    /// line, and re-salted every run. Kingfisher's `fingerprint` is NOT this —
    /// it keys on the match context, so one token on `T="…"` and on
    /// `token = "…"` fingerprints differently — and is not read at all.
    pub value_key: Option<String>,
}

/// Kingfisher's envelope after the allowlist, before any Git context.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Envelope {
    pub version: Option<String>,
    pub findings: Vec<RawFinding>,
    pub unreadable: u32,
    pub omitted: bool,
    pub completeness: ScanCompleteness,
    /// Kingfisher's audit named a failed fetch or scan.
    pub failed: bool,
}

const NOT_JSON: &str = "kingfisher output is not valid JSON";
const NOT_ENVELOPE: &str = "kingfisher output is not a findings report";
/// Longest version string carried into the report. It is untrusted program
/// output rendered in a header, not a payload.
const MAX_VERSION_CHARS: usize = 64;

/// Parse Kingfisher `--format json` stdout by allowlist.
///
/// The whole buffer must be one JSON object with a `findings` array. There is
/// deliberately no line-by-line fallback: `--format jsonl` prints one bare
/// finding object per line, and the old "first object that parses" rule read
/// such a line as an envelope with no findings — a clean scan.
///
/// Only rule id and name, path, line, confidence, the redaction hash inside
/// `snippet`, and the version/omission/audit metadata are read. A `snippet`
/// that is not exactly `[REDACTED:<8 hex>]` is ignored; `secret`, `response`,
/// `dependent_captures` and `fingerprint` are never read.
pub(crate) fn parse_kingfisher_json(
    stdout: &[u8],
    probed_version: Option<String>,
) -> Result<Envelope, String> {
    let text = std::str::from_utf8(stdout).map_err(|_| NOT_JSON.to_string())?;
    let root: serde_json::Value =
        serde_json::from_str(text.trim()).map_err(|_| NOT_JSON.to_string())?;
    let obj = root.as_object().ok_or_else(|| NOT_ENVELOPE.to_string())?;
    let items = obj
        .get("findings")
        .and_then(|v| v.as_array())
        .ok_or_else(|| NOT_ENVELOPE.to_string())?;

    let version = obj
        .get("metadata")
        .and_then(|m| m.get("kingfisher_version"))
        .and_then(|v| v.as_str())
        .map(str::to_string)
        .or(probed_version)
        .and_then(|v| clean_version(&v));

    let omitted = obj
        .get("findings_omitted")
        .and_then(|v| v.as_u64())
        .unwrap_or(0)
        > 0;

    let mut findings = Vec::with_capacity(items.len());
    let mut unreadable: u32 = 0;
    for item in items {
        match extract_finding(item) {
            Some(finding) => findings.push(finding),
            None => unreadable = unreadable.saturating_add(1),
        }
    }

    let (completeness, failed) = audit_verdict(obj.get("audit"));
    Ok(Envelope {
        version,
        findings,
        unreadable,
        omitted,
        completeness,
        failed,
    })
}

/// `kingfisher 2.7.0` (the `--version` line) and `2.7.0` (the envelope) name
/// the same release; the header prints "Kingfisher <version>" itself.
pub(super) fn clean_version(raw: &str) -> Option<String> {
    let trimmed = raw.trim();
    let bare = trimmed
        .strip_prefix("kingfisher ")
        .unwrap_or(trimmed)
        .trim();
    if bare.is_empty() || bare.chars().any(char::is_control) {
        return None;
    }
    // Redact before the length cap. Truncating first can split a token so
    // the redactor no longer recognises it, and the kept prefix is the leak.
    let redacted = crate::ledger::redact::text(bare);
    if redacted.is_empty() {
        return None;
    }
    Some(redacted.chars().take(MAX_VERSION_CHARS).collect())
}

/// Read Kingfisher's repository audit (`kingfisher.repository-audit.v1`).
///
/// Complete only on positive evidence: a non-empty repository list in which
/// every entry says `completed`, and a summary with nothing partial, failed
/// or pending. An absent or unrecognisable audit is `Unverified`, not
/// `Complete` — measured on 2.7.0, an unreadable file leaves exit status 0
/// and zero findings, and the audit is the only place the gap shows.
fn audit_verdict(audit: Option<&serde_json::Value>) -> (ScanCompleteness, bool) {
    let Some(audit) = audit.and_then(|a| a.as_object()) else {
        return (ScanCompleteness::Unverified, false);
    };
    let count = |key: &str| {
        audit
            .get("summary")
            .and_then(|s| s.get(key))
            .and_then(|v| v.as_u64())
    };
    let repositories = audit
        .get("repositories")
        .and_then(|r| r.as_array())
        .map(Vec::as_slice)
        .unwrap_or_default();
    let statuses: Vec<Option<&str>> = repositories
        .iter()
        .map(|r| r.pointer("/scan/status").and_then(|v| v.as_str()))
        .collect();

    let failed = count("scan_failed").is_some_and(|n| n > 0)
        || count("fetch_failed").is_some_and(|n| n > 0)
        || statuses.contains(&Some("failed"));
    if failed {
        return (ScanCompleteness::Partial, true);
    }
    let partial = count("scan_partial").is_some_and(|n| n > 0)
        || count("pending").is_some_and(|n| n > 0)
        || statuses
            .iter()
            .any(|s| matches!(s, Some("partial") | Some("pending")));
    if partial {
        return (ScanCompleteness::Partial, false);
    }
    let summary_known = ["scan_partial", "scan_failed", "pending"]
        .iter()
        .all(|key| count(key).is_some());
    if summary_known && !statuses.is_empty() && statuses.iter().all(|s| *s == Some("completed")) {
        return (ScanCompleteness::Complete, false);
    }
    (ScanCompleteness::Unverified, false)
}

fn extract_finding(item: &serde_json::Value) -> Option<RawFinding> {
    let rule_id = item
        .pointer("/rule/id")
        .and_then(|v| v.as_str())
        .map(str::trim)
        .filter(|s| !s.is_empty())?
        .to_string();
    let finding = item.get("finding")?.as_object()?;
    // A row with no path cannot be located, opened or classified; counting it
    // as unreadable keeps it visible instead of rendering an anonymous "—".
    let path = finding
        .get("path")
        .and_then(|v| v.as_str())
        .filter(|s| !s.is_empty())?
        .to_string();
    let rule_name = item
        .pointer("/rule/name")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .trim()
        .to_string();
    let line = finding
        .get("line")
        .and_then(|v| v.as_u64())
        .unwrap_or(0)
        .min(u64::from(u32::MAX)) as u32;
    let confidence = finding
        .get("confidence")
        .and_then(|v| v.as_str())
        .map(|s| s.trim().to_ascii_lowercase())
        .filter(|s| matches!(s.as_str(), "low" | "medium" | "high"))
        .unwrap_or_default();
    let value_key = finding
        .get("snippet")
        .and_then(|v| v.as_str())
        .and_then(redaction_key);

    Some(RawFinding {
        rule_id,
        rule_name,
        path,
        line,
        confidence,
        value_key,
    })
}

/// The hash out of a `--redact` snippet, `[REDACTED:4f82a9ad]`, and nothing
/// else. Anything not in exactly that form — which is what an unredacted
/// snippet would be — yields `None`, so a raw value can never become a key.
fn redaction_key(snippet: &str) -> Option<String> {
    let hex = snippet.strip_prefix("[REDACTED:")?.strip_suffix(']')?;
    (hex.len() == 8 && hex.bytes().all(|b| b.is_ascii_hexdigit())).then(|| hex.to_ascii_lowercase())
}
