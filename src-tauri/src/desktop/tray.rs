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

/// Attention when something needs a glance: warning tone, parked op, degraded
/// watch — or an agent waiting on the reader. The last is the one signal here
/// that is not about the repository, and the reason the dot matters most: the
/// tray is what is still on screen when the agent's terminal is not.
pub fn glyph_variant(state: &MenuState) -> GlyphVariant {
    if state.status.tone == "warning"
        || state.status.operation.is_some()
        || state.status.watch_status == "degraded"
        || state.agents_waiting > 0
        || state.pressure.needs_glance()
        || state
            .agent_requests
            .iter()
            .any(|req| req.urgency != "informational")
    {
        GlyphVariant::Attention
    } else {
        GlyphVariant::Plain
    }
}

/// The status item's hover text. The dot says only that something wants a
/// glance; when an agent is waiting, this is where the tray says which kind of
/// something, and how many. Unchanged when nobody is waiting.
pub fn tray_tooltip(state: &MenuState) -> String {
    let mut agents = String::new();
    let waiting_requests: Vec<&super::state::MenuAgentRequest> = state
        .agent_requests
        .iter()
        .filter(|req| req.urgency != "informational")
        .collect();
    let all_waiting = waiting_requests.len() == state.agent_requests.len();
    let waiting_count = if all_waiting {
        state
            .agents_waiting
            .max((waiting_requests.len() as u32).saturating_add(state.agent_requests_hidden))
    } else {
        state.agents_waiting.max(waiting_requests.len() as u32)
    };
    if waiting_count > 0 {
        if waiting_count == 1 {
            agents.push_str("1 agent needs you\n");
        } else {
            agents.push_str(&format!("{waiting_count} agents need you\n"));
        }
        let mut shown = 0u32;
        for req in waiting_requests.iter().take(3) {
            let agent = crate::alerts::text::sanitize(&req.agent_label);
            let repo = crate::alerts::text::sanitize(&req.repo_label);
            let reason = crate::alerts::text::sanitize(&req.reason);
            if repo.is_empty() {
                agents.push_str(&format!("• {agent} — {reason}\n"));
            } else {
                agents.push_str(&format!("• {agent} · {repo} — {reason}\n"));
            }
            shown += 1;
        }
        if shown > 0 && waiting_count > shown {
            agents.push_str(&format!("+{} more waiting\n", waiting_count - shown));
        }
    } else if state.agents_running > 0 || state.agents_informational > 0 {
        let mut parts = Vec::new();
        if state.agents_running > 0 {
            parts.push(format!("{} running", state.agents_running));
        }
        if state.agents_informational > 0 {
            parts.push(format!("{} finished", state.agents_informational));
        }
        agents.push_str(&parts.join(" · "));
        agents.push('\n');
    }
    // Host pressure leads: a paused build or a nearly exhausted Mac is what the
    // reader most needs to find here, and none of it comes from a repository.
    let pressure: String = state
        .pressure
        .tooltip_lines()
        .into_iter()
        .map(|line| format!("{line}\n"))
        .collect();
    // The detail and the summary carry repository and branch names, which a
    // repository controls: each is cleaned to one line, and the newlines here
    // are the tooltip's own.
    format!(
        "GitPulse\n{pressure}{agents}{}\n{}",
        crate::alerts::text::sanitize(&state.tray_detail),
        crate::alerts::text::sanitize(&state.tray_summary.text)
    )
}

/// Whether `next` changes anything the status item shows, so `apply` must run.
///
/// Kept beside `apply` and `tray_tooltip` rather than in `set_menu_state`: a
/// field the tray starts reading is a field this has to compare, and the two
/// drift apart when they live in different files. An agent count that moved
/// from one to two keeps the dot but changes the tooltip, which is exactly
/// the case a glyph-only comparison leaves stale.
pub fn needs_apply(previous: &MenuState, next: &MenuState) -> bool {
    previous.show_status_icon != next.show_status_icon
        || previous.tray_summary != next.tray_summary
        || previous.tray_detail != next.tray_detail
        || previous.tray_details != next.tray_details
        || previous.tray_title != next.tray_title
        || previous.repositories != next.repositories
        || previous.enabled(actions::REFRESH) != next.enabled(actions::REFRESH)
        || previous.enabled(actions::AGENTS) != next.enabled(actions::AGENTS)
        || previous.status.primary_label != next.status.primary_label
        || previous.agents_waiting != next.agents_waiting
        || previous.agent_requests != next.agent_requests
        || previous.agent_requests_hidden != next.agent_requests_hidden
        || previous.agents_running != next.agents_running
        || previous.agents_informational != next.agents_informational
        || previous.pressure != next.pressure
        || glyph_variant(previous) != glyph_variant(next)
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

/// Current GitPulse G-ring + heartbeat + commit-node mark, 4× supersampled then
/// box-filtered into an alpha-only template matching `Logo.svelte`.
fn pulse_icon(variant: GlyphVariant) -> tauri::image::Image<'static> {
    const SIZE: u32 = 36;
    const SUPER: u32 = 4;
    const HI: u32 = SIZE * SUPER;
    // Stroke weight in final pixels. tray-icon hands this 36px canvas to AppKit
    // at an 18pt size, so one final pixel is half a point: 3px is the ~1.5–2pt
    // weight macOS menu-bar glyphs carry.
    const STROKE_PX: u32 = 3;
    let mut hi = vec![0u8; (HI * HI) as usize];
    let stamp_disc = |buf: &mut [u8], cx: f32, cy: f32, radius: i32| {
        let rx = cx.round() as i32;
        let ry = cy.round() as i32;
        let r2 = radius * radius;
        for dy in -radius..=radius {
            for dx in -radius..=radius {
                if dx * dx + dy * dy > r2 {
                    continue;
                }
                let px = rx + dx;
                let py = ry + dy;
                if px < 0 || py < 0 || px >= HI as i32 || py >= HI as i32 {
                    continue;
                }
                buf[(py as u32 * HI + px as u32) as usize] = 255;
            }
        }
    };
    let stroke = (STROKE_PX * SUPER / 2) as i32;
    let cx = 17.5_f32;
    let cy = 18.5_f32;
    let radius = 11.5_f32;
    let scale = radius / 10.5_f32;
    let super_f = SUPER as f32;
    let map_svg = |sx: f32, sy: f32| -> (f32, f32) {
        (
            (cx + (sx - 16.0) * scale) * super_f,
            (cy + (sy - 16.0) * scale) * super_f,
        )
    };

    // Open G arc from commit node (24.04, 9.25) counter-clockwise in SVG y-down to (26.5, 16).
    const ARC_STEPS: usize = 240;
    let theta_start = (-6.75_f32).atan2(8.04_f32);
    let theta_end = -2.0 * std::f32::consts::PI;
    for step in 0..=ARC_STEPS {
        let t = step as f32 / ARC_STEPS as f32;
        let theta = theta_start + (theta_end - theta_start) * t;
        let x = (cx + radius * theta.cos()) * super_f;
        let y = (cy + radius * theta.sin()) * super_f;
        stamp_disc(&mut hi, x, y, stroke);
    }

    // Heartbeat crossbar matching `H22.6 L20.2 21 L16.8 9.9 L14.4 16 H11.8`.
    let crossbar = [
        map_svg(26.5, 16.0),
        map_svg(22.6, 16.0),
        map_svg(20.2, 21.0),
        map_svg(16.8, 9.9),
        map_svg(14.4, 16.0),
        map_svg(11.8, 16.0),
    ];
    for pair in crossbar.windows(2) {
        let (x0, y0) = pair[0];
        let (x1, y1) = pair[1];
        let steps = (x1 - x0).abs().max((y1 - y0).abs()).ceil().max(1.0) as i32;
        for step in 0..=steps {
            let t = step as f32 / steps as f32;
            stamp_disc(&mut hi, x0 + (x1 - x0) * t, y0 + (y1 - y0) * t, stroke);
        }
    }

    // Commit node matching `<circle cx="24.04" cy="9.25" r="2.9"/>`.
    let (node_x, node_y) = map_svg(24.04, 9.25);
    let node_r = (2.8 * super_f).round() as i32;
    stamp_disc(&mut hi, node_x, node_y, node_r);

    if variant == GlyphVariant::Attention {
        // Filled attention dot, top-right, with a one-pixel knock-out ring at final size.
        let dot_cx = (30.5 * super_f).round() as i32;
        let dot_cy = (5.5 * super_f).round() as i32;
        let outer = (3 * SUPER) as i32;
        let ring = SUPER as i32;
        for py in (dot_cy - outer - ring)..=(dot_cy + outer + ring) {
            for px in (dot_cx - outer - ring)..=(dot_cx + outer + ring) {
                if px < 0 || py < 0 || px >= HI as i32 || py >= HI as i32 {
                    continue;
                }
                let d2 = (px - dot_cx) * (px - dot_cx) + (py - dot_cy) * (py - dot_cy);
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
    let text = super::state::menu_text(label);
    MenuItem::with_id(app, format!("tray:{id}"), text, true, None::<&str>)
}

#[derive(Clone, Debug, PartialEq, Eq)]
enum TrayMenuEntry {
    Item { id: String, label: String },
    Separator,
}

fn tray_menu_plan(state: &MenuState) -> Vec<TrayMenuEntry> {
    let mut entries = vec![TrayMenuEntry::Item {
        id: "show".into(),
        label: "Open GitPulse".into(),
    }];
    let waiting_requests: Vec<&super::state::MenuAgentRequest> = state
        .agent_requests
        .iter()
        .filter(|req| req.urgency != "informational")
        .collect();
    let all_waiting = waiting_requests.len() == state.agent_requests.len();
    let total_waiting = if all_waiting {
        state
            .agents_waiting
            .max((waiting_requests.len() as u32).saturating_add(state.agent_requests_hidden))
    } else {
        state.agents_waiting.max(waiting_requests.len() as u32)
    };
    let has_agents = !waiting_requests.is_empty()
        || state.agents_waiting > 0
        || state.agents_running > 0
        || state.agents_informational > 0;
    let show_agents =
        has_agents && (!waiting_requests.is_empty() || state.enabled(actions::AGENTS));
    if show_agents {
        entries.push(TrayMenuEntry::Separator);
        let mut shown = 0u32;
        for req in waiting_requests.iter().take(5) {
            let label = if req.repo_label.is_empty() {
                format!("{} — {}", req.agent_label, req.reason)
            } else {
                format!("{} · {} — {}", req.agent_label, req.repo_label, req.reason)
            };
            entries.push(TrayMenuEntry::Item {
                id: actions::NativeAction::focus_agent_menu_id(&req.id),
                label,
            });
            shown += 1;
        }
        if state.enabled(actions::AGENTS) {
            let remaining = total_waiting.saturating_sub(shown);
            let agents_label = if remaining > 0 && shown > 0 {
                format!("Open Agents (+{remaining} more)…")
            } else if total_waiting > 0 && shown == 0 {
                format!("Open Agents ({total_waiting} waiting)…")
            } else {
                "Open Agents…".into()
            };
            entries.push(TrayMenuEntry::Item {
                id: actions::AGENTS.into(),
                label: agents_label,
            });
        }
    }
    let primary = state.tray_summary.id.as_str();
    let extra = primary != "open"
        && primary != "show"
        && state.enabled(primary)
        && !state.status.primary_label.is_empty();
    let refresh = state.enabled(actions::REFRESH);
    if extra || refresh {
        entries.push(TrayMenuEntry::Separator);
        if extra {
            entries.push(TrayMenuEntry::Item {
                id: primary.into(),
                label: state.status.primary_label.clone(),
            });
        }
        if refresh {
            entries.push(TrayMenuEntry::Item {
                id: actions::REFRESH.into(),
                label: "Refresh".into(),
            });
        }
    }
    if show_agents || extra || refresh {
        entries.push(TrayMenuEntry::Separator);
    }
    entries.push(TrayMenuEntry::Item {
        id: "settings".into(),
        label: "Settings…".into(),
    });
    entries.push(TrayMenuEntry::Item {
        id: "quit".into(),
        label: "Quit GitPulse".into(),
    });
    entries
}

/// Right-click escape hatch plus waiting agent requests and current primary/refresh actions.
pub fn build_tray_menu<R: Runtime>(
    app: &AppHandle<R>,
    state: &MenuState,
) -> tauri::Result<Menu<R>> {
    let menu = Menu::new(app)?;
    for entry in tray_menu_plan(state) {
        match entry {
            TrayMenuEntry::Item { id, label } => {
                menu.append(&tray_item(app, &id, &label)?)?;
            }
            TrayMenuEntry::Separator => {
                menu.append(&PredefinedMenuItem::separator(app)?)?;
            }
        }
    }
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
    let tooltip = tray_tooltip(state);
    let variant = glyph_variant(state);
    // tray-icon's macOS `set_title` ignores `None` outright — it only calls
    // `setTitle:` inside an `if let Some`. Sending `None` to clear the count
    // therefore left the previous number beside the glyph for the rest of the
    // session: switching the pref off, closing the last repository or opening a
    // bare one all stopped updating a count that stayed on screen. An empty
    // title is what actually clears the button, and it is what `None` already
    // means on the other two platforms (GTK maps it to `""`; Windows ignores
    // titles entirely), so this is the value every platform gets.
    let title = crate::alerts::text::sanitize(state.tray_title.as_deref().unwrap_or_default());
    let title = title.as_str();
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
        tray.set_title(Some(title))
            .map_err(|error| error.to_string())?;
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
            tray.set_title(Some(title))
                .map_err(|error| error.to_string())?;
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

    /// Longest run of at-least-half-covered pixels in each column that has ink.
    /// This is the same statistic a screen capture of the menu bar yields, so the
    /// numbers below are comparable with what the glyph actually measures there.
    fn column_thicknesses(icon: &tauri::image::Image<'_>) -> Vec<u32> {
        let size = icon.width();
        let alpha = |x: u32, y: u32| icon.rgba()[((y * size + x) * 4 + 3) as usize];
        let mut runs = Vec::new();
        for x in 0..size {
            let (mut best, mut run) = (0u32, 0u32);
            for y in 0..size {
                if alpha(x, y) >= 128 {
                    run += 1;
                    best = best.max(run);
                } else {
                    run = 0;
                }
            }
            if best > 0 {
                runs.push(best);
            }
        }
        runs.sort_unstable();
        runs
    }

    #[test]
    fn status_glyph_carries_menu_bar_stroke_weight() {
        // The canvas is 36px shown at 18pt, so one pixel is half a point. macOS
        // menu-bar glyphs — and the count digit GitPulse prints beside this one —
        // measure 1.5–2pt, i.e. 3–4px here. A 2px stroke rendered as a hairline
        // that read as a failure to draw the glyph at all.
        for variant in [GlyphVariant::Plain, GlyphVariant::Attention] {
            let runs = column_thicknesses(&pulse_icon(variant));
            assert!(!runs.is_empty(), "{variant:?} glyph must have ink");
            // Round ends taper, so the thinnest single column is not the weight.
            // The quarter-point is: three quarters of the mark is at least this thick.
            let quartile = runs[runs.len() / 4];
            assert!(
                quartile >= 3,
                "{variant:?} stroke is {quartile}px where it is thinnest, \
                 under the 3px (1.5pt) menu-bar weight; runs were {runs:?}"
            );
        }
    }

    #[test]
    fn status_glyph_stays_inside_its_template_box() {
        // A thicker stroke must not push ink off the canvas: tray-icon scales the
        // whole 36px square to 18pt, so a clipped edge is a clipped glyph.
        for variant in [GlyphVariant::Plain, GlyphVariant::Attention] {
            let icon = pulse_icon(variant);
            let size = icon.width();
            let alpha = |x: u32, y: u32| icon.rgba()[((y * size + x) * 4 + 3) as usize];
            for edge in 0..size {
                for (x, y) in [(edge, 0), (edge, size - 1), (0, edge), (size - 1, edge)] {
                    assert_eq!(
                        alpha(x, y),
                        0,
                        "{variant:?} glyph touches the canvas edge at ({x},{y})"
                    );
                }
            }
        }
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
    fn glyph_and_tooltip_reflect_severe_load_a_pause_and_an_unverified_status() {
        use crate::desktop::pressure::HostPressure;
        let calm = MenuState::default();
        assert_eq!(glyph_variant(&calm), GlyphVariant::Plain);
        for (pressure, line) in [
            (
                HostPressure {
                    severe: true,
                    ..Default::default()
                },
                "Memory is critically low",
            ),
            (
                HostPressure {
                    paused: 2,
                    ..Default::default()
                },
                "2 build or test paused by memory protection",
            ),
            (
                HostPressure {
                    unverified: true,
                    ..Default::default()
                },
                "Memory protection status could not be verified",
            ),
        ] {
            let state = MenuState {
                pressure,
                ..MenuState::default()
            };
            assert_eq!(glyph_variant(&state), GlyphVariant::Attention, "{line}");
            assert!(tray_tooltip(&state).contains(line), "{line}");
            // The status item is redrawn for a change in pressure alone.
            assert!(needs_apply(&calm, &state), "{line}");
        }
        assert!(!tray_tooltip(&calm).contains("Memory"));
    }

    #[test]
    fn glyph_variant_shows_agents_waiting_on_a_clean_repository() {
        let mut state = MenuState::default();
        state.status.tone = "clean".into();
        state.status.watch_status = "watching".into();
        assert_eq!(glyph_variant(&state), GlyphVariant::Plain);
        state.agents_waiting = 1;
        assert_eq!(glyph_variant(&state), GlyphVariant::Attention);
        state.agents_waiting = 0;
        assert_eq!(glyph_variant(&state), GlyphVariant::Plain);
    }

    #[test]
    fn the_tooltip_keeps_its_own_lines_and_drops_hidden_text_from_names() {
        let mut state = MenuState {
            tray_detail: "repo\u{202e}gnp.exe · main".into(),
            ..MenuState::default()
        };
        state.tray_summary.text = "Clean\u{2066}\nsecond line".into();
        assert_eq!(
            tray_tooltip(&state),
            "GitPulse\nrepognp.exe · main\nClean second line"
        );
    }

    #[test]
    fn tray_tooltip_names_the_agents_waiting_and_is_unchanged_without_them() {
        let mut state = MenuState {
            tray_detail: "GitPulse · main".into(),
            ..Default::default()
        };
        state.tray_summary.text = "Clean".into();
        assert_eq!(tray_tooltip(&state), "GitPulse\nGitPulse · main\nClean");
        state.agents_waiting = 1;
        assert_eq!(
            tray_tooltip(&state),
            "GitPulse\n1 agent needs you\nGitPulse · main\nClean"
        );
        state.agents_waiting = 2;
        assert_eq!(
            tray_tooltip(&state),
            "GitPulse\n2 agents need you\nGitPulse · main\nClean"
        );
    }

    #[test]
    fn tray_reapplies_when_the_agent_count_moves_under_an_unchanged_dot() {
        let previous = MenuState {
            agents_waiting: 1,
            ..Default::default()
        };
        let mut next = previous.clone();
        assert!(!needs_apply(&previous, &next));
        next.agents_waiting = 2;
        assert_eq!(glyph_variant(&previous), glyph_variant(&next));
        assert!(needs_apply(&previous, &next), "the tooltip count is stale");
        next.agents_waiting = 0;
        assert!(needs_apply(&previous, &next));
    }

    #[test]
    fn status_glyph_draws_open_g_ring_commit_node_and_heartbeat_crossbar() {
        let icon = pulse_icon(GlyphVariant::Plain);
        let alpha = |x: u32, y: u32| icon.rgba()[((y * 36 + x) * 4 + 3) as usize];
        let region_ink = |xs: std::ops::Range<u32>, ys: std::ops::Range<u32>| -> u32 {
            ys.flat_map(|y| xs.clone().map(move |x| alpha(x, y) as u32))
                .sum()
        };
        // Top, left, bottom quadrants of the G-ring, commit node, and heartbeat crossbar have ink.
        assert!(region_ink(14..22, 5..10) > 0, "top G-ring arc");
        assert!(region_ink(4..9, 14..23) > 0, "left G-ring arc");
        assert!(region_ink(14..22, 27..32) > 0, "bottom G-ring arc");
        assert!(region_ink(24..29, 9..14) > 0, "commit node");
        assert!(region_ink(16..22, 11..25) > 0, "heartbeat crossbar");
        // The G opening gap on the upper-right remains open.
        assert_eq!(alpha(30, 14), 0, "G opening gap stays open");
    }

    #[test]
    fn tray_tooltip_and_needs_apply_track_agent_requests_and_quiet_counts() {
        use crate::desktop::state::MenuAgentRequest;
        let req = MenuAgentRequest {
            id: "term:1".into(),
            source: "terminal".into(),
            provider: "claude".into(),
            agent_label: "Claude\u{202e}Code".into(),
            repo_label: "GitPulse".into(),
            repo_path: Some("/r/gp".into()),
            urgency: "blocking".into(),
            reason: "Needs your permission\nnow".into(),
            detail: None,
            at_ms: Some(1_700_000_000_000),
        };
        let mut state = MenuState {
            agents_waiting: 4,
            agent_requests: vec![req.clone()],
            agent_requests_hidden: 3,
            tray_detail: "GitPulse · main".into(),
            ..Default::default()
        };
        state.tray_summary.text = "Clean".into();
        let tip = tray_tooltip(&state);
        assert!(tip.contains("4 agents need you\n"));
        assert!(tip.contains("• ClaudeCode · GitPulse — Needs your permission now\n"));
        assert!(tip.contains("+3 more waiting\n"));

        let mut changed_req = state.clone();
        changed_req.agent_requests[0].reason = "Stopped on an error".into();
        assert!(needs_apply(&state, &changed_req));

        let mut quiet = MenuState {
            agents_running: 2,
            agents_informational: 1,
            tray_detail: "GitPulse · main".into(),
            ..Default::default()
        };
        quiet.tray_summary.text = "Clean".into();
        assert_eq!(glyph_variant(&quiet), GlyphVariant::Plain);
        assert_eq!(
            tray_tooltip(&quiet),
            "GitPulse\n2 running · 1 finished\nGitPulse · main\nClean"
        );
        let mut quiet_more = quiet.clone();
        quiet_more.agents_running = 3;
        assert!(needs_apply(&quiet, &quiet_more));

        let mut info_only_req = MenuState::default();
        info_only_req.agent_requests.push(MenuAgentRequest {
            urgency: "informational".into(),
            ..req
        });
        assert_eq!(glyph_variant(&info_only_req), GlyphVariant::Plain);
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

    #[test]
    fn tray_menu_plan_avoids_double_separators_when_agents_action_disabled() {
        use crate::desktop::state::MenuAgentRequest;
        let state = MenuState {
            agents_running: 1,
            agent_requests: Vec::new(),
            enabled: Vec::new(),
            ..Default::default()
        };
        let entries = tray_menu_plan(&state);
        assert_eq!(
            entries,
            vec![
                TrayMenuEntry::Item {
                    id: "show".into(),
                    label: "Open GitPulse".into()
                },
                TrayMenuEntry::Item {
                    id: "settings".into(),
                    label: "Settings…".into()
                },
                TrayMenuEntry::Item {
                    id: "quit".into(),
                    label: "Quit GitPulse".into()
                },
            ]
        );
        assert!(
            !entries
                .windows(2)
                .any(|w| matches!(w, [TrayMenuEntry::Separator, TrayMenuEntry::Separator])),
            "must never emit consecutive separators"
        );

        let with_agents_enabled = MenuState {
            agents_running: 1,
            enabled: vec![actions::AGENTS.into()],
            ..Default::default()
        };
        assert_eq!(
            tray_menu_plan(&with_agents_enabled),
            vec![
                TrayMenuEntry::Item {
                    id: "show".into(),
                    label: "Open GitPulse".into()
                },
                TrayMenuEntry::Separator,
                TrayMenuEntry::Item {
                    id: actions::AGENTS.into(),
                    label: "Open Agents…".into()
                },
                TrayMenuEntry::Separator,
                TrayMenuEntry::Item {
                    id: "settings".into(),
                    label: "Settings…".into()
                },
                TrayMenuEntry::Item {
                    id: "quit".into(),
                    label: "Quit GitPulse".into()
                },
            ]
        );

        let with_request_and_agents_disabled = MenuState {
            agents_running: 1,
            agent_requests: vec![MenuAgentRequest {
                id: "req-1".into(),
                source: "terminal".into(),
                provider: "claude".into(),
                agent_label: "Claude".into(),
                repo_label: String::new(),
                repo_path: None,
                urgency: "blocking".into(),
                reason: "Needs approval".into(),
                detail: None,
                at_ms: None,
            }],
            enabled: Vec::new(),
            ..Default::default()
        };
        assert_eq!(
            tray_menu_plan(&with_request_and_agents_disabled),
            vec![
                TrayMenuEntry::Item {
                    id: "show".into(),
                    label: "Open GitPulse".into()
                },
                TrayMenuEntry::Separator,
                TrayMenuEntry::Item {
                    id: "focus-agent:req-1".into(),
                    label: "Claude — Needs approval".into()
                },
                TrayMenuEntry::Separator,
                TrayMenuEntry::Item {
                    id: "settings".into(),
                    label: "Settings…".into()
                },
                TrayMenuEntry::Item {
                    id: "quit".into(),
                    label: "Quit GitPulse".into()
                },
            ]
        );
    }

    #[test]
    fn tray_tooltip_and_menu_plan_handle_informational_requests_with_hidden_overflow_consistently()
    {
        use crate::desktop::state::MenuAgentRequest;
        let req = |id: &str, urgency: &str| MenuAgentRequest {
            id: id.into(),
            source: "terminal".into(),
            provider: "claude".into(),
            agent_label: format!("Agent-{id}"),
            repo_label: "GitPulse".into(),
            repo_path: Some("/Projects/GitPulse".into()),
            urgency: urgency.into(),
            reason: format!("Reason-{id}"),
            detail: None,
            at_ms: Some(1_700_000_000_000),
        };

        let info_only = MenuState {
            agents_waiting: 0,
            agents_informational: 1,
            agent_requests: vec![req("i1", "informational")],
            enabled: vec![actions::AGENTS.into()],
            ..Default::default()
        };
        info_only.validate().unwrap();
        assert_eq!(glyph_variant(&info_only), GlyphVariant::Plain);
        let plan = tray_menu_plan(&info_only);
        assert!(
            !plan
                .iter()
                .any(|entry| matches!(entry, TrayMenuEntry::Item { id, .. } if id.starts_with("focus-agent:"))),
            "informational request must not appear as a focus-agent tray action: {plan:?}"
        );

        let mixed = MenuState {
            agents_waiting: 2,
            agent_requests: vec![
                req("b1", "blocking"),
                req("b2", "error"),
                req("i1", "informational"),
                req("i2", "informational"),
                req("i3", "informational"),
                req("i4", "informational"),
                req("i5", "informational"),
                req("i6", "informational"),
            ],
            agent_requests_hidden: 3,
            ..Default::default()
        };
        mixed.validate().unwrap();
        let tip = tray_tooltip(&mixed);
        assert!(
            !(tip.contains("2 agents need you") && tip.contains("+3 more waiting")),
            "tooltip must not simultaneously claim 2 agents need you and +3 more waiting:\n{tip}"
        );
    }
}
