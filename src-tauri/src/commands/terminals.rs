//! Embedded terminal commands and Manvi run actions.

use super::*;
use crate::ipc_error::CommandError;

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
    resume: Option<crate::terminal::TerminalResume>,
    session_id: Option<String>,
    reopen_row_id: Option<String>,
    purpose: Option<String>,
    on_output: crate::terminal::OutputChannel,
) -> Result<crate::terminal::TerminalSpawned, IpcError> {
    let state = state.inner().clone();
    off_thread(move || {
        // Judged first, before anything reads settings or spawns: a purpose
        // this build does not know is refused, never ignored. It selects the
        // model (`model_for_launch`) and nothing else below.
        let purpose = crate::tool_config::LaunchPurpose::parse(purpose.as_deref())
            .map_err(CommandError::plain)?;
        // Expanded here, at the boundary, rather than in the caller: the flags
        // for a mode and for resuming/starting a conversation are the
        // workbench's single table (`terminal_command::interactive_args`) and
        // the frontend never carries raw session/resume flags (`S2-07`).
        //
        // On the blocking pool with the spawn, not before it: the settings
        // file is read and the hook binary is looked for on disk, and an
        // `async fn` doing either parks a Tauri async-runtime worker.
        let defaults = crate::tool_config::agent_defaults();
        let sources = defaults.claude_setting_sources_arg();
        let hook = match program.as_deref() {
            Some(provider) => crate::workbench::claude_hook::for_launch(provider.trim()),
            None => crate::workbench::claude_hook::HookInjection::NotApplicable,
        };
        let checkout_for_resume = {
            let base = std::path::PathBuf::from(&repo_path);
            match start_dir
                .as_deref()
                .map(str::trim)
                .filter(|s| !s.is_empty() && *s != ".")
            {
                Some(sub) => base.join(sub),
                None => base,
            }
        };
        let is_fork = resume.as_ref().is_some_and(|r| r.fork);
        let args = match crate::workbench::terminal_command::interactive_args(
            program.as_deref(),
            permission_mode.as_deref(),
            acknowledged.unwrap_or(false),
            &crate::workbench::terminal_command::LaunchOptions {
                notify: crate::tool_config::session_alerts().configure_agents,
                setting_sources: sources.as_deref(),
                model: program
                    .as_deref()
                    .and_then(|launcher| defaults.model_for_launch(launcher, purpose)),
                hook: hook.binary(),
            },
            &checkout_for_resume,
            resume.as_ref().map(|r| r.session_id.as_str()),
            session_id.as_deref(),
            is_fork,
            args,
        ) {
            Ok(built) => built,
            Err(message) => {
                if let Some(ref row_id) = reopen_row_id {
                    if state
                        .session_ledger()
                        .record_reopen_failure(row_id, message.clone())
                        .is_some()
                    {
                        state.session_ledger().emit_view(&app);
                    }
                }
                return Err(CommandError::plain(message));
            }
        };
        let spawned = match crate::terminal::spawn_session_in(
            &app,
            &state,
            &repo_path,
            start_dir.as_deref(),
            rows,
            cols,
            program.clone(),
            args,
            env,
            crate::terminal::OutputSink::channel(on_output),
        ) {
            Ok(spawned) => spawned,
            Err(error) => {
                if let Some(ref row_id) = reopen_row_id {
                    if state
                        .session_ledger()
                        .record_reopen_failure(row_id, error.message().to_owned())
                        .is_some()
                    {
                        state.session_ledger().emit_view(&app);
                    }
                }
                return Err(error);
            }
        };
        let launcher = program
            .as_deref()
            .and_then(crate::terminal::launcher_kind)
            .unwrap_or("shell")
            .to_owned();
        let native_id = if is_fork {
            session_id
        } else {
            resume.map(|r| r.session_id).or(session_id)
        };
        state
            .session_ledger()
            .record_spawn(crate::terminal::session_ledger::NewSessionRow {
                terminal_id: spawned.id.clone(),
                launcher,
                native_id,
                repo_path,
                start_dir,
                cwd: spawned.cwd.clone(),
                mode: permission_mode,
                reopen_row_id,
            });
        state.session_ledger().emit_view(&app);
        Ok(spawned)
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

/// Silent loopback TCP `LISTEN` ports owned by `session_id` and its descendants,
/// queried directly via kernel socket tables without spawning `lsof`.
#[tauri::command(async)]
pub async fn cmd_terminal_listening_ports(
    state: State<'_, crate::terminal::TerminalSessions>,
    session_id: String,
) -> Result<Vec<u16>, String> {
    crate::terminal::control_async(&state, "terminal_listening_ports", move |state| {
        crate::terminal::session_listening_ports(state, &session_id)
    })
    .await
}

/// The newest `max_bytes` of a session's saved output (default and ceiling:
/// `MAX_TRANSCRIPT_TAIL_BYTES`), for a session still running or one that has
/// ended, including one that ended before the app or the terminal host
/// restarted. Read-only: it never attaches. `redact` masks secrets in the
/// host before the bytes leave it. `run_id` names the run the caller is asking
/// about: terminal ids repeat across reboots, so with it a session that a later
/// one reused answers `reused`, never the other session's output. The answer
/// says which of found, missing, unreadable (and why), reused and unavailable
/// (no host) it is; see `terminal::TranscriptRead`.
#[tauri::command(async)]
pub async fn cmd_terminal_transcript(
    state: State<'_, crate::terminal::TerminalSessions>,
    session_id: String,
    max_bytes: Option<usize>,
    redact: Option<bool>,
    run_id: Option<String>,
) -> Result<crate::terminal::TranscriptRead, String> {
    crate::terminal::control_async(&state, "terminal_transcript", move |state| {
        crate::terminal::session_transcript(
            state,
            &session_id,
            max_bytes.unwrap_or(crate::terminal::MAX_TRANSCRIPT_TAIL_BYTES),
            redact.unwrap_or(false),
            run_id.as_deref(),
        )
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

/// Wakes an idle `agy` terminal tab when unread peer messages are waiting in
/// `agent_mailbox`, without claiming mailbox envelopes upfront and only after
/// verifying `"agy"` is the foreground process under the session's child mutex.
#[tauri::command(async)]
pub async fn cmd_terminal_deliver_peer(
    state: State<'_, crate::terminal::TerminalSessions>,
    session_id: String,
    provider_session_id: String,
) -> Result<bool, String> {
    crate::terminal::deliver_peer_wake_async(&state, session_id, provider_session_id).await
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
///
/// `origin` says which route asked (`terminal::KillOrigin`); a caller that
/// does not say is `unspecified` and treated as a direct close. A session that
/// is protected from a batch route (`batch_close`, `stop_children`) is left
/// running and the call answers an error naming it, so a batch can report
/// what it skipped.
#[tauri::command(async)]
pub async fn cmd_terminal_kill(
    app: AppHandle,
    state: State<'_, crate::terminal::TerminalSessions>,
    session_id: String,
    origin: Option<crate::terminal::KillOrigin>,
) -> Result<(), String> {
    let origin = origin.unwrap_or_default();
    let ledger = state.session_ledger().clone();
    let kill_id = session_id.clone();
    let outcome = crate::terminal::control_async(&state, "terminal_kill", move |state| {
        crate::terminal::kill_session(state, &kill_id, origin)
    })
    .await?;
    match outcome {
        crate::terminal::KillOutcome::Killed => {
            if ledger.mark_user_closed(&session_id) {
                ledger.emit_view(&app);
            }
            Ok(())
        }
        crate::terminal::KillOutcome::SkippedProtected => Err(
            crate::terminal::lifecycle::skipped_protected_message(&session_id, origin),
        ),
    }
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

/// Whether Claude Code saved each of these hosted conversations: what decides
/// whether a tab, or an incident's "Resume N", offers to resume one. Found is
/// the only answer that offers it; Unknown is said as such and never read as
/// Absent. At most `MAX_TRANSCRIPT_QUERIES` ids, each a canonical session id.
///
/// On the blocking pool: it reads Claude Code's project directories.
#[tauri::command(async)]
pub async fn cmd_terminal_conversations(
    session_ids: Vec<String>,
) -> Result<Vec<crate::workbench::conversation::ConversationTranscript>, IpcError> {
    off_thread(move || {
        crate::workbench::conversation::transcripts(&session_ids).map_err(CommandError::from)
    })
    .await
}

/// Which backend holds this app's terminals, and what is wrong with the
/// terminal host when one holds them (another version's host still running
/// sessions, a host that would not start). The quit dialog asks whether to stop
/// or leave sessions only when they can outlive the app; Settings says which.
///
/// `managed` says the same of managed agent runs (Codex and Claude driven by
/// Manvi): kept running by a hosted Manvi, or ended with the app, and why.
/// Answering it may ask the installed Manvi whether it can (once per binary),
/// so it is read on the terminal control pool; the terminal half is two reads
/// of in-memory state.
#[tauri::command(async)]
pub async fn cmd_terminal_backend(
    state: State<'_, crate::terminal::TerminalSessions>,
) -> Result<crate::terminal::TerminalBackendStatus, String> {
    crate::terminal::control_async(&state, "terminal_backend", |state| {
        Ok(crate::terminal::TerminalBackendStatus {
            backend: state.backend().to_owned(),
            trouble: state.host_trouble(),
            managed: crate::harness::sidecar::managed_hosting(),
        })
    })
    .await
}

/// Releases rendered output bytes so the PTY reader can continue.
///
/// With the terminal host (all Unix platforms) this is a round trip to that
/// process (answered inline there, so a refused acknowledgement is refused
/// here), and a wait does not belong on an async-runtime worker: it goes to the
/// control pool, never the session's input lane, which would hold the release
/// of output behind a write waiting on a stuck child. On non-Unix in-process
/// targets it runs inline under short locks with no I/O.
#[tauri::command(async)]
pub async fn cmd_terminal_ack(
    state: State<'_, crate::terminal::TerminalSessions>,
    session_id: String,
    bytes: usize,
) -> Result<(), String> {
    #[cfg(unix)]
    {
        crate::terminal::control_async(&state, "terminal_ack", move |state| {
            crate::terminal::acknowledge_output(state, &session_id, bytes)
        })
        .await
    }
    #[cfg(not(unix))]
    {
        crate::terminal::acknowledge_output(&state, &session_id, bytes)
    }
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
) -> Result<crate::terminal::TerminalRunResult, IpcError> {
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
) -> Result<crate::terminal::TerminalRunResult, IpcError> {
    off_thread(move || {
        crate::terminal::run_manvi_action(&repo_path, &args, action_kind, timeout_secs)
    })
    .await
}

/// Decodes a base64 PNG pasted from the user's clipboard into an owner-only
/// (`0600`) scratch file so multimodal terminal agents (`claude`, `agy`, `codex`)
/// can inspect it via `@path`.
#[tauri::command(async)]
pub async fn cmd_terminal_save_clipboard_image(
    tab_id: String,
    png_base64: String,
) -> Result<String, IpcError> {
    const MAX_BASE64_LEN: usize = 24 * 1024 * 1024;
    if png_base64.is_empty() {
        return Err("Clipboard image payload is empty.".into());
    }
    if png_base64.len() > MAX_BASE64_LEN {
        return Err("Clipboard image exceeds 16 MiB.".into());
    }
    off_thread(move || {
        use base64::Engine as _;
        let trimmed = png_base64.trim();
        let bytes = base64::engine::general_purpose::STANDARD
            .decode(trimmed)
            .map_err(|e| CommandError::plain(format!("Invalid clipboard PNG base64: {e}")))?;
        let path = crate::terminal::scratch_media::write_scratch_png("clipboard", &tab_id, &bytes)
            .map_err(CommandError::plain)?;
        Ok(path.to_string_lossy().into_owned())
    })
    .await
}

/// Reads copied local file paths (`public.file-url`) from the native system
/// clipboard so `⌘C` in Finder -> `⌘V` in a GitPulse terminal attaches `@path`
/// or POSIX-escaped file arguments.
#[tauri::command(async)]
pub async fn cmd_terminal_clipboard_files(app: tauri::AppHandle) -> Result<Vec<String>, String> {
    #[cfg(target_os = "macos")]
    {
        crate::desktop::on_main_thread(app, |_| {
            Ok(crate::terminal::scratch_media::read_clipboard_file_paths())
        })
        .await
    }
    #[cfg(not(target_os = "macos"))]
    {
        let _ = app;
        Ok(crate::terminal::scratch_media::read_clipboard_file_paths())
    }
}

/// Reads the highest-priority Markdown plan or artifact from the artifact
/// directory reported by an agent (`agy` / `claude`) running inside `session_id`.
#[tauri::command(async)]
pub async fn cmd_terminal_read_agent_artifact(
    state: State<'_, crate::terminal::TerminalSessions>,
    session_id: String,
    hint_artifact_dir: Option<String>,
) -> Result<crate::terminal::AgentArtifactRead, String> {
    crate::terminal::control_async(&state, "terminal_read_agent_artifact", move |state| {
        crate::terminal::read_agent_artifact(state, &session_id, hint_artifact_dir.as_deref())
    })
    .await
}

/// Returns the durable backend ledger of interrupted hosted tabs and whether
/// each row can currently be resumed (`S2-03`, `S2-05`).
#[tauri::command(async)]
pub async fn cmd_terminal_session_ledger(
    state: State<'_, crate::terminal::TerminalSessions>,
) -> Result<crate::terminal::SessionLedgerView, IpcError> {
    let state = state.inner().clone();
    off_thread(move || Ok(state.session_ledger().view())).await
}

/// Dismisses one recovery row (or corrupt ledger row) from the durable
/// ledger (`S2-05`) and pushes the updated view.
#[tauri::command(async)]
pub async fn cmd_terminal_session_ledger_dismiss(
    app: AppHandle,
    state: State<'_, crate::terminal::TerminalSessions>,
    row_id: String,
) -> Result<crate::terminal::SessionLedgerView, IpcError> {
    let state = state.inner().clone();
    off_thread(move || {
        let changed = state.session_ledger().dismiss(&row_id);
        let view = state.session_ledger().view();
        if changed {
            state.session_ledger().emit_view(&app);
        }
        Ok(view)
    })
    .await
}

/// Verifies whether one ledger row can currently be reopened (transcript
/// present and working directory intact), recording any per-row failure while
/// keeping the row listed (`S2-03`, `S2-07`).
#[tauri::command(async)]
pub async fn cmd_terminal_session_ledger_reopen_check(
    app: AppHandle,
    state: State<'_, crate::terminal::TerminalSessions>,
    row_id: String,
) -> Result<crate::terminal::SessionReopenCheck, IpcError> {
    let state = state.inner().clone();
    off_thread(move || {
        let checked = state
            .session_ledger()
            .check_reopen(&row_id)
            .map_err(CommandError::plain)?;
        if !checked.offer.resumable {
            state.session_ledger().emit_view(&app);
        }
        Ok(checked)
    })
    .await
}

/// Persists a stabilized live or user-renamed session title in the durable
/// session ledger so detached listings and crash/quit recovery offers retain
/// the session's human-meaningful title across reloads and restarts.
///
/// The same command carries the session's kill protection: `protected: true`
/// spares it from batch closes, "Stop children", agent closes and the load
/// governor (see `terminal::KillOrigin::respects_protection`); it is kept on
/// the ledger row so it survives a reload and a restart.
#[tauri::command(async)]
pub async fn cmd_terminal_set_title(
    app: AppHandle,
    state: State<'_, crate::terminal::TerminalSessions>,
    session_id: String,
    title: Option<String>,
    user_named: Option<bool>,
    protected: Option<bool>,
) -> Result<bool, IpcError> {
    let state = state.inner().clone();
    off_thread(move || {
        // `protected` set means this call is about protection: the title is
        // only touched when one is given, so a caller can flip the flag
        // without clearing the name.
        if let Some(protected) = protected {
            let flag_changed = state.session_ledger().set_protected(&session_id, protected);
            state.session_ledger().record_lifecycle(
                "protection",
                serde_json::json!({
                    "session_id": session_id,
                    "protected": protected,
                    "changed": flag_changed,
                }),
            );
            if title.is_none() {
                return Ok(flag_changed);
            }
        }
        let (changed, offer_changed) = state.session_ledger().set_title(
            &session_id,
            title.as_deref(),
            user_named.unwrap_or(false),
        );
        if offer_changed {
            state.session_ledger().emit_view(&app);
        }
        Ok(changed)
    })
    .await
}
