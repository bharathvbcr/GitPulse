//! Merge conflict sessions: snapshot, save, preview.

use super::*;

#[tauri::command(async)]
pub async fn cmd_conflict_snapshot(
    repo_path: String,
    file_path: String,
) -> Result<ConflictSnapshot, String> {
    off_thread(move || conflict_session::snapshot(&repo_path, &file_path)).await
}

#[tauri::command(async)]
pub async fn cmd_save_conflict(
    repo_path: String,
    request: ConflictSaveRequest,
) -> Result<Guarded<ConflictSaveOutcome>, String> {
    off_thread(move || {
        save_conflict_folded(
            &repo_path,
            &request,
            |file, op| crate::harness::guard_file(&repo_path, file, op),
            |argv| guard(&repo_path, argv),
        )
    })
    .await
}

/// Saves a conflict resolution with every judged line, the target file's
/// included, folded into one verdict. Authorizes the literal target and the
/// exact object/index commands; the native transaction builds argv once for
/// judging and execution.
pub(super) fn save_conflict_folded(
    repo_path: &str,
    request: &ConflictSaveRequest,
    judge_file: impl Fn(&str, &str) -> Result<crate::harness::PolicyVerdict, String>,
    judge_command: impl Fn(&[&str]) -> Result<crate::harness::PolicyVerdict, String>,
) -> Result<Guarded<ConflictSaveOutcome>, String> {
    let fold = VerdictFold::default();
    let output = conflict_session::save_with_gate(
        repo_path,
        request,
        |file, op| {
            fold.record(judge_file(file, op)?);
            Ok(())
        },
        |argv| {
            fold.record(judge_command(argv)?);
            Ok(())
        },
    )?;
    let policy = fold
        .finish()
        .ok_or("Conflict transaction did not produce a verdict")?;
    Ok(Guarded { policy, output })
}

#[tauri::command(async)]
pub fn cmd_preview_conflict(document: ConflictDocument) -> Result<String, String> {
    ConflictResolver::render_preview(&document).map_err(str::to_owned)
}
