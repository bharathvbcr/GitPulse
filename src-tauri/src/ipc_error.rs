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
//! failure is produced, by the one module that produces it:
//!
//! * the spawn gate's deferral (`git_cli::refusal_message`), the repository
//!   trust refusal (`repository_trust`), the missing commit identity
//!   (`git_writer`) and the stale stash refusal (`engine::stash`) each call
//!   [`typed_text`] with the kind and the message they wrote;
//! * `From<LedgerError>` maps the ledger's own `code`;
//! * `From<String>` and `From<&str>` are [`IpcErrorKind::Unclassified`], which
//!   the frontend treats as opaque: it shows the message and never matches
//!   text inside it.
//!
//! [`typed_text`] hands the message back unchanged and remembers, on the
//! thread that produced it, that *this exact text* has that kind. When a
//! command body ends, [`run_command`] gives the error its kind only if the
//! text that came out is one of those, character for character. A message
//! another layer wrapped (`Stash failed: <deferral>`), rewrote
//! (`git_cli::not_retryable`) or merely quoted (a branch named like a marker)
//! is not equal to what the producer wrote, so it is unclassified. A command
//! that was a user action never carries the retryable kind out at all.
//!
//! The kind list is mirrored by `IPC_ERROR_KINDS` in `src/lib/ipc/invoke.ts`
//! and pinned against it by `scripts/enum-variant-contract.test.ts`.

use crate::engine::git_cli::{self, Admission, RunError};
use crate::ledger::LedgerError;
use serde::{Deserialize, Serialize};
use std::cell::RefCell;
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
}

impl IpcErrorKind {
    /// Every kind, in wire spelling order. The round-trip tests walk this so a
    /// new kind cannot be added without being serialised and read back.
    pub const ALL: [Self; 7] = [
        Self::Unclassified,
        Self::DeferredUnderLoad,
        Self::RepositoryTrustRequired,
        Self::IdentityMissing,
        Self::StaleStash,
        Self::LedgerNotInitialised,
        Self::LedgerFailed,
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

    /// The error for `text`, which is where a command body's `String` error
    /// ends up: typed only if a producer on this thread wrote exactly this
    /// text (see [`typed_text`]), and never retryable when the command was a
    /// user action.
    fn from_scope_text(text: String, user_action: bool) -> Self {
        let kind = match kind_written_as(&text) {
            IpcErrorKind::DeferredUnderLoad if user_action => IpcErrorKind::Unclassified,
            kind => kind,
        };
        Self::new(kind, text)
    }

    /// True when the message contains `needle`. For tests and Rust callers
    /// that held the error as a `String` before it was typed.
    pub fn contains(&self, needle: &str) -> bool {
        self.message.contains(needle)
    }
}

/// How many typed messages a thread remembers at once. A command body writes a
/// handful at most; the bound stops a loop that fails a thousand times from
/// growing the list.
const MAX_TYPED: usize = 16;

thread_local! {
    static TYPED: RefCell<Vec<(IpcErrorKind, String)>> = const { RefCell::new(Vec::new()) };
}

/// Records that `text` is a failure of `kind` and returns it unchanged. Called
/// by the code that writes the failure, at the point it writes it.
pub(crate) fn typed_text(kind: IpcErrorKind, text: String) -> String {
    TYPED.with(|typed| {
        let mut typed = typed.borrow_mut();
        if typed.len() >= MAX_TYPED {
            typed.remove(0);
        }
        typed.push((kind, text.clone()));
    });
    text
}

/// Whether a failure of `kind` was written on this thread since the current
/// command began. For a caller that holds a result struct rather than a
/// message and needs the same answer the error path would give.
pub(crate) fn kind_was_written(kind: IpcErrorKind) -> bool {
    TYPED.with(|typed| typed.borrow().iter().any(|(written, _)| *written == kind))
}

fn kind_written_as(text: &str) -> IpcErrorKind {
    TYPED.with(|typed| {
        typed
            .borrow()
            .iter()
            .rev()
            .find(|(_, written)| written == text)
            .map_or(IpcErrorKind::Unclassified, |(kind, _)| *kind)
    })
}

struct TypedScope;

impl TypedScope {
    fn enter() -> Self {
        TYPED.with(|typed| typed.borrow_mut().clear());
        Self
    }
}

impl Drop for TypedScope {
    fn drop(&mut self) {
        TYPED.with(|typed| typed.borrow_mut().clear());
    }
}

/// Runs one command body ([`git_cli::run_command_scope`]) and types its error.
///
/// A pool thread carries nothing in from the command before it or out to the
/// one after: the remembered messages are cleared on entry and on exit.
pub(crate) fn run_command<T>(body: impl FnOnce() -> Result<T, String>) -> Result<T, IpcError> {
    let _scope = TypedScope::enter();
    let mut user_action = false;
    let result = git_cli::run_command_scope(|| {
        let result = body();
        user_action = git_cli::current_admission() == Admission::Interactive;
        result
    });
    result.map_err(|text| IpcError::from_scope_text(text, user_action))
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
mod tests;
