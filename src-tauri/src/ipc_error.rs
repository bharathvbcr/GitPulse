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
//! One kind is typed more loosely, on purpose: a command that was *not* a user
//! action, failed, and had the spawn gate refuse something during it, carries
//! `DeferredUnderLoad` whatever the final text, because the engine wraps
//! runner errors in its own words in about twenty places and the frontend
//! retries a declined read. Only that kind drives a retry, and asking a read
//! again is idempotent and bounded (`MAX_DEFERRED_RETRIES`).
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

    /// The error for `text`, which is where a command body's `String` error
    /// ends up: typed only if a producer on this thread wrote exactly this
    /// text (see [`typed_text`]), and never retryable when the command was a
    /// user action.
    fn from_scope_text(text: String, replay_safe: bool, user_action: bool) -> Self {
        let (written, detail) = kind_written_as(&text);
        let kind = if user_action {
            // A user action may already have changed something, so it is never
            // asked again: not for its own deferral, not for a wrapped one.
            match written {
                IpcErrorKind::DeferredUnderLoad => IpcErrorKind::Unclassified,
                kind => kind,
            }
        } else if replay_safe && written == IpcErrorKind::Unclassified && saw_deferral() {
            // A READ that the spawn gate declined and that then failed, whatever
            // words the engine put around the refusal on the way out. Only a
            // command on `REPLAY_SAFE_READS` gets this: asking it again is
            // harmless, and the frontend bounds it (`MAX_DEFERRED_RETRIES`). Every
            // other command's deferral is typed by exact text or not at all.
            IpcErrorKind::DeferredUnderLoad
        } else {
            written
        };
        let error = Self::new(kind, text);
        match detail {
            Some(detail) if kind != IpcErrorKind::Unclassified => error.with_detail(detail),
            _ => error,
        }
    }

    /// True when the message contains `needle`. For tests and Rust callers
    /// that held the error as a `String` before it was typed.
    pub fn contains(&self, needle: &str) -> bool {
        self.message.contains(needle)
    }
}

/// How many distinct deferral texts a command remembers. A read that fails more
/// distinct spawns than this has seen the gate refuse a lot, and past the cap the
/// loose retry rule is switched off for that command (see [`Records::overflowed`])
/// rather than something being forgotten: it fails closed.
const MAX_DEFERRALS: usize = 16;

/// What the producers wrote on one thread during one command.
///
/// Trust, identity and stale-stash are singletons that nothing evicts: the
/// frontend acts on them (a prompt, a refresh), so a flood of deferrals must not
/// be able to push one out, and the last refusal of each is the one a command
/// returns. Deferrals are a deduplicated set with a cap.
#[derive(Clone, Default)]
pub(crate) struct Records {
    trust: Option<(String, Option<serde_json::Value>)>,
    identity: Option<String>,
    stale: Option<String>,
    ai_endpoint: Option<(String, Option<serde_json::Value>)>,
    deferrals: Vec<String>,
    /// More distinct deferrals than [`MAX_DEFERRALS`] were written.
    overflowed: bool,
}

impl Records {
    fn write(&mut self, kind: IpcErrorKind, text: &str, detail: Option<serde_json::Value>) {
        match kind {
            IpcErrorKind::RepositoryTrustRequired => self.trust = Some((text.to_string(), detail)),
            IpcErrorKind::IdentityMissing => self.identity = Some(text.to_string()),
            IpcErrorKind::StaleStash => self.stale = Some(text.to_string()),
            IpcErrorKind::AiEndpointUnconfirmed => {
                self.ai_endpoint = Some((text.to_string(), detail));
            }
            IpcErrorKind::DeferredUnderLoad => {
                if self.deferrals.iter().any(|written| written == text) {
                    return;
                }
                if self.deferrals.len() >= MAX_DEFERRALS {
                    self.overflowed = true;
                } else {
                    self.deferrals.push(text.to_string());
                }
            }
            // Kinds a producer does not write through here (the ledger maps
            // its own code, and `Unclassified` is the absence of a record).
            IpcErrorKind::Unclassified
            | IpcErrorKind::LedgerNotInitialised
            | IpcErrorKind::LedgerFailed => {}
        }
    }

    fn kind_of(&self, text: &str) -> (IpcErrorKind, Option<serde_json::Value>) {
        if let Some((written, detail)) = &self.trust {
            if written == text {
                return (IpcErrorKind::RepositoryTrustRequired, detail.clone());
            }
        }
        if self.identity.as_deref() == Some(text) {
            return (IpcErrorKind::IdentityMissing, None);
        }
        if self.stale.as_deref() == Some(text) {
            return (IpcErrorKind::StaleStash, None);
        }
        if let Some((written, detail)) = &self.ai_endpoint {
            if written == text {
                return (IpcErrorKind::AiEndpointUnconfirmed, detail.clone());
            }
        }
        if self.deferrals.iter().any(|written| written == text) {
            return (IpcErrorKind::DeferredUnderLoad, None);
        }
        (IpcErrorKind::Unclassified, None)
    }

    /// Folds another thread's records into these. Singletons of `other` win;
    /// deferrals are added under the same cap.
    fn absorb(&mut self, other: Records) {
        if other.trust.is_some() {
            self.trust = other.trust;
        }
        if other.identity.is_some() {
            self.identity = other.identity;
        }
        if other.stale.is_some() {
            self.stale = other.stale;
        }
        if other.ai_endpoint.is_some() {
            self.ai_endpoint = other.ai_endpoint;
        }
        for text in other.deferrals {
            self.write(IpcErrorKind::DeferredUnderLoad, &text, None);
        }
        self.overflowed |= other.overflowed;
    }
}

thread_local! {
    static TYPED: RefCell<Records> = RefCell::new(Records::default());
}

/// Records that `text` is a failure of `kind` and returns it unchanged. Called
/// by the code that writes the failure, at the point it writes it.
pub(crate) fn typed_text(kind: IpcErrorKind, text: String) -> String {
    TYPED.with(|typed| typed.borrow_mut().write(kind, &text, None));
    text
}

/// [`typed_text`] with structured detail the frontend can read, such as which
/// repository a refusal is about.
pub(crate) fn typed_text_with_detail(
    kind: IpcErrorKind,
    text: String,
    detail: serde_json::Value,
) -> String {
    TYPED.with(|typed| typed.borrow_mut().write(kind, &text, Some(detail)));
    text
}

/// Whether a failure of `kind` was written on this thread since the current
/// command began. For a caller that holds a result struct rather than a
/// message and needs the same answer the error path would give.
pub(crate) fn kind_was_written(kind: IpcErrorKind) -> bool {
    TYPED.with(|typed| {
        let typed = typed.borrow();
        match kind {
            IpcErrorKind::RepositoryTrustRequired => typed.trust.is_some(),
            IpcErrorKind::IdentityMissing => typed.identity.is_some(),
            IpcErrorKind::StaleStash => typed.stale.is_some(),
            IpcErrorKind::AiEndpointUnconfirmed => typed.ai_endpoint.is_some(),
            IpcErrorKind::DeferredUnderLoad => !typed.deferrals.is_empty(),
            IpcErrorKind::Unclassified
            | IpcErrorKind::LedgerNotInitialised
            | IpcErrorKind::LedgerFailed => false,
        }
    })
}

/// The gate refused something during this command, and not so many things that
/// the record is incomplete.
fn saw_deferral() -> bool {
    TYPED.with(|typed| {
        let typed = typed.borrow();
        !typed.deferrals.is_empty() && !typed.overflowed
    })
}

fn kind_written_as(text: &str) -> (IpcErrorKind, Option<serde_json::Value>) {
    TYPED.with(|typed| typed.borrow().kind_of(text))
}

/// A copy of what this thread has written (see [`with_records`]).
fn snapshot() -> Records {
    TYPED.with(|typed| typed.borrow().clone())
}

/// Runs `work` on this thread and returns its result with what its producers
/// wrote, for a seam that hands the answer to another thread (a single-flight
/// leader to its waiters, a scoped helper to the command that joins it). The
/// receiving thread calls [`absorb`] on the records.
///
/// The records returned are only what `work` wrote: this thread's earlier ones
/// (a single-flight leader's command wrote some before it led) are set aside
/// during `work` and put back after it, together with what `work` wrote, so
/// this thread's own command still sees both. Without that, a waiter would
/// inherit a refusal it never saw.
pub(crate) fn with_records<T>(work: impl FnOnce() -> T) -> (T, Records) {
    struct Restore(Option<Records>);
    impl Drop for Restore {
        fn drop(&mut self) {
            if let Some(mut earlier) = self.0.take() {
                // On unwind `work`'s partial record merges back too.
                let wrote = TYPED.with(|typed| std::mem::take(&mut *typed.borrow_mut()));
                earlier.absorb(wrote);
                TYPED.with(|typed| *typed.borrow_mut() = earlier);
            }
        }
    }
    let earlier = TYPED.with(|typed| std::mem::take(&mut *typed.borrow_mut()));
    let restore = Restore(Some(earlier));
    let value = work();
    let wrote = snapshot();
    drop(restore);
    (value, wrote)
}

/// Folds `records` into this thread's. A single-flight waiter is given the
/// leader's answer and, with it, what the leader's producers wrote, so the
/// waiter's command types the same text the leader's does.
pub(crate) fn absorb(records: Records) {
    TYPED.with(|typed| typed.borrow_mut().absorb(records));
}

struct TypedScope;

impl TypedScope {
    fn enter() -> Self {
        TYPED.with(|typed| *typed.borrow_mut() = Records::default());
        Self
    }
}

impl Drop for TypedScope {
    fn drop(&mut self) {
        TYPED.with(|typed| *typed.borrow_mut() = Records::default());
    }
}

/// The commands a deferred call may be asked again for, whatever words wrap the
/// refusal: reads, each checked to have no write a replay could repeat. A
/// command is on this list or its deferral is typed only by exact text, and a
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
pub(crate) fn run_command<T>(body: impl FnOnce() -> Result<T, String>) -> Result<T, IpcError> {
    run_command_as(None, body)
}

/// Runs one command body ([`git_cli::run_command_scope`]) and types its error.
///
/// A pool thread carries nothing in from the command before it or out to the
/// one after: the remembered messages are cleared on entry and on exit.
/// `command` is the command's name when it has one; it decides whether the
/// loose deferral rule may apply (see [`REPLAY_SAFE_READS`]).
pub(crate) fn run_command_as<T>(
    command: Option<&str>,
    body: impl FnOnce() -> Result<T, String>,
) -> Result<T, IpcError> {
    let replay_safe = command.is_some_and(|name| REPLAY_SAFE_READS.contains(&name));
    let _scope = TypedScope::enter();
    let mut user_action = false;
    let result = git_cli::run_command_scope(|| {
        let result = body();
        user_action = git_cli::current_admission() == Admission::Interactive;
        result
    });
    result.map_err(|text| IpcError::from_scope_text(text, replay_safe, user_action))
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
