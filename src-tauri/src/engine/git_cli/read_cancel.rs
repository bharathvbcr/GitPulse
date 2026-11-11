//! Cancelling a read whose caller has gone.
//!
//! A newer click supersedes an older one on the screen, but nothing used to
//! tell the backend: the older read's `git` child and its parse ran to the end
//! for an answer nobody would read, holding a spawn permit and a pool thread
//! the newer click may be queued behind.
//!
//! The wire is one optional `request_id` on a few reads and one command that
//! names it (`cmd_cancel_read`). This module is everything on this side of it:
//!
//! * a [`ReadCancel`] flag per request, found by id in a bounded registry;
//! * a thread-local scope ([`ReadScope::enter`]) so the flag reaches the `git`
//!   runner without a parameter on every reader between the command and it, the
//!   way the admission class does;
//! * the runner's side (`git_cli`) polls the flag through its existing
//!   `ProcessObserver::cancelled`, so the kill is the one owner's: SIGTERM so
//!   git releases its own locks, a grace, then SIGKILL, exactly as for a
//!   timeout. There is no second kill path here.
//!
//! The frontend decides when a request has no caller left (the last interested
//! caller of a call it joined with others has gone). The backend adds the same
//! rule for reads it joined itself: a shared read is cancelled only when every
//! request waiting on it has been cancelled, and a request that never named an
//! id counts as waiting for good.

use std::cell::RefCell;
use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, PoisonError};

use crate::ipc_error::{CommandError, IpcErrorKind};

/// What a cancelled read returns. Its caller has gone, so nobody sees it; it
/// is an error so that nothing downstream parses, keeps or shares the output.
pub(crate) const CANCELLED_MESSAGE: &str = "git read cancelled: its caller is gone";

/// The cancellation a read returns, typed where it is produced so that the
/// failure counter and the IPC edge read its kind, not its words.
pub(crate) fn cancelled() -> CommandError {
    CommandError::typed(IpcErrorKind::ReadCancelled, CANCELLED_MESSAGE)
}

/// Ids longer than this are not registered: the read runs, uncancellable.
const MAX_ID_LEN: usize = 64;
/// Reads that can be cancelled at once. A full registry makes a read
/// uncancellable, never a failure.
const MAX_REGISTERED: usize = 512;

/// One request's cancel flag, shared between the registry that sets it and the
/// runner that polls it.
#[derive(Clone, Debug, Default)]
pub(crate) struct ReadCancel(Arc<AtomicBool>);

impl ReadCancel {
    pub(crate) fn new() -> Self {
        Self::default()
    }

    pub(crate) fn cancel(&self) {
        self.0.store(true, Ordering::Release);
    }

    pub(crate) fn is_cancelled(&self) -> bool {
        self.0.load(Ordering::Acquire)
    }
}

thread_local! {
    static CURRENT: RefCell<Option<ReadCancel>> = const { RefCell::new(None) };
}

/// The cancel flag of the request the current thread is serving, if any.
pub(crate) fn current() -> Option<ReadCancel> {
    CURRENT.with(|slot| slot.borrow().clone())
}

/// Puts `cancel` in force on this thread until dropped, then restores what was
/// there, so a pooled thread never carries one request's flag into the next.
pub(crate) struct ScopeGuard(Option<ReadCancel>);

impl ScopeGuard {
    fn install(cancel: Option<ReadCancel>) -> Self {
        Self(CURRENT.with(|slot| slot.replace(cancel)))
    }
}

impl Drop for ScopeGuard {
    fn drop(&mut self) {
        CURRENT.with(|slot| *slot.borrow_mut() = self.0.take());
    }
}

/// Puts `cancel` in force for the current thread. For a thread a command
/// spawns itself: the scope is thread-local and does not follow the spawn.
pub(crate) fn enter(cancel: Option<&ReadCancel>) -> ScopeGuard {
    ScopeGuard::install(cancel.cloned())
}

fn registry() -> &'static Mutex<HashMap<String, ReadCancel>> {
    static REGISTRY: std::sync::OnceLock<Mutex<HashMap<String, ReadCancel>>> =
        std::sync::OnceLock::new();
    REGISTRY.get_or_init(|| Mutex::new(HashMap::new()))
}

fn id_is_usable(id: &str) -> bool {
    !id.is_empty()
        && id.len() <= MAX_ID_LEN
        && id
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b':' | b'.'))
}

/// A read's registration under the id its caller named, kept for as long as the
/// read runs. Dropping it (when the command's body ends, however it ends)
/// removes the id, so a cancel that arrives afterwards finds nothing.
pub(crate) struct ReadScope {
    id: Option<String>,
    cancel: Option<ReadCancel>,
}

impl ReadScope {
    /// Registers `request_id`. An absent, malformed or duplicate id, or a full
    /// registry, gives a scope with nothing to cancel: the read runs as it
    /// always did.
    pub(crate) fn open(request_id: Option<&str>) -> Self {
        let none = Self {
            id: None,
            cancel: None,
        };
        let Some(id) = request_id.filter(|id| id_is_usable(id)) else {
            return none;
        };
        let mut live = registry().lock().unwrap_or_else(PoisonError::into_inner);
        // A second read under a live id would make a cancel for one stop the
        // other: neither is cancellable.
        if live.len() >= MAX_REGISTERED || live.contains_key(id) {
            return none;
        }
        let cancel = ReadCancel::new();
        live.insert(id.to_string(), cancel.clone());
        Self {
            id: Some(id.to_string()),
            cancel: Some(cancel),
        }
    }

    /// Puts this request's flag in force on the current thread. A thread the
    /// command spawns takes it with [`current`] and [`enter`] (the free
    /// functions), since the scope is thread-local.
    pub(crate) fn enter(&self) -> ScopeGuard {
        enter(self.cancel.as_ref())
    }
}

impl Drop for ReadScope {
    fn drop(&mut self) {
        let (Some(id), Some(cancel)) = (self.id.take(), self.cancel.take()) else {
            return;
        };
        let mut live = registry().lock().unwrap_or_else(PoisonError::into_inner);
        if live
            .get(&id)
            .is_some_and(|held| Arc::ptr_eq(&held.0, &cancel.0))
        {
            live.remove(&id);
        }
    }
}

/// Asks the read registered under `request_id` to stop. True when a live read
/// was signalled. An unknown, finished or malformed id changes nothing and is
/// not an error: the cancel lost a race it was always allowed to lose.
pub(crate) fn cancel(request_id: &str) -> bool {
    if !id_is_usable(request_id) {
        return false;
    }
    let live = registry().lock().unwrap_or_else(PoisonError::into_inner);
    match live.get(request_id) {
        Some(read) => {
            read.cancel();
            true
        }
        None => false,
    }
}

/// The runner's observer for a read that is not shared: it stops when its own
/// request is cancelled.
pub(crate) struct OwnCancel<'a>(pub(crate) &'a ReadCancel);

impl super::ProcessObserver for OwnCancel<'_> {
    fn cancelled(&self) -> bool {
        self.0.is_cancelled()
    }

    fn cancellation(&self) -> Option<CommandError> {
        Some(cancelled())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The flag a scope puts in force, read the way a spawned walk reads it.
    fn flag_of(scope: &ReadScope) -> Option<ReadCancel> {
        let _entered = scope.enter();
        current()
    }

    #[test]
    fn an_unknown_finished_or_malformed_id_cancels_nothing() {
        assert!(!cancel("never-registered"));
        assert!(!cancel(""));
        assert!(!cancel("has space"));
        assert!(!cancel(&"x".repeat(MAX_ID_LEN + 1)));
        let scope = ReadScope::open(Some("rc-test-finished"));
        let flag = flag_of(&scope).unwrap();
        drop(scope);
        assert!(!cancel("rc-test-finished"));
        assert!(!flag.is_cancelled());
    }

    #[test]
    fn a_live_id_is_signalled_and_a_duplicate_is_not_cancellable() {
        let first = ReadScope::open(Some("rc-test-live"));
        let second = ReadScope::open(Some("rc-test-live"));
        assert!(
            flag_of(&second).is_none(),
            "a duplicate id must not share a flag"
        );
        assert!(cancel("rc-test-live"));
        assert!(flag_of(&first).unwrap().is_cancelled());
        // The duplicate's drop must not remove the first's registration.
        drop(second);
        assert!(cancel("rc-test-live"));
    }

    #[test]
    fn the_scope_is_restored_so_a_pooled_thread_never_carries_a_flag() {
        assert!(current().is_none());
        let scope = ReadScope::open(Some("rc-test-scope"));
        {
            let _in = scope.enter();
            assert!(current().is_some());
        }
        assert!(current().is_none());
    }
}
