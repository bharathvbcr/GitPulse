//! One GitPulse per profile on macOS, whichever copy of the app was launched.
//!
//! LaunchServices does not guarantee one instance of a bundle identifier. It
//! guarantees one instance of a bundle *path*: a machine with
//! `/Applications/GitPulse.app`, a build under `target/release/bundle`, and a
//! mounted disk image has three apps that are each happy to start. A banner
//! click is routed to whichever copy LaunchServices prefers, which need not be
//! the copy that is running — and once the running copy's bundle has been
//! deleted by a clean build it never can be. Each such click started a whole
//! second GitPulse: another window, another set of watchers and another
//! code-intelligence build over the same repositories and the same profile.
//!
//! So ownership is decided by the profile, not by LaunchServices:
//!
//! - `flock` on [`LOCK_FILE`] in the profile directory picks the owner. The
//!   kernel releases it when the owner dies however it dies, and two launches
//!   racing for it cannot both win — unlike "is the socket there", which two
//!   launches can both answer "no" to.
//! - A unix socket beside it is the owner's mailbox. A launch that loses the
//!   lock hands the owner what it was started to do and exits without ever
//!   building a window.
//!
//! The owner holding the lock is not by itself proof it is answering, and a
//! lock file can be deleted from under its holder. So a launch that *wins* the
//! lock still asks the mailbox first, and defers to a live answer there; and a
//! launch that *loses* it waits, bounded, for the mailbox — the owner may be
//! starting (lock taken, socket not yet bound) or quitting (socket gone, lock
//! not yet released). Neither wait ends in a second instance: an owner that
//! never answers is reported as [`Claim::Blocked`] and the launch gives up.

use std::collections::VecDeque;
use std::fs::{File, OpenOptions};
use std::io::{BufRead, BufReader, ErrorKind, Read, Write};
use std::os::unix::fs::{MetadataExt, OpenOptionsExt};
use std::os::unix::io::AsRawFd;
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Condvar, Mutex, PoisonError};
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};

use crate::local_socket::Wake;

pub const LOCK_FILE: &str = "instance.lock";
pub const SOCKET_FILE: &str = "instance.sock";

/// Bumped on any incompatible change to the frames below. A launch that meets
/// an owner speaking another version still never starts a second instance.
const PROTOCOL: u32 = 1;
/// How long a hook waits for the app's answer to [`Request::RecordGitOp`], and
/// so how long the app has, from the moment it receives that request, before
/// its asker has stopped waiting. The one budget both sides read: the hook
/// waits on it, and the app refuses to capture state once it has passed. Not a
/// request field, so the wire shape is unchanged.
pub const OP_RECORD_DEADLINE: Duration = Duration::from_secs(2);
/// Every frame is one JSON line no longer than this: room for [`MAX_PATHS`]
/// paths of [`MAX_PATH_BYTES`] even when every byte is a control character
/// JSON spells as six (`\u0001`). `forward` checks the encoded size anyway,
/// so a request this cannot carry fails at the sender.
const MAX_FRAME: usize = 128 * 1024;
const MAX_PATHS: usize = 16;
/// `PATH_MAX` on macOS.
const MAX_PATH_BYTES: usize = 1024;
/// The longest command line a hook may ask the app to judge. A longer one is
/// judged by the hook's own path, which has no frame to fit in.
const MAX_GATE_COMMAND_BYTES: usize = 48 * 1024;
/// The longest label a turn-checkpoint request may carry.
const MAX_CHECKPOINT_LABEL_BYTES: usize = 4 * 1024;
/// Native notification identifiers are well under this; see
/// `alerts::native_session_key` and `workbench::notifications::owned`, which
/// validate them properly on the owner's side.
const MAX_NATIVE_ID: usize = 256;
/// Connections answered at once. A connection past this is closed unanswered
/// and its sender retries until its own deadline.
const MAX_HANDLERS: usize = 8;
/// Hook verdicts in flight at once, counted on top of [`MAX_HANDLERS`] so a
/// burst of agent hooks can never use up the handlers a relaunch needs. One
/// past this is answered "busy" and its hook judges the command itself.
const MAX_GATE_HANDLERS: usize = 16;
/// Insights reads in flight at once: a share of [`MAX_GATE_HANDLERS`], not on
/// top of it. A snapshot's reads are heavier than a verdict or one file's
/// collision scan, and every agent session has an MCP process that may ask.
const MAX_INSIGHTS_HANDLERS: usize = 4;
/// Terminal orchestration requests (`Wait`, `Spawn`, `Read`, `Send`) in flight
/// at once, counted in their own budget so blocking `Wait` calls can neither
/// starve hooks nor block app relaunches.
const MAX_ORCHESTRATE_HANDLERS: usize = 8;
/// Agent A2A deliveries/kickoffs in flight at once: a share of
/// [`MAX_GATE_HANDLERS`], bounded inside [`HANDLER_TIMEOUT`].
const MAX_DELIVER_HANDLERS: usize = 4;
/// Turn checkpoint captures in flight at once: a share of
/// [`MAX_GATE_HANDLERS`], so the 14 slots left are always a verdict's. A capture
/// outlives its hook's 1 s wait (scan bound plus git calls of up to 90 s each)
/// and is asked at both boundaries of every turn, so it must not be able to
/// hold more than a sliver. Two lets one worktree's start and another's end
/// overlap; a third request is answered "busy" and its hook reports "not
/// captured". Captures of one worktree already coalesce in the engine.
const MAX_CHECKPOINT_HANDLERS: usize = 2;
/// Longest prompt text an unattended `gitpulse_kickoff_agent` spawn may pass.
const MAX_KICKOFF_PROMPT_BYTES: usize = 16 * 1024;
/// Requests held for an owner whose window is not up yet.
const MAX_QUEUED: usize = 32;
/// How long the owner gives one connection end to end. Longer than the
/// launch-intent capture on the sending side (`launch::capture`), which holds
/// its connection open while it learns what it was launched for.
pub const HANDLER_TIMEOUT: Duration = Duration::from_secs(4);
/// How long a sender waits for the owner's greeting on one attempt.
const HELLO_TIMEOUT: Duration = Duration::from_secs(2);
/// How long a launch that lost the lock keeps trying to reach the owner. Long
/// enough to outlast an owner's startup, a quit in progress, and a mailbox
/// saturated for one [`HANDLER_TIMEOUT`].
pub const CLAIM_DEADLINE: Duration = Duration::from_secs(8);
const RETRY: Duration = Duration::from_millis(50);

/// What a launch that found GitPulse already running asks of it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum Request {
    /// Open these repositories (Finder's "Open with", a dropped folder, a path
    /// on the command line), revealing the window on the way.
    Open { paths: Vec<String> },
    /// Come to the front: someone launched the app that is already running.
    Reveal,
    /// A banner was clicked, or its Snooze pressed, and macOS delivered the
    /// response to the launch rather than to the owner.
    Notification { id: String, snooze: bool },
    /// An agent hook asking for a verdict on a command it is about to let run.
    /// Answered on the connection, by whatever [`Primary::attach_gate`] put
    /// there, rather than handed to the window: see [`GateSink`].
    GateCommand {
        /// Echoed in the answer, so a reply that belongs to another request is
        /// refused by the hook rather than believed.
        id: String,
        cwd: String,
        command: String,
    },
    /// An agent hook asking which other worktrees hold `path` dirty before it
    /// lets an edit of it run. Answered on the connection like
    /// [`Request::GateCommand`], by the sink [`Primary::attach_collision`] put
    /// there: the app remembers the scan for a few seconds, so a burst of
    /// edits costs one set of `git status` reads instead of one each.
    CollisionScan {
        /// Echoed in the answer, as for [`Request::GateCommand`].
        id: String,
        /// The worktree the edit is in (absolute).
        repo: String,
        /// The edited file, relative to `repo`.
        path: String,
    },
    /// An agent asking for a browser session on a local dev server in the
    /// terminal dock of repository `repo` (`gitpulse_browser_open`). One-way,
    /// like [`Request::Open`]: the window opens the repository and the tab.
    Browse {
        /// The repository (absolute) whose dock gets the session.
        repo: String,
        /// A loopback `http`/`https` URL (`browser::policy`).
        url: String,
    },
    /// A `gitpulse-mcp` process asking for the git facets of
    /// `gitpulse_insights` (`worktrees`, `changes`, `collisions`) of `repo`,
    /// read-only. Answered on the connection like [`Request::CollisionScan`],
    /// by the sink [`Primary::attach_insights`] put there. An owner that
    /// predates this request refuses the unknown `kind` and the asker scans
    /// for itself; the protocol version is deliberately not bumped for it.
    InsightsRead {
        /// Echoed in the answer, as for [`Request::GateCommand`].
        id: String,
        /// The repository to read (absolute). The owner re-checks trust in it.
        repo: String,
    },
    /// An agent `SessionStart` hook running inside a GitPulse PTY reporting its
    /// authoritative host conversation id and optional artifact directory
    /// (such as Antigravity's `conversationId` and `artifactDirectoryPath`).
    AgentSessionMeta {
        terminal_id: String,
        agent: String,
        conversation_id: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        artifact_dir: Option<String>,
    },
    /// Reports a file or plan artifact an agent just modified inside a GitPulse PTY
    /// (`PostToolUse` -> `gitpulse-hook notify tool_finished`) so "Follow Agent"
    /// split panes live-open or refresh the active file/diff without polling.
    AgentTouchedFile {
        terminal_id: String,
        repo: String,
        path: String,
        artifact: bool,
    },
    /// An authenticated parent session orchestrating a scoped child terminal or
    /// requesting a terminal split file pane (`gitpulse_terminal_*` / `gitpulse-hook term`).
    TerminalOrchestrate {
        id: String,
        call: crate::terminal::orchestrate::OrchestrateCall,
    },
    /// A `gitpulse-mcp` process asking the running desktop owner to wake an
    /// idle hosted agent tab (`TerminalSession.svelte`), forward a peer turn to
    /// a Managed run (`managed_run::follow_up_with`), or spawn an unattended
    /// kickoff session (`gitpulse_kickoff_agent`). Answered on its connection
    /// within [`HANDLER_TIMEOUT`] under [`MAX_DELIVER_HANDLERS`].
    AgentDeliver {
        /// Echoed in the answer, as for [`Request::GateCommand`].
        id: String,
        /// The repository checkout (absolute). The owner re-checks trust in it.
        repo: String,
        /// Target peer identifier (or `"kickoff"` when `spawn` is set).
        peer_id: String,
        /// Persisted `agent_bus` message id (or kickoff request id).
        msg_id: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        spawn: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        provider: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        branch: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        prompt: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        task_id: Option<String>,
    },
    /// Asks the app owner to snapshot `refs/heads`, `refs/tags`, and
    /// `git stash create` under `refs/gitpulse/ops/<n>` before a gated agent
    /// git write proceeds; answered synchronously on the connection.
    ///
    /// The asker waits [`OP_RECORD_DEADLINE`] and then lets the write run; the
    /// app measures that budget from receipt and captures nothing after it.
    /// The fields below are the wire contract with every installed app: the
    /// enum refuses unknown fields, so a field added here would make a newer
    /// hook's request fail against an older app. See
    /// `the_record_git_op_request_keeps_the_fields_it_has_always_had`.
    RecordGitOp {
        id: String,
        cwd: String,
        command: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        session_id: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        task_id: Option<String>,
    },
    /// Asks the app owner to take a turn checkpoint of the worktree at `cwd`
    /// (`refs/gitpulse/turns/<key>/<stamp>`) at an agent turn boundary;
    /// answered synchronously on the connection like [`Request::RecordGitOp`].
    /// `event` is `turn_start` or `turn_end`. An owner that predates this
    /// request refuses the unknown `kind` and the hook reports "not captured";
    /// the protocol version is deliberately not bumped for it (see
    /// [`Request::InsightsRead`]), and an old hook never sends it.
    RecordTurnCheckpoint {
        id: String,
        cwd: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        session_id: Option<String>,
        event: String,
        /// A one-line label (what the turn ended on); redacted by the owner.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        label: Option<String>,
    },
}

impl Request {
    /// The id an answer must echo, for the requests that are answered on their
    /// own connection.
    fn answered_id(&self) -> Option<&str> {
        match self {
            Request::GateCommand { id, .. }
            | Request::CollisionScan { id, .. }
            | Request::InsightsRead { id, .. }
            | Request::TerminalOrchestrate { id, .. }
            | Request::AgentDeliver { id, .. }
            | Request::RecordGitOp { id, .. }
            | Request::RecordTurnCheckpoint { id, .. } => Some(id),
            _ => None,
        }
    }

    /// Bounds a frame from another process before anything acts on it. The
    /// sender is this user's own GitPulse, but a socket is a socket.
    pub fn validate(&self) -> Result<(), String> {
        match self {
            Request::Open { paths } => {
                if paths.is_empty() || paths.len() > MAX_PATHS {
                    return Err(format!(
                        "open names {} paths; 1..={MAX_PATHS} allowed",
                        paths.len()
                    ));
                }
                for path in paths {
                    if path.is_empty() || path.len() > MAX_PATH_BYTES {
                        return Err("open path is empty or too long".into());
                    }
                    if path.contains('\0') || !Path::new(path).is_absolute() {
                        return Err("open path must be absolute".into());
                    }
                }
                Ok(())
            }
            Request::Reveal => Ok(()),
            Request::Notification { id, .. } => {
                if id.is_empty() || id.len() > MAX_NATIVE_ID || id.chars().any(char::is_control) {
                    return Err("notification identity is empty, too long or not printable".into());
                }
                Ok(())
            }
            Request::GateCommand { id, cwd, command } => {
                if id.is_empty() || id.len() > MAX_NATIVE_ID || id.chars().any(char::is_control) {
                    return Err("gate request id is empty, too long or not printable".into());
                }
                if cwd.is_empty()
                    || cwd.len() > MAX_PATH_BYTES
                    || cwd.contains('\0')
                    || !Path::new(cwd).is_absolute()
                {
                    return Err("gate cwd must be an absolute path".into());
                }
                if command.is_empty() || command.len() > MAX_GATE_COMMAND_BYTES {
                    return Err("gate command is empty or too long".into());
                }
                Ok(())
            }
            Request::CollisionScan { id, repo, path } => {
                if id.is_empty() || id.len() > MAX_NATIVE_ID || id.chars().any(char::is_control) {
                    return Err("scan request id is empty, too long or not printable".into());
                }
                if repo.is_empty()
                    || repo.len() > MAX_PATH_BYTES
                    || repo.contains('\0')
                    || !Path::new(repo).is_absolute()
                {
                    return Err("scan repository must be an absolute path".into());
                }
                if path.is_empty()
                    || path.len() > MAX_PATH_BYTES
                    || path.contains('\0')
                    || Path::new(path).is_absolute()
                {
                    return Err("scan path must be relative to its repository".into());
                }
                Ok(())
            }
            Request::Browse { repo, url } => {
                if repo.is_empty()
                    || repo.len() > MAX_PATH_BYTES
                    || repo.contains('\0')
                    || !Path::new(repo).is_absolute()
                {
                    return Err("browse repository must be an absolute path".into());
                }
                if url.len() > crate::browser::MAX_URL_CHARS {
                    return Err("browse URL is too long".into());
                }
                crate::browser::policy::check_loopback(url)
                    .map(|_| ())
                    .map_err(|why| format!("browse URL refused: {why}"))
            }
            Request::InsightsRead { id, repo } => {
                if id.is_empty() || id.len() > MAX_NATIVE_ID || id.chars().any(char::is_control) {
                    return Err("insights request id is empty, too long or not printable".into());
                }
                if repo.is_empty()
                    || repo.len() > MAX_PATH_BYTES
                    || repo.contains('\0')
                    || !Path::new(repo).is_absolute()
                {
                    return Err("insights repository must be an absolute path".into());
                }
                Ok(())
            }
            Request::AgentSessionMeta {
                terminal_id,
                agent,
                conversation_id,
                artifact_dir,
            } => {
                if !crate::alerts::bridge::valid_key(terminal_id) {
                    return Err("agent session terminal_id is invalid".into());
                }
                if !matches!(agent.as_str(), "agy" | "claude") {
                    return Err("agent session kind must be agy or claude".into());
                }
                if !crate::workbench::terminal_command::is_canonical_uuid(conversation_id) {
                    return Err("agent session conversation_id must be a canonical UUID".into());
                }
                if let Some(dir) = artifact_dir {
                    if dir.is_empty()
                        || dir.len() > MAX_PATH_BYTES
                        || dir.contains('\0')
                        || !Path::new(dir).is_absolute()
                    {
                        return Err("agent session artifact_dir must be an absolute path".into());
                    }
                }
                Ok(())
            }
            Request::AgentTouchedFile {
                terminal_id,
                repo,
                path,
                ..
            } => {
                if !crate::alerts::bridge::valid_key(terminal_id) {
                    return Err("touched-file terminal_id is invalid".into());
                }
                if repo.is_empty()
                    || repo.len() > MAX_PATH_BYTES
                    || repo.contains('\0')
                    || !Path::new(repo).is_absolute()
                {
                    return Err("touched-file repository must be an absolute path".into());
                }
                if path.is_empty()
                    || path.len() > MAX_PATH_BYTES
                    || path.contains('\0')
                    || Path::new(path).is_absolute()
                {
                    return Err("touched-file path must be relative".into());
                }
                for component in Path::new(path).components() {
                    if !matches!(component, std::path::Component::Normal(_)) {
                        return Err(
                            "touched-file path must contain only normal relative segments".into(),
                        );
                    }
                }
                Ok(())
            }
            Request::TerminalOrchestrate { id, call } => {
                if id.is_empty() || id.len() > MAX_NATIVE_ID || id.chars().any(char::is_control) {
                    return Err("orchestrate request id is empty, too long or not printable".into());
                }
                call.validate()
            }
            Request::AgentDeliver {
                id,
                repo,
                peer_id,
                msg_id,
                spawn,
                provider,
                branch,
                prompt,
                task_id,
            } => {
                if id.is_empty() || id.len() > MAX_NATIVE_ID || id.chars().any(char::is_control) {
                    return Err("deliver request id is empty, too long or not printable".into());
                }
                if repo.is_empty()
                    || repo.len() > MAX_PATH_BYTES
                    || repo.contains('\0')
                    || !Path::new(repo).is_absolute()
                {
                    return Err("deliver repository must be an absolute path".into());
                }
                if peer_id.is_empty()
                    || peer_id.len() > MAX_NATIVE_ID
                    || peer_id.chars().any(char::is_control)
                {
                    return Err("deliver peer_id is empty, too long or not printable".into());
                }
                if msg_id.is_empty()
                    || msg_id.len() > MAX_NATIVE_ID
                    || msg_id.chars().any(char::is_control)
                {
                    return Err("deliver msg_id is empty, too long or not printable".into());
                }
                if let Some(mode) = spawn.as_deref() {
                    if !matches!(mode, "terminal" | "managed") {
                        return Err("deliver spawn must be 'terminal' or 'managed'".into());
                    }
                }
                if let Some(p) = provider.as_deref() {
                    if !matches!(p, "claude" | "agy" | "codex") {
                        return Err("deliver provider must be claude, agy or codex".into());
                    }
                }
                if let Some(b) = branch.as_deref() {
                    if b.is_empty() || b.len() > MAX_NATIVE_ID || b.chars().any(char::is_control) {
                        return Err("deliver branch is empty, too long or not printable".into());
                    }
                }
                if let Some(p) = prompt.as_deref() {
                    if p.trim().is_empty() || p.len() > MAX_KICKOFF_PROMPT_BYTES || p.contains('\0')
                    {
                        return Err(
                            "deliver prompt is empty, contains NUL or exceeds 16 KiB".into()
                        );
                    }
                }
                if let Some(tid) = task_id.as_deref() {
                    if tid.is_empty()
                        || tid.len() > MAX_NATIVE_ID
                        || tid.chars().any(char::is_control)
                    {
                        return Err("deliver task_id is empty, too long or not printable".into());
                    }
                }
                Ok(())
            }
            Request::RecordGitOp {
                id,
                cwd,
                command,
                session_id,
                task_id,
            } => {
                if id.is_empty() || id.len() > MAX_NATIVE_ID || id.chars().any(char::is_control) {
                    return Err("op-record request id is empty, too long or not printable".into());
                }
                if cwd.is_empty()
                    || cwd.len() > MAX_PATH_BYTES
                    || cwd.contains('\0')
                    || !Path::new(cwd).is_absolute()
                {
                    return Err("op-record cwd must be an absolute path".into());
                }
                if command.is_empty() || command.len() > MAX_GATE_COMMAND_BYTES {
                    return Err("op-record command is empty or too long".into());
                }
                if let Some(sid) = session_id.as_deref() {
                    if sid.len() > MAX_NATIVE_ID || sid.chars().any(char::is_control) {
                        return Err("op-record session_id is too long or not printable".into());
                    }
                }
                if let Some(tid) = task_id.as_deref() {
                    if tid.len() > MAX_NATIVE_ID || tid.chars().any(char::is_control) {
                        return Err("op-record task_id is too long or not printable".into());
                    }
                }
                Ok(())
            }
            Request::RecordTurnCheckpoint {
                id,
                cwd,
                session_id,
                event,
                label,
            } => {
                if id.is_empty() || id.len() > MAX_NATIVE_ID || id.chars().any(char::is_control) {
                    return Err("checkpoint request id is empty, too long or not printable".into());
                }
                if cwd.is_empty()
                    || cwd.len() > MAX_PATH_BYTES
                    || cwd.contains('\0')
                    || !Path::new(cwd).is_absolute()
                {
                    return Err("checkpoint cwd must be an absolute path".into());
                }
                if !matches!(event.as_str(), "turn_start" | "turn_end") {
                    return Err("checkpoint event must be turn_start or turn_end".into());
                }
                if let Some(sid) = session_id.as_deref() {
                    if sid.len() > MAX_NATIVE_ID || sid.chars().any(char::is_control) {
                        return Err("checkpoint session_id is too long or not printable".into());
                    }
                }
                if label
                    .as_deref()
                    .is_some_and(|l| l.len() > MAX_CHECKPOINT_LABEL_BYTES)
                {
                    return Err("checkpoint label is too long".into());
                }
                Ok(())
            }
        }
    }
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Hello {
    v: u32,
    pid: u32,
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Envelope {
    v: u32,
    request: Request,
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Ack {
    accepted: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    reason: Option<String>,
    /// The request's `id` and the verdict, for the one request that has an
    /// answer beyond "accepted".
    #[serde(default, skip_serializing_if = "Option::is_none")]
    id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    reply: Option<serde_json::Value>,
}

impl Ack {
    fn accepted() -> Self {
        Self {
            accepted: true,
            reason: None,
            id: None,
            reply: None,
        }
    }

    fn refused(reason: String) -> Self {
        Self {
            accepted: false,
            reason: Some(reason),
            id: None,
            reply: None,
        }
    }
}

/// Where the lock and the mailbox live for a profile directory.
#[derive(Debug, Clone)]
pub struct Paths {
    pub lock: PathBuf,
    pub socket: PathBuf,
}

impl Paths {
    pub fn in_dir(dir: &Path) -> Self {
        Self {
            lock: dir.join(LOCK_FILE),
            socket: dir.join(SOCKET_FILE),
        }
    }

    /// A socket path the kernel would truncate cannot be bound or reached. The
    /// lock still decides ownership; only forwarding is lost.
    fn socket_fits(&self) -> bool {
        self.socket.as_os_str().len() <= crate::local_socket::max_path_bytes()
    }
}

/// The outcome of [`claim`].
pub enum Claim {
    /// This process owns the profile. Keep it alive for the process lifetime:
    /// dropping it releases the lock.
    Primary(Primary),
    /// Another GitPulse owns the profile and is answering. Hand it the
    /// request with [`Secondary::forward`] and exit.
    Secondary(Secondary),
    /// Another GitPulse holds the lock but never answered within the deadline.
    /// Starting anyway is exactly the harm this module exists to prevent, so
    /// the launch gives up; `pid` is the holder's, for the log and for
    /// bringing it forward.
    Blocked { pid: Option<u32>, reason: String },
    /// The guard could not run at all (the profile directory is unwritable,
    /// say). Reported distinctly from `Primary` so that "unchecked" is never
    /// logged as "checked and alone".
    Unavailable(String),
}

/// Decides whether this process is the profile's GitPulse.
///
/// Returns within `deadline` plus one handshake in every case.
pub fn claim(dir: &Path, deadline: Duration) -> Claim {
    if let Err(error) = std::fs::create_dir_all(dir) {
        return Claim::Unavailable(format!("cannot create {}: {error}", dir.display()));
    }
    // Every launch must name the same socket, and the socket is bound only on
    // a path with no symlink in it (`local_socket`). Resolving the directory
    // once gives both, even when `$HOME` itself passes through a symlink.
    let dir = match dir.canonicalize() {
        Ok(dir) => dir,
        Err(error) => {
            return Claim::Unavailable(format!("cannot resolve {}: {error}", dir.display()))
        }
    };
    let paths = Paths::in_dir(&dir);
    let started = Instant::now();
    loop {
        let file = match open_lock(&paths.lock) {
            Ok(file) => file,
            Err(error) => {
                return Claim::Unavailable(format!("cannot open {}: {error}", paths.lock.display()))
            }
        };
        match try_lock(&file) {
            Ok(true) => {
                // The path may have been replaced between our open and our
                // lock; a lock on an inode nobody else can find decides nothing.
                if !same_file(&file, &paths.lock) {
                    if started.elapsed() >= deadline {
                        return Claim::Unavailable(format!(
                            "{} kept being replaced while it was locked",
                            paths.lock.display()
                        ));
                    }
                    continue;
                }
                // Holding the lock is one signal. A live answer on the mailbox
                // is the other, and it wins: it means the owner's lock file was
                // deleted from under it and we locked a fresh one.
                if paths.socket_fits() {
                    match Secondary::connect(&paths.socket) {
                        Ok(secondary) => return Claim::Secondary(secondary),
                        Err(Reach::Incompatible { pid, reason }) => {
                            return Claim::Blocked {
                                pid: Some(pid),
                                reason,
                            }
                        }
                        Err(Reach::Unreachable(_)) => {}
                    }
                }
                return Claim::Primary(Primary::start(file, paths));
            }
            Ok(false) => {
                if !paths.socket_fits() {
                    return Claim::Blocked {
                        pid: read_pid(&paths.lock),
                        reason: format!(
                            "the mailbox path {} is too long to reach",
                            paths.socket.display()
                        ),
                    };
                }
                match Secondary::connect(&paths.socket) {
                    Ok(secondary) => return Claim::Secondary(secondary),
                    Err(Reach::Incompatible { pid, reason }) => {
                        return Claim::Blocked {
                            pid: Some(pid),
                            reason,
                        }
                    }
                    Err(Reach::Unreachable(reason)) => {
                        if started.elapsed() >= deadline {
                            return Claim::Blocked {
                                pid: read_pid(&paths.lock),
                                reason,
                            };
                        }
                    }
                }
            }
            Err(error) => {
                return Claim::Unavailable(format!("cannot lock {}: {error}", paths.lock.display()))
            }
        }
        std::thread::sleep(RETRY);
    }
}

fn open_lock(path: &Path) -> std::io::Result<File> {
    // `std` opens with O_CLOEXEC, so the lock never leaks into a git child
    // that could outlive this process and keep the profile "owned".
    OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .mode(0o600)
        .open(path)
}

/// `Ok(true)` locked, `Ok(false)` held elsewhere.
fn try_lock(file: &File) -> std::io::Result<bool> {
    loop {
        // SAFETY: a valid descriptor owned by `file` for the whole call.
        let rc = unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) };
        if rc == 0 {
            return Ok(true);
        }
        let error = std::io::Error::last_os_error();
        match error.kind() {
            ErrorKind::WouldBlock => return Ok(false),
            ErrorKind::Interrupted => continue,
            _ => return Err(error),
        }
    }
}

fn same_file(file: &File, path: &Path) -> bool {
    match (file.metadata(), std::fs::metadata(path)) {
        (Ok(held), Ok(named)) => held.dev() == named.dev() && held.ino() == named.ino(),
        _ => false,
    }
}

fn read_pid(lock: &Path) -> Option<u32> {
    let mut text = String::new();
    File::open(lock)
        .ok()?
        .take(32)
        .read_to_string(&mut text)
        .ok()?;
    text.trim().parse().ok()
}

/// Reads one newline-terminated frame of at most [`MAX_FRAME`] bytes.
fn read_frame(reader: &mut impl BufRead) -> Result<Option<String>, String> {
    let mut line = Vec::new();
    let read = reader
        .take(MAX_FRAME as u64 + 1)
        .read_until(b'\n', &mut line)
        .map_err(|error| format!("read: {error}"))?;
    if read == 0 {
        return Ok(None);
    }
    if line.last() != Some(&b'\n') {
        return Err(if line.len() > MAX_FRAME {
            "frame exceeds its size limit".into()
        } else {
            "frame ended without a newline".into()
        });
    }
    line.pop();
    String::from_utf8(line)
        .map(Some)
        .map_err(|_| "frame is not UTF-8".into())
}

fn write_frame(stream: &mut UnixStream, value: &impl Serialize) -> Result<(), String> {
    let mut bytes = serde_json::to_vec(value).map_err(|error| error.to_string())?;
    bytes.push(b'\n');
    stream
        .write_all(&bytes)
        .and_then(|()| stream.flush())
        .map_err(|error| format!("write: {error}"))
}

/// Whether an owner's refusal says it is busy (every slot it gives this kind of
/// request is taken) rather than unable: a busy owner is alive and answering,
/// so an asker falls back for this call and asks again on the next.
pub fn is_busy_refusal(reason: &str) -> bool {
    reason.ends_with(" is busy")
}

/// Only this user's processes may hand the owner work. The profile directory
/// is already private to the user; this holds if it is not. A peer whose uid
/// could not be read is refused.
pub fn peer_allowed(peer_uid: Option<u32>, own_uid: u32) -> bool {
    peer_uid == Some(own_uid)
}

/// Why [`Secondary::connect`] did not produce a usable connection.
enum Reach {
    /// Nobody answered: no owner yet, one starting, or one leaving.
    Unreachable(String),
    /// An owner answered, in a protocol this build cannot speak.
    Incompatible { pid: u32, reason: String },
}

/// A launch that found an owner answering, with the connection still open.
pub struct Secondary {
    stream: UnixStream,
    reader: BufReader<UnixStream>,
    pid: u32,
}

impl Secondary {
    fn connect(socket: &Path) -> Result<Self, Reach> {
        Self::connect_within(socket, HELLO_TIMEOUT)
    }

    /// [`Self::connect`] with a greeting deadline of the caller's choosing. A
    /// peer that is not this user is not an owner: the profile directory is
    /// private, so one that answers there is an impostor or a misconfiguration,
    /// and either way nothing may be handed to it.
    fn connect_within(socket: &Path, hello: Duration) -> Result<Self, Reach> {
        let unreachable = |error: String| Reach::Unreachable(error);
        let stream = UnixStream::connect(socket)
            .map_err(|error| unreachable(format!("connect: {error}")))?;
        // SAFETY: `getuid` reads this process's real user id and cannot fail.
        let own_uid = unsafe { libc::getuid() };
        let peer = crate::local_socket::peer_uid(&stream);
        if !peer_allowed(peer, own_uid) {
            return Err(unreachable(format!(
                "the owner is uid {peer:?}, not this user"
            )));
        }
        stream
            .set_read_timeout(Some(hello))
            .and_then(|()| stream.set_write_timeout(Some(hello)))
            .map_err(|error| unreachable(format!("timeouts: {error}")))?;
        let mut reader = BufReader::new(
            stream
                .try_clone()
                .map_err(|error| unreachable(error.to_string()))?,
        );
        let line = read_frame(&mut reader)
            .map_err(unreachable)?
            .ok_or_else(|| unreachable("the owner closed without a greeting".into()))?;
        let hello: Hello = serde_json::from_str(&line)
            .map_err(|error| unreachable(format!("greeting: {error}")))?;
        // An owner on another protocol version is still an owner: it answered.
        if hello.v != PROTOCOL {
            return Err(Reach::Incompatible {
                pid: hello.pid,
                reason: format!(
                    "owner {} speaks protocol {}, this launch {PROTOCOL}",
                    hello.pid, hello.v
                ),
            });
        }
        Ok(Self {
            stream,
            reader,
            pid: hello.pid,
        })
    }

    /// The owner's process id, from its greeting.
    pub fn owner_pid(&self) -> u32 {
        self.pid
    }

    /// Hands the owner `request`. The connection was opened by [`claim`] and is
    /// held while the caller works out what to send, which the owner allows
    /// for up to [`HANDLER_TIMEOUT`].
    pub fn forward(self, request: &Request) -> Result<(), String> {
        self.exchange(request, HANDLER_TIMEOUT).map(|_| ())
    }

    /// Asks the owner for a verdict on a command: the answer is the owner's
    /// reply to [`Request::GateCommand`], returned only when the owner echoed
    /// this request's id. Anything else — a refusal, a missing or foreign id, a
    /// missing reply — is an error, and the caller judges the command itself.
    pub fn gate(self, request: &Request, within: Duration) -> Result<serde_json::Value, String> {
        if !matches!(request, Request::GateCommand { .. }) {
            return Err("not a gate request".into());
        }
        self.answered(request, within)
    }

    /// Asks the owner which worktrees hold a file dirty: the owner's reply to
    /// [`Request::CollisionScan`], on the same terms as [`Secondary::gate`] —
    /// only an answer that echoes this request's id and carries a reply counts,
    /// and anything else is an error the caller scans for itself on.
    pub fn collisions(
        self,
        request: &Request,
        within: Duration,
    ) -> Result<serde_json::Value, String> {
        if !matches!(request, Request::CollisionScan { .. }) {
            return Err("not a scan request".into());
        }
        self.answered(request, within)
    }

    /// Asks the owner for the git facets of a repository: the owner's reply to
    /// [`Request::InsightsRead`], on the same terms as [`Secondary::gate`].
    pub fn insights(
        self,
        request: &Request,
        within: Duration,
    ) -> Result<serde_json::Value, String> {
        if !matches!(request, Request::InsightsRead { .. }) {
            return Err("not an insights request".into());
        }
        self.answered(request, within)
    }

    /// Asks the owner to execute one Option A terminal orchestration call,
    /// waiting up to `call.reply_deadline()` and verifying the echoed `id`.
    pub fn orchestrate(
        self,
        id: &str,
        call: crate::terminal::orchestrate::OrchestrateCall,
    ) -> Result<serde_json::Value, String> {
        let within = call.reply_deadline();
        let request = Request::TerminalOrchestrate {
            id: id.to_owned(),
            call,
        };
        self.answered(&request, within)
    }

    /// Asks the owner to deliver an A2A peer message or spawn an unattended
    /// kickoff session: the owner's reply to [`Request::AgentDeliver`], on the
    /// same terms as [`Secondary::gate`].
    pub fn agent_deliver(
        self,
        request: &Request,
        within: Duration,
    ) -> Result<serde_json::Value, String> {
        if !matches!(request, Request::AgentDeliver { .. }) {
            return Err("not an agent deliver request".into());
        }
        self.answered(request, within)
    }

    /// Asks the owner to snapshot refs and working-tree changes before a gated
    /// agent git write proceeds: the owner's reply to [`Request::RecordGitOp`].
    ///
    /// The failure says how far the exchange got ([`Reached`]), because only a
    /// request that was sent and then went unanswered leaves the app's
    /// snapshot outcome unknown. `sent` is set the moment the whole request is
    /// written, so a caller that abandons this call from outside (its own
    /// budget) still knows whether the request left.
    pub fn record_git_op(
        self,
        request: &Request,
        within: Duration,
        sent: &AtomicBool,
    ) -> Result<serde_json::Value, AskFailure> {
        if !matches!(request, Request::RecordGitOp { .. }) {
            return Err(AskFailure::not_sent("not an op-record request"));
        }
        let id = request.answered_id().ok_or_else(|| {
            AskFailure::not_sent("this request is not answered on its connection")
        })?;
        let ack = self.exchange_reaching(request, within, Some(sent))?;
        if ack.id.as_deref() != Some(id) {
            return Err(AskFailure::unanswered("the answer names another request"));
        }
        ack.reply
            .ok_or_else(|| AskFailure::unanswered("the answer carries no verdict"))
    }

    /// Asks the owner to take a turn checkpoint: the owner's reply to
    /// [`Request::RecordTurnCheckpoint`].
    pub fn record_turn_checkpoint(
        self,
        request: &Request,
        within: Duration,
    ) -> Result<serde_json::Value, String> {
        if !matches!(request, Request::RecordTurnCheckpoint { .. }) {
            return Err("not a turn-checkpoint request".into());
        }
        self.answered(request, within)
    }

    fn answered(self, request: &Request, within: Duration) -> Result<serde_json::Value, String> {
        let id = request
            .answered_id()
            .ok_or("this request is not answered on its connection")?;
        let ack = self.exchange(request, within)?;
        if ack.id.as_deref() != Some(id) {
            return Err("the answer names another request".into());
        }
        ack.reply
            .ok_or_else(|| "the answer carries no verdict".into())
    }

    fn exchange(self, request: &Request, within: Duration) -> Result<Ack, String> {
        self.exchange_reaching(request, within, None)
            .map_err(|failure| failure.why)
    }

    /// [`Self::exchange`] saying how far it got when it failed. `sent` is set
    /// right after the request is completely written.
    fn exchange_reaching(
        mut self,
        request: &Request,
        within: Duration,
        sent: Option<&AtomicBool>,
    ) -> Result<Ack, AskFailure> {
        request.validate().map_err(AskFailure::not_sent)?;
        self.stream
            .set_read_timeout(Some(within))
            .and_then(|()| self.stream.set_write_timeout(Some(within)))
            .map_err(|error| AskFailure::not_sent(format!("timeouts: {error}")))?;
        let envelope = Envelope {
            v: PROTOCOL,
            request: request.clone(),
        };
        let encoded = serde_json::to_vec(&envelope)
            .map_err(|error| AskFailure::not_sent(error.to_string()))?;
        if encoded.len() > MAX_FRAME {
            return Err(AskFailure::not_sent(format!(
                "request is {} bytes; frames carry {MAX_FRAME}",
                encoded.len()
            )));
        }
        // A frame cut short by a failed write has no newline, so the owner
        // reads it as no request at all: a failed write is "never sent".
        write_frame(&mut self.stream, &envelope).map_err(AskFailure::not_sent)?;
        if let Some(sent) = sent {
            sent.store(true, Ordering::Release);
        }
        let line = read_frame(&mut self.reader)
            .map_err(AskFailure::unanswered)?
            .ok_or_else(|| AskFailure::unanswered("the owner closed without an answer"))?;
        let ack: Ack = serde_json::from_str(&line)
            .map_err(|error| AskFailure::unanswered(format!("answer: {error}")))?;
        if ack.accepted {
            Ok(ack)
        } else {
            Err(AskFailure {
                reached: Reached::Refused,
                why: ack.reason.unwrap_or_else(|| "refused".into()),
            })
        }
    }
}

/// How far an exchange with the owner got before it failed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Reached {
    /// The request was never completely written: the owner cannot have acted
    /// on it.
    NotSent,
    /// The owner read the request and answered that it refused it. A refusal
    /// is written before the request's work is attempted.
    Refused,
    /// The request was completely written and no usable answer came back: a
    /// timeout, a read error or an owner that closed. The owner may or may not
    /// have acted.
    SentUnanswered,
}

/// Why an exchange failed, and how far it got.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AskFailure {
    pub reached: Reached,
    pub why: String,
}

impl AskFailure {
    pub fn not_sent(why: impl Into<String>) -> Self {
        Self {
            reached: Reached::NotSent,
            why: why.into(),
        }
    }

    fn unanswered(why: impl Into<String>) -> Self {
        Self {
            reached: Reached::SentUnanswered,
            why: why.into(),
        }
    }
}

/// How a hook reaches the running app: `dir` is the profile directory its
/// owner claimed. `None` when there is no owner to ask — the ordinary case when
/// GitPulse is closed, which is why this never claims anything.
pub fn reach_owner(dir: &Path, hello: Duration) -> Option<Secondary> {
    let paths = Paths::in_dir(&dir.canonicalize().ok()?);
    if !paths.socket_fits() {
        return None;
    }
    Secondary::connect_within(&paths.socket, hello).ok()
}

/// Judges one command for a hook: `(cwd, command)` to the verdict's JSON.
pub type GateSink = Arc<dyn Fn(&str, &str) -> Result<serde_json::Value, String> + Send + Sync>;

/// Scans for a hook: `(repo, path)` to the collision scan's JSON.
pub type ScanSink = GateSink;

/// Reads for an MCP process: `repo` to the insights facets' JSON.
pub type InsightsSink = Arc<dyn Fn(&str) -> Result<serde_json::Value, String> + Send + Sync>;

/// Executes an authenticated [`crate::terminal::orchestrate::OrchestrateCall`].
pub type OrchestrateSink = Arc<
    dyn Fn(crate::terminal::orchestrate::OrchestrateCall) -> Result<serde_json::Value, String>
        + Send
        + Sync,
>;
/// Delivers an A2A peer message or kickoff spawn for an MCP process.
pub type DeliverSink = Arc<dyn Fn(&Request) -> Result<serde_json::Value, String> + Send + Sync>;
/// Snapshots pre-op refs and `git stash create` under `refs/gitpulse/ops/<n>`.
/// The instant is no later than the moment the asker sent the request (the
/// owner takes it just before it greets the asker, who cannot send before it
/// has read the greeting): the asker waits a full [`OP_RECORD_DEADLINE`] from
/// its send, so a deadline counted from this instant never falls after the
/// asker gave up.
pub type OpRecordSink =
    Arc<dyn Fn(&Request, Instant) -> Result<serde_json::Value, String> + Send + Sync>;

/// Takes a turn checkpoint (`refs/gitpulse/turns/<key>/<stamp>`).
pub type TurnCheckpointSink =
    Arc<dyn Fn(&Request) -> Result<serde_json::Value, String> + Send + Sync>;

type Sink = Arc<dyn Fn(Request) + Send + Sync>;

#[derive(Default)]
struct Inbox {
    sink: Option<Sink>,
    queued: VecDeque<Request>,
}

struct Shared {
    closing: AtomicBool,
    active: AtomicUsize,
    gating: AtomicUsize,
    orchestrating: AtomicUsize,
    gate: Mutex<Option<GateSink>>,
    collision: Mutex<Option<ScanSink>>,
    insights: Mutex<Option<InsightsSink>>,
    deliver: Mutex<Option<DeliverSink>>,
    op_record: Mutex<Option<OpRecordSink>>,
    turn_checkpoint: Mutex<Option<TurnCheckpointSink>>,
    /// Insights reads in flight, a share of `gating` of their own.
    reading: AtomicUsize,
    orchestrate: Mutex<Option<OrchestrateSink>>,
    /// Agent deliveries in flight, a share of `gating` of their own.
    delivering: AtomicUsize,
    /// Turn checkpoint captures in flight, a share of `gating` of their own.
    checkpointing: AtomicUsize,
    inbox: Mutex<Inbox>,
    own_uid: u32,
    /// Times the accept loop's wait returned. An idle mailbox adds none.
    accept_wakes: AtomicU64,
    /// Set when the accept loop has ended.
    accept_exited: AtomicBool,
    /// Threads started to answer connections.
    handler_threads: AtomicUsize,
}

impl Shared {
    fn new(own_uid: u32) -> Self {
        Self {
            closing: AtomicBool::new(false),
            active: AtomicUsize::new(0),
            gating: AtomicUsize::new(0),
            orchestrating: AtomicUsize::new(0),
            gate: Mutex::new(None),
            collision: Mutex::new(None),
            insights: Mutex::new(None),
            deliver: Mutex::new(None),
            op_record: Mutex::new(None),
            turn_checkpoint: Mutex::new(None),
            reading: AtomicUsize::new(0),
            orchestrate: Mutex::new(None),
            delivering: AtomicUsize::new(0),
            checkpointing: AtomicUsize::new(0),
            inbox: Mutex::new(Inbox::default()),
            own_uid,
            accept_wakes: AtomicU64::new(0),
            accept_exited: AtomicBool::new(false),
            handler_threads: AtomicUsize::new(0),
        }
    }
}

/// The profile's owner. Holds the lock for as long as it lives.
pub struct Primary {
    _lock: File,
    paths: Paths,
    /// The bound socket's identity, so shutdown removes only our own.
    socket_identity: Option<(u64, u64)>,
    shared: Arc<Shared>,
    mailbox_error: Option<String>,
    /// What [`Self::shutdown`] writes to so the accept thread, which waits
    /// with no timeout, looks at `closing`.
    wake: Option<Arc<Wake>>,
}

impl Primary {
    fn start(mut lock: File, paths: Paths) -> Self {
        // For the log of a launch that could not reach us, and nothing else.
        let _ = lock
            .set_len(0)
            .and_then(|()| write!(lock, "{}", std::process::id()))
            .and_then(|()| lock.flush());
        let shared = Arc::new(Shared::new(
            // SAFETY: `getuid` reads this process's real user id and cannot
            // fail — the same id the hook bridge compares peers against.
            unsafe { libc::getuid() },
        ));
        let mut primary = Self {
            _lock: lock,
            paths,
            socket_identity: None,
            shared,
            mailbox_error: None,
            wake: None,
        };
        if let Err(error) = primary.open_mailbox() {
            primary.mailbox_error = Some(error);
        }
        primary
    }

    fn open_mailbox(&mut self) -> Result<(), String> {
        // Before the socket, so a mailbox is never bound that cannot be told
        // to stop.
        let wake = Arc::new(
            Wake::new().map_err(|error| format!("cannot create the mailbox wake pipe: {error}"))?,
        );
        // We hold the lock, so a socket at this path is a dead owner's, which
        // this replaces; anything else there is left alone and reported.
        let listener =
            crate::local_socket::bind_private(&self.paths.socket, "single-instance mailbox")?;
        let meta = std::fs::metadata(&self.paths.socket)
            .map_err(|error| format!("mailbox vanished: {error}"))?;
        self.socket_identity = Some((meta.dev(), meta.ino()));
        let shared = self.shared.clone();
        let accept_wake = wake.clone();
        crate::supervisor::spawn_once("instance-mailbox", "single-instance mailbox", move || {
            accept_loop(listener, shared, &accept_wake)
        })
        .map_err(|error| format!("cannot start the mailbox: {error}"))?;
        self.wake = Some(wake);
        Ok(())
    }

    /// Why launches cannot reach this owner, when they cannot. The lock still
    /// holds them off; they just cannot say what they wanted.
    pub fn mailbox_error(&self) -> Option<&str> {
        self.mailbox_error.as_deref()
    }

    /// Where forwarded requests go from now on. Anything that arrived before
    /// this was queued, and is delivered here first, in arrival order.
    pub fn attach(&self, sink: impl Fn(Request) + Send + Sync + 'static) {
        let sink: Sink = Arc::new(sink);
        let queued = {
            let mut inbox = self
                .shared
                .inbox
                .lock()
                .unwrap_or_else(PoisonError::into_inner);
            inbox.sink = Some(sink.clone());
            std::mem::take(&mut inbox.queued)
        };
        for request in queued {
            sink(request);
        }
    }

    /// Where hooks' command verdicts are answered from now on. Until this is
    /// called a [`Request::GateCommand`] is refused, and its hook judges the
    /// command itself.
    pub fn attach_gate(&self, sink: GateSink) {
        *self
            .shared
            .gate
            .lock()
            .unwrap_or_else(PoisonError::into_inner) = Some(sink);
    }

    /// Where hooks' collision scans are answered from now on. Until this is
    /// called a [`Request::CollisionScan`] is refused, and its hook scans the
    /// worktrees itself.
    pub fn attach_collision(&self, sink: ScanSink) {
        *self
            .shared
            .collision
            .lock()
            .unwrap_or_else(PoisonError::into_inner) = Some(sink);
    }

    /// Where MCP processes' insights reads are answered from now on. Until
    /// this is called a [`Request::InsightsRead`] is refused, and its asker
    /// scans the worktrees itself.
    pub fn attach_insights(&self, sink: InsightsSink) {
        *self
            .shared
            .insights
            .lock()
            .unwrap_or_else(PoisonError::into_inner) = Some(sink);
    }

    /// Where Option A terminal orchestration calls are answered from now on.
    pub fn attach_terminal_orchestrate(&self, sink: OrchestrateSink) {
        *self
            .shared
            .orchestrate
            .lock()
            .unwrap_or_else(PoisonError::into_inner) = Some(sink);
    }

    /// Where MCP processes' A2A deliveries and kickoff spawns are answered once
    /// the Tauri `AppHandle` exists (`launch::attach`). Until this is called a
    /// [`Request::AgentDeliver`] is refused and the caller falls back to
    /// mailbox/bus hook-boundary delivery.
    pub fn attach_agent_deliver(&self, sink: DeliverSink) {
        *self
            .shared
            .deliver
            .lock()
            .unwrap_or_else(PoisonError::into_inner) = Some(sink);
    }

    /// Where pre-op agent git write snapshot requests ([`Request::RecordGitOp`])
    /// are answered.
    pub fn attach_op_recorder(&self, sink: OpRecordSink) {
        *self
            .shared
            .op_record
            .lock()
            .unwrap_or_else(PoisonError::into_inner) = Some(sink);
    }

    /// Where turn checkpoint requests ([`Request::RecordTurnCheckpoint`]) are
    /// answered. Until this is called they are refused.
    pub fn attach_turn_checkpointer(&self, sink: TurnCheckpointSink) {
        *self
            .shared
            .turn_checkpoint
            .lock()
            .unwrap_or_else(PoisonError::into_inner) = Some(sink);
    }

    /// Stops answering and takes the mailbox down, keeping the lock. A launch
    /// from here on waits for the lock instead, which the kernel releases when
    /// this process is gone — so a relaunch during a slow quit becomes the next
    /// owner rather than handing its request to an owner that is leaving.
    pub fn shutdown(&self) {
        if self.shared.closing.swap(true, Ordering::AcqRel) {
            return;
        }
        if let Some(wake) = &self.wake {
            wake.notify();
        }
        let ours = std::fs::symlink_metadata(&self.paths.socket)
            .ok()
            .map(|meta| (meta.dev(), meta.ino()));
        if ours.is_some() && ours == self.socket_identity {
            let _ = std::fs::remove_file(&self.paths.socket);
        }
    }
}

/// An owner that stops owning — a launch that deferred after winning the lock,
/// or a test — takes its mailbox down with it, so the next owner never meets a
/// socket nobody answers, and the accept thread ends when it is woken.
impl Drop for Primary {
    fn drop(&mut self) {
        self.shutdown();
    }
}

/// The threads that answer connections. A connection used to start a thread,
/// and the owner is asked about as often as a hook fires; these are started on
/// demand, kept, and ended when the mailbox closes.
///
/// No more than [`MAX_HANDLERS`] + [`MAX_GATE_HANDLERS`] exist, which is the
/// most connections the accept loop admits at once (each holds a
/// [`HandlerSlot`] from admission until its job ends), so a connection is never
/// left waiting for a thread that cannot be started. The last job's thread
/// takes the next job from the queue, so one that has not yet gone back to
/// waiting when the next connection arrives still serves it.
struct HandlerPool {
    state: Mutex<PoolState>,
    ready: Condvar,
    shared: Arc<Shared>,
}

type Job = Box<dyn FnOnce() + Send + 'static>;

#[derive(Default)]
struct PoolState {
    jobs: VecDeque<Job>,
    /// Workers waiting for a job.
    idle: usize,
    workers: usize,
    closed: bool,
}

impl HandlerPool {
    const MAX_WORKERS: usize = MAX_HANDLERS + MAX_GATE_HANDLERS + MAX_ORCHESTRATE_HANDLERS;

    fn new(shared: Arc<Shared>) -> Arc<Self> {
        Arc::new(Self {
            state: Mutex::new(PoolState::default()),
            ready: Condvar::new(),
            shared,
        })
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, PoolState> {
        crate::supervisor::lock_or_recover(&self.state)
    }

    /// Queues `job` for a worker, starting one if none is free and the pool
    /// has room. On `Err` the job was dropped unrun, releasing whatever it held.
    fn submit(self: &Arc<Self>, job: Job) -> std::io::Result<()> {
        let mut state = self.lock();
        state.jobs.push_back(job);
        if state.idle >= state.jobs.len() || state.workers >= Self::MAX_WORKERS {
            drop(state);
            self.ready.notify_one();
            return Ok(());
        }
        state.workers += 1;
        drop(state);
        let pool = self.clone();
        let started = crate::supervisor::spawn_once(
            "instance-mailbox-conn",
            "single-instance mailbox",
            move || pool.work(),
        );
        match started {
            Ok(_) => {
                self.shared.handler_threads.fetch_add(1, Ordering::Relaxed);
                Ok(())
            }
            Err(error) => {
                let mut state = self.lock();
                state.workers -= 1;
                let job = state.jobs.pop_back();
                drop(state);
                drop(job);
                Err(error)
            }
        }
    }

    /// Ends every worker as it goes idle. Jobs already queued still run.
    fn close(&self) {
        self.lock().closed = true;
        self.ready.notify_all();
    }

    fn work(&self) {
        let mut state = self.lock();
        loop {
            if let Some(job) = state.jobs.pop_front() {
                drop(state);
                // A panicking handler must neither end the worker nor leave
                // the mailbox without a record of it; its slot is released by
                // the job's own drop as it unwinds.
                match std::panic::catch_unwind(std::panic::AssertUnwindSafe(job)) {
                    Ok(()) => crate::supervisor::note_recovered("single-instance mailbox"),
                    Err(panic) => {
                        crate::supervisor::note_stopped("single-instance mailbox", &*panic);
                    }
                }
                state = self.lock();
                continue;
            }
            if state.closed {
                state.workers -= 1;
                return;
            }
            state.idle += 1;
            state = self
                .ready
                .wait(state)
                .unwrap_or_else(PoisonError::into_inner);
            state.idle -= 1;
        }
    }
}

fn accept_loop(listener: UnixListener, shared: Arc<Shared>, wake: &Wake) {
    let pool = HandlerPool::new(shared.clone());
    let _exited = std::mem::DropGuard::new((shared.clone(), pool.clone()), |(shared, pool)| {
        pool.close();
        shared.accept_exited.store(true, Ordering::Release);
    });
    while !shared.closing.load(Ordering::Acquire) {
        // No timeout: nothing here needs to run unless a connection arrives or
        // `Primary::shutdown` writes to the wake pipe.
        let ready =
            crate::local_socket::wait_readable_or_woken(listener.as_raw_fd(), wake.reader_fd());
        shared.accept_wakes.fetch_add(1, Ordering::Relaxed);
        if ready.woken {
            wake.drain();
        }
        if !ready.listener {
            continue;
        }
        // Accepting creates a descriptor that is made close-on-exec only
        // afterwards; the inheritance lock keeps a concurrent git spawn from
        // copying it in between. The listener is non-blocking, so the lock is
        // held for one syscall, never across a wait.
        let accepted = crate::procguard::with_inheritance_lock(|| listener.accept());
        let stream = match accepted {
            Ok((stream, _)) => stream,
            Err(error)
                if matches!(error.kind(), ErrorKind::WouldBlock | ErrorKind::Interrupted) =>
            {
                continue
            }
            Err(error) => {
                log::warn!(target: "desktop", "instance mailbox accept failed: {error}");
                std::thread::sleep(RETRY);
                continue;
            }
        };
        if shared.active.fetch_add(1, Ordering::AcqRel) >= HandlerPool::MAX_WORKERS {
            shared.active.fetch_sub(1, Ordering::AcqRel);
            continue;
        }
        // The slot is released by the guard's drop, so a handler that panics
        // gives it back like one that returns. Eight panics would otherwise
        // fill MAX_HANDLERS and silently drop every later relaunch.
        let slot = HandlerSlot(shared.clone());
        let submitted = pool.submit(Box::new(move || handle(stream, &slot.0)));
        if let Err(error) = submitted {
            // The job, and the slot it owned, were dropped by the failed
            // start, which released the count already.
            log::warn!(target: "desktop", "instance mailbox could not answer a launch: {error}");
        }
    }
}

/// One of the [`MAX_HANDLERS`] mailbox slots, released on drop.
struct HandlerSlot(Arc<Shared>);

impl Drop for HandlerSlot {
    fn drop(&mut self) {
        self.0.active.fetch_sub(1, Ordering::AcqRel);
    }
}

fn handle(mut stream: UnixStream, shared: &Shared) {
    if let Err(ended) = answer(&mut stream, shared) {
        let (level, line) = ended.log_line();
        log::log!(target: "desktop", level, "{line}");
    }
}

/// How a conversation ended when it did not serve its peer.
#[derive(Debug, PartialEq, Eq)]
enum Ended {
    /// The peer connected, never sent a byte and left. Hooks do this to ask
    /// whether the owner is alive, many times an hour; nothing was refused
    /// because nothing was asked.
    Probe(String),
    /// Everything else, including a peer that sent part of a frame, a
    /// stranger's uid, and a peer that stalled while still connected.
    Refused(String),
    /// A read-only ask (a gate verdict, a collision scan, an insights read)
    /// whose asker left on its own deadline before the answer was written. The
    /// asker has already fallen back to working it out itself, by design, so
    /// nothing was refused; the line keeps what was late and by how much.
    Unheard(String),
}

impl Ended {
    /// The level and line this ending is logged with. A probe is routine and
    /// is not a warning; a refusal is, and keeps the line it has always had.
    fn log_line(&self) -> (log::Level, String) {
        match self {
            Self::Probe(reason) => (
                log::Level::Debug,
                format!("instance mailbox probe left before its first frame: {reason}"),
            ),
            Self::Refused(reason) => (
                log::Level::Warn,
                format!("instance mailbox refused a launch: {reason}"),
            ),
            Self::Unheard(reason) => (log::Level::Info, format!("instance mailbox: {reason}")),
        }
    }

    /// A failure answering an asked request (`what`, read since `started`).
    /// [`Unheard`] only for a `read_only` ask whose asker is provably gone:
    /// one that acts (a delivery, an orchestration, an op record) has already
    /// done so when its ack cannot be written, and its asker does not know, so
    /// that stays a refusal. Either way the line says what and how long.
    ///
    /// [`Unheard`]: Ended::Unheard
    fn after_ask(
        stream: &UnixStream,
        what: &str,
        read_only: bool,
        started: Instant,
        reason: String,
    ) -> Self {
        let ms = started.elapsed().as_millis();
        if read_only && crate::local_socket::peer_hung_up_silently(stream) {
            Self::Unheard(format!(
                "the asker of {what} left before its answer, {ms} ms after asking: {reason}"
            ))
        } else {
            Self::Refused(format!("{what}, {ms} ms after asking: {reason}"))
        }
    }

    /// A failure writing the answer to the op log. Its asker (a git write's
    /// hook) waits [`OP_RECORD_DEADLINE`], lets the write run and leaves, by
    /// design, so a snapshot that finishes after that is delivered to nobody. The
    /// snapshot itself was taken, which is all the asker ever needed from the
    /// owner, so that is not a refusal. Unlike [`Ended::after_ask`] this does
    /// not insist the ask was read-only, because the answer is the only thing
    /// the asker could have missed; unlike a delivery, nothing it acted on
    /// depends on the asker hearing back.
    ///
    /// [`Unheard`] needs both signals: the write failed the way a departed peer
    /// makes it fail, and the socket reads as hung up with nothing queued. An
    /// error on a peer that is still connected stays a refusal.
    ///
    /// [`Unheard`]: Ended::Unheard
    fn after_op_record(stream: &UnixStream, started: Instant, reason: String) -> Self {
        let ms = started.elapsed().as_millis();
        if write_says_peer_left(&reason) && crate::local_socket::peer_hung_up_silently(stream) {
            Self::Unheard(format!(
                "the asker of the op log left before its answer, {ms} ms after asking: {reason}"
            ))
        } else {
            Self::Refused(format!("the op log, {ms} ms after asking: {reason}"))
        }
    }

    /// A failure with nothing of the peer's first frame read: a [`Probe`]
    /// only if the peer is provably gone and sent nothing. When that cannot be
    /// established it is a refusal, so an unexplained failure stays loud.
    ///
    /// [`Probe`]: Ended::Probe
    fn before_first_frame(stream: &UnixStream, reason: String) -> Self {
        if crate::local_socket::peer_hung_up_silently(stream) {
            Self::Probe(reason)
        } else {
            Self::Refused(reason)
        }
    }
}

/// Whether a failed write's `reason` (as [`write_frame`] words it) is one a
/// peer that closed its end causes: a broken pipe, a reset, or a socket that is
/// no longer connected. Read from the `os error` number rather than the text,
/// which differs between macOS and Linux. A timeout is a peer that is still
/// there and slow, and anything else is ours, so those are not this.
fn write_says_peer_left(reason: &str) -> bool {
    let code = reason
        .strip_suffix(')')
        .and_then(|rest| rest.rsplit_once("(os error "))
        .and_then(|(_, number)| number.parse::<i32>().ok());
    matches!(code, Some(libc::EPIPE | libc::ECONNRESET | libc::ENOTCONN))
}

impl From<String> for Ended {
    fn from(reason: String) -> Self {
        Self::Refused(reason)
    }
}

fn answer(stream: &mut UnixStream, shared: &Shared) -> Result<(), Ended> {
    // Inherited from the non-blocking listener on the BSDs.
    if let Err(error) = stream
        .set_nonblocking(false)
        .and_then(|()| stream.set_read_timeout(Some(HANDLER_TIMEOUT)))
        .and_then(|()| stream.set_write_timeout(Some(HANDLER_TIMEOUT)))
    {
        return Err(Ended::before_first_frame(
            stream,
            format!("socket setup: {error}"),
        ));
    }
    let uid = crate::local_socket::peer_uid(stream);
    if !peer_allowed(uid, shared.own_uid) {
        return Err(Ended::Refused(format!("connection from uid {uid:?}")));
    }
    if shared.closing.load(Ordering::Acquire) {
        return Ok(());
    }
    // The asker cannot send its request before it has read this `Hello`, so
    // this instant is no later than the moment it sent. The op-record budget
    // runs from here and not from when the request was read and parsed: the
    // asker's own wait runs a full `OP_RECORD_DEADLINE` from its send, so an
    // app deadline measured from any instant at or before the send can never
    // fall after the asker has given up, however slow this accept was.
    let before_hello = Instant::now();
    write_frame(
        stream,
        &Hello {
            v: PROTOCOL,
            pid: std::process::id(),
        },
    )
    .map_err(|reason| Ended::before_first_frame(stream, reason))?;
    let mut reader = BufReader::new(stream.try_clone().map_err(|error| error.to_string())?);
    // A launch that only wanted to know whether we are alive closes here. An
    // error with nothing buffered is also before the first frame; once a byte
    // is buffered, anything that goes wrong is a frame cut short.
    match reader.fill_buf() {
        Ok([]) => return Ok(()),
        Ok(_) => {}
        Err(error) => {
            return Err(Ended::before_first_frame(stream, format!("read: {error}")));
        }
    }
    let Some(line) = read_frame(&mut reader)? else {
        return Ok(());
    };
    let request = serde_json::from_str::<Envelope>(&line)
        .map_err(|error| format!("malformed request: {error}"))
        .and_then(|envelope| {
            if envelope.v != PROTOCOL {
                return Err(format!("protocol {} is not {PROTOCOL}", envelope.v));
            }
            envelope.request.validate()?;
            Ok(envelope.request)
        });
    let request = match request {
        Ok(request) => request,
        Err(reason) => {
            write_frame(stream, &Ack::refused(reason.clone()))?;
            return Err(Ended::Refused(reason));
        }
    };
    // An asked request's failure is the answer's write, or (vanishingly) its
    // encoding: `after_ask` names the request and the time it took.
    let started = Instant::now();
    if let Request::GateCommand { id, cwd, command } = &request {
        return answer_gate(stream, shared, id, cwd, command)
            .map_err(|reason| Ended::after_ask(stream, "the gate", true, started, reason));
    }
    if let Request::CollisionScan { id, repo, path } = &request {
        return answer_scan(stream, shared, id, repo, path).map_err(|reason| {
            Ended::after_ask(stream, "the collision scan", true, started, reason)
        });
    }
    if let Request::InsightsRead { id, repo } = &request {
        return answer_insights(stream, shared, id, repo).map_err(|reason| {
            Ended::after_ask(stream, "the insights read", true, started, reason)
        });
    }
    if let Request::TerminalOrchestrate { id, call } = request {
        return answer_orchestrate(stream, shared, &id, call).map_err(|reason| {
            Ended::after_ask(stream, "the terminal orchestration", false, started, reason)
        });
    }
    if let Request::AgentDeliver { id, .. } = &request {
        return answer_agent_deliver(stream, shared, id, &request).map_err(|reason| {
            Ended::after_ask(stream, "the agent delivery", false, started, reason)
        });
    }
    if let Request::RecordGitOp { id, .. } = &request {
        return answer_op_record(stream, shared, id, &request, before_hello)
            .map_err(|reason| Ended::after_op_record(stream, started, reason));
    }
    if let Request::RecordTurnCheckpoint { id, .. } = &request {
        return answer_turn_checkpoint(stream, shared, id, &request).map_err(|reason| {
            Ended::after_ask(stream, "the turn checkpoint", true, started, reason)
        });
    }
    let verdict = admit(shared, request);
    let ack = match &verdict {
        Ok(_) => Ack::accepted(),
        Err(reason) => Ack::refused(reason.clone()),
    };
    write_frame(stream, &ack)?;
    match verdict {
        Ok(Some((sink, request))) => {
            sink(request);
            Ok(())
        }
        Ok(None) => Ok(()),
        Err(reason) => Err(Ended::Refused(reason)),
    }
}

/// Answers a hook's [`Request::GateCommand`] on its connection. The sink's
/// error is a refusal, not a verdict: the hook reads it as "judge this
/// yourself", which is the path that has always decided.
fn answer_gate(
    stream: &mut UnixStream,
    shared: &Shared,
    id: &str,
    cwd: &str,
    command: &str,
) -> Result<(), String> {
    answer_asked(stream, shared, id, "the gate", true, || {
        attached(&shared.gate, "the gate")?(cwd, command)
    })
}

/// Answers a hook's [`Request::CollisionScan`] on its connection, on the same
/// terms as [`answer_gate`]: an error is a refusal the hook scans past, never
/// an answer. Counted against the same [`MAX_GATE_HANDLERS`], so hook traffic
/// of either kind cannot use up the handlers a relaunch needs.
fn answer_scan(
    stream: &mut UnixStream,
    shared: &Shared,
    id: &str,
    repo: &str,
    path: &str,
) -> Result<(), String> {
    answer_asked(stream, shared, id, "the collision scan", true, || {
        attached(&shared.collision, "the collision scan")?(repo, path)
    })
}

/// Answers an MCP process's [`Request::InsightsRead`] on its connection, on
/// the same terms as [`answer_scan`], plus a share of its own: at most
/// [`MAX_INSIGHTS_HANDLERS`] reads at once, so the many MCP processes of many
/// agent sessions cannot fill the [`MAX_GATE_HANDLERS`] slots the edit hooks
/// need. One past it is answered "busy" and its asker reads for itself.
fn answer_insights(
    stream: &mut UnixStream,
    shared: &Shared,
    id: &str,
    repo: &str,
) -> Result<(), String> {
    let reading = ReadSlot::take(shared);
    answer_asked(stream, shared, id, "the insights read", true, || {
        if reading.is_none() {
            return Err("the insights read is busy".to_string());
        }
        attached(&shared.insights, "the insights read")?(repo)
    })
}

fn answer_agent_deliver(
    stream: &mut UnixStream,
    shared: &Shared,
    id: &str,
    request: &Request,
) -> Result<(), String> {
    let delivering = DeliverSlot::take(shared);
    answer_asked(stream, shared, id, "the agent delivery", false, || {
        if delivering.is_none() {
            return Err("the agent delivery is busy".to_string());
        }
        attached(&shared.deliver, "the agent delivery")?(request)
    })
}

fn answer_op_record(
    stream: &mut UnixStream,
    shared: &Shared,
    id: &str,
    request: &Request,
    not_after_send: Instant,
) -> Result<(), String> {
    answer_asked(stream, shared, id, "the op log", false, || {
        attached(&shared.op_record, "the op log")?(request, not_after_send)
    })
}

fn answer_turn_checkpoint(
    stream: &mut UnixStream,
    shared: &Shared,
    id: &str,
    request: &Request,
) -> Result<(), String> {
    let capturing = CheckpointSlot::take(shared);
    answer_asked(stream, shared, id, "the turn checkpoint", false, || {
        if capturing.is_none() {
            return Err("the turn checkpoint is busy".to_string());
        }
        attached(&shared.turn_checkpoint, "the turn checkpoint")?(request)
    })
}

/// One of the [`MAX_CHECKPOINT_HANDLERS`] turn checkpoints in flight, released
/// on drop.
struct CheckpointSlot<'a>(&'a Shared);

impl<'a> CheckpointSlot<'a> {
    fn take(shared: &'a Shared) -> Option<Self> {
        if shared.checkpointing.fetch_add(1, Ordering::AcqRel) >= MAX_CHECKPOINT_HANDLERS {
            shared.checkpointing.fetch_sub(1, Ordering::AcqRel);
            return None;
        }
        Some(Self(shared))
    }
}

impl Drop for CheckpointSlot<'_> {
    fn drop(&mut self) {
        self.0.checkpointing.fetch_sub(1, Ordering::AcqRel);
    }
}

/// The sink in `slot`, cloned out so its lock is not held while the answer is
/// worked out.
fn attached<T: Clone>(slot: &Mutex<Option<T>>, name: &str) -> Result<T, String> {
    slot.lock()
        .unwrap_or_else(PoisonError::into_inner)
        .clone()
        .ok_or_else(|| format!("{name} is not attached"))
}

/// What a read-only ask reports when its asker had already hung up by the time
/// the owner got to it: [`Ended::after_ask`] reads it as the asker leaving.
const ASKER_GONE: &str =
    "the asker hung up before its answer was started, so no answer was worked out";

/// What a read-only ask reports when its asker hung up while the answer was
/// being worked out: [`Ended::after_ask`] reads it as the asker leaving.
const ASKER_LEFT_MIDWAY: &str =
    "the asker hung up while its answer was being worked out, so the rest of it was not";

/// How often the owner looks at an asker's socket while its answer is worked
/// out. One non-blocking `recv` per look, for at most [`MAX_GATE_HANDLERS`]
/// asks at once, and the longest an abandoned computation can run on is this
/// plus the git child it is in the middle of, which the runner stops.
const ASKER_WATCH_INTERVAL: Duration = Duration::from_millis(25);

/// Runs `call` for an abandonable ask, and stops it at its next safe point if
/// the asker hangs up while it runs.
///
/// The sinks (the gate, the scan, the insights read) are fixed closures with no
/// parameter to carry a cancellation, and their git reads already take one from
/// the thread they run on: [`read_cancel`] is the scope `git_cli` checks
/// before every spawn and polls while a child runs, so the kill is the runner's
/// one kill, a SIGTERM and a grace. So the flag is put in force on this thread
/// for the length of `call`, and a watcher sets it when the socket reads as
/// hung up with nothing queued. Nothing further is spawned for that ask, and
/// the child in flight is stopped. The flag is thread-local, so a sink that
/// hands its work to another thread carries it across with
/// [`read_cancel::CallerScope`] (the scan's pool and scoped threads and the
/// insights read's own thread do); a hand-off that does not capture it runs on
/// under no flag, as it did before.
///
/// An ask whose asker left is reported as [`ASKER_LEFT_MIDWAY`] even if
/// `call` finished first: the asker is not there to hear it, and what was
/// computed after it left may be partial. Anything that stops the watch from
/// running (no thread, no second descriptor) leaves the ask to run as it always
/// did, never to fail.
///
/// [`read_cancel`]: crate::engine::process::read_cancel
fn call_while_asker_stays(
    stream: &UnixStream,
    call: impl FnOnce() -> Result<serde_json::Value, String>,
) -> Result<serde_json::Value, String> {
    use crate::engine::process::read_cancel;

    let Ok(watched) = stream.try_clone() else {
        return call();
    };
    let cancel = read_cancel::ReadCancel::new();
    let finished = Arc::new(AtomicBool::new(false));
    // One watcher per ask, started once and never restarted: a restart after a
    // panic would watch an ask that is already over. The supervisor records a
    // panic instead of losing it.
    let watch = {
        let (finished, cancel) = (Arc::clone(&finished), cancel.clone());
        crate::supervisor::spawn_once("asker-watch", "asker watch", move || {
            while !finished.load(Ordering::Acquire) {
                if crate::local_socket::peer_hung_up_silently(&watched) {
                    cancel.cancel();
                    return;
                }
                std::thread::park_timeout(ASKER_WATCH_INTERVAL);
            }
        })
    };
    let outcome = {
        let _in_force = read_cancel::enter(watch.is_ok().then_some(&cancel));
        call()
    };
    finished.store(true, Ordering::Release);
    if let Ok(watch) = watch {
        watch.thread().unpark();
        // Ends within one poll, so the ask never leaves its watcher behind.
        let _ = watch.join();
    }
    if cancel.is_cancelled() {
        return Err(ASKER_LEFT_MIDWAY.to_string());
    }
    outcome
}

/// Runs `call` for a request answered on its own connection, and writes its
/// reply, or its error as a refusal. A reply that would not fit one frame is a
/// refusal too: the asker reads it at once and reads for itself, instead of
/// waiting out its deadline on a frame it cannot take.
///
/// `abandonable` is for an ask whose asker works the answer out itself when it
/// gives up (a gate verdict, a collision scan, an insights read). Such an ask
/// is not started once the socket reads as hung up with nothing queued, because
/// the answer would be computed for nobody and again by the asker, which is
/// what drained the git spawn budget under load. The check is `recv` with
/// `MSG_PEEK`: only an end-of-file or a reset says the asker is gone, and
/// anything else, including a failure of the check itself, computes the answer
/// as it always did. An ask that acts for the world (a delivery, an op record,
/// a checkpoint) is never abandonable: its work is not for the asker's ears.
fn answer_asked(
    stream: &mut UnixStream,
    shared: &Shared,
    id: &str,
    name: &str,
    abandonable: bool,
    call: impl FnOnce() -> Result<serde_json::Value, String>,
) -> Result<(), String> {
    let slot = GateSlot::take(shared);
    let outcome = if shared.closing.load(Ordering::Acquire) {
        Err("GitPulse is quitting".to_string())
    } else if slot.is_none() {
        Err(format!("{name} is busy"))
    } else if abandonable && crate::local_socket::peer_hung_up_silently(stream) {
        return Err(ASKER_GONE.to_string());
    } else if abandonable {
        call_while_asker_stays(stream, call)
    } else {
        call()
    };
    let ack = match outcome {
        Ok(reply) => Ack {
            accepted: true,
            reason: None,
            id: Some(id.to_string()),
            reply: Some(reply),
        },
        Err(reason) => Ack::refused(reason),
    };
    let encoded = serde_json::to_vec(&ack).map_err(|error| error.to_string())?;
    if encoded.len() >= MAX_FRAME {
        return write_frame(
            stream,
            &Ack::refused(format!("{name}: the answer is {} bytes", encoded.len())),
        );
    }
    write_frame(stream, &ack)
}

/// One of the [`MAX_INSIGHTS_HANDLERS`] insights reads in flight, released on
/// drop.
struct ReadSlot<'a>(&'a Shared);

impl<'a> ReadSlot<'a> {
    fn take(shared: &'a Shared) -> Option<Self> {
        if shared.reading.fetch_add(1, Ordering::AcqRel) >= MAX_INSIGHTS_HANDLERS {
            shared.reading.fetch_sub(1, Ordering::AcqRel);
            return None;
        }
        Some(Self(shared))
    }
}

impl Drop for ReadSlot<'_> {
    fn drop(&mut self) {
        self.0.reading.fetch_sub(1, Ordering::AcqRel);
    }
}

/// One of the [`MAX_DELIVER_HANDLERS`] agent deliveries in flight, released on
/// drop.
struct DeliverSlot<'a>(&'a Shared);

impl<'a> DeliverSlot<'a> {
    fn take(shared: &'a Shared) -> Option<Self> {
        if shared.delivering.fetch_add(1, Ordering::AcqRel) >= MAX_DELIVER_HANDLERS {
            shared.delivering.fetch_sub(1, Ordering::AcqRel);
            return None;
        }
        Some(Self(shared))
    }
}

impl Drop for DeliverSlot<'_> {
    fn drop(&mut self) {
        self.0.delivering.fetch_sub(1, Ordering::AcqRel);
    }
}

/// One of the [`MAX_GATE_HANDLERS`] verdicts in flight, released on drop.
struct GateSlot<'a>(&'a Shared);

impl<'a> GateSlot<'a> {
    fn take(shared: &'a Shared) -> Option<Self> {
        if shared.gating.fetch_add(1, Ordering::AcqRel) >= MAX_GATE_HANDLERS {
            shared.gating.fetch_sub(1, Ordering::AcqRel);
            return None;
        }
        Some(Self(shared))
    }
}

impl Drop for GateSlot<'_> {
    fn drop(&mut self) {
        self.0.gating.fetch_sub(1, Ordering::AcqRel);
    }
}

fn answer_orchestrate(
    stream: &mut UnixStream,
    shared: &Shared,
    id: &str,
    call: crate::terminal::orchestrate::OrchestrateCall,
) -> Result<(), String> {
    let slot = OrchestrateSlot::take(shared);
    let sink = shared
        .orchestrate
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .clone();
    let outcome = if shared.closing.load(Ordering::Acquire) {
        Err("GitPulse is quitting".to_string())
    } else if slot.is_none() {
        Err("terminal orchestration is busy".to_string())
    } else {
        match sink {
            Some(sink) => sink(call),
            None => Err("terminal orchestration is not attached".to_string()),
        }
    };
    let ack = match outcome {
        Ok(reply) => Ack {
            accepted: true,
            reason: None,
            id: Some(id.to_string()),
            reply: Some(reply),
        },
        Err(reason) => Ack::refused(reason),
    };
    write_frame(stream, &ack)
}

struct OrchestrateSlot<'a>(&'a Shared);

impl<'a> OrchestrateSlot<'a> {
    fn take(shared: &'a Shared) -> Option<Self> {
        if shared.orchestrating.fetch_add(1, Ordering::AcqRel) >= MAX_ORCHESTRATE_HANDLERS {
            shared.orchestrating.fetch_sub(1, Ordering::AcqRel);
            return None;
        }
        Some(Self(shared))
    }
}

impl Drop for OrchestrateSlot<'_> {
    fn drop(&mut self) {
        self.0.orchestrating.fetch_sub(1, Ordering::AcqRel);
    }
}

/// Queues `request`, or returns the sink to deliver it to once acknowledged.
fn admit(shared: &Shared, request: Request) -> Result<Option<(Sink, Request)>, String> {
    let mut inbox = shared.inbox.lock().unwrap_or_else(PoisonError::into_inner);
    if shared.closing.load(Ordering::Acquire) {
        return Err("GitPulse is quitting".into());
    }
    if let Some(sink) = &inbox.sink {
        return Ok(Some((sink.clone(), request)));
    }
    if inbox.queued.len() >= MAX_QUEUED {
        return Err("GitPulse is still starting and its queue is full".into());
    }
    inbox.queued.push_back(request);
    Ok(None)
}

#[cfg(test)]
#[path = "instance_tests.rs"]
mod tests;
