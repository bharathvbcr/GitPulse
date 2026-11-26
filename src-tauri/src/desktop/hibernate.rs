//! Main-window WebView hibernation, lazy `--background` boot helpers, and
//! backend wake-queue state.
//!
//! When `"main"` sits hidden to the status bar past the idle threshold, the
//! frontend flushes unsaved state and invokes [`cmd_main_hibernate`]. Rust
//! captures a bounded [`HibernationSnapshot`] and [`WindowGeometry`], marks
//! the lifecycle `Hibernating`, returns `Ok(true)` so the WebView's IPC call
//! settles cleanly, and then schedules [`finalize_hibernate_destroy`] on the
//! main thread (~150 ms later, cancellable via an episode token if the user
//! reveals the window in the meantime).
//!
//! Before destroying `"main"`, Rust detaches hosted `gitpulse-ptyd` terminal
//! views (keeping shells and AI agents alive), closes child browser panes on
//! macOS, disarms [`super::exit`], and clears `main_ready`. On
//! `WindowEvent::Destroyed` (and after `main.destroy()`), lifecycle moves to
//! `Hibernated`, [`crate::sysload::sampler::set_app_hibernated`] enables App Nap,
//! and [`crate::idle_trim::run_idle_trim`] trims process caches in the
//! background.

use serde::{Deserialize, Serialize};
use std::collections::VecDeque;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU8, Ordering};
use std::sync::Mutex;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};
use tauri::{
    AppHandle, Emitter, Manager, Runtime, WebviewUrl, WebviewWindow, WebviewWindowBuilder,
};

/// Maximum queued events held while `"main"` is hibernated, waking, or booting.
pub const MAX_PENDING_MAIN_EVENTS: usize = 32;
/// Maximum parked browser-pane URLs retained in a [`HibernationSnapshot`].
pub const MAX_PARKED_BROWSER_URLS: usize = 16;
/// Minimum main-window width mirrored from `tauri.conf.json`.
pub const MIN_WINDOW_WIDTH: u32 = 900;
/// Minimum main-window height mirrored from `tauri.conf.json`.
pub const MIN_WINDOW_HEIGHT: u32 = 600;
/// Delay between `cmd_main_hibernate` IPC reply and `main.destroy()` so WebKit
/// never tears down inside its own script-message callback.
const HIBERNATE_DESTROY_DELAY: Duration = Duration::from_millis(150);
/// Minimum pixels of saved window overlap required on a live monitor.
const MIN_MONITOR_OVERLAP_PX: i32 = 80;
const MAX_SURFACE_CHARS: usize = 64;
const MAX_REPO_CHARS: usize = 4096;
const MAX_URL_CHARS: usize = 2048;
const BOOT_TRAY_HINT_FILE: &str = "desktop_tray_boot.json";

/// Lifecycle of the `"main"` window as owned by the desktop shell.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MainWindowLifecycle {
    #[default]
    Visible,
    Hidden,
    Hibernating,
    Hibernated,
    Waking,
}

impl MainWindowLifecycle {
    const fn to_u8(self) -> u8 {
        match self {
            Self::Visible => 0,
            Self::Hidden => 1,
            Self::Hibernating => 2,
            Self::Hibernated => 3,
            Self::Waking => 4,
        }
    }

    const fn from_u8(raw: u8) -> Self {
        match raw {
            0 => Self::Visible,
            1 => Self::Hidden,
            2 => Self::Hibernating,
            3 => Self::Hibernated,
            4 => Self::Waking,
            _ => Self::Visible,
        }
    }

    /// Whether `"main"` is intentionally tearing down or absent due to hibernation.
    pub const fn is_intentional_teardown(self) -> bool {
        matches!(self, Self::Hibernating | Self::Hibernated)
    }

    /// Whether `"main"` cannot currently receive live JS events directly.
    pub const fn is_unavailable_for_live_events(self) -> bool {
        matches!(self, Self::Hibernating | Self::Hibernated | Self::Waking)
    }
}

/// Compact, bounded UI snapshot preserved across `"main"` WebView hibernation.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct HibernationSnapshot {
    #[serde(default)]
    pub surface: String,
    #[serde(default)]
    pub active_repo: Option<String>,
    #[serde(default)]
    pub sidebar_width: Option<f64>,
    #[serde(default)]
    pub has_terminal_dock: bool,
    #[serde(default)]
    pub parked_browser_urls: Vec<String>,
    #[serde(default)]
    pub saved_at_ms: u64,
}

impl HibernationSnapshot {
    /// Returns a sanitized copy with bounded strings, finite sidebar width, and
    /// capped parked browser URLs.
    pub fn bounded(self) -> Self {
        let surface = cap_str(self.surface.trim(), MAX_SURFACE_CHARS);
        let active_repo = self
            .active_repo
            .map(|r| cap_str(r.trim(), MAX_REPO_CHARS))
            .filter(|r| !r.is_empty());
        let sidebar_width = self
            .sidebar_width
            .filter(|w| w.is_finite() && (80.0..=1600.0).contains(w));
        let parked_browser_urls = self
            .parked_browser_urls
            .into_iter()
            .map(|u| cap_str(u.trim(), MAX_URL_CHARS))
            .filter(|u| !u.is_empty())
            .take(MAX_PARKED_BROWSER_URLS)
            .collect();
        Self {
            surface,
            active_repo,
            sidebar_width,
            has_terminal_dock: self.has_terminal_dock,
            parked_browser_urls,
            saved_at_ms: self.saved_at_ms,
        }
    }
}

/// Saved outer window position, inner size & maximized state restored on wake.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct WindowGeometry {
    pub x: i32,
    pub y: i32,
    pub width: u32,
    pub height: u32,
    pub maximized: bool,
}

impl WindowGeometry {
    /// Returns true if the geometry represents a non-degenerate window size and
    /// plausible desktop coordinates.
    pub const fn is_valid(self) -> bool {
        self.width >= MIN_WINDOW_WIDTH
            && self.height >= MIN_WINDOW_HEIGHT
            && self.width <= 16_384
            && self.height <= 16_384
            && self.x.abs() <= 32_768
            && self.y.abs() <= 32_768
    }

    /// Returns true when at least [`MIN_MONITOR_OVERLAP_PX`] of this window
    /// overlaps the given monitor rectangle on both axes.
    pub fn overlaps_monitor(self, mx: i32, my: i32, mw: u32, mh: u32) -> bool {
        let w = i64::from(self.width);
        let h = i64::from(self.height);
        let x0 = i64::from(self.x);
        let y0 = i64::from(self.y);
        let mx0 = i64::from(mx);
        let my0 = i64::from(my);
        let mx1 = mx0 + i64::from(mw);
        let my1 = my0 + i64::from(mh);
        let overlap_x = (x0 + w).min(mx1) - x0.max(mx0);
        let overlap_y = (y0 + h).min(my1) - y0.max(my0);
        overlap_x >= i64::from(MIN_MONITOR_OVERLAP_PX)
            && overlap_y >= i64::from(MIN_MONITOR_OVERLAP_PX)
    }
}

/// Events targeting `"main"` that arrived while the WebView was not ready,
/// hibernated, or waking. Re-emitted via `emit_to("main", ...)` after
/// [`cmd_main_ready`].
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", content = "payload", rename_all = "snake_case")]
pub enum PendingMainEvent {
    OpenRepo(String),
    Browse {
        repo: String,
        url: String,
    },
    AgentKickoff(serde_json::Value),
    Notification {
        id: String,
    },
    Emit {
        event: String,
        payload: serde_json::Value,
    },
}

impl PendingMainEvent {
    /// Converts a queued event into `(tauri_event_name, json_payload)` for
    /// `emit_to("main", ...)`.
    pub fn into_emit_parts(self) -> (String, serde_json::Value) {
        match self {
            Self::OpenRepo(path) => (
                super::OPEN_REPO_EVENT.to_string(),
                serde_json::json!(super::NativeEvent {
                    id: "open-repo".into(),
                    path: Some(path),
                    repo_path: None,
                }),
            ),
            Self::Browse { repo, url } => (
                crate::browser::BROWSE_EVENT.to_string(),
                serde_json::json!(crate::browser::BrowseRequest { repo, url }),
            ),
            Self::AgentKickoff(payload) => (super::AGENT_KICKOFF_EVENT.to_string(), payload),
            Self::Notification { id } => match crate::alerts::native_session_key(&id) {
                Some(key) => (
                    "gitpulse-session-notification-open".to_string(),
                    serde_json::Value::String(key.to_owned()),
                ),
                None => (
                    "workbench-notification-open".to_string(),
                    serde_json::Value::Null,
                ),
            },
            Self::Emit { event, payload } => (event, payload),
        }
    }
}

/// Diagnostic snapshot of idle memory & hibernation state exposed via
/// [`cmd_idle_memory_status`].
#[derive(Debug, Clone, Default, PartialEq, Serialize)]
pub struct IdleMemoryStatus {
    pub mode: crate::tool_config::IdleMemoryMode,
    pub lifecycle: MainWindowLifecycle,
    pub main_ready: bool,
    pub hibernate_count: u64,
    pub last_hibernated_at_ms: Option<u64>,
    pub last_wake_ms: Option<u64>,
    pub pending_event_count: usize,
    pub last_trim_report: Option<crate::idle_trim::IdleTrimReport>,
}

/// Payload returned by [`cmd_main_ready`] once the rebuilt `"main"` page mounts.
#[derive(Debug, Clone, Default, PartialEq, Serialize)]
pub struct WakePayload {
    pub wake_from_hibernation: bool,
    pub snapshot: Option<HibernationSnapshot>,
    pub pending_events: Vec<PendingMainEvent>,
}

#[derive(Default)]
struct HibernationInner {
    snapshot: Option<HibernationSnapshot>,
    geometry: Option<WindowGeometry>,
    pending_events: VecDeque<PendingMainEvent>,
    hibernate_episode: u64,
    wake_from_hibernation: bool,
    wake_after_destroy: bool,
    wake_started_at: Option<Instant>,
    hibernate_count: u64,
    last_hibernated_at_ms: Option<u64>,
    last_wake_ms: Option<u64>,
    last_trim_report: Option<crate::idle_trim::IdleTrimReport>,
}

/// Thread-safe hibernation lifecycle, readiness, geometry, snapshot, and
/// wake-queue state stored inside [`super::DesktopState`].
pub struct HibernationState {
    lifecycle: AtomicU8,
    main_ready: AtomicBool,
    closing_to_quit: AtomicBool,
    inner: Mutex<HibernationInner>,
}

impl Default for HibernationState {
    fn default() -> Self {
        Self {
            lifecycle: AtomicU8::new(MainWindowLifecycle::Visible.to_u8()),
            main_ready: AtomicBool::new(false),
            closing_to_quit: AtomicBool::new(false),
            inner: Mutex::new(HibernationInner::default()),
        }
    }
}

impl HibernationState {
    fn lock_inner(&self) -> std::sync::MutexGuard<'_, HibernationInner> {
        crate::supervisor::lock_or_recover(&self.inner)
    }

    pub fn lifecycle(&self) -> MainWindowLifecycle {
        MainWindowLifecycle::from_u8(self.lifecycle.load(Ordering::Acquire))
    }

    pub fn set_lifecycle(&self, next: MainWindowLifecycle) {
        self.lifecycle.store(next.to_u8(), Ordering::Release);
    }

    pub fn is_main_ready(&self) -> bool {
        self.main_ready.load(Ordering::Acquire)
    }

    pub fn set_main_ready(&self, ready: bool) {
        self.main_ready.store(ready, Ordering::Release);
    }

    /// True while `"main"` is intentionally tearing down (`Hibernating`) or
    /// already torn down (`Hibernated`), used to guard `ExitRequested` and
    /// `webcrash::on_terminated`.
    pub fn is_intentional_teardown(&self) -> bool {
        self.lifecycle().is_intentional_teardown()
    }

    /// True while `"main"` is hibernating, hibernated, or still waking.
    pub fn is_hibernated_or_absent(&self) -> bool {
        self.lifecycle().is_unavailable_for_live_events()
    }

    pub fn hibernate_count(&self) -> u64 {
        self.lock_inner().hibernate_count
    }

    /// Marks that `"main"` is closing to quit the app (not hibernating), so
    /// `route_exit_requested` allows the trailing code-less `ExitRequested`
    /// even after `WindowEvent::Destroyed` transitions lifecycle to absent.
    pub fn mark_closing_to_quit(&self) {
        self.closing_to_quit.store(true, Ordering::Release);
    }

    pub fn is_closing_to_quit(&self) -> bool {
        self.closing_to_quit.load(Ordering::Acquire)
    }

    /// Starts a cancellable hibernation teardown and returns its episode token.
    pub fn begin_hibernating(&self) -> u64 {
        let mut inner = self.lock_inner();
        inner.hibernate_episode = inner.hibernate_episode.saturating_add(1);
        inner.wake_after_destroy = false;
        self.closing_to_quit.store(false, Ordering::Release);
        self.set_lifecycle(MainWindowLifecycle::Hibernating);
        inner.hibernate_episode
    }

    /// Cancels any pending hibernation destroy if the user revealed `"main"`
    /// during the brief IPC-to-destroy window.
    pub fn cancel_hibernating(&self) {
        let mut inner = self.lock_inner();
        inner.hibernate_episode = inner.hibernate_episode.saturating_add(1);
        if self.lifecycle() == MainWindowLifecycle::Hibernating {
            self.set_lifecycle(MainWindowLifecycle::Visible);
        }
    }

    /// Verifies that `episode` is still the active `Hibernating` teardown.
    pub fn matches_hibernating_episode(&self, episode: u64) -> bool {
        let inner = self.lock_inner();
        inner.hibernate_episode == episode && self.lifecycle() == MainWindowLifecycle::Hibernating
    }

    /// Enqueues a pending main-window event, capping the queue at
    /// [`MAX_PENDING_MAIN_EVENTS`] by dropping the oldest entry when full.
    pub fn push_pending(&self, event: PendingMainEvent) {
        let mut inner = self.lock_inner();
        match &event {
            PendingMainEvent::OpenRepo(path) => {
                inner.pending_events.retain(
                    |existing| !matches!(existing, PendingMainEvent::OpenRepo(p) if p == path),
                );
            }
            PendingMainEvent::Notification { id } => {
                inner.pending_events.retain(
                    |existing| !matches!(existing, PendingMainEvent::Notification { id: eid } if eid == id),
                );
            }
            PendingMainEvent::Browse { .. }
            | PendingMainEvent::AgentKickoff(_)
            | PendingMainEvent::Emit { .. } => {}
        }
        while inner.pending_events.len() >= MAX_PENDING_MAIN_EVENTS {
            inner.pending_events.pop_front();
        }
        inner.pending_events.push_back(event);
    }

    /// Prepends an event back to the front of the queue (preserving FIFO order
    /// if `main_ready` turns false mid-replay).
    pub fn push_front_pending(&self, event: PendingMainEvent) {
        let mut inner = self.lock_inner();
        if inner.pending_events.len() < MAX_PENDING_MAIN_EVENTS {
            inner.pending_events.push_front(event);
        }
    }

    /// Drains all queued pending events in FIFO order.
    pub fn drain_pending(&self) -> Vec<PendingMainEvent> {
        self.lock_inner().pending_events.drain(..).collect()
    }

    pub fn pending_len(&self) -> usize {
        self.lock_inner().pending_events.len()
    }

    pub fn snapshot(&self) -> Option<HibernationSnapshot> {
        self.lock_inner().snapshot.clone()
    }

    pub fn set_snapshot(&self, snapshot: HibernationSnapshot) {
        self.lock_inner().snapshot = Some(snapshot.bounded());
    }

    pub fn geometry(&self) -> Option<WindowGeometry> {
        self.lock_inner().geometry
    }

    pub fn set_geometry(&self, geometry: WindowGeometry) {
        if geometry.is_valid() {
            self.lock_inner().geometry = Some(geometry);
        }
    }

    /// Marks the start of a wake from hibernation (or lazy `--background` boot).
    pub fn begin_wake(&self) {
        let mut inner = self.lock_inner();
        inner.hibernate_episode = inner.hibernate_episode.saturating_add(1);
        inner.wake_from_hibernation = true;
        inner.wake_after_destroy = false;
        inner.wake_started_at = Some(Instant::now());
        self.closing_to_quit.store(false, Ordering::Release);
        self.set_main_ready(false);
        self.set_lifecycle(MainWindowLifecycle::Waking);
    }

    /// Records that a reveal arrived after `Hibernated` was marked while the
    /// dying `"main"` window has not yet emitted `WindowEvent::Destroyed`.
    pub fn request_wake_after_destroy(&self) {
        self.lock_inner().wake_after_destroy = true;
    }

    /// Consumes a pending post-destroy wake request.
    pub fn take_wake_after_destroy(&self) -> bool {
        let mut inner = self.lock_inner();
        std::mem::take(&mut inner.wake_after_destroy)
    }

    /// Reverts a failed wake back to [`MainWindowLifecycle::Hibernated`].
    pub fn abort_wake(&self) {
        let mut inner = self.lock_inner();
        inner.wake_started_at = None;
        self.set_main_ready(false);
        self.set_lifecycle(MainWindowLifecycle::Hibernated);
    }

    /// Completes wake (or cold mount) when `"main"` calls [`cmd_main_ready`],
    /// returning `(wake_from_hibernation, snapshot)` and drained pending events.
    pub fn finish_wake(&self, on_screen: bool) -> (WakePayload, Vec<PendingMainEvent>) {
        let mut inner = self.lock_inner();
        let wake_from_hibernation = inner.wake_from_hibernation;
        inner.wake_from_hibernation = false;
        if let Some(started) = inner.wake_started_at.take() {
            inner.last_wake_ms =
                Some(u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX));
        }
        let snapshot = if wake_from_hibernation {
            inner.snapshot.take()
        } else {
            None
        };
        let drained: Vec<PendingMainEvent> = inner.pending_events.drain(..).collect();
        self.set_main_ready(true);
        self.set_lifecycle(if on_screen {
            MainWindowLifecycle::Visible
        } else {
            MainWindowLifecycle::Hidden
        });
        (
            WakePayload {
                wake_from_hibernation,
                snapshot,
                pending_events: Vec::new(),
            },
            drained,
        )
    }

    /// Transitions to [`MainWindowLifecycle::Hibernated`] once `"main"` is
    /// destroyed. Returns `true` on the first transition for this hibernation
    /// cycle (idempotent if called both from `WindowEvent::Destroyed` and
    /// after `main.destroy()`).
    pub fn record_hibernate(&self, at_ms: u64) -> bool {
        let mut inner = self.lock_inner();
        if self.lifecycle() == MainWindowLifecycle::Hibernated {
            return false;
        }
        inner.hibernate_count = inner.hibernate_count.saturating_add(1);
        inner.last_hibernated_at_ms = Some(at_ms);
        self.set_main_ready(false);
        self.set_lifecycle(MainWindowLifecycle::Hibernated);
        true
    }

    pub fn record_trim_report(&self, report: crate::idle_trim::IdleTrimReport) {
        self.lock_inner().last_trim_report = Some(report);
    }

    pub fn status(&self) -> IdleMemoryStatus {
        let inner = self.lock_inner();
        IdleMemoryStatus {
            mode: crate::tool_config::idle_memory_mode(),
            lifecycle: self.lifecycle(),
            main_ready: self.is_main_ready(),
            hibernate_count: inner.hibernate_count,
            last_hibernated_at_ms: inner.last_hibernated_at_ms,
            last_wake_ms: inner.last_wake_ms,
            pending_event_count: inner.pending_events.len(),
            last_trim_report: inner.last_trim_report.clone(),
        }
    }
}

fn cap_str(text: &str, max_chars: usize) -> String {
    match text.char_indices().nth(max_chars) {
        Some((end, _)) => text[..end].to_owned(),
        None => text.to_owned(),
    }
}

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| u64::try_from(d.as_millis()).unwrap_or(u64::MAX))
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
struct BootTrayHint {
    show_status_icon: bool,
    hide_dock_when_closed: bool,
}

static LAST_BOOT_TRAY_HINT: Mutex<Option<BootTrayHint>> = Mutex::new(None);

fn boot_tray_hint_path() -> Option<PathBuf> {
    crate::tool_config::default_config_dir().map(|dir| dir.join(BOOT_TRAY_HINT_FILE))
}

fn read_boot_tray_hint_from(path: &Path) -> Option<(bool, bool)> {
    let raw = fs::read_to_string(path).ok()?;
    let parsed: BootTrayHint = serde_json::from_str(&raw).ok()?;
    Some((parsed.show_status_icon, parsed.hide_dock_when_closed))
}

fn write_boot_tray_hint_to(path: &Path, hint: BootTrayHint) -> Result<(), String> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).map_err(|e| e.to_string())?;
    }
    let json = serde_json::to_string(&hint).map_err(|e| e.to_string())?;
    fs::write(path, json).map_err(|e| e.to_string())
}

/// Reads the persisted tray visibility hint so `--background` login launches can
/// install the tray and stay in the menu bar without booting `"main"`.
pub fn read_boot_tray_hint() -> Option<(bool, bool)> {
    let path = boot_tray_hint_path()?;
    read_boot_tray_hint_from(&path)
}

/// Persists `show_status_icon` & `hide_dock_when_closed` whenever they change.
pub fn persist_boot_tray_hint(show_status_icon: bool, hide_dock_when_closed: bool) {
    let next = BootTrayHint {
        show_status_icon,
        hide_dock_when_closed,
    };
    {
        let guard = crate::supervisor::lock_or_recover(&LAST_BOOT_TRAY_HINT);
        if *guard == Some(next) {
            return;
        }
    }
    let Some(path) = boot_tray_hint_path() else {
        return;
    };
    if write_boot_tray_hint_to(&path, next).is_ok() {
        let mut guard = crate::supervisor::lock_or_recover(&LAST_BOOT_TRAY_HINT);
        *guard = Some(next);
    }
}

/// Builds (or returns) the canonical `"main"` `WebviewWindow` from the merged
/// Tauri window config (preserving macOS `transparent`, `windowEffects`,
/// `backgroundColor`, `Overlay` titlebar, `hiddenTitle`, `trafficLightPosition`,
/// min size 900x600, and Windows `additionalBrowserArgs`).
///
/// Builds hidden (`visible: false`), applies saved physical [`WindowGeometry`]
/// clamped to live monitors so there is no Retina jump, unlocks 120 Hz, and
/// shows the window when `show_after` is `true`.
pub fn build_main_window_with_visibility<R: Runtime>(
    app: &AppHandle<R>,
    wake_from_hibernation: bool,
    show_after: bool,
) -> Result<WebviewWindow<R>, String> {
    if let Some(existing) = app.get_webview_window("main") {
        return Ok(existing);
    }
    if wake_from_hibernation {
        if let Some(desktop) = app.try_state::<super::DesktopState>() {
            desktop.hibernation.begin_wake();
        }
    }
    let url = if wake_from_hibernation {
        WebviewUrl::App("index.html?wake=1".into())
    } else {
        WebviewUrl::App("index.html".into())
    };
    let builder_result = if let Some(mut cfg) = app
        .config()
        .app
        .windows
        .iter()
        .find(|w| w.label == "main")
        .cloned()
    {
        cfg.create = true;
        // Always construct hidden so physical geometry can be applied before first paint.
        cfg.visible = false;
        cfg.url = url;
        cfg.title_bar_style = tauri::TitleBarStyle::Overlay;
        cfg.hidden_title = true;
        cfg.min_width = Some(f64::from(MIN_WINDOW_WIDTH));
        cfg.min_height = Some(f64::from(MIN_WINDOW_HEIGHT));
        // MockRuntime's raw window handle is a dangling pointer, so clearing
        // window_effects outside Wry prevents window_vibrancy from dereferencing it.
        if std::any::TypeId::of::<R>() != std::any::TypeId::of::<tauri::Wry>() {
            cfg.window_effects = None;
        }
        WebviewWindowBuilder::from_config(app, &cfg)
    } else {
        let builder = WebviewWindowBuilder::new(app, "main", url)
            .title("Waylode")
            .visible(false)
            .inner_size(1280.0, 850.0)
            .min_inner_size(f64::from(MIN_WINDOW_WIDTH), f64::from(MIN_WINDOW_HEIGHT))
            .resizable(true)
            .accept_first_mouse(true);
        #[cfg(target_os = "macos")]
        let builder = builder
            .title_bar_style(tauri::TitleBarStyle::Overlay)
            .hidden_title(true)
            .traffic_light_position(tauri::LogicalPosition::new(16.0, 14.0));
        Ok(builder)
    };

    let built = builder_result.and_then(|builder| builder.build());
    let window = match built {
        Ok(window) => window,
        Err(err) => {
            if wake_from_hibernation {
                if let Some(desktop) = app.try_state::<super::DesktopState>() {
                    desktop.hibernation.abort_wake();
                }
            }
            return Err(err.to_string());
        }
    };

    if let Some(geom) = app
        .try_state::<super::DesktopState>()
        .and_then(|desktop| desktop.hibernation.geometry())
    {
        apply_saved_geometry(app, &window, geom);
    }

    super::webview_perf::unlock_window_high_refresh_rate(&window);
    if show_after {
        if let Err(err) = window.show() {
            log::warn!(target: "desktop", "Could not show built main window: {err}");
        }
        if !wake_from_hibernation {
            if let Some(desktop) = app.try_state::<super::DesktopState>() {
                desktop
                    .hibernation
                    .set_lifecycle(MainWindowLifecycle::Visible);
            }
        }
    } else if let Some(desktop) = app.try_state::<super::DesktopState>() {
        desktop
            .hibernation
            .set_lifecycle(MainWindowLifecycle::Hidden);
    }
    Ok(window)
}

/// Builds (or returns) the canonical `"main"` `WebviewWindow` and shows it.
pub fn build_main_window<R: Runtime>(
    app: &AppHandle<R>,
    wake_from_hibernation: bool,
) -> Result<WebviewWindow<R>, String> {
    build_main_window_with_visibility(app, wake_from_hibernation, true)
}

fn apply_saved_geometry<R: Runtime>(
    app: &AppHandle<R>,
    window: &WebviewWindow<R>,
    geom: WindowGeometry,
) {
    if !geom.is_valid() {
        return;
    }
    let Ok(monitors) = app.available_monitors() else {
        return;
    };
    let visible_on_monitor = monitors.iter().any(|monitor| {
        let pos = monitor.position();
        let size = monitor.size();
        geom.overlaps_monitor(pos.x, pos.y, size.width, size.height)
    });
    if !visible_on_monitor {
        return;
    }
    if let Err(err) = window.set_size(tauri::PhysicalSize::new(geom.width, geom.height)) {
        log::debug!(target: "desktop", "Could not restore hibernated window size: {err}");
    }
    if let Err(err) = window.set_position(tauri::PhysicalPosition::new(geom.x, geom.y)) {
        log::debug!(target: "desktop", "Could not restore hibernated window position: {err}");
    }
    if geom.maximized {
        if let Err(err) = window.maximize() {
            log::debug!(target: "desktop", "Could not restore hibernated maximized state: {err}");
        }
    }
}

fn capture_geometry<R: Runtime>(window: &WebviewWindow<R>) -> Option<WindowGeometry> {
    let pos = window.outer_position().ok()?;
    let size = window.inner_size().ok()?;
    let maximized = window.is_maximized().unwrap_or(false);
    let geom = WindowGeometry {
        x: pos.x,
        y: pos.y,
        width: size.width,
        height: size.height,
        maximized,
    };
    geom.is_valid().then_some(geom)
}

/// Validates hibernation preconditions on the main thread, saves geometry and
/// snapshot, and marks `Hibernating`, returning the episode token if accepted.
///
/// Only windows hidden to the tray (`MainWindowLifecycle::Hidden`) may
/// hibernate; visible or minimized windows refuse (`Ok(None)`).
pub fn prepare_hibernate_main<R: Runtime>(
    app: &AppHandle<R>,
    snapshot: HibernationSnapshot,
) -> Result<Option<u64>, String> {
    let menu = super::menu_state(app);
    if !menu.show_status_icon || app.tray_by_id(super::tray::TRAY_ID).is_none() {
        return Ok(None);
    }
    let Some(main) = app.get_webview_window("main") else {
        return Ok(None);
    };
    let Some(desktop) = app.try_state::<super::DesktopState>() else {
        return Ok(None);
    };
    let lifecycle = desktop.hibernation.lifecycle();
    if lifecycle != MainWindowLifecycle::Hidden {
        return Ok(None);
    }
    #[cfg(not(test))]
    if main.is_visible().unwrap_or(false) || main.is_focused().unwrap_or(false) {
        return Ok(None);
    }
    if let Some(geom) = capture_geometry(&main) {
        desktop.hibernation.set_geometry(geom);
    }
    desktop.hibernation.set_snapshot(snapshot);
    let episode = desktop.hibernation.begin_hibernating();
    Ok(Some(episode))
}

/// Completes main-window teardown on the main thread for `episode` (aborts
/// cleanly if `reveal_main` cancelled the episode or the window became visible).
pub fn finalize_hibernate_destroy<R: Runtime>(
    app: &AppHandle<R>,
    episode: u64,
) -> Result<bool, String> {
    let Some(desktop) = app.try_state::<super::DesktopState>() else {
        return Ok(false);
    };
    if !desktop.hibernation.matches_hibernating_episode(episode) {
        return Ok(false);
    }
    let Some(main) = app.get_webview_window("main") else {
        return Ok(false);
    };
    #[cfg(not(test))]
    if main.is_visible().unwrap_or(false) || main.is_focused().unwrap_or(false) {
        desktop.hibernation.cancel_hibernating();
        return Ok(false);
    }

    if let Some(sessions) = app.try_state::<crate::terminal::TerminalSessions>() {
        let detached = crate::terminal::detach_all(&sessions);
        if detached > 0 {
            log::info!(
                target: "terminal",
                "main window hibernating; {detached} terminal session(s) detached and still running"
            );
        }
    }
    #[cfg(target_os = "macos")]
    match crate::browser::close_all() {
        Ok(0) => {}
        Ok(closed) => {
            log::info!(
                target: "browser",
                "main window hibernating; {closed} browser session(s) closed"
            );
        }
        Err(error) => {
            log::warn!(
                target: "browser",
                "main window hibernating; browser sessions were not closed: {error}"
            );
        }
    }

    crate::workbench::external_changes::note_main_window_on_screen(false);
    desktop.hibernation.set_main_ready(false);
    super::exit::disarm(app, "the main window is hibernating");

    if let Err(err) = main.destroy() {
        desktop
            .hibernation
            .set_lifecycle(MainWindowLifecycle::Hidden);
        return Err(format!(
            "Could not destroy main window for hibernation: {err}"
        ));
    }

    complete_hibernated_transition(app);
    Ok(true)
}

/// Marks `"main"` as `Hibernated`, notifies the App Nap sampler, and runs a
/// background `idle_trim` pass. Idempotent when invoked from both
/// `WindowEvent::Destroyed` and [`finalize_hibernate_destroy`].
pub fn complete_hibernated_transition<R: Runtime>(app: &AppHandle<R>) {
    let Some(desktop) = app.try_state::<super::DesktopState>() else {
        return;
    };
    if !desktop.hibernation.record_hibernate(now_ms()) {
        return;
    }
    crate::workbench::external_changes::note_main_window_on_screen(false);
    crate::sysload::sampler::set_app_hibernated(true);

    let trim_app = app.clone();
    if let Err(error) = crate::supervisor::spawn_once("idle-trim", "idle memory trim", move || {
        let report = crate::idle_trim::run_idle_trim(&trim_app);
        if let Some(state) = trim_app.try_state::<super::DesktopState>() {
            state.hibernation.record_trim_report(report);
        }
    }) {
        log::warn!(target: "desktop", "idle trim did not start: {error}");
    }
}

/// Synchronous helper used by tests to run prepare + destroy in one step.
pub fn hibernate_main<R: Runtime>(
    app: &AppHandle<R>,
    snapshot: HibernationSnapshot,
) -> Result<bool, String> {
    let Some(episode) = prepare_hibernate_main(app, snapshot)? else {
        return Ok(false);
    };
    finalize_hibernate_destroy(app, episode)
}

#[tauri::command(async)]
pub async fn cmd_webview_idle_hint(app: AppHandle, low_memory: bool) -> Result<(), String> {
    super::on_main_thread(app, move |app| {
        if let Some(window) = app.get_webview_window("main") {
            super::webview_perf::set_webview_idle_hint(&window, low_memory);
        }
        Ok(())
    })
    .await
}

#[tauri::command(async)]
pub async fn cmd_main_hibernate(
    app: AppHandle,
    snapshot: HibernationSnapshot,
) -> Result<bool, String> {
    if crate::tool_config::idle_memory_mode() == crate::tool_config::IdleMemoryMode::Off {
        return Ok(false);
    }
    let episode = super::on_main_thread(app.clone(), move |app| {
        prepare_hibernate_main(app, snapshot)
    })
    .await?;
    let Some(episode) = episode else {
        return Ok(false);
    };
    let handle = app.clone();
    if let Err(error) = crate::supervisor::spawn_once(
        "main-hibernate-destroy",
        "main webview hibernation",
        move || {
            std::thread::sleep(HIBERNATE_DESTROY_DELAY);
            let on_main = handle.clone();
            if let Err(err) = handle.run_on_main_thread(move || {
                if let Err(error) = finalize_hibernate_destroy(&on_main, episode) {
                    log::warn!(target: "desktop", "main hibernation destroy failed: {error}");
                }
            }) {
                log::warn!(target: "desktop", "could not schedule main hibernation destroy: {err}");
            }
        },
    ) {
        if let Some(desktop) = app.try_state::<super::DesktopState>() {
            desktop.hibernation.cancel_hibernating();
        }
        return Err(format!("could not schedule main hibernation: {error}"));
    }
    Ok(true)
}

#[tauri::command(async)]
pub fn cmd_main_ready(app: AppHandle) -> Result<WakePayload, String> {
    Ok(main_ready(&app))
}

/// What `"main"` saying it is ready does: finishes the wake, and replays on
/// the main thread the events queued while it was hibernated or still
/// mounting. [`cmd_main_ready`] is this for the page; a host that drives
/// another runtime (the mock one the native menu tests use) calls it
/// directly.
pub fn main_ready<R: Runtime>(app: &AppHandle<R>) -> WakePayload {
    let (visible, minimized) = app
        .get_webview_window("main")
        .map(|w| {
            (
                w.is_visible().unwrap_or(true),
                w.is_minimized().unwrap_or(false),
            )
        })
        .unwrap_or((false, false));
    // Only windows truly hidden to the tray (`!visible && !minimized`) stay
    // `Hidden`; minimized windows stay `Visible` so hibernation never drops
    // their Dock thumbnail.
    let tray_hidden = !visible && !minimized;
    let on_screen = visible && !minimized;
    let (payload, drained) = app
        .try_state::<super::DesktopState>()
        .map(|desktop| desktop.hibernation.finish_wake(!tray_hidden))
        .unwrap_or_default();
    crate::sysload::sampler::set_app_hibernated(false);
    crate::workbench::external_changes::note_main_window_on_screen(on_screen);

    if !drained.is_empty() {
        let replay_app = app.clone();
        if let Err(error) = app.run_on_main_thread(move || {
            replay_pending_events(&replay_app, drained);
        }) {
            log::warn!(target: "desktop", "could not replay pending main events: {error}");
        }
    }
    payload
}

fn replay_pending_events<R: Runtime>(app: &AppHandle<R>, events: Vec<PendingMainEvent>) {
    let mut iter = events.into_iter();
    while let Some(event) = iter.next() {
        let Some(desktop) = app.try_state::<super::DesktopState>() else {
            return;
        };
        if !desktop.hibernation.is_main_ready() || app.get_webview_window("main").is_none() {
            // A reload or re-hibernation interrupted replay: re-queue the
            // remaining events in order instead of dropping them.
            for remaining in std::iter::once(event)
                .chain(iter)
                .collect::<Vec<_>>()
                .into_iter()
                .rev()
            {
                desktop.hibernation.push_front_pending(remaining);
            }
            return;
        }
        let (event_name, body) = event.into_emit_parts();
        if let Err(error) = app.emit_to("main", &event_name, body) {
            log::warn!(target: "desktop", "could not replay queued {event_name} to main: {error}");
        }
    }
}

#[tauri::command(async)]
pub fn cmd_idle_memory_status(app: AppHandle) -> Result<IdleMemoryStatus, String> {
    Ok(app
        .try_state::<super::DesktopState>()
        .map(|desktop| desktop.hibernation.status())
        .unwrap_or_default())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::desktop::{route_exit_requested, DesktopState};

    #[test]
    fn lifecycle_transitions_and_intentional_teardown_flags() {
        let state = HibernationState::default();
        assert_eq!(state.lifecycle(), MainWindowLifecycle::Visible);
        assert!(!state.is_main_ready());
        assert!(!state.is_intentional_teardown());
        assert!(!state.is_hibernated_or_absent());

        state.set_lifecycle(MainWindowLifecycle::Hidden);
        assert!(!state.is_intentional_teardown());
        assert!(!state.is_hibernated_or_absent());

        let ep = state.begin_hibernating();
        assert!(state.matches_hibernating_episode(ep));
        assert!(state.is_intentional_teardown());
        assert!(state.is_hibernated_or_absent());

        // Cancelling during the pre-destroy window aborts that episode cleanly.
        state.cancel_hibernating();
        assert!(!state.matches_hibernating_episode(ep));
        assert_eq!(state.lifecycle(), MainWindowLifecycle::Visible);

        let ep2 = state.begin_hibernating();
        assert!(state.matches_hibernating_episode(ep2));
        assert!(state.record_hibernate(1_700_000_000_000));
        // Duplicate call (e.g. WindowEvent::Destroyed + finalize) is idempotent.
        assert!(!state.record_hibernate(1_700_000_000_100));
        assert_eq!(state.lifecycle(), MainWindowLifecycle::Hibernated);
        assert_eq!(state.hibernate_count(), 1);
        assert!(state.is_intentional_teardown());
        assert!(state.is_hibernated_or_absent());

        state.begin_wake();
        assert_eq!(state.lifecycle(), MainWindowLifecycle::Waking);
        assert!(!state.is_intentional_teardown());
        assert!(state.is_hibernated_or_absent());

        state.abort_wake();
        assert_eq!(state.lifecycle(), MainWindowLifecycle::Hibernated);
        assert!(state.is_intentional_teardown());

        state.begin_wake();
        let (wake, drained) = state.finish_wake(true);
        assert!(wake.wake_from_hibernation);
        assert!(wake.pending_events.is_empty());
        assert!(drained.is_empty());
        assert!(state.is_main_ready());
        assert_eq!(state.lifecycle(), MainWindowLifecycle::Visible);
        assert!(!state.is_intentional_teardown());
        assert!(!state.is_hibernated_or_absent());

        // Hidden reload preserves Hidden lifecycle instead of marking Visible.
        let (hidden_wake, _) = state.finish_wake(false);
        assert!(!hidden_wake.wake_from_hibernation);
        assert_eq!(state.lifecycle(), MainWindowLifecycle::Hidden);
    }

    #[test]
    fn wake_queue_caps_dedupes_and_drains_cleanly() {
        let state = HibernationState::default();
        for i in 0..(MAX_PENDING_MAIN_EVENTS + 10) {
            state.push_pending(PendingMainEvent::Browse {
                repo: "/r".into(),
                url: format!("http://127.0.0.1:3000/{i}"),
            });
        }
        assert_eq!(state.pending_len(), MAX_PENDING_MAIN_EVENTS);
        let drained = state.drain_pending();
        assert_eq!(drained.len(), MAX_PENDING_MAIN_EVENTS);
        assert_eq!(
            drained.first(),
            Some(&PendingMainEvent::Browse {
                repo: "/r".into(),
                url: "http://127.0.0.1:3000/10".into(),
            })
        );
        assert_eq!(state.pending_len(), 0);

        state.push_pending(PendingMainEvent::OpenRepo("/r/a".into()));
        state.push_pending(PendingMainEvent::OpenRepo("/r/a".into()));
        state.push_pending(PendingMainEvent::Notification {
            id: "notice-1".into(),
        });
        state.push_pending(PendingMainEvent::Notification {
            id: "notice-1".into(),
        });
        state.push_pending(PendingMainEvent::AgentKickoff(
            serde_json::json!({"repo": "/r/a", "provider": "claude", "prompt": "hi"}),
        ));
        state.push_pending(PendingMainEvent::Emit {
            event: "gitpulse-menu".into(),
            payload: serde_json::json!({"id": "focus-agent:t1"}),
        });
        assert_eq!(state.pending_len(), 4);
        let (payload, drained) = state.finish_wake(true);
        assert!(!payload.wake_from_hibernation);
        assert!(payload.pending_events.is_empty());
        assert_eq!(drained.len(), 4);
        assert_eq!(state.pending_len(), 0);
    }

    #[test]
    fn snapshot_bounding_caps_fields_and_clears_invalid_widths() {
        let snap = HibernationSnapshot {
            surface: "x".repeat(200),
            active_repo: Some("   ".into()),
            sidebar_width: Some(f64::NAN),
            has_terminal_dock: true,
            parked_browser_urls: (0..30).map(|i| format!("http://localhost/{i}")).collect(),
            saved_at_ms: 42,
        }
        .bounded();
        assert_eq!(snap.surface.chars().count(), MAX_SURFACE_CHARS);
        assert_eq!(snap.active_repo, None);
        assert_eq!(snap.sidebar_width, None);
        assert_eq!(snap.parked_browser_urls.len(), MAX_PARKED_BROWSER_URLS);
    }

    #[test]
    fn pending_event_emit_parts_match_expected_channels() {
        let (name, payload) = PendingMainEvent::OpenRepo("/repo".into()).into_emit_parts();
        assert_eq!(name, crate::desktop::OPEN_REPO_EVENT);
        assert_eq!(payload["path"], "/repo");

        let (name, payload) = PendingMainEvent::Notification {
            id: "gitpulse.session.term-1".into(),
        }
        .into_emit_parts();
        assert_eq!(name, "gitpulse-session-notification-open");
        assert_eq!(payload, "term-1");

        let (name, payload) = PendingMainEvent::Notification {
            id: "event-1".into(),
        }
        .into_emit_parts();
        assert_eq!(name, "workbench-notification-open");
        assert_eq!(payload, serde_json::Value::Null);
    }

    #[test]
    fn geometry_validation_and_monitor_overlap_guard_offscreen_windows() {
        let zero = WindowGeometry::default();
        assert!(!zero.is_valid());

        let valid = WindowGeometry {
            x: 100,
            y: 100,
            width: 1280,
            height: 850,
            maximized: false,
        };
        assert!(valid.is_valid());
        assert!(valid.overlaps_monitor(0, 0, 1920, 1080));
        assert!(!valid.overlaps_monitor(2000, 0, 1920, 1080));
    }

    #[test]
    fn route_exit_requested_prevents_exit_while_hibernating_or_hibernated() {
        let app = tauri::test::mock_builder()
            .manage(DesktopState::default())
            .build(crate::context())
            .unwrap();
        let handle = app.handle();
        let desktop = app.state::<DesktopState>();

        assert_eq!(
            route_exit_requested(handle, None),
            super::super::exit::Begin::Proceed,
            "normal unarmed boot allows exit"
        );

        desktop
            .hibernation
            .set_lifecycle(MainWindowLifecycle::Hibernating);
        assert_ne!(
            route_exit_requested(handle, None),
            super::super::exit::Begin::Proceed,
            "code-less ExitRequested during Hibernating must be prevented"
        );
        assert_eq!(
            route_exit_requested(handle, Some(0)),
            super::super::exit::Begin::Proceed,
            "explicit exit code still proceeds even if hibernating"
        );

        desktop
            .hibernation
            .set_lifecycle(MainWindowLifecycle::Hibernated);
        assert_ne!(
            route_exit_requested(handle, None),
            super::super::exit::Begin::Proceed,
            "code-less ExitRequested while Hibernated must be prevented"
        );
    }

    #[test]
    fn hibernate_main_refuses_when_tray_absent_or_window_visible() {
        let app = tauri::test::mock_builder()
            .manage(DesktopState::default())
            .build(crate::context())
            .unwrap();
        let main = WebviewWindowBuilder::new(&app, "main", WebviewUrl::App("index.html".into()))
            .build()
            .unwrap();

        assert!(
            !hibernate_main(app.handle(), HibernationSnapshot::default()).unwrap(),
            "refuses when show_status_icon is false and tray is absent"
        );

        app.state::<DesktopState>()
            .menu_state
            .lock()
            .unwrap()
            .show_status_icon = true;
        assert!(
            !hibernate_main(app.handle(), HibernationSnapshot::default()).unwrap(),
            "refuses when tray icon is absent"
        );

        drop(main);
    }

    #[test]
    fn boot_tray_hint_round_trips_on_disk() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join(BOOT_TRAY_HINT_FILE);
        assert_eq!(read_boot_tray_hint_from(&path), None);
        write_boot_tray_hint_to(
            &path,
            BootTrayHint {
                show_status_icon: true,
                hide_dock_when_closed: false,
            },
        )
        .unwrap();
        assert_eq!(read_boot_tray_hint_from(&path), Some((true, false)));
    }

    #[test]
    fn reveal_main_while_hibernated_window_still_destroying_defers_wake_instead_of_setting_visible()
    {
        let app = tauri::test::mock_builder()
            .manage(DesktopState::default())
            .build(crate::context())
            .unwrap();
        let _dying = WebviewWindowBuilder::new(&app, "main", WebviewUrl::App("index.html".into()))
            .build()
            .unwrap();
        let desktop = app.state::<DesktopState>();
        assert!(desktop.hibernation.record_hibernate(1_700_000_000_000));
        assert_eq!(
            desktop.hibernation.lifecycle(),
            MainWindowLifecycle::Hibernated
        );

        crate::desktop::reveal_main(app.handle()).unwrap();
        assert_ne!(
            desktop.hibernation.lifecycle(),
            MainWindowLifecycle::Visible,
            "reveal_main must not mark a dying Hibernated window Visible before Destroyed fires"
        );
        assert_eq!(
            desktop.hibernation.lifecycle(),
            MainWindowLifecycle::Hibernated
        );
        assert!(desktop.hibernation.take_wake_after_destroy());
    }

    #[test]
    fn destroyed_main_window_never_stays_visible_unless_waking() {
        let app = tauri::test::mock_builder()
            .manage(DesktopState::default())
            .build(crate::context())
            .unwrap();
        let main = WebviewWindowBuilder::new(&app, "main", WebviewUrl::App("index.html".into()))
            .build()
            .unwrap();
        let desktop = app.state::<DesktopState>();

        for initial in [MainWindowLifecycle::Visible, MainWindowLifecycle::Hidden] {
            desktop.hibernation.set_lifecycle(initial);
            desktop.hibernation.set_main_ready(true);
            crate::desktop::handle_window_event(
                &main.as_ref().window(),
                &tauri::WindowEvent::Destroyed,
            );
            assert_eq!(
                desktop.hibernation.lifecycle(),
                MainWindowLifecycle::Hibernated,
                "Destroyed from {initial:?} must transition to Hibernated (absent)"
            );
            assert!(!desktop.hibernation.is_main_ready());
        }

        desktop.hibernation.begin_wake();
        crate::desktop::handle_window_event(
            &main.as_ref().window(),
            &tauri::WindowEvent::Destroyed,
        );
        assert_eq!(
            desktop.hibernation.lifecycle(),
            MainWindowLifecycle::Waking,
            "Destroyed while Waking must preserve Waking"
        );
    }

    #[test]
    fn reveal_main_from_hibernated_absent_leaves_waking_until_finish_wake() {
        let app = tauri::test::mock_builder()
            .manage(DesktopState::default())
            .build(crate::context())
            .unwrap();
        let desktop = app.state::<DesktopState>();
        desktop
            .hibernation
            .set_lifecycle(MainWindowLifecycle::Hibernated);

        crate::desktop::reveal_main(app.handle()).unwrap();
        assert!(app.get_webview_window("main").is_some());
        assert_eq!(
            desktop.hibernation.lifecycle(),
            MainWindowLifecycle::Waking,
            "reveal_main rebuild must keep lifecycle Waking until cmd_main_ready"
        );
        assert!(!desktop.hibernation.is_main_ready());

        let (payload, _) = desktop.hibernation.finish_wake(true);
        assert!(payload.wake_from_hibernation);
        assert_eq!(
            desktop.hibernation.lifecycle(),
            MainWindowLifecycle::Visible
        );
        assert!(desktop.hibernation.is_main_ready());
    }
}
