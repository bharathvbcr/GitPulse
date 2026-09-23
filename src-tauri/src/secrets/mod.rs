//! Optional Kingfisher secret scan for the Insights Secrets section.
//!
//! Resolves `kingfisher` on `PATH` only, spawns with a scrubbed environment,
//! and parses stdout by allowlist so secret-bearing fields never enter the
//! report. Kingfisher stdout is not written to the diagnostic log, the ledger,
//! or the webview. Stderr is never copied into IPC errors.

mod parse;
mod run;

pub use parse::{parse_kingfisher_json, SecretFinding, SecretsReport};
pub use run::{
    build_scan_argv, build_scrubbed_env, build_version_argv, nested_repos_scanning_enabled,
    scan_secrets, SCAN_DEADLINE, STDOUT_CAP, SUCCESS_EXITS,
};

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    const PLANTED: &str = "ghp_PlantedTokenNeverSurviveXXXXXXXX";

    #[test]
    fn argv_forces_safe_scan_contract() {
        let argv = build_scan_argv("/tmp/repo");
        assert_eq!(argv[0], "--no-update-check");
        assert_eq!(argv[1], "scan");
        assert_eq!(argv[2], "/tmp/repo");
        assert!(argv.iter().any(|a| a == "--format"));
        assert!(argv.iter().any(|a| a == "json"));
        assert!(argv.iter().any(|a| a == "--no-validate"));
        assert!(argv.iter().any(|a| a == "--git-history"));
        assert!(argv.iter().any(|a| a == "none"));
        assert!(argv.iter().any(|a| a == "--redact"));
        assert!(argv.iter().any(|a| a == "--confidence"));
        assert!(argv.iter().any(|a| a == "medium"));
        assert!(argv.iter().any(|a| a == "--quiet"));
        assert!(!argv.iter().any(|a| a == "--config"));
        assert!(!argv.iter().any(|a| a == "--self-update"));
        assert!(!argv.iter().any(|a| a == "--manage-baseline"));
        assert!(!argv.iter().any(|a| a == "--audit-log"));
    }

    #[test]
    fn version_argv_is_bare() {
        assert_eq!(build_version_argv(), ["--version"]);
    }

    #[test]
    fn scrubbed_env_keeps_only_path_home_no_color() {
        let env = build_scrubbed_env(
            Some(std::ffi::OsStr::new("/usr/bin:/bin")),
            Some(std::ffi::OsStr::new("/Users/test")),
            Some(std::ffi::OsStr::new("secret-token")),
            Some(std::ffi::OsStr::new("aws-key")),
        );
        let keys: Vec<&str> = env.iter().map(|(k, _)| k.as_str()).collect();
        assert_eq!(keys, ["HOME", "NO_COLOR", "PATH"]);
        assert_eq!(
            env.get("NO_COLOR").map(|v| v.as_os_str()),
            Some(std::ffi::OsStr::new("1"))
        );
        assert_eq!(
            env.get("PATH").map(|v| v.as_os_str()),
            Some(std::ffi::OsStr::new("/usr/bin:/bin"))
        );
        assert_eq!(
            env.get("HOME").map(|v| v.as_os_str()),
            Some(std::ffi::OsStr::new("/Users/test"))
        );
        assert!(!env.contains_key("GITHUB_TOKEN"));
        assert!(!env.contains_key("AWS_ACCESS_KEY_ID"));
    }

    #[test]
    fn nested_repos_stay_on_because_flag_cannot_take_false() {
        // Verified against Kingfisher `inputs.rs`: `scan_nested_repos` is
        // `default_value_t = true` with no `ArgAction::Set`, so
        // `--scan-nested-repos false` is not accepted. Report that honestly.
        assert!(nested_repos_scanning_enabled());
    }

    #[test]
    fn success_exits_are_zero_two_hundred_two_oh_five() {
        assert_eq!(SUCCESS_EXITS, [0, 200, 205]);
    }

    #[test]
    fn planted_token_never_survives_serialized_report() {
        let fixture = json!({
            "findings": [{
                "rule": {
                    "title": "GITHUB-PAT => [betterleaks.github-pat]",
                    "name": "github-pat",
                    "id": "betterleaks.github-pat",
                    "description": "GitHub Personal Access Token"
                },
                "finding": {
                    "snippet": format!("token = \"{PLANTED}\""),
                    "fingerprint": "12345678901234567890",
                    "confidence": "medium",
                    "entropy": "4.12",
                    "validation": {
                        "outcome": "not_attempted",
                        "status": "Not Attempted",
                        "response": format!("echo {PLANTED}")
                    },
                    "language": "Shell",
                    "line": 12,
                    "column_start": 1,
                    "column_end": 40,
                    "path": "scripts/ci.sh",
                    "dependent_captures": { "token": PLANTED },
                    "secret": PLANTED
                }
            }],
            "metadata": {
                "kingfisher_version": "1.99.0",
                "summary": { "findings": 1 }
            },
            "findings_omitted": 0
        });
        let report =
            parse_kingfisher_json(fixture.to_string().as_bytes(), Some("1.99.0".into()), true)
                .expect("fixture must parse");
        assert!(report.ok);
        assert_eq!(report.findings.len(), 1);
        assert_eq!(report.findings[0].rule_id, "betterleaks.github-pat");
        assert_eq!(report.findings[0].path, "scripts/ci.sh");
        assert_eq!(report.findings[0].line, 12);
        let serialized = serde_json::to_string(&report).expect("serialize");
        assert!(
            !serialized.contains(PLANTED),
            "planted token leaked into report: {serialized}"
        );
        assert!(!serialized.contains("snippet"));
        assert!(!serialized.contains("secret"));
    }

    #[test]
    fn planted_token_never_survives_error_string_on_bad_json() {
        let junk = format!(r#"{{"broken": "{PLANTED}", not json"#);
        let report = parse_kingfisher_json(junk.as_bytes(), None, true)
            .unwrap_or_else(|_| run::failed_report("kingfisher output is not valid JSON", None));
        assert!(!report.ok);
        let err = report.error.clone().unwrap_or_default();
        assert!(
            !err.contains(PLANTED),
            "planted token leaked into error: {err}"
        );
        let serialized = serde_json::to_string(&report).expect("serialize");
        assert!(!serialized.contains(PLANTED));
    }

    #[test]
    fn missing_binary_is_not_clean() {
        let report = run::missing_binary_report();
        assert!(!report.ok);
        assert!(!report.kingfisher_present);
        assert!(report
            .error
            .as_deref()
            .is_some_and(|e| e.contains("brew install kingfisher")));
        assert!(report.findings.is_empty());
    }

    #[test]
    fn bad_json_is_not_clean() {
        let report = parse_kingfisher_json(b"not-json", None, true)
            .unwrap_or_else(|_| run::failed_report("kingfisher output is not valid JSON", None));
        assert!(!report.ok);
        assert!(report.findings.is_empty());
    }

    #[test]
    fn capped_stdout_is_not_clean() {
        let report = run::failed_report("kingfisher output was truncated", Some("1.0.0".into()));
        assert!(!report.ok);
        assert!(report.findings.is_empty());
        assert!(report
            .error
            .as_deref()
            .is_some_and(|e| e.contains("truncated")));
    }

    #[test]
    fn non_success_exit_is_not_clean() {
        let report = run::report_from_exit(1, b"{}", Some("1.0.0".into()));
        assert!(!report.ok);
        assert!(report.findings.is_empty());
    }

    #[test]
    fn exit_two_hundred_with_findings_is_ok() {
        let body = json!({
            "findings": [{
                "rule": { "id": "betterleaks.aws-access-token", "name": "aws", "title": "t", "description": "d" },
                "finding": {
                    "fingerprint": "1",
                    "confidence": "high",
                    "validation": { "outcome": "not_attempted" },
                    "line": 3,
                    "path": "a.env"
                }
            }],
            "metadata": { "kingfisher_version": "1.99.0" },
            "findings_omitted": 0
        });
        let report = run::report_from_exit(200, body.to_string().as_bytes(), Some("1.99.0".into()));
        assert!(report.ok);
        assert_eq!(report.findings.len(), 1);
        assert!(report.nested_repos_scanned);
    }
}
