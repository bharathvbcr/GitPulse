//! Option A scoped child terminal orchestration (`MCP` + `gitpulse-hook term`).
//!
//! Security boundary:
//! - Every PTY session started through GitPulse receives an unguessable
//!   capability token (`GITPULSE_SESSION_TOKEN`, `gpst_<64-hex>`). Only its
//!   `Sha256` digest is retained in memory, compared in constant time, and
//!   revoked the instant the session exits.
//! - **Option A lineage:** a caller session may `list`, `wait`, `read`, or
//!   `send` **only** child sessions whose `parent_session_id` equals the
//!   authenticated `caller_session_id`. User-opened shells (`parent_session_id
//!   == None`), the caller itself, and children belonging to any other parent
//!   return the same uniform refusal so no existence oracle exists.
//! - Child commands (`Spawn` initial shell command and `Send` input lines) run
//!   through [`crate::hooks::gate_child_command`] (`signal_guard` + Manvi
//!   command gate, bypass disabled) before any byte reaches the child PTY.
//! - Headless 2D terminal reads render through `vt100::Parser` (`rows × cols`,
//!   bounded scrollback) and scrub every returned string through
//!   [`crate::ledger::redact::text`] plus `gpst_` capability-token redaction.

use crate::engine::git_cli::{sandbox_join_canonical, validate_repo};
use crate::terminal::{
    bounded_pty_size, detach_session, is_session_id, spawn_at, write_input, OutputSink, Place,
    SessionEvents, SharedSessionTelemetry, TerminalSessions,
};
#[cfg(unix)]
use base64::Engine as _;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::collections::{HashMap, VecDeque};
use std::path::Path;
use std::sync::{Condvar, Mutex};
use std::time::{Duration, Instant};

/// Capability token environment variable injected into every GitPulse PTY.
pub const SESSION_TOKEN_ENV: &str = "GITPULSE_SESSION_TOKEN";
/// Prefix on every minted [`SESSION_TOKEN_ENV`] value so capability tokens can
/// be recognized and scrubbed unconditionally from terminal screen reads.
pub const SESSION_TOKEN_PREFIX: &str = "gpst_";
/// Emitted when an authenticated parent session spawns a scoped child session.
pub const CHILD_SPAWNED_EVENT: &str = "terminal-child-spawned";
/// Emitted when an authenticated session asks the UI to open a repository file
/// in a terminal split pane.
pub const OPEN_FILE_REQUEST_EVENT: &str = "terminal-open-file-request";
/// Emitted when an authenticated parent session closes one of its spawned children.
pub const CHILD_CLOSED_EVENT: &str = "terminal-child-closed";

pub const MAX_WAIT_TIMEOUT_MS: u64 = 25_000;
pub const DEFAULT_WAIT_TIMEOUT_MS: u64 = 10_000;
pub const MAX_READ_LINES: usize = 1_000;
pub const DEFAULT_READ_LINES: usize = 80;
pub const MAX_READ_BYTES: usize = 64 * 1024;
pub const MAX_SEND_BYTES: usize = 16 * 1024;
pub const MAX_PROMPT_BYTES: usize = 8 * 1024;
pub const MAX_PATTERN_BYTES: usize = 512;
pub const MAX_CHILD_TITLE_BYTES: usize = 120;
pub const MAX_CHILDREN_PER_PARENT: usize = 8;
pub const MAX_EXTERNAL_ORCHESTRATED_CHILDREN: usize = 16;
const PARSER_SCROLLBACK_ROWS: usize = MAX_READ_LINES * 2;
const MAX_ENDED_CHILDREN: usize = 32;
const HEX_TOKEN_LEN: usize = 64;
const FULL_TOKEN_LEN: usize = SESSION_TOKEN_PREFIX.len() + HEX_TOKEN_LEN;

const CREDENTIAL_REFUSAL: &str = "invalid or expired terminal session credentials";
const LINEAGE_REFUSAL: &str = "child terminal session is not owned by this caller";
const MAX_RECORDED_BLOCKS: usize = 16;
const MAX_BLOCK_CMD_BYTES: usize = 4 * 1024;
const MAX_BLOCK_RAW_BYTES: usize = MAX_READ_BYTES * 2;
const MAX_EXTERNAL_CALLER_SUFFIX_LEN: usize = 48;

pub(crate) fn is_valid_external_caller_id(id: &str) -> bool {
    let suffix = if let Some(s) = id.strip_prefix("mcp-") {
        s
    } else if let Some(s) = id.strip_prefix("ext-") {
        s
    } else {
        return false;
    };
    !suffix.is_empty()
        && suffix.len() <= MAX_EXTERNAL_CALLER_SUFFIX_LEN
        && suffix
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
}

/// Allowed child launchers for `Spawn`.
pub const ALLOWED_CHILD_LAUNCHERS: [&str; 6] = ["shell", "claude", "agy", "codex", "manvi", "grok"];

/// Metadata attached when [`spawn_at`] spawns an orchestrated child.
#[derive(Debug, Clone)]
pub(crate) struct ChildSpawnMeta {
    pub parent_session_id: String,
    pub launcher: String,
    pub title: Option<String>,
}

/// Event payload emitted on [`CHILD_SPAWNED_EVENT`]. Matches
/// `TerminalChildSpawnedPayload` in `src/lib/terminal/agentEvents.ts`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TerminalChildSpawnedPayload {
    pub id: String,
    pub parent_session_id: String,
    pub repo_path: String,
    pub cwd: String,
    pub shell: String,
    pub launcher: String,
    pub title: Option<String>,
    pub split: bool,
}

/// Event payload emitted on [`OPEN_FILE_REQUEST_EVENT`]. Matches
/// `TerminalOpenFileRequestPayload` in `src/lib/terminal/agentEvents.ts`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TerminalOpenFileRequestPayload {
    pub caller_session_id: String,
    pub repo_path: String,
    pub path: String,
    pub line: Option<u32>,
    pub column: Option<u32>,
}

/// Event payload emitted on [`CHILD_CLOSED_EVENT`]. Matches
/// `TerminalChildClosedPayload` in `src/lib/terminal/agentEvents.ts`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TerminalChildClosedPayload {
    pub id: String,
    pub parent_session_id: String,
    pub repo_path: String,
}

/// Abstraction over UI event emission so real runs emit Tauri events while unit
/// tests inspect emitted payloads without constructing a Tauri `AppHandle`.
pub trait OrchestrateUiSink: Send + Sync {
    fn child_spawned(&self, payload: TerminalChildSpawnedPayload);
    fn child_closed(&self, payload: TerminalChildClosedPayload);
    fn open_file_requested(&self, payload: TerminalOpenFileRequestPayload);
    fn session_events(&self, state: &TerminalSessions) -> std::sync::Arc<dyn SessionEvents>;
}

/// Production UI sink backed by a Tauri `AppHandle`.
pub struct AppOrchestrateUiSink<R: tauri::Runtime = tauri::Wry> {
    app: tauri::AppHandle<R>,
}

impl<R: tauri::Runtime> AppOrchestrateUiSink<R> {
    pub fn new(app: tauri::AppHandle<R>) -> Self {
        Self { app }
    }
}

impl<R: tauri::Runtime> OrchestrateUiSink for AppOrchestrateUiSink<R> {
    fn child_spawned(&self, payload: TerminalChildSpawnedPayload) {
        use tauri::Emitter as _;
        if let Err(error) = self.app.emit(CHILD_SPAWNED_EVENT, payload) {
            log::warn!(target: "terminal", "could not announce spawned child terminal: {error}");
        }
    }

    fn child_closed(&self, payload: TerminalChildClosedPayload) {
        use tauri::Emitter as _;
        if let Err(error) = self.app.emit(CHILD_CLOSED_EVENT, payload) {
            log::warn!(target: "terminal", "could not announce closed child terminal: {error}");
        }
    }

    fn open_file_requested(&self, payload: TerminalOpenFileRequestPayload) {
        use tauri::Emitter as _;
        if let Err(error) = self.app.emit(OPEN_FILE_REQUEST_EVENT, payload) {
            log::warn!(target: "terminal", "could not announce terminal open-file request: {error}");
        }
    }

    fn session_events(&self, state: &TerminalSessions) -> std::sync::Arc<dyn SessionEvents> {
        std::sync::Arc::new(crate::terminal::AppEvents::new(
            &self.app,
            state.incidents.clone(),
            state.session_ledger.clone(),
        ))
    }
}

/// Mints a fresh 256-bit CSPRNG session capability token (`gpst_<64 lowercase hex>`).
pub(crate) fn mint_session_token() -> Result<String, String> {
    let mut bytes = [0u8; 32];
    getrandom::fill(&mut bytes)
        .map_err(|error| format!("failed to generate terminal session token: {error}"))?;
    let mut out = String::with_capacity(FULL_TOKEN_LEN);
    out.push_str(SESSION_TOKEN_PREFIX);
    for byte in bytes {
        use std::fmt::Write as _;
        let _ = write!(&mut out, "{byte:02x}");
    }
    Ok(out)
}

fn is_well_formed_token(token: &str) -> bool {
    let Some(hex) = token.strip_prefix(SESSION_TOKEN_PREFIX) else {
        return false;
    };
    hex.len() == HEX_TOKEN_LEN
        && hex
            .bytes()
            .all(|b| b.is_ascii_hexdigit() && !b.is_ascii_uppercase())
}

fn hash_token(token: &str) -> Result<[u8; 32], String> {
    if !is_well_formed_token(token) {
        return Err(CREDENTIAL_REFUSAL.to_string());
    }
    let digest = Sha256::digest(token.as_bytes());
    let mut out = [0u8; 32];
    out.copy_from_slice(&digest);
    Ok(out)
}

pub(crate) fn token_hash_hex(token: &str) -> Result<String, String> {
    let digest = hash_token(token)?;
    let mut out = String::with_capacity(64);
    for byte in digest {
        use std::fmt::Write as _;
        let _ = write!(&mut out, "{byte:02x}");
    }
    Ok(out)
}

fn decode_token_hash_hex(hex: &str) -> Option<[u8; 32]> {
    if hex.len() != 64 {
        return None;
    }
    let mut out = [0u8; 32];
    let bytes = hex.as_bytes();
    for (i, slot) in out.iter_mut().enumerate() {
        let hi = (bytes[i * 2] as char).to_digit(16)?;
        let lo = (bytes[i * 2 + 1] as char).to_digit(16)?;
        *slot = ((hi << 4) | lo) as u8;
    }
    Some(out)
}

fn ct_eq_32(a: &[u8; 32], b: &[u8; 32]) -> bool {
    let mut diff = 0u8;
    for i in 0..32 {
        diff |= a[i] ^ b[i];
    }
    diff == 0
}

/// Scrubs secrets via [`crate::ledger::redact::text`] and replaces any
/// `gpst_<64-hex>` capability token with `[REDACTED]`.
pub(crate) fn scrub_screen_text(raw: &str) -> String {
    let scrubbed = crate::ledger::redact::text(raw);
    scrub_session_tokens(&scrubbed)
}

fn scrub_session_tokens(input: &str) -> String {
    if !input.contains(SESSION_TOKEN_PREFIX) {
        return input.to_owned();
    }
    let mut out = String::with_capacity(input.len());
    let mut rest = input;
    while let Some(pos) = rest.find(SESSION_TOKEN_PREFIX) {
        out.push_str(&rest[..pos]);
        let after_prefix = &rest[pos + SESSION_TOKEN_PREFIX.len()..];
        let hex_ok = after_prefix.len() >= HEX_TOKEN_LEN
            && after_prefix.as_bytes()[..HEX_TOKEN_LEN]
                .iter()
                .all(u8::is_ascii_hexdigit);
        if hex_ok {
            out.push_str("<redacted>");
            rest = &after_prefix[HEX_TOKEN_LEN..];
        } else {
            out.push_str(SESSION_TOKEN_PREFIX);
            rest = after_prefix;
        }
    }
    out.push_str(rest);
    out
}

fn nz_u16(value: u16) -> std::num::NonZeroU16 {
    std::num::NonZeroU16::new(value.max(1)).expect("clamped >= 1")
}

fn discard_sink() -> OutputSink {
    OutputSink::from_fn(|_, _| Ok(()))
}

#[derive(Debug, Clone)]
pub(crate) struct RecordedCommandBlock {
    command: Option<String>,
    output: String,
    exit_code: Option<i32>,
    tag: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
enum OscCaptureState {
    #[default]
    Ground,
    Esc,
    Osc,
    OscEsc,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
enum BlockCapturePhase {
    #[default]
    Idle,
    Command,
    Output,
}

#[derive(Debug, Default)]
struct CommandBlockRecorder {
    osc_state: OscCaptureState,
    osc_buf: Vec<u8>,
    osc_overflow: bool,
    phase: BlockCapturePhase,
    cmd_buf: Vec<u8>,
    out_buf: Vec<u8>,
    pending_cmd: Option<String>,
    blocks: VecDeque<RecordedCommandBlock>,
    total_recorded: u64,
}

impl CommandBlockRecorder {
    fn render_fragment(raw: &[u8], rows: u16, cols: u16) -> String {
        if raw.is_empty() {
            return String::new();
        }
        let mut parser = vt100::Parser::new(
            nz_u16(rows.max(4)),
            nz_u16(cols.max(20)),
            PARSER_SCROLLBACK_ROWS,
        );
        parser.process(raw);
        let mut lines = extract_parser_logical_lines(&parser);
        while lines.last().is_some_and(|l| l.trim().is_empty()) {
            lines.pop();
        }
        scrub_screen_text(&lines.join("\n"))
    }

    fn push_raw_byte(&mut self, byte: u8) {
        match self.phase {
            BlockCapturePhase::Idle => {}
            BlockCapturePhase::Command => {
                if self.cmd_buf.len() < MAX_BLOCK_CMD_BYTES {
                    self.cmd_buf.push(byte);
                }
            }
            BlockCapturePhase::Output => {
                if self.out_buf.len() < MAX_BLOCK_RAW_BYTES {
                    self.out_buf.push(byte);
                }
            }
        }
    }

    fn handle_osc(&mut self, cols: u16) {
        if self.osc_overflow {
            return;
        }
        let Ok(raw) = std::str::from_utf8(&self.osc_buf) else {
            return;
        };
        let Some(rest) = raw.strip_prefix("133;") else {
            return;
        };
        let mut parts = rest.trim().split(';');
        match parts.next() {
            Some("A") => {
                self.phase = BlockCapturePhase::Idle;
                self.cmd_buf.clear();
                self.pending_cmd = None;
            }
            Some("B") => {
                self.phase = BlockCapturePhase::Command;
                self.cmd_buf.clear();
                self.out_buf.clear();
                self.pending_cmd = None;
            }
            Some("C") => {
                if self.phase == BlockCapturePhase::Command && !self.cmd_buf.is_empty() {
                    let rendered = Self::render_fragment(&self.cmd_buf, 8, cols);
                    let trimmed = rendered
                        .lines()
                        .last()
                        .map(str::trim)
                        .filter(|s| !s.is_empty())
                        .map(str::to_owned);
                    self.pending_cmd = trimmed;
                }
                self.cmd_buf.clear();
                self.out_buf.clear();
                self.phase = BlockCapturePhase::Output;
            }
            Some("D") => {
                if self.phase != BlockCapturePhase::Output {
                    return;
                }
                let exit_code = parts
                    .next()
                    .map(str::trim)
                    .filter(|s| !s.is_empty())
                    .and_then(|s| s.parse::<i32>().ok());
                let tag = parts
                    .next()
                    .map(str::trim)
                    .filter(|s| !s.is_empty())
                    .map(str::to_owned);
                let mut output = Self::render_fragment(&self.out_buf, 24, cols);
                if output.len() > MAX_READ_BYTES {
                    let mut cut = output.len().saturating_sub(MAX_READ_BYTES);
                    while cut < output.len() && !output.is_char_boundary(cut) {
                        cut += 1;
                    }
                    output = output[cut..].to_string();
                }
                self.blocks.push_back(RecordedCommandBlock {
                    command: self.pending_cmd.take(),
                    output,
                    exit_code,
                    tag,
                });
                while self.blocks.len() > MAX_RECORDED_BLOCKS {
                    self.blocks.pop_front();
                }
                self.total_recorded = self.total_recorded.saturating_add(1);
                self.out_buf.clear();
                self.phase = BlockCapturePhase::Idle;
            }
            _ => {}
        }
    }

    fn feed(&mut self, bytes: &[u8], cols: u16) {
        for &byte in bytes {
            match self.osc_state {
                OscCaptureState::Ground => {
                    if byte == 0x1b {
                        self.osc_state = OscCaptureState::Esc;
                    } else {
                        self.push_raw_byte(byte);
                    }
                }
                OscCaptureState::Esc => {
                    if byte == b']' {
                        self.osc_state = OscCaptureState::Osc;
                        self.osc_buf.clear();
                        self.osc_overflow = false;
                    } else {
                        self.push_raw_byte(0x1b);
                        if byte == 0x1b {
                            // Keep Esc state for sequential ESC ESC bytes.
                        } else {
                            self.push_raw_byte(byte);
                            self.osc_state = OscCaptureState::Ground;
                        }
                    }
                }
                OscCaptureState::Osc => {
                    if byte == 0x07 {
                        self.handle_osc(cols);
                        self.osc_state = OscCaptureState::Ground;
                    } else if byte == 0x1b {
                        self.osc_state = OscCaptureState::OscEsc;
                    } else if self.osc_buf.len() < 512 {
                        self.osc_buf.push(byte);
                    } else {
                        self.osc_overflow = true;
                    }
                }
                OscCaptureState::OscEsc => {
                    if byte == b'\\' {
                        self.handle_osc(cols);
                        self.osc_state = OscCaptureState::Ground;
                    } else if byte == 0x1b {
                        self.push_osc_byte(0x1b);
                    } else {
                        self.push_osc_byte(0x1b);
                        self.push_osc_byte(byte);
                        self.osc_state = OscCaptureState::Osc;
                    }
                }
            }
        }
    }

    fn push_osc_byte(&mut self, byte: u8) {
        if self.osc_buf.len() < 512 {
            self.osc_buf.push(byte);
        } else {
            self.osc_overflow = true;
        }
    }

    fn last_completed(&self) -> Option<&RecordedCommandBlock> {
        self.blocks.back()
    }

    fn find_by_tag(&self, tag: &str) -> Option<&RecordedCommandBlock> {
        self.blocks
            .iter()
            .rev()
            .find(|blk| blk.tag.as_deref() == Some(tag))
    }

    fn has_tag(&self, tag: &str) -> bool {
        self.find_by_tag(tag).is_some()
    }
}

struct OrchestratedRecord {
    token_hash: Option<[u8; 32]>,
    parent_session_id: Option<String>,
    repo_path: String,
    cwd: String,
    launcher: String,
    title: Option<String>,
    rows: u16,
    cols: u16,
    ended: bool,
    exit_code: Option<i32>,
    observed_bytes: u64,
    baseline_event_seq: u64,
    parser: Option<vt100::Parser>,
    host_scanner: Option<crate::alerts::scan::Scanner>,
    blocks: Option<CommandBlockRecorder>,
    telemetry: SharedSessionTelemetry,
}

fn apply_scanner_telemetry(
    scanner: &mut crate::alerts::scan::Scanner,
    telemetry: &SharedSessionTelemetry,
    bytes: &[u8],
) {
    let _ = scanner.feed(bytes);
    let events = scanner.drain_telemetry();
    if events.is_empty() {
        return;
    }
    let (lock, cvar) = &**telemetry;
    let mut state = lock
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let (notify, _, _) = crate::terminal::apply_telemetry_events(&mut state, events);
    if notify {
        cvar.notify_all();
    }
}

#[derive(Default)]
struct RegistryInner {
    sessions: HashMap<String, OrchestratedRecord>,
    ended_children: VecDeque<String>,
}

/// Thread-safe registry of session capability tokens, Option A parent->child
/// lineage edges, and headless `vt100` screens for orchestrated child sessions.
#[derive(Default)]
pub struct OrchestrationRegistry {
    inner: Mutex<RegistryInner>,
    changed: Condvar,
}

impl OrchestrationRegistry {
    fn lock(&self) -> std::sync::MutexGuard<'_, RegistryInner> {
        self.inner
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    #[allow(clippy::too_many_arguments)]
    pub(crate) fn register_session(
        &self,
        session_id: &str,
        token: &str,
        repo_path: &Path,
        cwd: &Path,
        launcher: &str,
        rows: u16,
        cols: u16,
        telemetry: SharedSessionTelemetry,
        child_meta: Option<ChildSpawnMeta>,
    ) -> Result<(), String> {
        let digest = hash_token(token)?;
        let canonical_repo = std::fs::canonicalize(repo_path)
            .unwrap_or_else(|_| repo_path.to_path_buf())
            .to_string_lossy()
            .into_owned();
        let size = bounded_pty_size(rows, cols);
        let (parent_session_id, effective_launcher, title) = match child_meta {
            Some(meta) => (Some(meta.parent_session_id), meta.launcher, meta.title),
            None => (None, launcher.to_owned(), None),
        };
        let track_screen = parent_session_id.is_some() || effective_launcher == "shell";
        let parser = track_screen.then(|| {
            vt100::Parser::new(
                nz_u16(size.rows),
                nz_u16(size.cols),
                PARSER_SCROLLBACK_ROWS,
            )
        });
        let host_scanner = track_screen.then(crate::alerts::scan::Scanner::new);
        let blocks = track_screen.then(CommandBlockRecorder::default);
        let mut inner = self.lock();
        inner.sessions.insert(
            session_id.to_owned(),
            OrchestratedRecord {
                token_hash: Some(digest),
                parent_session_id,
                repo_path: canonical_repo,
                cwd: cwd.to_string_lossy().into_owned(),
                launcher: effective_launcher,
                title,
                rows: size.rows,
                cols: size.cols,
                ended: false,
                exit_code: None,
                observed_bytes: 0,
                baseline_event_seq: 0,
                parser,
                host_scanner,
                blocks,
                telemetry,
            },
        );
        self.changed.notify_all();
        Ok(())
    }

    pub(crate) fn notify_changed(&self) {
        self.changed.notify_all();
    }

    /// Resets an orchestrated child's `vt100` screen right before `gitpulse-ptyd`
    /// streams its ring replay on `Attach`, `AttachTracked`, or reconnect so
    /// replayed bytes never duplicate onto an already-populated grid.
    pub(crate) fn reset_parser_for_attach_replay(&self, session_id: &str, rows: u16, cols: u16) {
        let size = bounded_pty_size(rows, cols);
        let mut inner = self.lock();
        if let Some(record) = inner.sessions.get_mut(session_id) {
            record.rows = size.rows;
            record.cols = size.cols;
            if record.parser.is_some() {
                record.parser = Some(vt100::Parser::new(
                    nz_u16(size.rows),
                    nz_u16(size.cols),
                    PARSER_SCROLLBACK_ROWS,
                ));
                if record.blocks.is_some() {
                    record.blocks = Some(CommandBlockRecorder::default());
                }
                record.observed_bytes = 0;
            }
        }
    }

    /// Restores parent/child lineage, RAM-only caller capability token digests,
    /// and OSC telemetry from `gitpulse-ptyd` across app restarts or socket
    /// reconnects.
    pub(crate) fn adopt_hosted_sessions(&self, snapshots: &[crate::terminal::HostedSessionMeta]) {
        if snapshots.is_empty() {
            return;
        }
        let mut inner = self.lock();
        for snap in snapshots {
            let size = bounded_pty_size(snap.rows, snap.cols);
            let canonical_repo = std::fs::canonicalize(&snap.repo)
                .map(|p| p.to_string_lossy().into_owned())
                .unwrap_or_else(|_| snap.repo.clone());
            let decoded_hash = snap
                .token_hash_hex
                .as_deref()
                .and_then(decode_token_hash_hex);
            if let Some(existing) = inner.sessions.get_mut(&snap.session_id) {
                existing.rows = size.rows;
                existing.cols = size.cols;
                if existing.token_hash.is_none() && !existing.ended {
                    existing.token_hash = decoded_hash;
                }
                if existing.parent_session_id.is_none() {
                    existing.parent_session_id = snap.parent_session_id.clone();
                }
                let mut tel = existing
                    .telemetry
                    .0
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner);
                if snap.telemetry.event_seq >= tel.event_seq {
                    *tel = snap.telemetry.clone();
                }
                continue;
            }
            let effective_launcher = snap
                .child_launcher
                .clone()
                .unwrap_or_else(|| snap.launcher.clone());
            let is_child = snap.parent_session_id.is_some();
            let track_screen = is_child || effective_launcher == "shell";
            let telemetry = crate::terminal::new_shared_telemetry();
            *telemetry
                .0
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner) = snap.telemetry.clone();
            inner.sessions.insert(
                snap.session_id.clone(),
                OrchestratedRecord {
                    token_hash: decoded_hash,
                    parent_session_id: snap.parent_session_id.clone(),
                    repo_path: canonical_repo,
                    cwd: snap.cwd.clone(),
                    launcher: effective_launcher,
                    title: snap.child_title.clone(),
                    rows: size.rows,
                    cols: size.cols,
                    ended: snap.telemetry.ended,
                    exit_code: snap.telemetry.last_exit_code,
                    observed_bytes: 0,
                    baseline_event_seq: snap.telemetry.event_seq,
                    parser: track_screen.then(|| {
                        vt100::Parser::new(
                            nz_u16(size.rows),
                            nz_u16(size.cols),
                            PARSER_SCROLLBACK_ROWS,
                        )
                    }),
                    host_scanner: track_screen.then(crate::alerts::scan::Scanner::new),
                    blocks: track_screen.then(CommandBlockRecorder::default),
                    telemetry,
                },
            );
        }
        drop(inner);
        self.changed.notify_all();
    }

    pub(crate) fn telemetry_for(&self, session_id: &str) -> Option<SharedSessionTelemetry> {
        self.lock()
            .sessions
            .get(session_id)
            .map(|r| r.telemetry.clone())
    }

    pub(crate) fn remove_aborted(&self, session_id: &str) {
        self.lock().sessions.remove(session_id);
        self.changed.notify_all();
    }

    /// Feeds raw PTY output bytes into an orchestrated child's `vt100` parser
    /// and wakes any parent `Wait` callers. No-op for non-child user sessions.
    pub(crate) fn observe_output(&self, session_id: &str, bytes: &[u8]) {
        if bytes.is_empty() {
            return;
        }
        let mut inner = self.lock();
        let Some(record) = inner.sessions.get_mut(session_id) else {
            return;
        };
        let cols = record.cols;
        if let Some(parser) = record.parser.as_mut() {
            parser.process(bytes);
            if let Some(blocks) = record.blocks.as_mut() {
                blocks.feed(bytes, cols);
            }
            record.observed_bytes = record.observed_bytes.saturating_add(bytes.len() as u64);
            drop(inner);
            self.changed.notify_all();
        }
    }

    /// Feeds raw PTY output bytes arriving from `gitpulse-ptyd` (`HostClient`)
    /// into both the child's `vt100` parser and its OSC telemetry scanner so
    /// `gitpulse-ptyd` and in-process sessions expose identical `Wait`/`Read`
    /// telemetry.
    #[cfg(unix)]
    pub(crate) fn observe_output_from_host(&self, session_id: &str, bytes: &[u8]) {
        if bytes.is_empty() {
            return;
        }
        let telemetry_arc = {
            let mut inner = self.lock();
            let Some(record) = inner.sessions.get_mut(session_id) else {
                return;
            };
            let cols = record.cols;
            let Some(parser) = record.parser.as_mut() else {
                return;
            };
            parser.process(bytes);
            if let Some(blocks) = record.blocks.as_mut() {
                blocks.feed(bytes, cols);
            }
            record.observed_bytes = record.observed_bytes.saturating_add(bytes.len() as u64);
            record.host_scanner.as_mut().map(|scanner| {
                apply_scanner_telemetry(scanner, &record.telemetry, bytes);
                record.telemetry.clone()
            })
        };
        let _ = telemetry_arc;
        self.changed.notify_all();
    }

    /// Keeps an orchestrated child's `vt100` grid dimensions aligned with PTY resizes.
    pub(crate) fn resize(&self, session_id: &str, rows: u16, cols: u16) {
        let size = bounded_pty_size(rows, cols);
        let mut inner = self.lock();
        if let Some(record) = inner.sessions.get_mut(session_id) {
            record.rows = size.rows;
            record.cols = size.cols;
            if let Some(parser) = record.parser.as_mut() {
                parser
                    .screen_mut()
                    .set_size(nz_u16(size.rows), nz_u16(size.cols));
            }
        }
    }

    /// Revokes `session_id`'s caller token immediately on exit. Non-child
    /// sessions are removed once exited; orchestrated children retain their
    /// final `vt100` screen (bounded to `MAX_ENDED_CHILDREN`) so their parent
    /// can complete a post-exit `wait` or `read`.
    pub(crate) fn mark_ended(&self, session_id: &str, exit_code: Option<i32>) {
        let mut inner = self.lock();
        let retain = match inner.sessions.get_mut(session_id) {
            Some(record) => {
                record.token_hash = None;
                record.ended = true;
                record.exit_code = exit_code;
                record.parent_session_id.is_some() || record.launcher == "shell"
            }
            None => return,
        };
        if retain {
            if !inner.ended_children.iter().any(|id| id == session_id) {
                inner.ended_children.push_back(session_id.to_owned());
            }
            while inner.ended_children.len() > MAX_ENDED_CHILDREN {
                if let Some(evicted) = inner.ended_children.pop_front() {
                    if inner.sessions.get(&evicted).is_some_and(|rec| {
                        rec.ended && (rec.parent_session_id.is_some() || rec.launcher == "shell")
                    }) {
                        inner.sessions.remove(&evicted);
                    }
                }
            }
        } else {
            inner.sessions.remove(session_id);
        }
        drop(inner);
        self.changed.notify_all();
    }

    /// Registers a synthetic parent session for hermetic unit/contract tests
    /// that exercise orchestration without spawning a second outer PTY.
    #[cfg(test)]
    pub(crate) fn register_test_parent(&self, session_id: &str, repo_path: &Path) -> String {
        let token = mint_session_token().expect("token");
        self.register_session(
            session_id,
            &token,
            repo_path,
            repo_path,
            "claude",
            24,
            80,
            crate::terminal::new_shared_telemetry(),
            None,
        )
        .expect("register test parent");
        token
    }
}

/// Wire request carried inside `desktop::instance::Request::TerminalOrchestrate`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "op", rename_all = "snake_case", deny_unknown_fields)]
pub enum OrchestrateCall {
    Spawn {
        repo_path: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        caller_session_id: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        caller_token: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        external_caller_id: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        launcher: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        cwd: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        command: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        prompt: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        title: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        split: Option<bool>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        rows: Option<u16>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        cols: Option<u16>,
    },
    Exec {
        repo_path: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        caller_session_id: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        caller_token: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        external_caller_id: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        child_session_id: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        cwd: Option<String>,
        command: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        timeout_ms: Option<u64>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        pattern: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        keep_open: Option<bool>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        split: Option<bool>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        title: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        max_lines: Option<usize>,
    },
    OpenFile {
        repo_path: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        caller_session_id: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        caller_token: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        external_caller_id: Option<String>,
        path: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        line: Option<u32>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        column: Option<u32>,
    },
    List {
        repo_path: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        caller_session_id: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        caller_token: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        external_caller_id: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        include_repo_shells: Option<bool>,
    },
    Wait {
        repo_path: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        caller_session_id: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        caller_token: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        external_caller_id: Option<String>,
        child_session_id: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        timeout_ms: Option<u64>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        pattern: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        wait_for_idle: Option<bool>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        since_seq: Option<u64>,
    },
    Read {
        repo_path: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        caller_session_id: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        caller_token: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        external_caller_id: Option<String>,
        child_session_id: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        max_lines: Option<usize>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        offset_lines: Option<usize>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        grep: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        last_command_only: Option<bool>,
    },
    Send {
        repo_path: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        caller_session_id: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        caller_token: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        external_caller_id: Option<String>,
        child_session_id: String,
        input: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        append_newline: Option<bool>,
    },
    Close {
        repo_path: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        caller_session_id: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        caller_token: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        external_caller_id: Option<String>,
        child_session_id: String,
    },
}

impl OrchestrateCall {
    /// Socket reply deadline for `Secondary` callers (`timeout_ms + 5s` on `Wait` and `Exec`).
    pub fn reply_deadline(&self) -> Duration {
        match self {
            Self::Wait { timeout_ms, .. } | Self::Exec { timeout_ms, .. } => {
                let bounded = timeout_ms
                    .unwrap_or(DEFAULT_WAIT_TIMEOUT_MS)
                    .clamp(1, MAX_WAIT_TIMEOUT_MS);
                Duration::from_millis(bounded).saturating_add(Duration::from_secs(5))
            }
            _ => Duration::from_secs(12),
        }
    }

    /// Structural wire validation run at the socket boundary before dispatch.
    pub fn validate(&self) -> Result<(), String> {
        let check_common = |repo: &str,
                            sid: Option<&str>,
                            tok: Option<&str>,
                            ext: Option<&str>|
         -> Result<(), String> {
            if repo.is_empty()
                || repo.len() > 4096
                || repo.contains('\0')
                || !Path::new(repo).is_absolute()
            {
                return Err("orchestrate repo_path must be an absolute path".into());
            }
            if sid.is_some() != tok.is_some() {
                return Err(
                    "orchestrate caller_session_id and caller_token must be provided together"
                        .into(),
                );
            }
            if let (Some(sid), Some(tok)) = (sid, tok) {
                if sid.is_empty() || sid.len() > 64 || sid.chars().any(char::is_control) {
                    return Err("orchestrate caller_session_id is invalid".into());
                }
                if tok.is_empty() || tok.len() > 128 || tok.chars().any(char::is_control) {
                    return Err("orchestrate caller_token is invalid".into());
                }
            } else {
                let Some(ext) = ext else {
                    return Err(CREDENTIAL_REFUSAL.into());
                };
                if !is_valid_external_caller_id(ext) {
                    return Err(CREDENTIAL_REFUSAL.into());
                }
            }
            Ok(())
        };
        let check_child = |cid: &str| -> Result<(), String> {
            if cid.is_empty() || cid.len() > 64 || cid.chars().any(char::is_control) {
                return Err("orchestrate child_session_id is invalid".into());
            }
            Ok(())
        };
        let check_pattern = |pat: &str, label: &str| -> Result<(), String> {
            if pat.is_empty() || pat.len() > MAX_PATTERN_BYTES || pat.contains('\0') {
                return Err(format!("orchestrate {label} is empty or exceeds 512 bytes"));
            }
            Ok(())
        };

        match self {
            Self::Spawn {
                repo_path,
                caller_session_id,
                caller_token,
                external_caller_id,
                launcher,
                cwd,
                command,
                prompt,
                title,
                ..
            } => {
                check_common(
                    repo_path,
                    caller_session_id.as_deref(),
                    caller_token.as_deref(),
                    external_caller_id.as_deref(),
                )?;
                if let Some(l) = launcher {
                    if !ALLOWED_CHILD_LAUNCHERS.contains(&l.trim()) {
                        return Err(format!(
                            "unsupported launcher '{l}'; expected one of {}",
                            ALLOWED_CHILD_LAUNCHERS.join(", ")
                        ));
                    }
                }
                if let Some(dir) = cwd {
                    if dir.len() > 1024 || dir.contains('\0') {
                        return Err("orchestrate cwd is invalid".into());
                    }
                }
                if let Some(cmd) = command {
                    if cmd.is_empty() || cmd.len() > MAX_SEND_BYTES {
                        return Err("orchestrate command is empty or exceeds 16 KiB".into());
                    }
                }
                if let Some(p) = prompt {
                    if p.is_empty() || p.len() > MAX_PROMPT_BYTES || p.contains('\0') {
                        return Err("orchestrate prompt is empty or exceeds 8 KiB".into());
                    }
                }
                if let Some(t) = title {
                    if t.len() > MAX_CHILD_TITLE_BYTES || t.chars().any(char::is_control) {
                        return Err(
                            "orchestrate title exceeds 120 bytes or has control chars".into()
                        );
                    }
                }
                Ok(())
            }
            Self::Exec {
                repo_path,
                caller_session_id,
                caller_token,
                external_caller_id,
                child_session_id,
                cwd,
                command,
                timeout_ms,
                pattern,
                title,
                max_lines,
                ..
            } => {
                check_common(
                    repo_path,
                    caller_session_id.as_deref(),
                    caller_token.as_deref(),
                    external_caller_id.as_deref(),
                )?;
                if let Some(cid) = child_session_id {
                    check_child(cid)?;
                }
                if let Some(dir) = cwd {
                    if dir.len() > 1024 || dir.contains('\0') {
                        return Err("orchestrate cwd is invalid".into());
                    }
                }
                if command.is_empty() || command.len() > MAX_SEND_BYTES {
                    return Err("orchestrate command is empty or exceeds 16 KiB".into());
                }
                if timeout_ms.is_some_and(|ms| ms == 0 || ms > MAX_WAIT_TIMEOUT_MS) {
                    return Err(format!(
                        "orchestrate timeout_ms must be in 1..={MAX_WAIT_TIMEOUT_MS}"
                    ));
                }
                if let Some(pat) = pattern {
                    check_pattern(pat, "pattern")?;
                }
                if let Some(t) = title {
                    if t.len() > MAX_CHILD_TITLE_BYTES || t.chars().any(char::is_control) {
                        return Err(
                            "orchestrate title exceeds 120 bytes or has control chars".into()
                        );
                    }
                }
                if max_lines.is_some_and(|n| n == 0 || n > MAX_READ_LINES) {
                    return Err(format!(
                        "orchestrate max_lines must be in 1..={MAX_READ_LINES}"
                    ));
                }
                Ok(())
            }
            Self::OpenFile {
                repo_path,
                caller_session_id,
                caller_token,
                external_caller_id,
                path,
                line,
                column,
            } => {
                check_common(
                    repo_path,
                    caller_session_id.as_deref(),
                    caller_token.as_deref(),
                    external_caller_id.as_deref(),
                )?;
                let candidate = Path::new(path);
                if path.trim().is_empty()
                    || path.len() > 4096
                    || path.contains('\0')
                    || candidate.is_absolute()
                    || candidate.components().any(|c| {
                        matches!(
                            c,
                            std::path::Component::ParentDir
                                | std::path::Component::RootDir
                                | std::path::Component::Prefix(_)
                        )
                    })
                {
                    return Err(
                        "orchestrate file path must be a non-empty repository-relative path without '..'"
                            .into(),
                    );
                }
                if line.is_some_and(|l| l == 0 || l > 1_000_000) {
                    return Err("orchestrate line must be in 1..=1000000".into());
                }
                if column.is_some_and(|c| c == 0 || c > 10_000) {
                    return Err("orchestrate column must be in 1..=10000".into());
                }
                Ok(())
            }
            Self::List {
                repo_path,
                caller_session_id,
                caller_token,
                external_caller_id,
                ..
            } => check_common(
                repo_path,
                caller_session_id.as_deref(),
                caller_token.as_deref(),
                external_caller_id.as_deref(),
            ),
            Self::Wait {
                repo_path,
                caller_session_id,
                caller_token,
                external_caller_id,
                child_session_id,
                timeout_ms,
                pattern,
                ..
            } => {
                check_common(
                    repo_path,
                    caller_session_id.as_deref(),
                    caller_token.as_deref(),
                    external_caller_id.as_deref(),
                )?;
                check_child(child_session_id)?;
                if timeout_ms.is_some_and(|ms| ms == 0 || ms > MAX_WAIT_TIMEOUT_MS) {
                    return Err(format!(
                        "orchestrate timeout_ms must be in 1..={MAX_WAIT_TIMEOUT_MS}"
                    ));
                }
                if let Some(pat) = pattern {
                    check_pattern(pat, "pattern")?;
                }
                Ok(())
            }
            Self::Read {
                repo_path,
                caller_session_id,
                caller_token,
                external_caller_id,
                child_session_id,
                max_lines,
                offset_lines,
                grep,
                ..
            } => {
                check_common(
                    repo_path,
                    caller_session_id.as_deref(),
                    caller_token.as_deref(),
                    external_caller_id.as_deref(),
                )?;
                check_child(child_session_id)?;
                if max_lines.is_some_and(|n| n == 0 || n > MAX_READ_LINES) {
                    return Err(format!(
                        "orchestrate max_lines must be in 1..={MAX_READ_LINES}"
                    ));
                }
                if offset_lines.is_some_and(|o| o > PARSER_SCROLLBACK_ROWS) {
                    return Err(format!(
                        "orchestrate offset_lines must be in 0..={PARSER_SCROLLBACK_ROWS}"
                    ));
                }
                if let Some(pat) = grep {
                    check_pattern(pat, "grep")?;
                }
                Ok(())
            }
            Self::Send {
                repo_path,
                caller_session_id,
                caller_token,
                external_caller_id,
                child_session_id,
                input,
                ..
            } => {
                check_common(
                    repo_path,
                    caller_session_id.as_deref(),
                    caller_token.as_deref(),
                    external_caller_id.as_deref(),
                )?;
                check_child(child_session_id)?;
                if input.is_empty() || input.len() > MAX_SEND_BYTES {
                    return Err("orchestrate input is empty or exceeds 16 KiB".into());
                }
                Ok(())
            }
            Self::Close {
                repo_path,
                caller_session_id,
                caller_token,
                external_caller_id,
                child_session_id,
            } => {
                check_common(
                    repo_path,
                    caller_session_id.as_deref(),
                    caller_token.as_deref(),
                    external_caller_id.as_deref(),
                )?;
                check_child(child_session_id)
            }
        }
    }
}

fn validate_child_input_bytes(input: &str) -> Result<(), String> {
    if input.is_empty() || input.len() > MAX_SEND_BYTES {
        return Err(format!(
            "child terminal input must be 1..={MAX_SEND_BYTES} bytes"
        ));
    }
    for byte in input.bytes() {
        match byte {
            0x00 => return Err("NUL bytes are not permitted in child terminal input".into()),
            0x1b => {
                return Err(
                    "ANSI/OSC escape bytes (0x1b) are not permitted in child terminal input".into(),
                )
            }
            0x1a => {
                return Err("Ctrl+Z suspend (0x1a) is not permitted in child terminal input".into())
            }
            _ => {}
        }
    }
    Ok(())
}

fn gate_lines(cwd: &str, text: &str) -> Result<(bool, Option<String>), String> {
    let mut notices: Vec<String> = Vec::new();
    let mut checked_all = true;
    let mut any_line = false;
    for raw_line in text.lines() {
        let trimmed = raw_line.trim();
        if trimmed.is_empty() {
            continue;
        }
        any_line = true;
        if let Some(notice) = crate::hooks::gate_child_command(cwd, trimmed)? {
            if notice.contains("UNGATED") {
                checked_all = false;
            }
            if !notices.contains(&notice) {
                notices.push(notice);
            }
        }
    }
    if !any_line {
        return Ok((true, None));
    }
    let combined = (!notices.is_empty()).then(|| notices.join("\n"));
    Ok((checked_all, combined))
}

fn canonical_trusted_repo(repo_path: &str) -> Result<std::path::PathBuf, String> {
    crate::repository_trust::require(Path::new(repo_path)).map_err(|e| e.into_message())?;
    let validated = validate_repo(repo_path).map_err(|e| e.into_message())?;
    Ok(std::fs::canonicalize(&validated).unwrap_or(validated))
}

#[derive(Debug, Clone)]
enum CallerIdentity {
    Session(String),
    External(String),
}

impl CallerIdentity {
    fn parent_key(&self) -> &str {
        match self {
            Self::Session(id) | Self::External(id) => id.as_str(),
        }
    }

    fn is_external(&self) -> bool {
        matches!(self, Self::External(_))
    }
}

fn authorize_caller(
    registry: &OrchestrationRegistry,
    caller_session_id: Option<&str>,
    caller_token: Option<&str>,
    external_caller_id: Option<&str>,
    canonical_repo: &str,
) -> Result<CallerIdentity, String> {
    if caller_session_id.is_some() != caller_token.is_some() {
        return Err(CREDENTIAL_REFUSAL.to_string());
    }
    if let (Some(sid), Some(tok)) = (caller_session_id, caller_token) {
        if !is_session_id(sid) {
            return Err(CREDENTIAL_REFUSAL.to_string());
        }
        let provided = hash_token(tok)?;
        let inner = registry.lock();
        let Some(record) = inner.sessions.get(sid) else {
            return Err(CREDENTIAL_REFUSAL.to_string());
        };
        if record.ended || record.repo_path != canonical_repo {
            return Err(CREDENTIAL_REFUSAL.to_string());
        }
        let Some(stored) = &record.token_hash else {
            return Err(CREDENTIAL_REFUSAL.to_string());
        };
        if !ct_eq_32(stored, &provided) {
            return Err(CREDENTIAL_REFUSAL.to_string());
        }
        return Ok(CallerIdentity::Session(sid.to_owned()));
    }

    let Some(ext) = external_caller_id else {
        return Err(CREDENTIAL_REFUSAL.to_string());
    };
    if !is_valid_external_caller_id(ext) {
        return Err(CREDENTIAL_REFUSAL.to_string());
    }
    Ok(CallerIdentity::External(ext.to_owned()))
}

fn authorize_child<'a>(
    inner: &'a RegistryInner,
    caller_key: &str,
    child_session_id: &str,
    canonical_repo: &str,
) -> Result<&'a OrchestratedRecord, String> {
    if !is_session_id(child_session_id) || child_session_id == caller_key {
        return Err(LINEAGE_REFUSAL.to_string());
    }
    let Some(record) = inner.sessions.get(child_session_id) else {
        return Err(LINEAGE_REFUSAL.to_string());
    };
    if record.parent_session_id.as_deref() != Some(caller_key)
        || record.repo_path != canonical_repo
    {
        return Err(LINEAGE_REFUSAL.to_string());
    }
    Ok(record)
}

fn authorize_readable_session<'a>(
    inner: &'a RegistryInner,
    caller_key: &str,
    target_session_id: &str,
    canonical_repo: &str,
    allow_repo_shells: bool,
) -> Result<(&'a OrchestratedRecord, bool), String> {
    if !is_session_id(target_session_id) || target_session_id == caller_key {
        return Err(LINEAGE_REFUSAL.to_string());
    }
    let Some(record) = inner.sessions.get(target_session_id) else {
        return Err(LINEAGE_REFUSAL.to_string());
    };
    if record.repo_path != canonical_repo {
        return Err(LINEAGE_REFUSAL.to_string());
    }
    if record.parent_session_id.as_deref() == Some(caller_key) {
        return Ok((record, true));
    }
    if allow_repo_shells && record.parent_session_id.is_none() && record.launcher == "shell" {
        return Ok((record, false));
    }
    Err(LINEAGE_REFUSAL.to_string())
}

fn build_agent_child_args(
    launcher: &str,
    prompt: Option<&str>,
) -> Result<Option<Vec<String>>, String> {
    let prompt_args = match prompt.map(str::trim).filter(|s| !s.is_empty()) {
        Some(text) => match launcher {
            "claude" => Some(vec!["--".to_string(), text.to_string()]),
            "agy" => Some(vec!["--prompt-interactive".to_string(), text.to_string()]),
            _ => Some(vec![text.to_string()]),
        },
        None => None,
    };
    let defaults = crate::tool_config::agent_defaults();
    let sources = defaults.claude_setting_sources_arg();
    let hook = crate::workbench::claude_hook::for_launch(launcher);
    let with_flags = crate::workbench::terminal_command::with_launch_flags(
        Some(launcher),
        &crate::workbench::terminal_command::LaunchOptions {
            notify: crate::tool_config::session_alerts().configure_agents,
            setting_sources: sources.as_deref(),
            model: defaults.model_for(launcher),
            hook: hook.binary(),
        },
        prompt_args,
    )?;
    // `manvi` takes no permission-mode flag; every other supported agent CLI
    // starts in `"ask"` mode with `acknowledged = false`, making `bypass`
    // structurally unreachable from orchestrated child spawns.
    let mode = if matches!(launcher, "claude" | "agy" | "codex" | "grok") {
        Some("ask")
    } else {
        None
    };
    crate::workbench::terminal_command::apply_permission_mode(
        Some(launcher),
        mode,
        false,
        with_flags,
    )
}

#[cfg(unix)]
fn refresh_detached_host_screen(state: &TerminalSessions, child_session_id: &str) {
    let host = &state.host;
    let (dims, seen_bytes) = {
        let inner = state.orchestration.lock();
        let Some(rec) = inner.sessions.get(child_session_id) else {
            return;
        };
        if rec.parser.is_none() {
            return;
        }
        ((rec.rows, rec.cols), rec.observed_bytes)
    };
    let mut remote_tel_synced = false;
    if let Ok(Some(remote_tel)) = host.telemetry(child_session_id) {
        remote_tel_synced = true;
        let inner = state.orchestration.lock();
        if let Some(rec) = inner.sessions.get(child_session_id) {
            let (lock, cvar) = &*rec.telemetry;
            let mut local_tel = lock
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            if remote_tel.event_seq >= local_tel.event_seq || remote_tel != *local_tel {
                let ended = local_tel.ended || remote_tel.ended;
                *local_tel = remote_tel;
                local_tel.ended = ended;
                cvar.notify_all();
            }
        }
    }
    if host.is_attached(child_session_id) && seen_bytes > 0 {
        return;
    }
    if let Ok(crate::terminal::TranscriptRead::Found(tail)) = host.transcript(
        child_session_id,
        crate::terminal::MAX_TRANSCRIPT_TAIL_BYTES,
        false,
        None,
    ) {
        let stream_pos = if tail.total_bytes > 0 {
            tail.total_bytes
        } else {
            tail.retained
        };
        if stream_pos <= seen_bytes && (seen_bytes > 0 || tail.bytes == 0) && tail.live {
            return;
        }
        if let Ok(raw) = base64::engine::general_purpose::STANDARD.decode(&tail.data_b64) {
            let mut inner = state.orchestration.lock();
            if let Some(rec) = inner.sessions.get_mut(child_session_id) {
                let current_seen = rec.observed_bytes;
                if !raw.is_empty() && stream_pos >= current_seen {
                    let delta = stream_pos - current_seen;
                    if current_seen > 0
                        && delta > 0
                        && delta <= raw.len() as u64
                        && rec.parser.is_some()
                    {
                        let suffix = &raw[raw.len() - delta as usize..];
                        let cols = rec.cols;
                        if let Some(parser) = rec.parser.as_mut() {
                            parser.process(suffix);
                        }
                        if let Some(blocks) = rec.blocks.as_mut() {
                            blocks.feed(suffix, cols);
                        }
                        if !remote_tel_synced {
                            if let Some(scanner) = rec.host_scanner.as_mut() {
                                apply_scanner_telemetry(scanner, &rec.telemetry, suffix);
                            }
                        }
                    } else if current_seen == 0 || delta > raw.len() as u64 {
                        let mut parser = vt100::Parser::new(
                            nz_u16(dims.0),
                            nz_u16(dims.1),
                            PARSER_SCROLLBACK_ROWS,
                        );
                        parser.process(&raw);
                        rec.parser = Some(parser);
                        let mut blocks = CommandBlockRecorder::default();
                        blocks.feed(&raw, dims.1);
                        rec.blocks = Some(blocks);
                        if !remote_tel_synced {
                            let mut scanner = crate::alerts::scan::Scanner::new();
                            apply_scanner_telemetry(&mut scanner, &rec.telemetry, &raw);
                            rec.host_scanner = Some(scanner);
                        }
                    }
                    rec.observed_bytes = stream_pos.max(raw.len() as u64);
                }
                if !tail.live {
                    rec.ended = true;
                }
            }
        }
    }
}

fn extract_parser_logical_lines(parser: &vt100::Parser) -> Vec<String> {
    let mut logical_lines: Vec<String> = Vec::new();
    let mut screen = parser.screen().clone();
    let (rows_nz, cols_nz) = screen.size();
    let viewport_rows = usize::from(rows_nz.get());
    let cols = cols_nz.get();
    let sb_len = screen.scrollback_len();
    let total_physical_rows = sb_len + viewport_rows;
    let mut next_global_row = 0usize;
    let mut pending = String::new();

    while next_global_row < total_physical_rows {
        let (offset, skip) = if next_global_row <= sb_len {
            (sb_len - next_global_row, 0usize)
        } else {
            (0usize, next_global_row - sb_len)
        };
        screen.set_scrollback(offset);
        let window_rows: Vec<(String, bool)> = screen
            .rows(0, cols)
            .enumerate()
            .map(|(r, text)| (text, screen.row_wrapped(r as u16)))
            .collect();
        let count = window_rows.len().saturating_sub(skip);
        if count == 0 {
            break;
        }
        for (text, wrapped) in window_rows.into_iter().skip(skip) {
            pending.push_str(&text);
            if !wrapped {
                logical_lines.push(std::mem::take(&mut pending));
            }
        }
        next_global_row += count;
    }
    if !pending.is_empty() {
        logical_lines.push(pending);
    }
    logical_lines
}

fn select_scrubbed_lines(
    scrubbed_lines: Vec<String>,
    max_lines: usize,
    offset_lines: usize,
    grep: Option<&str>,
) -> (String, usize, usize, bool) {
    let matched: Vec<String> = match grep {
        Some(pat) => scrubbed_lines
            .into_iter()
            .filter(|line| line.contains(pat))
            .collect(),
        None => scrubbed_lines,
    };
    let total_lines = matched.len();
    let cap = max_lines.clamp(1, MAX_READ_LINES);
    let offset = offset_lines.min(total_lines);
    let end = total_lines.saturating_sub(offset);
    let start = end.saturating_sub(cap);
    let mut truncated = start > 0;
    let mut text = matched[start..end].join("\n");
    if text.len() > MAX_READ_BYTES {
        truncated = true;
        let mut cut = text.len().saturating_sub(MAX_READ_BYTES);
        while cut < text.len() && !text.is_char_boundary(cut) {
            cut += 1;
        }
        text = text[cut..].to_string();
    }
    let lines_returned = if text.is_empty() {
        0
    } else {
        text.lines().count()
    };
    (text, lines_returned, total_lines, truncated)
}

fn render_redacted_screen_filtered(
    record: &OrchestratedRecord,
    max_lines: usize,
    offset_lines: usize,
    grep: Option<&str>,
) -> (String, usize, usize, bool) {
    let mut logical_lines = record
        .parser
        .as_ref()
        .map(extract_parser_logical_lines)
        .unwrap_or_default();

    while logical_lines
        .last()
        .is_some_and(|line| line.trim().is_empty())
    {
        logical_lines.pop();
    }
    // Redact before `grep` so secret substrings can never become a grep oracle.
    let scrubbed = scrub_screen_text(&logical_lines.join("\n"));
    let scrubbed_lines: Vec<String> = if scrubbed.is_empty() {
        Vec::new()
    } else {
        scrubbed.lines().map(str::to_owned).collect()
    };
    select_scrubbed_lines(scrubbed_lines, max_lines, offset_lines, grep)
}

fn render_redacted_screen(
    record: &OrchestratedRecord,
    max_lines: usize,
) -> (String, usize, usize, bool) {
    render_redacted_screen_filtered(record, max_lines, 0, None)
}

fn herdr_state_str(status: &Option<crate::alerts::scan::HerdrStatus>) -> Option<String> {
    status.as_ref().map(|h| h.state.as_str().to_owned())
}

fn encode_exec_wrapper(command: &str, exec_tag: &str, exit_after: bool) -> String {
    let body = if !command.contains('\n') && !command.contains('\r') {
        let escaped = command.replace('\'', "'\\''");
        format!("eval '{escaped}'")
    } else {
        let mut encoded = String::with_capacity(command.len() + 16);
        for ch in command.chars() {
            match ch {
                '\\' => encoded.push_str("\\\\"),
                '\'' => encoded.push_str("\\047"),
                '\n' => encoded.push_str("\\n"),
                '\r' => encoded.push_str("\\r"),
                _ => encoded.push(ch),
            }
        }
        format!("eval \"$(printf '%b' '{encoded}')\"")
    };
    if exit_after {
        format!(
            "printf '\\033]133;C\\007'; {body}; __gp_ec=$?; printf '\\033]133;D;%d;%s\\007' \"$__gp_ec\" \"{exec_tag}\"; exit \"$__gp_ec\"\n"
        )
    } else {
        format!(
            "printf '\\033]133;C\\007'; {body}; __gp_ec=$?; printf '\\033]133;D;%d;%s\\007' \"$__gp_ec\" \"{exec_tag}\"\n"
        )
    }
}

struct SpawnParams {
    launcher: Option<String>,
    cwd: Option<String>,
    command: Option<String>,
    prompt: Option<String>,
    title: Option<String>,
    split: Option<bool>,
    rows: Option<u16>,
    cols: Option<u16>,
}

struct SpawnResult {
    canonical_repo: String,
    child_session_id: String,
    launcher: String,
    cwd: String,
    split: bool,
    gate_checked: bool,
    gate_notice: Option<String>,
}

fn spawn_child_internal(
    state: &TerminalSessions,
    ui: &dyn OrchestrateUiSink,
    caller: &CallerIdentity,
    repo_buf: &Path,
    canonical_repo: &str,
    params: SpawnParams,
) -> Result<SpawnResult, String> {
    {
        let inner = state.orchestration.lock();
        let active_children = inner
            .sessions
            .values()
            .filter(|rec| {
                !rec.ended && rec.parent_session_id.as_deref() == Some(caller.parent_key())
            })
            .count();
        if active_children >= MAX_CHILDREN_PER_PARENT {
            return Err(format!(
                "parent terminal session already has {active_children} active child sessions (limit {MAX_CHILDREN_PER_PARENT}); wait for or close an existing child before spawning another"
            ));
        }
        if caller.is_external() {
            let active_external = inner
                .sessions
                .values()
                .filter(|rec| {
                    !rec.ended
                        && rec
                            .parent_session_id
                            .as_deref()
                            .is_some_and(is_valid_external_caller_id)
                })
                .count();
            if active_external >= MAX_EXTERNAL_ORCHESTRATED_CHILDREN {
                return Err(format!(
                    "external terminal orchestrators already have {active_external} active child sessions (limit {MAX_EXTERNAL_ORCHESTRATED_CHILDREN}); close an existing child before spawning another"
                ));
            }
        }
    }

    let launcher = params
        .launcher
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .unwrap_or("shell")
        .to_owned();
    if !ALLOWED_CHILD_LAUNCHERS.contains(&launcher.as_str()) {
        return Err(format!("unsupported child launcher '{launcher}'"));
    }

    let start_dir = params
        .cwd
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_owned);
    let resolved_cwd = match &start_dir {
        Some(sub) => sandbox_join_canonical(repo_buf, sub)?,
        None => repo_buf.to_path_buf(),
    };
    if !resolved_cwd.is_dir() {
        return Err("requested child cwd is not an existing directory inside repo_path".into());
    }
    let resolved_cwd_str = resolved_cwd.to_string_lossy().into_owned();

    let (program, args, initial_shell_cmd, gate_checked, gate_notice) = if launcher == "shell" {
        if params.prompt.is_some() {
            return Err("prompt applies only to agent launchers; pass command for shell".into());
        }
        if let Some(cmd) = params.command {
            validate_child_input_bytes(&cmd)?;
            let (checked, notice) = gate_lines(&resolved_cwd_str, &cmd)?;
            (None, None, Some(cmd), checked, notice)
        } else {
            (None, None, None, true, None)
        }
    } else {
        if params.command.is_some() {
            return Err(
                "command applies only to shell launcher; pass prompt for agent launchers".into(),
            );
        }
        let agent_args = build_agent_child_args(&launcher, params.prompt.as_deref())?;
        (Some(launcher.clone()), agent_args, None, true, None)
    };

    let clean_title = params
        .title
        .map(|t| t.trim().to_owned())
        .filter(|t| !t.is_empty());
    let want_split = params.split.unwrap_or(true);
    let pty_rows = params.rows.unwrap_or(28);
    let pty_cols = params.cols.unwrap_or(100);
    let events = ui.session_events(state);

    let spawned = spawn_at(
        state,
        Place::Repo {
            repo_path: canonical_repo.to_owned(),
            start_dir,
        },
        pty_rows,
        pty_cols,
        program,
        args,
        None,
        discard_sink(),
        events,
        None,
        Some(ChildSpawnMeta {
            parent_session_id: caller.parent_key().to_owned(),
            launcher: launcher.clone(),
            title: clean_title.clone(),
        }),
    )
    .map_err(|e| e.into_message())?;

    // Immediately detach the discard sink so an adopting frontend tab can
    // attach cleanly without hitting "already shown in a tab".
    let _ = detach_session(state, &spawned.id);

    if let Some(cmd) = initial_shell_cmd {
        let mut line = cmd;
        if !line.ends_with('\n') {
            line.push('\n');
        }
        write_input(state, &spawned.id, line.as_bytes())?;
    }

    ui.child_spawned(TerminalChildSpawnedPayload {
        id: spawned.id.clone(),
        parent_session_id: caller.parent_key().to_owned(),
        repo_path: canonical_repo.to_owned(),
        cwd: spawned.cwd.clone(),
        shell: spawned.shell.clone(),
        launcher: launcher.clone(),
        title: clean_title,
        split: want_split,
    });

    Ok(SpawnResult {
        canonical_repo: canonical_repo.to_owned(),
        child_session_id: spawned.id,
        launcher,
        cwd: spawned.cwd,
        split: want_split,
        gate_checked,
        gate_notice,
    })
}

struct WaitOutcome {
    matched: bool,
    timed_out: bool,
    matched_reason: &'static str,
    event_seq: u64,
    ended: bool,
    exit_code: Option<i32>,
    command_running: bool,
    last_exit_code: Option<i32>,
    herdr_status: Option<String>,
    dev_server_url: Option<String>,
}

#[allow(clippy::too_many_arguments)]
fn wait_child_internal(
    state: &TerminalSessions,
    caller_key: &str,
    child_session_id: &str,
    canonical_repo: &str,
    timeout_ms: Option<u64>,
    pattern: Option<&str>,
    wait_for_idle: Option<bool>,
    since_seq: Option<u64>,
    required_exec_tag: Option<&str>,
    allow_repo_shells: bool,
) -> Result<WaitOutcome, String> {
    let wait_idle = wait_for_idle.unwrap_or(true);
    let bounded_ms = timeout_ms
        .unwrap_or(DEFAULT_WAIT_TIMEOUT_MS)
        .clamp(1, MAX_WAIT_TIMEOUT_MS);
    let deadline = Instant::now() + Duration::from_millis(bounded_ms);

    let effective_since = {
        let inner = state.orchestration.lock();
        let (rec, _) = authorize_readable_session(
            &inner,
            caller_key,
            child_session_id,
            canonical_repo,
            allow_repo_shells,
        )?;
        since_seq.unwrap_or(rec.baseline_event_seq)
    };

    let mut last_port_probe: Option<Instant> = None;
    loop {
        #[cfg(unix)]
        refresh_detached_host_screen(state, child_session_id);

        if required_exec_tag.is_none()
            && wait_idle
            && last_port_probe.is_none_or(|t| t.elapsed() >= Duration::from_millis(250))
        {
            last_port_probe = Some(Instant::now());
            let needs_port = {
                let inner = state.orchestration.lock();
                inner.sessions.get(child_session_id).is_some_and(|rec| {
                    !rec.ended
                        && rec
                            .telemetry
                            .0
                            .lock()
                            .unwrap_or_else(std::sync::PoisonError::into_inner)
                            .dev_server_url
                            .is_none()
                })
            };
            if needs_port {
                if let Ok(ports) = crate::terminal::session_listening_ports(state, child_session_id)
                {
                    if let Some(&port) = ports.first() {
                        let inner = state.orchestration.lock();
                        if let Some(rec) = inner.sessions.get(child_session_id) {
                            let mut guard = rec
                                .telemetry
                                .0
                                .lock()
                                .unwrap_or_else(std::sync::PoisonError::into_inner);
                            if guard.dev_server_url.is_none() {
                                guard.dev_server_url = Some(format!("http://localhost:{port}"));
                                guard.event_seq = guard.event_seq.saturating_add(1);
                                guard.last_dev_event_seq = guard.event_seq;
                            }
                        }
                    }
                }
            }
        }

        let mut inner = state.orchestration.lock();
        let (rec, owned) = authorize_readable_session(
            &inner,
            caller_key,
            child_session_id,
            canonical_repo,
            allow_repo_shells,
        )?;
        let tel = rec
            .telemetry
            .0
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone();
        let ended = rec.ended || tel.ended;
        let exit_code = rec.exit_code.or(tel.last_exit_code);
        let herdr = herdr_state_str(&tel.herdr_status);

        let matched_reason = if let Some(tag) = required_exec_tag {
            if let Some(pat) = pattern {
                let (screen, _, _, _) = render_redacted_screen(rec, MAX_READ_LINES);
                if screen.contains(pat) {
                    Some("pattern")
                } else if rec.blocks.as_ref().is_some_and(|b| b.has_tag(tag)) {
                    Some("command_finished")
                } else if ended {
                    Some("ended")
                } else {
                    None
                }
            } else if rec.blocks.as_ref().is_some_and(|b| b.has_tag(tag)) {
                Some("command_finished")
            } else if ended {
                Some("ended")
            } else {
                None
            }
        } else if let Some(pat) = pattern {
            let (screen, _, _, _) = render_redacted_screen(rec, MAX_READ_LINES);
            if screen.contains(pat) {
                Some("pattern")
            } else if ended {
                Some("ended")
            } else {
                None
            }
        } else if ended {
            Some("ended")
        } else if tel.last_herdr_event_seq > effective_since
            && matches!(herdr.as_deref(), Some("done" | "idle" | "blocked" | "error"))
        {
            Some("herdr_status")
        } else if wait_idle
            && tel.last_command_event_seq > effective_since
            && !tel.command_running
        {
            Some("command_finished")
        } else if wait_idle
            && tel.last_dev_event_seq > effective_since
            && tel.dev_server_url.is_some()
        {
            Some("dev_server_ready")
        } else {
            None
        };

        if let Some(reason) = matched_reason {
            if owned {
                if let Some(rec_mut) = inner.sessions.get_mut(child_session_id) {
                    rec_mut.baseline_event_seq = rec_mut.baseline_event_seq.max(tel.event_seq);
                }
            }
            let matched = pattern.is_none_or(|_| reason == "pattern");
            return Ok(WaitOutcome {
                matched,
                timed_out: false,
                matched_reason: reason,
                event_seq: tel.event_seq,
                ended,
                exit_code,
                command_running: tel.command_running,
                last_exit_code: tel.last_exit_code,
                herdr_status: herdr,
                dev_server_url: tel.dev_server_url,
            });
        }

        let now = Instant::now();
        if now >= deadline {
            return Ok(WaitOutcome {
                matched: false,
                timed_out: true,
                matched_reason: "timeout",
                event_seq: tel.event_seq,
                ended,
                exit_code,
                command_running: tel.command_running,
                last_exit_code: tel.last_exit_code,
                herdr_status: herdr,
                dev_server_url: tel.dev_server_url,
            });
        }

        let wait_slice = (deadline - now).min(Duration::from_millis(50));
        let _ = state
            .orchestration
            .changed
            .wait_timeout(inner, wait_slice)
            .unwrap_or_else(std::sync::PoisonError::into_inner);
    }
}

fn close_child_internal(
    state: &TerminalSessions,
    ui: &dyn OrchestrateUiSink,
    caller_key: &str,
    child_session_id: &str,
    canonical_repo: &str,
) -> Result<bool, String> {
    let already_ended = {
        let inner = state.orchestration.lock();
        let rec = authorize_child(&inner, caller_key, child_session_id, canonical_repo)?;
        let tel_ended = rec
            .telemetry
            .0
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .ended;
        rec.ended || tel_ended
    };

    if !already_ended {
        crate::terminal::kill_session(state, child_session_id)?;
        state.orchestration.mark_ended(child_session_id, None);
    }

    ui.child_closed(TerminalChildClosedPayload {
        id: child_session_id.to_owned(),
        parent_session_id: caller_key.to_owned(),
        repo_path: canonical_repo.to_owned(),
    });

    Ok(already_ended)
}

/// Executes one [`OrchestrateCall`] against `state` under Option A lineage
/// rules (with optional external MCP/hook callers and opt-in user shell reads).
pub fn execute(
    state: &TerminalSessions,
    ui: &dyn OrchestrateUiSink,
    call: OrchestrateCall,
) -> Result<Value, String> {
    let allow_repo_shells = crate::tool_config::agent_defaults().allows_agent_shell_reads();
    execute_with_shell_reads(state, ui, call, allow_repo_shells)
}

pub(crate) fn execute_with_shell_reads(
    state: &TerminalSessions,
    ui: &dyn OrchestrateUiSink,
    call: OrchestrateCall,
    allow_repo_shells: bool,
) -> Result<Value, String> {
    call.validate()?;
    match call {
        OrchestrateCall::Spawn {
            repo_path,
            caller_session_id,
            caller_token,
            external_caller_id,
            launcher,
            cwd,
            command,
            prompt,
            title,
            split,
            rows,
            cols,
        } => {
            let repo_buf = canonical_trusted_repo(&repo_path)?;
            let canonical_repo = repo_buf.to_string_lossy().into_owned();
            let caller = authorize_caller(
                &state.orchestration,
                caller_session_id.as_deref(),
                caller_token.as_deref(),
                external_caller_id.as_deref(),
                &canonical_repo,
            )?;
            let res = spawn_child_internal(
                state,
                ui,
                &caller,
                &repo_buf,
                &canonical_repo,
                SpawnParams {
                    launcher,
                    cwd,
                    command,
                    prompt,
                    title,
                    split,
                    rows,
                    cols,
                },
            )?;
            Ok(json!({
                "ok": true,
                "repo_path": res.canonical_repo,
                "child_session_id": res.child_session_id,
                "launcher": res.launcher,
                "cwd": res.cwd,
                "split": res.split,
                "baseline_seq": 0,
                "gate_checked": res.gate_checked,
                "gate_notice": res.gate_notice,
            }))
        }

        OrchestrateCall::Exec {
            repo_path,
            caller_session_id,
            caller_token,
            external_caller_id,
            child_session_id,
            cwd,
            command,
            timeout_ms,
            pattern,
            keep_open,
            split,
            title,
            max_lines,
        } => {
            let repo_buf = canonical_trusted_repo(&repo_path)?;
            let canonical_repo = repo_buf.to_string_lossy().into_owned();
            let caller = authorize_caller(
                &state.orchestration,
                caller_session_id.as_deref(),
                caller_token.as_deref(),
                external_caller_id.as_deref(),
                &canonical_repo,
            )?;
            validate_child_input_bytes(&command)?;
            let keep = keep_open.unwrap_or(true);

            let (target_id, spawned_new, effective_cwd) = if let Some(cid) = child_session_id {
                #[cfg(unix)]
                refresh_detached_host_screen(state, &cid);
                let inner = state.orchestration.lock();
                let rec =
                    authorize_child(&inner, caller.parent_key(), &cid, &canonical_repo)?;
                if rec.ended {
                    return Err("child terminal session has already ended".into());
                }
                if rec.launcher != "shell" {
                    return Err(
                        "exec targets only 'shell' child sessions; use spawn/send for agent launchers"
                            .into(),
                    );
                }
                let tel = rec
                    .telemetry
                    .0
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner);
                let dir = tel.cwd.clone().unwrap_or_else(|| rec.cwd.clone());
                (cid, false, dir)
            } else {
                let res = spawn_child_internal(
                    state,
                    ui,
                    &caller,
                    &repo_buf,
                    &canonical_repo,
                    SpawnParams {
                        launcher: Some("shell".into()),
                        cwd,
                        command: None,
                        prompt: None,
                        title,
                        split: Some(split.unwrap_or(keep)),
                        rows: None,
                        cols: None,
                    },
                )?;
                (res.child_session_id, true, res.cwd)
            };

            let (gate_checked, gate_notice) = match gate_lines(&effective_cwd, &command) {
                Ok(gated) => gated,
                Err(err) => {
                    if spawned_new {
                        let _ = close_child_internal(
                            state,
                            ui,
                            caller.parent_key(),
                            &target_id,
                            &canonical_repo,
                        );
                    }
                    return Err(err);
                }
            };

            let baseline_seq = {
                let mut inner = state.orchestration.lock();
                let rec = inner
                    .sessions
                    .get_mut(&target_id)
                    .ok_or_else(|| LINEAGE_REFUSAL.to_string())?;
                let seq = rec
                    .telemetry
                    .0
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner)
                    .event_seq;
                rec.baseline_event_seq = seq;
                seq
            };

            let nonce = mint_session_token()?;
            let exec_tag = format!("gpexec_{}", &nonce[SESSION_TOKEN_PREFIX.len()..21]);
            let exit_after = !keep && spawned_new;
            let wire = encode_exec_wrapper(&command, &exec_tag, exit_after);
            write_input(state, &target_id, wire.as_bytes())?;

            let wait = wait_child_internal(
                state,
                caller.parent_key(),
                &target_id,
                &canonical_repo,
                timeout_ms,
                pattern.as_deref(),
                Some(true),
                Some(baseline_seq),
                Some(&exec_tag),
                false,
            )?;

            let eff_max = max_lines.unwrap_or(DEFAULT_READ_LINES);
            let (text, lines_returned, total_lines, truncated, cmd_exit_code, active_cwd, ended) = {
                let inner = state.orchestration.lock();
                let rec = authorize_child(&inner, caller.parent_key(), &target_id, &canonical_repo)?;
                let tel = rec
                    .telemetry
                    .0
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner)
                    .clone();
                let active_cwd = tel.cwd.unwrap_or_else(|| rec.cwd.clone());
                let blk = rec
                    .blocks
                    .as_ref()
                    .and_then(|b| b.find_by_tag(&exec_tag))
                    .cloned();
                let (text, lines_returned, total_lines, truncated, code) = if let Some(b) = blk {
                    let scrubbed = scrub_screen_text(&b.output);
                    let raw_lines: Vec<String> = if scrubbed.is_empty() {
                        Vec::new()
                    } else {
                        scrubbed.lines().map(str::to_owned).collect()
                    };
                    let (t, lr, tl, tr) = select_scrubbed_lines(raw_lines, eff_max, 0, None);
                    let ec = b.exit_code.or(wait.last_exit_code).or(wait.exit_code);
                    (t, lr, tl, tr, ec)
                } else {
                    let (t, lr, tl, tr) = render_redacted_screen(rec, eff_max);
                    (t, lr, tl, tr, wait.exit_code.or(wait.last_exit_code))
                };
                (
                    text,
                    lines_returned,
                    total_lines,
                    truncated,
                    code,
                    active_cwd,
                    rec.ended || tel.ended,
                )
            };

            let closed_now = if !keep && !wait.timed_out {
                let _ = close_child_internal(
                    state,
                    ui,
                    caller.parent_key(),
                    &target_id,
                    &canonical_repo,
                )?;
                true
            } else {
                false
            };

            Ok(json!({
                "ok": true,
                "repo_path": canonical_repo,
                "child_session_id": target_id,
                "spawned": spawned_new,
                "kept_open": keep || wait.timed_out,
                "timed_out": wait.timed_out,
                "matched": wait.matched,
                "matched_reason": wait.matched_reason,
                "ended": ended || closed_now,
                "exit_code": cmd_exit_code,
                "text": text,
                "lines_returned": lines_returned,
                "total_lines": total_lines,
                "truncated": truncated,
                "cwd": active_cwd,
                "dev_server_url": wait.dev_server_url,
                "gate_checked": gate_checked,
                "gate_notice": gate_notice,
            }))
        }

        OrchestrateCall::OpenFile {
            repo_path,
            caller_session_id,
            caller_token,
            external_caller_id,
            path,
            line,
            column,
        } => {
            let repo_buf = canonical_trusted_repo(&repo_path)?;
            let canonical_repo = repo_buf.to_string_lossy().into_owned();
            let caller = authorize_caller(
                &state.orchestration,
                caller_session_id.as_deref(),
                caller_token.as_deref(),
                external_caller_id.as_deref(),
                &canonical_repo,
            )?;

            let trimmed = path.trim().trim_start_matches("./");
            let joined = sandbox_join_canonical(&repo_buf, trimmed)?;
            if !joined.is_file() {
                return Err(format!(
                    "'{}' is not an existing file in the repository",
                    trimmed
                ));
            }
            let rel = joined
                .strip_prefix(&repo_buf)
                .map_err(|_| "resolved file escaped repository root".to_string())?
                .components()
                .map(|c| c.as_os_str().to_string_lossy())
                .collect::<Vec<_>>()
                .join("/");

            ui.open_file_requested(TerminalOpenFileRequestPayload {
                caller_session_id: caller.parent_key().to_owned(),
                repo_path: canonical_repo.clone(),
                path: rel.clone(),
                line,
                column,
            });

            Ok(json!({
                "ok": true,
                "repo_path": canonical_repo,
                "path": rel,
                "line": line,
                "column": column,
            }))
        }

        OrchestrateCall::List {
            repo_path,
            caller_session_id,
            caller_token,
            external_caller_id,
            include_repo_shells,
        } => {
            let repo_buf = canonical_trusted_repo(&repo_path)?;
            let canonical_repo = repo_buf.to_string_lossy().into_owned();
            let caller = authorize_caller(
                &state.orchestration,
                caller_session_id.as_deref(),
                caller_token.as_deref(),
                external_caller_id.as_deref(),
                &canonical_repo,
            )?;
            let want_shells = include_repo_shells.unwrap_or(false) && allow_repo_shells;

            let mut children: Vec<(bool, String, Value)> = {
                let inner = state.orchestration.lock();
                inner
                    .sessions
                    .iter()
                    .filter_map(|(cid, rec)| {
                        if rec.repo_path != canonical_repo {
                            return None;
                        }
                        let owned = rec.parent_session_id.as_deref() == Some(caller.parent_key());
                        let readable_user_shell = want_shells
                            && rec.parent_session_id.is_none()
                            && rec.launcher == "shell"
                            && cid != caller.parent_key();
                        if !owned && !readable_user_shell {
                            return None;
                        }
                        let tel = rec
                            .telemetry
                            .0
                            .lock()
                            .unwrap_or_else(std::sync::PoisonError::into_inner)
                            .clone();
                        let active_cwd = tel.cwd.unwrap_or_else(|| rec.cwd.clone());
                        let ended = rec.ended || tel.ended;
                        let exit_code = rec.exit_code.or(tel.last_exit_code);
                        Some((
                            owned,
                            cid.clone(),
                            json!({
                                "child_session_id": cid,
                                "launcher": rec.launcher,
                                "cwd": active_cwd,
                                "title": rec.title,
                                "owned": owned,
                                "ended": ended,
                                "exit_code": exit_code,
                                "command_running": tel.command_running,
                                "last_exit_code": tel.last_exit_code,
                                "herdr_status": herdr_state_str(&tel.herdr_status),
                                "dev_server_url": tel.dev_server_url,
                            }),
                        ))
                    })
                    .collect()
            };
            children.sort_by(|a, b| (!a.0).cmp(&!b.0).then_with(|| a.1.cmp(&b.1)));
            let list: Vec<Value> = children.into_iter().map(|(_, _, v)| v).collect();
            let count = list.len();
            Ok(json!({
                "ok": true,
                "repo_path": canonical_repo,
                "shell_reads_enabled": allow_repo_shells,
                "total": count,
                "children": list,
            }))
        }

        OrchestrateCall::Wait {
            repo_path,
            caller_session_id,
            caller_token,
            external_caller_id,
            child_session_id,
            timeout_ms,
            pattern,
            wait_for_idle,
            since_seq,
        } => {
            let repo_buf = canonical_trusted_repo(&repo_path)?;
            let canonical_repo = repo_buf.to_string_lossy().into_owned();
            let caller = authorize_caller(
                &state.orchestration,
                caller_session_id.as_deref(),
                caller_token.as_deref(),
                external_caller_id.as_deref(),
                &canonical_repo,
            )?;
            let outcome = wait_child_internal(
                state,
                caller.parent_key(),
                &child_session_id,
                &canonical_repo,
                timeout_ms,
                pattern.as_deref(),
                wait_for_idle,
                since_seq,
                None,
                allow_repo_shells,
            )?;
            Ok(json!({
                "ok": true,
                "child_session_id": child_session_id,
                "matched": outcome.matched,
                "timed_out": outcome.timed_out,
                "matched_reason": outcome.matched_reason,
                "event_seq": outcome.event_seq,
                "ended": outcome.ended,
                "exit_code": outcome.exit_code,
                "command_running": outcome.command_running,
                "last_exit_code": outcome.last_exit_code,
                "herdr_status": outcome.herdr_status,
                "dev_server_url": outcome.dev_server_url,
            }))
        }

        OrchestrateCall::Read {
            repo_path,
            caller_session_id,
            caller_token,
            external_caller_id,
            child_session_id,
            max_lines,
            offset_lines,
            grep,
            last_command_only,
        } => {
            let repo_buf = canonical_trusted_repo(&repo_path)?;
            let canonical_repo = repo_buf.to_string_lossy().into_owned();
            let caller = authorize_caller(
                &state.orchestration,
                caller_session_id.as_deref(),
                caller_token.as_deref(),
                external_caller_id.as_deref(),
                &canonical_repo,
            )?;

            #[cfg(unix)]
            refresh_detached_host_screen(state, &child_session_id);

            let inner = state.orchestration.lock();
            let (rec, owned) = authorize_readable_session(
                &inner,
                caller.parent_key(),
                &child_session_id,
                &canonical_repo,
                allow_repo_shells,
            )?;
            let tel = rec
                .telemetry
                .0
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .clone();
            let eff_max = max_lines.unwrap_or(DEFAULT_READ_LINES);
            let offset = offset_lines.unwrap_or(0);
            let latest_block = rec.blocks.as_ref().and_then(|b| b.last_completed());

            let (text, lines_returned, total_lines, truncated, last_command, last_command_found) =
                if last_command_only.unwrap_or(false) {
                    if let Some(blk) = latest_block {
                        let scrubbed = scrub_screen_text(&blk.output);
                        let lines: Vec<String> = if scrubbed.is_empty() {
                            Vec::new()
                        } else {
                            scrubbed.lines().map(str::to_owned).collect()
                        };
                        let (t, lr, tl, tr) =
                            select_scrubbed_lines(lines, eff_max, offset, grep.as_deref());
                        (t, lr, tl, tr, blk.command.clone(), true)
                    } else {
                        let (t, lr, tl, tr) = render_redacted_screen_filtered(
                            rec,
                            eff_max,
                            offset,
                            grep.as_deref(),
                        );
                        (t, lr, tl, tr, None, false)
                    }
                } else {
                    let (t, lr, tl, tr) =
                        render_redacted_screen_filtered(rec, eff_max, offset, grep.as_deref());
                    (
                        t,
                        lr,
                        tl,
                        tr,
                        latest_block.and_then(|b| b.command.clone()),
                        latest_block.is_some(),
                    )
                };
            let ended = rec.ended || tel.ended;
            let exit_code = rec.exit_code.or(tel.last_exit_code);
            Ok(json!({
                "ok": true,
                "child_session_id": child_session_id,
                "owned": owned,
                "text": text,
                "lines_returned": lines_returned,
                "total_lines": total_lines,
                "offset_lines": offset,
                "truncated": truncated,
                "last_command": last_command,
                "last_command_found": last_command_found,
                "ended": ended,
                "exit_code": exit_code,
                "command_running": tel.command_running,
                "last_exit_code": tel.last_exit_code,
                "herdr_status": herdr_state_str(&tel.herdr_status),
                "dev_server_url": tel.dev_server_url,
            }))
        }

        OrchestrateCall::Send {
            repo_path,
            caller_session_id,
            caller_token,
            external_caller_id,
            child_session_id,
            input,
            append_newline,
        } => {
            let repo_buf = canonical_trusted_repo(&repo_path)?;
            let canonical_repo = repo_buf.to_string_lossy().into_owned();
            let caller = authorize_caller(
                &state.orchestration,
                caller_session_id.as_deref(),
                caller_token.as_deref(),
                external_caller_id.as_deref(),
                &canonical_repo,
            )?;
            validate_child_input_bytes(&input)?;

            #[cfg(unix)]
            refresh_detached_host_screen(state, &child_session_id);

            let effective_cwd = {
                let inner = state.orchestration.lock();
                let rec = authorize_child(
                    &inner,
                    caller.parent_key(),
                    &child_session_id,
                    &canonical_repo,
                )?;
                if rec.ended {
                    return Err("child terminal session has already ended".into());
                }
                let tel = rec
                    .telemetry
                    .0
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner);
                tel.cwd.clone().unwrap_or_else(|| rec.cwd.clone())
            };

            let is_pure_interrupt = matches!(input.as_str(), "\x03" | "\x04");
            let (gate_checked, gate_notice) = if is_pure_interrupt {
                (true, None)
            } else {
                gate_lines(&effective_cwd, &input)?
            };

            let baseline_seq = {
                let mut inner = state.orchestration.lock();
                let rec = inner
                    .sessions
                    .get_mut(&child_session_id)
                    .ok_or_else(|| LINEAGE_REFUSAL.to_string())?;
                let seq = rec
                    .telemetry
                    .0
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner)
                    .event_seq;
                rec.baseline_event_seq = seq;
                seq
            };

            let mut wire = input;
            if append_newline.unwrap_or(true) && !is_pure_interrupt && !wire.ends_with('\n') {
                wire.push('\n');
            }
            let bytes_sent = wire.len();
            write_input(state, &child_session_id, wire.as_bytes())?;

            Ok(json!({
                "ok": true,
                "child_session_id": child_session_id,
                "bytes_sent": bytes_sent,
                "baseline_seq": baseline_seq,
                "gate_checked": gate_checked,
                "gate_notice": gate_notice,
            }))
        }

        OrchestrateCall::Close {
            repo_path,
            caller_session_id,
            caller_token,
            external_caller_id,
            child_session_id,
        } => {
            let repo_buf = canonical_trusted_repo(&repo_path)?;
            let canonical_repo = repo_buf.to_string_lossy().into_owned();
            let caller = authorize_caller(
                &state.orchestration,
                caller_session_id.as_deref(),
                caller_token.as_deref(),
                external_caller_id.as_deref(),
                &canonical_repo,
            )?;

            let already_ended = close_child_internal(
                state,
                ui,
                caller.parent_key(),
                &child_session_id,
                &canonical_repo,
            )?;

            Ok(json!({
                "ok": true,
                "child_session_id": child_session_id,
                "already_ended": already_ended,
            }))
        }
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use crate::procguard::LockedSpawn;
    use crate::terminal::{SessionEnded, TerminalLossPayload};
    use std::sync::Arc;

    #[derive(Default)]
    struct RecordingEvents;

    impl SessionEvents for RecordingEvents {
        fn loss(&self, _payload: TerminalLossPayload) {}
        fn exit(&self, _ended: SessionEnded) {}
        fn notice(&self, _notice: crate::alerts::Notice) {}
        fn scan_drops(&self, _dropped: u64) {}
    }

    #[derive(Default)]
    pub(crate) struct TestUiSink {
        pub(crate) spawned: Mutex<Vec<TerminalChildSpawnedPayload>>,
        pub(crate) closed: Mutex<Vec<TerminalChildClosedPayload>>,
        pub(crate) opened: Mutex<Vec<TerminalOpenFileRequestPayload>>,
    }

    impl OrchestrateUiSink for TestUiSink {
        fn child_spawned(&self, payload: TerminalChildSpawnedPayload) {
            self.spawned.lock().unwrap().push(payload);
        }

        fn child_closed(&self, payload: TerminalChildClosedPayload) {
            self.closed.lock().unwrap().push(payload);
        }

        fn open_file_requested(&self, payload: TerminalOpenFileRequestPayload) {
            self.opened.lock().unwrap().push(payload);
        }

        fn session_events(&self, _state: &TerminalSessions) -> Arc<dyn SessionEvents> {
            Arc::new(RecordingEvents)
        }
    }

    struct TrustedTestRepo {
        _dir: tempfile::TempDir,
        canonical: String,
    }

    impl Drop for TrustedTestRepo {
        fn drop(&mut self) {
            let _ = crate::repository_trust::revoke(&self.canonical);
        }
    }

    fn init_trusted_repo() -> (TrustedTestRepo, String) {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().to_string_lossy().into_owned();
        let mut command = std::process::Command::new("git");
        crate::test_support::pin_git(&mut command);
        let out = command
            .args(["init", "-q", &path])
            .output_locked()
            .expect("git init");
        assert!(out.status.success());
        std::fs::create_dir_all(dir.path().join("src")).expect("mkdir src");
        std::fs::write(dir.path().join("src/main.rs"), "fn main() {}\n").expect("write main.rs");
        crate::test_support::trust_repo(dir.path());
        let canonical = std::fs::canonicalize(dir.path())
            .unwrap()
            .to_string_lossy()
            .into_owned();
        (
            TrustedTestRepo {
                _dir: dir,
                canonical: canonical.clone(),
            },
            canonical,
        )
    }

    #[test]
    fn session_tokens_are_unique_prefixed_hex_and_scrubbed_from_reads() {
        let a = mint_session_token().expect("token a");
        let b = mint_session_token().expect("token b");
        assert_ne!(a, b);
        assert!(is_well_formed_token(&a));
        assert!(is_well_formed_token(&b));
        assert!(ct_eq_32(&hash_token(&a).unwrap(), &hash_token(&a).unwrap()));
        assert!(!ct_eq_32(
            &hash_token(&a).unwrap(),
            &hash_token(&b).unwrap()
        ));

        let sample = format!(
            "TOKEN={a}\nbare_token={b}\nplain {a} inline\nANTHROPIC_API_KEY=sk-ant-api03-abcdefghijklmnopqrstuvwxyz0123456789\nAuthorization: Bearer eyJhbGciOiJIUzI1NiIsInR5cCI6IkpXVCJ9.secret"
        );
        let scrubbed = scrub_screen_text(&sample);
        assert!(!scrubbed.contains(&a), "leaked session token a: {scrubbed}");
        assert!(!scrubbed.contains(&b), "leaked session token b: {scrubbed}");
        assert!(
            !scrubbed.contains("sk-ant-api03"),
            "leaked anthropic key: {scrubbed}"
        );
        assert!(
            !scrubbed.contains("eyJhbGciOiJIUzI1NiIsInR5cCI6IkpXVCJ9"),
            "leaked bearer token: {scrubbed}"
        );
        assert!(scrubbed.contains("<redacted>"), "scrubbed: {scrubbed}");
    }

    #[test]
    fn option_a_lineage_blocks_user_shells_self_siblings_and_bad_tokens() {
        let (_dir, repo) = init_trusted_repo();
        let state = TerminalSessions::default();
        let ui = TestUiSink::default();

        // Spawn a genuine user-opened shell (`orchestrated = None`).
        let user_shell = spawn_at(
            &state,
            Place::Repo {
                repo_path: repo.clone(),
                start_dir: None,
            },
            24,
            80,
            None,
            None,
            None,
            discard_sink(),
            Arc::new(RecordingEvents),
            None,
            None,
        )
        .expect("user shell");

        let parent_a = "term-90001-1";
        let token_a = state
            .orchestration
            .register_test_parent(parent_a, Path::new(&repo));
        let parent_b = "term-90001-2";
        let token_b = state
            .orchestration
            .register_test_parent(parent_b, Path::new(&repo));

        // Wrong token is refused uniformly.
        let bad_tok = mint_session_token().unwrap();
        let err = execute(
            &state,
            &ui,
            OrchestrateCall::List {
                repo_path: repo.clone(),
                caller_session_id: Some(parent_a.into()),
                caller_token: Some(bad_tok),
                external_caller_id: None,
                include_repo_shells: None,
            },
        )
        .unwrap_err();
        assert_eq!(err, CREDENTIAL_REFUSAL);

        // Parent A spawns a child.
        let spawned = execute(
            &state,
            &ui,
            OrchestrateCall::Spawn {
                repo_path: repo.clone(),
                caller_session_id: Some(parent_a.into()),
                caller_token: Some(token_a.clone()),
                external_caller_id: None,
                launcher: Some("shell".into()),
                cwd: None,
                command: None,
                prompt: None,
                title: Some("worker-a".into()),
                split: Some(true),
                rows: Some(24),
                cols: Some(80),
            },
        )
        .expect("parent_a spawns child");
        let child_a = spawned["child_session_id"].as_str().unwrap().to_string();

        // `List` for parent A sees ONLY child_a, never user_shell.
        let list_a = execute(
            &state,
            &ui,
            OrchestrateCall::List {
                repo_path: repo.clone(),
                caller_session_id: Some(parent_a.into()),
                caller_token: Some(token_a.clone()),
                external_caller_id: None,
                include_repo_shells: None,
            },
        )
        .expect("list_a");
        assert_eq!(list_a["total"], 1);
        assert_eq!(list_a["children"][0]["child_session_id"], child_a);

        // Parent A CANNOT read, wait, or send to the user's shell, nor to itself.
        for forbidden in [user_shell.id.as_str(), parent_a, "term-90001-dead"] {
            let read_err = execute(
                &state,
                &ui,
                OrchestrateCall::Read {
                    repo_path: repo.clone(),
                    caller_session_id: Some(parent_a.into()),
                    caller_token: Some(token_a.clone()),
                    external_caller_id: None,
                    child_session_id: forbidden.into(),
                    max_lines: None,
                    offset_lines: None,
                    grep: None,
                    last_command_only: None,
                },
            )
            .unwrap_err();
            assert_eq!(read_err, LINEAGE_REFUSAL, "target {forbidden}");

            let send_err = execute(
                &state,
                &ui,
                OrchestrateCall::Send {
                    repo_path: repo.clone(),
                    caller_session_id: Some(parent_a.into()),
                    caller_token: Some(token_a.clone()),
                    external_caller_id: None,
                    child_session_id: forbidden.into(),
                    input: "echo nope".into(),
                    append_newline: Some(true),
                },
            )
            .unwrap_err();
            assert_eq!(send_err, LINEAGE_REFUSAL, "target {forbidden}");
        }

        // Parent B CANNOT read or send to Parent A's child (identical refusal, no oracle).
        let cross_err = execute(
            &state,
            &ui,
            OrchestrateCall::Read {
                repo_path: repo.clone(),
                caller_session_id: Some(parent_b.into()),
                caller_token: Some(token_b),
                external_caller_id: None,
                child_session_id: child_a.clone(),
                max_lines: None,
                offset_lines: None,
                grep: None,
                last_command_only: None,
            },
        )
        .unwrap_err();
        assert_eq!(cross_err, LINEAGE_REFUSAL);

        // Revoking Parent A on exit immediately invalidates its token.
        state.orchestration.mark_ended(parent_a, Some(0));
        let revoked_err = execute(
            &state,
            &ui,
            OrchestrateCall::List {
                repo_path: repo,
                caller_session_id: Some(parent_a.into()),
                caller_token: Some(token_a),
                external_caller_id: None,
                include_repo_shells: None,
            },
        )
        .unwrap_err();
        assert_eq!(revoked_err, CREDENTIAL_REFUSAL);

        let _ = crate::terminal::kill_session(&state, &user_shell.id);
        let _ = crate::terminal::kill_session(&state, &child_a);
    }

    #[test]
    fn command_gate_and_escape_filter_guard_child_spawn_and_send() {
        let (_dir, repo) = init_trusted_repo();
        let state = TerminalSessions::default();
        let ui = TestUiSink::default();
        let parent = "term-90002-1";
        let token = state
            .orchestration
            .register_test_parent(parent, Path::new(&repo));

        let unsafe_signal = "pkill -t ttys000 sleep".to_string();
        let refused_spawn = execute(
            &state,
            &ui,
            OrchestrateCall::Spawn {
                repo_path: repo.clone(),
                caller_session_id: Some(parent.into()),
                caller_token: Some(token.clone()),
                external_caller_id: None,
                launcher: Some("shell".into()),
                cwd: None,
                command: Some(unsafe_signal.clone()),
                prompt: None,
                title: None,
                split: Some(true),
                rows: Some(24),
                cols: Some(80),
            },
        );
        assert!(
            refused_spawn.is_err(),
            "signal_guard must refuse terminal-wide pkill in Spawn: {refused_spawn:?}"
        );

        let child = execute(
            &state,
            &ui,
            OrchestrateCall::Spawn {
                repo_path: repo.clone(),
                caller_session_id: Some(parent.into()),
                caller_token: Some(token.clone()),
                external_caller_id: None,
                launcher: Some("shell".into()),
                cwd: None,
                command: None,
                prompt: None,
                title: None,
                split: Some(false),
                rows: Some(24),
                cols: Some(80),
            },
        )
        .expect("clean child spawn")["child_session_id"]
            .as_str()
            .unwrap()
            .to_string();

        // Signal attack via Send is also refused before touching the PTY.
        let refused_send = execute(
            &state,
            &ui,
            OrchestrateCall::Send {
                repo_path: repo.clone(),
                caller_session_id: Some(parent.into()),
                caller_token: Some(token.clone()),
                external_caller_id: None,
                child_session_id: child.clone(),
                input: unsafe_signal,
                append_newline: Some(true),
            },
        );
        assert!(refused_send.is_err(), "{refused_send:?}");

        // Escape sequence and Ctrl+Z suspend injection are refused.
        for raw in ["echo hi\x1b]7501;done\x07", "sleep 5\x1a"] {
            let err = execute(
                &state,
                &ui,
                OrchestrateCall::Send {
                    repo_path: repo.clone(),
                    caller_session_id: Some(parent.into()),
                    caller_token: Some(token.clone()),
                    external_caller_id: None,
                    child_session_id: child.clone(),
                    input: raw.into(),
                    append_newline: Some(true),
                },
            );
            assert!(err.is_err(), "expected refusal for {raw:?}: {err:?}");
        }

        // OpenFile enforces repository sandbox boundaries.
        let open_ok = execute(
            &state,
            &ui,
            OrchestrateCall::OpenFile {
                repo_path: repo.clone(),
                caller_session_id: Some(parent.into()),
                caller_token: Some(token.clone()),
                external_caller_id: None,
                path: "src/main.rs".into(),
                line: Some(1),
                column: Some(4),
            },
        )
        .expect("open existing repo file");
        assert_eq!(open_ok["path"], "src/main.rs");
        assert_eq!(ui.opened.lock().unwrap().len(), 1);

        let traversal = execute(
            &state,
            &ui,
            OrchestrateCall::OpenFile {
                repo_path: repo,
                caller_session_id: Some(parent.into()),
                caller_token: Some(token),
                external_caller_id: None,
                path: "../outside.txt".into(),
                line: None,
                column: None,
            },
        );
        assert!(traversal.is_err(), "{traversal:?}");

        let _ = crate::terminal::kill_session(&state, &child);
    }

    #[cfg(unix)]
    #[test]
    fn real_pty_spawn_send_wait_and_vt100_read_renders_grid_and_redacts_secrets() {
        let (_dir, repo) = init_trusted_repo();
        let state = TerminalSessions::default();
        let ui = TestUiSink::default();
        let parent = "term-90003-1";
        let token = state
            .orchestration
            .register_test_parent(parent, Path::new(&repo));

        let spawned = execute(
            &state,
            &ui,
            OrchestrateCall::Spawn {
                repo_path: repo.clone(),
                caller_session_id: Some(parent.into()),
                caller_token: Some(token.clone()),
                external_caller_id: None,
                launcher: Some("shell".into()),
                cwd: None,
                command: None,
                prompt: None,
                title: Some("pty-e2e".into()),
                split: Some(true),
                rows: Some(24),
                cols: Some(100),
            },
        )
        .expect("spawn child");
        let child = spawned["child_session_id"].as_str().unwrap().to_string();
        assert_eq!(spawned["baseline_seq"], 0);
        assert_eq!(ui.spawned.lock().unwrap().len(), 1);

        // Print an ANSI carriage-return overwrite + a real secret + the child's
        // own GITPULSE_SESSION_TOKEN, then a completion marker.
        execute(
            &state,
            &ui,
            OrchestrateCall::Send {
                repo_path: repo.clone(),
                caller_session_id: Some(parent.into()),
                caller_token: Some(token.clone()),
                external_caller_id: None,
                child_session_id: child.clone(),
                input: "printf 'DRAFT\\rFINAL_GRID_OK\\nKEY=sk-ant-api03-1234567890abcdefghijklmnopqrstuvwxyz\\nTOK=%s\\nREADY_MARKER_42\\n' \"$GITPULSE_SESSION_TOKEN\"".into(),
                append_newline: Some(true),
            },
        )
        .expect("send printf");

        let waited = execute(
            &state,
            &ui,
            OrchestrateCall::Wait {
                repo_path: repo.clone(),
                caller_session_id: Some(parent.into()),
                caller_token: Some(token.clone()),
                external_caller_id: None,
                child_session_id: child.clone(),
                timeout_ms: Some(8_000),
                pattern: Some("READY_MARKER_42".into()),
                wait_for_idle: Some(false),
                since_seq: None,
            },
        )
        .expect("wait pattern");
        assert_eq!(waited["matched"], true, "{waited}");
        assert_eq!(waited["matched_reason"], "pattern");

        let read = execute(
            &state,
            &ui,
            OrchestrateCall::Read {
                repo_path: repo,
                caller_session_id: Some(parent.into()),
                caller_token: Some(token),
                external_caller_id: None,
                child_session_id: child.clone(),
                max_lines: Some(40),
                offset_lines: None,
                grep: None,
                last_command_only: None,
            },
        )
        .expect("read child screen");
        let text = read["text"].as_str().unwrap();
        // `vt100` resolved `DRAFT\rFINAL_GRID_OK` into `FINAL_GRID_OK` on the grid!
        assert!(text.contains("FINAL_GRID_OK"), "screen was: {text}");
        assert!(!text.contains("DRAFT\r"), "raw CR leaked: {text}");
        assert!(
            !text.contains("sk-ant-api03"),
            "Anthropic key leaked in screen read: {text}"
        );
        assert!(
            !text.contains(SESSION_TOKEN_PREFIX),
            "GITPULSE_SESSION_TOKEN leaked in screen read: {text}"
        );

        let _ = crate::terminal::kill_session(&state, &child);
    }

    #[test]
    fn vt100_scrollback_read_returns_up_to_200_lines_beyond_28_row_viewport() {
        let state = TerminalSessions::default();
        let dir = tempfile::tempdir().unwrap();
        let repo = dir.path().canonicalize().unwrap();
        let parent = "term-100-a";
        let _parent_tok = state.orchestration.register_test_parent(parent, &repo);
        let child = "term-100-b";
        let child_tok = mint_session_token().unwrap();
        state
            .orchestration
            .register_session(
                child,
                &child_tok,
                &repo,
                &repo,
                "shell",
                28,
                120,
                crate::terminal::new_shared_telemetry(),
                Some(ChildSpawnMeta {
                    parent_session_id: parent.into(),
                    launcher: "shell".into(),
                    title: None,
                }),
            )
            .unwrap();

        let mut payload = String::new();
        for idx in 1..=120 {
            payload.push_str(&format!("SCROLL_ROW_{idx:03}\r\n"));
        }
        state
            .orchestration
            .observe_output(child, payload.as_bytes());

        let inner = state.orchestration.lock();
        let rec = inner.sessions.get(child).unwrap();
        let (text, lines_returned, total_lines, truncated) = render_redacted_screen(rec, 120);
        assert_eq!(
            total_lines, 120,
            "expected 120 lines across scrollback + viewport, got {total_lines}"
        );
        assert_eq!(lines_returned, 120);
        assert!(!truncated);
        assert!(
            text.contains("SCROLL_ROW_001"),
            "missing earliest scrollback row: {text}"
        );
        assert!(
            text.contains("SCROLL_ROW_120"),
            "missing newest viewport row: {text}"
        );
        assert_eq!(rec.parser.as_ref().unwrap().screen().scrollback(), 0);
    }

    #[cfg(unix)]
    #[test]
    fn sticky_herdr_status_does_not_wake_subsequent_send_and_wait_in_zero_ms() {
        let (_dir, repo) = init_trusted_repo();
        let state = TerminalSessions::default();
        let ui = TestUiSink::default();
        let parent = "term-90004-1";
        let token = state
            .orchestration
            .register_test_parent(parent, Path::new(&repo));

        let spawned = execute(
            &state,
            &ui,
            OrchestrateCall::Spawn {
                repo_path: repo.clone(),
                caller_session_id: Some(parent.into()),
                caller_token: Some(token.clone()),
                external_caller_id: None,
                launcher: Some("shell".into()),
                cwd: None,
                command: None,
                prompt: None,
                title: Some("sticky-test".into()),
                split: Some(false),
                rows: Some(24),
                cols: Some(80),
            },
        )
        .expect("spawn child");
        let child = spawned["child_session_id"].as_str().unwrap().to_string();

        // Turn 1: emit OSC 7501 idle + loopback URL (split across %s so TTY input
        // echo does not contain a literal http://localhost URL before printf runs).
        execute(
            &state,
            &ui,
            OrchestrateCall::Send {
                repo_path: repo.clone(),
                caller_session_id: Some(parent.into()),
                caller_token: Some(token.clone()),
                external_caller_id: None,
                child_session_id: child.clone(),
                input: r#"printf '\033]7501;status;idle;Ready\007http://%s:4321/\n' localhost"#
                    .into(),
                append_newline: Some(true),
            },
        )
        .expect("send turn 1");

        let first_wait = execute(
            &state,
            &ui,
            OrchestrateCall::Wait {
                repo_path: repo.clone(),
                caller_session_id: Some(parent.into()),
                caller_token: Some(token.clone()),
                external_caller_id: None,
                child_session_id: child.clone(),
                timeout_ms: Some(5_000),
                pattern: None,
                wait_for_idle: Some(true),
                since_seq: None,
            },
        )
        .expect("first wait");
        assert_eq!(first_wait["matched_reason"], "herdr_status", "{first_wait}");

        // Turn 2: send a 500ms command; a short 80ms Wait MUST time out rather
        // than immediately returning the stale Turn-1 herdr_status/dev_server_url!
        execute(
            &state,
            &ui,
            OrchestrateCall::Send {
                repo_path: repo.clone(),
                caller_session_id: Some(parent.into()),
                caller_token: Some(token.clone()),
                external_caller_id: None,
                child_session_id: child.clone(),
                input: r#"sleep 0.5; printf '\033]7501;status;done;Finished\007'"#.into(),
                append_newline: Some(true),
            },
        )
        .expect("send turn 2");

        let early_wait = execute(
            &state,
            &ui,
            OrchestrateCall::Wait {
                repo_path: repo.clone(),
                caller_session_id: Some(parent.into()),
                caller_token: Some(token.clone()),
                external_caller_id: None,
                child_session_id: child.clone(),
                timeout_ms: Some(80),
                pattern: None,
                wait_for_idle: Some(true),
                since_seq: None,
            },
        )
        .expect("early wait");
        assert_eq!(
            early_wait["timed_out"], true,
            "stale Turn-1 herdr_status woke Turn-2 Wait prematurely: {early_wait}"
        );

        let _ = crate::terminal::kill_session(&state, &child);
    }

    #[test]
    fn adopt_hosted_sessions_restores_lineage_and_caller_token_without_raw_secret() {
        let (_dir, repo) = init_trusted_repo();
        let parent_id = "term-90005-1";
        let child_id = "term-90005-2";
        let parent_token = mint_session_token().unwrap();
        let child_token = mint_session_token().unwrap();
        let parent_hash_hex = token_hash_hex(&parent_token).unwrap();
        let child_hash_hex = token_hash_hex(&child_token).unwrap();

        let state = TerminalSessions::default();
        state.orchestration.adopt_hosted_sessions(&[
            crate::terminal::HostedSessionMeta {
                session_id: parent_id.into(),
                repo: repo.clone(),
                cwd: repo.clone(),
                rows: 28,
                cols: 100,
                launcher: "claude".into(),
                token_hash_hex: Some(parent_hash_hex),
                parent_session_id: None,
                child_launcher: None,
                child_title: None,
                agent_meta: None,
                telemetry: crate::terminal::SessionTelemetry::default(),
            },
            crate::terminal::HostedSessionMeta {
                session_id: child_id.into(),
                repo: repo.clone(),
                cwd: repo.clone(),
                rows: 28,
                cols: 100,
                launcher: "shell".into(),
                token_hash_hex: Some(child_hash_hex),
                parent_session_id: Some(parent_id.into()),
                child_launcher: Some("shell".into()),
                child_title: Some("adopted-child".into()),
                agent_meta: None,
                telemetry: crate::terminal::SessionTelemetry::default(),
            },
        ]);

        let ui = TestUiSink::default();
        let listed = execute(
            &state,
            &ui,
            OrchestrateCall::List {
                repo_path: repo,
                caller_session_id: Some(parent_id.into()),
                caller_token: Some(parent_token),
                external_caller_id: None,
                include_repo_shells: None,
            },
        )
        .expect("list adopted children");
        assert_eq!(listed["total"], 1);
        assert_eq!(listed["children"][0]["child_session_id"], child_id);
        assert_eq!(listed["children"][0]["title"], "adopted-child");
    }

    #[cfg(unix)]
    #[test]
    fn close_terminates_owned_child_frees_parent_slot_is_idempotent_and_refuses_foreign_sessions() {
        let (_dir, repo) = init_trusted_repo();
        let state = TerminalSessions::default();
        let ui = TestUiSink::default();

        let user_shell = spawn_at(
            &state,
            Place::Repo {
                repo_path: repo.clone(),
                start_dir: None,
            },
            24,
            80,
            None,
            None,
            None,
            discard_sink(),
            Arc::new(RecordingEvents),
            None,
            None,
        )
        .expect("user shell");

        let parent_a = "term-90006-1";
        let token_a = state
            .orchestration
            .register_test_parent(parent_a, Path::new(&repo));
        let parent_b = "term-90006-2";
        let token_b = state
            .orchestration
            .register_test_parent(parent_b, Path::new(&repo));

        let mut children_a = Vec::new();
        for idx in 0..MAX_CHILDREN_PER_PARENT {
            let spawned = execute(
                &state,
                &ui,
                OrchestrateCall::Spawn {
                    repo_path: repo.clone(),
                    caller_session_id: Some(parent_a.into()),
                    caller_token: Some(token_a.clone()),
                    external_caller_id: None,
                    launcher: Some("shell".into()),
                    cwd: None,
                    command: None,
                    prompt: None,
                    title: Some(format!("worker-{idx}")),
                    split: Some(false),
                    rows: Some(24),
                    cols: Some(80),
                },
            )
            .expect("spawn child within cap");
            children_a.push(spawned["child_session_id"].as_str().unwrap().to_string());
        }

        // At MAX_CHILDREN_PER_PARENT = 8, spawning a 9th child is refused.
        let ninth_err = execute(
            &state,
            &ui,
            OrchestrateCall::Spawn {
                repo_path: repo.clone(),
                caller_session_id: Some(parent_a.into()),
                caller_token: Some(token_a.clone()),
                external_caller_id: None,
                launcher: Some("shell".into()),
                cwd: None,
                command: None,
                prompt: None,
                title: Some("overflow".into()),
                split: Some(false),
                rows: Some(24),
                cols: Some(80),
            },
        )
        .unwrap_err();
        assert!(
            ninth_err.contains("already has 8 active child sessions (limit 8)"),
            "unexpected error: {ninth_err}"
        );

        // Option A: Parent B cannot close Parent A's child, and Parent A cannot close user_shell or itself.
        for (caller_id, caller_tok, target) in [
            (parent_b, &token_b, children_a[0].as_str()),
            (parent_a, &token_a, user_shell.id.as_str()),
            (parent_a, &token_a, parent_a),
            (parent_a, &token_a, "term-90006-missing"),
        ] {
            let err = execute(
                &state,
                &ui,
                OrchestrateCall::Close {
                    repo_path: repo.clone(),
                    caller_session_id: Some(caller_id.into()),
                    caller_token: Some(caller_tok.clone()),
                    external_caller_id: None,
                    child_session_id: target.into(),
                },
            )
            .unwrap_err();
            assert_eq!(err, LINEAGE_REFUSAL, "target {target}");
        }

        // Closing one owned child terminates it, emits child_closed, and frees the slot immediately.
        let closed_first = execute(
            &state,
            &ui,
            OrchestrateCall::Close {
                repo_path: repo.clone(),
                caller_session_id: Some(parent_a.into()),
                caller_token: Some(token_a.clone()),
                external_caller_id: None,
                child_session_id: children_a[0].clone(),
            },
        )
        .expect("close owned child");
        assert_eq!(closed_first["ok"], true);
        assert_eq!(closed_first["already_ended"], false);
        assert_eq!(ui.closed.lock().unwrap().len(), 1);
        assert_eq!(ui.closed.lock().unwrap()[0].id, children_a[0]);

        // Re-closing an already-ended child is idempotent.
        let closed_again = execute(
            &state,
            &ui,
            OrchestrateCall::Close {
                repo_path: repo.clone(),
                caller_session_id: Some(parent_a.into()),
                caller_token: Some(token_a.clone()),
                external_caller_id: None,
                child_session_id: children_a[0].clone(),
            },
        )
        .expect("idempotent close");
        assert_eq!(closed_again["already_ended"], true);

        // Parent A can immediately spawn another child in the freed slot.
        let replacement = execute(
            &state,
            &ui,
            OrchestrateCall::Spawn {
                repo_path: repo,
                caller_session_id: Some(parent_a.into()),
                caller_token: Some(token_a),
                external_caller_id: None,
                launcher: Some("shell".into()),
                cwd: None,
                command: None,
                prompt: None,
                title: Some("replacement".into()),
                split: Some(false),
                rows: Some(24),
                cols: Some(80),
            },
        )
        .expect("spawn replacement in freed slot");
        let replacement_id = replacement["child_session_id"]
            .as_str()
            .unwrap()
            .to_string();

        let _ = crate::terminal::kill_session(&state, &user_shell.id);
        let _ = crate::terminal::kill_session(&state, &replacement_id);
        for cid in children_a.iter().skip(1) {
            let _ = crate::terminal::kill_session(&state, cid);
        }
    }

    #[test]
    fn read_supports_offset_lines_grep_and_last_command_only_without_secret_oracle() {
        let (_dir, repo) = init_trusted_repo();
        let state = TerminalSessions::default();
        let ui = TestUiSink::default();
        let parent = "term-90007-1";
        let token = state
            .orchestration
            .register_test_parent(parent, Path::new(&repo));
        let child = "term-90007-2";
        let child_tok = mint_session_token().unwrap();
        state
            .orchestration
            .register_session(
                child,
                &child_tok,
                Path::new(&repo),
                Path::new(&repo),
                "shell",
                24,
                120,
                crate::terminal::new_shared_telemetry(),
                Some(ChildSpawnMeta {
                    parent_session_id: parent.into(),
                    launcher: "shell".into(),
                    title: None,
                }),
            )
            .unwrap();

        let stream = concat!(
            "\x1b]133;B\x07old cmd\r\n\x1b]133;C\x07FIRST_BLOCK_LINE\r\n\x1b]133;D;0\x07",
            "\x1b]133;B\x07cargo test\r\n\x1b]133;C\x07",
            "LINE_01 alpha\r\n",
            "LINE_02 match me\r\n",
            "LINE_03 secret sk-ant-api03-1234567890abcdefghijklmnopqrstuvwxyz\r\n",
            "LINE_04 match two\r\n",
            "LINE_05 match three\r\n",
            "\x1b]133;D;0\x07",
        );
        state.orchestration.observe_output(child, stream.as_bytes());

        // 1. last_command_only isolates ONLY the most recent completed block (no FIRST_BLOCK_LINE).
        let last_only = execute(
            &state,
            &ui,
            OrchestrateCall::Read {
                repo_path: repo.clone(),
                caller_session_id: Some(parent.into()),
                caller_token: Some(token.clone()),
                external_caller_id: None,
                child_session_id: child.into(),
                max_lines: None,
                offset_lines: None,
                grep: None,
                last_command_only: Some(true),
            },
        )
        .expect("last_command_only");
        assert_eq!(last_only["last_command_found"], true);
        assert_eq!(last_only["last_command"], "cargo test");
        let last_text = last_only["text"].as_str().unwrap();
        assert!(last_text.contains("LINE_01"), "{last_text}");
        assert!(!last_text.contains("FIRST_BLOCK_LINE"), "{last_text}");
        assert!(!last_text.contains("sk-ant-api03"), "{last_text}");

        // 2. grep + offset_lines paginate deterministically.
        let paged = execute(
            &state,
            &ui,
            OrchestrateCall::Read {
                repo_path: repo.clone(),
                caller_session_id: Some(parent.into()),
                caller_token: Some(token.clone()),
                external_caller_id: None,
                child_session_id: child.into(),
                max_lines: Some(1),
                offset_lines: Some(1),
                grep: Some("match".into()),
                last_command_only: Some(true),
            },
        )
        .expect("grep+offset");
        assert_eq!(paged["total_lines"], 3);
        assert_eq!(paged["lines_returned"], 1);
        assert_eq!(paged["text"], "LINE_04 match two");

        // 3. Redaction runs BEFORE grep so secrets cannot be probed via grep.
        let probe = execute(
            &state,
            &ui,
            OrchestrateCall::Read {
                repo_path: repo,
                caller_session_id: Some(parent.into()),
                caller_token: Some(token),
                external_caller_id: None,
                child_session_id: child.into(),
                max_lines: None,
                offset_lines: None,
                grep: Some("sk-ant-api03".into()),
                last_command_only: Some(true),
            },
        )
        .expect("grep redacted secret");
        assert_eq!(probe["total_lines"], 0);
        assert_eq!(probe["text"], "");
    }

    #[cfg(unix)]
    #[test]
    fn external_caller_spawns_parallel_shells_and_exec_supports_ephemeral_and_persistent_runs() {
        let (_dir, repo) = init_trusted_repo();
        let state = TerminalSessions::default();
        let ui = TestUiSink::default();

        // Partial credentials fail closed; missing external id fails closed.
        assert!(OrchestrateCall::List {
            repo_path: repo.clone(),
            caller_session_id: Some("term-1-1".into()),
            caller_token: None,
            external_caller_id: None,
            include_repo_shells: None,
        }
        .validate()
        .is_err());
        assert!(OrchestrateCall::List {
            repo_path: repo.clone(),
            caller_session_id: None,
            caller_token: None,
            external_caller_id: Some("badcaller".into()),
            include_repo_shells: None,
        }
        .validate()
        .is_err());

        let ext = Some("mcp-4242".to_string());

        // 1. Ephemeral Exec runs in a new shell, captures exact non-zero exit code & stdout, and closes.
        let eph = execute(
            &state,
            &ui,
            OrchestrateCall::Exec {
                repo_path: repo.clone(),
                caller_session_id: None,
                caller_token: None,
                external_caller_id: ext.clone(),
                child_session_id: None,
                cwd: None,
                command: "printf 'EPHEMERAL_OUT_91\\n'; (exit 7)".into(),
                timeout_ms: Some(10_000),
                pattern: None,
                keep_open: Some(false),
                split: Some(false),
                title: Some("ephemeral-run".into()),
                max_lines: Some(40),
            },
        )
        .expect("ephemeral exec");
        assert_eq!(eph["spawned"], true, "{eph}");
        assert_eq!(eph["kept_open"], false, "{eph}");
        assert_eq!(eph["timed_out"], false, "{eph}");
        assert_eq!(eph["exit_code"], 7, "{eph}");
        assert!(
            eph["text"].as_str().unwrap().contains("EPHEMERAL_OUT_91"),
            "{eph}"
        );

        // 2. Two persistent shells spawned by external caller preserve independent shell state in parallel.
        let s1 = execute(
            &state,
            &ui,
            OrchestrateCall::Exec {
                repo_path: repo.clone(),
                caller_session_id: None,
                caller_token: None,
                external_caller_id: ext.clone(),
                child_session_id: None,
                cwd: None,
                command: "export GP_TAG=ALPHA && cd src && pwd".into(),
                timeout_ms: Some(10_000),
                pattern: None,
                keep_open: Some(true),
                split: Some(false),
                title: Some("worker-1".into()),
                max_lines: None,
            },
        )
        .expect("exec shell 1 start");
        let id1 = s1["child_session_id"].as_str().unwrap().to_string();
        assert_eq!(s1["kept_open"], true, "{s1}");

        let s2 = execute(
            &state,
            &ui,
            OrchestrateCall::Exec {
                repo_path: repo.clone(),
                caller_session_id: None,
                caller_token: None,
                external_caller_id: ext.clone(),
                child_session_id: None,
                cwd: None,
                command: "export GP_TAG=BETA && echo SECOND_SHELL".into(),
                timeout_ms: Some(10_000),
                pattern: None,
                keep_open: Some(true),
                split: Some(false),
                title: Some("worker-2".into()),
                max_lines: None,
            },
        )
        .expect("exec shell 2 start");
        let id2 = s2["child_session_id"].as_str().unwrap().to_string();
        assert_ne!(id1, id2);

        // Re-running on shell 1 observes `GP_TAG=ALPHA` and subdir `src`!
        let s1_follow = execute(
            &state,
            &ui,
            OrchestrateCall::Exec {
                repo_path: repo.clone(),
                caller_session_id: None,
                caller_token: None,
                external_caller_id: ext.clone(),
                child_session_id: Some(id1.clone()),
                cwd: None,
                command: "printf '%s|%s\\n' \"$GP_TAG\" \"$(basename \"$PWD\")\"".into(),
                timeout_ms: Some(10_000),
                pattern: None,
                keep_open: Some(true),
                split: None,
                title: None,
                max_lines: None,
            },
        )
        .expect("exec shell 1 follow-up");
        assert!(
            s1_follow["text"].as_str().unwrap().contains("ALPHA|src"),
            "{s1_follow}"
        );

        let listed = execute(
            &state,
            &ui,
            OrchestrateCall::List {
                repo_path: repo.clone(),
                caller_session_id: None,
                caller_token: None,
                external_caller_id: ext.clone(),
                include_repo_shells: None,
            },
        )
        .expect("list external shells");
        assert!(listed["total"].as_u64().unwrap() >= 2, "{listed}");

        for cid in [&id1, &id2] {
            execute(
                &state,
                &ui,
                OrchestrateCall::Close {
                    repo_path: repo.clone(),
                    caller_session_id: None,
                    caller_token: None,
                    external_caller_id: ext.clone(),
                    child_session_id: cid.clone(),
                },
            )
            .expect("close external shell");
        }
    }

    #[cfg(unix)]
    #[test]
    fn allow_agent_shell_reads_grants_readonly_user_shell_access_while_blocking_send_and_close() {
        let (_dir, repo) = init_trusted_repo();
        let state = TerminalSessions::default();
        let ui = TestUiSink::default();

        let user_shell = spawn_at(
            &state,
            Place::Repo {
                repo_path: repo.clone(),
                start_dir: None,
            },
            24,
            80,
            None,
            None,
            None,
            discard_sink(),
            Arc::new(RecordingEvents),
            None,
            None,
        )
        .expect("user shell");

        state
            .orchestration
            .observe_output(&user_shell.id, b"USER_SHELL_OUTPUT_55\r\n");

        let ext = Some("mcp-9999".to_string());

        // When allow_agent_shell_reads == false (default), user shells are omitted & blocked.
        let list_off = execute_with_shell_reads(
            &state,
            &ui,
            OrchestrateCall::List {
                repo_path: repo.clone(),
                caller_session_id: None,
                caller_token: None,
                external_caller_id: ext.clone(),
                include_repo_shells: Some(true),
            },
            false,
        )
        .expect("list off");
        assert_eq!(list_off["shell_reads_enabled"], false);
        assert_eq!(list_off["total"], 0);
        assert_eq!(
            execute_with_shell_reads(
                &state,
                &ui,
                OrchestrateCall::Read {
                    repo_path: repo.clone(),
                    caller_session_id: None,
                    caller_token: None,
                    external_caller_id: ext.clone(),
                    child_session_id: user_shell.id.clone(),
                    max_lines: None,
                    offset_lines: None,
                    grep: None,
                    last_command_only: None,
                },
                false,
            )
            .unwrap_err(),
            LINEAGE_REFUSAL
        );

        // When enabled, List(include_repo_shells: true) and Read see the user shell with owned=false...
        let list_on = execute_with_shell_reads(
            &state,
            &ui,
            OrchestrateCall::List {
                repo_path: repo.clone(),
                caller_session_id: None,
                caller_token: None,
                external_caller_id: ext.clone(),
                include_repo_shells: Some(true),
            },
            true,
        )
        .expect("list on");
        assert_eq!(list_on["shell_reads_enabled"], true);
        assert_eq!(list_on["total"], 1);
        assert_eq!(list_on["children"][0]["child_session_id"], user_shell.id);
        assert_eq!(list_on["children"][0]["owned"], false);

        let read_on = execute_with_shell_reads(
            &state,
            &ui,
            OrchestrateCall::Read {
                repo_path: repo.clone(),
                caller_session_id: None,
                caller_token: None,
                external_caller_id: ext.clone(),
                child_session_id: user_shell.id.clone(),
                max_lines: Some(40),
                offset_lines: None,
                grep: None,
                last_command_only: None,
            },
            true,
        )
        .expect("read user shell when opted in");
        assert_eq!(read_on["owned"], false);
        assert!(read_on["text"].as_str().unwrap().contains("USER_SHELL_OUTPUT_55"));

        // ...while Send and Close on the user shell STILL fail closed with LINEAGE_REFUSAL!
        assert_eq!(
            execute_with_shell_reads(
                &state,
                &ui,
                OrchestrateCall::Send {
                    repo_path: repo.clone(),
                    caller_session_id: None,
                    caller_token: None,
                    external_caller_id: ext.clone(),
                    child_session_id: user_shell.id.clone(),
                    input: "echo nope".into(),
                    append_newline: Some(true),
                },
                true,
            )
            .unwrap_err(),
            LINEAGE_REFUSAL
        );
        assert_eq!(
            execute_with_shell_reads(
                &state,
                &ui,
                OrchestrateCall::Close {
                    repo_path: repo,
                    caller_session_id: None,
                    caller_token: None,
                    external_caller_id: ext,
                    child_session_id: user_shell.id.clone(),
                },
                true,
            )
            .unwrap_err(),
            LINEAGE_REFUSAL
        );

        let _ = crate::terminal::kill_session(&state, &user_shell.id);
    }
}
