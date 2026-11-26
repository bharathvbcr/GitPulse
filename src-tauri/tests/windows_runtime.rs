//! Real Windows WebView2/Win32 runtime smoke test, parallel to `gtk_runtime.rs`.
//! Windows CI runs this against the installed Microsoft Edge WebView2 Runtime.
//! A missing WebView2 Runtime is a failure; the macOS/Linux branches are inapplicable.

// The hermetic test environment: a per-process home, git config and trust
// store, rewritten before `main`. See `src/test_support/env.rs`.
#[path = "../src/test_support/env.rs"]
mod hermetic_env;

#[path = "common/nextest_list.rs"]
mod nextest_list;

#[cfg(target_os = "windows")]
fn main() {
    if nextest_list::answer_list("windows_runtime", true) {
        return;
    }
    use std::sync::{
        atomic::{AtomicBool, Ordering},
        mpsc, Arc,
    };
    use std::time::Duration;
    use tauri::{webview::PageLoadEvent, RunEvent, WebviewUrl, WindowEvent};
    use waylode_lib::desktop::{install_menu, set_recent_menu, DesktopState};

    let version = wry::webview_version()
        .expect("Windows runtime test requires Microsoft Edge WebView2 Runtime");
    println!("windows_runtime: WebView2 runtime {version}");

    let profile = tempfile::Builder::new()
        .prefix("gitpulse-win-runtime-")
        .tempdir()
        .expect("isolated WebView2 profile directory");
    let main_profile = profile.path().join("main");
    let child_profile = profile.path().join("child");

    const BROWSER_ARGS: &str =
        "--enable-unsafe-swiftshader --disable-features=msWebOOUI,msPdfOOUI,msSmartScreenProtection";

    let evaluated = Arc::new(AtomicBool::new(false));
    let observed = evaluated.clone();
    let app = tauri::Builder::default()
        .manage(DesktopState::default())
        .register_uri_scheme_protocol("win-runtime", |_, _| {
            tauri::http::Response::builder()
                .header("Content-Type", "text/html; charset=utf-8")
                .body(
                    b"<!doctype html><html><head><title>loading</title></head><body>Windows runtime test</body></html>"
                        .to_vec(),
                )
                .expect("valid local HTML response")
        })
        .setup(move |app| {
            let main = tauri::WebviewWindowBuilder::new(
                app,
                "win-runtime",
                WebviewUrl::External("http://win-runtime.localhost/index.html".parse()?),
            )
            .data_directory(main_profile)
            .additional_browser_args(BROWSER_ARGS)
            .on_page_load(|window, payload| {
                println!("windows_runtime: page {:?} {}", payload.event(), payload.url());
                if payload.event() == PageLoadEvent::Finished
                    && payload.url().scheme() == "http"
                    && payload.url().host_str() == Some("win-runtime.localhost")
                {
                    window
                        .eval(
                            "if (document.body && document.body.textContent.includes('Windows runtime test')) { \
                                 document.title = 'Windows runtime: λ / 日本語'; \
                             }",
                        )
                        .expect("evaluate JavaScript in the real WebView2 process");
                }
            })
            .on_document_title_changed(move |window, title| {
                println!("windows_runtime: title {title:?}");
                if title == "Windows runtime: λ / 日本語" {
                    observed.store(true, Ordering::SeqCst);
                    window.close().expect("close the WebView2 window");
                }
            })
            .build()?;
            main.set_size(tauri::LogicalSize::new(640.0, 480.0))?;
            main.hide()?;
            main.show()?;
            install_menu(app.handle())?;
            set_recent_menu(app.handle(), vec![r"C:\Temp\Windows λ repository".into()])?;
            set_recent_menu(app.handle(), vec![])?;
            assert!(main.is_menu_visible()?);

            let child = tauri::WebviewWindowBuilder::new(
                app,
                "win-child",
                WebviewUrl::External("about:blank".parse()?),
            )
            .owner(&main)?
            .data_directory(child_profile)
            .additional_browser_args(BROWSER_ARGS)
            .build()?;
            child.close()?;
            println!("windows_runtime: windows and menus initialized");
            Ok(())
        })
        .build(tauri::test::mock_context(tauri::test::noop_assets()))
        .expect("build the native Tauri Windows runtime");

    let handle = app.handle().clone();
    let (done, wait) = mpsc::channel();
    let watchdog = std::thread::spawn(move || {
        if wait.recv_timeout(Duration::from_secs(30)).is_err() {
            eprintln!("Windows runtime test timed out waiting for WebView2 and window closure");
            handle.exit(2);
        }
    });
    let destroyed = Arc::new(AtomicBool::new(false));
    let closed = destroyed.clone();
    let code = app.run_return(move |_, event| {
        if let RunEvent::WindowEvent {
            label,
            event: WindowEvent::Destroyed,
            ..
        } = event
        {
            if label == "win-runtime" {
                closed.store(true, Ordering::SeqCst);
            }
        }
    });
    let notified = done.send(());
    watchdog.join().expect("watchdog completed");
    assert_eq!(code, 0, "native event loop exited successfully");
    notified.expect("watchdog still waiting");
    assert!(
        evaluated.load(Ordering::SeqCst),
        "WebView2 executed JavaScript"
    );
    assert!(
        destroyed.load(Ordering::SeqCst),
        "native window was destroyed"
    );
    println!(
        "windows_runtime: WebView2 runtime, JavaScript eval, Win32 menus, child window and lifecycle passed"
    );
}

#[cfg(not(target_os = "windows"))]
fn main() {
    if nextest_list::answer_list("windows_runtime", false) {
        return;
    }
    println!("windows_runtime: not applicable on this platform (Windows WebView2 test)");
}
