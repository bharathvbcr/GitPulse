//! The error a command sends the frontend.
//!
//! A command used to reject with a bare string, and the frontend classified it
//! by looking for a phrase inside it, so rewording a Rust message could change
//! what the UI did, and text a repository controls (a branch name, a commit
//! subject, a hook's stderr) could pose as one. The payload is now
//! `{ kind, message, detail? }`: `kind` is a closed set the frontend branches
//! on, `message` is the text a person reads (byte-for-byte what the string
//! was), and `detail` is optional structured JSON.
//!
//! **Nothing in here reads inside a message.** A kind is attached where the
//! failure is produced, by the one module that produces it, and travels to the
//! command's edge in the error value itself, a [`CommandError`]:
//!
//! * the spawn gate's deferral (`git_cli::refusal_message_at`), the repository
//!   trust refusal (`repository_trust`), the missing commit identity
//!   (`git_writer`), the stale stash refusal (`engine::stash`) and the
//!   unconfirmed AI endpoint (`ai`) each build a typed [`CommandError`];
//! * `From<LedgerError>` maps the ledger's own `code`;
//! * a plain `String` or `&str` is [`IpcErrorKind::Unclassified`], which the
//!   frontend treats as opaque: it shows the message and never matches text
//!   inside it.
//!
//! A layer that puts its own words around a failure ([`CommandError::wrap`])
//! keeps its origin but not its exact kind: a message another layer wrapped
//! (`Stash failed: <deferral>`) or merely quoted (a branch named like a marker)
//! is unclassified. A command that was a user action never carries the
//! retryable kind out at all. Because the kind is in the value, an error handed
//! across a thread (a scoped helper's join, a single-flight leader's answer)
//! keeps it with nothing to hand back beside it.
//!
//! One kind is typed more loosely, on purpose: a read on [`REPLAY_SAFE_READS`]
//! that was *not* a user action and failed with an error whose origin is the
//! spawn gate's deferral carries `DeferredUnderLoad` whatever words the engine
//! put around it, because the engine wraps runner errors in its own words in
//! about twenty places and the frontend retries a declined read. A deferral a
//! read swallowed and an unrelated failure after it are not typed by it: the
//! error that comes out is the one the command is about. Only that kind drives a
//! retry, and asking a read again is idempotent and bounded
//! (`MAX_DEFERRED_RETRIES`).
//!
//! The kind list is mirrored by `IPC_ERROR_KINDS` in `src/lib/ipc/invoke.ts`
//! and pinned against it by `scripts/enum-variant-contract.test.ts`.

use crate::engine::git_cli::{self, Admission, RunError};
use crate::ledger::LedgerError;
use serde::{Deserialize, Serialize};
use std::fmt;

/// What went wrong, as far as the frontend acts on it.
///
/// Closed on purpose: a new kind is a new frontend behaviour, so it is added
/// here, in `IPC_ERROR_KINDS`, and where the frontend branches on it. A kind
/// nothing reads is not mirrored.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum IpcErrorKind {
    /// Nothing the frontend branches on. The message is for a person only.
    Unclassified,
    /// The spawn gate declined to start git under load; nothing ran, and the
    /// same call may succeed a few seconds later. Never carried by a command
    /// that was a user action.
    DeferredUnderLoad,
    /// The repository has not been approved for execution.
    RepositoryTrustRequired,
    /// A commit was refused for want of `user.name` / `user.email`.
    IdentityMissing,
    /// A stash action named a stack entry that has moved or gone.
    StaleStash,
    /// The activity ledger has not been initialised for this repository.
    LedgerNotInitialised,
    /// Any other ledger failure; the ledger's own code is in `detail.code`.
    LedgerFailed,
    /// An AI action found a model server nobody has chosen yet. Nothing was
    /// sent; `detail` names the runtime, host, port, base URL and model the
    /// frontend asks the user to confirm.
    AiEndpointUnconfirmed,
}

impl IpcErrorKind {
    /// Every kind, in wire spelling order. The round-trip tests walk this so a
    /// new kind cannot be added without being serialised and read back.
    pub const ALL: [Self; 8] = [
        Self::Unclassified,
        Self::DeferredUnderLoad,
        Self::RepositoryTrustRequired,
        Self::IdentityMissing,
        Self::StaleStash,
        Self::LedgerNotInitialised,
        Self::LedgerFailed,
        Self::AiEndpointUnconfirmed,
    ];
}

/// The runner's own typed view. Only a deferral is a kind the frontend acts
/// on; every other runner failure is a message for a person.
impl From<RunError> for IpcErrorKind {
    fn from(error: RunError) -> Self {
        match error {
            RunError::Deferred => Self::DeferredUnderLoad,
            RunError::SlotWaitTimeout
            | RunError::Timeout
            | RunError::SpawnNotFound
            | RunError::SpawnFailed
            | RunError::Other => Self::Unclassified,
        }
    }
}

/// The command error payload.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct IpcError {
    pub kind: IpcErrorKind,
    /// Renderable as it stands: exactly the text the command used to return.
    pub message: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub detail: Option<serde_json::Value>,
}

impl IpcError {
    pub fn new(kind: IpcErrorKind, message: impl Into<String>) -> Self {
        Self {
            kind,
            message: message.into(),
            detail: None,
        }
    }

    pub fn with_detail(mut self, detail: serde_json::Value) -> Self {
        self.detail = Some(detail);
        self
    }

    /// True when the message contains `needle`. For tests and Rust callers
    /// that held the error as a `String` before it was typed.
    pub fn contains(&self, needle: &str) -> bool {
        self.message.contains(needle)
    }
}

/// A failure on its way from the code that produced it to a command's edge,
/// carrying the kind its producer gave it.
///
/// `origin` is set once, by the producer, and survives every layer that puts
/// its own words around the message ([`CommandError::wrap`]). Wrapping only
/// marks the text as no longer the producer's: the edge types an unwrapped
/// error by its origin, and a wrapped one only by the loose deferral rule (see
/// [`REPLAY_SAFE_READS`]).
///
/// There is deliberately no `Display`, `Deref<Target = str>` or conversion to
/// `String`: each would let `format!("…: {e}")` or `e.to_string()` turn the
/// error back into text and drop its origin without a word. Reading the text
/// is [`CommandError::message`]; giving the origin up on purpose, where an
/// error leaves for a caller that has no use for kinds, is
/// [`CommandError::into_message`].
#[derive(Clone, PartialEq, Eq)]
pub struct CommandError {
    message: String,
    origin: Option<(IpcErrorKind, Option<serde_json::Value>)>,
    wrapped: bool,
}

impl CommandError {
    /// A failure nobody typed: what a plain `String` error was.
    pub fn plain(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
            origin: None,
            wrapped: false,
        }
    }

    /// A failure of `kind`, written by the one module that produces it.
    pub(crate) fn typed(kind: IpcErrorKind, message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
            origin: Some((kind, None)),
            wrapped: false,
        }
    }

    /// [`CommandError::typed`] with structured detail the frontend can read,
    /// such as which repository a refusal is about.
    pub(crate) fn typed_with_detail(
        kind: IpcErrorKind,
        message: impl Into<String>,
        detail: serde_json::Value,
    ) -> Self {
        Self {
            message: message.into(),
            origin: Some((kind, Some(detail))),
            wrapped: false,
        }
    }

    /// The text a person reads.
    pub fn message(&self) -> &str {
        &self.message
    }

    /// The text alone, for a caller outside any command (a log line, an MCP
    /// reply, a status field) that has no use for the kind.
    pub fn into_message(self) -> String {
        self.message
    }

    /// The same failure in another layer's words: `words` gets the current
    /// text and returns the new one. The origin is kept, so a deferral the
    /// engine explained is still a deferral to the loose rule, but the text is
    /// no longer the producer's, so no exact kind is read from it. Words that
    /// hand the text back unchanged (a cleanup note that had nothing to add)
    /// leave it the producer's.
    pub fn wrap(self, words: impl FnOnce(&str) -> String) -> Self {
        let message = words(&self.message);
        let wrapped = self.wrapped || message != self.message;
        Self {
            message,
            origin: self.origin,
            wrapped,
        }
    }

    /// [`CommandError::wrap`] for a rewrite that keeps the producer's text
    /// and only edits it in place (the runner's `not_retryable`): the edge
    /// still reads the origin, so nothing is typed by the new words.
    pub(crate) fn map_message(self, edit: impl FnOnce(String) -> String) -> Self {
        Self {
            message: edit(self.message),
            ..self
        }
    }

    /// The kind its producer gave it, whatever words were put around it since.
    pub(crate) fn origin(&self) -> Option<IpcErrorKind> {
        self.origin.as_ref().map(|(kind, _)| *kind)
    }

    /// The kind of this exact text: the origin while nobody has wrapped it.
    pub(crate) fn kind(&self) -> IpcErrorKind {
        match &self.origin {
            Some((kind, _)) if !self.wrapped => *kind,
            _ => IpcErrorKind::Unclassified,
        }
    }

    /// True when the message contains `needle`.
    pub fn contains(&self, needle: &str) -> bool {
        self.message.contains(needle)
    }

    /// The error a command sends: typed by its producer unless wrapped, and
    /// never retryable when the command was a user action.
    fn into_ipc(self, replay_safe: bool, user_action: bool) -> IpcError {
        let exact = self.kind();
        let kind = if user_action {
            // A user action may already have changed something, so it is never
            // asked again: not for its own deferral, not for a wrapped one.
            match exact {
                IpcErrorKind::DeferredUnderLoad => IpcErrorKind::Unclassified,
                kind => kind,
            }
        } else if replay_safe
            && exact == IpcErrorKind::Unclassified
            && self.origin() == Some(IpcErrorKind::DeferredUnderLoad)
        {
            // A READ that the spawn gate declined, whatever words the engine
            // put around the refusal on the way out. Only a command on
            // `REPLAY_SAFE_READS` gets this: asking it again is harmless, and
            // the frontend bounds it (`MAX_DEFERRED_RETRIES`). Every other
            // command's deferral is typed by its unwrapped origin or not at all.
            IpcErrorKind::DeferredUnderLoad
        } else {
            exact
        };
        let detail = match self.origin {
            Some((_, detail)) if kind != IpcErrorKind::Unclassified && !self.wrapped => detail,
            _ => None,
        };
        let error = IpcError::new(kind, self.message);
        match detail {
            Some(detail) => error.with_detail(detail),
            None => error,
        }
    }
}

/// The text, with the origin beside it, so a test failure says both.
impl fmt::Debug for CommandError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let mut debug = f.debug_tuple("CommandError");
        debug.field(&self.message);
        if let Some(kind) = self.origin() {
            debug.field(&kind);
        }
        if self.wrapped {
            debug.field(&"wrapped");
        }
        debug.finish()
    }
}

/// A plain string says nothing about its cause.
impl From<String> for CommandError {
    fn from(message: String) -> Self {
        Self::plain(message)
    }
}

impl From<&str> for CommandError {
    fn from(message: &str) -> Self {
        Self::plain(message)
    }
}

impl PartialEq<str> for CommandError {
    fn eq(&self, other: &str) -> bool {
        self.message == other
    }
}

impl PartialEq<&str> for CommandError {
    fn eq(&self, other: &&str) -> bool {
        self.message == *other
    }
}

impl PartialEq<String> for CommandError {
    fn eq(&self, other: &String) -> bool {
        &self.message == other
    }
}

/// The commands a deferred call may be asked again for, whatever words wrap the
/// refusal: reads, each checked to have no write a replay could repeat. A
/// command is on this list or its deferral is typed only while unwrapped, and a
/// user action never is. Unverified means not retried, so a command that is not
/// here stays correct and merely shows the refusal instead of retrying it.
///
/// Pinned by `replay_safe_reads_are_exactly_the_reviewed_list` and checked
/// against the command sources by `every_replay_safe_read_is_a_command`.
pub(crate) const REPLAY_SAFE_READS: &[&str] = &[
    "cmd_branch_stats",
    "cmd_get_commit_details",
    "cmd_get_commit_diff",
    "cmd_get_commit_file_diff",
    "cmd_get_commit_files",
    "cmd_get_commit_graph",
    "cmd_get_file_blame",
    "cmd_get_file_blob",
    "cmd_get_file_content",
    "cmd_get_file_diff",
    "cmd_get_head_id",
    "cmd_get_range_diff",
    "cmd_get_reflog",
    "cmd_get_status",
    "cmd_last_fetch_at",
    "cmd_list_branches",
    "cmd_list_remotes",
    "cmd_list_repo_files",
    "cmd_list_submodules",
    "cmd_list_tags",
    "cmd_repo_operation",
    "cmd_reset_preview",
    "cmd_stash_list",
    "cmd_stash_show",
];

/// The `cmd_*` function a command body's closure belongs to, from the closure's
/// type name (`gitpulse_lib::commands::inspect::cmd_get_status::{{closure}}`).
pub(crate) fn command_name(type_name: &str) -> Option<&str> {
    type_name
        .split("::")
        .find(|segment| segment.starts_with("cmd_"))
}

/// [`run_command_as`] for a body that is not a named command: nothing is replay
/// safe about it.
#[cfg(test)]
pub(crate) fn run_command<T, E: Into<CommandError>>(
    body: impl FnOnce() -> Result<T, E>,
) -> Result<T, IpcError> {
    run_command_as(None, body)
}

/// Runs one command body ([`git_cli::run_command_scope`]) and types its error.
///
/// The kind is the one the error carries (see [`CommandError`]), so nothing is
/// remembered on the pool thread between commands. `command` is the command's
/// name when it has one; it decides whether the loose deferral rule may apply
/// (see [`REPLAY_SAFE_READS`]).
pub(crate) fn run_command_as<T, E: Into<CommandError>>(
    command: Option<&str>,
    body: impl FnOnce() -> Result<T, E>,
) -> Result<T, IpcError> {
    let replay_safe = command.is_some_and(|name| REPLAY_SAFE_READS.contains(&name));
    let mut user_action = false;
    let result = git_cli::run_command_scope(|| {
        let result = body().map_err(Into::into);
        user_action = git_cli::current_admission() == Admission::Interactive;
        result
    });
    result.map_err(|error| error.into_ipc(replay_safe, user_action))
}

impl fmt::Display for IpcError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.message)
    }
}

impl std::error::Error for IpcError {}

/// A plain string says nothing about its cause, so it is unclassified and the
/// frontend never reads inside it.
impl From<String> for IpcError {
    fn from(message: String) -> Self {
        Self::new(IpcErrorKind::Unclassified, message)
    }
}

impl From<&str> for IpcError {
    fn from(message: &str) -> Self {
        Self::new(IpcErrorKind::Unclassified, message)
    }
}

/// The message is the ledger's `Display` (`[code] message`), which is what the
/// string conversion at the command edge produced, so the text is unchanged.
impl From<LedgerError> for IpcError {
    fn from(error: LedgerError) -> Self {
        let kind = match error.code {
            "not_initialised" => IpcErrorKind::LedgerNotInitialised,
            _ => IpcErrorKind::LedgerFailed,
        };
        Self::new(kind, error.to_string()).with_detail(serde_json::json!({ "code": error.code }))
    }
}

#[cfg(test)]
mod adv_l6_tests;
#[cfg(test)]
mod adversarial2_tests;
#[cfg(test)]
mod tests;
