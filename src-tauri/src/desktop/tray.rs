use super::state::MenuState;
use tauri::{
    menu::{Menu, MenuItem},
    AppHandle, Runtime,
};

pub const TRAY_ID: &str = "gitpulse-status";

/// A monochrome pulse with transparent margins, rendered as a macOS template.
fn pulse_icon() -> tauri::image::Image<'static> {
    const SIZE: u32 = 36;
    let mut rgba = vec![0; (SIZE * SIZE * 4) as usize];
    let points = [
        (5i32, 19i32),
        (11, 19),
        (15, 9),
        (21, 28),
        (25, 19),
        (31, 19),
    ];
    for pair in points.windows(2) {
        let (x0, y0) = pair[0];
        let (x1, y1) = pair[1];
        let steps = (x1 - x0).abs().max((y1 - y0).abs());
        for step in 0..=steps {
            let x = x0 + (x1 - x0) * step / steps;
            let y = y0 + (y1 - y0) * step / steps;
            for dy in -1..=1 {
                for dx in -1..=1 {
                    let offset = (((y + dy) as u32 * SIZE + (x + dx) as u32) * 4 + 3) as usize;
                    rgba[offset] = 255;
                }
            }
        }
    }
    tauri::image::Image::new_owned(rgba, SIZE, SIZE)
}

/// Right-click escape hatch. Left click opens the card popover.
pub fn build_tray_menu<R: Runtime>(app: &AppHandle<R>) -> tauri::Result<Menu<R>> {
    let menu = Menu::new(app)?;
    for (id, label) in [
        ("show", "Open GitPulse"),
        ("settings", "Settings…"),
        ("quit", "Quit GitPulse"),
    ] {
        menu.append(&MenuItem::with_id(
            app,
            format!("tray:{id}"),
            label,
            true,
            None::<&str>,
        )?)?;
    }
    Ok(menu)
}

/// Called on the main thread by the same owner as the application menu.
pub fn apply<R: Runtime>(app: &AppHandle<R>, state: &MenuState) -> Result<(), String> {
    if !state.show_status_icon {
        if app.tray_by_id(TRAY_ID).is_some() {
            // Removing the last escape hatch must first make its window reachable.
            super::reveal_main(app)?;
            super::popover::hide(app)?;
            app.remove_tray_by_id(TRAY_ID);
        }
        return Ok(());
    }
    let menu = build_tray_menu(app).map_err(|error| error.to_string())?;
    let tooltip = format!(
        "GitPulse\n{}\n{}",
        state.tray_detail, state.tray_summary.text
    );
    if let Some(tray) = app.tray_by_id(TRAY_ID) {
        tray.set_menu(Some(menu))
            .map_err(|error| error.to_string())?;
        tray.set_tooltip(Some(tooltip))
            .map_err(|error| error.to_string())?;
    } else {
        tauri::tray::TrayIconBuilder::with_id(TRAY_ID)
            .icon(pulse_icon())
            .icon_as_template(true)
            .tooltip(tooltip)
            .menu(&menu)
            .show_menu_on_left_click(false)
            .on_tray_icon_event(|tray, event| {
                if matches!(
                    event,
                    tauri::tray::TrayIconEvent::Click {
                        button: tauri::tray::MouseButton::Left,
                        button_state: tauri::tray::MouseButtonState::Up,
                        ..
                    }
                ) {
                    if let Err(error) = super::popover::toggle(tray.app_handle()) {
                        log::error!(target: "desktop", "Status popover failed: {error}");
                        super::handle_menu_event(tray.app_handle(), "tray:show");
                    }
                }
            })
            .build(app)
            .map_err(|error| error.to_string())?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    #[test]
    fn status_glyph_has_transparent_padding_and_visible_strokes() {
        let icon = super::pulse_icon();
        assert_eq!((icon.width(), icon.height()), (36, 36));
        assert!(icon
            .rgba()
            .as_chunks::<4>()
            .0
            .iter()
            .any(|pixel| pixel[3] == 255));
        assert!(icon.rgba()[..36 * 4].iter().all(|byte| *byte == 0));
    }
}
