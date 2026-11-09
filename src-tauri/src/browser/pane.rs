//! macOS hosting of the browser pane: one raw wry child webview over the main
//! window's content, owned by the main thread. See the module docs in
//! `browser/mod.rs` for why it is raw wry.

use super::{policy, Bounds, BrowserError};
use objc2::MainThreadMarker;
use std::cell::RefCell;
use wry::dpi::{LogicalPosition, LogicalSize};
use wry::{NewWindowResponse, Rect, WebView, WebViewBuilder};

/// The environment variable that opens the M0 pane at startup.
const ENV_URL: &str = "GITPULSE_BROWSER_PANE_URL";

thread_local! {
    // AppKit views are main-thread only and `WebView` is neither `Send` nor
    // `Sync`, so the pane lives in the main thread's own slot and every entry
    // point checks it is running there.
    static PANE: RefCell<Option<WebView>> = const { RefCell::new(None) };
}

fn main_thread() -> Result<MainThreadMarker, BrowserError> {
    MainThreadMarker::new().ok_or(BrowserError::NotMainThread)
}

fn rect(bounds: Bounds) -> Rect {
    Rect {
        position: LogicalPosition::new(bounds.x, bounds.y).into(),
        size: LogicalSize::new(bounds.width.max(0.0), bounds.height.max(0.0)).into(),
    }
}

/// The pane's webview configuration. Deliberately absent: an IPC handler, any
/// custom scheme, and any initialization script (see the module docs).
fn builder(url: &str, bounds: Bounds) -> WebViewBuilder<'static> {
    WebViewBuilder::new()
        .with_url(url)
        .with_bounds(rect(bounds))
        // A non-persistent WKWebsiteDataStore: nothing shared with GitPulse's
        // own webview, nothing written to disk.
        .with_incognito(true)
        .with_devtools(cfg!(debug_assertions))
        // Asked for every frame's navigations, so a remote iframe is refused.
        .with_navigation_handler(|url| policy::allows_navigation(&url))
        .with_new_window_req_handler(|_, _| NewWindowResponse::Deny)
    // No download handler: wry cancels a navigation that asks to download
    // when none is installed (`wkwebview/navigation.rs`).
}

/// Opens the pane on `window` at `bounds`, showing `url`. Main thread only.
pub fn open(window: &tauri::WebviewWindow, url: &str, bounds: Bounds) -> Result<(), BrowserError> {
    main_thread()?;
    let url = policy::check_loopback(url).map_err(BrowserError::Refused)?;
    PANE.with(|slot| {
        if slot.borrow().is_some() {
            return Err(BrowserError::AlreadyOpen);
        }
        // tao's window handle is the window's *current* content view, read on
        // every call (`tao/src/platform_impl/macos/window.rs`, `ns_view`), so
        // the pane lands beside Tauri's webview even after wry has replaced the
        // content view with its own parent view.
        let webview = builder(url.as_str(), bounds)
            .build_as_child(window)
            .map_err(|e| BrowserError::Native(format!("could not build the webview: {e}")))?;
        *slot.borrow_mut() = Some(webview);
        Ok(())
    })
}

/// Runs `f` with the open pane's webview. Main thread only.
pub fn with_webview<T>(f: impl FnOnce(&WebView) -> T) -> Result<T, BrowserError> {
    main_thread()?;
    PANE.with(|slot| slot.borrow().as_ref().map(f).ok_or(BrowserError::NotOpen))
}

pub fn is_open() -> bool {
    main_thread().is_ok() && PANE.with(|slot| slot.borrow().is_some())
}

/// Moves or resizes the pane. Main thread only.
pub fn set_bounds(bounds: Bounds) -> Result<(), BrowserError> {
    with_webview(|webview| webview.set_bounds(rect(bounds)))?
        .map_err(|e| BrowserError::Native(format!("could not move the webview: {e}")))
}

/// Shows or hides the pane without unloading its page. Main thread only.
pub fn set_visible(visible: bool) -> Result<(), BrowserError> {
    with_webview(|webview| webview.set_visible(visible))?
        .map_err(|e| BrowserError::Native(format!("could not show or hide the webview: {e}")))
}

/// Removes the pane from the window. Main thread only.
pub fn close() -> Result<(), BrowserError> {
    main_thread()?;
    // Taken out of the slot before it is dropped, so nothing the drop runs can
    // find the slot borrowed.
    let webview = PANE.with(|slot| slot.borrow_mut().take());
    match webview {
        Some(webview) => {
            drop(webview);
            Ok(())
        }
        None => Err(BrowserError::NotOpen),
    }
}

/// Opens the pane at startup when `GITPULSE_BROWSER_PANE_URL` names a loopback
/// URL: the M0 flag. It takes the right 40% of the main window and does not
/// follow later resizes (the M1 pane is laid out by the frontend). Main
/// thread only; failures are logged, never fatal.
pub fn open_from_env(app: &tauri::AppHandle) {
    let Some(url) = std::env::var_os(ENV_URL) else {
        return;
    };
    let Some(url) = url.to_str() else {
        log::warn!(target: "browser", "{ENV_URL} is not UTF-8; no browser pane opened");
        return;
    };
    use tauri::Manager;
    let Some(window) = app.get_webview_window("main") else {
        log::warn!(target: "browser", "{ENV_URL} is set but there is no main window; no browser pane opened");
        return;
    };
    let bounds = match (window.inner_size(), window.scale_factor()) {
        (Ok(size), Ok(scale)) => {
            let size = size.to_logical::<f64>(scale);
            Bounds {
                x: size.width * 0.6,
                y: 0.0,
                width: size.width * 0.4,
                height: size.height,
            }
        }
        (Err(e), _) | (_, Err(e)) => {
            log::warn!(target: "browser", "could not measure the main window ({e}); no browser pane opened");
            return;
        }
    };
    match open(&window, url, bounds) {
        Ok(()) => log::info!(target: "browser", "browser pane opened on {url}"),
        Err(e) => log::warn!(target: "browser", "{e}"),
    }
}
