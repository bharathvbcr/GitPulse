//! Parsers for the Rust supply-chain tools that sit beside `cargo audit`.
//!
//! `cargo deny check --format sarif` (cargo-deny 0.20) writes one SARIF
//! document to stdout. `cargo crev verify --json` writes JSONL records
//! `{name, version, source, status}` where `status` is the `VerificationStatus`
//! display: `pass`, `none`, `warn`, or `locl`. `cargo audit bin` reuses the
//! cargo-audit JSON report; a binary built without cargo-auditable fails with
//! "could not extract dependencies" instead of a report.

use serde_json::Value;

pub(crate) const AUDITABLE_EXTRACT_MARKER: &str = "could not extract dependencies";

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct DenyAdvisory {
    pub package: String,
    pub version: String,
    pub advisory_id: String,
    pub title: String,
    /// `high` or `moderate`, matching the health severity vocabulary.
    pub severity: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct DenyLint {
    pub code: String,
    pub severity: String,
    pub message: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct DenyScan {
    pub advisories: Vec<DenyAdvisory>,
    pub lints: Vec<DenyLint>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct CrevScan {
    pub verified: u32,
    pub local: u32,
    pub unreviewed: u32,
    pub negative: Vec<String>,
}

pub(crate) fn auditable_extract_failed(stderr: &str) -> bool {
    stderr
        .to_ascii_lowercase()
        .contains(AUDITABLE_EXTRACT_MARKER)
}

/// What `cargo audit bin` actually established.
///
/// `NotAuditable` covers three cargo-audit outcomes that must not be merged
/// into the vulnerability list: no embed and no panic strings, a parse
/// failure, and a partial list recovered from panic messages. That partial
/// list is incomplete by cargo-audit's own account and is not the lockfile.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum BinAuditKind {
    Complete,
    NotAuditable,
    Failed,
}

pub(crate) fn classify_bin_audit(stdout: &str, stderr: &str, success: bool) -> BinAuditKind {
    let stderr = stderr.to_ascii_lowercase();
    if stderr.contains("was not built with 'cargo auditable'")
        || stderr.contains("no dependency information found")
        || auditable_extract_failed(&stderr)
    {
        return BinAuditKind::NotAuditable;
    }
    if stdout.trim().is_empty() && !success {
        return BinAuditKind::Failed;
    }
    BinAuditKind::Complete
}

/// Outcome of probing one binary for the cargo-auditable linker section.
///
/// `NotFound` means the file was opened and read to its end with no marker. A
/// file that could not be opened or read is `Unreadable`: it says nothing
/// about the section, so it must never be counted as lacking it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum AuditableSection {
    Found,
    NotFound,
    Unreadable(String),
}

/// Most consecutive-or-total EINTR retries one probe tolerates before the read
/// is reported as failed.
const MAX_PROBE_INTERRUPTS: u32 = 64;

/// Probes `path` for the cargo-auditable linker section name `.dep-v0`.
///
/// The scan is capped and looks only for the section name. A `Found` is what
/// justifies spawning `cargo audit bin`; a `NotFound` is not a Rust binary
/// GitPulse should spend an audit timeout on.
pub(crate) fn probe_auditable_section(path: &std::path::Path) -> AuditableSection {
    const NEEDLE: &[u8] = b".dep-v0";
    const CHUNK: usize = 64 * 1024;
    let file = match std::fs::File::open(path) {
        Ok(file) => file,
        Err(err) => return AuditableSection::Unreadable(format!("open failed: {err}")),
    };
    let mut reader = std::io::Read::take(file, 100 * 1024 * 1024);
    let mut pending = Vec::with_capacity(NEEDLE.len());
    let mut buf = [0u8; CHUNK];
    let mut interrupts = 0u32;
    loop {
        let read = match std::io::Read::read(&mut reader, &mut buf) {
            Ok(0) => return AuditableSection::NotFound,
            Ok(n) => n,
            // EINTR is not a failure to read: retry as `read_to_end` does,
            // bounded so a signal storm cannot spin here forever.
            Err(err)
                if err.kind() == std::io::ErrorKind::Interrupted
                    && interrupts < MAX_PROBE_INTERRUPTS =>
            {
                interrupts += 1;
                continue;
            }
            Err(err) => return AuditableSection::Unreadable(format!("read failed: {err}")),
        };
        let mut window = Vec::with_capacity(pending.len() + read);
        window.extend_from_slice(&pending);
        window.extend_from_slice(&buf[..read]);
        if window.windows(NEEDLE.len()).any(|slice| slice == NEEDLE) {
            return AuditableSection::Found;
        }
        if window.len() > NEEDLE.len() - 1 {
            pending = window[window.len() - (NEEDLE.len() - 1)..].to_vec();
        } else {
            pending = window;
        }
    }
}

/// Parses cargo-deny's SARIF document.
///
/// A document that names results but yields none is an error: a policy check
/// must not look clean because the parser stopped understanding the format.
pub(crate) fn parse_cargo_deny_sarif(text: &str) -> Result<DenyScan, String> {
    let value: Value =
        serde_json::from_str(text.trim()).map_err(|e| format!("cargo deny SARIF: {e}"))?;
    let runs = value
        .get("runs")
        .and_then(|runs| runs.as_array())
        .ok_or_else(|| "cargo deny SARIF: missing runs".to_string())?;
    let mut scan = DenyScan {
        advisories: Vec::new(),
        lints: Vec::new(),
    };
    let mut result_count = 0usize;
    for run in runs {
        let Some(results) = run.get("results").and_then(|results| results.as_array()) else {
            continue;
        };
        for result in results {
            result_count += 1;
            absorb_deny_result(&mut scan, result);
        }
    }
    if result_count > 0 && scan.advisories.is_empty() && scan.lints.is_empty() {
        return Err(format!(
            "cargo deny SARIF reported {result_count} results but none could be read; treat this as unscanned, not clean"
        ));
    }
    Ok(scan)
}

fn absorb_deny_result(scan: &mut DenyScan, result: &Value) {
    let rule = result
        .get("ruleId")
        .and_then(|value| value.as_str())
        .unwrap_or("");
    let level = result
        .get("level")
        .and_then(|value| value.as_str())
        .unwrap_or("warning");
    let title = result
        .pointer("/message/text")
        .and_then(|value| value.as_str())
        .unwrap_or("")
        .trim();
    if title.is_empty() && rule.is_empty() {
        return;
    }
    let fingerprints = result
        .get("partialFingerprints")
        .cloned()
        .unwrap_or(Value::Null);
    let advisory_id = fingerprints
        .get("cargo-deny/advisory-id")
        .and_then(|value| value.as_str())
        .unwrap_or("")
        .trim()
        .to_string();
    let krate = fingerprints
        .get("cargo-deny/krate")
        .and_then(|value| value.as_str())
        .unwrap_or("");
    let (package, version) = krate_name_version(krate);
    if rule == "a:vulnerability" && !advisory_id.is_empty() {
        scan.advisories.push(DenyAdvisory {
            package,
            version,
            advisory_id,
            title: if title.is_empty() {
                rule.to_string()
            } else {
                title.to_string()
            },
            severity: if level == "error" {
                "high".into()
            } else if level == "warning" {
                "moderate".into()
            } else {
                "low".into()
            },
        });
        return;
    }
    let code = deny_lint_code(rule);
    let subject = if package.is_empty() {
        String::new()
    } else if version.is_empty() {
        package
    } else {
        format!("{package} {version}")
    };
    let mut message = if subject.is_empty() {
        if title.is_empty() {
            rule.to_string()
        } else {
            title.to_string()
        }
    } else if title.is_empty() {
        subject
    } else {
        format!("{subject}: {title}")
    };
    if !advisory_id.is_empty() && !message.contains(&advisory_id) {
        message = format!("{advisory_id}: {message}");
    }
    scan.lints.push(DenyLint {
        code,
        severity: sarif_issue_severity(level).to_string(),
        message: bound_message(&message),
    });
}

fn deny_lint_code(rule: &str) -> String {
    if rule.starts_with("a:") {
        "cargo_deny_advisory".into()
    } else if rule.starts_with("l:") {
        "cargo_deny_license".into()
    } else if rule.starts_with("b:") {
        "cargo_deny_ban".into()
    } else if rule.starts_with("s:") {
        "cargo_deny_source".into()
    } else {
        "cargo_deny".into()
    }
}

fn sarif_issue_severity(level: &str) -> &'static str {
    match level {
        "error" => "error",
        "note" | "none" => "info",
        _ => "warning",
    }
}

/// `ammonia@0.7.0` or `path+file://…#gnu-licenses@0.1.0`.
fn krate_name_version(raw: &str) -> (String, String) {
    let spec = raw.rsplit('#').next().unwrap_or(raw);
    match spec.rsplit_once('@') {
        Some((name, version)) if !name.is_empty() => (name.to_string(), version.to_string()),
        _ => (spec.to_string(), String::new()),
    }
}

pub(crate) fn parse_cargo_crev_jsonl(text: &str) -> Result<CrevScan, String> {
    let mut scan = CrevScan {
        verified: 0,
        local: 0,
        unreviewed: 0,
        negative: Vec::new(),
    };
    for (index, line) in text.lines().enumerate() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        let value: Value = serde_json::from_str(line)
            .map_err(|e| format!("cargo crev JSONL line {}: {e}", index + 1))?;
        let name = value
            .get("name")
            .and_then(|item| item.as_str())
            .unwrap_or("")
            .trim();
        let version = json_string(value.get("version")).unwrap_or_default();
        let status = value
            .get("status")
            .and_then(|item| item.as_str())
            .unwrap_or("")
            .trim();
        if name.is_empty() || status.is_empty() {
            return Err(format!(
                "cargo crev JSONL line {} omitted name or status",
                index + 1
            ));
        }
        match status {
            "pass" => scan.verified += 1,
            "locl" => scan.local += 1,
            "none" => scan.unreviewed += 1,
            "warn" => scan.negative.push(if version.is_empty() {
                name.to_string()
            } else {
                format!("{name} {version}")
            }),
            other => {
                return Err(format!(
                    "cargo crev JSONL line {} has unrecognized status {other}",
                    index + 1
                ));
            }
        }
    }
    if scan.verified + scan.local + scan.unreviewed == 0 && scan.negative.is_empty() {
        // A verify run lists every crate it judged; whitespace or nothing at
        // all is a run that produced no answer, not zero findings.
        return Err("cargo crev printed no crate rows".into());
    }
    Ok(scan)
}

fn json_string(value: Option<&Value>) -> Option<String> {
    value
        .and_then(|item| item.as_str())
        .map(|text| text.trim().to_string())
        .filter(|text| !text.is_empty())
}

pub(crate) fn bound_message(message: &str) -> String {
    const MAX: usize = 500;
    let message = message.trim();
    if message.chars().count() <= MAX {
        return message.to_string();
    }
    let end = message
        .char_indices()
        .nth(MAX - 1)
        .map(|(index, _)| index)
        .unwrap_or(message.len());
    format!("{}…", &message[..end])
}

pub(crate) fn crev_summary(scan: &CrevScan) -> Option<(String, String)> {
    let negative = scan.negative.len() as u32;
    if negative == 0 && scan.unreviewed == 0 {
        return None;
    }
    let total = scan.verified + scan.local + scan.unreviewed + negative;
    let shown: Vec<&str> = scan.negative.iter().take(8).map(String::as_str).collect();
    let listed = if shown.is_empty() {
        String::new()
    } else {
        let extra = negative.saturating_sub(shown.len() as u32);
        let mut text = shown.join(", ");
        if extra > 0 {
            text.push_str(&format!(", and {extra} more"));
        }
        format!(" Negative: {text}.")
    };
    let severity = if negative > 0 { "warning" } else { "info" };
    let message = format!(
        "cargo-crev: {negative} negative, {} unreviewed, {} verified, {} local (of {total}). A `none` status means the local web of trust has no review that meets the default requirements.{listed}",
        scan.unreviewed, scan.verified, scan.local
    );
    Some((severity.to_string(), bound_message(&message)))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn deny_sarif_maps_advisories_and_policy_lints() {
        let sarif = r#"{
          "runs": [{
            "results": [
              {
                "level": "error",
                "message": {"text": "Uncontrolled recursion"},
                "partialFingerprints": {
                  "cargo-deny/advisory-id": "RUSTSEC-2019-0001",
                  "cargo-deny/krate": "ammonia@0.7.0"
                },
                "ruleId": "a:vulnerability"
              },
              {
                "level": "warning",
                "message": {"text": "unmaintained"},
                "partialFingerprints": {
                  "cargo-deny/advisory-id": "RUSTSEC-2020-0001",
                  "cargo-deny/krate": "oldcrate@1.0.0"
                },
                "ruleId": "a:unmaintained"
              },
              {
                "level": "error",
                "message": {"text": "license is not explicitly allowed"},
                "partialFingerprints": {
                  "cargo-deny/krate": "path+file:///tmp/proj#gnu-licenses@0.1.0"
                },
                "ruleId": "l:rejected"
              },
              {
                "level": "error",
                "message": {"text": "crate comes from an untrusted source"},
                "partialFingerprints": {"cargo-deny/krate": "left-pad@1.0.0"},
                "ruleId": "s:unknown-registry"
              },
              {
                "level": "warning",
                "message": {"text": "duplicate versions"},
                "partialFingerprints": {"cargo-deny/krate": "winapi@0.3.9"},
                "ruleId": "b:duplicate"
              }
            ]
          }]
        }"#;
        let scan = parse_cargo_deny_sarif(sarif).unwrap();
        assert_eq!(scan.advisories.len(), 1);
        assert_eq!(scan.advisories[0].package, "ammonia");
        assert_eq!(scan.advisories[0].advisory_id, "RUSTSEC-2019-0001");
        assert_eq!(scan.advisories[0].severity, "high");
        let codes: Vec<_> = scan.lints.iter().map(|lint| lint.code.as_str()).collect();
        assert_eq!(
            codes,
            vec![
                "cargo_deny_advisory",
                "cargo_deny_license",
                "cargo_deny_source",
                "cargo_deny_ban"
            ]
        );
        assert!(scan.lints[1].message.contains("gnu-licenses 0.1.0"));
        assert_eq!(scan.lints[1].severity, "error");
    }

    #[test]
    fn deny_sarif_with_unreadable_results_is_not_clean() {
        let sarif = r#"{"runs":[{"results":[{"level":"error"}]}]}"#;
        let err = parse_cargo_deny_sarif(sarif).unwrap_err();
        assert!(err.contains("unscanned"));
    }

    #[test]
    fn deny_sarif_without_runs_is_not_a_report() {
        assert!(parse_cargo_deny_sarif(r#"{"ok":true}"#).is_err());
    }

    #[test]
    fn crev_jsonl_counts_trust_statuses() {
        let text = "\
{\"name\":\"libc\",\"version\":\"0.2.1\",\"source\":\"registry\",\"status\":\"warn\"}\n\
{\"name\":\"serde\",\"version\":\"1.0.0\",\"source\":\"registry\",\"status\":\"pass\"}\n\
{\"name\":\"demo\",\"version\":\"0.1.0\",\"source\":\"local\",\"status\":\"locl\"}\n\
{\"name\":\"old\",\"version\":\"0.1.0\",\"source\":\"registry\",\"status\":\"none\"}\n";
        let scan = parse_cargo_crev_jsonl(text).unwrap();
        assert_eq!(scan.verified, 1);
        assert_eq!(scan.local, 1);
        assert_eq!(scan.unreviewed, 1);
        assert_eq!(scan.negative, vec!["libc 0.2.1".to_string()]);
        let (severity, message) = crev_summary(&scan).unwrap();
        assert_eq!(severity, "warning");
        assert!(message.contains("1 negative"));
        assert!(message.contains("libc 0.2.1"));
    }

    #[test]
    fn crev_unknown_status_is_not_dropped() {
        let err = parse_cargo_crev_jsonl(
            "{\"name\":\"libc\",\"version\":\"0.2.1\",\"status\":\"maybe\"}\n",
        )
        .unwrap_err();
        assert!(err.contains("unrecognized status"));
    }

    #[test]
    fn auditable_marker_matches_cargo_audit_bin_stderr() {
        assert!(auditable_extract_failed(
            "error: parse error: could not extract dependencies from binary\n"
        ));
        assert!(!auditable_extract_failed("advisory db missing"));
    }

    #[test]
    fn bin_audit_partial_recovery_is_not_a_complete_report() {
        let stderr = "foo was not built with 'cargo auditable', the report will be incomplete (3 dependencies recovered)\n";
        let stdout = r#"{"vulnerabilities":{"list":[]}}"#;
        assert_eq!(
            classify_bin_audit(stdout, stderr, true),
            BinAuditKind::NotAuditable
        );
        assert_eq!(
            classify_bin_audit(
                "",
                "No dependency information found in target/release/app! Is it a Rust program built with cargo?",
                false,
            ),
            BinAuditKind::NotAuditable
        );
        assert_eq!(
            classify_bin_audit("", "could not extract dependencies from binary", false),
            BinAuditKind::NotAuditable
        );
        assert_eq!(
            classify_bin_audit("", "timed out", false),
            BinAuditKind::Failed
        );
        assert_eq!(
            classify_bin_audit(
                stdout,
                "Found 'cargo auditable' data in app (12 dependencies)",
                true
            ),
            BinAuditKind::Complete
        );
    }

    #[test]
    fn auditable_section_scan_ignores_binaries_without_the_marker() {
        let dir = std::env::temp_dir().join(format!(
            "gitpulse-auditable-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let plain = dir.join("plain");
        let embedded = dir.join("embedded");
        std::fs::write(&plain, b"#!/bin/sh\nexit 0\n").unwrap();
        let mut bytes = vec![0u8; 80];
        bytes.extend_from_slice(b"xxxx.dep-v0yyyy");
        std::fs::write(&embedded, &bytes).unwrap();
        assert_eq!(probe_auditable_section(&plain), AuditableSection::NotFound);
        assert_eq!(probe_auditable_section(&embedded), AuditableSection::Found);
        assert!(matches!(
            probe_auditable_section(&dir.join("absent")),
            AuditableSection::Unreadable(_)
        ));
        std::fs::remove_dir_all(&dir).ok();
    }

    /// adv: an `EINTR` from `read(2)` is not a failure to read the file; the
    /// std contract (`Read::read_to_end`, `BufReader`) is to retry it. A FIFO
    /// holds the reader inside `read` so a signal handler installed without
    /// `SA_RESTART` can interrupt it, then data arrives.
    #[cfg(unix)]
    #[test]
    fn adv_probe_retries_an_interrupted_read() {
        extern "C" fn noop(_: libc::c_int) {}
        let dir = std::env::temp_dir().join(format!(
            "gitpulse-adv-eintr-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let fifo = dir.join("pipe");
        let c_path = std::ffi::CString::new(fifo.to_str().unwrap()).unwrap();
        assert_eq!(unsafe { libc::mkfifo(c_path.as_ptr(), 0o600) }, 0);
        unsafe {
            let mut action: libc::sigaction = std::mem::zeroed();
            action.sa_sigaction = noop as extern "C" fn(libc::c_int) as usize;
            action.sa_flags = 0; // no SA_RESTART
            libc::sigemptyset(&mut action.sa_mask);
            assert_eq!(
                libc::sigaction(libc::SIGUSR1, &action, std::ptr::null_mut()),
                0
            );
        }
        let writer_path = fifo.clone();
        let writer = std::thread::spawn(move || {
            let mut file = std::fs::OpenOptions::new()
                .write(true)
                .open(&writer_path)
                .unwrap();
            std::thread::sleep(std::time::Duration::from_millis(900));
            std::io::Write::write_all(&mut file, b"xx.dep-v0xx").ok();
        });
        let (tx, rx) = std::sync::mpsc::channel();
        let probe_path = fifo.clone();
        let prober = std::thread::spawn(move || {
            tx.send(unsafe { libc::pthread_self() } as usize).unwrap();
            probe_auditable_section(&probe_path)
        });
        let tid = rx.recv().unwrap();
        std::thread::sleep(std::time::Duration::from_millis(400));
        unsafe { libc::pthread_kill(tid as libc::pthread_t, libc::SIGUSR1) };
        let outcome = prober.join().unwrap();
        writer.join().unwrap();
        std::fs::remove_dir_all(&dir).ok();
        assert_eq!(outcome, AuditableSection::Found);
    }
}
