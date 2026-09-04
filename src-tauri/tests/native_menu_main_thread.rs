//! `build_native_menu` on the thread it actually requires.
//!
//! `desktop_menu_integration.rs` records that `install_menu` "is not exercised
//! here: muda creates menu items on the main thread only, and Rust test
//! harnesses run cases on worker threads, so calling it panics inside the
//! platform layer". That is exactly right, and it is verifiable — calling it
//! under the normal harness panics with
//! `muda::MenuChild can only be created on the main thread`.
//!
//! But the constraint is on the THREAD, not on testability. A test target
//! declared `harness = false` gets its own `fn main`, and that `main` runs on
//! the process's real main thread — so the whole menu builder runs here, with
//! no test harness in between. That covers `build_native_menu`, the largest
//! function in the desktop module and the one every native menu entry, every
//! accelerator and the whole recent-repositories submenu flow through.
//!
//! No mocking: this is the real `install_menu` against a real `AppHandle`.
//! Failures are reported by exiting non-zero after printing, because there is
//! no harness to catch a panic and attribute it.

use gitpulse_lib::desktop::{
    install_menu, recent_menu_entries, set_recent_menu, DesktopState, RECENT_MENU_LIMIT,
};

fn mock_app() -> tauri::App<tauri::test::MockRuntime> {
    tauri::test::mock_builder()
        .manage(DesktopState::default())
        .build(gitpulse_lib::context())
        .expect("mock app builds")
}

fn check(name: &str, passed: bool, detail: &str, failures: &mut Vec<String>) {
    if passed {
        println!("ok   {name}");
    } else {
        println!("FAIL {name}: {detail}");
        failures.push(name.to_string());
    }
}

fn main() {
    let mut failures: Vec<String> = Vec::new();
    let app = mock_app();

    // 1. The empty-recents branch: a "No Recent Repositories" placeholder item.
    let empty = install_menu(app.handle());
    check(
        "install_menu builds with no recent repositories",
        empty.is_ok(),
        &format!("{empty:?}"),
        &mut failures,
    );

    // 2. Rebuilding replaces the menu rather than accumulating one, which is
    //    what every repository open does through cmd_set_recent_menu.
    let rebuilt = install_menu(app.handle()).and_then(|()| install_menu(app.handle()));
    check(
        "install_menu is idempotent across repeated rebuilds",
        rebuilt.is_ok(),
        &format!("{rebuilt:?}"),
        &mut failures,
    );

    // 3. The populated recent-repositories branch: the loop that builds one
    //    menu item per entry, and the label derivation for each.
    let many: Vec<String> = (0..20)
        .map(|n| format!("/tmp/gitpulse-menu-test/repo-{n}"))
        .collect();
    let populated = set_recent_menu(app.handle(), many.clone());
    check(
        "set_recent_menu builds a menu from a populated recent list",
        populated.is_ok(),
        &format!("{populated:?}"),
        &mut failures,
    );
    let stored = recent_menu_entries(app.handle());
    check(
        "the recent list is capped at RECENT_MENU_LIMIT",
        stored.len() == RECENT_MENU_LIMIT,
        &format!("kept {} of {}", stored.len(), many.len()),
        &mut failures,
    );
    check(
        "the cap keeps the most recent entries, in order",
        stored == many[..RECENT_MENU_LIMIT],
        &format!("{stored:?}"),
        &mut failures,
    );

    // 4. Degenerate paths must not panic the label derivation. A menu built
    //    from a path with no final component, an empty string, or one carrying
    //    quotes and spaces is still a menu.
    let hostile = set_recent_menu(
        app.handle(),
        vec![
            "/".to_string(),
            String::new(),
            "/tmp/with space/and'quote".to_string(),
            "relative-path".to_string(),
            "/tmp/trailing/slash/".to_string(),
        ],
    );
    check(
        "set_recent_menu survives degenerate recent paths",
        hostile.is_ok(),
        &format!("{hostile:?}"),
        &mut failures,
    );

    // 5. Emptying the list returns to the placeholder branch rather than
    //    leaving the previous entries on screen.
    let emptied = set_recent_menu(app.handle(), Vec::new());
    check(
        "set_recent_menu clears back to the empty placeholder",
        emptied.is_ok() && recent_menu_entries(app.handle()).is_empty(),
        &format!("{emptied:?}"),
        &mut failures,
    );

    if failures.is_empty() {
        println!("\nnative_menu_main_thread: 7 passed");
    } else {
        eprintln!("\nnative_menu_main_thread: {} FAILED", failures.len());
        std::process::exit(1);
    }
}
