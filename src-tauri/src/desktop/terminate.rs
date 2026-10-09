//! AppKit's own quit requests: the app menu's Quit item (Cmd-Q), Dock → Quit,
//! a Quit Apple event (`osascript -e 'quit app id "com.gitpulse.desktop"'`),
//! and logout or restart.
//!
//! TAO's application delegate implements `applicationWillTerminate:` but not
//! `applicationShouldTerminate:`, so AppKit approved every one of these without
//! asking anyone, and Tauri never raised `RunEvent::ExitRequested` for them. The
//! exit guard that protects unsaved editor drafts and running Git actions only
//! ever saw the status menu's Quit item and the main window's close button.
//! This adds the missing delegate method to the delegate's class and answers it
//! from `exit::begin`, the same owner every other quit goes through.
//!
//! A quit the frontend must confirm is answered `NSTerminateLater`. AppKit then
//! waits in a modal run loop until `reply` is called; TAO's event-loop source
//! and observers are registered for the common run-loop modes, so the
//! frontend's answer and the acknowledgement deadline are still dispatched
//! while it waits. An approved quit therefore leaves through AppKit's own
//! `terminate:` path (`applicationWillTerminate:` → `RunEvent::Exit`), and a
//! logout waits for the answer instead of being refused.
//!
//! A second `terminate:` while that wait is open is not asked about: AppKit
//! terminates at once (`native_quit_guard`'s `double` scenario). That makes a
//! second Cmd-Q the way out of a confirmation whose page has wedged. It must
//! never be sent from inside a TAO callback: AppKit's immediate termination
//! re-enters TAO's handler, whose lock that callback holds.

use std::ffi::CStr;
use std::panic::{catch_unwind, AssertUnwindSafe};
use std::sync::{Arc, RwLock};

use objc2::runtime::{AnyClass, AnyObject, Bool, Imp, Sel};
use objc2::{class, ffi, msg_send, sel};
use tauri::{AppHandle, Runtime};

use super::exit::{self, Begin, Origin};

/// `NSApplicationTerminateReply`, with the values objc2-app-kit's generated
/// `NSApplication.rs` declares.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(usize)]
pub enum Reply {
    Cancel = 0,
    Now = 1,
    Later = 2,
}

impl From<Begin> for Reply {
    fn from(begin: Begin) -> Self {
        match begin {
            Begin::Proceed => Reply::Now,
            Begin::Deferred => Reply::Later,
            // The waiting request will be replied to; this one owes no reply,
            // so it must not be `Later`.
            Begin::AlreadyPending => Reply::Cancel,
        }
    }
}

/// `- (NSApplicationTerminateReply)applicationShouldTerminate:(NSApplication *)`.
/// `NSUInteger` is `unsigned long` (`Q`) on both arm64 and x86_64.
const SIGNATURE: &CStr = c"Q@:@";

type Decide = Arc<dyn Fn() -> Reply + Send + Sync>;

static DECIDE: RwLock<Option<Decide>> = RwLock::new(None);

extern "C-unwind" fn should_terminate(_: &AnyObject, _: Sel, _: *mut AnyObject) -> usize {
    // Nobody to ask means nobody can answer a held quit: let it proceed.
    let Some(decide) = DECIDE.read().ok().and_then(|slot| slot.clone()) else {
        return Reply::Now as usize;
    };
    match catch_unwind(AssertUnwindSafe(|| decide())) {
        Ok(reply) => reply as usize,
        Err(_) => {
            log::error!(target: "desktop", "quit guard panicked; letting AppKit terminate");
            Reply::Now as usize
        }
    }
}

/// Adds `applicationShouldTerminate:` to `class`, answered by `decide`.
///
/// Refuses a class that already answers it rather than replacing someone
/// else's policy: if TAO grows its own, this must be revisited, not raced.
fn install_on(class: &AnyClass, decide: Decide) -> Result<(), String> {
    let selector = sel!(applicationShouldTerminate:);
    if class.instance_method(selector).is_some() {
        return Err(format!(
            "{} already answers applicationShouldTerminate:",
            class.name().to_string_lossy()
        ));
    }
    // SAFETY: `should_terminate` has exactly the ABI `SIGNATURE` declares
    // (self, _cmd, sender) -> NSUInteger; the runtime calls it through `Imp`.
    let added = unsafe {
        let imp: Imp = std::mem::transmute::<
            extern "C-unwind" fn(&AnyObject, Sel, *mut AnyObject) -> usize,
            Imp,
        >(should_terminate);
        ffi::class_addMethod(
            std::ptr::from_ref(class).cast_mut(),
            selector,
            imp,
            SIGNATURE.as_ptr(),
        )
    };
    if !added.as_bool() {
        return Err("the Objective-C runtime refused applicationShouldTerminate:".into());
    }
    *DECIDE
        .write()
        .map_err(|_| "quit guard lock poisoned".to_string())? = Some(decide);
    Ok(())
}

/// Routes AppKit's quit requests through the exit guard. Call once from setup,
/// on the main thread, after TAO has installed its application delegate.
pub fn install<R: Runtime>(app: &AppHandle<R>) -> Result<(), String> {
    // SAFETY: setup runs on the main thread inside the running NSApplication;
    // both messages are plain getters.
    let class = unsafe {
        let ns_app: *mut AnyObject = msg_send![class!(NSApplication), sharedApplication];
        let delegate: *mut AnyObject = msg_send![ns_app, delegate];
        if delegate.is_null() {
            return Err("NSApplication has no delegate".into());
        }
        (*delegate).class()
    };
    let app = app.clone();
    install_on(
        class,
        Arc::new(move || Reply::from(exit::begin(&app, Origin::AppKit))),
    )?;
    log::info!(
        target: "setup",
        "quit guard answers applicationShouldTerminate: on {}",
        class.name().to_string_lossy()
    );
    Ok(())
}

/// Answers a quit that was answered `Later`. Main thread only; `exit` calls it
/// exactly once per deferred request.
pub(super) fn reply(approve: bool) {
    log::info!(
        target: "desktop",
        "answering AppKit's quit request: {}",
        if approve { "terminate" } else { "keep running" }
    );
    // SAFETY: called on the main thread (through `run_on_main_thread`) while
    // AppKit waits for this reply.
    unsafe {
        let ns_app: *mut AnyObject = msg_send![class!(NSApplication), sharedApplication];
        let () = msg_send![ns_app, replyToApplicationShouldTerminate: Bool::new(approve)];
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use objc2::rc::Retained;
    use objc2::runtime::ClassBuilder;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::Mutex;

    #[test]
    fn every_appkit_quit_asks_the_guard_and_returns_its_reply() {
        let class = ClassBuilder::new(c"GitPulseTerminateProbe", class!(NSObject))
            .expect("probe class name is unique")
            .register();
        let next = Arc::new(Mutex::new(Reply::Later));
        let asked = Arc::new(AtomicUsize::new(0));
        let decide: Decide = {
            let next = Arc::clone(&next);
            let asked = Arc::clone(&asked);
            Arc::new(move || {
                asked.fetch_add(1, Ordering::SeqCst);
                *next.lock().unwrap()
            })
        };
        install_on(class, Arc::clone(&decide)).expect("install on a fresh class");

        let delegate: Retained<AnyObject> = unsafe { msg_send![class, new] };
        let quit = || -> usize {
            unsafe {
                msg_send![&*delegate, applicationShouldTerminate: std::ptr::null_mut::<AnyObject>()]
            }
        };
        for reply in [Reply::Later, Reply::Cancel, Reply::Now] {
            *next.lock().unwrap() = reply;
            assert_eq!(
                quit(),
                reply as usize,
                "AppKit must get the guard's {reply:?}"
            );
        }
        assert_eq!(asked.load(Ordering::SeqCst), 3, "every quit asks the guard");

        assert!(
            install_on(class, decide).is_err(),
            "an existing applicationShouldTerminate: must not be silently replaced"
        );
    }

    #[test]
    fn a_routing_outcome_maps_to_the_reply_appkit_needs() {
        assert_eq!(Reply::from(Begin::Proceed), Reply::Now);
        assert_eq!(Reply::from(Begin::Deferred), Reply::Later);
        assert_eq!(
            Reply::from(Begin::AlreadyPending),
            Reply::Cancel,
            "a second Later would owe a reply nothing will send"
        );
        assert_eq!(
            (
                Reply::Cancel as usize,
                Reply::Now as usize,
                Reply::Later as usize
            ),
            (0, 1, 2),
            "NSTerminateCancel, NSTerminateNow, NSTerminateLater"
        );
    }
}
