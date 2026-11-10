//! The browser pane's page cannot reach GitPulse's commands, on AppKit's real
//! main thread, against a positive control that can.
//!
//! A real Tauri app is built from GitPulse's own context and main-window
//! config, with `devUrl` pointed at a loopback test server, so Tauri treats
//! that origin as the app's own (`is_local_url`), which is the dev-build case
//! that makes a Tauri-managed child webview dangerous. Then:
//!
//! 1. **Control.** Tauri's webview in the main window loads `/main` from that
//!    origin. It must see `__TAURI_INTERNALS__`, and an invoke of a missing
//!    command must come back rejected *by Rust*, so the bridge is live.
//! 2. **Same origin.** `browser::open` puts the pane in the same window on the
//!    same origin. Every route to Rust must be absent or fail: the internals
//!    script, `window.ipc`, the `ipc` script-message handler, and a direct
//!    `fetch("ipc://localhost/...")`. Its storage must not see what the
//!    control page wrote.
//! 3. **Hierarchy.** The pane's view is in the window, under its current
//!    content view, at the bounds asked for; it moves, hides and shows.
//! 4. **Another origin.** The pane navigates to a second loopback server that
//!    is not `devUrl` and must still have no route. (The pane refuses
//!    non-loopback URLs, so a real remote origin cannot be loaded here.)
//! 5. **Policy wiring.** A navigation to a non-loopback URL is cancelled, and
//!    reported as refused. The page's loads and title were reported too.
//! 6. **Close.** The view leaves the window and is deallocated, and repeated
//!    open/close cycles leave no views behind.
//! 7. **Two at once.** Two panes, on the devUrl origin and another, are both
//!    in the window and both without a route; ids are unique and the count is
//!    capped at `MAX_PANES`; `close_all` (what a main-page reload runs) takes
//!    every one back out.
//! 8. **Coordinates.** One CSS pixel of the main page times its zoom is one
//!    point of the content area the panes are placed in, at zoom 1 and 1.25,
//!    which is what the frontend's placement assumes.
//! 9. **Reload.** A main-page reload closes every pane, through GitPulse's own
//!    `desktop::handle_page_load`.
//! 10. **The real commands.** `cmd_browser_*`, registered as `lib.rs` does,
//!     are called from the main page through Tauri's IPC under GitPulse's own
//!     capability grants, with a `tab-N` id: open, move, hide, navigate and
//!     reload work; a remote URL, an unknown step, a bad id, impossible bounds
//!     and an unknown session are refused with the reason; the page events
//!     arrive through the event plugin; close is idempotent.
//! 11. **An agent's request.** A `Request::Browse`, handed to the app's own
//!     `desktop::answer_forwarded` as the single-instance mailbox does, reaches
//!     the page as the `gitpulse-browse` event the frontend listens for.
//!
//! The test builds without the `custom-protocol` feature, so Tauri runs in
//! its dev mode (`tauri::is_dev()`), the mode in which `devUrl` is Local. The
//! release mode is not exercised here; nothing in `browser::open` depends on
//! the build mode (`browser/pane.rs`).

#[path = "../src/test_support/env.rs"]
mod hermetic_env;

#[path = "common/nextest_list.rs"]
mod nextest_list;

#[cfg(not(target_os = "macos"))]
fn main() {
    if nextest_list::answer_list("browser_pane_isolation", false) {
        return;
    }
    println!("SKIP browser_pane_isolation: the browser pane is macOS-only");
}

#[cfg(target_os = "macos")]
fn main() {
    if nextest_list::answer_list("browser_pane_isolation", true) {
        return;
    }
    mac::run();
}

#[cfg(target_os = "macos")]
mod mac {
    use gitpulse_lib::browser::{self, Bounds, EventSink, PageEvent, PaneEvent};
    use objc2::msg_send;
    use objc2::rc::Weak;
    use objc2::runtime::AnyObject;
    use serde_json::Value;
    use std::cell::RefCell;
    use std::io::{Read, Write};
    use std::net::{TcpListener, TcpStream};
    use std::sync::mpsc::{self, Receiver, Sender};
    use std::sync::{Arc, Mutex};
    use std::time::{Duration, Instant};
    use tauri::{AppHandle, Manager, RunEvent};
    use wry::WebViewExtMacOS;

    const DEADLINE: Duration = Duration::from_secs(120);
    /// The pane most steps act on.
    const A: &str = "pane-a";
    const B: &str = "pane-b";
    const REPORT_WAIT: Duration = Duration::from_secs(20);

    /// One page for every probe; `data-who` says which side it is.
    const PROBE: &str = r#"<script>
(async () => {
  const who = document.documentElement.dataset.who;
  if (who === 'main') { try { localStorage.setItem('gp-browser-probe', 'main-wrote'); } catch (e) {} }
  const r = { who, origin: location.origin };
  r.internals = typeof window.__TAURI_INTERNALS__;
  r.ipc = typeof window.ipc;
  r.handler = typeof (window.webkit && window.webkit.messageHandlers && window.webkit.messageHandlers.ipc);
  try {
    await window.__TAURI_INTERNALS__.invoke('browser_pane_probe_missing', {});
    r.invoke = 'resolved';
  } catch (e) {
    r.invoke = (window.__TAURI_INTERNALS__ ? 'rejected: ' : 'unreachable: ') + String((e && e.message) || e);
  }
  try {
    const res = await fetch('ipc://localhost/browser_pane_probe_missing', {
      method: 'POST', body: '{}', headers: { 'Content-Type': 'application/json' } });
    r.ipcFetch = 'reached ' + res.status;
  } catch (e) { r.ipcFetch = 'failed: ' + String(e); }
  try { r.storage = localStorage.getItem('gp-browser-probe'); } catch (e) { r.storage = 'error: ' + e; }
  await fetch('/report', { method: 'POST', body: JSON.stringify(r) });
})();
</script>"#;

    fn page(who: &str) -> String {
        format!("<!doctype html><html data-who={who}><meta charset=utf-8><title>{who}</title>{PROBE}</html>")
    }

    /// A loopback HTTP server: `/main` and `/child` serve the probe, a POST to
    /// `/report` hands its body to the receiver.
    fn serve() -> (u16, Receiver<String>) {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind a loopback port");
        let port = listener.local_addr().unwrap().port();
        let (sent, received) = mpsc::channel();
        std::thread::spawn(move || {
            for stream in listener.incoming().flatten() {
                let sent = sent.clone();
                std::thread::spawn(move || answer(stream, sent));
            }
        });
        (port, received)
    }

    fn answer(mut stream: TcpStream, reports: Sender<String>) {
        let _ = stream.set_read_timeout(Some(Duration::from_secs(5)));
        let mut buf = Vec::new();
        let mut chunk = [0u8; 4096];
        let head_end = loop {
            if let Some(i) = buf.windows(4).position(|w| w == b"\r\n\r\n") {
                break i + 4;
            }
            match stream.read(&mut chunk) {
                Ok(0) | Err(_) => return,
                Ok(n) => buf.extend_from_slice(&chunk[..n]),
            }
            if buf.len() > 64 * 1024 {
                return;
            }
        };
        let head = String::from_utf8_lossy(&buf[..head_end]).into_owned();
        let mut first = head.lines().next().unwrap_or_default().split(' ');
        let (method, path) = (
            first.next().unwrap_or_default(),
            first.next().unwrap_or_default(),
        );
        let length = head
            .lines()
            .find_map(|line| {
                let (name, value) = line.split_once(':')?;
                name.eq_ignore_ascii_case("content-length")
                    .then(|| value.trim().parse::<usize>().ok())?
            })
            .unwrap_or(0)
            .min(1024 * 1024);
        let mut body = buf[head_end..].to_vec();
        while body.len() < length {
            match stream.read(&mut chunk) {
                Ok(0) | Err(_) => break,
                Ok(n) => body.extend_from_slice(&chunk[..n]),
            }
        }
        let (status, content) = match (method, path) {
            ("GET", "/main") => ("200 OK", page("main")),
            ("GET", "/child") => ("200 OK", page("child")),
            ("POST", "/report") => {
                let _ = reports.send(String::from_utf8_lossy(&body).into_owned());
                ("204 No Content", String::new())
            }
            _ => ("404 Not Found", String::new()),
        };
        let _ = write!(
            stream,
            "HTTP/1.1 {status}\r\nContent-Type: text/html; charset=utf-8\r\nCache-Control: no-store\r\n\
             Content-Length: {}\r\nConnection: close\r\n\r\n{content}",
            content.len()
        );
    }

    fn report(from: &Receiver<String>, who: &str) -> Result<Value, String> {
        let deadline = Instant::now() + REPORT_WAIT;
        loop {
            let left = deadline.saturating_duration_since(Instant::now());
            let text = from
                .recv_timeout(left)
                .map_err(|_| format!("no report from the {who} page within {REPORT_WAIT:?}"))?;
            let value: Value = serde_json::from_str(&text)
                .map_err(|e| format!("{who} report is not JSON: {e}: {text}"))?;
            if value["who"] == who {
                return Ok(value);
            }
        }
    }

    /// Runs `f` on the main thread and waits for its answer.
    fn on_main<T: Send + 'static>(
        app: &AppHandle,
        f: impl FnOnce(&AppHandle) -> T + Send + 'static,
    ) -> T {
        let (sent, received) = mpsc::channel();
        let handle = app.clone();
        app.run_on_main_thread(move || {
            let _ = sent.send(f(&handle));
        })
        .expect("schedule on the main thread");
        received
            .recv_timeout(Duration::from_secs(20))
            .expect("the main thread answered")
    }

    fn content_view(window: &tauri::WebviewWindow) -> *mut AnyObject {
        let ns_window = window
            .ns_window()
            .expect("native window")
            .cast::<AnyObject>();
        unsafe { msg_send![ns_window, contentView] }
    }

    fn subview_count(view: *mut AnyObject) -> usize {
        unsafe {
            let subviews: *mut AnyObject = msg_send![view, subviews];
            msg_send![subviews, count]
        }
    }

    /// A pane's native view as a plain object pointer.
    fn pane_view(id: &str) -> Result<*mut AnyObject, String> {
        browser::with_webview(id, |webview| {
            objc2::rc::Retained::as_ptr(&webview.webview()) as *mut AnyObject
        })
        .map_err(|e| e.to_string())
    }

    thread_local! {
        static DEPARTED: RefCell<Option<Weak<AnyObject>>> = const { RefCell::new(None) };
    }

    struct Check {
        failures: Vec<String>,
    }

    impl Check {
        fn that(&mut self, ok: bool, what: impl Into<String>) {
            let what = what.into();
            if ok {
                println!("  ok   {what}");
            } else {
                println!("  FAIL {what}");
                self.failures.push(what);
            }
        }
        fn fact(&self, what: impl AsRef<str>) {
            println!("  note {}", what.as_ref());
        }
    }

    fn no_route(check: &mut Check, side: &str, r: &Value) {
        check.that(
            r["internals"] == "undefined",
            format!(
                "{side}: __TAURI_INTERNALS__ is undefined (got {})",
                r["internals"]
            ),
        );
        check.that(
            r["ipc"] == "undefined",
            format!("{side}: window.ipc is undefined (got {})", r["ipc"]),
        );
        check.that(
            r["handler"] == "undefined",
            format!(
                "{side}: webkit.messageHandlers.ipc is undefined (got {})",
                r["handler"]
            ),
        );
        check.that(
            r["invoke"]
                .as_str()
                .is_some_and(|s| s.starts_with("unreachable")),
            format!("{side}: invoke has no route (got {})", r["invoke"]),
        );
        check.that(
            r["ipcFetch"]
                .as_str()
                .is_some_and(|s| s.starts_with("failed")),
            format!(
                "{side}: fetch(ipc://localhost/...) fails (got {})",
                r["ipcFetch"]
            ),
        );
    }

    fn scenario(
        app: &AppHandle,
        main_reports: &Receiver<String>,
        other_port: u16,
        other_reports: &Receiver<String>,
        dev_origin: &str,
    ) -> Check {
        let mut check = Check {
            failures: Vec::new(),
        };
        check.fact(format!("tauri::is_dev() = {}", tauri::is_dev()));

        // 1. Control: Tauri's own webview reaches Rust.
        match report(main_reports, "main") {
            Ok(r) => {
                check.fact(format!("control page: {r}"));
                check.that(
                    r["internals"] == "object",
                    "control: Tauri's webview has __TAURI_INTERNALS__",
                );
                check.that(
                    r["invoke"].as_str().is_some_and(|s| {
                        s.starts_with("rejected") && s.contains("browser_pane_probe_missing")
                    }),
                    format!(
                        "control: an invoke reaches Rust and is rejected there (got {})",
                        r["invoke"]
                    ),
                );
            }
            Err(e) => check.that(false, format!("control: {e}")),
        }

        // 2. The pane, same window, same (devUrl) origin. Its page events are
        //    collected for step 5.
        let events: Arc<Mutex<Vec<PaneEvent>>> = Arc::default();
        let collected = events.clone();
        let sink: EventSink = Arc::new(move |event| collected.lock().unwrap().push(event));
        let child_url = format!("{dev_origin}child");
        let opened = on_main(app, move |app| {
            let main = app.get_webview_window("main").expect("main window");
            let baseline = subview_count(content_view(&main));
            browser::open(
                &main,
                A,
                &child_url,
                Bounds {
                    x: 40.0,
                    y: 30.0,
                    width: 320.0,
                    height: 240.0,
                },
                Some(sink),
            )
            .map(|()| baseline)
            .map_err(|e| e.to_string())
        });
        let baseline = match opened {
            Ok(baseline) => baseline,
            Err(e) => {
                check.that(false, format!("browser::open on the devUrl origin: {e}"));
                return check;
            }
        };
        match report(main_reports, "child") {
            Ok(r) => {
                check.fact(format!("pane page (devUrl origin): {r}"));
                check.that(
                    r["origin"].as_str() == Some(dev_origin.trim_end_matches('/')),
                    "pane: loaded on the devUrl origin",
                );
                no_route(&mut check, "pane on devUrl origin", &r);
                check.that(
                    r["storage"].is_null(),
                    format!(
                        "pane: does not see the app's localStorage (got {})",
                        r["storage"]
                    ),
                );
            }
            Err(e) => check.that(false, format!("pane: {e}")),
        }

        // 3. Hierarchy, bounds, visibility.
        let facts = on_main(app, |app| {
            let main = app.get_webview_window("main").expect("main window");
            let content = content_view(&main);
            let ns_window = main.ns_window().unwrap().cast::<AnyObject>();
            let view = pane_view(A)?;
            let (in_window, superview): (*mut AnyObject, *mut AnyObject) =
                unsafe { (msg_send![view, window], msg_send![view, superview]) };
            let bounds = browser::with_webview(A, |w| w.bounds())
                .map_err(|e| e.to_string())?
                .map_err(|e| e.to_string())?;
            let at = (
                bounds.position.to_logical::<f64>(1.0),
                bounds.size.to_logical::<f64>(1.0),
            );
            browser::set_bounds(
                A,
                Bounds {
                    x: 10.0,
                    y: 20.0,
                    width: 200.0,
                    height: 150.0,
                },
            )
            .map_err(|e| e.to_string())?;
            let moved = browser::with_webview(A, |w| w.bounds())
                .map_err(|e| e.to_string())?
                .map_err(|e| e.to_string())?;
            let moved = (
                moved.position.to_logical::<f64>(1.0),
                moved.size.to_logical::<f64>(1.0),
            );
            browser::set_visible(A, false).map_err(|e| e.to_string())?;
            let hidden: bool = unsafe { msg_send![view, isHidden] };
            browser::set_visible(A, true).map_err(|e| e.to_string())?;
            let shown: bool = unsafe { !msg_send![view, isHidden] };
            Ok::<_, String>((
                in_window == ns_window,
                superview == content,
                (at.0.x, at.0.y, at.1.width, at.1.height),
                (moved.0.x, moved.0.y, moved.1.width, moved.1.height),
                hidden,
                shown,
            ))
        });
        match facts {
            Ok((in_window, under_content, at, moved, hidden, shown)) => {
                check.that(
                    in_window,
                    "hierarchy: the pane's view is in the main window",
                );
                check.that(
                    under_content,
                    "hierarchy: its superview is the window's current content view",
                );
                check.that(
                    at == (40.0, 30.0, 320.0, 240.0),
                    format!("bounds: opened at 40,30 320x240 (got {at:?})"),
                );
                check.that(
                    moved == (10.0, 20.0, 200.0, 150.0),
                    format!("bounds: moved to 10,20 200x150 (got {moved:?})"),
                );
                check.that(hidden, "visibility: set_visible(false) hides the view");
                check.that(shown, "visibility: set_visible(true) shows it again");
            }
            Err(e) => check.that(false, format!("hierarchy: {e}")),
        }

        // 4. A second loopback origin that is not devUrl.
        let other = format!("http://127.0.0.1:{other_port}/child");
        let loaded = on_main(app, move |_| {
            browser::with_webview(A, |w| w.load_url(&other))
                .map_err(|e| e.to_string())?
                .map_err(|e| e.to_string())
        });
        check.that(
            loaded.is_ok(),
            format!("navigate to the second origin: {loaded:?}"),
        );
        match report(other_reports, "child") {
            Ok(r) => {
                check.fact(format!("pane page (second origin): {r}"));
                no_route(&mut check, "pane on a non-devUrl origin", &r);
            }
            Err(e) => check.that(false, format!("pane on the second origin: {e}")),
        }

        // 5. The navigation handler is wired. A `data:` page needs no network
        //    and commits at once when allowed, so a document that never
        //    becomes it within the wait was cancelled, not slow (removing the
        //    handler turns this red). The document's title is the signal, not
        //    the webview's URL: WebKit reports the provisional URL while the
        //    policy decision is pending, so the URL flickers to `data:` even
        //    when the navigation is then cancelled.
        let title = |app: &AppHandle| {
            let (sent, received) = mpsc::channel();
            let scheduled = on_main(app, move |_| {
                browser::with_webview(A, |w| {
                    w.evaluate_script_with_callback("document.title", move |title| {
                        let _ = sent.send(title);
                    })
                })
                .is_ok_and(|r| r.is_ok())
            });
            scheduled
                .then(|| received.recv_timeout(Duration::from_secs(2)).ok())
                .flatten()
        };
        let before = title(app);
        let _ = on_main(app, |_| {
            browser::with_webview(A, |w| w.load_url("data:text/html,<title>refused</title>"))
                .map(|_| ())
        });
        let committed = crate::hermetic_env::wait_until(Duration::from_millis(1500), || {
            title(app).is_some_and(|t| t.contains("refused"))
        });
        let after = title(app);
        check.that(
            !committed && before.is_some() && before == after,
            format!("policy: a non-loopback (data:) navigation is cancelled (title before {before:?}, after {after:?})"),
        );
        let seen = events.lock().unwrap().clone();
        let of_a =
            |want: &dyn Fn(&PageEvent) -> bool| seen.iter().any(|e| e.id == A && want(&e.event));
        check.that(
            of_a(&|e| matches!(e, PageEvent::Refused { url } if url.starts_with("data:"))),
            "events: the refused navigation was reported as refused",
        );
        check.that(
            of_a(&|e| matches!(e, PageEvent::Loaded { url } if url.ends_with(&format!("{other_port}/child")))),
            "events: the second origin's load was reported as loaded",
        );
        check.that(
            of_a(&|e| matches!(e, PageEvent::Loading { .. })),
            "events: a load was reported as started",
        );
        check.that(
            of_a(&|e| matches!(e, PageEvent::Title { title } if title == "child")),
            "events: the page's title was reported",
        );
        check.fact(format!("{} page events from pane A", seen.len()));

        // 6. Close: the view leaves the window; cycles leave nothing behind.
        let closed = on_main(app, move |app| {
            let main = app.get_webview_window("main").expect("main window");
            let view = pane_view(A)?;
            DEPARTED.with(|slot| *slot.borrow_mut() = Some(Weak::new(unsafe { &*view })));
            browser::close(A).map_err(|e| e.to_string())?;
            let superview: *mut AnyObject = unsafe { msg_send![view, superview] };
            let after_close = subview_count(content_view(&main));
            Ok::<_, String>((superview.is_null(), after_close))
        });
        match closed {
            Ok((detached, after_close)) => {
                check.that(detached, "close: the view is removed from its superview");
                check.that(after_close == baseline, format!("close: the content view is back to {baseline} subviews (got {after_close})"));
            }
            Err(e) => check.that(false, format!("close: {e}")),
        }
        let deallocated = crate::hermetic_env::wait_until(Duration::from_secs(5), || {
            on_main(app, |_| {
                DEPARTED.with(|slot| slot.borrow().as_ref().is_some_and(|w| w.load().is_none()))
            })
        });
        check.that(
            deallocated,
            "close: the native WKWebView is deallocated (its weak reference clears within 5 s)",
        );

        // about:blank, so no cycle's page can post a report that a later step
        // would take for its own.
        let cycles = 5;
        let url = "about:blank";
        let counts = on_main(app, move |app| {
            let main = app.get_webview_window("main").expect("main window");
            let content = content_view(&main);
            let mut counts = Vec::new();
            for _ in 0..cycles {
                browser::open(&main, "cycle", url, SMALL, None).map_err(|e| e.to_string())?;
                browser::close("cycle").map_err(|e| e.to_string())?;
                counts.push(subview_count(content));
            }
            Ok::<_, String>(counts)
        });
        match counts {
            Ok(counts) => check.that(
                counts.iter().all(|&n| n == baseline),
                format!("close: {cycles} open/close cycles leave {baseline} subviews each time (got {counts:?})"),
            ),
            Err(e) => check.that(false, format!("open/close cycles: {e}")),
        }
        check.that(
            on_main(app, |_| browser::open_count()) == 0,
            "close: no pane is open after the cycles",
        );

        // 7. Two panes at once, on the devUrl origin and on another.
        let (url_a, url_b) = (
            format!("{dev_origin}child"),
            format!("http://127.0.0.1:{other_port}/child"),
        );
        let both = on_main(app, move |app| {
            let main = app.get_webview_window("main").expect("main window");
            browser::open(&main, A, &url_a, SMALL, None).map_err(|e| e.to_string())?;
            browser::open(&main, B, &url_b, Bounds { x: 120.0, ..SMALL }, None)
                .map_err(|e| e.to_string())?;
            let content = content_view(&main);
            let (a, b) = (pane_view(A)?, pane_view(B)?);
            let (above_a, above_b): (*mut AnyObject, *mut AnyObject) =
                unsafe { (msg_send![a, superview], msg_send![b, superview]) };
            Ok::<_, String>((
                above_a == content && above_b == content && a != b,
                subview_count(content),
            ))
        });
        match both {
            Ok((placed, count)) => {
                check.that(
                    placed,
                    "two panes: both are distinct views under the content view",
                );
                check.that(
                    count == baseline + 2,
                    format!(
                        "two panes: the content view holds {} subviews (got {count})",
                        baseline + 2
                    ),
                );
            }
            Err(e) => check.that(false, format!("two panes: {e}")),
        }
        for (side, reports, who) in [
            ("pane A (devUrl origin)", main_reports, "child"),
            ("pane B (second origin)", other_reports, "child"),
        ] {
            match report(reports, who) {
                Ok(r) => no_route(&mut check, &format!("two panes, {side}"), &r),
                Err(e) => check.that(false, format!("two panes, {side}: {e}")),
            }
        }

        // The id is unique and the count is capped.
        let refusals = on_main(app, move |app| {
            let main = app.get_webview_window("main").expect("main window");
            let duplicate = browser::open(&main, A, "about:blank", SMALL, None);
            let mut opened = browser::open_count();
            let mut index = 0;
            while opened < browser::MAX_PANES {
                browser::open(&main, &format!("cap-{index}"), "about:blank", SMALL, None)
                    .map_err(|e| e.to_string())?;
                index += 1;
                opened = browser::open_count();
            }
            let over = browser::open(&main, "one-too-many", "about:blank", SMALL, None);
            Ok::<_, String>((
                matches!(duplicate, Err(browser::BrowserError::AlreadyOpen)),
                matches!(over, Err(browser::BrowserError::TooMany)),
                opened,
            ))
        });
        match refusals {
            Ok((duplicate, over, opened)) => {
                check.that(duplicate, "ids: a second pane under an open id is refused");
                check.that(over, format!("cap: a pane past {opened} open is refused"));
            }
            Err(e) => check.that(false, format!("cap: {e}")),
        }

        // What a main-page reload runs (`desktop::handle_page_load`).
        let cleared = on_main(app, move |app| {
            let main = app.get_webview_window("main").expect("main window");
            let closed = browser::close_all().map_err(|e| e.to_string())?;
            Ok::<_, String>((
                closed,
                browser::open_count(),
                subview_count(content_view(&main)),
            ))
        });
        match cleared {
            Ok((closed, left, count)) => check.that(
                closed == browser::MAX_PANES && left == 0 && count == baseline,
                format!("close_all: closed {closed} of {} panes, {left} left, {count} subviews (want {baseline})", browser::MAX_PANES),
            ),
            Err(e) => check.that(false, format!("close_all: {e}")),
        }

        // 8. The frontend places a pane at its CSS rectangle times the page
        //    zoom (`browserSession.toWindowPoints`). That holds only if one CSS
        //    pixel of the main page, times its zoom, is one point of the
        //    window's content area, which the pane is positioned in. Measured
        //    here on GitPulse's own window config, at no zoom and at 1.25.
        let content = on_main(app, |app| {
            let main = app.get_webview_window("main").expect("main window");
            let scale = main.scale_factor().map_err(|e| e.to_string())?;
            let size = main
                .inner_size()
                .map_err(|e| e.to_string())?
                .to_logical::<f64>(scale);
            Ok::<_, String>((size.width, size.height))
        });
        for zoom in [1.0_f64, 1.25] {
            let who = format!("zoom-{zoom}");
            let asked = who.clone();
            let sent = on_main(app, move |app| {
                let main = app.get_webview_window("main").expect("main window");
                main.set_zoom(zoom).map_err(|e| e.to_string())?;
                main.eval(format!(
                    "setTimeout(() => fetch('/report', {{ method: 'POST', body: JSON.stringify({{ who: '{asked}', w: innerWidth, h: innerHeight }}) }}), 300)"
                ))
                .map_err(|e| e.to_string())
            });
            match (&content, sent.and_then(|()| report(main_reports, &who))) {
                (Ok((width, height)), Ok(r)) => {
                    let (w, h) = (
                        r["w"].as_f64().unwrap_or(-1.0),
                        r["h"].as_f64().unwrap_or(-1.0),
                    );
                    check.that(
                        (w * zoom - width).abs() <= 1.0 && (h * zoom - height).abs() <= 1.0,
                        format!("zoom {zoom}: the main page's {w}x{h} CSS px × {zoom} is the {width}x{height}-point content area"),
                    );
                }
                (Err(e), _) => check.that(false, format!("zoom {zoom}: {e}")),
                (_, Err(e)) => check.that(false, format!("zoom {zoom}: {e}")),
            }
        }

        // 9. A main-page reload closes every pane, through GitPulse's own
        //    page-load handler (installed on this app, as `lib.rs` does).
        let reloaded = on_main(app, |app| {
            let main = app.get_webview_window("main").expect("main window");
            browser::open(&main, "before-reload", "about:blank", SMALL, None)
                .map_err(|e| e.to_string())?;
            let open = browser::open_count();
            main.eval("location.reload()").map_err(|e| e.to_string())?;
            Ok::<_, String>(open)
        });
        let closed = crate::hermetic_env::wait_until(Duration::from_secs(10), || {
            on_main(app, |_| browser::open_count()) == 0
        });
        check.that(
            reloaded == Ok(1) && closed,
            format!("reload: the pane open before a main-page reload is closed by it (open before: {reloaded:?}, all closed: {closed})"),
        );

        // 10. The real commands, called the way the frontend calls them:
        //     through Tauri's IPC from the main page, under GitPulse's own
        //     capability grants, with an id in the shape `tabs.ts` makes, and
        //     with the page events heard through the event plugin.
        let child = format!("http://127.0.0.1:{other_port}/child");
        let script = format!(
            r#"(async () => {{
  const inv = window.__TAURI_INTERNALS__.invoke;
  const r = {{ who: 'ipc', events: [] }};
  const handler = window.__TAURI_INTERNALS__.transformCallback((e) => r.events.push(e.payload));
  await inv('plugin:event|listen', {{ event: 'browser-pane', target: {{ kind: 'Any' }}, handler }});
  const step = async (name, cmd, args) => {{
    try {{ await inv(cmd, args); r[name] = 'ok'; }} catch (e) {{ r[name] = 'error: ' + String((e && e.message) || e); }}
  }};
  const id = 'tab-7';
  await step('open', 'cmd_browser_open', {{ id, url: '{child}', bounds: {{ x: 5, y: 6, width: 210, height: 120 }} }});
  await new Promise((done) => setTimeout(done, 1500));
  await step('bounds', 'cmd_browser_set_bounds', {{ id, bounds: {{ x: 7, y: 8, width: 200, height: 100 }} }});
  await step('hide', 'cmd_browser_set_visible', {{ id, visible: false }});
  await step('navigate', 'cmd_browser_navigate', {{ id, url: '{child}?again' }});
  // navigate returns once the load is requested. A reload issued while it is
  // still provisional cancels it and reloads `/child` (seen at load ~50,
  // 2026-11-10), so wait, bounded, for the navigated page to finish loading.
  const landed = (e) => e.id === id && e.kind === 'loaded' && String(e.url).endsWith('?again');
  for (let waited = 0; waited < 10000 && !r.events.some(landed); waited += 50) {{
    await new Promise((done) => setTimeout(done, 50));
  }}
  await step('remote', 'cmd_browser_navigate', {{ id, url: 'https://example.com/' }});
  await step('go', 'cmd_browser_go', {{ id, to: 'reload' }});
  await step('badGo', 'cmd_browser_go', {{ id, to: 'home' }});
  await step('badId', 'cmd_browser_open', {{ id: '../x', url: '{child}', bounds: {{ x: 0, y: 0, width: 1, height: 1 }} }});
  await step('badBounds', 'cmd_browser_set_bounds', {{ id, bounds: {{ x: 0, y: 0, width: -1, height: 1 }} }});
  await step('missing', 'cmd_browser_set_visible', {{ id: 'tab-404', visible: true }});
  await new Promise((done) => setTimeout(done, 1000));
  await fetch('/report', {{ method: 'POST', body: JSON.stringify(r) }});
}})()"#
        );
        let sent = on_main(app, move |app| {
            let main = app.get_webview_window("main").expect("main window");
            main.eval(script).map_err(|e| e.to_string())
        });
        match sent.and_then(|()| report(main_reports, "ipc")) {
            Ok(r) => {
                check.fact(format!("ipc steps: {}", serde_json::json!({
                    "open": r["open"], "bounds": r["bounds"], "hide": r["hide"], "navigate": r["navigate"],
                    "remote": r["remote"], "go": r["go"], "badGo": r["badGo"], "badId": r["badId"],
                    "badBounds": r["badBounds"], "missing": r["missing"],
                })));
                let ok = |name: &str| r[name] == "ok";
                let refused = |name: &str, says: &str| {
                    r[name]
                        .as_str()
                        .is_some_and(|s| s.starts_with("error") && s.contains(says))
                };
                check.that(ok("open") && ok("bounds") && ok("hide") && ok("navigate") && ok("go"), "ipc: open, set_bounds, set_visible, navigate and go are granted and succeed from the app's page");
                check.that(
                    refused("remote", "local development servers only"),
                    format!(
                        "ipc: navigate to a remote URL is refused with the reason (got {})",
                        r["remote"]
                    ),
                );
                check.that(
                    r["badGo"].as_str().is_some_and(|s| s.starts_with("error")),
                    format!(
                        "ipc: an unknown history step is refused (got {})",
                        r["badGo"]
                    ),
                );
                check.that(
                    refused("badId", "browser session id"),
                    format!(
                        "ipc: an id that is not a plain token is refused (got {})",
                        r["badId"]
                    ),
                );
                check.that(
                    refused("badBounds", "size"),
                    format!(
                        "ipc: impossible bounds are refused (got {})",
                        r["badBounds"]
                    ),
                );
                check.that(
                    refused("missing", "not open"),
                    format!(
                        "ipc: a command to a session that is not open says so (got {})",
                        r["missing"]
                    ),
                );
                let events = r["events"].as_array().cloned().unwrap_or_default();
                check.that(
                    events.iter().any(|e| e["id"] == "tab-7" && e["kind"] == "loaded")
                        && events.iter().any(|e| e["id"] == "tab-7" && e["kind"] == "title" && e["title"] == "child"),
                    format!("ipc: the page's load and title reach the app's page as browser-pane events ({} heard)", events.len()),
                );
            }
            Err(e) => check.that(false, format!("ipc: {e}")),
        }
        let state = on_main(app, |_| {
            let view = pane_view("tab-7")?;
            let hidden: bool = unsafe { msg_send![view, isHidden] };
            let bounds = browser::with_webview("tab-7", |w| w.bounds())
                .map_err(|e| e.to_string())?
                .map_err(|e| e.to_string())?;
            let (at, size) = (
                bounds.position.to_logical::<f64>(1.0),
                bounds.size.to_logical::<f64>(1.0),
            );
            let url = browser::with_webview("tab-7", |w| w.url())
                .map_err(|e| e.to_string())?
                .map_err(|e| e.to_string())?;
            Ok::<_, String>((hidden, (at.x, at.y, size.width, size.height), url))
        });
        match state {
            Ok((hidden, at, url)) => check.that(
                hidden && at == (7.0, 8.0, 200.0, 100.0) && url.ends_with("/child?again"),
                format!("ipc: the pane the commands drove is hidden, at 7,8 200x100, on the navigated URL (hidden {hidden}, at {at:?}, url {url})"),
            ),
            Err(e) => check.that(false, format!("ipc: the pane the commands opened: {e}")),
        }
        let closed = on_main(app, |app| {
            let main = app.get_webview_window("main").expect("main window");
            main.eval("window.__TAURI_INTERNALS__.invoke('cmd_browser_close', { id: 'tab-7' }).then(() => window.__TAURI_INTERNALS__.invoke('cmd_browser_close', { id: 'tab-7' })).then(() => fetch('/report', { method: 'POST', body: JSON.stringify({ who: 'ipc-close', ok: true }) }), (e) => fetch('/report', { method: 'POST', body: JSON.stringify({ who: 'ipc-close', ok: false, error: String(e) }) }))")
                .map_err(|e| e.to_string())
        })
        .and_then(|()| report(main_reports, "ipc-close"));
        check.that(
            closed.as_ref().is_ok_and(|r| r["ok"] == true)
                && on_main(app, |_| browser::open_count()) == 0,
            format!(
                "ipc: close removes the pane, and closing it again is not an error ({closed:?})"
            ),
        );

        // 11. An agent's `gitpulse_browser_open`, as the single-instance
        //     mailbox hands it to the app, reaches the page as the event the
        //     frontend listens for (`browseRequests.ts`).
        let listening = on_main(app, |app| {
            let main = app.get_webview_window("main").expect("main window");
            main.eval(
                r#"(async () => {
  const r = { who: 'browse', events: [] };
  const handler = window.__TAURI_INTERNALS__.transformCallback((e) => r.events.push(e.payload));
  await window.__TAURI_INTERNALS__.invoke('plugin:event|listen', { event: 'gitpulse-browse', target: { kind: 'Any' }, handler });
  await fetch('/report', { method: 'POST', body: JSON.stringify({ who: 'browse-ready' }) });
  await new Promise((done) => setTimeout(done, 1500));
  await fetch('/report', { method: 'POST', body: JSON.stringify(r) });
})()"#,
            )
            .map_err(|e| e.to_string())
        })
        .and_then(|()| report(main_reports, "browse-ready"));
        match listening {
            Ok(_) => {
                on_main(app, |app| {
                    gitpulse_lib::desktop::answer_forwarded(
                        app,
                        gitpulse_lib::desktop::instance::Request::Browse {
                            repo: "/fixture/repo".into(),
                            url: "http://localhost:5173/app".into(),
                        },
                    )
                });
                match report(main_reports, "browse") {
                    Ok(r) => check.that(
                        r["events"].as_array().is_some_and(|events| {
                            events.iter().any(|e| {
                                e["repo"] == "/fixture/repo" && e["url"] == "http://localhost:5173/app"
                            })
                        }),
                        format!("browse: an agent's request reaches the page as gitpulse-browse (heard {})", r["events"]),
                    ),
                    Err(e) => check.that(false, format!("browse: {e}")),
                }
            }
            Err(e) => check.that(
                false,
                format!("browse: the page did not start listening: {e}"),
            ),
        }
        check
    }

    const SMALL: Bounds = Bounds {
        x: 0.0,
        y: 0.0,
        width: 100.0,
        height: 100.0,
    };

    pub fn run() {
        std::thread::spawn(|| {
            std::thread::sleep(DEADLINE);
            eprintln!("FAIL browser_pane_isolation: deadline of {DEADLINE:?} expired");
            std::process::exit(1);
        });
        let (dev_port, main_reports) = serve();
        let (other_port, other_reports) = serve();
        let dev_origin = format!("http://127.0.0.1:{dev_port}/");

        let mut context = gitpulse_lib::context();
        context.config_mut().identifier = "com.gitpulse.tests.browser-pane".into();
        context.config_mut().build.dev_url = Some(dev_origin.parse().expect("dev url"));
        let mut main_window = context
            .config()
            .app
            .windows
            .first()
            .cloned()
            .expect("the config declares the main window");
        main_window.visible = false;
        main_window.url = tauri::WebviewUrl::App("main".into());
        context.config_mut().app.windows.clear();

        let app = tauri::Builder::default()
            // What GitPulse's page-load handler reads (step 9).
            .manage(gitpulse_lib::desktop::DesktopState::default())
            .manage(gitpulse_lib::terminal::TerminalSessions::default())
            .on_page_load(gitpulse_lib::desktop::handle_page_load)
            // The browser commands as `lib.rs` registers them (step 10).
            .invoke_handler(tauri::generate_handler![
                gitpulse_lib::browser::commands::cmd_browser_open,
                gitpulse_lib::browser::commands::cmd_browser_set_bounds,
                gitpulse_lib::browser::commands::cmd_browser_set_visible,
                gitpulse_lib::browser::commands::cmd_browser_navigate,
                gitpulse_lib::browser::commands::cmd_browser_go,
                gitpulse_lib::browser::commands::cmd_browser_close,
            ])
            .setup(move |app| {
                tauri::WebviewWindowBuilder::from_config(app.handle(), &main_window)?.build()?;
                Ok(())
            })
            .build(context)
            .expect("test application builds");

        // Taken once, on the first Ready: the run callback is called for every
        // event, and the receivers can only be moved out of it once.
        let mut pending = Some((main_reports, other_reports, dev_origin));
        app.run(move |app, event| {
            if !matches!(event, RunEvent::Ready) {
                return;
            }
            let Some((main_reports, other_reports, dev_origin)) = pending.take() else {
                return;
            };
            let handle = app.clone();
            std::thread::spawn(move || {
                // A probe needs a display to lay out and run a page in. With
                // the screen asleep or locked there is none; in CI that is a
                // misconfigured job and stays fatal, as in
                // native_status_material.
                let monitors = on_main(&handle, |app| app.available_monitors().unwrap_or_default().len());
                if monitors == 0 {
                    assert!(
                        std::env::var_os("CI").is_none(),
                        "no display is available in CI: the browser pane checks cannot run, and must not be reported as passing"
                    );
                    println!("SKIPPED browser_pane_isolation: no display is available to this process; NO check ran");
                    handle.exit(0);
                    return;
                }
                let check = scenario(&handle, &main_reports, other_port, &other_reports, &dev_origin);
                if check.failures.is_empty() {
                    println!("PASS browser_pane_isolation: the pane has no route to Rust; the control does");
                    handle.exit(0);
                } else {
                    for failure in &check.failures {
                        eprintln!("FAIL browser_pane_isolation: {failure}");
                    }
                    std::process::exit(1);
                }
            });
        });
    }
}
