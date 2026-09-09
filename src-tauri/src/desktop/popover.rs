//! One lightweight status window. The main webview remains the workspace owner.
use super::{menu_state, MenuState};
use tauri::{
    AppHandle, Emitter, Manager, PhysicalPosition, PhysicalSize, Runtime, WebviewUrl,
    WebviewWindow, WebviewWindowBuilder, WindowEvent,
};

pub const LABEL: &str = "status-popover";
pub const EVENT: &str = "gitpulse-status-state";
const WIDTH: f64 = 376.0;
const MAX_HEIGHT: f64 = 640.0;

/// Coordinates are physical so crossing displays does not reuse the old DPI.
fn placement(icon: [f64; 4], work: [f64; 4], scale: f64, height: f64) -> [f64; 4] {
    let inset = 6.0 * scale;
    let width = (WIDTH * scale).min((work[2] - inset * 2.0).max(1.0));
    let height = (height * scale).min((work[3] - inset * 2.0).max(1.0));
    let left = work[0] + inset;
    let top = work[1] + inset;
    let x = (icon[0] + icon[2] / 2.0 - width / 2.0)
        .clamp(left, (work[0] + work[2] - width - inset).max(left));
    let y =
        (icon[1] + icon[3] + 4.0 * scale).clamp(top, (work[1] + work[3] - height - inset).max(top));
    [x, y, width, height]
}

fn position<R: Runtime>(
    app: &AppHandle<R>,
    window: &WebviewWindow<R>,
    height: f64,
) -> Result<(), String> {
    let rect = app
        .tray_by_id(super::tray::TRAY_ID)
        .ok_or("Status icon is unavailable")?
        .rect()
        .map_err(|e| e.to_string())?
        .ok_or("Status icon position is unavailable")?;
    // TrayIconEvent and tray-icon's rect conversion supply physical coordinates.
    let point = rect.position.to_physical::<f64>(1.0);
    let size = rect.size.to_physical::<f64>(1.0);
    let monitor = app
        .monitor_from_point(point.x, point.y)
        .map_err(|e| e.to_string())?
        .ok_or("Status icon display is unavailable")?;
    let work = monitor.work_area();
    let [x, y, width, height] = placement(
        [point.x, point.y, size.width, size.height],
        [
            work.position.x as f64,
            work.position.y as f64,
            work.size.width as f64,
            work.size.height as f64,
        ],
        monitor.scale_factor(),
        height,
    );
    window
        .set_position(PhysicalPosition::new(x, y))
        .map_err(|e| e.to_string())?;
    window
        .set_size(PhysicalSize::new(width, height))
        .map_err(|e| e.to_string())
}

pub fn hide<R: Runtime>(app: &AppHandle<R>) -> Result<(), String> {
    if let Some(window) = app.get_webview_window(LABEL) {
        window.hide().map_err(|e| e.to_string())?;
    }
    Ok(())
}

pub fn publish<R: Runtime>(app: &AppHandle<R>, state: &MenuState) -> Result<(), String> {
    if let Some(window) = app.get_webview_window(LABEL) {
        window.emit(EVENT, state).map_err(|e| e.to_string())?;
    }
    Ok(())
}

pub fn toggle<R: Runtime>(app: &AppHandle<R>) -> Result<(), String> {
    if !menu_state(app).show_status_icon {
        return Ok(());
    }
    let window = if let Some(window) = app.get_webview_window(LABEL) {
        if window.is_visible().map_err(|e| e.to_string())? {
            return hide(app);
        }
        window
    } else {
        let window = WebviewWindowBuilder::new(app, LABEL, WebviewUrl::App("status.html".into()))
            .title("GitPulse Status")
            .inner_size(WIDTH, 400.0)
            .decorations(false)
            .resizable(false)
            .visible(false)
            .focused(false)
            .skip_taskbar(true)
            .always_on_top(true)
            .visible_on_all_workspaces(true)
            .transparent(true)
            .shadow(true)
            .build()
            .map_err(|e| e.to_string())?;
        let handle = app.clone();
        window.on_window_event(move |event| {
            if matches!(event, WindowEvent::Focused(false)) {
                if let Err(error) = hide(&handle) {
                    log::warn!(target: "desktop", "Status popover dismissal failed: {error}");
                }
            }
        });
        window
    };
    let height = window
        .inner_size()
        .map_err(|e| e.to_string())?
        .to_logical::<f64>(window.scale_factor().map_err(|e| e.to_string())?)
        .height;
    position(app, &window, height.clamp(240.0, MAX_HEIGHT))?;
    publish(app, &menu_state(app))?;
    window.show().map_err(|e| e.to_string())?;
    window.set_focus().map_err(|e| e.to_string())
}

fn validate_action(state: &MenuState, id: &str, repo_path: Option<&str>) -> Result<(), String> {
    if matches!(id, "show" | "settings" | "quit" | "dismiss") {
        return Ok(());
    }
    let allowed = matches!(
        id,
        "open"
            | "refresh"
            | "section:work:overview"
            | "section:work:resolve"
            | "section:history:graph"
    ) || id.starts_with(super::actions::REPOSITORY_PREFIX);
    if !allowed || !state.enabled(id) {
        return Err("This status action is unavailable".into());
    }
    if id != "open" && state.active_path.as_deref() != repo_path {
        return Err("Repository changed. Review the current status and try again.".into());
    }
    Ok(())
}

fn require_status_window<R: Runtime>(window: &WebviewWindow<R>) -> Result<(), String> {
    if window.label() == LABEL {
        Ok(())
    } else {
        Err("This command belongs to the status popover".into())
    }
}

#[tauri::command]
pub fn cmd_get_status_state(app: AppHandle, window: WebviewWindow) -> Result<MenuState, String> {
    require_status_window(&window)?;
    Ok(menu_state(&app))
}

#[tauri::command]
pub async fn cmd_status_action(
    app: AppHandle,
    window: WebviewWindow,
    id: String,
    repo_path: Option<String>,
) -> Result<(), String> {
    require_status_window(&window)?;
    super::on_main_thread(app, move |app| {
        validate_action(&menu_state(app), &id, repo_path.as_deref())?;
        if id == "dismiss" {
            return hide(app);
        }
        if id != "refresh" && !id.starts_with(super::actions::REPOSITORY_PREFIX) {
            hide(app)?;
        }
        super::handle_menu_event(app, &format!("tray:{id}"));
        Ok(())
    })
    .await
}

#[tauri::command]
pub async fn cmd_resize_status(
    app: AppHandle,
    window: WebviewWindow,
    height: f64,
) -> Result<(), String> {
    require_status_window(&window)?;
    if !height.is_finite() || !(100.0..=MAX_HEIGHT).contains(&height) {
        return Err("Invalid status popover height".into());
    }
    super::on_main_thread(app, move |app| {
        let window = app
            .get_webview_window(LABEL)
            .ok_or("Status popover is unavailable")?;
        position(app, &window, height)
    })
    .await
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn anchors_under_icon_and_stays_inside_each_display() {
        assert_eq!(
            placement(
                [900.0, 0.0, 22.0, 24.0],
                [0.0, 24.0, 1000.0, 776.0],
                1.0,
                400.0
            ),
            [618.0, 30.0, 376.0, 400.0]
        );
        for scale in [1.0, 2.0] {
            for left in [-1920.0, 0.0, 1920.0] {
                let work = [left, 48.0, 1920.0, 1032.0];
                for icon_x in [left, left + 1900.0] {
                    let [x, y, width, height] =
                        placement([icon_x, 0.0, 40.0, 48.0], work, scale, 640.0);
                    assert!(x >= left && x + width <= left + 1920.0);
                    assert!(y >= 48.0 && y + height <= 1080.0);
                }
            }
        }
    }
    #[test]
    fn status_actions_cannot_mutate_git_or_target_a_stale_repository() {
        let mut state = MenuState {
            active_path: Some("/a".into()),
            ..Default::default()
        };
        state.enabled.extend(["refresh".into(), "push".into()]);
        assert!(validate_action(&state, "refresh", Some("/a")).is_ok());
        assert!(validate_action(&state, "refresh", Some("/b")).is_err());
        assert!(validate_action(&state, "push", Some("/a")).is_err());
        assert!(validate_action(&state, "activate-repo:/missing", Some("/a")).is_err());
        assert!(validate_action(&state, "quit", None).is_ok());
    }
}
