//! The browser session commands. Each validates its input here, at the
//! boundary, then runs on the main thread, where the panes live
//! (`desktop::on_main_thread`, which also turns a panic there into an error
//! instead of a dead app). Elsewhere than macOS each answers
//! [`UNSUPPORTED`](super::UNSUPPORTED).

use super::{policy, validate_id, Bounds};
use serde::Deserialize;
use tauri::AppHandle;

/// A history step for [`cmd_browser_go`].
#[derive(Clone, Copy, Debug, Deserialize, PartialEq)]
#[serde(rename_all = "lowercase")]
pub enum BrowserGo {
    Back,
    Forward,
    Reload,
}

/// The URL as the pane may load it, or why not, worded for the address bar.
fn loadable(url: &str) -> Result<String, String> {
    policy::check_loopback(url.trim())
        .map(|url| url.to_string())
        .map_err(|why| format!("Browser sessions open local development servers only: {why}"))
}

/// Opens session `id`'s pane at `bounds` in the main window, showing `url`.
#[tauri::command]
pub async fn cmd_browser_open(
    app: AppHandle,
    id: String,
    url: String,
    bounds: Bounds,
) -> Result<(), String> {
    validate_id(&id)?;
    bounds.validate()?;
    let url = loadable(&url)?;
    imp::open(app, id, url, bounds).await
}

/// Moves or resizes session `id`'s pane.
#[tauri::command]
pub async fn cmd_browser_set_bounds(
    app: AppHandle,
    id: String,
    bounds: Bounds,
) -> Result<(), String> {
    validate_id(&id)?;
    bounds.validate()?;
    imp::set_bounds(app, id, bounds).await
}

/// Shows or hides session `id`'s pane, keeping its page.
#[tauri::command]
pub async fn cmd_browser_set_visible(
    app: AppHandle,
    id: String,
    visible: bool,
) -> Result<(), String> {
    validate_id(&id)?;
    imp::set_visible(app, id, visible).await
}

/// Points session `id` at `url`. A URL the policy refuses is answered here,
/// since the pane's own navigation handler cancels without a word.
#[tauri::command]
pub async fn cmd_browser_navigate(app: AppHandle, id: String, url: String) -> Result<(), String> {
    validate_id(&id)?;
    let url = loadable(&url)?;
    imp::navigate(app, id, url).await
}

/// Back, forward or reload in session `id`.
#[tauri::command]
pub async fn cmd_browser_go(app: AppHandle, id: String, to: BrowserGo) -> Result<(), String> {
    validate_id(&id)?;
    imp::go(app, id, to).await
}

/// Closes session `id`'s pane. Closing one that is not open is not an error:
/// the tab that owned it is going either way.
#[tauri::command]
pub async fn cmd_browser_close(app: AppHandle, id: String) -> Result<(), String> {
    validate_id(&id)?;
    imp::close(app, id).await
}

#[cfg(target_os = "macos")]
mod imp {
    use super::super::{self as browser, Bounds, BrowserError, EventSink, PaneEvent, EVENT};
    use super::BrowserGo;
    use crate::desktop::on_main_thread;
    use std::sync::Arc;
    use tauri::{AppHandle, Emitter, Manager};

    fn sink(app: &AppHandle) -> EventSink {
        let app = app.clone();
        Arc::new(move |event: PaneEvent| {
            if let Err(error) = app.emit(EVENT, event) {
                log::warn!(target: "browser", "a browser page event was not delivered: {error}");
            }
        })
    }

    fn text(error: BrowserError) -> String {
        error.to_string()
    }

    pub async fn open(
        app: AppHandle,
        id: String,
        url: String,
        bounds: Bounds,
    ) -> Result<(), String> {
        on_main_thread(app, move |app| {
            let window = app
                .get_webview_window("main")
                .ok_or("the main window is not open")?;
            browser::open(&window, &id, &url, bounds, Some(sink(app))).map_err(text)
        })
        .await
    }

    pub async fn set_bounds(app: AppHandle, id: String, bounds: Bounds) -> Result<(), String> {
        on_main_thread(app, move |_| browser::set_bounds(&id, bounds).map_err(text)).await
    }

    pub async fn set_visible(app: AppHandle, id: String, visible: bool) -> Result<(), String> {
        on_main_thread(app, move |_| {
            browser::set_visible(&id, visible).map_err(text)
        })
        .await
    }

    pub async fn navigate(app: AppHandle, id: String, url: String) -> Result<(), String> {
        on_main_thread(app, move |_| {
            browser::with_webview(&id, |webview| webview.load_url(&url))
                .map_err(text)?
                .map_err(|e| format!("the page could not be loaded: {e}"))
        })
        .await
    }

    pub async fn go(app: AppHandle, id: String, to: BrowserGo) -> Result<(), String> {
        on_main_thread(app, move |_| {
            browser::with_webview(&id, |webview| match to {
                BrowserGo::Back => webview.go_back(),
                BrowserGo::Forward => webview.go_forward(),
                BrowserGo::Reload => webview.reload(),
            })
            .map_err(text)?
            .map_err(|e| format!("the page could not do that: {e}"))
        })
        .await
    }

    pub async fn close(app: AppHandle, id: String) -> Result<(), String> {
        on_main_thread(app, move |_| match browser::close(&id) {
            Ok(()) | Err(BrowserError::NotOpen) => Ok(()),
            Err(error) => Err(text(error)),
        })
        .await
    }
}

#[cfg(not(target_os = "macos"))]
mod imp {
    use super::super::{Bounds, UNSUPPORTED};
    use super::BrowserGo;
    use tauri::AppHandle;

    pub async fn open(_: AppHandle, _: String, _: String, _: Bounds) -> Result<(), String> {
        Err(UNSUPPORTED.into())
    }
    pub async fn set_bounds(_: AppHandle, _: String, _: Bounds) -> Result<(), String> {
        Err(UNSUPPORTED.into())
    }
    pub async fn set_visible(_: AppHandle, _: String, _: bool) -> Result<(), String> {
        Err(UNSUPPORTED.into())
    }
    pub async fn navigate(_: AppHandle, _: String, _: String) -> Result<(), String> {
        Err(UNSUPPORTED.into())
    }
    pub async fn go(_: AppHandle, _: String, _: BrowserGo) -> Result<(), String> {
        Err(UNSUPPORTED.into())
    }
    pub async fn close(_: AppHandle, _: String) -> Result<(), String> {
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_loopback_urls_are_loadable_and_the_refusal_says_why() {
        assert_eq!(
            loadable("  http://localhost:5173/  ").unwrap(),
            "http://localhost:5173/"
        );
        let refused = loadable("https://example.com/").unwrap_err();
        assert!(
            refused.contains("local development servers only"),
            "{refused}"
        );
        assert!(refused.contains("not a loopback address"), "{refused}");
    }

    #[test]
    fn history_steps_are_lowercase_on_the_wire() {
        let go: BrowserGo = serde_json::from_str("\"back\"").unwrap();
        assert_eq!(go, BrowserGo::Back);
        assert!(serde_json::from_str::<BrowserGo>("\"Back\"").is_err());
        assert!(serde_json::from_str::<BrowserGo>("\"home\"").is_err());
    }
}
