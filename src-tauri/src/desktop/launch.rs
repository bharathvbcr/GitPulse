//! Whether this launch becomes GitPulse, or hands its request to the GitPulse
//! already running and leaves. See [`super::instance`] for why macOS needs this
//! at all.
//!
//! A launch that is not the owner must still learn what it was launched *for*
//! before it goes, and on macOS most of that does not arrive in `argv`:
//!
//! - a banner click is a notification response, delivered to the delegate of
//!   whichever process LaunchServices started — this one;
//! - Finder's "Open with" and a folder dropped on the Dock icon are an
//!   `odoc` Apple event, delivered to the application delegate.
//!
//! Both arrive only once AppKit has finished launching. So the hand-off runs
//! AppKit briefly — no window, no Dock icon — to receive them, then forwards
//! what it heard over the connection [`super::instance::claim`] opened. Tauri
//! never starts in this process.

use std::cell::{Cell, RefCell};
use std::path::Path;
use std::sync::OnceLock;
use std::time::{Duration, Instant};

use objc2::rc::{autoreleasepool, Retained};
use objc2::runtime::{AnyObject, Bool, NSObject, NSObjectProtocol};
use objc2::{class, define_class, msg_send, sel, AnyThread, DefinedClass, MainThreadMarker};
use objc2_foundation::{NSArray, NSString};

use super::instance::{self, Claim, Primary, Request};
use super::{launch_path, second_launch, SecondLaunch};

/// The owner's guard, held for the life of the process.
static PRIMARY: OnceLock<Primary> = OnceLock::new();

/// Longest a hand-off waits for AppKit to say what it was launched for.
/// Inside [`instance::HANDLER_TIMEOUT`], which is how long the owner holds the
/// connection open for it.
const CAPTURE_LIMIT: Duration = Duration::from_millis(2500);
/// How long after an ordinary launch finishes a banner response may still
/// arrive. AppKit does not mark a launch for a notification response as
/// non-default on every release, so a default launch waits this long too.
const RESPONSE_GRACE: Duration = Duration::from_millis(300);
const PUMP_SLICE: f64 = 0.05;
/// `NSApplicationActivationPolicyAccessory`: may be active, has no Dock icon.
const POLICY_ACCESSORY: isize = 1;
/// `NSApplicationActivationPolicyProhibited`: a command-line tool.
const POLICY_PROHIBITED: isize = 2;
/// `NSApplicationActivateAllWindows`.
const ACTIVATE_ALL_WINDOWS: usize = 1;

const _: () = assert!(CAPTURE_LIMIT.as_millis() < instance::HANDLER_TIMEOUT.as_millis());
/// How long an unguarded GitPulse is given to finish quitting before this
/// launch defers to it. An approved quit is over in about a second; the
/// sidecar's reaping, its slowest step, gives up after ~1.2s.
const UNGUARDED_QUIT_WAIT: Duration = Duration::from_secs(3);
/// The identifier GitPulse ships under, as `tauri.conf.json` names it. A binary
/// run outside its bundle (`tauri dev`, `target/*/gitpulse`) has no identifier
/// of its own, yet the copy it must not double is the one LaunchServices knows
/// by this one.
const SHIPPED_IDENTIFIER: &str = "com.gitpulse.desktop";

#[link(name = "AppKit", kind = "framework")]
extern "C" {
    static NSApplicationLaunchIsDefaultLaunchKey: &'static NSString;
}
#[link(name = "Foundation", kind = "framework")]
extern "C" {
    static NSDefaultRunLoopMode: &'static NSString;
}

/// What `run` should do after [`establish`].
pub enum Launch {
    /// Start GitPulse.
    Proceed,
    /// Another GitPulse has this launch's request, or cannot be reached and
    /// must not be doubled. Return without building anything.
    Exit,
}

/// Decides this launch's role. Call first thing in `run`, on the main thread,
/// before Tauri touches AppKit.
pub fn establish() -> Launch {
    let Some(dir) = crate::tool_config::default_config_dir() else {
        log::error!(target: "desktop", "single-instance guard did not run: the profile directory cannot be resolved");
        return unless_already_running();
    };
    match instance::claim(&dir, instance::CLAIM_DEADLINE) {
        Claim::Primary(primary) => {
            if let Some(error) = primary.mailbox_error() {
                log::warn!(target: "desktop", "single instance: owner, but other launches cannot reach it: {error}");
            }
            // A GitPulse that answers to no lock here: one from before this
            // guard existed, or (in a test) one on another profile. Only
            // LaunchServices can see it. On `Exit`, `primary` drops on return,
            // taking its mailbox with it.
            let launch = unless_already_running();
            if let Launch::Proceed = launch {
                let _ = PRIMARY.set(primary);
            }
            launch
        }
        Claim::Secondary(owner) => {
            let pid = owner.owner_pid();
            let request = capture(&std::env::args().collect::<Vec<_>>());
            let Some(request) = request else {
                log::info!(target: "desktop", "single instance: GitPulse {pid} is running; this launch asked for nothing");
                return Launch::Exit;
            };
            let reveals = !matches!(request, Request::Notification { snooze: true, .. });
            match owner.forward(&request) {
                Ok(()) => {
                    log::info!(target: "desktop", "single instance: handed {request:?} to GitPulse {pid}")
                }
                Err(error) => {
                    log::error!(target: "desktop", "single instance: GitPulse {pid} did not take {request:?}: {error}")
                }
            }
            if reveals {
                bring_forward(pid);
            }
            Launch::Exit
        }
        Claim::Blocked { pid, reason } => {
            log::error!(target: "desktop", "single instance: GitPulse {pid:?} holds the profile but did not answer ({reason}); not starting a second one");
            if let Some(pid) = pid {
                bring_forward(pid);
            }
            Launch::Exit
        }
        Claim::Unavailable(reason) => {
            log::error!(target: "desktop", "single-instance guard could not run: {reason}");
            unless_already_running()
        }
    }
}

/// Requests other launches send from now on go to `app`; any that arrived
/// while this one was starting are delivered first.
pub fn attach(app: &tauri::AppHandle) {
    if let Some(primary) = PRIMARY.get() {
        let app = app.clone();
        primary.attach(move |request| super::answer_forwarded(&app, request));
    }
}

/// Stops answering other launches. The lock is kept until the process is gone.
pub fn shutdown() {
    if let Some(primary) = PRIMARY.get() {
        primary.shutdown();
    }
}

/// What this launch was started to do, in the terms the owner accepts.
///
/// Pure. `response` wins: a banner response is the most specific thing a launch
/// can be for. Then documents AppKit delivered, then the command line.
pub fn request_for(
    argv: &[String],
    cwd: &Path,
    opened: &[String],
    response: Option<(String, bool)>,
) -> Option<Request> {
    if let Some((id, snooze)) = response {
        return Some(Request::Notification { id, snooze });
    }
    let absolute = |path: &str| launch_path(path, cwd).to_string_lossy().into_owned();
    if !opened.is_empty() {
        return Some(Request::Open {
            paths: opened.iter().map(|path| absolute(path)).collect(),
        });
    }
    match second_launch(argv) {
        // A relative path this launch cannot anchor (its cwd was unreadable)
        // would name a directory of the owner's choosing. Ask only to be seen.
        SecondLaunch::Open(path) if !launch_path(path, cwd).is_absolute() => Some(Request::Reveal),
        SecondLaunch::Open(path) => Some(Request::Open {
            paths: vec![absolute(path)],
        }),
        SecondLaunch::Reveal => Some(Request::Reveal),
        SecondLaunch::Ignore => None,
    }
}

fn capture(argv: &[String]) -> Option<Request> {
    let cwd = std::env::current_dir().unwrap_or_default();
    // Without a bundle there is no LaunchServices launch to read: no Apple
    // events and no notification centre. The command line is everything.
    let bundled = objc2_foundation::NSBundle::mainBundle()
        .bundleIdentifier()
        .is_some();
    if !bundled || MainThreadMarker::new().is_none() {
        return request_for(argv, &cwd, &[], None);
    }
    let heard = listen();
    request_for(argv, &cwd, &heard.opened, heard.response)
}

struct Heard {
    opened: Vec<String>,
    response: Option<(String, bool)>,
}

struct CaptureIvars {
    opened: RefCell<Vec<String>>,
    /// `Some(is_default_launch)` once AppKit finished launching.
    finished: Cell<Option<bool>>,
}

define_class!(
    // SAFETY: NSObject has no subclassing requirements. The methods match
    // NSApplicationDelegate's `application:openURLs:` and
    // `applicationDidFinishLaunching:` signatures.
    #[unsafe(super = NSObject)]
    #[ivars = CaptureIvars]
    struct LaunchCapture;

    unsafe impl NSObjectProtocol for LaunchCapture {}

    impl LaunchCapture {
        #[unsafe(method(application:openURLs:))]
        fn open_urls(&self, _application: &AnyObject, urls: &NSArray<AnyObject>) {
            for url in urls.iter() {
                // SAFETY: AppKit passes NSURL instances; both are NSURL methods.
                let is_file: Bool = unsafe { msg_send![&*url, isFileURL] };
                if !is_file.as_bool() {
                    continue;
                }
                let path: Option<Retained<NSString>> = unsafe { msg_send![&*url, path] };
                if let Some(path) = path {
                    self.ivars().opened.borrow_mut().push(path.to_string());
                }
            }
        }

        #[unsafe(method(applicationDidFinishLaunching:))]
        fn did_finish_launching(&self, notification: &AnyObject) {
            self.ivars().finished.set(Some(is_default_launch(notification)));
        }
    }
);

fn is_default_launch(notification: &AnyObject) -> bool {
    // SAFETY: an NSNotification; its userInfo is an NSDictionary or nil, and
    // the value under this key is an NSNumber when present.
    unsafe {
        let info: Option<Retained<AnyObject>> = msg_send![notification, userInfo];
        let Some(info) = info else { return true };
        let value: Option<Retained<AnyObject>> =
            msg_send![&*info, objectForKey: NSApplicationLaunchIsDefaultLaunchKey];
        match value {
            Some(value) => {
                let flag: Bool = msg_send![&*value, boolValue];
                flag.as_bool()
            }
            None => true,
        }
    }
}

/// Runs AppKit just long enough to hear what LaunchServices launched us for.
fn listen() -> Heard {
    let tap = match crate::workbench::notifications::responses() {
        Ok(tap) => Some(tap),
        Err(error) => {
            log::warn!(target: "desktop", "single instance: a banner response cannot be received by this launch: {error}");
            None
        }
    };
    let capture = LaunchCapture::alloc().set_ivars(CaptureIvars {
        opened: RefCell::new(Vec::new()),
        finished: Cell::new(None),
    });
    // SAFETY: NSObject's init initialises this freshly allocated subclass.
    let capture: Retained<LaunchCapture> = unsafe { msg_send![super(capture), init] };
    let mut response = None;
    // SAFETY: main thread (checked by the caller). NSApplication's delegate is
    // unretained, so `capture` outlives the assignment and is cleared below.
    unsafe {
        let app: Retained<AnyObject> = msg_send![class!(NSApplication), sharedApplication];
        let _: Bool = msg_send![&*app, setActivationPolicy: POLICY_ACCESSORY];
        let _: () = msg_send![&*app, setDelegate: &*capture];
        let _: () = msg_send![&*app, finishLaunching];
        let started = Instant::now();
        let mut finished_at: Option<Instant> = None;
        while started.elapsed() < CAPTURE_LIMIT {
            autoreleasepool(|_| {
                let until: Retained<AnyObject> =
                    msg_send![class!(NSDate), dateWithTimeIntervalSinceNow: PUMP_SLICE];
                let event: Option<Retained<AnyObject>> = msg_send![
                    &*app,
                    nextEventMatchingMask: usize::MAX,
                    untilDate: &*until,
                    inMode: NSDefaultRunLoopMode,
                    dequeue: Bool::YES
                ];
                if let Some(event) = event {
                    let _: () = msg_send![&*app, sendEvent: &*event];
                }
            });
            if response.is_none() {
                response = tap.as_ref().and_then(|tap| tap.try_take());
            }
            if response.is_some() {
                break;
            }
            let Some(default) = capture.ivars().finished.get() else {
                continue;
            };
            let finished = *finished_at.get_or_insert_with(Instant::now);
            if !capture.ivars().opened.borrow().is_empty() {
                break;
            }
            // A non-default launch with nothing opened is most likely a banner
            // response still on its way; wait for it up to the limit.
            if default && finished.elapsed() >= RESPONSE_GRACE {
                break;
            }
        }
        let _: () = msg_send![&*app, setDelegate: Option::<&AnyObject>::None];
    }
    let opened = capture.ivars().opened.take();
    Heard { opened, response }
}

/// `Proceed` only when LaunchServices knows no other GitPulse. The last word
/// for every launch that would start, whether or not the profile guard ran:
/// a guard that could not run is no reason to double a GitPulse it cannot see.
fn unless_already_running() -> Launch {
    let Some(pid) = unguarded_instance() else {
        return Launch::Proceed;
    };
    log::warn!(target: "desktop", "single instance: GitPulse {pid} is running but not under this profile's guard; bringing it forward instead of starting");
    bring_forward(pid);
    Launch::Exit
}

/// Whose running copies count as GitPulse for this launch: its own bundle's,
/// or the shipped app's when it was started outside a bundle.
fn watched_identifier(own: Option<String>) -> String {
    own.unwrap_or_else(|| SHIPPED_IDENTIFIER.to_owned())
}

/// Another process that LaunchServices knows as GitPulse and that is not this
/// one: the signature of a copy started before this guard existed. Waits,
/// bounded, for one that is quitting to finish.
fn unguarded_instance() -> Option<i32> {
    let own_identifier = objc2_foundation::NSBundle::mainBundle()
        .bundleIdentifier()
        .map(|identifier| identifier.to_string());
    let identifier = NSString::from_str(&watched_identifier(own_identifier));
    let own = std::process::id() as i32;
    let started = Instant::now();
    loop {
        let other = autoreleasepool(|_| {
            // SAFETY: class methods and properties of NSRunningApplication;
            // the array holds NSRunningApplication instances.
            unsafe {
                let apps: Retained<NSArray<AnyObject>> = msg_send![
                    class!(NSRunningApplication),
                    runningApplicationsWithBundleIdentifier: &*identifier
                ];
                apps.iter().find_map(|app| {
                    let pid: i32 = msg_send![&*app, processIdentifier];
                    let terminated: Bool = msg_send![&*app, isTerminated];
                    let policy: isize = msg_send![&*app, activationPolicy];
                    (pid != own && !terminated.as_bool() && policy != POLICY_PROHIBITED)
                        .then_some(pid)
                })
            }
        });
        match other {
            None => return None,
            Some(pid) if started.elapsed() >= UNGUARDED_QUIT_WAIT => return Some(pid),
            Some(_) => std::thread::sleep(Duration::from_millis(100)),
        }
    }
}

/// Hands the foreground to GitPulse `pid`. Best effort: the owner reveals its
/// own window too, but on macOS 14+ an app in the background may not take
/// focus unless the active app — this launch, which the user just started —
/// yields it.
fn bring_forward(pid: impl TryInto<i32>) {
    let Ok(pid) = pid.try_into() else { return };
    if MainThreadMarker::new().is_none() {
        return;
    }
    autoreleasepool(|_| {
        // SAFETY: main thread; NSApplication and NSRunningApplication methods,
        // with `yieldActivationToApplication:` probed before it is sent.
        unsafe {
            let target: Option<Retained<AnyObject>> = msg_send![
                class!(NSRunningApplication),
                runningApplicationWithProcessIdentifier: pid
            ];
            let Some(target) = target else { return };
            let app: Retained<AnyObject> = msg_send![class!(NSApplication), sharedApplication];
            let yields: Bool =
                msg_send![&*app, respondsToSelector: sel!(yieldActivationToApplication:)];
            if yields.as_bool() {
                let _: () = msg_send![&*app, yieldActivationToApplication: &*target];
            }
            let activated: Bool = msg_send![&*target, activateWithOptions: ACTIVATE_ALL_WINDOWS];
            if !activated.as_bool() {
                log::info!(target: "desktop", "single instance: macOS declined to activate GitPulse {pid}");
            }
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    fn argv(args: &[&str]) -> Vec<String> {
        std::iter::once("gitpulse")
            .chain(args.iter().copied())
            .map(str::to_owned)
            .collect()
    }

    #[test]
    fn a_banner_response_outranks_everything_else_a_launch_carries() {
        let request = request_for(
            &argv(&["/repo"]),
            Path::new("/cwd"),
            &["/opened".into()],
            Some(("native-1".into(), true)),
        );
        assert_eq!(
            request,
            Some(Request::Notification {
                id: "native-1".into(),
                snooze: true
            })
        );
    }

    #[test]
    fn documents_from_launchservices_outrank_the_command_line() {
        let request = request_for(
            &argv(&["/repo"]),
            Path::new("/cwd"),
            &["/a".into(), "/b".into()],
            None,
        );
        assert_eq!(
            request,
            Some(Request::Open {
                paths: vec!["/a".into(), "/b".into()]
            })
        );
    }

    #[test]
    fn a_relative_path_is_the_launching_shells_not_the_owners() {
        assert_eq!(
            request_for(&argv(&["."]), Path::new("/Users/me/repo"), &[], None),
            Some(Request::Open {
                paths: vec!["/Users/me/repo/.".into()]
            })
        );
        assert_eq!(
            request_for(&argv(&["rel"]), Path::new(""), &[], None),
            Some(Request::Reveal),
            "an unanchored relative path must not be resolved by the owner"
        );
        assert_eq!(
            request_for(&argv(&["--", "-dash"]), Path::new("/w"), &[], None),
            Some(Request::Open {
                paths: vec!["/w/-dash".into()]
            })
        );
    }

    #[test]
    fn a_plain_launch_reveals_and_a_login_item_launch_asks_for_nothing() {
        assert_eq!(
            request_for(&argv(&[]), Path::new("/"), &[], None),
            Some(Request::Reveal)
        );
        // The old process-serial-number argument some launches still carry.
        assert_eq!(
            request_for(&argv(&["-psn_0_12345"]), Path::new("/"), &[], None),
            Some(Request::Reveal)
        );
        assert_eq!(
            request_for(&argv(&["--background"]), Path::new("/"), &[], None),
            None
        );
        // An explicit repository still wins over `--background`.
        assert_eq!(
            request_for(&argv(&["--background", "/r"]), Path::new("/"), &[], None),
            Some(Request::Open {
                paths: vec!["/r".into()]
            })
        );
    }

    #[test]
    fn a_launch_outside_a_bundle_still_looks_for_the_shipped_app() {
        // Before this, a `tauri dev` or `target/*/gitpulse` launch had no
        // identifier to look under and started beside the installed app.
        assert_eq!(watched_identifier(None), SHIPPED_IDENTIFIER);
        // A bundle's own identifier wins, so a test copy under another
        // identifier only defers to copies of itself.
        assert_eq!(
            watched_identifier(Some("com.gitpulse.desktop.e2e".into())),
            "com.gitpulse.desktop.e2e"
        );
    }

    #[test]
    fn the_shipped_identifier_is_the_one_the_bundle_is_built_with() {
        let config: serde_json::Value = serde_json::from_str(include_str!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/tauri.conf.json"
        )))
        .expect("tauri.conf.json parses");
        assert_eq!(config["identifier"], SHIPPED_IDENTIFIER);
        let macos: serde_json::Value = serde_json::from_str(include_str!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/tauri.macos.conf.json"
        )))
        .expect("tauri.macos.conf.json parses");
        assert!(
            macos.get("identifier").is_none(),
            "the macOS overlay must not rename the bundle out from under the fallback"
        );
    }

    #[test]
    fn every_request_a_launch_can_form_is_one_the_owner_accepts() {
        let cases = [
            request_for(&argv(&["rel/path"]), Path::new("/abs"), &[], None),
            request_for(&argv(&[]), Path::new(""), &[], None),
            request_for(&argv(&["rel"]), Path::new(""), &[], None),
            request_for(&argv(&[]), Path::new("/"), &["/x".into()], None),
            request_for(&argv(&[]), Path::new("/"), &[], Some(("n".into(), false))),
        ];
        for request in cases.into_iter().flatten() {
            assert_eq!(request.validate(), Ok(()), "{request:?}");
        }
    }
}
