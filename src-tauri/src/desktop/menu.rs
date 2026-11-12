use super::actions::{self, NativeAction};
use tauri::menu::{
    AboutMetadata, CheckMenuItem, IsMenuItem, Menu, MenuItem, MenuItemKind, PredefinedMenuItem,
    Submenu, HELP_SUBMENU_ID, WINDOW_SUBMENU_ID,
};
use tauri::{AppHandle, Runtime};

/// File-menu accelerators for repository tabs. CmdOrCtrl+1–9 jump to a
/// repository tab (`REPO_TAB_BINDINGS`); the views take a different chord.
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

/// Agents accelerator. ⌘⇧A is free: the webview shortcut table does not
/// claim it, and it sits beside Fleet rather than on a reserved chord.
const AGENTS_ACCEL: &str = "CmdOrCtrl+Shift+A";

/// File-menu digit shortcuts: ⌘1–⌘8 select repository tab 1–8 and ⌘9 the last
/// one, as a browser does. The same on every OS, and a native accelerator so it
/// works while focus is in the terminal or an editor. The views must not reuse
/// these digits on the same modifier.
const REPO_TAB_BINDINGS: &[(&str, &str, &str)] = &[
    (actions::REPO_TAB_1, "Repository Tab 1", "CmdOrCtrl+1"),
    (actions::REPO_TAB_2, "Repository Tab 2", "CmdOrCtrl+2"),
    (actions::REPO_TAB_3, "Repository Tab 3", "CmdOrCtrl+3"),
    (actions::REPO_TAB_4, "Repository Tab 4", "CmdOrCtrl+4"),
    (actions::REPO_TAB_5, "Repository Tab 5", "CmdOrCtrl+5"),
    (actions::REPO_TAB_6, "Repository Tab 6", "CmdOrCtrl+6"),
    (actions::REPO_TAB_7, "Repository Tab 7", "CmdOrCtrl+7"),
    (actions::REPO_TAB_8, "Repository Tab 8", "CmdOrCtrl+8"),
    (actions::REPO_TAB_LAST, "Last Repository Tab", "CmdOrCtrl+9"),
];
const _: () = assert!(REPO_TAB_BINDINGS.len() == 9);

/// View-menu digit shortcuts on macOS: ⌥⌘1–3. Never ⌘⇧3, ⌘⇧4 or ⌘⇧5, which
/// are the system screenshot shortcuts, and never plain ⌥1–3, which are the
/// section chords the webview owns.
const VIEW_TAB_BINDINGS_MACOS: &[(&str, &str, &str)] = &[
    (actions::TAB_CODE, "Code", "CmdOrCtrl+Alt+1"),
    (actions::TAB_HISTORY, "History", "CmdOrCtrl+Alt+2"),
    (actions::TAB_INSIGHTS, "Insights", "CmdOrCtrl+Alt+3"),
];

/// View-menu digit shortcuts on Windows and Linux: Ctrl+Shift+1–3. Not
/// Ctrl+Alt+digit, which is AltGr on many keyboard layouts and so types a
/// character instead of reaching the menu; not Alt+digit, which the webview
/// owns for sections. Ctrl+Shift is also the off-macOS convention the
/// terminal's own find and text-size chords use.
const VIEW_TAB_BINDINGS_OTHER: &[(&str, &str, &str)] = &[
    (actions::TAB_CODE, "Code", "Ctrl+Shift+1"),
    (actions::TAB_HISTORY, "History", "Ctrl+Shift+2"),
    (actions::TAB_INSIGHTS, "Insights", "Ctrl+Shift+3"),
];

/// Nine digits are available; consolidation frees them as views merge, and
/// the list must never claim more than exist. Both tables name the same views
/// in the same order: only the chord differs per host.
const _: () = assert!(VIEW_TAB_BINDINGS_MACOS.len() <= 9);
const _: () = assert!(VIEW_TAB_BINDINGS_MACOS.len() == VIEW_TAB_BINDINGS_OTHER.len());

/// The view chords this host binds. Both tables are always compiled (the
/// choice is a value, not a `#[cfg]`), so the tests can check every host's
/// table from any host.
fn view_tab_bindings() -> &'static [(&'static str, &'static str, &'static str)] {
    if cfg!(target_os = "macos") {
        VIEW_TAB_BINDINGS_MACOS
    } else {
        VIEW_TAB_BINDINGS_OTHER
    }
}

fn item<R: Runtime>(
    app: &AppHandle<R>,
    id: &str,
    title: &str,
    accelerator: Option<&str>,
) -> tauri::Result<MenuItemKind<R>> {
    if super::state::checkable(id) {
        Ok(MenuItemKind::Check(CheckMenuItem::with_id(
            app,
            id,
            title,
            true,
            false,
            accelerator,
        )?))
    } else {
        Ok(MenuItemKind::MenuItem(MenuItem::with_id(
            app,
            id,
            title,
            true,
            accelerator,
        )?))
    }
}

/// The switcher's last row when more repositories are open than it lists.
fn hidden_repositories_label(hidden: u32) -> String {
    if hidden == 1 {
        "1 More Open Repository in the Tab Bar".to_string()
    } else {
        format!("{hidden} More Open Repositories in the Tab Bar")
    }
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
    state: &super::state::MenuState,
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
                super::state::menu_text(&if recents
                    .iter()
                    .filter(|other| recent_label(other) == recent_label(path))
                    .count()
                    > 1
                {
                    path.clone()
                } else {
                    recent_label(path)
                }),
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

    open_recent.append(&PredefinedMenuItem::separator(app)?)?;
    open_recent.append(&item(
        app,
        actions::CLEAR_RECENTS,
        "Clear Recent Repositories",
        None,
    )?)?;

    let open_repositories = Submenu::with_id_and_items(
        app,
        "open-repositories-menu",
        "Open Repositories",
        true,
        &[],
    )?;
    if state.repositories.is_empty() {
        open_repositories.append(&MenuItem::with_id(
            app,
            "repositories:empty",
            "No Open Repositories",
            false,
            None::<&str>,
        )?)?;
    }
    for repo in &state.repositories {
        open_repositories.append(&CheckMenuItem::with_id(
            app,
            format!("{}{}", actions::REPOSITORY_PREFIX, repo.path),
            super::state::menu_text(&repo.label),
            true,
            repo.active,
            None::<&str>,
        )?)?;
    }
    if state.repositories_hidden > 0 {
        open_repositories.append(&MenuItem::with_id(
            app,
            "repositories:hidden",
            hidden_repositories_label(state.repositories_hidden),
            false,
            None::<&str>,
        )?)?;
    }

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
    // Built from REPO_TAB_BINDINGS for the same reason as the view items: the
    // table is the one place the digits are listed.
    let repo_tab_items: Vec<MenuItemKind<R>> = REPO_TAB_BINDINGS
        .iter()
        .map(|(id, title, accel)| item(app, id, title, Some(accel)))
        .collect::<tauri::Result<_>>()?;
    let repo_tab_refs: Vec<&dyn IsMenuItem<R>> = repo_tab_items
        .iter()
        .map(|entry| entry as &dyn IsMenuItem<R>)
        .collect();
    let go_to_tab_menu = Submenu::with_items(app, "Go to Repository Tab", true, &repo_tab_refs)?;
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
        &open_repositories,
        &file_sep_tabs,
        &close_tab_item,
        &reopen_tab_item,
        &next_tab_item,
        &prev_tab_item,
        &go_to_tab_menu,
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

    // Built from the view table rather than nine hand-unrolled indexes.
    // The unrolled form silently required the list to be exactly nine long:
    // shortening it by one panics at startup, and lengthening it drops the
    // extra view from the menu with nothing to say so.
    let view_tab_items: Vec<MenuItemKind<R>> = view_tab_bindings()
        .iter()
        .map(|(id, title, accel)| item(app, id, title, Some(accel)))
        .collect::<tauri::Result<_>>()?;

    let work_item = item(app, actions::TAB_WORK, "Work", Some("F10"))?;
    let sep_one = PredefinedMenuItem::separator(app)?;
    let fleet_item = item(app, actions::FLEET, "Fleet", Some(FLEET_ACCEL))?;
    let agents_item = item(app, actions::AGENTS, "Agents", Some(AGENTS_ACCEL))?;
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
    let zoom_in = item(app, actions::ZOOM_IN, "Zoom In", Some("CmdOrCtrl+="))?;
    let zoom_out = item(app, actions::ZOOM_OUT, "Zoom Out", Some("CmdOrCtrl+-"))?;
    let reset_zoom = item(app, actions::RESET_ZOOM, "Actual Size", Some("CmdOrCtrl+0"))?;
    let zoom_separator = PredefinedMenuItem::separator(app)?;

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
    view_items.push(&agents_item);
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
    view_items.push(&zoom_in);
    view_items.push(&zoom_out);
    view_items.push(&reset_zoom);
    view_items.push(&zoom_separator);
    view_items.push(&fullscreen);

    let view_menu = Submenu::with_items(app, "View", true, &view_items)?;

    // Sections are destinations within the four views, not new top-level
    // views. Keep the existing view accelerators and per-repository selection.
    let section_menus: Vec<Submenu<R>> = actions::SECTION_MENUS
        .iter()
        .map(|group| {
            let entries: Vec<MenuItemKind<R>> = group
                .entries
                .iter()
                .map(|(id, title)| item(app, id, title, None))
                .collect::<tauri::Result<_>>()?;
            let refs: Vec<&dyn IsMenuItem<R>> = entries
                .iter()
                .map(|entry| entry as &dyn IsMenuItem<R>)
                .collect();
            Submenu::with_items(app, group.title, true, &refs)
        })
        .collect::<tauri::Result<_>>()?;
    let section_refs: Vec<&dyn IsMenuItem<R>> = section_menus
        .iter()
        .map(|entry| entry as &dyn IsMenuItem<R>)
        .collect();
    let go_menu = Submenu::with_items(app, "Go", true, &section_refs)?;

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
            &item(app, actions::STAGE_ALL, "Stage All", None)?,
            &item(app, actions::UNSTAGE_ALL, "Unstage All", None)?,
            &item(app, actions::QUICK_COMMIT, "Quick Commit…", None)?,
            &item(app, actions::REBASE, "Interactive Rebase…", None)?,
            &item(app, actions::CREATE_BRANCH, "Create Branch…", None)?,
            &item(app, actions::RENAME_BRANCH, "Rename Current Branch…", None)?,
            &PredefinedMenuItem::separator(app)?,
            &item(app, actions::OPERATION_CONTINUE, "Continue Operation", None)?,
            &item(app, actions::OPERATION_ABORT, "Abort Operation…", None)?,
            &item(app, actions::OPERATION_SKIP, "Skip Operation…", None)?,
            &PredefinedMenuItem::separator(app)?,
            &item(app, actions::COPY_REPO_PATH, "Copy Repository Path", None)?,
            &item(app, actions::COPY_BRANCH, "Copy Branch Name", None)?,
            &item(app, actions::COPY_COMMIT, "Copy HEAD SHA", None)?,
            &item(
                app,
                actions::REVEAL_REPO,
                // Each host's own name for its file manager. "File Manager" is
                // not what a Windows reader calls File Explorer, and the menu is
                // the one place GitPulse speaks the OS's vocabulary natively.
                if cfg!(target_os = "macos") {
                    "Reveal Repository in Finder"
                } else if cfg!(target_os = "windows") {
                    "Reveal Repository in File Explorer"
                } else {
                    "Reveal Repository in File Manager"
                },
                None,
            )?,
            &item(app, actions::OPEN_REMOTE, "Open Remote Website", None)?,
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
            &item(app, actions::DOCUMENTATION, "GitPulse Help", None)?,
            &item(
                app,
                actions::SHORTCUTS,
                "Keyboard Shortcuts…",
                Some("CmdOrCtrl+/"),
            )?,
            &PredefinedMenuItem::separator(app)?,
            &item(app, actions::DIAGNOSTICS, "Diagnostics…", None)?,
            &item(app, actions::SETUP_TOOLS, "Set Up Optional Tools…", None)?,
            &PredefinedMenuItem::separator(app)?,
            #[cfg(not(target_os = "macos"))]
            &item(app, actions::CHECK_UPDATES, "Check for Updates…", None)?,
            &item(app, actions::RELEASE_NOTES, "Release Notes", None)?,
            &item(app, actions::REPORT_ISSUE, "Report an Issue…", None)?,
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
            &item(app, actions::CHECK_UPDATES, "Check for Updates…", None)?,
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
            &go_menu,
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
            &go_menu,
            &repo_menu,
            &window_menu,
            &help_menu,
        ],
    )?;

    apply_presentation(&menu, state)?;
    Ok(menu)
}

/// Updates in place so a status refresh does not replace an open native menu.
pub fn apply_presentation<R: Runtime>(
    menu: &Menu<R>,
    state: &super::state::MenuState,
) -> tauri::Result<()> {
    fn visit<R: Runtime>(
        items: Vec<MenuItemKind<R>>,
        state: &super::state::MenuState,
    ) -> tauri::Result<()> {
        for entry in items {
            let id = entry.id().as_ref();
            let dynamic = id.starts_with(actions::RECENT_PREFIX)
                || id.starts_with(actions::REPOSITORY_PREFIX);
            let label = state.labels.iter().find(|label| label.id == id);
            match &entry {
                MenuItemKind::Submenu(sub) => visit(sub.items()?, state)?,
                MenuItemKind::MenuItem(item) if !dynamic && NativeAction::parse(id).is_some() => {
                    item.set_enabled(state.enabled(id))?;
                    if let Some(label) = label {
                        item.set_text(super::state::menu_text(&label.text))?;
                    }
                }
                MenuItemKind::Check(item) if id.starts_with(actions::REPOSITORY_PREFIX) => {
                    let path = id
                        .strip_prefix(actions::REPOSITORY_PREFIX)
                        .unwrap_or_default();
                    item.set_checked(
                        state
                            .repositories
                            .iter()
                            .any(|repo| repo.path == path && repo.active),
                    )?;
                }
                MenuItemKind::Check(item) if !dynamic => {
                    item.set_enabled(state.enabled(id))?;
                    item.set_checked(state.checked.iter().any(|checked| checked == id))?;
                    if let Some(label) = label {
                        item.set_text(super::state::menu_text(&label.text))?;
                    }
                }
                _ => {}
            }
        }
        Ok(())
    }
    visit(menu.items()?, state)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hidden_repositories_label_counts_what_the_switcher_left_out() {
        assert_eq!(
            hidden_repositories_label(1),
            "1 More Open Repository in the Tab Bar"
        );
        assert_eq!(
            hidden_repositories_label(936),
            "936 More Open Repositories in the Tab Bar"
        );
    }

    #[test]
    fn recent_label_uses_final_path_component() {
        assert_eq!(recent_label("/Users/acme/gitpulse"), "gitpulse");
        assert_eq!(recent_label("/"), "/");
        assert_eq!(recent_label("gitpulse"), "gitpulse");
    }

    /// Hosts whose chords differ. Windows and Linux share one table.
    const HOSTS: [bool; 2] = [true, false];

    fn is_plain_digit_accelerator(accel: &str) -> bool {
        (1..=9).any(|digit| {
            accel == format!("CmdOrCtrl+{digit}")
                || accel == format!("Cmd+{digit}")
                || accel == format!("Ctrl+{digit}")
        })
    }

    /// An accelerator as the host resolves it: modifiers sorted, `CmdOrCtrl`
    /// expanded to Command on macOS and Control elsewhere, so `CmdOrCtrl+W`
    /// and `Ctrl+W` are one chord off macOS and two on it.
    fn resolved(accel: &str, macos: bool) -> String {
        let (modifiers, key) = accel.rsplit_once('+').expect("accelerator has a key");
        let mut mods: Vec<&str> = modifiers
            .split('+')
            .map(|token| match token.to_ascii_lowercase().as_str() {
                "cmdorctrl" | "cmdorcontrol" | "commandorcontrol" | "commandorctrl" => {
                    if macos {
                        "cmd"
                    } else {
                        "ctrl"
                    }
                }
                "cmd" | "command" | "super" | "meta" => "cmd",
                "ctrl" | "control" => "ctrl",
                "alt" | "option" => "alt",
                "shift" => "shift",
                other => panic!("unknown modifier {other:?} in {accel}"),
            })
            .collect();
        mods.sort_unstable();
        format!("{}+{}", mods.join("+"), key.to_ascii_uppercase())
    }

    fn view_table(macos: bool) -> &'static [(&'static str, &'static str, &'static str)] {
        if macos {
            VIEW_TAB_BINDINGS_MACOS
        } else {
            VIEW_TAB_BINDINGS_OTHER
        }
    }

    /// Every accelerator the tables and constants here bind on `macos` or off
    /// it, resolved. Inline literals elsewhere in the builder are covered by
    /// `scripts/view-menu-contract.test.ts`, which reads the whole file.
    fn bound_accelerators(macos: bool) -> Vec<(String, String)> {
        let mut all: Vec<(String, String)> = [
            ("close tab", CLOSE_REPO_TAB_ACCEL),
            ("reopen tab", REOPEN_REPO_TAB_ACCEL),
            ("next tab", NEXT_REPO_TAB_ACCEL),
            ("previous tab", PREV_REPO_TAB_ACCEL),
            ("settings", SETTINGS_ACCEL),
            ("fleet", FLEET_ACCEL),
            ("agents", AGENTS_ACCEL),
        ]
        .into_iter()
        .map(|(owner, accel)| (owner.to_string(), resolved(accel, macos)))
        .collect();
        all.extend(
            REPO_TAB_BINDINGS
                .iter()
                .map(|(id, _, accel)| ((*id).to_string(), resolved(accel, macos))),
        );
        all.extend(
            view_table(macos)
                .iter()
                .map(|(id, _, accel)| ((*id).to_string(), resolved(accel, macos))),
        );
        all
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
            AGENTS_ACCEL,
        ] {
            assert!(
                !accel.eq_ignore_ascii_case("Shift+F10"),
                "{accel} takes the context-menu key"
            );
        }
        for (_, _, accel) in REPO_TAB_BINDINGS
            .iter()
            .chain(VIEW_TAB_BINDINGS_MACOS)
            .chain(VIEW_TAB_BINDINGS_OTHER)
        {
            assert!(
                !accel.eq_ignore_ascii_case("Shift+F10"),
                "{accel} takes the context-menu key"
            );
        }
    }

    #[test]
    fn no_two_bound_accelerators_collide_on_any_host() {
        for macos in HOSTS {
            let all = bound_accelerators(macos);
            // A parser that resolved nothing would pass the check below while
            // examining nothing.
            assert!(
                all.len() >= 7 + 9 + 3,
                "only {} accelerators resolved",
                all.len()
            );
            let mut seen = std::collections::HashMap::new();
            for (owner, chord) in &all {
                if let Some(first) = seen.insert(chord.clone(), owner.clone()) {
                    panic!(
                        "{chord} is bound by both {first} and {owner} (macos: {macos}); \
                         one of them silently never fires"
                    );
                }
            }
        }
    }

    #[test]
    fn repository_tab_chords_and_view_chords_are_disjoint_on_every_host() {
        for macos in HOSTS {
            let repo: std::collections::HashSet<String> = REPO_TAB_BINDINGS
                .iter()
                .map(|(_, _, accel)| resolved(accel, macos))
                .collect();
            assert_eq!(repo.len(), 9);
            for (id, _, accel) in view_table(macos) {
                let chord = resolved(accel, macos);
                assert!(
                    !repo.contains(&chord),
                    "{id} ({accel}) shares a chord with a repository tab (macos: {macos})"
                );
            }
        }
    }

    #[test]
    fn view_chords_avoid_altgr_and_the_macos_screenshot_shortcuts() {
        for (_, _, accel) in VIEW_TAB_BINDINGS_OTHER {
            let chord = resolved(accel, false);
            // Ctrl+Alt is AltGr on many layouts: the keystroke types a
            // character and never reaches the menu.
            assert!(
                !(chord.contains("ctrl") && chord.contains("alt")),
                "{accel} is AltGr on some layouts"
            );
            // Plain Alt+digit is the webview's section chord.
            assert!(
                chord.contains("ctrl") && chord.contains("shift"),
                "{accel} must be Ctrl+Shift+digit off macOS"
            );
        }
        for (_, _, accel) in VIEW_TAB_BINDINGS_MACOS {
            let chord = resolved(accel, true);
            assert_eq!(chord.split('+').count(), 3, "{accel} is not Cmd+Alt+digit");
            assert!(
                chord.starts_with("alt+cmd+"),
                "{accel} must be Option+Command+digit"
            );
            for screenshot in ["cmd+shift+3", "cmd+shift+4", "cmd+shift+5"] {
                assert_ne!(chord.to_ascii_lowercase(), screenshot);
            }
        }
        // And no macOS accelerator at all may be ⌘⇧3/4/5.
        for (owner, chord) in bound_accelerators(true) {
            for digit in 3..=5 {
                assert_ne!(
                    chord,
                    format!("cmd+shift+{digit}"),
                    "{owner} takes a macOS screenshot shortcut"
                );
            }
        }
    }

    #[test]
    fn repo_tab_accelerators_take_the_digits_and_nothing_else_does() {
        assert_eq!(CLOSE_REPO_TAB_ACCEL, "CmdOrCtrl+Shift+W");
        assert_eq!(REOPEN_REPO_TAB_ACCEL, "CmdOrCtrl+Shift+Y");
        assert_eq!(NEXT_REPO_TAB_ACCEL, "Ctrl+Tab");
        assert_eq!(PREV_REPO_TAB_ACCEL, "Ctrl+Shift+Tab");

        // The other File-menu tab accelerators stay off the digits.
        for accel in [
            CLOSE_REPO_TAB_ACCEL,
            REOPEN_REPO_TAB_ACCEL,
            NEXT_REPO_TAB_ACCEL,
            PREV_REPO_TAB_ACCEL,
        ] {
            assert!(
                !is_plain_digit_accelerator(accel),
                "{accel} must not bind Cmd/Ctrl+1–9; those select repository tabs"
            );
        }
        // Repository tabs are CmdOrCtrl+1…9 in order, 9 being the last tab.
        let digits: Vec<String> = REPO_TAB_BINDINGS
            .iter()
            .map(|(_, _, accel)| (*accel).to_string())
            .collect();
        let expected: Vec<String> = (1..=9).map(|digit| format!("CmdOrCtrl+{digit}")).collect();
        assert_eq!(digits, expected);
        let ids: Vec<&str> = REPO_TAB_BINDINGS.iter().map(|(id, _, _)| *id).collect();
        assert_eq!(ids, actions::REPO_TAB_IDS);
        for (id, _, _) in REPO_TAB_BINDINGS {
            assert!(NativeAction::parse(id).is_some(), "{id} has no action");
        }
    }

    #[test]
    fn view_tabs_take_the_digits_from_one_without_a_gap() {
        // Asserted as a shape rather than a fixed list of nine: consolidation
        // retires views, and a hardcoded expectation would have to be rewritten
        // every time while checking nothing that matters. What matters is that
        // the digits start at 1, have no gap, and no two views share one — a
        // gap or a duplicate is a shortcut that silently does the wrong thing.
        for macos in HOSTS {
            let table = view_table(macos);
            let digits: Vec<String> = table
                .iter()
                .map(|(_, _, accel)| accel.rsplit('+').next().unwrap_or_default().to_string())
                .collect();
            let expected: Vec<String> = (1..=table.len()).map(|digit| digit.to_string()).collect();
            assert_eq!(digits, expected, "macos: {macos}");
            assert_eq!(table[0].0, actions::TAB_CODE);
            assert_eq!(table[1].0, actions::TAB_HISTORY);
            // Retired views must not linger in the binding list: a menu item
            // for a view the frontend cannot resolve is inert, and looks like a
            // bug in the app rather than a stale table here.
            for (id, _, _) in table {
                assert!(
                    NativeAction::parse(id).is_some(),
                    "{id} has a digit shortcut but no action"
                );
            }
        }
        // Both hosts bind the same views in the same order; only the chord moves.
        fn ids(table: &'static [(&'static str, &'static str, &'static str)]) -> Vec<&'static str> {
            table.iter().map(|(id, _, _)| *id).collect()
        }
        assert_eq!(ids(VIEW_TAB_BINDINGS_MACOS), ids(VIEW_TAB_BINDINGS_OTHER));
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
        assert!(!is_plain_digit_accelerator(SETTINGS_ACCEL));
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
