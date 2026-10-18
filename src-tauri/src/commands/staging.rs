//! Index and commit commands: staging, selective patches, committing, discarding changes.

use super::*;

#[tauri::command(async)]
pub async fn cmd_stage_file(repo_path: String, file_path: String) -> Result<Guarded<()>, IpcError> {
    off_thread(move || {
        let (verdicts, _) =
            GitWriter::change_index_with(&repo_path, &[file_path], IndexAction::Stage, |argv| {
                guard(&repo_path, argv)
            })?;
        let policy = verdicts
            .into_iter()
            .reduce(strictest_verdict)
            .ok_or("No index commands were evaluated")?;
        Ok(Guarded { policy, output: () })
    })
    .await
}

#[tauri::command(async)]
pub async fn cmd_unstage_file(repo_path: String, file_path: String) -> Result<Guarded<()>, IpcError> {
    off_thread(move || {
        let (verdicts, _) =
            GitWriter::change_index_with(&repo_path, &[file_path], IndexAction::Unstage, |argv| {
                guard(&repo_path, argv)
            })?;
        let policy = verdicts
            .into_iter()
            .reduce(strictest_verdict)
            .ok_or("No index commands were evaluated")?;
        Ok(Guarded { policy, output: () })
    })
    .await
}

/// Batch stage/unstage uses the same literal-path executor as single-file actions.
#[tauri::command(async)]
pub async fn cmd_change_index(
    repo_path: String,
    file_paths: Vec<String>,
    action: IndexAction,
) -> Result<Guarded<usize>, IpcError> {
    off_thread(move || {
        let (verdicts, output) =
            GitWriter::change_index_with(&repo_path, &file_paths, action, |argv| {
                guard(&repo_path, argv)
            })?;
        let policy = verdicts
            .into_iter()
            .reduce(strictest_verdict)
            .ok_or("No index commands were evaluated")?;
        Ok(Guarded { policy, output })
    })
    .await
}

#[tauri::command(async)]
pub async fn cmd_stage_selective_patch(
    repo_path: String,
    file_patch: FilePatch,
) -> Result<(), IpcError> {
    off_thread(move || {
        // Paths and line contents are interpolated verbatim into the patch
        // text; validate before anything reaches `git apply`.
        PatchBuilder::validate_file_patch(&file_patch)?;
        let patch = PatchBuilder::build_selective_patch(&file_patch, true);
        // This is a mutating git action (index rewrite via `git apply
        // --cached`), so it goes through the same command gate as every
        // other mutation. The argv is the one
        // GitWriter::apply_patch_to_index runs: both come from
        // apply_patch_to_index_argv. The verdict is not returned:
        // this command's IPC shape predates Guarded<()>, and changing it
        // would break the frontend contract — an allow proceeds silently, a
        // refusal surfaces as Err either way.
        guard(&repo_path, &GitWriter::apply_patch_to_index_argv())?;
        GitWriter::apply_patch_to_index(&repo_path, &patch)
    })
    .await
}

#[tauri::command(async)]
pub async fn cmd_unstage_selective_patch(
    repo_path: String,
    file_patch: FilePatch,
) -> Result<(), IpcError> {
    off_thread(move || {
        PatchBuilder::validate_file_patch(&file_patch)?;
        let patch = PatchBuilder::build_selective_patch(&file_patch, false);
        guard(&repo_path, &GitWriter::apply_patch_to_index_argv())?;
        GitWriter::apply_patch_to_index(&repo_path, &patch)
    })
    .await
}

/// Commits the index, after the harness's command gate has judged the exact
/// command line this would run.
#[tauri::command(async)]
pub async fn cmd_commit(
    repo_path: String,
    message: String,
    amend: bool,
) -> Result<Guarded<String>, IpcError> {
    off_thread(move || {
        // Mirror GitWriter::commit_inner's branching exactly so the judged
        // line is the line that would run: an amend with an empty message is
        // `commit --amend --no-edit`, never a fictional `-m ''`.
        let argv: Vec<&str> = if amend && message.is_empty() {
            vec!["git", "commit", "--amend", "--no-edit"]
        } else {
            let mut argv = vec!["git", "commit", "-m", message.as_str()];
            if amend {
                argv.push("--amend");
            }
            argv
        };
        let policy = guard(&repo_path, &argv)?;
        let output = GitWriter::commit(&repo_path, &message, amend)?;
        // After the commit landed, and unable to fail it: a local caller
        // record of the type the user actually committed, when that setting
        // is on and a low-confidence draft for this repository is pending.
        crate::lappi::record_commit_outcome(&repo_path, &message);
        Ok(Guarded { policy, output })
    })
    .await
}

/// Stages every tracked change and untracked non-ignored file, then commits,
/// after the harness has judged both command lines this would run.
///
/// Interactive "quick commit" (commit-all) is one mutation, not `stageAll`
/// followed by `cmd_commit`: two lock acquisitions can absorb a concurrent
/// writer's index.
#[tauri::command(async)]
pub async fn cmd_quick_commit(
    repo_path: String,
    message: String,
) -> Result<Guarded<String>, IpcError> {
    off_thread(move || {
        let add_argv: Vec<&str> = std::iter::once("git")
            .chain(GitWriter::QUICK_COMMIT_ADD_ARGV.iter().copied())
            .collect();
        let add_policy = guard(&repo_path, &add_argv)?;
        let commit_owned = commit_amend_argv(&message, false);
        let commit_argv: Vec<&str> = commit_owned.iter().map(String::as_str).collect();
        let commit_policy = guard(&repo_path, &commit_argv)?;
        let policy = strictest_verdict(add_policy, commit_policy);
        let output = GitWriter::quick_commit(&repo_path, &message)?;
        crate::lappi::record_commit_outcome(&repo_path, &message);
        Ok(Guarded { policy, output })
    })
    .await
}

/// Throws away a file's working-tree changes, after the write gate has judged
/// the path. Discarding is a write to the file, and the hard rules that protect
/// credential and restricted paths apply to it exactly as they do to an edit.
#[tauri::command(async)]
pub async fn cmd_discard_changes(
    repo_path: String,
    file_path: String,
) -> Result<Guarded<()>, IpcError> {
    off_thread(move || {
        let op = discard_file_op(&repo_path, &file_path);
        let policy = guard_file(&repo_path, &file_path, op)?;
        GitWriter::discard_changes(&repo_path, &file_path)?;
        Ok(Guarded { policy, output: () })
    })
    .await
}

/// The file operation `GitWriter::discard_changes` will actually perform.
///
/// It runs `git restore` and `git clean -f` against the same path, so the act
/// depends on whether the path is in the index: a tracked file is restored, an
/// untracked one is *removed* by the clean. Declaring "modify" for both sent
/// the policy sidecar — which receives `op` in `policy.check.file` — and the
/// ledger, which records it as `file.modify`, a weaker operation than the one
/// that ran, for exactly the case where the file is destroyed.
///
/// An unreadable index fails closed to the destructive reading: a probe that
/// could not run must not produce the gentler claim.
///
/// The probe is git and runs before the gate does, so it promotes the click
/// first: shed as a deferrable read under memory pressure it would fail closed
/// to "delete" for a file that is only being restored.
pub(super) fn discard_file_op(repo_path: &str, file_path: &str) -> &'static str {
    crate::engine::git_cli::mark_user_action(repo_path);
    match GitReader::is_tracked(repo_path, file_path) {
        Ok(true) => "modify",
        Ok(false) | Err(_) => "delete",
    }
}

/// The amend argv [`crate::engine::git_writer::GitWriter::commit_inner`] runs
/// for `(message, amend)`. Kept in lockstep with that private fn by test
/// (`commit_amend_argv_mirrors_commit_inner`); if the writer ever changes flag
/// shape or order, both must move together.
pub(super) fn commit_amend_argv(message: &str, amend: bool) -> Vec<String> {
    let mut args = vec!["git".to_string(), "commit".to_string()];
    if amend && message.is_empty() {
        args.push("--amend".into());
        args.push("--no-edit".into());
    } else {
        args.push("-m".into());
        args.push(message.to_string());
        if amend {
            args.push("--amend".into());
        }
    }
    args
}
