//! Read-only repository inspection: status, diffs, file content, blame, history search, language stats.

use super::*;
use crate::engine::commit_signature::{verify_commit_signature, CommitSignatureCheck};

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
pub async fn cmd_list_branches(repo_path: String) -> Result<Vec<BranchInfo>, IpcError> {
    off_thread(move || GitReader::list_branches(&repo_path)).await
}

/// Backfills per-branch churn after list_branches has rendered: the heavy
/// shortstat walks stay off the initial load and memoize through the oid-keyed
/// churn cache, so repeat calls while browsing are cheap.
#[tauri::command(async)]
pub async fn cmd_branch_stats(repo_path: String) -> Result<BranchStatsReport, IpcError> {
    off_thread(move || GitReader::branch_stats(&repo_path)).await
}

#[tauri::command(async)]
pub async fn cmd_get_status(repo_path: String) -> Result<Vec<FileStatus>, IpcError> {
    off_thread(move || GitReader::get_status(&repo_path)).await
}

#[tauri::command(async)]
pub async fn cmd_get_file_diff(
    repo_path: String,
    file_path: String,
    is_staged: bool,
    ignore_whitespace: Option<bool>,
) -> Result<DiffPayload, IpcError> {
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
) -> Result<DiffPayload, IpcError> {
    off_thread(move || GitReader::get_commit_diff(&repo_path, &commit_id)).await
}

#[tauri::command(async)]
pub async fn cmd_get_commit_file_diff(
    repo_path: String,
    commit_id: String,
    file_path: String,
) -> Result<DiffPayload, IpcError> {
    off_thread(move || GitReader::get_commit_file_diff(&repo_path, &commit_id, &file_path)).await
}

#[tauri::command(async)]
pub async fn cmd_get_range_diff(
    repo_path: String,
    from: String,
    to: String,
) -> Result<DiffPayload, IpcError> {
    off_thread(move || GitReader::get_range_diff(&repo_path, &from, &to)).await
}

#[tauri::command(async)]
pub async fn cmd_get_commit_files(
    repo_path: String,
    commit_id: String,
) -> Result<Vec<CommitFileChange>, IpcError> {
    off_thread(move || GitReader::get_commit_files(&repo_path, &commit_id)).await
}

#[tauri::command(async)]
pub async fn cmd_get_commit_details(
    repo_path: String,
    commit_id: String,
) -> Result<CommitDetails, IpcError> {
    off_thread(move || GitReader::get_commit_details(&repo_path, &commit_id)).await
}

/// Verifies the signature of the one commit the person selected. Separate from
/// [`cmd_get_commit_details`] because it runs their signing program, which can
/// take seconds (a keyserver, a pinentry); the details pane renders first and
/// this answers after, cached for a minute against the signing configuration.
#[tauri::command(async)]
pub async fn cmd_verify_commit_signature(
    repo_path: String,
    commit_id: String,
) -> Result<CommitSignatureCheck, IpcError> {
    off_thread(move || verify_commit_signature(&repo_path, &commit_id)).await
}

#[tauri::command(async)]
pub async fn cmd_get_file_content(
    repo_path: String,
    file_path: String,
    commit_id: Option<String>,
) -> Result<String, IpcError> {
    off_thread(move || GitReader::get_file_content(&repo_path, &file_path, commit_id.as_deref()))
        .await
}

#[tauri::command(async)]
pub async fn cmd_get_file_blob(
    repo_path: String,
    file_path: String,
    commit_id: Option<String>,
    with_digest: Option<bool>,
) -> Result<FileBlob, IpcError> {
    off_thread(move || {
        // The digest names the version a save is judged against. A reader that
        // never saves (the diff viewer) asks for none and skips the hashing
        // pass; leaving the argument out keeps the digest, as before.
        if with_digest.unwrap_or(true) {
            GitReader::get_file_blob(&repo_path, &file_path, commit_id.as_deref())
        } else {
            GitReader::get_file_blob_view(&repo_path, &file_path, commit_id.as_deref())
        }
    })
    .await
}

/// Writes a file inside the repository, after the write gate has judged the
/// path. This is the file editor's save path, so it is a real edit and is
/// gated as one, and it keeps the file's CRLF endings and refuses to replace
/// bytes the editor could not decode (see `sandbox_write_edited_text`).
///
/// `expected_base` is the `disk_base` of the blob the draft was made from: the
/// save is refused as `changed_on_disk` when the file no longer has those
/// bytes. Leaving it out is an explicit overwrite. The reply's `disk_base` is
/// the base the draft carries from here on.
///
/// `overwrite` is the conflict banner's Overwrite: the buffer replaces a disk
/// file the editor could not round-trip (invalid UTF-8, mixed line endings)
/// byte-exactly instead of being refused. Leaving it out is an ordinary save.
#[tauri::command(async)]
pub async fn cmd_write_file_content(
    repo_path: String,
    file_path: String,
    content: String,
    expected_base: Option<String>,
    overwrite: Option<bool>,
) -> Result<Guarded<SavedFile>, FileWriteError> {
    super::payload::check_content("file content", &content, super::payload::MAX_CONTENT_BYTES)
        .map_err(|e| FileWriteError::from(e.to_string()))?;
    // The pool carries `String` errors; the typed one rides inside the `Ok`.
    off_thread(move || {
        Ok((|| {
            let policy = guard_file(&repo_path, &file_path, "modify")?;
            let save = if overwrite.unwrap_or(false) {
                sandbox_overwrite_edited_text
            } else {
                sandbox_save_edited_text
            };
            let output = save(&repo_path, &file_path, &content, expected_base.as_deref())?;
            Ok(Guarded { policy, output })
        })())
    })
    .await.map_err(|e| e.message)?
}

/// Creates an empty file that must not exist yet, for Explorer's New File and
/// the `.gitkeep` of New Folder. Unlike [`cmd_write_file_content`] it never
/// replaces anything: an existing file, directory or link at the path is an
/// `already_exists` error and is left as it was.
#[tauri::command(async)]
pub async fn cmd_create_file(
    repo_path: String,
    file_path: String,
) -> Result<Guarded<()>, FileWriteError> {
    off_thread(move || {
        Ok((|| {
            let policy = guard_file(&repo_path, &file_path, "create")?;
            sandbox_create(&repo_path, &file_path)?;
            Ok(Guarded { policy, output: () })
        })())
    })
    .await.map_err(|e| e.message)?
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
) -> Result<Vec<crate::syntax::SyntaxHighlightSpan>, IpcError> {
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
) -> Result<Vec<BlameLine>, IpcError> {
    off_thread(move || GitReader::get_file_blame(&repo_path, &file_path, revision.as_deref())).await
}

#[tauri::command(async)]
pub async fn cmd_list_repo_files(repo_path: String) -> Result<Vec<String>, IpcError> {
    off_thread(move || GitReader::list_repo_files(&repo_path)).await
}

#[tauri::command(async)]
pub async fn cmd_list_tags(
    repo_path: String,
) -> Result<crate::engine::git_reader::TagList, IpcError> {
    off_thread(move || GitReader::list_tags(&repo_path)).await
}

#[tauri::command(async)]
pub async fn cmd_get_reflog(
    repo_path: String,
    max_entries: Option<usize>,
) -> Result<Vec<ReflogEntry>, IpcError> {
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
) -> Result<crate::engine::content_search::ContentSearchReport, IpcError> {
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
pub async fn cmd_reset_preview(repo_path: String, target: String) -> Result<ResetPreview, IpcError> {
    off_thread(move || GitReader::reset_preview(&repo_path, &target)).await
}

#[tauri::command(async)]
pub async fn cmd_get_language_stats(repo_path: String) -> Result<LanguageStatsReport, IpcError> {
    off_thread(move || GitReader::get_repo_language_stats(&repo_path)).await
}

/// Fresh HEAD identity for menu copy actions, including detached HEAD.
#[tauri::command(async)]
pub async fn cmd_get_head_id(repo_path: String) -> Result<String, IpcError> {
    off_thread(move || GitReader::head_id(&repo_path)).await
}
