//! Real Tao Objective-C callbacks must let AppKit exceptions reach their native catcher.
//! Each case runs on a fresh process's main thread; a foreign-unwind abort cannot
//! be caught by libtest, and AppKit requires the process main thread.

#[cfg(target_os = "macos")]
mod macos {
    use objc2::exception::{catch, throw, Exception};
    use objc2::rc::Retained;
    use objc2::runtime::{AnyClass, AnyObject, ClassBuilder, Imp, Method, Sel};
    use objc2::{class, msg_send, sel};
    use objc2_foundation::{NSPoint, NSString};
    use std::process::{Command, Stdio};
    use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
    use std::time::{Duration, Instant};

    static THROW: AtomicBool = AtomicBool::new(false);
    static CALLS: AtomicUsize = AtomicUsize::new(0);

    extern "C-unwind" fn probe_event(_this: &AnyObject, _sel: Sel, _event: &AnyObject) {
        CALLS.fetch_add(1, Ordering::SeqCst);
        if THROW.load(Ordering::SeqCst) {
            let name = NSString::from_str("GitPulseEventProbe");
            let reason = NSString::from_str("native event exception");
            // SAFETY: NSException's factory returns an autoreleased exception;
            // msg_send retains it before throw transfers its ownership.
            let exception: Retained<Exception> = unsafe {
                msg_send![class!(NSException), exceptionWithName: &*name,
                    reason: &*reason, userInfo: std::ptr::null::<AnyObject>()]
            };
            throw(exception);
        }
    }

    struct RestoreMethod<'a>(&'a Method, Imp);

    impl Drop for RestoreMethod<'_> {
        fn drop(&mut self) {
            // SAFETY: Restore the original implementation of the same method.
            unsafe { self.0.set_implementation(self.1) };
        }
    }

    fn exercise(case: &str) {
        let app = tauri::Builder::default()
            .build(tauri::test::mock_context(tauri::test::noop_assets()))
            .expect("real Tauri runtime");
        let window = tauri::WebviewWindowBuilder::new(
            &app,
            "unwind-probe",
            tauri::WebviewUrl::External("about:blank".parse().unwrap()),
        )
        .visible(false)
        .build()
        .expect("real Tao window");
        // SAFETY: Both objects live until after this probe; all messages and
        // method replacements occur on this isolated process's main thread.
        unsafe {
            let application: &AnyObject = msg_send![class!(NSApplication), sharedApplication];
            let native_window = &*window.ns_window().expect("NSWindow").cast::<AnyObject>();
            // AppKit can wrap these objects in NSKVONotifying subclasses.
            let is_tao_app: bool = msg_send![application, isKindOfClass: class!(TaoApp)];
            let is_tao_window: bool = msg_send![native_window, isKindOfClass: class!(TaoWindow)];
            assert!(
                is_tao_app && is_tao_window,
                "probe must traverse Tao's real callbacks"
            );
            let mut probe = ClassBuilder::new(c"GitPulseEventProbe", class!(NSObject)).unwrap();
            probe.add_method(
                sel!(sendEvent:),
                probe_event as extern "C-unwind" fn(_, _, _),
            );
            let probe_class = probe.register();
            let parent: &AnyClass = if case == "app" {
                class!(NSApplication)
            } else {
                class!(NSWindow)
            };
            let method = parent
                .instance_method(sel!(sendEvent:))
                .expect("AppKit sendEvent:");
            let previous = method.set_implementation(
                probe_class
                    .instance_method(sel!(sendEvent:))
                    .unwrap()
                    .implementation(),
            );
            let _restore = RestoreMethod(method, previous);
            // ApplicationDefined bypasses device-event synthesis and reaches
            // the actual superclass sendEvent: forwarding seam.
            let event: Retained<AnyObject> = msg_send![class!(NSEvent),
                otherEventWithType: 15usize, location: NSPoint::new(0.0, 0.0),
                modifierFlags: 0usize, timestamp: 0.0f64, windowNumber: 0isize,
                context: std::ptr::null::<AnyObject>(), subtype: 0i16,
                data1: 0isize, data2: 0isize];
            let receiver = if case == "app" {
                application
            } else {
                native_window
            };
            let call = || {
                let (): () = msg_send![receiver, sendEvent: &*event];
            };
            for _ in 0..256 {
                THROW.store(false, Ordering::SeqCst);
                call();
                THROW.store(true, Ordering::SeqCst);
                // objc2 uses an Objective-C @catch here, not Rust catch_unwind.
                // The replacement only increments an atomic and throws; it
                // cannot leave either AppKit object partially mutated.
                let result = catch(std::panic::AssertUnwindSafe(call));
                let exception = result
                    .expect_err("injected exception must reach its native catcher")
                    .expect("exception object");
                assert!(exception.to_string().contains("native event exception"));
            }
            assert_eq!(CALLS.load(Ordering::SeqCst), 512);
        }
        window.destroy().expect("destroy probe window");
    }

    pub fn run() {
        if let Some(case) = std::env::args()
            .nth(1)
            .filter(|arg| arg.starts_with("--probe="))
        {
            exercise(case.trim_start_matches("--probe="));
            return;
        }
        let mut failures = Vec::new();
        for case in ["app", "window"] {
            let mut child = Command::new(std::env::current_exe().expect("test executable"))
                .arg(format!("--probe={case}"))
                .stdin(Stdio::null())
                .spawn()
                .expect("spawn native probe");
            let deadline = Instant::now() + Duration::from_secs(30);
            let status = loop {
                if let Some(status) = child.try_wait().expect("probe status") {
                    break status;
                }
                if Instant::now() >= deadline {
                    child.kill().expect("kill timed out native probe");
                    break child.wait().expect("reap native probe");
                }
                std::thread::sleep(Duration::from_millis(10));
            };
            println!("native {case} event exception: {status}");
            if !status.success() {
                failures.push(case);
            }
        }
        assert!(
            failures.is_empty(),
            "native event exception failures: {failures:?}"
        );
    }
}

#[cfg(target_os = "macos")]
fn main() {
    macos::run();
}

#[cfg(not(target_os = "macos"))]
fn main() {
    println!("native_event_unwind: inapplicable off macOS");
}
