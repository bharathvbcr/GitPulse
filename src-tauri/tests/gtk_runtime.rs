//! Real GTK3/WebKit smoke test. Linux CI runs this under Xvfb and a session bus.
//! A missing display is a failure; the macOS/Windows branches are inapplicable.

#[cfg(target_os = "linux")]
fn main() {
    use gitpulse_lib::desktop::{install_menu, set_recent_menu, DesktopState};
    use gtk::glib::variant::ToVariant;
    use gtk::prelude::GtkWindowExt;
    use std::sync::{
        atomic::{AtomicBool, Ordering},
        mpsc, Arc,
    };
    use std::time::Duration;
    use tauri::{webview::PageLoadEvent, RunEvent, WebviewUrl, WindowEvent};

    // Exercise both directions, empty input, UTF-8, exhaustion and oversized
    // skips in the iterator affected by RUSTSEC-2024-0429. Run in release mode
    // as well: the old shared-pointer write could be optimized away there.
    for values in [vec![], vec![""], vec!["first", "λ / 日本語", "last"]] {
        let variant = values.to_variant();
        let mut iter = variant.array_iter_str().expect("string array");
        assert_eq!(iter.len(), values.len());
        assert_eq!(iter.next(), values.first().copied());
        if values.len() > 1 {
            assert_eq!(iter.next_back(), values.last().copied());
            assert_eq!(iter.collect::<Vec<_>>(), values[1..values.len() - 1]);
        } else {
            assert_eq!(iter.next_back(), None);
            assert_eq!(iter.next(), None);
        }
        assert_eq!(
            variant.array_iter_str().unwrap().collect::<Vec<_>>(),
            values
        );
        let mut forward = variant.array_iter_str().unwrap();
        assert_eq!(forward.nth(usize::MAX), None);
        assert_eq!(forward.next_back(), None);
        let mut backward = variant.array_iter_str().unwrap();
        assert_eq!(backward.nth_back(usize::MAX), None);
        assert_eq!(backward.next(), None);
    }

    gtk::init().expect("GTK runtime test requires a display (use xvfb-run)");
    let evaluated = Arc::new(AtomicBool::new(false));
    let observed = evaluated.clone();
    let app = tauri::Builder::default()
        .manage(DesktopState::default())
        .register_uri_scheme_protocol("gtk-migration", |_, _| {
            tauri::http::Response::builder()
                .header("Content-Type", "text/html; charset=utf-8")
                .body(b"<!doctype html><html><head><title>loading</title></head><body>GTK runtime test</body></html>".to_vec())
                .expect("valid local HTML response")
        })
        .setup(move |app| {
            let main = tauri::WebviewWindowBuilder::new(
                app,
                "gtk-migration",
                WebviewUrl::External("gtk-migration://localhost/index.html".parse()?),
            )
            .on_page_load(|window, payload| {
                println!("gtk_runtime: page {:?} {}", payload.event(), payload.url());
                if payload.event() == PageLoadEvent::Finished {
                    window
                        .eval("document.title = 'GTK migration: λ / 日本語'")
                        .expect("evaluate JavaScript in the real WebKit process");
                }
            })
            .on_document_title_changed(move |window, title| {
                println!("gtk_runtime: title {title:?}");
                if title == "GTK migration: λ / 日本語" {
                    observed.store(true, Ordering::SeqCst);
                    window.close().expect("close the WebKit window");
                }
            })
            .build()?;
            main.set_size(tauri::LogicalSize::new(640.0, 480.0))?;
            main.hide()?;
            main.show()?;
            install_menu(app.handle())?;
            set_recent_menu(app.handle(), vec!["/tmp/GTK λ repository".into()])?;
            set_recent_menu(app.handle(), vec![])?;
            assert!(main.is_menu_visible()?);

            let parent = main.gtk_window()?;
            let child = tauri::WebviewWindowBuilder::new(
                app,
                "gtk-transient",
                WebviewUrl::External("about:blank".parse()?),
            )
            .transient_for_raw(&parent)
            .build()?;
            assert!(child.gtk_window()?.transient_for().is_some());
            child.close()?;
            println!("gtk_runtime: windows and menus initialized");
            Ok(())
        })
        .build(tauri::test::mock_context(tauri::test::noop_assets()))
        .expect("build the native Tauri runtime");

    let handle = app.handle().clone();
    let (done, wait) = mpsc::channel();
    let watchdog = std::thread::spawn(move || {
        if wait.recv_timeout(Duration::from_secs(30)).is_err() {
            eprintln!("GTK runtime test timed out waiting for WebKit and window closure");
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
            if label == "gtk-migration" {
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
        "WebKit executed JavaScript"
    );
    assert!(
        destroyed.load(Ordering::SeqCst),
        "native window was destroyed"
    );
    println!("gtk_runtime: GLib iterators, WebKit JavaScript, menus, transient window and lifecycle passed");
}

#[cfg(not(target_os = "linux"))]
fn main() {
    println!("gtk_runtime: not applicable on this platform (Linux GTK test)");
}
