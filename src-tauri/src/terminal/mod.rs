//! Terminal execution module: PTY session management and bounded command execution.

use crate::coverage_toolchain::{
    repo_local_program, GCOVR_MODULE, GCOVR_PACKAGES, JS_COVERAGE_PROVIDERS,
    NATIVE_COVERAGE_BUILD_DIR, NATIVE_COVERAGE_FLAG, NATIVE_COVERAGE_LCOV, PYTEST_PACKAGES,
    VENV_DIR_NAMES,
};
use crate::engine::git_cli::{
    extended_child_path, resolve_spawn_program_with, run_captured, sandbox_join,
    sandbox_join_canonical, validate_repo, RunOutcome,
};
use crate::harness::{guard_command, PolicyVerdict};
use portable_pty::{native_pty_system, Child, CommandBuilder, MasterPty, PtySize};
pub mod events;
mod executor;
mod flow;
pub(crate) mod foreground;
#[cfg(unix)]
pub mod host;
pub mod incident;
#[cfg(unix)]
mod input;
mod ipc;
#[cfg(unix)]
mod jobs;
#[cfg(unix)]
mod readiness;
mod ring;
mod sink;
pub use events::{AppEvents, SessionEnded, SessionEvents};
use executor::Executor;
use flow::{OutputFlow, Route};
pub use ipc::{control_async, resize_async, write_async};
pub use ring::RING_BYTES;
use serde::{Deserialize, Serialize};
use sink::OutputSlot;
pub use sink::{OutputChannel, OutputSink, END_OF_OUTPUT};
use std::collections::HashMap;
use std::io::{Read, Write};
use std::panic::{catch_unwind, AssertUnwindSafe};
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};
use tauri::AppHandle;

/// Bounds for one-shot command execution timeout.
const MIN_RUN_TIMEOUT: Duration = Duration::from_secs(1);
const MAX_RUN_TIMEOUT: Duration = Duration::from_secs(30 * 60);
const DEFAULT_RUN_TIMEOUT: Duration = Duration::from_secs(60);

/// Per-stream output tail cap (64 KB).
const TERMINAL_TAIL_CAP_BYTES: usize = 64 * 1024;
/// Command payload bounds. These protect both the process launcher and the
/// policy sidecar from an IPC caller sending an effectively unbounded argv.
const TERMINAL_ARG_COUNT_CAP: usize = 256;
const TERMINAL_ARG_BYTES_CAP: usize = 16 * 1024;
const TERMINAL_ARGV_BYTES_CAP: usize = 128 * 1024;
/// Interactive terminal resource bounds. The PTY is user-owned, but its IPC
/// surface must not permit unbounded processes, allocations, or resize work.
/// Public so frontend contract tests and native stress tests share one number.
///
/// How many sessions may be open at once is the user's
/// (`tool_config::AgentDefaults::max_terminal_sessions`), held live on
/// [`TerminalSessions`]: this many until they change it.
pub const DEFAULT_PTY_SESSIONS: usize = 32;
/// The most a user may choose. Each session is a PTY pair, a reader thread
/// and an output window of `flow::OUTPUT_WINDOW` (512 KiB), so this bounds
/// those at 256 descriptors, 128 reader threads and 64 MiB — and stays well
/// inside macOS's system-wide PTY allowance (`kern.tty.ptmx_max`, 511 by
/// default), which every other terminal on the machine shares. Threads beyond
/// the readers are on demand and end when idle (see `executor`): at most two
/// per session (its input and geometry lanes), and a control pool as wide as
/// this, so 128 + 256 + 128 at the very worst.
pub const MAX_PTY_SESSIONS: usize = 128;
const MAX_PTY_INPUT_BYTES: usize = 64 * 1024;
/// Writes that may wait behind the one being delivered to a session. The
/// frontend sends one at a time and waits for each, so this is headroom for a
/// burst of IPC calls, not a working depth; past it a write is refused at once.
/// Public so the native latency tests and the queue bound share one number.
pub const INPUT_QUEUE_CAPACITY: usize = 64;
/// Resizes that may wait behind the one being applied to a session.
pub const GEOMETRY_QUEUE_CAPACITY: usize = 16;
/// Slow terminal commands (context, close, attach) that may wait for a thread.
const CONTROL_QUEUE_CAPACITY: usize = 256;
/// How long a session's lane thread outlives its last job.
const LANE_IDLE: Duration = Duration::from_secs(30);
/// How long a control-pool thread outlives its last job.
const CONTROL_IDLE: Duration = Duration::from_secs(5);
/// Words that mark an answer as "there is no session any more". The frontend
/// ends a tab's input on an answer carrying one (`inputQueue.ts::sessionIsGone`,
/// matched as whole words, ignoring case) and on nothing else a backend says;
/// every other refusal drops one chunk of a session that is still there. Its
/// `SESSION_GONE_MARKERS` is checked against this line by text, so a message
/// that means "gone" must carry one of these, and one that does not must not.
pub const SESSION_GONE_MARKERS: [&str; 3] = ["not found", "session is closed", "Terminal closed"];

/// Whether the frontend reads `message` as a session that is gone: the same
/// rule as `sessionIsGone`, for tests that hold a backend answer to it.
#[doc(hidden)]
pub fn reads_as_session_gone(message: &str) -> bool {
    let lower = message.to_lowercase();
    let word = |c: Option<char>| c.is_some_and(|c| c.is_alphanumeric() || c == '_');
    SESSION_GONE_MARKERS.iter().any(|marker| {
        let marker = marker.to_lowercase();
        lower.match_indices(&marker).any(|(at, _)| {
            !word(lower[..at].chars().next_back())
                && !word(lower[at + marker.len()..].chars().next())
        })
    })
}

#[cfg(test)]
#[test]
fn reads_as_session_gone_matches_whole_words_ignoring_case() {
    for gone in [
        "Terminal session 'term-1' not found",
        "Terminal session is closed",
        "terminal closed after writing 0 of 9 input bytes",
        "x: NOT FOUND.",
    ] {
        assert!(reads_as_session_gone(gone), "{gone}");
    }
    for live in [
        "The terminal input queue is full (64 waiting); try again in a moment",
        "Terminal is busy; the resize gave up after 2000ms",
        "Terminal input timed out after writing 3 of 6 bytes; input was not retried",
        "cannot_found",
        "xnot found",
        "not foundry",
        "Terminal closedown",
    ] {
        assert!(!reads_as_session_gone(live), "{live}");
    }
}

/// How long a resize waits for the master lock before it answers "busy".
const MASTER_LOCK_BUDGET: Duration = Duration::from_secs(2);
const MAX_PTY_ROWS: u16 = 1_000;
const MAX_PTY_COLS: u16 = 1_000;

/// Why a model-authored command is being requested. The value is not merely
/// telemetry: it selects a purpose-specific allowlist before any process is
/// spawned, so a coverage *analysis* plan cannot become a package install and
/// a remediation plan cannot become an arbitrary shell. Coverage *generation*
/// may install one locked crate (`cargo-llvm-cov`) and add `llvm-tools-preview`
/// when the scanner planned that setup; it still cannot install arbitrary crates.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ManviActionKind {
    Health,
    Coverage,
    CoverageGenerator,
}

impl ManviActionKind {
    fn label(self) -> &'static str {
        match self {
            Self::Health => "health remediation",
            Self::Coverage => "coverage analysis",
            Self::CoverageGenerator => "coverage generation",
        }
    }
}

/// Result of one-shot command execution.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TerminalRunResult {
    pub command: String,
    pub gated: bool,
    pub policy: Option<PolicyVerdict>,
    pub timed_out: bool,
    pub exit_code: Option<i32>,
    pub stdout_tail: String,
    pub stderr_tail: String,
    pub truncated: bool,
    /// Why the tails are a prefix, when they are; `None` when they are whole.
    /// The UI renders this rather than assuming the cause: an output that
    /// could not be read to the end is not an output that was too big.
    pub truncation_reason: Option<String>,
    pub duration_ms: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TerminalSpawned {
    pub id: String,
    pub shell: String,
    pub cwd: String,
}

/// Bytes one output message may carry, and reserve. The renderer refuses a
/// message longer than `OUTPUT_CHUNK_BYTES` (and repays its credit) instead of
/// painting it. Written as a literal: the frontend's
/// `MAX_TERMINAL_OUTPUT_CHUNK` is checked against this line by text.
///
/// This is the size of a *message*, not of a `read`: the reader gathers what
/// the program has printed into one message because each one costs the webview
/// a script evaluation and a fetch, and the kernel hands a macOS PTY reader at
/// most 1 KiB at a time. A message per read made a flood cost one round trip per
/// KiB. `flow::OUTPUT_WINDOW` holds eight of these.
pub const OUTPUT_CHUNK_BYTES: usize = 65536;

/// What an in-process listener of [`OutputSink::events`] receives per chunk.
/// A view receives raw bytes on its channel and never this.
#[derive(Clone, Serialize)]
pub struct TerminalOutputPayload {
    pub id: String,
    pub data_b64: String,
    /// Length passed to `OutputFlow::reserve` for this chunk.
    pub bytes: usize,
}

#[cfg(test)]
#[test]
fn output_payload_names_the_reserved_length() {
    let payload = TerminalOutputPayload {
        id: "s".into(),
        data_b64: "QQ==".into(),
        bytes: 1,
    };
    let value = serde_json::to_value(&payload).unwrap();
    assert_eq!(value["bytes"], 1);
    assert_eq!(value["data_b64"], "QQ==");
    assert_eq!(OUTPUT_CHUNK_BYTES, 65536);
}

/// Output a live session's view will never see. The process keeps running;
/// the view says what was lost. Emitted as `terminal-loss`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TerminalLossPayload {
    pub id: String,
    pub message: String,
}

/// What a view is told when it fell behind and output was dropped for it.
pub const SHED_NOTICE: &str = "Terminal output was skipped while this window was not keeping up \
     (hidden, minimized, or busy). The session kept running; the screen was redrawn when it caught up.";

/// How long the reader waits for a view to repay its output window before it
/// sheds instead. Long enough that a busy page is not shed for a hiccup;
/// shedding no longer costs the process, only skipped output.
pub const DEFAULT_OUTPUT_STALL: Duration = Duration::from_secs(30);

/// Output at least this large, read and still dry, is the front of a stream
/// rather than an echo or a prompt, and is worth a short wait for more. Below
/// it the reader sends at once: a typed character must not pay for batching.
#[cfg(unix)]
const GATHER_MIN_BYTES: usize = 512;
/// How long a stream may go quiet before what has been gathered is sent. A
/// producer faster than the reader leaves gaps far shorter than this; a
/// finished frame leaves a longer one.
#[cfg(unix)]
const GATHER_QUIET: Duration = Duration::from_millis(2);
/// The longest output waits to be sent once it has begun to be gathered, so a
/// steady trickle still reaches the view this often.
#[cfg(unix)]
const GATHER_MAX_HOLD: Duration = Duration::from_millis(8);
/// The same bound for a session whose tab is not on screen. Nobody is reading
/// it, so it is sent a few times a second rather than at frame rate; its
/// credit is still repaid, only later, which the output window (and its 30 s
/// stall timeout) absorbs. A session that becomes visible waits at most this
/// long for its backlog.
#[cfg(unix)]
const HIDDEN_MAX_HOLD: Duration = Duration::from_millis(250);

/// How often a reader whose session is being closed looks up from its wait. It
/// holds back its question about the shell while a close runs (only the closer
/// reaps meanwhile), and must ask soon after the close is over rather than
/// sleep until the next event: with no more output and no hangup, there may
/// not be one.
const CLOSING_LAP: Duration = Duration::from_millis(50);

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TerminalExitPayload {
    pub id: String,
    pub exit_code: Option<i32>,
    pub signal: String,
    pub error: Option<String>,
    pub reaped: bool,
}

/// Task-run bookkeeping at the existing PTY boundary. The observer cannot
/// replace argv or execute a second child. A refused claim prevents spawning.
pub(crate) trait SessionObserver: Send + Sync {
    fn run_id(&self) -> &str;
    fn before_spawn(&self, session_id: &str) -> Result<(), String>;
    /// The process started: its id and creation identity, read right after
    /// the fork, or why they could not be read.
    fn started(
        &self,
        session_id: &str,
        process: Result<(u32, String), String>,
    ) -> Result<(), String>;
    fn spawn_failed(&self, session_id: &str, reason: &str);
    fn finished(&self, payload: &TerminalExitPayload);
}

struct SessionEntry {
    master: Arc<Mutex<Box<dyn MasterPty + Send>>>,
    writer: Arc<Mutex<Box<dyn Write + Send>>>,
    child: Arc<Mutex<Box<dyn Child + Send + Sync>>>,
    dead: Arc<AtomicBool>,
    flow: Arc<OutputFlow>,
    /// Times the reader returned from waiting for this session to do
    /// something: output, an exit, or a timer. An idle session adds none.
    wakeups: Arc<AtomicU64>,
    /// Where the reader sends output: the channel of the view showing this
    /// session, replaced when another view takes it over.
    output: Arc<OutputSlot>,
    /// The view reports this session's tab is not on screen, so its output is
    /// batched harder (see `HIDDEN_MAX_HOLD`). Unreported is visible.
    hidden: Arc<AtomicBool>,
    /// Closes in progress on this session (see `ClosingGuard`).
    closing: Arc<AtomicUsize>,
    tracked_run: Option<String>,
    spawned: TerminalSpawned,
    /// Canonical repository root. `spawned.cwd` is where the process started,
    /// which may be a directory inside it.
    repo: std::path::PathBuf,
    /// Where this session's keystrokes are written: one thread, in order, so a
    /// child that has stopped reading delays only its own input.
    input: Executor,
    /// Where this session's resizes are applied, apart from `input` so a
    /// write spinning on a full tty never holds a resize behind it.
    geometry: Executor,
    /// Where this session's losses and its end are reported.
    events: Arc<dyn SessionEvents>,
    /// Its newest output, whatever its view did with it. Also the order of
    /// delivery: the reader holds it from keeping a chunk to sending it, and
    /// a view taking the session over holds it across its replay, so the two
    /// never interleave.
    ring: Arc<Mutex<ring::Ring>>,
}

/// Which backend holds the terminals, as Settings and the quit dialog read it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TerminalBackendStatus {
    /// `host` (`gitpulse-ptyd`; sessions outlive the app) or `in-process`.
    pub backend: String,
    /// What is wrong with the host, plainly: version skew, a host that would
    /// not start. Empty when nothing is known to be wrong.
    pub trouble: Vec<String>,
}

/// Thread-safe registry of live PTY sessions.
#[derive(Clone)]
pub struct TerminalSessions {
    sessions: Arc<Mutex<HashMap<String, SessionEntry>>>,
    active_sessions: Arc<AtomicUsize>,
    /// How many sessions may be open at once, in `1..=MAX_PTY_SESSIONS`.
    /// Live rather than read per spawn, so a saved setting applies to the
    /// next session and a test registry never depends on the machine's file.
    limit: Arc<AtomicUsize>,
    /// Milliseconds a reader waits on an unacknowledged window before it
    /// sheds (`DEFAULT_OUTPUT_STALL`). Live, so a test can shorten it.
    output_stall_ms: Arc<AtomicU64>,
    /// Mass external-signal deaths, recorded once each (see [`incident`]).
    incidents: Arc<incident::Recorder>,
    /// The slower commands that are not per keystroke, off the pool every git
    /// command shares (see `executor`). As wide as the session cap, so closing
    /// every tab at once still closes them side by side, as it did.
    control: Executor,
    /// The terminal host that holds the PTYs, when this registry is the app's
    /// side of one (`docs/TERMINAL_HOST.md`). `None` is the engine in this
    /// process. Every `terminal::` function routes on this and nothing else.
    #[cfg(unix)]
    host: Option<host::client::HostClient>,
    /// What an exit of this app does to the sessions a host holds: leave
    /// them running (a crash, a signal, or a quit that chose to), or stop them.
    /// Meaningless in-process, where a session cannot outlive the app.
    leave_running: Arc<AtomicBool>,
}

impl Default for TerminalSessions {
    /// The engine in this process.
    fn default() -> Self {
        Self {
            sessions: Arc::default(),
            active_sessions: Arc::default(),
            limit: Arc::new(AtomicUsize::new(DEFAULT_PTY_SESSIONS)),
            output_stall_ms: Arc::new(AtomicU64::new(DEFAULT_OUTPUT_STALL.as_millis() as u64)),
            incidents: Arc::default(),
            control: Executor::new(
                "control",
                MAX_PTY_SESSIONS,
                CONTROL_QUEUE_CAPACITY,
                CONTROL_IDLE,
            ),
            #[cfg(unix)]
            host: None,
            leave_running: Arc::new(AtomicBool::new(false)),
        }
    }
}

impl TerminalSessions {
    /// A registry whose sessions live in the terminal host described by
    /// `config`. Nothing connects until the first call (or [`Self::connect_host`]).
    #[cfg(unix)]
    pub fn with_host(config: host::client::HostConfig) -> Self {
        Self {
            host: Some(host::client::HostClient::new(config)),
            leave_running: Arc::new(AtomicBool::new(true)),
            ..Self::default()
        }
    }

    /// The app's registry: the host when the user has chosen to keep
    /// terminals running across quits (Settings → Agents), else in-process.
    pub fn from_settings() -> Self {
        #[cfg(unix)]
        if crate::tool_config::agent_defaults().keeps_terminals_running() {
            match host::client::HostConfig::for_app() {
                Ok(config) => return Self::with_host(config),
                Err(error) => {
                    log::error!(target: "terminal", "terminals stay in the app: the terminal host has no directory: {error}")
                }
            }
        }
        Self::default()
    }

    /// The terminal host's pid while connected; `None` in-process. For tests
    /// that measure what the process holding the PTYs gives back.
    #[doc(hidden)]
    pub fn host_pid(&self) -> Option<u32> {
        #[cfg(unix)]
        if let Some(host) = &self.host {
            return host.host_pid();
        }
        None
    }

    /// Which backend holds the PTYs: `"host"` or `"in-process"`.
    pub fn backend(&self) -> &'static str {
        #[cfg(unix)]
        if self.host.is_some() {
            return "host";
        }
        "in-process"
    }

    /// Where session events go in the app (Tauri events, incidents), for the
    /// sessions it did not start itself: those a host kept from an earlier
    /// app. Called once at startup.
    pub fn bind_app<R: tauri::Runtime>(&self, app: &AppHandle<R>) {
        #[cfg(unix)]
        if let Some(host) = &self.host {
            host.set_events(Arc::new(AppEvents::new(app, self.incidents.clone())));
        }
        #[cfg(not(unix))]
        let _ = app;
    }

    /// How a task run whose terminal outlived the app that launched it is
    /// observed when it ends. Only a host has such runs.
    #[cfg(unix)]
    pub(crate) fn set_adopter(&self, adopter: host::client::Adopter) {
        if let Some(host) = &self.host {
            host.set_adopter(adopter);
        }
    }

    /// Connects to the host, adopting what it holds and recording what ended
    /// while no app was there. Nothing to do in-process.
    pub fn connect_host(&self) -> Result<(), String> {
        #[cfg(unix)]
        if let Some(host) = &self.host {
            host.connect()?;
            host.set_limit(self.session_limit());
        }
        Ok(())
    }

    /// What is wrong with the terminal host, if anything (version skew, a host
    /// that would not start). Always empty in-process.
    pub fn host_trouble(&self) -> Vec<String> {
        #[cfg(unix)]
        if let Some(host) = &self.host {
            return host.trouble();
        }
        Vec::new()
    }

    /// Whether this app's exit leaves the host's sessions running.
    pub fn leaves_sessions_running(&self) -> bool {
        #[cfg(unix)]
        if self.host.is_some() {
            return self.leave_running.load(Ordering::Acquire);
        }
        false
    }

    /// The quit's choice: leave the host's sessions running, or stop them.
    pub fn set_leave_running(&self, leave: bool) {
        self.leave_running.store(leave, Ordering::Release);
    }

    /// Lets the host go without stopping anything: every view detaches and
    /// every session keeps running for the next app. Nothing in-process.
    pub fn disconnect_host(&self) {
        #[cfg(unix)]
        if let Some(host) = &self.host {
            host.disconnect();
        }
    }

    /// The lane that delivers `session_id`'s keystrokes.
    fn input_lane(&self, session_id: &str) -> Result<Executor, String> {
        self.lock_sessions()
            .get(session_id)
            .map(|session| session.input.clone())
            .ok_or_else(|| format!("Terminal session '{session_id}' not found"))
    }

    /// Threads alive on the open sessions' input and geometry lanes. A
    /// process-wide thread count cannot tell these apart from tokio's pool,
    /// which grows and shrinks on its own; tests that need to see lanes
    /// retire ask this.
    #[doc(hidden)]
    pub fn lane_threads(&self) -> usize {
        #[cfg(unix)]
        if let Some(host) = &self.host {
            return host.lane_threads();
        }
        self.lock_sessions()
            .values()
            .map(|session| session.input.workers() + session.geometry.workers())
            .sum()
    }

    /// The lane that applies `session_id`'s resizes.
    fn geometry_lane(&self, session_id: &str) -> Result<Executor, String> {
        self.lock_sessions()
            .get(session_id)
            .map(|session| session.geometry.clone())
            .ok_or_else(|| format!("Terminal session '{session_id}' not found"))
    }

    /// The session map, recovering from a poisoned lock. The map holds only
    /// handles, and every writer inserts or removes one whole entry, so a
    /// panic under the lock cannot leave it half-updated; refusing to read it
    /// would leave every terminal untypeable and unclosable until restart.
    fn lock_sessions(&self) -> std::sync::MutexGuard<'_, HashMap<String, SessionEntry>> {
        self.sessions
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    /// Recorded mass-death incidents, newest first and bounded.
    pub fn incidents(&self) -> Vec<incident::TerminalIncidentPayload> {
        self.incidents.recent()
    }

    /// How many sessions may be open at once right now.
    pub fn session_limit(&self) -> usize {
        self.limit.load(Ordering::Acquire)
    }

    /// Applies the stored limit (`tool_config::AgentDefaults::max_terminal_sessions`)
    /// to this registry: the one path from the setting to the sessions, taken
    /// at startup and after every save. A file that cannot be read means the
    /// default. Returns the limit now in force.
    pub fn apply_stored_limit(&self) -> usize {
        self.set_session_limit(crate::tool_config::agent_defaults().terminal_sessions() as usize);
        self.session_limit()
    }

    /// Sets the limit for the next session, clamped to `1..=MAX_PTY_SESSIONS`.
    /// Sessions already open past a lowered limit keep running; new ones wait
    /// until enough close.
    pub fn set_session_limit(&self, limit: usize) {
        self.limit
            .store(limit.clamp(1, MAX_PTY_SESSIONS), Ordering::Release);
        #[cfg(unix)]
        if let Some(host) = &self.host {
            host.set_limit(self.session_limit());
        }
    }

    /// Sets how long a reader waits for a view before shedding, clamped to
    /// 10 ms..=5 min. For tests that cannot wait the default.
    #[doc(hidden)]
    pub fn set_output_stall_timeout(&self, timeout: Duration) {
        let ms = timeout.as_millis().clamp(10, 300_000) as u64;
        self.output_stall_ms.store(ms, Ordering::Release);
        #[cfg(unix)]
        if let Some(host) = &self.host {
            host.set_stall(Duration::from_millis(ms));
        }
    }

    /// How long a reader waits on an unacknowledged window before it sheds.
    fn output_stall(&self) -> Duration {
        Duration::from_millis(self.output_stall_ms.load(Ordering::Acquire))
    }
}

/// Removes a session from the map when its reader thread ends, however it
/// ends. The normal tail removes the entry itself, before it announces the
/// exit; this is the road for every other way out, so an entry never outlives
/// the only thread that could ever finish it.
struct SessionSlot {
    sessions: Arc<Mutex<HashMap<String, SessionEntry>>>,
    id: String,
}

impl Drop for SessionSlot {
    fn drop(&mut self) {
        self.sessions
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .remove(&self.id);
    }
}

struct SessionReservation {
    active_sessions: Arc<AtomicUsize>,
}

impl Drop for SessionReservation {
    fn drop(&mut self) {
        self.active_sessions.fetch_sub(1, Ordering::AcqRel);
    }
}

fn reserve_session(state: &TerminalSessions) -> Result<SessionReservation, String> {
    state
        .active_sessions
        .try_update(Ordering::AcqRel, Ordering::Acquire, |current| {
            (current < state.session_limit()).then_some(current + 1)
        })
        .map_err(|_| {
            format!(
                "Terminal session limit reached ({}). Close a session, or raise \"Terminal sessions open at once\" in Settings → Agents (up to {MAX_PTY_SESSIONS}).",
                state.session_limit()
            )
        })?;
    Ok(SessionReservation {
        active_sessions: state.active_sessions.clone(),
    })
}

fn bounded_pty_size(rows: u16, cols: u16) -> PtySize {
    PtySize {
        rows: rows.clamp(1, MAX_PTY_ROWS),
        cols: cols.clamp(1, MAX_PTY_COLS),
        pixel_width: 0,
        pixel_height: 0,
    }
}

/// Determines the default shell for interactive sessions.
fn default_shell() -> String {
    let (key, fallback) = if cfg!(windows) {
        ("COMSPEC", "powershell.exe")
    } else if cfg!(target_os = "macos") {
        ("SHELL", "/bin/zsh")
    } else {
        ("SHELL", "/bin/sh")
    };
    std::env::var(key)
        .ok()
        .filter(|shell| !shell.trim().is_empty())
        .unwrap_or_else(|| fallback.into())
}

/// Shells that accept `-l` as "start as a login shell".
///
/// A closed set rather than "append `-l` to whatever `$SHELL` names": an
/// unknown shell handed an unknown flag fails to start at all, and a terminal
/// that will not open is strictly worse than one with a short `PATH`.
const LOGIN_FLAG_SHELLS: &[&str] = &[
    "zsh", "bash", "sh", "dash", "ksh", "mksh", "fish", "tcsh", "csh",
];

/// The login flag `shell` must be started with, or `None` to start it plain.
///
/// A GUI-launched macOS app inherits launchd's environment, not the user's, so
/// none of the shell's startup files have run and `PATH` is the bare
/// `/usr/bin:/bin:/usr/sbin:/sbin`. zsh reads `~/.zprofile` — where Homebrew's
/// `brew shellenv` and the user's `export PATH=…` lines conventionally live —
/// only for a LOGIN shell, so a plain interactive spawn produced a shell whose
/// own `~/.zshrc` died on the first line that called an installed tool.
/// Terminal.app, iTerm2 and VS Code all default to a login shell for exactly
/// this reason. Interactivity needs no flag: both families read it off the tty,
/// which a PTY session always has.
fn login_flag(shell: &str) -> Option<&'static str> {
    if cfg!(windows) {
        return None;
    }
    let name = std::path::Path::new(shell).file_name()?.to_str()?;
    LOGIN_FLAG_SHELLS.contains(&name).then_some("-l")
}

/// Terminal-identity variables the child needs that a GUI launch does not set.
///
/// Returns only what must be ADDED: an inherited value always wins, because a
/// build started from a real terminal — or a deliberate override from the
/// caller — knows more about the user's environment than a default does.
///
/// `TERM` is the one that breaks visibly. `CommandBuilder` copies the parent
/// environment verbatim (plus a `SHELL` backfill) and never sets `TERM`, and a
/// macOS `.app` launched from Finder or the Dock has none — so the shell
/// believes it has no terminal at all. `clear` fails with "TERM environment
/// variable not set" and every full-screen program (an agent CLI, `less`,
/// `vim`, anything ncurses) degrades or refuses to start. The value names what
/// the frontend actually implements: xterm.js is an xterm-256color emulator,
/// with 24-bit colour, which is what `COLORTERM` advertises.
///
/// `inject_locale` is a UTF-8 floor, not a language choice, and callers pass it
/// only on macOS. With no locale at all a child runs in the C locale, where
/// every non-ASCII byte in a path, branch name or commit message renders as
/// mojibake; `en_US.UTF-8` is guaranteed present there, whereas on Linux an
/// ungenerated locale would make things worse than the C locale it replaced.
fn missing_terminal_env(
    lookup: impl Fn(&str) -> Option<String>,
    inject_locale: bool,
) -> Vec<(&'static str, &'static str)> {
    let is_set = |name: &str| lookup(name).is_some_and(|value| !value.trim().is_empty());
    let mut env = Vec::new();
    if !is_set("TERM") {
        env.push(("TERM", "xterm-256color"));
    }
    if !is_set("COLORTERM") {
        env.push(("COLORTERM", "truecolor"));
    }
    if inject_locale && !is_set("LANG") && !is_set("LC_ALL") && !is_set("LC_CTYPE") {
        env.push(("LANG", "en_US.UTF-8"));
    }
    env
}

/// The environment view one PTY session is constructed against.
///
/// Production reads the process. Tests build a Finder/Dock-style minimal one,
/// so GUI-launch behaviour is exercised without mutating process state — the
/// same seam shape [`crate::analyzer::deps::ScanOptions`] uses, for the same
/// reason: the bugs this repairs only exist in an environment the test process
/// does not have.
struct PtyEnv {
    vars: HashMap<String, String>,
    /// True on the one platform where a missing locale is both real (launchd
    /// hands GUI apps none) and safely repairable (`en_US.UTF-8` always
    /// exists there).
    inject_locale: bool,
}

impl PtyEnv {
    fn from_process() -> Self {
        Self {
            vars: std::env::vars().collect(),
            inject_locale: cfg!(target_os = "macos"),
        }
    }

    fn get(&self, name: &str) -> Option<String> {
        self.vars.get(name).cloned()
    }

    fn os(&self, name: &str) -> Option<std::ffi::OsString> {
        self.vars.get(name).map(std::ffi::OsString::from)
    }

    fn home(&self) -> Option<std::ffi::OsString> {
        self.os("HOME").or_else(|| self.os("USERPROFILE"))
    }
}

/// The command one PTY session runs, plus the binary the requested name
/// actually resolved to.
struct PtyCommand {
    cmd: CommandBuilder,
    /// Absolute where resolution found it, the requested name where it did
    /// not — never a guess dressed up as a location.
    resolved: String,
}

/// Builds the command for one PTY session.
///
/// Split out of [`spawn_session`] because everything that was wrong here is
/// decided before any process exists: which binary a bare name resolves to,
/// whether the shell starts as a login shell, and which variables the child
/// inherits. A `CommandBuilder` is fully inspectable, so all three are
/// testable without a PTY, an `AppHandle`, or an agent CLI on the host.
fn build_pty_command(
    shell: &str,
    is_default_shell: bool,
    args: Option<&[String]>,
    env: Option<&HashMap<String, String>>,
    cwd: &std::path::Path,
    pty_env: &PtyEnv,
) -> PtyCommand {
    // PATH repair for GUI launches, through the same owner every other
    // subsystem resolves spawns with (`git_cli`, written because
    // Finder-launched builds reported installed tools as missing). Two
    // distinct needs: a bare agent-CLI name is one THIS process must resolve,
    // and the child's own lookups — a login shell's rc file calling `brew`, an
    // agent CLI shelling out to `node` — must see the same directories.
    let home = pty_env.home();
    let child_path = extended_child_path(pty_env.os("PATH").as_deref(), home.as_deref());
    let resolved = resolve_spawn_program_with(shell, child_path.as_deref(), home.as_deref());

    let mut cmd = CommandBuilder::new(&resolved);
    // Only the user's own shell gets the login flag; an agent CLI handed `-l`
    // would fail to start.
    if is_default_shell {
        if let Some(flag) = login_flag(&resolved) {
            cmd.arg(flag);
        }
    }
    for arg in args.unwrap_or_default() {
        cmd.arg(arg);
    }
    // Order matters: defaults first, then PATH, then the caller's map, so an
    // explicit value from the IPC caller always wins over anything defaulted
    // here.
    for (key, value) in missing_terminal_env(|name| pty_env.get(name), pty_env.inject_locale) {
        cmd.env(key, value);
    }
    if let Some(ref path) = child_path {
        cmd.env("PATH", path);
    }
    for (key, value) in env.into_iter().flatten() {
        cmd.env(key, value);
    }
    cmd.cwd(cwd);
    PtyCommand { cmd, resolved }
}

/// Why cmd.exe would read this launch as more than one command, or `None`.
///
/// Windows starts a `.cmd`/`.bat` program — how npm installs `codex`,
/// `claude` and `grok` — through cmd.exe, which re-reads the command line
/// portable-pty built for a C runtime. There, `& | < > ^` outside a quoted
/// region are syntax, `%NAME%` expands quoted or not, and a line break ends
/// the command; so a prompt `fix&calc`, or a checkout under `C:\R&D`, ran a
/// second program. Escaping for cmd is a different grammar per character,
/// so this refuses instead, naming the argument.
///
/// The command line is rebuilt exactly as portable-pty's `append_quoted`
/// writes it — quoted only when an argument holds a space, tab, `\n`, `\v`
/// or `"`, inner quotes written `\"` — and read the way cmd reads it: every
/// `"` toggles its quoting, backslash or not. A task prompt's JSON-quoted
/// brief path therefore passes unless that path itself holds syntax.
fn batch_reparse_refusal(program: &str, args: &[String]) -> Option<String> {
    let lower = program.to_ascii_lowercase();
    if !(lower.ends_with(".cmd") || lower.ends_with(".bat")) {
        return None;
    }
    // cmd's quoting is one state across the whole line, not per argument: an
    // argument with an odd count of inner quotes leaves it inside a region,
    // and the next argument's opening quote then takes it out.
    let mut inside = false;
    std::iter::once(program)
        .chain(args.iter().map(String::as_str))
        .find(|arg| cmd_would_reinterpret(arg, &mut inside))
        .map(|arg| {
            let shown: String = arg.chars().take(80).collect();
            format!(
                "{program} is a batch file, which Windows starts through cmd.exe, and cmd.exe would read \
                 {shown:?} as part of a command rather than as text. Remove & | < > ^ % or line breaks from \
                 it, or install the program as an .exe."
            )
        })
}

/// Whether cmd.exe would act on `arg` as portable-pty quotes it, given
/// whether cmd is `inside` a quoted region where the argument starts; leaves
/// `inside` where the argument ends.
fn cmd_would_reinterpret(arg: &str, inside: &mut bool) -> bool {
    if arg.contains(['%', '\r', '\n']) {
        return true;
    }
    let quoted = arg.is_empty() || arg.contains([' ', '\t', '\n', '\x0b', '"']);
    // The opening quote, each inner one and the closing one all toggle.
    if quoted {
        *inside = !*inside;
    }
    for c in arg.chars() {
        match c {
            '"' => *inside = !*inside,
            '&' | '|' | '<' | '>' | '^' if !*inside => return true,
            _ => {}
        }
    }
    if quoted {
        *inside = !*inside;
    }
    false
}

/// The agent CLIs a terminal tab can start, by the name they are invoked as.
///
/// The same set the tab strip offers, minus the user's own shell.
/// `terminal_launcher_kinds_match_the_tab_strip` in `tabs.contract.test.ts`
/// reads this list and fails if the frontend's drifts from it.
pub const AGENT_LAUNCHERS: [&str; 5] = ["claude", "manvi", "codex", "grok", "agy"];

/// Which agent CLI a PTY program is, or `None` for a shell.
///
/// Matched on the file *stem*, not on the whole path. `shell.contains("claude")`
/// — which is what the ledger used to ask — is true of `/Users/claude/bin/zsh`,
/// so a user with that name had every interactive shell they opened recorded as
/// an agent session. One owner now answers the question for the ledger's actor
/// kind, for what a hook reports as its agent, and for whether a bell is worth
/// a banner, because those three must never disagree about what a session is.
pub(crate) fn launcher_kind(program: &str) -> Option<&'static str> {
    let stem = std::path::Path::new(program).file_stem()?.to_str()?;
    AGENT_LAUNCHERS.into_iter().find(|kind| *kind == stem)
}

/// What a banner calls that agent. Resolved through the bridge's vocabulary so
/// a session's own notification and a hook's report about it never use two
/// different names for the same CLI.
pub(crate) fn agent_launcher(program: &str) -> Option<&'static str> {
    let kind = launcher_kind(program)?;
    crate::alerts::bridge::AGENTS
        .iter()
        .find(|(name, _)| *name == kind)
        .map(|(_, label)| *label)
}

/// One session's standing interest in its own output.
///
/// Owned by the reader thread and touched by nothing else, so reading a
/// terminal costs no lock. It holds the resumable scanner state plus the two
/// strings a banner needs, resolved once at spawn.
struct SessionWatcher {
    key: String,
    label: String,
    place: Option<String>,
    is_agent: bool,
    scanner: crate::alerts::scan::Scanner,
    /// The scanner's own overflow count, mirrored so only the delta is
    /// reported and the total cannot be double-counted.
    reported_drops: u64,
    /// Where a notice goes: a banner in the app, a frame from the host.
    events: Arc<dyn SessionEvents>,
}

impl SessionWatcher {
    fn new(
        session_id: &str,
        program: &str,
        cwd: &std::path::Path,
        events: Arc<dyn SessionEvents>,
    ) -> Self {
        let agent = agent_launcher(program);
        Self {
            key: session_id.to_owned(),
            label: agent.unwrap_or("Terminal").to_owned(),
            place: cwd
                .file_name()
                .map(|name| name.to_string_lossy().into_owned()),
            is_agent: agent.is_some(),
            scanner: crate::alerts::scan::Scanner::new(),
            reported_drops: 0,
            events,
        }
    }

    fn observe(&mut self, bytes: &[u8]) {
        for signal in self.scanner.feed(bytes) {
            self.events.notice(crate::alerts::Notice {
                key: self.key.clone(),
                origin: crate::alerts::Origin::Terminal,
                label: self.label.clone(),
                place: self.place.clone(),
                // A terminal signal carries no reason of its own; only the
                // hook bridge knows why. Saying "needs your attention" here
                // would be inventing one.
                event: None,
                detail: signal.title.or(signal.body),
                is_agent: self.is_agent,
                channel: signal.channel.label(),
                subject: None,
                subagent: false,
            });
        }
        let dropped = self.scanner.dropped();
        let new_drops = dropped.saturating_sub(self.reported_drops);
        if new_drops > 0 {
            self.events.scan_drops(new_drops);
        }
        self.reported_drops = dropped;
    }

    /// The session's output has ended. Whatever it was asking can no longer
    /// be answered, whether or not its agent's own `SessionEnd` hook ran — a
    /// killed process runs no hooks.
    fn ended(&self) {
        self.events.notice(crate::alerts::Notice {
            key: self.key.clone(),
            origin: crate::alerts::Origin::Terminal,
            label: self.label.clone(),
            place: self.place.clone(),
            event: Some("session_ended"),
            detail: None,
            is_agent: self.is_agent,
            channel: "exit",
            subject: None,
            subagent: false,
        });
    }
}

static SESSION_COUNTER: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1);

fn normalize_task_environment(cmd: &mut CommandBuilder, cwd: &std::path::Path) {
    // Apply only to an explicitly task-linked launch. No user shell/global
    // settings are rewritten, and credentials remain inherited by the provider.
    let names: Vec<String> = cmd
        .iter_full_env_as_str()
        .map(|(key, _)| key.to_owned())
        .collect();
    for name in names {
        if matches!(
            name.as_str(),
            "GIT_DIR"
                | "GIT_WORK_TREE"
                | "GIT_INDEX_FILE"
                | "GIT_COMMON_DIR"
                | "GIT_OBJECT_DIRECTORY"
                | "GIT_ALTERNATE_OBJECT_DIRECTORIES"
                | "GIT_NAMESPACE"
                | "GIT_PREFIX"
                | "GIT_CONFIG"
                | "GIT_CONFIG_GLOBAL"
                | "GIT_CONFIG_SYSTEM"
                | "GIT_CONFIG_COUNT"
                | "GIT_CONFIG_PARAMETERS"
                | "CLAUDECODE"
                | "CLAUDE_CODE_ENTRYPOINT"
        ) || name.starts_with("GIT_CONFIG_KEY_")
            || name.starts_with("GIT_CONFIG_VALUE_")
        {
            cmd.env_remove(name);
        }
    }
    cmd.env("DEVCOUNCIL_ROOT", cwd);
    cmd.env("PWD", cwd);
}

#[cfg(test)]
#[test]
fn task_environment_replaces_root_overrides_but_preserves_auth_and_parent_settings() {
    use std::ffi::OsStr;
    let mut command = CommandBuilder::new("codex");
    command.env_clear();
    let roots = [
        "GIT_DIR",
        "GIT_WORK_TREE",
        "GIT_INDEX_FILE",
        "GIT_COMMON_DIR",
        "GIT_OBJECT_DIRECTORY",
        "GIT_ALTERNATE_OBJECT_DIRECTORIES",
        "GIT_NAMESPACE",
        "GIT_PREFIX",
        "GIT_CONFIG",
        "GIT_CONFIG_GLOBAL",
        "GIT_CONFIG_SYSTEM",
        "GIT_CONFIG_COUNT",
        "GIT_CONFIG_PARAMETERS",
        "GIT_CONFIG_KEY_0",
        "GIT_CONFIG_VALUE_0",
        "CLAUDECODE",
        "CLAUDE_CODE_ENTRYPOINT",
    ];
    for name in roots {
        command.env(name, "fixture-conflict");
    }
    for name in [
        "GIT_ASKPASS",
        "GIT_SSH_COMMAND",
        "SSH_AUTH_SOCK",
        "HOME",
        "CODEX_HOME",
        "CLAUDE_CONFIG_DIR",
    ] {
        command.env(name, "fixture-preserved");
    }
    command.env("DEVCOUNCIL_ROOT", "fixture-parent-root");
    let ordinary = command.clone();
    normalize_task_environment(&mut command, std::path::Path::new("/selected checkout"));
    for name in roots {
        assert!(
            command.get_env(name).is_none(),
            "conflicting setting {name}"
        );
        assert_eq!(ordinary.get_env(name), Some(OsStr::new("fixture-conflict")));
    }
    for name in [
        "GIT_ASKPASS",
        "GIT_SSH_COMMAND",
        "SSH_AUTH_SOCK",
        "HOME",
        "CODEX_HOME",
        "CLAUDE_CONFIG_DIR",
    ] {
        assert_eq!(command.get_env(name), Some(OsStr::new("fixture-preserved")));
    }
    assert_eq!(
        command.get_env("DEVCOUNCIL_ROOT"),
        Some(OsStr::new("/selected checkout"))
    );
    assert_eq!(
        command.get_env("PWD"),
        Some(OsStr::new("/selected checkout"))
    );
    assert_eq!(
        ordinary.get_env("DEVCOUNCIL_ROOT"),
        Some(OsStr::new("fixture-parent-root"))
    );
}

/// Whether a read error from the PTY master means the session's side closed.
/// On Unix that is `EIO` once every holder of the slave has gone; ConPTY's pipe
/// reports a broken pipe. Anything else is a fault, not an ending.
fn is_end_of_terminal(error: &std::io::Error) -> bool {
    #[cfg(unix)]
    if error.raw_os_error() == Some(libc::EIO) {
        return true;
    }
    matches!(
        error.kind(),
        std::io::ErrorKind::BrokenPipe | std::io::ErrorKind::UnexpectedEof
    )
}

/// Snapshot of the parts of [`portable_pty::ExitStatus`] the exit event
/// reports, extracted so the finalize logic is unit-testable without
/// spawning a real PTY-backed process.
#[derive(Debug, Clone, PartialEq, Eq)]
struct ExitStatusLike {
    code: u32,
    signal: Option<String>,
}

impl From<&portable_pty::ExitStatus> for ExitStatusLike {
    fn from(status: &portable_pty::ExitStatus) -> Self {
        // On Unix a signal death surfaces as portable-pty's fallback code 1
        // (its `From<std::process::ExitStatus>`) plus Some(signal name). The
        // code is invented there; `finalize_pty_session` drops it.
        ExitStatusLike {
            code: status.exit_code(),
            signal: status.signal().map(str::to_string),
        }
    }
}

/// Reaps the shell and emits exactly one authoritative "terminal-exit" event.
///
/// `wait` blocks until the child terminates; the reader thread calls this only
/// after the master side hit EOF/error. EOF can precede child exit, so the
/// caller keeps its killable registry entry until reaping. A successful wait
/// reports the real exit code. A shell killed by a signal has no exit code, so
/// it reports the signal name and `exit_code: None` — portable-pty's fallback 1
/// would read as an ordinary failure in a run record. A failed wait also
/// degrades to `exit_code: None` instead of inventing a status.
fn finalize_pty_session<E, W>(session_id: &str, wait: W, mut emit: E)
where
    W: FnOnce() -> std::io::Result<ExitStatusLike>,
    E: FnMut(TerminalExitPayload),
{
    let (exit_code, signal, error, reaped) = match wait() {
        Ok(status) => (
            status
                .signal
                .is_none()
                .then(|| i32::try_from(status.code).unwrap_or(-1)),
            status.signal.unwrap_or_default(),
            None,
            true,
        ),
        Err(e) => {
            log::warn!(
                target: "terminal",
                "could not reap shell process: {e}"
            );
            (
                None,
                String::new(),
                Some(format!("Could not confirm process exit: {e}")),
                false,
            )
        }
    };
    emit(TerminalExitPayload {
        id: session_id.to_string(),
        exit_code,
        signal,
        error,
        reaped,
    });
}

/// The exit of a child stopped because its reader thread could not start:
/// the same normalisation as every other exit, with the reason attached.
fn reader_failure_payload(
    session_id: &str,
    exit: std::io::Result<ExitStatusLike>,
    reason: &dyn std::fmt::Display,
) -> TerminalExitPayload {
    let mut payload = TerminalExitPayload {
        id: session_id.to_owned(),
        exit_code: None,
        signal: String::new(),
        error: None,
        reaped: false,
    };
    finalize_pty_session(session_id, || exit, |p| payload = p);
    payload.error = Some(format!("PTY reader failed: {reason}"));
    payload
}

// PTY commands are user-authored and may contain newlines (e.g. sh -c).
// They share size bounds with Console but do not inherit its control policy.
fn validate_pty_options(
    program: Option<&str>,
    args: Option<&[String]>,
    env: Option<&HashMap<String, String>>,
) -> Result<(), String> {
    if let Some(program) = program {
        if program.trim().is_empty()
            || program.len() > TERMINAL_ARG_BYTES_CAP
            || program.contains('\0')
        {
            return Err("Invalid terminal program".into());
        }
    }
    let args = args.unwrap_or_default();
    if args.len() > TERMINAL_ARG_COUNT_CAP {
        return Err("Too many terminal arguments".into());
    }
    let mut total = program.map_or(0, str::len);
    for arg in args {
        if arg.len() > TERMINAL_ARG_BYTES_CAP || arg.contains('\0') {
            return Err("Invalid or oversized terminal argument".into());
        }
        total = total.saturating_add(arg.len());
    }
    if total > TERMINAL_ARGV_BYTES_CAP {
        return Err("Terminal arguments exceed 128 KiB".into());
    }
    if let Some(env) = env {
        if env.len() > 256 {
            return Err("Too many terminal environment entries".into());
        }
        let mut total = 0usize;
        for (key, value) in env {
            if key.is_empty()
                || key.contains(['=', '\0'])
                || value.contains('\0')
                || key.len() > 1024
                || value.len() > TERMINAL_ARG_BYTES_CAP
            {
                return Err("Invalid or oversized terminal environment entry".into());
            }
            total = total.saturating_add(key.len()).saturating_add(value.len());
        }
        if total > TERMINAL_ARGV_BYTES_CAP {
            return Err("Terminal environment exceeds 128 KiB".into());
        }
    }
    Ok(())
}

/// Spawns a PTY session running an interactive shell or agent CLI in
/// `repo_path`. Its output goes to `output` from the first byte; a session is
/// never started without somewhere to send it, because a prompt printed before
/// a view attached would be lost.
#[allow(clippy::too_many_arguments)]
pub fn spawn_session<R: tauri::Runtime>(
    app: &AppHandle<R>,
    state: &TerminalSessions,
    repo_path: &str,
    rows: u16,
    cols: u16,
    program: Option<String>,
    args: Option<Vec<String>>,
    env: Option<HashMap<String, String>>,
    output: OutputSink,
) -> Result<TerminalSpawned, String> {
    spawn_session_in(
        app, state, repo_path, None, rows, cols, program, args, env, output,
    )
}

/// [`spawn_session`], started in `start_dir`: a path relative to the
/// repository root that must resolve, symlinks included, to a directory inside
/// it. `None`, `""` and `"."` start at the root. A directory that has gone, or
/// that escapes the repository, is refused — never silently replaced by the
/// root, because the caller asked for a place and a shell somewhere else is a
/// different answer it has to be told about.
#[allow(clippy::too_many_arguments)]
pub fn spawn_session_in<R: tauri::Runtime>(
    app: &AppHandle<R>,
    state: &TerminalSessions,
    repo_path: &str,
    start_dir: Option<&str>,
    rows: u16,
    cols: u16,
    program: Option<String>,
    args: Option<Vec<String>>,
    env: Option<HashMap<String, String>>,
    output: OutputSink,
) -> Result<TerminalSpawned, String> {
    spawn_session_inner(
        app, state, repo_path, start_dir, rows, cols, program, args, env, output, None,
    )
}

/// Longest `start_dir` accepted over IPC, in bytes: PATH_MAX on both Unix hosts.
const START_DIR_BYTES_CAP: usize = 4096;

/// Resolves a requested start directory against the canonical repository,
/// through the same containment owner every repository-relative path uses.
fn resolve_start_dir(
    repo: &std::path::Path,
    start_dir: Option<&str>,
) -> Result<std::path::PathBuf, String> {
    let Some(relative) = start_dir.filter(|dir| !dir.is_empty() && *dir != ".") else {
        return Ok(repo.to_path_buf());
    };
    if relative.len() > START_DIR_BYTES_CAP || relative.chars().any(char::is_control) {
        return Err("Invalid terminal start directory".into());
    }
    let joined = sandbox_join_canonical(repo, relative)
        .map_err(|e| format!("Terminal start directory refused: {e}"))?;
    if !joined.is_dir() {
        return Err(format!(
            "Terminal start directory is not a directory: {}",
            joined.display()
        ));
    }
    Ok(joined)
}

/// A directory inside a checkout, resolved: its canonical path, the root of
/// the checkout holding it (the nearest enclosing work tree, by
/// `find_git_root`), and the directory relative to that root (`None` at the
/// root itself). Grants nothing: the root still has to be validated and
/// trusted, and the relative part is held inside it by `resolve_start_dir`.
pub(crate) fn checkout_root(
    path: &str,
) -> Result<(std::path::PathBuf, std::path::PathBuf, Option<String>), String> {
    let dir = std::path::Path::new(path)
        .canonicalize()
        .map_err(|e| format!("Cannot access path '{path}': {e}"))?;
    if !dir.is_dir() {
        return Err(format!("Not a directory: {}", dir.display()));
    }
    let root = crate::engine::git_cli::find_git_root(&dir)
        .ok_or_else(|| format!("Not inside a Git checkout: {}", dir.display()))?;
    let relative = dir
        .strip_prefix(&root)
        .map_err(|_| format!("{} is outside its checkout", dir.display()))?
        .to_str()
        .ok_or("The directory path is not valid Unicode")?
        .to_owned();
    Ok((dir, root, (!relative.is_empty()).then_some(relative)))
}

/// The checkout root and start directory of a task attempt whose working
/// directory was recorded, canonical, at preparation. A recorded directory
/// that now resolves anywhere else — replaced by a link, or a link swapped
/// above it — is refused, never followed: the root found from where it
/// points would be some other repository's, trusted or not.
fn task_start(cwd: &str) -> Result<(String, Option<String>), String> {
    let (dir, root, relative) = checkout_root(cwd)?;
    if dir != std::path::Path::new(cwd) {
        return Err(format!(
            "The task's working directory {cwd} now resolves to {}. Prepare a new attempt.",
            dir.display()
        ));
    }
    let root = root
        .to_str()
        .ok_or("The checkout path is not valid Unicode")?
        .to_owned();
    Ok((root, relative))
}

/// What a live session is doing, read once, on request.
///
/// Every field is optional because the OS can decline to describe a process
/// (a setuid program, a process that exited between two calls). An unknown
/// is reported as unknown — never as idle, and never as the repository root.
#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct TerminalContext {
    /// Name of the program in the terminal's foreground.
    pub process: Option<String>,
    /// Whether that program is something other than the session's own
    /// process: a job running under the shell. `None` when the foreground
    /// could not be read.
    pub busy: Option<bool>,
    /// Whether the shell has any job left under it, in the foreground or not:
    /// `cmd &`, `nohup`, a dev server, a `^Z`'d editor. `busy` alone reads
    /// false for all of those, and a close that trusts it kills them. `None`
    /// when the session's processes could not be listed.
    pub jobs: Option<bool>,
    /// Absolute working directory of the foreground program.
    pub cwd: Option<String>,
    /// `cwd` relative to the repository root (`""` for the root itself), or
    /// `None` when it is outside the repository or unknown. Only this is a
    /// valid `start_dir` for a new session.
    pub repo_dir: Option<String>,
}

/// Reads [`TerminalContext`] for one session.
///
/// The child lock is held across the reads, as `terminate_pty_child` holds it
/// across its listings and signals: the reader thread cannot reap the shell
/// meanwhile, so its pid — and the session it leads — cannot be reused under
/// this call. A terminal that is being closed answers with nothing known, as an
/// exited one does: its closer holds the shell unreaped while it waits, and a
/// read here would reap it.
pub fn session_context(
    state: &TerminalSessions,
    session_id: &str,
) -> Result<TerminalContext, String> {
    #[cfg(unix)]
    if let Some(host) = &state.host {
        return host.context(session_id);
    }
    let (child, master, repo, closing) = state
        .lock_sessions()
        .get(session_id)
        .map(|s| {
            (
                s.child.clone(),
                s.master.clone(),
                s.repo.clone(),
                s.closing.clone(),
            )
        })
        .ok_or_else(|| format!("Terminal session '{session_id}' not found"))?;
    #[cfg(unix)]
    {
        let mut child = child
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        // Checked with the lock held: a close that has begun is relying on the
        // shell staying unreaped, and the `try_wait` below would reap it.
        if closing.load(Ordering::SeqCst) > 0 {
            return Ok(TerminalContext::default());
        }
        if child.try_wait().map_err(|e| e.to_string())?.is_some() {
            return Ok(TerminalContext::default());
        }
        let Some(root) = child.process_id().and_then(|p| i32::try_from(p).ok()) else {
            return Ok(TerminalContext::default());
        };
        let group = master
            .lock()
            .map_err(|e| format!("Terminal size lock error: {e}"))?
            .process_group_leader();
        // Same ownership rule as the kill path: a group counts only when it
        // belongs to this child's own session.
        // SAFETY: getsid takes a numeric id and reads nothing else.
        let leader = group.filter(|g| *g > 1 && unsafe { libc::getsid(*g) } == root);
        let jobs = foreground::session_members(root)
            .ok()
            .map(|members| !members.is_empty());
        let view = foreground::describe(leader.unwrap_or(root));
        // A program the OS will not describe (sudo, a setuid tool) still runs
        // in the shell's directory far more often than not, but "far more
        // often" is not "is": only the shell's own answer stands in for it.
        let cwd = view.cwd.or_else(|| {
            leader
                .filter(|g| *g != root)
                .and_then(|_| foreground::describe(root).cwd)
        });
        drop(child);
        let repo_dir = cwd
            .as_deref()
            .and_then(|cwd| cwd.strip_prefix(&repo).ok())
            .and_then(|rel| rel.to_str())
            .map(str::to_owned);
        Ok(TerminalContext {
            process: view.name,
            busy: leader.map(|g| g != root),
            jobs,
            cwd: cwd.map(|p| p.to_string_lossy().into_owned()),
            repo_dir,
        })
    }
    #[cfg(not(unix))]
    {
        let _ = (child, master, repo, closing);
        Ok(TerminalContext::default())
    }
}

/// Whether this process still holds the PTY session of `run_id`, dead or not.
///
/// Unlike `attach_tracked_session`, a session whose child has already exited still
/// counts: its entry leaves the map only after the run observer has written
/// the exit receipt, so while it is here the real outcome may still arrive,
/// and nothing else may record one in its place.
pub(crate) fn tracks_run(state: &TerminalSessions, run_id: &str) -> bool {
    #[cfg(unix)]
    if let Some(host) = &state.host {
        return host.tracks_run(run_id);
    }
    state
        .lock_sessions()
        .values()
        .any(|s| s.tracked_run.as_deref() == Some(run_id))
}

/// A live PTY as a reloaded renderer needs to see it: enough to count it
/// against the shared session limit, to say what it is and where, and to
/// take it over.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TerminalListing {
    pub id: String,
    pub shell: String,
    pub cwd: String,
    /// The repository the session was opened for (its canonical root).
    pub repo: String,
    /// The agent CLI it runs, when it runs one (`claude`, `codex`, …).
    pub launcher: Option<String>,
    /// The task attempt it belongs to, when it is one.
    pub run_id: Option<String>,
    /// No view holds it: the page that started it was reloaded.
    pub detached: bool,
    /// Its terminal has closed (or it was told to stop) but the process has
    /// not been reaped: `tracks_run` still counts it, so a run stays "running"
    /// while nothing showed it. Listed so it can be seen and stopped, never
    /// shown again.
    #[serde(default)]
    pub ending: bool,
}

/// Every session this process (or its terminal host) still owns, live or
/// ending. Bounded by `MAX_PTY_SESSIONS`. `Err` when the host could not be
/// asked, which is not the same as holding nothing.
pub fn list_sessions(state: &TerminalSessions) -> Result<Vec<TerminalListing>, String> {
    #[cfg(unix)]
    if let Some(host) = &state.host {
        return host.list();
    }
    let guard = state.lock_sessions();
    let mut listed: Vec<TerminalListing> = guard
        .values()
        .map(|s| TerminalListing {
            id: s.spawned.id.clone(),
            shell: s.spawned.shell.clone(),
            cwd: s.spawned.cwd.clone(),
            repo: s.repo.to_string_lossy().into_owned(),
            launcher: launcher_kind(&s.spawned.shell).map(str::to_owned),
            run_id: s.tracked_run.clone(),
            ending: s.dead.load(Ordering::Acquire),
            // An ending session has no view to show it in: it is only ever
            // listed, so it reads as detached.
            detached: s.flow.is_detached() || s.dead.load(Ordering::Acquire),
        })
        .collect();
    listed.sort_by(|a, b| a.id.cmp(&b.id));
    Ok(listed)
}

/// The page holding every session is being replaced (a webview reload).
/// Each live session stops waiting on that page's acknowledgements; none is
/// stopped. Returns how many were detached.
pub fn detach_all(state: &TerminalSessions) -> usize {
    #[cfg(unix)]
    if let Some(host) = &state.host {
        // A count is all the caller logs; a host that could not be told has
        // already detached every view of an app whose page it cannot reach.
        return host.detach_all().unwrap_or_else(|error| {
            log::warn!(target: "terminal", "the terminal host could not be told the page reloaded: {error}");
            0
        });
    }
    let live: Vec<(Arc<OutputFlow>, Arc<Mutex<ring::Ring>>)> = state
        .lock_sessions()
        .values()
        .filter(|session| !session.dead.load(Ordering::Acquire))
        .map(|session| (session.flow.clone(), session.ring.clone()))
        .collect();
    for (flow, ring) in &live {
        let_go(flow, ring);
    }
    live.len()
}

/// The view holding a session is gone: its reader stops waiting on that view's
/// credit, and the ring marks where unseen output begins.
///
/// The flow first, outside the ring's lock: a reader waiting for credit holds
/// that lock, and only the detach wakes it. The mark is set only if the
/// session is still detached once the lock is had, because a view may have
/// taken it over in between.
fn let_go(flow: &OutputFlow, ring: &Mutex<ring::Ring>) {
    flow.detach();
    let mut ring = ring
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    if flow.is_detached() {
        ring.detached();
    }
}

/// What a view taking a session over needs of it.
struct Takeover {
    flow: Arc<OutputFlow>,
    output: Arc<OutputSlot>,
    ring: Arc<Mutex<ring::Ring>>,
    events: Arc<dyn SessionEvents>,
    master: Arc<Mutex<Box<dyn MasterPty + Send>>>,
    spawned: TerminalSpawned,
}

impl Takeover {
    fn of(session: &SessionEntry) -> Self {
        Self {
            flow: session.flow.clone(),
            output: session.output.clone(),
            ring: session.ring.clone(),
            events: session.events.clone(),
            master: session.master.clone(),
            spawned: session.spawned.clone(),
        }
    }

    /// Hands the session to `output` at `rows`×`cols`: the ring's newest bytes
    /// first, on the new view's fresh window, then everything the program
    /// prints from here on, then a resize nudge so a full-screen program also
    /// redraws itself. `forced` takes it from a view that still holds it.
    fn hand_to(
        self,
        output: OutputSink,
        rows: u16,
        cols: u16,
        forced: bool,
        stall: Duration,
    ) -> Result<TerminalSpawned, String> {
        if forced {
            let_go(&self.flow, &self.ring);
        } else if !self.flow.is_detached() {
            // Refused before the ring's lock, which a reader waiting on the
            // holding view's credit keeps for as long as it waits.
            return Err("This terminal is already shown in a tab.".into());
        }
        {
            let mut ring = self
                .ring
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            if !self.flow.attach() {
                return Err("This terminal is already shown in a tab.".into());
            }
            // The new view's channel, before anything is sent to it: the
            // replay and the repaint are what it is shown first.
            self.output.replace(output);
            let replay = ring.attached();
            if replay.lost_while_detached > 0 {
                self.events.loss(TerminalLossPayload {
                    id: self.spawned.id.clone(),
                    message: ring::lost_notice(replay.lost_while_detached),
                });
            }
            let sink = self.output.current();
            for chunk in replay.bytes.chunks(OUTPUT_CHUNK_BYTES) {
                // The ring is one window, and the window is fresh, so this
                // never waits; anything but Deliver means the view went away.
                match self.flow.reserve(chunk.len(), stall) {
                    Ok(Route::Deliver | Route::Resume) => {
                        if sink.bytes(&self.spawned.id, chunk).is_err() {
                            self.flow.shed(chunk.len());
                            break;
                        }
                    }
                    Ok(Route::Hold | Route::Shed { .. }) | Err(_) => break,
                }
            }
        }
        repaint(&self.master, rows, cols);
        Ok(self.spawned)
    }
}

/// The one view holding this session has gone without closing it — a tab
/// unmounted by anything other than its Close. The process keeps running,
/// undelivered, until a view takes it over (`attach_session`) or the user
/// stops it. Returns whether it was live and is now detached; an id that has
/// already ended is not an error, there is simply nothing left to keep.
pub fn detach_session(state: &TerminalSessions, session_id: &str) -> Result<bool, String> {
    #[cfg(unix)]
    if let Some(host) = &state.host {
        return host.detach(session_id);
    }
    let Some((flow, ring)) = state
        .lock_sessions()
        .get(session_id)
        .filter(|s| !s.dead.load(Ordering::Acquire))
        .map(|s| (s.flow.clone(), s.ring.clone()))
    else {
        return Ok(false);
    };
    let_go(&flow, &ring);
    log::info!(target: "terminal", "terminal view went away; session detached and left running");
    Ok(true)
}

/// Hands a detached session to a new view at that view's size.
///
/// Refused for a session a view still holds: two views of one PTY would both
/// paint and both acknowledge. The new view is sent the session's newest
/// output (its ring) first, so a shell prompt, a test run or `tail -f` comes
/// back as it was, and is told when output printed while it was away is no
/// longer kept. The size is then set twice, the first time one column off,
/// because a resize to the size a process already has sends no `SIGWINCH`:
/// without the change a full-screen program (Claude Code, an editor) would not
/// redraw over the replay until the next keypress.
pub fn attach_session(
    state: &TerminalSessions,
    session_id: &str,
    rows: u16,
    cols: u16,
    output: OutputSink,
) -> Result<TerminalSpawned, String> {
    #[cfg(unix)]
    if let Some(host) = &state.host {
        return host.attach(session_id, rows, cols, output);
    }
    let takeover = state
        .lock_sessions()
        .get(session_id)
        .filter(|s| !s.dead.load(Ordering::Acquire))
        .map(Takeover::of)
        .ok_or_else(|| format!("Terminal session '{session_id}' has ended"))?;
    takeover.hand_to(output, rows, cols, false, state.output_stall())
}

/// The live tracked task session of `run_id`, which repairs a lost launch
/// response without issuing another process. The binding is created by the
/// PTY host, never supplied by storage.
///
/// Taken over by the calling view's channel, whether the page that held it was
/// reloaded or the launch response was lost in the same page.
pub(crate) fn attach_tracked_session(
    state: &TerminalSessions,
    run_id: &str,
    rows: u16,
    cols: u16,
    output: OutputSink,
) -> Option<TerminalSpawned> {
    #[cfg(unix)]
    if let Some(host) = &state.host {
        return host.attach_tracked(run_id, rows, cols, output);
    }
    let takeover = state
        .lock_sessions()
        .values()
        .find(|s| s.tracked_run.as_deref() == Some(run_id) && !s.dead.load(Ordering::Acquire))
        .map(Takeover::of)?;
    // The caller brings its own channel, so even a session a view still holds
    // is taken over: the channel the lost launch response went with belongs to
    // a call that never learned this session's id, and what it was sent will
    // never be shown or repaid. A fresh window, the ring and a repaint are what
    // a view that starts here needs.
    takeover
        .hand_to(output, rows, cols, true, state.output_stall())
        .inspect_err(|error| log::warn!(target: "terminal", "a task terminal could not be taken over: {error}"))
        .ok()
}

/// Has the program redraw at the size it already has: what a view that
/// skipped output needs to show the screen as it is.
fn repaint_current(master: &Arc<Mutex<Box<dyn MasterPty + Send>>>) {
    let size = master
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .get_size();
    match size {
        Ok(size) => repaint(master, size.rows, size.cols),
        Err(error) => {
            log::warn!(target: "terminal", "could not read a terminal's size to repaint it: {error}")
        }
    }
}

fn repaint(master: &Arc<Mutex<Box<dyn MasterPty + Send>>>, rows: u16, cols: u16) {
    let target = bounded_pty_size(rows, cols);
    let nudge = bounded_pty_size(rows, if target.cols > 1 { target.cols - 1 } else { 2 });
    let master = master
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    for size in [nudge, target] {
        if let Err(error) = master.resize(size) {
            log::warn!(target: "terminal", "could not resize a reattached terminal: {error}");
        }
    }
}

/// Starts a task attempt's agent in its recorded working directory: the
/// checkout's root is what is validated and trusted, and the PTY starts in
/// the directory inside it — a subdirectory when the attempt was prepared
/// for one.
#[allow(clippy::too_many_arguments)]
pub(crate) fn spawn_tracked_session<R: tauri::Runtime>(
    app: &AppHandle<R>,
    state: &TerminalSessions,
    cwd: &str,
    rows: u16,
    cols: u16,
    program: String,
    args: Vec<String>,
    output: OutputSink,
    observer: Arc<dyn SessionObserver>,
) -> Result<TerminalSpawned, String> {
    spawn_at(
        app,
        state,
        Place::Task {
            cwd: cwd.to_owned(),
        },
        rows,
        cols,
        Some(program),
        Some(args),
        None,
        output,
        Some(observer),
    )
}

/// Where a session starts.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum Place {
    /// `start_dir` (relative, `None` for the root) inside the repository
    /// `repo_path`.
    Repo {
        repo_path: String,
        start_dir: Option<String>,
    },
    /// A task attempt's recorded working directory: its checkout's root is
    /// what is validated and trusted.
    Task { cwd: String },
}

impl Place {
    /// The canonical repository and the directory inside it to start in,
    /// checked the way every spawn is: a git work tree, and a start directory
    /// that resolves inside it.
    fn resolve(&self) -> Result<(std::path::PathBuf, std::path::PathBuf), String> {
        let (root, start_dir) = match self {
            Place::Repo {
                repo_path,
                start_dir,
            } => (repo_path.clone(), start_dir.clone()),
            Place::Task { cwd } => task_start(cwd)?,
        };
        let repo = validate_repo(&root)?;
        let start = resolve_start_dir(&repo, start_dir.as_deref())?;
        Ok((repo, start))
    }
}

/// One request to start a session: everything the engine needs, and nothing
/// it could only find in the app. Crosses the host socket as it is.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SpawnRequest {
    /// Minted by the app (`next_session_id`), so a run's claim can name the
    /// session before the process exists.
    pub session_id: String,
    pub place: Place,
    pub rows: u16,
    pub cols: u16,
    pub program: Option<String>,
    pub args: Option<Vec<String>>,
    pub env: Option<HashMap<String, String>>,
    /// The task run this session belongs to.
    pub run_id: Option<String>,
}

/// One session's lanes: its keystrokes and its resizes, each one thread in
/// order. The engine's, and the app's in front of a terminal host, so a full
/// queue and a dropped call behave the same on both backends.
pub(crate) fn session_lanes() -> (Executor, Executor) {
    (
        Executor::new("input", 1, INPUT_QUEUE_CAPACITY, LANE_IDLE),
        Executor::new("geometry", 1, GEOMETRY_QUEUE_CAPACITY, LANE_IDLE),
    )
}

/// A new session id: `term-<this process's pid>-<counter>`. Unique for the
/// life of this process, and across its restarts because the pid changes.
pub(crate) fn next_session_id() -> String {
    let count = SESSION_COUNTER.fetch_add(1, Ordering::Relaxed);
    format!("term-{}-{:x}", std::process::id(), count)
}

/// Whether `id` is one [`next_session_id`] could have minted. The host takes
/// ids from the app; one it could not have made is refused, not stored.
fn is_session_id(id: &str) -> bool {
    let Some(rest) = id.strip_prefix("term-") else {
        return false;
    };
    let Some((pid, count)) = rest.split_once('-') else {
        return false;
    };
    !pid.is_empty()
        && pid.len() <= 10
        && pid.bytes().all(|b| b.is_ascii_digit())
        && !count.is_empty()
        && count.len() <= 16
        && count
            .bytes()
            .all(|b| b.is_ascii_hexdigit() && !b.is_ascii_uppercase())
}

#[allow(clippy::too_many_arguments)]
fn spawn_session_inner<R: tauri::Runtime>(
    app: &AppHandle<R>,
    state: &TerminalSessions,
    repo_path: &str,
    start_dir: Option<&str>,
    rows: u16,
    cols: u16,
    program: Option<String>,
    args: Option<Vec<String>>,
    env: Option<HashMap<String, String>>,
    output: OutputSink,
    observer: Option<Arc<dyn SessionObserver>>,
) -> Result<TerminalSpawned, String> {
    spawn_at(
        app,
        state,
        Place::Repo {
            repo_path: repo_path.to_owned(),
            start_dir: start_dir.map(str::to_owned),
        },
        rows,
        cols,
        program,
        args,
        env,
        output,
        observer,
    )
}

/// The app's side of every spawn, whichever backend holds the PTY: the checks
/// that need this process (the load governor's override is held here), the
/// session id, and the ledger record once the session exists.
#[allow(clippy::too_many_arguments)]
fn spawn_at<R: tauri::Runtime>(
    app: &AppHandle<R>,
    state: &TerminalSessions,
    place: Place,
    rows: u16,
    cols: u16,
    program: Option<String>,
    args: Option<Vec<String>>,
    env: Option<HashMap<String, String>>,
    output: OutputSink,
    observer: Option<Arc<dyn SessionObserver>>,
) -> Result<TerminalSpawned, String> {
    validate_pty_options(program.as_deref(), args.as_deref(), env.as_ref())?;
    let (repo, _) = place.resolve()?;
    // An agent CLI is a launch the load governor may refuse under severe
    // memory pressure; the user's own shell (no program) never is.
    if program.as_deref().is_some_and(|p| !p.trim().is_empty()) {
        crate::sysload::admit(crate::sysload::Site::PtySpawn, &repo.to_string_lossy())
            .map_err(|refused| refused.message())?;
    }
    let request = SpawnRequest {
        session_id: next_session_id(),
        place,
        rows,
        cols,
        program,
        args,
        env,
        run_id: observer.as_ref().map(|o| o.run_id().to_owned()),
    };
    let ledger = (
        request
            .program
            .clone()
            .filter(|p| !p.trim().is_empty())
            .unwrap_or_else(default_shell),
        request.args.clone(),
    );
    let events: Arc<dyn SessionEvents> = Arc::new(AppEvents::new(app, state.incidents.clone()));
    #[cfg(unix)]
    let started = match &state.host {
        Some(host) => host.spawn(events, request, output, observer)?,
        None => start_session(state, request, output, events, observer)?,
    };
    #[cfg(not(unix))]
    let started = start_session(state, request, output, events, observer)?;
    let spawned = started.spawned;
    // The ledger entry resolves the repository's address and task binding
    // (git) and appends (a lock another writer may hold), and none of it can
    // change what the caller is told: spawn used to wait on all of it, and a
    // renderer watchdog then killed the new session for being slow to start.
    // Done beside the session, never in front of it.
    record_session_spawn(
        repo.to_string_lossy().into_owned(),
        ledger.0,
        spawned.id.clone(),
        ledger.1,
        agent_launcher(&spawned.shell).is_some(),
    );
    Ok(spawned)
}

/// Starts one session in this process: the engine. Everything here runs where
/// the PTY will live — the repository and trust checks, the fork, the reader —
/// so the checks hold for the process that forks.
pub(crate) fn start_session(
    state: &TerminalSessions,
    request: SpawnRequest,
    output: OutputSink,
    events: Arc<dyn SessionEvents>,
    observer: Option<Arc<dyn SessionObserver>>,
) -> Result<Started, String> {
    let SpawnRequest {
        session_id,
        place,
        rows,
        cols,
        program,
        args,
        env,
        run_id,
    } = request;
    if !is_session_id(&session_id) {
        return Err("Invalid terminal session id".into());
    }
    if run_id.as_deref().is_some_and(|id| {
        id.is_empty() || id.len() > 128 || !id.chars().all(|c| c.is_ascii_graphic())
    }) {
        return Err("Invalid task run id".into());
    }
    validate_pty_options(program.as_deref(), args.as_deref(), env.as_ref())?;
    let (repo, start) = place.resolve()?;
    if state.lock_sessions().contains_key(&session_id) {
        return Err(format!("Terminal session '{session_id}' already exists"));
    }
    let reservation = reserve_session(state)?;
    let pty_system = native_pty_system();
    let size = bounded_pty_size(rows, cols);
    // `openpty` sets `FD_CLOEXEC` on the master and slave in two `fcntl` calls
    // after the descriptors exist, and `spawn_command` below forks. Both halves
    // of `procguard`'s inheritance race live in this function, and the shell
    // this starts is the longest-lived thief the app has: a stolen `git` pipe
    // end is held for the whole session rather than the next millisecond.
    let pair = crate::procguard::with_inheritance_lock(|| pty_system.openpty(size))
        .map_err(|e| format!("Failed to open PTY: {e}"))?;

    let requested = program.filter(|p| !p.trim().is_empty());
    // A caller-supplied program is an agent CLI (`claude`, `codex`, …); no
    // program means the user's own shell, and only that starts as a login
    // shell.
    let is_default_shell = requested.is_none();
    let shell = requested.unwrap_or_else(default_shell);
    let PtyCommand { mut cmd, resolved } = build_pty_command(
        &shell,
        is_default_shell,
        args.as_deref(),
        env.as_ref(),
        &start,
        &PtyEnv::from_process(),
    );
    if cfg!(windows) {
        if let Some(refusal) = batch_reparse_refusal(&resolved, args.as_deref().unwrap_or_default())
        {
            return Err(refusal);
        }
    }
    if run_id.is_some() {
        normalize_task_environment(&mut cmd, &repo);
    }

    // The shared child handle serializes termination with reaping. The reader
    // owns the capacity reservation until the child has been reaped.
    // Obtain handles before spawning: allocation failures cannot orphan a child.
    let mut reader = pair
        .master
        .try_clone_reader()
        .map_err(|e| format!("Failed to clone PTY reader: {e}"))?;
    #[cfg(unix)]
    let writer = input::open(pair.master.as_ref())?;
    #[cfg(unix)]
    let reader_fd = pair
        .master
        .as_raw_fd()
        .ok_or("PTY has no Unix master descriptor")?;
    #[cfg(not(unix))]
    let writer = pair
        .master
        .take_writer()
        .map_err(|e| format!("Failed to take PTY writer: {e}"))?;
    // What an agent's own hook needs in order to report back precisely: which
    // GitPulse session it is running under, and where to say so. Both are set
    // after the caller's `env` map has been applied, and deliberately so —
    // these two name *this* process's socket and *this* PTY, and an IPC caller
    // supplying its own values would be pointing hooks at neither.
    cmd.env(crate::alerts::bridge::SESSION_ENV, &session_id);
    // Which agent this is, decided here where the launcher is known rather
    // than guessed from a hook payload that names only the host.
    match launcher_kind(&resolved) {
        Some(kind) => cmd.env(crate::hooks::AGENT_KIND_ENV, kind),
        None => cmd.env_remove(crate::hooks::AGENT_KIND_ENV),
    }
    match crate::alerts::bridge::socket_path() {
        Some(path) => cmd.env(crate::alerts::bridge::SOCKET_ENV, path),
        // Nothing to point at. Removing it matters: a stale value inherited
        // from a parent GitPulse would send this session's hooks to a socket
        // that is not ours.
        None => cmd.env_remove(crate::alerts::bridge::SOCKET_ENV),
    }
    // The launch table's environment (the task run id), after the caller's
    // map for the same reason as the two above: it names this launch, and an
    // IPC caller must not be able to file a session under another run.
    for (name, value) in crate::workbench::terminal_command::launch_env(run_id.as_deref()) {
        match value {
            Some(value) => cmd.env(name, value),
            None => cmd.env_remove(name),
        }
    }
    if let Some(observer) = &observer {
        observer.before_spawn(&session_id)?;
    }
    // Not the plain `require`: a PTY child cannot be re-anchored the way
    // `procguard::spawn` re-anchors an ordinary one, because `portable_pty`
    // builds the command, owns its only `pre_exec` hook, and hands back no
    // slave descriptor. So the directory has to be one that cannot be
    // substituted between this check and the launch, and the guard is held
    // across `spawn_command` for the platforms where holding it is what does
    // the work.
    //
    // Trust resolution touches the filesystem and stays outside the inheritance
    // lock; only the fork itself is serialized. The two guards nest rather than
    // compete: `_anchored` is what keeps the approved directory from being
    // substituted, and the inheritance lock is what keeps a concurrent spawn
    // from stealing this child's pipe ends while `FD_CLOEXEC` is still being
    // set in two non-atomic `fcntl` calls.
    let spawn_result =
        crate::repository_trust::require_unsubstitutable_dir(&repo).and_then(|_anchored| {
            crate::procguard::with_inheritance_lock(|| pair.slave.spawn_command(cmd))
                .map_err(|error| format!("Failed to spawn process '{shell}': {error}"))
        });
    let child = match spawn_result {
        Ok(child) => child,
        Err(reason) => {
            if let Some(observer) = &observer {
                observer.spawn_failed(&session_id, &reason);
            }
            return Err(reason);
        }
    };
    // Read now, while the child cannot have been reaped: what a task run
    // records its start with, here or, through the host's reply, in the app.
    let identity = process_identity(child.process_id());
    let start_error = observer
        .as_ref()
        .and_then(|observer| observer.started(&session_id, identity.clone()).err());
    // Registered before the reader exists and before anything can have asked
    // the child whether it ran: a child that exits from here on is reported to
    // the watch, and one that already has is found by the reader's first ask.
    #[cfg(unix)]
    let readiness = readiness::Readiness::new(reader_fd, child.process_id());
    let child = Arc::new(Mutex::new(child));

    // Drop slave explicitly; master remains open.
    drop(pair.slave);

    let dead = Arc::new(AtomicBool::new(false));
    let output_flow = Arc::new(OutputFlow::default());
    let wakeups = Arc::new(AtomicU64::new(0));
    let output_slot = Arc::new(OutputSlot::new(output));
    let hidden = Arc::new(AtomicBool::new(false));
    let master = Arc::new(Mutex::new(pair.master));
    let closing = Arc::new(AtomicUsize::new(0));
    let ring = Arc::new(Mutex::new(ring::Ring::default()));
    let (input, geometry) = session_lanes();
    let entry = SessionEntry {
        master: master.clone(),
        writer: Arc::new(Mutex::new(writer)),
        child: child.clone(),
        dead: dead.clone(),
        flow: output_flow.clone(),
        wakeups: wakeups.clone(),
        output: output_slot.clone(),
        hidden: hidden.clone(),
        closing: closing.clone(),
        tracked_run: run_id.clone(),
        spawned: TerminalSpawned {
            id: session_id.clone(),
            shell: resolved.clone(),
            cwd: start.to_string_lossy().into_owned(),
        },
        repo: repo.clone(),
        input,
        geometry,
        events: events.clone(),
        ring: ring.clone(),
    };

    {
        let mut guard = state.lock_sessions();
        // Checked again where it counts: two requests naming one id raced past
        // the check above, and the second child is stopped, not filed.
        if guard.contains_key(&session_id) {
            drop(guard);
            dead.store(true, Ordering::SeqCst);
            if let Err(error) = terminate_pty_child(&child, &master, &closing) {
                log::error!(target: "terminal", "could not stop a duplicate session: {error}");
            }
            return Err(format!("Terminal session '{session_id}' already exists"));
        }
        guard.insert(session_id.clone(), entry);
    }

    // Reader thread: streams output to the session's view through its output sink.
    let thread_events = events.clone();
    let thread_ring = ring.clone();
    let sid_for_thread = session_id.clone();
    let sid_for_exit = session_id.clone();
    let dead_flag = dead.clone();
    let sessions_map = state.sessions.clone();
    let output_stall_ms = state.output_stall_ms.clone();
    // The program's name, not its path or the session's id (see
    // `log_sites_do_not_interpolate_session_identifiers`).
    let program_for_log = std::path::Path::new(&resolved).file_name().map_or_else(
        || "shell".to_string(),
        |name| name.to_string_lossy().into_owned(),
    );
    let thread_child = child.clone();
    let thread_master = master.clone();
    let thread_closing = closing.clone();
    let thread_observer = observer.clone();
    let exit_program = shell.clone();
    let exit_cwd = start.to_string_lossy().into_owned();
    if start_error.is_some() {
        dead.store(true, Ordering::SeqCst);
        output_flow.stop();
        if let Err(error) = terminate_pty_child(&child, &master, &closing) {
            log::error!(target: "terminal", "could not stop a task session after bookkeeping failure: {error}");
        }
    }

    // What a banner from this session will say. Resolved once, here, because
    // the reader thread outlives every borrow it could otherwise take.
    let watcher = SessionWatcher::new(&session_id, &resolved, &repo, events);

    let slot = SessionSlot {
        sessions: state.sessions.clone(),
        id: session_id.clone(),
    };
    let reader_thread = crate::supervisor::spawn_once(
        format!("pty-read-{session_id}"),
        "terminal reader",
        move || {
            // Dropped when this thread ends by any road, a panic included.
            let _slot = slot;
            let mut reservation = Some(reservation);
            let mut failure = None;
            let mut watcher = watcher;
            #[cfg(unix)]
            let mut readiness = readiness;
            // One message's worth: output is gathered here until the tty has
            // nothing more queued, or the message is full.
            let mut buf = vec![0u8; OUTPUT_CHUNK_BYTES];
            // How much of `buf` is read but not yet sent. Invariant at the top
            // of the loop: `filled < buf.len()`, so a read never has an empty
            // slice to fill (which would answer `Ok(0)`, the end of the tty).
            let mut filled = 0usize;
            // When `buf` last went from empty to holding output.
            #[cfg_attr(not(unix), allow(unused_assignments, unused_variables))]
            let mut gathering_since = Instant::now();
            // Logged once per run of failures, not once per chunk.
            let mut emit_failing = false;
            // Sends `data` to the view as one message. A reservation that fails
            // (the flow was stopped) ends the process, as before; everything
            // else (a view that is detached, behind, or whose emit failed) is
            // handled here and never ends it.
            let mut deliver = |data: &[u8], failure: &mut Option<String>| {
                let n = data.len();
                let stall = Duration::from_millis(output_stall_ms.load(Ordering::Acquire));
                // Kept before it is routed, whatever the route: a view that
                // takes this session over later is shown it. Held until the
                // chunk is sent, so a takeover's replay cannot land between
                // keeping it and sending it.
                let mut kept = thread_ring
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner);
                kept.push(data);
                let delivery = output_flow.reserve(n, stall).map(|route| {
                    let resumed = match route {
                        // Detached: the page that would paint this is gone.
                        // The process keeps running; the bytes are kept in
                        // the ring and not sent.
                        Route::Hold => return,
                        // The view is alive but behind. Never a reason to
                        // end the process: drop, keep draining, say so once.
                        Route::Shed { began } => {
                            if began {
                                log::warn!(
                                    target: "terminal",
                                    "{program_for_log} terminal: view did not acknowledge output for {}ms; skipping output until it catches up (process left running)",
                                    stall.as_millis()
                                );
                                thread_events.loss(TerminalLossPayload {
                                    id: sid_for_thread.clone(),
                                    message: SHED_NOTICE.into(),
                                });
                            }
                            return;
                        }
                        Route::Resume => true,
                        Route::Deliver => false,
                    };
                    // To the view that shows this session now: a channel that
                    // belongs to it alone, not an event every webview hears.
                    let sent = output_slot.current().bytes(&sid_for_thread, data);
                    match sent {
                        Ok(()) => {
                            emit_failing = false;
                            if resumed {
                                log::info!(target: "terminal", "{program_for_log} terminal: view caught up; output resumed");
                                repaint_current(&thread_master);
                            }
                        }
                        // A failed send is the view's trouble (its page is
                        // gone, or its event loop refused), not the
                        // process's; it keeps running. Shed as after a stall
                        // (see `OutputFlow::shed`).
                        Err(error) => {
                            if !std::mem::replace(&mut emit_failing, true) {
                                log::warn!(target: "terminal", "{program_for_log} terminal: failed to send output to its view, skipping output: {error}");
                            }
                            if output_flow.shed(n) {
                                thread_events.loss(TerminalLossPayload {
                                    id: sid_for_thread.clone(),
                                    message: SHED_NOTICE.into(),
                                });
                            }
                        }
                    }
                });
                drop(kept);
                if let Err(error) = delivery {
                    if !dead_flag.swap(true, Ordering::SeqCst) {
                        *failure = Some(error);
                        output_flow.stop();
                        if let Err(error) =
                            terminate_pty_child(&thread_child, &thread_master, &thread_closing)
                        {
                            log::warn!(target: "terminal", "could not signal failed terminal: {error}");
                        }
                    }
                }
            };
            // Sends what has been gathered, unless the session is being torn
            // down: then the bytes are only drained, as `dead_flag` always did.
            macro_rules! flush {
                () => {
                    if filled > 0 && !dead_flag.load(Ordering::Relaxed) {
                        deliver(&buf[..filled], &mut failure);
                    }
                    filled = 0;
                };
            }
            // A panic anywhere in the loop (the scanner, an event listener) is
            // contained here: the thread must not end with the child alive and
            // nothing reading its tty, which is a session "running" forever.
            let pumped = catch_unwind(AssertUnwindSafe(|| loop {
                match reader.read(&mut buf[filled..]) {
                    Ok(0) => {
                        flush!();
                        break;
                    }
                    Ok(n) => {
                        // Read before the liveness check below: a session being
                        // torn down can still be asking for attention, and this
                        // costs one pass over bytes already in cache.
                        watcher.observe(&buf[filled..filled + n]);
                        if filled == 0 {
                            gathering_since = Instant::now();
                        }
                        filled += n;
                        // On close, drain kernel output until EOF. Leaving a
                        // full tty queue undrained can prevent a killed writer
                        // from completing on macOS, stranding its process slot.
                        if dead_flag.load(Ordering::Relaxed) {
                            filled = 0;
                            continue;
                        }
                        // Keep reading while the tty has more: the kernel hands
                        // out a KiB or so per read, and a message per read is
                        // a webview round trip per KiB. A full message goes
                        // now; otherwise the WouldBlock arm below sends what
                        // has been gathered once the tty is dry. Off Unix the
                        // read blocks and never says WouldBlock, so each read
                        // is its own message there.
                        if filled == buf.len() || cfg!(not(unix)) {
                            flush!();
                        } else {
                            // A stream that never goes dry still reaches the
                            // view this often: a visible one at frame rate, a
                            // hidden one a few times a second.
                            #[cfg(unix)]
                            {
                                let held = gathering_since.elapsed();
                                if held >= GATHER_MAX_HOLD
                                    && !(held < HIDDEN_MAX_HOLD && hidden.load(Ordering::Relaxed))
                                {
                                    flush!();
                                }
                            }
                        }
                    }
                    Err(e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
                    #[cfg(unix)]
                    Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                        // Nothing is queued. Output that looks like a stream may
                        // be only a gap in one: give it a moment, never past
                        // `GATHER_MAX_HOLD` from the first byte. A tab nobody
                        // is looking at gets `HIDDEN_MAX_HOLD`, whatever it
                        // printed, and the wait is for the rest of that hold:
                        // any output ends it, and the hold is re-entered.
                        if filled > 0 {
                            let held = gathering_since.elapsed();
                            let hidden = hidden.load(Ordering::Relaxed);
                            let (limit, quiet) = if hidden {
                                (HIDDEN_MAX_HOLD, HIDDEN_MAX_HOLD)
                            } else if filled >= GATHER_MIN_BYTES {
                                (GATHER_MAX_HOLD, GATHER_QUIET)
                            } else {
                                (Duration::ZERO, Duration::ZERO)
                            };
                            if held < limit {
                                let wait = quiet.min(limit - held);
                                if matches!(
                                    input::wait_for_output_within(reader_fd, wait),
                                    Ok(true)
                                ) {
                                    continue;
                                }
                            }
                        }
                        // Everything the session printed has been read: send
                        // it before waiting for more.
                        flush!();
                        // A job left running in the background
                        // (`sleep &`, a dev server) still holds the terminal
                        // open, and EOF will not come while it lives: the
                        // session is over when its own process is, the way
                        // every terminal ends a tab whose shell has exited.
                        //
                        // Asked when the readiness wait says the answer may have
                        // changed (an exit, a timer, the first lap), not on every
                        // pass: output alone says nothing about the child.
                        //
                        // Not while a close is under way: only the closer reaps
                        // the shell until it is done (see `ClosingGuard`), and
                        // the hint is kept for the pass after.
                        let closing = thread_closing.load(Ordering::SeqCst) > 0;
                        if !closing && readiness.take_exit_hint() && child_has_exited(&thread_child)
                        {
                            break;
                        }
                        // thread_master owns reader_fd until this thread exits.
                        // The kernel wakes this thread for output or for the
                        // child's exit; a quiet session sleeps, bounded only by
                        // the safety net. During a close it laps instead, so
                        // an exit heard while the question was held back is
                        // asked about as soon as the close is over.
                        let waited = if closing {
                            readiness.wait_within(CLOSING_LAP)
                        } else {
                            readiness.wait()
                        };
                        wakeups.fetch_add(1, Ordering::Relaxed);
                        if let Err(error) = waited {
                            if !dead_flag.swap(true, Ordering::SeqCst) {
                                failure =
                                    Some(format!("Terminal output readiness failed: {error}"));
                                output_flow.stop();
                                if let Err(error) = terminate_pty_child(
                                    &thread_child,
                                    &thread_master,
                                    &thread_closing,
                                ) {
                                    log::warn!(target: "terminal", "could not signal failed terminal: {error}");
                                }
                            }
                            break;
                        }
                    }
                    Err(e) if is_end_of_terminal(&e) => {
                        flush!();
                        break;
                    }
                    Err(e) => {
                        // What was read before the failure is still output.
                        flush!();
                        // The shell may still be running, with nothing left
                        // reading what it prints or delivering what is typed:
                        // stop it and say why, rather than leave a frozen tab.
                        if !dead_flag.swap(true, Ordering::SeqCst) {
                            failure = Some(format!("Terminal output could not be read: {e}"));
                            output_flow.stop();
                            if let Err(error) =
                                terminate_pty_child(&thread_child, &thread_master, &thread_closing)
                            {
                                log::warn!(target: "terminal", "could not signal failed terminal: {error}");
                            }
                        }
                        break;
                    }
                }
            }));
            if let Err(payload) = pumped {
                // Not "terminal reader": every reader that ends normally
                // clears that record, which would erase this one at once.
                crate::supervisor::note_stopped("terminal reader panic", payload.as_ref());
                failure.get_or_insert_with(|| {
                    format!(
                        "Terminal output reader panicked: {}",
                        crate::supervisor::payload_text(payload.as_ref())
                    )
                });
                dead_flag.store(true, Ordering::SeqCst);
                output_flow.stop();
                if let Err(error) =
                    terminate_pty_child(&thread_child, &thread_master, &thread_closing)
                {
                    log::warn!(target: "terminal", "could not signal a terminal whose reader panicked: {error}");
                }
            }

            // Read before the store below: set means GitPulse ended it.
            let ended_by_gitpulse = dead_flag.load(Ordering::SeqCst);
            dead_flag.store(true, Ordering::SeqCst);
            output_flow.stop();
            watcher.ended();

            // Reap the shell and report its real exit status.
            finalize_pty_session(
                &sid_for_exit,
                move || loop {
                    // Do not hold the child mutex across a blocking wait: EOF
                    // can precede process exit, and Close must still work.
                    // Nor reap under a close in progress: its closer is relying on
                    // the shell staying unreaped (see `ClosingGuard`).
                    if thread_closing.load(Ordering::SeqCst) > 0 {
                        thread::sleep(Duration::from_millis(10));
                        continue;
                    }
                    let result = thread_child
                        .lock()
                        .unwrap_or_else(std::sync::PoisonError::into_inner)
                        .try_wait();
                    match result {
                        Ok(Some(status)) => break Ok(ExitStatusLike::from(&status)),
                        Ok(None) => thread::sleep(Duration::from_millis(10)),
                        Err(error) => break Err(error),
                    }
                },
                |mut payload| {
                    if payload.error.is_none() {
                        payload.error = failure.take();
                    }
                    // Every end is on record with its cause. A session that
                    // vanished with nothing in the log was undiagnosable.
                    log::info!(
                        target: "terminal",
                        "{program_for_log} terminal session ended: exit_code={:?} signal={:?} reaped={} error={:?}",
                        payload.exit_code, payload.signal, payload.reaped, payload.error
                    );
                    // Shutdown waits for session removal. Persist the owned
                    // process's outcome before telling shutdown it is cleaned up.
                    // A panic in the bookkeeping must not keep the exit from
                    // being announced or the entry from leaving the map.
                    if let Some(observer) = &thread_observer {
                        if let Err(panic) =
                            catch_unwind(AssertUnwindSafe(|| observer.finished(&payload)))
                        {
                            crate::supervisor::note_stopped(
                                "terminal observer panic",
                                panic.as_ref(),
                            );
                        }
                    }
                    drop(reservation.take());
                    sessions_map
                        .lock()
                        .unwrap_or_else(std::sync::PoisonError::into_inner)
                        .remove(&sid_for_exit);
                    // After the last byte, before the exit: the page delivers
                    // channel messages in order, but an exit is an event and
                    // would overtake bytes still being fetched. The marker is
                    // how the page knows every byte has arrived.
                    if let Err(error) = output_slot.current().end(&sid_for_exit) {
                        log::warn!(target: "terminal", "{program_for_log} terminal: could not mark the end of its output: {error}");
                    }
                    thread_events.exit(SessionEnded {
                        payload,
                        ended_by_gitpulse,
                        program: exit_program.clone(),
                        cwd: exit_cwd.clone(),
                    });
                },
            );
        },
    );
    if let Err(e) = reader_thread {
        let mut guard = state.lock_sessions();
        guard.remove(&session_id);
        drop(guard);
        dead.store(true, Ordering::SeqCst);
        terminate_pty_child(&child, &master, &closing)?;
        let exit = child
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .wait()
            .map(|status| ExitStatusLike::from(&status));
        if let Some(observer) = &observer {
            observer.finished(&reader_failure_payload(&session_id, exit, &e));
        }
        return Err(format!("Failed to spawn PTY reader thread: {e}"));
    }
    if let Some(error) = start_error {
        return Err(format!("Task process bookkeeping failed; the child was asked to stop. Inspect the run before another attempt: {error}"));
    }

    Ok(Started {
        spawned: TerminalSpawned {
            // The resolved path, not the requested name: on a GUI launch these
            // differ, and "which binary is this session running" is the
            // question the status row and the harness journal are actually
            // asking.
            shell: resolved,
            id: session_id,
            // Where the process started, the same value the session entry
            // holds.
            cwd: start.to_string_lossy().into_owned(),
        },
        process: identity,
    })
}

/// A session the engine has started.
pub(crate) struct Started {
    pub spawned: TerminalSpawned,
    /// The process's id and creation identity (`process_birth`), or why they
    /// could not be read.
    pub process: Result<(u32, String), String>,
}

/// The id and creation identity of a process just forked.
fn process_identity(pid: Option<u32>) -> Result<(u32, String), String> {
    let pid = pid
        .filter(|pid| *pid > 0)
        .ok_or("PTY did not report a process ID")?;
    Ok((pid, crate::workbench::process_birth::read(pid)?))
}

/// Records `session.spawn` on a thread of its own. `record` rather than
/// `append`: a PTY that could not be logged is still a PTY the user asked for.
/// The failure is counted and surfaced through `LedgerStatus` rather than
/// refusing the session.
fn record_session_spawn(
    repository: String,
    shell: String,
    session_id: String,
    args: Option<Vec<String>>,
    is_agent: bool,
) {
    let work = move || {
        let actor_kind = if is_agent {
            crate::ledger::ActorKind::Agent
        } else {
            crate::ledger::ActorKind::Human
        };
        let address = crate::ledger::bindings::repository_address(&repository).ok();
        let ledger_repo = address
            .as_ref()
            .map(|address| address.anchor.clone())
            .unwrap_or_else(|| repository.clone());
        let worktree_path = address.as_ref().and_then(|address| {
            (address.worktree != address.anchor).then(|| address.worktree.clone())
        });
        let task_id = address.and_then(|address| {
            crate::ledger::bindings::resolve(&address.anchor, &address.worktree)
                .ok()
                .flatten()
        });
        let _ = crate::ledger::record(crate::ledger::Draft {
            repo_path: ledger_repo,
            worktree_path,
            actor_kind: Some(actor_kind),
            actor_id: Some(shell.clone()),
            session_id: Some(session_id),
            task_id,
            action: "session.spawn".to_string(),
            object: Some(shell),
            argv_json: args.as_ref().and_then(|a| serde_json::to_string(a).ok()),
            outcome: Some(crate::ledger::Outcome::Ok),
            verdict_json: None,
            before_ref: None,
            after_ref: None,
            duration_ms: None,
            detail_json: None,
        });
    };
    if let Err(error) =
        crate::supervisor::spawn_once("session-spawn-ledger", "terminal ledger record", work)
    {
        log::warn!(target: "terminal", "session.spawn was not recorded: {error}");
    }
}

/// Writes user input bytes into a PTY session.
pub fn write_to_session(
    state: &TerminalSessions,
    session_id: &str,
    data: &str,
) -> Result<(), String> {
    write_input(state, session_id, data.as_bytes())
}

/// xterm's onBinary stream encodes one byte per Latin-1 character.
pub fn write_binary_to_session(
    state: &TerminalSessions,
    session_id: &str,
    data: &str,
) -> Result<(), String> {
    write_input(state, session_id, &binary_input(data)?)
}

/// xterm's onBinary characters as the bytes they stand for, one per
/// Latin-1 character.
fn binary_input(data: &str) -> Result<Vec<u8>, String> {
    if data.len() > 2 * MAX_PTY_INPUT_BYTES {
        return Err("Binary terminal input exceeds its limit".into());
    }
    data.chars()
        .map(|c| u8::try_from(u32::from(c)))
        .collect::<Result<_, _>>()
        .map_err(|_| "Invalid binary terminal input".into())
}

/// Input for a session, wherever it lives.
fn write_input(state: &TerminalSessions, session_id: &str, data: &[u8]) -> Result<(), String> {
    #[cfg(unix)]
    if let Some(host) = &state.host {
        if data.len() > MAX_PTY_INPUT_BYTES {
            return Err(format!(
                "Terminal input is {} bytes; the per-write limit is {MAX_PTY_INPUT_BYTES}",
                data.len()
            ));
        }
        return host.write_blocking(session_id, data);
    }
    write_pty_bytes(state, session_id, data)
}

struct EraseInputCounts {
    erase: usize,
    printable: usize,
    other: usize,
}

/// Counts BS/DEL against the rest of one PTY write. `None` when the write
/// contains no erase, so ordinary typing is not logged. The bytes themselves
/// are never formatted into a log line.
fn describe_erase_input(data: &[u8]) -> Option<EraseInputCounts> {
    if data.is_empty() {
        return None;
    }
    let mut erase = 0usize;
    let mut printable = 0usize;
    let mut other = 0usize;
    for &byte in data {
        if byte == 0x08 || byte == 0x7f {
            erase += 1;
        } else if (0x20..0x7f).contains(&byte) || byte >= 0x80 {
            printable += 1;
        } else {
            other += 1;
        }
    }
    if erase == 0 {
        None
    } else {
        Some(EraseInputCounts {
            erase,
            printable,
            other,
        })
    }
}

fn note_erase_input(data: &[u8]) {
    let Some(counts) = describe_erase_input(data) else {
        return;
    };
    if counts.printable > 0 {
        log::info!(
            target: "terminal",
            "pty write mixed erase with printable bytes: erase={} printable={} other={} bytes={}",
            counts.erase,
            counts.printable,
            counts.other,
            data.len()
        );
    } else {
        log::debug!(
            target: "terminal",
            "pty erase write: erase={} other={} bytes={}",
            counts.erase,
            counts.other,
            data.len()
        );
    }
}

fn write_pty_bytes(state: &TerminalSessions, session_id: &str, data: &[u8]) -> Result<(), String> {
    if data.len() > MAX_PTY_INPUT_BYTES {
        return Err(format!(
            "Terminal input is {} bytes; the per-write limit is {MAX_PTY_INPUT_BYTES}",
            data.len()
        ));
    }
    let (writer, dead) = {
        let guard = state.lock_sessions();
        guard
            .get(session_id)
            .map(|session| (session.writer.clone(), session.dead.clone()))
            .ok_or_else(|| format!("Terminal session '{session_id}' not found"))?
    };
    note_erase_input(data);
    #[cfg(unix)]
    {
        input::write(&writer, &dead, data)
    }
    #[cfg(not(unix))]
    {
        if dead.load(Ordering::Acquire) {
            return Err("Terminal session is closed".into());
        }
        let mut writer = writer
            .lock()
            .map_err(|e| format!("Terminal writer lock error: {e}"))?;
        writer
            .write_all(data)
            .map_err(|e| format!("Failed to write to terminal: {e}"))?;
        writer
            .flush()
            .map_err(|e| format!("Failed to flush terminal: {e}"))?;
        Ok(())
    }
}

/// Resizes a PTY session.
pub fn resize_session(
    state: &TerminalSessions,
    session_id: &str,
    rows: u16,
    cols: u16,
) -> Result<(), String> {
    #[cfg(unix)]
    if let Some(host) = &state.host {
        return host.resize_blocking(session_id, rows, cols);
    }
    // The map is let go before the master is waited for: one session's busy
    // master must not stall list, write, kill and attach for all the others.
    let master = state
        .lock_sessions()
        .get(session_id)
        .map(|session| session.master.clone())
        .ok_or_else(|| format!("Terminal session '{session_id}' not found"))?;
    // Bounded, like a write's wait for its lock: the lane that calls this is
    // one thread, and a master lock that never came free must make this resize
    // fail, not take every later resize of the session down with it.
    let resized = lock_within(&master, MASTER_LOCK_BUDGET)?.resize(bounded_pty_size(rows, cols));
    resized.map_err(|e| format!("Failed to resize terminal: {e}"))
}

/// Takes `lock` within `budget`, recovering a poisoned one (a PTY master holds
/// no state a panic could leave half-written).
fn lock_within<T: ?Sized>(
    lock: &Mutex<Box<T>>,
    budget: Duration,
) -> Result<std::sync::MutexGuard<'_, Box<T>>, String> {
    let deadline = Instant::now() + budget;
    loop {
        match lock.try_lock() {
            Ok(guard) => return Ok(guard),
            Err(std::sync::TryLockError::Poisoned(poisoned)) => return Ok(poisoned.into_inner()),
            Err(std::sync::TryLockError::WouldBlock) => {
                if Instant::now() >= deadline {
                    return Err(format!(
                        "Terminal is busy; the resize gave up after {}ms",
                        budget.as_millis()
                    ));
                }
                thread::sleep(Duration::from_millis(2));
            }
        }
    }
}

/// A session's master lock, held by a thread of its own until released or
/// dropped. For tests that need the one lock a resize waits on to be busy
/// without reaching into the session map.
#[doc(hidden)]
pub struct MasterHold {
    release: Option<std::sync::mpsc::Sender<()>>,
    holder: Option<thread::JoinHandle<()>>,
    /// Held in the terminal host instead: released by asking it.
    #[cfg(unix)]
    remote: Option<(host::client::HostClient, String)>,
}

impl MasterHold {
    /// Lets go of the lock and waits for the holding thread to end.
    pub fn release(mut self) {
        self.finish();
    }

    #[cfg(unix)]
    pub(crate) fn remote(host: host::client::HostClient, session_id: String) -> Self {
        Self {
            release: None,
            holder: None,
            remote: Some((host, session_id)),
        }
    }

    fn finish(&mut self) {
        #[cfg(unix)]
        if let Some((host, session_id)) = self.remote.take() {
            host.release(&session_id);
        }
        drop(self.release.take());
        if let Some(holder) = self.holder.take() {
            let _ = holder.join();
        }
    }
}

impl Drop for MasterHold {
    fn drop(&mut self) {
        self.finish();
    }
}

/// The size the kernel holds for `session_id`'s PTY, as `(rows, cols)`. For
/// tests that need to know which of many resizes was applied last.
#[doc(hidden)]
pub fn pty_size(state: &TerminalSessions, session_id: &str) -> Result<(u16, u16), String> {
    #[cfg(unix)]
    if let Some(host) = &state.host {
        return host.size(session_id);
    }
    let master = state
        .lock_sessions()
        .get(session_id)
        .map(|session| session.master.clone())
        .ok_or_else(|| format!("Terminal session '{session_id}' not found"))?;
    let size = lock_within(&master, MASTER_LOCK_BUDGET)?
        .get_size()
        .map_err(|e| format!("Failed to read the terminal size: {e}"))?;
    Ok((size.rows, size.cols))
}

/// Takes `session_id`'s master lock on a thread of its own and returns once it
/// is held.
#[doc(hidden)]
pub fn hold_master(state: &TerminalSessions, session_id: &str) -> Result<MasterHold, String> {
    #[cfg(unix)]
    if let Some(host) = &state.host {
        return host.hold(session_id);
    }
    let master = state
        .lock_sessions()
        .get(session_id)
        .map(|session| session.master.clone())
        .ok_or_else(|| format!("Terminal session '{session_id}' not found"))?;
    let (release, released) = std::sync::mpsc::channel::<()>();
    let (held_tx, held_rx) = std::sync::mpsc::channel::<()>();
    let holder = thread::Builder::new()
        .name("master-hold".into())
        .spawn(move || {
            let _guard = master
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            let _ = held_tx.send(());
            // Ends when the sender is dropped or sends.
            let _ = released.recv();
        })
        .map_err(|e| e.to_string())?;
    held_rx
        .recv()
        .map_err(|_| "the master hold thread ended before locking".to_string())?;
    Ok(MasterHold {
        release: Some(release),
        holder: Some(holder),
        #[cfg(unix)]
        remote: None,
    })
}

/// Kills a PTY session.
///
/// Signals only the still-owned child and its verified foreground tty group.
/// Waits for the reader to reap the child and release capacity before success;
/// a timeout retains ownership so a retry cannot signal an unrelated process.
pub fn kill_session(state: &TerminalSessions, session_id: &str) -> Result<(), String> {
    #[cfg(unix)]
    if let Some(host) = &state.host {
        log::info!(target: "terminal", "terminal close requested");
        return host.kill(session_id);
    }
    let handles = {
        let guard = state.lock_sessions();
        guard.get(session_id).map(|s| {
            (
                s.child.clone(),
                s.master.clone(),
                s.flow.clone(),
                s.dead.clone(),
                s.closing.clone(),
            )
        })
    };
    if let Some((child, master, flow, dead, closing)) = handles {
        log::info!(target: "terminal", "terminal close requested");
        // Marked before the first signal, not after: the reader that sees the
        // child die reads this flag to tell a close GitPulse asked for from an
        // external death, and a reader finishing between the signal and the
        // store would count our own hangup as someone else's kill.
        dead.store(true, Ordering::SeqCst);
        terminate_pty_child(&child, &master, &closing)?;
        flow.stop();
        let deadline = Instant::now() + Duration::from_secs(2);
        loop {
            let present = state.lock_sessions().contains_key(session_id);
            if !present {
                break;
            }
            if Instant::now() >= deadline {
                return Err("Terminal process has not finished cleanup; retry Close".into());
            }
            thread::sleep(Duration::from_millis(10));
        }
    }
    Ok(())
}

/// Application exit closes every owned PTY before the runtime disappears.
pub fn shutdown_sessions(state: &TerminalSessions) -> Result<(), String> {
    #[cfg(unix)]
    if let Some(host) = &state.host {
        return host.stop_all();
    }
    let ids: Vec<String> = state.lock_sessions().keys().cloned().collect();
    // Every session at once: each close waits for its own reader (a hangup
    // grace plus a reap), and quitting with a full dock one session after
    // another cost that wait once per session. At most MAX_PTY_SESSIONS threads.
    let failures = thread::scope(|scope| {
        let closes: Vec<_> = ids
            .iter()
            .map(|id| scope.spawn(move || kill_session(state, id).is_err()))
            .collect();
        closes
            .into_iter()
            .map(|close| close.join().unwrap_or(true))
            .filter(|failed| *failed)
            .count()
    });
    // Drain even after a failure: a session that did close still has to give
    // its slot back before the runtime goes.
    let deadline = Instant::now() + Duration::from_secs(2);
    while state.active_sessions.load(Ordering::Acquire) > 0 && Instant::now() < deadline {
        thread::sleep(Duration::from_millis(10));
    }
    let remaining = state.active_sessions.load(Ordering::Acquire);
    match (failures, remaining) {
        (0, 0) => Ok(()),
        (0, remaining) => Err(format!(
            "{remaining} terminal processes have not finished cleanup"
        )),
        (failures, _) => Err(format!("Could not stop {failures} terminal sessions")),
    }
}

/// How many times `session_id`'s reader has woken from an idle wait. `None`
/// for a session that is not (or no longer) open. An idle session's reader
/// sleeps until output, an exit or its safety net, so this does not move.
pub fn reader_wakeups(state: &TerminalSessions, session_id: &str) -> Option<u64> {
    #[cfg(unix)]
    if let Some(host) = &state.host {
        return host.wakeups(session_id).unwrap_or_else(|error| {
            panic!("the terminal host's reader could not be asked: {error}")
        });
    }
    state
        .lock_sessions()
        .get(session_id)
        .map(|s| s.wakeups.load(Ordering::Relaxed))
}

/// Longest list of hidden sessions accepted from the view: far past the
/// session cap, a bound on an IPC-supplied list rather than a product limit.
const MAX_BACKGROUND_REPORT: usize = 256;

/// The view reports which of its sessions are on a tab nobody is looking at.
/// Their output is batched harder (`HIDDEN_MAX_HOLD`); every other session,
/// including one the view has never mentioned, is treated as seen. The list
/// is the whole truth each time, so a session that left it is visible again.
pub fn set_background_sessions(state: &TerminalSessions, session_ids: &[String]) {
    #[cfg(unix)]
    if let Some(host) = &state.host {
        let bounded = &session_ids[..session_ids.len().min(MAX_BACKGROUND_REPORT)];
        return host.set_background(bounded);
    }
    let hidden: std::collections::HashSet<&str> = session_ids
        .iter()
        .take(MAX_BACKGROUND_REPORT)
        .map(String::as_str)
        .collect();
    for (id, session) in state.lock_sessions().iter() {
        session
            .hidden
            .store(hidden.contains(id.as_str()), Ordering::Relaxed);
    }
}

/// Acknowledgements release only bytes already emitted for this session.
pub fn acknowledge_output(
    state: &TerminalSessions,
    session_id: &str,
    bytes: usize,
) -> Result<(), String> {
    #[cfg(unix)]
    if let Some(host) = &state.host {
        return host.ack(session_id, bytes);
    }
    let flow = state
        .lock_sessions()
        .get(session_id)
        .map(|s| s.flow.clone());
    // Final renderer callbacks can arrive after EOF removed the session.
    if let Some(flow) = flow {
        flow.acknowledge(bytes)?;
    }
    Ok(())
}

/// Whether the session's own process has exited. Reaping here is safe: the
/// status is retained, so `finalize_pty_session`'s `try_wait` still reports it.
#[cfg(unix)]
fn child_has_exited(child: &Arc<Mutex<Box<dyn Child + Send + Sync>>>) -> bool {
    matches!(
        child
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .try_wait(),
        Ok(Some(_))
    )
}

/// How long a terminal's jobs get between the hangup and the kill. An agent
/// CLI runs its SessionEnd hooks and flushes its transcript on SIGHUP, which
/// takes seconds, not the 200 ms `portable_pty` leaves before its own SIGKILL.
/// The wait ends as soon as every job has gone, so only a job that ignores the
/// hangup pays all of it.
pub(crate) const HANGUP_GRACE: Duration = Duration::from_millis(2500);

/// Counts the closes in progress on one session. While it is above zero only a
/// closer reaps the shell: the reader's exit check and its final reap wait,
/// and a context read answers that the terminal is closing.
///
/// A close must not hold the shell's lock for its whole grace (a context read
/// or a second close waited out seconds behind a stubborn job), yet it relies
/// on the shell staying unreaped from the first signal to the last: an
/// unreaped leader keeps its pid, and so the session it leads, from being
/// reused while a signal is aimed at that session. This is that guarantee
/// without the lock.
struct ClosingGuard(Arc<AtomicUsize>);

impl ClosingGuard {
    fn enter(closing: &Arc<AtomicUsize>) -> Self {
        closing.fetch_add(1, Ordering::SeqCst);
        Self(closing.clone())
    }
}

impl Drop for ClosingGuard {
    fn drop(&mut self) {
        self.0.fetch_sub(1, Ordering::SeqCst);
    }
}

fn terminate_pty_child(
    child: &Arc<Mutex<Box<dyn Child + Send + Sync>>>,
    master: &Arc<Mutex<Box<dyn MasterPty + Send>>>,
    closing: &Arc<AtomicUsize>,
) -> Result<(), String> {
    terminate_pty_child_within(child, master, closing, HANGUP_GRACE)
}

fn terminate_pty_child_within(
    child: &Arc<Mutex<Box<dyn Child + Send + Sync>>>,
    master: &Arc<Mutex<Box<dyn MasterPty + Send>>>,
    closing: &Arc<AtomicUsize>,
    grace: Duration,
) -> Result<(), String> {
    // Before the first look at the shell and until this returns: nothing else
    // reaps it, so its pid, and the session it leads, cannot be reused under
    // this call, with or without the lock below. A panic elsewhere must not
    // make the terminal unkillable.
    let _closing = ClosingGuard::enter(closing);
    let lock = || {
        child
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    };
    #[cfg_attr(not(unix), allow(unused_mut))]
    let mut shell = lock();
    if shell.try_wait().map_err(|e| e.to_string())?.is_some() {
        return Ok(());
    }
    #[cfg(unix)]
    {
        // The shell is a session leader only when it still owns the tty; a
        // reused pid or an unrelated session is never signalled.
        let session = shell
            .process_id()
            .and_then(|p| i32::try_from(p).ok())
            // SAFETY: getsid takes a numeric id and reads nothing else.
            .filter(|pid| *pid > 1 && unsafe { libc::getsid(*pid) } == *pid);
        if let Some(sid) = session {
            // The lock is held to list and signal the session and to list and
            // kill what is left, and released for the wait between them.
            if let Some(hung) = jobs::hang_up(sid, &foreground::session_members) {
                drop(shell);
                let gone = jobs::wait_for_exit(hung, grace, &foreground::session_members);
                shell = lock();
                if !gone {
                    jobs::kill_stragglers(sid, &foreground::session_members);
                }
            }
        }
    }
    let _ = master;
    #[cfg(not(unix))]
    let _ = grace;
    // The shell may have gone with its jobs; there is nothing left to signal.
    if shell.try_wait().map_err(|e| e.to_string())?.is_some() {
        return Ok(());
    }
    #[cfg(unix)]
    {
        stop_leader(&mut shell)
    }
    // The shell itself: `kill` hangs it up before it kills it.
    #[cfg(not(unix))]
    {
        shell.kill().map_err(|e| {
            log::warn!(target: "terminal", "could not signal terminal child: {e}");
            format!("Could not stop terminal process: {e}")
        })
    }
}

/// How long the session leader has after its hangup before it is killed: what
/// `portable_pty`'s own `kill` allowed, which this replaces.
#[cfg(unix)]
const LEADER_HANGUP_WAIT: Duration = Duration::from_millis(200);

/// Hangs up the session's own process, then kills it if the hangup did not end
/// it — through `procguard::signal`, the one primitive every signal the app
/// (and the terminal host) sends goes through, never `portable_pty`'s raw
/// `kill(2)`. The caller holds the child's lock and has seen it unreaped, so
/// the pid is still this child's; the start time read here is what
/// `signal_process` re-checks at each signal.
///
/// The shell is often already on its way out: the hangup of its session's
/// jobs reaches it too, and on macOS a killed process blocked on a full tty
/// cannot finish exiting until the reader drains it, which starts only once
/// this close returns. An exiting process, and a zombie, cannot be described by
/// the process table, so the guarded primitive refuses to signal it — where a
/// raw `kill(2)` would have "succeeded" at nothing. This child is unreaped and
/// its lock is held, so its pid cannot belong to anyone else: one the table
/// cannot describe is exiting, needs no signal, and is left to the reaper. One
/// that never finishes is reported by the close's own cleanup deadline.
#[cfg(unix)]
fn stop_leader(shell: &mut Box<dyn Child + Send + Sync>) -> Result<(), String> {
    let pid = shell
        .process_id()
        .and_then(|p| i32::try_from(p).ok())
        .ok_or("The terminal process reports no id; it was not signalled")?;
    let Some(started) = start_of(pid) else {
        log::debug!(target: "terminal", "the terminal process is already exiting; nothing to signal");
        return Ok(());
    };
    if signal_leader(shell, pid, started, libc::SIGHUP)? {
        return Ok(());
    }
    if reaped_within(shell, LEADER_HANGUP_WAIT)? {
        return Ok(());
    }
    signal_leader(shell, pid, started, libc::SIGKILL).map(|_| ())
}

/// The start time the process table reports for `pid`, which
/// `signal_process` compares against the one recorded.
#[cfg(unix)]
fn start_of(pid: i32) -> Option<u64> {
    crate::sysload::procscan::probe_one(pid).map(|facts| facts.start_us)
}

/// Sends `signo` to the session leader through the guarded primitive.
/// `Ok(true)` when the child turned out to be gone already.
#[cfg(unix)]
fn signal_leader(
    shell: &mut Box<dyn Child + Send + Sync>,
    pid: i32,
    started: u64,
    signo: libc::c_int,
) -> Result<bool, String> {
    match crate::procguard::signal::signal_process(pid, started, &start_of, signo, "terminal close")
    {
        Ok(()) => Ok(false),
        Err(error) if error.raw_os_error() == Some(libc::ESRCH) => Ok(true),
        // Refused because the table no longer describes it: exiting.
        Err(_) if start_of(pid).is_none() => Ok(true),
        Err(error) => {
            if reaped_within(shell, LEADER_HANGUP_WAIT)? {
                return Ok(true);
            }
            log::warn!(target: "terminal", "could not signal terminal child: {error}");
            Err(format!("Could not stop terminal process: {error}"))
        }
    }
}

/// Whether the child exits (and is reaped here) within `within`.
#[cfg(unix)]
fn reaped_within(
    shell: &mut Box<dyn Child + Send + Sync>,
    within: Duration,
) -> Result<bool, String> {
    let deadline = Instant::now() + within;
    loop {
        if shell.try_wait().map_err(|e| e.to_string())?.is_some() {
            return Ok(true);
        }
        if Instant::now() >= deadline {
            return Ok(false);
        }
        thread::sleep(Duration::from_millis(10));
    }
}

/// One-shot bounded command execution in `repo_path`.
pub fn run_terminal(
    repo_path: &str,
    args: &[String],
    timeout_secs: Option<u64>,
) -> Result<TerminalRunResult, String> {
    run_terminal_inner(repo_path, args, timeout_secs, None)
}

/// Executes one command proposed by MANVI's local-model plane.
///
/// This is deliberately separate from [`run_terminal`], the user-owned
/// console. The app chooses the purpose, the backend validates the command
/// against that purpose, and every accepted command reaches the MANVI command
/// gate (not only Git). The allowlist remains authoritative because host
/// posture can demote a sidecar allowlist miss to an allow.
pub fn run_manvi_action(
    repo_path: &str,
    args: &[String],
    action_kind: ManviActionKind,
    timeout_secs: Option<u64>,
) -> Result<TerminalRunResult, String> {
    validate_manvi_action(args, action_kind)?;
    let repo = validate_repo(repo_path)?;
    validate_manvi_paths(&repo, args, action_kind)?;
    run_terminal_inner(repo_path, args, timeout_secs, Some(action_kind))
}

fn run_terminal_inner(
    repo_path: &str,
    args: &[String],
    timeout_secs: Option<u64>,
    manvi_action: Option<ManviActionKind>,
) -> Result<TerminalRunResult, String> {
    if args.is_empty() {
        return Err("No command provided".into());
    }
    validate_argv_bounds(args)?;
    let repo = validate_repo(repo_path)?;
    let program = &args[0];
    if program.trim().is_empty() {
        return Err("Invalid program name".into());
    }

    let arg_refs: Vec<&str> = args.iter().map(String::as_str).collect();
    let command_rendered = crate::harness::render_command(&arg_refs);

    // Git commands pass through the MANVI harness command gate.
    // Force pushes and other policy-violating git mutations are blocked.
    // Non-git commands bypass the harness gate (which only judges git commands)
    // and follow the deps/CI execution precedent.
    let is_git = program == "git"
        || program.ends_with("/git")
        || program.ends_with("\\git")
        || program.ends_with("\\git.exe")
        || program.ends_with("/git.exe");
    let should_gate = is_git || manvi_action.is_some();
    let (policy_verdict, gated) = if should_gate {
        match guard_command(repo_path, &arg_refs) {
            Ok(verdict) => (Some(verdict), true),
            Err(refusal) => {
                // Return honest refusal/failure if blocked or gate failed.
                return Err(refusal);
            }
        }
    } else {
        (None, false)
    };

    let timeout = timeout_secs
        .map(|s| Duration::from_secs(s).clamp(MIN_RUN_TIMEOUT, MAX_RUN_TIMEOUT))
        .unwrap_or(DEFAULT_RUN_TIMEOUT);

    let started = Instant::now();
    let outcome = run_captured(
        program,
        &arg_refs[1..],
        Some(&repo),
        timeout,
        &[],
        TERMINAL_TAIL_CAP_BYTES,
    );
    let duration_ms = u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX);

    match outcome {
        Ok(RunOutcome::Finished(run)) => Ok(TerminalRunResult {
            command: command_rendered,
            gated,
            policy: policy_verdict,
            timed_out: false,
            exit_code: Some(run.status_code),
            stdout_tail: run.stdout_tail,
            stderr_tail: run.stderr_tail,
            truncated: run.truncated,
            truncation_reason: run.truncation_reason,
            duration_ms,
        }),
        Ok(RunOutcome::TimedOut(dur)) => Ok(TerminalRunResult {
            command: command_rendered,
            gated,
            policy: policy_verdict,
            timed_out: true,
            exit_code: None,
            stdout_tail: String::new(),
            stderr_tail: format!("Command timed out after {}s", dur.as_secs()),
            truncated: false,
            truncation_reason: None,
            duration_ms,
        }),
        Err(e) => Err(e),
    }
}

fn validate_argv_bounds(args: &[String]) -> Result<(), String> {
    if args.len() > TERMINAL_ARG_COUNT_CAP {
        return Err(format!(
            "Command has {} arguments; the limit is {TERMINAL_ARG_COUNT_CAP}",
            args.len()
        ));
    }
    let mut total = 0usize;
    for arg in args {
        if arg.len() > TERMINAL_ARG_BYTES_CAP {
            return Err(format!(
                "Command argument is {} bytes; the per-argument limit is {TERMINAL_ARG_BYTES_CAP}",
                arg.len()
            ));
        }
        if arg.chars().any(char::is_control) {
            return Err("Command arguments cannot contain control characters".into());
        }
        total = total.saturating_add(arg.len());
        if total > TERMINAL_ARGV_BYTES_CAP {
            return Err(format!(
                "Command argv exceeds the {TERMINAL_ARGV_BYTES_CAP}-byte limit"
            ));
        }
    }
    Ok(())
}

pub(crate) fn normalized_program(program: &str) -> String {
    let lower = program.to_ascii_lowercase();
    lower
        .strip_suffix(".exe")
        .or_else(|| lower.strip_suffix(".cmd"))
        .or_else(|| lower.strip_suffix(".bat"))
        .unwrap_or(&lower)
        .to_string()
}

fn has_parent_or_absolute_path(value: &str) -> bool {
    let candidate = value.split_once('=').map_or(value, |(_, rhs)| rhs);
    if candidate.is_empty() || candidate.starts_with('-') {
        return false;
    }
    let path = std::path::Path::new(candidate);
    path.is_absolute()
        || candidate.starts_with("\\\\")
        || candidate.as_bytes().get(1) == Some(&b':')
        || path
            .components()
            .any(|part| matches!(part, std::path::Component::ParentDir))
}

fn command_is_one_of(args: &[String], allowed: &[&str]) -> bool {
    args.get(1)
        .map(|arg| allowed.contains(&arg.to_ascii_lowercase().as_str()))
        .unwrap_or(false)
}

/// `go test ...` or `go -C <dir> test ...`. `-C` must be first (see `go help`).
fn go_coverage_test_allowed(args: &[String]) -> bool {
    match args.get(1).map(String::as_str) {
        Some("test") => true,
        Some("-C") => {
            let dir = args.get(2).map(String::as_str).unwrap_or("");
            !dir.is_empty()
                && !dir.starts_with('-')
                && args.get(3).map(String::as_str) == Some("test")
        }
        _ => false,
    }
}

/// Coverage generation may mutate the host toolchain in exactly one way:
/// `cargo install cargo-llvm-cov --locked` (flag order may swap). Extra
/// crates, `--git`, `--path`, or a missing `--locked` are refused. Analysis
/// (`Coverage`) never calls this.
fn cargo_llvm_cov_install_allowed(args: &[String]) -> bool {
    if args.get(1).map(String::as_str) != Some("install") {
        return false;
    }
    let rest: Vec<&str> = args.iter().skip(2).map(String::as_str).collect();
    rest.len() == 2 && rest.contains(&"--locked") && rest.contains(&"cargo-llvm-cov")
}

/// `python -m venv .venv` — creating the project's own virtualenv.
///
/// The target directory is pinned to the two conventional names rather than
/// "any repository-relative path": coverage generation has exactly one reason
/// to create a virtualenv, and a free-form path would let plan text scatter
/// interpreters through the checkout.
/// Resolves a bare program name against the process PATH to its canonical
/// path. Host-controlled by construction — the repository has no say in it,
/// which is what makes it usable as a trust anchor.
fn resolve_on_path(program: &str) -> Option<std::path::PathBuf> {
    let path = std::env::var_os("PATH")?;
    std::env::split_paths(&path)
        .flat_map(|dir| path_candidates(&dir, program))
        .filter(|candidate| candidate.is_file())
        .find_map(|candidate| std::fs::canonicalize(candidate).ok())
}

/// Spellings of `program` to look for in one PATH directory.
///
/// Windows resolves a bare program name through an executable suffix: the file
/// on disk is `python3.exe`, and joining the bare name matched nothing there,
/// so the PATH identity check below could never succeed on Windows at all.
/// Only `.exe` is added, deliberately — the wider `PATHEXT` set would let this
/// resolve a `.bat`/`.cmd` shim, and the only caller is deciding which binary
/// it is willing to execute.
fn path_candidates(dir: &std::path::Path, program: &str) -> Vec<std::path::PathBuf> {
    let direct = dir.join(program);
    if cfg!(windows) && !program.contains('.') {
        vec![dir.join(format!("{program}.exe")), direct]
    } else {
        vec![direct]
    }
}

/// Upper bound on an interpreter this will read in order to compare it.
/// CPython's launcher is well under a megabyte on every platform.
const MAX_INTERPRETER_BYTES: u64 = 32 * 1024 * 1024;

/// Whether two paths hold the same program, byte for byte.
///
/// Length is compared first, so the common mismatch costs two stats rather
/// than two reads. A zero-length file matches nothing: an empty "interpreter"
/// is never the program that would otherwise have run.
fn same_program_bytes(a: &std::path::Path, b: &std::path::Path) -> bool {
    let (Ok(meta_a), Ok(meta_b)) = (std::fs::metadata(a), std::fs::metadata(b)) else {
        return false;
    };
    if !meta_a.is_file() || !meta_b.is_file() || meta_a.len() == 0 {
        return false;
    }
    if meta_a.len() != meta_b.len() || meta_a.len() > MAX_INTERPRETER_BYTES {
        return false;
    }
    match (std::fs::read(a), std::fs::read(b)) {
        (Ok(left), Ok(right)) => left == right,
        _ => false,
    }
}

/// Whether `target` is an unmodified copy of a virtualenv launcher shipped by
/// the Python installation that `host` belongs to.
///
/// Windows `python -m venv` does not copy the interpreter into the virtualenv.
/// It copies a launcher, which reads `pyvenv.cfg` at run time and re-executes
/// the real interpreter. Measured on CPython 3.12.10: the base install's
/// `python.exe` is 104,952 bytes, the virtualenv's is 274,424, and the latter
/// is byte-identical to `<install>\Lib\venv\scripts\nt\python.exe`.
///
/// The directory is scanned rather than a file name assumed: the launcher is
/// named `python.exe` there in 3.12 and `venvlauncher.exe` in older releases,
/// and a name list would silently stop matching on the next rename. Scanning
/// admits nothing extra — the bytes still have to equal a file the trusted
/// installation ships.
fn is_shipped_venv_launcher(host: &std::path::Path, target: &std::path::Path) -> bool {
    let Some(install) = host.parent() else {
        return false;
    };
    let launchers = install.join("Lib").join("venv").join("scripts").join("nt");
    let Ok(entries) = std::fs::read_dir(launchers) else {
        return false;
    };
    entries.flatten().any(|entry| {
        entry.file_type().is_ok_and(|kind| kind.is_file())
            && same_program_bytes(&entry.path(), target)
    })
}

/// True when `name` is a Python interpreter's file name (`python`, `python3`,
/// `python3.14`, `python3.14t`), and not some other executable a symlink was
/// pointed at. This is what separates a virtualenv from `.venv/bin/python`
/// aimed at `/bin/sh`.
fn is_python_interpreter_name(name: &str) -> bool {
    let name = name.strip_suffix(".exe").unwrap_or(name);
    if name == "python" || name == "python3" {
        return true;
    }
    name.strip_prefix("python3.")
        .is_some_and(|rest| rest.starts_with(|c: char| c.is_ascii_digit()))
}

/// True when the virtualenv's interpreter resolves to a Python that GitPulse
/// is willing to execute.
///
/// Every way to qualify starts from the same property: the process GitPulse
/// executes must be the interpreter that would have run anyway. A virtualenv
/// changes which site-packages are imported, not which binary runs.
///
/// The target must be named like a Python in every case. Then one of:
///
/// 1. It IS the Python on PATH — the same canonical path. Exact, and true
///    whenever GitPulse runs from a shell.
/// 2. It resolves somewhere outside the repository. This exists because (1)
///    silently fails for an installed app: a macOS app launched from Finder
///    inherits launchd's PATH, not the user's shell PATH, so anchoring only on
///    (1) made GitPulse refuse the very virtualenv it had just created — but
///    only once installed, which is the configuration users actually run.
/// 3. It is byte-for-byte a host interpreter, when it resolves INSIDE the
///    repository.
///
/// (3) exists because `python -m venv` does not build the same thing on every
/// platform. On Unix it symlinks `.venv/bin/python` out to the host toolchain,
/// which is what (2) is about. On Windows it COPIES `python.exe` into
/// `.venv\Scripts\`, so the target is repository-resident and (2) refused
/// every real virtualenv there — Python coverage could not run at all.
///
/// Windows `venv` does not even copy the interpreter — it installs a launcher
/// that re-executes the real one — so the comparison is against every file the
/// trusted installation ships for that purpose, found by scanning its
/// `Lib\venv\scripts\nt` directory. `pyvenv.cfg` is required to exist, as the
/// marker of a real virtualenv, but is never read: it is repository content,
/// and nothing a repository writes should be able to nominate the bytes
/// GitPulse will execute. A repository that drops a payload at the virtualenv
/// spelling fails every branch — the bytes are not the host interpreter's and
/// not any launcher the host ships.
///
/// SECURITY IMPACT: this is the one path on which GitPulse will execute a file
/// that lives inside the opened repository. Byte equality with a qualifying
/// host interpreter is the whole of what makes that safe — weaken it and the
/// Windows virtualenv spelling becomes an arbitrary execution primitive.
/// Writing a hostile binary named `python3` outside the repository already
/// requires the code execution this check exists to prevent.
fn resolves_to_trusted_python(repo: &std::path::Path, interpreter: &std::path::Path) -> bool {
    let Ok(target) = std::fs::canonicalize(interpreter) else {
        return false;
    };
    let hosts: Vec<std::path::PathBuf> = ["python3", "python"]
        .iter()
        .filter_map(|name| resolve_on_path(name))
        .collect();
    if hosts.contains(&target) {
        return true;
    }
    let Ok(root) = std::fs::canonicalize(repo) else {
        return false;
    };
    let Some(name) = target.file_name().and_then(|name| name.to_str()) else {
        return false;
    };
    if !is_python_interpreter_name(name) {
        return false;
    }
    if !target.starts_with(&root) {
        return true;
    }
    // Repository-resident: admitted only as bytes the trusted host
    // installation itself ships — either the interpreter, or the virtualenv
    // launcher it installs in place of one. `pyvenv.cfg` is deliberately not
    // consulted: it is repository content, and nothing a repository writes
    // should be able to nominate what GitPulse will execute.
    hosts
        .iter()
        .any(|host| same_program_bytes(host, &target) || is_shipped_venv_launcher(host, &target))
}

/// Validates the project virtualenv's interpreter as a program.
///
/// Canonical containment — the rule every other repository-local program is
/// held to — is the wrong test here and would reject every real virtualenv:
/// `python -m venv` builds `.venv/bin/python` as a symlink *out* to the host
/// toolchain, by construction. So the property is preserved a different way
/// rather than dropped. Three things must hold:
///
/// 1. the venv directory carries `pyvenv.cfg`, the marker of a real
///    virtualenv rather than a directory shaped like one;
/// 2. the interpreter path exists inside the repository, lexically; and
/// 3. it is the same program as `python3`/`python` on PATH — see
///    [`resolves_to_trusted_python`] for what "same" means on each platform.
///
/// (3) is the load-bearing one. It means the process GitPulse executes is the
/// interpreter it would have executed regardless — the virtualenv changes
/// which site-packages get imported, not which binary runs. A repository that
/// ships `.venv/bin/python` as a symlink to `/bin/sh`, to a checked-in
/// payload, or to any other interpreter fails it and is refused.
pub(crate) fn check_venv_interpreter(
    repo: &std::path::Path,
    relative: &str,
) -> Result<(), &'static str> {
    let Some(venv_dir) = relative.split('/').next().filter(|dir| !dir.is_empty()) else {
        return Err("has no virtualenv directory");
    };
    // Presence only. Its CONTENTS are never read: it is repository content,
    // and nothing a repository writes should influence what gets executed.
    match sandbox_join_canonical(repo, &format!("{venv_dir}/pyvenv.cfg")) {
        Ok(cfg) if cfg.is_file() => {}
        _ => return Err("is not inside a virtualenv (no pyvenv.cfg)"),
    }
    let Ok(interpreter) = sandbox_join(repo, relative) else {
        return Err("is not a repository-relative path");
    };
    if !interpreter.is_file() {
        return Err("is not a repository file");
    }
    if !resolves_to_trusted_python(repo, &interpreter) {
        return Err("does not resolve to a Python interpreter outside the repository");
    }
    Ok(())
}

fn validate_venv_interpreter(
    repo: &std::path::Path,
    relative: &str,
    kind: ManviActionKind,
) -> Result<(), String> {
    check_venv_interpreter(repo, relative).map_err(|detail| {
        format!(
            "MANVI {} action refused: virtualenv interpreter '{relative}' {detail}; GitPulse will not execute it",
            kind.label()
        )
    })
}

/// Single owner of "is this program a repository file rather than a PATH
/// name". Three checks must agree on it — the executable-path refusal, the
/// allowlist match, and path validation — and they previously each carried
/// their own literal list, which is how the vendored phpunit and venv
/// interpreter the scanner plans could be accepted by one and refused by
/// another.
fn program_is_repo_local(program_raw: &str) -> bool {
    repo_local_program(&normalized_program(program_raw)).is_some()
}

fn python_venv_create_allowed(args: &[String]) -> bool {
    args.len() == 4
        && args.get(2).map(String::as_str) == Some("venv")
        && args
            .get(3)
            .is_some_and(|dir| VENV_DIR_NAMES.contains(&dir.as_str()))
}

/// `<repo venv>/bin/python -m pip install pytest pytest-cov`.
///
/// `pip install` is absent from every other allowlist arm on purpose. This is
/// the single pinned exception and it is refused outright unless the
/// interpreter is the repository's own virtualenv: installing into whatever
/// interpreter happens to be on PATH mutates the host Python (and on a
/// PEP 668 system fails or demands `--break-system-packages`), which is not a
/// trade a coverage panel may make. The package set is fixed, so this cannot
/// become a general package installer.
fn pip_coverage_install_allowed(args: &[String], interpreter_is_repo_local: bool) -> bool {
    if !interpreter_is_repo_local {
        return false;
    }
    if args.get(3).map(String::as_str) != Some("install") {
        return false;
    }
    let packages: Vec<&str> = args.iter().skip(4).map(String::as_str).collect();
    // Two pinned sets, not a general installer: the pytest toolchain for the
    // Python family and gcovr for the C/C++ one. Both are rendered into the
    // planner's command text from these same slices.
    !packages.is_empty()
        && packages
            .iter()
            .all(|pkg| PYTEST_PACKAGES.contains(pkg) || GCOVR_PACKAGES.contains(pkg))
}

/// `cmake -S . -B build-gitpulse-coverage -DCMAKE_*_FLAGS=--coverage` and
/// `cmake --build build-gitpulse-coverage`.
///
/// Pinned to the two shapes the planner emits. Notably NOT permitted: any
/// `-D` other than the three flag variables and the build type, `--install`
/// (which writes outside the build directory), `-P` (which executes an
/// arbitrary CMake script), and any build directory other than the managed
/// one — a configure into the project's own `build/` would silently replace a
/// developer's build with an instrumented one.
fn cmake_coverage_allowed(args: &[String]) -> bool {
    let rest: Vec<&str> = args.iter().skip(1).map(String::as_str).collect();
    match rest.first().copied() {
        // `cmake --build <managed dir>` and nothing else.
        Some("--build") => rest.len() == 2 && rest[1] == NATIVE_COVERAGE_BUILD_DIR,
        // The configure step.
        Some("-S") => {
            let allowed_define = |arg: &str| {
                matches!(
                    arg,
                    "-DCMAKE_BUILD_TYPE=Debug" | "-DCMAKE_BUILD_TYPE=RelWithDebInfo"
                ) || matches!(
                    arg.strip_suffix(NATIVE_COVERAGE_FLAG)
                        .and_then(|head| head.strip_suffix('=')),
                    Some("-DCMAKE_C_FLAGS")
                        | Some("-DCMAKE_CXX_FLAGS")
                        | Some("-DCMAKE_EXE_LINKER_FLAGS")
                        | Some("-DCMAKE_SHARED_LINKER_FLAGS")
                )
            };
            rest.len() >= 4
                && rest[1] == "."
                && rest[2] == "-B"
                && rest[3] == NATIVE_COVERAGE_BUILD_DIR
                && rest[4..].iter().all(|arg| allowed_define(arg))
        }
        _ => false,
    }
}

/// `ctest --test-dir build-gitpulse-coverage --output-on-failure`.
///
/// The directory is pinned so this cannot drive an arbitrary test tree, and
/// the only other accepted token is the diagnostic output flag. `--build-and-test`
/// and `-S` (script mode) are excluded: both execute project-supplied code
/// beyond running the tests that were just built.
fn ctest_coverage_allowed(args: &[String]) -> bool {
    let rest: Vec<&str> = args.iter().skip(1).map(String::as_str).collect();
    if rest.first().copied() != Some("--test-dir")
        || rest.get(1).copied() != Some(NATIVE_COVERAGE_BUILD_DIR)
    {
        return false;
    }
    rest[2..].iter().all(|arg| *arg == "--output-on-failure")
}

/// `gcovr -r . build-gitpulse-coverage --lcov build-gitpulse-coverage/lcov.info`.
///
/// gcovr reads gcov data and writes a report; the only write it is allowed
/// here is the pinned lcov path inside the managed build directory. Every
/// other output flag (`--html`, `--json`, `--output`, `--txt`) is refused so
/// this cannot be turned into a file writer aimed anywhere else.
fn gcovr_coverage_allowed(args: &[String]) -> bool {
    let rest: Vec<&str> = args.iter().skip(1).map(String::as_str).collect();
    rest.first().copied() == Some("-r")
        && rest.get(1).copied() == Some(".")
        && rest.get(2).copied() == Some(NATIVE_COVERAGE_BUILD_DIR)
        && rest.get(3).copied() == Some("--lcov")
        && rest.get(4).copied() == Some(NATIVE_COVERAGE_LCOV)
        && rest.len() == 5
}

/// `npm install --save-dev @vitest/coverage-v8`.
///
/// A project devDependency, not a host mutation — but still a write to
/// package.json and the lockfile, so it is pinned to the provider set and
/// requires an explicit dev flag. A bare `npm install <anything>` under a
/// coverage action would let plan text add any package to the project.
fn node_coverage_provider_install_allowed(args: &[String]) -> bool {
    if !matches!(
        args.get(1).map(String::as_str),
        Some("install" | "i" | "add")
    ) {
        return false;
    }
    let rest: Vec<&str> = args.iter().skip(2).map(String::as_str).collect();
    let dev_flag = |arg: &str| matches!(arg, "--save-dev" | "-D" | "--dev");
    if !rest.iter().copied().any(dev_flag) {
        return false;
    }
    // Every token is either the dev flag or a pinned provider: no stray flags
    // (`--registry`, `--force`) and no extra packages ride along.
    let mut packages = 0usize;
    for arg in &rest {
        if dev_flag(arg) {
            continue;
        }
        if !JS_COVERAGE_PROVIDERS.contains(arg) {
            return false;
        }
        packages += 1;
    }
    packages > 0
}

fn rustup_llvm_tools_allowed(args: &[String]) -> bool {
    args.len() == 4
        && args.get(1).map(String::as_str) == Some("component")
        && args.get(2).map(String::as_str) == Some("add")
        && args.get(3).map(String::as_str) == Some("llvm-tools-preview")
}

fn npm_run_is_verification(args: &[String]) -> bool {
    let Some(script) = args.get(2).map(|s| s.to_ascii_lowercase()) else {
        return false;
    };
    [
        "test", "check", "lint", "cover", "coverage", "audit", "verify", "ci",
    ]
    .iter()
    .any(|needle| {
        script == *needle
            || script
                .strip_prefix(needle)
                .is_some_and(|suffix| suffix.starts_with(':') || suffix.starts_with('-'))
    })
}

fn local_js_runner(args: &[String], flag_index: usize) -> bool {
    matches!(
        (
            args.get(flag_index).map(String::as_str),
            args.get(flag_index + 1).map(String::as_str)
        ),
        (
            Some("--no" | "--no-install"),
            Some("vitest" | "jest" | "c8" | "nyc")
        )
    )
}

fn build_tool_tasks_allowed(args: &[String], kind: ManviActionKind) -> bool {
    let mut saw_task = false;
    for raw in args.iter().skip(1) {
        let arg = raw.to_ascii_lowercase();
        if matches!(
            arg.as_str(),
            "--no-daemon" | "--stacktrace" | "--info" | "--quiet" | "--offline" | "-q"
        ) {
            continue;
        }
        saw_task = true;
        let allowed = match kind {
            ManviActionKind::Health => matches!(
                arg.as_str(),
                "clean"
                    | "test"
                    | "check"
                    | "verify"
                    | "build"
                    | "dependencyupdates"
                    | "dependency:tree"
                    | "versions:display-dependency-updates"
                    | "versions:use-latest-releases"
            ),
            ManviActionKind::Coverage | ManviActionKind::CoverageGenerator => matches!(
                arg.as_str(),
                "clean"
                    | "test"
                    | "check"
                    | "verify"
                    | "jacocotestreport"
                    | "jacoco:test"
                    | "jacoco:report"
                    | "koverxmlreport"
                    | "koverhtmlreport"
            ),
        };
        if !allowed {
            return false;
        }
    }
    saw_task
}

fn node_package_command_allowed(args: &[String], kind: ManviActionKind) -> bool {
    let Some(subcommand) = args.get(1).map(|s| s.to_ascii_lowercase()) else {
        return false;
    };
    match kind {
        ManviActionKind::Health => match subcommand.as_str() {
            "audit" | "outdated" | "update" | "upgrade" | "install" | "ci" | "test" => true,
            "run" => npm_run_is_verification(args),
            _ => false,
        },
        ManviActionKind::Coverage => match subcommand.as_str() {
            "test" => true,
            "run" => npm_run_is_verification(args),
            // npm exec may otherwise download a missing package. `--no` makes
            // this a local-dependency-only execution on supported npm builds.
            "exec" => local_js_runner(args, 2),
            _ => false,
        },
        // Generation may additionally install the missing coverage provider.
        // Analysis (`Coverage`) never writes to the project.
        ManviActionKind::CoverageGenerator => match subcommand.as_str() {
            "test" => true,
            "run" => npm_run_is_verification(args),
            "exec" => local_js_runner(args, 2),
            "install" | "i" | "add" => node_coverage_provider_install_allowed(args),
            _ => false,
        },
    }
}

fn python_module_allowed(
    args: &[String],
    kind: ManviActionKind,
    interpreter_is_repo_local: bool,
) -> bool {
    if args.get(1).map(String::as_str) != Some("-m") {
        return false;
    }
    let module = args.get(2).map(|s| s.to_ascii_lowercase());
    match kind {
        ManviActionKind::Health => match module.as_deref() {
            Some("pip") => command_is_one_of(&args[2..], &["check", "list", "audit"]),
            Some("pip_audit" | "pytest") => true,
            _ => false,
        },
        ManviActionKind::Coverage => match module.as_deref() {
            Some("pytest") => true,
            Some("coverage") => command_is_one_of(
                &args[2..],
                &["run", "report", "html", "xml", "json", "lcov"],
            ),
            _ => false,
        },
        // Generation may additionally build the project virtualenv and put
        // the pytest coverage toolchain in it. Both steps are pinned; neither
        // is reachable from the read-only analysis action.
        ManviActionKind::CoverageGenerator => match module.as_deref() {
            Some("pytest") => true,
            // The C/C++ report step, run through the interpreter gcovr was
            // installed into. `args[2..]` starts at the module name, which is
            // where `gcovr_coverage_allowed` expects the program to sit.
            // Repo-local only, for the same reason `pip install` is: the
            // planner never names the host interpreter, so neither may the
            // gate accept one.
            Some(m) if m == GCOVR_MODULE => {
                interpreter_is_repo_local && gcovr_coverage_allowed(&args[2..])
            }
            Some("coverage") => command_is_one_of(
                &args[2..],
                &["run", "report", "html", "xml", "json", "lcov"],
            ),
            Some("venv") => python_venv_create_allowed(args),
            Some("pip") => pip_coverage_install_allowed(args, interpreter_is_repo_local),
            _ => false,
        },
    }
}

/// Backend authority boundary for commands originating in model text.
///
/// This intentionally validates shapes, not exact package names: reports can
/// cover any ecosystem and package. Program families and verbs are bounded,
/// shells/network/file utilities are absent, paths cannot be absolute or walk
/// upward, and the separate MANVI policy gate still judges every accepted
/// command immediately before execution.
pub(crate) fn validate_manvi_action(args: &[String], kind: ManviActionKind) -> Result<(), String> {
    if args.is_empty() {
        return Err("MANVI action has no command".into());
    }
    validate_argv_bounds(args).map_err(|e| format!("MANVI action refused: {e}"))?;

    let program_raw = args[0].trim();
    let repo_wrapper = program_is_repo_local(program_raw);
    if !repo_wrapper
        && (program_raw.contains('/')
            || program_raw.contains('\\')
            || has_parent_or_absolute_path(program_raw))
    {
        return Err(format!(
            "MANVI {} action refused: executable paths are not allowed",
            kind.label()
        ));
    }
    if args
        .iter()
        .skip(1)
        .any(|arg| has_parent_or_absolute_path(arg))
    {
        return Err(format!(
            "MANVI {} action refused: arguments must stay inside the open repository",
            kind.label()
        ));
    }
    if args.iter().skip(1).any(|arg| {
        let lower = arg.to_ascii_lowercase();
        ["http:", "https:", "file:", "git:", "git+", "ssh:"]
            .iter()
            .any(|prefix| lower.starts_with(prefix))
    }) {
        return Err(format!(
            "MANVI {} action refused: external URLs and transports are not allowed",
            kind.label()
        ));
    }
    if args.iter().skip(1).any(|arg| {
        matches!(
            arg.to_ascii_lowercase().as_str(),
            "-g" | "--global" | "--user" | "--system" | "--location=global"
        )
    }) {
        return Err(format!(
            "MANVI {} action refused: global package-environment mutation is not allowed",
            kind.label()
        ));
    }

    let spelled = normalized_program(program_raw);
    // A repository-local executable is judged as the tool it is, once its
    // spelling has been pinned to a known-safe exact path.
    let repo_local = repo_local_program(&spelled);
    let program = repo_local.unwrap_or(spelled.as_str()).to_string();
    let allowed = match program.as_str() {
        "npm" | "pnpm" | "yarn" | "bun" => node_package_command_allowed(args, kind),
        "npx" => {
            matches!(
                kind,
                ManviActionKind::Coverage | ManviActionKind::CoverageGenerator
            ) && local_js_runner(args, 1)
        }
        "cargo" => match kind {
            ManviActionKind::Health => command_is_one_of(
                args,
                &["audit", "update", "test", "check", "clippy", "tree"],
            ),
            ManviActionKind::Coverage => {
                command_is_one_of(args, &["llvm-cov", "tarpaulin", "test", "nextest"])
            }
            ManviActionKind::CoverageGenerator => {
                command_is_one_of(args, &["llvm-cov", "tarpaulin", "test", "nextest"])
                    || cargo_llvm_cov_install_allowed(args)
            }
        },
        "rustup" => kind == ManviActionKind::CoverageGenerator && rustup_llvm_tools_allowed(args),
        "python" | "python3" | "py" => {
            python_module_allowed(args, kind, repo_local == Some("python"))
        }
        "pip" | "pip3" => {
            kind == ManviActionKind::Health && command_is_one_of(args, &["check", "list", "audit"])
        }
        "pip-audit" => kind == ManviActionKind::Health,
        "pytest" => {
            matches!(
                kind,
                ManviActionKind::Coverage | ManviActionKind::CoverageGenerator
            ) || kind == ManviActionKind::Health
        }
        "coverage" => {
            matches!(
                kind,
                ManviActionKind::Coverage | ManviActionKind::CoverageGenerator
            ) && command_is_one_of(args, &["run", "report", "html", "xml", "json", "lcov"])
        }
        "go" => match kind {
            ManviActionKind::Health => command_is_one_of(args, &["get", "mod", "test", "list"]),
            ManviActionKind::Coverage | ManviActionKind::CoverageGenerator => {
                go_coverage_test_allowed(args)
            }
        },
        "govulncheck" => kind == ManviActionKind::Health,
        "composer" => match kind {
            ManviActionKind::Health => command_is_one_of(
                args,
                &["audit", "update", "require", "install", "show", "test"],
            ),
            // Materializing dependencies already pinned in composer.lock is
            // what makes vendor/bin/phpunit exist at all.
            ManviActionKind::CoverageGenerator => command_is_one_of(args, &["install", "test"]),
            ManviActionKind::Coverage => command_is_one_of(args, &["test"]),
        },
        "bundle" | "bundler" => match args.get(1).map(String::as_str) {
            Some("audit" | "update" | "check") => kind == ManviActionKind::Health,
            // `bundle install` resolves the Gemfile so `bundle exec` can run
            // at all; generation needs it, analysis does not.
            Some("install") => matches!(
                kind,
                ManviActionKind::Health | ManviActionKind::CoverageGenerator
            ),
            Some("exec") => args.get(2).is_some_and(|tool| match kind {
                ManviActionKind::Health => {
                    matches!(tool.as_str(), "rake" | "rspec" | "bundler-audit")
                }
                // The Ruby generate command the scanner plans, and only it.
                // Without this arm the command was planned and then refused at
                // the gate. `rake` stays health-only: coverage has no reason
                // to invoke arbitrary project tasks.
                ManviActionKind::Coverage | ManviActionKind::CoverageGenerator => {
                    tool.as_str() == "rspec"
                }
            }),
            _ => false,
        },
        "dotnet" => match kind {
            ManviActionKind::Health => {
                command_is_one_of(args, &["add", "remove", "list", "restore", "test"])
            }
            ManviActionKind::Coverage | ManviActionKind::CoverageGenerator => {
                command_is_one_of(args, &["test"])
            }
        },
        "swift" => {
            matches!(
                kind,
                ManviActionKind::Coverage | ManviActionKind::CoverageGenerator
            ) && command_is_one_of(args, &["test"])
        }
        "dart" => {
            matches!(
                kind,
                ManviActionKind::Coverage | ManviActionKind::CoverageGenerator
            ) && command_is_one_of(args, &["test"])
        }
        // C/C++: configure and build an INSTRUMENTED tree in a directory
        // GitPulse owns, run its tests, and turn the gcov data into lcov.
        // Generation only — coverage analysis is read-only and never builds.
        "cmake" => kind == ManviActionKind::CoverageGenerator && cmake_coverage_allowed(args),
        "ctest" => kind == ManviActionKind::CoverageGenerator && ctest_coverage_allowed(args),
        "gcovr" => kind == ManviActionKind::CoverageGenerator && gcovr_coverage_allowed(args),
        "mvn" | "mvnw" | "./mvnw" | ".\\mvnw" | "gradle" | "gradlew" | "./gradlew"
        | ".\\gradlew" => build_tool_tasks_allowed(args, kind),
        "phpunit" => matches!(
            kind,
            ManviActionKind::Coverage | ManviActionKind::CoverageGenerator
        ),
        _ => false,
    };

    if !allowed {
        return Err(format!(
            "MANVI {} action refused: '{}' is outside the purpose-specific command allowlist",
            kind.label(),
            crate::harness::render_command(&args.iter().map(String::as_str).collect::<Vec<_>>())
        ));
    }
    Ok(())
}

/// Resolves every path-bearing option the supported tools can write or read.
/// Lexical checks above reject obvious `/` and `..` escapes; canonical
/// resolution here also refuses a repository symlink that points outside.
pub(crate) fn validate_manvi_paths(
    repo: &std::path::Path,
    args: &[String],
    kind: ManviActionKind,
) -> Result<(), String> {
    const NEXT_PATH_FLAGS: &[&str] = &[
        "--manifest-path",
        "--output-path",
        "--package-path",
        "--junitxml",
        "--cov-config",
        "--rcfile",
        "--requirement",
        "--constraint",
        // pytest exclusion appended when GitPulse recovers a collection abort.
        "--ignore",
        "-r",
        "-c",
        "-C",
        // CMake source/build trees and the ctest build directory. Belt and
        // braces: the allowlist already pins these to one literal directory,
        // and this proves that directory resolves inside the repository.
        "-S",
        "-B",
        "--build",
        "--test-dir",
    ];
    const PREFIX_PATH_FLAGS: &[&str] = &[
        "--manifest-path=",
        "--output-path=",
        "--package-path=",
        "--junitxml=",
        "--cov-config=",
        "--rcfile=",
        "--requirement=",
        "--constraint=",
        "--ignore=",
        "-coverprofile=",
        "--coverage=",
    ];

    let program_raw = args.first().map(String::as_str).unwrap_or_default();
    // Programs that are files in the repository rather than names on PATH:
    // build wrappers, the project virtualenv's interpreter, vendored phpunit.
    // The allowlist has already pinned the spelling; this proves the path
    // resolves to a real file that has not been symlinked out of the tree.
    if program_is_repo_local(program_raw) {
        let normalized = program_raw.replace('\\', "/");
        let relative = normalized.strip_prefix("./").unwrap_or(&normalized);
        // The virtualenv interpreter is validated on its own terms; every
        // other repository-local program must canonically resolve inside the
        // tree. Both fall through to the argument checks below.
        if repo_local_program(&normalized_program(program_raw)) == Some("python") {
            validate_venv_interpreter(repo, relative, kind)?;
        } else {
            let wrapper = sandbox_join_canonical(repo, relative).map_err(|e| {
                format!(
                    "MANVI {} action refused: wrapper '{}' escapes the open repository: {e}",
                    kind.label(),
                    program_raw
                )
            })?;
            if !wrapper.is_file() {
                return Err(format!(
                    "MANVI {} action refused: wrapper '{}' is not a repository file",
                    kind.label(),
                    program_raw
                ));
            }
        }
    }

    let mut path_values: Vec<&str> = Vec::new();
    let mut index = 1usize;
    while index < args.len() {
        let arg = &args[index];
        if NEXT_PATH_FLAGS.contains(&arg.as_str()) {
            let value = args.get(index + 1).ok_or_else(|| {
                format!(
                    "MANVI {} action refused: {arg} requires a repository-relative path",
                    kind.label()
                )
            })?;
            path_values.push(value);
            index += 2;
            continue;
        }
        if let Some(value) = PREFIX_PATH_FLAGS
            .iter()
            .find_map(|prefix| arg.strip_prefix(prefix))
        {
            path_values.push(value);
        }
        // pytest's `--cov-report=xml:path` carries a path after the format.
        if let Some(value) = arg.strip_prefix("--cov-report=") {
            if let Some((_, path)) = value.split_once(':') {
                path_values.push(path);
            }
        }
        index += 1;
    }

    for value in path_values {
        if value.trim().is_empty() {
            return Err(format!(
                "MANVI {} action refused: output/input path cannot be empty",
                kind.label()
            ));
        }
        sandbox_join_canonical(repo, value).map_err(|e| {
            format!(
                "MANVI {} action refused: path '{}' escapes the open repository: {e}",
                kind.label(),
                value
            )
        })?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::procguard::LockedSpawn;
    use base64::Engine;
    use tempfile::TempDir;

    const BASE64_STANDARD: base64::engine::general_purpose::GeneralPurpose =
        base64::engine::general_purpose::STANDARD;

    /// Somewhere to send output that the test does not look at. It is no view
    /// and holds no app handle, so a session outlives the `App` that spawned it.
    fn discard() -> OutputSink {
        OutputSink::channel(tauri::ipc::Channel::new(|_| Ok(())))
    }

    /// A Windows batch shim re-reads the command line through cmd.exe, so an
    /// argument that is safe for the program can still be a second command.
    #[test]
    fn a_batch_shim_refuses_what_cmd_would_read_as_syntax() {
        let shim = r"C:\Users\a\AppData\Roaming\npm\codex.cmd";
        let ok = |args: &[&str]| {
            batch_reparse_refusal(
                shim,
                &args.iter().map(|a| a.to_string()).collect::<Vec<_>>(),
            )
        };
        // What a task launch passes: flags, an absolute path, and a prompt
        // whose JSON-quoted brief path sits inside cmd's quoted region.
        assert_eq!(
            ok(&[
                "--cd",
                r"C:\code\app",
                "--",
                "Read the brief at \"C:\\tmp\\brief.md\" and do it."
            ]),
            None
        );
        assert_eq!(ok(&["--model", "opus[1m]", "--effort", "high"]), None);
        // Quoted as a whole and holding no quote, a metacharacter is text.
        assert_eq!(ok(&["fix the R&D build"]), None);
        // Unquoted: cmd runs `calc` after the agent.
        assert!(ok(&["fix&calc"]).is_some());
        assert!(ok(&["--cd", r"C:\code\R&D\app"]).is_some());
        assert!(ok(&["a|b"]).is_some() && ok(&["a>out"]).is_some() && ok(&["a^b"]).is_some());
        // An inner quote ends cmd's quoted region early: the JSON-quoted path
        // is exactly the part cmd reads unquoted.
        assert!(ok(&["Read the brief at \"C:\\R&D\\brief.md\" now"]).is_some());
        // Expanded wherever it stands, quoted or not.
        assert!(ok(&["see %PATH% here"]).is_some());
        // A line break ends the command cmd runs.
        assert!(ok(&["one\r\ntwo"]).is_some());
        // The shim's own path is read the same way.
        assert!(batch_reparse_refusal(r"C:\R&D\npm\codex.cmd", &[]).is_some());
        // Only batch files are re-read; an .exe gets its arguments as written.
        assert_eq!(
            batch_reparse_refusal(r"C:\bin\codex.exe", &["fix&calc".into()]),
            None
        );
        assert!(batch_reparse_refusal(r"C:\bin\CODEX.BAT", &["fix&calc".into()]).is_some());
        // cmd's quoting carries across arguments: an odd inner quote leaves it
        // inside, so the next argument's opening quote takes it out again.
        assert!(ok(&["a \"b", "x & calc"]).is_some());
    }

    /// Fuzzes the batch check against the whole command line, built by a port
    /// of portable-pty's `append_quoted` and read as cmd reads it.
    #[test]
    fn the_batch_check_agrees_with_cmd_over_the_whole_line() {
        fn append_quoted(arg: &str, line: &mut String) {
            if !arg.is_empty() && !arg.contains([' ', '\t', '\n', '\x0b', '"']) {
                line.push_str(arg);
                return;
            }
            line.push('"');
            let chars: Vec<char> = arg.chars().collect();
            let mut i = 0;
            while i < chars.len() {
                let mut slashes = 0;
                while i < chars.len() && chars[i] == '\\' {
                    i += 1;
                    slashes += 1;
                }
                if i == chars.len() {
                    line.push_str(&"\\".repeat(slashes * 2));
                    break;
                } else if chars[i] == '"' {
                    line.push_str(&"\\".repeat(slashes * 2 + 1));
                } else {
                    line.push_str(&"\\".repeat(slashes));
                }
                line.push(chars[i]);
                i += 1;
            }
            line.push('"');
        }
        fn cmd_acts_on(line: &str) -> bool {
            let mut inside = false;
            for c in line.chars() {
                match c {
                    '%' | '\r' | '\n' => return true,
                    '"' => inside = !inside,
                    '&' | '|' | '<' | '>' | '^' if !inside => return true,
                    _ => {}
                }
            }
            false
        }
        let pool = [
            'a', ' ', '"', '\\', '&', '|', '^', '<', '>', '%', '\t', 'z', '-',
        ];
        let mut seed = 0x9e37_79b9_7f4a_7c15_u64;
        let mut next = || {
            seed ^= seed << 13;
            seed ^= seed >> 7;
            seed ^= seed << 17;
            seed
        };
        let (mut refused, mut cases) = (0, 0);
        for _ in 0..20_000 {
            let args: Vec<String> = (0..(next() % 4))
                .map(|_| {
                    (0..(next() % 7))
                        .map(|_| pool[(next() % pool.len() as u64) as usize])
                        .collect()
                })
                .collect();
            let program = r"C:\npm\codex.cmd";
            let mut line = String::new();
            append_quoted(program, &mut line);
            for arg in &args {
                line.push(' ');
                append_quoted(arg, &mut line);
            }
            let verdict = batch_reparse_refusal(program, &args);
            assert_eq!(
                verdict.is_some(),
                cmd_acts_on(&line),
                "args {args:?} → line {line}"
            );
            cases += 1;
            refused += usize::from(verdict.is_some());
        }
        // Both answers were exercised, not one of them 20 000 times.
        assert!(
            refused > 1_000 && cases - refused > 1_000,
            "{refused} of {cases} refused"
        );
    }

    #[test]
    fn late_trust_revocation_records_a_failed_spawn_without_starting_a_child() {
        struct RevokeBeforeSpawn {
            repo: std::path::PathBuf,
            failed: std::sync::atomic::AtomicBool,
        }
        impl SessionObserver for RevokeBeforeSpawn {
            fn run_id(&self) -> &str {
                "revoked-launch"
            }
            fn before_spawn(&self, _: &str) -> Result<(), String> {
                crate::repository_trust::revoke(self.repo.to_str().unwrap())
            }
            fn started(&self, _: &str, _: Result<(u32, String), String>) -> Result<(), String> {
                panic!("revoked launch started")
            }
            fn spawn_failed(&self, _: &str, reason: &str) {
                assert!(reason.contains(crate::repository_trust::REQUIRED));
                self.failed.store(true, Ordering::SeqCst);
            }
            fn finished(&self, _: &TerminalExitPayload) {
                panic!("revoked launch acquired a child")
            }
        }
        let dir = tempfile::tempdir().unwrap();
        init_test_repo(dir.path());
        let app = tauri::test::mock_builder().build(crate::context()).unwrap();
        let state = TerminalSessions::default();
        let observer = Arc::new(RevokeBeforeSpawn {
            repo: dir.path().to_path_buf(),
            failed: std::sync::atomic::AtomicBool::new(false),
        });
        let result = spawn_session_inner(
            app.handle(),
            &state,
            dir.path().to_str().unwrap(),
            None,
            24,
            80,
            Some("/bin/sh".into()),
            Some(vec!["-c".into(), "touch revoked-launch-marker".into()]),
            None,
            discard(),
            Some(observer.clone()),
        );
        assert!(result
            .unwrap_err()
            .contains(crate::repository_trust::REQUIRED));
        assert!(!dir.path().join("revoked-launch-marker").exists());
        assert!(
            observer.failed.load(Ordering::SeqCst),
            "a consumed launch claim must receive its failure receipt"
        );
    }

    /// Under severe memory pressure an agent launch is refused before its PTY
    /// opens and before it takes a session slot, naming the binding resource
    /// and value. The user's own shell is never gated, and the launch is
    /// admitted again once the pressure clears.
    #[cfg(unix)]
    #[test]
    fn an_agent_pty_is_refused_under_pressure_a_shell_is_not_and_it_resumes() {
        use crate::sysload::GovernorInput;
        let dir = tempfile::tempdir().unwrap();
        init_test_repo(dir.path());
        let repo = dir.path().to_str().unwrap();
        let app = tauri::test::mock_builder().build(crate::context()).unwrap();
        let state = TerminalSessions::default();
        let marker = dir.path().join("agent-ran");
        let agent = |state: &TerminalSessions| {
            spawn_session_inner(
                app.handle(),
                state,
                repo,
                None,
                24,
                80,
                Some("/bin/sh".into()),
                Some(vec!["-c".into(), format!("touch '{}'", marker.display())]),
                None,
                discard(),
                None,
            )
        };

        let pressed = crate::sysload::inject_governor(GovernorInput::test_pressure(
            crate::sysload::Level::Critical,
        ));
        let refusal = agent(&state).unwrap_err();
        assert!(
            refusal.contains("Agent launch refused (pty_spawn)"),
            "{refusal}"
        );
        assert!(refusal.contains("critical (kernel level 4)"), "{refusal}");
        assert_eq!(
            state.active_sessions.load(Ordering::SeqCst),
            0,
            "a refused launch holds no session slot"
        );
        assert!(!marker.exists());

        // The user's own shell (no program) is not an agent launch.
        let shell = spawn_session_inner(
            app.handle(),
            &state,
            repo,
            None,
            24,
            80,
            None,
            None,
            None,
            discard(),
            None,
        )
        .expect("a plain shell is never refused");
        kill_session(&state, &shell.id).unwrap();
        drop(pressed);

        // Pressure cleared: the same launch is admitted and runs.
        let _calm = crate::sysload::inject_governor(GovernorInput::test_pressure(
            crate::sysload::Level::Normal,
        ));
        agent(&state).expect("admitted once the pressure clears");
        let deadline = std::time::Instant::now() + Duration::from_secs(20);
        while !marker.exists() && std::time::Instant::now() < deadline {
            thread::sleep(Duration::from_millis(20));
        }
        assert!(marker.exists(), "the admitted agent ran");
    }

    /// A task attempt's terminal carries its run id in `GITPULSE_RUN_ID`; a
    /// plain terminal carries none. Neither can be set by the IPC caller: a
    /// spoofed value in the caller's map is replaced or removed.
    #[cfg(unix)]
    #[test]
    fn a_task_terminal_carries_its_run_id_and_a_plain_one_none_whatever_the_caller_sent() {
        struct Run;
        impl SessionObserver for Run {
            fn run_id(&self) -> &str {
                "run-4f2a"
            }
            fn before_spawn(&self, _: &str) -> Result<(), String> {
                Ok(())
            }
            fn started(&self, _: &str, _: Result<(u32, String), String>) -> Result<(), String> {
                Ok(())
            }
            fn spawn_failed(&self, _: &str, reason: &str) {
                panic!("spawn failed: {reason}")
            }
            fn finished(&self, _: &TerminalExitPayload) {}
        }
        let dir = tempfile::tempdir().unwrap();
        init_test_repo(dir.path());
        let app = tauri::test::mock_builder().build(crate::context()).unwrap();
        let state = TerminalSessions::default();
        let spoof = HashMap::from([(
            crate::sysload::RUN_ID_ENV.to_string(),
            "run-spoofed".to_string(),
        )]);
        let seen = |marker: &str, observer: Option<Arc<dyn SessionObserver>>| {
            let path = dir.path().join(marker);
            spawn_session_inner(
                app.handle(),
                &state,
                dir.path().to_str().unwrap(),
                None,
                24,
                80,
                Some("/bin/sh".into()),
                Some(vec![
                    "-c".into(),
                    format!(
                        "printf '[%s]' \"${{{}-unset}}\" > {marker}.tmp && mv {marker}.tmp {marker}",
                        crate::sysload::RUN_ID_ENV
                    ),
                ]),
                Some(spoof.clone()),
                discard(),
                observer,
            )
            .unwrap();
            let deadline = std::time::Instant::now() + Duration::from_secs(10);
            while !path.exists() && std::time::Instant::now() < deadline {
                thread::sleep(Duration::from_millis(20));
            }
            std::fs::read_to_string(&path).unwrap()
        };
        assert_eq!(seen("task-marker", Some(Arc::new(Run))), "[run-4f2a]");
        assert_eq!(seen("plain-marker", None), "[unset]");
    }

    /// The defect: a webview reload left every PTY running with nobody to
    /// acknowledge its output. The first time one printed a window's worth,
    /// its reader stalled, the program blocked on a full tty, and 30 s later
    /// the session was killed as "renderer did not acknowledge output".
    #[cfg(unix)]
    #[test]
    fn a_session_whose_page_reloaded_keeps_running_and_can_be_shown_again() {
        let dir = tempfile::tempdir().unwrap();
        init_test_repo(dir.path());
        let app = tauri::test::mock_builder().build(crate::context()).unwrap();
        let state = TerminalSessions::default();
        let marker = dir.path().join("printed-past-the-window");
        // Prints more than the output window, which nothing acknowledges,
        // then leaves a marker and waits.
        let script = format!(
            "head -c {} /dev/zero | tr '\\000' a; : > '{}'; read finish",
            flow::OUTPUT_WINDOW * 3,
            marker.display()
        );
        let spawned = spawn_session_inner(
            app.handle(),
            &state,
            dir.path().to_str().unwrap(),
            None,
            24,
            80,
            Some("/bin/sh".into()),
            Some(vec!["-c".into(), script]),
            None,
            discard(),
            None,
        )
        .unwrap();
        let wait_for = |done: &dyn Fn() -> bool, within: Duration| {
            let deadline = std::time::Instant::now() + within;
            while !done() && std::time::Instant::now() < deadline {
                thread::sleep(Duration::from_millis(20));
            }
            done()
        };
        // Attached and unacknowledged, the program is held back by the window.
        assert!(!wait_for(&|| marker.exists(), Duration::from_millis(1500)));
        assert!(!list_sessions(&state).unwrap()[0].detached);
        assert_eq!(
            attach_session(&state, &spawned.id, 24, 80, discard()).unwrap_err(),
            "This terminal is already shown in a tab."
        );

        assert_eq!(detach_all(&state), 1);
        assert!(
            wait_for(&|| marker.exists(), Duration::from_secs(10)),
            "a detached session must keep running past its output window"
        );
        let listed = list_sessions(&state).unwrap();
        assert_eq!(listed.len(), 1);
        assert_eq!(listed[0].id, spawned.id);
        assert!(listed[0].detached);
        assert_eq!(listed[0].launcher, None);
        assert_eq!(listed[0].run_id, None);

        // S2-11: what was printed while nobody held the session is not lost
        // to the next view. It is shown the ring's tail first — here all
        // `a`s, the newest window of the 1.5 MiB the program printed — and,
        // because more was printed while detached than the ring keeps, told
        // that the rest was not kept.
        let losses: Arc<Mutex<Vec<serde_json::Value>>> = Arc::default();
        {
            use tauri::Listener;
            let losses = losses.clone();
            app.listen_any("terminal-loss", move |event| {
                losses
                    .lock()
                    .unwrap()
                    .push(serde_json::from_str(event.payload()).unwrap());
            });
        }
        let (view, shown_bytes) = recording();
        let shown = attach_session(&state, &spawned.id, 30, 100, view).unwrap();
        assert_eq!(shown.id, spawned.id);
        let replayed = shown_bytes.lock().unwrap().concat();
        assert_eq!(replayed.len(), RING_BYTES, "the whole ring is replayed");
        assert!(replayed.iter().all(|b| *b == b'a'));
        let losses = losses.lock().unwrap();
        assert_eq!(losses.len(), 1, "{losses:?}");
        assert_eq!(losses[0]["id"], spawned.id.as_str());
        assert!(losses[0]["message"]
            .as_str()
            .unwrap()
            .starts_with("Output while detached was not kept"));
        drop(losses);
        assert!(!list_sessions(&state).unwrap()[0].detached);
        assert!(attach_session(&state, &spawned.id, 30, 100, discard()).is_err());
        assert!(attach_session(&state, "term-missing", 30, 100, discard()).is_err());

        kill_session(&state, &spawned.id).unwrap();
        assert!(list_sessions(&state).unwrap().is_empty());
        assert_eq!(detach_all(&state), 0);
    }

    /// S2-11's other half: a program that never repaints (a shell prompt, a
    /// test run) printing a little while detached comes back with exactly
    /// that, in order, and with no "not kept" notice, because nothing was lost.
    #[cfg(unix)]
    #[test]
    fn output_printed_while_detached_is_replayed_to_the_next_view() {
        let dir = tempfile::tempdir().unwrap();
        init_test_repo(dir.path());
        let app = tauri::test::mock_builder().build(crate::context()).unwrap();
        let state = TerminalSessions::default();
        let go = dir.path().join("go");
        let printed = dir.path().join("printed");
        let script = format!(
            "printf 'before\\n'; while [ ! -e '{go}' ]; do sleep 0.02; done; \
             printf 'while-away-1\\nwhile-away-2\\n'; : > '{printed}'; read finish",
            go = go.display(),
            printed = printed.display(),
        );
        let (first, first_bytes) = recording();
        let spawned = spawn_session_inner(
            app.handle(),
            &state,
            dir.path().to_str().unwrap(),
            None,
            24,
            80,
            Some("/bin/sh".into()),
            Some(vec!["-c".into(), script]),
            None,
            first,
            None,
        )
        .unwrap();
        let wait_for = |done: &dyn Fn() -> bool| {
            let deadline = std::time::Instant::now() + Duration::from_secs(10);
            while !done() && std::time::Instant::now() < deadline {
                thread::sleep(Duration::from_millis(20));
            }
            done()
        };
        let seen = |log: &Arc<Mutex<Vec<Vec<u8>>>>| {
            String::from_utf8_lossy(&log.lock().unwrap().concat()).into_owned()
        };
        assert!(wait_for(&|| seen(&first_bytes).contains("before")));
        assert!(detach_session(&state, &spawned.id).unwrap());
        std::fs::write(&go, b"").unwrap();
        assert!(wait_for(&|| printed.exists()));
        // Read past the marker's creation: the reader may not have kept the
        // last line yet when the file appears.
        thread::sleep(Duration::from_millis(200));
        assert!(
            !seen(&first_bytes).contains("while-away"),
            "the view that went away was sent nothing more"
        );

        let (second, second_bytes) = recording();
        attach_session(&state, &spawned.id, 24, 80, second).unwrap();
        let replay = seen(&second_bytes);
        let at = |needle: &str| {
            replay
                .find(needle)
                .unwrap_or_else(|| panic!("{needle:?} in {replay:?}"))
        };
        assert!(at("before") < at("while-away-1") && at("while-away-1") < at("while-away-2"));
        kill_session(&state, &spawned.id).unwrap();
    }

    /// A sink whose messages are kept, raw, in order.
    fn recording() -> (OutputSink, Arc<Mutex<Vec<Vec<u8>>>>) {
        let log: Arc<Mutex<Vec<Vec<u8>>>> = Arc::default();
        let into = log.clone();
        let sink = OutputSink::channel(tauri::ipc::Channel::new(move |body| {
            if let tauri::ipc::InvokeResponseBody::Raw(bytes) = body {
                into.lock().unwrap().push(bytes);
            }
            Ok(())
        }));
        (sink, log)
    }

    /// An observer that panics when told the process ended, standing for any
    /// bookkeeping the end of a session runs before it lets go of the entry.
    #[cfg(unix)]
    struct PanicsOnFinish;
    #[cfg(unix)]
    impl SessionObserver for PanicsOnFinish {
        fn run_id(&self) -> &str {
            "run-panics"
        }
        fn before_spawn(&self, _: &str) -> Result<(), String> {
            Ok(())
        }
        fn started(&self, _: &str, _: Result<(u32, String), String>) -> Result<(), String> {
            Ok(())
        }
        fn spawn_failed(&self, _: &str, reason: &str) {
            panic!("spawn failed: {reason}")
        }
        fn finished(&self, _: &TerminalExitPayload) {
            panic!("injected panic in the end-of-session bookkeeping");
        }
    }

    /// The defect: a panic in the reader thread's tail left the session in the
    /// map for good - listed as live and never announced as exited - because
    /// only the tail removed it. The frontend then showed a terminal with no
    /// process behind it.
    #[cfg(unix)]
    #[test]
    fn a_panic_ending_a_session_still_removes_it_and_announces_the_exit() {
        use tauri::Listener;
        let dir = tempfile::tempdir().unwrap();
        init_test_repo(dir.path());
        let app = tauri::test::mock_builder().build(crate::context()).unwrap();
        let state = TerminalSessions::default();
        let exited = Arc::new(AtomicBool::new(false));
        let flag = exited.clone();
        app.listen("terminal-exit", move |_| flag.store(true, Ordering::SeqCst));
        spawn_session_inner(
            app.handle(),
            &state,
            dir.path().to_str().unwrap(),
            None,
            24,
            80,
            Some("/bin/sh".into()),
            Some(vec!["-c".into(), "exit 0".into()]),
            None,
            discard(),
            Some(Arc::new(PanicsOnFinish)),
        )
        .unwrap();
        assert!(
            wait_until(Duration::from_secs(10), || list_sessions(&state)
                .unwrap()
                .is_empty()),
            "a session whose end-of-session bookkeeping panicked stayed in the map"
        );
        assert!(
            wait_until(Duration::from_secs(5), || exited.load(Ordering::SeqCst)),
            "the view is told the session ended"
        );
        assert_eq!(state.active_sessions.load(Ordering::SeqCst), 0);
    }

    /// The defect: a panic while reading output (the scanner, an event
    /// listener) ended the reader thread with the child still alive and
    /// nothing reading its tty. The session stayed "running" forever and its
    /// output backed up until the program blocked. The reader now contains the
    /// panic, stops the child, and takes the normal exit path.
    #[cfg(unix)]
    #[test]
    fn a_panic_while_reading_output_ends_the_session_instead_of_orphaning_it() {
        use tauri::Listener;
        let dir = tempfile::tempdir().unwrap();
        init_test_repo(dir.path());
        let app = tauri::test::mock_builder().build(crate::context()).unwrap();
        let state = TerminalSessions::default();
        app.listen("terminal-output", |_| {
            panic!("injected panic while delivering output");
        });
        let pid_file = dir.path().join("reader-panic.pid");
        spawn_session_inner(
            app.handle(),
            &state,
            dir.path().to_str().unwrap(),
            None,
            24,
            80,
            Some("/bin/sh".into()),
            Some(vec![
                "-c".into(),
                format!("echo $$ > '{}'; echo hello; sleep 300", pid_file.display()),
            ]),
            None,
            OutputSink::events(app.handle()),
            None,
        )
        .unwrap();
        let pid = read_pid(&pid_file);
        // The listener's panic poisons the event bus, so the exit event
        // itself cannot be announced here; what must hold is that the process
        // does not outlive its reader.
        assert!(
            wait_until(Duration::from_secs(15), || list_sessions(&state)
                .unwrap()
                .is_empty()),
            "the reader panicked and the session was left running"
        );
        assert!(
            wait_until(Duration::from_secs(5), || unsafe { libc::kill(pid, 0) }
                != 0),
            "the child of a reader that panicked was left running"
        );
    }

    /// The defect: `resize_session` held the whole session map while it waited
    /// for one session's master lock, so one busy master stalled list, write,
    /// kill and attach for every other session.
    #[cfg(unix)]
    #[test]
    fn a_resize_waiting_on_one_master_does_not_hold_the_session_map() {
        let dir = tempfile::tempdir().unwrap();
        let state = TerminalSessions::default();
        let spawned = spawn_script(&state, dir.path(), "sleep 300".into());
        let master = state
            .lock_sessions()
            .get(&spawned.id)
            .unwrap()
            .master
            .clone();
        let held = master.lock().unwrap();
        let resizer = {
            let state = state.clone();
            let id = spawned.id.clone();
            thread::spawn(move || resize_session(&state, &id, 30, 100))
        };
        thread::sleep(Duration::from_millis(200));
        let (send, receive) = std::sync::mpsc::channel();
        let lister = state.clone();
        thread::spawn(move || send.send(list_sessions(&lister).unwrap().len()));
        let listed = receive.recv_timeout(Duration::from_secs(2));
        drop(held);
        resizer.join().unwrap().unwrap();
        assert_eq!(
            listed.expect("list_sessions was stuck behind another session's resize"),
            1
        );
        kill_session(&state, &spawned.id).unwrap();
    }

    /// The defect: a poisoned master lock made every later resize fail with
    /// "Terminal size lock error", so the PTY kept its old size for good.
    #[cfg(unix)]
    #[test]
    fn a_poisoned_master_lock_does_not_stop_resizing() {
        let dir = tempfile::tempdir().unwrap();
        let state = TerminalSessions::default();
        let spawned = spawn_script(&state, dir.path(), "sleep 300".into());
        let master = state
            .lock_sessions()
            .get(&spawned.id)
            .unwrap()
            .master
            .clone();
        let poisoner = master.clone();
        let _ = thread::spawn(move || {
            let _held = poisoner.lock().unwrap();
            panic!("injected panic under the master lock");
        })
        .join();
        assert!(
            master.is_poisoned(),
            "positive control: the lock is poisoned"
        );
        resize_session(&state, &spawned.id, 40, 120).unwrap();
        let size = master
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .get_size()
            .unwrap();
        assert_eq!((size.rows, size.cols), (40, 120));
        kill_session(&state, &spawned.id).unwrap();
    }

    /// The native smoke test the browser harness cannot be: it executes a real
    /// command. Bytes typed into the session go through a real `openpty` pair to
    /// a real `/bin/sh`, and what the shell prints comes back out of the master
    /// as a `terminal-output` event, followed by `terminal-exit`. Every other
    /// test here asserts argv or a side-effect file, so a PTY that opened and
    /// spawned but never delivered output would pass them all.
    #[cfg(unix)]
    #[test]
    fn a_real_shell_command_round_trips_through_the_pty() {
        use tauri::Listener;
        let dir = tempfile::tempdir().unwrap();
        init_test_repo(dir.path());
        let app = tauri::test::mock_builder().build(crate::context()).unwrap();
        let state = TerminalSessions::default();
        let output = Arc::new(Mutex::new(Vec::<u8>::new()));
        let sink = output.clone();
        app.listen("terminal-output", move |event| {
            let payload: serde_json::Value = serde_json::from_str(event.payload()).unwrap();
            let bytes = BASE64_STANDARD
                .decode(payload["data_b64"].as_str().unwrap())
                .unwrap();
            sink.lock().unwrap().extend_from_slice(&bytes);
        });
        let exited = Arc::new(AtomicBool::new(false));
        let exit_flag = exited.clone();
        app.listen("terminal-exit", move |_| {
            exit_flag.store(true, Ordering::SeqCst)
        });

        let spawned = spawn_session_inner(
            app.handle(),
            &state,
            dir.path().to_str().unwrap(),
            None,
            24,
            80,
            Some("/bin/sh".into()),
            Some(vec![
                "-c".into(),
                "read word; printf 'round-trip:%s:%s\\n' \"$word\" $((6 * 7))".into(),
            ]),
            None,
            OutputSink::events(app.handle()),
            None,
        )
        .unwrap();
        // The arithmetic is evaluated by the shell, so the expected text cannot
        // be the tty merely echoing the typed input back.
        write_to_session(&state, &spawned.id, "ping\n").unwrap();
        assert!(
            wait_until(Duration::from_secs(15), || {
                String::from_utf8_lossy(&output.lock().unwrap()).contains("round-trip:ping:42")
            }),
            "shell output never came back through the PTY: {:?}",
            String::from_utf8_lossy(&output.lock().unwrap())
        );
        assert!(
            wait_until(Duration::from_secs(15), || exited.load(Ordering::SeqCst)),
            "terminal-exit was never emitted after the shell finished"
        );
        let _ = kill_session(&state, &spawned.id);
    }

    #[cfg(unix)]
    fn alive(pid: i32) -> bool {
        // SAFETY: signal 0 only probes.
        unsafe { libc::kill(pid, 0) == 0 }
    }

    #[cfg(unix)]
    fn wait_until(within: Duration, done: impl Fn() -> bool) -> bool {
        let deadline = Instant::now() + within;
        while !done() && Instant::now() < deadline {
            thread::sleep(Duration::from_millis(20));
        }
        done()
    }

    /// Spawns `/bin/sh -c script` in a trusted scratch repo.
    #[cfg(unix)]
    fn spawn_script(
        state: &TerminalSessions,
        dir: &std::path::Path,
        script: String,
    ) -> TerminalSpawned {
        init_test_repo(dir);
        let app = tauri::test::mock_builder().build(crate::context()).unwrap();
        let spawned = spawn_session_inner(
            app.handle(),
            state,
            dir.to_str().unwrap(),
            None,
            24,
            80,
            Some("/bin/sh".into()),
            Some(vec!["-c".into(), script]),
            None,
            discard(),
            None,
        )
        .unwrap();
        // The app handle must outlive nothing here: the session owns what it needs.
        drop(app);
        spawned
    }

    #[cfg(unix)]
    fn read_pid(file: &std::path::Path) -> i32 {
        let deadline = Instant::now() + Duration::from_secs(5);
        while Instant::now() < deadline {
            if let Ok(text) = std::fs::read_to_string(file) {
                if let Ok(pid) = text.trim().parse() {
                    return pid;
                }
            }
            thread::sleep(Duration::from_millis(20));
        }
        panic!("{} was never written", file.display());
    }

    /// The defect: every session's reader woke ten times a second for as long
    /// as the session lived, to ask "has the shell exited?" and "is there
    /// output?" of a kernel that would have said when either became true.
    /// One idle session was ~860k wakeups a day, and the wakeups kept the
    /// machine out of App Nap.
    #[cfg(unix)]
    #[test]
    fn an_idle_session_does_not_wake_its_reader() {
        let dir = tempfile::tempdir().unwrap();
        let state = TerminalSessions::default();
        let spawned = spawn_script(&state, dir.path(), "echo ready; exec cat".into());
        // Let the shell start and print before the window opens.
        thread::sleep(Duration::from_millis(700));
        let before = reader_wakeups(&state, &spawned.id).expect("open session");
        thread::sleep(Duration::from_millis(1500));
        let after = reader_wakeups(&state, &spawned.id).expect("open session");
        kill_session(&state, &spawned.id).unwrap();
        assert!(
            after - before <= 1,
            "the reader woke {} times in 1.5 s of idleness",
            after - before
        );
    }

    /// The view says which sessions are on a tab nobody is looking at; the rest,
    /// and any session it has never mentioned, stay at full rate. The list is
    /// the whole truth each time.
    #[cfg(unix)]
    #[test]
    fn only_the_sessions_the_view_names_are_batched_harder() {
        let dir = tempfile::tempdir().unwrap();
        let state = TerminalSessions::default();
        let a = spawn_script(&state, dir.path(), "exec cat".into());
        let b = spawn_script(&state, dir.path(), "exec cat".into());
        let hidden = |id: &str| {
            state
                .lock_sessions()
                .get(id)
                .unwrap()
                .hidden
                .load(Ordering::Relaxed)
        };
        assert!(!hidden(&a.id) && !hidden(&b.id), "unreported is seen");
        set_background_sessions(&state, &[a.id.clone(), "term-unknown".into()]);
        assert!(hidden(&a.id) && !hidden(&b.id));
        set_background_sessions(&state, std::slice::from_ref(&b.id));
        assert!(!hidden(&a.id) && hidden(&b.id));
        set_background_sessions(&state, &[]);
        assert!(!hidden(&a.id) && !hidden(&b.id));
        kill_session(&state, &a.id).unwrap();
        kill_session(&state, &b.id).unwrap();
    }

    /// A page that is gone (or whose event loop refused) is the view's trouble:
    /// a failed send sheds, says so, and the process keeps running.
    #[cfg(unix)]
    #[test]
    fn a_view_whose_channel_fails_is_told_and_the_process_keeps_running() {
        use tauri::Listener;
        let dir = tempfile::tempdir().unwrap();
        init_test_repo(dir.path());
        let app = tauri::test::mock_builder().build(crate::context()).unwrap();
        let state = TerminalSessions::default();
        let losses = Arc::new(AtomicUsize::new(0));
        let counted = losses.clone();
        app.listen("terminal-loss", move |_| {
            counted.fetch_add(1, Ordering::SeqCst);
        });
        let failing = OutputSink::channel(tauri::ipc::Channel::new(|_| {
            Err(tauri::Error::WebviewNotFound)
        }));
        let spawned = spawn_session_inner(
            app.handle(),
            &state,
            dir.path().to_str().unwrap(),
            None,
            24,
            80,
            Some("/bin/sh".into()),
            Some(vec![
                "-c".into(),
                "stty raw -echo; echo one; sleep 0.3; echo two; sleep 0.3; echo three; exec sleep 30"
                    .into(),
            ]),
            None,
            failing,
            None,
        )
        .unwrap();
        assert!(wait_until(Duration::from_secs(10), || losses
            .load(Ordering::SeqCst)
            >= 1));
        thread::sleep(Duration::from_millis(800));
        assert_eq!(
            list_sessions(&state).unwrap().len(),
            1,
            "the session kept running"
        );
        assert!(!list_sessions(&state).unwrap()[0].ending);
        kill_session(&state, &spawned.id).unwrap();
    }

    /// The defect: Close signalled the tty's one foreground group and then the
    /// shell, so a job in any other group (`cmd &` under job control, a dev
    /// server, a `^Z`'d editor) outlived the tab in the lane's worktree.
    #[cfg(unix)]
    #[test]
    fn closing_a_terminal_stops_jobs_in_every_group_of_its_session() {
        let dir = tempfile::tempdir().unwrap();
        let state = TerminalSessions::default();
        let pids = dir.path().join("pids");
        let spawned = spawn_script(
            &state,
            dir.path(),
            format!(
                "set -m; sleep 300 & echo $! > '{p}.a'; sleep 301 & echo $! > '{p}.b'; wait",
                p = pids.display()
            ),
        );
        let a = read_pid(&dir.path().join("pids.a"));
        let b = read_pid(&dir.path().join("pids.b"));
        assert!(alive(a) && alive(b));
        kill_session(&state, &spawned.id).unwrap();
        assert!(
            wait_until(Duration::from_secs(3), || !alive(a) && !alive(b)),
            "a background job survived Close (a alive: {}, b alive: {})",
            alive(a),
            alive(b)
        );
    }

    /// A job that ignores the hangup is killed after the grace, not left.
    #[cfg(unix)]
    #[test]
    fn a_job_that_ignores_the_hangup_is_killed_after_the_grace() {
        let dir = tempfile::tempdir().unwrap();
        let state = TerminalSessions::default();
        let spawned = spawn_script(
            &state,
            dir.path(),
            format!(
                "set -m; (trap '' HUP; exec sleep 300) & echo $! > '{}'; wait",
                dir.path().join("stubborn").display()
            ),
        );
        let pid = read_pid(&dir.path().join("stubborn"));
        kill_session(&state, &spawned.id).unwrap();
        assert!(wait_until(Duration::from_secs(1), || !alive(pid)));
    }

    /// An agent's SessionEnd runs on the hangup; the grace has to be long
    /// enough for it, so it is pinned and exercised: a job that cleans up for
    /// 1.2 s after SIGHUP finishes before it is killed.
    #[cfg(unix)]
    #[test]
    fn a_job_gets_the_hangup_grace_to_clean_up() {
        assert!(HANGUP_GRACE >= Duration::from_secs(2));
        let dir = tempfile::tempdir().unwrap();
        let state = TerminalSessions::default();
        let done = dir.path().join("cleaned-up");
        let spawned = spawn_script(
            &state,
            dir.path(),
            format!(
                "set -m; (trap 'sleep 1.2; : > \'{done}\'; exit 0' HUP; : > \'{ready}\'; while :; do sleep 0.05; done) & wait",
                done = done.display(),
                ready = dir.path().join("ready").display()
            ),
        );
        assert!(wait_until(Duration::from_secs(5), || dir
            .path()
            .join("ready")
            .exists()));
        kill_session(&state, &spawned.id).unwrap();
        assert!(
            done.exists(),
            "the job was killed before its cleanup finished"
        );
    }

    /// The shell's pid, the id of the session it leads.
    #[cfg(unix)]
    fn leader_pid(state: &TerminalSessions, id: &str) -> i32 {
        let child = state
            .lock_sessions()
            .get(id)
            .map(|s| s.child.clone())
            .expect("open session");
        let pid = child.lock().unwrap().process_id().expect("a running shell");
        i32::try_from(pid).unwrap()
    }

    /// The handles `kill_session` takes, so a test can run a close by hand.
    #[cfg(unix)]
    #[allow(clippy::type_complexity)]
    fn close_handles(
        state: &TerminalSessions,
        id: &str,
    ) -> (
        Arc<Mutex<Box<dyn Child + Send + Sync>>>,
        Arc<Mutex<Box<dyn MasterPty + Send>>>,
        Arc<AtomicUsize>,
    ) {
        state
            .lock_sessions()
            .get(id)
            .map(|s| (s.child.clone(), s.master.clone(), s.closing.clone()))
            .expect("open session")
    }

    /// A close with a chosen grace, the way `kill_session` runs it.
    #[cfg(unix)]
    fn close_for_test(
        child: &Arc<Mutex<Box<dyn Child + Send + Sync>>>,
        master: &Arc<Mutex<Box<dyn MasterPty + Send>>>,
        closing: &Arc<AtomicUsize>,
        grace: Duration,
    ) -> Result<(), String> {
        terminate_pty_child_within(child, master, closing, grace)
    }

    /// The signal target set. A process that left the terminal's session
    /// (`setsid`) is not that terminal's job any more and a close does not
    /// reach it, however it was started; every group still in the session
    /// does. Neither the wait nor its bookkeeping changes who is signalled.
    #[cfg(unix)]
    #[test]
    fn a_close_signals_the_groups_of_its_session_and_nobody_outside_it() {
        let dir = tempfile::tempdir().unwrap();
        let state = TerminalSessions::default();
        let spawned = spawn_script(
            &state,
            dir.path(),
            format!(
                "perl -e 'use POSIX; POSIX::setsid() or die; exec \"sleep\", \"302\"' & echo $! > '{p}.escapee'; \
                 set -m; sleep 300 & echo $! > '{p}.member'; sleep 301 & echo $! > '{p}.other'; wait",
                p = dir.path().join("pids").display()
            ),
        );
        let escapee = read_pid(&dir.path().join("pids.escapee"));
        let member = read_pid(&dir.path().join("pids.member"));
        let other = read_pid(&dir.path().join("pids.other"));
        // SAFETY: getsid reads one numeric id.
        let escaped = wait_until(Duration::from_secs(5), || unsafe {
            libc::getsid(escapee) == escapee
        });
        assert!(escaped, "the test's escapee never left the session");
        kill_session(&state, &spawned.id).unwrap();
        let stopped = wait_until(Duration::from_secs(3), || !alive(member) && !alive(other));
        let escapee_survived = alive(escapee);
        // SAFETY: the test started this process and holds its pid.
        unsafe { libc::kill(escapee, libc::SIGKILL) };
        assert!(stopped, "a job in the session survived Close");
        assert!(
            escapee_survived,
            "a process outside the session was signalled"
        );
    }

    /// The defect: the wait for the jobs to go was a 20 ms loop that read the
    /// whole process table each lap, so a cleanup that takes 0.8 s read it
    /// ~40 times, per closed terminal, and a quit with a full dock did that
    /// for every one at once. The wait now sleeps on the jobs' exit.
    #[cfg(unix)]
    #[test]
    fn waiting_for_jobs_to_clean_up_does_not_poll_the_process_table() {
        let dir = tempfile::tempdir().unwrap();
        let state = TerminalSessions::default();
        let ready = dir.path().join("ready");
        let spawned = spawn_script(
            &state,
            dir.path(),
            format!(
                "set -m; (trap 'sleep 0.8; exit 0' HUP; : > '{ready}'; while :; do sleep 0.05; done) & wait",
                ready = ready.display()
            ),
        );
        assert!(wait_until(Duration::from_secs(5), || ready.exists()));
        let sid = leader_pid(&state, &spawned.id);
        let scans = AtomicUsize::new(0);
        let started = Instant::now();
        jobs::stop_with(sid, HANGUP_GRACE, &|session| {
            scans.fetch_add(1, Ordering::SeqCst);
            foreground::session_members(session)
        });
        let waited = started.elapsed();
        let scans = scans.load(Ordering::SeqCst);
        println!("cleanup took {waited:?}, {scans} reads of the process table");
        assert!(
            waited < HANGUP_GRACE,
            "the job cleaned up on its own, so nothing waits out the grace ({waited:?})"
        );
        assert!(
            scans <= 4,
            "{scans} reads of the process table for a 0.8 s cleanup"
        );
        kill_session(&state, &spawned.id).unwrap();
    }

    /// A close that obeys the hangup is not slowed by the way it waits: the
    /// jobs are gone in a few milliseconds and so is the wait.
    #[cfg(unix)]
    #[test]
    fn a_job_that_obeys_the_hangup_ends_the_wait_at_once() {
        let dir = tempfile::tempdir().unwrap();
        let state = TerminalSessions::default();
        let pids = dir.path().join("pids");
        let spawned = spawn_script(
            &state,
            dir.path(),
            format!(
                "set -m; sleep 300 & echo $! > '{p}.a'; sleep 301 & echo $! > '{p}.b'; wait",
                p = pids.display()
            ),
        );
        let a = read_pid(&dir.path().join("pids.a"));
        let b = read_pid(&dir.path().join("pids.b"));
        let sid = leader_pid(&state, &spawned.id);
        let scans = AtomicUsize::new(0);
        let started = Instant::now();
        jobs::stop_with(sid, HANGUP_GRACE, &|session| {
            scans.fetch_add(1, Ordering::SeqCst);
            foreground::session_members(session)
        });
        let waited = started.elapsed();
        let scans = scans.load(Ordering::SeqCst);
        println!("hangup obeyed in {waited:?}, {scans} reads of the process table");
        assert!(
            waited < crate::test_support::coverage_relaxed(Duration::from_millis(50)),
            "{waited:?} for jobs that exit on the hangup"
        );
        assert!(scans <= 3, "{scans} reads of the process table");
        // The shell reaps them in its own time; until it does they are zombies
        // that still answer a probe.
        assert!(wait_until(Duration::from_secs(2), || !alive(a) && !alive(b)));
        kill_session(&state, &spawned.id).unwrap();
    }

    /// A job that ignores the hangup is escalated at the end of the grace, not
    /// before it and not later.
    #[cfg(unix)]
    #[test]
    fn a_job_that_ignores_the_hangup_is_killed_when_the_grace_ends() {
        let dir = tempfile::tempdir().unwrap();
        let state = TerminalSessions::default();
        let spawned = spawn_script(
            &state,
            dir.path(),
            format!(
                "set -m; (trap '' HUP; exec sleep 300) & echo $! > '{}'; wait",
                dir.path().join("stubborn").display()
            ),
        );
        let pid = read_pid(&dir.path().join("stubborn"));
        let sid = leader_pid(&state, &spawned.id);
        let grace = Duration::from_millis(600);
        let started = Instant::now();
        jobs::stop_with(sid, grace, &foreground::session_members);
        let waited = started.elapsed();
        assert!(
            waited >= grace,
            "escalated after {waited:?}, before the {grace:?} grace"
        );
        assert!(
            waited < grace + crate::test_support::coverage_relaxed(Duration::from_millis(500)),
            "{waited:?} for a {grace:?} grace"
        );
        assert!(wait_until(Duration::from_secs(1), || !alive(pid)));
        kill_session(&state, &spawned.id).unwrap();
    }

    /// The defect: the child lock was held across the whole grace, so anything
    /// that needed the shell (the unmount check's context read, a second
    /// close) waited out up to 2.5 s behind a stubborn job.
    #[cfg(unix)]
    #[test]
    fn a_close_waiting_on_a_stubborn_job_does_not_hold_the_shells_lock() {
        let dir = tempfile::tempdir().unwrap();
        let state = TerminalSessions::default();
        let ready = dir.path().join("ready");
        let spawned = spawn_script(
            &state,
            dir.path(),
            format!(
                "set -m; (trap '' HUP; : > '{ready}'; exec sleep 300) & echo $! > '{pid}'; wait",
                ready = ready.display(),
                pid = dir.path().join("stubborn").display()
            ),
        );
        assert!(wait_until(Duration::from_secs(5), || ready.exists()));
        let pid = read_pid(&dir.path().join("stubborn"));
        let (child, master, closing) = close_handles(&state, &spawned.id);
        let grace = Duration::from_millis(1500);
        let closing = {
            let (child, master, closing) = (child.clone(), master.clone(), closing.clone());
            thread::spawn(move || close_for_test(&child, &master, &closing, grace))
        };
        // Past the hangup, well inside the grace.
        thread::sleep(Duration::from_millis(300));
        let started = Instant::now();
        let free = child.try_lock().is_ok();
        let waited = started.elapsed();
        let context = session_context(&state, &spawned.id);
        let answered = started.elapsed();
        closing.join().unwrap().unwrap();
        println!("lock free: {free}; try_lock {waited:?}; context {answered:?} ({context:?})");
        assert!(free, "the shell's lock was held through the grace");
        assert!(
            matches!(&context, Ok(read) if read.jobs.is_none() && read.busy.is_none()),
            "a terminal being closed reports nothing known, not a guess: {context:?}"
        );
        assert!(
            answered < crate::test_support::coverage_relaxed(Duration::from_millis(300)),
            "a context read waited {answered:?} behind a close"
        );
        assert!(wait_until(Duration::from_secs(2), || !alive(pid)));
        kill_session(&state, &spawned.id).unwrap();
    }

    /// The process state letter `ps` reports for `pid`, empty when it is gone.
    #[cfg(unix)]
    fn process_state(pid: i32) -> String {
        let out = std::process::Command::new("ps")
            .args(["-o", "stat=", "-p", &pid.to_string()])
            .output_locked()
            .expect("ps");
        String::from_utf8_lossy(&out.stdout).trim().to_owned()
    }

    /// While a close waits out its grace the shell stays unreaped, even after
    /// it has died: the closer is signalling a session its pid still names.
    /// The closer used to guarantee that by holding the shell's lock, which
    /// kept the reader from reaping; now the close itself holds the reader
    /// back. A reaped leader frees its pid for a stranger.
    #[cfg(unix)]
    #[test]
    fn the_reader_does_not_reap_a_shell_that_dies_while_its_close_waits() {
        let dir = tempfile::tempdir().unwrap();
        let state = TerminalSessions::default();
        let ready = dir.path().join("ready");
        let spawned = spawn_script(
            &state,
            dir.path(),
            format!(
                "sh=$$; set -m; (trap \"kill -9 $sh; exit 0\" HUP; : > '{ready}'; while :; do sleep 0.05; done) & \
                 (trap '' HUP; exec sleep 300) & echo $! > '{stubborn}'; wait",
                ready = ready.display(),
                stubborn = dir.path().join("stubborn").display()
            ),
        );
        assert!(wait_until(Duration::from_secs(5), || ready.exists()));
        let stubborn = read_pid(&dir.path().join("stubborn"));
        let sid = leader_pid(&state, &spawned.id);
        let (child, master, closing) = close_handles(&state, &spawned.id);
        let closer = {
            let (child, master, closing) = (child.clone(), master.clone(), closing.clone());
            thread::spawn(move || {
                close_for_test(&child, &master, &closing, Duration::from_millis(1500))
            })
        };
        // The first job's hangup killed the shell; the second still runs, so
        // the close is still inside its grace.
        thread::sleep(Duration::from_millis(600));
        let state_of_shell = process_state(sid);
        let stubborn_alive = alive(stubborn);
        closer.join().unwrap().unwrap();
        assert!(stubborn_alive, "the close ended before its grace");
        assert!(
            state_of_shell.starts_with('Z'),
            "the shell should be dead and unreaped during its close, but ps says {state_of_shell:?}"
        );
        // Once the close is over the reader reaps it and the session ends.
        assert!(
            wait_until(Duration::from_secs(5), || list_sessions(&state)
                .unwrap()
                .is_empty()),
            "the session never ended after its close"
        );
        assert!(wait_until(Duration::from_secs(1), || !alive(stubborn)));
    }

    /// Two closes of one terminal at once (a click and a quit) both finish, and
    /// the session ends once.
    #[cfg(unix)]
    #[test]
    fn two_closes_of_one_terminal_at_once_both_finish() {
        let dir = tempfile::tempdir().unwrap();
        let state = Arc::new(TerminalSessions::default());
        let ready = dir.path().join("ready");
        let spawned = spawn_script(
            &state,
            dir.path(),
            format!(
                "set -m; (trap '' HUP; : > '{ready}'; exec sleep 300) & echo $! > '{pid}'; wait",
                ready = ready.display(),
                pid = dir.path().join("stubborn").display()
            ),
        );
        assert!(wait_until(Duration::from_secs(5), || ready.exists()));
        let pid = read_pid(&dir.path().join("stubborn"));
        let closes: Vec<_> = (0..2)
            .map(|_| {
                let (state, id) = (state.clone(), spawned.id.clone());
                thread::spawn(move || kill_session(&state, &id))
            })
            .collect();
        for close in closes {
            close.join().unwrap().unwrap();
        }
        assert!(wait_until(Duration::from_secs(1), || !alive(pid)));
        assert!(list_sessions(&state).unwrap().is_empty());
    }

    /// The jobs a close would kill are visible to the unmount check: a prompt
    /// with `sleep &` is not at its prompt.
    #[cfg(unix)]
    #[test]
    fn a_shell_with_a_background_job_reports_jobs_not_just_a_foreground() {
        let dir = tempfile::tempdir().unwrap();
        let state = TerminalSessions::default();
        let spawned = spawn_script(
            &state,
            dir.path(),
            format!(
                "set -m; sleep 300 & echo $! > '{}'; read finish",
                dir.path().join("job").display()
            ),
        );
        let job = read_pid(&dir.path().join("job"));
        let context = session_context(&state, &spawned.id).unwrap();
        assert_eq!(
            context.busy,
            Some(false),
            "the foreground is the shell's own"
        );
        assert_eq!(context.jobs, Some(true));
        kill_session(&state, &spawned.id).unwrap();
        assert!(wait_until(Duration::from_secs(3), || !alive(job)));
    }

    /// The defect: spawn/list/remove recovered from a poisoned map and
    /// write/resize/context/kill did not, so after one panic new terminals
    /// opened that could be neither typed into nor closed.
    #[cfg(unix)]
    #[test]
    fn a_poisoned_session_map_does_not_disable_terminals() {
        let dir = tempfile::tempdir().unwrap();
        let state = TerminalSessions::default();
        let spawned = spawn_script(&state, dir.path(), "read finish".into());
        let map = state.sessions.clone();
        let _ = thread::spawn(move || {
            let _guard = map.lock().unwrap();
            panic!("injected panic under the session map");
        })
        .join();
        assert!(state.sessions.is_poisoned());
        assert!(write_to_session(&state, &spawned.id, "x").is_ok());
        assert!(resize_session(&state, &spawned.id, 30, 90).is_ok());
        assert!(session_context(&state, &spawned.id).is_ok());
        // Whatever the flow says about 0 bytes, it is the flow's answer, not the lock's.
        assert!(
            !acknowledge_output(&state, &spawned.id, 0).is_err_and(|e| e.contains("Lock error"))
        );
        kill_session(&state, &spawned.id).unwrap();
        assert!(list_sessions(&state).unwrap().is_empty());
    }

    /// The defect: shutdown returned on a poisoned map before killing any PTY.
    #[cfg(unix)]
    #[test]
    fn shutdown_kills_every_terminal_even_when_the_session_map_is_poisoned() {
        let dir = tempfile::tempdir().unwrap();
        let state = TerminalSessions::default();
        let spawned = spawn_script(
            &state,
            dir.path(),
            format!(
                "echo $$ > '{}'; read finish",
                dir.path().join("shell").display()
            ),
        );
        let shell = read_pid(&dir.path().join("shell"));
        let map = state.sessions.clone();
        let _ = thread::spawn(move || {
            let _guard = map.lock().unwrap();
            panic!("injected panic under the session map");
        })
        .join();
        shutdown_sessions(&state).unwrap();
        assert!(wait_until(Duration::from_secs(3), || !alive(shell)));
        let _ = spawned;
    }

    /// The defect: Cmd-Q reached `RunEvent::Exit`, which stopped the terminals
    /// and never swept procguard's children, and a signal swept the children
    /// and never stopped the terminals. Both now run `teardown`; this pins its
    /// order: terminals first (a job gets its hangup grace), the sweep after.
    #[cfg(unix)]
    #[test]
    fn teardown_stops_terminal_jobs_before_the_sweep_runs() {
        let dir = tempfile::tempdir().unwrap();
        let state = TerminalSessions::default();
        let _spawned = spawn_script(
            &state,
            dir.path(),
            format!(
                "set -m; sleep 300 & echo $! > '{}'; wait",
                dir.path().join("job").display()
            ),
        );
        let job = read_pid(&dir.path().join("job"));
        let swept_with_job_alive = std::cell::Cell::new(None);
        crate::teardown::steps(&state, Duration::from_secs(10), || {
            swept_with_job_alive.set(Some(alive(job)));
        });
        assert_eq!(
            swept_with_job_alive.get(),
            Some(false),
            "the sweep ran before the terminals stopped"
        );
        assert!(list_sessions(&state).unwrap().is_empty());
    }

    /// The defect: spawn waited on the repository address (git), the task
    /// binding and the ledger append before returning, so a ledger another
    /// writer held, or a slow git, delayed the start until the renderer's own
    /// watchdog killed the session it was starting.
    #[cfg(unix)]
    #[test]
    fn spawn_returns_without_waiting_for_the_ledger() {
        let dir = tempfile::tempdir().unwrap();
        init_test_repo(dir.path());
        let state = TerminalSessions::default();
        let (held, release) = std::sync::mpsc::channel::<()>();
        let (taken, wait_taken) = std::sync::mpsc::channel::<()>();
        let holder = thread::spawn(move || {
            let _guard = crate::ledger::registry()
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            taken.send(()).unwrap();
            let _ = release.recv_timeout(Duration::from_secs(3));
        });
        wait_taken.recv().unwrap();
        let started = Instant::now();
        let spawned = spawn_script(&state, dir.path(), "read finish".into());
        let took = started.elapsed();
        drop(held);
        holder.join().unwrap();
        kill_session(&state, &spawned.id).unwrap();
        assert!(
            took < Duration::from_secs(1),
            "spawn waited on the ledger: {took:?}"
        );
    }

    /// The defect: a session whose terminal closed but whose process was not
    /// reaped vanished from the listing while `tracks_run` still counted it,
    /// leaving a run "running" with nothing to adopt or stop.
    #[cfg(unix)]
    #[test]
    fn a_dead_but_unreaped_session_is_listed_as_ending_and_can_be_stopped() {
        let dir = tempfile::tempdir().unwrap();
        let state = TerminalSessions::default();
        let spawned = spawn_script(&state, dir.path(), "read finish".into());
        assert!(!list_sessions(&state).unwrap()[0].ending);
        state
            .lock_sessions()
            .get(&spawned.id)
            .unwrap()
            .dead
            .store(true, Ordering::SeqCst);
        let listed = list_sessions(&state).unwrap();
        assert_eq!(listed.len(), 1, "an ending session was hidden");
        assert!(listed[0].ending && listed[0].detached);
        // Stop still works on it.
        kill_session(&state, &spawned.id).unwrap();
        assert!(list_sessions(&state).unwrap().is_empty());
    }

    fn init_test_repo(dir: &std::path::Path) {
        let output = std::process::Command::new("git")
            .args(["init", "-b", "main"])
            .current_dir(dir)
            .output_locked()
            .expect("git init");
        assert!(output.status.success());
        crate::test_support::trust_repo(dir);
    }

    /// A checkout inside `holder`, trusted, with `holder` set to `mode`.
    #[cfg(unix)]
    fn repo_under_holder(base: &std::path::Path, mode: u32) -> std::path::PathBuf {
        use std::os::unix::fs::PermissionsExt;
        let holder = base.join("holder");
        std::fs::create_dir(&holder).unwrap();
        let repo = holder.join("checkout");
        std::fs::create_dir(&repo).unwrap();
        init_test_repo(&repo);
        std::fs::set_permissions(&holder, std::fs::Permissions::from_mode(mode)).unwrap();
        repo
    }

    #[cfg(unix)]
    fn try_pty_launch(repo: &std::path::Path, marker: &str) -> Result<TerminalSpawned, String> {
        let app = tauri::test::mock_builder().build(crate::context()).unwrap();
        let state = TerminalSessions::default();
        spawn_session_inner(
            app.handle(),
            &state,
            repo.to_str().unwrap(),
            None,
            24,
            80,
            Some("/bin/sh".into()),
            Some(vec!["-c".into(), format!("touch {marker}")]),
            None,
            discard(),
            None,
        )
    }

    /// THE CLASS: trust is granted to an object, but a PTY child's working
    /// directory is resolved from a *path* at launch — and unlike
    /// `procguard::spawn`, nothing can re-anchor it afterwards, because
    /// `portable_pty` owns the command's only `pre_exec` hook. So a session may
    /// only start where the name cannot be swapped out from under it.
    #[cfg(unix)]
    #[test]
    fn a_terminal_refuses_a_checkout_whose_holder_other_users_can_rewrite() {
        let base = tempfile::tempdir().unwrap();
        let repo = repo_under_holder(base.path(), 0o777);
        let error = try_pty_launch(&repo, "world-writable-marker")
            .expect_err("a substitutable holder must not start an interactive session");
        assert!(
            error.contains(crate::repository_trust::SHARED_DIRECTORY),
            "refusal must name this specific cause, not an incidental failure: {error}"
        );
        // Granting trust cannot fix a writable holder, so this must not carry
        // the marker the desktop reacts to by prompting for approval — that
        // would send the user round a loop whose every turn ends here.
        assert!(
            !error.contains(crate::repository_trust::REQUIRED),
            "refusal invites a trust prompt that cannot resolve it: {error}"
        );
        assert!(
            error.contains("holder"),
            "a refusal has to name the directory to change: {error}"
        );
        std::thread::sleep(std::time::Duration::from_millis(200));
        assert!(
            !repo.join("world-writable-marker").exists(),
            "the shell ran despite the refusal"
        );
    }

    /// A gate that refuses most of the time is a different defect from one
    /// that always refuses, and only repetition tells them apart. Both verdicts
    /// have to be the same on every attempt, or a user retries until they get
    /// the answer they want.
    #[cfg(unix)]
    #[test]
    fn the_terminal_verdict_is_identical_on_every_attempt() {
        let base = tempfile::tempdir().unwrap();
        let (a, b) = (base.path().join("a"), base.path().join("b"));
        std::fs::create_dir(&a).unwrap();
        std::fs::create_dir(&b).unwrap();
        let shared = repo_under_holder(&a, 0o777);
        let sticky = repo_under_holder(&b, 0o1777);

        for attempt in 0..25 {
            let refusal = try_pty_launch(&shared, &format!("shared-{attempt}"))
                .expect_err("attempt {attempt} started a session in a substitutable checkout");
            assert!(
                refusal.contains(crate::repository_trust::SHARED_DIRECTORY),
                "attempt {attempt} refused for the wrong reason: {refusal}"
            );
        }
        for attempt in 0..25 {
            try_pty_launch(&sticky, &format!("sticky-{attempt}")).unwrap_or_else(|e| {
                panic!("attempt {attempt} wrongly refused a sticky holder: {e}")
            });
        }
    }

    /// The carve-out that keeps the refusal honest rather than merely strict:
    /// a sticky holder — `/tmp`, `/Users/Shared` — lets nobody but the entry's
    /// owner rename it, so those checkouts must still open a terminal.
    #[cfg(unix)]
    #[test]
    fn a_terminal_still_opens_under_a_sticky_world_writable_holder() {
        let base = tempfile::tempdir().unwrap();
        let repo = repo_under_holder(base.path(), 0o1777);
        let spawned = try_pty_launch(&repo, "sticky-marker")
            .expect("a sticky holder cannot be rewritten by another user");
        for _ in 0..100 {
            if repo.join("sticky-marker").exists() {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(20));
        }
        assert!(
            repo.join("sticky-marker").exists(),
            "session {} never ran in the checkout",
            spawned.id
        );
    }

    #[test]
    fn rejects_empty_argv() {
        let dir = TempDir::new().unwrap();
        init_test_repo(dir.path());
        let res = run_terminal(dir.path().to_str().unwrap(), &[], None);
        assert!(res.is_err());
        assert_eq!(res.unwrap_err(), "No command provided");
    }

    #[test]
    fn rejects_invalid_repo_path() {
        let res = run_terminal(
            "/nonexistent/directory/path/here",
            &["git".into(), "status".into()],
            None,
        );
        assert!(res.is_err());
    }

    #[test]
    fn runs_safe_git_command_in_valid_repo() {
        let dir = TempDir::new().unwrap();
        init_test_repo(dir.path());
        let res = run_terminal(
            dir.path().to_str().unwrap(),
            &["git".into(), "status".into()],
            Some(10),
        )
        .expect("runs git status");

        // Checked first and named: this test's subject *is* running a process,
        // so it cannot leave the spawn path, and a host that could not start
        // `git` inside ten seconds must not read as a runner that mis-reports an
        // exit code.
        assert!(
            !res.timed_out,
            "the host did not run `git status` inside 10s; that is machine \
             capacity, not the terminal runner"
        );
        assert_eq!(res.exit_code, Some(0));
        assert!(res.command.contains("git status"));
        assert!(res.stdout_tail.contains("On branch main"));
        assert!(res.gated);
    }

    #[test]
    fn runs_non_git_command() {
        let dir = TempDir::new().unwrap();
        init_test_repo(dir.path());
        let res = run_terminal(
            dir.path().to_str().unwrap(),
            &["echo".into(), "hello gitpulse".into()],
            Some(5),
        )
        .expect("runs echo");

        assert_eq!(res.exit_code, Some(0));
        assert!(res.stdout_tail.contains("hello gitpulse"));
        assert!(res.policy.is_none());
        assert!(!res.gated);
    }

    #[test]
    fn manvi_health_actions_refuse_arbitrary_processes_and_repo_escape() {
        for argv in [
            vec!["rm".into(), "-rf".into(), ".".into()],
            vec!["sh".into(), "-c".into(), "npm audit fix".into()],
            vec!["curl".into(), "https://example.com".into()],
            vec![
                "npm".into(),
                "install".into(),
                "https://example.com/pkg.tgz".into(),
            ],
            vec![
                "npm".into(),
                "--prefix".into(),
                "/tmp/outside".into(),
                "install".into(),
            ],
            vec!["../tool".into(), "audit".into()],
        ] {
            let err = validate_manvi_action(&argv, ManviActionKind::Health).unwrap_err();
            assert!(
                err.contains("MANVI"),
                "refusal must name the authority boundary: {err}"
            );
        }
    }

    #[test]
    fn health_actions_cannot_mutate_global_package_environments() {
        for argv in [
            vec![
                "npm".into(),
                "install".into(),
                "--global".into(),
                "eslint".into(),
            ],
            vec!["npm".into(), "install".into(), "-g".into(), "eslint".into()],
            vec![
                "python3".into(),
                "-m".into(),
                "pip".into(),
                "install".into(),
                "requests".into(),
            ],
            vec!["pip".into(), "install".into(), "requests".into()],
        ] {
            let err = validate_manvi_action(&argv, ManviActionKind::Health)
                .expect_err("global/environment package mutation must be refused");
            assert!(err.contains("MANVI"), "{argv:?}: {err}");
        }
    }

    #[test]
    fn python_module_actions_are_verb_scoped() {
        validate_manvi_action(
            &[
                "python3".into(),
                "-m".into(),
                "coverage".into(),
                "run".into(),
                "-m".into(),
                "pytest".into(),
            ],
            ManviActionKind::Coverage,
        )
        .unwrap();
        let err = validate_manvi_action(
            &[
                "python3".into(),
                "-m".into(),
                "coverage".into(),
                "erase".into(),
            ],
            ManviActionKind::Coverage,
        )
        .expect_err("coverage cleanup is not coverage generation");
        assert!(err.contains("allowlist"), "{err}");
    }

    #[test]
    fn manvi_health_actions_allow_remediation_and_verification_shapes() {
        for argv in [
            vec!["npm".into(), "audit".into(), "fix".into()],
            vec!["cargo".into(), "update".into(), "-p".into(), "serde".into()],
            vec!["python3".into(), "-m".into(), "pip".into(), "check".into()],
            vec!["go".into(), "test".into(), "./...".into()],
        ] {
            validate_manvi_action(&argv, ManviActionKind::Health).unwrap();
        }
    }

    #[test]
    fn manvi_coverage_actions_are_purpose_limited() {
        validate_manvi_action(
            &["cargo".into(), "llvm-cov".into(), "--workspace".into()],
            ManviActionKind::Coverage,
        )
        .unwrap();
        validate_manvi_action(
            &["pytest".into(), "--cov".into(), "--cov-report=xml".into()],
            ManviActionKind::CoverageGenerator,
        )
        .unwrap();
        let err = validate_manvi_action(
            &["npm".into(), "install".into(), "left-pad".into()],
            ManviActionKind::Coverage,
        )
        .unwrap_err();
        assert!(err.contains("coverage"));
    }

    #[test]
    fn allowlist_keywords_cannot_be_smuggled_behind_a_different_action() {
        for argv in [
            vec![
                "npx".into(),
                "--no-install".into(),
                "hostile-runner".into(),
                "vitest".into(),
            ],
            vec!["npm".into(), "run".into(), "contest-deploy".into()],
            vec!["./gradlew".into(), "publish".into(), "test".into()],
            vec!["mvn".into(), "deploy".into(), "verify".into()],
        ] {
            let err = validate_manvi_action(&argv, ManviActionKind::CoverageGenerator)
                .expect_err("an allowed keyword must not bless a different executable/task");
            assert!(err.contains("allowlist"), "{argv:?}: {err}");
        }
    }

    /// The virtualenv interpreter is the one program GitPulse executes that
    /// is *expected* to symlink out of the repository, so it is exempted from
    /// canonical containment. These tests pin what replaced that guarantee.
    /// Regression, found by running the installed app: the trust anchor used
    /// to be "resolves to the Python on PATH". A macOS app launched from
    /// Finder inherits launchd's PATH, not the shell's, so on a Homebrew
    /// machine the installed GitPulse refused the very virtualenv it had just
    /// created — while the shell-launched dev build accepted it. The rule must
    /// not depend on how the app was launched.
    #[cfg(unix)]
    #[test]
    fn venv_interpreter_is_accepted_without_a_matching_path_entry() {
        use std::os::unix::fs::symlink;
        let dir = tempfile::TempDir::new().expect("tempdir");
        let repo = dir.path();
        std::fs::create_dir_all(repo.join(".venv/bin")).unwrap();
        std::fs::write(repo.join(".venv/pyvenv.cfg"), "home = /elsewhere/bin\n").unwrap();

        // A Python interpreter that is deliberately NOT the one on PATH, in a
        // directory PATH does not contain — the Homebrew-vs-/usr/bin split.
        let elsewhere = tempfile::TempDir::new().expect("tempdir2");
        let real_python = elsewhere.path().join("python3.14");
        std::fs::write(&real_python, "#!/bin/sh\nexit 0\n").unwrap();
        symlink(&real_python, repo.join(".venv/bin/python")).unwrap();

        check_venv_interpreter(repo, ".venv/bin/python")
            .expect("a Python outside the repo must qualify without a PATH match");
    }

    /// The relaxation must not become "run anything the repository points at".
    #[cfg(unix)]
    #[test]
    fn venv_interpreter_still_refuses_non_python_and_in_repo_targets() {
        use std::os::unix::fs::symlink;
        let dir = tempfile::TempDir::new().expect("tempdir");
        let repo = dir.path();
        std::fs::create_dir_all(repo.join(".venv/bin")).unwrap();
        std::fs::write(repo.join(".venv/pyvenv.cfg"), "home = /usr/bin\n").unwrap();

        // (a) A real system binary that is not a Python.
        symlink("/bin/sh", repo.join(".venv/bin/python")).unwrap();
        assert!(
            check_venv_interpreter(repo, ".venv/bin/python").is_err(),
            "a shell is not a Python interpreter"
        );
        std::fs::remove_file(repo.join(".venv/bin/python")).unwrap();

        // (b) A checked-in payload named like a Python. Being inside the
        //     repository disqualifies it however it is named.
        let payload = repo.join("tools/python3");
        std::fs::create_dir_all(payload.parent().unwrap()).unwrap();
        std::fs::write(&payload, "#!/bin/sh\nexit 0\n").unwrap();
        symlink(&payload, repo.join(".venv/bin/python")).unwrap();
        assert!(
            check_venv_interpreter(repo, ".venv/bin/python").is_err(),
            "a repository-authored binary must never be executed as the interpreter"
        );
    }

    #[test]
    fn python_interpreter_names_are_recognized_precisely() {
        for good in [
            "python",
            "python3",
            "python3.14",
            "python3.9",
            "python3.14t",
        ] {
            assert!(is_python_interpreter_name(good), "{good}");
        }
        for bad in [
            "sh", "bash", "node", "python2", "pythonic", "python3x", "", "py",
        ] {
            assert!(!is_python_interpreter_name(bad), "{bad}");
        }
    }

    #[cfg(unix)]
    #[test]
    fn venv_interpreter_must_resolve_to_the_host_python() {
        use std::os::unix::fs::symlink;
        let dir = tempfile::TempDir::new().expect("tempdir");
        let repo = dir.path();
        std::fs::create_dir_all(repo.join(".venv/bin")).unwrap();
        std::fs::write(repo.join(".venv/pyvenv.cfg"), "home = /usr/bin\n").unwrap();
        // A repository that ships its own "interpreter" pointing at a shell.
        symlink("/bin/sh", repo.join(".venv/bin/python")).unwrap();

        let err = check_venv_interpreter(repo, ".venv/bin/python")
            .expect_err("a symlink to anything but the host python must be refused");
        assert!(err.contains("does not resolve"), "{err}");

        // And the refusal is what the command gate reports, not a bypass.
        let argv = vec![
            ".venv/bin/python".to_string(),
            "-m".to_string(),
            "pytest".to_string(),
        ];
        let gate = validate_manvi_paths(repo, &argv, ManviActionKind::CoverageGenerator)
            .expect_err("the gate must refuse it too");
        assert!(gate.contains("will not execute it"), "{gate}");
    }

    #[cfg(unix)]
    #[test]
    fn venv_interpreter_requires_a_real_virtualenv_marker() {
        let dir = tempfile::TempDir::new().expect("tempdir");
        let repo = dir.path();
        std::fs::create_dir_all(repo.join(".venv/bin")).unwrap();
        // A regular file in a directory merely shaped like a virtualenv.
        std::fs::write(repo.join(".venv/bin/python"), "#!/bin/sh\necho hi\n").unwrap();

        let err = check_venv_interpreter(repo, ".venv/bin/python")
            .expect_err("no pyvenv.cfg means this is not a virtualenv");
        assert!(err.contains("pyvenv.cfg"), "{err}");
    }

    #[test]
    fn venv_interpreter_must_exist() {
        let dir = tempfile::TempDir::new().expect("tempdir");
        let err = check_venv_interpreter(dir.path(), ".venv/bin/python")
            .expect_err("an absent interpreter is not runnable");
        assert!(
            err.contains("pyvenv.cfg") || err.contains("repository file"),
            "{err}"
        );
    }

    /// The positive case, against a virtualenv built the way `python -m venv`
    /// really builds one (interpreter symlinked out to the host toolchain).
    /// Skipped where no Python is installed; the negative cases above carry
    /// the security contract on every machine.
    #[test]
    fn a_real_virtualenv_interpreter_is_accepted() {
        let dir = tempfile::TempDir::new().expect("tempdir");
        let repo = dir.path();
        let built = std::process::Command::new("python3")
            .args(["-m", "venv", ".venv"])
            .current_dir(repo)
            .status_locked();
        match built {
            Ok(status) if status.success() => {}
            _ => return, // no usable python3 on this host
        }
        // Unix symlinks the interpreter out; Windows copies it in. Ask for the
        // platform's own spelling rather than assuming the Unix one.
        let rel = crate::coverage_toolchain::managed_venv_python();
        assert!(
            repo.join(rel).is_file(),
            "python reported success but produced no {rel}; a create that made \
             nothing must fail here rather than be read as a refused virtualenv"
        );
        // On a refusal, say what the interpreter actually IS. The Windows
        // build of `venv` does not necessarily copy the base interpreter in,
        // and a bare "refused" gives nothing to reason about from a CI log.
        if let Err(detail) = check_venv_interpreter(repo, rel) {
            let interpreter = repo.join(rel);
            let size = std::fs::metadata(&interpreter).map(|m| m.len());
            let cfg = std::fs::read_to_string(repo.join(".venv").join("pyvenv.cfg"))
                .unwrap_or_else(|e| format!("<unreadable: {e}>"));
            let hosts: Vec<String> = ["python3", "python"]
                .iter()
                .map(|name| match resolve_on_path(name) {
                    Some(path) => {
                        let len = std::fs::metadata(&path).map(|m| m.len());
                        format!("{name} -> {} ({len:?} bytes)", path.display())
                    }
                    None => format!("{name} -> <not on PATH>"),
                })
                .collect();
            panic!(
                "a virtualenv built by python -m venv must be accepted: {detail}\n\
                 interpreter: {} ({size:?} bytes)\n\
                 pyvenv.cfg:\n{cfg}\n\
                 hosts: {}",
                interpreter.display(),
                hosts.join(" | ")
            );
        }
        let argv = vec![
            rel.to_string(),
            "-m".to_string(),
            "pytest".to_string(),
            "--cov".to_string(),
            "--cov-report=xml".to_string(),
        ];
        validate_manvi_action(&argv, ManviActionKind::CoverageGenerator)
            .expect("the planned generate command must be allowed");
        validate_manvi_paths(repo, &argv, ManviActionKind::CoverageGenerator)
            .expect("and its paths must validate");
    }

    /// The copy admitted on Windows is admitted on bytes alone.
    ///
    /// This is the case the byte comparison exists for: a repository can place
    /// whatever it likes at the virtualenv spelling, and naming it `python`
    /// beside a `pyvenv.cfg` must not be enough to have it executed.
    #[test]
    fn a_repository_payload_at_the_venv_spelling_is_refused() {
        let dir = tempfile::TempDir::new().expect("tempdir");
        let repo = dir.path();
        let rel = crate::coverage_toolchain::managed_venv_python();
        let interpreter = repo.join(rel);
        std::fs::create_dir_all(interpreter.parent().expect("venv bin dir")).expect("mkdir");

        // `base-executable` names the REAL host interpreter, so every other
        // rule is satisfied: the virtualenv marker is present, the base
        // resolves, it lives outside the repository and is named like a
        // Python. The only thing left to refuse on is the bytes. Without a
        // host interpreter there is nothing to compare against and the test
        // would prove nothing, so it says so rather than passing quietly.
        let Some(host) = resolve_on_path("python3").or_else(|| resolve_on_path("python")) else {
            return;
        };
        std::fs::write(
            repo.join(".venv").join("pyvenv.cfg"),
            format!(
                "home = {}\nbase-executable = {}\n",
                host.parent().expect("host dir").display(),
                host.display()
            ),
        )
        .expect("cfg");
        std::fs::write(&interpreter, b"#!/bin/sh\necho payload\n").expect("payload");

        let err = check_venv_interpreter(repo, rel)
            .expect_err("a payload at the virtualenv spelling must be refused");
        assert!(err.contains("Python interpreter"), "{err}");
    }

    /// The setup steps coverage generation is now allowed to run.
    ///
    /// Each is pinned to one shape. This test is the inventory of what the
    /// widening actually bought; anything not listed here must be refused by
    /// the companion test below.
    #[test]
    fn coverage_generator_allows_exactly_the_planned_setup_steps() {
        for argv in [
            // Python: build the project's own virtualenv, then put the pytest
            // coverage toolchain in it, then run it.
            vec!["python3".into(), "-m".into(), "venv".into(), ".venv".into()],
            vec!["python".into(), "-m".into(), "venv".into(), "venv".into()],
            vec![
                ".venv/bin/python".into(),
                "-m".into(),
                "pip".into(),
                "install".into(),
                "pytest".into(),
                "pytest-cov".into(),
            ],
            vec![
                ".venv/bin/python".into(),
                "-m".into(),
                "pytest".into(),
                "--cov".into(),
                "--cov-report=xml".into(),
            ],
            // JavaScript: the missing coverage provider as a devDependency.
            vec![
                "npm".into(),
                "install".into(),
                "--save-dev".into(),
                "@vitest/coverage-v8".into(),
            ],
            vec![
                "npm".into(),
                "install".into(),
                "-D".into(),
                "@vitest/coverage-istanbul".into(),
            ],
            // PHP and Ruby: materialize dependencies the manifest already pins.
            vec!["composer".into(), "install".into()],
            vec!["bundle".into(), "install".into()],
            vec!["bundle".into(), "exec".into(), "rspec".into()],
            vec![
                "vendor/bin/phpunit".into(),
                "--coverage-clover".into(),
                "coverage.xml".into(),
            ],
            // JVM: the wrapper a project ships so nobody needs a system Maven.
            vec!["./mvnw".into(), "verify".into()],
        ] {
            validate_manvi_action(&argv, ManviActionKind::CoverageGenerator)
                .unwrap_or_else(|err| panic!("{argv:?} must be allowed: {err}"));
        }
    }

    /// The C/C++ toolchain the generator may now drive.
    ///
    /// Adding `cmake`, `ctest` and `gcovr` widened what this app can spawn, so
    /// each shape is pinned here and everything adjacent is refused below.
    #[test]
    fn coverage_generator_allows_exactly_the_native_build_steps() {
        for argv in [
            vec![
                "cmake".into(),
                "-S".into(),
                ".".into(),
                "-B".into(),
                NATIVE_COVERAGE_BUILD_DIR.into(),
                "-DCMAKE_BUILD_TYPE=Debug".into(),
                format!("-DCMAKE_C_FLAGS={NATIVE_COVERAGE_FLAG}"),
                format!("-DCMAKE_CXX_FLAGS={NATIVE_COVERAGE_FLAG}"),
                format!("-DCMAKE_EXE_LINKER_FLAGS={NATIVE_COVERAGE_FLAG}"),
            ],
            vec![
                "cmake".into(),
                "--build".into(),
                NATIVE_COVERAGE_BUILD_DIR.into(),
            ],
            vec![
                "ctest".into(),
                "--test-dir".into(),
                NATIVE_COVERAGE_BUILD_DIR.into(),
                "--output-on-failure".into(),
            ],
            vec![
                "gcovr".into(),
                "-r".into(),
                ".".into(),
                NATIVE_COVERAGE_BUILD_DIR.into(),
                "--lcov".into(),
                NATIVE_COVERAGE_LCOV.into(),
            ],
            vec![
                ".venv/bin/python".into(),
                "-m".into(),
                "gcovr".into(),
                "-r".into(),
                ".".into(),
                NATIVE_COVERAGE_BUILD_DIR.into(),
                "--lcov".into(),
                NATIVE_COVERAGE_LCOV.into(),
            ],
            vec![
                ".venv/bin/python".into(),
                "-m".into(),
                "pip".into(),
                "install".into(),
                "gcovr".into(),
            ],
        ] {
            validate_manvi_action(&argv, ManviActionKind::CoverageGenerator)
                .unwrap_or_else(|err| panic!("{argv:?} must be allowed: {err}"));
        }
    }

    /// The blast radius of the C/C++ widening, stated as refusals.
    ///
    /// The load-bearing cases are the ones that would turn a coverage panel
    /// into a general build runner or file writer: a configure into the
    /// project's own build directory, an arbitrary `-D`, CMake's script mode,
    /// and any gcovr output path other than the pinned one.
    #[test]
    fn coverage_generator_refuses_everything_adjacent_to_the_native_steps() {
        for (argv, why) in [
            (
                vec![
                    "cmake".into(),
                    "-S".into(),
                    ".".into(),
                    "-B".into(),
                    "build".into(),
                ],
                "configuring into the project's own build directory would replace a developer's build",
            ),
            (
                vec![
                    "cmake".into(),
                    "-S".into(),
                    ".".into(),
                    "-B".into(),
                    NATIVE_COVERAGE_BUILD_DIR.into(),
                    "-DCMAKE_TOOLCHAIN_FILE=/tmp/evil.cmake".into(),
                ],
                "only the coverage flag variables may be defined",
            ),
            (
                vec![
                    "cmake".into(),
                    "-S".into(),
                    ".".into(),
                    "-B".into(),
                    NATIVE_COVERAGE_BUILD_DIR.into(),
                    "-DCMAKE_C_FLAGS=-fsanitize=address".into(),
                ],
                "the flag variables may carry only the coverage flag",
            ),
            (
                vec!["cmake".into(), "-P".into(), "script.cmake".into()],
                "CMake script mode executes arbitrary project code",
            ),
            (
                vec![
                    "cmake".into(),
                    "--install".into(),
                    NATIVE_COVERAGE_BUILD_DIR.into(),
                ],
                "install writes outside the build directory",
            ),
            (
                vec![
                    "ctest".into(),
                    "--test-dir".into(),
                    "build".into(),
                    "--output-on-failure".into(),
                ],
                "only the managed build directory may be tested",
            ),
            (
                vec![
                    "ctest".into(),
                    "--test-dir".into(),
                    NATIVE_COVERAGE_BUILD_DIR.into(),
                    "--build-and-test".into(),
                ],
                "build-and-test drives a second, unpinned configure",
            ),
            (
                vec![
                    "gcovr".into(),
                    "-r".into(),
                    ".".into(),
                    NATIVE_COVERAGE_BUILD_DIR.into(),
                    "--lcov".into(),
                    "../escape.info".into(),
                ],
                "gcovr may write only the pinned lcov path",
            ),
            (
                vec![
                    "gcovr".into(),
                    "-r".into(),
                    ".".into(),
                    NATIVE_COVERAGE_BUILD_DIR.into(),
                    "--html".into(),
                    "report.html".into(),
                ],
                "no output format other than the pinned lcov one",
            ),
            (
                vec![
                    "python3".into(),
                    "-m".into(),
                    "gcovr".into(),
                    "-r".into(),
                    ".".into(),
                    NATIVE_COVERAGE_BUILD_DIR.into(),
                    "--lcov".into(),
                    NATIVE_COVERAGE_LCOV.into(),
                ],
                "the host interpreter is never the one GitPulse runs modules in",
            ),
            (
                vec![
                    "python3".into(),
                    "-m".into(),
                    "pip".into(),
                    "install".into(),
                    "gcovr".into(),
                ],
                "host interpreter must never receive an install",
            ),
            (
                vec![
                    ".venv/bin/python".into(),
                    "-m".into(),
                    "pip".into(),
                    "install".into(),
                    "gcovr".into(),
                    "evil-package".into(),
                ],
                "an extra package must not ride along with gcovr",
            ),
        ] {
            let err = validate_manvi_action(&argv, ManviActionKind::CoverageGenerator)
                .expect_err(&format!("{argv:?} must be refused: {why}"));
            assert!(err.contains("refused"), "{err}");
        }
    }

    /// Coverage *analysis* is read-only. None of the build steps may be
    /// reached from it, only from generation.
    #[test]
    fn read_only_coverage_analysis_cannot_run_the_native_build() {
        for argv in [
            vec![
                "cmake".into(),
                "--build".into(),
                NATIVE_COVERAGE_BUILD_DIR.into(),
            ],
            vec![
                "ctest".into(),
                "--test-dir".into(),
                NATIVE_COVERAGE_BUILD_DIR.into(),
            ],
            vec![
                "gcovr".into(),
                "-r".into(),
                ".".into(),
                NATIVE_COVERAGE_BUILD_DIR.into(),
                "--lcov".into(),
                NATIVE_COVERAGE_LCOV.into(),
            ],
        ] {
            validate_manvi_action(&argv, ManviActionKind::Coverage)
                .expect_err(&format!("{argv:?} must be generation-only"));
        }
    }

    /// The blast radius of the setup widening, stated as refusals.
    ///
    /// The load-bearing case is the first one: installing into whatever
    /// interpreter is on PATH mutates the host Python. Only the repository's
    /// own virtualenv may receive packages, and only the pinned ones.
    #[test]
    fn coverage_generator_refuses_everything_adjacent_to_the_setup_steps() {
        for (argv, why) in [
            (
                vec![
                    "python3".into(),
                    "-m".into(),
                    "pip".into(),
                    "install".into(),
                    "pytest".into(),
                ],
                "host interpreter must never receive an install",
            ),
            (
                vec!["pip".into(), "install".into(), "pytest".into()],
                "bare pip is not an install channel",
            ),
            (
                vec![
                    ".venv/bin/python".into(),
                    "-m".into(),
                    "pip".into(),
                    "install".into(),
                    "requests".into(),
                ],
                "package set is pinned to the pytest coverage toolchain",
            ),
            (
                vec![
                    ".venv/bin/python".into(),
                    "-m".into(),
                    "pip".into(),
                    "install".into(),
                    "pytest".into(),
                    "evil-package".into(),
                ],
                "an extra package must not ride along with a pinned one",
            ),
            (
                vec![
                    ".venv/bin/python".into(),
                    "-m".into(),
                    "pip".into(),
                    "uninstall".into(),
                    "pytest".into(),
                ],
                "only install is allowed",
            ),
            (
                vec![
                    "python3".into(),
                    "-m".into(),
                    "venv".into(),
                    "tools/env".into(),
                ],
                "virtualenv location is pinned to .venv/venv",
            ),
            (
                vec![
                    "npm".into(),
                    "install".into(),
                    "--save-dev".into(),
                    "left-pad".into(),
                ],
                "provider set is pinned",
            ),
            (
                vec!["npm".into(), "install".into(), "@vitest/coverage-v8".into()],
                "a coverage provider is a devDependency, not a dependency",
            ),
            (
                vec![
                    "npm".into(),
                    "install".into(),
                    "--save-dev".into(),
                    "@vitest/coverage-v8".into(),
                    "--force".into(),
                ],
                "no stray flags may ride along",
            ),
            (
                vec!["npm".into(), "install".into()],
                "a bare install resolves the whole tree, which is not a coverage step",
            ),
            (
                vec!["composer".into(), "require".into(), "evil/pkg".into()],
                "generation may install pinned deps, not add new ones",
            ),
            (
                vec![
                    "bundle".into(),
                    "exec".into(),
                    "rake".into(),
                    "release".into(),
                ],
                "coverage has no reason to run arbitrary project tasks",
            ),
            (
                vec![
                    "vendor/bin/evil".into(),
                    "--coverage-clover".into(),
                    "coverage.xml".into(),
                ],
                "a repository file is not an executable just because it is named",
            ),
            (
                vec![".venv/bin/pip".into(), "install".into(), "pytest".into()],
                "only the pinned interpreter spellings are repo-local programs",
            ),
        ] {
            let err = validate_manvi_action(&argv, ManviActionKind::CoverageGenerator)
                .expect_err(&format!("{argv:?} must be refused: {why}"));
            assert!(
                err.contains("refused"),
                "{argv:?} ({why}) must be refused explicitly: {err}"
            );
        }
    }

    /// Analysis never writes. Every setup step the generator may run must be
    /// refused for the read-only coverage action, so a mislabeled call site
    /// cannot install anything.
    #[test]
    fn coverage_analysis_refuses_every_setup_step() {
        for argv in [
            vec!["python3".into(), "-m".into(), "venv".into(), ".venv".into()],
            vec![
                ".venv/bin/python".into(),
                "-m".into(),
                "pip".into(),
                "install".into(),
                "pytest".into(),
                "pytest-cov".into(),
            ],
            vec![
                "npm".into(),
                "install".into(),
                "--save-dev".into(),
                "@vitest/coverage-v8".into(),
            ],
            vec!["composer".into(), "install".into()],
            vec!["bundle".into(), "install".into()],
        ] {
            let err = validate_manvi_action(&argv, ManviActionKind::Coverage)
                .expect_err("analysis must not be able to install anything");
            assert!(err.contains("allowlist"), "{argv:?}: {err}");
        }
    }

    /// A health remediation must not become an install channel either: the
    /// venv/pip pair is coverage-generation-only.
    #[test]
    fn health_actions_cannot_use_the_coverage_install_channel() {
        for argv in [
            vec!["python3".into(), "-m".into(), "venv".into(), ".venv".into()],
            vec![
                ".venv/bin/python".into(),
                "-m".into(),
                "pip".into(),
                "install".into(),
                "pytest".into(),
            ],
        ] {
            let err = validate_manvi_action(&argv, ManviActionKind::Health)
                .expect_err("health remediation is not a coverage setup channel");
            assert!(err.contains("allowlist"), "{argv:?}: {err}");
        }
    }

    /// The global refusals still bind the new shapes: no escaping the repo,
    /// no host-wide flags, no network sources.
    #[test]
    fn setup_steps_remain_subject_to_the_global_refusals() {
        for argv in [
            vec![
                "python3".into(),
                "-m".into(),
                "venv".into(),
                "/tmp/env".into(),
            ],
            vec![
                "python3".into(),
                "-m".into(),
                "venv".into(),
                "../outside".into(),
            ],
            vec![
                ".venv/bin/python".into(),
                "-m".into(),
                "pip".into(),
                "install".into(),
                "--user".into(),
                "pytest".into(),
            ],
            vec![
                "npm".into(),
                "install".into(),
                "--save-dev".into(),
                "https://evil.example/pkg.tgz".into(),
            ],
            vec![
                "npm".into(),
                "install".into(),
                "--global".into(),
                "@vitest/coverage-v8".into(),
            ],
        ] {
            validate_manvi_action(&argv, ManviActionKind::CoverageGenerator)
                .expect_err("global refusals must still bind");
        }
    }

    #[test]
    fn coverage_generator_allowlist_accepts_scanner_planned_commands() {
        for argv in [
            vec!["npm".into(), "run".into(), "coverage".into()],
            vec![
                "npx".into(),
                "--no-install".into(),
                "vitest".into(),
                "run".into(),
                "--coverage".into(),
            ],
            vec![
                "npx".into(),
                "--no-install".into(),
                "jest".into(),
                "--coverage".into(),
            ],
            vec![
                "cargo".into(),
                "llvm-cov".into(),
                "--manifest-path".into(),
                "src-tauri/Cargo.toml".into(),
                "--workspace".into(),
                "--lcov".into(),
                "--output-path".into(),
                "src-tauri/lcov.info".into(),
            ],
            vec![
                "cargo".into(),
                "llvm-cov".into(),
                "--workspace".into(),
                "--lcov".into(),
                "--output-path".into(),
                "lcov.info".into(),
            ],
            vec!["pytest".into(), "--cov".into(), "--cov-report=xml".into()],
            vec![
                "go".into(),
                "test".into(),
                "./...".into(),
                "-coverprofile=coverage.out".into(),
            ],
            vec![
                "go".into(),
                "-C".into(),
                "backend/go_orchestrator".into(),
                "test".into(),
                "./...".into(),
                "-coverprofile=coverage.out".into(),
            ],
            vec!["npm".into(), "run".into(), "test:coverage".into()],
            vec!["./gradlew".into(), "test".into(), "jacocoTestReport".into()],
            vec!["mvn".into(), "verify".into()],
            vec![
                "swift".into(),
                "test".into(),
                "--enable-code-coverage".into(),
            ],
            vec![
                "swift".into(),
                "test".into(),
                "--package-path".into(),
                "ios".into(),
                "--enable-code-coverage".into(),
            ],
            vec!["dart".into(), "test".into(), "--coverage=coverage".into()],
        ] {
            validate_manvi_action(&argv, ManviActionKind::CoverageGenerator)
                .unwrap_or_else(|err| panic!("{argv:?} must be allowed: {err}"));
        }
        let npx_without_no_install = validate_manvi_action(
            &[
                "npx".into(),
                "vitest".into(),
                "run".into(),
                "--coverage".into(),
            ],
            ManviActionKind::CoverageGenerator,
        )
        .unwrap_err();
        assert!(
            npx_without_no_install.contains("allowlist"),
            "npx must require --no-install: {npx_without_no_install}"
        );
    }

    #[test]
    fn coverage_generator_allowlist_refuses_go_chdir_without_test() {
        for argv in [
            vec![
                "go".into(),
                "-C".into(),
                "backend/go_orchestrator".into(),
                "run".into(),
                ".".into(),
            ],
            vec!["go".into(), "-C".into(), "backend".into()],
            vec![
                "go".into(),
                "-C".into(),
                "-modcache".into(),
                "test".into(),
                "./...".into(),
            ],
        ] {
            let err = validate_manvi_action(&argv, ManviActionKind::CoverageGenerator)
                .expect_err("only go -C <dir> test is allowed");
            assert!(err.contains("allowlist"), "{argv:?}: {err}");
        }
    }

    #[test]
    fn coverage_generator_allowlist_accepts_swift_and_dart_test_only() {
        validate_manvi_action(
            &[
                "swift".into(),
                "test".into(),
                "--enable-code-coverage".into(),
            ],
            ManviActionKind::CoverageGenerator,
        )
        .unwrap();
        validate_manvi_action(
            &[
                "swift".into(),
                "test".into(),
                "--package-path".into(),
                "ios".into(),
                "--enable-code-coverage".into(),
            ],
            ManviActionKind::CoverageGenerator,
        )
        .unwrap();
        validate_manvi_action(
            &["dart".into(), "test".into(), "--coverage=coverage".into()],
            ManviActionKind::CoverageGenerator,
        )
        .unwrap();
        for argv in [
            vec!["swift".into(), "package".into(), "reset".into()],
            vec!["dart".into(), "pub".into(), "get".into()],
        ] {
            let err = validate_manvi_action(&argv, ManviActionKind::CoverageGenerator)
                .expect_err("non-test swift/dart must stay refused");
            assert!(err.contains("allowlist"), "{argv:?}: {err}");
        }
    }

    #[test]
    fn coverage_package_path_must_stay_inside_the_repository() {
        let repo = TempDir::new().unwrap();
        init_test_repo(repo.path());
        std::fs::create_dir_all(repo.path().join("ios")).unwrap();
        let inside = vec![
            "swift".to_string(),
            "test".to_string(),
            "--package-path".to_string(),
            "ios".to_string(),
            "--enable-code-coverage".to_string(),
        ];
        validate_manvi_action(&inside, ManviActionKind::CoverageGenerator).unwrap();
        validate_manvi_paths(repo.path(), &inside, ManviActionKind::CoverageGenerator)
            .expect("first-party --package-path must be accepted");
        let escape = vec![
            "swift".to_string(),
            "test".to_string(),
            "--package-path".to_string(),
            "../outside".to_string(),
            "--enable-code-coverage".to_string(),
        ];
        let err = validate_manvi_paths(repo.path(), &escape, ManviActionKind::CoverageGenerator)
            .expect_err("escaped --package-path must be refused");
        assert!(err.contains("escapes"), "{err}");
    }

    // REGRESSION GUARD: repo-local wrapper spellings are the only executable
    // paths the allowlist accepts. Path validation must still canonicalize the
    // wrapper itself, or ./gradlew can be a symlink to an executable outside
    // the open repository.
    #[cfg(unix)]
    #[test]
    fn coverage_wrapper_symlink_escape_is_refused() {
        let repo = TempDir::new().unwrap();
        init_test_repo(repo.path());
        let outside = TempDir::new().unwrap();
        let executable = outside.path().join("gradlew");
        std::fs::write(&executable, "#!/bin/sh\nexit 0\n").unwrap();
        std::os::unix::fs::symlink(&executable, repo.path().join("gradlew")).unwrap();
        let argv = vec!["./gradlew".into(), "test".into(), "jacocoTestReport".into()];

        validate_manvi_action(&argv, ManviActionKind::CoverageGenerator).unwrap();
        let err = validate_manvi_paths(repo.path(), &argv, ManviActionKind::CoverageGenerator)
            .expect_err("outside wrapper symlink must be refused");
        assert!(err.contains("escapes"), "{err}");
    }

    #[test]
    fn coverage_generator_allowlist_accepts_locked_llvm_cov_setup() {
        for argv in [
            vec![
                "cargo".into(),
                "install".into(),
                "cargo-llvm-cov".into(),
                "--locked".into(),
            ],
            vec![
                "cargo".into(),
                "install".into(),
                "--locked".into(),
                "cargo-llvm-cov".into(),
            ],
            vec![
                "rustup".into(),
                "component".into(),
                "add".into(),
                "llvm-tools-preview".into(),
            ],
        ] {
            validate_manvi_action(&argv, ManviActionKind::CoverageGenerator)
                .unwrap_or_else(|err| panic!("{argv:?} must be allowed: {err}"));
            let analysis = validate_manvi_action(&argv, ManviActionKind::Coverage)
                .expect_err("analysis must not install tools");
            assert!(analysis.contains("allowlist"), "{argv:?}: {analysis}");
            let health = validate_manvi_action(&argv, ManviActionKind::Health)
                .expect_err("health must not install llvm-cov");
            assert!(health.contains("allowlist"), "{argv:?}: {health}");
        }
    }

    #[test]
    fn coverage_generator_allowlist_refuses_arbitrary_cargo_install() {
        for argv in [
            vec!["cargo".into(), "install".into(), "cargo-llvm-cov".into()],
            vec![
                "cargo".into(),
                "install".into(),
                "cargo-tarpaulin".into(),
                "--locked".into(),
            ],
            vec![
                "cargo".into(),
                "install".into(),
                "cargo-llvm-cov".into(),
                "--locked".into(),
                "left-pad".into(),
            ],
            vec![
                "cargo".into(),
                "install".into(),
                "cargo-llvm-cov".into(),
                "--locked".into(),
                "--git".into(),
                "https://example.invalid/llvm-cov.git".into(),
            ],
            vec![
                "cargo".into(),
                "install".into(),
                "--path".into(),
                ".".into(),
                "cargo-llvm-cov".into(),
            ],
            vec![
                "rustup".into(),
                "component".into(),
                "add".into(),
                "rust-src".into(),
            ],
            vec![
                "rustup".into(),
                "toolchain".into(),
                "install".into(),
                "nightly".into(),
            ],
        ] {
            let err = validate_manvi_action(&argv, ManviActionKind::CoverageGenerator)
                .expect_err("arbitrary toolchain mutation must stay refused");
            assert!(
                err.contains("allowlist") || err.contains("URL") || err.contains("transports"),
                "{argv:?}: {err}"
            );
        }
    }

    #[test]
    fn manvi_action_argv_is_bounded_and_control_character_free() {
        let oversized = vec![
            "npm".into(),
            "test".into(),
            "x".repeat(TERMINAL_ARG_BYTES_CAP + 1),
        ];
        assert!(validate_manvi_action(&oversized, ManviActionKind::Coverage).is_err());
        assert!(validate_manvi_action(
            &["npm".into(), "test\nforged-log".into()],
            ManviActionKind::Coverage,
        )
        .is_err());
    }

    #[test]
    fn allowed_non_git_manvi_action_is_always_gated() {
        let dir = TempDir::new().unwrap();
        init_test_repo(dir.path());
        let result = run_manvi_action(
            dir.path().to_str().unwrap(),
            &["npm".into(), "test".into()],
            ManviActionKind::Health,
            Some(5),
        )
        .expect("an allowed shape should run to an ordinary process result");
        assert!(
            result.gated,
            "model-authored non-git commands must reach MANVI"
        );
        assert!(
            result.policy.is_some(),
            "a gated action must retain its verdict"
        );
        // Before the exit code: a run that timed out reports `None`, which would
        // satisfy `!= Some(0)` without `npm` ever having been asked anything.
        // This test cannot be taken off the spawn path — running the command is
        // the behaviour under test — so the least it can do is fail for the
        // reason it failed.
        assert!(
            !result.timed_out,
            "the host did not run the gated command inside its budget; \
             that is machine capacity, not the gate"
        );
        assert_ne!(
            result.exit_code,
            Some(0),
            "the fixture has no package.json test script"
        );
    }

    /// A task attempt starts in its recorded directory inside the checkout,
    /// with the checkout's root as the repository. A recorded directory that
    /// has since become a link — here into another trusted repository — is
    /// refused rather than followed to that repository's root.
    #[cfg(unix)]
    #[test]
    fn task_start_is_the_checkout_root_and_never_follows_a_swapped_link() {
        let dir = TempDir::new().unwrap();
        init_test_repo(dir.path());
        let root = dir.path().canonicalize().unwrap();
        std::fs::create_dir_all(root.join("pkg/web")).unwrap();
        let folder = root.join("pkg/web");
        assert_eq!(
            super::task_start(folder.to_str().unwrap()).unwrap(),
            (
                root.to_str().unwrap().to_owned(),
                Some("pkg/web".to_owned())
            )
        );
        assert_eq!(
            super::task_start(root.to_str().unwrap()).unwrap(),
            (root.to_str().unwrap().to_owned(), None)
        );

        let elsewhere = TempDir::new().unwrap();
        init_test_repo(elsewhere.path());
        let recorded = root.join("pkg/api");
        std::os::unix::fs::symlink(elsewhere.path(), &recorded).unwrap();
        let err = super::task_start(recorded.to_str().unwrap()).unwrap_err();
        assert!(err.contains("now resolves to"), "{err}");
        // And the start directory itself is held inside the root.
        let start = super::resolve_start_dir(&root, Some("pkg/api")).unwrap_err();
        assert!(start.contains("refused"), "{start}");
        assert!(super::task_start(elsewhere.path().join("nope").to_str().unwrap()).is_err());
    }

    #[cfg(unix)]
    #[test]
    fn manvi_path_options_refuse_symlink_escape() {
        let dir = TempDir::new().unwrap();
        init_test_repo(dir.path());
        let outside = TempDir::new().unwrap();
        std::os::unix::fs::symlink(outside.path(), dir.path().join("coverage-link")).unwrap();
        let repo = validate_repo(dir.path().to_str().unwrap()).unwrap();
        let args = vec![
            "cargo".into(),
            "llvm-cov".into(),
            "--output-path".into(),
            "coverage-link/lcov.info".into(),
        ];
        let err = validate_manvi_paths(&repo, &args, ManviActionKind::Coverage).unwrap_err();
        assert!(err.contains("escapes the open repository"), "{err}");

        let chdir = vec![
            "go".into(),
            "-C".into(),
            "coverage-link".into(),
            "test".into(),
            "./...".into(),
            "-coverprofile=coverage.out".into(),
        ];
        let chdir_err =
            validate_manvi_paths(&repo, &chdir, ManviActionKind::CoverageGenerator).unwrap_err();
        assert!(
            chdir_err.contains("escapes the open repository"),
            "{chdir_err}"
        );
    }

    /// The exclusion GitPulse appends when it recovers a pytest collection
    /// abort is a path, and must be proven to live inside the open repository
    /// like every other path argument. Before `--ignore` was listed, its value
    /// was the one path in a coverage command that reached pytest unchecked.
    #[test]
    fn pytest_ignore_paths_are_confined_to_the_repository() {
        let dir = TempDir::new().unwrap();
        init_test_repo(dir.path());
        // Held for the test's lifetime so the link keeps a live target. Only
        // the symlinked case below needs it; the absolute and `..` escapes are
        // portable, so Windows still checks those rather than skipping the
        // whole confinement contract.
        #[cfg(unix)]
        let _outside = {
            let outside = TempDir::new().unwrap();
            std::os::unix::fs::symlink(outside.path(), dir.path().join("bench-link")).unwrap();
            outside
        };
        let repo = validate_repo(dir.path().to_str().unwrap()).unwrap();

        let ok: Vec<String> = vec![
            "pytest".into(),
            "--cov".into(),
            "--ignore=bench/stress_test.py".into(),
        ];
        validate_manvi_paths(&repo, &ok, ManviActionKind::CoverageGenerator)
            .expect("a repository-relative exclusion must validate");

        // Built by chaining rather than pushing under `#[cfg]`: a `mut` binding
        // that is only mutated on one platform is an `unused_mut` error on the
        // other, and `-D warnings` makes that a build failure.
        #[cfg(unix)]
        let symlinked: &[&str] = &["--ignore=bench-link/stress_test.py"];
        #[cfg(not(unix))]
        let symlinked: &[&str] = &[];

        let escapes: Vec<String> = ["--ignore=/etc/passwd", "--ignore=../outside.py"]
            .iter()
            .chain(symlinked.iter())
            .map(|s| (*s).to_string())
            .collect();

        for escaping in escapes {
            let argv: Vec<String> = vec!["pytest".into(), escaping.clone()];
            let err = validate_manvi_paths(&repo, &argv, ManviActionKind::CoverageGenerator)
                .expect_err("an escaping exclusion must be refused");
            assert!(
                err.contains("escapes the open repository"),
                "{escaping}: {err}"
            );
        }

        let spaced: Vec<String> = vec!["pytest".into(), "--ignore".into(), "/etc/passwd".into()];
        let err = validate_manvi_paths(&repo, &spaced, ManviActionKind::CoverageGenerator)
            .expect_err("the separated spelling must be checked too");
        assert!(err.contains("escapes the open repository"), "{err}");
    }

    #[test]
    fn rejects_whitespace_only_program() {
        let dir = TempDir::new().unwrap();
        init_test_repo(dir.path());
        let res = run_terminal(
            dir.path().to_str().unwrap(),
            &["   ".into(), "status".into()],
            None,
        );
        assert!(res.is_err());
        assert_eq!(res.unwrap_err(), "Invalid program name");
    }

    #[test]
    fn handles_failed_command_exit_code() {
        let dir = TempDir::new().unwrap();
        init_test_repo(dir.path());
        let res = run_terminal(
            dir.path().to_str().unwrap(),
            &["git".into(), "log".into(), "nonexistent-branch-xyz".into()],
            Some(5),
        )
        .expect("runs git log");

        // A timeout reports `None`, which satisfies `!= Some(0)` without `git`
        // having refused anything, so the timeout is ruled out by name first.
        assert!(
            !res.timed_out,
            "the host did not run `git log` inside 5s; that is machine capacity, \
             not the exit-code path"
        );
        assert_ne!(res.exit_code, Some(0));
        assert!(!res.stderr_tail.is_empty());
    }

    /// A mutating git command must reach the harness gate, whether or not a
    /// harness is installed to answer.
    ///
    /// This deliberately does *not* assert that `git push --force` is refused.
    /// `harness::gated` documents a missing harness as the one unchecked
    /// verdict allowed to proceed, so the refusal only happens on a machine
    /// that actually has MANVI installed. Asserting it made the suite pass on
    /// a developer laptop with `manvi` on PATH and fail on every CI runner
    /// without it — the test was describing the machine, not the code.
    ///
    /// The invariant this module really owns is routing: a git command is
    /// never spawned ungated. Either the gate refuses it, or it comes back
    /// marked `gated` with the verdict attached.
    #[test]
    fn force_push_is_never_spawned_ungated() {
        let dir = TempDir::new().unwrap();
        init_test_repo(dir.path());
        let res = run_terminal(
            dir.path().to_str().unwrap(),
            &[
                "git".into(),
                "push".into(),
                "--force".into(),
                "origin".into(),
                "main".into(),
            ],
            Some(5),
        );

        match res {
            // A harness answered and blocked it: the refusal must name a reason
            // rather than being an opaque failure.
            Err(err) => assert!(
                err.contains("policy")
                    || err.contains("force")
                    || err.contains("blocked")
                    || err.contains("refused")
                    || err.contains("MANVI"),
                "refusal should name why it was refused, got: {err}"
            ),
            // No harness to answer: it may proceed, but it must still be
            // recorded as having gone through the gate, with the verdict kept.
            // `gated == false` here would mean a force push took the non-git
            // bypass path.
            Ok(run) => {
                assert!(run.gated, "a git command must never report itself ungated");
                assert!(
                    run.policy.is_some(),
                    "a gated command must carry the verdict it was judged by"
                );
            }
        }
    }

    /// PTY-free tests for the post-EOF finalize logic: the exit payload must
    /// carry the real status when wait succeeds and degrade honestly to
    /// `exit_code: None` when reaping fails.
    /// The child stopped because its reader thread could not start is
    /// reported by the one normaliser, so a signal death is not recorded as
    /// portable-pty's invented exit code 1 — an agent failure — beside the
    /// signal that says GitPulse stopped it.
    #[test]
    fn a_reader_failure_reports_a_signal_death_without_an_exit_code() {
        let signalled = reader_failure_payload(
            "term-r1",
            Ok(ExitStatusLike {
                code: 1,
                signal: Some("Terminated".into()),
            }),
            &"no threads left",
        );
        assert_eq!(signalled.exit_code, None);
        assert_eq!(signalled.signal, "Terminated");
        assert_eq!(
            signalled.error.as_deref(),
            Some("PTY reader failed: no threads left")
        );
        assert!(signalled.reaped);
        let lost = reader_failure_payload("term-r1", Err(std::io::Error::other("gone")), &"x");
        assert_eq!((lost.exit_code, lost.reaped), (None, false));
        assert_eq!(lost.error.as_deref(), Some("PTY reader failed: x"));
    }

    #[test]
    fn finalize_propagates_clean_exit_code() {
        let mut emitted = Vec::new();
        finalize_pty_session(
            "term-t1",
            || {
                Ok(ExitStatusLike {
                    code: 0,
                    signal: None,
                })
            },
            |p| emitted.push(p),
        );
        assert_eq!(emitted.len(), 1, "exactly one exit event");
        assert_eq!(emitted[0].id, "term-t1");
        assert_eq!(emitted[0].exit_code, Some(0));
        assert_eq!(emitted[0].signal, "");
    }

    #[test]
    fn finalize_propagates_nonzero_exit_code() {
        let mut emitted = Vec::new();
        finalize_pty_session(
            "term-t2",
            || {
                Ok(ExitStatusLike {
                    code: 127,
                    signal: None,
                })
            },
            |p| emitted.push(p),
        );
        assert_eq!(emitted[0].exit_code, Some(127));
    }

    #[test]
    fn finalize_surfaces_signal_name_from_status() {
        let mut emitted = Vec::new();
        finalize_pty_session(
            "term-t3",
            || {
                Ok(ExitStatusLike {
                    code: 1,
                    signal: Some("Hangup".to_string()),
                })
            },
            |p| emitted.push(p),
        );
        // The 1 is portable-pty's stand-in, not a status the shell returned:
        // a run record must not show "exit 1" for a killed shell.
        assert_eq!(emitted[0].exit_code, None);
        assert_eq!(emitted[0].signal, "Hangup");
        assert!(
            emitted[0].reaped,
            "a signal death is still a confirmed exit"
        );
    }

    #[test]
    fn only_a_closed_terminal_ends_a_session_quietly() {
        use std::io::{Error, ErrorKind};
        #[cfg(unix)]
        assert!(is_end_of_terminal(&Error::from_raw_os_error(libc::EIO)));
        assert!(is_end_of_terminal(&Error::from(ErrorKind::BrokenPipe)));
        assert!(is_end_of_terminal(&Error::from(ErrorKind::UnexpectedEof)));
        #[cfg(unix)]
        for fault in [libc::EBADF, libc::EFAULT, libc::ENOMEM, libc::EINVAL] {
            assert!(
                !is_end_of_terminal(&Error::from_raw_os_error(fault)),
                "errno {fault} is a fault and must be reported, not treated as an ending"
            );
        }
        assert!(!is_end_of_terminal(&Error::other("reader fault")));
    }

    #[test]
    fn finalize_reports_unknown_exit_when_wait_fails() {
        let mut emitted = Vec::new();
        finalize_pty_session(
            "term-t4",
            || Err(std::io::Error::other("waitpid failed")),
            |p| emitted.push(p),
        );
        assert_eq!(emitted.len(), 1);
        assert_eq!(
            emitted[0].exit_code, None,
            "failed reap must not invent a status"
        );
        assert_eq!(emitted[0].signal, "");
        assert!(emitted[0]
            .error
            .as_deref()
            .is_some_and(|e| e.contains("waitpid failed")));
    }

    /// The adapter must mirror the vendored portable-pty ExitStatus exactly:
    /// plain deaths expose `exit_code()`, signal deaths keep the fallback code
    /// plus the signal name (vendored lib.rs `From<std::process::ExitStatus>`).
    #[test]
    fn exit_status_like_adapter_matches_vendored_api() {
        let ok = portable_pty::ExitStatus::with_exit_code(42);
        let like = ExitStatusLike::from(&ok);
        assert_eq!(
            like,
            ExitStatusLike {
                code: 42,
                signal: None
            }
        );

        let killed = portable_pty::ExitStatus::with_signal("Terminated");
        let like = ExitStatusLike::from(&killed);
        assert_eq!(like.code, 1, "vendored fallback code for signals is 1");
        assert_eq!(like.signal.as_deref(), Some("Terminated"));
    }

    /// kill_session on an unknown id stays a silent no-op success — killing a
    /// session that already exited (and was removed by its reader thread) must
    /// not surface as an error to the frontend.
    #[test]
    fn kill_session_of_unknown_id_is_ok_noop() {
        let state = TerminalSessions::default();
        assert!(kill_session(&state, "term-missing").is_ok());
    }

    #[test]
    fn pty_dimensions_are_clamped_at_both_boundaries() {
        let minimum = bounded_pty_size(0, 0);
        assert_eq!((minimum.rows, minimum.cols), (1, 1));

        let maximum = bounded_pty_size(u16::MAX, u16::MAX);
        assert_eq!((maximum.rows, maximum.cols), (MAX_PTY_ROWS, MAX_PTY_COLS));
    }

    #[test]
    fn pty_launch_payloads_are_bounded_without_restricting_user_scripts() {
        assert!(validate_pty_options(
            Some("sh"),
            Some(&["-c".into(), "printf hello\nprintf world".into()]),
            None
        )
        .is_ok());
        assert!(validate_pty_options(Some(""), None, None).is_err());
        assert!(validate_pty_options(Some("sh\0bad"), None, None).is_err());
        assert!(validate_pty_options(None, Some(&vec!["x".into(); 257]), None).is_err());
        assert!(validate_pty_options(None, Some(&["x".repeat(16385)]), None).is_err());
        assert!(validate_pty_options(None, Some(&vec!["x".repeat(16384); 9]), None).is_err());
        assert!(validate_pty_options(
            None,
            None,
            Some(&HashMap::from([("A=B".into(), "value".into())]))
        )
        .is_err());
        assert!(validate_pty_options(
            None,
            None,
            Some(&HashMap::from([("A".into(), "x".repeat(16385))]))
        )
        .is_err());
    }

    #[test]
    fn erase_input_counts_do_not_retain_the_typed_bytes() {
        assert!(describe_erase_input(b"").is_none());
        assert!(describe_erase_input(b"secret").is_none());
        assert!(describe_erase_input(b"\x1b").is_none());
        let pure = describe_erase_input(b"\x7f").expect("del");
        assert_eq!((pure.erase, pure.printable, pure.other), (1, 0, 0));
        let bs = describe_erase_input(b"\x08").expect("bs");
        assert_eq!((bs.erase, bs.printable, bs.other), (1, 0, 0));
        let mixed = describe_erase_input(b"\x7fsecret\x08\x1b").expect("mixed");
        assert_eq!((mixed.erase, mixed.printable, mixed.other), (2, 6, 1));
        let high = describe_erase_input(b"\x7f\x80\xc3").expect("high");
        assert_eq!((high.erase, high.printable, high.other), (1, 2, 0));
        let rendered = format!("{} {} {}", mixed.erase, mixed.printable, mixed.other);
        assert!(!rendered.contains("secret"));
    }

    #[test]
    fn oversized_pty_input_is_refused_before_session_lookup() {
        let state = TerminalSessions::default();
        let input = "x".repeat(MAX_PTY_INPUT_BYTES + 1);
        let err = write_to_session(&state, "term-missing", &input).unwrap_err();
        assert!(
            err.contains("input"),
            "oversized input must name the input bound"
        );
        assert!(
            err.contains("limit"),
            "oversized input must name the per-write limit"
        );
    }

    #[test]
    fn log_sites_do_not_interpolate_session_identifiers() {
        let src = include_str!("mod.rs");
        let payloads = log_macro_payloads(src);
        assert!(
            payloads
                .iter()
                .any(|payload| payload.contains("could not reap"))
                && payloads
                    .iter()
                    .any(|payload| payload.contains("send output to its view"))
                && payloads
                    .iter()
                    .any(|payload| payload.contains("could not signal")),
            "expected the PTY warn sites: {payloads:?}"
        );
        for payload in &payloads {
            for ident in ["session_id", "sid_for_thread", "sid_for_exit"] {
                assert!(
                    !payload.contains(ident),
                    "log payload interpolates {ident}: {payload}"
                );
            }
        }
    }

    #[test]
    fn oversized_input_assertion_does_not_dump_the_error_string() {
        let src = include_str!("mod.rs");
        let start = src
            .find("fn oversized_pty_input_is_refused_before_session_lookup")
            .expect("test exists");
        let rest = &src[start..];
        let end = rest.find("\n    fn ").unwrap_or(rest.len());
        let test = &rest[..end];
        assert!(
            !test.contains("{err}"),
            "dumping the error interpolates a session-id-tainted string into the panic log"
        );
    }

    fn log_macro_payloads(src: &str) -> Vec<&str> {
        const KINDS: [&str; 4] = ["log::error!", "log::warn!", "log::info!", "log::debug!"];
        let mut out = Vec::new();
        let mut from = 0;
        while from < src.len() {
            let next = KINDS
                .iter()
                .filter_map(|kind| src[from..].find(kind).map(|rel| (rel, kind.len())))
                .min_by_key(|(rel, _)| *rel);
            let Some((rel, kind_len)) = next else {
                break;
            };
            let open_search = from + rel + kind_len;
            let Some(open_rel) = src[open_search..].find('(') else {
                break;
            };
            let open = open_search + open_rel;
            let mut depth = 0i32;
            let mut end = open;
            for (offset, ch) in src[open..].char_indices() {
                match ch {
                    '(' => depth += 1,
                    ')' => {
                        depth -= 1;
                        if depth == 0 {
                            end = open + offset;
                            break;
                        }
                    }
                    _ => {}
                }
            }
            out.push(&src[open..=end]);
            from = end.saturating_add(1);
        }
        out
    }

    #[test]
    fn pty_session_reservations_are_globally_bounded() {
        let state = TerminalSessions::default();
        assert_eq!(state.session_limit(), DEFAULT_PTY_SESSIONS);
        let reservations: Vec<_> = (0..DEFAULT_PTY_SESSIONS)
            .map(|_| reserve_session(&state).expect("within cap"))
            .collect();
        let err = reserve_session(&state).err().expect("cap must refuse");
        assert!(err.contains("limit"), "{err}");
        drop(reservations);
        assert!(
            reserve_session(&state).is_ok(),
            "released slots must be reusable"
        );
    }

    /// The limit is the user's and applies to the next session: raised, the
    /// sessions past the old limit open; lowered below what is open, those
    /// keep running and nothing new opens until enough close. Clones share
    /// it, because the app hands clones to every lane. It never leaves
    /// `1..=MAX_PTY_SESSIONS`, whatever it is asked for.
    #[test]
    fn the_session_limit_is_the_users_and_applies_to_the_next_session() {
        let state = TerminalSessions::default();
        let lane = state.clone();
        state.set_session_limit(DEFAULT_PTY_SESSIONS + 8);
        assert_eq!(lane.session_limit(), DEFAULT_PTY_SESSIONS + 8);
        let mut held: Vec<_> = (0..DEFAULT_PTY_SESSIONS + 8)
            .map(|_| reserve_session(&lane).expect("a raised limit refused"))
            .collect();
        let refused = reserve_session(&lane).err().expect("past the raised limit");
        assert!(
            refused.contains(&format!("({})", DEFAULT_PTY_SESSIONS + 8)),
            "{refused}"
        );
        assert!(refused.contains("Settings"), "{refused}");

        state.set_session_limit(4);
        assert!(
            reserve_session(&lane).is_err(),
            "opened past a lowered limit"
        );
        held.truncate(4);
        assert!(reserve_session(&lane).is_err(), "4 open of 4 must refuse");
        held.truncate(3);
        held.push(reserve_session(&lane).expect("a closed session freed its place"));
        drop(held);

        for (asked, kept) in [
            (0, 1),
            (1, 1),
            (MAX_PTY_SESSIONS, MAX_PTY_SESSIONS),
            (usize::MAX, MAX_PTY_SESSIONS),
        ] {
            state.set_session_limit(asked);
            assert_eq!(state.session_limit(), kept, "{asked}");
        }
        let all: Vec<_> = (0..MAX_PTY_SESSIONS)
            .map(|_| reserve_session(&state).expect("within the ceiling"))
            .collect();
        assert!(reserve_session(&state).is_err());
        drop(all);
    }

    // -- agent session attention ----------------------------------------------

    #[test]
    fn every_launcher_the_ui_offers_is_recognised_and_has_a_name_to_show() {
        // Derived from the list rather than repeating it: a launcher added to
        // AGENT_LAUNCHERS with no entry in the bridge's vocabulary would
        // otherwise produce banners for a CLI the hook path cannot name.
        for kind in AGENT_LAUNCHERS {
            assert_eq!(launcher_kind(kind), Some(kind));
            // The same name as an absolute path, which is what resolution
            // actually hands us on a GUI launch.
            assert_eq!(
                launcher_kind(&format!("/opt/homebrew/bin/{kind}")),
                Some(kind)
            );
            let label = agent_launcher(kind)
                .unwrap_or_else(|| panic!("{kind} has no display name in alerts::bridge::AGENTS"));
            assert!(!label.is_empty());
        }
        assert_eq!(agent_launcher("claude"), Some("Claude Code"));
    }

    #[test]
    fn a_shell_is_not_an_agent_however_its_path_is_spelled() {
        for program in [
            "/bin/zsh",
            "/bin/bash",
            "/usr/local/bin/fish",
            // The defect the stem match exists for: a user called claude.
            "/Users/claude/bin/zsh",
            "/Users/codex/.local/bin/bash",
            "/opt/grok-tools/bin/sh",
            "",
        ] {
            assert_eq!(agent_launcher(program), None, "{program}");
        }
    }

    #[test]
    fn a_watcher_turns_an_agent_bell_into_a_notice_and_a_shell_bell_into_a_flagged_one() {
        let agent_events = Arc::new(RecordedEvents::default());
        let mut agent = SessionWatcher::new(
            "term-1",
            "/opt/homebrew/bin/claude",
            std::path::Path::new("/w/x"),
            agent_events.clone(),
        );
        assert!(agent.is_agent);
        assert_eq!(agent.label, "Claude Code");
        assert_eq!(agent.place.as_deref(), Some("x"));

        let shell_events = Arc::new(RecordedEvents::default());
        let mut shell = SessionWatcher::new(
            "term-2",
            "/bin/zsh",
            std::path::Path::new("/w/x"),
            shell_events.clone(),
        );
        assert!(!shell.is_agent);
        assert_eq!(shell.label, "Terminal");

        // What is asserted is that a stream of hostile bytes neither panics
        // nor is retained, and that every notice goes to the session's own
        // events, named for it.
        for _ in 0..64 {
            agent.observe(b"\x1b]9;4;1;40\x07\x07\x1b]0;title\x07");
            shell.observe(&[0x1b, b']', b'9', b';']);
        }
        assert_eq!(agent.reported_drops, agent.scanner.dropped());
        let notices = agent_events.notices.lock().unwrap();
        assert!(!notices.is_empty(), "an agent's bell is a notice");
        assert!(notices
            .iter()
            .all(|n| n.key == "term-1" && n.is_agent && n.label == "Claude Code"));
        drop(notices);
        assert!(shell_events.notices.lock().unwrap().is_empty());
        agent.ended();
        assert_eq!(
            agent_events.notices.lock().unwrap().last().map(|n| n.event),
            Some(Some("session_ended"))
        );
    }

    /// Every event an engine session reports, kept for a test to read.
    #[derive(Default)]
    struct RecordedEvents {
        notices: Mutex<Vec<crate::alerts::Notice>>,
        losses: Mutex<Vec<TerminalLossPayload>>,
        exits: Mutex<Vec<SessionEnded>>,
    }

    impl SessionEvents for RecordedEvents {
        fn loss(&self, payload: TerminalLossPayload) {
            self.losses.lock().unwrap().push(payload);
        }
        fn notice(&self, notice: crate::alerts::Notice) {
            self.notices.lock().unwrap().push(notice);
        }
        fn scan_drops(&self, _: u64) {}
        fn exit(&self, ended: SessionEnded) {
            self.exits.lock().unwrap().push(ended);
        }
    }

    #[test]
    fn a_session_tells_its_own_hooks_which_session_and_socket_to_use() {
        // The values are set on the command after the IPC caller's `env` map,
        // so a caller cannot redirect an agent's hooks somewhere else.
        let dir = tempfile::tempdir().unwrap();
        let caller = HashMap::from([
            (
                crate::alerts::bridge::SESSION_ENV.to_string(),
                "term-999".to_string(),
            ),
            (
                crate::alerts::bridge::SOCKET_ENV.to_string(),
                "/tmp/attacker.sock".to_string(),
            ),
        ]);
        let PtyCommand { mut cmd, .. } = build_pty_command(
            "claude",
            false,
            None,
            Some(&caller),
            dir.path(),
            &gui_launch_env(dir.path()),
        );
        // What `spawn_session_inner` does after building the command.
        cmd.env(crate::alerts::bridge::SESSION_ENV, "term-7-1a");
        match crate::alerts::bridge::socket_path() {
            Some(path) => cmd.env(crate::alerts::bridge::SOCKET_ENV, path),
            None => cmd.env_remove(crate::alerts::bridge::SOCKET_ENV),
        }
        assert_eq!(
            env_of(&cmd, crate::alerts::bridge::SESSION_ENV).as_deref(),
            Some("term-7-1a")
        );
        let socket = env_of(&cmd, crate::alerts::bridge::SOCKET_ENV);
        assert_ne!(socket.as_deref(), Some("/tmp/attacker.sock"));
        assert!(
            socket.is_none_or(|s| s.ends_with(crate::alerts::bridge::SOCKET_NAME)),
            "the socket variable pointed somewhere that is not this app's socket"
        );
    }

    // -- interactive PTY environment (GUI-launch repairs) ---------------------

    /// A Finder/Dock-style launch: launchd's minimal PATH, no TERM, no locale.
    fn gui_launch_env(home: &std::path::Path) -> PtyEnv {
        PtyEnv {
            vars: HashMap::from([
                (
                    "PATH".to_string(),
                    "/usr/bin:/bin:/usr/sbin:/sbin".to_string(),
                ),
                ("HOME".to_string(), home.to_string_lossy().into_owned()),
            ]),
            inject_locale: true,
        }
    }

    fn env_of(cmd: &CommandBuilder, key: &str) -> Option<String> {
        cmd.get_env(key).map(|v| v.to_string_lossy().into_owned())
    }

    fn argv_of(cmd: &CommandBuilder) -> Vec<String> {
        cmd.get_argv()
            .iter()
            .map(|a| a.to_string_lossy().into_owned())
            .collect()
    }

    /// The defect the user saw first: `clear` reporting "TERM environment
    /// variable not set". `CommandBuilder` copies the parent environment
    /// verbatim and never sets TERM, and a GUI-launched app has none — so the
    /// shell believed it had no terminal at all while xterm.js sat there
    /// implementing a full one.
    #[test]
    fn gui_launched_session_declares_the_terminal_it_actually_has() {
        let home = tempfile::tempdir().unwrap();
        let cmd = build_pty_command(
            "/bin/zsh",
            true,
            None,
            None,
            home.path(),
            &gui_launch_env(home.path()),
        )
        .cmd;
        assert_eq!(env_of(&cmd, "TERM").as_deref(), Some("xterm-256color"));
        assert_eq!(env_of(&cmd, "COLORTERM").as_deref(), Some("truecolor"));
        assert_eq!(env_of(&cmd, "LANG").as_deref(), Some("en_US.UTF-8"));
    }

    /// An inherited value is better information than a default: a build
    /// started from a real terminal already has the truth. "Wins" here means
    /// *nothing is added* — the child then inherits it untouched. Asserted
    /// against the explicitly-set set rather than the effective environment,
    /// because `CommandBuilder`'s base is the real process and only the
    /// overlay is this function's decision to make.
    #[test]
    fn an_inherited_terminal_env_is_left_untouched() {
        let home = tempfile::tempdir().unwrap();
        let mut inherited = gui_launch_env(home.path());
        inherited
            .vars
            .insert("TERM".to_string(), "screen-256color".to_string());
        inherited
            .vars
            .insert("LANG".to_string(), "de_DE.UTF-8".to_string());
        let cmd = build_pty_command("/bin/zsh", true, None, None, home.path(), &inherited).cmd;
        let overlay: HashMap<&str, &str> = cmd.iter_extra_env_as_str().collect();
        assert!(!overlay.contains_key("TERM"), "overlay: {overlay:?}");
        assert!(!overlay.contains_key("LANG"), "overlay: {overlay:?}");
        // COLORTERM was genuinely absent, so it is still repaired.
        assert_eq!(overlay.get("COLORTERM"), Some(&"truecolor"));
    }

    /// An IPC caller that names a value meant it, including a value that
    /// contradicts the default.
    #[test]
    fn a_caller_supplied_value_overrides_the_default() {
        let home = tempfile::tempdir().unwrap();
        let caller = HashMap::from([("TERM".to_string(), "dumb".to_string())]);
        let cmd = build_pty_command(
            "/bin/zsh",
            true,
            None,
            Some(&caller),
            home.path(),
            &gui_launch_env(home.path()),
        )
        .cmd;
        assert_eq!(env_of(&cmd, "TERM").as_deref(), Some("dumb"));
    }

    /// An empty inherited value is not a value: `TERM=` leaves the child just
    /// as terminal-less as no TERM at all.
    #[test]
    fn blank_inherited_terminal_env_is_treated_as_unset() {
        let vars = HashMap::from([("TERM".to_string(), "  ".to_string())]);
        let injected = missing_terminal_env(|n| vars.get(n).cloned(), false);
        assert!(injected.contains(&("TERM", "xterm-256color")));
    }

    /// A locale is repaired only where the repair is safe. On Linux an
    /// ungenerated `en_US.UTF-8` makes every child warn — worse than the C
    /// locale it replaced — so only macOS, whose GUI launches genuinely have
    /// no locale and which always ships that one, gets it.
    #[test]
    fn locale_is_injected_only_where_it_is_guaranteed_to_exist() {
        let empty: HashMap<String, String> = HashMap::new();
        let names = |inject| {
            missing_terminal_env(|n| empty.get(n).cloned(), inject)
                .into_iter()
                .map(|(k, _)| k)
                .collect::<Vec<_>>()
        };
        assert!(names(true).contains(&"LANG"));
        assert!(!names(false).contains(&"LANG"));
    }

    /// Any locale variable already in force means the child has one; adding
    /// LANG beside an inherited LC_ALL would silently change its meaning.
    #[test]
    fn any_inherited_locale_variable_suppresses_the_floor() {
        for name in ["LANG", "LC_ALL", "LC_CTYPE"] {
            let vars = HashMap::from([(name.to_string(), "de_DE.UTF-8".to_string())]);
            let injected = missing_terminal_env(|n| vars.get(n).cloned(), true);
            assert!(
                !injected.iter().any(|(k, _)| *k == "LANG"),
                "{name} already sets the locale"
            );
        }
    }

    /// The second defect: the shell started non-login, so zsh never read
    /// `~/.zprofile` — where `brew shellenv` and the user's PATH exports live
    /// — and `~/.zshrc` then died on its first call to an installed tool.
    // `login_flag` returns None on Windows by design, so this asserts Unix
    // behaviour — the same gate `login_flag_covers_known_shells_only` carries.
    #[cfg(unix)]
    #[test]
    fn the_user_shell_starts_as_a_login_shell() {
        let home = tempfile::tempdir().unwrap();
        let cmd = build_pty_command(
            "/bin/zsh",
            true,
            None,
            None,
            home.path(),
            &gui_launch_env(home.path()),
        )
        .cmd;
        assert_eq!(
            argv_of(&cmd),
            vec!["/bin/zsh".to_string(), "-l".to_string()]
        );
    }

    /// An agent CLI is not a shell: `claude -l` would fail to start.
    #[test]
    fn an_agent_cli_never_receives_the_login_flag() {
        let home = tempfile::tempdir().unwrap();
        let bin = home.path().join(".local/bin");
        std::fs::create_dir_all(&bin).unwrap();
        let claude = bin.join("claude");
        std::fs::write(&claude, "#!/bin/sh\nexit 0\n").unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&claude, std::fs::Permissions::from_mode(0o755)).unwrap();
        }
        let cmd = build_pty_command(
            "claude",
            false,
            Some(&["--resume".to_string()]),
            None,
            home.path(),
            &gui_launch_env(home.path()),
        );
        assert!(
            !argv_of(&cmd.cmd).contains(&"-l".to_string()),
            "argv: {:?}",
            argv_of(&cmd.cmd)
        );
        assert_eq!(
            argv_of(&cmd.cmd).last().map(String::as_str),
            Some("--resume")
        );
    }

    /// The third defect: agent launchers were bare names resolved against the
    /// app's own PATH, which under a GUI launch omits every directory a CLI is
    /// installed into. GitPulse already owns this repair for every other
    /// subsystem; the terminal simply was not using it.
    #[cfg(unix)]
    #[test]
    fn an_agent_cli_outside_the_gui_path_still_resolves_and_the_child_can_find_it() {
        use std::os::unix::fs::PermissionsExt;
        let home = tempfile::tempdir().unwrap();
        let bin = home.path().join(".local/bin");
        std::fs::create_dir_all(&bin).unwrap();
        let codex = bin.join("codex");
        std::fs::write(&codex, "#!/bin/sh\nexit 0\n").unwrap();
        std::fs::set_permissions(&codex, std::fs::Permissions::from_mode(0o755)).unwrap();

        let built = build_pty_command(
            "codex",
            false,
            None,
            None,
            home.path(),
            &gui_launch_env(home.path()),
        );
        // Resolved for this process...
        assert_eq!(built.resolved, codex.to_string_lossy());
        assert_eq!(
            argv_of(&built.cmd).first().map(String::as_str),
            Some(&*built.resolved)
        );
        // ...and reachable by the child's own nested lookups.
        let child_path = env_of(&built.cmd, "PATH").expect("child PATH is set");
        assert!(
            std::env::split_paths(&child_path).any(|p| p == bin),
            "child PATH {child_path} must contain {}",
            bin.display()
        );
        // The inherited entries are still there, ahead of the fallbacks.
        assert!(std::env::split_paths(&child_path).any(|p| p == std::path::Path::new("/usr/bin")));
    }

    /// A name that resolves nowhere passes through unchanged, so the spawn
    /// failure keeps naming the tool the user asked for rather than a path
    /// nothing was ever installed at.
    #[test]
    fn an_unresolvable_program_is_not_given_an_invented_location() {
        let home = tempfile::tempdir().unwrap();
        let built = build_pty_command(
            "definitely-not-installed-xyzzy",
            false,
            None,
            None,
            home.path(),
            &gui_launch_env(home.path()),
        );
        assert_eq!(built.resolved, "definitely-not-installed-xyzzy");
    }

    /// The flag is applied from a closed set: an unrecognised `$SHELL` handed
    /// an unknown flag would not start at all, and a terminal that will not
    /// open is worse than one with a short PATH.
    #[cfg(unix)]
    #[test]
    fn login_flag_covers_known_shells_only() {
        for shell in ["/bin/zsh", "/bin/bash", "/opt/homebrew/bin/fish", "sh"] {
            assert_eq!(login_flag(shell), Some("-l"), "{shell}");
        }
        for other in ["/usr/local/bin/nu", "/usr/bin/python3", "", "/"] {
            assert_eq!(login_flag(other), None, "{other}");
        }
    }

    #[test]
    fn every_pty_session_runs_in_the_repository() {
        let home = tempfile::tempdir().unwrap();
        let cmd = build_pty_command(
            "/bin/zsh",
            true,
            None,
            None,
            home.path(),
            &gui_launch_env(home.path()),
        )
        .cmd;
        assert_eq!(
            cmd.get_cwd().map(std::path::PathBuf::from),
            Some(home.path().to_path_buf())
        );
    }

    #[test]
    fn default_shell_is_non_empty() {
        let shell = default_shell();
        assert!(!shell.trim().is_empty());
    }

    /// Characterization (host-load governor, Part 0), recorded on the code as
    /// it stood: a terminal launch is admitted on the session count alone.
    /// `reserve_session` takes no host-load input, so a launch is admitted at
    /// memory-pressure level 4 exactly as at level 1. Admission under load is
    /// card 1b's change; this pins the starting point it changes.
    #[test]
    fn characterization_a_launch_is_admitted_on_the_session_count_alone() {
        let state = TerminalSessions::default();
        let limit = state.session_limit();
        let held: Vec<_> = (0..limit)
            .map(|_| reserve_session(&state).expect("admitted under the limit"))
            .collect();
        assert!(
            reserve_session(&state).is_err(),
            "the count is the only gate"
        );
        drop(held);
        assert!(reserve_session(&state).is_ok());
    }

    /// Characterization (host-load governor, Part 0): a `claude` started in a
    /// GitPulse terminal is invisible to the registry-based session count
    /// unless Claude Code wrote a registry entry where GitPulse looks — and an
    /// entry under another `CLAUDE_CONFIG_DIR` is not where it looks.
    #[cfg(unix)]
    #[test]
    fn characterization_a_terminal_claude_with_no_registry_entry_goes_uncounted() {
        let dir = tempfile::tempdir().unwrap();
        let repo = dir.path().canonicalize().unwrap();
        init_test_repo(&repo);
        let bin = tempfile::tempdir().unwrap();
        let claude = bin.path().join("claude");
        std::os::unix::fs::symlink("/bin/sleep", &claude).unwrap();
        let app = tauri::test::mock_builder().build(crate::context()).unwrap();
        let state = TerminalSessions::default();
        let spawned = spawn_session_inner(
            app.handle(),
            &state,
            repo.to_str().unwrap(),
            None,
            24,
            80,
            Some(claude.to_string_lossy().into_owned()),
            Some(vec!["30".into()]),
            None,
            discard(),
            None,
        )
        .unwrap();
        let deadline = std::time::Instant::now() + Duration::from_secs(5);
        let mut running = Vec::new();
        while running.is_empty() && std::time::Instant::now() < deadline {
            running = foreground::working_in(&repo).unwrap();
            thread::sleep(Duration::from_millis(20));
        }
        let registry = tempfile::tempdir().unwrap();
        let seen = crate::insights::live_sessions::observe_with(
            Some(registry.path()),
            &crate::workbench::process_birth::running_since,
        );
        let facet =
            crate::insights::live_sessions::attribute(&seen, &[repo.to_string_lossy().into()]);
        kill_session(&state, &spawned.id).unwrap();
        assert!(!running.is_empty(), "the terminal claude never started");
        assert_eq!(facet.sessions, 0, "{facet:?}");
    }

    /// The fix for the characterization above. A `claude` started in a
    /// GitPulse terminal, with no registry entry anywhere, is counted — by its
    /// executable — and counted as launched, through the session id the PTY
    /// seam gave it. The fixture is this test binary behind a symlink named
    /// `claude`, because macOS hides the environment of Apple platform
    /// binaries such as `/bin/sleep`.
    #[cfg(any(target_os = "macos", target_os = "linux"))]
    #[test]
    fn a_terminal_claude_with_no_registry_entry_is_counted_as_launched() {
        let dir = tempfile::tempdir().unwrap();
        let repo = dir.path().canonicalize().unwrap();
        init_test_repo(&repo);
        let bin = tempfile::tempdir().unwrap();
        let claude = bin.path().join("claude");
        let (exe, args) = crate::sysload::procscan::tests::fixture_command();
        std::os::unix::fs::symlink(&exe, &claude).unwrap();
        let pidfile = bin.path().join("claude.pid");
        let app = tauri::test::mock_builder().build(crate::context()).unwrap();
        let state = TerminalSessions::default();
        let spawned = spawn_session_inner(
            app.handle(),
            &state,
            repo.to_str().unwrap(),
            None,
            24,
            80,
            Some(claude.to_string_lossy().into_owned()),
            Some(args),
            Some(HashMap::from([(
                crate::sysload::procscan::tests::FIXTURE_ENV.to_string(),
                format!("sleep=30,pidfile={}", pidfile.display()),
            )])),
            discard(),
            None,
        )
        .unwrap();
        let pid = crate::sysload::procscan::tests::fixture_pid(&pidfile);
        let seen = crate::insights::live_sessions::observe();
        let facet =
            crate::insights::live_sessions::attribute(&seen, &[repo.to_string_lossy().into()]);
        let scanned = crate::sysload::procscan::observe_sessions().unwrap();
        let probed = crate::sysload::procscan::probe_one(pid);
        kill_session(&state, &spawned.id).unwrap();
        let ours: Vec<_> = facet
            .worktrees
            .iter()
            .flat_map(|w| &w.sessions)
            .filter(|s| s.pid == pid as u32)
            .collect();
        assert_eq!(ours.len(), 1, "{facet:?}\n{probed:?}\n{scanned:?}");
        assert_eq!(ours[0].kind, "claude");
        let session = scanned.iter().find(|s| s.pid == pid).expect("scanned");
        assert!(
            session
                .tree
                .as_deref()
                .is_some_and(|t| t.starts_with("session:term-")),
            "launched through the PTY seam: {session:?}"
        );
    }
}
