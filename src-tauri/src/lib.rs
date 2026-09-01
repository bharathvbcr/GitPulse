pub mod ai;
pub mod analyzer;
pub mod ci_local;
pub mod commands;
pub(crate) mod coverage_toolchain;
pub mod desktop;
pub mod diff;
pub mod engine;
pub mod github;
pub mod graph;
pub mod harness;
pub mod ingest;
pub mod ledger;
pub mod logging;
pub mod ops;
pub mod stack;
pub mod storage;
pub mod tasks;
pub mod terminal;
pub mod updates;
pub mod watcher;

use commands::*;
use desktop::{cmd_resolve_git_root, cmd_set_recent_menu, cmd_take_pending_open};
use logging::cmd_diagnostic_log_tail;

/// The application's Tauri context, generated once.
///
/// `generate_context!` embeds the bundle metadata (including Info.plist on
/// macOS) at each expansion site, so calling it in both `run` and the IPC
/// bridge tests made the linker warn about a duplicate `__EMBED_INFO_PLIST`
/// symbol. Generating it here lets the tests build a webview with the app's
/// real capabilities from the same source `run` uses.
pub fn context<R: tauri::Runtime>() -> tauri::Context<R> {
    tauri::generate_context!()
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    logging::init();
    #[cfg(not(test))]
    logging::install_panic_hook();
    tauri::Builder::default()
        .plugin(tauri_plugin_opener::init())
        .manage(crate::watcher::WatcherState::default())
        .manage(crate::terminal::TerminalSessions::default())
        .manage(desktop::DesktopState::default())
        .setup(|app| {
            // Installed before anything can mutate, so the first guarded action
            // of the session is announced like every one after it.
            crate::ledger::set_app_handle(app.handle().clone());
            if let Err(e) = desktop::install_menu(app.handle()) {
                log::error!(target: "setup", "menu installation failed: {e}");
                return Err(e.into());
            }
            Ok(())
        })
        .on_menu_event(|app, event| {
            desktop::handle_menu_event(app, event.id().as_ref());
        })
        .on_window_event(|window, event| {
            desktop::handle_window_event(window, event);
        })
        .invoke_handler(tauri::generate_handler![
            cmd_pick_folder,
            cmd_list_branches,
            cmd_branch_stats,
            cmd_get_status,
            cmd_get_commit_graph,
            cmd_get_file_diff,
            cmd_get_commit_diff,
            cmd_get_commit_file_diff,
            cmd_get_range_diff,
            cmd_get_commit_files,
            cmd_get_commit_details,
            cmd_get_file_content,
            cmd_get_file_blob,
            cmd_write_file_content,
            cmd_compute_word_diff,
            cmd_stage_file,
            cmd_unstage_file,
            cmd_stage_selective_patch,
            cmd_unstage_selective_patch,
            cmd_commit,
            cmd_quick_commit,
            cmd_checkout_branch,
            cmd_create_branch,
            cmd_delete_branch,
            cmd_rename_branch,
            cmd_clone_repo,
            cmd_parse_conflict,
            cmd_resolve_conflict,
            cmd_preview_conflict,
            cmd_detect_language,
            cmd_count_loc,
            cmd_parse_conventional_commit,
            cmd_get_file_blame,
            cmd_list_repo_files,
            cmd_rebase_interactive,
            cmd_get_stack_hierarchy,
            cmd_get_bezier_connector,
            cmd_list_tags,
            cmd_get_reflog,
            cmd_get_language_stats,
            cmd_scan_coverage,
            cmd_get_file_coverage,
            cmd_scan_deps_health,
            cmd_storage_scan,
            cmd_branch_cleanup_plan,
            cmd_review_outgoing_commits,
            cmd_fetch,
            cmd_pull,
            cmd_push,
            cmd_merge_branch,
            cmd_restack,
            cmd_stash_save,
            cmd_stash_pop,
            cmd_repo_operation,
            cmd_repo_operation_action,
            cmd_stash_list,
            cmd_stash_show,
            cmd_stash_action,
            cmd_cherry_pick,
            cmd_revert,
            cmd_reset,
            cmd_list_remotes,
            cmd_remote_change,
            cmd_list_submodules,
            cmd_submodule_change,
            cmd_discard_changes,
            cmd_github_context,
            cmd_github_dependabot_alerts,
            cmd_github_create_issue,
            cmd_github_checkout_pr,
            cmd_github_workflows,
            cmd_github_trigger_workflow,
            cmd_github_rerun_run,
            cmd_github_cancel_run,
            cmd_ci_local,
            cmd_watch_repo,
            cmd_unwatch_repo,
            cmd_resolve_repo,
            cmd_list_worktrees,
            cmd_add_worktree,
            cmd_remove_worktree,
            cmd_lock_worktree,
            cmd_unlock_worktree,
            cmd_prune_worktree,
            cmd_create_tag,
            cmd_delete_tag,
            cmd_publish_release,
            cmd_harness_status,
            cmd_harness_reconnect,
            cmd_policy_check_command,
            cmd_ai_status,
            cmd_ai_generate_commit_message,
            cmd_ai_explain_commit,
            cmd_ai_suggest_branch_name,
            cmd_ai_fix_health,
            cmd_ai_coverage_report,
            cmd_terminal_spawn,
            cmd_terminal_write,
            cmd_terminal_resize,
            cmd_terminal_kill,
            cmd_terminal_run,
            cmd_manvi_run_action,
            cmd_take_pending_open,
            cmd_set_recent_menu,
            cmd_resolve_git_root,
            cmd_diagnostic_log_tail,
            cmd_ledger_tail,
            cmd_ledger_status,
            cmd_task_view,
            cmd_task_scope,
            cmd_bind_worktree_task,
            cmd_unbind_worktree_task,
            cmd_worktree_task,
            cmd_catch_up,
            cmd_check_app_update,
        ])
        .build(context())
        .expect("error while building GitPulse")
        .run(|app, event| {
            desktop::handle_run_event(app, &event);
        });
}
