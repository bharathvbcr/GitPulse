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
//! `scripts/native-test-quiet-contract.test.ts` fails a test target that runs
//! a real Tauri loop without calling this.
//!
//! Declared by `#[path]`, under `#[cfg(target_os = "macos")]`, in the
//! `harness = false` targets that run a real loop. Linux and Windows have no
//! activation policy, so there is nothing to call there.

/// Makes the application a background one.
pub fn out_of_the_way<R: tauri::Runtime>(app: &mut tauri::App<R>) {
    app.set_activation_policy(tauri::ActivationPolicy::Accessory);
}
