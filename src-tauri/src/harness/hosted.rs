//! Hosting `manvi serve` outside this process, so managed runs can outlive it.
//!
//! The stdio sidecar ([`super::sidecar`]) lives and dies with GitPulse, and a
//! managed Codex or Claude run lives in its memory: closing it cancels every
//! run. Manvi can instead serve the same protocol on a unix socket
//! (`manvi serve --listen`), where the server and the runs it hosts survive the
//! program that started it and a later program reconnects to them. When the
//! person chooses it (`keep_terminals_running`, the one setting that already
//! says "keep my agents running when GitPulse quits"), the profile worker is
//! hosted that way; with the setting off, or an installed Manvi that cannot do
//! it, nothing here runs and the stdio child is exactly what it was.
//!
//! This module owns what is not wire framing: where the server's files live,
//! whether a Manvi can be hosted, reaching a socket without trusting it, the
//! record that says whose server it is, and the one-line account of all of it
//! that Settings and the quit question read. The framing, correlation and
//! deadlines stay in `sidecar.rs`, which speaks to a socket exactly as it does
//! to a pipe.
//!
//! **Trust.** The socket's directory is `0700`, the socket `0600`, and Manvi
//! checks every peer's uid; this side checks the same of the server it dials
//! (the socket must be a socket, ours, answering from our uid) before a byte
//! is sent. The peer uid is the only authentication there is: any process of
//! the same user can drive the server, as it could already write the profile
//! database the server uses. Whose server it is, is a separate question,
//! answered by the [`Record`] GitPulse wrote when it started that server.

// Hosting is a unix feature (a unix socket, `setsid`); the account of it that
// Settings reads is not, so the unix-only items are gated one by one.
#![cfg_attr(not(unix), allow(dead_code, unused_imports))]

#[cfg(unix)]
use std::collections::HashMap;
#[cfg(unix)]
use std::io::Read;
#[cfg(unix)]
use std::os::unix::fs::{FileTypeExt, MetadataExt, OpenOptionsExt};
#[cfg(unix)]
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicBool, AtomicU8, Ordering};
#[cfg(unix)]
use std::sync::OnceLock;
use std::sync::{Arc, Mutex};
use std::time::Duration;
#[cfg(unix)]
use std::time::Instant;

use serde::{Deserialize, Serialize};

use super::protocol::PROTOCOL_VERSION;
use super::sidecar::ModelSelection;

/// The server's directory, inside GitPulse's config directory.
pub(crate) const DIR_NAME: &str = "manvi-host";
const SOCKET_NAME: &str = "serve.sock";
const RECORD_NAME: &str = "host.json";
const LOG_NAME: &str = "serve.log";
/// The server's own state directory: persistent, because a server that
/// outlives GitPulse must not keep its state in a per-boot scratch directory.
const STATE_NAME: &str = "state";
/// How long the server waits with no client and no live run before it exits.
/// Manvi's default, stated so a change there does not change this silently.
pub(crate) const IDLE_EXIT: &str = "5m";
/// How long a freshly started server gets to open its socket.
const START_DEADLINE: Duration = Duration::from_secs(10);
const CONNECT_POLL: Duration = Duration::from_millis(50);
/// The most a record file is read from: it is a handful of short fields.
const MAX_RECORD_BYTES: u64 = 16 * 1024;
/// How long a probe of `manvi serve --help` may take. `cmd_terminal_backend`
/// asks it on a first read, so the quit question and Settings wait at most this
/// long; a probe that runs out is `Unknown` and asked again, never remembered.
const PROBE_DEADLINE: Duration = Duration::from_secs(3);
/// How long a server asked to exit gets to remove its socket.
const EXIT_DEADLINE: Duration = Duration::from_secs(5);
/// How long `serve.stop_all` is told to wait for runs to end. Manvi allows 30 s;
/// a quit does not wait that long for anything.
pub(crate) const STOP_ALL_WAIT: Duration = Duration::from_secs(10);
/// How long a reconnect waits for the previous controller's connection to be
/// served out (Manvi gives its in-flight requests 5 s) before saying the
/// server is busy.
#[cfg(not(test))]
const CONTROLLER_WAIT: Duration = Duration::from_secs(8);

/// [`CONTROLLER_WAIT`]; a test shortens it so "another client controls the
/// server" does not take eight seconds to be reported.
pub(crate) fn controller_wait() -> Duration {
    #[cfg(test)]
    return Duration::from_millis(700);
    #[cfg(not(test))]
    CONTROLLER_WAIT
}
/// The most jobs Manvi lists in `serve.status`. A listing this long may have
/// been cut, so a job missing from it proves nothing.
pub(crate) const MAX_LISTED_JOBS: usize = 64;

/// Where the hosted server keeps everything.
#[derive(Debug, Clone)]
pub(crate) struct HostPaths {
    pub dir: PathBuf,
    pub socket: PathBuf,
    pub state: PathBuf,
    pub log: PathBuf,
    pub record: PathBuf,
}

impl HostPaths {
    pub(crate) fn under(dir: &Path) -> Self {
        Self {
            dir: dir.to_path_buf(),
            socket: dir.join(SOCKET_NAME),
            state: dir.join(STATE_NAME),
            log: dir.join(LOG_NAME),
            record: dir.join(RECORD_NAME),
        }
    }

    /// The directory in this user's GitPulse configuration. A sibling of the
    /// terminal host's, and per user by construction, so another user's or
    /// another profile's server is never at this path.
    pub(crate) fn for_app() -> Result<Self, String> {
        crate::tool_config::default_config_dir()
            .map(|dir| Self::under(&dir.join(DIR_NAME)))
            .ok_or_else(|| "the application configuration directory is unavailable".to_string())
    }

    /// Creates the directory private and checks the socket path fits.
    #[cfg(unix)]
    pub(crate) fn prepare(&self) -> Result<(), String> {
        let limit = crate::local_socket::max_path_bytes();
        if self.socket.as_os_str().len() > limit {
            return Err(format!(
                "the socket path is {} bytes and this system allows {limit}: {}",
                self.socket.as_os_str().len(),
                self.socket.display()
            ));
        }
        if !crate::local_socket::peer_identity_is_checkable() {
            return Err("this platform cannot report a socket peer's user".into());
        }
        crate::local_socket::private_dir(&self.dir)?;
        crate::local_socket::private_dir(&self.state)
    }
}

/// Whether an installed Manvi can serve on a socket.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Support {
    Supported,
    /// It answered, and does not know `--listen`.
    Unsupported(String),
    /// It could not be asked.
    Unknown(String),
}

/// Asks `binary` whether it serves on a socket: its own usage text for
/// `serve` names `--listen` when it does, and a Manvi that predates the mode
/// refuses the flag and says so. Reading usage starts nothing and creates
/// nothing, which is why this is the probe and not a trial `--listen`.
///
/// The verdict is remembered per binary file (path, size, modification time),
/// so Settings and the quit question do not run it again, and an upgrade is
/// noticed.
#[cfg(unix)]
pub(crate) fn probe_support(binary: &str) -> Support {
    type Cache = Mutex<HashMap<(String, u64, i64), Support>>;
    static CACHE: OnceLock<Cache> = OnceLock::new();
    let key = std::fs::metadata(binary)
        .map(|meta| (binary.to_string(), meta.len(), meta.mtime()))
        .ok();
    let cache = CACHE.get_or_init(Mutex::default);
    if let Some(key) = &key {
        if let Some(known) = crate::supervisor::lock_or_recover(cache).get(key) {
            return known.clone();
        }
    }
    let verdict = ask_usage(binary);
    // An unanswered question is asked again next time, not remembered.
    if let (Some(key), false) = (key, matches!(verdict, Support::Unknown(_))) {
        crate::supervisor::lock_or_recover(cache).insert(key, verdict.clone());
    }
    verdict
}

#[cfg(unix)]
fn ask_usage(binary: &str) -> Support {
    let usage = match super::sidecar::serve_usage(binary, PROBE_DEADLINE) {
        Ok(usage) => usage,
        Err(why) => return Support::Unknown(why),
    };
    if usage.contains("--listen") {
        Support::Supported
    } else {
        Support::Unsupported(format!(
            "the installed Manvi (`{binary}`) cannot serve on a socket; update Manvi"
        ))
    }
}

/// Why a socket could not be dialled.
#[cfg(unix)]
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Reach {
    /// Nothing is there, or nothing answers on what is: no server.
    Gone,
    /// Something is there and it is not ours to talk to, or the question could
    /// not be answered. Never read as "no server".
    Refused(String),
}

/// Dials the server's socket, refusing anything that is not a socket owned by
/// this user answering from this user's uid.
#[cfg(unix)]
pub(crate) fn connect(socket: &Path) -> Result<UnixStream, Reach> {
    let euid = unsafe { libc::geteuid() };
    match std::fs::symlink_metadata(socket) {
        Ok(meta) => {
            if !meta.file_type().is_socket() {
                return Err(Reach::Refused(format!(
                    "{} exists and is not a socket",
                    socket.display()
                )));
            }
            if meta.uid() != euid {
                return Err(Reach::Refused(format!(
                    "{} belongs to another user",
                    socket.display()
                )));
            }
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Err(Reach::Gone),
        Err(error) => {
            return Err(Reach::Refused(format!("{}: {error}", socket.display())));
        }
    }
    let stream = match UnixStream::connect(socket) {
        Ok(stream) => stream,
        // The file of a server that was killed. Manvi replaces such a socket
        // when it starts, so for this purpose there is no server.
        Err(error)
            if matches!(
                error.kind(),
                std::io::ErrorKind::ConnectionRefused | std::io::ErrorKind::NotFound
            ) =>
        {
            return Err(Reach::Gone)
        }
        Err(error) => return Err(Reach::Refused(format!("{}: {error}", socket.display()))),
    };
    match crate::local_socket::peer_uid(&stream) {
        Some(uid) if uid == euid => Ok(stream),
        Some(_) => Err(Reach::Refused(format!(
            "the server at {} is running as another user",
            socket.display()
        ))),
        None => Err(Reach::Refused(format!(
            "the user of the server at {} could not be established",
            socket.display()
        ))),
    }
}

/// Polls for a server that was just started. `exited` is the starter's report
/// that the process is already gone, which ends the wait with its log.
#[cfg(unix)]
pub(crate) fn wait_for_socket(
    paths: &HostPaths,
    exited: &AtomicBool,
) -> Result<UnixStream, String> {
    let deadline = Instant::now() + START_DEADLINE;
    loop {
        match connect(&paths.socket) {
            Ok(stream) => return Ok(stream),
            Err(Reach::Refused(why)) => return Err(why),
            Err(Reach::Gone) => {}
        }
        if exited.load(Ordering::Acquire) || Instant::now() >= deadline {
            let said = log_tail(&paths.log);
            return Err(format!(
                "the hosted Manvi {}{}",
                if exited.load(Ordering::Acquire) {
                    "exited before it opened its socket".to_string()
                } else {
                    format!(
                        "did not open its socket within {}s",
                        START_DEADLINE.as_secs()
                    )
                },
                if said.is_empty() {
                    String::new()
                } else {
                    format!(" (its log ends: {said})")
                }
            ));
        }
        std::thread::sleep(CONNECT_POLL);
    }
}

/// The last lines of the server's log, for saying why it failed.
#[cfg(unix)]
pub(crate) fn log_tail(log: &Path) -> String {
    let Ok(mut file) = std::fs::File::open(log) else {
        return String::new();
    };
    let mut text = Vec::new();
    let _ = (&mut file).take(4096).read_to_end(&mut text);
    let text = String::from_utf8_lossy(&text);
    let lines: Vec<&str> = text.lines().rev().take(3).collect();
    lines.into_iter().rev().collect::<Vec<_>>().join(" / ")
}

/// `manvi-{pid}-{nanos}-{token}`: the owner a Manvi process stamps on the
/// claims it makes and on its own identity. The token is the random part that
/// keeps owners unique; only its shape is checked. The one parser: run
/// reconciliation and this module both read owners through it.
pub(crate) fn manvi_owner(owner: &str) -> Option<(u32, u128)> {
    let mut parts = owner.strip_prefix("manvi-")?.split('-');
    let pid = parts.next()?.parse::<u32>().ok().filter(|p| *p > 0)?;
    let stamp = parts.next()?.parse::<u128>().ok()?;
    let token = parts.next()?;
    let opaque =
        !token.is_empty() && token.len() <= 64 && token.bytes().all(|b| b.is_ascii_hexdigit());
    (opaque && parts.next().is_none()).then_some((pid, stamp))
}

/// What the server says about itself and its jobs (`serve.status`).
#[derive(Debug, Clone, Deserialize)]
pub(crate) struct ServeStatus {
    #[serde(default)]
    pub protocol: i64,
    pub owner_id: String,
    #[serde(default)]
    pub process_id: u32,
    #[serde(default)]
    pub controller: bool,
    #[serde(default)]
    pub jobs: Vec<ServeJob>,
}

#[derive(Debug, Clone, Deserialize)]
pub(crate) struct ServeJob {
    pub id: String,
    #[serde(default)]
    pub phase: String,
}

impl ServeStatus {
    /// Whether the job list may be the whole list: Manvi caps it, and a listing
    /// at the cap may have been cut.
    pub(crate) fn jobs_complete(&self) -> bool {
        self.jobs.len() < MAX_LISTED_JOBS
    }

    /// Whether the checks that do not depend on who we are pass: the protocol
    /// this build speaks, and an owner that has the shape of a Manvi's whose
    /// process is the one answering.
    pub(crate) fn verify_identity(&self) -> Result<(u32, u128), String> {
        if self.protocol != PROTOCOL_VERSION {
            return Err(format!(
                "the hosted Manvi speaks protocol {} and this build speaks {PROTOCOL_VERSION}",
                self.protocol
            ));
        }
        let (pid, stamp) = manvi_owner(&self.owner_id).ok_or_else(|| {
            format!(
                "the hosted Manvi named itself {:?}, which is not a Manvi owner id",
                self.owner_id
            )
        })?;
        if self.process_id != pid {
            return Err(format!(
                "the hosted Manvi reports process {} but names owner {}",
                self.process_id, self.owner_id
            ));
        }
        Ok((pid, stamp))
    }
}

/// What GitPulse wrote down when it started (or last verified) a server:
/// whose it is, which profile it works on, and what model it was started with.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Record {
    pub version: u8,
    pub socket: String,
    pub owner_id: String,
    pub process_id: u32,
    pub protocol: i64,
    /// [`fingerprint`] of the model selection the server was started with.
    pub selection: String,
    /// The profile database the server serves.
    pub profile: String,
    /// Set when something the server was started with (the Claude key) has
    /// changed: the server is replaced as soon as it holds no run.
    #[serde(default)]
    pub restart: bool,
    /// The server process's start time as the operating system reported it
    /// when GitPulse started it, in microseconds; 0 when it could not be read.
    /// What lets a later signal tell this process from a pid that was reused.
    #[serde(default)]
    pub start_us: u64,
}

const RECORD_VERSION: u8 = 1;

/// The start time of `pid` as the operating system reports it.
fn start_of(pid: i32) -> Option<u64> {
    crate::sysload::procscan::probe_one(pid).map(|facts| facts.start_us)
}

impl Record {
    pub(crate) fn new(
        paths: &HostPaths,
        status: &ServeStatus,
        selection: &str,
        profile: &Path,
    ) -> Self {
        Self {
            version: RECORD_VERSION,
            socket: paths.socket.display().to_string(),
            owner_id: status.owner_id.clone(),
            process_id: status.process_id,
            protocol: status.protocol,
            selection: selection.to_string(),
            profile: profile.display().to_string(),
            restart: false,
            start_us: i32::try_from(status.process_id)
                .ok()
                .and_then(start_of)
                .unwrap_or(0),
        }
    }
}

/// Reads the record. `Ok(None)` is "none written"; a record that cannot be
/// read or parsed is an error, never "none".
#[cfg(unix)]
pub(crate) fn read_record(paths: &HostPaths) -> Result<Option<Record>, String> {
    let mut file = match std::fs::OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW)
        .open(&paths.record)
    {
        Ok(file) => file,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(format!("{}: {error}", paths.record.display())),
    };
    let mut text = Vec::new();
    (&mut file)
        .take(MAX_RECORD_BYTES + 1)
        .read_to_end(&mut text)
        .map_err(|e| format!("{}: {e}", paths.record.display()))?;
    if text.len() as u64 > MAX_RECORD_BYTES {
        return Err(format!("{} is too large", paths.record.display()));
    }
    let record: Record = serde_json::from_slice(&text)
        .map_err(|e| format!("{} is unreadable: {e}", paths.record.display()))?;
    if record.version != RECORD_VERSION {
        return Err(format!(
            "{} is version {}, not {RECORD_VERSION}",
            paths.record.display(),
            record.version
        ));
    }
    Ok(Some(record))
}

/// Writes the record atomically and privately (`0600`, via a temporary file in
/// the same private directory).
#[cfg(unix)]
pub(crate) fn write_record(paths: &HostPaths, record: &Record) -> Result<(), String> {
    use std::io::Write;
    let text = serde_json::to_vec(record).map_err(|e| e.to_string())?;
    let temporary = paths
        .dir
        .join(format!("{RECORD_NAME}.{}.tmp", std::process::id()));
    let result = (|| {
        let mut file = std::fs::OpenOptions::new()
            .create_new(true)
            .write(true)
            .mode(0o600)
            .custom_flags(libc::O_NOFOLLOW)
            .open(&temporary)?;
        file.write_all(&text)?;
        file.sync_all()?;
        std::fs::rename(&temporary, &paths.record)
    })();
    if result.is_err() {
        let _ = std::fs::remove_file(&temporary);
    }
    result.map_err(|e| format!("{}: {e}", paths.record.display()))
}

/// Marks the server as due for replacement when it next holds no run: what a
/// changed Claude key needs, since the server keeps the key it was started
/// with. A missing record has nothing to mark.
#[cfg(unix)]
pub(crate) fn mark_restart(paths: &HostPaths) -> Result<(), String> {
    match read_record(paths)? {
        Some(mut record) if !record.restart => {
            record.restart = true;
            write_record(paths, &record)
        }
        _ => Ok(()),
    }
}

/// The model selection as a record keeps it: enough to tell two selections
/// apart, never a credential.
pub(crate) fn fingerprint(selection: Option<&ModelSelection>) -> String {
    match selection {
        None => "default".to_string(),
        Some(ModelSelection::Local { base_url, model }) => format!("local|{base_url}|{model}"),
        Some(ModelSelection::Claude { model }) => format!("claude|{model}"),
    }
}

/// What to do with a live server for the connection that wants it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Reuse {
    /// It was started the way this connection would start it.
    Yes,
    /// It was not, and holds no run: replace it.
    Replace,
    /// It was not, and holds runs: use it for what does not depend on the
    /// model, and say the selection waits for the runs to end.
    KeepForRuns(usize),
}

pub(crate) fn judge_server(record: &Record, wanted: &str, jobs: usize) -> Reuse {
    if record.selection == wanted && !record.restart {
        Reuse::Yes
    } else if jobs == 0 {
        Reuse::Replace
    } else {
        Reuse::KeepForRuns(jobs)
    }
}

/// Asks an idle server to exit and waits for its socket to go.
///
/// The server has no "exit" request, and a new selection or key reaches a
/// Manvi only through its environment at start, so a replaced server is ended
/// with SIGTERM — which Manvi handles by closing its socket and removing the
/// file. Only ever the process the server itself reported over the
/// authenticated socket, that GitPulse's own record names, and whose start
/// time still equals the one recorded when GitPulse started it — the signal
/// goes through `procguard::signal`, which also refuses this process, pid 1 and
/// a recycled pid; never when the job list was not empty.
#[cfg(unix)]
pub(crate) fn end_idle_server(
    paths: &HostPaths,
    record: &Record,
    status: &ServeStatus,
) -> Result<(), String> {
    if !status.jobs.is_empty() {
        return Err("the hosted Manvi holds runs".into());
    }
    let pid = status.process_id;
    if pid <= 1 || pid == std::process::id() || pid != record.process_id {
        return Err(format!(
            "the hosted Manvi's process ({pid}) is not the one recorded ({})",
            record.process_id
        ));
    }
    let signed = i32::try_from(pid).map_err(|_| "process id out of range".to_string())?;
    if start_of(signed).is_none() {
        // Nothing to end: the socket check below finishes the job.
    } else if let Err(error) = crate::procguard::signal::signal_process(
        signed,
        record.start_us,
        &start_of,
        libc::SIGTERM,
        "replacing the hosted Manvi",
    ) {
        return Err(format!("could not end process {pid}: {error}"));
    }
    let deadline = Instant::now() + EXIT_DEADLINE;
    loop {
        if matches!(connect(&paths.socket), Err(Reach::Gone)) {
            return Ok(());
        }
        if Instant::now() >= deadline {
            return Err(format!(
                "process {pid} did not close its socket within {}s",
                EXIT_DEADLINE.as_secs()
            ));
        }
        std::thread::sleep(CONNECT_POLL);
    }
}

/// What a started server turned out to be: its pid, and a flag the reaper sets
/// when it exits, which ends the wait for its socket early.
pub(crate) struct Launched {
    pub exited: Arc<AtomicBool>,
}

/// Starts the server from a built command. The one seam a test replaces.
pub(crate) type Launcher =
    Arc<dyn Fn(Command, &HostPaths) -> Result<Launched, String> + Send + Sync>;

/// Everything a hosting profile worker needs to know.
#[derive(Clone)]
pub(crate) struct HostPlan {
    pub paths: HostPaths,
    pub launch: Launcher,
}

impl HostPlan {
    /// The plan the app runs under when the person has chosen to keep agents
    /// running, or `None`: no choice, or nowhere to keep a server.
    pub(crate) fn from_settings() -> Option<Self> {
        #[cfg(not(unix))]
        return None;
        #[cfg(unix)]
        Self::from_settings_unix()
    }

    #[cfg(unix)]
    fn from_settings_unix() -> Option<Self> {
        if !crate::tool_config::agent_defaults().keeps_terminals_running() {
            return None;
        }
        match HostPaths::for_app() {
            Ok(paths) => Some(Self::detached(paths)),
            Err(error) => {
                log::error!(target: "harness", "managed runs stay in the app: {error}");
                None
            }
        }
    }

    #[cfg(unix)]
    pub(crate) fn detached(paths: HostPaths) -> Self {
        Self {
            paths,
            launch: Arc::new(launch_detached),
        }
    }
}

/// Starts `command` in a session of its own, so nothing aimed at this app's
/// process group or session reaches it, and outside `procguard`, whose quit
/// sweep must never reach it: it is meant to outlive this process.
#[cfg(unix)]
fn launch_detached(mut command: Command, paths: &HostPaths) -> Result<Launched, String> {
    use crate::procguard::LockedSpawn;
    use std::os::unix::process::CommandExt;
    let log = std::fs::OpenOptions::new()
        .create(true)
        .write(true)
        .truncate(true)
        .mode(0o600)
        .custom_flags(libc::O_NOFOLLOW)
        .open(&paths.log)
        .map_err(|e| format!("{}: {e}", paths.log.display()))?;
    command
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(log);
    // SAFETY: `setsid` is async-signal-safe and touches nothing but the
    // child's own session, between fork and exec.
    unsafe {
        command.pre_exec(|| {
            if libc::setsid() == -1 {
                return Err(std::io::Error::last_os_error());
            }
            Ok(())
        });
    }
    let mut child = command
        .spawn_locked()
        .map_err(|e| format!("could not start the hosted Manvi: {e}"))?;
    let pid = child.id();
    log::info!(target: "harness", "started the hosted Manvi (pid {pid})");
    let exited = Arc::new(AtomicBool::new(false));
    let flag = exited.clone();
    // Reaped when it ends, so a server that exits while this app runs is not
    // left a zombie. The app never signals it here.
    let waited =
        crate::supervisor::spawn_one_of_many("manvi-host-wait", "hosted manvi wait", move || {
            match child.wait() {
                Ok(status) => log::info!(target: "harness", "the hosted Manvi exited: {status}"),
                Err(error) => {
                    log::warn!(target: "harness", "could not wait for the hosted Manvi: {error}")
                }
            }
            flag.store(true, Ordering::Release);
        });
    if let Err(error) = waited {
        log::warn!(target: "harness", "the hosted Manvi will not be reaped by this app: {error}");
    }
    Ok(Launched { exited })
}

/// Permissions the hosted directory must have; checked by tests and by
/// anything that wants to say the directory is private.
#[cfg(unix)]
#[cfg(test)]
pub(crate) fn is_private(dir: &Path) -> bool {
    use std::os::unix::fs::PermissionsExt;
    std::fs::metadata(dir)
        .map(|meta| meta.permissions().mode() & 0o077 == 0)
        .unwrap_or(false)
}

/// What run reconciliation can learn from the hosted server: three outcomes,
/// because "no server" and "could not ask" are different facts and only the
/// first is evidence that a run is stranded.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum View {
    /// The profile worker is not hosted (not chosen, or the stdio child).
    NotHosting,
    /// Nothing is serving at the hosted socket.
    Gone,
    /// There may be a server and it could not be read: busy, another
    /// controller, another owner, a protocol it does not speak.
    Unknown(String),
    /// A server answered, and this is what it holds.
    Live {
        pid: u32,
        /// Every job it listed, with its phase.
        jobs: Vec<(String, String)>,
        /// Whether the list may be the whole list (see [`MAX_LISTED_JOBS`]).
        complete: bool,
    },
}

impl View {
    pub(crate) fn from_status(status: &ServeStatus) -> Self {
        View::Live {
            pid: status.process_id,
            jobs: status
                .jobs
                .iter()
                .map(|job| (job.id.clone(), job.phase.clone()))
                .collect(),
            complete: status.jobs_complete(),
        }
    }
}

// ----- what a quit does ------------------------------------------------------

/// What the quit chose for the hosted runs. Process-wide, like the terminal
/// registry's `leave_running`: it is set by the quit question and read by the
/// two places that close the worker.
const EXIT_UNSET: u8 = 0;
const EXIT_LEAVE: u8 = 1;
const EXIT_STOP: u8 = 2;
static EXIT_CHOICE: AtomicU8 = AtomicU8::new(EXIT_UNSET);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ExitChoice {
    /// Disconnect and leave the runs going. Also what an exit nobody was asked
    /// about does — a signal, a crash: the person opted into keeping runs.
    Leave,
    /// Stop every run, wait a bounded time, then disconnect.
    Stop,
}

pub(crate) fn set_exit_choice(choice: ExitChoice) {
    EXIT_CHOICE.store(
        match choice {
            ExitChoice::Leave => EXIT_LEAVE,
            ExitChoice::Stop => EXIT_STOP,
        },
        Ordering::Release,
    );
}

pub(crate) fn exit_choice() -> ExitChoice {
    match EXIT_CHOICE.load(Ordering::Acquire) {
        EXIT_STOP => ExitChoice::Stop,
        _ => ExitChoice::Leave,
    }
}

#[cfg(test)]
pub(crate) fn reset_exit_choice() {
    EXIT_CHOICE.store(EXIT_UNSET, Ordering::Release);
}

// ----- what Settings and the quit question read ---------------------------------

/// What this process knows about hosting the profile worker, as last observed.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub(crate) enum Observed {
    /// Nothing has asked yet.
    #[default]
    Untried,
    /// A hosted connection is live.
    Hosted,
    /// The worker is not connected to a hosted server right now.
    Detached,
    /// Hosting was wanted and not used, and why.
    FellBack(String),
    /// A server is there and could not be used, and why. Nothing else was
    /// started beside it.
    Blocked(String),
}

static OBSERVED: Mutex<Observed> = Mutex::new(Observed::Untried);

pub(crate) fn observe(now: Observed) {
    *crate::supervisor::lock_or_recover(&OBSERVED) = now;
}

pub(crate) fn observed() -> Observed {
    crate::supervisor::lock_or_recover(&OBSERVED).clone()
}

/// The account of managed-run hosting that Settings shows and the quit
/// question branches on.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ManagedHosting {
    /// The person chose to keep agents running (`keep_terminals_running`).
    pub requested: bool,
    /// Managed runs are held by a server that survives a quit, right now.
    pub hosted: bool,
    /// One plain sentence: what happens to managed runs when GitPulse quits,
    /// and, when it is not what was asked for, why.
    pub detail: String,
}

/// [`ManagedHosting`] from its inputs. Pure, so each sentence is pinned.
pub(crate) fn describe(
    requested: bool,
    observed: &Observed,
    support: Option<&Support>,
) -> ManagedHosting {
    let ends = "Managed agent runs end when GitPulse quits";
    let (hosted, detail) = if !requested {
        (false, format!("{ends}."))
    } else if *observed == Observed::Hosted {
        (
            true,
            "Managed agent runs are kept running when GitPulse quits, and the next launch picks them up."
                .to_string(),
        )
    } else if let Observed::FellBack(why) | Observed::Blocked(why) = observed {
        (false, format!("{ends}: {why}."))
    } else {
        match support {
            Some(Support::Unsupported(why)) => (false, format!("{ends}: {why}.")),
            Some(Support::Unknown(why)) => (
                false,
                format!("{ends}, because whether Manvi can keep them could not be checked: {why}."),
            ),
            Some(Support::Supported) => (
                false,
                "Managed agent runs will be kept running when GitPulse quits once the agent runtime starts."
                    .to_string(),
            ),
            None => (
                false,
                format!("{ends}; whether the installed Manvi can keep them has not been checked."),
            ),
        }
    };
    ManagedHosting {
        requested,
        hosted,
        detail,
    }
}

#[cfg(all(test, unix))]
#[path = "hosted_tests.rs"]
mod tests;
