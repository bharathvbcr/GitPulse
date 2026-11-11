//! Two opt-in macOS rendering switches: native Liquid Glass for the window, and
//! WebKit's 60 Hz frame-rate cap on the main webview.
//!
//! Both are off by default and both fail closed. "Fail closed" here means the
//! window keeps today's `underWindowBackground` material and WebKit keeps its
//! default pacing, and the answer says why instead of looking applied.
//!
//! **Liquid Glass.** `NSGlassEffectView` (macOS 26+) is applied through
//! Tauri's window-effect wrapper, which calls `window_vibrancy::apply_liquid_glass`.
//! The wrapper swallows the result (`let _ =` around a main-thread task), so
//! this module does not trust it: the OS gate runs first, with the same number
//! the library gates on, and afterwards the window's content view is asked
//! whether the glass view is really there. Only that observation reports
//! `active`.
//!
//! **Frame rate.** WKWebView's `PreferPageRenderingUpdatesNear60FPSEnabled`
//! feature is on by default and holds `requestAnimationFrame` to 60 Hz on a
//! 120 Hz display. Turning it off is a private WebKit SPI
//! (`+[WKPreferences _features]`, `-[WKPreferences _setEnabled:forFeature:]`).
//! Nothing here is hard-coded against a guess: the class, both selectors and
//! the feature's `key` are looked up at runtime, the feature object is found by
//! iterating the list WebKit returns (never constructed), and the value is read
//! back after it is written. A missing piece leaves WebKit at its default.
//!
//! The decisions are pure functions so they are tested on every host; only the
//! Objective-C calls that feed them are macOS-only.

use serde::Serialize;

/// Whether this binary can apply Liquid Glass at all. The *OS* version is a
/// runtime fact and is reported by [`cmd_native_rendering_apply`]; see
/// [`crate::host_platform`], which stays compile-time only.
pub const LIQUID_GLASS: bool = cfg!(target_os = "macos");

/// Whether this binary can change the webview's frame pacing (WKWebView only).
pub const HIGH_REFRESH: bool = cfg!(target_os = "macos");

/// `NSAppKitVersionNumber` of macOS 26.0, the number `window-vibrancy` 0.8.1
/// refuses to go below in `apply_liquid_glass` (`MIN_APPKIT_VERSION_LIQUID_GLASS`).
/// Gating here with the same value means a refusal is reported before the
/// wrapper clears the window's existing material, not silently after.
pub const APPKIT_VERSION_LIQUID_GLASS: f64 = 2685.0;

/// The WebKit feature whose default holds rendering updates near 60 Hz.
pub const FRAME_RATE_FEATURE_KEY: &str = "PreferPageRenderingUpdatesNear60FPSEnabled";

/// What one switch ended up as. `active` is an observation, never a request
/// echoed back; `reason` is present whenever the reader asked for something
/// that is not in effect.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct FlagStatus {
    pub requested: bool,
    pub active: bool,
    pub reason: Option<String>,
}

impl FlagStatus {
    fn off() -> Self {
        Self {
            requested: false,
            active: false,
            reason: None,
        }
    }
    fn on() -> Self {
        Self {
            requested: true,
            active: true,
            reason: None,
        }
    }
    fn refused(reason: impl Into<String>) -> Self {
        Self {
            requested: true,
            active: false,
            reason: Some(reason.into()),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct NativeRenderingStatus {
    pub liquid_glass: FlagStatus,
    pub high_refresh: FlagStatus,
}

// --- Liquid Glass decisions --------------------------------------------------

/// What to do about the window material.
#[derive(Debug, Clone, PartialEq)]
pub enum GlassPlan {
    /// Not requested: the window should be on today's vibrancy.
    Vibrancy,
    /// Requested and possible: apply the glass view.
    ApplyGlass,
    /// Requested and impossible: stay on vibrancy, for this reason.
    Refuse(String),
}

/// `appkit_version` is `None` where there is no AppKit to ask.
pub fn glass_plan(requested: bool, compiled_in: bool, appkit_version: Option<f64>) -> GlassPlan {
    if !requested {
        return GlassPlan::Vibrancy;
    }
    if !compiled_in {
        return GlassPlan::Refuse("Liquid Glass is only available on macOS.".to_string());
    }
    match appkit_version {
        None => GlassPlan::Refuse(
            "GitPulse couldn't read this Mac's AppKit version, so it didn't try Liquid Glass.".to_string(),
        ),
        Some(version) if !version.is_finite() || version < APPKIT_VERSION_LIQUID_GLASS => GlassPlan::Refuse(
            "Liquid Glass needs macOS 26 or later. This Mac is older, so the window keeps its standard material."
                .to_string(),
        ),
        Some(_) => GlassPlan::ApplyGlass,
    }
}

/// Turns what the window actually has afterwards into a status.
pub fn glass_outcome(plan: &GlassPlan, glass_view_present: Option<bool>) -> FlagStatus {
    match plan {
        GlassPlan::Vibrancy => match glass_view_present {
            Some(true) => FlagStatus::refused("The glass view is still on the window after it was cleared."),
            _ => FlagStatus::off(),
        },
        GlassPlan::Refuse(reason) => FlagStatus::refused(reason.clone()),
        GlassPlan::ApplyGlass => match glass_view_present {
            Some(true) => FlagStatus::on(),
            Some(false) => FlagStatus::refused(
                "AppKit didn't add the glass view, so the window keeps its standard material.",
            ),
            None => FlagStatus::refused(
                "GitPulse couldn't check the window for the glass view, so it can't say Liquid Glass is on.",
            ),
        },
    }
}

// --- Frame-rate decisions ----------------------------------------------------

/// What the runtime lookup found. Each field is one thing that has to exist
/// before a write is attempted.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FeatureProbe {
    pub features_listed: bool,
    pub setter: bool,
    pub getter: bool,
    /// The feature with [`FRAME_RATE_FEATURE_KEY`] was in the list.
    pub feature_found: bool,
}

impl FeatureProbe {
    /// Why the flag cannot be written, or `None` when every piece exists.
    pub fn missing(&self) -> Option<&'static str> {
        if !self.features_listed {
            Some("this WebKit no longer lists its preference features")
        } else if !self.setter || !self.getter {
            Some("this WebKit no longer has the feature setter")
        } else if !self.feature_found {
            Some("this WebKit has no such frame-rate feature")
        } else {
            None
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RefreshPlan {
    /// The feature already has the value the request implies.
    AlreadyThere,
    /// Write this value for the "prefer 60 Hz" feature.
    Write(bool),
    /// Cannot be done; the string is the reason, or `None` when nothing was
    /// asked for and so nothing needs saying.
    Unavailable(Option<String>),
}

/// `requested` means "let the page render above 60 Hz", which is the feature
/// turned *off*; not requested means the feature at WebKit's own default.
pub fn refresh_plan(
    requested: bool,
    compiled_in: bool,
    probe: Option<FeatureProbe>,
    current_prefer_60: Option<bool>,
    default_prefer_60: bool,
) -> RefreshPlan {
    let unavailable = |why: String| RefreshPlan::Unavailable(requested.then_some(why));
    if !compiled_in {
        return unavailable("The 120 Hz setting is only available on macOS.".to_string());
    }
    let Some(probe) = probe else {
        return unavailable(
            "GitPulse couldn't reach the main webview, so frame pacing is unchanged.".to_string(),
        );
    };
    if let Some(missing) = probe.missing() {
        return unavailable(format!("Frame pacing is unchanged: {missing}."));
    }
    let Some(current) = current_prefer_60 else {
        return unavailable(
            "Frame pacing is unchanged: WebKit wouldn't report the feature's current value."
                .to_string(),
        );
    };
    let target = if requested { false } else { default_prefer_60 };
    if current == target {
        RefreshPlan::AlreadyThere
    } else {
        RefreshPlan::Write(target)
    }
}

/// The status once the write has been read back.
pub fn refresh_outcome(
    requested: bool,
    plan: &RefreshPlan,
    readback_prefer_60: Option<bool>,
) -> FlagStatus {
    match plan {
        RefreshPlan::Unavailable(Some(reason)) => FlagStatus::refused(reason.clone()),
        RefreshPlan::Unavailable(None) => FlagStatus::off(),
        RefreshPlan::AlreadyThere | RefreshPlan::Write(_) => {
            let target = match plan {
                RefreshPlan::Write(target) => Some(*target),
                _ => None,
            };
            match (target, readback_prefer_60) {
                // A write whose value WebKit did not keep is a failure, whatever
                // the setter returned.
                (Some(target), Some(read)) if read != target => FlagStatus {
                    requested,
                    active: false,
                    reason: Some(
                        "WebKit didn't keep the new value, so frame pacing is unchanged."
                            .to_string(),
                    ),
                },
                (Some(_), None) => FlagStatus {
                    requested,
                    active: false,
                    reason: Some(
                        "Frame pacing may have changed but couldn't be confirmed.".to_string(),
                    ),
                },
                _ if requested => FlagStatus::on(),
                _ => FlagStatus::off(),
            }
        }
    }
}

/// The status when the main-thread work could not be run or finished.
pub fn failed(liquid_glass: bool, high_refresh: bool, error: &str) -> NativeRenderingStatus {
    let reason = format!("GitPulse couldn't reach the window to apply this: {error}");
    let one = |requested: bool| {
        if requested {
            FlagStatus::refused(reason.clone())
        } else {
            FlagStatus::off()
        }
    };
    NativeRenderingStatus {
        liquid_glass: one(liquid_glass),
        high_refresh: one(high_refresh),
    }
}

// --- The Objective-C side ----------------------------------------------------

#[cfg(target_os = "macos")]
mod apple {
    use super::{FeatureProbe, FRAME_RATE_FEATURE_KEY};
    use objc2::runtime::{AnyClass, AnyObject, Bool};
    use objc2::{msg_send, sel};
    use objc2_foundation::NSString;
    use std::ffi::c_void;

    /// `NSView.tag` window-vibrancy gives its glass view (`NS_VIEW_TAG_GLASS_VIEW`).
    const GLASS_VIEW_TAG: isize = 96_945_937;

    #[link(name = "AppKit", kind = "framework")]
    extern "C" {
        static NSAppKitVersionNumber: f64;
    }

    pub fn appkit_version() -> f64 {
        // SAFETY: a process-wide constant AppKit exports; reading it has no
        // preconditions.
        unsafe { NSAppKitVersionNumber }
    }

    /// Whether the window's content view carries the glass view. `None` when the
    /// window or its content view cannot be reached.
    ///
    /// # Safety
    /// `ns_window` must be a live `NSWindow` and this must run on the main thread.
    pub unsafe fn glass_view_present(ns_window: *mut c_void) -> Option<bool> {
        let window = ns_window.cast::<AnyObject>();
        if window.is_null() {
            return None;
        }
        // SAFETY: caller guarantees a live NSWindow on the main thread;
        // `contentView` and `viewWithTag:` are public AppKit API.
        let content: *mut AnyObject = unsafe { msg_send![window, contentView] };
        if content.is_null() {
            return None;
        }
        let found: *mut AnyObject = unsafe { msg_send![content, viewWithTag: GLASS_VIEW_TAG] };
        Some(!found.is_null())
    }

    fn responds(object: *mut AnyObject, selector: objc2::runtime::Sel) -> bool {
        // SAFETY: `object` is a live Objective-C object or class and
        // `respondsToSelector:` is NSObject protocol API.
        let answer: Bool = unsafe { msg_send![object, respondsToSelector: selector] };
        answer.as_bool()
    }

    /// Everything the frame-rate switch needs, looked up and held together.
    pub struct Preferences {
        prefs: *mut AnyObject,
        feature: *mut AnyObject,
        pub probe: FeatureProbe,
    }

    /// Resolves the live `WKPreferences` and the frame-rate feature from a
    /// `WKWebView`.
    ///
    /// `WKWebView.configuration` hands back a copy of the configuration, but its
    /// `preferences` are the live ones: setting the feature on them reaches the
    /// running page (measured in a plain WKWebView before this was written:
    /// 60.0 Hz default, 119.0 Hz after the write).
    ///
    /// # Safety
    /// `webview` must be a live `WKWebView` and this must run on the main thread.
    pub unsafe fn resolve(webview: *mut c_void) -> Option<Preferences> {
        let web = webview.cast::<AnyObject>();
        if web.is_null() {
            return None;
        }
        // SAFETY: caller guarantees a live WKWebView on the main thread;
        // `configuration` and `preferences` are public WebKit API.
        let config: *mut AnyObject = unsafe { msg_send![web, configuration] };
        if config.is_null() {
            return None;
        }
        let prefs: *mut AnyObject = unsafe { msg_send![config, preferences] };
        if prefs.is_null() {
            return None;
        }

        let class: Option<&'static AnyClass> = AnyClass::get(c"WKPreferences");
        let listed: *mut AnyObject = match class {
            Some(class) => {
                let class_object = std::ptr::from_ref(class).cast_mut().cast::<AnyObject>();
                if responds(class_object, sel!(_features)) {
                    // SAFETY: the class answered `_features` just above.
                    unsafe { msg_send![class_object, _features] }
                } else {
                    std::ptr::null_mut()
                }
            }
            None => std::ptr::null_mut(),
        };
        let setter = responds(prefs, sel!(_setEnabled:forFeature:));
        let getter = responds(prefs, sel!(_isEnabledForFeature:));

        let mut feature: *mut AnyObject = std::ptr::null_mut();
        if !listed.is_null() {
            let wanted = NSString::from_str(FRAME_RATE_FEATURE_KEY);
            // SAFETY: `listed` is the NSArray WebKit returned.
            let count: usize = unsafe { msg_send![listed, count] };
            for index in 0..count {
                let item: *mut AnyObject = unsafe { msg_send![listed, objectAtIndex: index] };
                if item.is_null() || !responds(item, sel!(key)) {
                    continue;
                }
                let key: *mut AnyObject = unsafe { msg_send![item, key] };
                if key.is_null() {
                    continue;
                }
                let same: Bool = unsafe { msg_send![key, isEqualToString: &*wanted] };
                if same.as_bool() {
                    feature = item;
                    break;
                }
            }
        }
        Some(Preferences {
            prefs,
            feature,
            probe: FeatureProbe {
                features_listed: !listed.is_null(),
                setter,
                getter,
                feature_found: !feature.is_null(),
            },
        })
    }

    impl Preferences {
        /// Current "prefer 60 Hz" value, or `None` when it cannot be read.
        ///
        /// # Safety
        /// Same contract as [`resolve`]; only valid while `probe.missing()` is `None`.
        pub unsafe fn current(&self) -> Option<bool> {
            if self.probe.missing().is_some() {
                return None;
            }
            // SAFETY: both selectors were confirmed present by the probe.
            let value: Bool = unsafe { msg_send![self.prefs, _isEnabledForFeature: self.feature] };
            Some(value.as_bool())
        }

        /// WebKit's own default for the feature, `true` when it will not say.
        ///
        /// # Safety
        /// Same contract as [`resolve`].
        pub unsafe fn default_value(&self) -> bool {
            if self.feature.is_null() || !responds(self.feature, sel!(defaultValue)) {
                return true;
            }
            // SAFETY: the feature answered `defaultValue` just above.
            let value: Bool = unsafe { msg_send![self.feature, defaultValue] };
            value.as_bool()
        }

        /// # Safety
        /// Same contract as [`resolve`]; only valid while `probe.missing()` is `None`.
        pub unsafe fn write(&self, prefer_60: bool) {
            // SAFETY: both selectors were confirmed present by the probe.
            let () = unsafe {
                msg_send![self.prefs, _setEnabled: Bool::new(prefer_60), forFeature: self.feature]
            };
        }
    }
}

/// Applies both switches on the main thread. Never panics on a missing piece:
/// every absence becomes a refusal with its reason.
#[cfg(target_os = "macos")]
fn apply_on_main(
    window: &tauri::WebviewWindow,
    liquid_glass: bool,
    high_refresh: bool,
) -> NativeRenderingStatus {
    NativeRenderingStatus {
        liquid_glass: apply_glass(window, liquid_glass),
        high_refresh: apply_refresh(window, high_refresh),
    }
}

#[cfg(target_os = "macos")]
fn ns_window_pointer(window: &tauri::WebviewWindow) -> Option<*mut std::ffi::c_void> {
    window.ns_window().ok()
}

#[cfg(target_os = "macos")]
fn present(window: &tauri::WebviewWindow) -> Option<bool> {
    let pointer = ns_window_pointer(window)?;
    // SAFETY: `ns_window` is the live window and this runs on the main thread
    // (`desktop::on_main_thread`).
    unsafe { apple::glass_view_present(pointer) }
}

/// Today's window material: what `tauri.macos.conf.json` configures.
#[cfg(target_os = "macos")]
fn material_effects(with_glass: bool) -> tauri::utils::config::WindowEffectsConfig {
    use tauri::window::{Effect, EffectState, EffectsBuilder};
    let builder = EffectsBuilder::new();
    let builder = if with_glass {
        builder.effect(Effect::LiquidGlassRegular)
    } else {
        builder
    };
    builder
        .effect(Effect::UnderWindowBackground)
        .state(EffectState::FollowsWindowActiveState)
        .radius(WINDOW_MATERIAL_RADIUS)
        .build()
}

/// `windowEffects.radius` in `tauri.macos.conf.json`.
#[cfg(target_os = "macos")]
const WINDOW_MATERIAL_RADIUS: f64 = 12.0;

#[cfg(target_os = "macos")]
fn apply_glass(window: &tauri::WebviewWindow, requested: bool) -> FlagStatus {
    let plan = glass_plan(requested, LIQUID_GLASS, Some(apple::appkit_version()));
    let before = present(window);
    match &plan {
        GlassPlan::Refuse(_) => {}
        // Already in the state asked for: no churn, and no second glass view
        // (the library inserts a new subview on every call).
        GlassPlan::ApplyGlass if before == Some(true) => {}
        GlassPlan::Vibrancy if before != Some(true) => {}
        GlassPlan::ApplyGlass => {
            if let Err(error) = window.set_effects(material_effects(true)) {
                log::warn!(target: "desktop", "Liquid Glass was not applied: {error}");
            }
        }
        GlassPlan::Vibrancy => {
            if let Err(error) = window.set_effects(material_effects(false)) {
                log::warn!(target: "desktop", "the standard window material was not restored: {error}");
            }
        }
    }
    let after = present(window);
    let status = glass_outcome(&plan, after);
    // A refused or unconfirmed glass request must not leave the window without
    // its usual material: `set_effects` clears the old one before applying.
    if requested && !status.active && after != Some(true) {
        if let Err(error) = window.set_effects(material_effects(false)) {
            log::warn!(target: "desktop", "the standard window material was not restored: {error}");
        }
    }
    status
}

#[cfg(target_os = "macos")]
fn apply_refresh(window: &tauri::WebviewWindow, requested: bool) -> FlagStatus {
    use std::sync::{Arc, Mutex};

    let slot: Arc<Mutex<Option<FlagStatus>>> = Arc::new(Mutex::new(None));
    let write_slot = Arc::clone(&slot);
    let scheduled = window.with_webview(move |platform| {
        // SAFETY: `with_webview` runs this on the main thread with the live
        // WKWebView.
        let status = unsafe {
            match apple::resolve(platform.inner()) {
                None => {
                    let plan = refresh_plan(requested, HIGH_REFRESH, None, None, true);
                    refresh_outcome(requested, &plan, None)
                }
                Some(preferences) => {
                    let current = preferences.current();
                    let default = preferences.default_value();
                    let plan = refresh_plan(
                        requested,
                        HIGH_REFRESH,
                        Some(preferences.probe),
                        current,
                        default,
                    );
                    if let RefreshPlan::Write(target) = plan {
                        preferences.write(target);
                    }
                    let readback = match plan {
                        RefreshPlan::Write(_) => preferences.current(),
                        _ => None,
                    };
                    refresh_outcome(requested, &plan, readback)
                }
            }
        };
        if let Ok(mut guard) = write_slot.lock() {
            *guard = Some(status);
        }
    });
    if let Err(error) = scheduled {
        return refresh_failure(requested, &error.to_string());
    }
    // `desktop::on_main_thread` already put this call on the main thread, where
    // `with_webview` runs the closure inline; an empty slot means it did not.
    let taken = slot.lock().ok().and_then(|mut guard| guard.take());
    taken.unwrap_or_else(|| refresh_failure(requested, "the webview did not answer"))
}

#[cfg(target_os = "macos")]
fn refresh_failure(requested: bool, error: &str) -> FlagStatus {
    failed(false, requested, error).high_refresh
}

/// Applies the two switches and reports what is actually in effect.
///
/// Idempotent: the frontend sends the stored preferences on boot and on every
/// change, so asking for what is already true changes nothing.
#[tauri::command]
pub async fn cmd_native_rendering_apply(
    window: tauri::WebviewWindow,
    liquid_glass: bool,
    high_refresh: bool,
) -> NativeRenderingStatus {
    #[cfg(target_os = "macos")]
    {
        use tauri::Manager;
        let app = window.app_handle().clone();
        match crate::desktop::on_main_thread(app, move |_| {
            Ok(apply_on_main(&window, liquid_glass, high_refresh))
        })
        .await
        {
            Ok(status) => status,
            Err(error) => failed(liquid_glass, high_refresh, &error),
        }
    }
    #[cfg(not(target_os = "macos"))]
    {
        let _ = window;
        NativeRenderingStatus {
            liquid_glass: glass_outcome(&glass_plan(liquid_glass, LIQUID_GLASS, None), None),
            high_refresh: refresh_outcome(
                high_refresh,
                &refresh_plan(high_refresh, HIGH_REFRESH, None, None, true),
                None,
            ),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const MACOS_26: f64 = 2685.0;
    const MACOS_15: f64 = 2575.0;

    fn full_probe() -> FeatureProbe {
        FeatureProbe {
            features_listed: true,
            setter: true,
            getter: true,
            feature_found: true,
        }
    }

    // --- Liquid Glass ---

    #[test]
    fn glass_is_not_attempted_unless_asked_for() {
        assert_eq!(glass_plan(false, true, Some(MACOS_26)), GlassPlan::Vibrancy);
        assert_eq!(glass_plan(false, false, None), GlassPlan::Vibrancy);
    }

    #[test]
    fn glass_applies_from_macos_26_up() {
        assert_eq!(
            glass_plan(true, true, Some(MACOS_26)),
            GlassPlan::ApplyGlass
        );
        assert_eq!(glass_plan(true, true, Some(3000.0)), GlassPlan::ApplyGlass);
    }

    #[test]
    fn glass_below_macos_26_is_refused_with_the_reason() {
        let GlassPlan::Refuse(reason) = glass_plan(true, true, Some(MACOS_15)) else {
            panic!("an older macOS must refuse");
        };
        assert!(reason.contains("macOS 26"), "{reason}");
        // One tick under the threshold is still older.
        assert!(matches!(
            glass_plan(true, true, Some(2684.99)),
            GlassPlan::Refuse(_)
        ));
    }

    #[test]
    fn glass_off_macos_or_without_a_version_is_refused() {
        assert!(matches!(
            glass_plan(true, false, None),
            GlassPlan::Refuse(_)
        ));
        assert!(matches!(glass_plan(true, true, None), GlassPlan::Refuse(_)));
        assert!(matches!(
            glass_plan(true, true, Some(f64::NAN)),
            GlassPlan::Refuse(_)
        ));
    }

    #[test]
    fn glass_reports_active_only_when_the_view_was_seen() {
        let plan = GlassPlan::ApplyGlass;
        assert_eq!(glass_outcome(&plan, Some(true)), FlagStatus::on());
        let missing = glass_outcome(&plan, Some(false));
        assert!(!missing.active && missing.requested && missing.reason.is_some());
        // A check that could not run must not read like one that passed.
        let unchecked = glass_outcome(&plan, None);
        assert!(!unchecked.active && unchecked.reason.is_some());
    }

    #[test]
    fn a_refusal_keeps_its_reason_and_is_never_active() {
        let status = glass_outcome(&GlassPlan::Refuse("older".into()), Some(true));
        assert_eq!(status, FlagStatus::refused("older"));
    }

    #[test]
    fn clearing_glass_that_stays_is_a_failure_not_a_quiet_off() {
        assert_eq!(
            glass_outcome(&GlassPlan::Vibrancy, Some(false)),
            FlagStatus::off()
        );
        assert_eq!(glass_outcome(&GlassPlan::Vibrancy, None), FlagStatus::off());
        assert!(glass_outcome(&GlassPlan::Vibrancy, Some(true))
            .reason
            .is_some());
    }

    #[test]
    fn the_threshold_is_the_one_window_vibrancy_gates_on() {
        let source = std::fs::read_to_string(concat!(env!("CARGO_MANIFEST_DIR"), "/Cargo.lock"))
            .expect("Cargo.lock");
        // Pin the library version the threshold was read from. A bump has to
        // re-read `MIN_APPKIT_VERSION_LIQUID_GLASS` before this number is
        // trusted again.
        assert!(
            source.contains("name = \"window-vibrancy\"\nversion = \"0.8.1\""),
            "window-vibrancy changed: re-check APPKIT_VERSION_LIQUID_GLASS against its liquid_glass.rs"
        );
    }

    // --- Frame rate ---

    #[test]
    fn every_missing_piece_names_itself_and_blocks_the_write() {
        let cases = [
            (
                FeatureProbe {
                    features_listed: false,
                    ..full_probe()
                },
                "lists",
            ),
            (
                FeatureProbe {
                    setter: false,
                    ..full_probe()
                },
                "setter",
            ),
            (
                FeatureProbe {
                    getter: false,
                    ..full_probe()
                },
                "setter",
            ),
            (
                FeatureProbe {
                    feature_found: false,
                    ..full_probe()
                },
                "no such",
            ),
        ];
        for (probe, needle) in cases {
            let plan = refresh_plan(true, true, Some(probe), Some(true), true);
            let RefreshPlan::Unavailable(Some(reason)) = plan else {
                panic!("a missing piece must be unavailable: {probe:?}");
            };
            assert!(reason.contains(needle), "{reason}");
            assert!(
                refresh_outcome(true, &RefreshPlan::Unavailable(Some(reason)), None)
                    .reason
                    .is_some()
            );
        }
    }

    #[test]
    fn a_missing_piece_says_nothing_when_nothing_was_asked() {
        let probe = FeatureProbe {
            feature_found: false,
            ..full_probe()
        };
        let plan = refresh_plan(false, true, Some(probe), Some(true), true);
        assert_eq!(plan, RefreshPlan::Unavailable(None));
        assert_eq!(refresh_outcome(false, &plan, None), FlagStatus::off());
    }

    #[test]
    fn no_webview_or_no_current_value_fails_closed() {
        assert!(matches!(
            refresh_plan(true, true, None, None, true),
            RefreshPlan::Unavailable(Some(_))
        ));
        assert!(matches!(
            refresh_plan(true, true, Some(full_probe()), None, true),
            RefreshPlan::Unavailable(Some(_))
        ));
        assert!(matches!(
            refresh_plan(true, false, Some(full_probe()), Some(true), true),
            RefreshPlan::Unavailable(Some(_))
        ));
    }

    #[test]
    fn asking_for_high_refresh_turns_the_prefer_60_feature_off() {
        assert_eq!(
            refresh_plan(true, true, Some(full_probe()), Some(true), true),
            RefreshPlan::Write(false)
        );
        assert_eq!(
            refresh_plan(true, true, Some(full_probe()), Some(false), true),
            RefreshPlan::AlreadyThere
        );
    }

    #[test]
    fn switching_it_off_restores_webkits_own_default() {
        assert_eq!(
            refresh_plan(false, true, Some(full_probe()), Some(false), true),
            RefreshPlan::Write(true)
        );
        assert_eq!(
            refresh_plan(false, true, Some(full_probe()), Some(true), true),
            RefreshPlan::AlreadyThere
        );
        // A WebKit whose default were off would already be at "not requested".
        assert_eq!(
            refresh_plan(false, true, Some(full_probe()), Some(false), false),
            RefreshPlan::AlreadyThere
        );
    }

    #[test]
    fn a_write_is_active_only_when_it_reads_back() {
        let write = RefreshPlan::Write(false);
        assert_eq!(refresh_outcome(true, &write, Some(false)), FlagStatus::on());
        // The setter ran but WebKit kept the old value.
        let kept = refresh_outcome(true, &write, Some(true));
        assert!(!kept.active && kept.reason.is_some());
        // The read-back itself could not run.
        let unknown = refresh_outcome(true, &write, None);
        assert!(!unknown.active && unknown.reason.is_some());
    }

    #[test]
    fn restoring_the_default_reports_off_and_a_failed_restore_says_so() {
        let write = RefreshPlan::Write(true);
        assert_eq!(
            refresh_outcome(false, &write, Some(true)),
            FlagStatus::off()
        );
        let stuck = refresh_outcome(false, &write, Some(false));
        assert!(!stuck.active && stuck.reason.is_some());
    }

    #[test]
    fn a_main_thread_failure_reports_each_requested_switch_and_nothing_else() {
        let status = failed(true, false, "timed out");
        assert!(status
            .liquid_glass
            .reason
            .as_deref()
            .unwrap()
            .contains("timed out"));
        assert_eq!(status.high_refresh, FlagStatus::off());
    }

    #[test]
    fn the_wire_shape_the_frontend_parses() {
        let json = serde_json::to_value(NativeRenderingStatus {
            liquid_glass: FlagStatus::on(),
            high_refresh: FlagStatus::refused("why"),
        })
        .unwrap();
        assert_eq!(json["liquid_glass"]["active"], true);
        assert_eq!(json["high_refresh"]["reason"], "why");
        assert_eq!(json["high_refresh"]["requested"], true);
        assert_eq!(json.as_object().unwrap().len(), 2);
    }

    #[test]
    fn the_capabilities_track_the_macos_code_paths() {
        assert_eq!(LIQUID_GLASS, cfg!(target_os = "macos"));
        assert_eq!(HIGH_REFRESH, cfg!(target_os = "macos"));
    }

    /// The key is looked up by name, so a typo would turn the switch into a
    /// permanent "no such feature".
    #[test]
    fn the_feature_key_is_the_documented_one() {
        assert_eq!(
            FRAME_RATE_FEATURE_KEY,
            "PreferPageRenderingUpdatesNear60FPSEnabled"
        );
    }
}
