//! Mavi: a chat-first surface for coding agents.
//!
//! Every other agent GitPulse hosts is a terminal. Mavi runs Claude Code in
//! its structured mode (`--input-format stream-json --output-format
//! stream-json --permission-prompt-tool stdio`) and hands the page typed
//! lines, so the page can render a timeline, inline permission requests and
//! the files each turn changed, instead of a screen of text.
//!
//! The agent runs on this computer, in a repository GitPulse trusts, or on
//! another machine through the system `ssh` ([`ssh`]). The process lives in
//! this app: quitting GitPulse ends it, and reopening a thread resumes the
//! conversation by its id (`--resume`), which the agent keeps in its own
//! transcript.
//!
//! **Authority.** The permission mode is expanded only by the shared policy
//! table (`workbench::terminal_command::apply_permission_mode`), so `bypass`
//! needs the same acknowledgement as anywhere else and the default is `ask`.
//! A permission answer carries only a request id and allow or deny; the input
//! the agent proposed is echoed back by this host from the request it
//! received ([`wire::Pending`]). There is no "allow for the session": the
//! suggestion Claude Code sends with a request can raise the whole session's
//! mode, and that choice belongs to the thread's mode, made in the open.
//!
//! **One live local thread per worktree.** Two agents in one checkout would
//! make every "what this turn changed" answer a mix of both. A second thread
//! for the same worktree is refused until the first ends; use a worktree.

pub mod history;
pub mod session;
pub mod ssh;
pub(crate) mod wire;
pub(crate) mod worktree;

use crate::commands::off_thread;
use crate::ipc_error::{CommandError, IpcError};
use serde::{Deserialize, Serialize};
use session::{MaviSummary, MaviTarget, Session};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

/// The most threads alive at once.
const MAX_LIVE: usize = 12;
/// The most sessions (live or ended) the host remembers.
const MAX_KEPT: usize = 48;

/// The arguments that put Claude Code in Mavi's structured mode.
const CLAUDE_STRUCTURED: [&str; 9] = [
    "-p",
    "--input-format",
    "stream-json",
    "--output-format",
    "stream-json",
    "--verbose",
    "--include-partial-messages",
    "--permission-prompt-tool",
    "stdio",
];

#[derive(Default, Clone)]
pub struct MaviState {
    sessions: Arc<Mutex<HashMap<String, Arc<Session>>>>,
    counter: Arc<AtomicU64>,
    /// Worktrees and conversations a start in progress has claimed, so two
    /// quick starts (a double click) cannot both pass the checks.
    starting: Arc<Mutex<std::collections::HashSet<String>>>,
}

/// Releases a start's claims however the start ends.
struct Claim {
    set: Arc<Mutex<std::collections::HashSet<String>>>,
    keys: Vec<String>,
}

impl Drop for Claim {
    fn drop(&mut self) {
        let mut set = self.set.lock().unwrap_or_else(|p| p.into_inner());
        for key in &self.keys {
            set.remove(key);
        }
    }
}

impl MaviState {
    fn get(&self, id: &str) -> Result<Arc<Session>, CommandError> {
        self.sessions
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .get(id)
            .cloned()
            .ok_or_else(|| CommandError::plain("That Mavi thread is not running in this window."))
    }

    fn all(&self) -> Vec<Arc<Session>> {
        self.sessions
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .values()
            .cloned()
            .collect()
    }
}

/// Stops every agent. Called when the app exits, so no agent outlives it:
/// the grace period is waited here (a detached thread would not outlive the
/// app), then whatever is left is killed.
pub fn stop_all(state: &MaviState) {
    let live: Vec<Arc<Session>> = state
        .all()
        .into_iter()
        .filter(|s| !s.has_exited())
        .collect();
    for session in &live {
        session.stop();
    }
    let deadline = std::time::Instant::now() + session::STOP_GRACE;
    while std::time::Instant::now() < deadline && live.iter().any(|s| !s.has_exited()) {
        std::thread::sleep(Duration::from_millis(50));
    }
    for session in &live {
        session.kill_now();
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct StartRequest {
    /// Local: the repository or worktree to run in.
    #[serde(default)]
    repo_path: Option<String>,
    /// Remote: an ssh destination and a directory on it.
    #[serde(default)]
    host: Option<String>,
    #[serde(default)]
    dir: Option<String>,
    mode: String,
    #[serde(default)]
    acknowledged: bool,
    #[serde(default)]
    model: Option<String>,
    /// A conversation to continue.
    #[serde(default)]
    resume: Option<String>,
    /// The page's thread key (a UUID, stable across resumes). On a local start
    /// it becomes the agent's orchestrate owner key.
    #[serde(default)]
    thread_key: Option<String>,
}

/// The owner key a local agent orchestrates as, from the page's thread key:
/// `None` for a missing or non-UUID key, so only a stable thread identity is
/// ever handed to the agent's `gitpulse-mcp`.
fn owner_key_for(request: &StartRequest) -> Option<String> {
    request
        .thread_key
        .as_deref()
        .and_then(crate::terminal::orchestrate::mavi_owner_key)
}

fn claude_args(request: &StartRequest) -> Result<Vec<String>, String> {
    // An unanswered question is not consent, and a terminal agent waiting on
    // the person must be named to them: said to the agent up front.
    let mut args: Vec<String> = vec![
        "--append-system-prompt".to_owned(),
        wire::AGENT_GUIDANCE.to_owned(),
    ];
    args.extend(CLAUDE_STRUCTURED.iter().map(|s| (*s).to_owned()));
    if let Some(id) = request
        .resume
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
    {
        // The resume flags have one owner; Mavi asks it rather than spelling them.
        args.extend(crate::workbench::terminal_command::resume(
            "claude",
            id,
            Path::new(""),
        )?);
    }
    if let Some(model) = request
        .model
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
    {
        crate::workbench::terminal_command::validate_model_id("model", model)?;
        args.push("--model".into());
        args.push(model.to_owned());
    }
    let expanded = crate::workbench::terminal_command::apply_permission_mode(
        Some("claude"),
        Some(&request.mode),
        request.acknowledged,
        Some(args),
    )?;
    Ok(expanded.unwrap_or_default())
}

/// `program` resolved on the PATH a GUI launch can see, as an absolute path.
fn resolve(program: &str) -> Result<(String, Option<std::ffi::OsString>), String> {
    let home = std::env::var_os("HOME").or_else(|| std::env::var_os("USERPROFILE"));
    let path = crate::engine::process::extended_child_path(
        std::env::var_os("PATH").as_deref(),
        home.as_deref(),
    );
    let resolved = crate::engine::process::resolve_spawn_program_with(
        program,
        path.as_deref().or(std::env::var_os("PATH").as_deref()),
        home.as_deref(),
    );
    if !Path::new(&resolved).is_absolute() || !Path::new(&resolved).is_file() {
        return Err(match program {
            "claude" => "Claude Code is not installed, or not on the PATH GitPulse sees.".into(),
            other => format!("{other} is not installed."),
        });
    }
    Ok((resolved, path))
}

fn command_for(program: &str, path: Option<std::ffi::OsString>) -> Command {
    let mut command = Command::new(program);
    if let Some(path) = path {
        command.env("PATH", path);
    }
    // A GitPulse started from inside an agent session must not make the
    // child believe it is nested in that session.
    for name in [
        "CLAUDECODE",
        "CLAUDE_CODE_ENTRYPOINT",
        "CLAUDE_CODE_SSE_PORT",
    ] {
        command.env_remove(name);
    }
    // Nor may a GitPulse started from one of its own terminals hand the agent
    // that terminal's id without its token (or the reverse), or an owner key
    // of its own: the agent's identity is the one `start` sets, or none.
    for name in [
        crate::terminal::TERMINAL_SESSION_ENV,
        crate::terminal::orchestrate::SESSION_TOKEN_ENV,
        crate::alerts::bridge::SESSION_ENV,
        crate::terminal::orchestrate::ORCHESTRATE_CALLER_ENV,
    ] {
        command.env_remove(name);
    }
    command
}

fn worktree_root(repo_path: &str) -> Result<PathBuf, String> {
    let dir =
        std::fs::canonicalize(repo_path).map_err(|_| "That folder does not exist.".to_string())?;
    // The nearest ancestor holding `.git` (a directory, or a linked worktree's
    // `.git` file) is what `git rev-parse --show-toplevel` answers. Found
    // without running git, because nothing runs in a repository before its
    // trust is checked, and that check needs the root this finds.
    let root = dir
        .ancestors()
        .find(|candidate| candidate.join(".git").exists())
        .ok_or_else(|| "That folder is not in a Git repository.".to_string())?;
    Ok(root.to_path_buf())
}

fn start(
    state: &MaviState,
    request: StartRequest,
    emit: session::Emit,
) -> Result<MaviSummary, CommandError> {
    let args = claude_args(&request)?;
    let (target, command) = match (&request.repo_path, &request.host) {
        (Some(repo), None) => {
            let root = worktree_root(repo)?;
            crate::repository_trust::require(&root)?;
            // Trust is judged on the canonical spelling; the agent and its
            // reports use the ordinary one (no `\\?\` prefix on Windows),
            // so that is the root its paths are matched against.
            let root_text = crate::engine::process::plain_path_text(&root.to_string_lossy());
            let (program, path) = resolve("claude")?;
            let mut command = command_for(&program, path);
            command.args(&args).current_dir(&root_text);
            if let Some(owner) = owner_key_for(&request) {
                command.env(crate::terminal::orchestrate::ORCHESTRATE_CALLER_ENV, owner);
            }
            (MaviTarget::Local { root: root_text }, command)
        }
        (None, Some(host)) => {
            let dir = request.dir.as_deref().unwrap_or("~").trim().to_owned();
            if !ssh::valid_host(host) {
                return Err(CommandError::plain(
                    "That is not a host name ssh accepts here.",
                ));
            }
            if !ssh::valid_remote_dir(&dir) {
                return Err(CommandError::plain(
                    "Give the remote directory as an absolute or ~/ path.",
                ));
            }
            let (program, path) = resolve("ssh")?;
            let mut command = command_for(&program, path);
            command.args(ssh::ssh_args(
                host,
                &ssh::remote_command(&dir, "claude", &args),
            ));
            (
                MaviTarget::Ssh {
                    host: host.clone(),
                    dir,
                },
                command,
            )
        }
        _ => {
            return Err(CommandError::plain(
                "Choose this computer or an ssh host, not both.",
            ))
        }
    };
    let resume_id = request
        .resume
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_ascii_lowercase);
    let mut keys = Vec::new();
    if let MaviTarget::Local { root } = &target {
        keys.push(format!("root:{root}"));
    }
    if let Some(id) = &resume_id {
        keys.push(format!("conv:{id}"));
    }
    // The checks and the claim happen under one lock, and the claim is held
    // until the session is registered, so two quick starts cannot both pass.
    let _claim = {
        let sessions = state.sessions.lock().unwrap_or_else(|p| p.into_inner());
        let mut starting = state.starting.lock().unwrap_or_else(|p| p.into_inner());
        if keys.iter().any(|key| starting.contains(key)) {
            return Err(CommandError::plain(
                "A thread is already starting for this worktree or conversation.",
            ));
        }
        if let MaviTarget::Local { root } = &target {
            let busy = sessions.values().any(|s| {
                s.is_live()
                    && matches!(&s.target, MaviTarget::Local { root: other } if other == root)
            });
            if busy {
                return Err(CommandError::plain(
                    "A Mavi thread is already working in this worktree. Stop it, or start this one in another worktree.",
                ));
            }
        }
        // One process per conversation: two would append to one transcript.
        // A session that has not printed its `init` yet is matched by the id
        // it was started to resume.
        if let Some(resume) = &resume_id {
            let open_here = sessions.values().any(|s| {
                let summary = s.summary();
                summary.phase != session::MaviPhase::Ended
                    && (summary
                        .conversation_id
                        .as_deref()
                        .is_some_and(|id| id.eq_ignore_ascii_case(resume))
                        || s.resumed.as_deref() == Some(resume.as_str()))
            });
            if open_here {
                return Err(CommandError::plain(
                    "This conversation is already open in another Mavi thread.",
                ));
            }
        }
        if sessions.values().filter(|s| s.is_live()).count() + starting.len() >= MAX_LIVE {
            return Err(CommandError::plain(format!(
                "{MAX_LIVE} Mavi threads are already running. Stop one first."
            )));
        }
        for key in &keys {
            starting.insert(key.clone());
        }
        Claim {
            set: Arc::clone(&state.starting),
            keys,
        }
    };
    let id = format!(
        "mavi-{}-{}",
        session::now_ms(),
        state.counter.fetch_add(1, Ordering::Relaxed)
    );
    let model = request.model.clone().filter(|m| !m.trim().is_empty());
    let mut history = earlier_turns(&request, &target)?;
    history.resumed = resume_id;
    let session = Session::spawn(
        id.clone(),
        target,
        request.mode.clone(),
        model,
        command,
        emit,
        history,
    )?;
    let summary = session.summary();
    let mut sessions = state.sessions.lock().unwrap_or_else(|p| p.into_inner());
    sessions.insert(id, session);
    // Forget the oldest ended threads past the bound.
    if sessions.len() > MAX_KEPT {
        let mut ended: Vec<(u64, String)> = sessions
            .values()
            .filter(|s| !s.is_live())
            .map(|s| (s.started_at_ms, s.id.clone()))
            .collect();
        ended.sort();
        for (_, old) in ended.into_iter().take(sessions.len() - MAX_KEPT) {
            sessions.remove(&old);
        }
    }
    Ok(summary)
}

/// A resumed conversation's earlier turns, from Claude Code's transcript.
///
/// Refuses a local resume whose transcript was saved for another directory:
/// Claude Code resumes a conversation only from where it was saved, so the
/// agent would start without it and the thread would quietly be a new one.
fn earlier_turns(
    request: &StartRequest,
    target: &MaviTarget,
) -> Result<history::History, CommandError> {
    let Some(id) = request
        .resume
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
    else {
        return Ok(history::History::default());
    };
    let id = id.to_ascii_lowercase();
    match target {
        MaviTarget::Local { root } => {
            let root = Path::new(root);
            let found = history::check(root, &id);
            match found.state {
                history::MaviTranscriptState::Found => Ok(history::load(root, &id)
                    .unwrap_or_else(|reason| history::unavailable(&reason))),
                history::MaviTranscriptState::Elsewhere => Err(CommandError::plain(
                    found.reason.unwrap_or_else(|| "This conversation belongs to another directory.".into()),
                )),
                _ => Ok(history::unavailable(
                    found.reason.as_deref().unwrap_or("The transcript could not be found."),
                )),
            }
        }
        MaviTarget::Ssh { .. } => Ok(history::unavailable(
            "The earlier messages of a conversation on an SSH host stay on that host; this view starts here.",
        )),
    }
}

fn tauri_emit(app: tauri::AppHandle) -> session::Emit {
    use tauri::Emitter;
    Arc::new(move |name, payload| {
        let _ = app.emit(name, payload);
    })
}

/// Starts a Mavi thread's agent.
#[tauri::command(async)]
pub async fn cmd_mavi_start(
    app: tauri::AppHandle,
    state: tauri::State<'_, MaviState>,
    request: StartRequest,
) -> Result<MaviSummary, IpcError> {
    let state = state.inner().clone();
    let emit = tauri_emit(app);
    off_thread(move || start(&state, request, emit)).await
}

/// Sends one user turn; returns the sequence number of its timeline line.
#[tauri::command(async)]
pub async fn cmd_mavi_send(
    state: tauri::State<'_, MaviState>,
    id: String,
    text: String,
) -> Result<u64, IpcError> {
    let state = state.inner().clone();
    off_thread(move || Ok(state.get(&id)?.send(&text)?)).await
}

/// Answers one permission request with `allow` or `deny`.
#[tauri::command(async)]
pub async fn cmd_mavi_answer(
    state: tauri::State<'_, MaviState>,
    id: String,
    request_id: String,
    decision: String,
    message: Option<String>,
) -> Result<(), IpcError> {
    let state = state.inner().clone();
    off_thread(move || {
        let decision = wire::Decision::parse(&decision)
            .ok_or_else(|| CommandError::plain("Answer allow or deny."))?;
        if message
            .as_ref()
            .is_some_and(|m| m.len() > wire::MAX_TEXT_BYTES)
        {
            return Err(CommandError::plain("The note is too long."));
        }
        Ok(state
            .get(&id)?
            .answer(&request_id, decision, message.as_deref())?)
    })
    .await
}

/// Interrupts the running turn.
#[tauri::command(async)]
pub async fn cmd_mavi_interrupt(
    state: tauri::State<'_, MaviState>,
    id: String,
) -> Result<(), IpcError> {
    let state = state.inner().clone();
    off_thread(move || Ok(state.get(&id)?.interrupt()?)).await
}

/// Ends a thread's agent.
#[tauri::command(async)]
pub async fn cmd_mavi_stop(state: tauri::State<'_, MaviState>, id: String) -> Result<(), IpcError> {
    let state = state.inner().clone();
    off_thread(move || {
        state.get(&id)?.stop();
        Ok(())
    })
    .await
}

/// Every thread this window's host knows, live or ended.
#[tauri::command(async)]
pub async fn cmd_mavi_list(
    state: tauri::State<'_, MaviState>,
) -> Result<Vec<MaviSummary>, IpcError> {
    let state = state.inner().clone();
    off_thread(move || {
        let mut all: Vec<MaviSummary> = state.all().iter().map(|s| s.summary()).collect();
        all.sort_by_key(|s| std::cmp::Reverse(s.started_at_ms));
        Ok(all)
    })
    .await
}

#[derive(Serialize)]
pub struct MaviReplay {
    entries: Vec<session::MaviEntry>,
    next_seq: u64,
    summary: MaviSummary,
}

/// The kept timeline lines after `after`, for a page that mounts or reloads
/// while a thread runs.
#[tauri::command(async)]
pub async fn cmd_mavi_events(
    state: tauri::State<'_, MaviState>,
    id: String,
    after: u64,
) -> Result<MaviReplay, IpcError> {
    let state = state.inner().clone();
    off_thread(move || {
        let session = state.get(&id)?;
        let (entries, next_seq) = session.replay(after);
        Ok(MaviReplay {
            entries,
            next_seq,
            summary: session.summary(),
        })
    })
    .await
}

#[derive(Serialize)]
pub struct MaviFileDiff {
    patch: String,
    truncated: bool,
}

/// One file's patch against `HEAD` in a thread's worktree, local or remote.
#[tauri::command(async)]
pub async fn cmd_mavi_file_diff(
    state: tauri::State<'_, MaviState>,
    id: String,
    path: String,
) -> Result<MaviFileDiff, IpcError> {
    let state = state.inner().clone();
    off_thread(move || {
        let session = state.get(&id)?;
        match &session.target {
            MaviTarget::Local { root } => {
                let root = PathBuf::from(root);
                let path = relative_to(&root, &path);
                let (patch, truncated) = worktree::file_diff(&root, &path)?;
                Ok(MaviFileDiff { patch, truncated })
            }
            MaviTarget::Ssh { host, dir } => {
                // Relative to the thread's directory only: an absolute path
                // would let a diff request read any file on the host.
                if !worktree::safe_relative(&path) {
                    return Err(CommandError::plain(
                        "That path is not inside the remote directory.",
                    ));
                }
                if path.contains('\0') || path.contains('\n') {
                    return Err(CommandError::plain("That path cannot be shown."));
                }
                let (program, env_path) = resolve("ssh")?;
                let args = ssh::ssh_args(host, &ssh::remote_diff_command(dir, &path));
                let refs: Vec<&str> = args.iter().map(String::as_str).collect();
                let _ = env_path;
                let (ok, bytes, truncated) = worktree::run_capped(
                    &program,
                    &refs,
                    None,
                    Duration::from_secs(30),
                    worktree::MAX_DIFF_BYTES,
                )?;
                if !ok && bytes.is_empty() {
                    return Err(CommandError::plain(
                        "Couldn't read that file's diff from the host.",
                    ));
                }
                let patch = dc_redact::redact_secrets(&String::from_utf8_lossy(&bytes));
                Ok(MaviFileDiff { patch, truncated })
            }
        }
    })
    .await
}

/// A path the page holds (absolute, from a tool call, or relative, from the
/// turn's fingerprint) as a path relative to the worktree root.
fn relative_to(root: &Path, path: &str) -> String {
    worktree::relative_text(&root.to_string_lossy(), path).unwrap_or_else(|| path.to_owned())
}

/// Whether a conversation can be resumed in `repo_path`: its transcript is
/// saved for that checkout, for another directory, or not at all. Asked before
/// a terminal tab is ended to move its conversation here.
#[tauri::command(async)]
pub async fn cmd_mavi_conversation_check(
    repo_path: String,
    conversation_id: String,
) -> Result<history::MaviConversationCheck, IpcError> {
    off_thread(move || {
        if !ssh::is_uuid(conversation_id.trim()) {
            return Err(CommandError::plain("A conversation id is a UUID."));
        }
        let root = worktree_root(&repo_path)?;
        Ok(history::check(
            &root,
            &conversation_id.trim().to_ascii_lowercase(),
        ))
    })
    .await
}

/// The concrete hosts in `~/.ssh/config`.
#[tauri::command(async)]
pub async fn cmd_mavi_ssh_hosts() -> Result<Vec<String>, IpcError> {
    off_thread(move || Ok(ssh::list_hosts())).await
}

/// Whether Mavi can run in `dir` on `host`, as a typed answer.
#[tauri::command(async)]
pub async fn cmd_mavi_ssh_probe(host: String, dir: String) -> Result<ssh::MaviProbe, IpcError> {
    off_thread(move || Ok(ssh::probe(&host, &dir))).await
}

#[cfg(test)]
mod tests {
    use super::*;

    fn request(mode: &str, acknowledged: bool) -> StartRequest {
        StartRequest {
            repo_path: None,
            host: None,
            dir: None,
            mode: mode.into(),
            acknowledged,
            model: None,
            resume: None,
            thread_key: None,
        }
    }

    #[test]
    fn only_a_uuid_thread_key_names_an_owner() {
        let key = "0f8e2b9a-1c3d-4e5f-8a6b-7c8d9e0f1a2b";
        let mut req = request("ask", false);
        assert_eq!(owner_key_for(&req), None);
        req.thread_key = Some("t1795634163229abc".into());
        assert_eq!(owner_key_for(&req), None);
        req.thread_key = Some(key.into());
        assert_eq!(owner_key_for(&req), Some(format!("ext-mavi-{key}")));
        let parsed: StartRequest = serde_json::from_value(
            serde_json::json!({ "repoPath": "/r", "mode": "ask", "threadKey": key }),
        )
        .expect("threadKey is accepted");
        assert_eq!(parsed.thread_key.as_deref(), Some(key));
    }

    #[test]
    fn the_agent_never_inherits_a_terminal_callers_credentials() {
        // A GitPulse started from one of its own terminals carries that
        // terminal's id and token; the Mavi agent must see neither half, nor a
        // stale owner key from the environment GitPulse was started in.
        let command = command_for("claude", None);
        let removed: Vec<_> = command
            .get_envs()
            .filter(|(_, value)| value.is_none())
            .map(|(name, _)| name.to_string_lossy().into_owned())
            .collect();
        for name in [
            crate::terminal::TERMINAL_SESSION_ENV,
            crate::terminal::orchestrate::SESSION_TOKEN_ENV,
            crate::alerts::bridge::SESSION_ENV,
            crate::terminal::orchestrate::ORCHESTRATE_CALLER_ENV,
        ] {
            assert!(
                removed.iter().any(|r| r == name),
                "{name} is inherited: {removed:?}"
            );
        }
    }

    #[test]
    fn the_agent_is_told_an_unanswered_question_is_not_consent() {
        let args = claude_args(&request("ask", false)).unwrap();
        let at = args
            .iter()
            .position(|a| a == "--append-system-prompt")
            .expect("the agent guidance is appended to the system prompt");
        assert_eq!(args[at + 1], wire::AGENT_GUIDANCE);
    }

    #[test]
    fn the_mode_comes_from_the_shared_policy_table() {
        let args = claude_args(&request("ask", false)).unwrap();
        assert_eq!(&args[..2], ["--permission-mode", "manual"]);
        assert!(args.ends_with(&["--permission-prompt-tool".to_string(), "stdio".to_string()]));
        assert!(
            claude_args(&request("bypass", false)).is_err(),
            "bypass needs acknowledgement"
        );
        assert!(
            claude_args(&request("ask", true)).is_err(),
            "only bypass takes one"
        );
        assert!(claude_args(&request("yolo", false)).is_err());
    }

    #[test]
    fn resume_ids_and_models_are_validated() {
        let mut r = request("edit", false);
        r.resume = Some("0D235A3D-03CF-44A9-AD04-EFFB59321456".into());
        let args = claude_args(&r).unwrap();
        let at = args.iter().position(|a| a == "--resume").unwrap();
        assert_eq!(args[at + 1], "0d235a3d-03cf-44a9-ad04-effb59321456");
        r.resume = Some("--dangerously-skip-permissions".into());
        assert!(claude_args(&r).is_err());
        let mut m = request("edit", false);
        m.model = Some("--settings".into());
        assert!(claude_args(&m).is_err());
    }

    #[test]
    fn paths_are_made_relative_to_the_root() {
        let root = Path::new("/r/app");
        assert_eq!(relative_to(root, "/r/app/src/a.rs"), "src/a.rs");
        assert_eq!(relative_to(root, "src/a.rs"), "src/a.rs");
        assert_eq!(relative_to(root, "/elsewhere/x"), "/elsewhere/x");
    }
}
