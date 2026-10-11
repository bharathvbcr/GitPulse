//! Embedded terminal commands and Manvi run actions.

use super::*;

// ---------------------------------------------------------------------------
// Terminal: interactive PTY sessions plus bounded one-shot execution.
// The interactive shell is intentionally *not* gate-routed (a shell can run
// anything, so claiming otherwise would be a check that cannot run reporting
// what a check that ran reports); one-shot git commands still pass through
// harness::guard_command. See `terminal/mod.rs` for the full story.
// ---------------------------------------------------------------------------

/// Spawns an interactive shell in the open repository inside a real PTY.
/// Output streams back as `terminal-output` events; exit lands as
/// `terminal-exit`, both keyed by the returned session id.
#[tauri::command(async)]
#[allow(clippy::too_many_arguments)]
pub async fn cmd_terminal_spawn(
    app: AppHandle,
    state: State<'_, crate::terminal::TerminalSessions>,
    repo_path: String,
    rows: u16,
    cols: u16,
    program: Option<String>,
    args: Option<Vec<String>>,
    env: Option<std::collections::HashMap<String, String>>,
    permission_mode: Option<String>,
    acknowledged: Option<bool>,
    start_dir: Option<String>,
) -> Result<crate::terminal::TerminalSpawned, String> {
    let state = state.inner().clone();
    // Expanded here, at the boundary, rather than in the caller: the flags for
    // a mode are the workbench's table and the frontend never carries them.
    // The refusals inside are what make a bypassed session impossible without
    // an acknowledgement, whatever the frontend did or forgot to do. The
    // notification and setting-source flags go in first so the policy flags
    // land in front of them: policy, launch flags, then the caller's prompt.
    let defaults = crate::tool_config::agent_defaults();
    let sources = defaults.claude_setting_sources_arg();
    let args = crate::workbench::terminal_command::apply_permission_mode(
        program.as_deref(),
        permission_mode.as_deref(),
        acknowledged.unwrap_or(false),
        crate::workbench::terminal_command::with_launch_flags(
            program.as_deref(),
            &crate::workbench::terminal_command::LaunchOptions {
                notify: crate::tool_config::session_alerts().configure_agents,
                setting_sources: sources.as_deref(),
                model: program
                    .as_deref()
                    .and_then(|launcher| defaults.model_for(launcher)),
            },
            args,
        )?,
    )?;
    off_thread(move || {
        crate::terminal::spawn_session_in(
            &app,
            &state,
            &repo_path,
            start_dir.as_deref(),
            rows,
            cols,
            program,
            args,
            env,
        )
    })
    .await
}

/// What a live session is running and where, read once on request: the
/// foreground program, whether it is a job under the shell, and its working
/// directory — inside the repository or not.
#[tauri::command(async)]
pub async fn cmd_terminal_context(
    state: State<'_, crate::terminal::TerminalSessions>,
    session_id: String,
) -> Result<crate::terminal::TerminalContext, String> {
    let state = state.inner().clone();
    off_thread(move || crate::terminal::session_context(&state, &session_id)).await
}

/// Every live terminal session, including ones started by a page that has
/// since been reloaded — which the renderer cannot otherwise know exist, and
/// which still count against the shared session limit.
#[tauri::command(async)]
pub async fn cmd_terminal_sessions(
    state: State<'_, crate::terminal::TerminalSessions>,
) -> Result<Vec<crate::terminal::TerminalListing>, String> {
    let state = state.inner().clone();
    off_thread(move || Ok(crate::terminal::list_sessions(&state))).await
}

/// Shows a session a reloaded page left running in a new tab. Refused for a
/// session a tab still holds.
#[tauri::command(async)]
pub async fn cmd_terminal_attach(
    state: State<'_, crate::terminal::TerminalSessions>,
    session_id: String,
    rows: u16,
    cols: u16,
) -> Result<crate::terminal::TerminalSpawned, String> {
    let state = state.inner().clone();
    off_thread(move || crate::terminal::attach_session(&state, &session_id, rows, cols)).await
}

/// Feeds keystrokes into a live session's PTY.
#[tauri::command(async)]
pub async fn cmd_terminal_write(
    state: State<'_, crate::terminal::TerminalSessions>,
    session_id: String,
    data: String,
    binary: Option<bool>,
) -> Result<(), String> {
    let state = state.inner().clone();
    off_thread(move || {
        if binary.unwrap_or(false) {
            crate::terminal::write_binary_to_session(&state, &session_id, &data)
        } else {
            crate::terminal::write_to_session(&state, &session_id, &data)
        }
    })
    .await
}

/// Resizes a live session's PTY to the frontend grid.
#[tauri::command(async)]
pub async fn cmd_terminal_resize(
    state: State<'_, crate::terminal::TerminalSessions>,
    session_id: String,
    rows: u16,
    cols: u16,
) -> Result<(), String> {
    crate::terminal::resize_session(&state, &session_id, rows, cols)
}

/// Leaves a session running with no view, for a tab that went away without
/// being closed. Returns whether there was a live session to detach.
#[tauri::command(async)]
pub async fn cmd_terminal_detach(
    state: State<'_, crate::terminal::TerminalSessions>,
    session_id: String,
) -> Result<bool, String> {
    let state = state.inner().clone();
    off_thread(move || crate::terminal::detach_session(&state, &session_id)).await
}

/// Kills a session's shell and frees its slot.
#[tauri::command(async)]
pub async fn cmd_terminal_kill(
    state: State<'_, crate::terminal::TerminalSessions>,
    session_id: String,
) -> Result<(), String> {
    let state = state.inner().clone();
    off_thread(move || crate::terminal::kill_session(&state, &session_id)).await
}

/// Exports user-selected terminal output through the native save dialog.
#[tauri::command(async)]
pub async fn cmd_terminal_export(data: String) -> Result<bool, String> {
    if data.len() > 16 * 1024 * 1024 {
        return Err("Terminal export exceeds 16 MiB".into());
    }
    let Some(file) = rfd::AsyncFileDialog::new()
        .set_title("Export retained terminal output")
        .set_file_name("terminal-output.txt")
        .add_filter("Text", &["txt"])
        .save_file()
        .await
    else {
        return Ok(false);
    };
    file.write(data.as_bytes())
        .await
        .map_err(|e| format!("Could not export terminal output: {e}"))?;
    Ok(true)
}

/// Releases rendered output bytes so the PTY reader can continue.
#[tauri::command(async)]
pub async fn cmd_terminal_ack(
    state: State<'_, crate::terminal::TerminalSessions>,
    session_id: String,
    bytes: usize,
) -> Result<(), String> {
    crate::terminal::acknowledge_output(&state, &session_id, bytes)
}

/// Runs one argv to completion with a hard timeout and capped tails.
///
/// Blocking by definition (up to MAX_RUN_TIMEOUT), so this belongs on the
/// same pool as every other long-running reader.
#[tauri::command(async)]
pub async fn cmd_terminal_run(
    repo_path: String,
    args: Vec<String>,
    timeout_secs: Option<u64>,
) -> Result<crate::terminal::TerminalRunResult, String> {
    off_thread(move || crate::terminal::run_terminal(&repo_path, &args, timeout_secs)).await
}

/// Runs one model-authored health or coverage command through the app's
/// purpose-specific allowlist and the MANVI command gate.
///
/// Keeping this separate from `cmd_terminal_run` makes the authority boundary
/// structural: plan text cannot reach the user-owned arbitrary argv console by
/// accidentally omitting a flag or spoofing an origin string.
#[tauri::command(async)]
pub async fn cmd_manvi_run_action(
    repo_path: String,
    args: Vec<String>,
    action_kind: crate::terminal::ManviActionKind,
    timeout_secs: Option<u64>,
) -> Result<crate::terminal::TerminalRunResult, String> {
    off_thread(move || {
        crate::terminal::run_manvi_action(&repo_path, &args, action_kind, timeout_secs)
    })
    .await
}
