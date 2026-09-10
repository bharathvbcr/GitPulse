use super::actions;
use super::state::MenuState;
use tauri::{
    menu::{Menu, MenuItem, PredefinedMenuItem},
    tray::{MouseButton, MouseButtonState, TrayIconEvent},
    AppHandle, Manager, Runtime,
};

pub const TRAY_ID: &str = "gitpulse-status";

/// Which template glyph the status item currently shows.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum GlyphVariant {
    Plain,
    Attention,
}

/// Left opens the popover; right shows the temporary context menu.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TrayClick {
    Left,
    Right,
}

/// Attention when the workspace needs a glance: warning tone, parked op, or degraded watch.
pub fn glyph_variant(state: &MenuState) -> GlyphVariant {
    if state.status.tone == "warning"
        || state.status.operation.is_some()
        || state.status.watch_status == "degraded"
    {
        GlyphVariant::Attention
    } else {
        GlyphVariant::Plain
    }
}

/// Maps a tray-icon event onto the single click dispatcher. Only mouse-up clicks route.
pub fn map_tray_event(event: &TrayIconEvent) -> Option<TrayClick> {
    match event {
        TrayIconEvent::Click {
            button: MouseButton::Left,
            button_state: MouseButtonState::Up,
            ..
        } => Some(TrayClick::Left),
        TrayIconEvent::Click {
            button: MouseButton::Right,
            button_state: MouseButtonState::Up,
            ..
        } => Some(TrayClick::Right),
        _ => None,
    }
}

/// One dispatcher for overlay clicks (and a future button action, if ever needed).
pub fn dispatch_click<R: Runtime>(app: &AppHandle<R>, click: TrayClick) {
    match click {
        TrayClick::Left => {
            if let Err(error) = super::popover::toggle(app) {
                log::error!(target: "desktop", "Status popover failed: {error}");
                super::handle_menu_event(app, "tray:show");
            }
        }
        TrayClick::Right => {
            let state = super::menu_state(app);
            if let Err(error) = show_context_menu(app, &state) {
                log::error!(target: "desktop", "Status context menu failed: {error}");
            }
        }
    }
}

/// A brand pulse polyline, 4× supersampled then box-filtered into an alpha-only template.
fn pulse_icon(variant: GlyphVariant) -> tauri::image::Image<'static> {
    const SIZE: u32 = 36;
    const SUPER: u32 = 4;
    const HI: u32 = SIZE * SUPER;
    let mut hi = vec![0u8; (HI * HI) as usize];
    let points = [
        (5i32 * SUPER as i32, 19i32 * SUPER as i32),
        (11 * SUPER as i32, 19 * SUPER as i32),
        (15 * SUPER as i32, 9 * SUPER as i32),
        (21 * SUPER as i32, 28 * SUPER as i32),
        (25 * SUPER as i32, 19 * SUPER as i32),
        (31 * SUPER as i32, 19 * SUPER as i32),
    ];
    let stroke = SUPER as i32;
    for pair in points.windows(2) {
        let (x0, y0) = pair[0];
        let (x1, y1) = pair[1];
        let steps = (x1 - x0).abs().max((y1 - y0).abs()).max(1);
        for step in 0..=steps {
            let x = x0 + (x1 - x0) * step / steps;
            let y = y0 + (y1 - y0) * step / steps;
            for dy in -stroke..=stroke {
                for dx in -stroke..=stroke {
                    if dx * dx + dy * dy > stroke * stroke {
                        continue;
                    }
                    let px = x + dx;
                    let py = y + dy;
                    if px < 0 || py < 0 || px >= HI as i32 || py >= HI as i32 {
                        continue;
                    }
                    hi[(py as u32 * HI + px as u32) as usize] = 255;
                }
            }
        }
    }
    if variant == GlyphVariant::Attention {
        // Filled attention dot, top-right, with a one-pixel knock-out ring at final size.
        let cx = (29 * SUPER) as i32;
        let cy = (7 * SUPER) as i32;
        let outer = (3 * SUPER) as i32;
        let ring = SUPER as i32;
        for py in (cy - outer - ring)..=(cy + outer + ring) {
            for px in (cx - outer - ring)..=(cx + outer + ring) {
                if px < 0 || py < 0 || px >= HI as i32 || py >= HI as i32 {
                    continue;
                }
                let d2 = (px - cx) * (px - cx) + (py - cy) * (py - cy);
                let idx = (py as u32 * HI + px as u32) as usize;
                if d2 <= outer * outer {
                    hi[idx] = 255;
                } else if d2 <= (outer + ring) * (outer + ring) {
                    hi[idx] = 0;
                }
            }
        }
    }
    let mut rgba = vec![0u8; (SIZE * SIZE * 4) as usize];
    for y in 0..SIZE {
        for x in 0..SIZE {
            let mut sum = 0u32;
            for dy in 0..SUPER {
                for dx in 0..SUPER {
                    sum += hi[((y * SUPER + dy) * HI + (x * SUPER + dx)) as usize] as u32;
                }
            }
            let alpha = (sum / (SUPER * SUPER)) as u8;
            rgba[((y * SIZE + x) * 4 + 3) as usize] = alpha;
        }
    }
    tauri::image::Image::new_owned(rgba, SIZE, SIZE)
}

fn tray_item<R: Runtime>(app: &AppHandle<R>, id: &str, label: &str) -> tauri::Result<MenuItem<R>> {
    MenuItem::with_id(app, format!("tray:{id}"), label, true, None::<&str>)
}

/// Right-click escape hatch plus the current primary/refresh actions. Left click opens the card popover.
pub fn build_tray_menu<R: Runtime>(
    app: &AppHandle<R>,
    state: &MenuState,
) -> tauri::Result<Menu<R>> {
    let menu = Menu::new(app)?;
    menu.append(&tray_item(app, "show", "Open GitPulse")?)?;
    let primary = state.tray_summary.id.as_str();
    let extra = primary != "open"
        && primary != "show"
        && state.enabled(primary)
        && !state.status.primary_label.is_empty();
    let refresh = state.enabled(actions::REFRESH);
    if extra || refresh {
        menu.append(&PredefinedMenuItem::separator(app)?)?;
        if extra {
            menu.append(&tray_item(app, primary, &state.status.primary_label)?)?;
        }
        if refresh {
            menu.append(&tray_item(app, actions::REFRESH, "Refresh")?)?;
        }
        menu.append(&PredefinedMenuItem::separator(app)?)?;
    }
    menu.append(&tray_item(app, "settings", "Settings…")?)?;
    menu.append(&tray_item(app, "quit", "Quit GitPulse")?)?;
    Ok(menu)
}

/// Attach the menu only for the synchronous tracking click, then detach so left-clicks cannot pop it.
fn show_context_menu<R: Runtime>(app: &AppHandle<R>, state: &MenuState) -> Result<(), String> {
    let Some(tray) = app.tray_by_id(TRAY_ID) else {
        return Ok(());
    };
    let menu = build_tray_menu(app, state).map_err(|error| error.to_string())?;
    tray.set_menu(Some(menu))
        .map_err(|error| error.to_string())?;
    tray.with_inner_tray_icon(|inner| inner.show_menu())
        .map_err(|error| error.to_string())?;
    tray.set_menu(None::<Menu<R>>)
        .map_err(|error| error.to_string())?;
    Ok(())
}

#[cfg(all(debug_assertions, target_os = "macos"))]
fn dump_status_item_frames<R: Runtime>(tray: &tauri::tray::TrayIcon<R>) {
    use objc2::encode::{Encode, Encoding};
    use objc2::msg_send;
    use objc2::runtime::AnyObject;

    #[repr(C)]
    #[derive(Clone, Copy)]
    struct CGPoint {
        x: f64,
        y: f64,
    }
    #[repr(C)]
    #[derive(Clone, Copy)]
    struct CGSize {
        width: f64,
        height: f64,
    }
    #[repr(C)]
    #[derive(Clone, Copy)]
    struct CGRect {
        origin: CGPoint,
        size: CGSize,
    }

    // SAFETY: layout matches CoreGraphics CGRect / CGPoint / CGSize.
    unsafe impl Encode for CGPoint {
        const ENCODING: Encoding = Encoding::Struct("CGPoint", &[f64::ENCODING, f64::ENCODING]);
    }
    unsafe impl Encode for CGSize {
        const ENCODING: Encoding = Encoding::Struct("CGSize", &[f64::ENCODING, f64::ENCODING]);
    }
    unsafe impl Encode for CGRect {
        const ENCODING: Encoding =
            Encoding::Struct("CGRect", &[CGPoint::ENCODING, CGSize::ENCODING]);
    }

    let _ = tray.with_inner_tray_icon(|inner| {
        let Some(item) = inner.ns_status_item() else {
            log::debug!(target: "desktop", "tray frame dump: no NSStatusItem");
            return;
        };
        // SAFETY: tray-icon owns this status item for the icon's lifetime; we only read geometry.
        unsafe {
            let item_ptr = objc2::rc::Retained::as_ptr(&item).cast::<AnyObject>();
            let button: *mut AnyObject = msg_send![item_ptr, button];
            if button.is_null() {
                log::debug!(target: "desktop", "tray frame dump: no button");
                return;
            }
            let bounds: CGRect = msg_send![button, bounds];
            let frame: CGRect = msg_send![button, frame];
            log::debug!(
                target: "desktop",
                "tray button bounds=({:.1},{:.1} {:.1}x{:.1}) frame=({:.1},{:.1} {:.1}x{:.1})",
                bounds.origin.x,
                bounds.origin.y,
                bounds.size.width,
                bounds.size.height,
                frame.origin.x,
                frame.origin.y,
                frame.size.width,
                frame.size.height
            );
            let subviews: *mut AnyObject = msg_send![button, subviews];
            if subviews.is_null() {
                return;
            }
            let count: usize = msg_send![subviews, count];
            for index in 0..count {
                let view: *mut AnyObject = msg_send![subviews, objectAtIndex: index];
                if view.is_null() {
                    continue;
                }
                let sub_frame: CGRect = msg_send![view, frame];
                let class: *const AnyObject = msg_send![view, class];
                let name: *mut AnyObject = msg_send![class, description];
                let utf8: *const std::ffi::c_char = msg_send![name, UTF8String];
                let class_name = if utf8.is_null() {
                    "<unknown>".into()
                } else {
                    std::ffi::CStr::from_ptr(utf8)
                        .to_string_lossy()
                        .into_owned()
                };
                log::debug!(
                    target: "desktop",
                    "tray subview[{index}] {class_name} frame=({:.1},{:.1} {:.1}x{:.1})",
                    sub_frame.origin.x,
                    sub_frame.origin.y,
                    sub_frame.size.width,
                    sub_frame.size.height
                );
            }
        }
    });
}

/// Called on the main thread by the same owner as the application menu.
pub fn apply<R: Runtime>(app: &AppHandle<R>, state: &MenuState) -> Result<(), String> {
    if !state.show_status_icon {
        if app.tray_by_id(TRAY_ID).is_some() {
            // Removing the last escape hatch must first make its window reachable.
            super::reveal_main(app)?;
            super::popover::hide(app)?;
            app.remove_tray_by_id(TRAY_ID);
            if let Ok(mut glyph) = app.state::<super::DesktopState>().tray_glyph.lock() {
                *glyph = None;
            }
        }
        return Ok(());
    }
    let tooltip = format!(
        "GitPulse\n{}\n{}",
        state.tray_detail, state.tray_summary.text
    );
    let variant = glyph_variant(state);
    let title = state.tray_title.as_deref();
    if let Some(tray) = app.tray_by_id(TRAY_ID) {
        // Never leave a menu attached at rest — that is what steals left clicks on macOS.
        tray.set_menu(None::<Menu<R>>)
            .map_err(|error| error.to_string())?;
        tray.set_tooltip(Some(tooltip))
            .map_err(|error| error.to_string())?;
        let desktop = app.state::<super::DesktopState>();
        let mut glyph = desktop
            .tray_glyph
            .lock()
            .map_err(|error| error.to_string())?;
        if *glyph != Some(variant) {
            tray.set_icon_with_as_template(Some(pulse_icon(variant)), true)
                .map_err(|error| error.to_string())?;
            *glyph = Some(variant);
        }
        drop(glyph);
        tray.set_title(title).map_err(|error| error.to_string())?;
    } else {
        tauri::tray::TrayIconBuilder::with_id(TRAY_ID)
            .icon(pulse_icon(variant))
            .icon_as_template(true)
            .tooltip(tooltip)
            .show_menu_on_left_click(false)
            .on_tray_icon_event(|tray, event| {
                log::debug!(target: "desktop", "TrayIconEvent: {event:?}");
                if let Some(click) = map_tray_event(&event) {
                    dispatch_click(tray.app_handle(), click);
                }
            })
            .build(app)
            .map_err(|error| error.to_string())?;
        if let Ok(mut glyph) = app.state::<super::DesktopState>().tray_glyph.lock() {
            *glyph = Some(variant);
        }
        if let Some(tray) = app.tray_by_id(TRAY_ID) {
            tray.set_title(title).map_err(|error| error.to_string())?;
            #[cfg(all(debug_assertions, target_os = "macos"))]
            dump_status_item_frames(&tray);
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::desktop::state::StatusCard;

    #[test]
    fn status_glyph_has_transparent_padding_and_visible_strokes() {
        let icon = pulse_icon(GlyphVariant::Plain);
        assert_eq!((icon.width(), icon.height()), (36, 36));
        assert!(icon
            .rgba()
            .as_chunks::<4>()
            .0
            .iter()
            .any(|pixel| pixel[3] == 255));
        assert!(icon.rgba()[..36 * 4].iter().all(|byte| *byte == 0));
    }

    #[test]
    fn status_glyph_is_anti_aliased() {
        let icon = pulse_icon(GlyphVariant::Plain);
        assert!(
            icon.rgba()
                .as_chunks::<4>()
                .0
                .iter()
                .any(|pixel| pixel[3] > 0 && pixel[3] < 255),
            "supersampled strokes must leave intermediate alpha"
        );
    }

    #[test]
    fn attention_glyph_adds_a_top_right_dot() {
        let plain = pulse_icon(GlyphVariant::Plain);
        let attention = pulse_icon(GlyphVariant::Attention);
        let plain_ink: u32 = plain
            .rgba()
            .as_chunks::<4>()
            .0
            .iter()
            .map(|pixel| pixel[3] as u32)
            .sum();
        let attention_ink: u32 = attention
            .rgba()
            .as_chunks::<4>()
            .0
            .iter()
            .map(|pixel| pixel[3] as u32)
            .sum();
        assert!(attention_ink > plain_ink, "attention variant must add ink");
        // Top-right 10×10 should carry the filled dot.
        let mut top_right = 0u32;
        for y in 0..10 {
            for x in 26..36 {
                top_right += attention.rgba()[((y * 36 + x) * 4 + 3) as usize] as u32;
            }
        }
        assert!(top_right > 0, "attention dot sits in the top-right");
    }

    #[test]
    fn glyph_variant_follows_menu_attention_signals() {
        let mut state = MenuState::default();
        assert_eq!(glyph_variant(&state), GlyphVariant::Plain);
        state.status.tone = "warning".into();
        assert_eq!(glyph_variant(&state), GlyphVariant::Attention);
        state.status = StatusCard::default();
        state.status.operation = Some("Merge in progress".into());
        assert_eq!(glyph_variant(&state), GlyphVariant::Attention);
        state.status.operation = None;
        state.status.watch_status = "degraded".into();
        assert_eq!(glyph_variant(&state), GlyphVariant::Attention);
    }

    #[test]
    fn map_tray_event_routes_only_button_up_clicks() {
        fn click(button: MouseButton, button_state: MouseButtonState) -> TrayIconEvent {
            TrayIconEvent::Click {
                id: tauri::tray::TrayIconId::new("gitpulse-status"),
                position: tauri::PhysicalPosition::new(0.0, 0.0),
                rect: tauri::Rect {
                    position: tauri::Position::Physical(tauri::PhysicalPosition::new(0, 0)),
                    size: tauri::Size::Physical(tauri::PhysicalSize::new(1, 1)),
                },
                button,
                button_state,
            }
        }
        assert_eq!(
            map_tray_event(&click(MouseButton::Left, MouseButtonState::Up)),
            Some(TrayClick::Left)
        );
        assert_eq!(
            map_tray_event(&click(MouseButton::Right, MouseButtonState::Up)),
            Some(TrayClick::Right)
        );
        assert_eq!(
            map_tray_event(&click(MouseButton::Left, MouseButtonState::Down)),
            None
        );
    }
}
