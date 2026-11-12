//! macOS hosting of browser panes: raw wry child webviews over the main
//! window's content, keyed by session id and owned by the main thread. See the
//! module docs in `browser/mod.rs` for why they are raw wry.

use super::{
    cap, policy, Bounds, BrowserError, PageEvent, PaneEvent, MAX_PANES, MAX_TITLE_CHARS,
    MAX_URL_CHARS,
};
use objc2::MainThreadMarker;
use std::cell::RefCell;
use std::collections::HashMap;
use std::sync::Arc;
use wry::dpi::{LogicalPosition, LogicalSize};
use wry::{NewWindowResponse, PageLoadEvent, Rect, WebView, WebViewBuilder};

/// Where a pane reports what its page did.
pub type EventSink = Arc<dyn Fn(PaneEvent) + Send + Sync>;

thread_local! {
    // AppKit views are main-thread only and `WebView` is neither `Send` nor
    // `Sync`, so the panes live in the main thread's own map and every entry
    // point checks it is running there.
    static PANES: RefCell<HashMap<String, WebView>> = RefCell::new(HashMap::new());
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

/// A pane's webview configuration. Deliberately absent: an IPC handler, any
/// custom scheme, and any initialization script (see the module docs). The
/// page-load and title handlers are native delegate callbacks; they put
/// nothing into the page.
fn builder(
    id: &str,
    url: &str,
    bounds: Bounds,
    sink: Option<EventSink>,
) -> WebViewBuilder<'static> {
    let report = move |event: PageEvent, id: &str| {
        if let Some(sink) = &sink {
            sink(PaneEvent {
                id: id.to_owned(),
                event,
            });
        }
    };
    let report = Arc::new(report);
    let (on_nav, nav_id) = (report.clone(), id.to_owned());
    let (on_load, load_id) = (report.clone(), id.to_owned());
    let (on_title, title_id) = (report, id.to_owned());
    WebViewBuilder::new()
        .with_url(url)
        .with_bounds(rect(bounds))
        // A non-persistent WKWebsiteDataStore: nothing shared with GitPulse's
        // own webview or with another pane, nothing written to disk.
        .with_incognito(true)
        .with_devtools(cfg!(debug_assertions))
        // Asked for every frame's navigations, so a remote iframe is refused;
        // the refusal is reported, since it is otherwise silent.
        .with_navigation_handler(move |url| {
            let allowed = policy::allows_navigation(&url);
            if !allowed {
                on_nav(
                    PageEvent::Refused {
                        url: cap(&url, MAX_URL_CHARS),
                    },
                    &nav_id,
                );
            }
            allowed
        })
        .with_on_page_load_handler(move |event, url| {
            let url = cap(&url, MAX_URL_CHARS);
            let event = match event {
                PageLoadEvent::Started => PageEvent::Loading { url },
                PageLoadEvent::Finished => PageEvent::Loaded { url },
            };
            on_load(event, &load_id);
        })
        .with_document_title_changed_handler(move |title| {
            on_title(
                PageEvent::Title {
                    title: cap(&title, MAX_TITLE_CHARS),
                },
                &title_id,
            );
        })
        .with_new_window_req_handler(|_, _| NewWindowResponse::Deny)
    // No download handler: wry cancels a navigation that asks to download
    // when none is installed (`wkwebview/navigation.rs`).
}

/// Opens pane `id` on `window` at `bounds`, showing `url`, reporting its page
/// to `sink`. Main thread only.
pub fn open(
    window: &tauri::WebviewWindow,
    id: &str,
    url: &str,
    bounds: Bounds,
    sink: Option<EventSink>,
) -> Result<(), BrowserError> {
    main_thread()?;
    let url = policy::check_loopback(url).map_err(BrowserError::Refused)?;
    PANES.with(|panes| {
        {
            let panes = panes.borrow();
            if panes.contains_key(id) {
                return Err(BrowserError::AlreadyOpen);
            }
            if panes.len() >= MAX_PANES {
                return Err(BrowserError::TooMany);
            }
        }
        // tao's window handle is the window's *current* content view, read on
        // every call (`tao/src/platform_impl/macos/window.rs`, `ns_view`), so
        // the pane lands beside Tauri's webview even after wry has replaced the
        // content view with its own parent view.
        let webview = builder(id, url.as_str(), bounds, sink)
            .build_as_child(window)
            .map_err(|e| BrowserError::Native(format!("could not build the webview: {e}")))?;
        {
            use wry::WebViewExtMacOS as _;
            let raw = objc2::rc::Retained::as_ptr(&webview.webview()) as *mut std::ffi::c_void;
            // SAFETY: `webview.webview()` is a live `WKWebView` on the AppKit main thread.
            let _ = unsafe { crate::desktop::webview_perf::unlock_wkwebview_120hz(raw) };
        }
        panes.borrow_mut().insert(id.to_owned(), webview);
        Ok(())
    })
}

/// Runs `f` with pane `id`'s webview. Main thread only.
pub fn with_webview<T>(id: &str, f: impl FnOnce(&WebView) -> T) -> Result<T, BrowserError> {
    main_thread()?;
    PANES.with(|panes| panes.borrow().get(id).map(f).ok_or(BrowserError::NotOpen))
}

pub fn is_open(id: &str) -> bool {
    main_thread().is_ok() && PANES.with(|panes| panes.borrow().contains_key(id))
}

/// How many panes are open; 0 off the main thread, which cannot see them.
pub fn open_count() -> usize {
    if main_thread().is_err() {
        return 0;
    }
    PANES.with(|panes| panes.borrow().len())
}

/// Moves or resizes pane `id`. Main thread only.
pub fn set_bounds(id: &str, bounds: Bounds) -> Result<(), BrowserError> {
    with_webview(id, |webview| webview.set_bounds(rect(bounds)))?
        .map_err(|e| BrowserError::Native(format!("could not move the webview: {e}")))
}

/// Shows or hides pane `id` without unloading its page. Main thread only.
pub fn set_visible(id: &str, visible: bool) -> Result<(), BrowserError> {
    with_webview(id, |webview| webview.set_visible(visible))?
        .map_err(|e| BrowserError::Native(format!("could not show or hide the webview: {e}")))
}

/// Removes pane `id` from the window. Main thread only.
pub fn close(id: &str) -> Result<(), BrowserError> {
    main_thread()?;
    // Taken out of the map before it is dropped, so nothing the drop runs can
    // find the map borrowed.
    let webview = PANES.with(|panes| panes.borrow_mut().remove(id));
    match webview {
        Some(webview) => {
            drop(webview);
            Ok(())
        }
        None => Err(BrowserError::NotOpen),
    }
}

/// Removes every pane; returns how many there were. Main thread only.
pub fn close_all() -> Result<usize, BrowserError> {
    main_thread()?;
    let panes: Vec<WebView> =
        PANES.with(|panes| panes.borrow_mut().drain().map(|(_, w)| w).collect());
    let count = panes.len();
    drop(panes);
    Ok(count)
}

/// Captures a PNG screenshot of pane `id`'s current rendered viewport via
/// `-[WKWebView takeSnapshotWithConfiguration:completionHandler:]`, without
/// injecting any script or touching the page's execution context.
///
/// Main thread only; `done` is invoked asynchronously on the AppKit run loop
/// once WebKit finishes compositing the snapshot.
pub fn snapshot_png(
    id: &str,
    done: Box<dyn FnOnce(Result<Vec<u8>, String>) + Send>,
) -> Result<(), BrowserError> {
    use block2::RcBlock;
    use objc2::exception::catch;
    use objc2::rc::Retained;
    use objc2::runtime::AnyObject;
    use objc2::{class, msg_send};
    use objc2_foundation::NSError;
    use std::panic::AssertUnwindSafe;
    use std::sync::Mutex;
    use wry::WebViewExtMacOS as _;

    /// `NSBitmapImageFileTypePNG` (`AppKit/NSBitmapImageRep.h`).
    const NS_BITMAP_IMAGE_FILE_TYPE_PNG: usize = 4;

    let raw_webview = with_webview(id, |webview| {
        objc2::rc::Retained::as_ptr(&webview.webview()) as *mut AnyObject
    })?;
    if raw_webview.is_null() {
        return Err(BrowserError::Native("webview handle is null".into()));
    }

    let slot = Mutex::new(Some(done));
    let block = RcBlock::new(move |image: *mut AnyObject, error: *mut NSError| {
        let Some(deliver) = slot
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .take()
        else {
            return;
        };
        let encoded = catch(AssertUnwindSafe(|| -> Result<Vec<u8>, String> {
            if !error.is_null() {
                let desc = unsafe { (&*error).localizedDescription() }.to_string();
                return Err(format!("snapshot failed: {desc}"));
            }
            if image.is_null() {
                return Err("snapshot returned no image".into());
            }
            unsafe {
                let tiff: *mut AnyObject = msg_send![&*image, TIFFRepresentation];
                if tiff.is_null() {
                    return Err("could not encode snapshot TIFF".into());
                }
                let rep: *mut AnyObject =
                    msg_send![class!(NSBitmapImageRep), imageRepWithData: &*tiff];
                if rep.is_null() {
                    return Err("could not decode snapshot bitmap".into());
                }
                let empty_props: Retained<AnyObject> = msg_send![class!(NSDictionary), new];
                let png_data: *mut AnyObject = msg_send![
                    &*rep,
                    representationUsingType: NS_BITMAP_IMAGE_FILE_TYPE_PNG,
                    properties: &*empty_props
                ];
                if png_data.is_null() {
                    return Err("could not encode snapshot PNG".into());
                }
                let len: usize = msg_send![&*png_data, length];
                let ptr: *const u8 = msg_send![&*png_data, bytes];
                if len == 0 || ptr.is_null() {
                    return Err("snapshot PNG is empty".into());
                }
                Ok(std::slice::from_raw_parts(ptr, len).to_vec())
            }
        }));
        let outcome = match encoded {
            Ok(res) => res,
            Err(Some(exc)) => Err(format!("Objective-C exception encoding snapshot: {exc:?}")),
            Err(None) => Err("Unknown Objective-C exception encoding snapshot".into()),
        };
        deliver(outcome);
    });

    let dispatched = catch(AssertUnwindSafe(|| unsafe {
        let null_cfg: *const AnyObject = std::ptr::null();
        let _: () = msg_send![
            &*raw_webview,
            takeSnapshotWithConfiguration: null_cfg,
            completionHandler: &*block
        ];
    }));
    match dispatched {
        Ok(()) => Ok(()),
        Err(Some(exc)) => Err(BrowserError::Native(format!(
            "Objective-C exception taking snapshot: {exc:?}"
        ))),
        Err(None) => Err(BrowserError::Native(
            "Unknown Objective-C exception taking snapshot".into(),
        )),
    }
}

/// Evaluates [`super::inspect_point_script`] at normalized coordinates `(norm_x, norm_y)`
/// inside `WKContentWorld.defaultClientWorld` (an isolated client JS world that neither
/// installs a `WKUserScript` nor exposes `window.ipc` / page globals), falling back to
/// `wry::WebView::evaluate_script_with_callback` if `WKContentWorld` is unavailable.
///
/// Main thread only.
pub fn inspect_point(
    id: &str,
    norm_x: f64,
    norm_y: f64,
    done: Box<dyn FnOnce(Result<String, String>) + Send>,
) -> Result<(), BrowserError> {
    use block2::RcBlock;
    use objc2::exception::catch;
    use objc2::runtime::{AnyClass, AnyObject};
    use objc2::{class, msg_send};
    use objc2_foundation::{NSError, NSString};
    use std::ffi::CStr;
    use std::panic::AssertUnwindSafe;
    use std::sync::Mutex;
    use wry::WebViewExtMacOS as _;

    let script = super::inspect_point_script(norm_x, norm_y);

    if let Some(world_cls) = AnyClass::get(c"WKContentWorld") {
        let raw_webview = with_webview(id, |webview| {
            objc2::rc::Retained::as_ptr(&webview.webview()) as *mut AnyObject
        })?;
        if raw_webview.is_null() {
            return Err(BrowserError::Native("webview handle is null".into()));
        }

        let slot = Mutex::new(Some(done));
        let block = RcBlock::new(move |result: *mut AnyObject, error: *mut NSError| {
            let Some(deliver) = slot
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .take()
            else {
                return;
            };
            let parsed = catch(AssertUnwindSafe(|| -> Result<String, String> {
                if !error.is_null() {
                    let desc = unsafe { (&*error).localizedDescription() }.to_string();
                    return Err(format!("element inspection failed: {desc}"));
                }
                if result.is_null() {
                    return Err("no DOM element found at the selected point".into());
                }
                unsafe {
                    let is_str: bool = msg_send![&*result, isKindOfClass: class!(NSString)];
                    if !is_str {
                        return Err("no DOM element found at the selected point".into());
                    }
                    let utf8: *const std::ffi::c_char = msg_send![&*result, UTF8String];
                    if utf8.is_null() {
                        return Err("element inspection returned invalid UTF-8".into());
                    }
                    Ok(CStr::from_ptr(utf8).to_string_lossy().into_owned())
                }
            }));
            let outcome = match parsed {
                Ok(res) => res,
                Err(Some(exc)) => Err(format!(
                    "Objective-C exception reading inspected element: {exc:?}"
                )),
                Err(None) => Err("Unknown Objective-C exception reading inspected element".into()),
            };
            deliver(outcome);
        });

        let js_ns = NSString::from_str(&script);
        let dispatched = catch(AssertUnwindSafe(|| unsafe {
            let client_world: *mut AnyObject = msg_send![world_cls, defaultClientWorld];
            if client_world.is_null() {
                return Err("WKContentWorld.defaultClientWorld returned null".to_owned());
            }
            let null_frame: *const AnyObject = std::ptr::null();
            let _: () = msg_send![
                &*raw_webview,
                evaluateJavaScript: &*js_ns,
                inFrame: null_frame,
                inContentWorld: &*client_world,
                completionHandler: &*block
            ];
            Ok(())
        }));
        return match dispatched {
            Ok(Ok(())) => Ok(()),
            Ok(Err(why)) => Err(BrowserError::Native(why)),
            Err(Some(exc)) => Err(BrowserError::Native(format!(
                "Objective-C exception inspecting element: {exc:?}"
            ))),
            Err(None) => Err(BrowserError::Native(
                "Unknown Objective-C exception inspecting element".into(),
            )),
        };
    }

    let slot = Mutex::new(Some(done));
    with_webview(id, |webview| {
        webview.evaluate_script_with_callback(&script, move |raw| {
            if let Some(deliver) = slot
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .take()
            {
                deliver(Ok(raw));
            }
        })
    })?
    .map_err(|e| BrowserError::Native(format!("could not inspect element: {e}")))
}
