//! Tool lookup, scrubbed spawn, fixed argv, and scan scheduling for Kingfisher.

use super::parse::{parse_kingfisher_json, SecretsReport};
use super::report::{assemble, git_view, scan_exclusions, RunFacts};
use crate::engine::process::{run_observed, ExitFacts, Incomplete, OutputStream, ProcessObserver};
use crate::ipc_error::CommandError;
use std::collections::BTreeMap;
use std::ffi::{OsStr, OsString};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

/// Wall-clock budget for one working-tree secrets scan.
pub const SCAN_DEADLINE: Duration = Duration::from_secs(5 * 60);
/// Version probe budget.
pub const VERSION_DEADLINE: Duration = Duration::from_secs(5);
/// Stdout budget. Hitting it fails the scan entirely — never a partial list.
/// Sized for `--no-dedup`: a finding is ~0.9 KB of JSON, so this holds
/// roughly 35,000 rows before the scan reports itself truncated.
pub const STDOUT_CAP: usize = 32 * 1024 * 1024;
/// Kingfisher exit codes that mean a completed scan (no findings / findings /
/// validated findings). Any other status is a failure.
pub const SUCCESS_EXITS: [i32; 3] = [0, 200, 205];
/// Passed explicitly rather than inherited from Kingfisher's default, so the
/// limit the panel names is one this code chose. Larger files are skipped
/// with no entry in Kingfisher's audit (measured on 2.7.0).
pub const MAX_FILE_SIZE_MB: u32 = 256;

const SUPERSEDED: &str = "superseded by a newer secrets scan";

/// Kingfisher's `scan_nested_repos` defaults to true and lacks `ArgAction::Set`,
/// so `--scan-nested-repos false` is not accepted. Our scans therefore walk
/// nested repositories; the report must say so.
pub fn nested_repos_scanning_enabled() -> bool {
    true
}

/// Scanner threads. Kingfisher defaults to one per core and measured 974% CPU
/// on this repository; half the machine keeps the app it runs inside usable.
fn scan_jobs() -> usize {
    std::thread::available_parallelism()
        .map(|n| n.get() / 2)
        .unwrap_or(1)
        .max(1)
}

/// Fixed argv after the binary path. Nothing else — no repo config, no
/// self-update, no validation, no git history.
///
/// `--no-dedup` because Kingfisher's default reports one location per secret
/// value: the same token in two files rendered as one row, and rotating it
/// means finding both. Inline `kingfisher:ignore` directives stay honoured
/// (no `--no-ignore`): they are the repository author's recorded decision,
/// and the panel says they apply.
///
/// `excluded` are absolute directories left out with `--exclude`
/// ([`super::report::scan_exclusions`] chooses them and keeps out any path
/// that would read as a glob).
pub fn build_scan_argv(repo: impl AsRef<Path>, excluded: &[PathBuf]) -> Vec<String> {
    let mut argv: Vec<String> = vec![
        "--no-update-check".into(),
        "scan".into(),
        repo.as_ref().to_string_lossy().into_owned(),
        "--format".into(),
        "json".into(),
        "--no-validate".into(),
        "--git-history".into(),
        "none".into(),
        "--redact".into(),
        "--no-dedup".into(),
        "--confidence".into(),
        "medium".into(),
        "--max-file-size".into(),
        MAX_FILE_SIZE_MB.to_string(),
        "--jobs".into(),
        scan_jobs().to_string(),
        "--quiet".into(),
    ];
    for tree in excluded {
        argv.push("--exclude".into());
        argv.push(tree.to_string_lossy().into_owned());
    }
    argv
}

/// Linked worktrees of `repo`, canonical, for [`super::report::scan_exclusions`]
/// to keep the ones inside it. A listing that cannot be read leaves none out:
/// the scan is then slower, never narrower.
fn linked_worktrees(repo: &Path) -> Vec<PathBuf> {
    match crate::engine::worktree::read_worktree_entries(repo) {
        Ok(entries) => entries
            .into_iter()
            .map(|entry| PathBuf::from(entry.path))
            .map(|path| path.canonicalize().unwrap_or(path))
            .filter(|path| path != repo)
            .collect(),
        Err(error) => {
            log::info!(
                target: "secrets",
                "worktrees not listed, so none are left out of the scan: {}",
                error.message()
            );
            Vec::new()
        }
    }
}

pub fn build_version_argv() -> [&'static str; 1] {
    ["--version"]
}

/// Environment map for the child: `PATH`, `HOME`, and `NO_COLOR` only.
///
/// Ambient credentials (`AWS_*`, `GITHUB_TOKEN`, `KF_*`, `KINGFISHER_*`, …)
/// must not reach the process even if `--no-validate` were ever omitted.
/// Nothing is taken from the caller but these two values, so there is no
/// parameter through which a credential could be passed in.
pub fn build_scrubbed_env(
    path: Option<&OsStr>,
    home: Option<&OsStr>,
) -> BTreeMap<String, OsString> {
    let mut env = BTreeMap::new();
    if let Some(path) = path {
        env.insert("PATH".into(), path.to_os_string());
    }
    if let Some(home) = home {
        env.insert("HOME".into(), home.to_os_string());
    }
    env.insert("NO_COLOR".into(), OsString::from("1"));
    env
}

fn process_scrubbed_env() -> BTreeMap<String, OsString> {
    build_scrubbed_env(
        std::env::var_os("PATH").as_deref(),
        std::env::var_os("HOME")
            .or_else(|| std::env::var_os("USERPROFILE"))
            .as_deref(),
    )
}

/// Resolve `kingfisher` the way every other external tool is resolved: PATH
/// first, then the GUI-launch fallback directories. PATH alone was wrong for
/// the launch that matters most — a Dock-launched app inherits launchd's
/// `/usr/bin:/bin:/usr/sbin:/sbin`, so a Homebrew install was reported as
/// "not installed".
pub fn resolve_kingfisher() -> Option<PathBuf> {
    crate::engine::process::find_external_tool("kingfisher").map(PathBuf::from)
}

/// [`resolve_kingfisher`] under an injected environment, for reproducing a
/// GUI launch in tests.
#[cfg(test)]
#[cfg(unix)]
pub(super) fn resolve_kingfisher_with(
    path_var: Option<&OsStr>,
    home: Option<&OsStr>,
) -> Option<PathBuf> {
    crate::engine::process::find_external_tool_with("kingfisher", path_var, home).map(PathBuf::from)
}

fn scrubbed_command(binary: &Path) -> Command {
    let mut cmd = Command::new(binary);
    cmd.env_clear();
    for (key, value) in process_scrubbed_env() {
        cmd.env(key, value);
    }
    cmd.stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped());
    cmd
}

pub fn failed_report(reason: &str, version: Option<String>) -> SecretsReport {
    SecretsReport::failed(reason, version)
}

/// The one reason a non-success exit is reported with. A signal has no exit
/// status on Unix (`code()` is `None`, carried as -1), and saying "exited
/// with status -1" names a status that never existed.
///
/// When the signal is known it is named, and a fault the scanner raised itself
/// (SIGBUS, SIGSEGV, ...) is a crash, not a termination: a crash repeats on
/// the same input, a termination does not, and the person reading the banner
/// needs to know which they are looking at. A scan GitPulse stopped never gets
/// here (it is reported as superseded first), nor does one that hit its
/// deadline.
fn exit_reason(status: i32, signal: Option<i32>) -> String {
    if let Some(signal) = signal {
        let name = signal_name(signal);
        return if is_crash_signal(signal) {
            format!("kingfisher crashed ({name})")
        } else {
            format!("kingfisher was stopped by {name} before it finished")
        };
    }
    if cfg!(unix) && status == -1 {
        "kingfisher was terminated before it finished".into()
    } else {
        format!("kingfisher exited with status {status}")
    }
}

/// Whether `signal` is one a process raises on itself by faulting, as opposed
/// to one somebody sent it.
fn is_crash_signal(signal: i32) -> bool {
    #[cfg(unix)]
    {
        matches!(
            signal,
            libc::SIGBUS
                | libc::SIGSEGV
                | libc::SIGILL
                | libc::SIGABRT
                | libc::SIGFPE
                | libc::SIGTRAP
        )
    }
    #[cfg(not(unix))]
    {
        let _ = signal;
        false
    }
}

fn signal_name(signal: i32) -> String {
    #[cfg(unix)]
    {
        let known = [
            (libc::SIGBUS, "SIGBUS"),
            (libc::SIGSEGV, "SIGSEGV"),
            (libc::SIGILL, "SIGILL"),
            (libc::SIGABRT, "SIGABRT"),
            (libc::SIGFPE, "SIGFPE"),
            (libc::SIGTRAP, "SIGTRAP"),
            (libc::SIGKILL, "SIGKILL"),
            (libc::SIGTERM, "SIGTERM"),
            (libc::SIGINT, "SIGINT"),
            (libc::SIGHUP, "SIGHUP"),
            (libc::SIGPIPE, "SIGPIPE"),
        ];
        if let Some((_, name)) = known.iter().find(|(number, _)| *number == signal) {
            return (*name).to_string();
        }
    }
    format!("signal {signal}")
}

/// Map a finished child (any exit) into a report without retaining stdout,
/// locating findings under `root` with `view`.
pub(crate) fn report_from_run(
    status: i32,
    signal: Option<i32>,
    stdout: &[u8],
    version: Option<String>,
    root: &Path,
    view: impl FnOnce() -> Result<super::report::GitView, CommandError>,
    facts: RunFacts,
) -> SecretsReport {
    if !SUCCESS_EXITS.contains(&status) {
        return failed_report(&exit_reason(status, signal), version);
    }
    match parse_kingfisher_json(stdout, version.clone()) {
        Ok(envelope) => assemble(
            envelope,
            root,
            view().map_err(CommandError::into_message),
            facts,
        ),
        Err(reason) => failed_report(&reason, version),
    }
}

/// [`report_from_run`] for callers with no repository to classify against:
/// every path stays unlocated. Kept for the unit tests of exit and parse
/// handling, which are about the envelope rather than the tree.
pub fn report_from_exit(status: i32, stdout: &[u8], version: Option<String>) -> SecretsReport {
    report_from_run(
        status,
        None,
        stdout,
        version,
        Path::new("/repo"),
        || Err("no repository".into()),
        RunFacts {
            scanned_at_ms: 0,
            duration_ms: 0,
            nested_repos_scanned: nested_repos_scanning_enabled(),
            max_file_size_mb: MAX_FILE_SIZE_MB,
            excluded_paths: Vec::new(),
        },
    )
}

/// The newest request's cancel flag. Starting a scan raises the previous
/// one's, so a burst of repository switches leaves exactly one Kingfisher
/// running instead of a queue of full-machine scans nobody is waiting for.
static LATEST: Mutex<Option<Arc<AtomicBool>>> = Mutex::new(None);
/// Held for the life of one scan: at most one Kingfisher at a time.
static RUNNING: Mutex<()> = Mutex::new(());

/// A scan's claim on being the newest request.
pub struct ScanTicket {
    cancel: Arc<AtomicBool>,
    stdout_bytes: u64,
    stderr_bytes: u64,
    /// How the last process this ticket observed was waited on, as the runner
    /// saw it. Cleared with the byte counts, so a version probe's death is
    /// never read as the scan's.
    exit: Option<ExitFacts>,
}

impl ScanTicket {
    /// Register a new scan and cancel whichever one was newest before it.
    pub fn claim() -> Self {
        let flag = Arc::new(AtomicBool::new(false));
        let mut latest = LATEST.lock().unwrap_or_else(PoisonError::into_inner);
        if let Some(previous) = latest.replace(Arc::clone(&flag)) {
            previous.store(true, Ordering::SeqCst);
        }
        Self {
            cancel: flag,
            stdout_bytes: 0,
            stderr_bytes: 0,
            exit: None,
        }
    }

    pub fn superseded(&self) -> bool {
        self.cancel.load(Ordering::SeqCst)
    }

    /// The version probe and the scan share one ticket. Drop the probe's
    /// bytes so a timed-out scan does not report them as its own output.
    fn reset_output_counts(&mut self) {
        self.stdout_bytes = 0;
        self.stderr_bytes = 0;
        self.exit = None;
    }
}

impl ProcessObserver for ScanTicket {
    fn cancelled(&self) -> bool {
        self.superseded()
    }

    fn output(&mut self, stream: OutputStream, bytes: &[u8]) {
        let n = u64::try_from(bytes.len()).unwrap_or(u64::MAX);
        let slot = match stream {
            OutputStream::Stdout => &mut self.stdout_bytes,
            OutputStream::Stderr => &mut self.stderr_bytes,
        };
        *slot = slot.saturating_add(n);
    }

    fn exited(&mut self, facts: &ExitFacts) {
        self.exit = Some(*facts);
    }
}

/// Facts a failed scan can name without quoting Kingfisher. Byte counts are
/// how much was captured, not how much the child produced past the cap.
pub(super) struct ScanDiagnostic {
    pub version: Option<String>,
    pub binary: String,
    pub jobs: String,
    pub deadline_s: u64,
    pub elapsed_ms: u64,
    pub stdout_bytes_captured: u64,
    pub stderr_bytes_captured: u64,
    /// What the runner knew about how the scanner's wait ended; `None` when
    /// the run never got as far as a wait.
    pub exit: Option<ExitFacts>,
    /// Whether a newer scan had claimed the ticket by the time this was read.
    pub superseded: bool,
}

/// One diagnostic field.
///
/// Control characters become spaces, so a newline in a path or a version
/// cannot open a forged line. The value is redacted before the byte cap:
/// cutting first keeps a prefix of a token that is too short for the
/// redactor to recognise.
fn one_field(value: &str) -> String {
    const MAX_BYTES: usize = 512;
    let collapsed: String = value
        .chars()
        .map(|ch| if ch.is_control() { ' ' } else { ch })
        .collect();
    let redacted = crate::ledger::redact::text(&collapsed);
    let mut out = String::new();
    for ch in redacted.chars() {
        if out.len() + ch.len_utf8() > MAX_BYTES {
            break;
        }
        out.push(ch);
    }
    out
}

/// One copyable block. Credential-shaped text is stripped before this leaves
/// the process; the streams themselves are never included.
pub(super) fn format_scan_diagnostic(facts: &ScanDiagnostic) -> String {
    let version = one_field(facts.version.as_deref().unwrap_or("unknown"));
    let binary = one_field(&facts.binary);
    let jobs = one_field(&facts.jobs);
    let raw = format!(
        "kingfisher: {version}\nbinary: {binary}\njobs: {jobs}\ndeadline_s: {}\nelapsed_ms: {}\nstdout_bytes_captured: {}\nstderr_bytes_captured: {}",
        facts.deadline_s,
        facts.elapsed_ms,
        facts.stdout_bytes_captured,
        facts.stderr_bytes_captured,
    );
    let raw = format!("{raw}\n{}", exit_lines(facts));
    crate::ledger::redact::text(&raw)
}

/// `NAME (n)` for a known signal, `signal n` otherwise, so a number is never
/// printed twice.
fn signal_label(signal: i32) -> String {
    let name = signal_name(signal);
    if name.starts_with("signal ") {
        name
    } else {
        format!("{name} ({signal})")
    }
}

/// The lines that say how the scanner's wait ended. A scan that "was terminated
/// before it finished" has no exit status to quote, and the cause was not
/// recoverable from the banner; these are every fact the runner holds, each
/// printed whether or not it is interesting, so an absent fact reads as absent
/// and a line that says `none` was observed to be none.
fn exit_lines(facts: &ScanDiagnostic) -> String {
    let token = facts.superseded || facts.exit.is_some_and(|exit| exit.cancelled);
    let Some(exit) = facts.exit else {
        return format!("wait_status_raw: not observed\ncancel_token_fired: {token}");
    };
    let raw = exit.raw_wait_status.map_or_else(
        || "none (no status was read)".to_string(),
        |raw| format!("{raw:#06x}"),
    );
    let signal = exit.signal.map_or_else(|| "none".to_string(), signal_label);
    let stopped = exit
        .stopped_signal
        .map_or_else(|| "none".to_string(), signal_label);
    format!(
        "wait_status_raw: {raw}\nexit_signal: {signal}\ncore_dumped: {}\nstopped_signal: {stopped}\ncontinued: {}\ncancel_token_fired: {token}\nrunner_signalled_group: {}\ndeadline_expired: {}\nshutdown_sweep_started: {}",
        exit.core_dumped,
        exit.continued,
        exit.runner_signalled_group,
        exit.deadline_expired,
        exit.shutdown_sweep_started,
    )
}

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| u64::try_from(d.as_millis()).unwrap_or(u64::MAX))
        .unwrap_or(0)
}

/// Probe version, scan the working tree, parse by allowlist, drop stdout, with
/// the binary and deadline injected so stub scanners can drive every path
/// without touching `PATH`.
/// `repo` must already have passed the trust gate.
pub fn scan_with_binary(
    repo: &Path,
    binary: &Path,
    mut ticket: ScanTicket,
    deadline: Duration,
) -> SecretsReport {
    let _running = RUNNING.lock().unwrap_or_else(PoisonError::into_inner);
    if ticket.superseded() {
        return failed_report(SUPERSEDED, None);
    }
    let version = probe_version(binary, &mut ticket);
    ticket.reset_output_counts();
    if ticket.superseded() {
        return failed_report(SUPERSEDED, version);
    }
    // Read before the scan so the same listing chooses what is left out and
    // locates what is found.
    let view = git_view(repo);
    let excluded_paths = scan_exclusions(repo, view.as_ref().ok(), &linked_worktrees(repo));
    let excluded: Vec<PathBuf> = excluded_paths
        .iter()
        .map(|rel| repo.join(rel.trim_end_matches('/')))
        .collect();
    let argv = build_scan_argv(repo, &excluded);
    let jobs = argv_value(&argv, "--jobs");
    let mut cmd = scrubbed_command(binary);
    cmd.args(&argv);

    let started = Instant::now();
    let run = match run_observed(
        &mut cmd,
        "kingfisher",
        deadline,
        None,
        STDOUT_CAP,
        &mut ticket,
    ) {
        Ok(run) => run,
        Err(err) => {
            let reason = runner_failure_reason(err.message(), deadline);
            let diag = current_diagnostic(
                version.as_deref(),
                binary,
                &jobs,
                deadline,
                &started,
                &ticket,
            );
            return failed_with_diagnostic(&reason, &diag);
        }
    };
    let duration_ms = u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX);

    // Destructure so stderr is dropped here and never reaches the report.
    // Byte counts were already taken by the ticket; the bytes stay here.
    let crate::engine::process::BoundedRun {
        stdout,
        stderr: _stderr,
        status_code,
        signal,
        incomplete,
        cancelled,
        ..
    } = run;

    // Before the exit status: a cancelled child was killed by us, and its
    // status (-1) is not something Kingfisher said. A child that died by a
    // signal nobody here sent (`cancelled` is false) crashed before any newer
    // scan could have mattered, so it is reported as that and not as
    // superseded.
    if cancelled || (ticket.superseded() && signal.is_none()) {
        return failed_report(SUPERSEDED, version);
    }
    if let Some(incomplete) = incomplete {
        let reason = match incomplete {
            Incomplete::OverCap(_) => "kingfisher output was truncated",
            Incomplete::Unread(_) => "kingfisher output could not be read to the end",
            Incomplete::Deadline { .. } => "kingfisher did not finish within its deadline",
        };
        let diag = current_diagnostic(
            version.as_deref(),
            binary,
            &jobs,
            deadline,
            &started,
            &ticket,
        );
        return failed_with_diagnostic(reason, &diag);
    }

    let mut report = report_from_run(
        status_code,
        signal,
        &stdout,
        version.clone(),
        repo,
        || view,
        RunFacts {
            scanned_at_ms: now_ms(),
            duration_ms,
            nested_repos_scanned: nested_repos_scanning_enabled(),
            max_file_size_mb: MAX_FILE_SIZE_MB,
            excluded_paths,
        },
    );
    if !report.ok {
        let reason = report
            .error
            .clone()
            .unwrap_or_else(|| "kingfisher failed to run".into());
        let diag = current_diagnostic(
            version.as_deref(),
            binary,
            &jobs,
            deadline,
            &started,
            &ticket,
        );
        note_failure(&mut report, &reason, &diag);
    }
    report
}

fn argv_value(argv: &[String], flag: &str) -> String {
    argv.iter()
        .position(|arg| arg == flag)
        .and_then(|index| argv.get(index + 1))
        .cloned()
        .unwrap_or_else(|| "unknown".into())
}

fn current_diagnostic(
    version: Option<&str>,
    binary: &Path,
    jobs: &str,
    deadline: Duration,
    started: &Instant,
    ticket: &ScanTicket,
) -> ScanDiagnostic {
    ScanDiagnostic {
        version: version.map(|s| s.to_string()),
        binary: binary.display().to_string(),
        jobs: jobs.to_string(),
        deadline_s: deadline.as_secs(),
        elapsed_ms: u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX),
        stdout_bytes_captured: ticket.stdout_bytes,
        stderr_bytes_captured: ticket.stderr_bytes,
        exit: ticket.exit,
        superseded: ticket.superseded(),
    }
}

fn failed_with_diagnostic(reason: &str, facts: &ScanDiagnostic) -> SecretsReport {
    let mut report = failed_report(reason, facts.version.clone());
    note_failure(&mut report, reason, facts);
    report
}

/// Log the failure and attach the copyable facts. Superseded scans are the
/// ordinary result of switching repositories, so they stay quiet.
fn note_failure(report: &mut SecretsReport, reason: &str, facts: &ScanDiagnostic) {
    if reason == SUPERSEDED {
        return;
    }
    let diagnostic = format_scan_diagnostic(facts);
    log::warn!(
        target: "secrets",
        "{reason} | {}",
        diagnostic.replace('\n', " | ")
    );
    report.diagnostic = Some(diagnostic);
}

/// Fixed wording for a runner error. The runner's own message can carry a
/// path, so none of it is forwarded — but each cause gets its own sentence:
/// "timed out waiting for a process slot" is not the scan timing out.
pub(super) fn runner_failure_reason(err: &str, deadline: Duration) -> String {
    if crate::engine::process::is_deferred_under_load(err) {
        "kingfisher was deferred: GitPulse was under load and started no new process in time".into()
    } else if crate::engine::process::is_slot_wait_timeout(err) {
        "kingfisher could not start: no process slot became free in time".into()
    } else if err.contains("cancelled before spawn") {
        SUPERSEDED.into()
    } else if err.contains(" timed out after ") {
        format!("kingfisher did not finish within {}s", deadline.as_secs())
    } else if err.starts_with("Failed to spawn") {
        "kingfisher could not be started".into()
    } else {
        "kingfisher failed to run".into()
    }
}

/// Observes the ticket too: a superseded scan holds [`RUNNING`], so a probe
/// that hung for its full deadline would hold the newest scan back with it.
fn probe_version(binary: &Path, ticket: &mut ScanTicket) -> Option<String> {
    let mut cmd = scrubbed_command(binary);
    cmd.args(build_version_argv());
    let run = run_observed(
        &mut cmd,
        "kingfisher",
        VERSION_DEADLINE,
        None,
        64 * 1024,
        ticket,
    )
    .ok()?;
    if run.incomplete.is_some() || !run.success {
        return None;
    }
    let text = String::from_utf8_lossy(&run.stdout);
    let line = text.lines().next()?.trim();
    if line.is_empty() {
        return None;
    }
    // Same cleaner the JSON path uses, so a `--version` line cannot carry a
    // token or a second line onto the report.
    super::parse::clean_version(line)
}
