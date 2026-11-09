//! The browser pane: a web page shown inside GitPulse's main window, for a
//! developer and their agents to look at the UI they are building (M0 of the
//! agent-integrated browser, board task `gp-browser-m0-hosting-spike`).
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
//! view. The pane is still parented correctly through the Tauri window's
//! handle because tao reads the *current* content view on every call
//! (`framework/tao/src/platform_impl/macos/window.rs`, `ns_view`), not the
//! view it created; the isolation test asserts the pane's superview is that
//! content view.
//!
//! # Policy
//!
//! - Loopback only: `http`/`https` to `localhost`, `127.0.0.0/8` or `::1`, and
//!   `about:blank`. wry asks the navigation handler for every frame's
//!   navigations, so a remote iframe is refused too. Subresources (`fetch`,
//!   images) are not navigations and are not filtered here.
//! - No new windows, no downloads.
//! - A non-persistent data store, so a page on the app's own dev origin does
//!   not share GitPulse's localStorage or cookies.
//!
//! macOS only for now; the Linux and Windows hosting paths are unverified.

pub mod policy;

#[cfg(target_os = "macos")]
mod pane;

#[cfg(target_os = "macos")]
pub use pane::{close, is_open, open, open_from_env, set_bounds, set_visible, with_webview};

/// Where the pane sits, in logical points from the top-left of the window's
/// content area.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Bounds {
    pub x: f64,
    pub y: f64,
    pub width: f64,
    pub height: f64,
}

/// Why the pane could not be opened or changed.
#[derive(Debug)]
pub enum BrowserError {
    /// The URL is not one the pane may load (see [`policy`]).
    Refused(String),
    /// A pane is already open; M0 hosts one.
    AlreadyOpen,
    /// No pane is open.
    NotOpen,
    /// Called off the main thread; AppKit views may only be touched there.
    NotMainThread,
    /// The native window or webview could not be reached or built.
    Native(String),
}

impl std::fmt::Display for BrowserError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Refused(why) => write!(f, "browser pane refused the URL: {why}"),
            Self::AlreadyOpen => f.write_str("a browser pane is already open"),
            Self::NotOpen => f.write_str("no browser pane is open"),
            Self::NotMainThread => {
                f.write_str("the browser pane can only be changed on the main thread")
            }
            Self::Native(why) => write!(f, "browser pane: {why}"),
        }
    }
}

impl std::error::Error for BrowserError {}
