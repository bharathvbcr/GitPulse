//! Allowlist parse of Kingfisher JSON so secret fields never enter the report.

use serde::{Deserialize, Serialize};

/// One allowlisted finding row for the Insights Secrets panel.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct SecretFinding {
    pub rule_id: String,
    pub path: String,
    pub line: u32,
    pub confidence: String,
    pub validation_outcome: String,
    pub fingerprint: String,
}

/// Result of an on-demand Kingfisher secrets scan.
///
/// `ok: false` means the scan did not complete successfully — missing binary,
/// truncated stdout, bad JSON, timeout, or a non-success exit. That must never
/// render as a clean repository.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct SecretsReport {
    pub ok: bool,
    /// Fixed reason string only. Never carries Kingfisher stdout or stderr.
    pub error: Option<String>,
    pub kingfisher_present: bool,
    pub kingfisher_version: Option<String>,
    /// Kingfisher defaults nested-repo scanning on, and its CLI flag cannot
    /// take `false` (no `ArgAction::Set`). Always true for our invocation.
    pub nested_repos_scanned: bool,
    pub findings: Vec<SecretFinding>,
    /// True when Kingfisher itself omitted findings past its own report cap.
    pub findings_truncated: bool,
}

impl SecretsReport {
    pub fn unavailable(error: impl Into<String>) -> Self {
        Self {
            ok: false,
            error: Some(error.into()),
            kingfisher_present: false,
            kingfisher_version: None,
            nested_repos_scanned: true,
            findings: Vec::new(),
            findings_truncated: false,
        }
    }

    pub fn failed(error: impl Into<String>, version: Option<String>) -> Self {
        Self {
            ok: false,
            error: Some(error.into()),
            kingfisher_present: true,
            kingfisher_version: version,
            nested_repos_scanned: true,
            findings: Vec::new(),
            findings_truncated: false,
        }
    }
}

/// Parse Kingfisher `--format json` stdout by allowlist.
///
/// Only rule id, path, line, confidence, validation outcome, fingerprint, and
/// version/omission metadata are read. Fields such as `snippet`, `secret`,
/// `response`, and `dependent_captures` are ignored even when present.
pub fn parse_kingfisher_json(
    stdout: &[u8],
    probed_version: Option<String>,
    nested_repos_scanned: bool,
) -> Result<SecretsReport, String> {
    let text = std::str::from_utf8(stdout).map_err(|_| "kingfisher output is not valid JSON")?;
    let trimmed = text.trim();
    if trimmed.is_empty() {
        return Err("kingfisher output is not valid JSON".into());
    }
    // Kingfisher may emit one envelope per line (JSONL) for parallel scans.
    // Take the first non-empty line that parses as an object with findings.
    let mut last_err = "kingfisher output is not valid JSON".to_string();
    for line in trimmed.lines().filter(|l| !l.trim().is_empty()) {
        match parse_envelope(line, probed_version.clone(), nested_repos_scanned) {
            Ok(report) => return Ok(report),
            Err(e) => last_err = e,
        }
    }
    // Whole buffer as a single JSON value (pretty or compact).
    parse_envelope(trimmed, probed_version, nested_repos_scanned).map_err(|_| last_err)
}

fn parse_envelope(
    text: &str,
    probed_version: Option<String>,
    nested_repos_scanned: bool,
) -> Result<SecretsReport, String> {
    let root: serde_json::Value =
        serde_json::from_str(text).map_err(|_| "kingfisher output is not valid JSON")?;
    let obj = root
        .as_object()
        .ok_or_else(|| "kingfisher output is not valid JSON".to_string())?;

    let version = obj
        .get("metadata")
        .and_then(|m| m.get("kingfisher_version"))
        .and_then(|v| v.as_str())
        .map(str::to_string)
        .or(probed_version);

    let findings_omitted = obj
        .get("findings_omitted")
        .and_then(|v| v.as_u64())
        .unwrap_or(0);
    let findings_raw = obj.get("findings").and_then(|v| v.as_array());
    let mut findings = Vec::new();
    if let Some(items) = findings_raw {
        for item in items {
            if let Some(finding) = extract_finding(item) {
                findings.push(finding);
            }
        }
    } else if obj.contains_key("findings") {
        return Err("kingfisher output is not valid JSON".into());
    }

    Ok(SecretsReport {
        ok: true,
        error: None,
        kingfisher_present: true,
        kingfisher_version: version,
        nested_repos_scanned,
        findings,
        findings_truncated: findings_omitted > 0,
    })
}

fn extract_finding(item: &serde_json::Value) -> Option<SecretFinding> {
    let rule_id = item
        .pointer("/rule/id")
        .and_then(|v| v.as_str())
        .filter(|s| !s.is_empty())?
        .to_string();
    let finding = item.get("finding")?;
    let path = finding
        .get("path")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();
    let line = finding
        .get("line")
        .and_then(|v| v.as_u64())
        .unwrap_or(0)
        .min(u64::from(u32::MAX)) as u32;
    let confidence = finding
        .get("confidence")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();
    let fingerprint = finding
        .get("fingerprint")
        .and_then(|v| {
            v.as_str()
                .map(str::to_string)
                .or_else(|| v.as_u64().map(|n| n.to_string()))
        })
        .unwrap_or_default();
    let validation_outcome = finding
        .pointer("/validation/outcome")
        .and_then(|v| v.as_str())
        .unwrap_or("not_attempted")
        .to_string();

    Some(SecretFinding {
        rule_id,
        path,
        line,
        confidence,
        validation_outcome,
        fingerprint,
    })
}
