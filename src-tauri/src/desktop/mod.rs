pub mod actions;
pub mod confirm;
pub mod exit;
#[cfg(target_os = "macos")]
pub mod instance;
#[cfg(target_os = "macos")]
pub mod launch;
#[cfg(any(target_os = "macos", test))]
pub mod launch_agent;
mod menu;
pub mod popover;
pub mod shell;
pub mod state;
#[cfg(target_os = "macos")]
pub mod terminate;
mod tray;
#[cfg(target_os = "macos")]
pub mod webcrash;

pub use state::MenuState;
pub use tray::build_tray_menu;

use std::path::Path;
use std::sync::Mutex;

use serde::Serialize;
use tauri::{AppHandle, Emitter, Manager, RunEvent, Runtime, State, Window, WindowEvent};

use crate::engine::find_git_root;
use actions::NativeAction;
use menu::build_native_menu;
use tray::GlyphVariant;

pub const MENU_EVENT: &str = "gitpulse-menu";
pub const OPEN_REPO_EVENT: &str = "gitpulse-open-repo";
pub const OPEN_ERROR_EVENT: &str = "gitpulse-open-error";
pub const EXIT_REQUESTED_EVENT: &str = "gitpulse-exit-requested";

#[derive(Default)]
pub struct DesktopState {
    recents: Mutex<Vec<String>>,
    menu_state: Mutex<MenuState>,
    pending_open: Mutex<Option<String>>,
    exit: exit::ExitGate,
    tray_glyph: Mutex<Option<GlyphVariant>>,
    popover: popover::Presence,
}

#[derive(Clone, Serialize)]
pub struct NativeEvent {
    pub id: String,
    pub path: Option<String>,
    pub repo_path: Option<String>,
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
                if exit::begin(app, exit::Origin::Internal) == exit::Begin::Proceed {
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
    // Refresh, switching, copy and appearance stay in the status menu. Reveal and
    // remote website run in the background so Finder or a browser can take over.
    if from_tray
        && !matches!(
            action,
            NativeAction::Refresh
                | NativeAction::ActivateRepo(_)
                | NativeAction::ToggleTheme
                | NativeAction::CopyBranch
                | NativeAction::CopyRepoPath
                | NativeAction::CopyCommit
                | NativeAction::RevealRepo
                | NativeAction::OpenRemote
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
    // A window that has just taken focus is on screen. (Losing focus says
    // nothing: the window may still be showing.)
    if matches!(event, WindowEvent::Focused(true)) && window.label() == "main" {
        crate::workbench::external_changes::note_main_window_on_screen(true);
    }
    if let WindowEvent::CloseRequested { api, .. } = event {
        if request_window_close(window) {
            api.prevent_close();
        }
    }
}

/// Returns whether the native close must be prevented after routing it.
fn request_window_close<R: Runtime>(window: &Window<R>) -> bool {
    if window.label() == popover::LABEL {
        // Through the popover's own hide, so its visibility and idle timer
        // follow a close the way they follow every other dismissal.
        if let Err(error) = popover::hide(window.app_handle()) {
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
            Ok(()) => {
                crate::workbench::external_changes::note_main_window_on_screen(false);
                #[cfg(target_os = "macos")]
                {
                    let state = menu_state(app);
                    apply_dock_policy(
                        app,
                        dock_policy(state.show_status_icon, state.hide_dock_when_closed, false),
                    );
                }
                return true;
            }
            Err(error) => log::warn!(target: "desktop", "Could not hide main window: {error}"),
        }
    }
    if exit::begin(app, exit::Origin::Internal) != exit::Begin::Proceed {
        true
    } else {
        // Before the listener is installed, nobody can answer a quit request.
        // macOS normally leaves the process alive after its last window closes.
        #[cfg(target_os = "macos")]
        app.exit(0);
        false
    }
}

/// Whether a webview may navigate to `url`: the app's own origin only.
///
/// The app is a single page that opens external links through the opener
/// plugin, so no legitimate flow leaves its origin. Anything else (a link
/// that slipped past the opener, a redirect, injected markup) would load
/// remote content into a webview that holds the IPC bridge. `tauri://` is the
/// macOS/Linux scheme, `tauri.localhost` Windows's, and the Vite dev server is
/// admitted in debug builds only.
pub fn navigation_allowed(url: &tauri::Url) -> bool {
    match url.scheme() {
        "tauri" => url.host_str() == Some("localhost"),
        "http" | "https" => {
            url.host_str() == Some("tauri.localhost")
                || (cfg!(debug_assertions)
                    && matches!(url.host_str(), Some("localhost" | "127.0.0.1"))
                    && url.port() == Some(5173))
        }
        "about" => url.as_str() == "about:blank",
        _ => false,
    }
}

/// Registers [`navigation_allowed`] for every webview the app creates, the
/// configured main window included, which has no builder to hang it on.
pub fn navigation_guard<R: Runtime>() -> tauri::plugin::TauriPlugin<R> {
    tauri::plugin::Builder::new("navigation-guard")
        .on_navigation(|webview, url| {
            let allowed = navigation_allowed(url);
            if !allowed {
                log::warn!(
                    target: "desktop",
                    "blocked {} navigation to {}://{}",
                    webview.label(),
                    url.scheme(),
                    url.host_str().unwrap_or("")
                );
            }
            allowed
        })
        .build()
}

/// A reload replaces the page that acknowledged terminal output and the one
/// that confirms quits, while the processes it started keep running here.
/// Both are let go before the new page's script runs.
pub fn handle_page_load<R: Runtime>(
    webview: &tauri::Webview<R>,
    payload: &tauri::webview::PageLoadPayload<'_>,
) {
    if webview.label() != "main"
        || !matches!(payload.event(), tauri::webview::PageLoadEvent::Started)
    {
        return;
    }
    exit::disarm(webview.app_handle(), "the main window's page is reloading");
    let sessions = webview.state::<crate::terminal::TerminalSessions>();
    let detached = crate::terminal::detach_all(&sessions);
    if detached > 0 {
        log::info!(target: "terminal", "page reloaded; {detached} terminal session(s) detached and still running");
    }
}

impl DesktopState {
    /// The quit gate, for checks that must see whether quits are confirmed.
    pub fn exit_gate(&self) -> &exit::ExitGate {
        &self.exit
    }
}

/// `AppHandle::exit` supplies a code: that exit was already approved and is
/// never asked about again. Only a code-less request (the last window closing)
/// goes to the frontend.
fn route_exit_requested<R: Runtime>(app: &AppHandle<R>, code: Option<i32>) -> exit::Begin {
    if code.is_some() {
        exit::Begin::Proceed
    } else {
        exit::begin(app, exit::Origin::Internal)
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
            if route_exit_requested(app, *code) != exit::Begin::Proceed {
                api.prevent_exit();
            } else {
                if let Some(workbench) = app.try_state::<crate::workbench::WorkbenchState>() {
                    workbench.shutdown();
                }
                crate::harness::sidecar::shutdown();
            }
        }
        RunEvent::Exit => {
            // First: a launch from here on must wait for this process to be
            // gone and take over, not hand its request to one that is leaving.
            #[cfg(target_os = "macos")]
            launch::shutdown();
            if let Some(workbench) = app.try_state::<crate::workbench::WorkbenchState>() {
                workbench.shutdown();
            }
            crate::harness::sidecar::shutdown();
        }
        _ => {}
    }
}

/// The repository a launch was asked to open, if its arguments name one.
///
/// `bundle.fileAssociations` claims `.git`, so a launch from Explorer, from a
/// desktop shortcut, or from "Open with GitPulse" arrives as a path in `argv`.
/// Only macOS was reading it — it gets the same request as `RunEvent::Opened`
/// instead, through LaunchServices — so on Windows and Linux opening a
/// repository from the file manager started an instance that ignored the
/// repository it was started for.
///
/// Pure, and separated from the launch it serves, so the argument shapes a real
/// launch produces can be pinned in a test rather than only on a desktop.
///
/// `argv[0]` is the program itself. Flags are skipped rather than stopping the
/// scan: `--background` is passed by the login item and may precede a path.
/// Everything after `--` is taken literally, so a repository whose name begins
/// with a dash is still openable.
pub fn repository_arg(argv: &[String]) -> Option<&str> {
    let mut rest = argv.iter().skip(1);
    let mut literal = false;
    for arg in rest.by_ref() {
        if literal {
            return (!arg.is_empty()).then_some(arg.as_str());
        }
        if arg == "--" {
            literal = true;
            continue;
        }
        if arg.starts_with('-') || arg.is_empty() {
            continue;
        }
        return Some(arg.as_str());
    }
    None
}

/// A launch that found this app already running: adopt its request and go away.
///
/// The second process exits as soon as this returns; everything it was started
/// to do has to happen here, against the instance that stays. Revealing the
/// window is not optional — without it a user who opened a repository from the
/// file manager sees nothing happen at all, because the window that answered is
/// behind whatever they were looking at.
///
/// `cwd` is the second launch's directory, not ours: `gitpulse .` typed in a
/// terminal names the repository the terminal is in.
#[cfg(not(target_os = "macos"))]
pub fn handle_second_instance<R: Runtime>(app: &AppHandle<R>, argv: &[String], cwd: &str) {
    match second_launch(argv) {
        // Reveals the window itself, after queuing the repository.
        SecondLaunch::Open(path) => queue_and_emit_open(app, &launch_path(path, Path::new(cwd))),
        SecondLaunch::Reveal => {
            if let Err(error) = reveal_main(app) {
                log::warn!(target: "desktop", "Could not reveal GitPulse for a second launch: {error}");
            }
        }
        SecondLaunch::Ignore => {}
    }
}

/// A path from another process's command line, made absolute against that
/// process's working directory. One that is already absolute is untouched; an
/// empty `cwd` (the launch could not read its own) leaves it as given.
pub fn launch_path(path: &str, cwd: &Path) -> std::path::PathBuf {
    let path = Path::new(path);
    if path.is_absolute() || cwd.as_os_str().is_empty() {
        path.to_path_buf()
    } else {
        cwd.join(path)
    }
}

/// What a launch handed over through [`instance`] asks of this one. Runs on
/// the main thread: revealing changes the Dock policy, which AppKit only
/// accepts there.
#[cfg(target_os = "macos")]
pub fn answer_forwarded(app: &AppHandle, request: instance::Request) {
    let handle = app.clone();
    let scheduled = app.run_on_main_thread(move || match request {
        instance::Request::Open { paths } => {
            for path in paths {
                queue_and_emit_open(&handle, Path::new(&path));
            }
        }
        instance::Request::Reveal => {
            if let Err(error) = reveal_main(&handle) {
                log::warn!(target: "desktop", "Could not reveal GitPulse for another launch: {error}");
            }
        }
        // Answered on its own connection by the gate attached to the mailbox;
        // it never reaches the sink, and there is nothing here for the window.
        instance::Request::GateCommand { .. } => {}
        instance::Request::Notification { id, snooze } => {
            if let Err(error) = crate::workbench::notifications::forward(&handle, id, snooze) {
                log::warn!(target: "desktop", "A banner response from another launch was not delivered: {error}");
                // The click still asked for GitPulse. Snooze asked for nothing.
                if !snooze {
                    if let Err(error) = reveal_main(&handle) {
                        log::warn!(target: "desktop", "Could not reveal GitPulse for a banner: {error}");
                    }
                }
            }
        }
    });
    if let Err(error) = scheduled {
        log::error!(target: "desktop", "A request from another launch was dropped: {error}");
    }
}

/// What a launch that found this app already running is asking for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SecondLaunch<'a> {
    /// Open this repository, revealing the window on the way.
    Open(&'a str),
    /// Come to the front: someone launched the app that is already running.
    Reveal,
    /// Neither. Answering would take over a screen nobody asked about.
    Ignore,
}

/// Pure so the matrix is pinned in a test rather than on a desktop.
pub fn second_launch(argv: &[String]) -> SecondLaunch<'_> {
    if let Some(path) = repository_arg(argv) {
        return SecondLaunch::Open(path);
    }
    // `--background` is the login item's launch, and it means the opposite of
    // "look at me". The running instance must not raise its window because the
    // session started a second one that was never meant to be seen.
    if argv.iter().any(|arg| arg == "--background") {
        return SecondLaunch::Ignore;
    }
    SecondLaunch::Reveal
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
    {
        apply_dock_policy(app, DockMode::Regular);
        app.show().map_err(|error| error.to_string())?;
    }
    window.show().map_err(|error| error.to_string())?;
    window.unminimize().map_err(|error| error.to_string())?;
    crate::workbench::external_changes::note_main_window_on_screen(true);
    window.set_focus().map_err(|error| error.to_string())
}

/// Whether this host has a Dock icon whose visibility GitPulse can control.
///
/// `apply_dock_policy` below is compiled only for macOS, because
/// `set_activation_policy` exists only there. This constant is what the
/// frontend is told, so it must track the same condition: a reader offered
/// "Hide Dock icon while closed" on a host with no such code gets a switch
/// that silently does nothing. `host_platform` asserts the two agree.
pub const DOCK_HIDING: bool = cfg!(target_os = "macos");

/// Dock visibility for menu-bar-only mode. Pure so MockRuntime tests can pin the matrix.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DockMode {
    Accessory,
    Regular,
}

pub fn dock_policy(show_status_icon: bool, hide_dock_pref: bool, main_visible: bool) -> DockMode {
    if show_status_icon && hide_dock_pref && !main_visible {
        DockMode::Accessory
    } else {
        DockMode::Regular
    }
}

#[cfg(target_os = "macos")]
fn apply_dock_policy<R: Runtime>(app: &AppHandle<R>, mode: DockMode) {
    let policy = match mode {
        DockMode::Accessory => tauri::ActivationPolicy::Accessory,
        DockMode::Regular => tauri::ActivationPolicy::Regular,
    };
    let _ = app.set_activation_policy(policy);
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
    let rebuild_menu = previous
        .repositories
        .iter()
        .map(state::repository_switcher_key)
        .ne(next.repositories.iter().map(state::repository_switcher_key))
        || previous.repositories_hidden != next.repositories_hidden;
    let menu = if rebuild_menu {
        let menu = build_native_menu(app, &recent_menu_entries(app), &next)
            .map_err(|error| error.to_string())?;
        app.set_menu(menu.clone())
            .map_err(|error| error.to_string())?;
        menu
    } else {
        app.menu().ok_or("Native menu is unavailable")?
    };
    menu::apply_presentation(&menu, &next).map_err(|error| error.to_string())?;
    if tray::needs_apply(&previous, &next) {
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
        *crate::supervisor::lock_or_recover(&state.recents) = capped.clone();
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
    state.exit.arm();
}

/// Completes a frontend-approved exit (see `exit::approve`).
#[tauri::command(async)]
pub fn cmd_exit_app(app: AppHandle) {
    exit::approve(&app);
}

/// The frontend acknowledges a quit request, or cancels it.
#[tauri::command(async)]
pub fn cmd_exit_request_answer(app: AppHandle, answer: exit::ExitAnswer) {
    exit::answer(&app, answer);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn navigation_is_limited_to_the_app_origin() {
        let allow = |u: &str| navigation_allowed(&tauri::Url::parse(u).unwrap());
        assert!(allow("tauri://localhost/index.html"));
        assert!(allow("http://tauri.localhost/status.html"));
        assert!(allow("https://tauri.localhost/"));
        assert!(allow("about:blank"));
        for hostile in [
            "https://example.com/",
            "http://evil.test/tauri.localhost",
            "https://tauri.localhost.evil.test/",
            "http://localhost:5173/",
            "tauri://evil/",
            "file:///etc/passwd",
            "data:text/html,<script>1</script>",
            "javascript:alert(1)",
            "blob:https://tauri.localhost/x",
            "about:srcdoc",
        ] {
            // The dev server is admitted in debug builds, which is how tests run.
            if hostile == "http://localhost:5173/" {
                assert_eq!(allow(hostile), cfg!(debug_assertions));
                continue;
            }
            assert!(!allow(hostile), "{hostile} must be refused");
        }
    }

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
        // Settled before its deadline: the mock runtime cannot exit.
        exit::answer(app.handle(), exit::ExitAnswer::Cancel);
    }

    #[test]
    fn a_quit_is_handed_to_the_frontend_once_and_only_while_it_listens() {
        use std::sync::{Arc, Mutex};
        use tauri::Listener;
        let app = tauri::test::mock_builder()
            .manage(DesktopState::default())
            .build(crate::context())
            .unwrap();
        let confirmations = Arc::new(Mutex::new(0));
        let received = Arc::clone(&confirmations);
        app.listen(EXIT_REQUESTED_EVENT, move |_| {
            *received.lock().unwrap() += 1;
        });
        let handle = app.handle();
        let state = app.state::<DesktopState>();
        assert_eq!(
            exit::begin(handle, exit::Origin::Internal),
            exit::Begin::Proceed,
            "before the frontend listens, nobody could answer a held quit"
        );
        assert_eq!(*confirmations.lock().unwrap(), 0);

        cmd_set_exit_guard_ready(app.state::<DesktopState>());
        assert_eq!(
            exit::begin(handle, exit::Origin::Internal),
            exit::Begin::Deferred
        );
        assert_eq!(
            exit::begin(handle, exit::Origin::Internal),
            exit::Begin::AlreadyPending,
            "a repeated quit must not stack a second confirmation"
        );
        assert_eq!(*confirmations.lock().unwrap(), 1);
        exit::answer(handle, exit::ExitAnswer::Seen);
        exit::answer(handle, exit::ExitAnswer::Cancel);
        assert!(state.exit.take().is_none(), "a cancelled quit is settled");

        // A reload or crash takes the listener away: quits go straight through.
        exit::disarm(handle, "test reload");
        assert_eq!(
            exit::begin(handle, exit::Origin::Internal),
            exit::Begin::Proceed
        );
        assert_eq!(*confirmations.lock().unwrap(), 1);
    }

    #[test]
    fn a_launch_names_the_repository_it_was_started_for() {
        let argv = |args: &[&str]| -> Vec<String> {
            std::iter::once("gitpulse.exe")
                .chain(args.iter().copied())
                .map(String::from)
                .collect()
        };
        // The shape Explorer and a desktop shortcut produce.
        assert_eq!(
            repository_arg(&argv(&["C:\\Users\\me\\Code\\app"])),
            Some("C:\\Users\\me\\Code\\app")
        );
        assert_eq!(
            repository_arg(&argv(&["/home/me/code/app"])),
            Some("/home/me/code/app")
        );
        // The login item passes --background, and may pass it before a path.
        assert_eq!(repository_arg(&argv(&["--background"])), None);
        assert_eq!(
            repository_arg(&argv(&["--background", "/r/a"])),
            Some("/r/a")
        );
        // A repository whose name starts with a dash is still openable.
        assert_eq!(
            repository_arg(&argv(&["--", "-weird-name"])),
            Some("-weird-name")
        );
        // Nothing to open: the program name alone, or only flags, or blanks.
        assert_eq!(repository_arg(&argv(&[])), None);
        assert_eq!(repository_arg(&[]), None);
        assert_eq!(repository_arg(&argv(&["", "-x", ""])), None);
        assert_eq!(repository_arg(&argv(&["--"])), None);
        // argv[0] is never the request, however much it looks like a path.
        assert_eq!(repository_arg(&["/opt/gitpulse".into()]), None);
    }

    /// `gitpulse .` from a terminal names the terminal's repository. The
    /// running instance has its own working directory, which is not the one
    /// the request was made in.
    #[test]
    fn a_launch_path_is_resolved_where_the_launch_was_made() {
        let cwd = std::env::temp_dir();
        assert_eq!(launch_path(".", &cwd), cwd.join("."));
        assert_eq!(launch_path("sub/repo", &cwd), cwd.join("sub/repo"));
        let absolute = cwd.join("elsewhere");
        let absolute = absolute.to_str().unwrap();
        assert_eq!(
            launch_path(absolute, Path::new("/ignored")),
            Path::new(absolute)
        );
        // A launch that could not read its own directory sends none; the path
        // is left as given rather than anchored somewhere arbitrary.
        assert_eq!(launch_path("rel", Path::new("")), Path::new("rel"));
    }

    #[test]
    fn a_second_launch_reveals_the_app_unless_it_was_meant_to_stay_hidden() {
        let argv = |args: &[&str]| -> Vec<String> {
            std::iter::once("gitpulse.exe")
                .chain(args.iter().copied())
                .map(String::from)
                .collect()
        };
        assert_eq!(
            second_launch(&argv(&["/r/a"])),
            SecondLaunch::Open("/r/a"),
            "a repository launch opens it"
        );
        assert_eq!(
            second_launch(&argv(&[])),
            SecondLaunch::Reveal,
            "launching the app again asks to see it"
        );
        assert_eq!(
            second_launch(&argv(&["--background"])),
            SecondLaunch::Ignore,
            "the login item must not raise a window nobody asked for"
        );
        assert_eq!(
            second_launch(&argv(&["--background", "/r/a"])),
            SecondLaunch::Open("/r/a"),
            "a repository still wins: it is an explicit request"
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
        let app = tauri::test::mock_builder()
            .manage(DesktopState::default())
            .build(crate::context())
            .unwrap();
        let handle = app.handle();
        assert_eq!(route_exit_requested(handle, None), exit::Begin::Proceed);
        assert_eq!(route_exit_requested(handle, Some(0)), exit::Begin::Proceed);
        cmd_set_exit_guard_ready(app.state::<DesktopState>());
        assert_eq!(
            route_exit_requested(handle, Some(0)),
            exit::Begin::Proceed,
            "an approved exit must never be asked about again"
        );
        assert_eq!(route_exit_requested(handle, None), exit::Begin::Deferred);
        exit::answer(handle, exit::ExitAnswer::Cancel);
    }

    #[test]
    fn dock_policy_hides_only_while_closed_with_status_icon() {
        assert_eq!(dock_policy(true, true, false), DockMode::Accessory);
        assert_eq!(dock_policy(true, true, true), DockMode::Regular);
        assert_eq!(dock_policy(true, false, false), DockMode::Regular);
        assert_eq!(dock_policy(false, true, false), DockMode::Regular);
    }

    #[test]
    fn repository_count_churn_does_not_change_switcher_identity() {
        use state::{repository_switcher_key, MenuRepository};
        let a = MenuRepository {
            path: "/r/a".into(),
            label: "A".into(),
            active: true,
            changed: Some(1),
            conflicts: None,
            busy: false,
        };
        let mut b = a.clone();
        b.changed = Some(9);
        b.conflicts = Some(2);
        b.busy = true;
        assert_eq!(repository_switcher_key(&a), repository_switcher_key(&b));
        b.active = false;
        assert_ne!(repository_switcher_key(&a), repository_switcher_key(&b));
    }
}
