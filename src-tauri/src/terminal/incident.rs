//! Mass-death incidents: one record when many sessions are killed from outside.
//!
//! On 2026-10-12 about twenty hosted agent sessions died within one second of
//! an external SIGTERM, and the only trace in GitPulse's log was twenty
//! unrelated "session ended" lines. Finding the sender took an hour in the
//! macOS unified log. This module keeps the first half of that forensics
//! automatic: it notices the pattern, records it once, and asks the unified
//! log who sent the signal.
//!
//! Three layers, each testable alone:
//!
//! * [`Aggregator`] is pure and deterministic. It is fed [`SessionEnd`]s and
//!   an explicit clock, and answers with [`Incident`]s. It owns no threads.
//! * [`parse_launchd_ndjson`] and [`attribute_with`] turn `log show` output
//!   into a sender, or into a note saying why there is none.
//! * [`Recorder`] is the only impure part: one lazily started worker thread
//!   that exists only while a group is pending, an attribution thread per
//!   *incident* (never per end), a bounded history, the log line and the event.
//!
//! What counts: a session that ended from a signal nobody inside GitPulse sent.
//! The caller decides that (a session closed through GitPulse has its `dead`
//! flag set before its reader finishes); this module only counts.

use serde::Serialize;
use std::collections::{HashMap, VecDeque};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Condvar, Mutex, MutexGuard};
use std::time::{Duration, Instant};

/// How close together deaths by one signal must be to be one incident.
///
/// Measured from the group's first end, on a monotonic clock. The observed
/// event spread twenty deaths over under one second (a `pkill` walks its
/// matches in a loop), and ordinary independent deaths by one signal are
/// minutes apart, so 2 s separates the two with a wide margin either side. It
/// is anchored rather than sliding on purpose: a sliding window lets a slow
/// trickle chain into one unbounded incident, and an anchored one has a fixed
/// end that the worker can sleep to.
pub const WINDOW: Duration = Duration::from_secs(2);
/// Fewer than this is an ordinary death, not an incident. Two agents closed by
/// one SIGTERM can be a coincidence; three is a pattern.
pub const THRESHOLD: usize = 3;
/// Ends held across every open group. Past it a group keeps counting but stops
/// remembering who, and the incident says so.
pub const MAX_PENDING_ENDS: usize = 1024;
/// Distinct signals tracked at once. A fourth hundred signal numbers cannot
/// exist; this bounds the map against a hostile caller anyway.
pub const MAX_GROUPS: usize = 16;
/// Session ids one incident reports. The true count is always carried beside it.
pub const MAX_MEMBERS: usize = 512;
/// Incidents kept for [`Recorder::recent`].
pub const MAX_STORED: usize = 32;
/// Attributions allowed to run at once. The rest are recorded without a sender
/// rather than queueing behind a slow `log show`.
const MAX_ATTRIBUTIONS: usize = 2;
/// How long the unified-log query may run.
///
/// Measured, not guessed: `log show` pays a fixed 4-7 s to open the log store
/// on this machine whatever the window (a 6 s window an instant ago took
/// 4.1 s; the 2026-10-12 00:46 window took 7.3 s), so a 3 s deadline timed
/// out on every query, including the integration test's. The query runs on
/// its own thread and delays only the incident's event, never a session or
/// the UI, so 12 s buys the answer at no cost to anything waiting.
#[cfg(target_os = "macos")]
const QUERY_DEADLINE: Duration = Duration::from_secs(12);
/// How much query output is kept, and the longest single line parsed.
#[cfg(target_os = "macos")]
const QUERY_OUTPUT_CAP: usize = 4 * 1024 * 1024;
const MAX_LINE_BYTES: usize = 256 * 1024;
/// How far before the first death, and after the last, the log is searched.
#[cfg(target_os = "macos")]
const LOOKBACK_MS: u64 = 2_000;
#[cfg(target_os = "macos")]
const LOOKAHEAD_MS: u64 = 3_000;

/// One session's end, as the terminal's reader thread reports it.
#[derive(Debug, Clone)]
pub struct SessionEnd {
    /// Monotonic time of the end.
    pub at: Instant,
    /// Wall-clock milliseconds since the epoch, for display and the log query.
    pub wall_ms: u64,
    pub session_id: String,
    pub program: String,
    pub cwd: String,
    pub exit_code: Option<i32>,
    /// The raw signal text the PTY layer reported, e.g. `Terminated: 15`.
    pub signal: String,
}

/// Why [`Aggregator::record`] did not take an end.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Rejected {
    /// Not a signal death, so not this module's business.
    NoSignal,
    /// Older than an end already seen. A clock that runs backwards cannot be
    /// windowed, so the end is refused rather than guessed into a group.
    NonMonotonic,
    /// More distinct signals are open than [`MAX_GROUPS`].
    TooManyGroups,
}

/// One session in an incident.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Member {
    pub session_id: String,
    pub program: String,
    pub cwd: String,
    pub exit_code: Option<i32>,
}

/// A closed group that reached the threshold.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Incident {
    pub id: String,
    pub signal: String,
    /// Wall-clock milliseconds of the first end.
    pub at_ms: u64,
    /// Milliseconds from the first end to the last.
    pub span_ms: u64,
    /// At most [`MAX_MEMBERS`] of them.
    pub members: Vec<Member>,
    /// How many ended, whether or not they were remembered.
    pub total: usize,
}

struct Group {
    first_at: Instant,
    last_at: Instant,
    first_wall_ms: u64,
    members: Vec<Member>,
    total: usize,
}

/// Groups session ends into incidents. Pure: time is whatever the caller says.
pub struct Aggregator {
    window: Duration,
    threshold: usize,
    groups: HashMap<String, Group>,
    closed: Vec<Incident>,
    high_water: Option<Instant>,
    pending: usize,
    seq: u64,
}

impl Aggregator {
    pub fn new(window: Duration, threshold: usize) -> Self {
        Self {
            window,
            threshold: threshold.max(1),
            groups: HashMap::new(),
            closed: Vec::new(),
            high_water: None,
            pending: 0,
            seq: 0,
        }
    }

    /// Takes one end. A group stays open for `end.at - first <= window`
    /// (inclusive); an end after that closes it and starts the next.
    pub fn record(&mut self, end: SessionEnd) -> Result<(), Rejected> {
        if end.signal.trim().is_empty() {
            return Err(Rejected::NoSignal);
        }
        if self.high_water.is_some_and(|seen| end.at < seen) {
            return Err(Rejected::NonMonotonic);
        }
        self.high_water = Some(end.at);
        let expired = self
            .groups
            .get(&end.signal)
            .is_some_and(|g| end.at.duration_since(g.first_at) > self.window);
        if expired {
            self.close(&end.signal);
        }
        if !self.groups.contains_key(&end.signal) && self.groups.len() >= MAX_GROUPS {
            return Err(Rejected::TooManyGroups);
        }
        let remember = self.pending < MAX_PENDING_ENDS;
        let group = self.groups.entry(end.signal.clone()).or_insert(Group {
            first_at: end.at,
            last_at: end.at,
            first_wall_ms: end.wall_ms,
            members: Vec::new(),
            total: 0,
        });
        group.last_at = end.at;
        group.total += 1;
        if remember && group.members.len() < MAX_MEMBERS {
            group.members.push(Member {
                session_id: end.session_id,
                program: end.program,
                cwd: end.cwd,
                exit_code: end.exit_code,
            });
            self.pending += 1;
        }
        Ok(())
    }

    fn close(&mut self, signal: &str) {
        let Some(group) = self.groups.remove(signal) else {
            return;
        };
        self.pending = self.pending.saturating_sub(group.members.len());
        if group.total < self.threshold {
            return;
        }
        self.seq += 1;
        self.closed.push(Incident {
            id: format!("inc-{}-{}", group.first_wall_ms, self.seq),
            signal: signal.to_string(),
            at_ms: group.first_wall_ms,
            span_ms: u64::try_from(group.last_at.duration_since(group.first_at).as_millis())
                .unwrap_or(u64::MAX),
            members: group.members,
            total: group.total,
        });
    }

    /// Closes every group whose window ended before `now` and returns the
    /// incidents, oldest first. A group is still open at exactly
    /// `first + window`, because an end at that instant still joins it.
    pub fn drain_ready(&mut self, now: Instant) -> Vec<Incident> {
        let mut due: Vec<(Instant, String)> = self
            .groups
            .iter()
            .filter(|(_, g)| now.saturating_duration_since(g.first_at) > self.window)
            .map(|(signal, g)| (g.first_at, signal.clone()))
            .collect();
        due.sort();
        for (_, signal) in due {
            self.close(&signal);
        }
        let mut out = std::mem::take(&mut self.closed);
        out.sort_by_key(|i| i.at_ms);
        out
    }

    /// The earliest instant at which [`Self::drain_ready`] will have work.
    pub fn next_deadline(&self) -> Option<Instant> {
        if !self.closed.is_empty() {
            return Some(Instant::now());
        }
        self.groups
            .values()
            .map(|g| g.first_at + self.window + Duration::from_millis(1))
            .min()
    }

    /// Ends held in open groups.
    pub fn pending(&self) -> usize {
        self.pending
    }
}

// -- attribution --------------------------------------------------------------

/// A process the unified log names as a signal's sender.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Sender {
    pub pid: u32,
    pub name: String,
}

/// The sender, or the reason there is none.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Attribution {
    pub sender: Option<Sender>,
    pub note: String,
}

impl Attribution {
    fn none(note: impl Into<String>) -> Self {
        Self {
            sender: None,
            note: note.into(),
        }
    }
}

/// launchd's name for a signal, from the PTY layer's text (`Terminated: 15`).
/// The number is authoritative when present; the words cover a platform that
/// prints none. Numbers are macOS's, the only platform the query exists on.
pub fn launchd_signal_name(reported: &str) -> Option<&'static str> {
    let number = reported
        .rsplit(':')
        .next()
        .and_then(|n| n.trim().parse::<i32>().ok());
    if let Some(n) = number {
        return match n {
            1 => Some("SIGHUP"),
            2 => Some("SIGINT"),
            3 => Some("SIGQUIT"),
            6 => Some("SIGABRT"),
            9 => Some("SIGKILL"),
            14 => Some("SIGALRM"),
            15 => Some("SIGTERM"),
            30 => Some("SIGUSR1"),
            31 => Some("SIGUSR2"),
            _ => None,
        };
    }
    let word = reported.split(':').next().unwrap_or("").trim();
    match word {
        "Hangup" => Some("SIGHUP"),
        "Interrupt" => Some("SIGINT"),
        "Quit" => Some("SIGQUIT"),
        "Killed" => Some("SIGKILL"),
        "Terminated" => Some("SIGTERM"),
        _ => None,
    }
}

/// What [`parse_launchd_ndjson`] found.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Parsed {
    /// Lines that matched the signal and named a sender.
    pub matching: usize,
    /// Lines that were not valid JSON, were oversized, or lacked a message.
    pub unreadable: usize,
    /// The most-cited sender, its citation count, and the first line citing it.
    pub best: Option<(Sender, usize, String)>,
}

#[derive(serde::Deserialize)]
struct LogRow {
    #[serde(rename = "eventMessage")]
    event_message: Option<String>,
}

/// Reads `exited due to <SIG> | sent by <name>[<pid>]` out of one message.
fn cited_sender(message: &str, signal_name: &str) -> Option<Sender> {
    let after = message.split_once("exited due to ")?.1;
    let (signal, rest) = after.split_once(|c: char| c.is_whitespace() || c == '|')?;
    if signal != signal_name {
        return None;
    }
    let sent = rest.split_once("sent by ")?.1;
    let (name, tail) = sent.split_once('[')?;
    let (pid, _) = tail.split_once(']')?;
    let name = name.trim();
    if name.is_empty() || name.len() > 128 || name.chars().any(char::is_control) {
        return None;
    }
    Some(Sender {
        pid: pid.trim().parse().ok()?,
        name: name.to_string(),
    })
}

/// Counts which sender launchd cites for `signal_name` in `log show --style
/// ndjson` output. Tolerates truncation, garbage and giant lines: each line
/// stands alone and a bad one is counted, never fatal.
pub fn parse_launchd_ndjson(bytes: &[u8], signal_name: &str) -> Parsed {
    let mut parsed = Parsed::default();
    let mut counts: Vec<(Sender, usize, String)> = Vec::new();
    for line in bytes.split(|b| *b == b'\n') {
        if line.iter().all(u8::is_ascii_whitespace) {
            continue;
        }
        if line.len() > MAX_LINE_BYTES {
            parsed.unreadable += 1;
            continue;
        }
        let Some(message) = serde_json::from_slice::<LogRow>(line)
            .ok()
            .and_then(|row| row.event_message)
        else {
            parsed.unreadable += 1;
            continue;
        };
        let Some(sender) = cited_sender(&message, signal_name) else {
            continue;
        };
        parsed.matching += 1;
        match counts.iter_mut().find(|(s, _, _)| *s == sender) {
            Some((_, count, _)) => *count += 1,
            None => counts.push((sender, 1, message.chars().take(240).collect())),
        }
    }
    // The first of equally cited senders wins: the max is stable on ties.
    parsed.best = counts
        .into_iter()
        .reduce(|best, next| if next.1 > best.1 { next } else { best });
    parsed
}

/// The unified-log outcome [`attribute_with`] interprets.
pub enum LogQuery {
    Output { bytes: Vec<u8>, truncated: bool },
    TimedOut,
    Failed(String),
}

/// Turns a log query into an [`Attribution`]. Never guesses: no evidence is
/// `sender: None` with the reason.
pub fn attribute_with(signal: &str, query: impl FnOnce(&str) -> LogQuery) -> Attribution {
    let Some(name) = launchd_signal_name(signal) else {
        return Attribution::none(format!("no launchd name for signal {signal:?}"));
    };
    match query(name) {
        LogQuery::TimedOut => Attribution::none("log query timed out"),
        LogQuery::Failed(error) => Attribution::none(format!("log query failed: {error}")),
        LogQuery::Output { bytes, truncated } => {
            let parsed = parse_launchd_ndjson(&bytes, name);
            let cut = if truncated {
                "; log output was truncated"
            } else {
                ""
            };
            match parsed.best {
                None => Attribution::none(format!("no launchd job reported a sender{cut}")),
                Some((sender, count, evidence)) => Attribution {
                    note: format!(
                        "launchd reported {count} of {} {name} exits sent by {}[{}]; \
                         correlated by time, the sessions' own signals are not logged: \
                         {evidence}{cut}",
                        parsed.matching, sender.name, sender.pid
                    ),
                    sender: Some(sender),
                },
            }
        }
    }
}

/// Formats epoch milliseconds as the local `YYYY-MM-DD HH:MM:SS` `log show`
/// takes for `--start`/`--end`.
#[cfg(target_os = "macos")]
fn local_log_time(ms: u64) -> Option<String> {
    let secs = libc::time_t::try_from(ms / 1000).ok()?;
    // SAFETY: tm is plain data; localtime_r writes only into it.
    let tm = unsafe {
        let mut tm: libc::tm = std::mem::zeroed();
        if libc::localtime_r(&secs, &mut tm).is_null() {
            return None;
        }
        tm
    };
    Some(format!(
        "{:04}-{:02}-{:02} {:02}:{:02}:{:02}",
        tm.tm_year + 1900,
        tm.tm_mon + 1,
        tm.tm_mday,
        tm.tm_hour,
        tm.tm_min,
        tm.tm_sec
    ))
}

/// Asks the macOS unified log who signalled launchd's jobs around `incident`.
/// Blocking: call it off the UI and async-runtime threads. Runs through
/// `git_cli::run_captured`, the project's bounded spawn path (procguard
/// registration, deadline, capped output).
#[cfg(target_os = "macos")]
pub fn attribute_sender(incident: &Incident) -> Attribution {
    attribute_with(&incident.signal, |_| {
        let start = local_log_time(incident.at_ms.saturating_sub(LOOKBACK_MS));
        let end = local_log_time(incident.at_ms + incident.span_ms + LOOKAHEAD_MS);
        let (Some(start), Some(end)) = (start, end) else {
            return LogQuery::Failed("could not format the search window".into());
        };
        let args = [
            "show",
            "--style",
            "ndjson",
            "--start",
            start.as_str(),
            "--end",
            end.as_str(),
            "--predicate",
            r#"process == "launchd" AND eventMessage CONTAINS "sent by""#,
        ];
        match crate::engine::git_cli::run_captured(
            "/usr/bin/log",
            &args,
            None,
            QUERY_DEADLINE,
            &[],
            QUERY_OUTPUT_CAP,
        ) {
            Ok(crate::engine::git_cli::RunOutcome::TimedOut(_)) => LogQuery::TimedOut,
            Ok(crate::engine::git_cli::RunOutcome::Finished(run)) if run.success => {
                LogQuery::Output {
                    bytes: run.stdout_tail.into_bytes(),
                    truncated: run.truncated,
                }
            }
            Ok(crate::engine::git_cli::RunOutcome::Finished(run)) => LogQuery::Failed(format!(
                "exit status {}: {}",
                run.status_code,
                run.stderr_tail.chars().take(160).collect::<String>()
            )),
            Err(error) => LogQuery::Failed(error.chars().take(200).collect()),
        }
    })
}

#[cfg(not(target_os = "macos"))]
pub fn attribute_sender(_incident: &Incident) -> Attribution {
    Attribution::none("attribution unavailable on this platform")
}

// -- the event ------------------------------------------------------------------

/// The `terminal-incident` event payload and `cmd_terminal_incidents` row.
/// Field names are a contract with the frontend: snake_case, exactly these.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct TerminalIncidentPayload {
    pub id: String,
    pub signal: String,
    pub at_ms: u64,
    /// Terminal session ids; at most [`MAX_MEMBERS`] (the log line and
    /// `sender_note` carry the true count when it is higher).
    pub session_ids: Vec<String>,
    pub sender: Option<Sender>,
    pub sender_note: String,
}

fn payload(incident: &Incident, attribution: Attribution) -> TerminalIncidentPayload {
    let mut note = attribution.note;
    if incident.total > incident.members.len() {
        note.push_str(&format!(
            "; {} of {} session ids listed",
            incident.members.len(),
            incident.total
        ));
    }
    TerminalIncidentPayload {
        id: incident.id.clone(),
        signal: incident.signal.clone(),
        at_ms: incident.at_ms,
        session_ids: incident
            .members
            .iter()
            .map(|m| m.session_id.clone())
            .collect(),
        sender: attribution.sender,
        sender_note: note,
    }
}

/// The one WARN line an incident produces.
pub fn warn_line(incident: &Incident, payload: &TerminalIncidentPayload) -> String {
    let sender = match &payload.sender {
        Some(s) => format!("{}[{}]", s.name, s.pid),
        None => format!("unknown ({})", payload.sender_note),
    };
    format!(
        "[terminal] incident: {} sessions ended by {} within {} ms; sender {}",
        incident.total, incident.signal, incident.span_ms, sender
    )
}

/// The reader thread's one call: reports `exit` when it is an external signal
/// death. `ended_by_gitpulse` is the session's `dead` flag as the reader found
/// it: set by Close, shutdown and the reader's own failure paths, so those
/// never count. Emits `terminal-incident` when an incident closes.
pub(super) fn report_exit<R: tauri::Runtime>(
    recorder: &Arc<Recorder>,
    app: &tauri::AppHandle<R>,
    ended_by_gitpulse: bool,
    exit: &super::TerminalExitPayload,
    program: &str,
    cwd: &str,
) {
    if ended_by_gitpulse || exit.signal.is_empty() {
        return;
    }
    let wall_ms = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| u64::try_from(d.as_millis()).unwrap_or(u64::MAX));
    let end = SessionEnd {
        at: Instant::now(),
        wall_ms,
        session_id: exit.id.clone(),
        program: program.to_string(),
        cwd: cwd.to_string(),
        exit_code: exit.exit_code,
        signal: exit.signal.clone(),
    };
    let handle = app.clone();
    let sink: Sink = Arc::new(move |incident| {
        use tauri::Emitter;
        if let Err(error) = handle.emit("terminal-incident", incident) {
            log::warn!(target: "terminal", "failed to emit terminal-incident event: {error}");
        }
    });
    if let Err(rejected) = recorder.note_end(end, sink) {
        log::debug!(target: "terminal", "session end not counted toward an incident: {rejected:?}");
    }
}

// -- the recorder -----------------------------------------------------------------

/// Delivers a finished incident (the Tauri event, in production).
pub type Sink = Arc<dyn Fn(&TerminalIncidentPayload) + Send + Sync>;
type Attributor = Arc<dyn Fn(&Incident) -> Attribution + Send + Sync>;

struct Inner {
    aggregator: Aggregator,
    sink: Option<Sink>,
    worker_running: bool,
}

/// Owns the aggregator, its worker and the bounded history.
pub struct Recorder {
    inner: Mutex<Inner>,
    wake: Condvar,
    recent: Mutex<VecDeque<TerminalIncidentPayload>>,
    attributing: AtomicUsize,
    attributor: Attributor,
}

fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(std::sync::PoisonError::into_inner)
}

impl Default for Recorder {
    fn default() -> Self {
        Self::new(WINDOW, THRESHOLD, Arc::new(attribute_sender))
    }
}

impl Recorder {
    pub fn new(window: Duration, threshold: usize, attributor: Attributor) -> Self {
        Self {
            inner: Mutex::new(Inner {
                aggregator: Aggregator::new(window, threshold),
                sink: None,
                worker_running: false,
            }),
            wake: Condvar::new(),
            recent: Mutex::new(VecDeque::new()),
            attributing: AtomicUsize::new(0),
            attributor,
        }
    }

    /// Reports one external signal death. Cheap and non-blocking: takes a lock,
    /// updates the aggregator, and makes sure the single worker is running.
    /// `at` is read under the lock so concurrent reporters cannot reorder.
    pub fn note_end(self: &Arc<Self>, mut end: SessionEnd, sink: Sink) -> Result<(), Rejected> {
        let mut inner = lock(&self.inner);
        end.at = Instant::now();
        inner.aggregator.record(end)?;
        inner.sink = Some(sink);
        if inner.worker_running {
            self.wake.notify_all();
            return Ok(());
        }
        let me = Arc::clone(self);
        match crate::supervisor::spawn_once("pty-incident", "terminal incident worker", move || {
            me.work()
        }) {
            Ok(_) => inner.worker_running = true,
            Err(error) => {
                log::warn!(target: "terminal", "could not start the incident worker: {error}");
            }
        }
        Ok(())
    }

    fn work(self: Arc<Self>) {
        loop {
            let mut inner = lock(&self.inner);
            let now = Instant::now();
            let ready = inner.aggregator.drain_ready(now);
            if !ready.is_empty() {
                let sink = inner.sink.clone();
                drop(inner);
                for incident in ready {
                    self.finish(incident, sink.clone());
                }
                continue;
            }
            match inner.aggregator.next_deadline() {
                None => {
                    inner.worker_running = false;
                    return;
                }
                Some(deadline) => {
                    let wait = deadline.saturating_duration_since(now);
                    drop(
                        self.wake
                            .wait_timeout(inner, wait)
                            .unwrap_or_else(std::sync::PoisonError::into_inner),
                    );
                }
            }
        }
    }

    /// Attributes and publishes one incident without blocking the worker: the
    /// log query can take seconds and other groups may be closing.
    fn finish(self: &Arc<Self>, incident: Incident, sink: Option<Sink>) {
        if self.attributing.fetch_add(1, Ordering::AcqRel) >= MAX_ATTRIBUTIONS {
            self.attributing.fetch_sub(1, Ordering::AcqRel);
            let skipped =
                Attribution::none("attribution skipped: other incidents were being attributed");
            self.publish(&incident, skipped, sink.as_ref());
            return;
        }
        let me = Arc::clone(self);
        let for_thread = incident.clone();
        let for_sink = sink.clone();
        let spawned = crate::supervisor::spawn_once(
            "pty-incident-sender",
            "terminal incident attribution",
            move || {
                // A lookup that panics must neither lose the incident nor keep
                // its place among MAX_ATTRIBUTIONS: it is published without a
                // sender, saying why.
                let attribution = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                    (me.attributor)(&for_thread)
                }))
                .unwrap_or_else(|_| {
                    Attribution::none("attribution failed: the sender lookup panicked")
                });
                me.attributing.fetch_sub(1, Ordering::AcqRel);
                me.publish(&for_thread, attribution, for_sink.as_ref());
            },
        );
        if let Err(error) = spawned {
            self.attributing.fetch_sub(1, Ordering::AcqRel);
            let note = format!("attribution not started: {error}");
            self.publish(&incident, Attribution::none(note), sink.as_ref());
        }
    }

    fn publish(&self, incident: &Incident, attribution: Attribution, sink: Option<&Sink>) {
        let payload = payload(incident, attribution);
        log::warn!(target: "terminal", "{}", warn_line(incident, &payload));
        {
            let mut recent = lock(&self.recent);
            recent.push_back(payload.clone());
            while recent.len() > MAX_STORED {
                recent.pop_front();
            }
        }
        if let Some(sink) = sink {
            sink(&payload);
        }
    }

    /// Recorded incidents, newest first.
    pub fn recent(&self) -> Vec<TerminalIncidentPayload> {
        lock(&self.recent).iter().rev().cloned().collect()
    }

    /// Ends held in open groups.
    pub fn pending(&self) -> usize {
        lock(&self.inner).aggregator.pending()
    }

    /// Whether the worker thread is alive.
    pub fn worker_running(&self) -> bool {
        lock(&self.inner).worker_running
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::thread;

    const TERM: &str = "Terminated: 15";

    fn end(base: Instant, ms: u64, id: &str, signal: &str) -> SessionEnd {
        SessionEnd {
            at: base + Duration::from_millis(ms),
            wall_ms: 1_000_000 + ms,
            session_id: id.into(),
            program: "claude".into(),
            cwd: "/repo".into(),
            exit_code: None,
            signal: signal.into(),
        }
    }

    fn run(ends: &[(u64, &str, &str)]) -> Vec<Incident> {
        let base = Instant::now();
        let mut agg = Aggregator::new(WINDOW, THRESHOLD);
        for (ms, id, sig) in ends {
            agg.record(end(base, *ms, id, sig)).unwrap();
        }
        agg.drain_ready(base + Duration::from_secs(3600))
    }

    #[test]
    fn exactly_at_threshold_is_one_incident() {
        let out = run(&[(0, "a", TERM), (10, "b", TERM), (20, "c", TERM)]);
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].total, 3);
        assert_eq!(out[0].span_ms, 20);
        let ids: Vec<_> = out[0]
            .members
            .iter()
            .map(|m| m.session_id.as_str())
            .collect();
        assert_eq!(ids, ["a", "b", "c"]);
    }

    #[test]
    fn below_threshold_is_not_an_incident() {
        assert!(run(&[(0, "a", TERM), (10, "b", TERM)]).is_empty());
        assert!(run(&[(0, "a", TERM)]).is_empty());
    }

    #[test]
    fn mixed_signals_do_not_combine() {
        let out = run(&[
            (0, "a", TERM),
            (1, "b", "Hangup: 1"),
            (2, "c", TERM),
            (3, "d", "Hangup: 1"),
            (4, "e", "Killed: 9"),
        ]);
        assert!(out.is_empty(), "two + two + one is no incident: {out:?}");
        let out = run(&[
            (0, "a", TERM),
            (1, "b", "Hangup: 1"),
            (2, "c", TERM),
            (3, "d", TERM),
        ]);
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].signal, TERM);
        assert_eq!(out[0].total, 3);
    }

    #[test]
    fn a_normal_exit_is_not_a_signal() {
        let base = Instant::now();
        let mut agg = Aggregator::new(WINDOW, THRESHOLD);
        assert_eq!(agg.record(end(base, 0, "a", "")), Err(Rejected::NoSignal));
        assert_eq!(agg.record(end(base, 0, "a", "  ")), Err(Rejected::NoSignal));
        assert_eq!(agg.pending(), 0);
    }

    #[test]
    fn the_window_edge_is_inclusive_and_one_past_it_is_not() {
        let edge = WINDOW.as_millis() as u64;
        let out = run(&[(0, "a", TERM), (1, "b", TERM), (edge, "c", TERM)]);
        assert_eq!(out.len(), 1, "an end exactly at first + window joins");
        assert_eq!(out[0].total, 3);
        let out = run(&[(0, "a", TERM), (1, "b", TERM), (edge + 1, "c", TERM)]);
        assert!(
            out.is_empty(),
            "one ms past the window starts a new group: {out:?}"
        );
    }

    #[test]
    fn a_late_member_joins_an_incident_that_already_reached_the_threshold() {
        let base = Instant::now();
        let mut agg = Aggregator::new(WINDOW, THRESHOLD);
        for (ms, id) in [(0, "a"), (1, "b"), (2, "c")] {
            agg.record(end(base, ms, id, TERM)).unwrap();
        }
        assert!(
            agg.drain_ready(base + Duration::from_millis(500))
                .is_empty(),
            "the incident is held until the window closes"
        );
        agg.record(end(base, 1900, "late", TERM)).unwrap();
        let out = agg.drain_ready(base + Duration::from_millis(2100));
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].total, 4);
        assert_eq!(out[0].members.last().unwrap().session_id, "late");
    }

    #[test]
    fn two_incidents_back_to_back_stay_two() {
        let out = run(&[
            (0, "a", TERM),
            (1, "b", TERM),
            (2, "c", TERM),
            (5000, "d", TERM),
            (5001, "e", TERM),
            (5002, "f", TERM),
        ]);
        assert_eq!(out.len(), 2);
        assert_eq!(out[0].total, 3);
        assert_eq!(out[1].total, 3);
        assert_ne!(out[0].id, out[1].id);
        assert_eq!(out[1].members[0].session_id, "d");
    }

    #[test]
    fn a_clock_that_runs_backwards_is_refused() {
        let base = Instant::now() + Duration::from_secs(10);
        let mut agg = Aggregator::new(WINDOW, THRESHOLD);
        agg.record(end(base, 100, "a", TERM)).unwrap();
        assert_eq!(
            agg.record(end(base, 50, "b", TERM)),
            Err(Rejected::NonMonotonic)
        );
        assert_eq!(agg.pending(), 1, "the refused end left no trace");
        agg.record(end(base, 100, "c", TERM)).unwrap();
    }

    #[test]
    fn ten_thousand_ends_are_one_bounded_incident_with_the_true_count() {
        let base = Instant::now();
        let mut agg = Aggregator::new(WINDOW, THRESHOLD);
        for i in 0..10_000u64 {
            agg.record(end(base, i / 10, &format!("s{i}"), TERM))
                .unwrap();
            assert!(agg.pending() <= MAX_PENDING_ENDS);
        }
        let out = agg.drain_ready(base + Duration::from_secs(10));
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].total, 10_000);
        assert_eq!(out[0].members.len(), MAX_MEMBERS);
        assert_eq!(agg.pending(), 0);
        let p = payload(&out[0], Attribution::none("x"));
        assert!(
            p.sender_note.contains("512 of 10000 session ids listed"),
            "{p:?}"
        );
    }

    #[test]
    fn pending_memory_is_capped_across_groups() {
        let base = Instant::now();
        let mut agg = Aggregator::new(WINDOW, THRESHOLD);
        for sig in 0..MAX_GROUPS {
            for i in 0..MAX_MEMBERS {
                agg.record(end(base, 0, &format!("{sig}-{i}"), &format!("sig{sig}")))
                    .unwrap();
            }
        }
        assert_eq!(agg.pending(), MAX_PENDING_ENDS);
        assert_eq!(
            agg.record(end(base, 0, "x", "one-too-many")),
            Err(Rejected::TooManyGroups)
        );
        let out = agg.drain_ready(base + Duration::from_secs(10));
        assert_eq!(out.len(), MAX_GROUPS);
        assert_eq!(
            out.iter().map(|i| i.total).sum::<usize>(),
            MAX_GROUPS * MAX_MEMBERS
        );
    }

    #[test]
    fn the_next_deadline_follows_the_oldest_open_group() {
        let base = Instant::now();
        let mut agg = Aggregator::new(WINDOW, THRESHOLD);
        assert!(agg.next_deadline().is_none());
        agg.record(end(base, 0, "a", TERM)).unwrap();
        agg.record(end(base, 400, "b", "Hangup: 1")).unwrap();
        assert_eq!(
            agg.next_deadline(),
            Some(base + WINDOW + Duration::from_millis(1))
        );
        agg.drain_ready(base + Duration::from_secs(3));
        assert!(agg.next_deadline().is_none());
    }

    // -- parser ---------------------------------------------------------------

    /// Real lines captured read-only from this machine's unified log for the
    /// 2026-10-12 00:46:40 incident, trimmed of fields the parser ignores only
    /// by removing nothing from eventMessage.
    const REAL: &str = r#"{"timezoneName":"","messageType":"Default","eventType":"logEvent","subsystem":"gui\/501\/com.apple.spotlightknowledged.updater [791]","processImagePath":"\/sbin\/launchd","timestamp":"2026-10-12 00:46:40.506922-0700","eventMessage":"exited due to SIGTERM | sent by pkill[89829], ran for 27355833ms","processID":1}
{"timezoneName":"","messageType":"Default","eventType":"logEvent","subsystem":"pid\/791\/com.apple.SetStoreUpdateService [1795]","processImagePath":"\/sbin\/launchd","timestamp":"2026-10-12 00:46:40.522640-0700","eventMessage":"exited due to SIGKILL | sent by launchd[1] during teardown of process-scoped services after host exited, ran for 27304968ms","processID":1}
{"timezoneName":"","messageType":"Default","eventType":"logEvent","subsystem":"gui\/501\/com.bharath.chronicle.watch [1202]","processImagePath":"\/sbin\/launchd","timestamp":"2026-10-12 00:46:40.529136-0700","eventMessage":"exited due to SIGTERM | sent by pkill[89829], ran for 27343014ms","processID":1}
"#;

    #[test]
    fn real_launchd_lines_name_pkill() {
        let parsed = parse_launchd_ndjson(REAL.as_bytes(), "SIGTERM");
        assert_eq!(parsed.matching, 2);
        assert_eq!(parsed.unreadable, 0);
        let (sender, count, evidence) = parsed.best.unwrap();
        assert_eq!(
            sender,
            Sender {
                pid: 89829,
                name: "pkill".into()
            }
        );
        assert_eq!(count, 2);
        assert!(evidence.starts_with("exited due to SIGTERM | sent by pkill[89829]"));
        let kill = parse_launchd_ndjson(REAL.as_bytes(), "SIGKILL")
            .best
            .unwrap();
        assert_eq!(
            kill.0,
            Sender {
                pid: 1,
                name: "launchd".into()
            }
        );
    }

    #[test]
    fn the_most_cited_sender_wins_and_ties_go_to_the_first() {
        let line = |name: &str, pid: u32| {
            format!(
                r#"{{"eventMessage":"exited due to SIGTERM | sent by {name}[{pid}], ran for 1ms"}}"#
            )
        };
        let text = [
            line("a", 1),
            line("b", 2),
            line("b", 2),
            line("a", 1),
            line("c", 3),
        ]
        .join("\n");
        let best = parse_launchd_ndjson(text.as_bytes(), "SIGTERM")
            .best
            .unwrap();
        assert_eq!(
            (best.0.name.as_str(), best.1),
            ("a", 2),
            "tie keeps the first"
        );
        let text = [line("a", 1), line("b", 2), line("b", 2)].join("\n");
        assert_eq!(
            parse_launchd_ndjson(text.as_bytes(), "SIGTERM")
                .best
                .unwrap()
                .0
                .name,
            "b"
        );
        // Same name, different pid: different senders.
        let text = [line("pkill", 1), line("pkill", 2), line("pkill", 2)].join("\n");
        assert_eq!(
            parse_launchd_ndjson(text.as_bytes(), "SIGTERM")
                .best
                .unwrap()
                .0
                .pid,
            2
        );
    }

    #[test]
    fn malformed_truncated_and_huge_input_is_counted_not_fatal() {
        let truncated = &REAL.as_bytes()[..REAL.len() / 2];
        let parsed = parse_launchd_ndjson(truncated, "SIGTERM");
        assert_eq!(parsed.matching, 1);
        assert_eq!(parsed.unreadable, 1, "the cut line is counted");
        for junk in [
            "",
            "\n\n",
            "not json",
            "[]",
            "{}",
            r#"{"eventMessage":null}"#,
            r#"{"eventMessage":"exited due to SIGTERM | sent by [12]"}"#,
            r#"{"eventMessage":"exited due to SIGTERM | sent by x[notapid]"}"#,
            r#"{"eventMessage":"exited due to SIGTERM | sent by x[99999999999999]"}"#,
            r#"{"eventMessage":"exited due to SIGTERM | sent by x"}"#,
            r#"{"eventMessage":"exited due to SIGTERMINATED | sent by x[1]"}"#,
            r#"{"eventMessage":"exited due to SIGTERM | sent by \u0007bell[1]"}"#,
        ] {
            assert!(
                parse_launchd_ndjson(junk.as_bytes(), "SIGTERM")
                    .best
                    .is_none(),
                "{junk:?}"
            );
        }
        let mut huge = vec![b'x'; MAX_LINE_BYTES + 1];
        huge.push(b'\n');
        huge.extend_from_slice(REAL.as_bytes());
        let parsed = parse_launchd_ndjson(&huge, "SIGTERM");
        assert_eq!(parsed.unreadable, 1);
        assert_eq!(parsed.matching, 2, "a giant line does not poison the next");
        let big = REAL.repeat(20_000);
        assert_eq!(
            parse_launchd_ndjson(big.as_bytes(), "SIGTERM").matching,
            40_000
        );
    }

    #[test]
    fn signal_text_maps_to_launchds_name() {
        for (reported, name) in [
            ("Terminated: 15", "SIGTERM"),
            ("Killed: 9", "SIGKILL"),
            ("Hangup: 1", "SIGHUP"),
            ("Terminated", "SIGTERM"),
            ("Hangup", "SIGHUP"),
        ] {
            assert_eq!(launchd_signal_name(reported), Some(name), "{reported}");
        }
        assert_eq!(launchd_signal_name("Mystery: 99"), None);
        assert_eq!(launchd_signal_name(""), None);
    }

    #[test]
    fn attribution_says_which_failure_it_was() {
        let timed = attribute_with(TERM, |_| LogQuery::TimedOut);
        assert_eq!(timed, Attribution::none("log query timed out"));
        let failed = attribute_with(TERM, |_| LogQuery::Failed("spawn denied".into()));
        assert!(failed.sender.is_none() && failed.note.contains("spawn denied"));
        let empty = attribute_with(TERM, |_| LogQuery::Output {
            bytes: vec![],
            truncated: false,
        });
        assert_eq!(empty.note, "no launchd job reported a sender");
        let other = attribute_with(TERM, |_| LogQuery::Output {
            bytes: REAL.replace("SIGTERM", "SIGHUP").into_bytes(),
            truncated: true,
        });
        assert!(other.sender.is_none());
        assert!(other.note.starts_with("no launchd job reported a sender"));
        assert!(other.note.contains("truncated"));
        let unknown = attribute_with("Mystery: 99", |_| {
            unreachable!("no query for an unnameable signal")
        });
        assert!(unknown.sender.is_none());
    }

    #[test]
    fn attribution_with_evidence_names_the_sender_and_the_line() {
        let hit = attribute_with(TERM, |name| {
            assert_eq!(name, "SIGTERM");
            LogQuery::Output {
                bytes: REAL.as_bytes().to_vec(),
                truncated: false,
            }
        });
        assert_eq!(
            hit.sender,
            Some(Sender {
                pid: 89829,
                name: "pkill".into()
            })
        );
        assert!(
            hit.note
                .contains("2 of 2 SIGTERM exits sent by pkill[89829]"),
            "{}",
            hit.note
        );
        assert!(hit
            .note
            .contains("exited due to SIGTERM | sent by pkill[89829]"));
    }

    #[test]
    fn the_payload_and_warn_line_match_the_contract() {
        let incident = Incident {
            id: "inc-1".into(),
            signal: TERM.into(),
            at_ms: 42,
            span_ms: 900,
            members: vec![
                Member {
                    session_id: "t1".into(),
                    program: "claude".into(),
                    cwd: "/r".into(),
                    exit_code: None,
                },
                Member {
                    session_id: "t2".into(),
                    program: "claude".into(),
                    cwd: "/r".into(),
                    exit_code: None,
                },
                Member {
                    session_id: "t3".into(),
                    program: "claude".into(),
                    cwd: "/r".into(),
                    exit_code: None,
                },
            ],
            total: 3,
        };
        let hit = payload(
            &incident,
            Attribution {
                sender: Some(Sender {
                    pid: 7,
                    name: "pkill".into(),
                }),
                note: "n".into(),
            },
        );
        assert_eq!(
            serde_json::to_value(&hit).unwrap(),
            serde_json::json!({
                "id": "inc-1", "signal": "Terminated: 15", "at_ms": 42,
                "session_ids": ["t1", "t2", "t3"],
                "sender": {"pid": 7, "name": "pkill"}, "sender_note": "n"
            })
        );
        assert_eq!(
            warn_line(&incident, &hit),
            "[terminal] incident: 3 sessions ended by Terminated: 15 within 900 ms; sender pkill[7]"
        );
        let none = payload(&incident, Attribution::none("log query timed out"));
        assert_eq!(
            serde_json::to_value(&none).unwrap()["sender"],
            serde_json::Value::Null
        );
        assert_eq!(
            warn_line(&incident, &none),
            "[terminal] incident: 3 sessions ended by Terminated: 15 within 900 ms; sender unknown (log query timed out)"
        );
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn the_log_window_is_formatted_the_way_log_show_reads_it() {
        let text = local_log_time(1_791_791_200_000).unwrap();
        let shape: Vec<char> = text
            .chars()
            .map(|c| if c.is_ascii_digit() { '9' } else { c })
            .collect();
        assert_eq!(shape.iter().collect::<String>(), "9999-99-99 99:99:99");
        assert!(text.starts_with("2026-10-1"), "{text}");
    }

    // -- recorder -----------------------------------------------------------------

    fn recorder(
        window_ms: u64,
    ) -> (
        Arc<Recorder>,
        Arc<Mutex<Vec<TerminalIncidentPayload>>>,
        Sink,
    ) {
        let rec = Arc::new(Recorder::new(
            Duration::from_millis(window_ms),
            THRESHOLD,
            Arc::new(|_| Attribution::none("test")),
        ));
        let seen = Arc::new(Mutex::new(Vec::new()));
        let into = seen.clone();
        let sink: Sink = Arc::new(move |p| into.lock().unwrap().push(p.clone()));
        (rec, seen, sink)
    }

    fn report(rec: &Arc<Recorder>, sink: &Sink, id: &str) {
        let base = Instant::now();
        rec.note_end(end(base, 0, id, TERM), sink.clone()).unwrap();
    }

    fn wait_until(what: &str, mut done: impl FnMut() -> bool) {
        let deadline = Instant::now() + Duration::from_secs(10);
        while !done() {
            assert!(Instant::now() < deadline, "timed out waiting for {what}");
            thread::sleep(Duration::from_millis(5));
        }
    }

    #[test]
    fn fifty_threads_ending_at_once_make_exactly_one_incident_every_time() {
        // 200 rounds, eight at a time. The window is generous (400 ms) because
        // the property under test is "no loss, no duplication, one incident
        // when the ends fit the window"; a loaded CI box can starve a thread
        // for longer than a tight window, which is the window working.
        thread::scope(|lanes| {
            for lane in 0..8 {
                lanes.spawn(move || {
                    for round in 0..25 {
                        let (rec, seen, sink) = recorder(400);
                        let start = Arc::new(std::sync::Barrier::new(50));
                        let threads: Vec<_> = (0..50)
                            .map(|i| {
                                let (rec, sink, start) = (rec.clone(), sink.clone(), start.clone());
                                thread::spawn(move || {
                                    start.wait();
                                    report(&rec, &sink, &format!("s{i}"));
                                })
                            })
                            .collect();
                        for t in threads {
                            t.join().unwrap();
                        }
                        wait_until("the incident", || !rec.recent().is_empty());
                        wait_until("the worker to stop", || !rec.worker_running());
                        let got = seen.lock().unwrap().clone();
                        assert_eq!(got.len(), 1, "lane {lane} round {round}: {got:?}");
                        let mut ids = got[0].session_ids.clone();
                        ids.sort();
                        ids.dedup();
                        assert_eq!(
                            ids.len(),
                            50,
                            "lane {lane} round {round}: lost or duplicated"
                        );
                        assert_eq!(rec.recent().len(), 1);
                        assert_eq!(rec.pending(), 0);
                    }
                });
            }
        });
    }

    #[test]
    fn the_worker_stops_when_nothing_is_pending_and_restarts_for_the_next_group() {
        let (rec, seen, sink) = recorder(20);
        report(&rec, &sink, "lonely");
        wait_until("the worker to stop", || !rec.worker_running());
        assert!(
            seen.lock().unwrap().is_empty(),
            "one end is not an incident"
        );
        for round in 0..2 {
            for i in 0..3 {
                report(&rec, &sink, &format!("r{round}-{i}"));
            }
            wait_until("an incident", || seen.lock().unwrap().len() == round + 1);
            wait_until("the worker to stop", || !rec.worker_running());
        }
        assert_eq!(rec.recent().len(), 2);
        assert_eq!(rec.recent()[0].session_ids[0], "r1-0", "newest first");
    }

    #[test]
    fn stored_incidents_are_bounded() {
        let (rec, _seen, sink) = recorder(5);
        for round in 0..(MAX_STORED + 5) {
            for i in 0..3 {
                report(&rec, &sink, &format!("r{round}-{i}"));
            }
            wait_until("the round's incident", || {
                rec.recent()
                    .first()
                    .is_some_and(|p| p.session_ids[0].starts_with(&format!("r{round}-")))
            });
            wait_until("the worker to stop", || !rec.worker_running());
        }
        assert_eq!(rec.recent().len(), MAX_STORED);
    }

    /// The defect: the attribution thread was a raw start whose panic nothing
    /// caught, so a lookup that panicked lost the incident it was attributing
    /// and never returned its place among `MAX_ATTRIBUTIONS`: after a couple of
    /// them every later incident was published without a sender.
    #[test]
    fn an_attributor_that_panics_still_delivers_the_incident_and_frees_its_place() {
        let rec = Arc::new(Recorder::new(
            Duration::from_millis(20),
            THRESHOLD,
            Arc::new(|_| panic!("the log query blew up")),
        ));
        let seen = Arc::new(Mutex::new(Vec::new()));
        let into = seen.clone();
        let sink: Sink = Arc::new(move |p| into.lock().unwrap().push(p.clone()));
        for i in 0..THRESHOLD {
            report(&rec, &sink, &format!("p{i}"));
        }
        wait_until("the incident to be delivered", || {
            !seen.lock().unwrap().is_empty()
        });
        let got = seen.lock().unwrap().clone();
        assert_eq!(got.len(), 1);
        assert_eq!(got[0].session_ids.len(), THRESHOLD);
        assert!(
            got[0].sender_note.contains("attribution failed"),
            "{:?}",
            got[0].sender_note
        );
        wait_until("the attribution place to be returned", || {
            rec.attributing.load(Ordering::Acquire) == 0
        });
    }
}
