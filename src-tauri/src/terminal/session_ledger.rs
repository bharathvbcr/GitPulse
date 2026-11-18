//! Durable per-tab ledger for hosted terminal & agent sessions (S2-03, S2-05).
//!
//! Records one bounded row per spawned interactive tab (`~/.config/gitpulse/terminal-sessions.v1.jsonl`
//! in production, in-memory for unit tests unless constructed with [`SessionLedger::open`])
//! with its launcher, native conversation id, checkout/start directory, permission mode,
//! timestamps, and lifecycle end state:
//! - `running`: tab currently has a live PTY
//! - `exited_clean`: natural exit (`/exit`, `exit 0`) or user-requested Close (never offered)
//! - `exited_signal`: external signal killed the PTY while the app stayed up
//! - `app_quit`: written synchronously in teardown *before* `SIGHUP` so quit+reopen keeps them
//! - `lost`: left `running` by a prior `SIGKILL`, crash, or reboot and reconciled at startup
//! - `unreadable`: corrupt JSONL line on disk, preserved and surfaced as `"unreadable"`

use crate::workbench::{conversation, terminal_command};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::{HashMap, HashSet};
use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Mutex;
use std::time::{SystemTime, UNIX_EPOCH};

/// Maximum rows kept on disk and in memory so the ledger stays strictly bounded.
pub const MAX_LEDGER_ROWS: usize = 64;
/// Maximum byte length accepted for one JSONL line before treating it as corrupt.
const MAX_LINE_BYTES: usize = 16 * 1024;
/// Maximum length retained when preserving a corrupt raw line across unrelated writes.
const MAX_CORRUPT_RAW_BYTES: usize = 4096;
/// Current schema version for JSONL rows.
const LEDGER_SCHEMA_VERSION: u32 = 1;

/// Durable JSONL filename inside GitPulse's config directory.
pub const SESSION_LEDGER_FILE: &str = "terminal-sessions.v1.jsonl";
/// Tauri event emitted whenever the visible recovery offers change.
pub const SESSION_LEDGER_EVENT: &str = "terminal-session-ledger";

static TMP_SEQ: AtomicU64 = AtomicU64::new(0);

/// Lifecycle end state of a recorded terminal tab row.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SessionEndState {
    Running,
    ExitedClean,
    ExitedSignal,
    AppQuit,
    Lost,
    Unreadable,
}

/// Typed resume (or fork-resume) request accepted by `cmd_terminal_spawn` (S2-07, CARD 33).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TerminalResume {
    pub session_id: String,
    #[serde(default)]
    pub fork: bool,
}

/// Recovery row projected to the frontend "Reopen N agents" banner and Health.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SessionLedgerOffer {
    pub row_id: String,
    pub terminal_id: Option<String>,
    pub launcher: String,
    pub native_id: Option<String>,
    pub repo_path: String,
    pub start_dir: Option<String>,
    pub cwd: String,
    pub mode: Option<String>,
    pub started_at: u64,
    pub ended_at: Option<u64>,
    pub end_state: SessionEndState,
    pub signal: Option<String>,
    pub resumable: bool,
    pub reason: Option<String>,
    pub last_error: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
}

/// Current snapshot of durable recovery offers pushed via `SESSION_LEDGER_EVENT`
/// and returned by `cmd_terminal_session_ledger`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SessionLedgerView {
    pub offers: Vec<SessionLedgerOffer>,
    pub reopenable_count: usize,
    pub unreadable_count: usize,
}

/// Result of pre-checking whether one ledger row can be reopened right now.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SessionReopenCheck {
    pub offer: SessionLedgerOffer,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
struct StoredRow {
    v: u32,
    row_id: String,
    terminal_id: Option<String>,
    launcher: String,
    native_id: Option<String>,
    repo_path: String,
    start_dir: Option<String>,
    cwd: String,
    mode: Option<String>,
    started_at: u64,
    ended_at: Option<u64>,
    end_state: SessionEndState,
    signal: Option<String>,
    exit_code: Option<i32>,
    last_error: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    title: Option<String>,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    title_pinned: bool,
    #[serde(default)]
    withdrawn: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum Entry {
    Valid(Box<StoredRow>),
    Corrupt {
        row_id: String,
        raw_line: String,
        detail: String,
    },
}

/// Parameters captured when spawning a hosted terminal tab.
#[derive(Debug, Clone)]
pub struct NewSessionRow {
    pub terminal_id: String,
    pub launcher: String,
    pub native_id: Option<String>,
    pub repo_path: String,
    pub start_dir: Option<String>,
    pub cwd: String,
    pub mode: Option<String>,
    pub reopen_row_id: Option<String>,
}

struct LedgerState {
    entries: Vec<Entry>,
    load_error: Option<String>,
}

/// Thread-safe, bounded durable store for hosted terminal tab recovery rows.
pub struct SessionLedger {
    path: Option<PathBuf>,
    state: Mutex<LedgerState>,
}

impl SessionLedger {
    /// Creates an in-memory-only ledger (used by `TerminalSessions::default`
    /// in isolated unit tests so tests never touch `~/.config/gitpulse`).
    pub fn in_memory() -> Self {
        Self {
            path: None,
            state: Mutex::new(LedgerState {
                entries: Vec::new(),
                load_error: None,
            }),
        }
    }

    /// Opens the ledger at `crate::tool_config::default_config_dir()/terminal-sessions.v1.jsonl`,
    /// falling back to in-memory if no home/config directory exists.
    pub fn for_app() -> Self {
        match crate::tool_config::default_config_dir() {
            Some(dir) => Self::open(dir.join(SESSION_LEDGER_FILE)),
            None => Self::in_memory(),
        }
    }

    /// Opens (or initializes) a durable ledger backed by `path`.
    pub fn open(path: impl Into<PathBuf>) -> Self {
        let path = path.into();
        let (entries, load_error) = match load_entries(&path) {
            Ok(entries) => (entries, None),
            Err(error) => {
                log::warn!(
                    target: "terminal",
                    "session ledger at {} could not be read: {error}",
                    path.display()
                );
                (Vec::new(), Some(error))
            }
        };
        Self {
            path: Some(path),
            state: Mutex::new(LedgerState {
                entries,
                load_error,
            }),
        }
    }

    /// Path of the durable `.jsonl` file, or `None` when in-memory.
    pub fn path(&self) -> Option<&Path> {
        self.path.as_deref()
    }

    /// Records a newly spawned PTY tab (and withdraws `reopen_row_id` if this
    /// spawn is reopening a prior offer). Returns the new `row_id`.
    pub fn record_spawn(&self, spec: NewSessionRow) -> String {
        let row_id = mint_row_id();
        let launcher = normalize_launcher(&spec.launcher);
        let native_id = sanitize_native_id(&launcher, spec.native_id.as_deref());
        let mode = spec
            .mode
            .as_deref()
            .map(str::trim)
            .filter(|m| !m.is_empty())
            .map(str::to_owned);
        let mut guard = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let mut inherited_title: Option<String> = None;
        let mut inherited_pinned = false;
        if let Some(reopen_id) = spec.reopen_row_id.as_deref() {
            for entry in &mut guard.entries {
                if let Entry::Valid(existing) = entry {
                    if existing.row_id == reopen_id {
                        inherited_title = existing.title.clone();
                        inherited_pinned = existing.title_pinned && inherited_title.is_some();
                        existing.withdrawn = true;
                        existing.last_error = None;
                    }
                }
            }
        }
        let row = StoredRow {
            v: LEDGER_SCHEMA_VERSION,
            row_id: row_id.clone(),
            terminal_id: Some(spec.terminal_id),
            launcher,
            native_id,
            repo_path: spec.repo_path,
            start_dir: spec.start_dir.filter(|s| !s.is_empty() && s != "."),
            cwd: spec.cwd,
            mode,
            started_at: now_ms(),
            ended_at: None,
            end_state: SessionEndState::Running,
            signal: None,
            exit_code: None,
            last_error: None,
            title: inherited_title,
            title_pinned: inherited_pinned,
            withdrawn: false,
        };
        guard.entries.push(Entry::Valid(Box::new(row)));
        enforce_bound(&mut guard.entries);
        self.persist_locked(&guard.entries);
        row_id
    }

    /// Updates the stabilized live or user-renamed session title for `terminal_id`.
    /// Returns `(changed_on_disk, offer_view_changed)` so live running updates do
    /// not churn disk when unchanged or emit unnecessary recovery-banner events.
    pub fn set_title(
        &self,
        terminal_id: &str,
        title: Option<&str>,
        user_named: bool,
    ) -> (bool, bool) {
        let normalized = title
            .map(crate::alerts::scan::sanitize_osc_title)
            .filter(|t| !t.is_empty());
        let mut guard = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let mut changed = false;
        let mut offer_changed = false;
        for entry in guard.entries.iter_mut().rev() {
            let Entry::Valid(row) = entry else { continue };
            if row.terminal_id.as_deref() != Some(terminal_id) {
                continue;
            }
            if !user_named && row.title_pinned && row.title.is_some() {
                return (false, false);
            }
            let next_pinned = user_named && normalized.is_some();
            if row.title != normalized || row.title_pinned != next_pinned {
                row.title = normalized;
                row.title_pinned = next_pinned;
                changed = true;
                offer_changed = !row.withdrawn && is_offer_end_state(row.end_state);
            }
            break;
        }
        if changed {
            self.persist_locked(&guard.entries);
        }
        (changed, offer_changed)
    }

    /// Resolves the best known session title for `terminal_id` by combining a
    /// live host `OSC 0`/`OSC 2` title with the durable ledger's stored (or
    /// user-pinned) title, syncing new host titles into the ledger row.
    pub fn resolved_title_for_terminal(
        &self,
        terminal_id: &str,
        live_title: Option<&str>,
    ) -> Option<String> {
        let live_clean = live_title
            .map(crate::alerts::scan::sanitize_osc_title)
            .filter(|t| !t.is_empty());
        let mut guard = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let mut persist = false;
        let mut resolved: Option<String> = None;
        for entry in guard.entries.iter_mut().rev() {
            let Entry::Valid(row) = entry else { continue };
            if row.terminal_id.as_deref() != Some(terminal_id) {
                continue;
            }
            if row.title_pinned && row.title.is_some() {
                resolved = row.title.clone();
            } else if let Some(ref fresh) = live_clean {
                if row.title.as_deref() != Some(fresh.as_str()) {
                    row.title = Some(fresh.clone());
                    persist = true;
                }
                resolved = Some(fresh.clone());
            } else {
                resolved = row.title.clone();
            }
            break;
        }
        if persist {
            self.persist_locked(&guard.entries);
        }
        resolved.or(live_clean)
    }

    /// Associates a validated conversation UUID (from hook `SessionStart` /
    /// `AgentSessionMeta`) with a running or ended tab row.
    pub fn bind_native_id(&self, terminal_id: &str, agent: &str, conversation_id: &str) -> bool {
        if !terminal_command::is_canonical_uuid(conversation_id) {
            return false;
        }
        let normalized_id = conversation_id.trim().to_ascii_lowercase();
        let mut guard = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let mut changed = false;
        for entry in guard.entries.iter_mut().rev() {
            let Entry::Valid(row) = entry else { continue };
            if row.terminal_id.as_deref() != Some(terminal_id) {
                continue;
            }
            if row.launcher == "shell" {
                row.launcher = normalize_launcher(agent);
            }
            if row.native_id.as_deref() != Some(normalized_id.as_str()) {
                row.native_id = Some(normalized_id);
                changed = true;
            }
            break;
        }
        if changed {
            self.persist_locked(&guard.entries);
        }
        changed
    }

    /// Marks a tab closed by explicit user action (`cmd_terminal_kill`) as
    /// cleanly withdrawn so it is never offered for reopen.
    pub fn mark_user_closed(&self, terminal_id: &str) -> bool {
        let mut guard = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let now = now_ms();
        let mut changed = false;
        for entry in guard.entries.iter_mut().rev() {
            let Entry::Valid(row) = entry else { continue };
            if row.terminal_id.as_deref() != Some(terminal_id) {
                continue;
            }
            if !row.withdrawn || row.end_state != SessionEndState::ExitedClean {
                row.end_state = SessionEndState::ExitedClean;
                row.withdrawn = true;
                if row.ended_at.is_none() {
                    row.ended_at = Some(now);
                }
                changed = true;
            }
            break;
        }
        if changed {
            self.persist_locked(&guard.entries);
        }
        changed
    }

    /// Marks a tab GitPulse closed on the person's click to give its memory back
    /// (`sysload::activity::park`) as [`SessionEndState::AppQuit`], so it is
    /// offered for reopening with its conversation resumed and the hangup
    /// that follows cannot overwrite it ([`Self::record_exit`] never does).
    /// Unlike [`Self::mark_user_closed`] it is never withdrawn: a parked
    /// session is meant to come back. Returns whether a row changed.
    pub fn mark_parked(&self, terminal_id: &str) -> bool {
        let mut guard = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let now = now_ms();
        let mut changed = false;
        for entry in guard.entries.iter_mut().rev() {
            let Entry::Valid(row) = entry else { continue };
            if row.terminal_id.as_deref() != Some(terminal_id) {
                continue;
            }
            row.end_state = SessionEndState::AppQuit;
            row.withdrawn = false;
            row.ended_at = Some(now);
            ensure_agy_fallback(row);
            changed = true;
            break;
        }
        if changed {
            self.persist_locked(&guard.entries);
        }
        changed
    }

    /// Marks every non-withdrawn `Running` row as [`SessionEndState::AppQuit`]
    /// synchronously and flushes to disk before teardown sends `SIGHUP`.
    pub fn mark_app_quit(&self) -> usize {
        let mut guard = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let now = now_ms();
        let mut count = 0;
        for entry in &mut guard.entries {
            let Entry::Valid(row) = entry else { continue };
            if row.withdrawn || row.end_state != SessionEndState::Running {
                continue;
            }
            row.end_state = SessionEndState::AppQuit;
            row.ended_at = Some(now);
            ensure_agy_fallback(row);
            count += 1;
        }
        if count > 0 {
            self.persist_locked(&guard.entries);
        }
        count
    }

    /// Records the observed process exit for `terminal_id`.
    ///
    /// Crucial race guard (`S2-03`): if [`Self::mark_app_quit`] already marked
    /// this row `AppQuit` (or the user withdrew it via Close), subsequent
    /// `SIGHUP` exit delivery never overwrites `AppQuit`.
    pub fn record_exit(
        &self,
        terminal_id: &str,
        ended_by_gitpulse: bool,
        exit_code: Option<i32>,
        signal: Option<&str>,
    ) -> bool {
        let mut guard = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let now = now_ms();
        let mut changed = false;
        for entry in guard.entries.iter_mut().rev() {
            let Entry::Valid(row) = entry else { continue };
            if row.terminal_id.as_deref() != Some(terminal_id) {
                continue;
            }
            if row.end_state == SessionEndState::AppQuit || row.withdrawn {
                return false;
            }
            if ended_by_gitpulse {
                row.end_state = SessionEndState::ExitedClean;
                row.withdrawn = true;
            } else if let Some(sig) = signal.map(str::trim).filter(|s| !s.is_empty()) {
                row.end_state = SessionEndState::ExitedSignal;
                row.signal = Some(sig.to_owned());
                ensure_agy_fallback(row);
            } else {
                row.end_state = SessionEndState::ExitedClean;
            }
            row.ended_at = Some(now);
            row.exit_code = exit_code;
            changed = true;
            break;
        }
        if changed {
            self.persist_locked(&guard.entries);
        }
        changed
    }

    /// Startup reconciliation: any non-withdrawn `Running` row whose
    /// `terminal_id` is absent from `live_terminal_ids` (left behind by
    /// `SIGKILL`, crash, or reboot) transitions to [`SessionEndState::Lost`].
    pub fn reconcile_startup(&self, live_terminal_ids: &HashSet<String>) -> usize {
        let mut guard = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let now = now_ms();
        let mut count = 0;
        for entry in &mut guard.entries {
            let Entry::Valid(row) = entry else { continue };
            if row.withdrawn || row.end_state != SessionEndState::Running {
                continue;
            }
            let still_alive = row
                .terminal_id
                .as_deref()
                .is_some_and(|id| live_terminal_ids.contains(id));
            if still_alive {
                continue;
            }
            row.end_state = SessionEndState::Lost;
            row.ended_at = Some(now);
            ensure_agy_fallback(row);
            count += 1;
        }
        if count > 0 {
            self.persist_locked(&guard.entries);
        }
        count
    }

    /// Withdraws a valid recovery offer or removes a corrupt `Unreadable` row.
    /// The only way a non-reopened recovery offer disappears from the UI (`S2-05`).
    pub fn dismiss(&self, row_id: &str) -> bool {
        let mut guard = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let before = guard.entries.len();
        let mut changed = false;
        guard.entries.retain(|entry| match entry {
            Entry::Corrupt { row_id: id, .. } => {
                if id == row_id {
                    changed = true;
                    false
                } else {
                    true
                }
            }
            Entry::Valid(_) => true,
        });
        if guard.entries.len() != before {
            changed = true;
        }
        for entry in &mut guard.entries {
            if let Entry::Valid(row) = entry {
                if row.row_id == row_id && !row.withdrawn {
                    row.withdrawn = true;
                    row.last_error = None;
                    changed = true;
                }
            }
        }
        if changed {
            self.persist_locked(&guard.entries);
        }
        changed
    }

    /// Attaches a per-row error when Reopen fails (e.g., gone `cwd`) without
    /// withdrawing the row so it stays listed (`S2-03`).
    pub fn record_reopen_failure(&self, row_id: &str, error: String) -> Option<SessionLedgerOffer> {
        let mut guard = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let mut updated: Option<StoredRow> = None;
        for entry in &mut guard.entries {
            let Entry::Valid(row) = entry else { continue };
            if row.row_id == row_id && !row.withdrawn {
                row.last_error = Some(error.clone());
                updated = Some(row.as_ref().clone());
                break;
            }
        }
        let row = updated?;
        self.persist_locked(&guard.entries);
        Some(project_valid_row(&row, |prov, sid| {
            conversation::check_session_resumability(prov, Some(sid), Path::new(&row.cwd))
        }))
    }

    /// Verifies right now whether `row_id` can be reopened (`check_reopen`
    /// with an injected transcript probe for deterministic testing).
    pub(crate) fn check_reopen_with<F>(
        &self,
        row_id: &str,
        mut probe: F,
    ) -> Result<SessionReopenCheck, String>
    where
        F: FnMut(&str, &str) -> conversation::Transcript,
    {
        let mut guard = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let mut found: Option<SessionLedgerOffer> = None;
        let mut dirty = false;
        for entry in &mut guard.entries {
            match entry {
                Entry::Corrupt {
                    row_id: id, detail, ..
                } if id == row_id => {
                    found = Some(project_corrupt_row(id, detail));
                    break;
                }
                Entry::Valid(row) if row.row_id == row_id && !row.withdrawn => {
                    let check = conversation::check_session_resumability_with(
                        &row.launcher,
                        row.native_id.as_deref(),
                        Path::new(&row.cwd),
                        |prov, sid| probe(prov, sid),
                    );
                    if !check.resumable {
                        if row.last_error != check.reason {
                            row.last_error = check.reason.clone();
                            dirty = true;
                        }
                    } else if row.last_error.take().is_some() {
                        dirty = true;
                    }
                    found = Some(project_valid_row_with_status(row, check));
                    break;
                }
                _ => {}
            }
        }
        if dirty {
            self.persist_locked(&guard.entries);
        }
        found
            .map(|offer| SessionReopenCheck { offer })
            .ok_or_else(|| format!("Session ledger row {row_id:?} was not found."))
    }

    /// Verifies right now whether `row_id` can be reopened against real disk
    /// transcripts and checkout paths.
    pub fn check_reopen(&self, row_id: &str) -> Result<SessionReopenCheck, String> {
        let configs = conversation::claude_config_dirs();
        let agy_homes = conversation::antigravity_homes();
        self.check_reopen_with(row_id, |provider, sid| {
            probe_with_dirs(&configs, &agy_homes, provider, sid)
        })
    }

    /// Projects the durable ledger with an injected transcript probe (for tests).
    pub(crate) fn view_with<F>(&self, mut probe: F) -> SessionLedgerView
    where
        F: FnMut(&str, &str) -> conversation::Transcript,
    {
        let guard = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let mut offers = Vec::new();
        if let Some(err) = &guard.load_error {
            offers.push(project_corrupt_row("corrupt-ledger-file", err));
        }
        // Cache repeated (provider, session_id) transcript filesystem probes within one view build.
        let mut cache: HashMap<(String, String), conversation::Transcript> = HashMap::new();
        for entry in guard.entries.iter().rev() {
            match entry {
                Entry::Corrupt { row_id, detail, .. } => {
                    offers.push(project_corrupt_row(row_id, detail));
                }
                Entry::Valid(row) => {
                    if row.withdrawn || !is_offer_end_state(row.end_state) {
                        continue;
                    }
                    let offer = project_valid_row(row, |prov, sid| {
                        conversation::check_session_resumability_with(
                            prov,
                            Some(sid),
                            Path::new(&row.cwd),
                            |p, s| {
                                let key = (p.to_owned(), s.to_owned());
                                if let Some(hit) = cache.get(&key) {
                                    return clone_transcript(hit);
                                }
                                let fresh = probe(p, s);
                                cache.insert(key, clone_transcript(&fresh));
                                fresh
                            },
                        )
                    });
                    offers.push(offer);
                }
            }
        }
        let reopenable_count = offers.iter().filter(|o| o.resumable).count();
        let unreadable_count = offers
            .iter()
            .filter(|o| o.end_state == SessionEndState::Unreadable)
            .count();
        SessionLedgerView {
            offers,
            reopenable_count,
            unreadable_count,
        }
    }

    /// Projects the durable ledger against live transcript directories on disk.
    pub fn view(&self) -> SessionLedgerView {
        let configs = conversation::claude_config_dirs();
        let agy_homes = conversation::antigravity_homes();
        self.view_with(|provider, sid| probe_with_dirs(&configs, &agy_homes, provider, sid))
    }

    /// Emits `SESSION_LEDGER_EVENT` to all webview listeners with the current view.
    pub fn emit_view<R: tauri::Runtime>(&self, app: &tauri::AppHandle<R>) {
        use tauri::Emitter as _;
        let snapshot = self.view();
        if let Err(error) = app.emit(SESSION_LEDGER_EVENT, snapshot) {
            log::warn!(target: "terminal", "failed to emit {SESSION_LEDGER_EVENT}: {error}");
        }
    }

    /// Raw snapshot helper for tests checking exact persisted `end_state`.
    #[cfg(test)]
    fn raw_states(&self) -> Vec<(String, SessionEndState, bool)> {
        let guard = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        guard
            .entries
            .iter()
            .map(|entry| match entry {
                Entry::Valid(r) => (r.row_id.clone(), r.end_state, r.withdrawn),
                Entry::Corrupt { row_id, .. } => {
                    (row_id.clone(), SessionEndState::Unreadable, false)
                }
            })
            .collect()
    }

    fn persist_locked(&self, entries: &[Entry]) {
        let Some(path) = &self.path else {
            return;
        };
        if let Err(error) = write_entries_atomically(path, entries) {
            log::warn!(
                target: "terminal",
                "could not persist session ledger to {}: {error}",
                path.display()
            );
        }
    }
}

fn clone_transcript(t: &conversation::Transcript) -> conversation::Transcript {
    match t {
        conversation::Transcript::Found => conversation::Transcript::Found,
        conversation::Transcript::Absent => conversation::Transcript::Absent,
        conversation::Transcript::Unknown(why) => conversation::Transcript::Unknown(why.clone()),
    }
}

fn probe_with_dirs(
    configs: &conversation::ClaudeConfigDirs,
    agy_homes: &[PathBuf],
    provider: &str,
    sid: &str,
) -> conversation::Transcript {
    if provider == "agy" || provider == "antigravity" {
        conversation::search_antigravity_homes(agy_homes, sid)
    } else {
        conversation::search_claude_configs(configs, sid)
    }
}

fn is_offer_end_state(state: SessionEndState) -> bool {
    matches!(
        state,
        SessionEndState::AppQuit
            | SessionEndState::Lost
            | SessionEndState::ExitedSignal
            | SessionEndState::Unreadable
    )
}

fn ensure_agy_fallback(row: &mut StoredRow) {
    if row.launcher == "agy" && row.native_id.is_none() {
        row.native_id = Some(terminal_command::AGY_CONTINUE_SENTINEL.to_owned());
    }
}

fn normalize_launcher(raw: &str) -> String {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return "shell".to_owned();
    }
    if trimmed == "antigravity" {
        return "agy".to_owned();
    }
    super::launcher_kind(trimmed)
        .unwrap_or_else(|| if trimmed == "shell" { "shell" } else { trimmed })
        .to_owned()
}

fn sanitize_native_id(launcher: &str, native_id: Option<&str>) -> Option<String> {
    let sid = native_id.map(str::trim).filter(|s| !s.is_empty())?;
    if launcher == "agy" && sid == terminal_command::AGY_CONTINUE_SENTINEL {
        return Some(sid.to_owned());
    }
    if terminal_command::is_canonical_uuid(sid) {
        Some(sid.to_ascii_lowercase())
    } else {
        None
    }
}

fn project_corrupt_row(row_id: &str, detail: &str) -> SessionLedgerOffer {
    SessionLedgerOffer {
        row_id: row_id.to_owned(),
        terminal_id: None,
        launcher: "unknown".to_owned(),
        native_id: None,
        repo_path: String::new(),
        start_dir: None,
        cwd: String::new(),
        mode: None,
        started_at: 0,
        ended_at: None,
        end_state: SessionEndState::Unreadable,
        signal: None,
        resumable: false,
        reason: Some("unreadable".to_owned()),
        last_error: Some(detail.to_owned()),
        title: None,
    }
}

fn project_valid_row<F>(row: &StoredRow, mut evaluate: F) -> SessionLedgerOffer
where
    F: FnMut(&str, &str) -> conversation::ProviderResumability,
{
    let status = match row.native_id.as_deref() {
        Some(sid) => evaluate(&row.launcher, sid),
        None => conversation::check_session_resumability_with(
            &row.launcher,
            None,
            Path::new(&row.cwd),
            |_, _| conversation::Transcript::Absent,
        ),
    };
    project_valid_row_with_status(row, status)
}

fn project_valid_row_with_status(
    row: &StoredRow,
    status: conversation::ProviderResumability,
) -> SessionLedgerOffer {
    let reason = if status.resumable {
        None
    } else {
        status.reason
    };
    SessionLedgerOffer {
        row_id: row.row_id.clone(),
        terminal_id: row.terminal_id.clone(),
        launcher: row.launcher.clone(),
        native_id: row.native_id.clone(),
        repo_path: row.repo_path.clone(),
        start_dir: row.start_dir.clone(),
        cwd: row.cwd.clone(),
        mode: row.mode.clone(),
        started_at: row.started_at,
        ended_at: row.ended_at,
        end_state: row.end_state,
        signal: row.signal.clone(),
        resumable: status.resumable,
        reason,
        last_error: row.last_error.clone(),
        title: row.title.clone(),
    }
}

fn enforce_bound(entries: &mut Vec<Entry>) {
    while entries.len() > MAX_LEDGER_ROWS {
        if let Some(pos) = entries
            .iter()
            .position(|e| matches!(e, Entry::Valid(r) if r.withdrawn))
        {
            entries.remove(pos);
            continue;
        }
        if let Some(pos) = entries.iter().position(|e| {
            matches!(
                e,
                Entry::Valid(r) if r.end_state == SessionEndState::ExitedClean
            )
        }) {
            entries.remove(pos);
            continue;
        }
        if let Some(pos) = entries.iter().position(|e| {
            matches!(
                e,
                Entry::Valid(r) if r.end_state != SessionEndState::Running
            )
        }) {
            entries.remove(pos);
            continue;
        }
        if let Some(pos) = entries
            .iter()
            .position(|e| matches!(e, Entry::Corrupt { .. }))
        {
            entries.remove(pos);
            continue;
        }
        entries.remove(0);
    }
}

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

fn mint_row_id() -> String {
    let mut bytes = [0u8; 16];
    if getrandom::fill(&mut bytes).is_err() {
        let seq = TMP_SEQ.fetch_add(1, Ordering::Relaxed);
        return format!("row-{:016x}-{:08x}", now_ms(), seq);
    }
    let mut out = String::with_capacity(4 + 32);
    out.push_str("row-");
    for byte in bytes {
        use std::fmt::Write as _;
        let _ = write!(&mut out, "{byte:02x}");
    }
    out
}

fn corrupt_row_id(index: usize, raw: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(index.to_le_bytes());
    hasher.update(raw.as_bytes());
    let digest = hasher.finalize();
    let mut hex = String::with_capacity(16);
    for byte in &digest[..8] {
        use std::fmt::Write as _;
        let _ = write!(&mut hex, "{byte:02x}");
    }
    format!("corrupt-{hex}")
}

fn sanitize_raw_line(line: &str) -> String {
    let mut clean: String = line
        .chars()
        .filter(|c| *c != '\n' && *c != '\r' && *c != '\0')
        .collect();
    if clean.len() > MAX_CORRUPT_RAW_BYTES {
        let mut end = MAX_CORRUPT_RAW_BYTES;
        while !clean.is_char_boundary(end) {
            end -= 1;
        }
        clean.truncate(end);
    }
    clean
}

fn load_entries(path: &Path) -> Result<Vec<Entry>, String> {
    if let Some(parent) = path.parent() {
        check_not_symlink(parent)?;
    }
    match fs::symlink_metadata(path) {
        Ok(meta) => {
            if meta.file_type().is_symlink() {
                return Err(format!(
                    "refusing symlinked session ledger file {}",
                    path.display()
                ));
            }
            if !meta.is_file() {
                return Err(format!(
                    "session ledger path {} is not a regular file",
                    path.display()
                ));
            }
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(error) => return Err(format!("could not stat {}: {error}", path.display())),
    }
    let contents = fs::read_to_string(path)
        .map_err(|error| format!("could not read {}: {error}", path.display()))?;
    let mut entries = Vec::new();
    let mut seen_corrupt: HashSet<String> = HashSet::new();
    for (idx, line) in contents.lines().enumerate() {
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }
        if line.len() > MAX_LINE_BYTES {
            let row_id = unique_corrupt_id(idx, trimmed, &mut seen_corrupt);
            entries.push(Entry::Corrupt {
                row_id,
                raw_line: sanitize_raw_line(trimmed),
                detail: format!("ledger row {} exceeds {MAX_LINE_BYTES} bytes", idx + 1),
            });
            continue;
        }
        match serde_json::from_str::<StoredRow>(trimmed) {
            Ok(row) if row.v == LEDGER_SCHEMA_VERSION && !row.row_id.trim().is_empty() => {
                entries.push(Entry::Valid(Box::new(row)));
            }
            Ok(row) => {
                let row_id = unique_corrupt_id(idx, trimmed, &mut seen_corrupt);
                entries.push(Entry::Corrupt {
                    row_id,
                    raw_line: sanitize_raw_line(trimmed),
                    detail: format!(
                        "unsupported or malformed ledger row (v={}, row_id={:?})",
                        row.v, row.row_id
                    ),
                });
            }
            Err(error) => {
                let row_id = unique_corrupt_id(idx, trimmed, &mut seen_corrupt);
                entries.push(Entry::Corrupt {
                    row_id,
                    raw_line: sanitize_raw_line(trimmed),
                    detail: format!("corrupt JSON on line {}: {error}", idx + 1),
                });
            }
        }
    }
    enforce_bound(&mut entries);
    Ok(entries)
}

fn unique_corrupt_id(idx: usize, trimmed: &str, seen: &mut HashSet<String>) -> String {
    let mut candidate = corrupt_row_id(idx, trimmed);
    let mut suffix = 1usize;
    while seen.contains(&candidate) {
        candidate = format!("{candidate}-{suffix}");
        suffix += 1;
    }
    seen.insert(candidate.clone());
    candidate
}

fn check_not_symlink(dir: &Path) -> Result<(), String> {
    let mut current = PathBuf::new();
    for component in dir.components() {
        current.push(component);
        if let Ok(meta) = fs::symlink_metadata(&current) {
            if meta.file_type().is_symlink() {
                // Allow OS-standard /tmp -> private/tmp or /var -> private/var symlinks on macOS
                // only when resolving system root prefixes; refuse symlinks inside config directories.
                #[cfg(target_os = "macos")]
                if matches!(current.to_str(), Some("/tmp" | "/var" | "/etc")) {
                    continue;
                }
                return Err(format!(
                    "refusing symlink ancestor {} in session ledger path",
                    current.display()
                ));
            }
        }
    }
    Ok(())
}

fn ensure_private_dir(dir: &Path) -> Result<(), String> {
    check_not_symlink(dir)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::{DirBuilderExt, PermissionsExt};
        let mut builder = fs::DirBuilder::new();
        builder.recursive(true).mode(0o700);
        builder
            .create(dir)
            .map_err(|e| format!("could not create {}: {e}", dir.display()))?;
        let _ = fs::set_permissions(dir, fs::Permissions::from_mode(0o700));
    }
    #[cfg(not(unix))]
    {
        fs::create_dir_all(dir).map_err(|e| format!("could not create {}: {e}", dir.display()))?;
    }
    Ok(())
}

fn write_entries_atomically(path: &Path, entries: &[Entry]) -> Result<(), String> {
    let Some(parent) = path.parent() else {
        return Err("session ledger path has no parent directory".into());
    };
    ensure_private_dir(parent)?;
    if let Ok(meta) = fs::symlink_metadata(path) {
        if meta.file_type().is_symlink() {
            return Err(format!(
                "refusing to write over symlinked session ledger {}",
                path.display()
            ));
        }
    }
    let mut payload = String::new();
    for entry in entries {
        match entry {
            Entry::Valid(row) => {
                let serialized =
                    serde_json::to_string(row).map_err(|e| format!("serialize error: {e}"))?;
                payload.push_str(&serialized);
                payload.push('\n');
            }
            Entry::Corrupt { raw_line, .. } => {
                payload.push_str(raw_line);
                payload.push('\n');
            }
        }
    }
    let seq = TMP_SEQ.fetch_add(1, Ordering::Relaxed);
    let file_name = path
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or(SESSION_LEDGER_FILE);
    let tmp_path = parent.join(format!(".{file_name}.tmp.{}.{seq}", std::process::id()));
    let write_res: Result<(), String> = (|| {
        #[cfg(unix)]
        let mut file = {
            use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
            let f = OpenOptions::new()
                .create_new(true)
                .write(true)
                .mode(0o600)
                .open(&tmp_path)
                .map_err(|e| format!("open temp ledger {}: {e}", tmp_path.display()))?;
            let _ = f.set_permissions(fs::Permissions::from_mode(0o600));
            f
        };
        #[cfg(not(unix))]
        let mut file = OpenOptions::new()
            .create_new(true)
            .write(true)
            .open(&tmp_path)
            .map_err(|e| format!("open temp ledger {}: {e}", tmp_path.display()))?;
        file.write_all(payload.as_bytes())
            .map_err(|e| format!("write temp ledger: {e}"))?;
        file.sync_all()
            .map_err(|e| format!("sync temp ledger: {e}"))?;
        drop(file);
        fs::rename(&tmp_path, path)
            .map_err(|e| format!("rename ledger into {}: {e}", path.display()))
    })();
    if write_res.is_err() {
        let _ = fs::remove_file(&tmp_path);
    }
    write_res
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::workbench::conversation::Transcript;

    const UUID_1: &str = "6f1c2a7e-0d4b-4c1e-9a55-3b0e8f2d9c11";
    const UUID_2: &str = "7a2b3c4d-1111-4222-8333-444455556666";
    const UUID_3: &str = "8b3c4d5e-2222-4333-9444-555566667777";

    fn spawn_spec(
        term_id: &str,
        launcher: &str,
        native_id: Option<&str>,
        repo: &Path,
        reopen_row_id: Option<&str>,
    ) -> NewSessionRow {
        let repo_str = repo.to_string_lossy().into_owned();
        NewSessionRow {
            terminal_id: term_id.to_owned(),
            launcher: launcher.to_owned(),
            native_id: native_id.map(str::to_owned),
            repo_path: repo_str.clone(),
            start_dir: None,
            cwd: repo_str,
            mode: Some("edit".to_owned()),
            reopen_row_id: reopen_row_id.map(str::to_owned),
        }
    }

    #[test]
    fn app_quit_writes_durable_rows_and_survives_subsequent_sighup_exit() {
        let dir = tempfile::tempdir().unwrap();
        let ledger_path = dir.path().join(SESSION_LEDGER_FILE);
        let repo = dir.path().join("repo");
        fs::create_dir_all(&repo).unwrap();

        let ledger = SessionLedger::open(&ledger_path);
        for (idx, sid) in [UUID_1, UUID_2, UUID_3].iter().enumerate() {
            ledger.record_spawn(spawn_spec(
                &format!("term-1-{idx}"),
                "claude",
                Some(sid),
                &repo,
                None,
            ));
        }
        assert_eq!(ledger.mark_app_quit(), 3);
        // Simulate reader thread reporting SIGHUP immediately after mark_app_quit.
        for idx in 0..3 {
            assert!(
                !ledger.record_exit(&format!("term-1-{idx}"), true, None, Some("Hangup: 1")),
                "SIGHUP after AppQuit must not overwrite AppQuit"
            );
        }

        // Reopen from disk in a fresh ledger (simulates relaunch after Cmd-Q).
        let relaunched = SessionLedger::open(&ledger_path);
        assert_eq!(relaunched.reconcile_startup(&HashSet::new()), 0);
        let view = relaunched.view_with(|_, _| Transcript::Found);
        assert_eq!(view.offers.len(), 3);
        assert_eq!(view.reopenable_count, 3);
        for offer in &view.offers {
            assert_eq!(offer.end_state, SessionEndState::AppQuit);
            assert_eq!(offer.mode.as_deref(), Some("edit"));
            assert!(offer.resumable);
        }
    }

    #[test]
    fn a_parked_session_is_offered_for_reopening_and_the_hangup_cannot_withdraw_it() {
        let dir = tempfile::tempdir().unwrap();
        let ledger_path = dir.path().join(SESSION_LEDGER_FILE);
        let repo = dir.path().join("repo");
        fs::create_dir_all(&repo).unwrap();
        let ledger = SessionLedger::open(&ledger_path);
        ledger.record_spawn(spawn_spec("term-1-0", "claude", Some(UUID_1), &repo, None));
        ledger.record_spawn(spawn_spec("term-1-1", "claude", Some(UUID_2), &repo, None));
        // The person closed the second tab with Close: it is withdrawn.
        assert!(ledger.mark_user_closed("term-1-1"));
        // The first was parked; GitPulse's own hangup arrives afterwards.
        assert!(ledger.mark_parked("term-1-0"));
        assert!(!ledger.record_exit("term-1-0", true, None, Some("Hangup: 1")));
        assert!(
            !ledger.mark_parked("term-nope"),
            "an unknown tab changes nothing"
        );
        let view = ledger.view_with(|_, _| Transcript::Found);
        assert_eq!(view.offers.len(), 1, "only the parked tab is offered");
        assert_eq!(view.offers[0].terminal_id.as_deref(), Some("term-1-0"));
        assert_eq!(view.offers[0].end_state, SessionEndState::AppQuit);
        assert!(view.offers[0].resumable);
        // It survives a relaunch, which reconciles only what was running.
        let relaunched = SessionLedger::open(&ledger_path);
        assert_eq!(relaunched.reconcile_startup(&HashSet::new()), 0);
        assert_eq!(
            relaunched.view_with(|_, _| Transcript::Found).offers.len(),
            1
        );
    }

    #[test]
    fn sigkill_running_rows_become_lost_on_relaunch_and_clean_exit_is_never_offered() {
        let dir = tempfile::tempdir().unwrap();
        let ledger_path = dir.path().join(SESSION_LEDGER_FILE);
        let repo = dir.path().join("repo");
        fs::create_dir_all(&repo).unwrap();

        let first = SessionLedger::open(&ledger_path);
        first.record_spawn(spawn_spec("term-live", "claude", Some(UUID_1), &repo, None));
        let clean_row = first.record_spawn(spawn_spec(
            "term-clean",
            "claude",
            Some(UUID_2),
            &repo,
            None,
        ));
        assert!(first.record_exit("term-clean", false, Some(0), None));
        drop(first);

        // Relaunch after SIGKILL (no mark_app_quit ran).
        let relaunched = SessionLedger::open(&ledger_path);
        assert_eq!(relaunched.reconcile_startup(&HashSet::new()), 1);
        let states = relaunched.raw_states();
        assert!(states
            .iter()
            .any(|(id, state, _)| id == &clean_row && *state == SessionEndState::ExitedClean));

        let view = relaunched.view_with(|_, _| Transcript::Found);
        assert_eq!(view.offers.len(), 1);
        assert_eq!(view.offers[0].end_state, SessionEndState::Lost);
        assert_eq!(view.offers[0].native_id.as_deref(), Some(UUID_1));
    }

    #[test]
    fn shells_corrupt_lines_and_bounded_eviction_follow_s2_03() {
        let dir = tempfile::tempdir().unwrap();
        let ledger_path = dir.path().join(SESSION_LEDGER_FILE);
        let repo = dir.path().join("repo");
        fs::create_dir_all(&repo).unwrap();

        fs::write(&ledger_path, "{corrupt json line\n").unwrap();
        let ledger = SessionLedger::open(&ledger_path);
        ledger.record_spawn(spawn_spec("term-shell", "shell", None, &repo, None));
        assert_eq!(ledger.mark_app_quit(), 1);

        let view = ledger.view_with(|_, _| Transcript::Found);
        assert_eq!(view.unreadable_count, 1);
        assert_eq!(view.reopenable_count, 0);
        let corrupt = view
            .offers
            .iter()
            .find(|o| o.end_state == SessionEndState::Unreadable)
            .expect("corrupt row must be listed");
        assert_eq!(corrupt.reason.as_deref(), Some("unreadable"));
        let shell = view
            .offers
            .iter()
            .find(|o| o.launcher == "shell")
            .expect("interrupted shell must be listed");
        assert!(!shell.resumable);
        assert_eq!(shell.reason.as_deref(), Some("not restorable"));

        // Corrupt line survives unrelated writes until explicitly dismissed.
        ledger.record_spawn(spawn_spec("term-c1", "claude", Some(UUID_1), &repo, None));
        let reloaded = SessionLedger::open(&ledger_path);
        assert_eq!(
            reloaded
                .view_with(|_, _| Transcript::Found)
                .unreadable_count,
            1
        );
        assert!(reloaded.dismiss(&corrupt.row_id));
        assert_eq!(
            reloaded
                .view_with(|_, _| Transcript::Found)
                .unreadable_count,
            0
        );

        // Exceeding MAX_LEDGER_ROWS never lets stored line count surpass MAX_LEDGER_ROWS.
        for i in 0..(MAX_LEDGER_ROWS + 15) {
            reloaded.record_spawn(spawn_spec(
                &format!("term-bound-{i}"),
                "claude",
                Some(UUID_1),
                &repo,
                None,
            ));
        }
        let line_count = fs::read_to_string(&ledger_path).unwrap().lines().count();
        assert_eq!(line_count, MAX_LEDGER_ROWS);
    }

    #[test]
    fn gone_cwd_per_row_error_and_reopen_withdrawal_work() {
        let dir = tempfile::tempdir().unwrap();
        let ledger_path = dir.path().join(SESSION_LEDGER_FILE);
        let gone_repo = dir.path().join("deleted-repo");
        fs::create_dir_all(&gone_repo).unwrap();

        let ledger = SessionLedger::open(&ledger_path);
        let row_id = ledger.record_spawn(spawn_spec(
            "term-gone",
            "claude",
            Some(UUID_1),
            &gone_repo,
            None,
        ));
        ledger.mark_app_quit();
        fs::remove_dir_all(&gone_repo).unwrap();

        let check = ledger
            .check_reopen_with(&row_id, |_, _| Transcript::Found)
            .unwrap();
        assert!(!check.offer.resumable);
        assert!(check
            .offer
            .last_error
            .as_deref()
            .is_some_and(|e| e.contains("no longer exists")));
        // Still listed with error persisted.
        let reloaded = SessionLedger::open(&ledger_path);
        let view = reloaded.view_with(|_, _| Transcript::Found);
        assert_eq!(view.offers.len(), 1);
        assert!(view.offers[0]
            .last_error
            .as_deref()
            .is_some_and(|e| e.contains("no longer exists")));

        // Successful reopen spawn withdraws the old row.
        let live_repo = dir.path().join("live-repo");
        fs::create_dir_all(&live_repo).unwrap();
        reloaded.record_spawn(spawn_spec(
            "term-reopened",
            "claude",
            Some(UUID_1),
            &live_repo,
            Some(&row_id),
        ));
        let after = reloaded.view_with(|_, _| Transcript::Found);
        assert!(after.offers.is_empty());
    }

    #[test]
    fn title_persists_across_quit_crash_user_pin_and_v1_rows_without_title() {
        let dir = tempfile::tempdir().unwrap();
        let ledger_path = dir.path().join(SESSION_LEDGER_FILE);
        let repo = dir.path().join("repo");
        fs::create_dir_all(&repo).unwrap();

        let ledger = SessionLedger::open(&ledger_path);
        ledger.record_spawn(spawn_spec("term-osc", "claude", Some(UUID_1), &repo, None));
        ledger.record_spawn(spawn_spec("term-pinned", "agy", Some(UUID_2), &repo, None));

        // Spinner frames normalize and repeated identical titles do not re-write disk.
        assert_eq!(
            ledger.set_title("term-osc", Some("⠋ Auditing split panes"), false),
            (true, false)
        );
        assert_eq!(
            ledger.set_title("term-osc", Some("⠙ Auditing split panes"), false),
            (false, false)
        );

        // User-pinned custom title resists subsequent non-user OSC title overwrites.
        assert_eq!(
            ledger.set_title("term-pinned", Some("Auth Reviewer"), true),
            (true, false)
        );
        assert_eq!(
            ledger.set_title("term-pinned", Some("claude ~"), false),
            (false, false)
        );
        assert_eq!(
            ledger.resolved_title_for_terminal("term-pinned", Some("claude ~")),
            Some("Auth Reviewer".into())
        );

        assert_eq!(ledger.mark_app_quit(), 2);
        drop(ledger);

        let reloaded = SessionLedger::open(&ledger_path);
        let view = reloaded.view_with(|_, _| Transcript::Found);
        let osc_offer = view
            .offers
            .iter()
            .find(|o| o.terminal_id.as_deref() == Some("term-osc"))
            .unwrap();
        assert_eq!(osc_offer.title.as_deref(), Some("Auditing split panes"));
        let pinned_offer = view
            .offers
            .iter()
            .find(|o| o.terminal_id.as_deref() == Some("term-pinned"))
            .unwrap();
        assert_eq!(pinned_offer.title.as_deref(), Some("Auth Reviewer"));

        // Reopening inherits the saved title onto the newly spawned row.
        reloaded.record_spawn(spawn_spec(
            "term-reopened-pinned",
            "agy",
            Some(UUID_2),
            &repo,
            Some(&pinned_offer.row_id),
        ));
        assert_eq!(
            reloaded.resolved_title_for_terminal("term-reopened-pinned", None),
            Some("Auth Reviewer".into())
        );
    }
}
