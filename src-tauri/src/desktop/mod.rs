pub mod actions;
mod menu;
pub mod popover;
pub mod shell;
pub mod state;
mod tray;

pub use state::MenuState;
pub use tray::build_tray_menu;

use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Mutex;

use serde::Serialize;
use tauri::{AppHandle, Emitter, Manager, RunEvent, Runtime, State, Window, WindowEvent};

use crate::engine::find_git_root;
use actions::NativeAction;
use menu::build_native_menu;

pub const MENU_EVENT: &str = "gitpulse-menu";
pub const OPEN_REPO_EVENT: &str = "gitpulse-open-repo";
pub const OPEN_ERROR_EVENT: &str = "gitpulse-open-error";
pub const EXIT_REQUESTED_EVENT: &str = "gitpulse-exit-requested";

#[derive(Default)]
pub struct DesktopState {
    recents: Mutex<Vec<String>>,
    menu_state: Mutex<MenuState>,
    pending_open: Mutex<Option<String>>,
    exit_guard_ready: AtomicBool,
}

#[derive(Clone, Serialize)]
pub struct NativeEvent {
    pub id: String,
    pub path: Option<String>,
    pub repo_path: Option<String>,
}

fn should_guard_exit(frontend_ready: bool, programmatic_exit: bool) -> bool {
    frontend_ready && !programmatic_exit
}

fn request_exit_confirmation<R: Runtime>(app: &AppHandle<R>) {
    if let Err(error) = reveal_main(app) {
        log::warn!(target: "desktop", "Could not reveal quit confirmation: {error}");
    }
    if let Err(error) = app.emit(EXIT_REQUESTED_EVENT, ()) {
        log::warn!(target: "desktop", "exit-request emit failed: {error}");
    }
}

pub fn install_menu<R: Runtime>(app: &AppHandle<R>) -> tauri::Result<()> {
    let recents = app
        .state::<DesktopState>()
        .recents
        .lock()
        .map(|g| g.clone())
        .unwrap_or_default();
    let state = menu_state(app);
    let menu = build_native_menu(app, &recents, &state)?;
    app.set_menu(menu)?;
    Ok(())
}

pub fn handle_menu_event<R: Runtime>(app: &AppHandle<R>, id: &str) {
    let (id, from_tray) = id
        .strip_prefix("tray:")
        .map_or((id, false), |id| (id, true));
    if from_tray {
        match id {
            "show" => {
                if let Err(error) = reveal_main(app) {
                    log::error!(target: "desktop", "Could not reveal GitPulse: {error}");
                }
                return;
            }
            "quit" => {
                if app
                    .state::<DesktopState>()
                    .exit_guard_ready
                    .load(Ordering::Acquire)
                {
                    request_exit_confirmation(app);
                } else {
                    app.exit(0);
                }
                return;
            }
            _ => {}
        }
    }
    let Some(action) = NativeAction::parse(id) else {
        return;
    };
    let state = menu_state(app);
    let recent = id
        .strip_prefix(actions::RECENT_PREFIX)
        .is_some_and(|path| recent_menu_entries(app).iter().any(|entry| entry == path));
    if !recent && !state.enabled(id) {
        return;
    }
    // Refresh and repository switching are useful without leaving the status menu.
    if from_tray
        && !matches!(
            action,
            NativeAction::Refresh | NativeAction::ActivateRepo(_)
        )
    {
        if let Err(error) = reveal_main(app) {
            log::error!(target: "desktop", "Could not reveal GitPulse: {error}");
        }
    }
    if let Err(e) = app.emit(
        MENU_EVENT,
        NativeEvent {
            id: action.event_id().to_string(),
            path: action.path().map(str::to_string),
            repo_path: state.active_path.clone(),
        },
    ) {
        log::warn!(target: "desktop", "menu event {id} emit failed: {e}");
    }
    if from_tray {
        if let Err(error) = tray::apply(app, &state) {
            log::warn!(target: "desktop", "Could not restore status menu: {error}");
        }
    }
    // Check menu items toggle themselves before the frontend can respond.
    // Restore source-of-truth state; the subscribed projection applies the result.
    if let Some(menu) = app.menu() {
        if let Err(error) = menu::apply_presentation(&menu, &state) {
            log::warn!(target: "desktop", "menu state restore failed: {error}");
        }
    }
}

pub fn handle_window_event<R: Runtime>(window: &Window<R>, event: &WindowEvent) {
    if let WindowEvent::CloseRequested { api, .. } = event {
        if request_window_close(window) {
            api.prevent_close();
        }
    }
}

/// Returns whether the native close must be prevented after routing it.
fn request_window_close<R: Runtime>(window: &Window<R>) -> bool {
    if window.label() == popover::LABEL {
        if let Err(error) = window.hide() {
            log::warn!(target: "desktop", "Could not dismiss status popover: {error}");
        }
        return true;
    }
    // Auxiliary windows may close independently; only the main window owns
    // the session's hide-or-quit behavior.
    if window.label() != "main" {
        return false;
    }
    let app = window.app_handle();
    if menu_state(app).show_status_icon && app.tray_by_id(tray::TRAY_ID).is_some() {
        match window.hide() {
            Ok(()) => return true,
            Err(error) => log::warn!(target: "desktop", "Could not hide main window: {error}"),
        }
    }
    let ready = app
        .state::<DesktopState>()
        .exit_guard_ready
        .load(Ordering::Acquire);
    if ready {
        request_exit_confirmation(app);
        true
    } else {
        // Before the listener is installed, nobody can answer a quit request.
        // macOS normally leaves the process alive after its last window closes.
        #[cfg(target_os = "macos")]
        app.exit(0);
        false
    }
}

pub fn handle_run_event<R: Runtime>(app: &AppHandle<R>, event: &RunEvent) {
    // Only the macOS arms below read `app`; the exit arms reap the sidecar and
    // take nothing. Consumed explicitly rather than renamed to `_app` so the
    // parameter keeps its name on the platform that uses it, matching
    // `handle_window_event` above.
    #[cfg(not(target_os = "macos"))]
    let _ = app;
    match event {
        #[cfg(target_os = "macos")]
        RunEvent::Reopen {
            has_visible_windows,
            ..
        } => {
            if !has_visible_windows {
                if let Err(error) = reveal_main(app) {
                    log::warn!(target: "desktop", "Could not reveal main window: {error}");
                }
            }
        }
        #[cfg(target_os = "macos")]
        RunEvent::Opened { urls } => {
            for url in urls {
                if let Ok(path) = url.to_file_path() {
                    queue_and_emit_open(app, &path);
                }
            }
        }
        // The sidecar lives in a static that never drops, so it must be
        // reaped explicitly on quit or a live `manvi serve` is orphaned.
        // Both exit events are handled defensively (idempotent): which of
        // them fires depends on how the app is told to quit, and
        // `sidecar::shutdown` gives up after ~1.2s rather than stalling
        // exit behind an in-flight request.
        RunEvent::ExitRequested { code, api, .. } => {
            let ready = app
                .state::<DesktopState>()
                .exit_guard_ready
                .load(Ordering::Acquire);
            if should_guard_exit(ready, code.is_some()) {
                api.prevent_exit();
                request_exit_confirmation(app);
            } else {
                crate::harness::sidecar::shutdown();
            }
        }
        RunEvent::Exit => crate::harness::sidecar::shutdown(),
        _ => {}
    }
}

pub fn queue_and_emit_open<R: Runtime>(app: &AppHandle<R>, path: &Path) {
    match find_git_root(path) {
        Some(root) => {
            let root_str = root.to_string_lossy().into_owned();
            {
                let desktop_state = app.state::<DesktopState>();
                let mut pending = desktop_state
                    .pending_open
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner);
                *pending = Some(root_str.clone());
            }
            if let Err(error) = reveal_main(app) {
                log::warn!(target: "desktop", "Could not reveal main window: {error}");
            }
            // The pending slot is a fallback for a listener that was not yet
            // mounted. Once the event channel confirmed delivery, clearing it
            // prevents a late `cmd_take_pending_open` consumer from opening
            // the same repository a second time. A failed emit keeps the slot
            // so the fallback still delivers.
            if app
                .emit(
                    OPEN_REPO_EVENT,
                    NativeEvent {
                        id: "open-repo".into(),
                        path: Some(root_str),
                        repo_path: None,
                    },
                )
                .is_ok()
            {
                let desktop_state = app.state::<DesktopState>();
                let mut pending = desktop_state
                    .pending_open
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner);
                *pending = None;
            }
        }
        None => {
            if let Err(e) = app.emit(
                OPEN_ERROR_EVENT,
                format!("Not a Git repository: {}", path.display()),
            ) {
                log::warn!(
                    target: "desktop",
                    "open-error emit failed for {}: {e}",
                    path.display()
                );
            }
        }
    }
}

fn reveal_main<R: Runtime>(app: &AppHandle<R>) -> Result<(), String> {
    let window = app
        .get_webview_window("main")
        .ok_or("GitPulse main window is unavailable")?;
    #[cfg(target_os = "macos")]
    app.show().map_err(|error| error.to_string())?;
    window.show().map_err(|error| error.to_string())?;
    window.unminimize().map_err(|error| error.to_string())?;
    window.set_focus().map_err(|error| error.to_string())
}

pub fn menu_state<R: Runtime>(app: &AppHandle<R>) -> MenuState {
    app.state::<DesktopState>()
        .menu_state
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .clone()
}

/// All callers run on the GUI thread. No state lock is held while AppKit runs.
pub fn set_menu_state<R: Runtime>(app: &AppHandle<R>, next: MenuState) -> Result<(), String> {
    next.validate()?;
    let previous = menu_state(app);
    let menu = if previous.repositories != next.repositories {
        let menu = build_native_menu(app, &recent_menu_entries(app), &next)
            .map_err(|error| error.to_string())?;
        app.set_menu(menu.clone())
            .map_err(|error| error.to_string())?;
        menu
    } else {
        app.menu().ok_or("Native menu is unavailable")?
    };
    menu::apply_presentation(&menu, &next).map_err(|error| error.to_string())?;
    if previous.show_status_icon != next.show_status_icon
        || previous.tray_summary != next.tray_summary
        || previous.tray_detail != next.tray_detail
        || previous.tray_details != next.tray_details
        || previous.repositories != next.repositories
        || previous.enabled(actions::REFRESH) != next.enabled(actions::REFRESH)
    {
        tray::apply(app, &next)?;
    }
    *app.state::<DesktopState>()
        .menu_state
        .lock()
        .map_err(|error| error.to_string())? = next.clone();
    popover::publish(app, &next)
}

async fn on_main_thread<R: Runtime, F>(app: AppHandle<R>, update: F) -> Result<(), String>
where
    F: FnOnce(&AppHandle<R>) -> Result<(), String> + Send + 'static,
{
    let (sender, receiver) = std::sync::mpsc::sync_channel(1);
    let handle = app.clone();
    app.run_on_main_thread(move || {
        if sender.send(update(&handle)).is_err() {
            log::warn!(target: "desktop", "Native menu caller disconnected");
        }
    })
    .map_err(|error| error.to_string())?;
    tauri::async_runtime::spawn_blocking(move || {
        receiver.recv_timeout(std::time::Duration::from_secs(10))
    })
    .await
    .map_err(|error| error.to_string())?
    .map_err(|error| format!("Native menu update did not finish: {error}"))?
}

#[tauri::command]
pub async fn cmd_set_menu_state(app: AppHandle, presentation: MenuState) -> Result<(), String> {
    presentation.validate()?;
    on_main_thread(app, move |app| set_menu_state(app, presentation)).await
}

#[tauri::command(async)]
pub fn cmd_take_pending_open(state: State<DesktopState>) -> Option<String> {
    let mut pending = state
        .pending_open
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    pending.take()
}

/// Records the recent-repository list and rebuilds the native menu from it.
///
/// Extracted from `cmd_set_recent_menu` so the logic is reachable on a runtime
/// other than Wry. A `#[tauri::command]` binds itself to the concrete `Wry`
/// handle, which put the cap, the state write and the menu rebuild — every
/// part of this that can be wrong — behind a signature no test can construct.
/// The command below is now the thin wrapper it should always have been.
pub fn set_recent_menu<R: Runtime>(app: &AppHandle<R>, paths: Vec<String>) -> Result<(), String> {
    let mut seen = std::collections::HashSet::new();
    let capped: Vec<String> = paths
        .into_iter()
        .filter(|path| !path.is_empty() && path.len() <= 16384 && seen.insert(path.clone()))
        .take(RECENT_MENU_LIMIT)
        .collect();
    let state = menu_state(app);
    let menu = build_native_menu(app, &capped, &state).map_err(|e| e.to_string())?;
    app.set_menu(menu).map_err(|e| e.to_string())?;
    {
        let state = app.state::<DesktopState>();
        let mut recents = state
            .recents
            .lock()
            .map_err(|e| format!("Recent-repo lock poisoned: {e}"))?;
        *recents = capped.clone();
    }
    Ok(())
}

/// How many recent repositories the File menu will show.
///
/// Named rather than inline so the test asserting the cap and the code applying
/// it cannot disagree about the number.
pub const RECENT_MENU_LIMIT: usize = 12;

/// Reads back the recorded recent list. Exists so a caller — including a test —
/// can check what was stored without reaching into private state.
pub fn recent_menu_entries<R: Runtime>(app: &AppHandle<R>) -> Vec<String> {
    app.state::<DesktopState>()
        .recents
        .lock()
        .map(|entries| entries.clone())
        .unwrap_or_default()
}

#[tauri::command(async)]
pub async fn cmd_set_recent_menu(app: AppHandle, paths: Vec<String>) -> Result<(), String> {
    on_main_thread(app, move |app| set_recent_menu(app, paths)).await
}

#[tauri::command(async)]
pub fn cmd_resolve_git_root(path: String) -> Result<String, String> {
    find_git_root(Path::new(&path))
        .map(|p| p.to_string_lossy().into_owned())
        .ok_or_else(|| format!("Not a Git repository: {path}"))
}

/// Arms close protection only after the frontend listener is installed.
#[tauri::command(async)]
pub fn cmd_set_exit_guard_ready(state: State<DesktopState>) {
    state.exit_guard_ready.store(true, Ordering::Release);
}

/// Completes a frontend-approved exit. `AppHandle::exit` supplies a code, so
/// the run-event guard can distinguish this from an OS/user quit request.
#[tauri::command(async)]
pub fn cmd_exit_app(app: AppHandle) {
    app.exit(0);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dismissing_status_does_not_request_app_exit_but_main_close_does() {
        use std::sync::{Arc, Mutex};
        use tauri::{Listener, WebviewUrl, WebviewWindowBuilder};
        let app = tauri::test::mock_builder()
            .manage(DesktopState::default())
            .build(crate::context())
            .unwrap();
        let main = WebviewWindowBuilder::new(&app, "main", WebviewUrl::App("index.html".into()))
            .build()
            .unwrap();
        let status =
            WebviewWindowBuilder::new(&app, popover::LABEL, WebviewUrl::App("status.html".into()))
                .build()
                .unwrap();
        let confirmations = Arc::new(Mutex::new(0));
        let received = Arc::clone(&confirmations);
        app.listen(EXIT_REQUESTED_EVENT, move |_| {
            *received.lock().unwrap() += 1;
        });
        cmd_set_exit_guard_ready(app.state::<DesktopState>());
        assert!(
            request_window_close(&status.as_ref().window()),
            "dismissal must retain the reusable window"
        );
        assert_eq!(
            *confirmations.lock().unwrap(),
            0,
            "popover close must never ask to quit the app"
        );
        assert!(request_window_close(&main.as_ref().window()));
        assert_eq!(
            *confirmations.lock().unwrap(),
            1,
            "main-window quit protection must remain active"
        );
    }

    #[test]
    fn pending_open_is_taken_once() {
        let state = DesktopState::default();
        *state.pending_open.lock().unwrap() = Some("/tmp/repo".into());
        assert_eq!(
            state.pending_open.lock().unwrap().take(),
            Some("/tmp/repo".into())
        );
        assert_eq!(state.pending_open.lock().unwrap().take(), None);
    }

    #[test]
    fn exit_guard_only_intercepts_user_requests_after_frontend_is_ready() {
        assert!(!should_guard_exit(false, false));
        assert!(!should_guard_exit(false, true));
        assert!(should_guard_exit(true, false));
        assert!(!should_guard_exit(true, true));
    }
}
