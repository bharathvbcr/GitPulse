//! Durable per-tab ledger for hosted terminal & agent sessions (S2-03, S2-05).
//!
//! Records one bounded row per spawned interactive tab (`~/.config/gitpulse/terminal-sessions.v1.jsonl`
//! in production, in-memory for unit tests unless constructed with [`SessionLedger::open`])
//! with its launcher, native conversation id, checkout/start directory, permission mode,
//! timestamps, and lifecycle end state:
//! - `running`: tab currently has a live PTY
//! - `exited_clean`: an exit GitPulse *observed* (it reaped the process and has a code) or a
//!   user-requested Close (never offered). A clean row with neither an exit code nor a
//!   signal is impossible by construction and is repaired to `lost` when the ledger opens
//! - `exited_signal`: external signal killed the PTY while the app stayed up
//! - `app_quit`: written synchronously in teardown *before* `SIGHUP` so quit+reopen keeps them
//! - `lost`: the outcome was never observed: left `running` by a prior `SIGKILL`, crash, or
//!   reboot (reconciled at startup), the host ended while holding it (the synthetic exit
//!   with no code, no signal, not reaped), or GitPulse's own reader failed. Offered, and
//!   never overwritten by a later exit; a deliberate Close of such a session withdraws it
//! - `unreadable`: corrupt JSONL line on disk, preserved and surfaced as `"unreadable"`

use super::lifecycle::{self, KillOrigin, RotatingJsonl};
use crate::workbench::{conversation, terminal_command};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::{HashMap, HashSet};
use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
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
/// How long a reopened session must stay alive before the offer it replaced is
/// withdrawn, for launchers whose `SessionStart` hook never reports in.
pub const REOPEN_SURVIVAL_MS: u64 = 20_000;

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
    /// The route the close came by (`KillOrigin`), set when a close is
    /// requested. Absent on rows written before it existed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    closed_by: Option<KillOrigin>,
    /// Spared by batch closes, "Stop children", agent closes and the load
    /// governor. Kept on the row so it survives a page reload and a restart.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    protected: bool,
    /// Set on a row that reopened `resumes_row_id` and has not yet proved the
    /// resumed conversation is alive. Cleared by [`confirm_reopen`] (a
    /// `SessionStart` bind, [`REOPEN_SURVIVAL_MS`] alive, a deliberate close, a
    /// quit) or turned into a failed reopen by [`fail_reopen`].
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    resume_unconfirmed: bool,
    /// The row this one reopened. With `resume_unconfirmed` clear it marks a
    /// reopen that failed: the row is withdrawn and the old offer stands.
    /// Older builds ignore both fields (no `deny_unknown_fields` here), so a
    /// downgrade shows the old row and the new one as two offers, never none.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    resumes_row_id: Option<String>,
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

/// What was observed when a session ended, as [`SessionLedger::record_exit`]
/// needs it. A host's synthetic exit is `reaped: false`, no code, no signal.
#[derive(Debug, Clone, Copy, Default)]
pub struct ExitFacts<'a> {
    /// GitPulse ended it (Close, shutdown, or its own reader failing).
    pub ended_by_gitpulse: bool,
    pub exit_code: Option<i32>,
    pub signal: Option<&'a str>,
    /// Why the exit is not a plain one (a failed reap, a failed reader, the
    /// host ending).
    pub error: Option<&'a str>,
    /// GitPulse reaped the process and so knows how it ended.
    pub reaped: bool,
}

impl<'a> ExitFacts<'a> {
    /// An exit GitPulse reaped, with no error: how it ended is known.
    pub fn reaped(
        ended_by_gitpulse: bool,
        exit_code: Option<i32>,
        signal: Option<&'a str>,
    ) -> Self {
        Self {
            ended_by_gitpulse,
            exit_code,
            signal,
            error: None,
            reaped: true,
        }
    }
}

/// `last_error` of a row [`SessionLedger::record_exit`] could not classify and
/// the exit carried no reason for.
const UNOBSERVED_EXIT: &str = "The session ended and GitPulse never observed how.";
/// `last_error` of a row [`SessionLedger::repair_unobserved_exits`] rewrote.
const REPAIRED_UNOBSERVED_EXIT: &str =
    "The session's outcome was never observed (its host ended); repaired at startup.";

/// Whether a close from `origin` means the person is finished with the
/// session, so an exit nobody observed after it is withdrawn rather than
/// offered. The kill stamps `closed_by` before it asks the host, so when the
/// host is already dead the kill errors, `mark_user_closed` never runs, and
/// this is all that says the person closed it.
///
/// Every route is deliberate except [`KillOrigin::LoadGovernor`]: it parks an
/// idle session to give memory back and means it to come back (see
/// [`SessionLedger::mark_parked`]). `Unspecified` is a direct close by
/// definition, and `QuitStopAll` is a stop-everything the person chose.
fn is_deliberate_close(origin: KillOrigin) -> bool {
    !matches!(origin, KillOrigin::LoadGovernor)
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
    /// Append-only record of every spawn, attach, detach, close request and
    /// exit, beside the ledger. `None` for an in-memory ledger.
    lifecycle: Option<RotatingJsonl>,
    /// Rows the cap evicted, kept so a row is never silently lost.
    archive: Option<RotatingJsonl>,
    /// Rows [`Self::repair_unobserved_exits`] rewrote while opening, handed to
    /// the first [`Self::reconcile_startup`] so its caller emits the view.
    repaired_on_open: AtomicUsize,
}

/// What a close needs to know about a session, from its ledger row.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SessionBrief {
    pub launcher: String,
    pub repo_path: String,
    pub protected: bool,
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
            lifecycle: None,
            archive: None,
            repaired_on_open: AtomicUsize::new(0),
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
        let lifecycle = path
            .parent()
            .map(|dir| RotatingJsonl::standard(dir.join(lifecycle::LIFECYCLE_FILE)));
        let archive = path
            .parent()
            .map(|dir| RotatingJsonl::standard(dir.join(lifecycle::ARCHIVE_FILE)));
        let mut trimmed_on_load = false;
        let (entries, load_error) = match load_entries(&path) {
            Ok((entries, evicted)) => {
                archive_evicted(archive.as_ref(), &evicted);
                trimmed_on_load = !evicted.is_empty();
                (entries, None)
            }
            Err(error) => {
                log::warn!(
                    target: "terminal",
                    "session ledger at {} could not be read: {error}",
                    path.display()
                );
                (Vec::new(), Some(error))
            }
        };
        let ledger = Self {
            path: Some(path),
            state: Mutex::new(LedgerState {
                entries,
                load_error,
            }),
            lifecycle,
            archive,
            repaired_on_open: AtomicUsize::new(0),
        };
        // Before anything can deliver an exit: rows an older build recorded
        // as clean for sessions whose outcome it never saw become offers.
        let repaired = ledger.repair_unobserved_exits();
        ledger.repaired_on_open.store(repaired, Ordering::Relaxed);
        if trimmed_on_load {
            // The evicted rows are archived now; rewriting the file keeps the
            // next open from archiving them a second time.
            let guard = ledger
                .state
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            ledger.persist_locked(&guard.entries);
        }
        ledger
    }

    /// Appends one record to the lifecycle log: `event` plus `fields` (a JSON
    /// object), stamped with the time and this app's pid (`app_pid`, never the
    /// session's process, which the host protocol does not report). Never
    /// fails or blocks the caller on an I/O error; an in-memory ledger records
    /// nothing.
    pub fn record_lifecycle(&self, event: &str, fields: serde_json::Value) {
        let Some(log) = &self.lifecycle else { return };
        let mut record = serde_json::Map::new();
        record.insert("ts_ms".into(), now_ms().into());
        record.insert("app_pid".into(), std::process::id().into());
        record.insert("event".into(), event.into());
        if let serde_json::Value::Object(extra) = fields {
            for (key, value) in extra {
                record.insert(key, clamp_field(value));
            }
        }
        match serde_json::to_string(&serde_json::Value::Object(record)) {
            Ok(line) => log.append(&line),
            Err(error) => {
                log::warn!(target: "terminal", "lifecycle record {event} not serialized: {error}");
            }
        }
    }

    /// Every retained lifecycle line, oldest first (tests and tooling).
    pub fn lifecycle_lines(&self) -> Vec<String> {
        self.lifecycle
            .as_ref()
            .map_or_else(Vec::new, RotatingJsonl::read_all)
    }

    /// Every retained archived row, oldest first (tests and tooling).
    pub fn archived_lines(&self) -> Vec<String> {
        self.archive
            .as_ref()
            .map_or_else(Vec::new, RotatingJsonl::read_all)
    }

    /// The launcher, repository and protection of the newest row for
    /// `terminal_id`, or `None` for a session this ledger never recorded.
    pub fn brief(&self, terminal_id: &str) -> Option<SessionBrief> {
        let guard = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        guard.entries.iter().rev().find_map(|entry| match entry {
            Entry::Valid(row) if row.terminal_id.as_deref() == Some(terminal_id) => {
                Some(SessionBrief {
                    launcher: row.launcher.clone(),
                    repo_path: row.repo_path.clone(),
                    // Only a live, unwithdrawn row protects: terminal ids
                    // repeat across restarts, and an old row that ended must
                    // never shield an unrelated session that reuses its id.
                    protected: row.protected
                        && !row.withdrawn
                        && row.end_state == SessionEndState::Running,
                })
            }
            _ => None,
        })
    }

    /// Whether `terminal_id` is protected from batch closes, agent closes and
    /// the load governor. A session the ledger does not know is not.
    pub fn is_protected(&self, terminal_id: &str) -> bool {
        self.brief(terminal_id).is_some_and(|brief| brief.protected)
    }

    /// Sets or clears the protected flag on the newest row for `terminal_id`.
    /// Returns whether the flag changed (false for an unknown session).
    pub fn set_protected(&self, terminal_id: &str, protected: bool) -> bool {
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
            if row.protected != protected {
                row.protected = protected;
                changed = true;
            }
            break;
        }
        if changed {
            self.persist_locked(&guard.entries);
        }
        changed
    }

    /// Notes which route asked for each of `terminal_ids` to be closed, in one
    /// write. Returns how many rows changed.
    pub fn set_closed_by_many(&self, terminal_ids: &[String], origin: KillOrigin) -> usize {
        let mut guard = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let mut changed = 0;
        for id in terminal_ids {
            let newest = guard
                .entries
                .iter_mut()
                .rev()
                .find_map(|entry| match entry {
                    Entry::Valid(row) if row.terminal_id.as_deref() == Some(id.as_str()) => {
                        Some(row)
                    }
                    _ => None,
                });
            if let Some(row) = newest {
                if row.closed_by != Some(origin) {
                    row.closed_by = Some(origin);
                    changed += 1;
                }
            }
        }
        if changed > 0 {
            self.persist_locked(&guard.entries);
        }
        changed
    }

    /// Notes which route asked for `terminal_id` to be closed. Returns whether
    /// a row changed; a session with no row has nowhere to carry it.
    pub fn set_closed_by(&self, terminal_id: &str, origin: KillOrigin) -> bool {
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
            if row.closed_by != Some(origin) {
                row.closed_by = Some(origin);
                changed = true;
            }
            break;
        }
        if changed {
            self.persist_locked(&guard.entries);
        }
        changed
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
        let mut resumes_row_id: Option<String> = None;
        if let Some(reopen_id) = spec.reopen_row_id.as_deref() {
            // The old row (and its siblings) stay offered-in-waiting: the
            // newest-row rule hides them while this one runs, and they are
            // withdrawn only once the reopened session proves itself.
            for entry in &guard.entries {
                if let Entry::Valid(existing) = entry {
                    if existing.row_id == reopen_id {
                        inherited_title = existing.title.clone();
                        inherited_pinned = existing.title_pinned && inherited_title.is_some();
                    }
                }
            }
            resumes_row_id = Some(reopen_id.to_owned());
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
            closed_by: None,
            protected: false,
            resume_unconfirmed: resumes_row_id.is_some(),
            resumes_row_id,
        };
        let spawned_launcher = row.launcher.clone();
        let spawned_repo = row.repo_path.clone();
        let spawned_terminal = row.terminal_id.clone();
        guard.entries.push(Entry::Valid(Box::new(row)));
        let evicted = enforce_bound(&mut guard.entries);
        self.persist_locked(&guard.entries);
        drop(guard);
        archive_evicted(self.archive.as_ref(), &evicted);
        self.record_lifecycle(
            "spawn",
            serde_json::json!({
                "session_id": spawned_terminal,
                "row_id": row_id,
                "launcher": spawned_launcher,
                "repo": spawned_repo,
            }),
        );
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
        let mut confirmed: Option<String> = None;
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
            // SessionStart fired: the agent is alive, whether or not the id moved.
            if row.resume_unconfirmed {
                confirmed = Some(row.row_id.clone());
            }
            break;
        }
        if let Some(id) = confirmed {
            changed |= confirm_reopen(&mut guard.entries, &id);
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
        let mut confirmed: Option<String> = None;
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
            if row.resume_unconfirmed {
                confirmed = Some(row.row_id.clone());
            }
            break;
        }
        if let Some(id) = confirmed {
            changed |= confirm_reopen(&mut guard.entries, &id);
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
        let mut confirmed: Option<String> = None;
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
            if row.resume_unconfirmed {
                confirmed = Some(row.row_id.clone());
            }
            break;
        }
        if let Some(id) = confirmed {
            confirm_reopen(&mut guard.entries, &id);
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
        // A reopened session still running at quit is alive: it becomes the
        // one offer for its conversation and the row it reopened goes.
        let alive: Vec<String> = guard
            .entries
            .iter()
            .filter_map(|entry| match entry {
                Entry::Valid(row)
                    if row.resume_unconfirmed
                        && !row.withdrawn
                        && row.end_state == SessionEndState::Running =>
                {
                    Some(row.row_id.clone())
                }
                _ => None,
            })
            .collect();
        let mut settled = false;
        for id in alive {
            settled |= confirm_reopen(&mut guard.entries, &id);
        }
        settled |= settle_reopens(&mut guard.entries, now);
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
        if count > 0 || settled {
            self.persist_locked(&guard.entries);
        }
        count
    }

    /// Settles unconfirmed reopens as of `now` and persists a change.
    pub(crate) fn settle_reopens_at(&self, now: u64) -> bool {
        let mut guard = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let changed = settle_reopens(&mut guard.entries, now);
        if changed {
            self.persist_locked(&guard.entries);
        }
        changed
    }

    /// Rewrites every row shaped like an exit nobody observed into
    /// [`SessionEndState::Lost`], and returns how many it changed.
    ///
    /// Before exits carried their facts, the synthetic exit a dead host's
    /// sessions get (no code, no signal, not reaped) was recorded as a clean
    /// exit, which is never offered. Such a row is `ExitedClean`, not
    /// withdrawn, with neither an exit code nor a signal: a user Close is
    /// withdrawn, and a reaped exit always has a code or a signal
    /// (`finalize_pty_session`). Idempotent: a repaired row is `Lost`.
    pub fn repair_unobserved_exits(&self) -> usize {
        let mut guard = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let mut repaired = 0;
        for entry in &mut guard.entries {
            let Entry::Valid(row) = entry else { continue };
            if row.end_state == SessionEndState::ExitedClean
                && !row.withdrawn
                && row.exit_code.is_none()
                && row.signal.is_none()
            {
                row.end_state = SessionEndState::Lost;
                row.last_error = Some(REPAIRED_UNOBSERVED_EXIT.to_owned());
                row.ended_at.get_or_insert_with(now_ms);
                ensure_agy_fallback(row);
                repaired += 1;
            }
        }
        if repaired > 0 {
            self.persist_locked(&guard.entries);
        }
        repaired
    }

    /// Records how the session `terminal_id` ended.
    ///
    /// Classification, in order:
    /// - A row already `AppQuit` or withdrawn is never overwritten: the
    ///   `SIGHUP` after [`Self::mark_app_quit`] or a Close must not undo it
    ///   (`S2-03`). A `Lost` row is never overwritten either; it only gains
    ///   `last_error` when it has none.
    /// - GitPulse never observed how it ended (`!reaped` and no signal, as for
    ///   the exit a dead host's sessions get): a row whose close was a
    ///   [`is_deliberate_close`] is withdrawn, anything else is `Lost` so it
    ///   is offered for reopening, with `error` as `last_error`.
    /// - GitPulse ended it and says why (`ended_by_gitpulse` with an `error`:
    ///   its own reader failed) and nobody deliberately closed it: `Lost` too.
    /// - Otherwise the outcome was observed: GitPulse ended it, withdrawn;
    ///   a signal, `ExitedSignal`; else `ExitedClean`.
    pub fn record_exit(&self, terminal_id: &str, facts: ExitFacts<'_>) -> bool {
        let mut guard = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let now = now_ms();
        let signal = facts.signal.map(str::trim).filter(|s| !s.is_empty());
        let error = facts.error.map(str::trim).filter(|e| !e.is_empty());
        let mut changed = false;
        // A reopened session that lived long enough is confirmed before the
        // exit is classified, so only an early exit can fail the reopen.
        settle_reopens(&mut guard.entries, now);
        let mut reopen: Option<(String, bool, String)> = None;
        for entry in guard.entries.iter_mut().rev() {
            let Entry::Valid(row) = entry else { continue };
            if row.terminal_id.as_deref() != Some(terminal_id) {
                continue;
            }
            if row.resume_unconfirmed {
                let detail = if let Some(e) = error {
                    e.to_owned()
                } else if let Some(sig) = signal {
                    format!("was killed by signal {sig}")
                } else if let Some(code) = facts.exit_code {
                    format!("exited with code {code}")
                } else {
                    "ended without a known outcome".to_owned()
                };
                let secs = now.saturating_sub(row.started_at) / 1000;
                let deliberate = row.closed_by.is_some_and(is_deliberate_close);
                reopen = Some((
                    row.row_id.clone(),
                    deliberate,
                    format!("Reopen failed: {} {detail} after {secs} s.", row.launcher),
                ));
            }
            if row.end_state == SessionEndState::Lost {
                if row.last_error.is_none() && !row.withdrawn {
                    row.last_error = error.map(str::to_owned);
                    changed = row.last_error.is_some();
                }
                break;
            }
            if row.end_state == SessionEndState::AppQuit || row.withdrawn {
                return false;
            }
            let deliberate = row.closed_by.is_some_and(is_deliberate_close);
            let observed = facts.reaped || signal.is_some();
            let unknown = (!observed && !deliberate)
                || (facts.ended_by_gitpulse && error.is_some() && !deliberate);
            if unknown {
                row.end_state = SessionEndState::Lost;
                row.last_error = Some(error.unwrap_or(UNOBSERVED_EXIT).to_owned());
                ensure_agy_fallback(row);
            } else if facts.ended_by_gitpulse || !observed {
                row.end_state = SessionEndState::ExitedClean;
                row.withdrawn = true;
            } else if let Some(sig) = signal {
                row.end_state = SessionEndState::ExitedSignal;
                row.signal = Some(sig.to_owned());
                ensure_agy_fallback(row);
            } else {
                row.end_state = SessionEndState::ExitedClean;
            }
            row.ended_at = Some(now);
            row.exit_code = facts.exit_code;
            changed = true;
            break;
        }
        if let Some((id, deliberate, why)) = reopen {
            changed |= if deliberate {
                confirm_reopen(&mut guard.entries, &id)
            } else {
                fail_reopen(&mut guard.entries, &id, &why)
            };
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
        // A reopen that outlived the window before the crash is confirmed; one
        // that did not, and is no longer live, fails below and the old offer
        // stands, so exactly one row offers the conversation either way.
        if settle_reopens(&mut guard.entries, now) {
            count += 1;
        }
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
        if settle_reopens(&mut guard.entries, now) {
            count += 1;
        }
        if count > 0 {
            self.persist_locked(&guard.entries);
        }
        // Rows repaired while opening count as changed the first time, so the
        // caller emits the view for them too.
        count + self.repaired_on_open.swap(0, Ordering::Relaxed)
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
        let mut guard = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if settle_reopens(&mut guard.entries, now_ms()) {
            self.persist_locked(&guard.entries);
        }
        // Rows an unconfirmed reopen is standing in for: hidden while it runs
        // even when they name no conversation (agy's continue sentinel).
        let reopening: HashSet<&str> = guard
            .entries
            .iter()
            .filter_map(|entry| match entry {
                Entry::Valid(row) if row.resume_unconfirmed && !row.withdrawn => {
                    row.resumes_row_id.as_deref()
                }
                _ => None,
            })
            .collect();
        let mut offers = Vec::new();
        if let Some(err) = &guard.load_error {
            offers.push(project_corrupt_row("corrupt-ledger-file", err));
        }
        // Cache repeated (provider, session_id) transcript filesystem probes within one view build.
        let mut cache: HashMap<(String, String), conversation::Transcript> = HashMap::new();
        // One offer per conversation: the newest row of any state wins, and a
        // conversation that is running again offers nothing. A conversation
        // continued in a later tab that then ended cleanly, or was closed,
        // must not be offered again from an older row.
        let mut taken: HashSet<(String, String)> = guard
            .entries
            .iter()
            .filter_map(|entry| match entry {
                Entry::Valid(row)
                    if !row.withdrawn && row.end_state == SessionEndState::Running =>
                {
                    conversation_key(row)
                }
                _ => None,
            })
            .collect();
        for entry in guard.entries.iter().rev() {
            match entry {
                Entry::Corrupt { row_id, detail, .. } => {
                    offers.push(project_corrupt_row(row_id, detail));
                }
                Entry::Valid(row) => {
                    // Claimed before the offer test, so a newer row that is
                    // not an offer still hides every older one.
                    if is_failed_reopen(row) {
                        continue;
                    }
                    let newest = conversation_key(row).is_none_or(|key| taken.insert(key));
                    if !newest || reopening.contains(row.row_id.as_str()) || row.withdrawn || !is_offer_end_state(row.end_state) {
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

/// The reopened session `new_row_id` is alive: clear its flag and withdraw
/// the row it reopened plus every other non-running row for the same
/// conversation, so nothing offers it a second time. Returns whether it
/// changed anything.
fn confirm_reopen(entries: &mut [Entry], new_row_id: &str) -> bool {
    let mut old_id = None;
    let mut key = None;
    for entry in entries.iter_mut() {
        let Entry::Valid(row) = entry else { continue };
        if row.row_id == new_row_id && row.resume_unconfirmed {
            row.resume_unconfirmed = false;
            old_id = row.resumes_row_id.take();
            key = conversation_key(row);
            break;
        }
    }
    let Some(old_id) = old_id else { return false };
    for entry in entries.iter_mut() {
        let Entry::Valid(other) = entry else { continue };
        let sibling = other.end_state != SessionEndState::Running
            && key.is_some()
            && conversation_key(other) == key;
        if !other.withdrawn && (other.row_id == old_id || sibling) && other.row_id != new_row_id {
            other.withdrawn = true;
            other.last_error = None;
        }
    }
    true
}

/// The reopened session `new_row_id` ended before proving itself: withdraw it
/// (it is not a second offer), keep it marked as a failed reopen so it does
/// not hide the conversation, and put `why` on the row that is still offered.
fn fail_reopen(entries: &mut [Entry], new_row_id: &str, why: &str) -> bool {
    let mut old_id = None;
    for entry in entries.iter_mut() {
        let Entry::Valid(row) = entry else { continue };
        if row.row_id == new_row_id && row.resume_unconfirmed {
            row.resume_unconfirmed = false;
            row.withdrawn = true;
            row.last_error = Some(why.to_owned());
            old_id = row.resumes_row_id.clone();
            break;
        }
    }
    let Some(old_id) = old_id else { return false };
    for entry in entries.iter_mut() {
        let Entry::Valid(old) = entry else { continue };
        if old.row_id == old_id && !old.withdrawn {
            old.last_error = Some(why.to_owned());
        }
    }
    true
}

/// A row that reopened another and was withdrawn without being confirmed.
fn is_failed_reopen(row: &StoredRow) -> bool {
    row.resumes_row_id.is_some() && !row.resume_unconfirmed
}

/// Settles every unconfirmed reopen as of `now`, lazily (there is no timer):
/// a running one alive for [`REOPEN_SURVIVAL_MS`] is confirmed, one someone
/// already withdrew is confirmed (a person finished with it), and one that is
/// no longer running (a reboot's `Lost`, a stray exit) failed.
fn settle_reopens(entries: &mut [Entry], now: u64) -> bool {
    let pending: Vec<(String, bool, bool, u64)> = entries
        .iter()
        .filter_map(|entry| match entry {
            Entry::Valid(row) if row.resume_unconfirmed => Some((
                row.row_id.clone(),
                row.withdrawn,
                row.end_state == SessionEndState::Running,
                row.started_at,
            )),
            _ => None,
        })
        .collect();
    let mut changed = false;
    for (id, withdrawn, running, started) in pending {
        if withdrawn || (running && now.saturating_sub(started) >= REOPEN_SURVIVAL_MS) {
            changed |= confirm_reopen(entries, &id);
        } else if !running {
            changed |= fail_reopen(
                entries,
                &id,
                "Reopen failed: the resumed session was no longer running when the app checked.",
            );
        }
    }
    changed
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

/// The conversation a row belongs to: its launcher and native id. The agy
/// "continue" sentinel names no conversation, so it has no key.
fn conversation_key(row: &StoredRow) -> Option<(String, String)> {
    let native = row.native_id.as_deref()?;
    if native == terminal_command::AGY_CONTINUE_SENTINEL {
        return None;
    }
    Some((row.launcher.clone(), native.to_owned()))
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

/// Longest string a lifecycle field keeps: several fields are IPC arguments
/// (a session id the caller made up), and the log is bounded in size, not in
/// what one line may carry.
const MAX_LIFECYCLE_FIELD_BYTES: usize = 512;

fn clamp_field(value: serde_json::Value) -> serde_json::Value {
    match value {
        serde_json::Value::String(mut text) if text.len() > MAX_LIFECYCLE_FIELD_BYTES => {
            let mut end = MAX_LIFECYCLE_FIELD_BYTES;
            while !text.is_char_boundary(end) {
                end -= 1;
            }
            text.truncate(end);
            serde_json::Value::String(text)
        }
        serde_json::Value::Array(items) => {
            serde_json::Value::Array(items.into_iter().map(clamp_field).collect())
        }
        other => other,
    }
}

/// Appends evicted rows to the archive so the cap never silently loses one.
fn archive_evicted(archive: Option<&RotatingJsonl>, evicted: &[Entry]) {
    let Some(archive) = archive else { return };
    let archived_at = now_ms();
    for entry in evicted {
        let line = match entry {
            Entry::Valid(row) => serde_json::to_value(row).map(|mut value| {
                if let Some(object) = value.as_object_mut() {
                    object.insert("archived_at".into(), archived_at.into());
                }
                value
            }),
            Entry::Corrupt {
                row_id,
                raw_line,
                detail,
            } => Ok(serde_json::json!({
                "archived_at": archived_at,
                "row_id": row_id,
                "corrupt": raw_line,
                "detail": detail,
            })),
        };
        match line.and_then(|value| serde_json::to_string(&value)) {
            Ok(text) => archive.append(&text),
            Err(error) => {
                log::warn!(target: "terminal", "evicted ledger row not archived: {error}");
            }
        }
    }
}

/// Trims `entries` to [`MAX_LEDGER_ROWS`] and returns what it removed, oldest
/// first, for [`archive_evicted`].
fn enforce_bound(entries: &mut Vec<Entry>) -> Vec<Entry> {
    let mut evicted = Vec::new();
    while entries.len() > MAX_LEDGER_ROWS {
        if let Some(pos) = entries
            .iter()
            .position(|e| matches!(e, Entry::Valid(r) if r.withdrawn))
        {
            evicted.push(entries.remove(pos));
            continue;
        }
        if let Some(pos) = entries.iter().position(|e| {
            matches!(
                e,
                Entry::Valid(r) if r.end_state == SessionEndState::ExitedClean
            )
        }) {
            evicted.push(entries.remove(pos));
            continue;
        }
        // A corrupt line goes before any offer: it can only be dismissed,
        // while an offer is a conversation the person can still get back.
        if let Some(pos) = entries
            .iter()
            .position(|e| matches!(e, Entry::Corrupt { .. }))
        {
            evicted.push(entries.remove(pos));
            continue;
        }
        if let Some(pos) = entries.iter().position(|e| {
            matches!(
                e,
                Entry::Valid(r) if r.end_state != SessionEndState::Running
            )
        }) {
            evicted.push(entries.remove(pos));
            continue;
        }
        evicted.push(entries.remove(0));
    }
    evicted
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

/// Reads the ledger; the second list is what the row cap evicted.
fn load_entries(path: &Path) -> Result<(Vec<Entry>, Vec<Entry>), String> {
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
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return Ok((Vec::new(), Vec::new()))
        }
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
    let evicted = enforce_bound(&mut entries);
    Ok((entries, evicted))
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
                !ledger.record_exit(
                    &format!("term-1-{idx}"),
                    ExitFacts::reaped(true, None, Some("Hangup: 1"))
                ),
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
        assert!(!ledger.record_exit("term-1-0", ExitFacts::reaped(true, None, Some("Hangup: 1"))));
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
        assert!(first.record_exit("term-clean", ExitFacts::reaped(false, Some(0), None)));
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

    #[test]
    fn rows_past_the_cap_are_archived_not_lost() {
        let dir = tempfile::tempdir().unwrap();
        let ledger_path = dir.path().join(SESSION_LEDGER_FILE);
        let ledger = SessionLedger::open(&ledger_path);
        let extra = 7;
        for i in 0..(MAX_LEDGER_ROWS + extra) {
            ledger.record_spawn(spawn_spec(
                &format!("term-arch-{i}"),
                "claude",
                Some(UUID_1),
                dir.path(),
                None,
            ));
        }
        assert_eq!(
            fs::read_to_string(&ledger_path).unwrap().lines().count(),
            MAX_LEDGER_ROWS
        );
        let archived = ledger.archived_lines();
        assert_eq!(archived.len(), extra, "{archived:?}");
        for i in 0..extra {
            assert!(
                archived
                    .iter()
                    .any(|l| l.contains(&format!("\"term-arch-{i}\""))),
                "row {i} was evicted without being archived"
            );
        }
        assert!(archived[0].contains("archived_at"));
    }

    #[test]
    fn an_old_shaped_ledger_loads_and_new_fields_round_trip() {
        let dir = tempfile::tempdir().unwrap();
        let ledger_path = dir.path().join(SESSION_LEDGER_FILE);
        // A row exactly as written before `closed_by` and `protected` existed.
        let old = format!(
            "{{\"v\":1,\"row_id\":\"row-old\",\"terminal_id\":\"term-old\",\"launcher\":\"claude\",\"native_id\":\"{UUID_1}\",\"repo_path\":\"/r\",\"start_dir\":null,\"cwd\":\"/r\",\"mode\":null,\"started_at\":1,\"ended_at\":null,\"end_state\":\"running\",\"signal\":null,\"exit_code\":null,\"last_error\":null,\"withdrawn\":false}}\n"
        );
        fs::write(&ledger_path, old).unwrap();
        let ledger = SessionLedger::open(&ledger_path);
        let brief = ledger
            .brief("term-old")
            .expect("old row loads as a valid row");
        assert!(!brief.protected);
        assert!(ledger.set_protected("term-old", true));
        assert!(ledger.set_closed_by("term-old", KillOrigin::BatchClose));
        let reopened = SessionLedger::open(&ledger_path);
        assert!(
            reopened.is_protected("term-old"),
            "protection survives a reload"
        );
        let text = fs::read_to_string(&ledger_path).unwrap();
        assert!(text.contains("\"closed_by\":\"batch_close\""), "{text}");
        assert_eq!(reopened.view().unreadable_count, 0);
    }

    #[test]
    fn a_finished_protected_row_does_not_shield_a_session_that_reuses_its_id() {
        let dir = tempfile::tempdir().unwrap();
        let ledger = SessionLedger::open(dir.path().join(SESSION_LEDGER_FILE));
        ledger.record_spawn(spawn_spec("term-7-1", "claude", None, dir.path(), None));
        assert!(ledger.set_protected("term-7-1", true));
        assert!(ledger.is_protected("term-7-1"));
        // The session exits on its own, then the id is reused after a restart.
        assert!(ledger.record_exit("term-7-1", ExitFacts::reaped(false, Some(0), None)));
        assert!(
            !ledger.is_protected("term-7-1"),
            "an ended row protects nothing"
        );
        ledger.record_spawn(spawn_spec("term-7-1", "claude", None, dir.path(), None));
        assert!(
            !ledger.is_protected("term-7-1"),
            "the new session starts unprotected"
        );
    }

    #[test]
    fn lifecycle_records_spawn_and_an_unwritable_log_does_not_fail_the_ledger() {
        let dir = tempfile::tempdir().unwrap();
        let ledger_path = dir.path().join(SESSION_LEDGER_FILE);
        let ledger = SessionLedger::open(&ledger_path);
        ledger.record_spawn(spawn_spec("term-life", "claude", None, dir.path(), None));
        let lines = ledger.lifecycle_lines();
        assert_eq!(lines.len(), 1, "{lines:?}");
        assert!(lines[0].contains("\"event\":\"spawn\""));
        assert!(lines[0].contains("\"session_id\":\"term-life\""));
        // The writer's pid goes under a name nobody reads as the session's
        // process: a bare `pid` beside `session_id` was read that way in the
        // first real-app reload test.
        let record: serde_json::Value = serde_json::from_str(&lines[0]).unwrap();
        assert_eq!(record["app_pid"], u64::from(std::process::id()), "{record}");
        assert!(
            record.get("pid").is_none(),
            "no ambiguous bare pid: {record}"
        );
        ledger.record_lifecycle(
            "protection",
            serde_json::json!({ "session_id": "x".repeat(100_000) }),
        );
        let last = ledger.lifecycle_lines().pop().unwrap();
        assert!(
            last.len() < 1024,
            "an IPC-sized id is clamped, got {} bytes",
            last.len()
        );

        // A directory where the lifecycle file belongs: every append fails.
        let blocked = tempfile::tempdir().unwrap();
        fs::create_dir(blocked.path().join(lifecycle::LIFECYCLE_FILE)).unwrap();
        let ledger = SessionLedger::open(blocked.path().join(SESSION_LEDGER_FILE));
        ledger.record_spawn(spawn_spec("term-blocked", "claude", None, dir.path(), None));
        assert!(
            ledger.brief("term-blocked").is_some(),
            "the spawn still recorded"
        );
    }

    // ---- exit classification, repair, eviction, dedupe (reboot-loss fix) ----

    /// The synthetic exit ptyd and the client give every session a dead host
    /// held: no code, no signal, not reaped, `HOST_ENDED`.
    fn host_ended(ledger: &SessionLedger, id: &str) -> bool {
        exit_facts(ledger, id, false, None, None, Some("host ended"), false)
    }

    fn exit_facts(
        ledger: &SessionLedger,
        id: &str,
        ended_by_gitpulse: bool,
        exit_code: Option<i32>,
        signal: Option<&str>,
        error: Option<&str>,
        reaped: bool,
    ) -> bool {
        ledger.record_exit(
            id,
            ExitFacts {
                ended_by_gitpulse,
                exit_code,
                signal,
                error,
                reaped,
            },
        )
    }

    fn state_of(ledger: &SessionLedger, row_id: &str) -> (SessionEndState, bool) {
        ledger
            .raw_states()
            .into_iter()
            .find(|(id, _, _)| id == row_id)
            .map(|(_, s, w)| (s, w))
            .expect("row exists")
    }

    #[test]
    fn a_host_ended_exit_makes_the_session_lost_and_offered_not_clean() {
        let dir = tempfile::tempdir().unwrap();
        let ledger = SessionLedger::open(dir.path().join(SESSION_LEDGER_FILE));
        let row = ledger.record_spawn(spawn_spec(
            "term-h",
            "claude",
            Some(UUID_1),
            dir.path(),
            None,
        ));
        assert!(host_ended(&ledger, "term-h"));
        assert_eq!(state_of(&ledger, &row), (SessionEndState::Lost, false));
        let view = ledger.view_with(|_, _| Transcript::Found);
        assert_eq!(view.offers.len(), 1);
        assert!(view.offers[0].resumable);
        assert_eq!(view.offers[0].last_error.as_deref(), Some("host ended"));
        assert!(view.offers[0].ended_at.is_some());
    }

    #[test]
    fn an_unobserved_exit_after_a_deliberate_close_is_withdrawn_but_a_parked_origin_is_offered() {
        let dir = tempfile::tempdir().unwrap();
        let ledger = SessionLedger::open(dir.path().join(SESSION_LEDGER_FILE));
        let closed = ledger.record_spawn(spawn_spec(
            "term-c",
            "claude",
            Some(UUID_1),
            dir.path(),
            None,
        ));
        let governed = ledger.record_spawn(spawn_spec(
            "term-g",
            "claude",
            Some(UUID_2),
            dir.path(),
            None,
        ));
        assert!(ledger.set_closed_by("term-c", KillOrigin::TabClose));
        assert!(ledger.set_closed_by("term-g", KillOrigin::LoadGovernor));
        assert!(host_ended(&ledger, "term-c"));
        assert!(host_ended(&ledger, "term-g"));
        assert_eq!(
            state_of(&ledger, &closed),
            (SessionEndState::ExitedClean, true)
        );
        assert_eq!(state_of(&ledger, &governed), (SessionEndState::Lost, false));
    }

    #[test]
    fn a_reader_failure_is_lost_with_its_error_but_a_plain_close_is_withdrawn() {
        let dir = tempfile::tempdir().unwrap();
        let ledger = SessionLedger::open(dir.path().join(SESSION_LEDGER_FILE));
        let failed = ledger.record_spawn(spawn_spec(
            "term-f",
            "claude",
            Some(UUID_1),
            dir.path(),
            None,
        ));
        let closed = ledger.record_spawn(spawn_spec(
            "term-k",
            "claude",
            Some(UUID_2),
            dir.path(),
            None,
        ));
        ledger.set_closed_by("term-k", KillOrigin::TabClose);
        // reader_failure_payload: reaped, a code, GitPulse-ended, with an error.
        assert!(exit_facts(
            &ledger,
            "term-f",
            true,
            Some(0),
            None,
            Some("PTY reader failed: x"),
            true
        ));
        // A Close: reaped, signalled, GitPulse-ended, no error.
        assert!(exit_facts(
            &ledger,
            "term-k",
            true,
            None,
            Some("Hangup: 1"),
            None,
            true
        ));
        assert_eq!(state_of(&ledger, &failed), (SessionEndState::Lost, false));
        assert_eq!(
            state_of(&ledger, &closed),
            (SessionEndState::ExitedClean, true)
        );
        let view = ledger.view_with(|_, _| Transcript::Found);
        assert_eq!(view.offers.len(), 1);
        assert_eq!(
            view.offers[0].last_error.as_deref(),
            Some("PTY reader failed: x")
        );
    }

    #[test]
    fn a_lost_row_is_never_overwritten_by_a_later_exit() {
        let dir = tempfile::tempdir().unwrap();
        let ledger = SessionLedger::open(dir.path().join(SESSION_LEDGER_FILE));
        let row = ledger.record_spawn(spawn_spec(
            "term-l",
            "claude",
            Some(UUID_1),
            dir.path(),
            None,
        ));
        assert_eq!(ledger.reconcile_startup(&HashSet::new()), 1);
        exit_facts(&ledger, "term-l", false, Some(0), None, None, true);
        exit_facts(
            &ledger,
            "term-l",
            false,
            None,
            Some("Killed: 9"),
            None,
            true,
        );
        assert_eq!(state_of(&ledger, &row), (SessionEndState::Lost, false));
    }

    #[test]
    fn reaped_exits_keep_their_classification() {
        let dir = tempfile::tempdir().unwrap();
        let ledger = SessionLedger::open(dir.path().join(SESSION_LEDGER_FILE));
        let sig = ledger.record_spawn(spawn_spec(
            "term-s",
            "claude",
            Some(UUID_1),
            dir.path(),
            None,
        ));
        let ok = ledger.record_spawn(spawn_spec(
            "term-o",
            "claude",
            Some(UUID_2),
            dir.path(),
            None,
        ));
        exit_facts(
            &ledger,
            "term-s",
            false,
            None,
            Some("Killed: 9"),
            None,
            true,
        );
        exit_facts(&ledger, "term-o", false, Some(0), None, None, true);
        assert_eq!(
            state_of(&ledger, &sig),
            (SessionEndState::ExitedSignal, false)
        );
        assert_eq!(
            state_of(&ledger, &ok),
            (SessionEndState::ExitedClean, false)
        );
    }

    #[test]
    fn startup_repairs_clean_rows_that_carry_no_code_and_no_signal() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join(SESSION_LEDGER_FILE);
        let row = |id: &str, state: &str, code: &str, wd: bool, sid: &str| {
            format!(
                "{{\"v\":1,\"row_id\":\"{id}\",\"terminal_id\":\"t-{id}\",\"launcher\":\"claude\",\"native_id\":\"{sid}\",\"repo_path\":\"/r\",\"start_dir\":null,\"cwd\":\"/r\",\"mode\":null,\"started_at\":1,\"ended_at\":2,\"end_state\":\"{state}\",\"signal\":null,\"exit_code\":{code},\"last_error\":null,\"withdrawn\":{wd}}}\n"
            )
        };
        let mut text = String::new();
        text += &row("bad", "exited_clean", "null", false, UUID_1);
        text += &row("user", "exited_clean", "null", true, UUID_2);
        text += &row("coded", "exited_clean", "0", false, UUID_3);
        fs::write(&path, text).unwrap();
        let ledger = SessionLedger::open(&path);
        assert_eq!(state_of(&ledger, "bad"), (SessionEndState::Lost, false));
        assert_eq!(
            state_of(&ledger, "user"),
            (SessionEndState::ExitedClean, true)
        );
        assert_eq!(
            state_of(&ledger, "coded"),
            (SessionEndState::ExitedClean, false)
        );
        let offer = &ledger.view_with(|_, _| Transcript::Found).offers[0];
        assert!(offer
            .last_error
            .as_deref()
            .is_some_and(|e| e.contains("repaired at startup")));
        // Persisted and idempotent.
        let again = SessionLedger::open(&path);
        assert_eq!(state_of(&again, "bad"), (SessionEndState::Lost, false));
        assert_eq!(again.repair_unobserved_exits(), 0);
        assert_eq!(again.reconcile_startup(&HashSet::new()), 0);
    }

    #[test]
    fn eviction_spares_unactioned_offers_and_stays_bounded() {
        let dir = tempfile::tempdir().unwrap();
        let ledger = SessionLedger::open(dir.path().join(SESSION_LEDGER_FILE));
        let offer = ledger.record_spawn(spawn_spec(
            "term-keep",
            "claude",
            Some(UUID_1),
            dir.path(),
            None,
        ));
        ledger.mark_app_quit();
        for i in 0..(MAX_LEDGER_ROWS + 20) {
            let id = format!("term-n-{i}");
            ledger.record_spawn(spawn_spec(&id, "claude", Some(UUID_2), dir.path(), None));
            exit_facts(&ledger, &id, false, Some(0), None, None, true);
        }
        assert_eq!(ledger.raw_states().len(), MAX_LEDGER_ROWS);
        assert_eq!(state_of(&ledger, &offer), (SessionEndState::AppQuit, false));
    }

    #[test]
    fn a_full_ledger_evicts_a_corrupt_line_before_any_unactioned_offer() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join(SESSION_LEDGER_FILE);
        fs::write(&path, "{corrupt json line\n").unwrap();
        let ledger = SessionLedger::open(&path);
        let first = ledger.record_spawn(spawn_spec(
            "term-0",
            "claude",
            Some(UUID_1),
            dir.path(),
            None,
        ));
        for i in 1..(MAX_LEDGER_ROWS - 1) {
            ledger.record_spawn(spawn_spec(
                &format!("term-{i}"),
                "claude",
                Some(UUID_2),
                dir.path(),
                None,
            ));
        }
        ledger.mark_app_quit();
        ledger.record_spawn(spawn_spec(
            "term-new",
            "claude",
            Some(UUID_3),
            dir.path(),
            None,
        ));
        let states = ledger.raw_states();
        assert_eq!(states.len(), MAX_LEDGER_ROWS);
        assert_eq!(state_of(&ledger, &first), (SessionEndState::AppQuit, false));
        assert!(
            states
                .iter()
                .all(|(_, s, _)| *s != SessionEndState::Unreadable),
            "the corrupt line went first"
        );
    }

    #[test]
    fn offers_dedupe_by_launcher_and_native_id_and_yield_to_a_running_row() {
        let dir = tempfile::tempdir().unwrap();
        let ledger = SessionLedger::open(dir.path().join(SESSION_LEDGER_FILE));
        ledger.record_spawn(spawn_spec(
            "term-a",
            "claude",
            Some(UUID_1),
            dir.path(),
            None,
        ));
        let new = ledger.record_spawn(spawn_spec(
            "term-b",
            "claude",
            Some(UUID_1),
            dir.path(),
            None,
        ));
        let other = ledger.record_spawn(spawn_spec(
            "term-c",
            "claude",
            Some(UUID_2),
            dir.path(),
            None,
        ));
        ledger.mark_app_quit();
        let view = ledger.view_with(|_, _| Transcript::Found);
        let ids: Vec<&str> = view.offers.iter().map(|o| o.row_id.as_str()).collect();
        assert_eq!(ids, vec![other.as_str(), new.as_str()]);
        assert_eq!(view.reopenable_count, 2);
        // A running session with the same conversation hides its stale offers.
        ledger.record_spawn(spawn_spec(
            "term-d",
            "claude",
            Some(UUID_2),
            dir.path(),
            None,
        ));
        let view = ledger.view_with(|_, _| Transcript::Found);
        assert_eq!(view.offers.len(), 1);
        assert_eq!(view.offers[0].row_id, new);
    }

    /// A conversation lost in one reboot and then continued in a later tab
    /// that ended on its own must not be offered again from the older row:
    /// the startup repair revives old rows, and the newest row decides.
    #[test]
    fn an_older_offer_yields_to_a_newer_row_of_any_state_for_the_same_conversation() {
        let dir = tempfile::tempdir().unwrap();
        let ledger = SessionLedger::open(dir.path().join(SESSION_LEDGER_FILE));
        let lost = ledger.record_spawn(spawn_spec(
            "term-old",
            "claude",
            Some(UUID_1),
            dir.path(),
            None,
        ));
        assert!(ledger.record_exit(
            "term-old",
            ExitFacts {
                error: Some("host ended"),
                ..ExitFacts::default()
            },
        ));
        assert_eq!(state_of(&ledger, &lost).0, SessionEndState::Lost);
        // Continued by hand in a new tab (not a reopen), which then exits cleanly.
        ledger.record_spawn(spawn_spec(
            "term-new",
            "claude",
            Some(UUID_1),
            dir.path(),
            None,
        ));
        assert!(ledger.record_exit("term-new", ExitFacts::reaped(false, Some(0), None)));
        let view = ledger.view_with(|_, _| Transcript::Found);
        assert!(
            view.offers.iter().all(|o| o.row_id != lost),
            "a conversation that was continued and ended cleanly is not offered again"
        );
        assert_eq!(view.reopenable_count, 0);
    }

    #[test]
    fn a_reopen_withdraws_every_other_non_running_row_for_the_same_conversation() {
        let dir = tempfile::tempdir().unwrap();
        let ledger = SessionLedger::open(dir.path().join(SESSION_LEDGER_FILE));
        let a = ledger.record_spawn(spawn_spec(
            "term-a",
            "claude",
            Some(UUID_1),
            dir.path(),
            None,
        ));
        let b = ledger.record_spawn(spawn_spec(
            "term-b",
            "claude",
            Some(UUID_1),
            dir.path(),
            None,
        ));
        let c = ledger.record_spawn(spawn_spec(
            "term-c",
            "claude",
            Some(UUID_2),
            dir.path(),
            None,
        ));
        ledger.mark_app_quit();
        ledger.record_spawn(spawn_spec(
            "term-r",
            "claude",
            Some(UUID_1),
            dir.path(),
            Some(&b),
        ));
        // Not withdrawn until the reopened session proves itself.
        assert!(!state_of(&ledger, &a).1);
        assert!(!state_of(&ledger, &b).1);
        assert!(ledger.bind_native_id("term-r", "claude", UUID_1));
        assert!(state_of(&ledger, &a).1);
        assert!(state_of(&ledger, &b).1);
        assert!(!state_of(&ledger, &c).1);
    }

    /// Quit, reopen `UUID_1` as `term-r`, and return (ledger, old row id, new row id).
    fn reopened(dir: &Path, native: &str, launcher: &str) -> (SessionLedger, String, String) {
        let ledger = SessionLedger::open(dir.join(SESSION_LEDGER_FILE));
        let old = ledger.record_spawn(spawn_spec("term-old", launcher, Some(native), dir, None));
        ledger.mark_app_quit();
        let new = ledger.record_spawn(spawn_spec("term-r", launcher, Some(native), dir, Some(&old)));
        (ledger, old, new)
    }

    fn offered(ledger: &SessionLedger) -> Vec<SessionLedgerOffer> {
        ledger.view_with(|_, _| Transcript::Found).offers
    }

    #[test]
    fn a_reopen_that_dies_at_once_leaves_the_old_offer_with_the_reason() {
        let dir = tempfile::tempdir().unwrap();
        let (ledger, old, new) = reopened(dir.path(), UUID_1, "claude");
        assert!(offered(&ledger).is_empty(), "hidden while the reopen runs");
        assert!(exit_facts(&ledger, "term-r", false, Some(1), None, None, true));
        let offers = offered(&ledger);
        assert_eq!(offers.len(), 1, "{offers:?}");
        assert_eq!(offers[0].row_id, old);
        assert!(offers[0]
            .last_error
            .as_deref()
            .is_some_and(|e| e.contains("exited with code 1")));
        assert!(state_of(&ledger, &new).1, "the failed reopen is not a duplicate offer");
        // It survives a restart as the same single offer.
        let reloaded = SessionLedger::open(dir.path().join(SESSION_LEDGER_FILE));
        let offers = offered(&reloaded);
        assert_eq!(offers.len(), 1);
        assert_eq!(offers[0].row_id, old);
        // And can be reopened again.
        reloaded.record_spawn(spawn_spec("term-r2", "claude", Some(UUID_1), dir.path(), Some(&old)));
        assert!(offered(&reloaded).is_empty());
    }

    #[test]
    fn a_reopen_is_confirmed_by_session_start_even_without_an_id_change() {
        let dir = tempfile::tempdir().unwrap();
        let (ledger, old, _) = reopened(dir.path(), UUID_1, "claude");
        // The id is unchanged (the spawn already carried it); the bind still confirms.
        ledger.bind_native_id("term-r", "claude", UUID_1);
        assert!(state_of(&ledger, &old).1);
        // A later early-looking exit is now an ordinary exit, not a failed reopen.
        assert!(exit_facts(&ledger, "term-r", false, Some(1), None, None, true));
        assert!(offered(&ledger).is_empty());
    }

    #[test]
    fn a_reopen_that_survives_the_window_is_confirmed_lazily() {
        let dir = tempfile::tempdir().unwrap();
        let (ledger, old, new) = reopened(dir.path(), UUID_1, "claude");
        assert!(!ledger.settle_reopens_at(now_ms() + REOPEN_SURVIVAL_MS - 5_000));
        assert!(!state_of(&ledger, &old).1);
        assert!(ledger.settle_reopens_at(now_ms() + REOPEN_SURVIVAL_MS + 1_000));
        assert!(state_of(&ledger, &old).1);
        assert!(!state_of(&ledger, &new).1);
        assert!(offered(&ledger).is_empty());
    }

    #[test]
    fn a_reopen_cut_short_by_quit_leaves_exactly_one_offer() {
        let dir = tempfile::tempdir().unwrap();
        let (ledger, _, new) = reopened(dir.path(), UUID_1, "claude");
        ledger.mark_app_quit();
        let reloaded = SessionLedger::open(dir.path().join(SESSION_LEDGER_FILE));
        let offers = offered(&reloaded);
        assert_eq!(offers.len(), 1, "{offers:?}");
        assert_eq!(offers[0].row_id, new);
    }

    #[test]
    fn a_reopen_lost_to_a_reboot_leaves_exactly_one_offer_the_old_one() {
        let dir = tempfile::tempdir().unwrap();
        let (ledger, old, new) = reopened(dir.path(), UUID_1, "claude");
        drop(ledger);
        let reloaded = SessionLedger::open(dir.path().join(SESSION_LEDGER_FILE));
        assert!(reloaded.reconcile_startup(&HashSet::new()) > 0);
        let offers = offered(&reloaded);
        assert_eq!(offers.len(), 1, "{offers:?}");
        assert_eq!(offers[0].row_id, old);
        assert!(state_of(&reloaded, &new).1);
    }

    #[test]
    fn an_agy_reopen_without_a_conversation_key_hides_the_old_offer_while_it_runs() {
        let dir = tempfile::tempdir().unwrap();
        let ledger = SessionLedger::open(dir.path().join(SESSION_LEDGER_FILE));
        let old = ledger.record_spawn(spawn_spec("term-old", "agy", None, dir.path(), None));
        ledger.mark_app_quit();
        assert_eq!(offered(&ledger).len(), 1);
        ledger.record_spawn(spawn_spec("term-r", "agy", None, dir.path(), Some(&old)));
        assert!(offered(&ledger).is_empty());
        assert!(exit_facts(&ledger, "term-r", false, Some(1), None, None, true));
        assert_eq!(offered(&ledger)[0].row_id, old);
    }

    #[test]
    fn a_deliberate_close_of_a_reopen_does_not_restore_the_old_offer() {
        let dir = tempfile::tempdir().unwrap();
        let (ledger, _, _) = reopened(dir.path(), UUID_1, "claude");
        assert!(ledger.mark_user_closed("term-r"));
        assert!(offered(&ledger).is_empty());
    }
}
