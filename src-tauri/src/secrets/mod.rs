//! Optional Kingfisher secret scan for the Insights Secrets section.
//!
//! Resolves `kingfisher` like every other external tool (PATH, then the
//! GUI-launch fallback directories), spawns it with a scrubbed environment,
//! and parses stdout by allowlist so secret-bearing fields never enter the
//! report. Kingfisher stdout is not written to the diagnostic log, the ledger,
//! or the webview. Stderr is never copied into IPC errors. Kingfisher's value
//! fingerprints stay in this process; the webview gets an ordinal instead.

mod parse;
mod report;
mod run;

pub use parse::{ScanCompleteness, SecretFinding, SecretLocation, SecretsReport};
pub use report::MAX_FINDINGS;
pub use run::{
    build_scan_argv, build_scrubbed_env, build_version_argv, nested_repos_scanning_enabled,
    report_from_exit, resolve_kingfisher, scan_secrets, scan_with_binary, ScanTicket,
    MAX_FILE_SIZE_MB, SCAN_DEADLINE, STDOUT_CAP, SUCCESS_EXITS,
};

#[cfg(test)]
mod tests {
    use super::report::{assemble, GitView, RunFacts};
    use super::*;
    use serde_json::json;
    use std::path::Path;

    const PLANTED: &str = "ghp_PlantedTokenNeverSurviveXXXXXXXX";

    /// Kingfisher 2.7.0's envelope for a tree holding one unreadable file:
    /// exit 0, zero findings, and `audit.repositories[0].scan.status:
    /// "partial"`. Captured from a real run with the root rewritten to /repo.
    const PARTIAL: &str = include_str!("fixtures/kingfisher-2.7.0-partial.json");
    /// Kingfisher 2.7.0 `--no-dedup` over a fixture tree: seven findings, two
    /// of them the same token in `ci.sh` and `nested/inner.sh`.
    const FINDINGS: &str = include_str!("fixtures/kingfisher-2.7.0-findings.json");

    fn facts() -> RunFacts {
        RunFacts {
            scanned_at_ms: 1,
            duration_ms: 2,
            nested_repos_scanned: true,
            max_file_size_mb: MAX_FILE_SIZE_MB,
        }
    }

    fn envelope(text: &str) -> parse::Envelope {
        parse::parse_kingfisher_json(text.as_bytes(), None).expect("envelope parses")
    }

    fn clean_envelope() -> String {
        let mut doc: serde_json::Value = serde_json::from_str(PARTIAL).expect("fixture");
        doc["audit"]["summary"]["scan_partial"] = json!(0);
        doc["audit"]["summary"]["scan_succeeded"] = json!(1);
        doc["audit"]["repositories"][0]["scan"]["status"] = json!("completed");
        doc["audit"]["repositories"][0]["scan"]
            .as_object_mut()
            .expect("scan object")
            .remove("error");
        doc.to_string()
    }

    /// A Kingfisher row. `value` is the hex the `--redact` snippet carries
    /// (zero-padded to eight digits); `None` leaves the snippet unredacted,
    /// which must never be read as a key. The fingerprint is constant on
    /// purpose: it keys on match context, and grouping must not follow it.
    fn finding(path: &str, value: Option<&str>) -> serde_json::Value {
        let snippet = match value {
            Some(v) => format!("[REDACTED:{v:0>8}]"),
            None => PLANTED.to_string(),
        };
        json!({
            "rule": { "id": "betterleaks.github-pat", "name": "github-pat" },
            "finding": {
                "path": path, "line": 3, "confidence": "high",
                "fingerprint": "777", "snippet": snippet, "secret": PLANTED
            }
        })
    }

    #[test]
    fn groups_follow_the_redacted_value_not_the_fingerprint() {
        let mut other_rule = finding("/repo/e", Some("a"));
        other_rule["rule"]["id"] = json!("betterleaks.other");
        let mut upper = finding("/repo/f", Some("a"));
        upper["finding"]["snippet"] = json!("[REDACTED:0000000A]");
        let text = json!({ "findings": [
            finding("/repo/a", Some("a")),
            finding("/repo/b", Some("a")),
            finding("/repo/c", Some("b")),
            finding("/repo/d", None),
            other_rule,
            upper,
            { "rule": { "id": "betterleaks.github-pat" },
              "finding": { "path": "/repo/g", "snippet": "[REDACTED:0000000a] trailing" } },
        ] })
        .to_string();
        let report = report_from_exit(200, text.as_bytes(), None);
        let group = |p: &str| {
            report
                .findings
                .iter()
                .find(|f| f.path == p)
                .unwrap()
                .secret_group
        };
        assert_eq!(group("a"), group("b"), "same value, same group");
        assert_eq!(group("a"), group("f"), "hex case is not a different value");
        assert_ne!(
            group("a"),
            group("c"),
            "equal fingerprints are not equal values"
        );
        assert_ne!(group("a"), group("e"), "a hash is scoped to its rule");
        assert_eq!(group("d"), 0, "an unredacted snippet is never a key");
        assert_eq!(group("g"), 0, "only the exact redacted form is a key");
        assert!(!serde_json::to_string(&report).unwrap().contains(PLANTED));
    }

    #[test]
    fn argv_forces_safe_scan_contract() {
        let argv = build_scan_argv("/tmp/repo");
        assert_eq!(argv[0], "--no-update-check");
        assert_eq!(argv[1], "scan");
        assert_eq!(argv[2], "/tmp/repo");
        let pair = |flag: &str| {
            let at = argv.iter().position(|a| a == flag)?;
            argv.get(at + 1).cloned()
        };
        assert_eq!(pair("--format").as_deref(), Some("json"));
        assert_eq!(pair("--git-history").as_deref(), Some("none"));
        assert_eq!(pair("--confidence").as_deref(), Some("medium"));
        assert_eq!(pair("--max-file-size"), Some(MAX_FILE_SIZE_MB.to_string()));
        let jobs: usize = pair("--jobs").expect("--jobs").parse().expect("number");
        assert!(jobs >= 1);
        for flag in ["--no-validate", "--redact", "--no-dedup", "--quiet"] {
            assert!(argv.iter().any(|a| a == flag), "missing {flag}");
        }
        // Inline ignore directives stay honoured on purpose (see
        // build_scan_argv); a later --no-ignore must be a decision, not drift.
        for flag in [
            "--config",
            "--self-update",
            "--manage-baseline",
            "--audit-log",
            "--no-ignore",
            "--alert-webhook",
            "--view-report",
            "--output",
        ] {
            assert!(!argv.iter().any(|a| a == flag), "must not pass {flag}");
        }
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
        );
        let keys: Vec<&str> = env.keys().map(|k| k.as_str()).collect();
        assert_eq!(keys, ["HOME", "NO_COLOR", "PATH"]);
        assert_eq!(
            env.get("NO_COLOR").map(|v| v.as_os_str()),
            Some(std::ffi::OsStr::new("1"))
        );
    }

    #[test]
    fn nested_repos_stay_on_because_flag_cannot_take_false() {
        // Verified against Kingfisher 2.7.0: `--scan-nested-repos=false` exits
        // 2 with "unexpected value 'false'". Report that honestly.
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
                    "path": "/repo/scripts/ci.sh",
                    "dependent_captures": { "token": PLANTED },
                    "secret": PLANTED
                }
            }],
            "metadata": { "kingfisher_version": "1.99.0", "summary": { "findings": 1 } },
            "findings_omitted": 0
        });
        let report = report_from_exit(200, fixture.to_string().as_bytes(), None);
        assert!(report.ok);
        assert_eq!(report.findings.len(), 1);
        assert_eq!(report.findings[0].rule_id, "betterleaks.github-pat");
        assert_eq!(report.findings[0].rule_name, "github-pat");
        assert_eq!(report.findings[0].path, "scripts/ci.sh");
        assert_eq!(report.findings[0].line, 12);
        let serialized = serde_json::to_string(&report).expect("serialize");
        for needle in [
            PLANTED,
            "snippet",
            "secret\"",
            "12345678901234567890",
            "not_attempted",
        ] {
            assert!(
                !serialized.contains(needle),
                "{needle} leaked: {serialized}"
            );
        }
    }

    #[test]
    fn planted_token_never_survives_error_string_on_bad_json() {
        let junk = format!(r#"{{"broken": "{PLANTED}", not json"#);
        let report = report_from_exit(0, junk.as_bytes(), None);
        assert!(!report.ok);
        let serialized = serde_json::to_string(&report).expect("serialize");
        assert!(!serialized.contains(PLANTED), "leaked: {serialized}");
    }

    #[test]
    fn missing_binary_is_not_clean_and_says_where_it_looked() {
        let report = run::missing_binary_report();
        assert!(!report.ok);
        assert!(!report.kingfisher_present);
        let error = report.error.as_deref().unwrap_or_default();
        assert!(error.contains("brew install kingfisher"));
        // Not "not on PATH": the lookup also searched the fallback dirs, and
        // naming only PATH was the false cause a Dock launch got.
        assert!(error.contains("standard install directories"), "{error}");
        assert!(report.findings.is_empty());
    }

    /// The Dock-launch reproduction: launchd hands a GUI app
    /// `/usr/bin:/bin:/usr/sbin:/sbin`, and the old PATH-only lookup reported
    /// a `~/.local/bin` (or Homebrew) install as "not installed".
    #[cfg(unix)]
    #[test]
    fn a_gui_launch_path_still_finds_a_user_install() {
        use std::os::unix::fs::PermissionsExt;
        let home = tempfile::tempdir().unwrap();
        let bin = home.path().join(".local/bin");
        std::fs::create_dir_all(&bin).unwrap();
        let tool = bin.join("kingfisher");
        std::fs::write(&tool, "#!/bin/sh\n").unwrap();
        std::fs::set_permissions(&tool, std::fs::Permissions::from_mode(0o755)).unwrap();
        let launchd = std::ffi::OsStr::new("/usr/bin:/bin:/usr/sbin:/sbin");
        // System fallback dirs rank above `~/.local/bin`; on a host with a
        // real Homebrew install that one wins, which is equally "found".
        let system = ["/opt/homebrew/bin/kingfisher", "/usr/local/bin/kingfisher"]
            .into_iter()
            .map(std::path::PathBuf::from)
            .find(|p| p.is_file());
        assert_eq!(
            run::resolve_kingfisher_with(Some(launchd), Some(home.path().as_os_str())),
            Some(system.clone().unwrap_or_else(|| tool.clone()))
        );
        // A non-executable file of the right name is not an install: the old
        // `is_file()` probe accepted it and turned "missing" into a spawn error.
        std::fs::set_permissions(&tool, std::fs::Permissions::from_mode(0o644)).unwrap();
        let found = run::resolve_kingfisher_with(Some(launchd), Some(home.path().as_os_str()));
        assert_ne!(found, Some(tool));
    }

    #[test]
    fn bad_json_and_non_success_exits_are_not_clean() {
        assert!(!report_from_exit(0, b"not-json", None).ok);
        assert!(!report_from_exit(1, b"{\"findings\": []}", None).ok);
        assert!(!report_from_exit(2, b"", None).ok);
    }

    #[test]
    fn a_signal_is_not_reported_as_an_exit_status() {
        let report = report_from_exit(-1, b"", None);
        let error = report.error.unwrap_or_default();
        if cfg!(unix) {
            assert_eq!(error, "kingfisher was terminated before it finished");
        } else {
            assert_eq!(error, "kingfisher exited with status -1");
        }
    }

    #[test]
    fn runner_failures_name_their_own_cause() {
        let deadline = std::time::Duration::from_secs(7);
        let reason = |err: &str| run::runner_failure_reason(err, deadline);
        assert_eq!(
            reason("kingfisher timed out after 7s waiting for a process slot"),
            "kingfisher could not start: no process slot became free in time"
        );
        assert_eq!(
            reason("kingfisher timed out after 7s"),
            "kingfisher did not finish within 7s"
        );
        assert_eq!(
            reason("kingfisher cancelled before spawn"),
            "superseded by a newer secrets scan"
        );
        assert_eq!(
            reason("Failed to spawn kingfisher: EACCES"),
            "kingfisher could not be started"
        );
        assert_eq!(
            reason("Failed to poll kingfisher output: /x/y"),
            "kingfisher failed to run"
        );
    }

    #[test]
    fn red_a_partial_scan_never_reports_like_a_clean_one() {
        let partial = report_from_exit(0, PARTIAL.as_bytes(), None);
        let clean = report_from_exit(0, clean_envelope().as_bytes(), None);
        assert_ne!(
            serde_json::to_value(&partial).unwrap(),
            serde_json::to_value(&clean).unwrap(),
            "a scan Kingfisher marked partial rendered identically to a complete one"
        );
        assert_eq!(partial.completeness, ScanCompleteness::Partial);
        assert_eq!(clean.completeness, ScanCompleteness::Complete);
        assert!(partial.findings.is_empty());
    }

    #[test]
    fn red_an_object_without_findings_is_not_a_clean_scan() {
        let report = report_from_exit(0, b"{}", None);
        assert!(!report.ok, "`{{}}` was accepted as a clean scan");
        let report = report_from_exit(0, b"{\"findings\": {}}", None);
        assert!(!report.ok, "a non-array findings field was accepted");
        let report = report_from_exit(0, b"[]", None);
        assert!(!report.ok, "a bare array was accepted");
    }

    #[test]
    fn red_a_bare_finding_object_is_not_an_envelope() {
        // `--format jsonl` prints one finding per line. The old first-object
        // fallback took such a line for an envelope with no findings key.
        let line = finding("/repo/a.sh", Some("1"));
        let report = report_from_exit(200, format!("{line}\n{line}\n").as_bytes(), None);
        assert!(
            !(report.ok && report.findings.is_empty()),
            "a finding line was read as a clean envelope"
        );
        assert!(!report.ok);
    }

    #[test]
    fn red_an_unreadable_finding_is_never_silently_dropped() {
        let good = finding("/repo/a.sh", Some("1"));
        let one = json!({ "findings": [good.clone()] }).to_string();
        let two = json!({ "findings": [
            good.clone(),
            { "finding": { "path": "/repo/b.sh" } },
            { "rule": { "id": "x" }, "finding": { "line": 2 } },
            { "rule": { "id": "  " }, "finding": { "path": "/repo/c.sh" } },
            "not an object",
        ] })
        .to_string();
        let one = report_from_exit(200, one.as_bytes(), None);
        let two = report_from_exit(200, two.as_bytes(), None);
        assert_ne!(
            serde_json::to_value(&one).unwrap(),
            serde_json::to_value(&two).unwrap(),
            "a finding the parser could not read vanished without a trace"
        );
        assert_eq!(two.findings.len(), 1);
        assert_eq!(two.findings_unreadable, 4);
        assert_eq!(one.findings_unreadable, 0);
    }

    #[test]
    fn the_captured_report_is_relativised_grouped_and_fingerprint_free() {
        let report = report_from_exit(200, FINDINGS.as_bytes(), Some("kingfisher 2.7.0".into()));
        assert!(report.ok);
        assert_eq!(report.completeness, ScanCompleteness::Complete);
        assert_eq!(report.kingfisher_version.as_deref(), Some("2.7.0"));
        assert_eq!(report.findings_total, 7);
        let paths: Vec<&str> = report.findings.iter().map(|f| f.path.as_str()).collect();
        for expected in ["a.env", "ci.sh", "nested/inner.sh", "build/out.txt"] {
            assert!(
                paths.contains(&expected),
                "{expected} missing from {paths:?}"
            );
        }
        assert!(paths.iter().all(|p| !p.starts_with('/')), "{paths:?}");
        let group = |path: &str| {
            report
                .findings
                .iter()
                .find(|f| f.path == path)
                .map(|f| f.secret_group)
                .expect(path)
        };
        assert_eq!(group("ci.sh"), group("nested/inner.sh"));
        assert_ne!(group("ci.sh"), group("a.env"));
        assert!(report.findings.iter().all(|f| f.secret_group > 0));
        let serialized = serde_json::to_string(&report).unwrap();
        let raw: serde_json::Value = serde_json::from_str(FINDINGS).unwrap();
        for item in raw["findings"].as_array().unwrap() {
            let fingerprint = item["finding"]["fingerprint"].as_str().unwrap();
            assert!(
                !serialized.contains(fingerprint),
                "fingerprint {fingerprint} leaked"
            );
            let snippet = item["finding"]["snippet"].as_str().unwrap();
            assert!(!serialized.contains(snippet), "snippet {snippet} leaked");
        }
    }

    #[test]
    fn completeness_needs_positive_evidence() {
        let with_audit = |audit: serde_json::Value| {
            report_from_exit(
                0,
                json!({ "findings": [], "audit": audit })
                    .to_string()
                    .as_bytes(),
                None,
            )
        };
        // No audit at all: an older Kingfisher. Unknown, never complete.
        let none = report_from_exit(0, b"{\"findings\": []}", None);
        assert!(none.ok);
        assert_eq!(none.completeness, ScanCompleteness::Unverified);
        // Audit naming no repositories.
        let empty = with_audit(json!({
            "summary": { "scan_partial": 0, "scan_failed": 0, "pending": 0 },
            "repositories": []
        }));
        assert_eq!(empty.completeness, ScanCompleteness::Unverified);
        // A status this code has never seen.
        let novel = with_audit(json!({
            "summary": { "scan_partial": 0, "scan_failed": 0, "pending": 0 },
            "repositories": [{ "scan": { "status": "throttled" } }]
        }));
        assert_eq!(novel.completeness, ScanCompleteness::Unverified);
        // Completed rows but a summary missing its counters.
        let thin = with_audit(json!({ "repositories": [{ "scan": { "status": "completed" } }] }));
        assert_eq!(thin.completeness, ScanCompleteness::Unverified);
        // Pending inputs are a partial scan.
        let pending = with_audit(json!({
            "summary": { "scan_partial": 0, "scan_failed": 0, "pending": 1 },
            "repositories": [{ "scan": { "status": "completed" } }]
        }));
        assert_eq!(pending.completeness, ScanCompleteness::Partial);
        assert!(pending.ok);
    }

    #[test]
    fn a_failed_scan_is_not_ok_but_keeps_what_it_found() {
        let body = json!({
            "findings": [finding("/repo/a.sh", Some("9"))],
            "audit": {
                "summary": { "scan_partial": 0, "scan_failed": 1, "pending": 0 },
                "repositories": [{ "scan": { "status": "failed", "error": PLANTED } }]
            }
        });
        let report = report_from_exit(200, body.to_string().as_bytes(), None);
        assert!(!report.ok);
        assert_eq!(report.completeness, ScanCompleteness::Partial);
        assert_eq!(
            report.findings.len(),
            1,
            "a found secret must not be hidden"
        );
        let serialized = serde_json::to_string(&report).unwrap();
        assert!(!serialized.contains(PLANTED), "audit error text leaked");
    }

    #[test]
    fn version_and_confidence_are_normalised() {
        let body = |version: &str, confidence: &str| {
            json!({
                "findings": [{
                    "rule": { "id": "r" },
                    "finding": { "path": "/repo/a", "confidence": confidence }
                }],
                "metadata": { "kingfisher_version": version }
            })
            .to_string()
        };
        let r = report_from_exit(0, body("2.7.0", "HIGH").as_bytes(), None);
        assert_eq!(r.findings[0].confidence, "high");
        assert_eq!(r.kingfisher_version.as_deref(), Some("2.7.0"));
        let r = report_from_exit(0, body("2.7.0\u{1b}[31m", "certain").as_bytes(), None);
        assert_eq!(r.findings[0].confidence, "");
        assert_eq!(r.kingfisher_version, None);
        let long = "9".repeat(500);
        let r = report_from_exit(0, body(&long, "low").as_bytes(), None);
        assert_eq!(r.kingfisher_version.map(|v| v.len()), Some(64));
    }

    fn view(tracked: &[&str], ignored: &[&str]) -> GitView {
        GitView {
            tracked: tracked.iter().map(|s| s.to_string()).collect(),
            ignored: ignored.iter().map(|s| s.to_string()).collect(),
        }
    }

    fn located(
        text: &str,
        root: &Path,
        git: Result<GitView, String>,
    ) -> Vec<(String, SecretLocation)> {
        assemble(envelope(text), root, git, facts())
            .findings
            .into_iter()
            .map(|f| (f.path, f.location))
            .collect()
    }

    #[test]
    fn every_finding_is_located_against_git() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        std::fs::create_dir_all(root.join("vendor/lib/.git")).unwrap();
        std::fs::create_dir_all(root.join("sub")).unwrap();
        std::fs::write(root.join("sub/.git"), "gitdir: ../.git/modules/sub\n").unwrap();
        let p = |rel: &str| root.join(rel).to_string_lossy().into_owned();
        let text = json!({ "findings": [
            finding(&p("src/app.rs"), Some("1")),
            finding(&p(".env"), Some("2")),
            finding(&p("target/debug/x.o"), Some("3")),
            finding(&p("notes.txt"), Some("4")),
            finding(&p(".git/config"), Some("5")),
            finding(&p("vendor/lib/key.pem"), Some("6")),
            finding(&p("vendor/lib/.git/config"), Some("7")),
            finding(&p("sub/inner.txt"), Some("8")),
            finding("/elsewhere/x", Some("9")),
            finding(&format!("{}/../escape", root.display()), Some("10")),
            finding(&format!("{}-sibling/x", root.display()), Some("11")),
        ] })
        .to_string();
        let got = located(&text, root, Ok(view(&["src/app.rs"], &[".env", "target/"])));
        let find = |path: &str| {
            got.iter()
                .find(|(p, _)| p == path || p.ends_with(path))
                .map(|(_, l)| *l)
                .unwrap_or_else(|| panic!("{path} not in {got:?}"))
        };
        assert_eq!(find("src/app.rs"), SecretLocation::Tracked);
        assert_eq!(find(".env"), SecretLocation::Ignored);
        assert_eq!(find("target/debug/x.o"), SecretLocation::Ignored);
        assert_eq!(find("notes.txt"), SecretLocation::Untracked);
        assert_eq!(find(".git/config"), SecretLocation::GitMetadata);
        assert_eq!(find("vendor/lib/key.pem"), SecretLocation::NestedRepo);
        assert_eq!(find("vendor/lib/.git/config"), SecretLocation::GitMetadata);
        assert_eq!(find("sub/inner.txt"), SecretLocation::NestedRepo);
        assert_eq!(find("/elsewhere/x"), SecretLocation::Outside);
        assert_eq!(find("../escape"), SecretLocation::Outside);
        assert_eq!(find("-sibling/x"), SecretLocation::Outside);
        // Most actionable first.
        assert_eq!(got[0].1, SecretLocation::GitMetadata);
        assert_eq!(got[2].1, SecretLocation::Tracked);
        assert_eq!(got.last().unwrap().1, SecretLocation::Outside);
    }

    #[test]
    fn without_git_listings_only_structural_locations_are_decided() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        let p = |rel: &str| root.join(rel).to_string_lossy().into_owned();
        let text = json!({ "findings": [
            finding(&p("src/app.rs"), Some("1")),
            finding(&p(".git/config"), Some("2")),
        ] })
        .to_string();
        let report = assemble(envelope(&text), root, Err("git failed".into()), facts());
        assert!(!report.git_status_known);
        let locations: Vec<_> = report.findings.iter().map(|f| f.location).collect();
        assert_eq!(
            locations,
            [SecretLocation::GitMetadata, SecretLocation::Unknown]
        );
        let report = assemble(envelope(&text), root, Ok(GitView::default()), facts());
        assert!(report.git_status_known);
    }

    #[test]
    fn the_display_cap_drops_the_least_actionable_rows_and_says_so() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        let mut items = Vec::new();
        for i in 0..(MAX_FINDINGS + 500) {
            items.push(finding(
                &root.join(format!("build/{i}.txt")).to_string_lossy(),
                None,
            ));
        }
        items.push(finding(
            &root.join("src/real.rs").to_string_lossy(),
            Some("f"),
        ));
        let text = json!({ "findings": items }).to_string();
        let report = assemble(
            envelope(&text),
            root,
            Ok(view(&["src/real.rs"], &["build/"])),
            facts(),
        );
        assert_eq!(report.findings.len(), MAX_FINDINGS);
        assert_eq!(report.findings_total as usize, MAX_FINDINGS + 501);
        assert_eq!(
            report.findings[0].path, "src/real.rs",
            "the tracked row survived the cap"
        );
        assert_eq!(report.findings[0].secret_group, 1);
        assert!(report.findings[1..].iter().all(|f| f.secret_group == 0));
    }
}
