//! Browser sessions: web pages shown inside GitPulse's main window, as a
//! session type of the terminal dock, for a developer and their agents to look
//! at the UI they are building (board tasks `gp-browser-m0-hosting-spike` and
//! `gp-browser-m1-pane-picker`).
//!
//! # Why this is a raw wry webview and not a Tauri one
//!
//! Every webview Tauri manages is given the invoke bridge: the
//! `__TAURI_INTERNALS__` script, the `ipc` script-message handler and the
//! `ipc://` scheme (`framework/tauri/src/manager/webview.rs`). Which commands
//! a page may call is then decided by the capability ACL, which matches the
//! webview label *or the window label* (`framework/tauri/src/ipc/authority.rs`),
//! and GitPulse's capabilities are granted by window. A Tauri child webview of
//! `main` would therefore inherit every `allow-cmd-*` grant. In a dev build the
//! app's own origin (`devUrl`, `http://localhost:5173`) is treated as local
//! (`Webview::is_local_url`), so a page on that port would be admitted too.
//!
//! A webview built directly with wry has none of that: wry installs
//! `window.ipc` only when an IPC handler is given
//! (`framework/wry/src/wkwebview/mod.rs`), and this module never gives one, nor
//! registers a custom scheme or an initialization script. The page has no path
//! to a GitPulse command. `tests/browser_pane_isolation.rs` checks this against
//! a positive control (Tauri's own webview in the same window, which does
//! reach Rust).
//!
//! # Where the view goes
//!
//! wry's main webview replaces the window's content view with its own parent
//! view. A pane is still parented correctly through the Tauri window's handle
//! because tao reads the *current* content view on every call
//! (`framework/tao/src/platform_impl/macos/window.rs`, `ns_view`), not the
//! view it created; the isolation test asserts the pane's superview is that
//! content view. The frontend owns the layout: it sends each pane the bounds of
//! the tab it belongs to and hides it whenever that tab is off screen or
//! something of GitPulse's own is drawn over it (`browserPlacement.ts`).
//!
//! # Policy
//!
//! - Loopback only: `http`/`https` to `localhost`, `127.0.0.0/8` or `::1`, and
//!   `about:blank`. wry asks the navigation handler for every frame's
//!   navigations, so a remote iframe is refused too, and the refusal is
//!   reported to the session. Subresources (`fetch`, images) are not
//!   navigations and are not filtered here.
//! - No new windows, no downloads.
//! - A non-persistent data store per pane, so a page on the app's own dev
//!   origin does not share GitPulse's localStorage or cookies.
//! - Everything a page reports (its URL, its title) is capped here before it
//!   is sent on.
//! - At most [`MAX_PANES`] at once, and all of them are closed when the main
//!   page reloads (`desktop::handle_page_load`), since the page that laid them
//!   out is gone.
//!
//! macOS only for now: elsewhere the commands answer [`UNSUPPORTED`] and the
//! session type is not offered.

pub mod commands;
pub mod policy;

#[cfg(target_os = "macos")]
mod pane;

#[cfg(target_os = "macos")]
pub use pane::{
    close, close_all, is_open, open, open_count, set_bounds, set_visible, with_webview, EventSink,
};

use serde::{Deserialize, Serialize};

/// Whether browser sessions are built for this target; reported to the
/// frontend as `HostPlatform::browser_sessions`.
pub const SUPPORTED: bool = cfg!(target_os = "macos");

/// The most panes open at once, across every repository's dock.
pub const MAX_PANES: usize = 8;

/// The event every pane's page activity is emitted as.
pub const EVENT: &str = "browser-pane";

/// What the commands answer where browser sessions are not built.
pub const UNSUPPORTED: &str = "Browser sessions are available on macOS only for now";

/// The longest URL and title passed on from a page, in characters.
pub const MAX_URL_CHARS: usize = 2048;
pub const MAX_TITLE_CHARS: usize = 256;

/// Where a pane sits, in logical points from the top-left of the window's
/// content area.
#[derive(Clone, Copy, Debug, PartialEq, Deserialize, Serialize)]
pub struct Bounds {
    pub x: f64,
    pub y: f64,
    pub width: f64,
    pub height: f64,
}

/// No real window is this large; anything past it is a bad measurement.
const MAX_EXTENT: f64 = 100_000.0;

impl Bounds {
    /// Finite, inside a plausible window, and not negative in size.
    pub fn validate(&self) -> Result<(), String> {
        let finite = [self.x, self.y, self.width, self.height]
            .iter()
            .all(|v| v.is_finite());
        if !finite {
            return Err("browser bounds must be finite numbers".into());
        }
        if self.x.abs() > MAX_EXTENT || self.y.abs() > MAX_EXTENT {
            return Err("browser bounds are outside any window".into());
        }
        if !(0.0..=MAX_EXTENT).contains(&self.width) || !(0.0..=MAX_EXTENT).contains(&self.height) {
            return Err("browser bounds have a negative or impossible size".into());
        }
        Ok(())
    }
}

/// A pane's id: the frontend's tab id, 1 to 64 of `[A-Za-z0-9_-]`.
pub fn validate_id(id: &str) -> Result<(), String> {
    let ok = (1..=64).contains(&id.len())
        && id
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_');
    if ok {
        Ok(())
    } else {
        Err("a browser session id is 1 to 64 letters, digits, '-' or '_'".into())
    }
}

/// `text` cut to at most `max` characters, on a character boundary.
pub fn cap(text: &str, max: usize) -> String {
    match text.char_indices().nth(max) {
        Some((end, _)) => text[..end].to_owned(),
        None => text.to_owned(),
    }
}

/// What a pane's page did, for the session's address bar and tab title.
#[derive(Clone, Debug, PartialEq, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum PageEvent {
    /// A navigation started.
    Loading { url: String },
    /// A navigation finished.
    Loaded { url: String },
    /// The document's title changed.
    Title { title: String },
    /// A navigation (of the page or one of its frames) was refused by policy.
    Refused { url: String },
}

/// A [`PageEvent`] and the pane it came from.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct PaneEvent {
    pub id: String,
    #[serde(flatten)]
    pub event: PageEvent,
}

/// Why a pane could not be opened or changed.
#[derive(Debug)]
pub enum BrowserError {
    /// The URL is not one a pane may load (see [`policy`]).
    Refused(String),
    /// A pane with this id is already open.
    AlreadyOpen,
    /// [`MAX_PANES`] are open.
    TooMany,
    /// No pane with this id is open.
    NotOpen,
    /// Called off the main thread; AppKit views may only be touched there.
    NotMainThread,
    /// The native window or webview could not be reached or built.
    Native(String),
}

impl std::fmt::Display for BrowserError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Refused(why) => write!(f, "{why}"),
            Self::AlreadyOpen => f.write_str("this browser session is already open"),
            Self::TooMany => write!(
                f,
                "{MAX_PANES} browser sessions are open, the most there can be; close one to open another"
            ),
            Self::NotOpen => f.write_str("this browser session is not open"),
            Self::NotMainThread => {
                f.write_str("a browser session can only be changed on the main thread")
            }
            Self::Native(why) => write!(f, "browser session: {why}"),
        }
    }
}

impl std::error::Error for BrowserError {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ids_are_short_plain_tokens() {
        for ok in ["t1", "tab-12", "a_b", &"x".repeat(64)] {
            assert!(validate_id(ok).is_ok(), "{ok}");
        }
        for bad in ["", &"x".repeat(65), "a b", "a/b", "é", "t1\n", "../x"] {
            assert!(validate_id(bad).is_err(), "{bad:?}");
        }
    }

    #[test]
    fn bounds_must_be_a_plausible_rectangle() {
        let ok = Bounds {
            x: 10.0,
            y: -5.0,
            width: 300.0,
            height: 0.0,
        };
        assert!(ok.validate().is_ok());
        for bad in [
            Bounds { x: f64::NAN, ..ok },
            Bounds {
                y: f64::INFINITY,
                ..ok
            },
            Bounds { width: -1.0, ..ok },
            Bounds { height: 1e9, ..ok },
            Bounds { x: -1e9, ..ok },
        ] {
            assert!(bad.validate().is_err(), "{bad:?}");
        }
    }

    #[test]
    fn page_text_is_capped_on_a_character_boundary() {
        assert_eq!(cap("hello", 3), "hel");
        assert_eq!(cap("hé", 2), "hé");
        assert_eq!(cap("ééé", 2), "éé");
        assert_eq!(cap("", 4), "");
    }

    #[test]
    fn page_events_serialize_flat_for_the_frontend() {
        let event = PaneEvent {
            id: "t1".into(),
            event: PageEvent::Title {
                title: "Home".into(),
            },
        };
        assert_eq!(
            serde_json::to_value(&event).unwrap(),
            serde_json::json!({ "id": "t1", "kind": "title", "title": "Home" })
        );
    }
}
