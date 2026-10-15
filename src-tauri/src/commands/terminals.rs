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
/// Output streams back on `on_output`, a channel of raw bytes that belongs to
/// this session alone and is open before the shell starts, so its first prompt
/// cannot be missed; exit lands as a `terminal-exit` event keyed by the
/// returned session id, after the channel's last message.
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
    on_output: crate::terminal::OutputChannel,
) -> Result<crate::terminal::TerminalSpawned, String> {
    let state = state.inner().clone();
    off_thread(move || {
        // Expanded here, at the boundary, rather than in the caller: the flags
        // for a mode are the workbench's table and the frontend never carries
        // them. The refusals inside are what make a bypassed session
        // impossible without an acknowledgement, whatever the frontend did or
        // forgot to do. The notification and setting-source flags go in first
        // so the policy flags land in front of them: policy, launch flags,
        // then the caller's prompt.
        //
        // On the blocking pool with the spawn, not before it: the settings
        // file is read and the hook binary is looked for on disk, and an
        // `async fn` doing either parks a Tauri async-runtime worker.
        let defaults = crate::tool_config::agent_defaults();
        let sources = defaults.claude_setting_sources_arg();
        // Before the flags: the hook goes on every claude launch, plugin or not
        // (`workbench::claude_hook`).
        let hook = match program.as_deref() {
            Some(provider) => crate::workbench::claude_hook::for_launch(provider.trim()),
            None => crate::workbench::claude_hook::HookInjection::NotApplicable,
        };
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
                    hook: hook.binary(),
                },
                args,
            )?,
        )?;
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
            crate::terminal::OutputSink::channel(on_output),
        )
    })
    .await
}

/// What a live session is running and where, read once on request: the
/// foreground program, whether it is a job under the shell, and its working
/// directory — inside the repository or not.
///
/// The terminal commands below this one do not use `off_thread`: it is tokio's
/// shared blocking pool, which git commands can hold for minutes, wrapped in a
/// git admission scope a PTY has no use for. They run on `terminal::executor`
/// instead (a lane per session for keystrokes and resizes, a control pool for
/// the rest), and the frontend's five-second deadlines are measured against
/// that, not against the slowest git command in flight.
#[tauri::command(async)]
pub async fn cmd_terminal_context(
    state: State<'_, crate::terminal::TerminalSessions>,
    session_id: String,
) -> Result<crate::terminal::TerminalContext, String> {
    crate::terminal::control_async(&state, "terminal_context", move |state| {
        crate::terminal::session_context(state, &session_id)
    })
    .await
}

/// Every live terminal session, including ones started by a page that has
/// since been reloaded — which the renderer cannot otherwise know exist, and
/// which still count against the shared session limit.
#[tauri::command(async)]
pub async fn cmd_terminal_sessions(
    state: State<'_, crate::terminal::TerminalSessions>,
) -> Result<Vec<crate::terminal::TerminalListing>, String> {
    crate::terminal::control_async(&state, "terminal_sessions", |state| {
        crate::terminal::list_sessions(state)
    })
    .await
}

/// Shows a session a reloaded page left running in a new tab, from now on on
/// `on_output`. Refused for a session a tab still holds.
#[tauri::command(async)]
pub async fn cmd_terminal_attach(
    state: State<'_, crate::terminal::TerminalSessions>,
    session_id: String,
    rows: u16,
    cols: u16,
    on_output: crate::terminal::OutputChannel,
) -> Result<crate::terminal::TerminalSpawned, String> {
    crate::terminal::control_async(&state, "terminal_attach", move |state| {
        crate::terminal::attach_session(
            state,
            &session_id,
            rows,
            cols,
            crate::terminal::OutputSink::channel(on_output),
        )
    })
    .await
}

/// Feeds keystrokes into a live session's PTY, in the order the calls arrive.
#[tauri::command(async)]
pub async fn cmd_terminal_write(
    state: State<'_, crate::terminal::TerminalSessions>,
    session_id: String,
    data: String,
    binary: Option<bool>,
) -> Result<(), String> {
    crate::terminal::write_async(&state, session_id, data, binary.unwrap_or(false)).await
}

/// Resizes a live session's PTY to the frontend grid.
#[tauri::command(async)]
pub async fn cmd_terminal_resize(
    state: State<'_, crate::terminal::TerminalSessions>,
    session_id: String,
    rows: u16,
    cols: u16,
) -> Result<(), String> {
    crate::terminal::resize_async(&state, session_id, rows, cols).await
}

/// Leaves a session running with no view, for a tab that went away without
/// being closed. Returns whether there was a live session to detach.
#[tauri::command(async)]
pub async fn cmd_terminal_detach(
    state: State<'_, crate::terminal::TerminalSessions>,
    session_id: String,
) -> Result<bool, String> {
    crate::terminal::control_async(&state, "terminal_detach", move |state| {
        crate::terminal::detach_session(state, &session_id)
    })
    .await
}

/// Kills a session's shell and frees its slot.
///
/// A close waits out a hangup grace and polls for cleanup for seconds, which is
/// why it is on the control pool and never on a session's input lane.
#[tauri::command(async)]
pub async fn cmd_terminal_kill(
    state: State<'_, crate::terminal::TerminalSessions>,
    session_id: String,
) -> Result<(), String> {
    crate::terminal::control_async(&state, "terminal_kill", move |state| {
        crate::terminal::kill_session(state, &session_id)
    })
    .await
}

/// Mass external-signal deaths recorded this run, newest first and bounded.
///
/// Inline on purpose: one short mutex around an in-memory list, no I/O.
#[tauri::command(async)]
pub async fn cmd_terminal_incidents(
    state: State<'_, crate::terminal::TerminalSessions>,
) -> Result<Vec<crate::terminal::incident::TerminalIncidentPayload>, String> {
    Ok(state.incidents())
}

/// Which backend holds this app's terminals, and what is wrong with the
/// terminal host when one holds them (another version's host still running
/// sessions, a host that would not start). The quit dialog asks whether to stop
/// or leave sessions only when they can outlive the app; Settings says which.
///
/// Inline: two reads of in-memory state, no I/O.
#[tauri::command(async)]
pub async fn cmd_terminal_backend(
    state: State<'_, crate::terminal::TerminalSessions>,
) -> Result<crate::terminal::TerminalBackendStatus, String> {
    Ok(crate::terminal::TerminalBackendStatus {
        backend: state.backend().to_owned(),
        trouble: state.host_trouble(),
    })
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
///
/// Inline on purpose, unlike the commands above: it is a map lookup and one
/// counter update under short locks, no I/O and no wait, and it is sent for
/// every rendered chunk, so a hop to another thread would cost more than the
/// work. Routing it through the session's input lane would also hold the
/// release of output behind a write that is waiting on a stuck child.
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
