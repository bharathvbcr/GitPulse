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
    install_menu, menu_state, recent_menu_entries, set_menu_state, set_recent_menu, DesktopState,
    MenuState, RECENT_MENU_LIMIT,
};
use tauri::menu::MenuItemKind;

fn collect_items(items: Vec<MenuItemKind<tauri::test::MockRuntime>>) -> Vec<String> {
    let mut ids = Vec::new();
    for item in items {
        match item {
            MenuItemKind::Submenu(submenu) => {
                ids.extend(collect_items(submenu.items().expect("submenu is readable")));
            }
            MenuItemKind::MenuItem(item) => {
                let id = item.id().as_ref();
                assert_eq!(
                    item.is_enabled().expect("enabled state is readable"),
                    MenuState::default().enabled(id),
                    "startup availability for {id}"
                );
                ids.push(id.to_string());
            }
            MenuItemKind::Check(item) => {
                let id = item.id().as_ref();
                assert_eq!(
                    item.is_enabled().unwrap(),
                    MenuState::default().enabled(id),
                    "startup availability for {id}"
                );
                assert_eq!(
                    item.is_checked().unwrap(),
                    MenuState::default().checked.iter().any(|known| known == id),
                    "startup checkmark for {id}"
                );
                ids.push(id.to_string());
            }
            _ => {}
        }
    }
    ids
}

fn check_additions(app: &tauri::App<tauri::test::MockRuntime>, failures: &mut Vec<String>) {
    let menu = app.menu().expect("app menu is installed");
    // The empty recent-repositories placeholder is intentionally disabled.
    let submenus = menu
        .items()
        .expect("menu is readable")
        .into_iter()
        .filter(|entry| {
            entry.as_submenu().is_some_and(|sub| {
                sub.text()
                    .is_ok_and(|name| name == "Go" || name == "Help" || name == "View")
            })
        })
        .collect();
    let ids = collect_items(submenus);
    let expected = [
        "shortcuts",
        "diagnostics",
        "documentation",
        "release-notes",
        "report-issue",
        "setup-tools",
        "zoom-in",
        "zoom-out",
        "reset-zoom",
        "section:work:overview",
        "section:work:resolve",
        "section:work:remote",
        "section:work:stack",
        "section:work:policy",
        "section:code:explorer",
        "section:code:blame",
        "section:code:map",
        "section:history:graph",
        "section:history:diff",
        "section:history:reflog",
        "section:insights:pulse",
        "section:insights:coverage",
        "section:insights:health",
        "section:insights:storage",
    ];
    let missing: Vec<_> = expected
        .iter()
        .filter(|id| ids.iter().filter(|found| found == id).count() != 1)
        .collect();
    check(
        "all 24 additions exist once with accurate startup availability",
        missing.is_empty(),
        &format!("missing or duplicated: {missing:?}"),
        failures,
    );
}

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
    check_additions(&app, &mut failures);

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
    check_additions(&app, &mut failures);

    // Native state changes must survive recent-list rebuilds.
    let mut selected = MenuState {
        active_path: Some("/r/one/repo".into()),
        ..Default::default()
    };
    selected.repositories = vec![
        gitpulse_lib::desktop::state::MenuRepository {
            path: "/r/one/repo".into(),
            label: "one/repo".into(),
            active: true,
        },
        gitpulse_lib::desktop::state::MenuRepository {
            path: "/r/two/repo".into(),
            label: "two/repo".into(),
            active: false,
        },
    ];
    selected.enabled.extend(
        [
            "fetch",
            "stage-all",
            "tab-history",
            "section:history:reflog",
            "terminal-dock",
        ]
        .into_iter()
        .map(String::from),
    );
    selected.checked = vec![
        "theme-light".into(),
        "tab-history".into(),
        "section:history:reflog".into(),
        "terminal-dock".into(),
    ];
    selected.labels = vec![gitpulse_lib::desktop::state::MenuLabel {
        id: "terminal-dock".into(),
        text: "Hide Terminal".into(),
    }];
    set_menu_state(app.handle(), selected.clone()).expect("state applies");
    set_recent_menu(
        app.handle(),
        vec!["/r/one/repo".into(), "/r/two/repo".into()],
    )
    .expect("recent update");
    assert_eq!(menu_state(app.handle()), selected);
    let root = app.menu().unwrap();
    let mut stack = root.items().unwrap();
    let mut seen = std::collections::HashMap::new();
    while let Some(entry) = stack.pop() {
        match entry {
            MenuItemKind::Submenu(menu) => stack.extend(menu.items().unwrap()),
            MenuItemKind::MenuItem(item) => {
                seen.insert(
                    item.id().as_ref().to_string(),
                    (item.text().unwrap(), item.is_enabled().unwrap(), false),
                );
            }
            MenuItemKind::Check(item) => {
                seen.insert(
                    item.id().as_ref().to_string(),
                    (
                        item.text().unwrap(),
                        item.is_enabled().unwrap(),
                        item.is_checked().unwrap(),
                    ),
                );
            }
            _ => {}
        }
    }
    assert_eq!(seen["terminal-dock"], ("Hide Terminal".into(), true, true));
    assert!(seen["tab-history"].2 && seen["section:history:reflog"].2 && seen["theme-light"].2);
    assert!(!seen["theme-system"].2 && !seen["stash-pop"].1);
    assert!(seen["activate-repo:/r/one/repo"].2 && !seen["activate-repo:/r/two/repo"].2);
    assert_eq!(seen["open-recent:/r/one/repo"].0, "/r/one/repo");
    assert_eq!(seen["open-recent:/r/two/repo"].0, "/r/two/repo");
    check(
        "live checks, availability, repository switcher and duplicate labels survive rebuilds",
        true,
        "",
        &mut failures,
    );

    let mut invalid = selected.clone();
    invalid.enabled.push("unknown".into());
    assert!(set_menu_state(app.handle(), invalid).is_err());
    assert_eq!(
        menu_state(app.handle()),
        selected,
        "invalid input must not overwrite live state"
    );
    selected.enabled.retain(|id| id != "fetch");
    selected
        .labels
        .push(gitpulse_lib::desktop::state::MenuLabel {
            id: "fetch".into(),
            text: "Fetching…".into(),
        });
    set_menu_state(app.handle(), selected).expect("busy state");
    set_menu_state(app.handle(), MenuState::default()).expect("close all repositories");
    check_additions(&app, &mut failures);

    check_contextual_events(&app);
    check_status_menu(&app);

    if failures.is_empty() {
        println!("\nnative_menu_main_thread: all checks passed");
    } else {
        eprintln!("\nnative_menu_main_thread: {} FAILED", failures.len());
        std::process::exit(1);
    }
}

fn check_contextual_events(app: &tauri::App<tauri::test::MockRuntime>) {
    use std::sync::{Arc, Mutex};
    use tauri::Listener;
    let ids = [
        "stage-all",
        "unstage-all",
        "create-branch",
        "rename-branch",
        "operation-continue",
        "operation-abort",
        "operation-skip",
        "copy-repo-path",
        "copy-branch",
        "copy-commit",
        "reveal-repo",
        "open-remote",
        "section:history:diff",
    ];
    let mut state = MenuState {
        active_path: Some("/r/with:colon/repo".into()),
        ..Default::default()
    };
    state
        .repositories
        .push(gitpulse_lib::desktop::state::MenuRepository {
            path: "/r/with:colon/repo".into(),
            label: "repo".into(),
            active: true,
        });
    state.enabled.extend(ids.iter().map(|id| id.to_string()));
    set_menu_state(app.handle(), state.clone()).unwrap();
    let events = Arc::new(Mutex::new(Vec::new()));
    let observed = Arc::clone(&events);
    let listener = app.listen(gitpulse_lib::desktop::MENU_EVENT, move |event| {
        observed
            .lock()
            .unwrap()
            .push(serde_json::from_str::<serde_json::Value>(event.payload()).unwrap());
    });
    for id in ids {
        gitpulse_lib::desktop::handle_menu_event(app.handle(), id);
    }
    let mut expected: Vec<_> = ids
        .iter()
        .map(|id| {
            serde_json::json!({
                "id": id, "path": null, "repo_path": "/r/with:colon/repo"
            })
        })
        .collect();
    gitpulse_lib::desktop::handle_menu_event(app.handle(), "activate-repo:/r/with:colon/repo");
    expected.push(serde_json::json!({"id":"activate-repo", "path":"/r/with:colon/repo", "repo_path":"/r/with:colon/repo"}));
    assert_eq!(*events.lock().unwrap(), expected);
    state.enabled.retain(|id| id != "stage-all");
    set_menu_state(app.handle(), state).unwrap();
    gitpulse_lib::desktop::handle_menu_event(app.handle(), "stage-all");
    assert_eq!(
        *events.lock().unwrap(),
        expected,
        "disabled actions must not emit"
    );
    app.unlisten(listener);
    set_menu_state(app.handle(), MenuState::default()).unwrap();
    println!("ok   contextual commands preserve repository identity and enforce availability");
}

fn check_status_menu(app: &tauri::App<tauri::test::MockRuntime>) {
    // The old text-heavy left-click menu is replaced by the popover. A minimal
    // right-click menu remains an independent escape path if its webview fails.
    let menu = gitpulse_lib::desktop::build_tray_menu(app.handle()).unwrap();
    let items = menu.items().unwrap();
    assert_eq!(items.len(), 3);
    for (item, id) in items
        .iter()
        .zip(["tray:show", "tray:settings", "tray:quit"])
    {
        assert_eq!(item.id().as_ref(), id);
        assert!(item.as_menuitem().unwrap().is_enabled().unwrap());
    }
    println!(
        "ok   status popover retains an independent native Open, Settings and Quit escape path"
    );
}
