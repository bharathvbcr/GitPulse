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
//! 5. **Policy wiring.** A navigation to a non-loopback URL is cancelled.
//! 6. **Close.** The view leaves the window and is deallocated, and repeated
//!    open/close cycles leave no views behind.
//! 7. **Startup flag.** `GITPULSE_BROWSER_PANE_URL` opens nothing for a remote
//!    URL and the right 40% of the window for a loopback one.
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
    use gitpulse_lib::browser::{self, Bounds};
    use objc2::msg_send;
    use objc2::rc::Weak;
    use objc2::runtime::AnyObject;
    use serde_json::Value;
    use std::cell::RefCell;
    use std::io::{Read, Write};
    use std::net::{TcpListener, TcpStream};
    use std::sync::mpsc::{self, Receiver, Sender};
    use std::time::{Duration, Instant};
    use tauri::{AppHandle, Manager, RunEvent};
    use wry::WebViewExtMacOS;

    const DEADLINE: Duration = Duration::from_secs(90);
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

    /// The pane's native view as a plain object pointer.
    fn pane_view() -> Result<*mut AnyObject, String> {
        browser::with_webview(|webview| {
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

        // 2. The pane, same window, same (devUrl) origin.
        let child_url = format!("{dev_origin}child");
        let opened = on_main(app, move |app| {
            let main = app.get_webview_window("main").expect("main window");
            let baseline = subview_count(content_view(&main));
            browser::open(
                &main,
                &child_url,
                Bounds {
                    x: 40.0,
                    y: 30.0,
                    width: 320.0,
                    height: 240.0,
                },
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
            let view = pane_view()?;
            let (in_window, superview): (*mut AnyObject, *mut AnyObject) =
                unsafe { (msg_send![view, window], msg_send![view, superview]) };
            let bounds = browser::with_webview(|w| w.bounds())
                .map_err(|e| e.to_string())?
                .map_err(|e| e.to_string())?;
            let at = (
                bounds.position.to_logical::<f64>(1.0),
                bounds.size.to_logical::<f64>(1.0),
            );
            browser::set_bounds(Bounds {
                x: 10.0,
                y: 20.0,
                width: 200.0,
                height: 150.0,
            })
            .map_err(|e| e.to_string())?;
            let moved = browser::with_webview(|w| w.bounds())
                .map_err(|e| e.to_string())?
                .map_err(|e| e.to_string())?;
            let moved = (
                moved.position.to_logical::<f64>(1.0),
                moved.size.to_logical::<f64>(1.0),
            );
            browser::set_visible(false).map_err(|e| e.to_string())?;
            let hidden: bool = unsafe { msg_send![view, isHidden] };
            browser::set_visible(true).map_err(|e| e.to_string())?;
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
            browser::with_webview(|w| w.load_url(&other))
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
                browser::with_webview(|w| {
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
            browser::with_webview(|w| w.load_url("data:text/html,<title>refused</title>"))
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

        // 6. Close: the view leaves the window; cycles leave nothing behind.
        let closed = on_main(app, move |app| {
            let main = app.get_webview_window("main").expect("main window");
            let view = pane_view()?;
            DEPARTED.with(|slot| *slot.borrow_mut() = Some(Weak::new(unsafe { &*view })));
            browser::close().map_err(|e| e.to_string())?;
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

        let cycles = 5;
        let url = format!("http://127.0.0.1:{other_port}/child");
        let counts = on_main(app, move |app| {
            let main = app.get_webview_window("main").expect("main window");
            let content = content_view(&main);
            let mut counts = Vec::new();
            for _ in 0..cycles {
                browser::open(
                    &main,
                    &url,
                    Bounds {
                        x: 0.0,
                        y: 0.0,
                        width: 100.0,
                        height: 100.0,
                    },
                )
                .map_err(|e| e.to_string())?;
                browser::close().map_err(|e| e.to_string())?;
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
            !on_main(app, |_| browser::is_open()),
            "close: no pane is open after the cycles",
        );

        // 7. The startup flag: a remote URL opens nothing; a loopback one
        //    opens the pane over the right 40% of the main window.
        // Through the env owner, so the change is serialized and restored.
        let serial = crate::hermetic_env::env_serial();
        let remote_flag = crate::hermetic_env::bind_env(&serial)
            .set("GITPULSE_BROWSER_PANE_URL", "https://example.com/");
        let remote = on_main(app, |app| {
            browser::open_from_env(app);
            browser::is_open()
        });
        drop(remote_flag);
        check.that(
            !remote,
            "flag: a non-loopback GITPULSE_BROWSER_PANE_URL opens no pane",
        );
        let loopback_flag = crate::hermetic_env::bind_env(&serial).set(
            "GITPULSE_BROWSER_PANE_URL",
            format!("http://127.0.0.1:{other_port}/child"),
        );
        let flagged = on_main(app, |app| {
            browser::open_from_env(app);
            let main = app.get_webview_window("main").expect("main window");
            let scale = main.scale_factor().unwrap();
            let size = main.inner_size().unwrap().to_logical::<f64>(scale);
            let bounds = browser::with_webview(|w| w.bounds())
                .map_err(|e| e.to_string())?
                .map_err(|e| e.to_string())?;
            let (at, extent) = (
                bounds.position.to_logical::<f64>(1.0),
                bounds.size.to_logical::<f64>(1.0),
            );
            browser::close().map_err(|e| e.to_string())?;
            Ok::<_, String>((
                (at.x, at.y, extent.width, extent.height),
                (size.width, size.height),
            ))
        });
        drop(loopback_flag);
        drop(serial);
        match flagged {
            Ok(((x, y, w, h), (width, height))) => check.that(
                (x - width * 0.6).abs() < 1.0 && y == 0.0 && (w - width * 0.4).abs() < 1.0 && (h - height).abs() < 1.0,
                format!("flag: a loopback URL opens the pane at the right 40% of a {width}x{height} window (got {x},{y} {w}x{h})"),
            ),
            Err(e) => check.that(false, format!("flag: a loopback GITPULSE_BROWSER_PANE_URL opens the pane: {e}")),
        }
        check
    }

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
