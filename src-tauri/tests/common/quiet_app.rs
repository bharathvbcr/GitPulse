//! Keeps a real-AppKit test out of the person's way.
//!
//! Tao's macOS event loop launches with `ActivationPolicy::Regular` and
//! `activate_ignoring_other_apps: true` unless told otherwise
//! (`framework/tao/src/platform_impl/macos/app_delegate.rs`, applied at launch
//! by `app_state.rs`). A `harness = false` test that runs the loop therefore
//! puts a Dock icon and a menu bar on the person's screen for as long as it
//! runs, even though every window it builds is `visible(false)`. `Accessory`
//! has no Dock icon and no menu bar of its own; it is what the shipped app
//! uses for `--background` (`src/lib.rs`), so the tray, status item, menu and
//! webview behaviour these tests check is the same under it.
//!
//! `App::set_activation_policy` stores the policy on the runtime before the
//! loop starts, so the launch applies `Accessory` directly instead of showing
//! the icon for a moment and removing it. Call it between `build` and `run`.
//!
//! The policy alone is not enough. Production paths that the tests drive on
//! purpose call `desktop::reveal_main` (every quit confirmation, an agent's
//! browse request), which switches back to `Regular`, shows the window and
//! focuses it. That put a window on screen and switched Spaces once per
//! scenario, for every `cargo test` run in any worktree.
//! `desktop::keep_off_screen` keeps those reveals off the screen, and
//! [`in_the_way`] lets a test assert that they stayed off.
//!
//! `scripts/native-test-quiet-contract.test.ts` fails a test target that runs
//! a real Tauri loop without calling this.
//!
//! Declared by `#[path]`, under `#[cfg(target_os = "macos")]`, in the
//! `harness = false` targets that run a real loop. Linux and Windows have no
//! activation policy, so there is nothing to call there.

/// Launches without taking focus. Tao's launch calls
/// `activateIgnoringOtherApps:` with `true` by default, which makes the test
/// process the active application, so the person's next keystrokes went to it.
/// With `false` it activates only when no other application is active. The
/// switch exists only on the builder, so wrap `tauri::Builder::default()` in it.
pub fn background_launch<R: tauri::Runtime>(builder: tauri::Builder<R>) -> tauri::Builder<R> {
    builder.activate_ignoring_other_apps(false)
}

/// Makes the application a background one, and keeps its reveals off the screen.
pub fn out_of_the_way<R: tauri::Runtime>(app: &mut tauri::App<R>) {
    app.set_activation_policy(tauri::ActivationPolicy::Accessory);
    waylode_lib::desktop::keep_off_screen();
}

/// Whether this process has put something in the person's way: a visible
/// `main` window, or a Dock icon (the `Regular` activation policy). Call it on
/// the main thread.
#[allow(
    dead_code,
    reason = "native_status_material shows its popover on purpose and does not ask"
)]
pub fn in_the_way<R: tauri::Runtime>(app: &tauri::AppHandle<R>) -> bool {
    use objc2::runtime::AnyObject;
    use objc2::{class, msg_send};
    use tauri::Manager;

    let visible = app
        .get_webview_window("main")
        .is_some_and(|window| window.is_visible().unwrap_or(false));
    // SAFETY: NSApplication's shared instance, read on the main thread as the
    // caller promises. NSApplicationActivationPolicyRegular is 0.
    let regular = unsafe {
        let ns_app: *mut AnyObject = msg_send![class!(NSApplication), sharedApplication];
        let policy: isize = msg_send![ns_app, activationPolicy];
        policy == 0
    };
    visible || regular
}
