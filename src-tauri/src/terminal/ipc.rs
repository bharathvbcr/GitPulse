//! The async face of the latency-sensitive terminal commands.
//!
//! `commands::terminals` is the Tauri surface; these are what it awaits. Each
//! looks up the session's lane (or the control pool) and runs the existing
//! blocking function there, so none of them touches tokio's blocking pool, the
//! git admission scope that pool's wrapper sets up, or an async-runtime worker.

use super::{resize_session, write_binary_to_session, write_to_session, TerminalSessions};

/// Delivers keystrokes to a session, in the order the calls were made.
pub async fn write_async(
    state: &TerminalSessions,
    session_id: String,
    data: String,
    binary: bool,
) -> Result<(), String> {
    let lane = state.input_lane(&session_id)?;
    let state = state.clone();
    lane.run("terminal_write", move || {
        if binary {
            write_binary_to_session(&state, &session_id, &data)
        } else {
            write_to_session(&state, &session_id, &data)
        }
    })
    .await
}

/// Applies a size to a session's PTY.
pub async fn resize_async(
    state: &TerminalSessions,
    session_id: String,
    rows: u16,
    cols: u16,
) -> Result<(), String> {
    let lane = state.geometry_lane(&session_id)?;
    let state = state.clone();
    lane.run("terminal_resize", move || {
        resize_session(&state, &session_id, rows, cols)
    })
    .await
}

/// Runs a slower terminal command (reading a session's context, closing or
/// attaching one) on the control pool. `operation` is its slow-command label.
pub async fn control_async<T, F>(
    state: &TerminalSessions,
    operation: &'static str,
    body: F,
) -> Result<T, String>
where
    T: Send + 'static,
    F: FnOnce(&TerminalSessions) -> Result<T, String> + Send + 'static,
{
    let control = state.control.clone();
    let state = state.clone();
    control.run(operation, move || body(&state)).await
}
