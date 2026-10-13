//! Markdown rendering, docs vault and path move/delete commands.

use super::*;

/// Renders markdown for embedding through MarkDev's renderer. With a note
/// location (`repo_path` + repo-relative `file_path`) relative pictures are
/// embedded from that repository; without one nothing is read from disk.
#[tauri::command(async)]
pub async fn cmd_markdown_render(
    text: String,
    repo_path: Option<String>,
    file_path: Option<String>,
) -> Result<crate::markdown::RenderedMarkdown, String> {
    off_thread(move || {
        // A location is both or neither: a path without its repository could
        // only be resolved against something the caller did not name.
        let note = match (repo_path.as_deref(), file_path.as_deref()) {
            (Some(repo), Some(file)) => Some((repo, file)),
            (None, None) => None,
            _ => return Err("a note location needs both repo_path and file_path".to_string()),
        };
        crate::markdown::render(&text, note)
    })
    .await
}

/// Refreshes tracked documents; watcher work yields subprocess capacity to user actions.
#[tauri::command(async)]
pub async fn cmd_docs_refresh(
    repo_path: String,
    background: Option<bool>,
) -> Result<crate::docs::DocsStatus, String> {
    off_thread(move || {
        if background.unwrap_or(false) {
            crate::engine::git_cli::with_background_processes(|| crate::docs::refresh(&repo_path))
        } else {
            crate::docs::refresh(&repo_path)
        }
    })
    .await
}

#[tauri::command(async)]
pub async fn cmd_docs_status(repo_path: String) -> Result<crate::docs::DocsStatus, String> {
    off_thread(move || crate::docs::status(&repo_path)).await
}

#[tauri::command(async)]
pub async fn cmd_docs_search(
    repo_path: String,
    query: String,
    limit: Option<u32>,
) -> Result<Vec<markdev_vault::SearchHit>, String> {
    off_thread(move || {
        crate::docs::search(
            &repo_path,
            &query,
            limit.unwrap_or(crate::docs::DEFAULT_SEARCH_LIMIT as u32) as usize,
        )
    })
    .await
}

#[tauri::command(async)]
pub async fn cmd_docs_broken_links(
    repo_path: String,
) -> Result<Vec<crate::docs::BrokenLink>, String> {
    off_thread(move || crate::docs::broken_links(&repo_path)).await
}

#[tauri::command(async)]
pub async fn cmd_docs_backlinks(
    repo_path: String,
    path: String,
) -> Result<Vec<markdev_vault::Backlink>, String> {
    off_thread(move || crate::docs::backlinks(&repo_path, &path)).await
}

#[tauri::command(async)]
pub async fn cmd_docs_graph(
    repo_path: String,
    focus: Option<String>,
    depth: Option<u32>,
    tag: Option<String>,
    folder: Option<String>,
) -> Result<markdev_vault::Graph, String> {
    off_thread(move || {
        crate::docs::graph(
            &repo_path,
            focus.as_deref(),
            depth.unwrap_or(0),
            tag.as_deref(),
            folder.as_deref(),
        )
    })
    .await
}

/// Link-preserving markdown rename: `git mv` + staged `rewrite_links_in` edits.
#[tauri::command(async)]
pub async fn cmd_docs_rename(
    repo_path: String,
    from: String,
    to: String,
) -> Result<Guarded<crate::docs::DocRenameOutcome>, String> {
    off_thread(move || {
        let mut policy = None;
        let outcome = crate::docs::rename_doc(&repo_path, &from, &to, |plan| {
            policy = Some(guard_move(&repo_path, plan)?);
            Ok(())
        })?;
        let policy = policy.ok_or("Doc rename did not produce a verdict")?;
        Ok(Guarded {
            policy,
            output: outcome,
        })
    })
    .await
}

/// Judges what [`GitWriter::move_path`] is about to do: the `git mv` line it
/// will run, or — for an untracked source, where no git command applies —
/// the delete of the source and the write of the destination.
pub(super) fn guard_move(
    repo_path: &str,
    plan: &crate::engine::git_writer::MovePlan,
) -> Result<crate::harness::PolicyVerdict, String> {
    use crate::engine::git_writer::MovePlan;
    match plan {
        MovePlan::Git { argv } => {
            let refs: Vec<&str> = argv.iter().map(String::as_str).collect();
            guard(repo_path, &refs)
        }
        MovePlan::Untracked { from, to } => {
            let source = guard_file(repo_path, from, "delete")?;
            let destination = guard_file(repo_path, to, "modify")?;
            Ok(strictest_verdict(source, destination))
        }
    }
}

/// Renames or moves a file or directory from the file tree. Tracked content
/// moves with `git mv` (staged); untracked content with a filesystem rename.
#[tauri::command(async)]
pub async fn cmd_move_path(
    repo_path: String,
    from: String,
    to: String,
) -> Result<Guarded<()>, String> {
    off_thread(move || {
        let mut policy = None;
        GitWriter::move_path(&repo_path, &from, &to, |plan| {
            policy = Some(guard_move(&repo_path, plan)?);
            Ok(())
        })?;
        let policy = policy.ok_or("Move did not produce a verdict")?;
        Ok(Guarded { policy, output: () })
    })
    .await
}

/// Deletes a file or directory from the file tree: `git rm -r` for tracked
/// content, `git clean -f -d` for untracked content, every line judged before
/// the first runs. The frontend confirms first; this does not second-guess.
#[tauri::command(async)]
pub async fn cmd_delete_path(
    repo_path: String,
    path: String,
) -> Result<Guarded<crate::engine::git_writer::DeleteOutcome>, String> {
    off_thread(move || {
        let fold = VerdictFold::default();
        let output = GitWriter::delete_path(&repo_path, &path, |plan| {
            for argv in plan {
                let refs: Vec<&str> = argv.iter().map(String::as_str).collect();
                fold.record(guard(&repo_path, &refs)?);
            }
            Ok(())
        })?;
        let policy = fold.finish().ok_or("Delete did not produce a verdict")?;
        Ok(Guarded { policy, output })
    })
    .await
}
