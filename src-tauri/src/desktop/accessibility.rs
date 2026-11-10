//! The system's Reduce Transparency setting (System Settings → Accessibility →
//! Display), read from AppKit and pushed to the webviews.
//!
//! WKWebView does not implement the `prefers-reduced-transparency` media
//! feature: `(prefers-reduced-transparency: reduce)` and `(… : no-preference)`
//! both evaluate false, exactly like a made-up feature (probed 2026-11-09 on
//! macOS 27.2). So the CSS fallbacks keyed on it never fire for a user who
//! turned the setting on. This module is the bridge: the frontend stamps
//! `data-transparency="reduced"` on `<html>` from what it sends.
//!
//! One path for every platform. Off macOS there is no such setting to read, so
//! the answer is [`Transparency::Standard`] and the frontend needs no branch.
//!
//! A read that cannot run is [`Transparency::Unknown`] with the reason, never
//! `Standard` and never `Reduced`: a check that could not run must not look
//! like one that passed, nor claim a preference nobody expressed.
//!
//! Delivery is a command plus an event, the shape `popover` uses for the
//! status state: [`cmd_get_accessibility_display`] answers the initial value
//! (and installs the observer on first use, so a window that never asks costs
//! nothing), and [`ACCESSIBILITY_EVENT`] carries every change after. `seq`
//! orders them: a slow command reply must not overwrite a newer event.

use std::sync::atomic::{AtomicU64, Ordering};

use serde::Serialize;
use tauri::{AppHandle, Runtime};

/// Emitted to every window whenever the setting is re-read after a change.
pub const ACCESSIBILITY_EVENT: &str = "gitpulse-accessibility-display";

/// Source of [`AccessibilityDisplay::seq`]; each read takes the next number.
static SEQUENCE: AtomicU64 = AtomicU64::new(1);

/// What the system says about transparency.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Transparency {
    /// Reduce Transparency is on: surfaces must be opaque.
    Reduced,
    /// Reduce Transparency is off, or the platform has no such setting.
    Standard,
    /// The read could not run. Not a statement about the preference.
    Unknown,
}

/// The payload of both the command and the event.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AccessibilityDisplay {
    pub reduce_transparency: Transparency,
    /// Why the answer is `unknown`, or why changes are not being observed.
    pub reason: Option<String>,
    /// Orders reads: a larger number is a later read.
    pub seq: u64,
}

/// The pure mapping from what a read returned to what the frontend is told.
///
/// `observed` is the outcome of installing the change observer. A failed read
/// is `Unknown` whatever the observer did; a good read keeps its value and
/// carries the observer's failure as `reason`, so the frontend can say that
/// later changes will not arrive by themselves.
pub fn describe(
    read: Result<bool, String>,
    observed: &Result<(), String>,
    seq: u64,
) -> AccessibilityDisplay {
    match read {
        Ok(reduced) => AccessibilityDisplay {
            reduce_transparency: if reduced {
                Transparency::Reduced
            } else {
                Transparency::Standard
            },
            reason: observed
                .as_ref()
                .err()
                .map(|why| format!("changes to Reduce Transparency are not observed: {why}")),
            seq,
        },
        Err(why) => AccessibilityDisplay {
            reduce_transparency: Transparency::Unknown,
            reason: Some(why),
            seq,
        },
    }
}

fn read_now(observed: &Result<(), String>) -> AccessibilityDisplay {
    let seq = SEQUENCE.fetch_add(1, Ordering::SeqCst);
    let display = describe(platform::reduce_transparency(), observed, seq);
    if display.reduce_transparency == Transparency::Unknown {
        log::warn!(
            target: "desktop",
            "Reduce Transparency could not be read: {}",
            display.reason.as_deref().unwrap_or("no reason given")
        );
    }
    display
}

/// The current value, installing the change observer on first use.
pub fn current<R: Runtime>(app: &AppHandle<R>) -> AccessibilityDisplay {
    let observed = platform::observe(app);
    read_now(&observed)
}

#[tauri::command]
pub fn cmd_get_accessibility_display(app: AppHandle) -> AccessibilityDisplay {
    current(&app)
}

#[cfg(target_os = "macos")]
mod platform {
    use std::ptr::NonNull;
    use std::sync::OnceLock;

    use block2::RcBlock;
    use objc2::rc::Retained;
    use objc2::runtime::{AnyClass, AnyObject, Bool};
    use objc2::{msg_send, sel};
    use objc2_foundation::NSString;
    use tauri::{AppHandle, Emitter, Runtime};

    use super::{read_now, ACCESSIBILITY_EVENT};

    #[link(name = "AppKit", kind = "framework")]
    extern "C" {
        /// Posted on `NSWorkspace.notificationCenter` when any accessibility
        /// display option changes (declared in `NSAccessibility.h`).
        static NSWorkspaceAccessibilityDisplayOptionsDidChangeNotification: &'static NSString;
    }

    fn workspace() -> Result<Retained<AnyObject>, String> {
        let class = AnyClass::get(c"NSWorkspace").ok_or("NSWorkspace is not loaded")?;
        // SAFETY: `sharedWorkspace` is a plain class getter returning an
        // NSWorkspace or nil.
        let workspace: Option<Retained<AnyObject>> = unsafe { msg_send![class, sharedWorkspace] };
        workspace.ok_or_else(|| "NSWorkspace has no shared instance".to_string())
    }

    /// `NSWorkspace.shared.accessibilityDisplayShouldReduceTransparency`.
    pub(super) fn reduce_transparency() -> Result<bool, String> {
        let workspace = workspace()?;
        // SAFETY: `respondsToSelector:` is NSObject's; the property getter
        // takes no arguments and returns BOOL (NSAccessibility.h, macOS 10.10).
        unsafe {
            let responds: Bool = msg_send![
                &*workspace,
                respondsToSelector: sel!(accessibilityDisplayShouldReduceTransparency)
            ];
            if !responds.as_bool() {
                return Err(
                    "NSWorkspace does not answer accessibilityDisplayShouldReduceTransparency"
                        .into(),
                );
            }
            let reduced: Bool =
                msg_send![&*workspace, accessibilityDisplayShouldReduceTransparency];
            Ok(reduced.as_bool())
        }
    }

    fn install<R: Runtime>(app: &AppHandle<R>) -> Result<(), String> {
        let workspace = workspace()?;
        // SAFETY: `notificationCenter` is a readonly property returning an
        // NSNotificationCenter.
        let center: Option<Retained<AnyObject>> =
            unsafe { msg_send![&*workspace, notificationCenter] };
        let center = center.ok_or("NSWorkspace has no notification center")?;
        let app = app.clone();
        // Runs on whichever thread posts the notification (queue is nil). It
        // re-reads rather than trusting the notification, which carries no
        // value, and an emit failure is logged, never silent.
        let block = RcBlock::new(move |_notification: NonNull<AnyObject>| {
            // The observer is installed, or this block would not be running.
            let display = read_now(&Ok(()));
            if let Err(error) = app.emit(ACCESSIBILITY_EVENT, &display) {
                log::warn!(target: "desktop", "could not emit {ACCESSIBILITY_EVENT}: {error}");
            }
        });
        // SAFETY: `addObserverForName:object:queue:usingBlock:` takes a
        // nullable name, object and queue and a block of one NSNotification
        // argument; the center copies the block.
        let token: Option<Retained<AnyObject>> = unsafe {
            msg_send![
                &*center,
                addObserverForName: Some(NSWorkspaceAccessibilityDisplayOptionsDidChangeNotification),
                object: Option::<&AnyObject>::None,
                queue: Option::<&AnyObject>::None,
                usingBlock: &*block
            ]
        };
        let token = token.ok_or("NSNotificationCenter returned no observer token")?;
        // The observer lives for the process; there is nothing to remove it for.
        std::mem::forget(token);
        Ok(())
    }

    /// Installs the observer once. Every caller gets the first outcome, so a
    /// failure is reported on every read instead of being retried per call.
    pub(super) fn observe<R: Runtime>(app: &AppHandle<R>) -> Result<(), String> {
        static OBSERVED: OnceLock<Result<(), String>> = OnceLock::new();
        OBSERVED
            .get_or_init(|| {
                let outcome = install(app);
                if let Err(why) = &outcome {
                    log::error!(
                        target: "desktop",
                        "Reduce Transparency observer not installed: {why}"
                    );
                }
                outcome
            })
            .clone()
    }
}

#[cfg(not(target_os = "macos"))]
mod platform {
    use tauri::{AppHandle, Runtime};

    /// No such setting here: not reduced, through the same path.
    pub(super) fn reduce_transparency() -> Result<bool, String> {
        Ok(false)
    }

    /// Nothing can change, so there is nothing to observe.
    pub(super) fn observe<R: Runtime>(_app: &AppHandle<R>) -> Result<(), String> {
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_read_maps_to_reduced_or_standard() {
        let on = describe(Ok(true), &Ok(()), 3);
        assert_eq!(on.reduce_transparency, Transparency::Reduced);
        assert_eq!(on.reason, None);
        assert_eq!(on.seq, 3);
        let off = describe(Ok(false), &Ok(()), 4);
        assert_eq!(off.reduce_transparency, Transparency::Standard);
        assert_eq!(off.reason, None);
    }

    #[test]
    fn a_read_that_could_not_run_is_unknown_and_never_either_answer() {
        let failed = describe(Err("no workspace".into()), &Ok(()), 1);
        assert_eq!(failed.reduce_transparency, Transparency::Unknown);
        assert_eq!(failed.reason.as_deref(), Some("no workspace"));
        // Even when the observer did install: the value is still unread.
        assert_ne!(failed.reduce_transparency, Transparency::Standard);
        assert_ne!(failed.reduce_transparency, Transparency::Reduced);
    }

    #[test]
    fn a_missing_observer_keeps_the_value_and_says_changes_will_not_arrive() {
        let display = describe(Ok(true), &Err("no center".into()), 2);
        assert_eq!(display.reduce_transparency, Transparency::Reduced);
        let reason = display.reason.expect("the failure is reported");
        assert!(reason.contains("not observed") && reason.contains("no center"));
    }

    #[test]
    fn the_wire_shape_is_what_the_frontend_reads() {
        let value = serde_json::to_value(describe(Ok(true), &Ok(()), 7)).expect("serializes");
        assert_eq!(
            value,
            serde_json::json!({ "reduceTransparency": "reduced", "reason": null, "seq": 7 })
        );
        let unknown = serde_json::to_value(describe(Err("x".into()), &Ok(()), 8)).unwrap();
        assert_eq!(unknown["reduceTransparency"], "unknown");
    }

    #[test]
    fn each_read_is_numbered_after_the_one_before() {
        let first = read_now(&Ok(()));
        let second = read_now(&Ok(()));
        assert!(second.seq > first.seq);
    }

    #[cfg(not(target_os = "macos"))]
    #[test]
    fn other_platforms_are_not_reduced() {
        assert_eq!(
            read_now(&Ok(())).reduce_transparency,
            Transparency::Standard
        );
    }

    /// The real AppKit read: it must run (not `Unknown`) on a Mac, whatever
    /// the setting is. Proves the selector exists and the msg_send types agree.
    #[cfg(target_os = "macos")]
    #[test]
    fn appkit_answers_the_setting_on_this_mac() {
        let read = platform::reduce_transparency();
        assert!(read.is_ok(), "AppKit read failed: {read:?}");
    }

    /// The change path end to end: the observer is installed, the very
    /// notification AppKit posts when a display option changes is posted by
    /// hand, and a webview-visible event with a fresh `seq` comes out.
    #[cfg(target_os = "macos")]
    #[test]
    fn a_display_option_change_is_emitted_to_the_webviews() {
        use objc2::msg_send;
        use objc2::rc::Retained;
        use objc2::runtime::{AnyClass, AnyObject};
        use std::sync::{Arc, Mutex};
        use std::time::{Duration, Instant};
        use tauri::Listener;

        let app = tauri::test::mock_builder()
            .build(crate::context())
            .expect("mock app");
        let seen: Arc<Mutex<Vec<String>>> = Arc::default();
        {
            let seen = Arc::clone(&seen);
            app.handle().listen_any(ACCESSIBILITY_EVENT, move |event| {
                seen.lock().unwrap().push(event.payload().to_string());
            });
        }
        platform::observe(app.handle()).expect("observer installs");

        // SAFETY: plain getters and `postNotificationName:object:`, which
        // takes an NSString name and a nullable object.
        unsafe {
            let class = AnyClass::get(c"NSWorkspace").expect("NSWorkspace");
            let workspace: Retained<AnyObject> = msg_send![class, sharedWorkspace];
            let center: Retained<AnyObject> = msg_send![&*workspace, notificationCenter];
            let name = objc2_foundation::NSString::from_str(
                "NSWorkspaceAccessibilityDisplayOptionsDidChangeNotification",
            );
            let () = msg_send![
                &*center,
                postNotificationName: &*name,
                object: Option::<&AnyObject>::None
            ];
        }

        let deadline = Instant::now() + Duration::from_secs(5);
        while seen.lock().unwrap().is_empty() && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(10));
        }
        let seen = seen.lock().unwrap();
        assert_eq!(seen.len(), 1, "one change, one event: {seen:?}");
        let payload: serde_json::Value = serde_json::from_str(&seen[0]).expect("JSON payload");
        assert!(
            matches!(
                payload["reduceTransparency"].as_str(),
                Some("reduced" | "standard")
            ),
            "the re-read must run: {payload}"
        );
        assert!(payload["seq"].as_u64().is_some_and(|seq| seq > 0));
    }
}
