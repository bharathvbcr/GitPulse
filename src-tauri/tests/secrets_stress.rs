//! Stress and adversarial coverage for the Insights → Secrets scan.
//!
//! Every test drives `scan_with_binary` — the production path past PATH lookup
//! — with a stub `kingfisher` shell script, so exit codes, malformed output,
//! signals, timeouts, oversized output and superseded scans are exercised
//! against the real process runner, the real trust gate and real Git.
#![cfg(unix)]

mod common;

use gitpulse_lib::engine::git_cli::validate_repo;
use gitpulse_lib::secrets::{
    scan_with_binary, ScanCompleteness, ScanTicket, SecretLocation, SecretsReport, MAX_FINDINGS,
};
use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::{Duration, Instant};
use tempfile::TempDir;

const PLANTED: &str = "ghp_StressPlantedTokenNeverSurvives00";

/// Scans share one process-wide "newest wins" slot, so tests that scan must
/// not interleave or they would cancel each other.
static SERIAL: Mutex<()> = Mutex::new(());

fn serial() -> std::sync::MutexGuard<'static, ()> {
    SERIAL
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

fn repo() -> (TempDir, PathBuf) {
    let dir = TempDir::new().expect("tempdir");
    common::run_git(dir.path(), &["init", "-q", "-b", "main"]);
    let path = validate_repo(dir.path().to_str().unwrap()).expect("trusted fixture");
    (dir, path)
}

/// Writes an executable stub that answers `--version` and otherwise runs
/// `body`. The scan's root is `$3` (`--no-update-check scan <root> ...`).
fn stub(dir: &Path, body: &str) -> PathBuf {
    let path = dir.join("kingfisher");
    let script = format!(
        "#!/bin/sh\nif [ \"$1\" = \"--version\" ]; then echo 'kingfisher 9.9.9'; exit 0; fi\nR=\"$3\"\n{body}\n"
    );
    fs::write(&path, script).unwrap();
    fs::set_permissions(&path, fs::Permissions::from_mode(0o755)).unwrap();
    path
}

fn scan(repo: &Path, binary: &Path) -> SecretsReport {
    scan_with_binary(repo, binary, ScanTicket::claim(), Duration::from_secs(60))
}

const COMPLETE_AUDIT: &str = r#""audit":{"summary":{"scan_partial":0,"scan_failed":0,"pending":0},"repositories":[{"scan":{"status":"completed"}}]}"#;

/// A Kingfisher row whose `--redact` snippet carries `value` (hex, padded to
/// eight digits). The fingerprint is constant: grouping must not follow it.
fn finding_json(path: &str, value: &str) -> String {
    format!(
        r#"{{"rule":{{"id":"betterleaks.github-pat","name":"github-pat"}},"finding":{{"path":"{path}","line":1,"confidence":"high","fingerprint":"777","snippet":"[REDACTED:{value:0>8}]","secret":"{PLANTED}"}}}}"#
    )
}

fn assert_no_plant(report: &SecretsReport) {
    let serialized = serde_json::to_string(report).unwrap();
    assert!(
        !serialized.contains(PLANTED),
        "planted token leaked: {serialized}"
    );
}

/// A failed scan carries one short, line-oriented diagnostic and nothing the
/// child wrote. A successful or superseded scan carries none.
fn assert_failure_diagnostic(report: &SecretsReport) {
    let diagnostic = report
        .diagnostic
        .as_deref()
        .unwrap_or_else(|| panic!("failed scan has no copyable diagnostic: {report:?}"));
    assert!(diagnostic.len() < 2_000, "{}", diagnostic.len());
    assert!(!diagnostic.contains(PLANTED), "{diagnostic}");
    assert_eq!(
        diagnostic
            .lines()
            .filter(|line| line.starts_with("stdout_bytes_captured:"))
            .count(),
        1,
        "{diagnostic}"
    );
    assert_eq!(
        diagnostic
            .lines()
            .filter(|line| line.starts_with("stderr_bytes_captured:"))
            .count(),
        1,
        "{diagnostic}"
    );
}

#[test]
fn findings_are_located_through_real_git() {
    let _serial = serial();
    let (dir, root) = repo();
    let work = dir.path();
    fs::write(work.join(".gitignore"), "secret.env\nbuild/\n").unwrap();
    fs::write(work.join("tracked.txt"), "x").unwrap();
    common::run_git(work, &["add", "tracked.txt", ".gitignore"]);
    fs::write(work.join("secret.env"), "x").unwrap();
    fs::create_dir_all(work.join("build/deep")).unwrap();
    fs::write(work.join("build/deep/out.o"), "x").unwrap();
    fs::write(work.join("notes.txt"), "x").unwrap();
    fs::create_dir_all(work.join("dir with space")).unwrap();
    fs::write(work.join("dir with space/ünïcode.txt"), "x").unwrap();
    common::run_git(work, &["add", "dir with space/ünïcode.txt"]);

    let tools = TempDir::new().unwrap();
    let argv_log = tools.path().join("argv");
    let env_log = tools.path().join("env");
    let at = |rel: &str| format!("{}/{rel}", root.display());
    let findings = [
        finding_json(&at("tracked.txt"), "1"),
        finding_json(&at("secret.env"), "2"),
        finding_json(&at("build/deep/out.o"), "1"),
        finding_json(&at("notes.txt"), "3"),
        finding_json(&at(".git/config"), "4"),
        finding_json(&at("dir with space/ünïcode.txt"), "5"),
    ]
    .join(",");
    let payload = tools.path().join("payload.json");
    fs::write(
        &payload,
        format!("{{\"findings\":[{findings}],{COMPLETE_AUDIT}}}"),
    )
    .unwrap();
    let body = format!(
        "printf '%s\\n' \"$@\" > '{}'\nenv > '{}'\ncat '{}'\nexit 200",
        argv_log.display(),
        env_log.display(),
        payload.display(),
    );
    let report = scan(&root, &stub(tools.path(), &body));

    assert!(report.ok, "{:?}", report.error);
    assert!(report.diagnostic.is_none(), "{:?}", report.diagnostic);
    assert_eq!(report.completeness, ScanCompleteness::Complete);
    assert!(report.git_status_known);
    assert_eq!(report.kingfisher_version.as_deref(), Some("9.9.9"));
    assert!(report.scanned_at_ms > 0);
    let location = |path: &str| {
        report
            .findings
            .iter()
            .find(|f| f.path == path)
            .unwrap_or_else(|| panic!("{path} missing: {:?}", report.findings))
            .location
    };
    assert_eq!(location("tracked.txt"), SecretLocation::Tracked);
    assert_eq!(
        location("dir with space/ünïcode.txt"),
        SecretLocation::Tracked
    );
    assert_eq!(location("secret.env"), SecretLocation::Ignored);
    assert_eq!(location("build/deep/out.o"), SecretLocation::Ignored);
    assert_eq!(location("notes.txt"), SecretLocation::Untracked);
    assert_eq!(location(".git/config"), SecretLocation::GitMetadata);
    // Same redacted value, two files: one group.
    let group = |path: &str| {
        report
            .findings
            .iter()
            .find(|f| f.path == path)
            .unwrap()
            .secret_group
    };
    assert_eq!(group("tracked.txt"), group("build/deep/out.o"));
    assert_no_plant(&report);

    let argv = fs::read_to_string(&argv_log).unwrap();
    assert!(argv.lines().any(|l| l == "--no-dedup"), "{argv}");
    assert!(argv.lines().any(|l| l == "--no-validate"), "{argv}");
    // The child saw only the scrubbed variables plus what `sh` sets itself.
    let env = fs::read_to_string(&env_log).unwrap();
    for line in env.lines() {
        let key = line.split('=').next().unwrap_or_default();
        assert!(
            matches!(
                key,
                "PATH" | "HOME" | "NO_COLOR" | "PWD" | "SHLVL" | "_" | "OLDPWD"
            ),
            "unexpected variable reached kingfisher: {key}"
        );
    }
}

#[test]
fn malformed_and_failed_runs_fail_closed() {
    let _serial = serial();
    let (_dir, root) = repo();
    let tools = TempDir::new().unwrap();
    let cases: &[(&str, &str, &str)] = &[
        ("empty object", "echo '{}'; exit 0", "not a findings report"),
        ("non-json", "echo 'Scanning...'; exit 0", "not valid JSON"),
        ("no output", "exit 0", "not valid JSON"),
        ("jsonl rows", "echo '{\"rule\":{\"id\":\"r\"},\"finding\":{\"path\":\"/x\"}}'; echo '{\"rule\":{\"id\":\"r\"},\"finding\":{\"path\":\"/y\"}}'; exit 200", "not"),
        ("bad flag", &format!("echo '{PLANTED}' >&2; exit 2"), "exited with status 2"),
        ("signal", "kill -9 $$", "terminated before it finished"),
        ("crash with output", &format!("echo '{{\"findings\":[]}}'; echo '{PLANTED}' >&2; exit 101"), "status 101"),
    ];
    for (name, body, expected) in cases {
        let report = scan(&root, &stub(tools.path(), body));
        assert!(!report.ok, "{name}: reported ok");
        assert!(report.findings.is_empty(), "{name}");
        let error = report.error.clone().unwrap_or_default();
        assert!(error.contains(expected), "{name}: {error}");
        assert_failure_diagnostic(&report);
        assert_no_plant(&report);
    }
}

#[test]
fn a_partial_scan_is_reported_as_partial() {
    let _serial = serial();
    let (_dir, root) = repo();
    let tools = TempDir::new().unwrap();
    let body = r#"echo '{"findings":[],"audit":{"summary":{"scan_partial":1,"scan_failed":0,"pending":0},"repositories":[{"scan":{"status":"partial","error":"one or more repository inputs could not be enumerated"}}]}}'; exit 0"#;
    let report = scan(&root, &stub(tools.path(), body));
    assert!(report.ok);
    assert!(report.diagnostic.is_none(), "{:?}", report.diagnostic);
    assert_eq!(report.completeness, ScanCompleteness::Partial);
    assert!(report.findings.is_empty());
}

#[test]
fn oversized_output_fails_closed_instead_of_listing_a_prefix() {
    let _serial = serial();
    let (_dir, root) = repo();
    let tools = TempDir::new().unwrap();
    // 40 MB of a syntactically open JSON array: over the 32 MiB cap.
    let body =
        "printf '{\"findings\":['; head -c 40000000 /dev/zero | tr '\\0' ' '; echo ']}'; exit 0";
    let started = Instant::now();
    let report = scan(&root, &stub(tools.path(), body));
    assert!(!report.ok);
    assert_eq!(
        report.error.as_deref(),
        Some("kingfisher output was truncated")
    );
    assert_failure_diagnostic(&report);
    let diagnostic = report.diagnostic.as_deref().unwrap_or("");
    assert!(
        !diagnostic.contains("    "),
        "truncated stdout reached the diagnostic: {diagnostic}"
    );
    assert!(started.elapsed() < Duration::from_secs(30));
}

#[test]
fn twenty_thousand_findings_are_capped_with_both_numbers() {
    let _serial = serial();
    let (dir, root) = repo();
    let work = dir.path();
    fs::write(work.join("real.rs"), "x").unwrap();
    common::run_git(work, &["add", "real.rs"]);
    fs::write(work.join(".gitignore"), "gen/\n").unwrap();
    // One real file makes Git list the whole directory as `gen/`; the other
    // 19,999 rows are located through that ancestor entry.
    fs::create_dir_all(work.join("gen")).unwrap();
    fs::write(work.join("gen/0.txt"), "x").unwrap();

    let tools = TempDir::new().unwrap();
    let mut items: Vec<String> = (0..20_000)
        .map(|i| {
            finding_json(
                &format!("{}/gen/{i}.txt", root.display()),
                &format!("{:x}", i % 50),
            )
        })
        .collect();
    items.push(finding_json(&format!("{}/real.rs", root.display()), "abc"));
    let payload = tools.path().join("payload.json");
    fs::write(
        &payload,
        format!("{{\"findings\":[{}],{COMPLETE_AUDIT}}}", items.join(",")),
    )
    .unwrap();
    let body = format!("cat '{}'; exit 200", payload.display());
    let started = Instant::now();
    let report = scan(&root, &stub(tools.path(), &body));
    let elapsed = started.elapsed();

    assert!(report.ok, "{:?}", report.error);
    assert!(report.diagnostic.is_none(), "{:?}", report.diagnostic);
    assert_eq!(report.findings.len(), MAX_FINDINGS);
    assert_eq!(report.findings_total, 20_001);
    assert_eq!(report.findings[0].path, "real.rs");
    assert_eq!(report.findings[0].location, SecretLocation::Tracked);
    assert!(report.findings[1..]
        .iter()
        .all(|f| f.location == SecretLocation::Ignored));
    // 50 distinct values among the generated rows plus the real one.
    let groups: std::collections::HashSet<u32> =
        report.findings.iter().map(|f| f.secret_group).collect();
    assert!(groups.len() <= 51);
    assert_no_plant(&report);
    assert!(
        elapsed < Duration::from_secs(20),
        "20k findings took {elapsed:?}"
    );
}

#[test]
fn a_hung_scanner_is_killed_at_the_deadline() {
    let _serial = serial();
    let (_dir, root) = repo();
    let tools = TempDir::new().unwrap();
    let started = Instant::now();
    let marker = "stderr-marker-not-forwarded";
    let report = scan_with_binary(
        &root,
        &stub(
            tools.path(),
            &format!("echo '{marker}' >&2; sleep 30; echo '{{\"findings\":[]}}'"),
        ),
        ScanTicket::claim(),
        Duration::from_secs(1),
    );
    assert!(!report.ok);
    assert_eq!(
        report.error.as_deref(),
        Some("kingfisher did not finish within 1s")
    );
    assert_failure_diagnostic(&report);
    let diagnostic = report.diagnostic.as_deref().unwrap_or("");
    assert!(diagnostic.contains("deadline_s: 1"), "{diagnostic}");
    assert!(
        diagnostic.contains("stdout_bytes_captured: 0"),
        "{diagnostic}"
    );
    assert!(
        diagnostic
            .lines()
            .any(|line| line.starts_with("stderr_bytes_captured: ") && !line.ends_with(": 0")),
        "{diagnostic}"
    );
    let dumped = format!("{report:?}");
    assert!(
        !dumped.contains(marker),
        "scanner stderr reached the report"
    );
    assert!(
        started.elapsed() < Duration::from_secs(10),
        "{:?}",
        started.elapsed()
    );
}

/// Both streams get noisy before the deadline. Counts stay, the text does not,
/// and the diagnostic cannot grow with the flood.
#[test]
fn a_noisy_timeout_keeps_counts_and_drops_both_streams() {
    let _serial = serial();
    let (_dir, root) = repo();
    let tools = TempDir::new().unwrap();
    let marker = "stdout-marker-not-forwarded";
    let report = scan_with_binary(
        &root,
        &stub(
            tools.path(),
            &format!(
                "printf '%s' '{marker}'; dd if=/dev/zero bs=1024 count=256 2>/dev/null | tr '\\0' x >&2; sleep 30"
            ),
        ),
        ScanTicket::claim(),
        Duration::from_secs(1),
    );
    assert!(!report.ok, "{:?}", report.error);
    assert_failure_diagnostic(&report);
    let diagnostic = report.diagnostic.as_deref().unwrap_or("");
    assert!(
        diagnostic
            .lines()
            .any(|line| line.starts_with("stdout_bytes_captured: ") && !line.ends_with(": 0")),
        "{diagnostic}"
    );
    assert!(
        diagnostic
            .lines()
            .any(|line| line.starts_with("stderr_bytes_captured: ") && !line.ends_with(": 0")),
        "{diagnostic}"
    );
    let serialized = serde_json::to_string(&report).unwrap();
    assert!(!serialized.contains(marker), "{serialized}");
    assert!(
        !serialized.contains(&"x".repeat(40)),
        "stderr flood reached the report"
    );
}

#[test]
fn a_version_probe_cannot_smuggle_a_token() {
    let _serial = serial();
    let (_dir, root) = repo();
    let tools = TempDir::new().unwrap();
    let token = "ghp_0123456789abcdefghijklmnopqrstuvwxyzA";
    let binary = tools.path().join("kingfisher");
    let script = format!(
        "#!/bin/sh\nif [ \"$1\" = \"--version\" ]; then echo '{token}'; exit 0; fi\necho '{{\"findings\":[],{COMPLETE_AUDIT}}}'\nexit 0\n"
    );
    fs::write(&binary, script).unwrap();
    fs::set_permissions(&binary, fs::Permissions::from_mode(0o755)).unwrap();
    let report = scan(&root, &binary);
    assert!(report.ok, "{:?}", report.error);
    assert!(report.diagnostic.is_none(), "{:?}", report.diagnostic);
    let serialized = serde_json::to_string(&report).unwrap();
    assert!(!serialized.contains(token), "{serialized}");
    assert!(report.kingfisher_version.is_some());
}

/// Against the real Kingfisher, when one is installed: the shapes the stubs
/// above imitate must be the shapes it actually prints. Absent, the check
/// says so and returns, like `deps_live_scan.rs`.
#[test]
fn live_kingfisher_report_holds_end_to_end() {
    let _serial = serial();
    let Some(binary) = gitpulse_lib::secrets::resolve_kingfisher() else {
        eprintln!("SKIPPED live_kingfisher_report_holds_end_to_end: kingfisher is not installed");
        return;
    };
    let (dir, root) = repo();
    let work = dir.path();
    // Assembled at runtime so this source file is not itself a finding.
    let token = format!("ghp_{}", "aB3dE5fG7hJ9kL1mN3pQ5rS7tU9vW1xY3zA5");
    let other = format!("ghp_{}", "Zz9yX8wV7uT6sR5qP4oN3mL2kJ1iH0gF9eD8");
    fs::write(work.join(".gitignore"), "local.env\n").unwrap();
    fs::write(work.join("ci.sh"), format!("token = \"{token}\"\n")).unwrap();
    fs::create_dir_all(work.join("docs")).unwrap();
    fs::write(work.join("docs/copy.sh"), format!("T=\"{token}\"\n")).unwrap();
    common::run_git(work, &["add", "ci.sh", ".gitignore"]);
    fs::write(work.join("local.env"), format!("GITHUB={other}\n")).unwrap();
    common::run_git(
        work,
        &[
            "remote",
            "add",
            "origin",
            &format!("https://{other}@github.com/acme/repo"),
        ],
    );
    let locked = work.join("locked.env");
    fs::write(&locked, format!("K={other}\n")).unwrap();
    fs::set_permissions(&locked, fs::Permissions::from_mode(0o000)).unwrap();

    let report = scan_with_binary(
        &root,
        &binary,
        ScanTicket::claim(),
        Duration::from_secs(120),
    );
    fs::set_permissions(&locked, fs::Permissions::from_mode(0o644)).unwrap();

    assert!(report.ok, "{:?}", report.error);
    assert!(report.diagnostic.is_none(), "{:?}", report.diagnostic);
    assert!(report.kingfisher_version.is_some());
    // The unreadable file is the only thing standing between this scan and a
    // complete one, and only Kingfisher's audit says so.
    assert_eq!(report.completeness, ScanCompleteness::Partial);
    let row = |path: &str| {
        report
            .findings
            .iter()
            .find(|f| f.path == path)
            .unwrap_or_else(|| panic!("{path} missing: {:?}", report.findings))
            .clone()
    };
    assert_eq!(row("ci.sh").location, SecretLocation::Tracked);
    assert_eq!(row("docs/copy.sh").location, SecretLocation::Untracked);
    assert_eq!(row("local.env").location, SecretLocation::Ignored);
    assert_eq!(row(".git/config").location, SecretLocation::GitMetadata);
    // `--no-dedup` keeps both locations of the one token, grouped.
    assert_eq!(row("ci.sh").secret_group, row("docs/copy.sh").secret_group);
    assert_ne!(row("ci.sh").secret_group, row("local.env").secret_group);
    assert_eq!(report.findings[0].location, SecretLocation::GitMetadata);
    let serialized = serde_json::to_string(&report).unwrap();
    for secret in [&token, &other] {
        assert!(!serialized.contains(secret.as_str()), "value leaked");
    }
    assert!(
        !serialized.contains(&work.display().to_string()),
        "absolute path leaked"
    );
}

#[test]
fn a_superseded_scan_stuck_in_its_version_probe_does_not_delay_the_next() {
    let _serial = serial();
    let (_dir, root) = repo();
    let tools = TempDir::new().unwrap();
    let marker = tools.path().join("first-probe");
    // The first `--version` hangs for four seconds; later ones answer at once.
    let script = format!(
        "#!/bin/sh\nif [ \"$1\" = \"--version\" ]; then\n  if [ ! -e '{m}' ]; then : > '{m}'; sleep 4; fi\n  echo 'kingfisher 9.9.9'; exit 0\nfi\necho '{{\"findings\":[],{COMPLETE_AUDIT}}}'\nexit 0\n",
        m = marker.display()
    );
    let binary = tools.path().join("kingfisher");
    fs::write(&binary, script).unwrap();
    fs::set_permissions(&binary, fs::Permissions::from_mode(0o755)).unwrap();

    let first = {
        let ticket = ScanTicket::claim();
        let (root, binary) = (root.clone(), binary.clone());
        std::thread::spawn(move || {
            scan_with_binary(&root, &binary, ticket, Duration::from_secs(60))
        })
    };
    while !marker.exists() {
        std::thread::sleep(Duration::from_millis(10));
    }
    let started = Instant::now();
    let second = scan_with_binary(&root, &binary, ScanTicket::claim(), Duration::from_secs(60));
    let waited = started.elapsed();
    let first = first.join().unwrap();

    assert_eq!(
        first.error.as_deref(),
        Some("superseded by a newer secrets scan")
    );
    assert!(first.diagnostic.is_none(), "{:?}", first.diagnostic);
    assert!(second.ok, "{:?}", second.error);
    assert!(second.diagnostic.is_none(), "{:?}", second.diagnostic);
    assert!(
        waited < Duration::from_secs(2),
        "newest scan waited {waited:?} behind a dead one"
    );
}

#[test]
fn superseded_scans_stop_and_never_overlap() {
    let _serial = serial();
    let (_dir, root) = repo();
    let tools = TempDir::new().unwrap();
    let pids = tools.path().join("pids");
    let overlap = tools.path().join("overlap");
    // Each scan first checks that no earlier scan's process is still alive,
    // then records itself and runs for two seconds.
    let body = format!(
        "for p in $(cat '{pids}' 2>/dev/null); do if kill -0 \"$p\" 2>/dev/null; then echo \"$p\" >> '{overlap}'; fi; done\n\
         echo $$ >> '{pids}'\nsleep 2\necho '{{\"findings\":[],{COMPLETE_AUDIT}}}'\nexit 0",
        pids = pids.display(),
        overlap = overlap.display(),
    );
    let binary = stub(tools.path(), &body);

    const SCANS: usize = 6;
    let started = Instant::now();
    let mut handles = Vec::new();
    for _ in 0..SCANS {
        let ticket = ScanTicket::claim();
        let (root, binary) = (root.clone(), binary.clone());
        handles.push(std::thread::spawn(move || {
            scan_with_binary(&root, &binary, ticket, Duration::from_secs(60))
        }));
        std::thread::sleep(Duration::from_millis(400));
    }
    let reports: Vec<SecretsReport> = handles.into_iter().map(|h| h.join().unwrap()).collect();
    let elapsed = started.elapsed();

    for (i, report) in reports[..SCANS - 1].iter().enumerate() {
        assert!(!report.ok, "scan {i} should have been superseded");
        assert_eq!(
            report.error.as_deref(),
            Some("superseded by a newer secrets scan"),
            "scan {i}"
        );
        assert!(
            report.diagnostic.is_none(),
            "scan {i} diagnostic: {:?}",
            report.diagnostic
        );
    }
    let last = &reports[SCANS - 1];
    assert!(last.ok, "{:?}", last.error);
    assert!(last.diagnostic.is_none(), "{:?}", last.diagnostic);
    assert_eq!(last.completeness, ScanCompleteness::Complete);
    assert!(
        !overlap.exists(),
        "two scanners ran at once: {}",
        fs::read_to_string(&overlap).unwrap_or_default()
    );
    let spawned = fs::read_to_string(&pids)
        .unwrap_or_default()
        .lines()
        .count();
    assert!(
        spawned >= 2,
        "superseding never interrupted a running scan ({spawned} spawned)"
    );
    // Six back-to-back two-second scans would take 12s; killing the
    // superseded ones leaves the last scan plus the stagger.
    assert!(elapsed < Duration::from_secs(12), "{elapsed:?}");
}
