//! Exercise the production popover constructor on AppKit's real main thread.
//! This checks the actual native effect view, not only the builder's options.

#[cfg(not(target_os = "macos"))]
fn main() {
    println!("SKIP native_status_material: requires macOS/AppKit");
}

#[cfg(target_os = "macos")]
fn main() {
    use gitpulse_lib::desktop::{install_menu, popover, set_menu_state, DesktopState, MenuState};
    use std::sync::mpsc;
    use std::time::Duration;
    use tauri::{Manager, RunEvent};

    // A lost main-thread callback must fail the check rather than hang CI.
    std::thread::spawn(|| {
        std::thread::sleep(Duration::from_secs(20));
        eprintln!("FAIL native status material: main-thread deadline expired");
        std::process::exit(1);
    });
    let mut context = gitpulse_lib::context();
    context.config_mut().identifier = "com.gitpulse.tests.status-material".into();
    context.config_mut().app.windows.clear();
    let app = tauri::Builder::default()
        .manage(DesktopState::default())
        .setup(|app| {
            install_menu(app.handle())?;
            set_menu_state(
                app.handle(),
                MenuState {
                    show_status_icon: true,
                    ..Default::default()
                },
            )?;
            // Phase 1: the status item must not keep a menu attached at rest.
            if let Some(tray) = app.handle().tray_by_id("gitpulse-status") {
                tray.with_inner_tray_icon(|inner| {
                    #[cfg(target_os = "macos")]
                    {
                        use objc2::msg_send;
                        use objc2::runtime::AnyObject;
                        let item = inner.ns_status_item().expect("status item");
                        unsafe {
                            let item_ptr = objc2::rc::Retained::as_ptr(&item).cast::<AnyObject>();
                            let menu: *mut AnyObject = msg_send![item_ptr, menu];
                            assert!(
                                menu.is_null(),
                                "status item must not keep an NSMenu attached at rest"
                            );
                        }
                    }
                })?;
            }
            Ok(())
        })
        .build(context)
        .expect("native test application builds");
    app.run(|app, event| {
        if !matches!(event, RunEvent::Ready) {
            return;
        }
        let handle = app.clone();
        std::thread::spawn(move || {
            // AppKit lays out a newly installed status item after Ready.
            std::thread::sleep(Duration::from_millis(300));
            for step in 0..4 {
                let app = handle.clone();
                let (sent, received) = mpsc::channel();
                handle
                    .run_on_main_thread(move || {
                        if step == 0 {
                            println!("native test displays: {:?}", app.available_monitors().unwrap());
                            println!("native test status rect: {:?}", app.tray_by_id("gitpulse-status").map(|tray| tray.rect()));
                            popover::toggle(&app).expect("production popover opens");
                        } else {
                            let window = app
                                .get_webview_window(popover::LABEL)
                                .expect("status window exists");
                            inspect_material(&window);
                            let size = window
                                .inner_size()
                                .unwrap()
                                .to_logical::<f64>(window.scale_factor().unwrap());
                            assert_eq!(size.width, 360.0, "material fits the panel width");
                            if step == 1 {
                                window
                                    .set_size(tauri::LogicalSize::new(360.0, 180.0))
                                    .expect("resize to a short connecting/error state");
                            } else if step == 2 {
                                popover::hide(&app).unwrap();
                                popover::toggle(&app).unwrap();
                            } else {
                                assert_eq!(size.height, 180.0, "reopen preserves measured height");
                                println!("PASS native status material: real blur, rounded bounds, resize and reopen");
                                app.exit(0);
                            }
                        }
                        sent.send(()).unwrap();
                    })
                    .expect("schedule main-thread check");
                received
                    .recv_timeout(Duration::from_secs(3))
                    .expect("main-thread check completed");
                // Let AppKit process native size/show events before the next read.
                std::thread::sleep(Duration::from_millis(150));
            }
        });
    });
}

#[cfg(target_os = "macos")]
fn inspect_material(window: &tauri::WebviewWindow) {
    use objc2::{msg_send, runtime::AnyClass, runtime::AnyObject, sel};

    let pointer = window.ns_window().expect("AppKit window handle");
    assert!(!pointer.is_null());
    // SAFETY: Tauri provides this live NSWindow, and the caller runs on the
    // AppKit main thread. All borrowed native views stay owned by that window.
    unsafe {
        let native = &*pointer.cast::<AnyObject>();
        let content: *mut AnyObject = msg_send![native, contentView];
        assert!(!content.is_null(), "native content view exists");
        // Tag from the pinned window-vibrancy implementation used by Tauri.
        let effect: *mut AnyObject = msg_send![content, viewWithTag: 91376254_isize];
        assert!(!effect.is_null(), "native blur view was actually attached");
        let class = AnyClass::get(c"NSVisualEffectView").expect("AppKit effect class");
        let is_effect: bool = msg_send![effect, isKindOfClass: class];
        assert!(is_effect);
        let material: isize = msg_send![effect, material];
        let blending: isize = msg_send![effect, blendingMode];
        let state: isize = msg_send![effect, state];
        let autoresizing: usize = msg_send![effect, autoresizingMask];
        // Values from the installed AppKit SDK's NSVisualEffectView/NSView headers.
        assert_eq!(material, 21, "UnderWindowBackground matches the main app");
        assert_eq!(blending, 0, "blur samples behind the native window");
        assert_eq!(state, 0, "effect follows window activation");
        assert_eq!(autoresizing & 18, 18, "effect follows width and height");
        let has_radius: bool = msg_send![effect, respondsToSelector: sel!(cornerRadius)];
        assert!(has_radius, "the pinned material exposes its rounded bounds");
        let radius: f64 = msg_send![effect, cornerRadius];
        assert_eq!(radius, 18.0, "native and CSS corners align");
    }
}
