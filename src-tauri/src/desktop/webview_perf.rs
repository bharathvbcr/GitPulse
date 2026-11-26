//! High-refresh-rate (120 Hz ProMotion) unlock for macOS `WKWebView`.
//!
//! By default, WebKit on macOS throttles `requestAnimationFrame`, scrolling, and
//! page rendering updates in `WKWebView` near 60 FPS (`preferPageRenderingUpdatesNear60FPSEnabled = YES`),
//! even on 120 Hz ProMotion displays. Toggling that `WKPreferences` KVC key to
//! `false` lets xterm's WebGL2 renderer and frame-budgeted write queue pace at
//! the display's native 120 Hz refresh rate (`8.33 ms` vs `16.67 ms`).
//!
//! Because `preferPageRenderingUpdatesNear60FPSEnabled` is a KVC preference key
//! rather than a public typed setter on `WKPreferences`, an OS release that
//! renames or removes it raises `NSUndefinedKeyException` from `setValue:forKey:`.
//! Uncaught across a Rust FFI frame, an Objective-C exception aborts the process.
//! Every KVC lookup and write here is therefore wrapped in `objc2::exception::catch`.

pub const PREFER_60FPS_KVC_KEY: &str = "preferPageRenderingUpdatesNear60FPSEnabled";

#[cfg(target_os = "macos")]
mod imp {
    use super::PREFER_60FPS_KVC_KEY;
    use objc2::exception::catch;
    use objc2::msg_send;
    use objc2::runtime::AnyObject;
    use objc2_foundation::{NSNumber, NSString};
    use std::ffi::c_void;
    use std::panic::AssertUnwindSafe;

    /// Sets a boolean KVC key on `target`, catching Objective-C exceptions
    /// (such as `NSUndefinedKeyException`) instead of aborting the process.
    ///
    /// # Safety
    /// `target` must be a valid Objective-C object reference.
    pub unsafe fn set_kvc_bool_caught(
        target: &AnyObject,
        key: &str,
        value: bool,
    ) -> Result<(), String> {
        let ns_key = NSString::from_str(key);
        let ns_val = NSNumber::numberWithBool(value);
        catch(AssertUnwindSafe(|| {
            let _: () = msg_send![target, setValue: &*ns_val, forKey: &*ns_key];
        }))
        .map_err(|exc| match exc {
            Some(exception) => format!("Objective-C exception setting {key}: {exception:?}"),
            None => format!("Unknown Objective-C exception setting {key}"),
        })
    }

    /// Sets WebKit's 60 FPS preference key on a raw `WKWebView` pointer.
    ///
    /// # Safety
    /// When non-null, `webview_ptr` must point to a live `WKWebView` on the AppKit main thread.
    pub unsafe fn set_wkwebview_prefer_60fps(
        webview_ptr: *mut c_void,
        prefer_60: bool,
    ) -> Result<(), String> {
        if webview_ptr.is_null() {
            return Err("WKWebView pointer is null".to_string());
        }
        let webview = &*(webview_ptr as *const AnyObject);
        let prefs_ptr: Result<*mut AnyObject, String> = catch(AssertUnwindSafe(|| {
            let config: *mut AnyObject = msg_send![webview, configuration];
            if config.is_null() {
                return std::ptr::null_mut();
            }
            let prefs: *mut AnyObject = msg_send![&*config, preferences];
            prefs
        }))
        .map_err(|exc| match exc {
            Some(exception) => {
                format!("Objective-C exception reading WKWebView preferences: {exception:?}")
            }
            None => "Unknown Objective-C exception reading WKWebView preferences".to_string(),
        });
        let prefs = prefs_ptr?;
        if prefs.is_null() {
            return Err("WKWebView configuration or preferences is null".to_string());
        }
        set_kvc_bool_caught(&*prefs, PREFER_60FPS_KVC_KEY, prefer_60)
    }

    /// Disables WebKit's 60 FPS cap on a raw `WKWebView` pointer.
    ///
    /// # Safety
    /// When non-null, `webview_ptr` must point to a live `WKWebView` on the AppKit main thread.
    pub unsafe fn unlock_wkwebview_120hz(webview_ptr: *mut c_void) -> Result<(), String> {
        set_wkwebview_prefer_60fps(webview_ptr, false)
    }

    /// Applies or clears the native WebView idle memory/frame-pacing hint on macOS.
    ///
    /// When `low_memory` is `true`, re-enables WebKit's 60 FPS cap so idle or
    /// occluded windows stop pacing at 120 Hz; when `false`, restores 120 Hz.
    pub fn set_webview_idle_hint<R: tauri::Runtime>(
        window: &tauri::WebviewWindow<R>,
        low_memory: bool,
    ) {
        if let Err(err) = window.with_webview(move |webview| {
            #[cfg(any(target_os = "macos", target_os = "ios"))]
            let ptr = webview.inner();
            // SAFETY: `with_webview` runs on the main thread with a live `WKWebView` handle.
            if let Err(reason) = unsafe { set_wkwebview_prefer_60fps(ptr, low_memory) } {
                log::debug!(
                    target: "desktop",
                    "WebView idle hint ({low_memory}) was not applied: {reason}"
                );
            }
        }) {
            log::debug!(
                target: "desktop",
                "Could not access native webview for idle hint: {err}"
            );
        }
    }

    pub fn unlock_window_high_refresh_rate<R: tauri::Runtime>(window: &tauri::WebviewWindow<R>) {
        set_webview_idle_hint(window, false);
    }
}

#[cfg(target_os = "macos")]
pub use imp::{
    set_kvc_bool_caught, set_webview_idle_hint, set_wkwebview_prefer_60fps,
    unlock_window_high_refresh_rate, unlock_wkwebview_120hz,
};

#[cfg(not(target_os = "macos"))]
/// Unlocks high-refresh-rate rendering on platforms that support it (no-op outside macOS).
pub fn unlock_window_high_refresh_rate<R: tauri::Runtime>(_window: &tauri::WebviewWindow<R>) {}

#[cfg(not(target_os = "macos"))]
/// Applies or clears the native WebView low-memory hint.
///
/// Outside macOS, `Cargo.toml` does not link `wry` or `webview2-com` directly
/// into the app crate and `tauri::webview::PlatformWebview` does not wrap
/// `MemoryUsageLevel` / `WebKitMemoryPressureSettings`, so this remains a safe
/// no-op until a platform binding is exposed.
pub fn set_webview_idle_hint<R: tauri::Runtime>(
    _window: &tauri::WebviewWindow<R>,
    _low_memory: bool,
) {
}

#[cfg(test)]
mod tests {
    // Only the macOS test below reaches into the parent module.
    #[cfg(target_os = "macos")]
    use super::*;

    #[cfg(target_os = "macos")]
    #[test]
    fn null_webview_pointer_is_refused_without_crashing() {
        let err = unsafe { unlock_wkwebview_120hz(std::ptr::null_mut()) }
            .expect_err("null pointer must fail cleanly");
        assert!(err.contains("null"), "unexpected error: {err}");
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn unknown_kvc_key_on_nsobject_is_caught_instead_of_aborting() {
        use objc2_foundation::NSObject;
        let obj = NSObject::new();
        let err = unsafe { set_kvc_bool_caught(&obj, PREFER_60FPS_KVC_KEY, false) }
            .expect_err("NSObject does not implement preferPageRenderingUpdatesNear60FPSEnabled");
        assert!(
            err.contains("NSUndefinedKeyException") || err.contains(PREFER_60FPS_KVC_KEY),
            "expected caught KVC exception, got: {err}"
        );
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn kvc_bool_writes_false_and_true_to_kvc_compliant_container() {
        use objc2::rc::Retained;
        use objc2::runtime::AnyObject;
        use objc2::{class, msg_send};
        use objc2_foundation::{NSNumber, NSString};

        let dict: Retained<AnyObject> = unsafe { msg_send![class!(NSMutableDictionary), new] };
        unsafe { set_kvc_bool_caught(&dict, PREFER_60FPS_KVC_KEY, true) }
            .expect("NSMutableDictionary accepts KVC bool true writes");
        let key = NSString::from_str(PREFER_60FPS_KVC_KEY);
        let stored_true: *const NSNumber = unsafe { msg_send![&*dict, valueForKey: &*key] };
        assert!(!stored_true.is_null(), "KVC true value should be stored");
        assert!(unsafe { (&*stored_true).as_bool() }, "expected true");

        unsafe { set_kvc_bool_caught(&dict, PREFER_60FPS_KVC_KEY, false) }
            .expect("NSMutableDictionary accepts KVC bool false writes");
        let stored_false: *const NSNumber = unsafe { msg_send![&*dict, valueForKey: &*key] };
        assert!(!stored_false.is_null(), "KVC false value should be stored");
        assert!(!unsafe { (&*stored_false).as_bool() }, "expected false");

        let err = unsafe { set_wkwebview_prefer_60fps(std::ptr::null_mut(), true) }
            .expect_err("null pointer must fail cleanly for idle hint");
        assert!(err.contains("null"), "unexpected error: {err}");
    }
}
