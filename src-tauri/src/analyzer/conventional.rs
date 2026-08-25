use regex::Regex;
use serde::{Deserialize, Serialize};
use std::sync::OnceLock;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ConventionalCommit {
    pub commit_type: String,   // "feat", "fix", "chore", etc.
    pub scope: Option<String>, // e.g. "auth", "parser"
    pub is_breaking: bool,     // "feat!:" or BREAKING CHANGE footer
    pub description: String,   // Short commit message
    pub body: Option<String>,
    pub issue_references: Vec<String>, // e.g. ["#123", "PROJ-456"]
    pub color_badge: &'static str,     // UI tag color
}

pub struct ConventionalCommitParser;

/// Uppercase keys that look like tracker references but are standards or
/// algorithm names; a `KEY-123` match starting with one of these is prose.
const NON_ISSUE_KEYS: &[&str] = &[
    "SHA", "MD", "UTF", "ISO", "CRC", "AES", "RSA", "TLS", "SSL", "IEEE", "RFC", "HTTP", "HTTPS",
    "ECMA", "ASCII", "UUID", "MIME", "SQL", "LLVM", "GCC", "BASE", "HMAC", "JWT", "CVE", "X509",
    "PKCS", "SNI", "W3C",
];

impl Default for ConventionalCommitParser {
    fn default() -> Self {
        Self::new()
    }
}

impl ConventionalCommitParser {
    pub fn new() -> Self {
        Self
    }

    fn header_regex() -> &'static Regex {
        static RE: OnceLock<Regex> = OnceLock::new();
        RE.get_or_init(|| {
            Regex::new(r"^([a-zA-Z]+)(?:\(([^\)]+)\))?(!)?:\s*(.*)$")
                .expect("conventional commit header regex is valid")
        })
    }

    fn issue_regex() -> &'static Regex {
        static RE: OnceLock<Regex> = OnceLock::new();
        RE.get_or_init(|| {
            Regex::new(r"(?:#\d+|[A-Z]{2,}-\d+)").expect("issue reference regex is valid")
        })
    }

    pub fn parse(&self, raw_message: &str) -> Option<ConventionalCommit> {
        let mut lines = raw_message.lines();
        let header = lines.next()?.trim();

        let captures = Self::header_regex().captures(header)?;
        let commit_type = captures.get(1)?.as_str().to_lowercase();
        let scope = captures.get(2).map(|m| m.as_str().to_string());
        let is_breaking_header = captures.get(3).is_some();
        let description = captures.get(4)?.as_str().to_string();

        let body_lines: Vec<&str> = lines.collect();
        let body = if body_lines.is_empty() {
            None
        } else {
            Some(body_lines.join("\n").trim().to_string())
        };

        let has_breaking_footer = body.as_ref().is_some_and(|b| b.contains("BREAKING CHANGE"));
        let is_breaking = is_breaking_header || has_breaking_footer;

        let mut issue_references = Vec::new();
        for mat in Self::issue_regex().find_iter(raw_message) {
            let token = mat.as_str();
            // A `KEY-123` match whose key is a technical acronym (SHA, UTF,
            // RFC…) is prose about standards, not a tracker reference.
            if let Some((key, _)) = token.split_once('-') {
                if NON_ISSUE_KEYS.contains(&key) {
                    continue;
                }
            }
            if !issue_references.iter().any(|r| r == token) {
                issue_references.push(token.to_string());
            }
        }

        let color_badge = match commit_type.as_str() {
            "feat" => "#22c55e",         // Green
            "fix" => "#ef4444",          // Red
            "refactor" => "#a855f7",     // Purple
            "perf" => "#eab308",         // Yellow
            "docs" => "#3b82f6",         // Blue
            "test" => "#06b6d4",         // Cyan
            "build" | "ci" => "#f97316", // Orange
            "chore" => "#6b7280",        // Gray
            _ => "#64748b",
        };

        Some(ConventionalCommit {
            commit_type,
            scope,
            is_breaking,
            description,
            body,
            issue_references,
            color_badge,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_conventional_feat() {
        let parser = ConventionalCommitParser::new();
        let msg = "feat(auth)!: add OAuth2 login flow (#42)\n\nCloses JIRA-99";
        let parsed = parser.parse(msg).unwrap();

        assert_eq!(parsed.commit_type, "feat");
        assert_eq!(parsed.scope, Some("auth".to_string()));
        assert!(parsed.is_breaking);
        assert_eq!(parsed.description, "add OAuth2 login flow (#42)");
        assert_eq!(parsed.issue_references, vec!["#42", "JIRA-99"]);
        assert_eq!(parsed.color_badge, "#22c55e");
    }

    #[test]
    fn test_non_conventional_commit() {
        let parser = ConventionalCommitParser::new();
        let msg = "update readme with new installation instructions";
        assert!(parser.parse(msg).is_none());
    }

    /// Regression (audit L2): technical tokens in a body (`SHA-256`, `UTF-8`)
    /// matched the key-number pattern and surfaced as phantom issue
    /// references; only real tracker keys (#42, JIRA-123) belong there.
    #[test]
    fn technical_tokens_are_not_issue_references() {
        let parser = ConventionalCommitParser::new();
        let msg = "fix: verify hashes\n\nSHA-256 digests, UTF-8 text, ISO-9001 docs;\ntracks JIRA-123 and #42 (see RFC-9110).";
        let parsed = parser.parse(msg).unwrap();
        assert!(parsed.issue_references.contains(&"#42".to_string()));
        assert!(parsed.issue_references.contains(&"JIRA-123".to_string()));
        for noise in ["SHA-256", "UTF-8", "ISO-9001", "RFC-9110"] {
            assert!(
                !parsed.issue_references.iter().any(|r| r == noise),
                "{noise} must not be reported as an issue reference"
            );
        }
    }

    /// Repeated references appear once, in first-seen order.
    #[test]
    fn duplicate_references_are_deduped_in_order() {
        let parser = ConventionalCommitParser::new();
        let msg = "fix: thing\n\nSee #7, then #7 again, then PROJ-2, then #7.";
        let parsed = parser.parse(msg).unwrap();
        assert_eq!(parsed.issue_references, vec!["#7", "PROJ-2"]);
    }
}
