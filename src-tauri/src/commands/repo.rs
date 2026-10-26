//! Repository resolution, trust grants and filesystem watching.

use super::*;
use crate::ipc_error::CommandError;

#[tauri::command(async)]
pub async fn cmd_resolve_repo(repo_path: String) -> Result<ResolvedRepo, IpcError> {
    off_thread(move || resolve_repo(&repo_path).map_err(CommandError::from)).await
}

#[tauri::command(async)]
pub async fn cmd_repository_trust(
    repo_path: String,
) -> Result<crate::repository_trust::TrustPreview, IpcError> {
    off_thread(move || crate::repository_trust::inspect(&repo_path)).await
}

#[tauri::command(async)]
pub async fn cmd_grant_repository_trust(
    repo_path: String,
    expected_identity: String,
) -> Result<(), IpcError> {
    off_thread(move || {
        crate::repository_trust::grant_confirmed(
            &repo_path,
            &expected_identity,
            &crate::desktop::confirm::native,
        )
    })
    .await
}

#[tauri::command(async)]
pub async fn cmd_revoke_repository_trust(repo_path: String) -> Result<(), IpcError> {
    off_thread(move || crate::repository_trust::revoke(&repo_path)).await
}

#[tauri::command(async)]
pub async fn cmd_watch_repo(
    app: AppHandle,
    state: State<'_, WatcherState>,
    repo_path: String,
) -> Result<String, IpcError> {
    // start_watch shells out to git (validate_repo, resolve_git_dir/common
    // dir) and installs OS watches — the same blocking profile as every other
    // reader command, so it belongs on the blocking pool rather than an async
    // worker thread. WatcherState is an Arc-backed Clone; snapshot it so the
    // 'static closure does not borrow the request-scoped State.
    let watcher_state = state.inner().clone();
    off_thread(move || start_watch(app, &watcher_state, repo_path).map_err(CommandError::from))
        .await
}

#[tauri::command(async)]
pub async fn cmd_unwatch_repo(
    state: State<'_, WatcherState>,
    repo_path: String,
) -> Result<(), IpcError> {
    let watcher_state = state.inner().clone();
    off_thread(move || unwatch(&watcher_state, repo_path).map_err(CommandError::from)).await
}
