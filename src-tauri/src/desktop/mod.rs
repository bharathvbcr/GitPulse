pub mod accessibility;
pub mod actions;
pub mod confirm;
pub mod editor;
pub mod exit;
pub mod hibernate;
#[cfg(any(target_os = "macos", test))]
pub mod identity_migration;
#[cfg(target_os = "macos")]
pub mod instance;
#[cfg(target_os = "macos")]
pub mod launch;
#[cfg(any(target_os = "macos", test))]
pub mod launch_agent;
mod menu;
pub mod popover;
pub mod pressure;
pub mod shell;
pub mod state;
#[cfg(target_os = "macos")]
pub mod terminate;
mod tray;
#[cfg(target_os = "macos")]
pub mod webcrash;
pub mod webview_perf;

pub use hibernate::{
    cmd_idle_memory_status, cmd_main_hibernate, cmd_main_ready, cmd_webview_idle_hint,
};
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
pub const PEER_INBOX_EVENT: &str = "gitpulse-peer-inbox";
pub const AGENT_KICKOFF_EVENT: &str = "gitpulse-agent-kickoff";

#[derive(Default)]
pub struct DesktopState {
    recents: Mutex<Vec<String>>,
    menu_state: Mutex<MenuState>,
    menu_slot: std::sync::Arc<MenuStateSlot>,
    pending_open: Mutex<Option<String>>,
    exit: exit::ExitGate,
    tray_glyph: Mutex<Option<GlyphVariant>>,
    popover: popover::Presence,
    pub hibernation: hibernate::HibernationState,
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
    let should_reveal = from_tray
        && (!matches!(
            action,
            NativeAction::Refresh
                | NativeAction::ActivateRepo(_)
                | NativeAction::ToggleTheme
                | NativeAction::CopyBranch
                | NativeAction::CopyRepoPath
                | NativeAction::CopyCommit
                | NativeAction::RevealRepo
                | NativeAction::OpenRemote
        ) || app
            .try_state::<DesktopState>()
            .is_some_and(|d| d.hibernation.is_hibernated_or_absent()));
    let event_payload = NativeEvent {
        id: action.event_id().to_string(),
        path: action.path().map(str::to_string),
        repo_path: state.active_path.clone(),
    };
    if let Err(e) = deliver_to_main(app, MENU_EVENT, event_payload, should_reveal) {
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
        if let Some(desktop) = window.app_handle().try_state::<DesktopState>() {
            if !desktop.hibernation.is_hibernated_or_absent() {
                desktop
                    .hibernation
                    .set_lifecycle(hibernate::MainWindowLifecycle::Visible);
            }
        }
    }
    if matches!(event, WindowEvent::Destroyed) && window.label() == "main" {
        let app = window.app_handle();
        if let Some(desktop) = app.try_state::<DesktopState>() {
            desktop.hibernation.set_main_ready(false);
            let lifecycle = desktop.hibernation.lifecycle();
            if lifecycle == hibernate::MainWindowLifecycle::Hibernating {
                hibernate::complete_hibernated_transition(app);
            } else if lifecycle != hibernate::MainWindowLifecycle::Waking
                && lifecycle != hibernate::MainWindowLifecycle::Hibernated
            {
                desktop
                    .hibernation
                    .set_lifecycle(hibernate::MainWindowLifecycle::Hibernated);
                crate::workbench::external_changes::note_main_window_on_screen(false);
            }
            if desktop.hibernation.take_wake_after_destroy() {
                let wake_app = app.clone();
                if let Err(error) = app.run_on_main_thread(move || {
                    if let Err(err) = reveal_main(&wake_app) {
                        log::warn!(target: "desktop", "Could not reveal main window after destroy: {err}");
                    }
                }) {
                    log::warn!(target: "desktop", "Could not schedule post-destroy wake: {error}");
                }
            }
        }
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
                if let Some(desktop) = app.try_state::<DesktopState>() {
                    desktop
                        .hibernation
                        .set_lifecycle(hibernate::MainWindowLifecycle::Hidden);
                }
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
        if let Some(desktop) = app.try_state::<DesktopState>() {
            desktop.hibernation.mark_closing_to_quit();
        }
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

/// Whether `webview_label` may navigate to `url`.
///
/// Extends [`navigation_allowed`] only for the `"main"` webview's sandboxed
/// `<iframe srcdoc>` HTML preview: WKWebView and WebKitGTK invoke the webview's
/// navigation delegate for subframe `about:srcdoc` loads (and in-page
/// `about:srcdoc#<fragment>` jumps), so refusing them leaves `HtmlPreviewViewer`
/// blank. Top-level frames cannot load external content via `about:srcdoc`, the
/// main window's CSP still enforces `script-src 'self'`, and the preview iframe
/// itself is strictly sandboxed (`sandbox="allow-forms"`, no `allow-scripts` or
/// `allow-same-origin`). Non-main webviews (such as the status popover) keep
/// refusing `about:srcdoc`.
pub fn navigation_allowed_for_webview(webview_label: &str, url: &tauri::Url) -> bool {
    if navigation_allowed(url) {
        return true;
    }
    webview_label == "main"
        && (url.as_str() == "about:srcdoc" || url.as_str().starts_with("about:srcdoc#"))
}

/// Registers [`navigation_allowed_for_webview`] for every webview the app
/// creates, the configured main window included, which has no builder to hang
/// it on.
pub fn navigation_guard<R: Runtime>() -> tauri::plugin::TauriPlugin<R> {
    tauri::plugin::Builder::new("navigation-guard")
        .on_navigation(|webview, url| {
            let allowed = navigation_allowed_for_webview(webview.label(), url);
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
    if let Some(desktop) = webview.app_handle().try_state::<DesktopState>() {
        desktop.hibernation.set_main_ready(false);
    }
    exit::disarm(webview.app_handle(), "the main window's page is reloading");
    let sessions = webview.state::<crate::terminal::TerminalSessions>();
    let detached = crate::terminal::detach_all(&sessions);
    if detached > 0 {
        log::info!(target: "terminal", "page reloaded; {detached} terminal session(s) detached and still running");
    }
    // Browser panes are native views the old page laid out. Nothing on the new
    // page knows them, so left open they would sit over it with no tab, no
    // address bar and no way to close them. Scheduled rather than run here so
    // it lands on the main thread wherever this callback is called from.
    #[cfg(target_os = "macos")]
    if let Err(error) = webview.app_handle().run_on_main_thread(|| {
        match crate::browser::close_all() {
            Ok(0) => {}
            Ok(closed) => log::info!(target: "browser", "page reloaded; {closed} browser session(s) closed"),
            Err(error) => log::warn!(target: "browser", "page reloaded; browser sessions were not closed: {error}"),
        }
    }) {
        log::warn!(target: "browser", "page reloaded; closing browser sessions was not scheduled: {error}");
    }
}

impl DesktopState {
    /// The quit gate, for checks that must see whether quits are confirmed.
    pub fn exit_gate(&self) -> &exit::ExitGate {
        &self.exit
    }

    /// True while `"main"` is intentionally tearing down (`Hibernating`) or
    /// already hibernated (`Hibernated`), suppressing crash recovery and
    /// code-less exit teardown.
    pub fn is_hibernating_main(&self) -> bool {
        self.hibernation.is_intentional_teardown()
    }
}

/// `AppHandle::exit` supplies a code: that exit was already approved and is
/// never asked about again. Only a code-less request (the last window closing)
/// goes to the frontend.
pub(crate) fn route_exit_requested<R: Runtime>(
    app: &AppHandle<R>,
    code: Option<i32>,
) -> exit::Begin {
    if code.is_some() {
        return exit::Begin::Proceed;
    }
    if app.try_state::<DesktopState>().is_some_and(|desktop| {
        desktop.is_hibernating_main() && !desktop.hibernation.is_closing_to_quit()
    }) {
        return exit::Begin::Deferred;
    }
    let begin = exit::begin(app, exit::Origin::Internal);
    if begin != exit::Begin::Proceed {
        // The exit is held for a confirmation, so whatever an earlier approval
        // chose for the hosted runs is not for this one.
        crate::harness::hosted::reset_exit_choice();
    }
    begin
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
        instance::Request::GateCommand { .. }
        | instance::Request::CollisionScan { .. }
        | instance::Request::InsightsRead { .. }
        | instance::Request::TerminalOrchestrate { .. }
        | instance::Request::AgentDeliver { .. }
        | instance::Request::RecordGitOp { .. }
        | instance::Request::RecordTurnCheckpoint { .. } => {}
        // An agent's `gitpulse_browser_open`. The window opens the repository
        // and its dock's browser tab (`browseRequests.ts`); the request was
        // validated (absolute repository, loopback URL) before it got here.
        instance::Request::Browse { repo, url } => {
            if should_queue_for_main(&handle) {
                handle
                    .state::<DesktopState>()
                    .hibernation
                    .push_pending(hibernate::PendingMainEvent::Browse { repo, url });
                if let Err(error) = reveal_main(&handle) {
                    log::warn!(target: "desktop", "Could not reveal GitPulse for a browser session: {error}");
                }
            } else {
                if let Err(error) = reveal_main(&handle) {
                    log::warn!(target: "desktop", "Could not reveal GitPulse for a browser session: {error}");
                }
                let request = crate::browser::BrowseRequest { repo, url };
                if let Err(error) = handle.emit(crate::browser::BROWSE_EVENT, request) {
                    log::warn!(target: "browser", "A browser session request was not delivered to the window: {error}");
                }
            }
        }
        instance::Request::AgentSessionMeta {
            terminal_id,
            agent,
            conversation_id,
            artifact_dir,
        } => {
            let sessions = handle.state::<crate::terminal::TerminalSessions>();
            if crate::terminal::host::client::registry_holds(&sessions, &terminal_id) != Some(true)
            {
                log::debug!(
                    target: "terminal",
                    "ignoring agent session metadata for unknown terminal {terminal_id}"
                );
                return;
            }
            sessions.record_agent_meta(
                &terminal_id,
                crate::terminal::AgentSessionMeta {
                    agent: agent.clone(),
                    conversation_id: conversation_id.clone(),
                    artifact_dir: artifact_dir.clone(),
                },
            );
            sessions.session_ledger().emit_view(&handle);
            let payload = crate::terminal::TerminalAgentMetaPayload {
                id: terminal_id,
                agent,
                conversation_id,
                artifact_dir,
            };
            if let Err(error) = handle.emit(crate::terminal::AGENT_META_EVENT, payload) {
                log::warn!(
                    target: "terminal",
                    "could not emit terminal-agent-meta event: {error}"
                );
            }
        }
        instance::Request::AgentTouchedFile {
            terminal_id,
            repo,
            path,
            artifact,
        } => {
            let sessions = handle.state::<crate::terminal::TerminalSessions>();
            if crate::terminal::host::client::registry_holds(&sessions, &terminal_id) != Some(true)
            {
                log::debug!(
                    target: "terminal",
                    "ignoring touched-file event for unknown terminal {terminal_id}"
                );
                return;
            }
            let payload = crate::terminal::TerminalAgentTouchedFilePayload {
                id: terminal_id,
                repo_path: repo,
                path,
                artifact,
            };
            if let Err(error) = handle.emit(crate::terminal::AGENT_TOUCHED_FILE_EVENT, payload) {
                log::warn!(
                    target: "terminal",
                    "could not emit terminal-agent-touched-file event: {error}"
                );
            }
        }
        instance::Request::Notification { id, snooze } => {
            let main_was_live = !should_queue_for_main(&handle);
            let forwarded = crate::workbench::notifications::forward(&handle, id.clone(), snooze);
            if let Err(ref error) = forwarded {
                log::warn!(target: "desktop", "A banner response from another launch was not delivered: {error}");
                if !snooze && !main_was_live {
                    handle
                        .state::<DesktopState>()
                        .hibernation
                        .push_pending(hibernate::PendingMainEvent::Notification { id });
                }
            }
            // The click still asked for GitPulse (including waking a hibernated
            // window). Snooze asked for nothing.
            if !snooze && (forwarded.is_err() || !main_was_live) {
                if let Err(error) = reveal_main(&handle) {
                    log::warn!(target: "desktop", "Could not reveal GitPulse for a banner: {error}");
                }
            }
        }
    });
    if let Err(error) = scheduled {
        log::error!(target: "desktop", "A request from another launch was dropped: {error}");
    }
}

/// Answers an MCP process's [`instance::Request::AgentDeliver`] within the 4s
/// [`instance::HANDLER_TIMEOUT`]: re-verifies repository trust, wakes an idle
/// hosted terminal tab (`TerminalSession.svelte`), forwards a peer turn to a
/// parked Managed run (`managed_run::deliver_peer_follow_up`), or spawns an
/// unattended kickoff session (`gitpulse_kickoff_agent`).
#[cfg(target_os = "macos")]
pub fn serve_agent_deliver<R: tauri::Runtime>(
    app: &tauri::AppHandle<R>,
    request: &instance::Request,
) -> Result<serde_json::Value, String> {
    let instance::Request::AgentDeliver {
        repo,
        peer_id,
        msg_id,
        spawn,
        provider,
        branch,
        prompt,
        task_id,
        ..
    } = request
    else {
        return Err("not an agent deliver request".into());
    };
    crate::repository_trust::require(Path::new(repo)).map_err(|e| e.message().to_string())?;

    if let Some(mode) = spawn.as_deref() {
        return serve_kickoff_spawn(
            app,
            repo,
            msg_id,
            mode,
            provider.as_deref(),
            branch.as_deref(),
            prompt.as_deref(),
            task_id.as_deref(),
        );
    }

    let peers = crate::engine::agent_bus::list_reconciled_peers(repo)
        .map_err(|e| e.message().to_string())?;
    let Some(peer) = peers.into_iter().find(|p| p.record.peer_id == *peer_id) else {
        return Ok(serde_json::json!({
            "ok": true,
            "delivery_outcome": "queued_for_hook_boundary",
            "channel": "mailbox",
        }));
    };

    let msg = crate::engine::agent_bus::get_message_by_id(repo, msg_id)
        .ok()
        .flatten();
    if msg
        .as_ref()
        .is_some_and(|m| m.hop.unwrap_or(0) >= m.max_hops.unwrap_or(6))
    {
        return Ok(serde_json::json!({
            "ok": true,
            "delivery_outcome": "paused_hop_limit",
            "channel": "mailbox",
        }));
    }

    if peer.liveness != "live" {
        let outcome = if peer.liveness == "resumable" {
            "requires_resume"
        } else {
            "queued_for_hook_boundary"
        };
        return Ok(serde_json::json!({
            "ok": true,
            "delivery_outcome": outcome,
            "channel": "mailbox",
        }));
    }

    if peer.record.surface == "managed" {
        if let (Some(run_id), Some(wb), Some(m)) = (
            peer.record.run_id.clone(),
            app.try_state::<crate::workbench::WorkbenchState>()
                .map(|s| s.inner().clone()),
            msg.as_ref(),
        ) {
            let formatted = crate::hooks::deliver::format_peer_xml(
                msg_id,
                &m.from_agent,
                "peer",
                m.branch.as_deref().unwrap_or(""),
                m.thread_key.as_deref().unwrap_or(""),
                m.hop.unwrap_or(1),
                m.max_hops.unwrap_or(6),
                &m.body,
            );
            let req_id: String = msg_id
                .chars()
                .map(|c| {
                    if c.is_ascii_alphanumeric() || c == '-' || c == '_' {
                        c
                    } else {
                        '_'
                    }
                })
                .take(64)
                .collect();
            let (tx, rx) = std::sync::mpsc::sync_channel(1);
            let run_id_clone = run_id.clone();
            let _ = crate::supervisor::spawn_one_of_many(
                "agent-deliver-managed",
                "agent-deliver",
                move || {
                    let res = crate::workbench::managed_run::deliver_peer_follow_up(
                        &wb,
                        &run_id_clone,
                        &req_id,
                        &formatted,
                    );
                    let _ = tx.send(res);
                },
            );
            if let Ok(Ok(ans)) = rx.recv_timeout(std::time::Duration::from_millis(2800)) {
                if ans["state"] == "delivered" {
                    let _ = crate::engine::agent_bus::ack_messages(
                        repo,
                        &peer.record.peer_id,
                        std::slice::from_ref(msg_id),
                    );
                    if let (Some(sid), Ok(mb)) = (
                        peer.record.provider_session_id.as_deref(),
                        crate::agent_mailbox::Mailbox::at_default(),
                    ) {
                        let _ = mb.mark_claimed_by_id(sid, msg_id, std::time::SystemTime::now());
                    }
                    return Ok(serde_json::json!({
                        "ok": true,
                        "delivery_outcome": "woken_immediately",
                        "channel": "managed",
                    }));
                }
            }
            return Ok(serde_json::json!({
                "ok": true,
                "delivery_outcome": "queued_for_hook_boundary",
                "channel": "mailbox",
            }));
        }
    }

    if peer.record.provider == "agy" && peer.record.turn_state != "working" {
        if let Some(tid) = peer.record.terminal_session_id.as_deref() {
            let main_live = !should_queue_for_main(app) && app.get_webview_window("main").is_some();
            let agy_foreground = main_live
                && app
                    .try_state::<crate::terminal::TerminalSessions>()
                    .and_then(|ts| crate::terminal::session_context(ts.inner(), tid).ok())
                    .is_some_and(|ctx| {
                        crate::terminal::is_agy_foreground_process(ctx.process.as_deref())
                    });
            if agy_foreground {
                let _ = app.emit(
                    PEER_INBOX_EVENT,
                    serde_json::json!({
                        "session_id": peer.record.terminal_session_id,
                        "provider_session_id": peer.record.provider_session_id,
                        "peer_id": peer.record.peer_id,
                        "from_peer": msg.as_ref().map(|m| m.from_agent.as_str()),
                        "msg_id": msg_id,
                        "repo": repo,
                    }),
                );
                let outcome = if peer.record.auto_wake && peer.record.turn_state == "idle" {
                    "woken_immediately"
                } else {
                    "awaiting_banner_confirm"
                };
                return Ok(serde_json::json!({
                    "ok": true,
                    "delivery_outcome": outcome,
                    "channel": "banner",
                }));
            }
        }
    }

    if peer.record.provider == "claude" {
        if let Some(sid) = peer.record.provider_session_id.as_deref() {
            let listening = crate::agent_mailbox::Mailbox::at_default()
                .ok()
                .is_some_and(|mb| mb.listening(sid, std::time::SystemTime::now()));
            if listening {
                return Ok(serde_json::json!({
                    "ok": true,
                    "delivery_outcome": "woken_immediately",
                    "channel": "mailbox",
                }));
            }
        }
    }

    Ok(serde_json::json!({
        "ok": true,
        "delivery_outcome": "queued_for_hook_boundary",
        "channel": "mailbox",
    }))
}

#[cfg(target_os = "macos")]
#[allow(clippy::too_many_arguments)]
fn serve_kickoff_spawn<R: tauri::Runtime>(
    app: &tauri::AppHandle<R>,
    repo: &str,
    request_id: &str,
    mode: &str,
    provider: Option<&str>,
    branch: Option<&str>,
    prompt: Option<&str>,
    task_id: Option<&str>,
) -> Result<serde_json::Value, String> {
    let chosen_provider = provider.unwrap_or("claude");
    if !matches!(chosen_provider, "claude" | "agy" | "codex") {
        return Err(format!("unsupported kickoff provider '{chosen_provider}'"));
    }
    match mode {
        "terminal" => {
            let prompt_text = prompt
                .map(str::trim)
                .filter(|p| !p.is_empty())
                .ok_or("terminal kickoff spawn requires a non-empty prompt")?;
            let payload = serde_json::json!({
                "repo": repo,
                "provider": chosen_provider,
                "branch": branch,
                "prompt": prompt_text,
                "auto_wake": true,
            });
            let main_was_live = !should_queue_for_main(app);
            let outcome = if main_was_live {
                app.emit(AGENT_KICKOFF_EVENT, payload)
                    .map_err(|e| format!("could not emit kickoff request: {e}"))?;
                "woken_immediately"
            } else {
                if let Some(desktop) = app.try_state::<DesktopState>() {
                    desktop
                        .hibernation
                        .push_pending(hibernate::PendingMainEvent::AgentKickoff(payload));
                }
                let wake_handle = app.clone();
                if let Err(error) = app.run_on_main_thread(move || {
                    if let Err(err) = reveal_main(&wake_handle) {
                        log::warn!(target: "desktop", "Could not reveal GitPulse for agent kickoff: {err}");
                    }
                }) {
                    log::warn!(target: "desktop", "Could not schedule wake for agent kickoff: {error}");
                }
                "queued_for_main_wake"
            };
            Ok(serde_json::json!({
                "ok": true,
                "spawn": "terminal",
                "provider": chosen_provider,
                "auto_wake": true,
                "delivery_outcome": outcome,
            }))
        }
        "managed" => {
            let tid = task_id
                .map(str::trim)
                .filter(|t| !t.is_empty())
                .ok_or("managed kickoff spawn requires task_id")?;
            let wb = app
                .try_state::<crate::workbench::WorkbenchState>()
                .map(|s| s.inner().clone())
                .ok_or("workbench state is unavailable")?;
            let task = wb
                .request("items.get", &serde_json::json!({ "id": tid }).to_string())
                .map_err(|e| format!("{}: {}", e.code, e.message))?;
            let source_revision = task["item"]["revision"]
                .as_u64()
                .ok_or("task item has no revision")?;
            let repo_id = task["item"]["primary_repository_id"]
                .as_str()
                .filter(|s| !s.is_empty())
                .ok_or("task item has no primary_repository_id")?
                .to_string();
            let repo_row = wb
                .request(
                    "repositories.get",
                    &serde_json::json!({ "id": repo_id }).to_string(),
                )
                .map_err(|e| format!("{}: {}", e.code, e.message))?;
            let repo_revision = repo_row["item"]["revision"]
                .as_u64()
                .ok_or("repository has no revision")?;
            let clean_req: String = request_id
                .chars()
                .map(|c| {
                    if c.is_ascii_alphanumeric() || c == '-' || c == '_' {
                        c
                    } else {
                        '_'
                    }
                })
                .take(48)
                .collect();
            let run_id = format!("run_{clean_req}");
            let prepare_body = serde_json::json!({
                "id": run_id,
                "request_id": format!("prep_{clean_req}"),
                "task_id": tid,
                "source_revision": source_revision,
                "repository_id": repo_id,
                "repository_revision": repo_revision,
                "provider": chosen_provider,
                "permission_mode": "inspect",
                "repo_path": repo,
            });
            wb.request("runs.prepare_managed", &prepare_body.to_string())
                .map_err(|e| format!("{}: {}", e.code, e.message))?;
            let launch_id = run_id.clone();
            crate::supervisor::spawn_one_of_many(
                "agent-kickoff-managed",
                "agent-kickoff",
                move || {
                    let launch_body = serde_json::json!({
                        "id": launch_id,
                        "max_continuations": 4,
                    });
                    if let Err(err) = wb.request("runs.launch_managed", &launch_body.to_string()) {
                        log::warn!(
                            target: "workbench",
                            "managed kickoff launch {} failed: {}: {}",
                            launch_id,
                            err.code,
                            err.message
                        );
                    }
                },
            )
            .map_err(|e| format!("could not launch managed kickoff worker: {e}"))?;
            Ok(serde_json::json!({
                "ok": true,
                "spawn": "managed",
                "provider": chosen_provider,
                "run_id": run_id,
                "delivery_outcome": "woken_immediately",
            }))
        }
        other => Err(format!("unsupported spawn surface '{other}'")),
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

/// Whether `"main"` exists, is not hibernating/hibernated/waking, and has
/// registered its JS listeners via `cmd_main_ready`.
pub fn is_main_ready<R: Runtime>(app: &AppHandle<R>) -> bool {
    app.get_webview_window("main").is_some()
        && app.try_state::<DesktopState>().is_some_and(|desktop| {
            desktop.hibernation.is_main_ready() && !desktop.hibernation.is_hibernated_or_absent()
        })
}

fn should_queue_for_main<R: Runtime>(app: &AppHandle<R>) -> bool {
    let Some(desktop) = app.try_state::<DesktopState>() else {
        return false;
    };
    if desktop.hibernation.is_hibernated_or_absent() {
        return true;
    }
    app.get_webview_window("main").is_some() && !desktop.hibernation.is_main_ready()
}

/// Queues a `(event, payload)` for `"main"` to be re-emitted via
/// `emit_to("main", ...)` after `cmd_main_ready`.
pub fn queue_main_event<R: Runtime>(app: &AppHandle<R>, event: &str, payload: serde_json::Value) {
    if let Some(desktop) = app.try_state::<DesktopState>() {
        desktop
            .hibernation
            .push_pending(hibernate::PendingMainEvent::Emit {
                event: event.to_string(),
                payload,
            });
    }
}

/// Single choke point for main-window-targeted events (`MENU_EVENT`,
/// notifications, etc.). Wakes `"main"` when `reveal` is true, emits directly
/// when ready, or queues into [`hibernate::HibernationState`] for replay after
/// `cmd_main_ready`.
pub fn deliver_to_main<R: Runtime, P: Serialize + Clone>(
    app: &AppHandle<R>,
    event: &str,
    payload: P,
    reveal: bool,
) -> Result<(), String> {
    if reveal {
        reveal_main(app)?;
    }
    if should_queue_for_main(app) {
        let value = serde_json::to_value(payload).map_err(|e| e.to_string())?;
        queue_main_event(app, event, value);
        Ok(())
    } else if app.get_webview_window("main").is_some() {
        app.emit_to("main", event, payload)
            .map_err(|e| e.to_string())
    } else {
        app.emit(event, payload).map_err(|e| e.to_string())
    }
}

/// Installs the tray icon and Dock policy during `--background` lazy boot when
/// persisted settings indicate `show_status_icon` is enabled, keeping `"main"`
/// hibernated with zero WebView memory.
pub fn apply_background_boot_tray<R: Runtime>(
    app: &AppHandle<R>,
    show_status_icon: bool,
    hide_dock_when_closed: bool,
) -> Result<(), String> {
    if !show_status_icon {
        return Ok(());
    }
    let next = {
        let desktop = app.state::<DesktopState>();
        let mut guard = crate::supervisor::lock_or_recover(&desktop.menu_state);
        guard.show_status_icon = true;
        guard.hide_dock_when_closed = hide_dock_when_closed;
        guard.clone()
    };
    tray::apply(app, &next)?;
    if let Some(desktop) = app.try_state::<DesktopState>() {
        desktop
            .hibernation
            .set_lifecycle(hibernate::MainWindowLifecycle::Hibernated);
    }
    crate::workbench::external_changes::note_main_window_on_screen(false);
    crate::sysload::sampler::set_app_hibernated(true);
    #[cfg(target_os = "macos")]
    apply_dock_policy(
        app,
        dock_policy(show_status_icon, hide_dock_when_closed, false),
    );
    Ok(())
}

pub fn queue_and_emit_open<R: Runtime>(app: &AppHandle<R>, path: &Path) {
    match find_git_root(path) {
        Some(root) => {
            let root_str = root.to_string_lossy().into_owned();
            let desktop_state = app.state::<DesktopState>();
            if should_queue_for_main(app) {
                desktop_state
                    .hibernation
                    .push_pending(hibernate::PendingMainEvent::OpenRepo(root_str));
                if let Err(error) = reveal_main(app) {
                    log::warn!(target: "desktop", "Could not reveal main window: {error}");
                }
                return;
            }
            {
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

pub fn reveal_main<R: Runtime>(app: &AppHandle<R>) -> Result<(), String> {
    if let Some(desktop) = app.try_state::<DesktopState>() {
        desktop.hibernation.cancel_hibernating();
        if desktop.hibernation.lifecycle() == hibernate::MainWindowLifecycle::Hibernated
            && app.get_webview_window("main").is_some()
        {
            // `main.destroy()` has run, but `WindowEvent::Destroyed` has not
            // yet removed `"main"` from Tauri's window registry. Defer wake
            // until `Destroyed` completes instead of reviving a dying window.
            desktop.hibernation.request_wake_after_destroy();
            crate::sysload::sampler::set_app_hibernated(false);
            return Ok(());
        }
    }
    let (window, built_from_absent) = match app.get_webview_window("main") {
        Some(window) => (window, false),
        None => (hibernate::build_main_window(app, true)?, true),
    };
    if let Some(desktop) = app.try_state::<DesktopState>() {
        if !built_from_absent
            && desktop.hibernation.lifecycle() != hibernate::MainWindowLifecycle::Waking
        {
            desktop
                .hibernation
                .set_lifecycle(hibernate::MainWindowLifecycle::Visible);
        }
    }
    crate::sysload::sampler::set_app_hibernated(false);
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

/// Whether the window sits on a native translucent material.
///
/// The `html.macos` glass tokens (material intensity, float blur, the
/// background field) only mean something over `NSVisualEffectView`; Windows and
/// Linux paint an opaque window. The frontend gates the Appearance material
/// settings on this capability, not on the OS name. `host_platform` asserts the
/// two agree.
pub const NATIVE_MATERIAL: bool = cfg!(target_os = "macos");

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
    // The renderer does not know the host's pressure, so its state never
    // carries it: keep what the native poller last set.
    let next = MenuState {
        pressure: previous.pressure,
        ..next
    };
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
    hibernate::persist_boot_tray_hint(next.show_status_icon, next.hide_dock_when_closed);
    popover::publish(app, &next)
}

/// Records the host's pressure for the status item and redraws it when that
/// changes what it shows. Main thread, like every tray update.
pub fn set_host_pressure<R: Runtime>(
    app: &AppHandle<R>,
    pressure: pressure::HostPressure,
) -> Result<(), String> {
    let previous = menu_state(app);
    if previous.pressure == pressure {
        return Ok(());
    }
    let next = MenuState {
        pressure,
        ..previous.clone()
    };
    if tray::needs_apply(&previous, &next) {
        tray::apply(app, &next)?;
    }
    *app.state::<DesktopState>()
        .menu_state
        .lock()
        .map_err(|error| error.to_string())? = next;
    Ok(())
}

/// Runs `update`, turning a panic into the error the caller is waiting for.
///
/// Everything handed to `run_on_main_thread` executes inside the AppKit event
/// loop. A panic there does not fail one command: tao re-raises it out of the
/// run loop, or the Objective-C trampoline aborts on it, and either way the
/// whole process goes with it. The caller's channel would also never be
/// answered. Only the message is kept; the panic hook has already logged the
/// payload and the backtrace.
fn unwind_to_error<T>(update: impl FnOnce() -> Result<T, String>) -> Result<T, String> {
    std::panic::catch_unwind(std::panic::AssertUnwindSafe(update))
        .unwrap_or_else(|_| Err("The main-thread update panicked and was abandoned".to_string()))
}

/// Runs `update` on the main thread (AppKit's, for anything that touches
/// native views or menus) and waits, off the async runtime, for its answer.
/// The native menu and the browser sessions (`browser::commands`) both go
/// through here.
pub(crate) async fn on_main_thread<R: Runtime, T, F>(
    app: AppHandle<R>,
    update: F,
) -> Result<T, String>
where
    T: Send + 'static,
    F: FnOnce(&AppHandle<R>) -> Result<T, String> + Send + 'static,
{
    let (sender, receiver) = std::sync::mpsc::sync_channel(1);
    let handle = app.clone();
    app.run_on_main_thread(move || {
        if sender.send(unwind_to_error(|| update(&handle))).is_err() {
            log::warn!(target: "desktop", "A main-thread caller disconnected before its answer");
        }
    })
    .map_err(|error| error.to_string())?;
    wait_for_main_thread(receiver)
        .await?
        .map_err(|error| format!("The main-thread update did not finish: {error}"))?
}

/// Waits, off the async runtime, up to [`MAIN_THREAD_WAIT`] for what the main
/// thread sends back. Every main-thread caller waits through this one blocking
/// start, so a stalled AppKit loop costs each caller one pool thread for a
/// bounded time and never more.
async fn wait_for_main_thread<T: Send + 'static>(
    receiver: std::sync::mpsc::Receiver<T>,
) -> Result<Result<T, std::sync::mpsc::RecvTimeoutError>, String> {
    tauri::async_runtime::spawn_blocking(move || receiver.recv_timeout(MAIN_THREAD_WAIT))
        .await
        .map_err(|error| error.to_string())
}

/// The newest menu presentation waiting for the main thread, and the callers
/// waiting on it.
///
/// Presentation is idempotent and latest-wins (`menuSync.ts` already coalesces
/// on the frontend), so a stalled main thread must never accumulate one queued
/// closure per update: each would apply a state nobody wants any more, all at
/// once, when AppKit finally drains. At most one closure is queued; an update
/// that arrives while it is queued replaces the pending state and joins the
/// waiters.
#[derive(Default)]
pub(crate) struct MenuStateSlot {
    inner: Mutex<SlotInner>,
}

#[derive(Default)]
struct SlotInner {
    pending: Option<MenuState>,
    /// When the queued closure was handed to the main thread; `Some` means a
    /// closure is queued and has not yet run.
    queued_at: Option<std::time::Instant>,
    waiters: Vec<std::sync::mpsc::SyncSender<Result<(), String>>>,
}

/// How long a caller waits for the main thread before it stops waiting.
const MAIN_THREAD_WAIT: std::time::Duration = std::time::Duration::from_secs(10);

impl MenuStateSlot {
    /// Records `next` as the newest state and queues the one closure that will
    /// apply it, unless one is already queued. Returns the channel the outcome
    /// arrives on.
    fn submit(
        slot: &std::sync::Arc<Self>,
        next: MenuState,
        dispatch: &dyn Fn(Box<dyn FnOnce() + Send>) -> Result<(), String>,
        apply: std::sync::Arc<dyn Fn(MenuState) -> Result<(), String> + Send + Sync>,
    ) -> Result<std::sync::mpsc::Receiver<Result<(), String>>, String> {
        let (sender, receiver) = std::sync::mpsc::sync_channel(1);
        {
            let mut inner = crate::supervisor::lock_or_recover(&slot.inner);
            inner.pending = Some(next);
            inner.waiters.push(sender);
            if inner.queued_at.is_some() {
                return Ok(receiver);
            }
            inner.queued_at = Some(std::time::Instant::now());
        }
        let runner = std::sync::Arc::clone(slot);
        let queued = dispatch(Box::new(move || runner.run(&*apply)));
        if let Err(error) = queued {
            let mut inner = crate::supervisor::lock_or_recover(&slot.inner);
            inner.queued_at = None;
            inner.pending = None;
            inner.waiters.clear();
            return Err(error);
        }
        Ok(receiver)
    }

    /// The queued closure: applies whatever is newest *now*, not what was
    /// newest when it was queued, and answers every caller it covers.
    fn run(&self, apply: &(dyn Fn(MenuState) -> Result<(), String> + Send + Sync)) {
        let (state, queued_at, waiters) = {
            let mut inner = crate::supervisor::lock_or_recover(&self.inner);
            (
                inner.pending.take(),
                inner.queued_at.take(),
                std::mem::take(&mut inner.waiters),
            )
        };
        if let Some(queued_at) = queued_at {
            let blocked = queued_at.elapsed();
            if blocked >= MAIN_THREAD_WAIT {
                log::warn!(
                    target: "desktop",
                    "The main thread was blocked for {blocked:?} before it applied the menu state"
                );
            }
        }
        let outcome = match state {
            Some(state) => unwind_to_error(|| apply(state)),
            None => Ok(()),
        };
        for waiter in waiters {
            // A caller that stopped waiting has already been told it was queued.
            let _ = waiter.send(outcome.clone());
        }
    }
}

/// Waits, off the async runtime, for the outcome of a queued menu update.
///
/// A timeout is not a failure: the update is still queued and will apply the
/// newest state when the main thread frees up, so the caller is told it went
/// through and the delay is logged once, at WARN, with the wait.
async fn await_menu_outcome(
    receiver: std::sync::mpsc::Receiver<Result<(), String>>,
) -> Result<(), String> {
    match wait_for_main_thread(receiver).await? {
        Ok(result) => result,
        Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {
            log::warn!(
                target: "desktop",
                "The main thread did not apply the menu state within {MAIN_THREAD_WAIT:?}; it stays queued and the newest state applies when it frees up"
            );
            Ok(())
        }
        Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => {
            Err("The main-thread update was dropped before it ran".to_string())
        }
    }
}

#[tauri::command]
pub async fn cmd_set_menu_state(app: AppHandle, presentation: MenuState) -> Result<(), String> {
    presentation.validate()?;
    let slot = std::sync::Arc::clone(&app.state::<DesktopState>().menu_slot);
    let handle = app.clone();
    let receiver = MenuStateSlot::submit(
        &slot,
        presentation,
        &|job| {
            app.run_on_main_thread(job)
                .map_err(|error| error.to_string())
        },
        std::sync::Arc::new(move |state| set_menu_state(&handle, state)),
    )?;
    await_menu_outcome(receiver).await
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

/// Completes a frontend-approved exit (see `exit::approve`). `terminals`
/// is the quit's choice for sessions a terminal host holds: `"leave"` keeps
/// them running for the next launch, `"stop"` ends them. Absent leaves the
/// backend's default: in-process sessions always stop, a host's are left.
/// `managed` is the same choice for managed agent runs a hosted Manvi holds
/// (Settings → Agents → keep running): `"leave"` disconnects and leaves them,
/// `"stop"` stops them first. Absent leaves them, which is also what an exit
/// nobody was asked about does; without a hosted Manvi the runs end with the
/// process whatever this says.
#[tauri::command(async)]
pub fn cmd_exit_app(
    app: AppHandle,
    terminals: Option<String>,
    managed: Option<String>,
) -> Result<(), String> {
    let sessions = app.state::<crate::terminal::TerminalSessions>();
    // Both choices are validated before either is applied, so a refused
    // request changes nothing.
    let leave_terminals = match terminals.as_deref() {
        None => None,
        Some("leave") => Some(true),
        Some("stop") => Some(false),
        Some(other) => return Err(format!("Unknown choice for terminals on quit: {other:?}")),
    };
    let managed_exit = match managed.as_deref() {
        None => None,
        Some("leave") => Some(crate::harness::hosted::ExitChoice::Leave),
        Some("stop") => Some(crate::harness::hosted::ExitChoice::Stop),
        Some(other) => {
            return Err(format!(
                "Unknown choice for managed runs on quit: {other:?}"
            ))
        }
    };
    exit::approve(&app, || {
        if let Some(leave) = leave_terminals {
            sessions.set_leave_running(leave);
        }
        if let Some(choice) = managed_exit {
            crate::harness::sidecar::set_managed_exit(choice);
        }
    });
    Ok(())
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
    fn main_webview_admits_srcdoc_subframes_only_while_other_webviews_refuse() {
        let allow_main =
            |u: &str| navigation_allowed_for_webview("main", &tauri::Url::parse(u).unwrap());
        let allow_popover = |u: &str| {
            navigation_allowed_for_webview(popover::LABEL, &tauri::Url::parse(u).unwrap())
        };
        assert!(allow_main("about:srcdoc"));
        assert!(allow_main("about:srcdoc#section-1"));
        assert!(!allow_main("about:srcdoc?"));
        assert!(!allow_main("about:srcdoc?x=1"));
        assert!(!allow_main("about:srcdoc?x=1#section-1"));
        assert!(!allow_main("about:srcdoc/other"));
        assert!(!allow_main("about://evil.example/srcdoc"));
        assert!(!allow_main("about:config"));
        assert!(!allow_main("https://example.com/"));
        assert!(!allow_main("data:text/html,<h1>x</h1>"));
        assert!(!allow_popover("about:srcdoc"));
        assert!(!allow_popover("about:srcdoc#section-1"));
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

    /// The main-thread dispatches a test captured instead of running them.
    type JobQueue = std::sync::Arc<Mutex<Vec<Box<dyn FnOnce() + Send>>>>;

    /// A stalled main thread must not collect one closure per update: ten
    /// updates queue exactly one, and the newest state is the one applied.
    #[test]
    fn menu_updates_behind_a_stalled_main_thread_queue_one_closure_and_apply_the_newest() {
        let slot = std::sync::Arc::new(MenuStateSlot::default());
        let queue: JobQueue = Default::default();
        let applied: std::sync::Arc<Mutex<Vec<MenuState>>> = Default::default();
        let mut receivers = Vec::new();
        for index in 0..10 {
            let queue = std::sync::Arc::clone(&queue);
            let sink = std::sync::Arc::clone(&applied);
            let next = MenuState {
                active_path: Some(format!("/r/{index}")),
                ..MenuState::default()
            };
            receivers.push(
                MenuStateSlot::submit(
                    &slot,
                    next,
                    &move |job| {
                        queue.lock().unwrap().push(job);
                        Ok(())
                    },
                    std::sync::Arc::new(move |state| {
                        sink.lock().unwrap().push(state);
                        Ok(())
                    }),
                )
                .unwrap(),
            );
        }
        assert_eq!(queue.lock().unwrap().len(), 1, "one closure, not ten");
        let job = queue.lock().unwrap().pop().unwrap();
        job();
        let applied = applied.lock().unwrap();
        assert_eq!(applied.len(), 1, "the newest state is applied once");
        assert_eq!(applied[0].active_path.as_deref(), Some("/r/9"));
        for receiver in receivers {
            assert_eq!(receiver.recv().unwrap(), Ok(()), "every caller is answered");
        }
    }

    #[test]
    fn a_menu_apply_failure_reaches_every_waiting_caller_and_frees_the_slot() {
        let slot = std::sync::Arc::new(MenuStateSlot::default());
        let queue: JobQueue = Default::default();
        let submit = |slot: &std::sync::Arc<MenuStateSlot>| {
            let queue = std::sync::Arc::clone(&queue);
            MenuStateSlot::submit(
                slot,
                MenuState::default(),
                &move |job| {
                    queue.lock().unwrap().push(job);
                    Ok(())
                },
                std::sync::Arc::new(|_| Err("menu rebuild failed".to_string())),
            )
            .unwrap()
        };
        let first = submit(&slot);
        let second = submit(&slot);
        let job = queue.lock().unwrap().pop().unwrap();
        job();
        assert_eq!(
            first.recv().unwrap(),
            Err("menu rebuild failed".to_string())
        );
        assert_eq!(
            second.recv().unwrap(),
            Err("menu rebuild failed".to_string())
        );
        let _third = submit(&slot);
        assert_eq!(
            queue.lock().unwrap().len(),
            1,
            "the slot accepts a new closure after a run"
        );
    }

    #[test]
    fn a_refused_dispatch_leaves_no_stale_queued_flag() {
        let slot = std::sync::Arc::new(MenuStateSlot::default());
        let refused = MenuStateSlot::submit(
            &slot,
            MenuState::default(),
            &|_| Err("event loop closed".to_string()),
            std::sync::Arc::new(|_| Ok(())),
        );
        assert_eq!(refused.err(), Some("event loop closed".to_string()));
        let queue: JobQueue = Default::default();
        let sink = std::sync::Arc::clone(&queue);
        MenuStateSlot::submit(
            &slot,
            MenuState::default(),
            &move |job| {
                sink.lock().unwrap().push(job);
                Ok(())
            },
            std::sync::Arc::new(|_| Ok(())),
        )
        .unwrap();
        assert_eq!(queue.lock().unwrap().len(), 1);
    }

    /// `on_main_thread` runs its closure inside the AppKit event loop, where an
    /// unwinding panic ends the process rather than failing the command. It
    /// has to come back as the error the awaiting command reports.
    #[test]
    fn a_panic_inside_a_main_thread_update_is_an_error_not_a_dead_app() {
        let app = tauri::test::mock_builder()
            .manage(DesktopState::default())
            .build(crate::context())
            .unwrap();
        let outcome = tauri::async_runtime::block_on(on_main_thread(
            app.handle().clone(),
            |_| -> Result<(), String> { panic!("the menu rebuild hit an impossible state") },
        ));
        let message = outcome.expect_err("a panicking update cannot have succeeded");
        assert!(message.contains("panicked"), "unexpected error: {message}");
        let fine = tauri::async_runtime::block_on(on_main_thread(app.handle().clone(), |_| Ok(())));
        assert_eq!(fine, Ok(()), "a healthy update is unchanged");
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

    /// The quit's choice for hosted runs is for one approved quit. When that
    /// quit is held for a confirmation or cancelled, a later exit nobody was
    /// asked about (a signal) must not find `Stop` still set.
    #[test]
    fn a_prevented_or_cancelled_quit_does_not_leave_stop_behind() {
        use crate::harness::hosted::{exit_choice, set_exit_choice, ExitChoice};
        let _serial = crate::harness::sidecar::test_serial();
        let app = tauri::test::mock_builder().build(crate::context()).unwrap();
        app.manage(DesktopState::default());
        let handle = app.handle();
        let state = handle.state::<DesktopState>();

        // A quit that proceeds keeps the choice: it is the one it was made for.
        set_exit_choice(ExitChoice::Stop);
        assert_eq!(route_exit_requested(handle, Some(0)), exit::Begin::Proceed);
        assert_eq!(exit_choice(), ExitChoice::Stop);

        // A code-less exit held behind a request already waiting is prevented.
        state.exit.arm();
        let (opened, _) = state.exit.open(exit::Origin::Internal);
        assert_eq!(opened, exit::Begin::Deferred);
        assert_eq!(
            route_exit_requested(handle, None),
            exit::Begin::AlreadyPending
        );
        assert_eq!(
            exit_choice(),
            ExitChoice::Leave,
            "a prevented exit must not leave Stop set for a later exit nobody was asked about"
        );

        // The person cancels the quit they had answered Stop to.
        set_exit_choice(ExitChoice::Stop);
        let pending = state.exit.take().expect("the open request");
        exit::settle(handle, pending, false);
        assert_eq!(exit_choice(), ExitChoice::Leave);
        crate::harness::hosted::reset_exit_choice();
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn serve_agent_deliver_rejects_ide_external_and_working_agy_peers_without_emitting_banner() {
        use tauri::Listener;
        let repo_dir = crate::codeintel::tests::repo_with_one_generation();
        let repo = repo_dir.path().to_str().unwrap().to_string();
        crate::test_support::trust_repo(repo_dir.path());

        let my_pid = std::process::id();
        let my_birth = crate::workbench::process_birth::started_unix_nanos(my_pid).unwrap();

        // 1. Live Antigravity IDE peer (`terminal_session_id: None`, `surface: "ide"`).
        let ide_sid = "99999999-9999-4999-8999-999999999901";
        let ide_peer = crate::engine::agent_bus::upsert_peer(
            &repo,
            crate::engine::agent_bus::PeerUpsert {
                peer_id: None,
                provider: "agy".into(),
                surface: "ide".into(),
                provider_session_id: Some(ide_sid.into()),
                terminal_session_id: None,
                run_id: None,
                worktree_path: repo.clone(),
                branch: Some("main".into()),
                role: Some("ide-agent".into()),
                turn_state: Some("idle".into()),
                auto_wake: Some(true),
                pid: Some(my_pid),
                proc_start_ns: Some(my_birth),
                transcript_path: None,
            },
        )
        .unwrap();

        // 2. Hosted `agy` terminal tab whose `TerminalSessions` has no live `"agy"` foreground PTY.
        let dead_pty_sid = "99999999-9999-4999-8999-999999999902";
        let dead_pty_peer = crate::engine::agent_bus::upsert_peer(
            &repo,
            crate::engine::agent_bus::PeerUpsert {
                peer_id: None,
                provider: "agy".into(),
                surface: "terminal".into(),
                provider_session_id: Some(dead_pty_sid.into()),
                terminal_session_id: Some("term-missing-42".into()),
                run_id: None,
                worktree_path: repo.clone(),
                branch: Some("main".into()),
                role: Some("missing-pty".into()),
                turn_state: Some("idle".into()),
                auto_wake: Some(true),
                pid: Some(my_pid),
                proc_start_ns: Some(my_birth),
                transcript_path: None,
            },
        )
        .unwrap();

        let app = tauri::test::mock_builder()
            .manage(DesktopState::default())
            .manage(crate::terminal::TerminalSessions::default())
            .build(crate::context())
            .unwrap();
        let emitted = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let emitted_sink = std::sync::Arc::clone(&emitted);
        let _listener = app.listen_any(PEER_INBOX_EVENT, move |_| {
            emitted_sink.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        });

        for (label, pid_key) in [
            ("ide", ide_peer.peer_id.clone()),
            ("missing_pty", dead_pty_peer.peer_id.clone()),
        ] {
            let req = instance::Request::AgentDeliver {
                id: format!("req_{label}"),
                repo: repo.clone(),
                peer_id: pid_key,
                msg_id: format!("msg_{label}"),
                spawn: None,
                provider: None,
                branch: None,
                prompt: None,
                task_id: None,
            };
            let ans = serve_agent_deliver(app.handle(), &req).expect("serve_agent_deliver");
            assert_eq!(
                ans["delivery_outcome"], "queued_for_hook_boundary",
                "{label} must queue for hook boundary instead of claiming banner/wake: {ans}"
            );
        }
        assert_eq!(
            emitted.load(std::sync::atomic::Ordering::SeqCst),
            0,
            "PEER_INBOX_EVENT must not be emitted when no hosted agy PTY is in the foreground"
        );
    }

    #[test]
    fn deliver_to_main_queues_when_hibernated_and_emits_when_ready() {
        use tauri::{Listener, WebviewUrl, WebviewWindowBuilder};
        let app = tauri::test::mock_builder()
            .manage(DesktopState::default())
            .build(crate::context())
            .unwrap();
        let desktop = app.state::<DesktopState>();
        desktop
            .hibernation
            .set_lifecycle(hibernate::MainWindowLifecycle::Hibernated);

        deliver_to_main(
            app.handle(),
            MENU_EVENT,
            NativeEvent {
                id: "focus-agent:term-1".into(),
                path: None,
                repo_path: None,
            },
            false,
        )
        .unwrap();
        assert_eq!(desktop.hibernation.pending_len(), 1);
        let drained = desktop.hibernation.drain_pending();
        assert!(matches!(
            drained.as_slice(),
            [hibernate::PendingMainEvent::Emit { event, .. }] if event == MENU_EVENT
        ));

        let main = WebviewWindowBuilder::new(&app, "main", WebviewUrl::App("index.html".into()))
            .build()
            .unwrap();
        desktop
            .hibernation
            .set_lifecycle(hibernate::MainWindowLifecycle::Visible);
        desktop.hibernation.set_main_ready(true);

        let delivered = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let sink = std::sync::Arc::clone(&delivered);
        let _listener = main.listen(MENU_EVENT, move |_| {
            sink.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        });
        deliver_to_main(
            app.handle(),
            MENU_EVENT,
            NativeEvent {
                id: "refresh".into(),
                path: None,
                repo_path: None,
            },
            false,
        )
        .unwrap();
        assert_eq!(desktop.hibernation.pending_len(), 0);
        assert_eq!(delivered.load(std::sync::atomic::Ordering::SeqCst), 1);
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn serve_kickoff_spawn_queues_terminal_when_main_is_hibernated() {
        let repo_dir = crate::codeintel::tests::repo_with_one_generation();
        let repo = repo_dir.path().to_str().unwrap().to_string();
        let app = tauri::test::mock_builder()
            .manage(DesktopState::default())
            .build(crate::context())
            .unwrap();
        let desktop = app.state::<DesktopState>();
        desktop
            .hibernation
            .set_lifecycle(hibernate::MainWindowLifecycle::Hibernated);

        let ans = serve_kickoff_spawn(
            app.handle(),
            &repo,
            "req-1",
            "terminal",
            Some("claude"),
            Some("main"),
            Some("inspect"),
            None,
        )
        .expect("serve_kickoff_spawn");
        assert_eq!(ans["delivery_outcome"], "queued_for_main_wake");
        assert_eq!(desktop.hibernation.pending_len(), 1);
        let drained = desktop.hibernation.drain_pending();
        assert!(matches!(
            drained.as_slice(),
            [hibernate::PendingMainEvent::AgentKickoff(_)]
        ));
    }
}
