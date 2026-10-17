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

use super::incident;
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
}

/// The engine's events, inside the app: the Tauri events and banners the
/// frontend listens for, and the incident record.
pub struct AppEvents<R: tauri::Runtime> {
    app: AppHandle<R>,
    incidents: Arc<incident::Recorder>,
}

impl<R: tauri::Runtime> AppEvents<R> {
    pub fn new(app: &AppHandle<R>, incidents: Arc<incident::Recorder>) -> Self {
        Self {
            app: app.clone(),
            incidents,
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

    fn exit(&self, ended: SessionEnded) {
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
