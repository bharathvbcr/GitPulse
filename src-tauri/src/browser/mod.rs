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
    close, close_all, inspect_point, is_open, open, open_count, set_bounds, set_visible,
    snapshot_png, with_webview, EventSink,
};

use serde::{Deserialize, Serialize};

/// Whether browser sessions are built for this target; reported to the
/// frontend as `HostPlatform::browser_sessions`.
pub const SUPPORTED: bool = cfg!(target_os = "macos");

/// The most panes open at once, across every repository's dock.
pub const MAX_PANES: usize = 8;

/// The event every pane's page activity is emitted as.
pub const EVENT: &str = "browser-pane";

/// The event an agent's request for a browser session reaches the window as
/// (`gitpulse_browser_open` → `desktop::instance::Request::Browse`).
pub const BROWSE_EVENT: &str = "gitpulse-browse";

/// A browser session an agent asked for: repository `repo`'s dock, on `url`.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct BrowseRequest {
    pub repo: String,
    pub url: String,
}

#[cfg(test)]
thread_local! {
    /// A test's own profile to ask instead of the hermetic HOME's, whose
    /// mailbox path is too long for a unix socket.
    pub(crate) static TEST_PROFILE: std::cell::RefCell<Option<std::path::PathBuf>> =
        const { std::cell::RefCell::new(None) };
}

/// The profile directory whose running GitPulse `gitpulse_browser_open` asks.
pub(crate) fn profile_dir() -> Option<std::path::PathBuf> {
    #[cfg(test)]
    if let Some(dir) = TEST_PROFILE.with(|slot| slot.borrow().clone()) {
        return Some(dir);
    }
    crate::tool_config::default_config_dir()
}

/// How long `ask_app_to_browse` waits for the running app's greeting.
#[cfg(target_os = "macos")]
const BROWSE_HELLO: std::time::Duration = std::time::Duration::from_secs(2);

/// Asks the GitPulse that owns profile directory `dir` to open a browser
/// session on `url` in repository `repo`'s dock, through its single-instance
/// mailbox (`desktop::instance`, this user only). `Ok` means the app took the
/// request; the window then opens the repository and the tab. The URL is
/// checked here and again by the app.
pub fn ask_app_to_browse(dir: &std::path::Path, repo: &str, url: &str) -> Result<(), String> {
    policy::check_loopback(url)?;
    #[cfg(target_os = "macos")]
    {
        use crate::desktop::instance::{reach_owner, Request};
        let owner = reach_owner(dir, BROWSE_HELLO)
            .ok_or("GitPulse is not running, so there is no window to open a browser session in")?;
        owner
            .forward(&Request::Browse {
                repo: repo.to_owned(),
                url: url.to_owned(),
            })
            .map_err(|why| {
                format!(
                    "GitPulse did not take the request ({why}). A GitPulse built before browser \
                     sessions refuses it: rebuild and reinstall the app."
                )
            })
    }
    #[cfg(not(target_os = "macos"))]
    {
        let _ = (dir, repo);
        Err(UNSUPPORTED.to_owned())
    }
}

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

/// A viewport screenshot saved to scratch disk together with a `data:image/png;base64,...`
/// preview URL so the frontend can display an interactive inspection overlay while hiding
/// the native `WKWebView`.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct BrowserSnapshotPreview {
    pub path: String,
    pub data_url: String,
}

/// Bounding box of an inspected DOM element in the child webview's CSS viewport coordinates.
#[derive(Clone, Copy, Debug, PartialEq, Deserialize, Serialize)]
pub struct BrowserElementRect {
    pub x: f64,
    pub y: f64,
    pub width: f64,
    pub height: f64,
}

/// Metadata for a DOM element picked at `(x, y)` inside a browser session.
#[derive(Clone, Debug, PartialEq, Deserialize, Serialize)]
pub struct BrowserElementPick {
    pub selector: String,
    pub tag: String,
    pub id: String,
    pub classes: Vec<String>,
    pub text: String,
    pub url: String,
    pub rect: BrowserElementRect,
    pub viewport_width: f64,
    pub viewport_height: f64,
}

/// Validates click coordinates `(x, y)` inside `viewport_width` x `viewport_height` and
/// returns normalized `(norm_x, norm_y)` ratios in `[0.0, 1.0]`.
pub fn validate_inspect_point(
    x: f64,
    y: f64,
    viewport_width: f64,
    viewport_height: f64,
) -> Result<(f64, f64), String> {
    if ![x, y, viewport_width, viewport_height]
        .iter()
        .all(|v| v.is_finite())
    {
        return Err("inspect coordinates must be finite numbers".into());
    }
    if !(1.0..=MAX_EXTENT).contains(&viewport_width)
        || !(1.0..=MAX_EXTENT).contains(&viewport_height)
    {
        return Err("inspect viewport dimensions are out of range".into());
    }
    if x < 0.0 || y < 0.0 || x > viewport_width || y > viewport_height {
        return Err("inspect coordinates fall outside the browser viewport".into());
    }
    Ok((x / viewport_width, y / viewport_height))
}

/// Builds a self-contained one-shot JavaScript IIFE that inspects the element at
/// `(norm_x * innerWidth, norm_y * innerHeight)` without installing any globals.
pub fn inspect_point_script(norm_x: f64, norm_y: f64) -> String {
    let nx = norm_x.clamp(0.0, 1.0);
    let ny = norm_y.clamp(0.0, 1.0);
    format!(
        r##"(() => {{
  const vw = Math.max(1, Number(window.innerWidth || document.documentElement?.clientWidth || 1));
  const vh = Math.max(1, Number(window.innerHeight || document.documentElement?.clientHeight || 1));
  const px = Math.min(vw - 0.5, Math.max(0, {nx:.6} * vw));
  const py = Math.min(vh - 0.5, Math.max(0, {ny:.6} * vh));
  const el = document.elementFromPoint(px, py);
  if (!el) return JSON.stringify(null);
  const tag = String(el.tagName || "element").toLowerCase().slice(0, 48);
  const rawId = typeof el.id === "string" ? el.id.trim().slice(0, 64) : "";
  const classes = Array.from(el.classList || []).map((c) => String(c).trim()).filter(Boolean).slice(0, 6).map((c) => c.slice(0, 64));
  const safeTok = (s) => s.replace(/[^A-Za-z0-9_-]/g, "");
  let selector = safeTok(tag) || "element";
  if (rawId && safeTok(rawId)) selector += "#" + safeTok(rawId);
  for (const cls of classes.slice(0, 3)) {{
    const tok = safeTok(cls);
    if (tok) selector += "." + tok;
  }}
  const aria = typeof el.getAttribute === "function" ? (el.getAttribute("aria-label") || "") : "";
  const inner = typeof el.innerText === "string" ? el.innerText : (el.textContent || "");
  const text = String(inner || aria || "").replace(/\s+/g, " ").trim().slice(0, 200);
  const box = el.getBoundingClientRect();
  return JSON.stringify({{
    selector,
    tag,
    id: rawId,
    classes,
    text,
    url: String(location.href || "").slice(0, 2048),
    rect: {{
      x: Number(box.left) || 0,
      y: Number(box.top) || 0,
      width: Math.max(0, Number(box.width) || 0),
      height: Math.max(0, Number(box.height) || 0)
    }},
    viewport_width: vw,
    viewport_height: vh
  }});
}})()"##
    )
}

#[derive(Deserialize)]
struct RawElementRect {
    #[serde(default)]
    x: f64,
    #[serde(default)]
    y: f64,
    #[serde(default)]
    width: f64,
    #[serde(default)]
    height: f64,
}

#[derive(Deserialize)]
struct RawElementPick {
    #[serde(default)]
    selector: String,
    #[serde(default)]
    tag: String,
    #[serde(default)]
    id: String,
    #[serde(default)]
    classes: Vec<String>,
    #[serde(default)]
    text: String,
    #[serde(default)]
    url: String,
    rect: RawElementRect,
    #[serde(default)]
    viewport_width: f64,
    #[serde(default)]
    viewport_height: f64,
}

fn finite_or_zero(val: f64) -> f64 {
    if val.is_finite() {
        val.clamp(-MAX_EXTENT, MAX_EXTENT)
    } else {
        0.0
    }
}

/// Parses and bounds-caps the untrusted JSON returned by [`inspect_point_script`],
/// accepting both direct JSON objects (`WKContentWorld`) and JSON-quoted strings (`wry`).
pub fn parse_evaluated_element_pick(raw: &str) -> Result<BrowserElementPick, String> {
    let trimmed = raw.trim();
    if trimmed.is_empty() || trimmed == "null" {
        return Err("no DOM element found at the selected point".into());
    }
    let val: serde_json::Value = serde_json::from_str(trimmed)
        .map_err(|e| format!("invalid element inspection payload: {e}"))?;
    let obj_val = match val {
        serde_json::Value::Null => {
            return Err("no DOM element found at the selected point".into());
        }
        serde_json::Value::String(inner) => {
            let inner_trimmed = inner.trim();
            if inner_trimmed.is_empty() || inner_trimmed == "null" {
                return Err("no DOM element found at the selected point".into());
            }
            serde_json::from_str::<serde_json::Value>(inner_trimmed)
                .map_err(|e| format!("invalid inner element inspection payload: {e}"))?
        }
        other => other,
    };
    if obj_val.is_null() {
        return Err("no DOM element found at the selected point".into());
    }
    let parsed: RawElementPick = serde_json::from_value(obj_val)
        .map_err(|e| format!("malformed element inspection payload: {e}"))?;
    let tag = cap(&parsed.tag.trim().to_ascii_lowercase(), 48);
    let selector = cap(parsed.selector.trim(), 128);
    let classes: Vec<String> = parsed
        .classes
        .into_iter()
        .take(8)
        .map(|c| cap(c.trim(), 64))
        .filter(|c| !c.is_empty())
        .collect();
    Ok(BrowserElementPick {
        selector: if selector.is_empty() {
            if tag.is_empty() {
                "element".into()
            } else {
                tag.clone()
            }
        } else {
            selector
        },
        tag: if tag.is_empty() {
            "element".into()
        } else {
            tag
        },
        id: cap(parsed.id.trim(), 64),
        classes,
        text: cap(parsed.text.trim(), 160),
        url: cap(parsed.url.trim(), MAX_URL_CHARS),
        rect: BrowserElementRect {
            x: finite_or_zero(parsed.rect.x),
            y: finite_or_zero(parsed.rect.y),
            width: finite_or_zero(parsed.rect.width).max(0.0),
            height: finite_or_zero(parsed.rect.height).max(0.0),
        },
        viewport_width: finite_or_zero(parsed.viewport_width).max(1.0),
        viewport_height: finite_or_zero(parsed.viewport_height).max(1.0),
    })
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

    #[test]
    fn inspect_point_validates_coordinates_and_caps_untrusted_dom_payloads() {
        assert!(validate_inspect_point(120.0, 80.0, 640.0, 480.0).is_ok());
        assert!(validate_inspect_point(-1.0, 80.0, 640.0, 480.0).is_err());
        assert!(validate_inspect_point(f64::NAN, 80.0, 640.0, 480.0).is_err());
        assert!(validate_inspect_point(10.0, 20.0, 0.0, 480.0).is_err());
        assert!(validate_inspect_point(700.0, 20.0, 640.0, 480.0).is_err());

        let raw_json = serde_json::json!({
            "selector": "button#deploy.cta.primary",
            "tag": "BUTTON",
            "id": "deploy",
            "classes": ["cta", "primary", "x".repeat(300)],
            "text": format!("Deploy now {}", "z".repeat(400)),
            "url": "http://localhost:5173/dashboard",
            "rect": { "x": 12.5, "y": 24.0, "width": 140.0, "height": 38.0 },
            "viewport_width": 800.0,
            "viewport_height": 600.0
        })
        .to_string();

        let direct = parse_evaluated_element_pick(&raw_json).unwrap();
        assert_eq!(direct.tag, "button");
        assert_eq!(direct.selector, "button#deploy.cta.primary");
        assert!(direct.text.chars().count() <= 160);
        assert!(direct.classes[2].chars().count() <= 64);
        assert_eq!(direct.rect.width, 140.0);

        // wry's `evaluate_script_with_callback` wraps JS string results as JSON strings.
        let wry_wrapped = serde_json::to_string(&raw_json).unwrap();
        let unwrapped = parse_evaluated_element_pick(&wry_wrapped).unwrap();
        assert_eq!(unwrapped, direct);

        assert!(parse_evaluated_element_pick("null").is_err());
        assert!(parse_evaluated_element_pick("\"null\"").is_err());
    }
}
