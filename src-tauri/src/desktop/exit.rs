//! The one owner of "may GitPulse quit now?".
//!
//! Every way to quit reaches [`begin`]: the status menu's Quit item, the main
//! window's close button, Tauri's `ExitRequested`, and AppKit's own requests
//! (Cmd-Q, Dock → Quit, a Quit Apple event, logout) through `terminate`.
//! Once the frontend has installed its listener and armed the gate, a quit is
//! handed to it for confirmation: running Git actions, in-flight saves and
//! unsaved editor drafts are only known there.
//!
//! The frontend must acknowledge a request within [`ACK_DEADLINE`]. A page
//! that crashed, is reloading, or is wedged never does, and the quit then
//! proceeds rather than leaving Force Quit as the only way out. After the
//! acknowledgement there is no timer: a person deciding whether to keep
//! their edits may take as long as they like.
//!
//! AppKit's requests are answered `NSTerminateLater` and replied to exactly
//! once, so an approved quit leaves through AppKit's own `terminate:` path
//! and a logout waits for the answer instead of being refused.

use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Mutex;
use std::time::Duration;

use serde::Deserialize;
use tauri::{AppHandle, Emitter, Manager, Runtime};

use super::{reveal_main, DesktopState, EXIT_REQUESTED_EVENT};

/// How long the frontend has to say it received a quit request.
pub const ACK_DEADLINE: Duration = Duration::from_secs(3);

/// Where a quit request came from, which decides how it is finally answered.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Origin {
    /// AppKit asked `applicationShouldTerminate:` and waits for a reply.
    AppKit,
    /// GitPulse's own surfaces; an approved quit calls `AppHandle::exit`.
    Internal,
}

/// What the caller of [`begin`] must do now.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Begin {
    /// Nobody can confirm a quit: let it happen.
    Proceed,
    /// The frontend was asked; the answer arrives later.
    Deferred,
    /// A request is already waiting for the frontend; hold this one.
    AlreadyPending,
}

/// The frontend's answers to a quit request.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ExitAnswer {
    /// The request reached the listener; stop the acknowledgement deadline.
    Seen,
    /// The person chose to keep working.
    Cancel,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Pending {
    id: u64,
    pub origin: Origin,
    acked: bool,
}

/// The quit state, free of any app handle so every transition is testable.
#[derive(Default)]
pub struct ExitGate {
    armed: AtomicBool,
    next_id: AtomicU64,
    pending: Mutex<Option<Pending>>,
}

impl ExitGate {
    /// The frontend's listener exists: quits are confirmed from now on.
    pub fn arm(&self) {
        self.armed.store(true, Ordering::Release);
    }

    pub fn is_armed(&self) -> bool {
        self.armed.load(Ordering::Acquire)
    }

    /// The page that listens is gone (reload or crash). Returns the request it
    /// can no longer answer, if any, so the caller can settle it.
    pub fn disarm(&self) -> Option<Pending> {
        self.armed.store(false, Ordering::Release);
        self.slot().take()
    }

    fn slot(&self) -> std::sync::MutexGuard<'_, Option<Pending>> {
        // A panic while holding this lock must not wedge every later quit.
        self.pending
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    /// Opens a request, or says why none was opened. The id names the request
    /// so a late deadline cannot settle a newer one.
    pub fn open(&self, origin: Origin) -> (Begin, Option<u64>) {
        if !self.is_armed() {
            return (Begin::Proceed, None);
        }
        let mut slot = self.slot();
        if slot.is_some() {
            return (Begin::AlreadyPending, None);
        }
        let id = self.next_id.fetch_add(1, Ordering::Relaxed);
        *slot = Some(Pending {
            id,
            origin,
            acked: false,
        });
        (Begin::Deferred, Some(id))
    }

    /// Records the frontend's acknowledgement. Returns whether a request was
    /// waiting for it.
    pub fn acknowledge(&self) -> bool {
        match self.slot().as_mut() {
            Some(pending) => {
                pending.acked = true;
                true
            }
            None => false,
        }
    }

    /// Takes the open request for its final answer. Taken exactly once: a
    /// second answer finds nothing to settle.
    pub fn take(&self) -> Option<Pending> {
        self.slot().take()
    }

    /// Takes request `id` only if it is still open and unacknowledged.
    pub fn expire(&self, id: u64) -> Option<Pending> {
        let mut slot = self.slot();
        match *slot {
            Some(pending) if pending.id == id && !pending.acked => slot.take(),
            _ => None,
        }
    }
}

/// Hands a quit to the frontend's confirmation. Returns whether it was handed
/// on: a request nobody received must not hold the app open waiting for it.
fn request_exit_confirmation<R: Runtime>(app: &AppHandle<R>) -> bool {
    if let Err(error) = reveal_main(app) {
        log::warn!(target: "desktop", "Could not reveal quit confirmation: {error}");
    }
    match app.emit(EXIT_REQUESTED_EVENT, ()) {
        Ok(()) => true,
        Err(error) => {
            log::warn!(target: "desktop", "exit-request emit failed: {error}");
            false
        }
    }
}

/// Routes a quit request. See [`Begin`] for what the caller does next.
pub fn begin<R: Runtime>(app: &AppHandle<R>, origin: Origin) -> Begin {
    let gate = &app.state::<DesktopState>().exit;
    let (outcome, id) = gate.open(origin);
    let Some(id) = id else {
        if outcome == Begin::AlreadyPending {
            // Show the confirmation that is already waiting.
            if let Err(error) = reveal_main(app) {
                log::warn!(target: "desktop", "Could not reveal quit confirmation: {error}");
            }
        }
        return outcome;
    };
    if !request_exit_confirmation(app) {
        gate.take();
        return Begin::Proceed;
    }
    let deadline = app.clone();
    let spawned = std::thread::Builder::new()
        .name("exit-ack-deadline".into())
        .spawn(move || {
            std::thread::sleep(ACK_DEADLINE);
            let gate = &deadline.state::<DesktopState>().exit;
            if let Some(pending) = gate.expire(id) {
                log::warn!(
                    target: "desktop",
                    "the window did not acknowledge the quit within {}s; quitting",
                    ACK_DEADLINE.as_secs()
                );
                settle(&deadline, pending, true);
            }
        });
    if let Err(error) = spawned {
        // Without a deadline an unanswered request could hold the app forever.
        log::warn!(target: "desktop", "exit deadline did not start ({error}); quitting");
        gate.take();
        return Begin::Proceed;
    }
    outcome
}

/// Gives a taken request its final answer.
pub fn settle<R: Runtime>(app: &AppHandle<R>, pending: Pending, approve: bool) {
    match pending.origin {
        Origin::Internal => {
            if approve {
                app.exit(0);
            }
        }
        Origin::AppKit => reply_to_appkit(app, approve),
    }
}

#[cfg(target_os = "macos")]
fn reply_to_appkit<R: Runtime>(app: &AppHandle<R>, approve: bool) {
    let replied = app.run_on_main_thread(move || super::terminate::reply(approve));
    if let Err(error) = replied {
        // The event loop is gone; nothing is left to keep alive.
        log::error!(target: "desktop", "could not answer AppKit's quit request: {error}");
        std::process::exit(0);
    }
}

#[cfg(not(target_os = "macos"))]
fn reply_to_appkit<R: Runtime>(app: &AppHandle<R>, approve: bool) {
    // AppKit requests exist only on macOS; treat one like an internal quit.
    if approve {
        app.exit(0);
    }
}

/// The listening page is gone. A request it had acknowledged was being
/// decided by a person who can no longer see it, so it is cancelled; one it
/// never saw is treated like an expired deadline and proceeds.
pub fn disarm<R: Runtime>(app: &AppHandle<R>, why: &str) {
    let gate = &app.state::<DesktopState>().exit;
    let was_armed = gate.is_armed();
    if let Some(pending) = gate.disarm() {
        let approve = !pending.acked;
        log::warn!(
            target: "desktop",
            "{why} while a quit was waiting; {}",
            if approve { "quitting" } else { "cancelled" }
        );
        settle(app, pending, approve);
    } else if was_armed {
        log::info!(target: "desktop", "{why}; quit confirmation disarmed until the page listens again");
    }
}

/// Completes a frontend-approved exit.
pub fn approve<R: Runtime>(app: &AppHandle<R>) {
    match app.state::<DesktopState>().exit.take() {
        Some(pending) => settle(app, pending, true),
        // No request is open: the frontend chose to quit on its own. An exit
        // code distinguishes this from an OS/user quit request.
        None => app.exit(0),
    }
}

/// Applies the frontend's acknowledgement or cancellation.
pub fn answer<R: Runtime>(app: &AppHandle<R>, answer: ExitAnswer) {
    let gate = &app.state::<DesktopState>().exit;
    match answer {
        ExitAnswer::Seen => {
            gate.acknowledge();
        }
        ExitAnswer::Cancel => {
            if let Some(pending) = gate.take() {
                settle(app, pending, false);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_unarmed_gate_lets_every_quit_through_without_opening_a_request() {
        let gate = ExitGate::default();
        assert_eq!(gate.open(Origin::AppKit), (Begin::Proceed, None));
        assert!(gate.take().is_none());
        assert!(!gate.acknowledge(), "nothing to acknowledge");
    }

    #[test]
    fn one_request_at_a_time_and_each_is_answered_once() {
        let gate = ExitGate::default();
        gate.arm();
        let (first, id) = gate.open(Origin::AppKit);
        assert_eq!(first, Begin::Deferred);
        assert_eq!(
            gate.open(Origin::Internal),
            (Begin::AlreadyPending, None),
            "a second quit must not open a second request"
        );
        let taken = gate.take().expect("the open request");
        assert_eq!(taken.origin, Origin::AppKit);
        assert!(gate.take().is_none(), "a request is answered exactly once");
        assert!(
            gate.expire(id.unwrap()).is_none(),
            "a late deadline settles nothing"
        );
    }

    #[test]
    fn the_deadline_settles_only_its_own_unacknowledged_request() {
        let gate = ExitGate::default();
        gate.arm();
        let (_, old) = gate.open(Origin::Internal);
        gate.take();
        let (_, new) = gate.open(Origin::Internal);
        assert!(
            gate.expire(old.unwrap()).is_none(),
            "an old deadline must not settle a newer request"
        );
        assert!(gate.acknowledge());
        assert!(
            gate.expire(new.unwrap()).is_none(),
            "an acknowledged request waits for its person, not a timer"
        );
        assert!(gate.take().is_some(), "still open after both deadlines");
    }

    #[test]
    fn an_unacknowledged_request_expires() {
        let gate = ExitGate::default();
        gate.arm();
        let (_, id) = gate.open(Origin::AppKit);
        let expired = gate.expire(id.unwrap()).expect("unacknowledged");
        assert!(!expired.acked);
        assert!(gate.take().is_none());
    }

    #[test]
    fn disarming_hands_back_the_request_and_stops_guarding() {
        let gate = ExitGate::default();
        gate.arm();
        gate.open(Origin::AppKit);
        gate.acknowledge();
        let orphan = gate.disarm().expect("the request the page was answering");
        assert!(orphan.acked);
        assert!(!gate.is_armed());
        assert_eq!(
            gate.open(Origin::AppKit),
            (Begin::Proceed, None),
            "with no page listening, a quit goes through"
        );
    }

    #[test]
    fn a_poisoned_lock_does_not_wedge_quitting() {
        let gate = std::sync::Arc::new(ExitGate::default());
        gate.arm();
        let poisoner = std::sync::Arc::clone(&gate);
        let _ = std::thread::spawn(move || {
            let _held = poisoner.pending.lock().unwrap();
            panic!("poison the exit gate");
        })
        .join();
        assert!(gate.pending.is_poisoned());
        assert_eq!(gate.open(Origin::Internal).0, Begin::Deferred);
        assert!(gate.take().is_some());
    }

    #[test]
    fn answers_are_the_frontend_vocabulary() {
        assert_eq!(
            serde_json::from_str::<ExitAnswer>("\"seen\"").unwrap(),
            ExitAnswer::Seen
        );
        assert_eq!(
            serde_json::from_str::<ExitAnswer>("\"cancel\"").unwrap(),
            ExitAnswer::Cancel
        );
        assert!(serde_json::from_str::<ExitAnswer>("\"approve\"").is_err());
    }
}
