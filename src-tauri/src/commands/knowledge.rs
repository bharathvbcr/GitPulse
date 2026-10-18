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
) -> Result<crate::markdown::RenderedMarkdown, IpcError> {
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
) -> Result<crate::docs::DocsStatus, IpcError> {
    if background.unwrap_or(false) {
        off_thread_background(move || crate::docs::refresh(&repo_path)).await
    } else {
        off_thread(move || crate::docs::refresh(&repo_path)).await
    }
}

#[tauri::command(async)]
pub async fn cmd_docs_status(repo_path: String) -> Result<crate::docs::DocsStatus, IpcError> {
    off_thread(move || crate::docs::status(&repo_path)).await
}

#[tauri::command(async)]
pub async fn cmd_docs_search(
    repo_path: String,
    query: String,
    limit: Option<u32>,
) -> Result<Vec<markdev_vault::SearchHit>, IpcError> {
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
) -> Result<Vec<crate::docs::BrokenLink>, IpcError> {
    off_thread(move || crate::docs::broken_links(&repo_path)).await
}

#[tauri::command(async)]
pub async fn cmd_docs_backlinks(
    repo_path: String,
    path: String,
) -> Result<Vec<markdev_vault::Backlink>, IpcError> {
    off_thread(move || crate::docs::backlinks(&repo_path, &path)).await
}

#[tauri::command(async)]
pub async fn cmd_docs_graph(
    repo_path: String,
    focus: Option<String>,
    depth: Option<u32>,
    tag: Option<String>,
    folder: Option<String>,
) -> Result<markdev_vault::Graph, IpcError> {
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
) -> Result<Guarded<crate::docs::DocRenameOutcome>, IpcError> {
    off_thread(move || docs_rename_guarded(&repo_path, &from, &to)).await
}

/// The body of [`cmd_docs_rename`]. Every path the rename will write is judged
/// with the move, before the move: one blocked path refuses the whole rename,
/// so a note the task may not touch is never rewritten behind a verdict that
/// only looked at the `git mv` line.
pub(super) fn docs_rename_guarded(
    repo_path: &str,
    from: &str,
    to: &str,
) -> Result<Guarded<crate::docs::DocRenameOutcome>, String> {
    let fold = VerdictFold::default();
    let outcome = crate::docs::rename_doc(repo_path, from, to, |plan| {
        fold.record(guard_move(repo_path, plan.mv)?);
        // The move is judged as the command it runs, but a write gate that
        // knows paths refuses by path: the source leaves and the destination
        // appears whether or not either note has links to rewrite.
        fold.record(guard_file(repo_path, plan.from, "delete")?);
        fold.record(guard_file(repo_path, plan.to, "modify")?);
        // Every rewrite is judged on its own path, but together: one scope
        // lookup and a bounded fan-out to the sidecar, not a round trip per
        // note while the repository lock is held.
        let rewrites: Vec<&str> = plan
            .rewrites
            .iter()
            .map(String::as_str)
            .filter(|path| *path != plan.to)
            .collect();
        for verdict in guard_files(repo_path, &rewrites, "modify")? {
            fold.record(verdict);
        }
        Ok(())
    })?;
    let policy = fold
        .finish()
        .ok_or("Doc rename did not produce a verdict")?;
    Ok(Guarded {
        policy,
        output: outcome,
    })
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
) -> Result<Guarded<()>, IpcError> {
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
) -> Result<Guarded<crate::engine::git_writer::DeleteOutcome>, IpcError> {
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

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use crate::harness::sidecar::{bind_test_binary, test_serial};
    use crate::test_support::{git_in, git_repo, write};
    use std::os::unix::fs::PermissionsExt;

    /// A `manvi serve` stand-in that records every request after the handshake
    /// and denies a file check for exactly one path.
    const FAKE_DENY_ONE_PATH: &str = r#"#!/bin/sh
reply() {
  id=$(printf '%s' "$1" | sed -n 's/^{"id":"\([0-9]*\)".*/\1/p')
  printf '{"id":"%s","ok":true,"result":%s}\n' "$id" "$2"
}
IFS= read -r line || exit 1
reply "$line" '{"protocol":1,"ops":["hello","policy.check.command","policy.check.file"],"posture":"host"}'
while IFS= read -r line; do
  printf '%s\n' "$line" >> "@REQUESTS@"
  case "$line" in
    *'"path":"@DENIED@"'*)
      reply "$line" '{"action":"deny","rule":"scope.forbidden","severity":"hard","reason":"forbidden by the task","target":"@DENIED@","task_id":"TASK-DOC","demoted":"","degraded":[]}';;
    *)
      reply "$line" '{"action":"allow","rule":"","severity":"","reason":"","target":"","task_id":"","demoted":"","degraded":[]}';;
  esac
done
"#;

    /// A bound repository whose task forbids `docs/Guide.md`, behind a sidecar
    /// that denies a write to `denied`.
    fn bound_doc_repo(denied: &str) -> (tempfile::TempDir, String, std::path::PathBuf) {
        let dir = git_repo();
        write(dir.path(), "docs/Guide.md", "# Guide\n\n[r](Roadmap.md)\n");
        write(dir.path(), "docs/Roadmap.md", "# Roadmap\n");
        write(dir.path(), "README.md", "[r](docs/Roadmap.md)\n");
        git_in(dir.path(), &["add", "docs", "README.md"]);
        git_in(dir.path(), &["commit", "-qm", "docs"]);
        let repo = dir
            .path()
            .canonicalize()
            .expect("canonical repository")
            .to_str()
            .expect("utf8")
            .to_string();

        let db = crate::tasks::store_path(&repo);
        std::fs::create_dir_all(db.parent().unwrap()).unwrap();
        dc_store::Store::open(&db)
            .unwrap()
            .connection()
            .execute(
                "INSERT INTO tasks (id, title, description, planned_files_json,
                     forbidden_changes_json, status)
                 VALUES ('TASK-DOC', 'Docs', '',
                     '[{\"path\":\"README.md\",\"allowed_change\":\"modify\"},
                       {\"path\":\"docs/Roadmap.md\",\"allowed_change\":\"modify\"}]',
                     '[\"docs/Guide.md\"]', 'in_progress')",
                [],
            )
            .unwrap();
        crate::ledger::bindings::bind(&repo, &repo, "TASK-DOC").expect("bind");

        let requests = dir.path().join("requests.ndjson");
        let script = dir.path().join("deny-one-manvi");
        let body = FAKE_DENY_ONE_PATH
            .replace("@REQUESTS@", &requests.display().to_string())
            .replace("@DENIED@", denied);
        std::fs::write(&script, body).unwrap();
        std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).unwrap();
        (dir, repo, script)
    }

    fn file_requests(requests: &std::path::Path) -> Vec<serde_json::Value> {
        std::fs::read_to_string(requests)
            .unwrap_or_default()
            .lines()
            .filter_map(|line| serde_json::from_str::<serde_json::Value>(line).ok())
            .filter(|request| request["op"] == "policy.check.file")
            .collect()
    }

    #[test]
    fn a_forbidden_rewrite_path_refuses_the_rename_before_the_move() {
        let (dir, repo, script) = bound_doc_repo("docs/Guide.md");
        let serial = test_serial();
        let _binary = bind_test_binary(&serial, script.to_string_lossy());

        let refused = docs_rename_guarded(&repo, "docs/Roadmap.md", "plans/Roadmap.md");
        assert!(refused.is_err(), "a denied rewrite must refuse the rename");

        // The judgement was made on the declared scope, for the file itself.
        let asked = file_requests(&dir.path().join("requests.ndjson"));
        let guide = asked
            .iter()
            .find(|request| request["params"]["path"] == "docs/Guide.md")
            .unwrap_or_else(|| panic!("no file check for docs/Guide.md: {asked:?}"));
        assert_eq!(guide["params"]["op"], "modify");
        assert_eq!(guide["params"]["scope"]["task_id"], "TASK-DOC");
        assert_eq!(
            guide["params"]["scope"]["forbidden_changes"][0],
            "docs/Guide.md"
        );

        // Nothing moved, nothing was written, nothing was staged.
        assert!(dir.path().join("docs/Roadmap.md").exists());
        assert!(!dir.path().join("plans").exists());
        assert_eq!(
            std::fs::read_to_string(dir.path().join("docs/Guide.md")).unwrap(),
            "# Guide\n\n[r](Roadmap.md)\n"
        );
        let staged =
            crate::engine::git_cli::git_text(dir.path(), &["diff", "--cached", "--name-only"])
                .unwrap();
        assert_eq!(staged, "");
    }

    #[test]
    fn an_allowed_rename_judges_the_move_and_each_path_it_writes() {
        let (dir, repo, script) = bound_doc_repo("nothing/is/denied.md");
        let serial = test_serial();
        let _binary = bind_test_binary(&serial, script.to_string_lossy());

        let guarded =
            docs_rename_guarded(&repo, "docs/Roadmap.md", "plans/Roadmap.md").expect("rename");
        assert!(guarded.output.complete, "{:?}", guarded.output);

        let asked: Vec<String> = file_requests(&dir.path().join("requests.ndjson"))
            .iter()
            .filter_map(|request| request["params"]["path"].as_str().map(String::from))
            .collect();
        for path in ["README.md", "docs/Guide.md"] {
            assert!(
                asked.iter().any(|p| p == path),
                "{path} was not judged: {asked:?}"
            );
        }
    }

    /// `count` more notes that all link at `docs/Roadmap.md`, committed.
    fn add_linking_notes(dir: &tempfile::TempDir, count: usize) -> Vec<String> {
        let paths: Vec<String> = (0..count)
            .map(|i| format!("docs/extra-{i:03}.md"))
            .collect();
        for path in &paths {
            write(dir.path(), path, "[r](Roadmap.md)\n");
        }
        git_in(dir.path(), &["add", "docs"]);
        git_in(dir.path(), &["commit", "-qm", "linking notes"]);
        paths
    }

    #[test]
    fn a_hub_rename_judges_every_rewrite_once_and_stages_in_one_batch() {
        let (dir, repo, script) = bound_doc_repo("nothing/is/denied.md");
        let extra = add_linking_notes(&dir, 40);
        let serial = test_serial();
        let _binary = bind_test_binary(&serial, script.to_string_lossy());

        let (guarded, labels) = crate::engine::git_cli::spawn_labels_during(|| {
            docs_rename_guarded(&repo, "docs/Roadmap.md", "plans/Roadmap.md")
        });
        let guarded = guarded.expect("rename");
        assert!(guarded.output.complete, "{:?}", guarded.output);
        assert_eq!(
            guarded.output.rewritten_paths.len(),
            42,
            "{:?}",
            guarded.output
        );

        let asked: Vec<String> = file_requests(&dir.path().join("requests.ndjson"))
            .iter()
            .filter_map(|request| request["params"]["path"].as_str().map(String::from))
            .collect();
        for path in extra
            .iter()
            .map(String::as_str)
            .chain(["README.md", "docs/Guide.md"])
        {
            assert_eq!(
                asked.iter().filter(|p| p.as_str() == path).count(),
                1,
                "{path} must be judged exactly once: {asked:?}"
            );
        }
        let adds = labels.iter().filter(|l| l.as_str() == "git add").count();
        assert_eq!(adds, 1, "{labels:?}");
    }

    #[test]
    fn a_refusal_in_the_middle_of_a_hub_refuses_the_rename_before_the_move() {
        let (dir, repo, script) = bound_doc_repo("docs/extra-017.md");
        add_linking_notes(&dir, 40);
        let serial = test_serial();
        let _binary = bind_test_binary(&serial, script.to_string_lossy());

        let refused = docs_rename_guarded(&repo, "docs/Roadmap.md", "plans/Roadmap.md");
        assert!(refused.is_err(), "one denied rewrite refuses the rename");
        assert!(dir.path().join("docs/Roadmap.md").exists());
        assert!(!dir.path().join("plans").exists());
        let staged =
            crate::engine::git_cli::git_text(dir.path(), &["diff", "--cached", "--name-only"])
                .unwrap();
        assert_eq!(staged, "");
    }

    /// A denial at file k refuses before the move, and the ledger holds a
    /// record for the files up to k and none after it, though the workers had
    /// already asked the sidecar about later files. Runs the production
    /// 8-worker path against the fake sidecar.
    #[test]
    fn a_denial_at_file_k_records_nothing_after_k() {
        let (dir, repo, script) = bound_doc_repo("docs/extra-017.md");
        let extra = add_linking_notes(&dir, 120);
        let serial = test_serial();
        let _binary = bind_test_binary(&serial, script.to_string_lossy());

        let refused = docs_rename_guarded(&repo, "docs/Roadmap.md", "plans/Roadmap.md");
        assert!(refused.is_err());
        assert!(dir.path().join("docs/Roadmap.md").exists());

        let recorded: Vec<String> = crate::ledger::tail(&repo, 0, 1000)
            .unwrap()
            .into_iter()
            .filter(|event| event.action == "file.modify")
            .filter_map(|event| event.object)
            .collect();
        // Every rewrite is judged in the order the plan lists it; the denied
        // one is the last record, so nothing that sorts after it was written.
        let denied = recorded
            .iter()
            .position(|p| p == "docs/extra-017.md")
            .unwrap_or_else(|| panic!("the denial is not recorded: {recorded:?}"));
        assert_eq!(denied, recorded.len() - 1, "{recorded:?}");
        for path in &extra {
            let asked_after = recorded[denied + 1..].contains(path);
            assert!(!asked_after, "{path} recorded after the denial");
        }
    }

    /// A warning on one file among many still reaches the caller's verdict.
    #[test]
    fn a_warning_among_many_rewrites_still_surfaces() {
        let (dir, repo, script) = bound_doc_repo("docs/extra-021.md");
        add_linking_notes(&dir, 30);
        // The same fake, answering "warn" where it would have said "deny".
        let body = std::fs::read_to_string(&script).unwrap();
        std::fs::write(
            &script,
            body.replace("\"action\":\"deny\"", "\"action\":\"warn\""),
        )
        .unwrap();
        let serial = test_serial();
        let _binary = bind_test_binary(&serial, script.to_string_lossy());
        let guarded =
            docs_rename_guarded(&repo, "docs/Roadmap.md", "plans/Roadmap.md").expect("rename");
        assert_eq!(
            guarded.policy.status,
            crate::harness::PolicyStatus::Warned,
            "{:?}",
            guarded.policy
        );
    }

    // ---- adversarial gate cases ------------------------------------------

    /// Writes `body` out as an executable stand-in for the `manvi serve` binary.
    fn script_with(dir: &tempfile::TempDir, body: &str) -> std::path::PathBuf {
        let script = dir.path().join("adversarial-manvi");
        std::fs::write(&script, body).unwrap();
        std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).unwrap();
        script
    }

    /// A sidecar that records every request and answers `@ACTION@` for a file
    /// check naming `@PATH@`, and allows everything else.
    const FAKE_ONE_VERDICT: &str = r#"#!/bin/sh
reply() {
  id=$(printf '%s' "$1" | sed -n 's/^{"id":"\([0-9]*\)".*/\1/p')
  printf '{"id":"%s","ok":true,"result":%s}\n' "$id" "$2"
}
IFS= read -r line || exit 1
reply "$line" '{"protocol":1,"ops":["hello","policy.check.command","policy.check.file"],"posture":"host"}'
while IFS= read -r line; do
  printf '%s\n' "$line" >> "@REQUESTS@"
  case "$line" in
    *'"path":"@PATH@"'*)
      reply "$line" '{"action":"@ACTION@","rule":"scope.forbidden","severity":"hard","reason":"adversarial","target":"@PATH@","task_id":"TASK-DOC","demoted":"","degraded":[]}';;
    *)
      reply "$line" '{"action":"allow","rule":"","severity":"","reason":"","target":"","task_id":"","demoted":"","degraded":[]}';;
  esac
done
"#;

    fn verdict_script(dir: &tempfile::TempDir, path: &str, action: &str) -> std::path::PathBuf {
        let requests = dir.path().join("requests.ndjson");
        let body = FAKE_ONE_VERDICT
            .replace("@REQUESTS@", &requests.display().to_string())
            .replace("@PATH@", path)
            .replace("@ACTION@", action);
        script_with(dir, &body)
    }

    #[test]
    fn adversarial_a_sidecar_that_dies_refuses_the_rename_before_the_move() {
        let (dir, repo, _) = bound_doc_repo("nothing");
        let dead = script_with(&dir, "#!/bin/sh\nexit 1\n");
        let serial = test_serial();
        let _binary = bind_test_binary(&serial, dead.to_string_lossy());

        let result = docs_rename_guarded(&repo, "docs/Roadmap.md", "plans/Roadmap.md");
        assert!(
            result.is_err(),
            "a gate that could not answer must refuse: {result:?}"
        );
        assert!(dir.path().join("docs/Roadmap.md").exists());
        assert!(!dir.path().join("plans").exists());
        assert_eq!(
            std::fs::read_to_string(dir.path().join("docs/Guide.md")).unwrap(),
            "# Guide\n\n[r](Roadmap.md)\n"
        );
    }

    #[test]
    fn adversarial_a_warning_on_one_rewrite_is_the_verdict_the_rename_reports() {
        let (dir, repo, _) = bound_doc_repo("nothing");
        let script = verdict_script(&dir, "README.md", "warn");
        let serial = test_serial();
        let _binary = bind_test_binary(&serial, script.to_string_lossy());

        let guarded = docs_rename_guarded(&repo, "docs/Roadmap.md", "plans/Roadmap.md")
            .expect("a warning does not refuse");
        assert_eq!(
            guarded.policy.status,
            crate::harness::PolicyStatus::Warned,
            "the strictest of Warned and Allowed must win: {:?}",
            guarded.policy
        );
        assert!(guarded.output.complete);
    }

    #[test]
    fn adversarial_a_destination_only_a_file_check_would_refuse_still_refuses_the_rename() {
        // The moved note has no links to rewrite, so `plans/Roadmap.md` is not
        // among the rewrites. `git mv` is judged as a command line; the file
        // gate is what knows the task forbids writing that path.
        let (dir, repo, _) = bound_doc_repo("nothing");
        let script = verdict_script(&dir, "plans/Roadmap.md", "deny");
        let serial = test_serial();
        let _binary = bind_test_binary(&serial, script.to_string_lossy());

        let result = docs_rename_guarded(&repo, "docs/Roadmap.md", "plans/Roadmap.md");
        let asked: Vec<String> = file_requests(&dir.path().join("requests.ndjson"))
            .iter()
            .filter_map(|r| r["params"]["path"].as_str().map(String::from))
            .collect();
        assert!(
            result.is_err(),
            "renamed into a path the file gate denies; file checks made: {asked:?}"
        );
    }

    #[test]
    fn adversarial_a_denied_moved_note_that_is_rewritten_refuses_the_rename() {
        let (dir, repo, _) = bound_doc_repo("nothing");
        // The moved note links to Guide, so `plans/Roadmap.md` is rewritten.
        write(
            dir.path(),
            "docs/Roadmap.md",
            "# Roadmap\n\n[g](Guide.md)\n",
        );
        let script = verdict_script(&dir, "plans/Roadmap.md", "deny");
        let serial = test_serial();
        let _binary = bind_test_binary(&serial, script.to_string_lossy());

        let result = docs_rename_guarded(&repo, "docs/Roadmap.md", "plans/Roadmap.md");
        assert!(result.is_err(), "{result:?}");
        assert!(dir.path().join("docs/Roadmap.md").exists());
        assert!(!dir.path().join("plans").exists());
    }
}
