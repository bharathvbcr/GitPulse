use super::actions::{self, NativeAction};
use tauri::menu::{
    AboutMetadata, IsMenuItem, Menu, MenuItem, PredefinedMenuItem, Submenu, HELP_SUBMENU_ID,
    WINDOW_SUBMENU_ID,
};
use tauri::{AppHandle, Runtime};

/// File-menu accelerators for repository tabs. CmdOrCtrl+1–9 stay on View tabs.
const CLOSE_REPO_TAB_ACCEL: &str = "CmdOrCtrl+Shift+W";
const REOPEN_REPO_TAB_ACCEL: &str = "CmdOrCtrl+Shift+Y";
const NEXT_REPO_TAB_ACCEL: &str = "Ctrl+Tab";
const PREV_REPO_TAB_ACCEL: &str = "Ctrl+Shift+Tab";

/// App-menu Settings accelerator. The comma is the platform-standard ⌘, binding.
const SETTINGS_ACCEL: &str = "CmdOrCtrl+,";

/// Fleet accelerator.
///
/// Deliberately NOT Shift+F10, which it used to be. Shift+F10 is the platform
/// key for "open the context menu", and `CommitRow` and `FileTreePanel` both
/// implement it as exactly that — a native menu accelerator is consumed before
/// the webview sees the keystroke, so binding Fleet to it took the only
/// keyboard route to those context menus away. ⌘⇧F is free and sits beside
/// ⌘F (Search Commits) rather than on top of a reserved chord.
const FLEET_ACCEL: &str = "CmdOrCtrl+Shift+F";

/// View-menu digit shortcuts. Repository tab actions must not reuse these.
const VIEW_TAB_BINDINGS: &[(&str, &str, &str)] = &[
    (actions::TAB_CODE, "Code", "CmdOrCtrl+1"),
    (actions::TAB_HISTORY, "History", "CmdOrCtrl+2"),
    (actions::TAB_INSIGHTS, "Insights", "CmdOrCtrl+3"),
];
/// Nine digits are available; consolidation frees them as views merge, and
/// the list must never claim more than exist.
const _: () = assert!(VIEW_TAB_BINDINGS.len() <= 9);

fn item<R: Runtime>(
    app: &AppHandle<R>,
    id: &str,
    title: &str,
    accelerator: Option<&str>,
) -> tauri::Result<MenuItem<R>> {
    MenuItem::with_id(app, id, title, true, accelerator)
}

fn recent_label(path: &str) -> String {
    std::path::Path::new(path)
        .file_name()
        .and_then(|n| n.to_str())
        .filter(|n| !n.is_empty())
        .map(|n| n.to_string())
        .unwrap_or_else(|| path.to_string())
}

pub fn build_native_menu<R: Runtime>(
    app: &AppHandle<R>,
    recents: &[String],
) -> tauri::Result<Menu<R>> {
    let pkg = app.package_info();
    let about = AboutMetadata {
        name: Some(pkg.name.clone()),
        version: Some(pkg.version.to_string()),
        copyright: app.config().bundle.copyright.clone(),
        authors: app.config().bundle.publisher.clone().map(|p| vec![p]),
        ..Default::default()
    };

    let mut recent_items: Vec<MenuItem<R>> = Vec::new();
    if recents.is_empty() {
        recent_items.push(MenuItem::with_id(
            app,
            actions::RECENT_EMPTY,
            "No Recent Repositories",
            false,
            None::<&str>,
        )?);
    } else {
        for path in recents {
            recent_items.push(MenuItem::with_id(
                app,
                NativeAction::recent_menu_id(path),
                recent_label(path),
                true,
                None::<&str>,
            )?);
        }
    }
    let recent_refs: Vec<&dyn tauri::menu::IsMenuItem<R>> = recent_items
        .iter()
        .map(|item| item as &dyn tauri::menu::IsMenuItem<R>)
        .collect();
    let open_recent =
        Submenu::with_id_and_items(app, "open-recent-menu", "Open Recent", true, &recent_refs)?;

    let open_item = item(app, actions::OPEN, "Open Repository…", Some("CmdOrCtrl+O"))?;
    let clone_item = item(
        app,
        actions::CLONE,
        "Clone Repository…",
        Some("CmdOrCtrl+Shift+O"),
    )?;
    let close_tab_item = item(
        app,
        actions::CLOSE_TAB,
        "Close Repository Tab",
        Some(CLOSE_REPO_TAB_ACCEL),
    )?;
    let reopen_tab_item = item(
        app,
        actions::REOPEN_REPO_TAB,
        "Reopen Closed Repository",
        Some(REOPEN_REPO_TAB_ACCEL),
    )?;
    let next_tab_item = item(
        app,
        actions::NEXT_REPO_TAB,
        "Next Repository Tab",
        Some(NEXT_REPO_TAB_ACCEL),
    )?;
    let prev_tab_item = item(
        app,
        actions::PREV_REPO_TAB,
        "Previous Repository Tab",
        Some(PREV_REPO_TAB_ACCEL),
    )?;
    let close_window_item = PredefinedMenuItem::close_window(app, Some("Close Window"))?;
    let file_sep_tabs = PredefinedMenuItem::separator(app)?;
    let file_sep_close = PredefinedMenuItem::separator(app)?;
    #[cfg(not(target_os = "macos"))]
    let file_settings_sep = PredefinedMenuItem::separator(app)?;
    // macOS hosts Settings… in the app menu; Windows/Linux follow the
    // convention of a preferences entry at the bottom of the File menu.
    #[cfg(not(target_os = "macos"))]
    let file_settings_item = item(app, actions::SETTINGS, "Settings…", Some(SETTINGS_ACCEL))?;

    #[allow(unused_mut)]
    let mut file_refs: Vec<&dyn tauri::menu::IsMenuItem<R>> = vec![
        &open_item,
        &clone_item,
        &open_recent,
        &file_sep_tabs,
        &close_tab_item,
        &reopen_tab_item,
        &next_tab_item,
        &prev_tab_item,
        &file_sep_close,
        &close_window_item,
    ];
    #[cfg(not(target_os = "macos"))]
    {
        file_refs.push(&file_settings_sep);
        file_refs.push(&file_settings_item);
    }
    let file_menu = Submenu::with_items(app, "File", true, &file_refs)?;

    let edit_menu = Submenu::with_items(
        app,
        "Edit",
        true,
        &[
            &PredefinedMenuItem::undo(app, None)?,
            &PredefinedMenuItem::redo(app, None)?,
            &PredefinedMenuItem::separator(app)?,
            &PredefinedMenuItem::cut(app, None)?,
            &PredefinedMenuItem::copy(app, None)?,
            &PredefinedMenuItem::paste(app, None)?,
            &PredefinedMenuItem::select_all(app, None)?,
        ],
    )?;

    // Built from VIEW_TAB_BINDINGS rather than nine hand-unrolled indexes.
    // The unrolled form silently required the list to be exactly nine long:
    // shortening it by one panics at startup, and lengthening it drops the
    // extra view from the menu with nothing to say so.
    let view_tab_items: Vec<MenuItem<R>> = VIEW_TAB_BINDINGS
        .iter()
        .map(|(id, title, accel)| item(app, id, title, Some(accel)))
        .collect::<tauri::Result<_>>()?;

    let work_item = item(app, actions::TAB_WORK, "Work", Some("F10"))?;
    let sep_one = PredefinedMenuItem::separator(app)?;
    let fleet_item = item(app, actions::FLEET, "Fleet", Some(FLEET_ACCEL))?;
    let terminal_item = item(app, actions::TERMINAL_DOCK, "Terminal", Some("Ctrl+`"))?;
    let sep_two = PredefinedMenuItem::separator(app)?;
    let search_item = item(
        app,
        actions::FOCUS_FILTER,
        "Search Commits…",
        Some("CmdOrCtrl+F"),
    )?;
    let palette_item = item(app, actions::PALETTE, "Command Palette…", None)?;
    let refresh_item = item(app, actions::REFRESH, "Refresh", Some("CmdOrCtrl+R"))?;
    let sep_three = PredefinedMenuItem::separator(app)?;
    let theme_system = item(app, actions::THEME_SYSTEM, "Use System Appearance", None)?;
    let theme_light = item(app, actions::THEME_LIGHT, "Light Appearance", None)?;
    let theme_dark = item(app, actions::THEME_DARK, "Dark Appearance", None)?;
    let theme_toggle = item(
        app,
        actions::TOGGLE_THEME,
        "Toggle Dark / Light",
        Some("CmdOrCtrl+Shift+T"),
    )?;
    let sep_four = PredefinedMenuItem::separator(app)?;
    let fullscreen = PredefinedMenuItem::fullscreen(app, None)?;

    let mut view_items: Vec<&dyn IsMenuItem<R>> = Vec::new();
    for entry in &view_tab_items {
        view_items.push(entry);
    }
    // Work is the projection of everything else — tasks, worktrees, pull
    // requests, runs and verdicts on one screen. It takes F10 rather than a
    // digit: the digits are spoken for, and renumbering them to make room
    // would break muscle memory for the sake of ordering.
    view_items.push(&work_item);
    view_items.push(&sep_one);
    // Neither Fleet nor the terminal is a repository view. Fleet shows every
    // open repository at once; the terminal is a dock beneath whichever view
    // is on screen. Both sit after the separator, away from the views.
    // Ctrl, not Cmd, on every platform — Cmd+` is the macOS window cycler.
    view_items.push(&fleet_item);
    view_items.push(&terminal_item);
    view_items.push(&sep_two);
    view_items.push(&search_item);
    view_items.push(&palette_item);
    view_items.push(&refresh_item);
    view_items.push(&sep_three);
    view_items.push(&theme_system);
    view_items.push(&theme_light);
    view_items.push(&theme_dark);
    view_items.push(&theme_toggle);
    view_items.push(&sep_four);
    view_items.push(&fullscreen);

    let view_menu = Submenu::with_items(app, "View", true, &view_items)?;

    let repo_menu = Submenu::with_items(
        app,
        "Repository",
        true,
        &[
            &item(app, actions::FETCH, "Fetch", Some("CmdOrCtrl+Shift+K"))?,
            &item(app, actions::PULL, "Pull", Some("CmdOrCtrl+Shift+P"))?,
            &item(app, actions::PUSH, "Push", Some("CmdOrCtrl+Shift+U"))?,
            &PredefinedMenuItem::separator(app)?,
            &item(app, actions::STASH, "Stash Working Tree", None)?,
            &item(app, actions::STASH_POP, "Pop Stash", None)?,
            &PredefinedMenuItem::separator(app)?,
            &item(app, actions::QUICK_COMMIT, "Quick Commit…", None)?,
            &item(app, actions::REBASE, "Interactive Rebase…", None)?,
        ],
    )?;

    let window_menu = Submenu::with_id_and_items(
        app,
        WINDOW_SUBMENU_ID,
        "Window",
        true,
        &[
            &PredefinedMenuItem::minimize(app, None)?,
            &PredefinedMenuItem::maximize(app, None)?,
            &PredefinedMenuItem::separator(app)?,
            &PredefinedMenuItem::close_window(app, None)?,
        ],
    )?;

    let help_menu = Submenu::with_id_and_items(
        app,
        HELP_SUBMENU_ID,
        "Help",
        true,
        &[
            #[cfg(not(target_os = "macos"))]
            &PredefinedMenuItem::about(app, None, Some(about.clone()))?,
        ],
    )?;

    #[cfg(target_os = "macos")]
    let app_menu = Submenu::with_items(
        app,
        pkg.name.clone(),
        true,
        &[
            &PredefinedMenuItem::about(app, None, Some(about))?,
            &PredefinedMenuItem::separator(app)?,
            &item(app, actions::SETTINGS, "Settings…", Some(SETTINGS_ACCEL))?,
            &PredefinedMenuItem::separator(app)?,
            &PredefinedMenuItem::services(app, None)?,
            &PredefinedMenuItem::separator(app)?,
            &PredefinedMenuItem::hide(app, None)?,
            &PredefinedMenuItem::hide_others(app, None)?,
            &PredefinedMenuItem::show_all(app, None)?,
            &PredefinedMenuItem::separator(app)?,
            &PredefinedMenuItem::quit(app, None)?,
        ],
    )?;

    #[cfg(target_os = "macos")]
    let menu = Menu::with_items(
        app,
        &[
            &app_menu,
            &file_menu,
            &edit_menu,
            &view_menu,
            &repo_menu,
            &window_menu,
            &help_menu,
        ],
    )?;

    #[cfg(not(target_os = "macos"))]
    let menu = Menu::with_items(
        app,
        &[
            &file_menu,
            &edit_menu,
            &view_menu,
            &repo_menu,
            &window_menu,
            &help_menu,
        ],
    )?;

    Ok(menu)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn recent_label_uses_final_path_component() {
        assert_eq!(recent_label("/Users/acme/gitpulse"), "gitpulse");
        assert_eq!(recent_label("/"), "/");
        assert_eq!(recent_label("gitpulse"), "gitpulse");
    }

    fn is_view_digit_accelerator(accel: &str) -> bool {
        (1..=9).any(|digit| {
            accel == format!("CmdOrCtrl+{digit}")
                || accel == format!("Cmd+{digit}")
                || accel == format!("Ctrl+{digit}")
        })
    }

    /// Shift+F10 is the platform "open the context menu" key, and two
    /// components implement it for keyboard users. A native accelerator wins
    /// over the webview, so binding anything here to it silently disables
    /// them.
    #[test]
    fn no_accelerator_claims_the_context_menu_key() {
        assert_ne!(FLEET_ACCEL, "Shift+F10");
        for accel in [
            CLOSE_REPO_TAB_ACCEL,
            REOPEN_REPO_TAB_ACCEL,
            NEXT_REPO_TAB_ACCEL,
            PREV_REPO_TAB_ACCEL,
            SETTINGS_ACCEL,
            FLEET_ACCEL,
        ] {
            assert!(
                !accel.eq_ignore_ascii_case("Shift+F10"),
                "{accel} takes the context-menu key"
            );
        }
        for (_, _, accel) in VIEW_TAB_BINDINGS {
            assert!(
                !accel.eq_ignore_ascii_case("Shift+F10"),
                "{accel} takes the context-menu key"
            );
        }
    }

    #[test]
    fn fleet_does_not_collide_with_another_accelerator() {
        let mut all = vec![
            CLOSE_REPO_TAB_ACCEL,
            REOPEN_REPO_TAB_ACCEL,
            NEXT_REPO_TAB_ACCEL,
            PREV_REPO_TAB_ACCEL,
            SETTINGS_ACCEL,
            FLEET_ACCEL,
        ];
        all.extend(VIEW_TAB_BINDINGS.iter().map(|(_, _, accel)| *accel));
        let before = all.len();
        all.sort_unstable();
        all.dedup();
        assert_eq!(before, all.len(), "two menu items share an accelerator");
    }

    #[test]
    fn repo_tab_accelerators_do_not_steal_view_digits() {
        assert_eq!(CLOSE_REPO_TAB_ACCEL, "CmdOrCtrl+Shift+W");
        assert_eq!(REOPEN_REPO_TAB_ACCEL, "CmdOrCtrl+Shift+Y");
        assert_eq!(NEXT_REPO_TAB_ACCEL, "Ctrl+Tab");
        assert_eq!(PREV_REPO_TAB_ACCEL, "Ctrl+Shift+Tab");

        for accel in [
            CLOSE_REPO_TAB_ACCEL,
            REOPEN_REPO_TAB_ACCEL,
            NEXT_REPO_TAB_ACCEL,
            PREV_REPO_TAB_ACCEL,
        ] {
            assert!(
                !is_view_digit_accelerator(accel),
                "{accel} must not bind Cmd/Ctrl+1–9; those are view tabs"
            );
        }
    }

    #[test]
    fn view_tabs_take_the_digits_from_one_without_a_gap() {
        // Asserted as a shape rather than a fixed list of nine: consolidation
        // retires views, and a hardcoded expectation would have to be rewritten
        // every time while checking nothing that matters. What matters is that
        // the digits start at 1, have no gap, and no two views share one — a
        // gap or a duplicate is a shortcut that silently does the wrong thing.
        let digits: Vec<String> = VIEW_TAB_BINDINGS
            .iter()
            .map(|(_, _, accel)| (*accel).to_string())
            .collect();
        let expected: Vec<String> = (1..=VIEW_TAB_BINDINGS.len())
            .map(|digit| format!("CmdOrCtrl+{digit}"))
            .collect();
        assert_eq!(digits, expected);
        assert_eq!(VIEW_TAB_BINDINGS[0].0, actions::TAB_CODE);
        assert_eq!(VIEW_TAB_BINDINGS[1].0, actions::TAB_HISTORY);
        // Retired views must not linger in the binding list: a menu item for
        // a view the frontend cannot resolve is inert, and looks like a bug
        // in the app rather than a stale table here.
        for (id, _, _) in VIEW_TAB_BINDINGS {
            assert!(
                NativeAction::parse(id).is_some(),
                "{id} has a digit shortcut but no action"
            );
        }
    }

    #[test]
    fn settings_menu_item_is_parseable_and_platform_standard() {
        assert_eq!(
            NativeAction::parse(actions::SETTINGS),
            Some(NativeAction::Settings)
        );
        // ⌘, is the macOS-standard application-settings accelerator; the
        // portable CmdOrCtrl spelling maps onto it per platform (⌘, on macOS,
        // Ctrl+, for the Windows/Linux File-menu entry).
        assert_eq!(
            SETTINGS_ACCEL, "CmdOrCtrl+,",
            "Settings accelerator must stay ⌘,"
        );
        assert!(!is_view_digit_accelerator(SETTINGS_ACCEL));
        // tauri 2.x exposes no public Accelerator parse API (the type lives in
        // the internal muda crate); guard the accelerator shape here instead
        // and rely on the menu builder to reject malformed strings at runtime.
        let parts: Vec<&str> = SETTINGS_ACCEL.split('+').collect();
        assert_eq!(parts.len(), 2, "expected exactly one modifier and one key");
        assert!(parts.iter().all(|p| !p.is_empty()));
        // Every platform exposes exactly one Settings entry: app menu on macOS,
        // bottom of File elsewhere. build_native_menu enforces this by adding
        // file_settings_item only under #[cfg(not(target_os = "macos"))].
    }
}
