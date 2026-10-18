//! The error a command sends the frontend.
//!
//! A command used to reject with a bare string, and the frontend classified it
//! by looking for a phrase inside it, so rewording a Rust message could change
//! what the UI did without a type error or a failing test. The payload is now
//! `{ kind, message, detail? }`: `kind` is a closed set the frontend branches
//! on, `message` is the text a person reads (byte-for-byte what the string
//! was), and `detail` is optional structured JSON.
//!
//! `kind` has one producer per source, all here:
//! * [`IpcError::from_command_text`] reads a runner or command message with
//!   [`RunError::classify`] and the Rust-owned markers of the few refusals the
//!   UI acts on. It is the only place that looks inside a message.
//! * `From<LedgerError>` maps the ledger's `code`.
//! * `From<String>` and `From<&str>` are [`IpcErrorKind::Unclassified`], which
//!   the frontend treats as opaque: it shows the message and never matches
//!   text inside it.
//!
//! The kind list is mirrored by `IPC_ERROR_KINDS` in `src/lib/ipc/invoke.ts`
//! and pinned against it by `scripts/enum-variant-contract.test.ts`.

use crate::engine::git_cli::RunError;
use crate::engine::git_writer::IDENTITY_MISSING;
use crate::ledger::LedgerError;
use serde::{Deserialize, Serialize};
use std::fmt;

/// What went wrong, as far as the frontend acts on it.
///
/// Closed on purpose: a new kind is a new frontend behaviour, so it is added
/// here, in `IPC_ERROR_KINDS`, and where the frontend branches on it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum IpcErrorKind {
    /// Nothing the frontend branches on. The message is for a person only.
    Unclassified,
    /// The spawn gate declined to start git under load; nothing ran, and the
    /// same call may succeed a few seconds later.
    DeferredUnderLoad,
    /// The child never got a process slot inside its deadline; it did not run.
    SlotWaitTimeout,
    /// The child started and was killed at its deadline, so whether its side
    /// effect happened is unknown.
    TimedOut,
    /// The OS found no program to run.
    SpawnNotFound,
    /// The OS refused to start the program for another reason.
    SpawnFailed,
    /// The repository has not been approved for execution.
    RepositoryTrustRequired,
    /// A commit was refused for want of `user.name` / `user.email`.
    IdentityMissing,
    /// The user cancelled a command that was running hooks.
    HookCancelled,
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
    pub const ALL: [Self; 12] = [
        Self::Unclassified,
        Self::DeferredUnderLoad,
        Self::SlotWaitTimeout,
        Self::TimedOut,
        Self::SpawnNotFound,
        Self::SpawnFailed,
        Self::RepositoryTrustRequired,
        Self::IdentityMissing,
        Self::HookCancelled,
        Self::StaleStash,
        Self::LedgerNotInitialised,
        Self::LedgerFailed,
    ];
}

impl From<RunError> for IpcErrorKind {
    fn from(error: RunError) -> Self {
        match error {
            RunError::Deferred => Self::DeferredUnderLoad,
            RunError::SlotWaitTimeout => Self::SlotWaitTimeout,
            RunError::Timeout => Self::TimedOut,
            RunError::SpawnNotFound => Self::SpawnNotFound,
            RunError::SpawnFailed => Self::SpawnFailed,
            RunError::Other => Self::Unclassified,
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

    /// Types a command's message by its source: the runner's own failure
    /// shapes through [`RunError::classify`], then the markers of the refusals
    /// the UI acts on, each owned by the module that writes it.
    ///
    /// For text a command produced from git, a program, or a repository check.
    /// Text from anywhere else is a plain `String` and stays unclassified.
    pub fn from_command_text(message: String) -> Self {
        let kind = match RunError::classify(&message).into() {
            IpcErrorKind::Unclassified => classify_owned_marker(&message),
            kind => kind,
        };
        Self::new(kind, message)
    }

    /// True when the message contains `needle`. For tests and Rust callers
    /// that held the error as a `String` before it was typed.
    pub fn contains(&self, needle: &str) -> bool {
        self.message.contains(needle)
    }
}

/// The refusals whose phrase is defined by Rust and acted on by the UI.
fn classify_owned_marker(message: &str) -> IpcErrorKind {
    if crate::engine::git_cli::hooks::is_cancelled(message) {
        IpcErrorKind::HookCancelled
    } else if crate::repository_trust::refused(message) {
        IpcErrorKind::RepositoryTrustRequired
    } else if message.starts_with(IDENTITY_MISSING) {
        IpcErrorKind::IdentityMissing
    } else if message.contains(crate::engine::stash::STALE_STACK_HINT) {
        IpcErrorKind::StaleStash
    } else {
        IpcErrorKind::Unclassified
    }
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
