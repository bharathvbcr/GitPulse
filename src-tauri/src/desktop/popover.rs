//! One lightweight status window. The main webview remains the workspace owner.
use super::actions::NativeAction;
use super::{menu_state, DesktopState, MenuState};
use std::sync::{Condvar, Mutex};
use std::time::{Duration, Instant};
use tauri::{
    AppHandle, Emitter, Manager, PhysicalPosition, PhysicalSize, Runtime, WebviewUrl,
    WebviewWindow, WebviewWindowBuilder, WindowEvent,
};

pub const LABEL: &str = "status-popover";
pub const EVENT: &str = "gitpulse-status-state";
/// How long the popover may stay hidden before its webview is destroyed. A
/// hidden WKWebView keeps a whole WebContent process resident; the next open
/// rebuilds it, which costs a moment the reader is already waiting for anyway.
pub const IDLE_DESTROY: Duration = Duration::from_secs(5 * 60);

/// Whether the popover is on screen, and the one idle timer a hidden popover
/// runs. Kept by the code that shows and hides it rather than asked of the
/// window, so the decision to publish or destroy has a single owner.
pub struct Presence {
    idle: Duration,
    phase: Mutex<Phase>,
    changed: Condvar,
}

#[derive(Default)]
struct Phase {
    visible: bool,
    /// An idle timer is running for the current `episode`.
    armed: bool,
    /// Bumped on every show, which is what cancels a timer that is waiting.
    episode: u64,
    /// Webviews destroyed for being idle, since the app started.
    destroyed: u64,
}

impl Default for Presence {
    fn default() -> Self {
        Self::with_idle(IDLE_DESTROY)
    }
}

impl Presence {
    pub fn with_idle(idle: Duration) -> Self {
        Self {
            idle,
            phase: Mutex::new(Phase::default()),
            changed: Condvar::new(),
        }
    }

    fn phase(&self) -> std::sync::MutexGuard<'_, Phase> {
        crate::supervisor::lock_or_recover(&self.phase)
    }

    pub fn visible(&self) -> bool {
        self.phase().visible
    }

    /// How many times the popover's webview has been destroyed for being idle.
    pub fn destroyed(&self) -> u64 {
        self.phase().destroyed
    }

    /// The popover is being shown: any idle timer is cancelled.
    fn showing(&self) {
        let mut phase = self.phase();
        phase.visible = true;
        phase.armed = false;
        phase.episode += 1;
        self.changed.notify_all();
    }

    /// The popover is hidden, or was built and not yet shown. Returns the
    /// episode to time when no timer is running, so hiding twice (focus loss
    /// and a dismiss arrive together) keeps the first timer.
    fn hidden(&self) -> Option<u64> {
        let mut phase = self.phase();
        phase.visible = false;
        if phase.armed {
            return None;
        }
        phase.armed = true;
        Some(phase.episode)
    }

    /// Blocks until `idle` has passed with `episode` still current and hidden
    /// (`true`), or until a show cancels it (`false`).
    fn wait_idle(&self, episode: u64) -> bool {
        let deadline = Instant::now() + self.idle;
        let mut phase = self.phase();
        loop {
            if phase.episode != episode || !phase.armed {
                return false;
            }
            let Some(remaining) = deadline.checked_duration_since(Instant::now()) else {
                return true;
            };
            phase = self
                .changed
                .wait_timeout(phase, remaining)
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .0;
        }
    }

    /// Consumes an expired timer. `false` if the popover was shown since.
    fn expire(&self, episode: u64) -> bool {
        let mut phase = self.phase();
        if phase.visible || !phase.armed || phase.episode != episode {
            return false;
        }
        phase.armed = false;
        true
    }
}
// macOS draws its own shadow outside the material. Its content bounds must
// match the 360px panel, not the browser preview's padded shadow canvas.
const WIDTH: f64 = if cfg!(target_os = "macos") {
    360.0
} else {
    376.0
};
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
    // Keep display selection in the tray's physical coordinate space. On
    // macOS, monitor_from_point delegates to CGDisplayBounds (logical points),
    // so a valid Retina tray position can appear outside every display.
    let monitor = app
        .available_monitors()
        .map_err(|e| e.to_string())?
        .into_iter()
        .find(|monitor| {
            let origin = monitor.position();
            let extent = monitor.size();
            let x = point.x + size.width / 2.0;
            let y = point.y + size.height / 2.0;
            x >= origin.x as f64
                && y >= origin.y as f64
                && x < origin.x as f64 + extent.width as f64
                && y < origin.y as f64 + extent.height as f64
        })
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
        arm_idle_destroy(app);
    }
    Ok(())
}

/// Starts the idle timer of a popover that is not on screen, unless one is
/// already running. When it runs out the webview is destroyed on the main
/// thread, and the next [`toggle`] builds a new one.
fn arm_idle_destroy<R: Runtime>(app: &AppHandle<R>) {
    let Some(state) = app.try_state::<DesktopState>() else {
        return;
    };
    let Some(episode) = state.popover.hidden() else {
        return;
    };
    let handle = app.clone();
    let spawned = crate::supervisor::spawn_once(
        "status-popover-idle",
        "status popover idle destroy",
        move || {
            if !handle.state::<DesktopState>().popover.wait_idle(episode) {
                return;
            }
            let on_main = handle.clone();
            if let Err(error) =
                handle.run_on_main_thread(move || destroy_if_idle(&on_main, episode))
            {
                log::warn!(target: "desktop", "Status popover idle destroy could not run: {error}");
            }
        },
    );
    if let Err(error) = spawned {
        // The popover just stays resident, as it always did; the next hide retries.
        state.popover.expire(episode);
        log::warn!(target: "desktop", "Status popover idle timer did not start: {error}");
    }
}

/// Runs on the main thread, where [`toggle`] also runs, so a show cannot land
/// between the check and the destroy.
fn destroy_if_idle<R: Runtime>(app: &AppHandle<R>, episode: u64) {
    let Some(state) = app.try_state::<DesktopState>() else {
        return;
    };
    if !state.popover.expire(episode) {
        return;
    }
    if let Some(window) = app.get_webview_window(LABEL) {
        match window.destroy() {
            Ok(()) => {
                state.popover.phase().destroyed += 1;
                log::debug!(target: "desktop", "Status popover destroyed after sitting hidden");
            }
            Err(error) => {
                log::warn!(target: "desktop", "Status popover could not be destroyed: {error}");
            }
        }
    }
}

/// Publishes menu state to the popover while it is on screen. A hidden popover
/// is not told: nobody can see it, and the next [`toggle`] sends the state as
/// it is then, so the latest one is all it ever needs.
pub fn publish<R: Runtime>(app: &AppHandle<R>, state: &MenuState) -> Result<(), String> {
    if !app.state::<DesktopState>().popover.visible() {
        return Ok(());
    }
    emit_state(app, state)
}

/// Addressed to the popover alone; a global emit serialises the payload for,
/// and offers it to, every webview.
fn emit_state<R: Runtime>(app: &AppHandle<R>, state: &MenuState) -> Result<(), String> {
    if app.get_webview_window(LABEL).is_some() {
        app.emit_to(LABEL, EVENT, state)
            .map_err(|e| e.to_string())?;
    }
    Ok(())
}

/// The popover window, built hidden if it does not exist.
fn ensure_window<R: Runtime>(app: &AppHandle<R>) -> Result<WebviewWindow<R>, String> {
    let window = if let Some(window) = app.get_webview_window(LABEL) {
        window
    } else {
        let builder = WebviewWindowBuilder::new(app, LABEL, WebviewUrl::App("status.html".into()))
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
            .shadow(true);
        // Not in this crate's unit tests: they run on the mock runtime, which has
        // no NSWindow for the material to be applied to. The material itself is
        // exercised on AppKit by tests/native_status_material.rs.
        #[cfg(all(target_os = "macos", not(test)))]
        let builder = {
            use tauri::window::{Color, Effect, EffectState, EffectsBuilder};
            // Clears WKWebView's under-page backdrop as well as the window's.
            // `transparent` alone leaves that backdrop at WebKit's default,
            // which shows through wherever the page has not been painted.
            builder.background_color(Color(0, 0, 0, 0)).effects(
                EffectsBuilder::new()
                    .effect(Effect::UnderWindowBackground)
                    .state(EffectState::FollowsWindowActiveState)
                    .radius(18.0)
                    .build(),
            )
        };
        // A second toggle arriving between the lookup above and this build
        // would fail on the duplicate label, and the toggle it belongs to would
        // report an error instead of showing the panel the reader asked for.
        // The window that won the race is the one to use.
        match builder.build() {
            Ok(window) => {
                let handle = app.clone();
                window.on_window_event(move |event| {
                    if matches!(event, WindowEvent::Focused(false)) {
                        if let Err(error) = hide(&handle) {
                            log::warn!(target: "desktop", "Status popover dismissal failed: {error}");
                        }
                    }
                });
                // Built hidden. If it is never shown (positioning can fail),
                // the idle timer still reaps it; showing it cancels the timer.
                arm_idle_destroy(app);
                window
            }
            Err(error) => app
                .get_webview_window(LABEL)
                .ok_or_else(|| error.to_string())?,
        }
    };
    Ok(window)
}

pub fn toggle<R: Runtime>(app: &AppHandle<R>) -> Result<(), String> {
    if !menu_state(app).show_status_icon {
        return Ok(());
    }
    if let Some(window) = app.get_webview_window(LABEL) {
        if window.is_visible().map_err(|e| e.to_string())? {
            return hide(app);
        }
    }
    let window = ensure_window(app)?;
    let height = window
        .inner_size()
        .map_err(|e| e.to_string())?
        .to_logical::<f64>(window.scale_factor().map_err(|e| e.to_string())?)
        .height;
    // Preserve the measured panel height on reopen, including short connecting
    // and error states; extra height would expose a strip of native material.
    position(app, &window, height.clamp(100.0, MAX_HEIGHT))?;
    reveal(app, &window)
}

/// Shows the popover with the state as it is now. This is the one publish a
/// hidden popover gets: [`publish`] skipped every change since it was hidden,
/// and a freshly built webview has seen none.
fn reveal<R: Runtime>(app: &AppHandle<R>, window: &WebviewWindow<R>) -> Result<(), String> {
    // Visible first, so a change from here on is published rather than skipped;
    // the state sent below already includes everything before it.
    app.state::<DesktopState>().popover.showing();
    let shown = emit_state(app, &menu_state(app))
        .and_then(|()| window.show().map_err(|e| e.to_string()))
        .and_then(|()| window.set_focus().map_err(|e| e.to_string()));
    if shown.is_err() {
        arm_idle_destroy(app);
    }
    shown
}

fn keeps_popover(id: &str) -> bool {
    matches!(
        id,
        "refresh" | "toggle-theme" | "copy-branch" | "copy-repo-path" | "copy-commit"
    ) || id.starts_with(super::actions::REPOSITORY_PREFIX)
}

fn is_status_allowed(id: &str) -> bool {
    matches!(id, "show" | "settings" | "quit" | "dismiss")
        || matches!(
            NativeAction::parse(id),
            Some(
                NativeAction::Open
                    | NativeAction::Clone
                    | NativeAction::Refresh
                    | NativeAction::Section(_)
                    | NativeAction::TabWork
                    | NativeAction::TabHistory
                    | NativeAction::TabCode
                    | NativeAction::TabInsights
                    | NativeAction::Fleet
                    | NativeAction::Agents
                    | NativeAction::TerminalDock
                    | NativeAction::Palette
                    | NativeAction::CopyBranch
                    | NativeAction::CopyRepoPath
                    | NativeAction::CopyCommit
                    | NativeAction::RevealRepo
                    | NativeAction::OpenRemote
                    | NativeAction::ToggleTheme
                    | NativeAction::Shortcuts
                    | NativeAction::Diagnostics
                    | NativeAction::ActivateRepo(_),
            )
        )
}

fn validate_action(state: &MenuState, id: &str, repo_path: Option<&str>) -> Result<(), String> {
    if matches!(id, "show" | "settings" | "quit" | "dismiss" | "palette") {
        return Ok(());
    }
    if !is_status_allowed(id) || !state.enabled(id) {
        return Err("This status action is unavailable".into());
    }
    let workspace = matches!(
        id,
        "open"
            | "clone"
            | "fleet"
            | "agents"
            | "palette"
            | "toggle-theme"
            | "shortcuts"
            | "diagnostics"
    ) || id.starts_with(super::actions::REPOSITORY_PREFIX);
    if !workspace && state.active_path.as_deref() != repo_path {
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
        if !keeps_popover(&id) {
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
            [1000.0 - WIDTH - 6.0, 30.0, WIDTH, 400.0]
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
        state.enabled.extend([
            "refresh".into(),
            "push".into(),
            "fleet".into(),
            "copy-branch".into(),
            "section:insights:pulse".into(),
            "toggle-theme".into(),
        ]);
        assert!(validate_action(&state, "refresh", Some("/a")).is_ok());
        assert!(validate_action(&state, "refresh", Some("/b")).is_err());
        assert!(validate_action(&state, "push", Some("/a")).is_err());
        assert!(validate_action(&state, "fetch", Some("/a")).is_err());
        assert!(validate_action(&state, "quick-commit", Some("/a")).is_err());
        assert!(validate_action(&state, "fleet", Some("/a")).is_ok());
        assert!(validate_action(&state, "agents", Some("/b")).is_ok());
        assert!(validate_action(&state, "copy-branch", Some("/a")).is_ok());
        assert!(validate_action(&state, "section:insights:pulse", Some("/a")).is_ok());
        assert!(validate_action(&state, "toggle-theme", Some("/a")).is_ok());
        assert!(validate_action(&state, "activate-repo:/missing", Some("/a")).is_err());
        assert!(validate_action(&state, "quit", None).is_ok());
        assert!(validate_action(&state, "clone", None).is_ok());
        assert!(keeps_popover("copy-branch"));
        assert!(keeps_popover("refresh"));
        assert!(!keeps_popover("section:work:overview"));
        assert!(!keeps_popover("reveal-repo"));
    }
}

/// The popover's life: what it is told while hidden, and when its webview goes.
#[cfg(test)]
mod lifecycle_tests {
    use super::*;
    use std::sync::Arc;
    use tauri::test::{mock_builder, MockRuntime};
    use tauri::{Listener, WebviewUrl, WebviewWindowBuilder};

    type Seen = Arc<Mutex<Vec<String>>>;

    fn app(idle: Duration) -> tauri::App<MockRuntime> {
        mock_builder()
            .manage(DesktopState {
                popover: Presence::with_idle(idle),
                ..Default::default()
            })
            .build(crate::context())
            .unwrap()
    }

    /// A `main` window that records every status event it is offered.
    fn main_window(app: &tauri::App<MockRuntime>) -> Seen {
        let main = WebviewWindowBuilder::new(app, "main", WebviewUrl::App("index.html".into()))
            .build()
            .unwrap();
        listening(&main)
    }

    fn listening(window: &WebviewWindow<MockRuntime>) -> Seen {
        let seen: Seen = Arc::default();
        let sink = Arc::clone(&seen);
        window.listen(EVENT, move |event| {
            sink.lock().unwrap().push(event.payload().to_string());
        });
        seen
    }

    fn state_for(path: &str) -> MenuState {
        MenuState {
            active_path: Some(path.into()),
            show_status_icon: true,
            ..Default::default()
        }
    }

    fn set_state(app: &tauri::App<MockRuntime>, state: &MenuState) {
        *app.state::<DesktopState>().menu_state.lock().unwrap() = state.clone();
    }

    fn active_path_of(payload: &str) -> Option<String> {
        serde_json::from_str::<MenuState>(payload)
            .unwrap()
            .active_path
    }

    fn wait_for(within: Duration, done: impl Fn() -> bool) -> bool {
        let deadline = Instant::now() + within;
        while !done() && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(5));
        }
        done()
    }

    #[test]
    fn a_hidden_popover_is_not_published_to_and_opens_on_the_latest_state() {
        let app = app(Duration::from_secs(3600));
        let main = main_window(&app);
        let window = ensure_window(app.handle()).unwrap();
        let popover = listening(&window);

        // A storm of changes while nobody can see the popover.
        for n in 0..50 {
            publish(app.handle(), &state_for(&format!("/repo-{n}"))).unwrap();
        }
        assert_eq!(popover.lock().unwrap().len(), 0, "hidden popover was woken");
        assert_eq!(main.lock().unwrap().len(), 0, "main webview was offered it");

        // Opening sends the state as it is then, once.
        set_state(&app, &state_for("/repo-49"));
        reveal(app.handle(), &window).unwrap();
        let received = popover.lock().unwrap().clone();
        assert_eq!(received.len(), 1, "{received:?}");
        assert_eq!(active_path_of(&received[0]).as_deref(), Some("/repo-49"));

        // While it is open, changes flow, to the popover alone.
        publish(app.handle(), &state_for("/repo-50")).unwrap();
        let received = popover.lock().unwrap().clone();
        assert_eq!(received.len(), 2, "{received:?}");
        assert_eq!(active_path_of(&received[1]).as_deref(), Some("/repo-50"));
        assert_eq!(
            main.lock().unwrap().len(),
            0,
            "main webview got a status event"
        );

        // And stop the moment it is hidden again.
        hide(app.handle()).unwrap();
        publish(app.handle(), &state_for("/repo-51")).unwrap();
        assert_eq!(popover.lock().unwrap().len(), 2);
        assert_eq!(main.lock().unwrap().len(), 0);
    }

    #[test]
    fn a_popover_hidden_past_the_idle_timeout_is_destroyed_once_per_hide_and_shows_again() {
        // The mock runtime never reports a destroyed window back to the
        // manager, so the count of destroys is what is observable here; that
        // the real webview is gone, and rebuilt by `ensure_window` on the next
        // open, needs the AppKit run of tests/native_status_material.rs.
        let app = app(Duration::from_millis(60));
        let window = ensure_window(app.handle()).unwrap();
        let seen = listening(&window);
        let presence = &app.state::<DesktopState>().popover;
        assert_eq!(presence.destroyed(), 0);
        reveal(app.handle(), &window).unwrap();
        assert_eq!(seen.lock().unwrap().len(), 1);

        for round in 0..2u64 {
            hide(app.handle()).unwrap();
            assert!(
                wait_for(Duration::from_secs(5), || presence.destroyed() == round + 1),
                "round {round}: the hidden popover was never destroyed"
            );
            // Once: the timer is spent, not re-armed by the destroy.
            std::thread::sleep(Duration::from_millis(200));
            assert_eq!(presence.destroyed(), round + 1);

            set_state(&app, &state_for(&format!("/after-{round}")));
            reveal(app.handle(), &window).unwrap();
            let received = seen.lock().unwrap().clone();
            assert_eq!(received.len() as u64, round + 2, "{received:?}");
            assert_eq!(
                active_path_of(received.last().unwrap()).as_deref(),
                Some(format!("/after-{round}").as_str())
            );
            assert!(presence.visible());
        }
    }

    #[test]
    fn a_popover_that_was_never_shown_is_reaped_too() {
        // Positioning can fail after the window is built; nothing then shows it.
        let app = app(Duration::from_millis(60));
        ensure_window(app.handle()).unwrap();
        let presence = &app.state::<DesktopState>().popover;
        assert!(wait_for(Duration::from_secs(5), || presence.destroyed() == 1));
    }

    #[test]
    fn showing_the_popover_cancels_its_idle_timer() {
        let app = app(Duration::from_millis(300));
        let window = ensure_window(app.handle()).unwrap();
        let presence = &app.state::<DesktopState>().popover;
        reveal(app.handle(), &window).unwrap();
        // Well past the timeout, still on screen: not destroyed from under it.
        std::thread::sleep(Duration::from_millis(900));
        assert_eq!(presence.destroyed(), 0);
        assert!(presence.visible());

        // Hidden and shown again before the timeout: the old timer must not fire.
        hide(app.handle()).unwrap();
        std::thread::sleep(Duration::from_millis(100));
        reveal(app.handle(), &window).unwrap();
        std::thread::sleep(Duration::from_millis(900));
        assert_eq!(presence.destroyed(), 0);
    }

    #[test]
    fn a_close_request_hides_through_the_same_path_as_every_other_dismissal() {
        let app = app(Duration::from_millis(60));
        let window = ensure_window(app.handle()).unwrap();
        reveal(app.handle(), &window).unwrap();
        assert!(super::super::request_window_close(
            &window.as_ref().window()
        ));
        let presence = &app.state::<DesktopState>().popover;
        assert!(!presence.visible());
        assert!(wait_for(Duration::from_secs(5), || presence.destroyed() == 1));
    }

    #[test]
    fn hiding_twice_keeps_the_first_timer_and_a_show_cancels_it() {
        let presence = Presence::with_idle(Duration::from_millis(200));
        let episode = presence.hidden().expect("first hide arms the timer");
        assert_eq!(
            presence.hidden(),
            None,
            "focus loss and dismiss arrive together"
        );
        presence.showing();
        assert!(!presence.wait_idle(episode), "a show cancels the wait");
        assert!(!presence.expire(episode), "and the expiry");
        let next = presence.hidden().expect("hidden again, armed again");
        assert_ne!(next, episode);
        assert!(presence.wait_idle(next));
        presence.showing();
        assert!(
            !presence.expire(next),
            "shown between the wait and the destroy"
        );
    }
}
