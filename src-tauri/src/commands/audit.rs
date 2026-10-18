//! Ledger, task binding and catch-up commands.

use super::*;

// --- the action ledger -------------------------------------------------

/// Reads durable ledger events after `cursor`, oldest first. With `newest`,
/// the newest `limit` of them, so a reader that keeps only recent rows takes
/// one request rather than paging the whole history.
///
/// The frontend's action list is a projection of this, not a store of its own:
/// it holds the last cursor it saw and asks for what followed. That is what
/// makes the history survive a reload, a crash, and a restart — and what makes
/// "everything visible is a projection of the log" true rather than aspirational.
#[tauri::command(async)]
pub async fn cmd_ledger_tail(
    repo_path: String,
    cursor: i64,
    limit: u32,
    newest: Option<bool>,
) -> Result<Vec<crate::ledger::LedgerEvent>, IpcError> {
    off_thread(move || {
        let repo = validate_repo(&repo_path)?;
        let address = crate::ledger::bindings::repository_address(&repo.to_string_lossy())
            .map_err(|e| e.to_string())?;
        let read = if newest.unwrap_or(false) {
            crate::ledger::tail_newest
        } else {
            crate::ledger::tail
        };
        read(&address.anchor, cursor, limit).map_err(|e| e.to_string())
    })
    .await
}

/// Reports whether the ledger is recording for this repository.
///
/// Separate from the events themselves on purpose. A repository whose ledger
/// cannot be opened returns an empty history, and an empty history is exactly
/// what a repository with nothing in it returns; without this, the two would be
/// the same picture.
#[tauri::command(async)]
pub async fn cmd_ledger_status(repo_path: String) -> Result<crate::ledger::LedgerStatus, IpcError> {
    off_thread(move || {
        let repo = validate_repo(&repo_path)?;
        crate::ledger::bindings::repository_status(&repo.to_string_lossy())
            .map_err(|e| e.to_string())
    })
    .await
}

// --- task binding ------------------------------------------------------

/// DevCouncil tasks currently leased in this repository.
///
/// Read-only. GitPulse never acquires, renews or releases a lease: those
/// contend with an active agent's writer lease, and a UI process that takes one
/// strands the task when the window closes.
#[tauri::command(async)]
pub async fn cmd_task_view(repo_path: String) -> Result<crate::tasks::TaskView, IpcError> {
    off_thread(move || {
        let repo = validate_repo(&repo_path)?;
        let address = crate::ledger::bindings::repository_address(&repo.to_string_lossy())
            .map_err(|e| e.to_string())?;
        Ok(crate::tasks::view(&address.anchor))
    })
    .await
}

/// The scopes several tasks declare. A task the store does not hold, or a
/// repository with no store, is simply absent from the list.
///
/// One call for the whole set, because finding the store authenticates the
/// repository with a `git worktree list`: a per-task command started one git
/// process for every task title the Work view showed, on every refresh.
#[tauri::command(async)]
pub async fn cmd_task_scopes(
    repo_path: String,
    task_ids: Vec<String>,
) -> Result<Vec<crate::tasks::TaskScope>, IpcError> {
    off_thread(move || task_scopes(&repo_path, &task_ids)).await
}

pub(super) fn task_scopes(
    repo_path: &str,
    task_ids: &[String],
) -> Result<Vec<crate::tasks::TaskScope>, String> {
    let repo = validate_repo(repo_path)?;
    if task_ids.is_empty() {
        return Ok(Vec::new());
    }
    let address = crate::ledger::bindings::repository_address(&repo.to_string_lossy())
        .map_err(|e| e.to_string())?;
    crate::tasks::scopes(&address.anchor, task_ids)
}

/// The catch-up a bind or unbind runs first: the worktree's own, inside the
/// ordinary catch-up budget.
fn catch_up_before_binding(
    address: &crate::ledger::bindings::BindingAddress,
) -> crate::ingest::CatchUp {
    crate::ingest::catch_up_into_bounded(
        &address.anchor,
        &address.worktree,
        Some(crate::ingest::CATCH_UP_DEADLINE),
    )
}

/// Runs `catch_up` for the address a binding change is about to be written at,
/// and returns what it found as the `detail_json` of that row:
/// `{"catch_up": {"ok", "truncated", "recorded", "error"}}`, plus a `note` when
/// there was nothing to read.
///
/// The catch-up is best-effort. Whatever it does, the binding is written, and
/// one that did not finish is counted
/// ([`crate::ledger::record_catch_up_failure`], surfaced as
/// `LedgerStatus::catch_up_failures`, apart from dropped appends) as well as
/// recorded on the row, so a failure never reads as a clean bind. No home
/// directory to find transcripts in is "nothing to read": `ok` stays true, it
/// is not counted, and the row keeps the reason as a `note`.
fn catch_up_note(
    address: &crate::ledger::bindings::BindingAddress,
    catch_up: impl FnOnce(&crate::ledger::bindings::BindingAddress) -> crate::ingest::CatchUp,
) -> Option<String> {
    let found = catch_up(address);
    let nothing_to_read = found.error == crate::ingest::NO_TRANSCRIPT_ROOT;
    let ok = (found.error.is_empty() || nothing_to_read) && !found.truncated;
    if !ok {
        crate::ledger::record_catch_up_failure(&address.anchor);
    }
    let mut note = serde_json::json!({
        "ok": ok,
        "truncated": found.truncated,
        "recorded": found.recorded,
        "error": if nothing_to_read {
            String::new()
        } else {
            found.error.chars().take(500).collect::<String>()
        },
    });
    if nothing_to_read {
        note["note"] = found.error.into();
    }
    Some(serde_json::json!({ "catch_up": note }).to_string())
}

/// [`crate::ledger::bindings::bind_with`] after `catch_up`.
pub(super) fn bind_after_catch_up(
    repo_path: &str,
    worktree_path: &str,
    task_id: &str,
    catch_up: impl FnOnce(&crate::ledger::bindings::BindingAddress) -> crate::ingest::CatchUp,
) -> Result<i64, String> {
    crate::ledger::bindings::bind_with(repo_path, worktree_path, task_id, |address| {
        catch_up_note(address, catch_up)
    })
    .map_err(|e| e.to_string())
}

/// [`crate::ledger::bindings::unbind_with`] after `catch_up`.
pub(super) fn unbind_after_catch_up(
    repo_path: &str,
    worktree_path: &str,
    catch_up: impl FnOnce(&crate::ledger::bindings::BindingAddress) -> crate::ingest::CatchUp,
) -> Result<i64, String> {
    crate::ledger::bindings::unbind_with(repo_path, worktree_path, |address| {
        catch_up_note(address, catch_up)
    })
    .map_err(|e| e.to_string())
}

/// Binds a worktree to a task, so every later mutation in it is judged against
/// that task's plan.
///
/// Recorded as a ledger event rather than a setting, so a later audit can say
/// which task a commit was made under — not merely which task the worktree is
/// bound to now. Work an agent did while GitPulse was closed is caught up
/// first, so the new binding's row is appended after it and the rows that
/// catch-up writes are stamped with the task that held when they happened. A
/// catch-up that fails or runs out of time does not stop the bind: the result
/// is recorded on the row (`detail_json.catch_up`) and counted.
#[tauri::command(async)]
pub async fn cmd_bind_worktree_task(
    repo_path: String,
    worktree_path: String,
    task_id: String,
) -> Result<i64, IpcError> {
    off_thread(move || {
        bind_after_catch_up(
            &repo_path,
            &worktree_path,
            &task_id,
            catch_up_before_binding,
        )
    })
    .await
}

/// Clears a worktree's task binding. The bind stays on the record. Catches up
/// first, exactly as [`cmd_bind_worktree_task`] does.
#[tauri::command(async)]
pub async fn cmd_unbind_worktree_task(
    repo_path: String,
    worktree_path: String,
) -> Result<i64, IpcError> {
    off_thread(move || unbind_after_catch_up(&repo_path, &worktree_path, catch_up_before_binding))
        .await
}

/// The task a worktree is bound to, or `null`.
#[tauri::command(async)]
pub async fn cmd_worktree_task(
    repo_path: String,
    worktree_path: String,
) -> Result<Option<String>, IpcError> {
    off_thread(move || {
        crate::ledger::bindings::resolve(&repo_path, &worktree_path).map_err(|e| e.to_string())
    })
    .await
}

// --- catching up ------------------------------------------------------

/// Replays what happened while GitPulse was closed.
///
/// Two sources, both observed rather than self-reported: git's reflog, which is
/// authoritative for ref movements and survives GitPulse being uninstalled, and
/// agent transcripts, which attribute file edits and commands to a session.
/// Both replays are idempotent, so this is safe to run on every repo open. The
/// call is bounded by the catch-up budget however a phase stalls, and repeated
/// calls inside a minute share one pass.
#[tauri::command(async)]
pub async fn cmd_catch_up(repo_path: String) -> Result<crate::ingest::CatchUp, IpcError> {
    off_thread(move || {
        let repo = validate_repo(&repo_path)?;
        // Resolving the repository's family happens inside the pass's budget,
        // and a pass that finished moments ago answers for this one: the app
        // asks on every repository change.
        Ok(crate::ingest::catch_up_coalesced(&repo.to_string_lossy()))
    })
    .await
}

#[cfg(test)]
mod catch_up_binding_tests {
    use super::*;

    use crate::ingest::CatchUp;
    use crate::ledger::{self, bindings};
    use crate::test_support::git_in;

    fn git_repo() -> (tempfile::TempDir, String) {
        let dir = tempfile::tempdir().expect("temp repository");
        let path = dir.path().canonicalize().expect("canonical path");
        git_in(&path, &["init", "-b", "main"]);
        std::fs::write(path.join("seed.txt"), "seed").expect("seed repository");
        git_in(&path, &["add", "seed.txt"]);
        git_in(&path, &["commit", "-m", "seed"]);
        let text = path.to_string_lossy().into_owned();
        (dir, text)
    }

    fn detail(event: &ledger::LedgerEvent) -> serde_json::Value {
        serde_json::from_str(event.detail_json.as_deref().expect("a detail_json")).unwrap()
    }

    fn marker(repo: &str) -> i64 {
        ledger::append(ledger::Draft {
            repo_path: repo.to_string(),
            action: "session.edit".into(),
            object: Some("caught-up".into()),
            ..Default::default()
        })
        .unwrap()
    }

    #[test]
    fn a_bind_catches_up_before_its_row_is_written() {
        let (_dir, repo) = git_repo();
        let mut caught_up = 0;
        let bound = bind_after_catch_up(&repo, &repo, "T-1", |address| {
            caught_up = marker(&address.anchor);
            CatchUp {
                recorded: 1,
                ..CatchUp::default()
            }
        })
        .unwrap();
        assert!(
            caught_up > 0 && bound > caught_up,
            "the bind ({bound}) must come after what the catch-up recorded ({caught_up})"
        );
        let row = ledger::tail(&repo, 0, 100)
            .unwrap()
            .into_iter()
            .find(|event| event.id == bound)
            .unwrap();
        let note = &detail(&row)["catch_up"];
        assert_eq!(note["ok"], true, "{note}");
        assert_eq!(note["recorded"], 1, "{note}");
        assert_eq!(ledger::status(&repo).catch_up_failures, 0);
    }

    #[test]
    fn a_failed_catch_up_still_binds_and_is_recorded_and_counted() {
        let (_dir, repo) = git_repo();
        let failed = bind_after_catch_up(&repo, &repo, "T-1", |_| CatchUp {
            error: "the reflog could not be read".into(),
            ..CatchUp::default()
        });
        let bound = failed.expect("the bind goes ahead whatever the catch-up did");
        assert_eq!(bindings::resolve(&repo, &repo).unwrap(), Some("T-1".into()));
        let row = ledger::tail(&repo, 0, 100)
            .unwrap()
            .into_iter()
            .find(|event| event.id == bound)
            .unwrap();
        let note = &detail(&row)["catch_up"];
        assert_eq!(note["ok"], false, "{note}");
        assert_eq!(note["error"], "the reflog could not be read", "{note}");
        assert_eq!(
            ledger::status(&repo).catch_up_failures,
            1,
            "the failure is counted"
        );

        // A pass the deadline cut short is a failure too, however empty its error.
        unbind_after_catch_up(&repo, &repo, |_| CatchUp {
            truncated: true,
            ..CatchUp::default()
        })
        .unwrap();
        assert_eq!(ledger::status(&repo).catch_up_failures, 2);
        assert_eq!(bindings::resolve(&repo, &repo).unwrap(), None);
    }

    #[test]
    fn an_unbind_catches_up_what_happened_under_the_binding_it_ends() {
        let (_dir, repo) = git_repo();
        bindings::bind(&repo, &repo, "T-1").unwrap();
        std::thread::sleep(std::time::Duration::from_millis(20));
        let corpus = tempfile::tempdir().unwrap();
        let slug = corpus.path().join("slug");
        std::fs::create_dir_all(&slug).unwrap();
        let file = format!("{repo}/a.rs");
        let line = serde_json::json!({
            "type": "assistant",
            "sessionId": "AU",
            "timestamp": ledger::ids::iso8601_utc(ledger::ids::now_millis()),
            "version": "2.1.241",
            "cwd": repo,
            "message": { "content": [
                { "type": "tool_use", "name": "Edit", "input": { "file_path": file } }
            ]}
        })
        .to_string();
        std::fs::write(slug.join("AU.jsonl"), line + "\n").unwrap();

        let serial = crate::test_support::env::env_serial();
        let _env = crate::test_support::env::bind_env(&serial)
            .set("GITPULSE_TRANSCRIPT_ROOT", corpus.path());
        std::thread::sleep(std::time::Duration::from_millis(20));
        let unbound = unbind_after_catch_up(&repo, &repo, catch_up_before_binding).unwrap();

        let events = ledger::tail(&repo, 0, 100).unwrap();
        let edit = events
            .iter()
            .find(|event| event.object.as_deref() == Some(file.as_str()))
            .expect("the unbind caught the call up first");
        assert!(edit.id < unbound, "the call was recorded before the unbind");
        assert_eq!(edit.task_id.as_deref(), Some("T-1"));
        let unbind_row = events.iter().find(|event| event.id == unbound).unwrap();
        assert!(detail(unbind_row)["catch_up"]["recorded"].as_i64().unwrap() >= 1);
    }

    /// Concurrent binds of one worktree: every row lands, the live read agrees
    /// with the newest `(ts_utc, ulid)`, and each failed catch-up is counted
    /// exactly once.
    #[test]
    fn concurrent_binds_all_land_and_each_failed_catch_up_is_counted_once() {
        let (_dir, repo) = git_repo();
        let ids: Vec<i64> = std::thread::scope(|scope| {
            let threads: Vec<_> = (0..8)
                .map(|n| {
                    let repo = &repo;
                    scope.spawn(move || {
                        bind_after_catch_up(repo, repo, &format!("T-{n}"), |_| CatchUp {
                            truncated: n % 2 == 0,
                            ..CatchUp::default()
                        })
                        .unwrap()
                    })
                })
                .collect();
            threads.into_iter().map(|t| t.join().unwrap()).collect()
        });
        assert_eq!(ids.len(), 8);
        let rows = ledger::tail(&repo, 0, 100).unwrap();
        let mut binds: Vec<_> = rows
            .iter()
            .filter(|event| event.action == bindings::BIND)
            .collect();
        assert_eq!(binds.len(), 8);
        binds.sort_by(|a, b| (&a.ts_utc, &a.ulid).cmp(&(&b.ts_utc, &b.ulid)));
        let newest = binds.last().unwrap().task_id.clone();
        assert_eq!(bindings::resolve(&repo, &repo).unwrap(), newest);
        assert_eq!(ledger::status(&repo).catch_up_failures, 4);
    }

    /// A catch-up that fails for a reason that is not about the ledger (no home
    /// directory to look for transcripts in) makes `LedgerStatus::dropped` say
    /// an append was dropped. Nothing was: the bind row landed and the ledger is
    /// complete. The counter is the only thing a person sees, and it is
    /// per-process and never cleared.
    #[test]
    fn a_catch_up_that_had_nothing_to_read_does_not_read_as_a_dropped_append() {
        let (_dir, repo) = git_repo();
        let serial = crate::test_support::env::env_serial();
        let _env = crate::test_support::env::bind_env(&serial)
            .remove("HOME")
            .remove("GITPULSE_TRANSCRIPT_ROOT");
        let before = ledger::status(&repo);
        let bound = bind_after_catch_up(&repo, &repo, "T-1", catch_up_before_binding).unwrap();
        let row = ledger::tail(&repo, 0, 100)
            .unwrap()
            .into_iter()
            .find(|event| event.id == bound)
            .unwrap();
        let note = &detail(&row)["catch_up"];
        let after = ledger::status(&repo);
        assert_eq!(
            after.dropped, before.dropped,
            "a bind whose catch-up merely found no transcript root is counted as a dropped append"
        );
        assert_eq!(after.catch_up_failures, before.catch_up_failures, "{note}");
        // Visible on the row, not a failure.
        assert_eq!(note["ok"], true, "{note}");
        assert_eq!(note["note"], crate::ingest::NO_TRANSCRIPT_ROOT, "{note}");

        // A real failure is counted in its own counter, and never as a drop.
        bind_after_catch_up(&repo, &repo, "T-2", |_| crate::ingest::CatchUp {
            error: "the reflog could not be read".into(),
            ..crate::ingest::CatchUp::default()
        })
        .unwrap();
        let failed = ledger::status(&repo);
        assert_eq!(failed.catch_up_failures, before.catch_up_failures + 1);
        assert_eq!(failed.dropped, before.dropped);
    }
}
