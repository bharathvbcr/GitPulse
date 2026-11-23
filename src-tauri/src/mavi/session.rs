//! One Mavi thread's agent process and everything the host keeps about it.
//!
//! Four threads per session, none of which can block a Tauri command:
//! - **writer** owns the child's stdin and drains a channel, so a child that
//!   stops reading never stalls the command that sent it a line;
//! - **stdout reader** splits lines (bounded) and hands them on, then waits
//!   for the process to exit;
//! - **stderr reader** keeps a bounded tail;
//! - **processor** prepares each line (`wire::prepare`), updates the session's
//!   summary, appends to the replay ring, and emits batches coalesced over
//!   [`COALESCE`].
//!
//! The ring is memory only. It is never written to disk or logged: tool
//! results are the agent's view of the person's files.

use super::wire::{self, Signal};
use super::worktree::{self, Fingerprint};
use crate::procguard::LockedSpawn;
use serde::Serialize;
use serde_json::{json, Value};
use std::collections::{HashSet, VecDeque};
use std::io::{BufRead, BufReader, Read, Write};
use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::sync::mpsc::{self, RecvTimeoutError};
use std::sync::{Arc, Mutex};
use std::time::Duration;

pub(crate) const EVENT: &str = "mavi-event";
pub(crate) const STATUS_EVENT: &str = "mavi-status";

/// How long the processor gathers lines before it emits one batch. Streaming
/// deltas arrive many per second; one IPC event per line would make the page
/// re-render per token.
const COALESCE: Duration = Duration::from_millis(40);
const MAX_BATCH: usize = 256;
/// The replay ring's bounds. Deltas are never kept (see `wire::Prepared`), so
/// this holds hundreds of turns of finished blocks.
const RING_MAX_BYTES: usize = 24 * 1024 * 1024;
const RING_MAX_ENTRIES: usize = 20_000;
/// A line past this is discarded while it is read, never buffered whole.
const MAX_READ_LINE: usize = 32 * 1024 * 1024;
const MAX_STDERR_LINES: usize = 200;
const MAX_TOUCHED: usize = 500;
/// How long a stopped agent gets to exit before its process group is killed.
const STOP_GRACE: Duration = Duration::from_secs(3);

/// Something that delivers an event to the page. Tauri's `emit` in the app; a
/// collector in tests.
pub(crate) type Emit = Arc<dyn Fn(&'static str, Value) + Send + Sync>;

/// Where the agent runs.
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case", tag = "kind")]
pub enum MaviTarget {
    Local { root: String },
    Ssh { host: String, dir: String },
}

#[derive(Debug, Clone, Copy, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum MaviPhase {
    /// Launched; no turn has been sent yet.
    Ready,
    /// Between turns.
    Idle,
    Working,
    /// Waiting for the person to answer a permission request.
    Approval,
    Ended,
}

/// What the rail, the Live pulse strip and a reloaded page read without
/// replaying a thread.
#[derive(Debug, Clone, Serialize)]
pub struct MaviSummary {
    pub id: String,
    pub target: MaviTarget,
    pub mode: String,
    pub model: Option<String>,
    pub phase: MaviPhase,
    pub conversation_id: Option<String>,
    pub pending: usize,
    pub turns: u32,
    /// Cumulative, as the agent reports it in each `result`.
    pub cost_usd: f64,
    /// Files the agent edited with its own tools, relative to the worktree
    /// root when local, in first-touched order.
    pub touched: Vec<String>,
    pub background_tasks: usize,
    pub started_at_ms: u64,
    pub last_activity_ms: u64,
    pub exit: Option<String>,
    /// Ended cleanly: exit code 0, or stopped here on the person's request.
    pub exit_ok: bool,
}

#[derive(Debug, Clone, Serialize)]
pub struct MaviEntry {
    pub seq: u64,
    pub line: String,
}

struct Core {
    ring: VecDeque<MaviEntry>,
    ring_bytes: usize,
    next_seq: u64,
    pending: wire::Pending,
    phase: MaviPhase,
    conversation_id: Option<String>,
    turns: u32,
    cost_usd: f64,
    touched: Vec<String>,
    background: HashSet<String>,
    /// The worktree's fingerprint when the running turn was sent.
    baseline: Option<Fingerprint>,
    last_activity_ms: u64,
    exit: Option<String>,
    stderr_tail: VecDeque<String>,
    interrupts: u64,
    /// `stop` was called: a signal exit is then the one this host sent.
    stopped: bool,
    exit_ok: bool,
}

pub(crate) struct Session {
    pub id: String,
    pub target: MaviTarget,
    pub mode: String,
    pub model: Option<String>,
    pub started_at_ms: u64,
    pid: u32,
    stdin: Mutex<Option<mpsc::Sender<String>>>,
    core: Mutex<Core>,
    emit: Emit,
}

enum Msg {
    Line(String),
    Stderr(String),
    /// How the process ended, and whether it exited with code 0.
    Exit((String, bool)),
}

pub(crate) fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

impl Session {
    /// Starts `command` and the threads that serve it.
    pub(crate) fn spawn(
        id: String,
        target: MaviTarget,
        mode: String,
        model: Option<String>,
        mut command: Command,
        emit: Emit,
    ) -> Result<Arc<Session>, String> {
        command
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        // Its own process group, so stopping the agent also stops the shells
        // and background tasks it started.
        #[cfg(unix)]
        {
            use std::os::unix::process::CommandExt;
            command.process_group(0);
        }
        let mut child: Child = command
            .spawn_locked()
            .map_err(|e| format!("Could not start the agent: {e}"))?;
        let stdin = child.stdin.take().ok_or("The agent has no input pipe.")?;
        let stdout = child.stdout.take().ok_or("The agent has no output pipe.")?;
        let stderr = child.stderr.take();
        let now = now_ms();
        let (stdin_tx, stdin_rx) = mpsc::channel::<String>();
        let session = Arc::new(Session {
            id,
            target,
            mode,
            model,
            started_at_ms: now,
            pid: child.id(),
            stdin: Mutex::new(Some(stdin_tx)),
            core: Mutex::new(Core {
                ring: VecDeque::new(),
                ring_bytes: 0,
                next_seq: 1,
                pending: wire::Pending::default(),
                phase: MaviPhase::Ready,
                conversation_id: None,
                turns: 0,
                cost_usd: 0.0,
                touched: Vec::new(),
                background: HashSet::new(),
                baseline: None,
                last_activity_ms: now,
                exit: None,
                stderr_tail: VecDeque::new(),
                interrupts: 0,
                stopped: false,
                exit_ok: false,
            }),
            emit,
        });

        // Writer.
        std::thread::Builder::new()
            .name("mavi-stdin".into())
            .spawn(move || {
                let mut stdin = stdin;
                for line in stdin_rx {
                    if stdin.write_all(line.as_bytes()).is_err()
                        || stdin.write_all(b"\n").is_err()
                        || stdin.flush().is_err()
                    {
                        break;
                    }
                }
            })
            .map_err(|e| e.to_string())?;

        let (tx, rx) = mpsc::channel::<Msg>();
        // Stdout, then the exit.
        let out_tx = tx.clone();
        std::thread::Builder::new()
            .name("mavi-stdout".into())
            .spawn(move || {
                read_lines(stdout, |line| {
                    let _ = out_tx.send(Msg::Line(line));
                });
                let exit = match child.wait() {
                    Ok(status) => (describe_exit(status), status.success()),
                    Err(e) => (format!("could not be waited on: {e}"), false),
                };
                let _ = out_tx.send(Msg::Exit(exit));
            })
            .map_err(|e| e.to_string())?;
        if let Some(stderr) = stderr {
            let err_tx = tx.clone();
            std::thread::Builder::new()
                .name("mavi-stderr".into())
                .spawn(move || {
                    read_lines(stderr, |line| {
                        let _ = err_tx.send(Msg::Stderr(line));
                    })
                })
                .map_err(|e| e.to_string())?;
        }
        drop(tx);
        let processor = Arc::clone(&session);
        std::thread::Builder::new()
            .name("mavi-events".into())
            .spawn(move || processor.process(rx))
            .map_err(|e| e.to_string())?;
        Ok(session)
    }

    pub(crate) fn summary(&self) -> MaviSummary {
        let core = self.core.lock().unwrap_or_else(|p| p.into_inner());
        self.summary_of(&core)
    }

    fn summary_of(&self, core: &Core) -> MaviSummary {
        MaviSummary {
            id: self.id.clone(),
            target: self.target.clone(),
            mode: self.mode.clone(),
            model: self.model.clone(),
            phase: core.phase,
            conversation_id: core.conversation_id.clone(),
            pending: core.pending.len(),
            turns: core.turns,
            cost_usd: core.cost_usd,
            touched: core.touched.clone(),
            background_tasks: core.background.len(),
            started_at_ms: self.started_at_ms,
            last_activity_ms: core.last_activity_ms,
            exit: core.exit.clone(),
            exit_ok: core.exit_ok,
        }
    }

    pub(crate) fn is_live(&self) -> bool {
        self.summary().phase != MaviPhase::Ended
    }

    /// Entries after `after`, oldest first, plus the next sequence number.
    pub(crate) fn replay(&self, after: u64) -> (Vec<MaviEntry>, u64) {
        let core = self.core.lock().unwrap_or_else(|p| p.into_inner());
        let entries = core
            .ring
            .iter()
            .filter(|e| e.seq > after)
            .cloned()
            .collect();
        (entries, core.next_seq)
    }

    fn push(core: &mut Core, line: String, keep: bool) -> MaviEntry {
        let entry = MaviEntry {
            seq: core.next_seq,
            line,
        };
        core.next_seq += 1;
        if keep {
            core.ring_bytes += entry.line.len();
            core.ring.push_back(entry.clone());
            while core.ring_bytes > RING_MAX_BYTES || core.ring.len() > RING_MAX_ENTRIES {
                match core.ring.pop_front() {
                    Some(old) => core.ring_bytes -= old.line.len(),
                    None => break,
                }
            }
        }
        entry
    }

    fn emit_entries(&self, entries: Vec<MaviEntry>) {
        if !entries.is_empty() {
            (self.emit)(EVENT, json!({"id": self.id, "entries": entries}));
        }
    }

    fn emit_status(&self) {
        (self.emit)(
            STATUS_EVENT,
            serde_json::to_value(self.summary()).unwrap_or(Value::Null),
        );
    }

    /// Adds a host line (a `mavi_*` marker) and emits it at once.
    fn note(&self, kind: &str, fields: Value) -> u64 {
        let entry = {
            let mut core = self.core.lock().unwrap_or_else(|p| p.into_inner());
            Self::push(&mut core, wire::synthetic(kind, fields), true)
        };
        let seq = entry.seq;
        self.emit_entries(vec![entry]);
        seq
    }

    fn write(&self, line: String) -> Result<(), String> {
        let guard = self.stdin.lock().unwrap_or_else(|p| p.into_inner());
        match guard.as_ref() {
            Some(tx) => tx
                .send(line)
                .map_err(|_| "The agent is no longer reading input.".to_string()),
            None => Err("The agent has ended.".into()),
        }
    }

    /// Sends one user turn. The turn's baseline fingerprint is taken first, so
    /// it describes the worktree before the agent could touch it.
    pub(crate) fn send(&self, text: &str) -> Result<u64, String> {
        let line = wire::user_message(text)?;
        {
            let core = self.core.lock().unwrap_or_else(|p| p.into_inner());
            match core.phase {
                MaviPhase::Ended => {
                    return Err("The agent has ended. Start or resume the thread.".into())
                }
                MaviPhase::Working | MaviPhase::Approval => {
                    return Err("A turn is still running. The message waits in the queue.".into())
                }
                MaviPhase::Ready | MaviPhase::Idle => {}
            }
        }
        let (baseline, baseline_note) = match &self.target {
            MaviTarget::Local { root } => match worktree::fingerprint(&PathBuf::from(root)) {
                Ok(print) => {
                    let partial = print.partial;
                    (Some(print), partial.then_some("partial"))
                }
                Err(_) => (None, Some("unavailable")),
            },
            MaviTarget::Ssh { .. } => (None, Some("remote")),
        };
        let entry = {
            let mut core = self.core.lock().unwrap_or_else(|p| p.into_inner());
            if matches!(
                core.phase,
                MaviPhase::Working | MaviPhase::Approval | MaviPhase::Ended
            ) {
                return Err("The thread changed state; try again.".into());
            }
            core.turns += 1;
            core.phase = MaviPhase::Working;
            core.baseline = baseline;
            core.last_activity_ms = now_ms();
            let turn = core.turns;
            Self::push(
                &mut core,
                wire::synthetic(
                    "mavi_user",
                    json!({"text": text.trim_end(), "turn": turn, "at": now_ms(), "baseline": baseline_note}),
                ),
                true,
            )
        };
        let seq = entry.seq;
        self.emit_entries(vec![entry]);
        if let Err(e) = self.write(line) {
            let mut core = self.core.lock().unwrap_or_else(|p| p.into_inner());
            core.phase = MaviPhase::Idle;
            drop(core);
            self.emit_status();
            return Err(e);
        }
        self.emit_status();
        Ok(seq)
    }

    /// Answers one open permission request.
    pub(crate) fn answer(
        &self,
        request_id: &str,
        decision: wire::Decision,
        message: Option<&str>,
    ) -> Result<(), String> {
        let line = {
            let mut core = self.core.lock().unwrap_or_else(|p| p.into_inner());
            let line = core.pending.answer(request_id, decision, message)?;
            if core.pending.len() == 0 && core.phase == MaviPhase::Approval {
                core.phase = MaviPhase::Working;
            }
            line
        };
        self.write(line)?;
        let decision = match decision {
            wire::Decision::Allow => "allow",
            wire::Decision::Deny => "deny",
        };
        self.note(
            "mavi_answered",
            json!({"request_id": request_id, "decision": decision}),
        );
        self.emit_status();
        Ok(())
    }

    /// Asks the agent to stop the running turn. Open permission requests are
    /// void: the agent will not act on an answer to an interrupted turn.
    pub(crate) fn interrupt(&self) -> Result<(), String> {
        let request_id = {
            let mut core = self.core.lock().unwrap_or_else(|p| p.into_inner());
            if !matches!(core.phase, MaviPhase::Working | MaviPhase::Approval) {
                return Err("No turn is running.".into());
            }
            core.interrupts += 1;
            core.pending.clear();
            format!("mavi-interrupt-{}", core.interrupts)
        };
        self.write(wire::interrupt(&request_id))?;
        self.note("mavi_interrupt", json!({"request_id": request_id}));
        Ok(())
    }

    /// Ends the agent: closes its input, then terminates its process group,
    /// then kills it if it is still there after [`STOP_GRACE`].
    pub(crate) fn stop(&self) {
        self.core.lock().unwrap_or_else(|p| p.into_inner()).stopped = true;
        self.stdin.lock().unwrap_or_else(|p| p.into_inner()).take();
        terminate_group(self.pid, false);
        let pid = self.pid;
        let _ = std::thread::Builder::new()
            .name("mavi-stop".into())
            .spawn(move || {
                std::thread::sleep(STOP_GRACE);
                terminate_group(pid, true);
            });
    }

    fn process(self: Arc<Self>, rx: mpsc::Receiver<Msg>) {
        let mut batch: Vec<MaviEntry> = Vec::new();
        let mut status_dirty = false;
        let mut window_opened: Option<std::time::Instant> = None;
        loop {
            let wait = window_opened.map_or(COALESCE, |at| COALESCE.saturating_sub(at.elapsed()));
            let msg = match rx.recv_timeout(wait.max(Duration::from_millis(1))) {
                Ok(msg) => Some(msg),
                Err(RecvTimeoutError::Timeout) => None,
                Err(RecvTimeoutError::Disconnected) => break,
            };
            let window_closed = msg.is_none();
            match msg {
                Some(Msg::Line(line)) => {
                    let (entry, dirty, turn_ended) = self.take_line(line);
                    batch.push(entry);
                    status_dirty |= dirty;
                    if turn_ended {
                        self.flush(&mut batch, &mut status_dirty);
                        self.finish_turn();
                    }
                }
                Some(Msg::Stderr(line)) => {
                    let entry = {
                        let mut core = self.core.lock().unwrap_or_else(|p| p.into_inner());
                        if core.stderr_tail.len() >= MAX_STDERR_LINES {
                            core.stderr_tail.pop_front();
                        }
                        let text: String = dc_redact::redact_secrets(&line)
                            .chars()
                            .take(2000)
                            .collect();
                        core.stderr_tail.push_back(text.clone());
                        Self::push(
                            &mut core,
                            wire::synthetic("mavi_stderr", json!({"text": text})),
                            true,
                        )
                    };
                    batch.push(entry);
                }
                Some(Msg::Exit((exit, success))) => {
                    let was_working = {
                        let core = self.core.lock().unwrap_or_else(|p| p.into_inner());
                        matches!(core.phase, MaviPhase::Working | MaviPhase::Approval)
                    };
                    self.flush(&mut batch, &mut status_dirty);
                    // A turn the process did not finish still changed files.
                    if was_working {
                        self.finish_turn();
                    }
                    let entry = {
                        let mut core = self.core.lock().unwrap_or_else(|p| p.into_inner());
                        core.phase = MaviPhase::Ended;
                        core.pending.clear();
                        core.exit = Some(exit.clone());
                        core.exit_ok = success || core.stopped;
                        let ok = core.exit_ok;
                        let tail: Vec<String> = core
                            .stderr_tail
                            .iter()
                            .rev()
                            .take(8)
                            .rev()
                            .cloned()
                            .collect();
                        Self::push(
                            &mut core,
                            wire::synthetic(
                                "mavi_exit",
                                json!({"exit": exit, "ok": ok, "stderr": tail}),
                            ),
                            true,
                        )
                    };
                    batch.push(entry);
                    status_dirty = true;
                    self.stdin.lock().unwrap_or_else(|p| p.into_inner()).take();
                }
                None => {}
            }
            if window_opened.is_none() && !batch.is_empty() {
                window_opened = Some(std::time::Instant::now());
            }
            // A window closes COALESCE after its first line (a steady stream
            // of deltas never lets the receive time out), when the receive
            // times out, or when the batch is full.
            let due = window_opened.is_some_and(|at| at.elapsed() >= COALESCE);
            if window_closed || due || batch.len() >= MAX_BATCH {
                self.flush(&mut batch, &mut status_dirty);
                window_opened = None;
            }
        }
        self.flush(&mut batch, &mut status_dirty);
    }

    fn flush(&self, batch: &mut Vec<MaviEntry>, status_dirty: &mut bool) {
        self.emit_entries(std::mem::take(batch));
        if std::mem::take(status_dirty) {
            self.emit_status();
        }
    }

    /// Takes one stdout line into the session. Returns the entry, whether the
    /// summary changed, and whether a turn just ended.
    fn take_line(&self, line: String) -> (MaviEntry, bool, bool) {
        let prepared = wire::prepare(line);
        let mut core = self.core.lock().unwrap_or_else(|p| p.into_inner());
        core.last_activity_ms = now_ms();
        let mut dirty = false;
        let mut turn_ended = false;
        match &prepared.signal {
            Signal::PermissionRequest { request_id } => {
                if let Some(value) = &prepared.value {
                    core.pending.record(request_id, value);
                }
                if core.phase != MaviPhase::Ended {
                    core.phase = MaviPhase::Approval;
                }
                dirty = true;
            }
            Signal::TurnEnded => {
                if core.phase != MaviPhase::Ended {
                    core.phase = MaviPhase::Idle;
                }
                core.pending.clear();
                if let Some(cost) = prepared
                    .value
                    .as_ref()
                    .and_then(|v| v.get("total_cost_usd"))
                    .and_then(Value::as_f64)
                {
                    core.cost_usd = cost;
                }
                dirty = true;
                turn_ended = true;
            }
            Signal::Init { session_id } => {
                if core.conversation_id.as_deref() != Some(session_id) {
                    core.conversation_id = Some(session_id.clone());
                    dirty = true;
                }
            }
            Signal::None => {}
        }
        if let Some(value) = &prepared.value {
            dirty |= self.track(&mut core, value);
        }
        let entry = Self::push(&mut core, prepared.text, prepared.keep);
        (entry, dirty, turn_ended)
    }

    /// Updates touched files and background tasks from one line.
    fn track(&self, core: &mut Core, value: &Value) -> bool {
        let mut dirty = false;
        match value.get("type").and_then(Value::as_str) {
            Some("assistant") => {
                let blocks = value.pointer("/message/content").and_then(Value::as_array);
                for block in blocks.into_iter().flatten() {
                    if block.get("type").and_then(Value::as_str) != Some("tool_use") {
                        continue;
                    }
                    let name = block
                        .get("name")
                        .and_then(Value::as_str)
                        .unwrap_or_default();
                    if !matches!(name, "Edit" | "Write" | "MultiEdit" | "NotebookEdit") {
                        continue;
                    }
                    let path = block
                        .pointer("/input/file_path")
                        .or_else(|| block.pointer("/input/notebook_path"))
                        .and_then(Value::as_str);
                    if let Some(path) = path {
                        let shown = self.relative(path);
                        if !core.touched.contains(&shown) && core.touched.len() < MAX_TOUCHED {
                            core.touched.push(shown);
                            dirty = true;
                        }
                    }
                }
            }
            Some("system")
                if value.get("subtype").and_then(Value::as_str)
                    == Some("background_tasks_changed") =>
            {
                let tasks: HashSet<String> = value
                    .get("tasks")
                    .and_then(Value::as_array)
                    .into_iter()
                    .flatten()
                    .filter_map(|t| t.get("task_id").and_then(Value::as_str).map(str::to_owned))
                    .collect();
                if tasks != core.background {
                    core.background = tasks;
                    dirty = true;
                }
            }
            Some("user") if core.phase == MaviPhase::Approval && core.pending.len() == 0 => {
                core.phase = MaviPhase::Working;
                dirty = true;
            }
            _ => {}
        }
        dirty
    }

    /// A path relative to the local worktree root when it lies inside it.
    fn relative(&self, path: &str) -> String {
        match &self.target {
            MaviTarget::Local { root } => {
                let root = root.trim_end_matches('/');
                path.strip_prefix(root)
                    .and_then(|rest| rest.strip_prefix('/'))
                    .map(str::to_owned)
                    .unwrap_or_else(|| path.to_owned())
            }
            MaviTarget::Ssh { .. } => path.to_owned(),
        }
    }

    /// Fingerprints the worktree again and records what moved during the turn,
    /// off the processor thread (git status can take seconds).
    fn finish_turn(self: &Arc<Self>) {
        let MaviTarget::Local { root } = &self.target else {
            return;
        };
        let (baseline, turn) = {
            let mut core = self.core.lock().unwrap_or_else(|p| p.into_inner());
            (core.baseline.take(), core.turns)
        };
        let Some(baseline) = baseline else {
            self.note("mavi_turn_files", json!({"turn": turn, "available": false}));
            return;
        };
        let root = PathBuf::from(root);
        let me = Arc::clone(self);
        let _ = std::thread::Builder::new().name("mavi-turn".into()).spawn(move || {
            match worktree::fingerprint(&root) {
                Ok(after) => {
                    let moved = worktree::moved(&baseline, &after);
                    me.note(
                        "mavi_turn_files",
                        json!({"turn": turn, "available": true, "partial": baseline.partial || after.partial, "moved": moved}),
                    );
                }
                Err(reason) => {
                    me.note("mavi_turn_files", json!({"turn": turn, "available": false, "reason": reason}));
                }
            }
        });
    }
}

/// Calls `each` with every line of `reader`, without the newline. A line
/// longer than [`MAX_READ_LINE`] is replaced by a marker and skipped to its
/// end, so one runaway line cannot exhaust memory.
pub(crate) fn read_lines(reader: impl Read, mut each: impl FnMut(String)) {
    let mut reader = BufReader::with_capacity(64 * 1024, reader);
    let mut buf: Vec<u8> = Vec::new();
    let mut overflow = 0usize;
    loop {
        let available = match reader.fill_buf() {
            Ok([]) => break,
            Ok(bytes) => bytes,
            Err(e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
            Err(_) => break,
        };
        let (chunk, found) = match available.iter().position(|b| *b == b'\n') {
            Some(at) => (&available[..at], true),
            None => (available, false),
        };
        if overflow > 0 || buf.len() + chunk.len() > MAX_READ_LINE {
            overflow += chunk.len() + buf.len();
            buf.clear();
        } else {
            buf.extend_from_slice(chunk);
        }
        let used = chunk.len() + usize::from(found);
        reader.consume(used);
        if found {
            if overflow > 0 {
                each(wire::synthetic(
                    "mavi_truncated",
                    json!({"bytes": overflow}),
                ));
                overflow = 0;
            } else {
                let line = String::from_utf8_lossy(&buf)
                    .trim_end_matches('\r')
                    .to_owned();
                if !line.trim().is_empty() {
                    each(line);
                }
            }
            buf.clear();
        }
    }
    if !buf.is_empty() {
        each(String::from_utf8_lossy(&buf).into_owned());
    }
}

fn describe_exit(status: std::process::ExitStatus) -> String {
    if let Some(code) = status.code() {
        return format!("exited with code {code}");
    }
    #[cfg(unix)]
    {
        use std::os::unix::process::ExitStatusExt;
        if let Some(signal) = status.signal() {
            return format!("stopped by signal {signal}");
        }
    }
    "ended".into()
}

fn terminate_group(pid: u32, kill: bool) {
    #[cfg(unix)]
    {
        let Ok(pid) = i32::try_from(pid) else { return };
        if pid <= 1 {
            return;
        }
        let signal = if kill { libc::SIGKILL } else { libc::SIGTERM };
        // SAFETY: `kill` with a negative pid signals the process group the
        // agent was started in (`process_group(0)` makes its pgid its pid).
        unsafe {
            libc::kill(-pid, signal);
        }
    }
    #[cfg(windows)]
    {
        if kill {
            let _ = Command::new("taskkill")
                .args(["/PID", &pid.to_string(), "/T", "/F"])
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .status_locked();
        } else {
            let _ = Command::new("taskkill")
                .args(["/PID", &pid.to_string(), "/T"])
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .status_locked();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Instant;

    type Seen = Arc<Mutex<Vec<(&'static str, Value)>>>;

    fn collector() -> (Emit, Seen) {
        let seen = Arc::new(Mutex::new(Vec::new()));
        let sink = Arc::clone(&seen);
        (
            Arc::new(move |name, value| sink.lock().unwrap().push((name, value))),
            seen,
        )
    }

    fn wait_for(
        seen: &Arc<Mutex<Vec<(&'static str, Value)>>>,
        what: impl Fn(&[(&'static str, Value)]) -> bool,
    ) {
        let started = Instant::now();
        while started.elapsed() < Duration::from_secs(10) {
            if what(&seen.lock().unwrap()) {
                return;
            }
            std::thread::sleep(Duration::from_millis(20));
        }
        panic!("timed out; saw {:?}", seen.lock().unwrap());
    }

    fn lines_of(seen: &[(&'static str, Value)]) -> Vec<Value> {
        seen.iter()
            .filter(|(name, _)| *name == EVENT)
            .flat_map(|(_, v)| v["entries"].as_array().cloned().unwrap_or_default())
            .map(|e| serde_json::from_str(e["line"].as_str().unwrap()).unwrap_or(Value::Null))
            .collect()
    }

    #[test]
    fn read_lines_bounds_runaway_lines() {
        let mut input = b"a\n\nb\r\n".to_vec();
        input.extend(vec![b'x'; MAX_READ_LINE + 10]);
        input.extend(b"\nc");
        let mut got = Vec::new();
        read_lines(&input[..], |l| got.push(l));
        assert_eq!(got[0], "a");
        assert_eq!(got[1], "b");
        assert!(got[2].contains("mavi_truncated"));
        assert_eq!(got[3], "c");
    }

    /// A fake agent (a shell script) that echoes stream-json the way Claude
    /// Code does: an init, a permission request, then a result.
    #[cfg(unix)]
    #[test]
    fn a_session_relays_lines_tracks_approvals_and_records_the_turn() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        let git = |args: &[&str]| {
            assert!(Command::new("git")
                .args([
                    "-c",
                    "user.email=t@t",
                    "-c",
                    "user.name=t",
                    "-c",
                    "commit.gpgsign=false"
                ])
                .args(args)
                .current_dir(root)
                .status_locked()
                .unwrap()
                .success())
        };
        git(&["init", "-q"]);
        std::fs::write(root.join("a.txt"), "a\n").unwrap();
        git(&["add", "."]);
        git(&["commit", "-qm", "init"]);
        let root_text = root.to_string_lossy().into_owned();
        let script = format!(
            r#"read first
echo '{{"type":"system","subtype":"init","session_id":"0d235a3d-03cf-44a9-ad04-effb59321456","tools":["Edit"]}}'
echo '{{"type":"assistant","message":{{"id":"m1","content":[{{"type":"tool_use","id":"t1","name":"Edit","input":{{"file_path":"{root_text}/a.txt","old_string":"a","new_string":"b"}}}}]}}}}'
echo '{{"type":"control_request","request_id":"r1","request":{{"subtype":"can_use_tool","tool_name":"Edit","input":{{"file_path":"{root_text}/a.txt"}},"tool_use_id":"t1"}}}}'
read answer
echo "$answer" > answer.json
echo b > a.txt
echo shell > made-by-shell.txt
echo 'warning on stderr' >&2
echo '{{"type":"user","message":{{"role":"user","content":[{{"type":"tool_result","tool_use_id":"t1","content":"ok"}}]}}}}'
echo '{{"type":"result","subtype":"success","total_cost_usd":0.5}}'
read never
"#
        );
        let mut command = Command::new("sh");
        command.args(["-c", &script]).current_dir(root);
        let (emit, seen) = collector();
        let session = Session::spawn(
            "t".into(),
            MaviTarget::Local {
                root: root_text.clone(),
            },
            "ask".into(),
            None,
            command,
            emit,
        )
        .unwrap();
        assert_eq!(session.summary().phase, MaviPhase::Ready);
        session.send("change a").unwrap();
        wait_for(&seen, |s| {
            s.iter()
                .any(|(n, v)| *n == STATUS_EVENT && v["phase"] == "approval")
        });
        assert!(
            session.send("again").is_err(),
            "a second turn is refused while one runs"
        );
        assert!(session.answer("nope", wire::Decision::Allow, None).is_err());
        session.answer("r1", wire::Decision::Allow, None).unwrap();
        wait_for(&seen, |s| {
            lines_of(s).iter().any(|l| l["type"] == "mavi_turn_files")
        });
        // stderr is its own pipe: its line may land after the turn's.
        wait_for(&seen, |s| {
            lines_of(s).iter().any(|l| l["type"] == "mavi_stderr")
        });

        let summary = session.summary();
        assert_eq!(summary.phase, MaviPhase::Idle);
        assert_eq!(summary.turns, 1);
        assert_eq!(summary.cost_usd, 0.5);
        assert_eq!(summary.touched, vec!["a.txt"]);
        assert_eq!(
            summary.conversation_id.as_deref(),
            Some("0d235a3d-03cf-44a9-ad04-effb59321456")
        );

        let lines = lines_of(&seen.lock().unwrap());
        let kinds: Vec<&str> = lines
            .iter()
            .map(|l| l["type"].as_str().unwrap_or("?"))
            .collect();
        assert_eq!(kinds.first(), Some(&"mavi_user"));
        assert!(kinds.contains(&"mavi_answered"));
        assert!(kinds.contains(&"mavi_stderr"));
        let turn = lines
            .iter()
            .find(|l| l["type"] == "mavi_turn_files")
            .unwrap();
        let moved: Vec<&str> = turn["moved"]
            .as_array()
            .unwrap()
            .iter()
            .map(|m| m["path"].as_str().unwrap())
            .collect();
        assert!(
            moved.contains(&"a.txt") && moved.contains(&"made-by-shell.txt"),
            "{moved:?}"
        );

        // The answer echoed the agent's own input.
        let answer: Value =
            serde_json::from_str(&std::fs::read_to_string(root.join("answer.json")).unwrap())
                .unwrap();
        assert_eq!(
            answer["response"]["response"]["updatedInput"]["file_path"],
            format!("{root_text}/a.txt")
        );

        // MaviReplay returns every kept line, and nothing before the cursor.
        let (all, next) = session.replay(0);
        assert!(all.len() >= 6);
        let (tail, _) = session.replay(next - 1);
        assert!(tail.len() <= 1);

        session.stop();
        wait_for(&seen, |s| {
            lines_of(s).iter().any(|l| l["type"] == "mavi_exit")
        });
        assert_eq!(session.summary().phase, MaviPhase::Ended);
        assert!(session.send("after").is_err());
    }

    /// The real Claude Code CLI through the real host: an edit is requested,
    /// approved here, applied, and the turn's fingerprint sees the file.
    /// Opt-in (it spends tokens): `MAVI_LIVE_CLAUDE=1 cargo test --lib -- --ignored mavi_live`.
    #[cfg(unix)]
    #[test]
    #[ignore]
    fn mavi_live_claude_edit_round_trip() {
        if std::env::var("MAVI_LIVE_CLAUDE").as_deref() != Ok("1") {
            return;
        }
        let dir = tempfile::tempdir().unwrap();
        let root = std::fs::canonicalize(dir.path()).unwrap();
        let git = |args: &[&str]| {
            assert!(Command::new("git")
                .args([
                    "-c",
                    "user.email=t@t",
                    "-c",
                    "user.name=t",
                    "-c",
                    "commit.gpgsign=false"
                ])
                .args(args)
                .current_dir(&root)
                .status_locked()
                .unwrap()
                .success())
        };
        git(&["init", "-q"]);
        std::fs::write(root.join("a.txt"), "hello\n").unwrap();
        git(&["add", "."]);
        git(&["commit", "-qm", "init"]);
        let mut args: Vec<String> = super::super::CLAUDE_STRUCTURED
            .iter()
            .map(|s| (*s).to_owned())
            .collect();
        args = crate::workbench::terminal_command::apply_permission_mode(
            Some("claude"),
            Some("ask"),
            false,
            Some(args),
        )
        .unwrap()
        .unwrap();
        let mut command = Command::new("claude");
        command.args(&args).current_dir(&root);
        let (emit, seen) = collector();
        let root_text = root.to_string_lossy().into_owned();
        let session = Session::spawn(
            "live".into(),
            MaviTarget::Local { root: root_text },
            "ask".into(),
            None,
            command,
            emit,
        )
        .unwrap();
        session.send("Use the Edit tool to append the line world to a.txt. Do nothing else, then reply done.").unwrap();
        let started = Instant::now();
        loop {
            assert!(
                started.elapsed() < Duration::from_secs(180),
                "timed out: {:?}",
                session.summary()
            );
            let request = lines_of(&seen.lock().unwrap())
                .into_iter()
                .find(|l| l["type"] == "control_request" && l["request"]["tool_name"] == "Edit")
                .and_then(|l| l["request_id"].as_str().map(str::to_owned));
            if let Some(id) = request {
                session.answer(&id, wire::Decision::Allow, None).unwrap();
                break;
            }
            std::thread::sleep(Duration::from_millis(100));
        }
        let started = Instant::now();
        while !lines_of(&seen.lock().unwrap())
            .iter()
            .any(|l| l["type"] == "mavi_turn_files")
        {
            assert!(
                started.elapsed() < Duration::from_secs(180),
                "no turn end: {:?}",
                session.summary()
            );
            std::thread::sleep(Duration::from_millis(100));
        }
        assert_eq!(
            std::fs::read_to_string(root.join("a.txt")).unwrap(),
            "hello\nworld\n"
        );
        let lines = lines_of(&seen.lock().unwrap());
        let turn = lines
            .iter()
            .find(|l| l["type"] == "mavi_turn_files")
            .unwrap();
        assert!(
            turn["moved"]
                .as_array()
                .unwrap()
                .iter()
                .any(|m| m["path"] == "a.txt"),
            "{turn}"
        );
        let patched = lines
            .iter()
            .any(|l| l["type"] == "user" && l["tool_use_result"]["structuredPatch"].is_array());
        assert!(patched, "the edit's structuredPatch reached the page");
        assert!(
            lines
                .iter()
                .all(|l| l["tool_use_result"].get("originalFile").is_none()),
            "originalFile was slimmed"
        );
        let summary = session.summary();
        assert_eq!(summary.touched, vec!["a.txt"]);
        assert!(summary.conversation_id.is_some());
        session.stop();
    }
}
