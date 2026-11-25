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
use crate::terminal::screen_feed::{Batch, Lag, ScreenFeed};
use crate::terminal::{
    bounded_pty_size, detach_session, is_session_id, spawn_at, write_input, OutputSink, Place,
    SessionEvents, SharedSessionTelemetry, TerminalSessions,
};
use crate::terminal::{KillOrigin, KillOutcome};
#[cfg(unix)]
use base64::Engine as _;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::collections::{HashMap, VecDeque};
use std::path::Path;
use std::sync::{Arc, Condvar, Mutex};
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
/// How long a `Read`, `Send` or `Exec` waits for the screen to catch up with
/// the output already delivered before it answers from what the screen has
/// and says how far behind it is. The queue is bounded, so a healthy parse
/// catches up in a few milliseconds; this is the cap when it cannot.
const SCREEN_SYNC_BUDGET: Duration = Duration::from_secs(2);
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

/// Owner-key prefix of terminal children launched by a Mavi thread:
/// `ext-mavi-<thread key>`, where the thread key is the page's
/// `ThreadRecord.key`, a UUID (45 bytes in all, under the caller-id limit).
pub(crate) const MAVI_OWNER_PREFIX: &str = "ext-mavi-";

/// Environment variable a host sets on an agent process so that agent's
/// `gitpulse-mcp` / `gitpulse-hook` orchestrate as a stable external caller
/// (today only `ext-mavi-<uuid>`) instead of the per-process `mcp-<pid>`.
pub const ORCHESTRATE_CALLER_ENV: &str = "GITPULSE_ORCHESTRATE_CALLER";

/// A canonical (lowercase, hyphenated, 36-byte) UUID, as `crypto.randomUUID`
/// writes it. Anything else is not a Mavi thread key.
fn is_canonical_uuid(s: &str) -> bool {
    s.len() == 36
        && s.bytes().enumerate().all(|(i, b)| match i {
            8 | 13 | 18 | 23 => b == b'-',
            _ => b.is_ascii_digit() || (b'a'..=b'f').contains(&b),
        })
}

/// The owner key for a Mavi thread's children, or `None` when the thread key
/// is not a UUID (an older fallback key, or anything a page made up). Upper
/// case is folded so the key is one spelling per thread.
pub(crate) fn mavi_owner_key(thread_key: &str) -> Option<String> {
    let lower = thread_key.trim().to_ascii_lowercase();
    is_canonical_uuid(&lower).then(|| format!("{MAVI_OWNER_PREFIX}{lower}"))
}

/// The Mavi thread key an owner key names, or `None` when the key is not
/// exactly `ext-mavi-<canonical uuid>`.
pub(crate) fn mavi_thread_of_owner(owner_key: &str) -> Option<&str> {
    owner_key
        .strip_prefix(MAVI_OWNER_PREFIX)
        .filter(|rest| is_canonical_uuid(rest))
}

/// The external caller id a process with no terminal credentials orchestrates
/// as: the Mavi owner key its host put in [`ORCHESTRATE_CALLER_ENV`], or
/// `fallback()` (the per-process `mcp-<pid>` / `ext-<ppid>`) when the variable
/// is unset or blank.
///
/// A value that is not exactly `ext-mavi-<uuid>` is an error, never a quiet
/// fallback: the caller would otherwise orchestrate under an owner the host
/// did not name, and its children would be unreachable from the thread.
///
/// The key carries no secret. Any same-user process that sets the variable can
/// act as that thread's orchestrator over its own children in a trusted
/// repository; that is no wider than the `ext-*` / `mcp-*` keys any same-user
/// process can already claim, and repository trust still gates every call.
pub fn standalone_caller_id(fallback: impl FnOnce() -> String) -> Result<String, String> {
    let raw = std::env::var(ORCHESTRATE_CALLER_ENV).ok();
    standalone_caller_id_from(raw.as_deref(), fallback)
}

fn standalone_caller_id_from(
    raw: Option<&str>,
    fallback: impl FnOnce() -> String,
) -> Result<String, String> {
    let Some(value) = raw.map(str::trim).filter(|v| !v.is_empty()) else {
        return Ok(fallback());
    };
    if mavi_thread_of_owner(value).is_some() {
        return Ok(value.to_owned());
    }
    let shown: String = value.chars().take(64).collect();
    Err(format!(
        "{ORCHESTRATE_CALLER_ENV} must be ext-mavi-<thread uuid> (got {shown:?}); unset it to orchestrate as a standalone caller"
    ))
}

/// The process id inside a standalone owner key (`mcp-<pid>` or `ext-<pid>`),
/// or `None` for any other key (a terminal session id, `ext-mavi-<uuid>`).
fn standalone_owner_pid(key: &str) -> Option<u32> {
    let digits = key
        .strip_prefix("mcp-")
        .or_else(|| key.strip_prefix("ext-"))?;
    if digits.is_empty() || !digits.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    digits.parse().ok()
}

/// Whether a process with this id exists. `EPERM` means it exists and is not
/// ours (macOS answers it for a process group that is all zombies too), so it
/// counts as alive: only `ESRCH` proves the owner gone.
#[cfg(unix)]
fn process_exists(pid: u32) -> bool {
    let Ok(pid) = i32::try_from(pid) else {
        return false;
    };
    if pid == 0 {
        return false;
    }
    // SAFETY: signal 0 only checks that the process exists and may be signalled.
    if unsafe { libc::kill(pid, 0) } == 0 {
        return true;
    }
    std::io::Error::last_os_error().raw_os_error() == Some(libc::EPERM)
}

/// Without a cheap probe, an owner is never declared dead.
#[cfg(not(unix))]
fn process_exists(_pid: u32) -> bool {
    true
}

/// Whether `key` names a standalone orchestrator (`mcp-<pid>` / `ext-<pid>`)
/// whose process is gone. A Mavi thread key, or a terminal session id, is
/// never dead by this test: a resumed thread owns its children again.
pub(crate) fn owner_is_dead(key: &str) -> bool {
    standalone_owner_pid(key).is_some_and(|pid| !process_exists(pid))
}

/// What [`close_owner_children`] did.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct OwnerCloseSummary {
    pub closed: usize,
    pub skipped_protected: usize,
}

/// Closes every live child owned by the Mavi thread key `owner_key`, as the
/// user's "stop children" does: a protected session is left running and
/// counted. Only the app calls this (the user forgot the thread); no MCP
/// caller reaches it, and children of any other owner are never touched.
pub(crate) fn close_owner_children(
    state: &TerminalSessions,
    ui: &dyn OrchestrateUiSink,
    owner_key: &str,
) -> Result<OwnerCloseSummary, String> {
    if mavi_thread_of_owner(owner_key).is_none() {
        return Err("not a Mavi thread owner key".into());
    }
    let targets: Vec<(String, String)> = {
        let inner = state.orchestration.lock();
        inner
            .sessions
            .iter()
            .filter(|(_, rec)| !rec.ended && rec.parent_session_id.as_deref() == Some(owner_key))
            .map(|(id, rec)| (id.clone(), rec.repo_path.clone()))
            .collect()
    };
    let mut summary = OwnerCloseSummary {
        closed: 0,
        skipped_protected: 0,
    };
    let mut failures = Vec::new();
    for (id, repo_path) in targets {
        match crate::terminal::kill_session_as(
            state,
            &id,
            KillOrigin::StopChildren,
            Some(owner_key),
        ) {
            Ok(KillOutcome::Killed) => {
                state.orchestration.mark_ended(&id, None);
                ui.child_closed(TerminalChildClosedPayload {
                    id,
                    parent_session_id: owner_key.to_owned(),
                    repo_path,
                });
                summary.closed += 1;
            }
            Ok(KillOutcome::SkippedProtected) => summary.skipped_protected += 1,
            Err(error) => failures.push(format!("{id}: {error}")),
        }
    }
    if failures.is_empty() {
        Ok(summary)
    } else {
        Err(format!(
            "closed {}, left {} protected, and could not close {}: {}",
            summary.closed,
            summary.skipped_protected,
            failures.len(),
            failures.join("; ")
        ))
    }
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

/// Tauri event telling a Mavi thread's page that one of its terminal children
/// needs the user, stopped needing them, or ended.
pub const MAVI_CHILD_STATE_EVENT: &str = "mavi-child-state";

/// Payload of [`MAVI_CHILD_STATE_EVENT`], snake_case on the wire. `state` is
/// `needs_input` (a dialog is open), `cleared` (it was answered while the
/// child lives), `done` (the child exited 0) or `ended` (any other end).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MaviChildStatePayload {
    pub thread_id: String,
    pub child_session_id: String,
    pub title: Option<String>,
    pub state: String,
    pub detail: Option<String>,
    pub exit_code: Option<i32>,
}

/// Where [`MaviChildStatePayload`]s go. The app's sink emits a Tauri event;
/// tests record them.
pub trait ChildStateEmitter: Send + Sync {
    fn mavi_child_state(&self, payload: MaviChildStatePayload);
}

impl<R: tauri::Runtime> ChildStateEmitter for AppOrchestrateUiSink<R> {
    fn mavi_child_state(&self, payload: MaviChildStatePayload) {
        use tauri::Emitter as _;
        if let Err(error) = self.app.emit(MAVI_CHILD_STATE_EVENT, payload) {
            log::warn!(target: "terminal", "could not announce a Mavi child's state: {error}");
        }
    }
}

type ChildEmitterSlot = Mutex<Option<Arc<dyn ChildStateEmitter>>>;

fn emit_child_state(emitter: &ChildEmitterSlot, payload: Option<MaviChildStatePayload>) {
    let Some(payload) = payload else {
        return;
    };
    let sink = emitter
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .clone();
    if let Some(sink) = sink {
        sink.mavi_child_state(payload);
    }
}

/// The payload for a child owned by a Mavi thread, `None` for any other.
fn mavi_child_payload(
    session_id: &str,
    record: &OrchestratedRecord,
    state: &str,
    detail: Option<String>,
    exit_code: Option<i32>,
) -> Option<MaviChildStatePayload> {
    let thread = mavi_thread_of_owner(record.parent_session_id.as_deref()?)?;
    Some(MaviChildStatePayload {
        thread_id: thread.to_owned(),
        child_session_id: session_id.to_owned(),
        title: record.title.clone(),
        state: state.to_owned(),
        detail,
        exit_code,
    })
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

#[cfg(unix)]
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

    /// Forgets what the bytes dropped in a gap may have left half-read (an
    /// escape or OSC sequence) and keeps everything else: a command in flight
    /// keeps its phase, so its `133;D` after the gap still records the block
    /// (with the output the gap spared) instead of being ignored as stray.
    fn resync(&mut self) {
        self.osc_state = OscCaptureState::Ground;
        self.osc_buf.clear();
        self.osc_overflow = false;
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
    #[cfg_attr(not(unix), allow(dead_code))]
    host_scanner: Option<crate::alerts::scan::Scanner>,
    blocks: Option<CommandBlockRecorder>,
    telemetry: SharedSessionTelemetry,
}

/// The app's telemetry scanner over the output it mirrors from the terminal
/// host. It reads no agent dialogs: the host reads them (it sees every
/// session, attached or not) and says so in a notice, which
/// [`OrchestrationRegistry::note_screen_notice`] takes.
fn host_scanner() -> crate::alerts::scan::Scanner {
    let mut scanner = crate::alerts::scan::Scanner::new();
    scanner.set_agent_prompts(false);
    scanner
}

#[cfg(unix)]
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
///
/// Output is not parsed where it is delivered. [`Self::observe_output`] and
/// [`Self::observe_output_from_host`] queue it on [`ScreenFeed`] and return;
/// one `terminal-screen-parse` worker applies it to the screens, in order per
/// session, so a flood never holds the delivery thread (or another session's
/// reader) behind the registry lock while it is parsed. Readers call
/// [`Self::sync_screen`] to wait, bounded, for the screen to reach the bytes
/// already delivered, and `Read`/`Wait`/`Exec` replies carry a `screen` object
/// saying whether it is complete.
#[derive(Default)]
pub struct OrchestrationRegistry {
    inner: Arc<Mutex<RegistryInner>>,
    changed: Arc<Condvar>,
    feed: Arc<ScreenFeed>,
    worker: Mutex<Option<std::thread::JoinHandle<()>>>,
    /// Where `mavi-child-state` goes; set once the app exists.
    child_emitter: Arc<ChildEmitterSlot>,
}

/// What a reader got from [`OrchestrationRegistry::sync_screen`].
#[derive(Debug, Clone, Copy)]
pub(crate) struct ScreenSync {
    /// The screen reflects every byte that was delivered when the wait began
    /// (always true for a session with no screen).
    pub caught_up: bool,
}

impl Drop for OrchestrationRegistry {
    fn drop(&mut self) {
        self.feed.shutdown();
        let worker = self
            .worker
            .get_mut()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .take();
        if let Some(worker) = worker {
            let _ = worker.join();
        }
    }
}

/// Applies one batch of a session's output to its screen, block recorder and
/// (for a host session) telemetry scanner. Holds the registry lock for that
/// one batch only (`BATCH_BYTES`), so a reader waits behind at most one.
fn apply_screen_batch(inner: &Mutex<RegistryInner>, feed: &ScreenFeed, batch: &Batch) {
    let mut guard = inner
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    // A reset since the batch was taken means a screen rebuilt from a replay
    // already covers these bytes.
    if !feed.is_current(batch) {
        return;
    }
    if let Some(record) = guard.sessions.get_mut(&batch.session) {
        let (rows, cols) = (record.rows, record.cols);
        if record.parser.is_some() {
            if batch.resync {
                // Bytes were dropped (or a parse failed) ahead of this batch:
                // the old screen may be inside an escape sequence they opened,
                // so it is replaced rather than fed across the gap.
                record.parser = Some(vt100::Parser::new(
                    nz_u16(rows),
                    nz_u16(cols),
                    PARSER_SCROLLBACK_ROWS,
                ));
                if let Some(blocks) = record.blocks.as_mut() {
                    blocks.resync();
                }
                if record.host_scanner.is_some() {
                    record.host_scanner = Some(host_scanner());
                }
            }
            if let Some(parser) = record.parser.as_mut() {
                parser.process(&batch.bytes);
            }
            if let Some(blocks) = record.blocks.as_mut() {
                blocks.feed(&batch.bytes, cols);
            }
            record.observed_bytes = record
                .observed_bytes
                .saturating_add(batch.dropped_before)
                .saturating_add(batch.bytes.len() as u64);
            #[cfg(unix)]
            {
                if batch.scan {
                    if let Some(scanner) = record.host_scanner.as_mut() {
                        apply_scanner_telemetry(scanner, &record.telemetry, &batch.bytes);
                    }
                }
            }
        }
    }
    feed.applied(batch);
}

fn run_screen_worker(inner: &Mutex<RegistryInner>, changed: &Condvar, feed: &ScreenFeed) {
    while let Some(batch) = feed.take() {
        let applied = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            apply_screen_batch(inner, feed, &batch);
        }));
        if let Err(panic) = applied {
            // The batch is lost and the screen it was part of cannot be
            // trusted: readers see it as a gap, never as a clean screen.
            log::error!(
                target: "terminal",
                "a terminal screen parse panicked, the screen resyncs: {}",
                crate::supervisor::payload_text(panic.as_ref())
            );
            feed.failed(&batch);
        }
        changed.notify_all();
    }
}

impl OrchestrationRegistry {
    fn lock(&self) -> std::sync::MutexGuard<'_, RegistryInner> {
        self.inner
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    /// Starts the screen worker the first time a session has a screen. A
    /// registry that never tracks one (most unit tests) never starts a thread.
    fn ensure_worker(&self) {
        let mut worker = self
            .worker
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if worker.is_some() {
            return;
        }
        let (inner, changed, feed) = (self.inner.clone(), self.changed.clone(), self.feed.clone());
        match crate::supervisor::spawn_service("terminal-screen-parse", move || {
            run_screen_worker(&inner, &changed, &feed);
        }) {
            Ok(handle) => *worker = Some(handle),
            Err(error) => log::error!(
                target: "terminal",
                "the terminal screen worker could not start, screens will report lag: {error}"
            ),
        }
    }

    /// Waits, for at most `budget`, until `session_id`'s screen reflects every
    /// byte delivered when this was called. A flood that keeps arriving does
    /// not move the target, so the wait ends. `caught_up: false` means the
    /// budget ran out first; the caller answers from the screen it has and
    /// reports [`Self::screen_status`].
    pub(crate) fn sync_screen(&self, session_id: &str, budget: Duration) -> ScreenSync {
        let Some(target) = self.feed.lag(session_id).map(|lag| lag.delivered) else {
            return ScreenSync { caught_up: true };
        };
        let deadline = Instant::now() + budget;
        let mut inner = self.lock();
        loop {
            // The worker advances `parsed` while holding this lock, so a check
            // made under it cannot miss the wake-up that follows.
            let Some(Lag { parsed, .. }) = self.feed.lag(session_id) else {
                return ScreenSync { caught_up: true };
            };
            if parsed >= target {
                return ScreenSync { caught_up: true };
            }
            let now = Instant::now();
            if now >= deadline {
                return ScreenSync { caught_up: false };
            }
            let slice = (deadline - now).min(Duration::from_millis(50));
            inner = self
                .changed
                .wait_timeout(inner, slice)
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .0;
        }
    }

    /// How complete `session_id`'s screen is right now, for a reply: `null`
    /// when the session has no screen, otherwise whether it reflects every
    /// byte delivered, how many it trails by, and how many bytes were dropped
    /// (making the screen a rebuild across a gap) since it was last replayed.
    pub(crate) fn screen_status(&self, session_id: &str) -> Value {
        match self.feed.lag(session_id) {
            Some(lag) => json!({
                "complete": lag.complete(),
                "lag_bytes": lag.lag_bytes(),
                "gap_bytes": lag.gap_bytes,
            }),
            None => Value::Null,
        }
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
            vt100::Parser::new(nz_u16(size.rows), nz_u16(size.cols), PARSER_SCROLLBACK_ROWS)
        });
        let host_scanner = track_screen.then(|| host_scanner());
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
        if track_screen {
            self.feed.register(session_id);
        } else {
            self.feed.forget(session_id);
        }
        drop(inner);
        if track_screen {
            self.ensure_worker();
        }
        self.changed.notify_all();
        Ok(())
    }

    pub(crate) fn notify_changed(&self) {
        self.changed.notify_all();
    }

    /// Resets an orchestrated child's `vt100` screen right before `gitpulse-ptyd`
    /// streams its ring replay on `Attach`, `AttachTracked`, or reconnect so
    /// replayed bytes never duplicate onto an already-populated grid.
    #[cfg(unix)]
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
                // Queued bytes predate the replay and are covered by it.
                self.feed.reset(session_id);
                self.changed.notify_all();
            }
        }
    }

    /// Restores parent/child lineage, RAM-only caller capability token digests,
    /// and OSC telemetry from `gitpulse-ptyd` across app restarts or socket
    /// reconnects.
    #[cfg(unix)]
    pub(crate) fn adopt_hosted_sessions(&self, snapshots: &[crate::terminal::HostedSessionMeta]) {
        if snapshots.is_empty() {
            return;
        }
        let mut inner = self.lock();
        let mut tracked = false;
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
            if track_screen {
                self.feed.register(&snap.session_id);
                tracked = true;
            }
            let host_scanner = track_screen.then(|| host_scanner());
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
                    host_scanner,
                    blocks: track_screen.then(CommandBlockRecorder::default),
                    telemetry,
                },
            );
        }
        drop(inner);
        if tracked {
            self.ensure_worker();
        }
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
        self.feed.forget(session_id);
        self.changed.notify_all();
    }

    /// Queues raw PTY output bytes for an orchestrated child's or shell's
    /// `vt100` screen and returns at once; the screen worker applies them and
    /// wakes any parent `Wait` caller. No-op for sessions with no screen.
    pub(crate) fn observe_output(&self, session_id: &str, bytes: &[u8]) {
        self.feed.push(session_id, bytes, false);
    }

    /// Queues raw PTY output bytes arriving from `gitpulse-ptyd` (`HostClient`)
    /// for both the child's `vt100` screen and its OSC telemetry scanner so
    /// `gitpulse-ptyd` and in-process sessions expose identical `Wait`/`Read`
    /// telemetry. Returns at once, like [`Self::observe_output`].
    #[cfg(unix)]
    pub(crate) fn observe_output_from_host(&self, session_id: &str, bytes: &[u8]) {
        self.feed.push(session_id, bytes, true);
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
        let mut announce = None;
        let retain = match inner.sessions.get_mut(session_id) {
            Some(record) => {
                if !record.ended {
                    let state = if exit_code == Some(0) { "done" } else { "ended" };
                    announce = mavi_child_payload(session_id, record, state, None, exit_code);
                }
                record.token_hash = None;
                record.ended = true;
                record.exit_code = exit_code;
                // Nobody can answer a dialog in a session that is gone.
                crate::terminal::clear_agent_prompt(
                    &mut record
                        .telemetry
                        .0
                        .lock()
                        .unwrap_or_else(std::sync::PoisonError::into_inner),
                );
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
                        self.feed.forget(&evicted);
                    }
                }
            }
        } else {
            inner.sessions.remove(session_id);
            self.feed.forget(session_id);
        }
        drop(inner);
        self.changed.notify_all();
        emit_child_state(&self.child_emitter, announce);
    }

    /// Where `mavi-child-state` events go. Called once, when the app exists.
    pub(crate) fn set_child_state_emitter(&self, emitter: Arc<dyn ChildStateEmitter>) {
        *self
            .child_emitter
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(emitter);
    }

    /// Keystrokes written to `session_id`'s PTY. Anything but cursor
    /// navigation answers or dismisses a standing dialog.
    pub(crate) fn note_input(&self, session_id: &str, bytes: &[u8]) {
        if crate::terminal::input_answers_prompt(bytes) {
            self.clear_prompt(session_id, true);
        }
    }

    /// A hook reported the agent working again in `session_id`: a dialog read
    /// off its screen is no longer open.
    pub(crate) fn note_hook_resolution(&self, session_id: &str) {
        self.clear_prompt(session_id, true);
    }

    /// Drops the dialog read off `session_id`'s screen, wakes waiters, resolves
    /// the banner and tells the Mavi page. Nothing happens when none is open.
    fn clear_prompt(&self, session_id: &str, resolve_banner: bool) {
        let (cleared, announce, label, place) = {
            let inner = self.lock();
            let Some(record) = inner.sessions.get(session_id) else {
                return;
            };
            let cleared = crate::terminal::clear_agent_prompt(
                &mut record
                    .telemetry
                    .0
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner),
            );
            (
                cleared,
                (cleared && !record.ended)
                    .then(|| mavi_child_payload(session_id, record, "cleared", None, None))
                    .flatten(),
                crate::terminal::agent_launcher("agy").unwrap_or("Antigravity"),
                Path::new(&record.cwd)
                    .file_name()
                    .map(|name| name.to_string_lossy().into_owned()),
            )
        };
        if !cleared {
            return;
        }
        self.changed.notify_all();
        if resolve_banner {
            crate::alerts::offer(crate::alerts::bridge::screen_prompt_notice(
                session_id,
                label,
                place,
                "prompt_submitted",
                None,
            ));
        }
        emit_child_state(&self.child_emitter, announce);
    }

    /// The terminal host read a dialog off a session's screen (or saw it
    /// answered) and said so in a notice: the one place the app learns it,
    /// for attached and detached sessions alike. Records it in the session's
    /// telemetry so `wait` and `read` can say so, and tells the Mavi page.
    pub(crate) fn note_screen_notice(&self, notice: &crate::alerts::Notice) {
        if notice.channel != crate::alerts::bridge::SCREEN_CHANNEL {
            return;
        }
        match notice.event {
            Some("permission_prompt") => {
                let announce = {
                    let inner = self.lock();
                    let Some(record) = inner.sessions.get(&notice.key) else {
                        return;
                    };
                    if record.ended {
                        return;
                    }
                    let (lock, cvar) = &*record.telemetry;
                    let mut state = lock
                        .lock()
                        .unwrap_or_else(std::sync::PoisonError::into_inner);
                    let before = state.last_prompt_event_seq;
                    crate::terminal::apply_telemetry_events(
                        &mut state,
                        vec![crate::alerts::scan::TelemetryEvent::AgentPrompt {
                            detail: notice.detail.clone().unwrap_or_default(),
                        }],
                    );
                    if state.last_prompt_event_seq == before {
                        return;
                    }
                    cvar.notify_all();
                    let detail = state.agent_prompt.as_ref().and_then(|p| p.detail.clone());
                    mavi_child_payload(&notice.key, record, "needs_input", detail, None)
                };
                self.changed.notify_all();
                emit_child_state(&self.child_emitter, announce);
            }
            Some("prompt_submitted") => self.clear_prompt(&notice.key, false),
            _ => {}
        }
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
    if record.parent_session_id.as_deref() != Some(caller_key) || record.repo_path != canonical_repo
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

/// Brings `session_id`'s screen up to what has been delivered (waiting at most
/// `budget` for the screen worker) and, for a detached host session, refreshes
/// it from the host's transcript. Every reader calls this before it looks.
fn settle_screen(state: &TerminalSessions, session_id: &str, budget: Duration) -> ScreenSync {
    let sync = state.orchestration.sync_screen(session_id, budget);
    // A refresh applies transcript bytes on top of the screen (or rebuilds it
    // and drops what is queued): done while bytes are still queued it would
    // apply some twice or lose them, so a lagging screen is left to its feed.
    #[cfg(unix)]
    {
        if sync.caught_up {
            refresh_detached_host_screen(state, session_id);
        }
    }
    sync
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
            // The host reads no dialogs off the screen (the app does, from the
            // output it mirrors), so what the app holds about one survives the sync.
            let mut merged = remote_tel;
            merged.agent_prompt = local_tel.agent_prompt.clone();
            merged.last_prompt_event_seq = local_tel.last_prompt_event_seq;
            merged.ended = local_tel.ended || merged.ended;
            if merged != *local_tel {
                *local_tel = merged;
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
                        // The screen is rebuilt whole from the transcript, which
                        // covers whatever was still queued for the old one.
                        state.orchestration.feed.reset(child_session_id);
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
                                let mut scanner = host_scanner();
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
        // A child whose standalone owner process is gone is orphaned, not
        // active: it stays open for the user to close, but it must not keep
        // the next orchestrator out. One probe per distinct owner.
        let mut dead_owners: HashMap<String, bool> = HashMap::new();
        let mut counts_toward_caps = |rec: &OrchestratedRecord| {
            !rec.ended
                && rec.parent_session_id.as_deref().is_some_and(|owner| {
                    let dead = match dead_owners.get(owner) {
                        Some(known) => *known,
                        None => {
                            let probed = owner_is_dead(owner);
                            dead_owners.insert(owner.to_owned(), probed);
                            probed
                        }
                    };
                    !dead
                })
        };
        let active_children = inner
            .sessions
            .values()
            .filter(|rec| {
                rec.parent_session_id.as_deref() == Some(caller.parent_key())
                    && counts_toward_caps(rec)
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
                    rec.parent_session_id
                        .as_deref()
                        .is_some_and(is_valid_external_caller_id)
                        && counts_toward_caps(rec)
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
    /// The dialog the child is blocked on, when one is open.
    agent_prompt: Option<crate::terminal::AgentPrompt>,
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
        let wait_slice = deadline
            .saturating_duration_since(Instant::now())
            .min(Duration::from_millis(50));
        let screen = settle_screen(state, child_session_id, wait_slice);
        // A match on what the screen has is a match. Only the exit verdict waits
        // for the screen to catch up: an exit seen before its last output is on
        // the screen would end a pattern wait that output is about to satisfy.
        let behind = !screen.caught_up && Instant::now() < deadline;

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

        let exit_known = ended && !behind;
        let mut matched_reason = if let Some(tag) = required_exec_tag {
            if let Some(pat) = pattern {
                let (screen, _, _, _) = render_redacted_screen(rec, MAX_READ_LINES);
                if screen.contains(pat) {
                    Some("pattern")
                } else if rec.blocks.as_ref().is_some_and(|b| b.has_tag(tag)) {
                    Some("command_finished")
                } else if exit_known {
                    Some("ended")
                } else {
                    None
                }
            } else if rec.blocks.as_ref().is_some_and(|b| b.has_tag(tag)) {
                Some("command_finished")
            } else if exit_known {
                Some("ended")
            } else {
                None
            }
        } else if let Some(pat) = pattern {
            let (screen, _, _, _) = render_redacted_screen(rec, MAX_READ_LINES);
            if screen.contains(pat) {
                Some("pattern")
            } else if exit_known {
                Some("ended")
            } else {
                None
            }
        } else if exit_known {
            Some("ended")
        } else if tel.last_herdr_event_seq > effective_since
            && matches!(
                herdr.as_deref(),
                Some("done" | "idle" | "blocked" | "error")
            )
        {
            Some("herdr_status")
        } else if wait_idle && tel.last_command_event_seq > effective_since && !tel.command_running
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
        // A child blocked on a dialog is not progressing, whatever else its
        // screen says: that outranks every reason but its exit. A dialog still
        // open answers every wait, since it still holds.
        if tel.agent_prompt.is_some() && matched_reason != Some("ended") {
            matched_reason = Some("needs_input");
        }

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
                agent_prompt: tel.agent_prompt,
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
                agent_prompt: tel.agent_prompt,
            });
        }

        let idle_slice = (deadline - now).min(Duration::from_millis(50));
        let _ = state
            .orchestration
            .changed
            .wait_timeout(inner, idle_slice)
            .unwrap_or_else(std::sync::PoisonError::into_inner);
    }
}

fn close_child_internal(
    state: &TerminalSessions,
    ui: &dyn OrchestrateUiSink,
    caller_key: &str,
    child_session_id: &str,
    canonical_repo: &str,
    origin: KillOrigin,
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
        // The caller rides with the origin into the log and lifecycle record.
        match crate::terminal::kill_session_as(state, child_session_id, origin, Some(caller_key))? {
            KillOutcome::Killed => {}
            KillOutcome::SkippedProtected => {
                return Err(crate::terminal::lifecycle::skipped_protected_message(
                    child_session_id,
                    origin,
                ));
            }
        }
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
                let _ = settle_screen(state, &cid, SCREEN_SYNC_BUDGET);
                let inner = state.orchestration.lock();
                let rec = authorize_child(&inner, caller.parent_key(), &cid, &canonical_repo)?;
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
                            KillOrigin::AgentExecCleanup,
                        );
                    }
                    return Err(err);
                }
            };

            let _ = state
                .orchestration
                .sync_screen(&target_id, SCREEN_SYNC_BUDGET);
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
            let _ = state
                .orchestration
                .sync_screen(&target_id, SCREEN_SYNC_BUDGET);
            let (text, lines_returned, total_lines, truncated, cmd_exit_code, active_cwd, ended) = {
                let inner = state.orchestration.lock();
                let rec =
                    authorize_child(&inner, caller.parent_key(), &target_id, &canonical_repo)?;
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
                    KillOrigin::AgentExecCleanup,
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
                "agent_prompt": wait.agent_prompt,
                "screen": state.orchestration.screen_status(&target_id),
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
                                "orphaned": rec
                                    .parent_session_id
                                    .as_deref()
                                    .is_some_and(owner_is_dead),
                                "ended": ended,
                                "exit_code": exit_code,
                                "command_running": tel.command_running,
                                "last_exit_code": tel.last_exit_code,
                                "herdr_status": herdr_state_str(&tel.herdr_status),
                                "dev_server_url": tel.dev_server_url,
                                "agent_prompt": tel.agent_prompt,
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
                "agent_prompt": outcome.agent_prompt,
                "screen": state.orchestration.screen_status(&child_session_id),
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

            let _ = settle_screen(state, &child_session_id, SCREEN_SYNC_BUDGET);

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
                        let (t, lr, tl, tr) =
                            render_redacted_screen_filtered(rec, eff_max, offset, grep.as_deref());
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
                "agent_prompt": tel.agent_prompt,
                "screen": state.orchestration.screen_status(&child_session_id),
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

            // An interrupt is for a session that may be flooding: it does not
            // wait for that session's screen.
            let interrupt = matches!(input.as_str(), "\x03" | "\x04");
            let _ = settle_screen(
                state,
                &child_session_id,
                if interrupt {
                    Duration::ZERO
                } else {
                    SCREEN_SYNC_BUDGET
                },
            );

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
                KillOrigin::AgentClose,
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

    #[test]
    fn mavi_owner_key_accepts_only_canonical_uuid_thread_keys() {
        let key = "0f8e2b9a-1c3d-4e5f-8a6b-7c8d9e0f1a2b";
        let owner = mavi_owner_key(key).expect("uuid thread key");
        assert_eq!(owner, format!("ext-mavi-{key}"));
        assert!(owner.len() <= 64);
        assert!(is_valid_external_caller_id(&owner));
        assert_eq!(mavi_thread_of_owner(&owner), Some(key));
        // Upper case folds to one spelling per thread.
        assert_eq!(mavi_owner_key(&key.to_ascii_uppercase()), Some(owner));
        for bad in [
            "",
            "t1795634163229abc",
            "0f8e2b9a1c3d4e5f8a6b7c8d9e0f1a2b",
            "0f8e2b9a-1c3d-4e5f-8a6b-7c8d9e0f1a2",
            "0f8e2b9a-1c3d-4e5f-8a6b-7c8d9e0f1a2bb",
            "0f8e2b9a-1c3d-4e5f-8a6b-7c8d9e0f1a2g",
            "../../../../etc/passwd-0000-0000-0000",
        ] {
            assert_eq!(mavi_owner_key(bad), None, "{bad:?}");
        }
        for not_mavi in [
            "ext-1234",
            "mcp-99",
            "ext-mavi-",
            "ext-mavi-not-a-uuid",
            "ext-mavi-0F8E2B9A-1C3D-4E5F-8A6B-7C8D9E0F1A2B",
            "term-1-ab",
        ] {
            assert_eq!(mavi_thread_of_owner(not_mavi), None, "{not_mavi:?}");
        }
    }

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

        let _ = crate::terminal::kill_session(&state, &user_shell.id, KillOrigin::Unspecified);
        let _ = crate::terminal::kill_session(&state, &child_a, KillOrigin::Unspecified);
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

        let _ = crate::terminal::kill_session(&state, &child, KillOrigin::Unspecified);
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

        let _ = crate::terminal::kill_session(&state, &child, KillOrigin::Unspecified);
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
        // Output is parsed off the delivery path: wait for the screen before
        // looking at it directly (a `Read` does this itself).
        assert!(
            state
                .orchestration
                .sync_screen(
                    child,
                    crate::test_support::coverage_relaxed(Duration::from_secs(20))
                )
                .caught_up
        );

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

        let _ = crate::terminal::kill_session(&state, &child, KillOrigin::Unspecified);
    }

    /// A shell child that prints agy's permission dialog, driven only through
    /// `execute`. The command is typed with printf escapes, so the echoed input
    /// line never starts with the marker and only the output can raise it.
    #[cfg(unix)]
    #[test]
    fn a_shown_agy_permission_dialog_ends_a_wait_as_needs_input_and_input_clears_it() {
        let (_dir, repo) = init_trusted_repo();
        let state = TerminalSessions::default();
        let ui = TestUiSink::default();
        let parent = "term-90007-1";
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
                title: Some("agy-dialog".into()),
                split: Some(false),
                rows: Some(24),
                cols: Some(100),
            },
        )
        .expect("spawn child");
        let child = spawned["child_session_id"].as_str().unwrap().to_string();
        let call = |call: OrchestrateCall| execute(&state, &ui, call).expect("call");
        let wait = |ms: u64| {
            call(OrchestrateCall::Wait {
                repo_path: repo.clone(),
                caller_session_id: Some(parent.into()),
                caller_token: Some(token.clone()),
                external_caller_id: None,
                child_session_id: child.clone(),
                timeout_ms: Some(ms),
                pattern: None,
                wait_for_idle: Some(true),
                since_seq: None,
            })
        };
        let read = || {
            call(OrchestrateCall::Read {
                repo_path: repo.clone(),
                caller_session_id: Some(parent.into()),
                caller_token: Some(token.clone()),
                external_caller_id: None,
                child_session_id: child.clone(),
                max_lines: None,
                offset_lines: None,
                grep: None,
                last_command_only: None,
            })
        };
        // Let the shell's own startup marks settle so the dialog is the news.
        let _ = wait(1_500);
        call(OrchestrateCall::Send {
            repo_path: repo.clone(),
            caller_session_id: Some(parent.into()),
            caller_token: Some(token.clone()),
            external_caller_id: None,
            child_session_id: child.clone(),
            input: r#"printf 'Requesting permission for:\r\n   git status\r\n\r\nRun this command?\r\n> 1. Yes, run command\r\n  4. No, cancel\r\n'; sleep 30"#
                .into(),
            append_newline: Some(true),
        });
        let first = wait(8_000);
        assert_eq!(
            first["matched_reason"], "needs_input",
            "{first}\nscreen: {}", read()["text"]
        );
        assert_eq!(first["agent_prompt"]["kind"], "permission", "{first}");
        assert!(
            first["agent_prompt"]["detail"]
                .as_str()
                .is_some_and(|d| d.contains("git status")),
            "{first}"
        );
        let seen = read();
        assert_eq!(seen["agent_prompt"]["kind"], "permission", "{seen}");

        // The user answers in the tab: the question is no longer open.
        call(OrchestrateCall::Send {
            repo_path: repo.clone(),
            caller_session_id: Some(parent.into()),
            caller_token: Some(token.clone()),
            external_caller_id: None,
            child_session_id: child.clone(),
            input: "1".into(),
            append_newline: Some(true),
        });
        let after = read();
        assert!(after["agent_prompt"].is_null(), "{after}");

        let _ = crate::terminal::kill_session(&state, &child, KillOrigin::Unspecified);
    }

    #[derive(Default)]
    struct RecordedChildStates(Mutex<Vec<MaviChildStatePayload>>);

    impl ChildStateEmitter for RecordedChildStates {
        fn mavi_child_state(&self, payload: MaviChildStatePayload) {
            self.0.lock().unwrap().push(payload);
        }
    }

    fn registry_with_child(
        repo: &str,
        parent: &str,
        child: &str,
    ) -> (OrchestrationRegistry, Arc<RecordedChildStates>) {
        let registry = OrchestrationRegistry::default();
        let seen = Arc::new(RecordedChildStates::default());
        registry.set_child_state_emitter(seen.clone());
        let token = mint_session_token().unwrap();
        registry
            .register_session(
                child,
                &token,
                Path::new(repo),
                Path::new(repo),
                "shell",
                24,
                100,
                crate::terminal::new_shared_telemetry(),
                Some(ChildSpawnMeta {
                    parent_session_id: parent.into(),
                    launcher: "shell".into(),
                    title: Some("audit".into()),
                }),
            )
            .unwrap();
        (registry, seen)
    }

    #[test]
    fn a_mavi_childs_end_is_announced_once_as_done_or_ended() {
        let (_dir, repo) = init_trusted_repo();
        let thread = "0f8e2b9a-1c3d-4e5f-8a6b-7c8d9e0f1a2b";
        let owner = mavi_owner_key(thread).unwrap();
        for (code, state) in [(Some(0), "done"), (Some(2), "ended"), (None, "ended")] {
            let (registry, seen) = registry_with_child(&repo, &owner, "term-91000-1");
            registry.mark_ended("term-91000-1", code);
            registry.mark_ended("term-91000-1", code);
            let seen = seen.0.lock().unwrap();
            assert_eq!(seen.len(), 1, "{seen:?}");
            assert_eq!(seen[0].thread_id, thread);
            assert_eq!(seen[0].child_session_id, "term-91000-1");
            assert_eq!(seen[0].title.as_deref(), Some("audit"));
            assert_eq!((seen[0].state.as_str(), seen[0].exit_code), (state, code));
            let wire = serde_json::to_value(&seen[0]).unwrap();
            assert_eq!(wire["thread_id"], thread);
        }
    }

    #[test]
    fn only_mavi_owned_children_are_announced() {
        let (_dir, repo) = init_trusted_repo();
        for parent in ["term-90001-1", "ext-4242", "ext-mavi-not-a-uuid"] {
            let (registry, seen) = registry_with_child(&repo, parent, "term-91001-1");
            registry.mark_ended("term-91001-1", Some(0));
            assert!(seen.0.lock().unwrap().is_empty(), "{parent}");
        }
    }

    #[cfg(unix)]
    #[test]
    fn an_open_dialog_is_announced_cleared_by_input_not_by_navigation_and_dropped_at_exit() {
        let (_dir, repo) = init_trusted_repo();
        let owner = mavi_owner_key("0f8e2b9a-1c3d-4e5f-8a6b-7c8d9e0f1a2b").unwrap();
        let child = "term-91002-1";
        let (registry, seen) = registry_with_child(&repo, &owner, child);
        let telemetry = registry.telemetry_for(child).unwrap();
        let open = |detail: &str| {
            registry.note_screen_notice(&crate::alerts::bridge::screen_prompt_notice(
                child,
                "Antigravity",
                None,
                "permission_prompt",
                Some(detail.to_owned()),
            ));
        };
        open("git status");
        {
            let seen = seen.0.lock().unwrap();
            assert_eq!(seen.len(), 1, "{seen:?}");
            assert_eq!(seen[0].state, "needs_input");
            assert_eq!(seen[0].detail.as_deref(), Some("git status"));
        }
        // The host repeating the same dialog is the same dialog.
        open("git status");
        assert_eq!(seen.0.lock().unwrap().len(), 1);
        // Moving the highlight answers nothing; typing does.
        registry.note_input(child, b"\x1b[B\x1b[A");
        assert!(telemetry.0.lock().unwrap().agent_prompt.is_some());
        registry.note_input(child, b"1\r");
        assert!(telemetry.0.lock().unwrap().agent_prompt.is_none());
        assert_eq!(seen.0.lock().unwrap().last().unwrap().state, "cleared");
        // A second answer with nothing open announces nothing.
        let count = seen.0.lock().unwrap().len();
        registry.note_input(child, b"\r");
        assert_eq!(seen.0.lock().unwrap().len(), count);

        // A hook saying the agent works again closes it too.
        open("git log");
        assert!(telemetry.0.lock().unwrap().agent_prompt.is_some());
        registry.note_hook_resolution(child);
        assert!(telemetry.0.lock().unwrap().agent_prompt.is_none());

        // And so does the session ending, which says `ended`, never `cleared`.
        open("git diff");
        let before = seen.0.lock().unwrap().len();
        registry.mark_ended(child, Some(1));
        assert!(telemetry.0.lock().unwrap().agent_prompt.is_none());
        let seen = seen.0.lock().unwrap();
        assert_eq!(seen.len(), before + 1);
        assert_eq!(seen.last().unwrap().state, "ended");
    }

    #[cfg(unix)]
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
            "the ninth child was not refused for the 8-session limit"
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

        let _ = crate::terminal::kill_session(&state, &user_shell.id, KillOrigin::Unspecified);
        let _ = crate::terminal::kill_session(&state, &replacement_id, KillOrigin::Unspecified);
        for cid in children_a.iter().skip(1) {
            let _ = crate::terminal::kill_session(&state, cid, KillOrigin::Unspecified);
        }
    }

    /// Before: only the UI's spawn command wrote a ledger row, so an agent
    /// started by another agent or over MCP could never be offered for reopen.
    #[cfg(unix)]
    #[test]
    fn an_orchestrated_agent_child_is_recorded_with_its_parent_row() {
        use std::os::unix::fs::PermissionsExt;
        let (_dir, repo) = init_trusted_repo();
        let state = TerminalSessions::default();
        let parent = "term-90011-1";
        state
            .orchestration
            .register_test_parent(parent, Path::new(&repo));
        let ledger = state.session_ledger();
        let parent_row = ledger.record_spawn(crate::terminal::session_ledger::NewSessionRow {
            terminal_id: parent.into(),
            launcher: "claude".into(),
            native_id: None,
            repo_path: repo.clone(),
            start_dir: None,
            cwd: repo.clone(),
            mode: None,
            reopen_row_id: None,
            parent_row_id: None,
        });
        // A stand-in named like the agent CLI, so the launcher is `claude`
        // without starting the real one.
        let bin = tempfile::tempdir().unwrap();
        let fake = bin.path().join("claude");
        std::fs::write(&fake, "#!/bin/sh\nsleep 30\n").unwrap();
        std::fs::set_permissions(&fake, std::fs::Permissions::from_mode(0o755)).unwrap();
        let child = crate::terminal::spawn_at(
            &state,
            Place::Repo {
                repo_path: repo.clone(),
                start_dir: None,
            },
            24,
            80,
            Some(fake.to_string_lossy().into_owned()),
            None,
            None,
            discard_sink(),
            Arc::new(RecordingEvents),
            None,
            Some(ChildSpawnMeta {
                parent_session_id: parent.into(),
                launcher: "claude".into(),
                title: None,
            }),
        )
        .expect("agent child");
        assert!(ledger.row_id_for_terminal(&child.id).is_some());
        ledger.mark_app_quit();
        let offers = ledger.view().offers;
        let offer = offers
            .iter()
            .find(|o| o.terminal_id.as_deref() == Some(child.id.as_str()))
            .expect("the child is offered for reopen");
        assert_eq!(offer.launcher, "claude");
        assert_eq!(offer.parent_row_id.as_deref(), Some(parent_row.as_str()));
        let _ = crate::terminal::kill_session(&state, &child.id, KillOrigin::Unspecified);
    }

    /// An MCP/CLI `Spawn` goes through `execute`; its shell child is recorded
    /// too, and a shell is never resumable.
    #[cfg(unix)]
    #[test]
    fn a_spawned_shell_child_is_recorded_and_never_resumable() {
        let (_dir, repo) = init_trusted_repo();
        let state = TerminalSessions::default();
        let ui = TestUiSink::default();
        let parent = "term-90012-1";
        let token = state
            .orchestration
            .register_test_parent(parent, Path::new(&repo));
        let spawned = execute(
            &state,
            &ui,
            OrchestrateCall::Spawn {
                repo_path: repo.clone(),
                caller_session_id: Some(parent.into()),
                caller_token: Some(token),
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
        .expect("spawn child");
        let child = spawned["child_session_id"].as_str().unwrap().to_string();
        let ledger = state.session_ledger();
        assert!(ledger.row_id_for_terminal(&child).is_some());
        ledger.mark_app_quit();
        let offers = ledger.view().offers;
        let offer = offers
            .iter()
            .find(|o| o.terminal_id.as_deref() == Some(child.as_str()))
            .expect("listed");
        assert!(!offer.resumable);
        let _ = crate::terminal::kill_session(&state, &child, KillOrigin::Unspecified);
    }

    /// Before: an agent's close (`gitpulse_terminal_close`, `term close`) always
    /// ended the child, whatever the user had asked to keep.
    #[cfg(unix)]
    #[test]
    fn an_agent_close_skips_a_protected_child_and_says_why() {
        let (_dir, repo) = init_trusted_repo();
        let state = TerminalSessions::default();
        let ui = TestUiSink::default();
        let parent = "term-90007-1";
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
                title: Some("kept".into()),
                split: Some(false),
                rows: Some(24),
                cols: Some(80),
            },
        )
        .expect("spawn child");
        let child = spawned["child_session_id"].as_str().unwrap().to_string();
        // The spawn owner recorded the child; nothing here does.
        let ledger = state.session_ledger();
        assert!(ledger.set_protected(&child, true));
        let close = || {
            execute(
                &state,
                &ui,
                OrchestrateCall::Close {
                    repo_path: repo.clone(),
                    caller_session_id: Some(parent.into()),
                    caller_token: Some(token.clone()),
                    external_caller_id: None,
                    child_session_id: child.clone(),
                },
            )
        };
        let refusal = close().unwrap_err();
        // The message says what is missing; the refusal names the repository
        // path and the session, which stay out of the test log.
        assert!(
            refusal.contains("is protected") && refusal.contains(&child),
            "the refusal does not say the child session is protected"
        );
        assert!(
            ui.closed.lock().unwrap().is_empty(),
            "no child_closed event"
        );
        assert!(crate::terminal::list_sessions(&state)
            .unwrap()
            .iter()
            .any(|s| s.id == child && !s.ending));
        assert!(ledger.set_protected(&child, false));
        assert_eq!(close().expect("close once unprotected")["ok"], true);
    }

    /// A pid no process can have: past the kernel's pid ceiling on every
    /// supported system, so `kill(pid, 0)` answers ESRCH.
    const DEAD_PID_OWNER: &str = "ext-2000000000";
    const THREAD_A: &str = "0f8e2b9a-1c3d-4e5f-8a6b-7c8d9e0f1a2b";
    const THREAD_B: &str = "7a1b2c3d-4e5f-4a6b-8c7d-9e0f1a2b3c4d";

    /// A child with no PTY, owned by `owner`, for tests about ownership alone.
    fn register_fake_child(state: &TerminalSessions, repo: &str, id: &str, owner: &str) {
        state
            .orchestration
            .register_session(
                id,
                &mint_session_token().unwrap(),
                Path::new(repo),
                Path::new(repo),
                "shell",
                24,
                80,
                crate::terminal::new_shared_telemetry(),
                Some(ChildSpawnMeta {
                    parent_session_id: owner.into(),
                    launcher: "shell".into(),
                    title: None,
                }),
            )
            .unwrap();
    }

    fn external_spawn(repo: &str, owner: &str) -> OrchestrateCall {
        OrchestrateCall::Spawn {
            repo_path: repo.into(),
            caller_session_id: None,
            caller_token: None,
            external_caller_id: Some(owner.into()),
            launcher: Some("shell".into()),
            cwd: None,
            command: None,
            prompt: None,
            title: None,
            split: Some(false),
            rows: Some(24),
            cols: Some(80),
        }
    }

    /// Before: sixteen children of owners whose process was gone filled the
    /// external cap for good, so every later external spawn was refused.
    #[test]
    fn children_of_a_dead_owner_do_not_fill_the_external_cap() {
        let (_dir, repo) = init_trusted_repo();
        let state = TerminalSessions::default();
        let ui = TestUiSink::default();
        for n in 0..MAX_EXTERNAL_ORCHESTRATED_CHILDREN {
            register_fake_child(&state, &repo, &format!("term-91001-{n}"), DEAD_PID_OWNER);
        }
        let live_owner = format!("mcp-{}", std::process::id());
        let spawned = execute(&state, &ui, external_spawn(&repo, &live_owner))
            .expect("a dead owner's children must not count toward the cap");
        let child = spawned["child_session_id"].as_str().unwrap().to_string();
        let _ = crate::terminal::kill_session(&state, &child, KillOrigin::Unspecified);
    }

    /// A live owner's children still count, so the cap is not simply gone.
    #[test]
    fn children_of_a_live_owner_still_fill_the_external_cap() {
        let (_dir, repo) = init_trusted_repo();
        let state = TerminalSessions::default();
        let ui = TestUiSink::default();
        let live_owner = format!("mcp-{}", std::process::id());
        for n in 0..MAX_EXTERNAL_ORCHESTRATED_CHILDREN {
            register_fake_child(&state, &repo, &format!("term-91002-{n}"), &live_owner);
        }
        let err = execute(&state, &ui, external_spawn(&repo, "ext-mavi-0f8e2b9a-1c3d-4e5f-8a6b-7c8d9e0f1a2b"))
            .unwrap_err();
        assert!(err.contains("external terminal orchestrators already have"), "{err}");
    }

    /// Before: nothing said a child's owner was gone, and nothing kept the
    /// rest of the world from reaching a dead owner's children.
    #[test]
    fn a_dead_owners_children_are_marked_orphaned_and_unreachable_to_others() {
        let (_dir, repo) = init_trusted_repo();
        let state = TerminalSessions::default();
        let ui = TestUiSink::default();
        let live_owner = format!("mcp-{}", std::process::id());
        register_fake_child(&state, &repo, "term-91003-1", DEAD_PID_OWNER);
        register_fake_child(&state, &repo, "term-91003-2", &live_owner);
        let list = |owner: &str| {
            execute(
                &state,
                &ui,
                OrchestrateCall::List {
                    repo_path: repo.clone(),
                    caller_session_id: None,
                    caller_token: None,
                    external_caller_id: Some(owner.into()),
                    include_repo_shells: None,
                },
            )
            .expect("list")
        };
        let dead = list(DEAD_PID_OWNER);
        assert_eq!(dead["children"][0]["child_session_id"], "term-91003-1");
        assert_eq!(dead["children"][0]["orphaned"], true, "{dead}");
        let live = list(&live_owner);
        assert_eq!(live["total"], 1, "a live owner sees only its own: {live}");
        assert_eq!(live["children"][0]["orphaned"], false, "{live}");
        let refusal = execute(
            &state,
            &ui,
            OrchestrateCall::Close {
                repo_path: repo.clone(),
                caller_session_id: None,
                caller_token: None,
                external_caller_id: Some(live_owner),
                child_session_id: "term-91003-1".into(),
            },
        )
        .unwrap_err();
        assert_eq!(refusal, LINEAGE_REFUSAL);
    }

    /// New entry point: closing a forgotten thread's children closes its own,
    /// leaves another thread's alone, and leaves a protected one running.
    #[cfg(unix)]
    #[test]
    fn closing_a_threads_children_skips_protected_and_other_threads() {
        let (_dir, repo) = init_trusted_repo();
        let state = TerminalSessions::default();
        let ui = TestUiSink::default();
        let owner_a = mavi_owner_key(THREAD_A).unwrap();
        let owner_b = mavi_owner_key(THREAD_B).unwrap();
        let spawn = |owner: &str| {
            let out = execute(&state, &ui, external_spawn(&repo, owner)).expect("spawn");
            out["child_session_id"].as_str().unwrap().to_string()
        };
        let (a1, a2, a3) = (spawn(&owner_a), spawn(&owner_a), spawn(&owner_a));
        let b1 = spawn(&owner_b);
        assert!(state.session_ledger().set_protected(&a3, true));

        let summary = close_owner_children(&state, &ui, &owner_a).expect("close thread children");
        assert_eq!((summary.closed, summary.skipped_protected), (2, 1));
        let live: Vec<String> = crate::terminal::list_sessions(&state)
            .unwrap()
            .into_iter()
            .filter(|s| !s.ending)
            .map(|s| s.id)
            .collect();
        assert!(!live.contains(&a1) && !live.contains(&a2), "{live:?}");
        assert!(live.contains(&a3), "a protected child stays: {live:?}");
        assert!(live.contains(&b1), "another thread's child stays: {live:?}");
        assert_eq!(ui.closed.lock().unwrap().len(), 2);

        // Not a Mavi owner key: refused, nothing closed.
        assert!(close_owner_children(&state, &ui, "mcp-1234").is_err());
        assert!(live.contains(&b1));
        for id in [&a3, &b1] {
            let _ = crate::terminal::kill_session(&state, id, KillOrigin::Unspecified);
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
        assert!(read_on["text"]
            .as_str()
            .unwrap()
            .contains("USER_SHELL_OUTPUT_55"));

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

        let _ = crate::terminal::kill_session(&state, &user_shell.id, KillOrigin::Unspecified);
    }

    /// A tracked shell child of a synthetic parent, in `state`.
    fn tracked_child(state: &TerminalSessions, repo: &Path, child: &str) {
        let parent = format!("{child}-parent");
        let _ = state.orchestration.register_test_parent(&parent, repo);
        let token = mint_session_token().unwrap();
        state
            .orchestration
            .register_session(
                child,
                &token,
                repo,
                repo,
                "shell",
                28,
                120,
                crate::terminal::new_shared_telemetry(),
                Some(ChildSpawnMeta {
                    parent_session_id: parent,
                    launcher: "shell".into(),
                    title: None,
                }),
            )
            .unwrap();
    }

    /// Delivery hands output to the screen worker and returns: it does not wait
    /// for the registry lock a reader holds, nor for the parse. With the lock
    /// held the worker cannot apply anything, so a flood far past the bound is
    /// a gap the screen reports, queued bytes stay within the bound, and the
    /// reply status says the screen is behind and incomplete.
    #[test]
    fn delivery_never_waits_on_the_screen_and_a_flood_past_the_bound_is_a_reported_gap() {
        use crate::terminal::screen_feed::{BATCH_BYTES, SESSION_BACKLOG_BYTES};
        let state = Arc::new(TerminalSessions::default());
        let dir = tempfile::tempdir().unwrap();
        let repo = dir.path().canonicalize().unwrap();
        let child = "term-200-b";
        tracked_child(&state, &repo, child);

        let mut flood = vec![b'f'; 64 * 1024];
        let last = flood.len() - 16;
        flood[last..].copy_from_slice(b"\r\nTAIL_MARKER\r\n!");
        let pushes = 256; // 16 MiB against a 1 MiB bound
        let held = state.orchestration.lock();
        let (tx, rx) = std::sync::mpsc::channel();
        let pusher = {
            let state = state.clone();
            crate::supervisor::spawn_once("screen-flood-test", "screen flood test", move || {
                for _ in 0..pushes {
                    state.orchestration.observe_output(child, &flood);
                }
                let _ = tx.send(());
            })
            .unwrap()
        };
        assert!(
            rx.recv_timeout(crate::test_support::coverage_relaxed(Duration::from_secs(
                20
            )))
            .is_ok(),
            "delivery waited on the registry lock a reader held"
        );
        pusher.join().unwrap();

        let lag = state.orchestration.feed.lag(child).unwrap();
        assert_eq!(lag.delivered, (pushes * 64 * 1024) as u64);
        // At most one taken batch plus the bound is ever held back from the gap.
        assert!(
            lag.gap_bytes + (SESSION_BACKLOG_BYTES + BATCH_BYTES) as u64 >= lag.delivered,
            "memory is bounded, the rest is counted as a gap: {lag:?}"
        );
        assert!(lag.gap_bytes > 0);
        let status = state.orchestration.screen_status(child);
        assert_eq!(status["complete"], false, "{status}");
        assert!(status["lag_bytes"].as_u64().unwrap() > 0, "{status}");
        assert!(status["gap_bytes"].as_u64().unwrap() > 0, "{status}");

        drop(held);
        assert!(
            state
                .orchestration
                .sync_screen(
                    child,
                    crate::test_support::coverage_relaxed(Duration::from_secs(20))
                )
                .caught_up,
            "the screen catches up once the reader lets go"
        );
        let status = state.orchestration.screen_status(child);
        assert_eq!(status["lag_bytes"], 0, "{status}");
        assert_eq!(
            status["complete"], false,
            "a screen rebuilt across a gap never reads as complete: {status}"
        );
        let inner = state.orchestration.lock();
        let (text, ..) = render_redacted_screen(inner.sessions.get(child).unwrap(), 50);
        assert!(
            text.contains("TAIL_MARKER"),
            "the newest output survives: {text}"
        );
    }

    /// Output reaches the screen in the order it was delivered however it was
    /// split into pushes, a reader that waits sees all of it after a flood, and
    /// an exit right behind the last byte does not lose it.
    #[test]
    fn a_reader_sees_every_row_in_order_after_a_flood_and_an_immediate_exit() {
        let state = TerminalSessions::default();
        let dir = tempfile::tempdir().unwrap();
        let repo = dir.path().canonicalize().unwrap();
        let child = "term-201-b";
        tracked_child(&state, &repo, child);

        // 40_000 rows of 12 bytes is under the bound, so nothing can be dropped
        // however far behind the worker is.
        let rows = 40_000usize;
        let stream: String = (1..=rows).map(|n| format!("ROW{n:07}\r\n")).collect();
        let bytes = stream.as_bytes();
        let (mut at, mut size) = (0usize, 1usize);
        while at < bytes.len() {
            let end = (at + size).min(bytes.len());
            state.orchestration.observe_output(child, &bytes[at..end]);
            at = end;
            size = size % 4_099 + 7; // splits rows and escape-free lines anywhere
        }
        state.orchestration.mark_ended(child, Some(0));

        assert!(
            state
                .orchestration
                .sync_screen(
                    child,
                    crate::test_support::coverage_relaxed(Duration::from_secs(20))
                )
                .caught_up
        );
        let status = state.orchestration.screen_status(child);
        assert_eq!(
            status,
            json!({"complete": true, "lag_bytes": 0, "gap_bytes": 0})
        );
        let inner = state.orchestration.lock();
        let rec = inner
            .sessions
            .get(child)
            .expect("an ended shell child is retained");
        let (text, lines, ..) = render_redacted_screen(rec, MAX_READ_LINES);
        assert_eq!(lines, MAX_READ_LINES);
        let seen: Vec<usize> = text
            .lines()
            .map(|l| l.strip_prefix("ROW").unwrap().parse().unwrap())
            .collect();
        assert_eq!(
            *seen.last().unwrap(),
            rows,
            "the final row is on the screen"
        );
        assert!(
            seen.windows(2).all(|w| w[1] == w[0] + 1),
            "rows are consecutive, so in order and none lost"
        );
    }

    /// A command in flight when a gap drops bytes still records its block: the
    /// resync forgets half-read escapes, not the command the recorder is in.
    #[test]
    fn a_command_block_that_straddles_a_gap_is_still_recorded() {
        let state = Arc::new(TerminalSessions::default());
        let dir = tempfile::tempdir().unwrap();
        let repo = dir.path().canonicalize().unwrap();
        let child = "term-203-b";
        tracked_child(&state, &repo, child);
        let patient = crate::test_support::coverage_relaxed(Duration::from_secs(20));
        state
            .orchestration
            .observe_output(child, b"\x1b]133;B\x07slow cmd\r\n\x1b]133;C\x07");
        assert!(state.orchestration.sync_screen(child, patient).caught_up);

        // The command's output overflows the bound while the screen is held.
        let mut flood = vec![b'o'; 64 * 1024];
        flood.extend_from_slice(b"\n");
        let held = state.orchestration.lock();
        let (tx, rx) = std::sync::mpsc::channel();
        let pusher = {
            let state = state.clone();
            crate::supervisor::spawn_once("screen-gap-test", "screen gap test", move || {
                for _ in 0..48 {
                    state.orchestration.observe_output(child, &flood);
                }
                state
                    .orchestration
                    .observe_output(child, b"\x1b]133;D;0;gpexec_gap\x07");
                let _ = tx.send(());
            })
            .unwrap()
        };
        assert!(rx.recv_timeout(patient).is_ok());
        pusher.join().unwrap();
        drop(held);

        assert!(state.orchestration.sync_screen(child, patient).caught_up);
        assert!(state.orchestration.feed.lag(child).unwrap().gap_bytes > 0);
        let inner = state.orchestration.lock();
        let blocks = inner.sessions.get(child).unwrap().blocks.as_ref().unwrap();
        assert!(
            blocks.has_tag("gpexec_gap"),
            "the block that straddled the gap was lost"
        );
    }

    /// Dropping the registry stops its screen worker instead of leaking it, and
    /// forgetting a session leaves no queue behind.
    #[test]
    fn dropping_the_registry_stops_the_screen_worker_and_a_forgotten_session_has_no_queue() {
        let dir = tempfile::tempdir().unwrap();
        let repo = dir.path().canonicalize().unwrap();
        let state = TerminalSessions::default();
        tracked_child(&state, &repo, "term-202-b");
        state
            .orchestration
            .observe_output("term-202-b", b"hello\r\n");
        state.orchestration.remove_aborted("term-202-b");
        assert!(state.orchestration.feed.lag("term-202-b").is_none());
        assert_eq!(state.orchestration.screen_status("term-202-b"), Value::Null);
        let worker_started = state.orchestration.worker.lock().unwrap().is_some();
        assert!(worker_started, "a tracked session starts the worker");

        let (tx, rx) = std::sync::mpsc::channel();
        crate::supervisor::spawn_once("screen-drop-test", "screen drop test", move || {
            drop(state);
            let _ = tx.send(());
        })
        .unwrap();
        assert!(
            rx.recv_timeout(crate::test_support::coverage_relaxed(Duration::from_secs(
                20
            )))
            .is_ok(),
            "dropping the registry did not stop its worker"
        );
    }
}
