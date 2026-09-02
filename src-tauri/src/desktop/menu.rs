use super::actions::{self, NativeAction};
use tauri::menu::{
    AboutMetadata, Menu, MenuItem, PredefinedMenuItem, Submenu, HELP_SUBMENU_ID, WINDOW_SUBMENU_ID,
};
use tauri::{AppHandle, Runtime};

/// File-menu accelerators for repository tabs. CmdOrCtrl+1–9 stay on View tabs.
const CLOSE_REPO_TAB_ACCEL: &str = "CmdOrCtrl+Shift+W";
const REOPEN_REPO_TAB_ACCEL: &str = "CmdOrCtrl+Shift+Y";
const NEXT_REPO_TAB_ACCEL: &str = "Ctrl+Tab";
const PREV_REPO_TAB_ACCEL: &str = "Ctrl+Shift+Tab";

/// App-menu Settings accelerator. The comma is the platform-standard ⌘, binding.
const SETTINGS_ACCEL: &str = "CmdOrCtrl+,";

/// View-menu digit shortcuts. Repository tab actions must not reuse these.
const VIEW_TAB_BINDINGS: &[(&str, &str, &str)] = &[
    (actions::TAB_FILES, "Files", "CmdOrCtrl+1"),
    (actions::TAB_HISTORY, "Graph", "CmdOrCtrl+2"),
    (actions::TAB_DIFF, "Diff", "CmdOrCtrl+3"),
    (actions::TAB_CONFLICT, "Resolve Conflicts", "CmdOrCtrl+4"),
    (actions::TAB_BLAME, "Blame", "CmdOrCtrl+5"),
    (actions::TAB_STACK, "Stack", "CmdOrCtrl+6"),
    (actions::TAB_GITHUB, "GitHub", "CmdOrCtrl+7"),
    (actions::TAB_COVERAGE, "Coverage", "CmdOrCtrl+8"),
    (actions::TAB_HEALTH, "Health", "CmdOrCtrl+9"),
];
const _: () = assert!(VIEW_TAB_BINDINGS.len() == 9);

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

    let view_menu = Submenu::with_items(
        app,
        "View",
        true,
        &[
            &item(
                app,
                VIEW_TAB_BINDINGS[0].0,
                VIEW_TAB_BINDINGS[0].1,
                Some(VIEW_TAB_BINDINGS[0].2),
            )?,
            &item(
                app,
                VIEW_TAB_BINDINGS[1].0,
                VIEW_TAB_BINDINGS[1].1,
                Some(VIEW_TAB_BINDINGS[1].2),
            )?,
            &item(
                app,
                VIEW_TAB_BINDINGS[2].0,
                VIEW_TAB_BINDINGS[2].1,
                Some(VIEW_TAB_BINDINGS[2].2),
            )?,
            &item(
                app,
                VIEW_TAB_BINDINGS[3].0,
                VIEW_TAB_BINDINGS[3].1,
                Some(VIEW_TAB_BINDINGS[3].2),
            )?,
            &item(
                app,
                VIEW_TAB_BINDINGS[4].0,
                VIEW_TAB_BINDINGS[4].1,
                Some(VIEW_TAB_BINDINGS[4].2),
            )?,
            &item(
                app,
                VIEW_TAB_BINDINGS[5].0,
                VIEW_TAB_BINDINGS[5].1,
                Some(VIEW_TAB_BINDINGS[5].2),
            )?,
            &item(
                app,
                VIEW_TAB_BINDINGS[6].0,
                VIEW_TAB_BINDINGS[6].1,
                Some(VIEW_TAB_BINDINGS[6].2),
            )?,
            &item(
                app,
                VIEW_TAB_BINDINGS[7].0,
                VIEW_TAB_BINDINGS[7].1,
                Some(VIEW_TAB_BINDINGS[7].2),
            )?,
            &item(
                app,
                VIEW_TAB_BINDINGS[8].0,
                VIEW_TAB_BINDINGS[8].1,
                Some(VIEW_TAB_BINDINGS[8].2),
            )?,
            // Work is the projection of everything else — tasks, worktrees,
            // pull requests, runs and verdicts on one screen. It sits above
            // Terminal because it is where a session starts, and takes F10
            // rather than a digit: CmdOrCtrl+1..9 are already spoken for, and
            // renumbering nine existing shortcuts to make room would break
            // muscle memory for the sake of ordering.
            &item(app, actions::TAB_WORK, "Work", Some("F10"))?,
            &item(app, actions::TAB_TERMINAL, "Terminal", None)?,
            &item(app, actions::TAB_MANVI, "MANVI", None)?,
            // Storage and Reflog are registered views with a menu group, but
            // had no menu item: Storage had no action at all, and Reflog had
            // one that nothing could emit. Both were reachable only through
            // the command palette.
            &item(app, actions::TAB_STORAGE, "Storage", None)?,
            &item(app, actions::TAB_REPO, "Repo", None)?,
            &item(app, actions::TAB_REFLOG, "Reflog", None)?,
            &PredefinedMenuItem::separator(app)?,
            &item(
                app,
                actions::FOCUS_FILTER,
                "Search Commits…",
                Some("CmdOrCtrl+F"),
            )?,
            &item(app, actions::PALETTE, "Command Palette…", None)?,
            &item(app, actions::REFRESH, "Refresh", Some("CmdOrCtrl+R"))?,
            &PredefinedMenuItem::separator(app)?,
            &item(app, actions::THEME_SYSTEM, "Use System Appearance", None)?,
            &item(app, actions::THEME_LIGHT, "Light Appearance", None)?,
            &item(app, actions::THEME_DARK, "Dark Appearance", None)?,
            &item(
                app,
                actions::TOGGLE_THEME,
                "Toggle Dark / Light",
                Some("CmdOrCtrl+Shift+T"),
            )?,
            &PredefinedMenuItem::separator(app)?,
            &PredefinedMenuItem::fullscreen(app, None)?,
        ],
    )?;

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
    fn view_tabs_own_cmd_or_ctrl_1_through_9() {
        let digits: Vec<String> = VIEW_TAB_BINDINGS
            .iter()
            .map(|(_, _, accel)| (*accel).to_string())
            .collect();
        let expected: Vec<String> = (1..=9).map(|digit| format!("CmdOrCtrl+{digit}")).collect();
        assert_eq!(digits, expected);
        assert_eq!(VIEW_TAB_BINDINGS[0].0, actions::TAB_FILES);
        assert_eq!(VIEW_TAB_BINDINGS[1].0, actions::TAB_HISTORY);
        assert_eq!(VIEW_TAB_BINDINGS[7].0, actions::TAB_COVERAGE);
        assert_eq!(VIEW_TAB_BINDINGS[8].0, actions::TAB_HEALTH);
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
