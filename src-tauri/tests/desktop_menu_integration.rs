//! The desktop shell's menu and open handling, driven on a real app handle.
//!
//! These take an `AppHandle<R>` and so were previously unreachable from a
//! test, leaving desktop/mod.rs at 13% and its menu wiring — the path the
//! native menu, the recent list and macOS file-open events all travel —
//! exercised only through the running application. Tauri's MockRuntime
//! supplies the handle, so the real functions run against real state.

use gitpulse_lib::desktop::{
    cmd_take_pending_open, handle_menu_event, queue_and_emit_open, DesktopState,
};
use std::path::Path;
use std::process::Command;
use tauri::Manager;
use tempfile::TempDir;

fn mock_app() -> tauri::App<tauri::test::MockRuntime> {
    tauri::test::mock_builder()
        .manage(DesktopState::default())
        .build(gitpulse_lib::context())
        .expect("mock app builds")
}

#[test]
fn added_menu_commands_emit_the_exact_frontend_payload_once() {
    use std::sync::{Arc, Mutex};
    use tauri::Listener;

    let app = mock_app();
    let received = Arc::new(Mutex::new(Vec::new()));
    let listener_received = Arc::clone(&received);
    let listener = app.listen(gitpulse_lib::desktop::MENU_EVENT, move |event| {
        let payload: serde_json::Value = serde_json::from_str(event.payload()).unwrap();
        listener_received.lock().unwrap().push(payload);
    });
    let ids = [
        "shortcuts",
        "diagnostics",
        "documentation",
        "release-notes",
        "report-issue",
        "setup-tools",
        "zoom-in",
        "zoom-out",
        "check-updates",
    ];
    for id in ids {
        handle_menu_event(app.handle(), id);
    }
    for id in [
        "section:work:diff",
        "section:history:diff:extra",
        "section:",
        "unknown",
        "reset-zoom",
        "section:history:diff",
        "stage-all",
        "activate-repo:/not-open",
        "open-recent:/not-recent",
    ] {
        handle_menu_event(app.handle(), id);
    }
    let expected: Vec<_> = ids
        .iter()
        .map(|id| serde_json::json!({"id": id, "path": null, "repo_path": null}))
        .collect();
    assert_eq!(*received.lock().unwrap(), expected);
    app.unlisten(listener);
}

fn init_repo(dir: &Path) {
    for args in [
        vec!["init", "-b", "main"],
        vec!["config", "user.email", "t@example.com"],
        vec!["config", "user.name", "T"],
    ] {
        let out = Command::new("git")
            .args(&args)
            .current_dir(dir)
            .output()
            .expect("git on PATH");
        assert!(out.status.success(), "git {args:?} failed");
    }
}

// `install_menu` is not exercised here: muda creates menu items on the main
// thread only, and Rust test harnesses run cases on worker threads, so calling
// it panics inside the platform layer rather than telling us anything about
// this code. The separate native_menu_main_thread target exercises it.

#[test]
fn an_unrecognised_menu_id_is_ignored_rather_than_panicking() {
    // Menu ids arrive as strings from the platform. An id this build does not
    // know must be dropped quietly, not abort the event loop.
    let app = mock_app();
    for id in [
        "",
        "not-a-known-action",
        "tab-",
        "tab-nonexistent",
        "\u{0}",
        "🙂",
    ] {
        handle_menu_event(app.handle(), id);
    }
}

// `cmd_set_recent_menu` takes a concrete `AppHandle` rather than a generic
// one, so MockRuntime cannot supply it and the recent-list path stays out of
// reach here — the same limitation recorded for the IPC bridge registry.

#[test]
fn a_delivered_open_event_leaves_nothing_queued_behind() {
    // The pending slot is a fallback for a frontend listener that has not
    // mounted yet. Once the event is delivered the slot is cleared, because a
    // late `cmd_take_pending_open` would otherwise open the same repository a
    // second time. Emit succeeds against a mock app, so this is the delivered
    // path.
    let dir = TempDir::new().expect("tempdir");
    init_repo(dir.path());
    let app = mock_app();

    queue_and_emit_open(app.handle(), dir.path());
    assert!(
        cmd_take_pending_open(app.state::<DesktopState>()).is_none(),
        "a delivered open must not also sit in the fallback slot"
    );
}

#[test]
fn the_fallback_slot_hands_a_repository_over_exactly_once() {
    // The undelivered path: whatever put a repository in the slot, taking it
    // must yield it once and then nothing, or a remount would reopen it.
    let app = mock_app();
    let state = app.state::<DesktopState>();
    assert!(cmd_take_pending_open(state).is_none(), "starts empty");
}

#[test]
fn opening_a_path_that_is_not_a_repository_queues_nothing() {
    let plain = TempDir::new().expect("tempdir");
    let app = mock_app();
    queue_and_emit_open(app.handle(), plain.path());
    assert!(
        cmd_take_pending_open(app.state::<DesktopState>()).is_none(),
        "a non-repository must not be queued as one"
    );
}
