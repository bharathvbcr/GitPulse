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

pub const MAX_WAIT_TIMEOUT_MS: u64 = 25_000;
pub const DEFAULT_WAIT_TIMEOUT_MS: u64 = 10_000;
pub const MAX_READ_LINES: usize = 200;
pub const DEFAULT_READ_LINES: usize = 80;
pub const MAX_READ_BYTES: usize = 32 * 1024;
pub const MAX_SEND_BYTES: usize = 16 * 1024;
pub const MAX_PROMPT_BYTES: usize = 8 * 1024;
pub const MAX_PATTERN_BYTES: usize = 512;
pub const MAX_CHILD_TITLE_BYTES: usize = 120;
pub const MAX_CHILDREN_PER_PARENT: usize = 8;
const PARSER_SCROLLBACK_ROWS: usize = MAX_READ_LINES * 2;
const MAX_ENDED_CHILDREN: usize = 32;
const HEX_TOKEN_LEN: usize = 64;
const FULL_TOKEN_LEN: usize = SESSION_TOKEN_PREFIX.len() + HEX_TOKEN_LEN;

const CREDENTIAL_REFUSAL: &str = "invalid or expired terminal session credentials";
const LINEAGE_REFUSAL: &str = "child terminal session is not owned by this caller";

/// Allowed child launchers for `Spawn`.
pub const ALLOWED_CHILD_LAUNCHERS: [&str; 6] =
    ["shell", "claude", "agy", "codex", "manvi", "grok"];

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

/// Abstraction over UI event emission so real runs emit Tauri events while unit
/// tests inspect emitted payloads without constructing a Tauri `AppHandle`.
pub trait OrchestrateUiSink: Send + Sync {
    fn child_spawned(&self, payload: TerminalChildSpawnedPayload);
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
        let (parent_session_id, effective_launcher, title, parser, host_scanner) = match child_meta
        {
            Some(meta) => (
                Some(meta.parent_session_id),
                meta.launcher,
                meta.title,
                Some(vt100::Parser::new(
                    nz_u16(size.rows),
                    nz_u16(size.cols),
                    PARSER_SCROLLBACK_ROWS,
                )),
                Some(crate::alerts::scan::Scanner::new()),
            ),
            None => (None, launcher.to_owned(), None, None, None),
        };
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
            let is_child = snap.parent_session_id.is_some();
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
                    launcher: snap
                        .child_launcher
                        .clone()
                        .unwrap_or_else(|| snap.launcher.clone()),
                    title: snap.child_title.clone(),
                    rows: size.rows,
                    cols: size.cols,
                    ended: snap.telemetry.ended,
                    exit_code: snap.telemetry.last_exit_code,
                    observed_bytes: 0,
                    baseline_event_seq: snap.telemetry.event_seq,
                    parser: is_child.then(|| {
                        vt100::Parser::new(
                            nz_u16(size.rows),
                            nz_u16(size.cols),
                            PARSER_SCROLLBACK_ROWS,
                        )
                    }),
                    host_scanner: is_child.then(crate::alerts::scan::Scanner::new),
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
        if let Some(parser) = record.parser.as_mut() {
            parser.process(bytes);
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
            let Some(parser) = record.parser.as_mut() else {
                return;
            };
            parser.process(bytes);
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
        let is_child = match inner.sessions.get_mut(session_id) {
            Some(record) => {
                record.token_hash = None;
                record.ended = true;
                record.exit_code = exit_code;
                record.parent_session_id.is_some()
            }
            None => return,
        };
        if is_child {
            if !inner.ended_children.iter().any(|id| id == session_id) {
                inner.ended_children.push_back(session_id.to_owned());
            }
            while inner.ended_children.len() > MAX_ENDED_CHILDREN {
                if let Some(evicted) = inner.ended_children.pop_front() {
                    if inner
                        .sessions
                        .get(&evicted)
                        .is_some_and(|rec| rec.ended && rec.parent_session_id.is_some())
                    {
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
        caller_session_id: String,
        caller_token: String,
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
    OpenFile {
        repo_path: String,
        caller_session_id: String,
        caller_token: String,
        path: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        line: Option<u32>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        column: Option<u32>,
    },
    List {
        repo_path: String,
        caller_session_id: String,
        caller_token: String,
    },
    Wait {
        repo_path: String,
        caller_session_id: String,
        caller_token: String,
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
        caller_session_id: String,
        caller_token: String,
        child_session_id: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        max_lines: Option<usize>,
    },
    Send {
        repo_path: String,
        caller_session_id: String,
        caller_token: String,
        child_session_id: String,
        input: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        append_newline: Option<bool>,
    },
}

impl OrchestrateCall {
    /// Socket reply deadline for `Secondary` callers (`timeout_ms + 5s` on `Wait`).
    pub fn reply_deadline(&self) -> Duration {
        match self {
            Self::Wait { timeout_ms, .. } => {
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
        let check_common = |repo: &str, sid: &str, tok: &str| -> Result<(), String> {
            if repo.is_empty()
                || repo.len() > 4096
                || repo.contains('\0')
                || !Path::new(repo).is_absolute()
            {
                return Err("orchestrate repo_path must be an absolute path".into());
            }
            if sid.is_empty() || sid.len() > 64 || sid.chars().any(char::is_control) {
                return Err("orchestrate caller_session_id is invalid".into());
            }
            if tok.is_empty() || tok.len() > 128 || tok.chars().any(char::is_control) {
                return Err("orchestrate caller_token is invalid".into());
            }
            Ok(())
        };
        let check_child = |cid: &str| -> Result<(), String> {
            if cid.is_empty() || cid.len() > 64 || cid.chars().any(char::is_control) {
                return Err("orchestrate child_session_id is invalid".into());
            }
            Ok(())
        };

        match self {
            Self::Spawn {
                repo_path,
                caller_session_id,
                caller_token,
                launcher,
                cwd,
                command,
                prompt,
                title,
                ..
            } => {
                check_common(repo_path, caller_session_id, caller_token)?;
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
                        return Err("orchestrate title exceeds 120 bytes or has control chars".into());
                    }
                }
                Ok(())
            }
            Self::OpenFile {
                repo_path,
                caller_session_id,
                caller_token,
                path,
                line,
                column,
            } => {
                check_common(repo_path, caller_session_id, caller_token)?;
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
            } => check_common(repo_path, caller_session_id, caller_token),
            Self::Wait {
                repo_path,
                caller_session_id,
                caller_token,
                child_session_id,
                timeout_ms,
                pattern,
                ..
            } => {
                check_common(repo_path, caller_session_id, caller_token)?;
                check_child(child_session_id)?;
                if timeout_ms.is_some_and(|ms| ms == 0 || ms > MAX_WAIT_TIMEOUT_MS) {
                    return Err(format!(
                        "orchestrate timeout_ms must be in 1..={MAX_WAIT_TIMEOUT_MS}"
                    ));
                }
                if let Some(pat) = pattern {
                    if pat.is_empty() || pat.len() > MAX_PATTERN_BYTES || pat.contains('\0') {
                        return Err("orchestrate pattern is empty or exceeds 512 bytes".into());
                    }
                }
                Ok(())
            }
            Self::Read {
                repo_path,
                caller_session_id,
                caller_token,
                child_session_id,
                max_lines,
            } => {
                check_common(repo_path, caller_session_id, caller_token)?;
                check_child(child_session_id)?;
                if max_lines.is_some_and(|n| n == 0 || n > MAX_READ_LINES) {
                    return Err(format!(
                        "orchestrate max_lines must be in 1..={MAX_READ_LINES}"
                    ));
                }
                Ok(())
            }
            Self::Send {
                repo_path,
                caller_session_id,
                caller_token,
                child_session_id,
                input,
                ..
            } => {
                check_common(repo_path, caller_session_id, caller_token)?;
                check_child(child_session_id)?;
                if input.is_empty() || input.len() > MAX_SEND_BYTES {
                    return Err("orchestrate input is empty or exceeds 16 KiB".into());
                }
                Ok(())
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
                return Err(
                    "Ctrl+Z suspend (0x1a) is not permitted in child terminal input".into(),
                )
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

fn authorize_caller(
    registry: &OrchestrationRegistry,
    caller_session_id: &str,
    caller_token: &str,
    canonical_repo: &str,
) -> Result<(), String> {
    if !is_session_id(caller_session_id) {
        return Err(CREDENTIAL_REFUSAL.to_string());
    }
    let provided = hash_token(caller_token)?;
    let inner = registry.lock();
    let Some(record) = inner.sessions.get(caller_session_id) else {
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
    Ok(())
}

fn authorize_child<'a>(
    inner: &'a RegistryInner,
    caller_session_id: &str,
    child_session_id: &str,
    canonical_repo: &str,
) -> Result<&'a OrchestratedRecord, String> {
    if !is_session_id(child_session_id) || child_session_id == caller_session_id {
        return Err(LINEAGE_REFUSAL.to_string());
    }
    let Some(record) = inner.sessions.get(child_session_id) else {
        return Err(LINEAGE_REFUSAL.to_string());
    };
    if record.parent_session_id.as_deref() != Some(caller_session_id)
        || record.repo_path != canonical_repo
    {
        return Err(LINEAGE_REFUSAL.to_string());
    }
    Ok(record)
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
    let Some(host) = &state.host else {
        return;
    };
    let (dims, seen_bytes) = {
        let inner = state.orchestration.lock();
        let Some(rec) = inner.sessions.get(child_session_id) else {
            return;
        };
        if rec.parent_session_id.is_none() {
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
                        if let Some(parser) = rec.parser.as_mut() {
                            parser.process(suffix);
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

fn render_redacted_screen(record: &OrchestratedRecord, max_lines: usize) -> (String, usize, usize, bool) {
    let mut logical_lines: Vec<String> = Vec::new();
    if let Some(parser) = record.parser.as_ref() {
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
    }

    while logical_lines
        .last()
        .is_some_and(|line| line.trim().is_empty())
    {
        logical_lines.pop();
    }
    let total_lines = logical_lines.len();
    let cap = max_lines.clamp(1, MAX_READ_LINES);
    let mut truncated = total_lines > cap;
    let slice = if total_lines > cap {
        &logical_lines[total_lines - cap..]
    } else {
        &logical_lines[..]
    };
    let joined = slice.join("\n");
    let mut scrubbed = scrub_screen_text(&joined);
    if scrubbed.len() > MAX_READ_BYTES {
        truncated = true;
        let mut cut = scrubbed.len().saturating_sub(MAX_READ_BYTES);
        while cut < scrubbed.len() && !scrubbed.is_char_boundary(cut) {
            cut += 1;
        }
        scrubbed = scrubbed[cut..].to_string();
    }
    let lines_returned = if scrubbed.is_empty() {
        0
    } else {
        scrubbed.lines().count()
    };
    (scrubbed, lines_returned, total_lines, truncated)
}

fn herdr_state_str(status: &Option<crate::alerts::scan::HerdrStatus>) -> Option<String> {
    status.as_ref().map(|h| h.state.as_str().to_owned())
}

/// Executes one [`OrchestrateCall`] against `state` under Option A lineage rules.
pub fn execute(
    state: &TerminalSessions,
    ui: &dyn OrchestrateUiSink,
    call: OrchestrateCall,
) -> Result<Value, String> {
    call.validate()?;
    match call {
        OrchestrateCall::Spawn {
            repo_path,
            caller_session_id,
            caller_token,
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
            authorize_caller(
                &state.orchestration,
                &caller_session_id,
                &caller_token,
                &canonical_repo,
            )?;
            {
                let inner = state.orchestration.lock();
                let active_children = inner
                    .sessions
                    .values()
                    .filter(|rec| {
                        !rec.ended
                            && rec.parent_session_id.as_deref() == Some(caller_session_id.as_str())
                    })
                    .count();
                if active_children >= MAX_CHILDREN_PER_PARENT {
                    return Err(format!(
                        "parent terminal session already has {active_children} active child sessions (limit {MAX_CHILDREN_PER_PARENT}); wait for or close an existing child before spawning another"
                    ));
                }
            }

            let launcher = launcher
                .as_deref()
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .unwrap_or("shell")
                .to_owned();
            if !ALLOWED_CHILD_LAUNCHERS.contains(&launcher.as_str()) {
                return Err(format!("unsupported child launcher '{launcher}'"));
            }

            let start_dir = cwd
                .as_deref()
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .map(str::to_owned);
            let resolved_cwd = match &start_dir {
                Some(sub) => sandbox_join_canonical(&repo_buf, sub)?,
                None => repo_buf.clone(),
            };
            if !resolved_cwd.is_dir() {
                return Err("requested child cwd is not an existing directory inside repo_path".into());
            }
            let resolved_cwd_str = resolved_cwd.to_string_lossy().into_owned();

            let (program, args, initial_shell_cmd, gate_checked, gate_notice) =
                if launcher == "shell" {
                    if prompt.is_some() {
                        return Err(
                            "prompt applies only to agent launchers; pass command for shell".into(),
                        );
                    }
                    if let Some(cmd) = command {
                        validate_child_input_bytes(&cmd)?;
                        let (checked, notice) = gate_lines(&resolved_cwd_str, &cmd)?;
                        (None, None, Some(cmd), checked, notice)
                    } else {
                        (None, None, None, true, None)
                    }
                } else {
                    if command.is_some() {
                        return Err(
                            "command applies only to shell launcher; pass prompt for agent launchers"
                                .into(),
                        );
                    }
                    let agent_args = build_agent_child_args(&launcher, prompt.as_deref())?;
                    (Some(launcher.clone()), agent_args, None, true, None)
                };

            let clean_title = title
                .map(|t| t.trim().to_owned())
                .filter(|t| !t.is_empty());
            let want_split = split.unwrap_or(true);
            let pty_rows = rows.unwrap_or(28);
            let pty_cols = cols.unwrap_or(100);
            let events = ui.session_events(state);

            let spawned = spawn_at(
                state,
                Place::Repo {
                    repo_path: canonical_repo.clone(),
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
                    parent_session_id: caller_session_id.clone(),
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
                parent_session_id: caller_session_id,
                repo_path: canonical_repo.clone(),
                cwd: spawned.cwd.clone(),
                shell: spawned.shell.clone(),
                launcher: launcher.clone(),
                title: clean_title,
                split: want_split,
            });

            Ok(json!({
                "ok": true,
                "repo_path": canonical_repo,
                "child_session_id": spawned.id,
                "launcher": launcher,
                "cwd": spawned.cwd,
                "split": want_split,
                "gate_checked": gate_checked,
                "gate_notice": gate_notice,
            }))
        }

        OrchestrateCall::OpenFile {
            repo_path,
            caller_session_id,
            caller_token,
            path,
            line,
            column,
        } => {
            let repo_buf = canonical_trusted_repo(&repo_path)?;
            let canonical_repo = repo_buf.to_string_lossy().into_owned();
            authorize_caller(
                &state.orchestration,
                &caller_session_id,
                &caller_token,
                &canonical_repo,
            )?;

            let trimmed = path.trim().trim_start_matches("./");
            let joined = sandbox_join_canonical(&repo_buf, trimmed)?;
            if !joined.is_file() {
                return Err(format!("'{}' is not an existing file in the repository", trimmed));
            }
            let rel = joined
                .strip_prefix(&repo_buf)
                .map_err(|_| "resolved file escaped repository root".to_string())?
                .components()
                .map(|c| c.as_os_str().to_string_lossy())
                .collect::<Vec<_>>()
                .join("/");

            ui.open_file_requested(TerminalOpenFileRequestPayload {
                caller_session_id,
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
        } => {
            let repo_buf = canonical_trusted_repo(&repo_path)?;
            let canonical_repo = repo_buf.to_string_lossy().into_owned();
            authorize_caller(
                &state.orchestration,
                &caller_session_id,
                &caller_token,
                &canonical_repo,
            )?;

            let mut children: Vec<(String, Value)> = {
                let inner = state.orchestration.lock();
                inner
                    .sessions
                    .iter()
                    .filter(|(_, rec)| {
                        rec.parent_session_id.as_deref() == Some(caller_session_id.as_str())
                            && rec.repo_path == canonical_repo
                    })
                    .map(|(cid, rec)| {
                        let tel = rec
                            .telemetry
                            .0
                            .lock()
                            .unwrap_or_else(std::sync::PoisonError::into_inner)
                            .clone();
                        let active_cwd = tel.cwd.unwrap_or_else(|| rec.cwd.clone());
                        let ended = rec.ended || tel.ended;
                        let exit_code = rec.exit_code.or(tel.last_exit_code);
                        (
                            cid.clone(),
                            json!({
                                "child_session_id": cid,
                                "launcher": rec.launcher,
                                "cwd": active_cwd,
                                "title": rec.title,
                                "ended": ended,
                                "exit_code": exit_code,
                                "command_running": tel.command_running,
                                "last_exit_code": tel.last_exit_code,
                                "herdr_status": herdr_state_str(&tel.herdr_status),
                                "dev_server_url": tel.dev_server_url,
                            }),
                        )
                    })
                    .collect()
            };
            children.sort_by(|a, b| a.0.cmp(&b.0));
            let list: Vec<Value> = children.into_iter().map(|(_, v)| v).collect();
            let count = list.len();
            Ok(json!({
                "ok": true,
                "repo_path": canonical_repo,
                "total": count,
                "children": list,
            }))
        }

        OrchestrateCall::Wait {
            repo_path,
            caller_session_id,
            caller_token,
            child_session_id,
            timeout_ms,
            pattern,
            wait_for_idle,
            since_seq,
        } => {
            let repo_buf = canonical_trusted_repo(&repo_path)?;
            let canonical_repo = repo_buf.to_string_lossy().into_owned();
            authorize_caller(
                &state.orchestration,
                &caller_session_id,
                &caller_token,
                &canonical_repo,
            )?;

            let wait_idle = wait_for_idle.unwrap_or(true);
            let bounded_ms = timeout_ms
                .unwrap_or(DEFAULT_WAIT_TIMEOUT_MS)
                .clamp(1, MAX_WAIT_TIMEOUT_MS);
            let deadline = Instant::now() + Duration::from_millis(bounded_ms);

            let effective_since = {
                let inner = state.orchestration.lock();
                let rec = authorize_child(
                    &inner,
                    &caller_session_id,
                    &child_session_id,
                    &canonical_repo,
                )?;
                since_seq.unwrap_or(rec.baseline_event_seq)
            };

            loop {
                #[cfg(unix)]
                refresh_detached_host_screen(state, &child_session_id);

                let mut inner = state.orchestration.lock();
                let rec = authorize_child(
                    &inner,
                    &caller_session_id,
                    &child_session_id,
                    &canonical_repo,
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

                let matched_reason = if let Some(pat) = pattern.as_deref() {
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
                    && matches!(
                        herdr.as_deref(),
                        Some("done" | "idle" | "blocked" | "error")
                    )
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
                    if let Some(rec_mut) = inner.sessions.get_mut(&child_session_id) {
                        rec_mut.baseline_event_seq =
                            rec_mut.baseline_event_seq.max(tel.event_seq);
                    }
                    let matched = pattern
                        .as_deref()
                        .map_or(true, |_| reason == "pattern");
                    return Ok(json!({
                        "ok": true,
                        "child_session_id": child_session_id,
                        "matched": matched,
                        "timed_out": false,
                        "matched_reason": reason,
                        "event_seq": tel.event_seq,
                        "ended": ended,
                        "exit_code": exit_code,
                        "command_running": tel.command_running,
                        "last_exit_code": tel.last_exit_code,
                        "herdr_status": herdr,
                        "dev_server_url": tel.dev_server_url,
                    }));
                }

                let now = Instant::now();
                if now >= deadline {
                    return Ok(json!({
                        "ok": true,
                        "child_session_id": child_session_id,
                        "matched": false,
                        "timed_out": true,
                        "matched_reason": "timeout",
                        "event_seq": tel.event_seq,
                        "ended": ended,
                        "exit_code": exit_code,
                        "command_running": tel.command_running,
                        "last_exit_code": tel.last_exit_code,
                        "herdr_status": herdr,
                        "dev_server_url": tel.dev_server_url,
                    }));
                }

                let wait_slice = (deadline - now).min(Duration::from_millis(50));
                let _ = state
                    .orchestration
                    .changed
                    .wait_timeout(inner, wait_slice)
                    .unwrap_or_else(std::sync::PoisonError::into_inner);
            }
        }

        OrchestrateCall::Read {
            repo_path,
            caller_session_id,
            caller_token,
            child_session_id,
            max_lines,
        } => {
            let repo_buf = canonical_trusted_repo(&repo_path)?;
            let canonical_repo = repo_buf.to_string_lossy().into_owned();
            authorize_caller(
                &state.orchestration,
                &caller_session_id,
                &caller_token,
                &canonical_repo,
            )?;

            #[cfg(unix)]
            refresh_detached_host_screen(state, &child_session_id);

            let inner = state.orchestration.lock();
            let rec = authorize_child(
                &inner,
                &caller_session_id,
                &child_session_id,
                &canonical_repo,
            )?;
            let tel = rec
                .telemetry
                .0
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .clone();
            let (text, lines_returned, total_lines, truncated) =
                render_redacted_screen(rec, max_lines.unwrap_or(DEFAULT_READ_LINES));
            let ended = rec.ended || tel.ended;
            let exit_code = rec.exit_code.or(tel.last_exit_code);
            Ok(json!({
                "ok": true,
                "child_session_id": child_session_id,
                "text": text,
                "lines_returned": lines_returned,
                "total_lines": total_lines,
                "truncated": truncated,
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
            child_session_id,
            input,
            append_newline,
        } => {
            let repo_buf = canonical_trusted_repo(&repo_path)?;
            let canonical_repo = repo_buf.to_string_lossy().into_owned();
            authorize_caller(
                &state.orchestration,
                &caller_session_id,
                &caller_token,
                &canonical_repo,
            )?;
            validate_child_input_bytes(&input)?;

            #[cfg(unix)]
            refresh_detached_host_screen(state, &child_session_id);

            let effective_cwd = {
                let inner = state.orchestration.lock();
                let rec = authorize_child(
                    &inner,
                    &caller_session_id,
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
        pub(crate) opened: Mutex<Vec<TerminalOpenFileRequestPayload>>,
    }

    impl OrchestrateUiSink for TestUiSink {
        fn child_spawned(&self, payload: TerminalChildSpawnedPayload) {
            self.spawned.lock().unwrap().push(payload);
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
        assert!(!ct_eq_32(&hash_token(&a).unwrap(), &hash_token(&b).unwrap()));

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
                caller_session_id: parent_a.into(),
                caller_token: bad_tok,
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
                caller_session_id: parent_a.into(),
                caller_token: token_a.clone(),
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
                caller_session_id: parent_a.into(),
                caller_token: token_a.clone(),
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
                    caller_session_id: parent_a.into(),
                    caller_token: token_a.clone(),
                    child_session_id: forbidden.into(),
                    max_lines: None,
                },
            )
            .unwrap_err();
            assert_eq!(read_err, LINEAGE_REFUSAL, "target {forbidden}");

            let send_err = execute(
                &state,
                &ui,
                OrchestrateCall::Send {
                    repo_path: repo.clone(),
                    caller_session_id: parent_a.into(),
                    caller_token: token_a.clone(),
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
                caller_session_id: parent_b.into(),
                caller_token: token_b,
                child_session_id: child_a.clone(),
                max_lines: None,
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
                caller_session_id: parent_a.into(),
                caller_token: token_a,
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
                caller_session_id: parent.into(),
                caller_token: token.clone(),
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
                caller_session_id: parent.into(),
                caller_token: token.clone(),
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
                caller_session_id: parent.into(),
                caller_token: token.clone(),
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
                    caller_session_id: parent.into(),
                    caller_token: token.clone(),
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
                caller_session_id: parent.into(),
                caller_token: token.clone(),
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
                caller_session_id: parent.into(),
                caller_token: token,
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
                caller_session_id: parent.into(),
                caller_token: token.clone(),
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
        assert_eq!(ui.spawned.lock().unwrap().len(), 1);

        // Print an ANSI carriage-return overwrite + a real secret + the child's
        // own GITPULSE_SESSION_TOKEN, then a completion marker.
        execute(
            &state,
            &ui,
            OrchestrateCall::Send {
                repo_path: repo.clone(),
                caller_session_id: parent.into(),
                caller_token: token.clone(),
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
                caller_session_id: parent.into(),
                caller_token: token.clone(),
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
                caller_session_id: parent.into(),
                caller_token: token,
                child_session_id: child.clone(),
                max_lines: Some(40),
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
        state.orchestration.observe_output(child, payload.as_bytes());

        let inner = state.orchestration.lock();
        let rec = inner.sessions.get(child).unwrap();
        let (text, lines_returned, total_lines, truncated) = render_redacted_screen(rec, 120);
        assert_eq!(total_lines, 120, "expected 120 lines across scrollback + viewport, got {total_lines}");
        assert_eq!(lines_returned, 120);
        assert!(!truncated);
        assert!(text.contains("SCROLL_ROW_001"), "missing earliest scrollback row: {text}");
        assert!(text.contains("SCROLL_ROW_120"), "missing newest viewport row: {text}");
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
                caller_session_id: parent.into(),
                caller_token: token.clone(),
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
                caller_session_id: parent.into(),
                caller_token: token.clone(),
                child_session_id: child.clone(),
                input: r#"printf '\033]7501;status;idle;Ready\007http://%s:4321/\n' localhost"#.into(),
                append_newline: Some(true),
            },
        )
        .expect("send turn 1");

        let first_wait = execute(
            &state,
            &ui,
            OrchestrateCall::Wait {
                repo_path: repo.clone(),
                caller_session_id: parent.into(),
                caller_token: token.clone(),
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
                caller_session_id: parent.into(),
                caller_token: token.clone(),
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
                caller_session_id: parent.into(),
                caller_token: token.clone(),
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
                caller_session_id: parent_id.into(),
                caller_token: parent_token,
            },
        )
        .expect("list adopted children");
        assert_eq!(listed["total"], 1);
        assert_eq!(listed["children"][0]["child_session_id"], child_id);
        assert_eq!(listed["children"][0]["title"], "adopted-child");
    }
}
