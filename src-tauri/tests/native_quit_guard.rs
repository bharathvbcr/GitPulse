//! AppKit's real quit and crash paths, end to end, on the process main thread.
//!
//! Each scenario runs in its own child process, because an approved
//! `terminate:` ends the process: the parent re-executes this binary with
//! `GP_QUIT_SCENARIO` set and requires exit status 0 *and* the child's
//! `PASS <scenario>` line, which is printed only from `RunEvent::Exit` after the
//! scenario's checks hold. A child that hangs is killed by its own watchdog
//! (exit 1) and again by the parent's deadline.
//!
//! The frontend is played by Rust listening for the exit-request event and
//! answering through the same functions the IPC commands call, from another
//! thread, as IPC would. What this does exercise for real: AppKit calling the
//! installed `applicationShouldTerminate:`, the `NSTerminateLater` modal wait
//! being serviced by TAO's event loop, the single reply, the acknowledgement
//! deadline, and a WKWebView content process killed with SIGKILL. The content
//! process id is read with WebKit's private `_webProcessIdentifier`; that is a
//! test-only use, never shipped.

#[cfg(not(target_os = "macos"))]
fn main() {
    println!("SKIP native_quit_guard: requires macOS/AppKit");
}

#[cfg(target_os = "macos")]
fn main() {
    match std::env::var("GP_QUIT_SCENARIO") {
        Ok(scenario) => child::run(&scenario),
        Err(_) => parent(),
    }
}

#[cfg(target_os = "macos")]
const SCENARIOS: &[&str] = &[
    "unarmed",
    "approve",
    "cancel_then_approve",
    "no_ack",
    "double",
    "internal",
    "crash_reload",
    "crash_while_held",
    "crash_loop",
];

#[cfg(target_os = "macos")]
fn parent() {
    use gitpulse_lib::procguard::LockedSpawn;
    use std::io::Read;
    use std::process::{Command, Stdio};
    use std::time::{Duration, Instant};

    // Drained while the child runs: a child that fills a pipe would otherwise
    // block on it and be reported as a hang.
    fn drain(pipe: Option<impl Read + Send + 'static>) -> std::thread::JoinHandle<String> {
        std::thread::spawn(move || {
            let mut text = String::new();
            if let Some(mut pipe) = pipe {
                let _ = pipe.read_to_string(&mut text);
            }
            text
        })
    }

    let only = std::env::var("GP_QUIT_ONLY").ok();
    let exe = std::env::current_exe().expect("test binary path");
    let mut failed = Vec::new();
    let mut ran = 0;
    for scenario in SCENARIOS {
        if only.as_deref().is_some_and(|only| only != *scenario) {
            continue;
        }
        ran += 1;
        let started = Instant::now();
        let mut child = Command::new(&exe)
            .env("GP_QUIT_SCENARIO", scenario)
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn_locked()
            .expect("spawn scenario");
        let stdout = drain(child.stdout.take());
        let stderr = drain(child.stderr.take());
        let status = loop {
            if let Some(status) = child.try_wait().expect("poll scenario") {
                break Some(status);
            }
            if started.elapsed() > Duration::from_secs(45) {
                let _ = child.kill();
                let _ = child.wait();
                break None;
            }
            std::thread::sleep(Duration::from_millis(50));
        };
        let stdout = stdout.join().unwrap_or_default();
        let stderr = stderr.join().unwrap_or_default();
        let passed = status.is_some_and(|status| status.success())
            && stdout
                .lines()
                .any(|line| line == format!("PASS {scenario}"));
        println!(
            "{} {scenario} ({:.1}s, status {:?})",
            if passed { "ok  " } else { "FAIL" },
            started.elapsed().as_secs_f64(),
            status.map(|status| status.code())
        );
        if !passed {
            println!("--- stdout\n{stdout}--- stderr (tail)");
            let lines: Vec<&str> = stderr.lines().collect();
            for line in &lines[lines.len().saturating_sub(40)..] {
                println!("{line}");
            }
            failed.push(*scenario);
        }
    }
    assert!(ran > 0, "GP_QUIT_ONLY named no scenario");
    if !failed.is_empty() {
        eprintln!("FAIL native quit guard: {failed:?}");
        std::process::exit(1);
    }
    println!("native quit guard: {ran} scenario(s) passed");
}

#[cfg(target_os = "macos")]
mod child {
    use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
    use std::sync::mpsc;
    use std::sync::{Mutex, OnceLock};
    use std::time::{Duration, Instant};

    use gitpulse_lib::desktop::{
        self, cmd_set_exit_guard_ready, exit, terminate, webcrash, DesktopState,
        EXIT_REQUESTED_EVENT,
    };
    use objc2::runtime::AnyObject;
    use objc2::{class, msg_send, sel};
    use tauri::{AppHandle, Listener, Manager, RunEvent, WebviewUrl, Wry};

    #[derive(Default)]
    struct Probe {
        requests: AtomicUsize,
        page_starts: AtomicUsize,
        page_loads: AtomicUsize,
        load_times: Mutex<Vec<Instant>>,
        armed_after_crash_start: AtomicBool,
        quit_at: Mutex<Option<Instant>>,
        survived_cancel: AtomicBool,
        crash_reload_delays: Mutex<Vec<Duration>>,
    }

    static PROBE: OnceLock<Probe> = OnceLock::new();
    static SCENARIO: OnceLock<String> = OnceLock::new();

    fn probe() -> &'static Probe {
        PROBE.get_or_init(Probe::default)
    }

    fn fail(why: &str) -> ! {
        println!("FAIL {}: {why}", SCENARIO.get().map_or("?", String::as_str));
        eprintln!("FAIL native quit guard: {why}");
        std::process::exit(1);
    }

    fn check(condition: bool, why: &str) {
        if !condition {
            fail(why);
        }
    }

    pub fn run(scenario: &str) {
        SCENARIO.set(scenario.to_string()).unwrap();
        std::thread::spawn(|| {
            std::thread::sleep(Duration::from_secs(35));
            fail("main-thread deadline expired");
        });
        let mut context = gitpulse_lib::context();
        context.config_mut().identifier = "com.gitpulse.tests.quit-guard".into();
        context.config_mut().app.windows.clear();
        let app = tauri::Builder::default()
            .manage(DesktopState::default())
            .manage(gitpulse_lib::terminal::TerminalSessions::default())
            .on_web_content_process_terminate(webcrash::on_terminated)
            // A page of its own: a debug build points `WebviewUrl::App` at the
            // dev server, which a test cannot assume is running.
            .register_uri_scheme_protocol("quitprobe", |_, _| {
                tauri::http::Response::builder()
                    .header("Content-Type", "text/html")
                    .body(b"<!doctype html><p>quit probe</p>".to_vec())
                    .unwrap()
            })
            .on_page_load(|webview, payload| {
                desktop::handle_page_load(webview, payload);
                if webview.label() != "main" {
                    return;
                }
                match payload.event() {
                    tauri::webview::PageLoadEvent::Started => {
                        let starts = probe().page_starts.fetch_add(1, Ordering::SeqCst);
                        if starts > 0 {
                            let armed = webview.state::<DesktopState>().exit_gate().is_armed();
                            probe()
                                .armed_after_crash_start
                                .fetch_or(armed, Ordering::SeqCst);
                        }
                    }
                    tauri::webview::PageLoadEvent::Finished => {
                        probe().page_loads.fetch_add(1, Ordering::SeqCst);
                        probe().load_times.lock().unwrap().push(Instant::now());
                    }
                }
            })
            .setup(|app| {
                tauri::WebviewWindowBuilder::new(
                    app,
                    "main",
                    WebviewUrl::CustomProtocol("quitprobe://localhost/".parse().unwrap()),
                )
                .visible(false)
                .build()?;
                terminate::install(app.handle()).map_err(|error| {
                    Box::<dyn std::error::Error>::from(format!("install: {error}"))
                })?;
                Ok(())
            })
            .build(context)
            .expect("native test application builds");

        let handle = app.handle().clone();
        let answering = handle.clone();
        app.listen(EXIT_REQUESTED_EVENT, move |_| {
            let index = probe().requests.fetch_add(1, Ordering::SeqCst);
            let app = answering.clone();
            // IPC answers arrive from outside the emitting call.
            std::thread::spawn(move || frontend(&app, index));
        });

        app.run(move |app, event| {
            desktop::handle_run_event(app, &event);
            match event {
                RunEvent::Ready => {
                    let driver = handle.clone();
                    std::thread::spawn(move || drive(&driver));
                }
                RunEvent::Exit => verify(),
                _ => {}
            }
        });
        fail("the event loop returned without RunEvent::Exit");
    }

    /// The frontend's answer to request number `index` in this scenario.
    fn frontend(app: &AppHandle<Wry>, index: usize) {
        let scenario = SCENARIO.get().unwrap().as_str();
        let seen = || exit::answer(app, exit::ExitAnswer::Seen);
        match (scenario, index) {
            ("no_ack", _) => {}
            ("cancel_then_approve", 0) => {
                seen();
                std::thread::sleep(Duration::from_millis(200));
                exit::answer(app, exit::ExitAnswer::Cancel);
            }
            ("double", _) | ("crash_while_held", _) => seen(),
            _ => {
                seen();
                std::thread::sleep(Duration::from_millis(200));
                exit::approve(app);
            }
        }
    }

    fn on_main<T: Send + 'static>(
        app: &AppHandle<Wry>,
        work: impl FnOnce() -> T + Send + 'static,
    ) -> T {
        let (sent, received) = mpsc::channel();
        app.run_on_main_thread(move || {
            let _ = sent.send(work());
        })
        .expect("main thread reachable");
        received
            .recv_timeout(Duration::from_secs(10))
            .unwrap_or_else(|_| fail("main-thread work did not run"))
    }

    fn arm(app: &AppHandle<Wry>) {
        let handle = app.clone();
        on_main(app, move || {
            cmd_set_exit_guard_ready(handle.state::<DesktopState>())
        });
    }

    /// Quits the way Cmd-Q does: `terminate:` from the run loop, outside any
    /// TAO callback.
    fn quit(app: &AppHandle<Wry>) {
        *probe().quit_at.lock().unwrap() = Some(Instant::now());
        on_main(app, || unsafe {
            let ns_app: *mut AnyObject = msg_send![class!(NSApplication), sharedApplication];
            let () = msg_send![
                ns_app,
                performSelector: sel!(terminate:),
                withObject: std::ptr::null_mut::<AnyObject>(),
                afterDelay: 0.0f64
            ];
        });
    }

    fn wait_for(what: &str, limit: Duration, mut ready: impl FnMut() -> bool) {
        let start = Instant::now();
        while !ready() {
            if start.elapsed() > limit {
                fail(&format!("timed out waiting for {what}"));
            }
            std::thread::sleep(Duration::from_millis(20));
        }
    }

    fn loads() -> usize {
        probe().page_loads.load(Ordering::SeqCst)
    }

    fn gate_armed(app: &AppHandle<Wry>) -> bool {
        app.state::<DesktopState>().exit_gate().is_armed()
    }

    /// Kills the main webview's content process and waits for its reload.
    fn crash_and_wait_for_reload(app: &AppHandle<Wry>) -> Duration {
        let before = loads();
        let window = app.get_webview_window("main").expect("main window");
        let (sent, received) = mpsc::channel();
        window
            .with_webview(move |platform| {
                // SAFETY: test-only private API on the live WKWebView, read on
                // the main thread where `with_webview` runs this.
                let pid: i32 = unsafe {
                    msg_send![platform.inner().cast::<AnyObject>(), _webProcessIdentifier]
                };
                let _ = sent.send(pid);
            })
            .expect("webview reachable");
        let pid = received
            .recv_timeout(Duration::from_secs(10))
            .unwrap_or_else(|_| fail("no content process id"));
        check(pid > 0, "the page has a content process");
        let killed = Instant::now();
        // SAFETY: plain kill(2) of WebKit's content process for this webview.
        check(
            unsafe { libc::kill(pid, libc::SIGKILL) } == 0,
            "SIGKILL delivered",
        );
        wait_for("the reload after a crash", Duration::from_secs(20), || {
            loads() > before
        });
        killed.elapsed()
    }

    fn drive(app: &AppHandle<Wry>) {
        wait_for("the first page load", Duration::from_secs(10), || {
            loads() >= 1
        });
        let scenario = SCENARIO.get().unwrap().as_str();
        match scenario {
            "unarmed" => quit(app),
            "approve" | "no_ack" => {
                arm(app);
                quit(app);
            }
            "cancel_then_approve" => {
                arm(app);
                quit(app);
                wait_for("the cancelled quit", Duration::from_secs(5), || {
                    probe().requests.load(Ordering::SeqCst) == 1
                });
                std::thread::sleep(Duration::from_millis(1500));
                // Still here, still guarding, nothing left open.
                check(gate_armed(app), "a cancel keeps quits guarded");
                check(
                    app.state::<DesktopState>().exit_gate().take().is_none(),
                    "a cancelled request is settled",
                );
                probe().survived_cancel.store(true, Ordering::SeqCst);
                quit(app);
            }
            "double" => {
                arm(app);
                quit(app);
                wait_for("the held quit", Duration::from_secs(5), || {
                    probe().requests.load(Ordering::SeqCst) == 1
                });
                std::thread::sleep(Duration::from_millis(300));
                // A second Cmd-Q while AppKit waits on the first. It arrives
                // from the run loop in AppKit's modal mode, as a menu key
                // equivalent does — never from inside a TAO callback, whose
                // handler lock AppKit's immediate termination would need.
                // AppKit does not ask the delegate again: it terminates.
                on_main(app, || unsafe {
                    use objc2_foundation::{NSArray, NSString};
                    let modes = NSArray::from_retained_slice(&[NSString::from_str(
                        "kCFRunLoopCommonModes",
                    )]);
                    let ns_app: *mut AnyObject =
                        msg_send![class!(NSApplication), sharedApplication];
                    let () = msg_send![
                        ns_app,
                        performSelector: sel!(terminate:),
                        withObject: std::ptr::null_mut::<AnyObject>(),
                        afterDelay: 0.0f64,
                        inModes: &*modes
                    ];
                });
            }
            "internal" => {
                arm(app);
                let handle = app.clone();
                on_main(app, move || {
                    desktop::handle_menu_event(&handle, "tray:quit")
                });
            }
            "crash_reload" => {
                arm(app);
                crash_and_wait_for_reload(app);
                check(!gate_armed(app), "a crashed page cannot confirm quits");
                arm(app); // the reloaded page listens again
                quit(app);
            }
            "crash_while_held" => {
                arm(app);
                quit(app);
                wait_for("the held quit", Duration::from_secs(5), || {
                    probe().requests.load(Ordering::SeqCst) == 1
                });
                std::thread::sleep(Duration::from_millis(300));
                crash_and_wait_for_reload(app);
                // The person deciding lost the page: the quit was cancelled and
                // the app is still here. With no page listening, a quit goes.
                std::thread::sleep(Duration::from_millis(300));
                probe().survived_cancel.store(true, Ordering::SeqCst);
                quit(app);
            }
            "crash_loop" => {
                arm(app);
                let mut delays = Vec::new();
                for _ in 0..4 {
                    delays.push(crash_and_wait_for_reload(app));
                }
                *probe().crash_reload_delays.lock().unwrap() = delays;
                quit(app);
            }
            other => fail(&format!("unknown scenario {other}")),
        }
    }

    fn verify() {
        let scenario = SCENARIO.get().unwrap().as_str();
        let probe = probe();
        let requests = probe.requests.load(Ordering::SeqCst);
        let held = probe
            .quit_at
            .lock()
            .unwrap()
            .map(|at| at.elapsed())
            .unwrap_or_default();
        match scenario {
            "unarmed" => check(requests == 0, "an unarmed quit asks nobody"),
            "approve" | "internal" | "crash_reload" => {
                check(requests == 1, "one confirmation per quit")
            }
            "cancel_then_approve" => {
                check(
                    probe.survived_cancel.load(Ordering::SeqCst),
                    "the cancelled quit kept the app",
                );
                check(requests == 2, "each quit asked once");
            }
            "no_ack" => {
                check(requests == 1, "the silent page was asked once");
                check(
                    held >= exit::ACK_DEADLINE,
                    "an unacknowledged quit waits out the deadline",
                );
            }
            "double" => {
                check(requests == 1, "a repeated quit must not ask twice");
                check(
                    held < exit::ACK_DEADLINE,
                    "the second quit ended the app, not the deadline or an approval",
                );
            }
            "crash_while_held" => {
                check(
                    probe.survived_cancel.load(Ordering::SeqCst),
                    "the app outlived the crash",
                );
                check(
                    requests == 1,
                    "the second quit, with no page listening, asked nobody",
                );
            }
            "crash_loop" => {
                let delays = probe.crash_reload_delays.lock().unwrap().clone();
                check(delays.len() == 4, "four crashes, four reloads");
                check(
                    delays[2] >= Duration::from_secs(1) && delays[3] >= Duration::from_secs(2),
                    &format!("repeated crashes back off: {delays:?}"),
                );
                check(requests == 0, "after a crash, a quit is not held");
            }
            _ => {}
        }
        if scenario.starts_with("crash") {
            check(
                !probe.armed_after_crash_start.load(Ordering::SeqCst),
                "the reloading page was never treated as listening",
            );
        }
        println!("PASS {scenario}");
    }
}
