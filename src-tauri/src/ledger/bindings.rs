//! Which task a worktree is working on.
//!
//! A binding is not a setting stored in a config file — it is an *event*, and
//! it lives in the ledger with everything else. That choice buys two things a
//! settings file would not: the binding is timestamped, so a later audit can
//! say which task a commit was made under rather than which task the worktree
//! is bound to *now*; and it survives the same crash the rest of the history
//! survives.
//!
//! Resolution is "the most recent bind or unbind for this path wins". An
//! unbind is recorded rather than deleting the bind, for the same reason: the
//! work that happened while the binding held really did happen under it.

use super::{append, ActorKind, Draft, LedgerError, Outcome};

/// The action name a binding event carries.
pub const BIND: &str = "worktree.bind";
/// The action name an unbinding event carries.
pub const UNBIND: &str = "worktree.unbind";

/// The error code for a checkout path that no longer exists on disk.
pub const MISSING_WORKTREE: &str = "missing_worktree";

/// The authenticated repository-wide address behind one binding lookup.
///
/// The ledger and DevCouncil task store live at `anchor`; `worktree` remains
/// the checkout whose mutations the task constrains.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BindingAddress {
    pub anchor: String,
    pub worktree: String,
}

struct FamilyAddress {
    address: BindingAddress,
    members: Vec<std::path::PathBuf>,
}

impl From<crate::engine::worktree::WorktreeFamily> for FamilyAddress {
    fn from(family: crate::engine::worktree::WorktreeFamily) -> Self {
        FamilyAddress {
            address: BindingAddress {
                anchor: family.anchor.to_string_lossy().into_owned(),
                worktree: family.worktree.to_string_lossy().into_owned(),
            },
            members: family.members,
        }
    }
}

/// The error code for a family resolution that failed with `message`.
///
/// One classifier for both halves of resolution — the metadata admission and
/// the `git worktree list` authentication — so a failure reads the same
/// whichever half caught it.
fn classify(repo_path: &str, worktree_path: &str, message: String) -> LedgerError {
    // A checkout that is no longer on disk is neither untrusted nor
    // broken: it was removed or moved. Named first and on its own,
    // because "could not resolve this worktree's task binding" read as
    // a policy failure and was rendered as a hard harness block on a
    // worktree that had simply been deleted. `try_exists` answers
    // `Ok(false)` only for a definite absence; a path that could not
    // be examined (permissions) is left to the codes below.
    if [repo_path, worktree_path]
        .iter()
        .any(|path| std::path::Path::new(path).try_exists().ok() == Some(false))
    {
        return LedgerError::new(MISSING_WORKTREE, message);
    }
    // Trust first, and not merely for tidiness: resolution runs Git,
    // so an untrusted checkout fails before anything about the
    // worktree has been examined. Reading that refusal as one of the
    // verdicts below would report a repository we never looked at as
    // one we looked at and rejected.
    let code = if crate::repository_trust::refused(&message) {
        "untrusted_worktree"
    } else if message.contains("does not belong")
        || message.contains("not registered")
        || message.contains("different repository")
    {
        "unrelated_worktree"
    } else {
        "invalid_worktree"
    };
    LedgerError::new(code, message)
}

fn resolve_address(repo_path: &str, worktree_path: &str) -> Result<FamilyAddress, LedgerError> {
    crate::engine::worktree::resolve_worktree_family(repo_path, worktree_path)
        .map(FamilyAddress::from)
        .map_err(|message| classify(repo_path, worktree_path, message))
}

/// Admits the pair from metadata, or `Ok(None)` when no checkout of the
/// repository holds a ledger — so there is no binding to find, and nothing to
/// consolidate either.
///
/// That answer costs no Git: a binding is a ledger event, and the repository's
/// own metadata names every checkout a ledger could live in. Anything short
/// of that — a ledger somewhere, or a family the metadata cannot account for —
/// goes on to `git worktree list`, which alone authenticates membership. A
/// hand-written gitfile pointing into a repository with no ledger anywhere is
/// therefore answered "unbound" rather than "not registered": with no ledger,
/// no checkout of it is bound.
fn admit_with_ledger(
    repo_path: &str,
    worktree_path: &str,
) -> Result<Option<crate::engine::worktree::AdmittedPair>, LedgerError> {
    let pair = crate::engine::worktree::admit_worktree_pair(repo_path, worktree_path)
        .map_err(|message| classify(repo_path, worktree_path, message))?;
    let no_ledger = pair.metadata_checkouts().is_some_and(|checkouts| {
        !checkouts
            .iter()
            .any(|checkout| super::is_initialised(&checkout.to_string_lossy()))
    });
    Ok((!no_ledger).then_some(pair))
}

fn authenticate(
    repo_path: &str,
    worktree_path: &str,
    pair: crate::engine::worktree::AdmittedPair,
) -> Result<FamilyAddress, LedgerError> {
    pair.authenticate()
        .map(FamilyAddress::from)
        .map_err(|message| classify(repo_path, worktree_path, message))
}

fn address(repo_path: &str, worktree_path: &str) -> Result<BindingAddress, LedgerError> {
    let family = resolve_address(repo_path, worktree_path)?;
    super::consolidate_worktree_ledgers(&family.address.anchor, &family.members)?;
    Ok(family.address)
}

/// Repository-wide address for a checkout, used by the gate and ledger IPC so
/// writers and readers cannot choose different databases for sibling paths.
pub fn repository_address(repo_path: &str) -> Result<BindingAddress, LedgerError> {
    address(repo_path, repo_path)
}

/// Status is the one family entrypoint that can still return a useful object
/// when an old sibling ledger is unreadable. Reads and binding resolution fail
/// closed; status names the canonical destination and loudly marks it degraded.
pub(crate) fn repository_status(repo_path: &str) -> Result<super::LedgerStatus, LedgerError> {
    let family = resolve_address(repo_path, repo_path)?;
    match super::consolidate_worktree_ledgers(&family.address.anchor, &family.members) {
        Ok(_) => Ok(super::status(&family.address.anchor)),
        Err(error) => {
            let mut status = super::status(&family.address.anchor);
            status.recording = false;
            status.error = format!("legacy worktree ledger could not be consolidated: {error}");
            status.error_code = "legacy_migration_failed".to_string();
            Ok(status)
        }
    }
}

/// [`repository_status`] for a caller that must not create or migrate anything.
///
/// Two things are skipped relative to the creating path: the legacy
/// consolidation (which *imports rows* into the anchor database, and is a
/// migration, not a read), and the open-to-probe in [`super::status`]. Both
/// were reachable from tools annotated `readOnlyHint: true`.
pub(crate) fn repository_status_readonly(
    repo_path: &str,
) -> Result<super::LedgerStatus, LedgerError> {
    let family = resolve_address(repo_path, repo_path)?;
    let mut status = super::status_readonly(&family.address.anchor);
    // A repository whose siblings still carry pre-consolidation ledgers has
    // rows this read cannot see. Saying so beats silently under-reporting, and
    // beats running the migration from a read to make the number right.
    if status.recording
        && family.members.iter().any(|member| {
            member.as_path() != std::path::Path::new(&family.address.anchor)
                && super::is_initialised(&member.to_string_lossy())
        })
    {
        status.error =
            "a sibling worktree still holds its own ledger; open this repository in GitPulse to \
             consolidate. Events recorded there are not included here."
                .to_string();
        status.error_code = "legacy_ledger_unconsolidated".to_string();
    }
    Ok(status)
}

fn validate_task(anchor: &str, task_id: &str) -> Result<(), LedgerError> {
    if task_id.trim().is_empty() {
        return Err(LedgerError::new(
            "no_task",
            "a binding must name a task; use unbind() to clear one",
        ));
    }
    if task_id.contains('\0') || task_id.chars().any(char::is_control) {
        return Err(LedgerError::new(
            "invalid_task",
            "a binding task id cannot contain control characters",
        ));
    }
    if super::redact::text(task_id) != task_id {
        return Err(LedgerError::new(
            "sensitive_task",
            "a binding task id cannot contain credential-shaped data",
        ));
    }

    // Repositories without DevCouncil are allowed to carry a future/external
    // task label. Once its store exists, however, that store is authoritative:
    // accepting an unknown id would make the UI claim a scope the gate cannot
    // load, silently falling back to an unbound posture.
    if !crate::tasks::store_path(anchor).exists() {
        return Ok(());
    }
    match crate::tasks::scope(anchor, task_id) {
        Ok(Some(_)) => Ok(()),
        Ok(None) => Err(LedgerError::new(
            "unknown_task",
            format!("DevCouncil task '{task_id}' does not exist in this repository"),
        )),
        Err(error) => Err(LedgerError::new(
            "task_store_failed",
            format!("could not verify DevCouncil task '{task_id}': {error}"),
        )),
    }
}

/// Binds `worktree_path` to `task_id`, recording it as an event.
pub fn bind(repo_path: &str, worktree_path: &str, task_id: &str) -> Result<i64, LedgerError> {
    let address = address(repo_path, worktree_path)?;
    validate_task(&address.anchor, task_id)?;
    append(Draft {
        repo_path: address.anchor,
        worktree_path: Some(address.worktree.clone()),
        task_id: Some(task_id.to_string()),
        action: BIND.to_string(),
        object: Some(address.worktree),
        actor_kind: Some(ActorKind::Human),
        outcome: Some(Outcome::Ok),
        ..Default::default()
    })
}

/// Clears whatever `worktree_path` was bound to.
pub fn unbind(repo_path: &str, worktree_path: &str) -> Result<i64, LedgerError> {
    let address = address(repo_path, worktree_path)?;
    append(Draft {
        repo_path: address.anchor,
        worktree_path: Some(address.worktree.clone()),
        action: UNBIND.to_string(),
        object: Some(address.worktree),
        actor_kind: Some(ActorKind::Human),
        outcome: Some(Outcome::Ok),
        ..Default::default()
    })
}

/// The task `worktree_path` is bound to, if any.
///
/// Finds the newest bind/unbind naming this path with one indexed query.
/// Precedence uses the timestamp/ULID pair that survives a legacy
/// sibling-ledger import; the anchor's newly assigned integer cursor cannot
/// establish cross-database event order.
pub fn resolve(repo_path: &str, worktree_path: &str) -> Result<Option<String>, LedgerError> {
    Ok(resolve_binding(repo_path, worktree_path)?.map(|binding| binding.task_id))
}

/// A resolved binding together with the authenticated locations it connects.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ResolvedBinding {
    pub task_id: String,
    pub anchor: String,
    pub worktree: String,
}

pub(crate) fn resolve_binding(
    repo_path: &str,
    worktree_path: &str,
) -> Result<Option<ResolvedBinding>, LedgerError> {
    let Some(pair) = admit_with_ledger(repo_path, worktree_path)? else {
        return Ok(None);
    };
    let family = authenticate(repo_path, worktree_path, pair)?;
    super::consolidate_worktree_ledgers(&family.address.anchor, &family.members)?;
    binding_at(&family.address)
}

/// The binding at an address [`repository_address`] already resolved and
/// consolidated.
///
/// The app's gate resolves the family once per gated action and hands that one
/// answer to both the scope lookup and the ledger row it records, rather than
/// resolving it again for each.
pub(crate) fn binding_at(address: &BindingAddress) -> Result<Option<ResolvedBinding>, LedgerError> {
    // A binding *is* a ledger event, so a repository with no ledger has none —
    // and scanning for one would open the database, which creates it. That put
    // `.devcouncil/` into a fresh checkout on a `gitpulse_change_context` call,
    // a tool annotated `readOnlyHint: true`. `Ok(None)` here is the same answer
    // the scan would have reached, without the side effect.
    if !super::is_initialised(&address.anchor) {
        return Ok(None);
    }
    let stored_worktree = super::redact::text(&address.worktree);
    // Imported rows receive new integer cursors in the anchor database. ULIDs
    // retain their original timestamp and are the ledger's cross-database sort
    // key, so binding precedence uses them rather than import order.
    let newest = super::latest_binding(&address.anchor, &address.worktree, &stored_worktree)?;
    Ok(newest.flatten().map(|task_id| ResolvedBinding {
        task_id,
        anchor: address.anchor.clone(),
        worktree: address.worktree.clone(),
    }))
}

/// [`resolve_binding`] for the agent hook, which must not write.
///
/// The hook is a fresh process on every Bash call and every file edit an
/// agent makes, so whatever this costs is paid per tool call. It answers the
/// same question with the same fail-closed outcomes — missing, untrusted,
/// unrelated or invalid checkouts, a foreign row, an unreadable ledger — but:
///
/// * a repository with no ledger in any checkout costs no Git at all;
/// * the ledger is read through a read-only connection: no directory or file
///   chmod, no WAL or synchronous pragma, no schema DDL, no cached handle;
/// * legacy sibling ledgers are never migrated from here. If one holds rows
///   the anchor has not imported, the binding cannot be ruled out, so the
///   lookup refuses (`legacy_ledger_unconsolidated`) until GitPulse — which
///   consolidates whenever it resolves the repository — has run there.
pub(crate) fn resolve_binding_readonly(
    repo_path: &str,
    worktree_path: &str,
) -> Result<Option<ResolvedBinding>, LedgerError> {
    let Some(pair) = admit_with_ledger(repo_path, worktree_path)? else {
        return Ok(None);
    };
    let family = authenticate(repo_path, worktree_path, pair)?;
    if let Some(source) =
        super::unconsolidated_legacy_ledger(&family.address.anchor, &family.members)?
    {
        return Err(LedgerError::new(
            "legacy_ledger_unconsolidated",
            format!(
                "the worktree ledger at {} holds history not yet consolidated into this \
                 repository's ledger, so its task binding cannot be ruled out; open this \
                 repository in GitPulse to consolidate it",
                source.display()
            ),
        ));
    }
    let address = family.address;
    if !super::is_initialised(&address.anchor) {
        return Ok(None);
    }
    let stored_worktree = super::redact::text(&address.worktree);
    let newest =
        super::latest_binding_readonly(&address.anchor, &address.worktree, &stored_worktree)?;
    Ok(newest.flatten().map(|task_id| ResolvedBinding {
        task_id,
        anchor: address.anchor,
        worktree: address.worktree,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    use crate::ledger::tail;
    use crate::test_support::git_in;

    fn git_repo() -> tempfile::TempDir {
        let dir = tempfile::tempdir().expect("temp repository");
        git_in(dir.path(), &["init", "-b", "main"]);
        std::fs::write(dir.path().join("seed.txt"), "seed").expect("seed repository");
        git_in(dir.path(), &["add", "seed.txt"]);
        git_in(dir.path(), &["commit", "-m", "seed"]);
        dir
    }

    fn linked_worktree_named(repo: &std::path::Path, name: &str) -> (tempfile::TempDir, String) {
        let parent = tempfile::tempdir().expect("worktree parent");
        let path = parent.path().join(name);
        git_in(
            repo,
            &[
                "worktree",
                "add",
                "--detach",
                path.to_str().expect("utf8 worktree path"),
            ],
        );
        crate::test_support::trust_repo(&path);
        let canonical = path
            .canonicalize()
            .expect("canonical worktree")
            .to_string_lossy()
            .into_owned();
        (parent, canonical)
    }

    fn linked_worktree(repo: &std::path::Path) -> (tempfile::TempDir, String) {
        linked_worktree_named(repo, "task-worktree")
    }

    fn repo() -> (tempfile::TempDir, String) {
        let dir = git_repo();
        let path = dir.path().to_str().unwrap().to_string();
        (dir, path)
    }

    /// A checkout nobody let us open is not a broken checkout.
    ///
    /// Resolution runs Git, so an untrusted repository fails at admission —
    /// before a single fact about the worktree has been established. The
    /// classifier used to fall through to `invalid_worktree`, and the app's
    /// diagnostics duly told the user that six valid worktrees were invalid
    /// when the only thing wrong was that they had not been trusted yet.
    #[test]
    fn an_untrusted_checkout_is_not_reported_as_an_invalid_one() {
        let (dir, path) = repo();
        crate::repository_trust::revoke(&path).expect("revoke fixture trust");

        let error = repository_address(&path).expect_err("an untrusted checkout must not resolve");
        assert_eq!(
            error.code, "untrusted_worktree",
            "a refusal to look was reported as a verdict: {error}"
        );
        assert!(
            crate::repository_trust::refused(&error.message),
            "and it must still carry the refusal the user has to act on: {error}"
        );

        // The sibling arms still mean what they did: a directory that is not a
        // repository at all is `invalid_worktree` whether or not trust is in
        // play, so the new arm cannot be swallowing them.
        let plain = dir.path().join("not-a-repository");
        std::fs::create_dir(&plain).expect("plain directory");
        let invalid = repository_address(plain.to_str().expect("utf8 path"))
            .expect_err("a non-repository must not resolve");
        assert_eq!(invalid.code, "invalid_worktree", "{invalid}");

        // And a path with nothing on disk is the absence it is.
        let gone = dir.path().join("deleted-worktree");
        let missing = repository_address(gone.to_str().expect("utf8 path"))
            .expect_err("a missing path must not resolve");
        assert_eq!(missing.code, MISSING_WORKTREE, "{missing}");
        assert!(!gone.exists(), "resolving it must not create it");
    }

    #[test]
    fn zz_characterize_a_read_creates_the_state_directory() {
        let (dir, repo) = repo();
        let state = dir.path().join(".devcouncil");
        println!(
            "CHARACTERIZE before read: .devcouncil exists = {}",
            state.exists()
        );
        let status = repository_status(&repo).expect("status");
        println!(
            "CHARACTERIZE after repository_status: .devcouncil exists = {}, recording = {}",
            state.exists(),
            status.recording
        );
        let mut created: Vec<String> = std::fs::read_dir(&state)
            .map(|entries| {
                entries
                    .map(|e| e.expect("entry").file_name().to_string_lossy().into_owned())
                    .collect()
            })
            .unwrap_or_default();
        created.sort();
        println!("CHARACTERIZE files written into the repository: {created:?}");
    }

    #[test]
    fn an_unbound_worktree_resolves_to_nothing() {
        let (_d, repo) = repo();
        assert_eq!(resolve(&repo, &repo).unwrap(), None);
    }

    #[test]
    fn a_binding_resolves_to_its_task() {
        let (_d, repo) = repo();
        bind(&repo, &repo, "TASK-1").unwrap();
        assert_eq!(resolve(&repo, &repo).unwrap(), Some("TASK-1".into()));
    }

    #[test]
    fn the_newest_binding_wins() {
        let (_d, repo) = repo();
        bind(&repo, &repo, "TASK-1").unwrap();
        bind(&repo, &repo, "TASK-2").unwrap();
        assert_eq!(resolve(&repo, &repo).unwrap(), Some("TASK-2".into()));
    }

    #[test]
    fn unbinding_clears_it_without_erasing_the_history() {
        let (_d, repo) = repo();
        bind(&repo, &repo, "TASK-1").unwrap();
        unbind(&repo, &repo).unwrap();
        assert_eq!(resolve(&repo, &repo).unwrap(), None);

        // The bind is still on the record: work done while it held was done
        // under that task, and an unbind does not retract that.
        let events = tail(&repo, 0, 100).unwrap();
        assert_eq!(events.len(), 2);
        assert_eq!(events[0].action, BIND);
        assert_eq!(events[0].task_id.as_deref(), Some("TASK-1"));
    }

    #[test]
    fn bindings_are_per_worktree() {
        let (main, repo) = repo();
        let (_parent, worktree) = linked_worktree(main.path());
        bind(&repo, &repo, "TASK-1").unwrap();
        bind(&repo, &worktree, "TASK-2").unwrap();
        unbind(&repo, &repo).unwrap();
        assert_eq!(resolve(&repo, &repo).unwrap(), None);
        assert_eq!(resolve(&repo, &worktree).unwrap(), Some("TASK-2".into()));
    }

    #[test]
    fn a_binding_must_name_a_task() {
        let (_d, repo) = repo();
        assert_eq!(bind(&repo, &repo, "").unwrap_err().code, "no_task");
    }

    #[test]
    fn resolution_survives_a_restart() {
        let (_d, repo) = repo();
        bind(&repo, &repo, "TASK-1").unwrap();
        super::super::tests_support::reset_registry();
        assert_eq!(resolve(&repo, &repo).unwrap(), Some("TASK-1".into()));
    }

    /// Resolution runs on every gated mutation and once per worktree per Work
    /// refresh. It paged the whole ledger, 1000 rows per query, holding the
    /// app-wide ledger lock for each page; it is one query now.
    #[test]
    fn a_binding_lookup_is_one_ledger_query_however_long_the_history() {
        let (_d, repo) = repo();
        bind(&repo, &repo, "TASK-1").unwrap();
        for i in 0..2500 {
            append(Draft {
                repo_path: repo.clone(),
                action: format!("git.commit{i}"),
                actor_kind: Some(ActorKind::Human),
                outcome: Some(Outcome::Ok),
                ..Default::default()
            })
            .unwrap();
        }
        let reads = || super::super::tests::LEDGER_READS.with(std::cell::Cell::get);
        let before = reads();
        assert_eq!(resolve(&repo, &repo).unwrap(), Some("TASK-1".into()));
        assert_eq!(reads() - before, 1, "one query, not one per 1000 rows");
    }

    /// The query must stay on its partial index: without it, every lookup is
    /// a scan of the whole events table under the ledger lock.
    #[test]
    fn the_binding_query_uses_its_index() {
        let (_d, repo) = repo();
        bind(&repo, &repo, "TASK-1").unwrap();
        let plan = super::super::with_conn(&repo, |conn| {
            let mut stmt = conn
                .prepare(
                    "EXPLAIN QUERY PLAN SELECT action, task_id FROM events
                     WHERE action IN ('worktree.bind', 'worktree.unbind')
                       AND worktree_path IN (?1, ?2)
                     ORDER BY ts_utc DESC, ulid DESC
                     LIMIT 1",
                )
                .unwrap();
            let rows = stmt
                .query_map(rusqlite::params!["a", "b"], |row| row.get::<_, String>(3))
                .unwrap();
            Ok(rows.map(Result::unwrap).collect::<Vec<_>>().join(" | "))
        })
        .unwrap();
        assert!(plan.contains("idx_events_binding"), "{plan}");
        assert_eq!(
            (BIND, UNBIND),
            ("worktree.bind", "worktree.unbind"),
            "the query and the index spell these as literals"
        );
    }

    /// A row naming another repository refused every binding read when the
    /// lookup paged through `tail`; the single query keeps that refusal.
    #[test]
    fn a_foreign_row_still_refuses_binding_resolution() {
        let (_d, repo) = repo();
        bind(&repo, &repo, "TASK-1").unwrap();
        super::super::with_conn(&repo, |conn| {
            conn.execute(
                "UPDATE events SET repo_path = '/somewhere/else' WHERE id = (SELECT MIN(id) FROM events)",
                [],
            )
            .unwrap();
            Ok(())
        })
        .unwrap();
        let error = resolve(&repo, &repo).expect_err("a foreign row refuses the read");
        assert_eq!(error.code, "repository_mismatch");
    }

    /// The foreign-row probe must seek the repository index. The `NOT IN` it
    /// replaced scanned the whole index on every gated mutation.
    #[test]
    fn the_foreign_row_probe_seeks_its_index() {
        let (_d, repo) = repo();
        bind(&repo, &repo, "TASK-1").unwrap();
        let plan = super::super::with_conn(&repo, |conn| {
            let mut stmt = conn
                .prepare(&format!(
                    "EXPLAIN QUERY PLAN {}",
                    super::super::FOREIGN_ROW_SQL
                ))
                .unwrap();
            let rows = stmt
                .query_map(rusqlite::params!["a", "b"], |row| row.get::<_, String>(3))
                .unwrap();
            Ok(rows.map(Result::unwrap).collect::<Vec<_>>().join(" | "))
        })
        .unwrap();
        assert!(plan.contains("SEARCH"), "{plan}");
        assert!(!plan.contains("SCAN"), "{plan}");
    }

    /// The probes cover both sides of the two spellings and the gap between
    /// them, not only paths that sort below the repository.
    #[test]
    fn a_foreign_row_sorting_after_the_repository_still_refuses() {
        let (_d, repo) = repo();
        bind(&repo, &repo, "TASK-1").unwrap();
        super::super::with_conn(&repo, |conn| {
            conn.execute(
                "UPDATE events SET repo_path = repo_path || '~elsewhere'
                 WHERE id = (SELECT MIN(id) FROM events)",
                [],
            )
            .unwrap();
            Ok(())
        })
        .unwrap();
        let error = resolve(&repo, &repo).expect_err("a foreign row refuses the read");
        assert_eq!(error.code, "repository_mismatch");
    }

    #[test]
    fn resolution_pages_past_the_first_window() {
        // The newest binding is at the END of an append-only log, so a scan
        // that stopped after one page would return a stale task forever.
        let (_d, repo) = repo();
        bind(&repo, &repo, "TASK-1").unwrap();
        for i in 0..1200 {
            append(Draft {
                repo_path: repo.clone(),
                action: format!("git.commit{i}"),
                actor_kind: Some(ActorKind::Human),
                outcome: Some(Outcome::Ok),
                ..Default::default()
            })
            .unwrap();
        }
        bind(&repo, &repo, "TASK-2").unwrap();
        assert_eq!(resolve(&repo, &repo).unwrap(), Some("TASK-2".into()));
    }

    /// A caller must not be able to write a task binding for a path belonging
    /// to a different repository. That would make a UI selection in repo A
    /// manufacture policy context for repo B.
    #[test]
    fn binding_refuses_a_worktree_from_an_unrelated_repository() {
        let repo = git_repo();
        let unrelated = git_repo();
        let error = bind(
            repo.path().to_str().expect("utf8 repository"),
            unrelated.path().to_str().expect("utf8 repository"),
            "TASK-1",
        )
        .expect_err("an unrelated repository is not this repository's worktree");
        assert_eq!(error.code, "unrelated_worktree");
    }

    #[test]
    fn unbinding_refuses_a_worktree_from_an_unrelated_repository() {
        let repo = git_repo();
        let unrelated = git_repo();
        let error = unbind(
            repo.path().to_str().expect("utf8 repository"),
            unrelated.path().to_str().expect("utf8 repository"),
        )
        .expect_err("an unrelated repository is not this repository's worktree");
        assert_eq!(error.code, "unrelated_worktree");
    }

    #[test]
    fn resolving_refuses_a_worktree_from_an_unrelated_repository() {
        let repo = git_repo();
        let unrelated = git_repo();
        let error = resolve(
            repo.path().to_str().expect("utf8 repository"),
            unrelated.path().to_str().expect("utf8 repository"),
        )
        .expect_err("an unrelated repository is not this repository's worktree");
        assert_eq!(error.code, "unrelated_worktree");
    }

    #[test]
    fn binding_refuses_an_unregistered_directory_that_points_at_the_same_git_dir() {
        let repo = git_repo();
        let impostor = tempfile::tempdir().expect("impostor directory");
        std::fs::write(
            impostor.path().join(".git"),
            format!("gitdir: {}\n", repo.path().join(".git").display()),
        )
        .expect("impostor gitfile");
        crate::test_support::trust_repo(impostor.path());

        let error = bind(
            repo.path().to_str().expect("utf8 repository"),
            impostor.path().to_str().expect("utf8 impostor"),
            "TASK-1",
        )
        .expect_err("a shared git-dir pointer is not a registered worktree");
        assert_eq!(error.code, "unrelated_worktree");
    }

    /// Once a DevCouncil store exists, a free-form id is not a harmless label:
    /// it selects the scope the mutation gate will enforce. Unknown ids must
    /// therefore be rejected at the bind boundary.
    #[test]
    fn binding_refuses_an_unknown_task_when_the_store_exists() {
        let repo = git_repo();
        let repo_path = repo.path().to_str().expect("utf8 repository");
        let db = crate::tasks::store_path(repo_path);
        std::fs::create_dir_all(db.parent().expect("store parent")).expect("store directory");
        let _store = dc_store::Store::open(&db).expect("task store");

        let error = bind(repo_path, repo_path, "TASK-DOES-NOT-EXIST")
            .expect_err("an existing store must authenticate the task id");
        assert_eq!(error.code, "unknown_task");
    }

    #[test]
    fn binding_refuses_a_task_id_that_redaction_would_change() {
        let repo = git_repo();
        let repo_path = repo.path().to_str().expect("utf8 repository");
        let error = bind(repo_path, repo_path, "password=opaque-secret")
            .expect_err("a stored task id must remain the id the caller selected");
        assert_eq!(error.code, "sensitive_task");
        assert_eq!(resolve(repo_path, repo_path).expect("binding lookup"), None);
    }

    #[test]
    fn a_binding_resolves_when_the_worktree_path_itself_is_redacted_on_disk() {
        let repo = git_repo();
        let (_worktree_parent, worktree) =
            linked_worktree_named(repo.path(), "password=opaque-secret");
        let repo_path = repo.path().to_str().expect("utf8 repository");
        assert_ne!(super::super::redact::text(&worktree), worktree);

        bind(repo_path, &worktree, "TASK-REDACTED-PATH").expect("bind redacted path");
        assert_eq!(
            resolve(&worktree, &worktree).expect("resolve redacted path"),
            Some("TASK-REDACTED-PATH".to_string())
        );
    }

    /// WorktreesPanel binds from the repository tab while mutations are later
    /// guarded from the linked worktree's own tab. Both viewpoints must resolve
    /// the same event, or the UI shows a binding that the gate never consults.
    #[test]
    fn a_linked_worktree_resolves_a_binding_recorded_from_the_main_checkout() {
        let repo = git_repo();
        let (_worktree_parent, worktree) = linked_worktree(repo.path());
        let repo_path = repo.path().to_str().expect("utf8 repository");

        bind(repo_path, &worktree, "TASK-1").expect("bind linked worktree");

        assert_eq!(
            resolve(&worktree, &worktree).expect("resolve from target worktree"),
            Some("TASK-1".into())
        );
    }

    /// Before the family ledger existed, opening a linked checkout wrote its
    /// rows into `<linked>/.devcouncil/ledger.sqlite`. Switching readers to the
    /// primary checkout must not silently strand that already-durable history,
    /// and repeated family resolution must not duplicate the imported row.
    #[test]
    fn legacy_linked_ledger_rows_are_imported_once_into_the_family_anchor() {
        let repo = git_repo();
        let (_worktree_parent, worktree) = linked_worktree(repo.path());
        let repo_path = repo.path().to_str().expect("utf8 repository");

        append(Draft {
            repo_path: worktree.clone(),
            worktree_path: Some(worktree.clone()),
            action: "git.commit".to_string(),
            object: Some("legacy linked action".to_string()),
            actor_kind: Some(ActorKind::Human),
            outcome: Some(Outcome::Ok),
            ..Default::default()
        })
        .expect("legacy linked ledger row");

        let first = repository_address(repo_path).expect("first family resolution");
        let first_rows = tail(&first.anchor, 0, 100).expect("family ledger after import");
        assert_eq!(first_rows.len(), 1, "the legacy row must remain visible");
        assert_eq!(first_rows[0].repo_path, first.anchor);
        assert_eq!(
            first_rows[0].worktree_path.as_deref(),
            Some(worktree.as_str())
        );

        append(Draft {
            repo_path: worktree.clone(),
            worktree_path: Some(worktree.clone()),
            action: "git.push".to_string(),
            object: Some("later legacy action".to_string()),
            actor_kind: Some(ActorKind::Human),
            outcome: Some(Outcome::Ok),
            ..Default::default()
        })
        .expect("later legacy linked ledger row");

        let second = repository_address(repo_path).expect("incremental family resolution");
        let second_rows = tail(&second.anchor, 0, 100).expect("family ledger after update");
        assert_eq!(
            second_rows.len(),
            2,
            "rows appended after the first migration must not be missed"
        );
        assert_eq!(second_rows[0].ulid, first_rows[0].ulid);

        let third = repository_address(repo_path).expect("repeated family resolution");
        let third_rows = tail(&third.anchor, 0, 100).expect("family ledger after retry");
        assert_eq!(third_rows.len(), 2, "ULID-based import must be idempotent");
        assert_eq!(third_rows, second_rows);
    }

    #[test]
    fn a_legacy_linked_binding_is_enforced_after_family_consolidation() {
        let repo = git_repo();
        let (_worktree_parent, worktree) = linked_worktree(repo.path());
        append(Draft {
            repo_path: worktree.clone(),
            worktree_path: Some(worktree.clone()),
            task_id: Some("TASK-LEGACY".to_string()),
            action: BIND.to_string(),
            object: Some(worktree.clone()),
            actor_kind: Some(ActorKind::Human),
            outcome: Some(Outcome::Ok),
            ..Default::default()
        })
        .expect("legacy binding row");

        assert_eq!(
            resolve(&worktree, &worktree).expect("resolve consolidated binding"),
            Some("TASK-LEGACY".to_string())
        );
    }

    #[test]
    fn equal_local_cursors_from_two_legacy_siblings_preserve_both_rows() {
        let repo = git_repo();
        let (_first_parent, first) = linked_worktree(repo.path());
        let (_second_parent, second) = linked_worktree(repo.path());
        for (worktree, action) in [(&first, "git.commit"), (&second, "git.push")] {
            append(Draft {
                repo_path: worktree.clone(),
                worktree_path: Some(worktree.clone()),
                action: action.to_string(),
                actor_kind: Some(ActorKind::Human),
                outcome: Some(Outcome::Ok),
                ..Default::default()
            })
            .expect("legacy sibling row");
            assert_eq!(
                tail(worktree, 0, 10).expect("source ledger")[0].id,
                1,
                "each old database owns an independent integer cursor"
            );
        }

        let repo_path = repo.path().to_str().expect("utf8 repository");
        let address = repository_address(repo_path).expect("family consolidation");
        let rows = tail(&address.anchor, 0, 10).expect("family ledger");
        assert_eq!(rows.len(), 2, "neither source-local id may be dropped");
        assert_ne!(rows[0].ulid, rows[1].ulid);
        assert_eq!(
            rows.iter()
                .filter_map(|event| event.worktree_path.as_deref())
                .collect::<std::collections::HashSet<_>>(),
            std::collections::HashSet::from([first.as_str(), second.as_str()])
        );
    }

    #[test]
    fn redaction_colliding_source_labels_keep_distinct_migration_markers() {
        let repo = git_repo();
        let worktree_parent = tempfile::tempdir().expect("worktree parent");
        let create = |name: &str| {
            let path = worktree_parent.path().join(name);
            git_in(
                repo.path(),
                &[
                    "worktree",
                    "add",
                    "--detach",
                    path.to_str().expect("utf8 worktree"),
                ],
            );
            path.canonicalize()
                .expect("canonical worktree")
                .to_string_lossy()
                .into_owned()
        };
        let first = create("password=alpha");
        let second = create("password=beta");
        assert_eq!(
            super::super::redact::text(&first),
            super::super::redact::text(&second),
            "the fixture must exercise a redacted-label collision"
        );
        for (worktree, action) in [(&first, "git.commit"), (&second, "git.push")] {
            append(Draft {
                repo_path: worktree.clone(),
                worktree_path: Some(worktree.clone()),
                action: action.to_string(),
                actor_kind: Some(ActorKind::Human),
                outcome: Some(Outcome::Ok),
                ..Default::default()
            })
            .expect("legacy sibling row");
        }

        let repo_path = repo.path().to_str().expect("utf8 repository");
        let address = repository_address(repo_path).expect("family consolidation");
        let rows = tail(&address.anchor, 0, 10).expect("family ledger");
        assert_eq!(
            rows.len(),
            2,
            "opaque source keys must not collapse redaction-equivalent paths"
        );
        super::super::with_conn(&address.anchor, |conn| {
            let marker_count: i64 = conn
                .query_row("SELECT COUNT(*) FROM ledger_family_imports", [], |row| {
                    row.get(0)
                })
                .map_err(|error| LedgerError::new("fixture_failed", error.to_string()))?;
            assert_eq!(marker_count, 2);
            Ok(())
        })
        .expect("migration markers");
    }

    #[test]
    fn invalid_legacy_attribution_is_bounded_to_its_authenticated_source() {
        let repo = git_repo();
        let unrelated = git_repo();
        let (_worktree_parent, worktree) = linked_worktree(repo.path());
        let unrelated_path = unrelated
            .path()
            .canonicalize()
            .expect("canonical unrelated")
            .to_string_lossy()
            .into_owned();
        append(Draft {
            repo_path: worktree.clone(),
            worktree_path: Some(unrelated_path.clone()),
            action: "git.commit".to_string(),
            actor_kind: Some(ActorKind::Human),
            outcome: Some(Outcome::Ok),
            ..Default::default()
        })
        .expect("legacy ordinary row");
        append(Draft {
            repo_path: worktree.clone(),
            worktree_path: Some(unrelated_path),
            task_id: Some("TASK-UNRELATED".to_string()),
            action: BIND.to_string(),
            actor_kind: Some(ActorKind::Human),
            outcome: Some(Outcome::Ok),
            ..Default::default()
        })
        .expect("legacy invalid binding");

        let repo_path = repo.path().to_str().expect("utf8 repository");
        let address = repository_address(repo_path).expect("family consolidation");
        let rows = tail(&address.anchor, 0, 10).expect("family rows");
        let ordinary = rows
            .iter()
            .find(|event| event.action == "git.commit")
            .expect("ordinary row");
        assert_eq!(
            ordinary.worktree_path.as_deref(),
            Some(super::super::redact::text(&worktree).as_str())
        );
        let binding = rows
            .iter()
            .find(|event| event.action == BIND)
            .expect("binding row retained as history");
        assert_eq!(binding.worktree_path, None, "invalid authority stays inert");
    }

    #[test]
    fn importing_an_older_legacy_bind_does_not_override_a_newer_unbind() {
        let repo = git_repo();
        let (_worktree_parent, worktree) = linked_worktree(repo.path());
        let repo_path = repo.path().to_str().expect("utf8 repository");
        append(Draft {
            repo_path: worktree.clone(),
            worktree_path: Some(worktree.clone()),
            task_id: Some("TASK-OLD".to_string()),
            action: BIND.to_string(),
            actor_kind: Some(ActorKind::Human),
            outcome: Some(Outcome::Ok),
            ..Default::default()
        })
        .expect("older legacy bind");
        append(Draft {
            repo_path: repo_path.to_string(),
            worktree_path: Some(worktree.clone()),
            action: UNBIND.to_string(),
            actor_kind: Some(ActorKind::Human),
            outcome: Some(Outcome::Ok),
            ..Default::default()
        })
        .expect("newer family unbind");

        assert_eq!(
            resolve(repo_path, &worktree).expect("resolve after consolidation"),
            None
        );
    }

    #[test]
    fn an_unreadable_legacy_ledger_is_loud_and_blocks_binding_reads() {
        let repo = git_repo();
        let (_worktree_parent, worktree) = linked_worktree(repo.path());
        let state = std::path::Path::new(&worktree).join(".devcouncil");
        std::fs::create_dir_all(&state).expect("legacy state directory");
        std::fs::write(state.join("ledger.sqlite"), b"not a sqlite database")
            .expect("corrupt legacy ledger");
        let repo_path = repo.path().to_str().expect("utf8 repository");

        let status = repository_status(repo_path).expect("family status");
        assert!(!status.recording);
        assert_eq!(status.error_code, "legacy_migration_failed");
        assert!(status.error.contains("legacy worktree ledger"));

        let error = repository_address(repo_path)
            .expect_err("history and binding reads must fail instead of omitting legacy state");
        assert_eq!(error.code, "legacy_read_failed");
    }

    #[test]
    fn a_legacy_ulid_collision_with_different_payload_is_not_silently_deduplicated() {
        let repo = git_repo();
        let (_worktree_parent, worktree) = linked_worktree(repo.path());
        let repo_path = repo.path().to_str().expect("utf8 repository");
        append(Draft {
            repo_path: worktree.clone(),
            worktree_path: Some(worktree.clone()),
            action: "git.commit".to_string(),
            actor_kind: Some(ActorKind::Human),
            outcome: Some(Outcome::Ok),
            ..Default::default()
        })
        .expect("legacy row");
        let legacy = tail(&worktree, 0, 10).expect("legacy source").remove(0);
        super::super::with_conn(repo_path, |conn| {
            conn.execute(
                "INSERT INTO events (
                     ulid, ts_utc, schema_version, repo_path, worktree_path,
                     actor_kind, action, outcome
                 ) VALUES (?1, ?2, 1, ?3, ?4, 'human', 'git.push', 'ok')",
                rusqlite::params![legacy.ulid, legacy.ts_utc, repo_path, worktree],
            )
            .map_err(|error| LedgerError::new("fixture_failed", error.to_string()))?;
            Ok(())
        })
        .expect("conflicting anchor row");

        let error = repository_address(repo_path)
            .expect_err("different events sharing an identity must stop migration");
        assert_eq!(error.code, "legacy_ulid_conflict");
        let status = repository_status(repo_path).expect("degraded family status");
        assert!(!status.recording);
        assert_eq!(status.error_code, "legacy_migration_failed");
        assert!(status.error.contains("legacy_ulid_conflict"));
    }
}

/// The agent hook's binding lookup ([`resolve_binding_readonly`]): what it
/// costs, what it must not touch, and that it still fails closed.
#[cfg(test)]
mod readonly_tests {
    use super::*;
    use crate::engine::git_cli::spawn_log;
    use crate::test_support::git_in;

    fn fresh_repo() -> tempfile::TempDir {
        let dir = tempfile::tempdir().unwrap();
        git_in(dir.path(), &["init", "-b", "main"]);
        std::fs::write(dir.path().join("seed.txt"), "seed").unwrap();
        git_in(dir.path(), &["add", "seed.txt"]);
        git_in(dir.path(), &["commit", "-m", "seed"]);
        dir
    }

    fn canonical(dir: &std::path::Path) -> String {
        dir.canonicalize()
            .expect("canonical path")
            .to_string_lossy()
            .into_owned()
    }

    fn linked(repo: &std::path::Path) -> (tempfile::TempDir, String) {
        let parent = tempfile::tempdir().expect("worktree parent");
        let path = parent.path().join("task-worktree");
        git_in(
            repo,
            &["worktree", "add", "--detach", path.to_str().expect("utf8")],
        );
        crate::test_support::trust_repo(&path);
        (parent, canonical(&path))
    }

    /// Git children spawned in `cwd` by `f`, by argument vector.
    fn spawned_by<T>(cwd: &str, f: impl FnOnce() -> T) -> (T, Vec<Vec<String>>) {
        let cwd = std::path::Path::new(cwd);
        let before = spawn_log::spawns_in(cwd).len();
        let value = f();
        let spawned = spawn_log::spawns_in(cwd).into_iter().skip(before).collect();
        (value, spawned)
    }

    fn record(repo: &str) {
        crate::ledger::append(crate::ledger::Draft {
            repo_path: repo.to_string(),
            action: "git.commit".into(),
            actor_kind: Some(crate::ledger::ActorKind::Human),
            outcome: Some(crate::ledger::Outcome::Ok),
            ..Default::default()
        })
        .expect("ledger row");
    }

    /// An uninitialised ledger has no binding: answering that must cost no
    /// Git, from the main checkout or a linked worktree, and create nothing.
    /// Both lookups take the shortcut — the app's too, since with no ledger
    /// there is nothing for it to consolidate either.
    #[test]
    fn an_uninitialised_ledger_costs_zero_git_spawns() {
        let dir = fresh_repo();
        let repo = canonical(dir.path());
        let (_parent, worktree) = linked(dir.path());
        assert!(!super::super::is_initialised(&repo));

        for path in [&repo, &worktree] {
            let (readonly, spawned) = spawned_by(path, || resolve_binding_readonly(path, path));
            assert_eq!(readonly.expect("hook lookup"), None);
            assert!(
                spawned.is_empty(),
                "the hook lookup spawned git: {spawned:?}"
            );

            let (app, spawned) = spawned_by(path, || resolve_binding(path, path));
            assert_eq!(app.expect("app lookup"), None);
            assert!(
                spawned.is_empty(),
                "the app lookup spawned git: {spawned:?}"
            );
        }
        assert!(
            !super::super::is_initialised(&repo) && !super::super::is_initialised(&worktree),
            "a lookup created a ledger"
        );
        assert!(!dir.path().join(".devcouncil").exists());
    }

    /// The shortcut is a necessary condition, never a verdict on its own: a
    /// ledger anywhere in the family — here only in a linked sibling — sends
    /// the lookup on to Git.
    #[test]
    fn a_ledger_in_any_checkout_is_not_shortcut() {
        let dir = fresh_repo();
        let repo = canonical(dir.path());
        let (_parent, worktree) = linked(dir.path());
        record(&worktree);

        let (_, spawned) = spawned_by(&repo, || resolve_binding_readonly(&repo, &repo));
        assert!(
            spawned
                .iter()
                .any(|argv| argv.iter().any(|arg| arg == "worktree")),
            "a sibling's ledger must not be answered from metadata: {spawned:?}"
        );
    }

    /// A fresh hook process (no cached connection) reads the binding without
    /// any of the creating path's writes: no read-write open, no chmod, no
    /// WAL pragma, no schema DDL. Each has an observable proxy, set up before
    /// the lookup so the creating path would visibly undo it.
    #[cfg(unix)]
    #[test]
    fn the_readonly_lookup_does_not_chmod_pragma_or_ddl() {
        use std::os::unix::fs::PermissionsExt;
        let dir = fresh_repo();
        let repo = canonical(dir.path());
        bind(&repo, &repo, "TASK-RO").expect("bind");
        crate::ledger::tests_support::reset_registry();

        let db = crate::ledger::ledger_path(&repo);
        {
            let conn = rusqlite::Connection::open(&db).expect("fixture connection");
            let mode: String = conn
                .query_row("PRAGMA journal_mode = DELETE", [], |row| row.get(0))
                .expect("leave WAL");
            assert_eq!(mode, "delete");
            conn.execute_batch("DROP INDEX idx_events_task")
                .expect("drop an index the schema DDL recreates");
        }
        std::fs::set_permissions(&db, std::fs::Permissions::from_mode(0o644)).unwrap();
        let opens = crate::ledger::tests_support::write_opens();

        let resolved = resolve_binding_readonly(&repo, &repo).expect("hook lookup");
        assert_eq!(resolved.map(|b| b.task_id).as_deref(), Some("TASK-RO"));

        assert_eq!(
            crate::ledger::tests_support::write_opens(),
            opens,
            "the hook lookup opened the ledger read-write"
        );
        let mode = std::fs::metadata(&db).unwrap().permissions().mode() & 0o777;
        assert_eq!(
            mode, 0o644,
            "the hook lookup chmodded the ledger to {mode:o}"
        );
        let conn =
            rusqlite::Connection::open_with_flags(&db, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY)
                .unwrap();
        let journal: String = conn
            .query_row("PRAGMA journal_mode", [], |row| row.get(0))
            .unwrap();
        assert_eq!(
            journal, "delete",
            "the hook lookup switched the journal to WAL"
        );
        let index: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM sqlite_master WHERE name = 'idx_events_task'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(index, 0, "the hook lookup ran the schema DDL");
    }

    /// A ledger file whose creator died before its schema holds no binding;
    /// the read-only reader cannot add the schema, and must not fail closed
    /// on every command for want of it.
    #[test]
    fn a_ledger_without_a_schema_is_unbound() {
        let dir = fresh_repo();
        let repo = canonical(dir.path());
        let db = crate::ledger::ledger_path(&repo);
        std::fs::create_dir_all(db.parent().unwrap()).unwrap();
        std::fs::File::create(&db).unwrap();
        assert_eq!(resolve_binding_readonly(&repo, &repo).unwrap(), None);
    }

    /// Legacy sibling history is never migrated from the hook. Until GitPulse
    /// has consolidated it the binding cannot be ruled out, so the lookup
    /// refuses; afterwards it reads the consolidated binding. Sources persist
    /// after import, so "consolidated" is the anchor's marker, not absence.
    #[test]
    fn legacy_sibling_history_fails_closed_until_consolidated() {
        let dir = fresh_repo();
        let repo = canonical(dir.path());
        let (_parent, worktree) = linked(dir.path());
        append(Draft {
            repo_path: worktree.clone(),
            worktree_path: Some(worktree.clone()),
            task_id: Some("TASK-LEGACY".to_string()),
            action: BIND.to_string(),
            object: Some(worktree.clone()),
            actor_kind: Some(ActorKind::Human),
            outcome: Some(Outcome::Ok),
            ..Default::default()
        })
        .expect("legacy binding row");

        let error = resolve_binding_readonly(&worktree, &worktree)
            .expect_err("unimported legacy history must not read as unbound");
        assert_eq!(error.code, "legacy_ledger_unconsolidated", "{error}");
        assert!(
            !super::super::is_initialised(&repo),
            "the hook lookup consolidated (created the anchor ledger)"
        );

        repository_address(&repo).expect("GitPulse consolidates");
        let resolved = resolve_binding_readonly(&worktree, &worktree).expect("consolidated");
        assert_eq!(resolved.map(|b| b.task_id).as_deref(), Some("TASK-LEGACY"));

        // A later legacy row is unimported history again.
        record(&worktree);
        let error = resolve_binding_readonly(&worktree, &worktree)
            .expect_err("a new unimported row refuses again");
        assert_eq!(error.code, "legacy_ledger_unconsolidated", "{error}");
    }

    /// The one outcome the zero-spawn shortcut changes, pinned so a later
    /// tightening meets a test rather than a surprise: a directory claiming a
    /// repository's git dir without being registered, where *no* checkout has
    /// a ledger, is answered unbound — membership is never checked, because
    /// with no ledger no checkout of that repository can be bound. Once a
    /// ledger exists the same directory is refused (see below).
    #[test]
    fn an_unregistered_gitfile_into_a_ledgerless_repository_is_unbound() {
        let other = fresh_repo();
        let other_repo = canonical(other.path());
        let forged_parent = tempfile::tempdir().unwrap();
        let forged = forged_parent.path().join("forged");
        std::fs::create_dir(&forged).unwrap();
        std::fs::write(forged.join(".git"), format!("gitdir: {other_repo}/.git\n")).unwrap();
        crate::test_support::trust_repo(&forged);
        let forged = canonical(&forged);
        assert_eq!(
            resolve_binding_readonly(&other_repo, &forged).expect("no ledger, no binding"),
            None
        );
        assert!(!super::super::is_initialised(&other_repo));
    }

    /// Fail-closed outcomes survive the read-only path, each under the code
    /// the gate sorts it by: Untrusted, Missing, and Broken (a foreign row,
    /// an unregistered gitfile into a repository that has a ledger).
    #[test]
    fn the_readonly_lookup_keeps_every_fail_closed_outcome() {
        // Untrusted, with and without a ledger.
        let dir = fresh_repo();
        let repo = canonical(dir.path());
        crate::repository_trust::revoke(&repo).expect("revoke");
        let error = resolve_binding_readonly(&repo, &repo).expect_err("untrusted");
        assert_eq!(error.code, "untrusted_worktree", "{error}");
        crate::test_support::trust_repo(dir.path());
        bind(&repo, &repo, "TASK-1").expect("bind");
        crate::repository_trust::revoke(&repo).expect("revoke");
        let error = resolve_binding_readonly(&repo, &repo).expect_err("untrusted");
        assert_eq!(error.code, "untrusted_worktree", "{error}");
        crate::test_support::trust_repo(dir.path());

        // Missing.
        let (_parent, worktree) = linked(dir.path());
        std::fs::remove_dir_all(&worktree).expect("delete worktree");
        let error = resolve_binding_readonly(&worktree, &worktree).expect_err("missing");
        assert_eq!(error.code, MISSING_WORKTREE, "{error}");
        assert!(!std::path::Path::new(&worktree).exists());

        // Broken: a row attributed to another repository.
        super::super::with_conn(&repo, |conn| {
            conn.execute(
                "UPDATE events SET repo_path = '/somewhere/else' WHERE id = (SELECT MIN(id) FROM events)",
                [],
            )
            .unwrap();
            Ok(())
        })
        .unwrap();
        let error = resolve_binding_readonly(&repo, &repo).expect_err("foreign row");
        assert_eq!(error.code, "repository_mismatch", "{error}");

        // Broken: a directory claiming the repository's git dir without being
        // a registered worktree, once a ledger exists to be read.
        let other = fresh_repo();
        let other_repo = canonical(other.path());
        record(&other_repo);
        let forged_parent = tempfile::tempdir().unwrap();
        let forged = forged_parent.path().join("forged");
        std::fs::create_dir(&forged).unwrap();
        std::fs::write(
            forged.join(".git"),
            format!("gitdir: {}/.git\n", other_repo),
        )
        .unwrap();
        crate::test_support::trust_repo(&forged);
        let forged = canonical(&forged);
        let error =
            resolve_binding_readonly(&other_repo, &forged).expect_err("unregistered checkout");
        assert_eq!(error.code, "unrelated_worktree", "{error}");
    }
}
