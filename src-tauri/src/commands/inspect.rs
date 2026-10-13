//! Read-only repository inspection: status, diffs, file content, blame, history search, language stats.

use super::*;

#[tauri::command(async)]
pub fn cmd_pick_folder() -> Option<String> {
    rfd::FileDialog::new()
        .set_title("Select Git Repository Directory")
        .pick_folder()
        .map(|p| p.to_string_lossy().to_string())
}

/// Long-running commands are `async fn`, but that alone only moves them off
/// the main thread: their bodies would still run as blocking code on the
/// shared async runtime's worker threads, starving every other command. Each
/// heavy body therefore runs through [`off_thread`], Tauri's blocking pool,
/// so an agent hammering status/fetch/graph in parallel keeps the runtime
/// responsive.
#[tauri::command(async)]
pub async fn cmd_list_branches(repo_path: String) -> Result<Vec<BranchInfo>, String> {
    off_thread(move || GitReader::list_branches(&repo_path)).await
}

/// Backfills per-branch churn after list_branches has rendered: the heavy
/// shortstat walks stay off the initial load and memoize through the oid-keyed
/// churn cache, so repeat calls while browsing are cheap.
#[tauri::command(async)]
pub async fn cmd_branch_stats(repo_path: String) -> Result<BranchStatsReport, String> {
    off_thread(move || GitReader::branch_stats(&repo_path)).await
}

#[tauri::command(async)]
pub async fn cmd_get_status(repo_path: String) -> Result<Vec<FileStatus>, String> {
    off_thread(move || GitReader::get_status(&repo_path)).await
}

#[tauri::command(async)]
pub async fn cmd_get_file_diff(
    repo_path: String,
    file_path: String,
    is_staged: bool,
    ignore_whitespace: Option<bool>,
) -> Result<DiffPayload, String> {
    off_thread(move || {
        GitReader::get_file_diff(
            &repo_path,
            &file_path,
            is_staged,
            ignore_whitespace.unwrap_or(false),
        )
    })
    .await
}

#[tauri::command(async)]
pub async fn cmd_get_commit_diff(
    repo_path: String,
    commit_id: String,
) -> Result<DiffPayload, String> {
    off_thread(move || GitReader::get_commit_diff(&repo_path, &commit_id)).await
}

#[tauri::command(async)]
pub async fn cmd_get_commit_file_diff(
    repo_path: String,
    commit_id: String,
    file_path: String,
) -> Result<DiffPayload, String> {
    off_thread(move || GitReader::get_commit_file_diff(&repo_path, &commit_id, &file_path)).await
}

#[tauri::command(async)]
pub async fn cmd_get_range_diff(
    repo_path: String,
    from: String,
    to: String,
) -> Result<DiffPayload, String> {
    off_thread(move || GitReader::get_range_diff(&repo_path, &from, &to)).await
}

#[tauri::command(async)]
pub async fn cmd_get_commit_files(
    repo_path: String,
    commit_id: String,
) -> Result<Vec<CommitFileChange>, String> {
    off_thread(move || GitReader::get_commit_files(&repo_path, &commit_id)).await
}

#[tauri::command(async)]
pub async fn cmd_get_commit_details(
    repo_path: String,
    commit_id: String,
) -> Result<CommitDetails, String> {
    off_thread(move || GitReader::get_commit_details(&repo_path, &commit_id)).await
}

#[tauri::command(async)]
pub async fn cmd_get_file_content(
    repo_path: String,
    file_path: String,
    commit_id: Option<String>,
) -> Result<String, String> {
    off_thread(move || GitReader::get_file_content(&repo_path, &file_path, commit_id.as_deref()))
        .await
}

#[tauri::command(async)]
pub async fn cmd_get_file_blob(
    repo_path: String,
    file_path: String,
    commit_id: Option<String>,
) -> Result<FileBlob, String> {
    off_thread(move || GitReader::get_file_blob(&repo_path, &file_path, commit_id.as_deref())).await
}

/// Writes a file inside the repository, after the write gate has judged the
/// path. This is the file editor's save path, so it is a real edit and is
/// gated as one, and it keeps the file's CRLF endings and refuses to replace
/// bytes the editor could not decode (see `sandbox_write_edited_text`).
#[tauri::command(async)]
pub async fn cmd_write_file_content(
    repo_path: String,
    file_path: String,
    content: String,
) -> Result<Guarded<()>, String> {
    super::payload::check_content("file content", &content, super::payload::MAX_CONTENT_BYTES)?;
    off_thread(move || {
        let policy = crate::harness::guard_file(&repo_path, &file_path, "modify")?;
        sandbox_write_edited_text(&repo_path, &file_path, &content)?;
        Ok(Guarded { policy, output: () })
    })
    .await
}

#[tauri::command(async)]
pub fn cmd_compute_word_diff(old_line: String, new_line: String) -> IntraLineDiff {
    compute_word_diff(&old_line, &new_line)
}

/// Tree-sitter syntax highlight spans for one document (UTF-16 offsets).
///
/// Languages MarkDev has no grammar for return an empty list — the frontend
/// owner then keeps its regex tokenizer. Oversized or malformed input is an
/// error so a truncated answer cannot look complete.
#[tauri::command(async)]
pub async fn cmd_syntax_highlight(
    language: String,
    code: String,
) -> Result<Vec<crate::syntax::SyntaxHighlightSpan>, String> {
    off_thread(move || crate::syntax::highlight(&language, &code)).await
}

#[tauri::command(async)]
pub fn cmd_detect_language(file_path: String) -> LanguageInfo {
    LanguageDetector::detect_from_path(&file_path)
}

#[tauri::command(async)]
pub fn cmd_count_loc(content: String, comment_prefix: Option<String>) -> LineCounts {
    LocCounter::count(&content, comment_prefix.as_deref())
}

#[tauri::command(async)]
pub fn cmd_parse_conventional_commit(message: String) -> Option<ConventionalCommit> {
    ConventionalCommitParser::new().parse(&message)
}

#[tauri::command(async)]
pub async fn cmd_get_file_blame(
    repo_path: String,
    file_path: String,
    revision: Option<String>,
) -> Result<Vec<BlameLine>, String> {
    off_thread(move || GitReader::get_file_blame(&repo_path, &file_path, revision.as_deref())).await
}

#[tauri::command(async)]
pub async fn cmd_list_repo_files(repo_path: String) -> Result<Vec<String>, String> {
    off_thread(move || GitReader::list_repo_files(&repo_path)).await
}

#[tauri::command(async)]
pub async fn cmd_list_tags(
    repo_path: String,
) -> Result<crate::engine::git_reader::TagList, String> {
    off_thread(move || GitReader::list_tags(&repo_path)).await
}

#[tauri::command(async)]
pub async fn cmd_get_reflog(
    repo_path: String,
    max_entries: Option<usize>,
) -> Result<Vec<ReflogEntry>, String> {
    off_thread(move || GitReader::get_reflog(&repo_path, max_entries.unwrap_or(200))).await
}

/// Repository-wide content search (`git grep`) over the working tree or one
/// revision. Bounded, with the reason named when the answer is partial.
/// `cancel_token` joins the shared query registry, so `cmd_codeintel_cancel`
/// stops a running search the same way it stops a code-graph query.
#[tauri::command(async)]
pub async fn cmd_search_content(
    repo_path: String,
    pattern: String,
    options: Option<crate::engine::content_search::ContentSearchOptions>,
    cancel_token: Option<String>,
) -> Result<crate::engine::content_search::ContentSearchReport, String> {
    off_thread(move || {
        let cancel = crate::codeintel::begin_cancellable_query(cancel_token.as_deref());
        let result = crate::engine::content_search::search(
            &repo_path,
            &pattern,
            &options.unwrap_or_default(),
            &|| cancel.is_cancelled(),
        );
        crate::codeintel::finish_cancellable_query(cancel_token.as_deref());
        result
    })
    .await
}

/// What resetting HEAD's branch to `target` would take off it. Read-only and
/// ungated; the reset itself goes through the guarded [`cmd_reset`].
#[tauri::command(async)]
pub async fn cmd_reset_preview(repo_path: String, target: String) -> Result<ResetPreview, String> {
    off_thread(move || GitReader::reset_preview(&repo_path, &target)).await
}

#[tauri::command(async)]
pub async fn cmd_get_language_stats(repo_path: String) -> Result<LanguageStatsReport, String> {
    off_thread(move || GitReader::get_repo_language_stats(&repo_path)).await
}

/// Fresh HEAD identity for menu copy actions, including detached HEAD.
#[tauri::command(async)]
pub async fn cmd_get_head_id(repo_path: String) -> Result<String, String> {
    off_thread(move || GitReader::head_id(&repo_path)).await
}
