//! What the engine says about a session other than its output.
//!
//! The engine (spawn, reader, flow control, kill and reap) used to take a
//! Tauri `AppHandle` and emit `terminal-loss`, `terminal-exit` and
//! `terminal-incident` itself, and call `alerts::offer` from the reader. That
//! tied it to the app process: a PTY could not outlive the app because the code
//! that owns it could not run anywhere else. It now reports through a
//! [`SessionEvents`] its caller supplies. In the app that is [`AppEvents`],
//! which does exactly what the engine used to; in `gitpulse-ptyd` it is a frame
//! to whichever app is connected (`host::server`).

use super::{incident, session_ledger};
use super::{TerminalExitPayload, TerminalLossPayload};
use std::sync::Arc;
use tauri::{AppHandle, Emitter};

/// How a session ended, with what the incident recorder needs.
#[derive(Debug, Clone)]
pub struct SessionEnded {
    pub payload: TerminalExitPayload,
    /// The session's `dead` flag as the reader found it: set by Close,
    /// shutdown and the reader's own failure paths, so those never count as
    /// an external death.
    pub ended_by_gitpulse: bool,
    /// The program as it was requested (`claude`, or the user's shell).
    pub program: String,
    /// Where it started.
    pub cwd: String,
}

/// Where the engine reports a session's events. Called from its reader thread;
/// an implementation must not block on anything a reader could be waiting for.
pub trait SessionEvents: Send + Sync {
    /// Output a live view will never see. Emitted as `terminal-loss`.
    fn loss(&self, payload: TerminalLossPayload);
    /// Something the session's output asked attention for.
    fn notice(&self, notice: crate::alerts::Notice);
    /// The output scanner gave up on this many sequences since the last call.
    fn scan_drops(&self, dropped: u64);
    /// The session ended. Called once, after its last byte was sent and its
    /// entry left the session map.
    fn exit(&self, ended: SessionEnded);
    /// A connection to the terminal host listed its sessions: the one moment a
    /// reconcile that could not ask the host earlier can run. Must not block.
    fn host_adopted(&self) {}
}

/// The engine's events, inside the app: the Tauri events and banners the
/// frontend listens for, and the incident record.
pub struct AppEvents<R: tauri::Runtime> {
    app: AppHandle<R>,
    incidents: Arc<incident::Recorder>,
    session_ledger: Arc<session_ledger::SessionLedger>,
}

impl<R: tauri::Runtime> AppEvents<R> {
    pub fn new(
        app: &AppHandle<R>,
        incidents: Arc<incident::Recorder>,
        session_ledger: Arc<session_ledger::SessionLedger>,
    ) -> Self {
        Self {
            app: app.clone(),
            incidents,
            session_ledger,
        }
    }
}

impl<R: tauri::Runtime> SessionEvents for AppEvents<R> {
    fn loss(&self, payload: TerminalLossPayload) {
        if let Err(error) = self.app.emit("terminal-loss", payload) {
            log::warn!(target: "terminal", "could not report skipped output: {error}");
        }
    }

    fn notice(&self, notice: crate::alerts::Notice) {
        crate::alerts::offer(notice);
    }

    fn scan_drops(&self, dropped: u64) {
        crate::alerts::record_scan_drops(dropped);
    }

    fn host_adopted(&self) {
        if !self.session_ledger.reconcile_pending() {
            return;
        }
        let app = self.app.clone();
        let spawned = crate::supervisor::spawn_one_of_many(
            "terminal-ledger-reconcile",
            "terminal session ledger reconcile",
            move || {
                use tauri::Manager as _;
                let Some(terminals) = app.try_state::<super::TerminalSessions>() else {
                    return;
                };
                if terminals.retry_session_ledger_reconcile().is_some() {
                    terminals.session_ledger().emit_view(&app);
                }
            },
        );
        if let Err(error) = spawned {
            log::warn!(target: "terminal", "ledger reconcile retry did not start: {error}");
        }
    }

    fn exit(&self, ended: SessionEnded) {
        self.session_ledger.record_lifecycle(
            "exit",
            serde_json::json!({
                "session_id": ended.payload.id,
                "exit_code": ended.payload.exit_code,
                "signal": ended.payload.signal,
                "error": ended.payload.error,
                "reaped": ended.payload.reaped,
                "ended_by_gitpulse": ended.ended_by_gitpulse,
                "program": ended.program,
                "cwd": ended.cwd,
            }),
        );
        if self.session_ledger.record_exit(
            &ended.payload.id,
            session_ledger::ExitFacts {
                ended_by_gitpulse: ended.ended_by_gitpulse,
                exit_code: ended.payload.exit_code,
                signal: Some(ended.payload.signal.as_str()).filter(|s| !s.is_empty()),
                error: ended.payload.error.as_deref(),
                reaped: ended.payload.reaped,
            },
        ) {
            self.session_ledger.emit_view(&self.app);
        }
        incident::report_exit(
            &self.incidents,
            &self.app,
            ended.ended_by_gitpulse,
            &ended.payload,
            &ended.program,
            &ended.cwd,
        );
        if let Err(e) = self.app.emit("terminal-exit", ended.payload) {
            log::warn!(target: "terminal", "failed to emit terminal-exit event: {e}");
        }
    }
}
