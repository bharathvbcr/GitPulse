pub mod actions;
mod menu;

use std::path::Path;
use std::sync::Mutex;

use serde::Serialize;
use tauri::{AppHandle, Emitter, Manager, RunEvent, Runtime, State, Window, WindowEvent};

use crate::engine::find_git_root;
use actions::NativeAction;
use menu::build_native_menu;

pub const MENU_EVENT: &str = "gitpulse-menu";
pub const OPEN_REPO_EVENT: &str = "gitpulse-open-repo";
pub const OPEN_ERROR_EVENT: &str = "gitpulse-open-error";

#[derive(Default)]
pub struct DesktopState {
    recents: Mutex<Vec<String>>,
    pending_open: Mutex<Option<String>>,
}

#[derive(Clone, Serialize)]
pub struct NativeEvent {
    pub id: String,
    pub path: Option<String>,
}

pub fn install_menu<R: Runtime>(app: &AppHandle<R>) -> tauri::Result<()> {
    let recents = app
        .state::<DesktopState>()
        .recents
        .lock()
        .map(|g| g.clone())
        .unwrap_or_default();
    let menu = build_native_menu(app, &recents)?;
    app.set_menu(menu)?;
    Ok(())
}

pub fn handle_menu_event<R: Runtime>(app: &AppHandle<R>, id: &str) {
    let Some(action) = NativeAction::parse(id) else {
        return;
    };
    let _ = app.emit(
        MENU_EVENT,
        NativeEvent {
            id: action.event_id().to_string(),
            path: action.path().map(str::to_string),
        },
    );
}

pub fn handle_window_event<R: Runtime>(window: &Window<R>, event: &WindowEvent) {
    #[cfg(target_os = "macos")]
    if let WindowEvent::CloseRequested { api, .. } = event {
        api.prevent_close();
        let _ = window.hide();
    }
    #[cfg(not(target_os = "macos"))]
    {
        let _ = (window, event);
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
                reveal_main(app);
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
        RunEvent::ExitRequested { .. } => crate::harness::sidecar::shutdown(),
        RunEvent::Exit => crate::harness::sidecar::shutdown(),
        _ => {}
    }
}

pub fn queue_and_emit_open<R: Runtime>(app: &AppHandle<R>, path: &Path) {
    match find_git_root(path) {
        Some(root) => {
            let root_str = root.to_string_lossy().into_owned();
            if let Ok(mut pending) = app.state::<DesktopState>().pending_open.lock() {
                *pending = Some(root_str.clone());
            }
            reveal_main(app);
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
                    },
                )
                .is_ok()
            {
                if let Ok(mut pending) = app.state::<DesktopState>().pending_open.lock() {
                    *pending = None;
                }
            }
        }
        None => {
            let _ = app.emit(
                OPEN_ERROR_EVENT,
                format!("Not a Git repository: {}", path.display()),
            );
        }
    }
}

fn reveal_main<R: Runtime>(app: &AppHandle<R>) {
    if let Some(window) = app.get_webview_window("main") {
        let _ = window.show();
        let _ = window.set_focus();
    }
}

#[tauri::command(async)]
pub fn cmd_take_pending_open(state: State<DesktopState>) -> Option<String> {
    state.pending_open.lock().ok()?.take()
}

#[tauri::command(async)]
pub fn cmd_set_recent_menu(app: AppHandle, paths: Vec<String>) -> Result<(), String> {
    let capped: Vec<String> = paths.into_iter().take(12).collect();
    {
        let state = app.state::<DesktopState>();
        let mut recents = state
            .recents
            .lock()
            .map_err(|e| format!("Recent-repo lock poisoned: {e}"))?;
        *recents = capped.clone();
    }
    let menu = build_native_menu(&app, &capped).map_err(|e| e.to_string())?;
    app.set_menu(menu).map_err(|e| e.to_string())?;
    Ok(())
}

#[tauri::command(async)]
pub fn cmd_resolve_git_root(path: String) -> Result<String, String> {
    find_git_root(Path::new(&path))
        .map(|p| p.to_string_lossy().into_owned())
        .ok_or_else(|| format!("Not a Git repository: {path}"))
}

#[cfg(test)]
mod tests {
    use super::*;

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
}
