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

use super::{append, tail, ActorKind, Draft, LedgerError, Outcome};

/// The action name a binding event carries.
pub const BIND: &str = "worktree.bind";
/// The action name an unbinding event carries.
pub const UNBIND: &str = "worktree.unbind";

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

fn resolve_address(repo_path: &str, worktree_path: &str) -> Result<FamilyAddress, LedgerError> {
    crate::engine::worktree::resolve_worktree_family(repo_path, worktree_path)
        .map(|family| FamilyAddress {
            address: BindingAddress {
                anchor: family.anchor.to_string_lossy().into_owned(),
                worktree: family.worktree.to_string_lossy().into_owned(),
            },
            members: family.members,
        })
        .map_err(|message| {
            let code = if message.contains("does not belong")
                || message.contains("not registered")
                || message.contains("different repository")
            {
                "unrelated_worktree"
            } else {
                "invalid_worktree"
            };
            LedgerError::new(code, message)
        })
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
/// Scans the ledger for the newest bind/unbind naming this path. Pages are
/// bounded, while precedence uses the timestamp/ULID pair that survives a
/// legacy sibling-ledger import; the anchor's newly assigned integer cursor
/// cannot establish cross-database event order.
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
    let address = address(repo_path, worktree_path)?;
    let stored_worktree = super::redact::text(&address.worktree);
    let mut cursor = 0i64;
    // Imported rows receive new integer cursors in the anchor database. ULIDs
    // retain their original timestamp and are the ledger's cross-database sort
    // key, so binding precedence must use them rather than import order.
    let mut current: Option<((String, String), Option<String>)> = None;
    loop {
        let page = tail(&address.anchor, cursor, 1000)?;
        if page.is_empty() {
            break;
        }
        for event in &page {
            if !matches!(
                event.worktree_path.as_deref(),
                Some(path) if path == address.worktree || path == stored_worktree
            ) {
                continue;
            }
            let task_id = match event.action.as_str() {
                BIND => event.task_id.clone(),
                UNBIND => None,
                _ => continue,
            };
            let key = (event.ts_utc.clone(), event.ulid.clone());
            if current
                .as_ref()
                .is_none_or(|(current_key, _)| key > *current_key)
            {
                current = Some((key, task_id));
            }
        }
        cursor = page[page.len() - 1].id;
        if page.len() < 1000 {
            break;
        }
    }
    Ok(current.and_then(|(_, task_id)| {
        task_id.map(|task_id| ResolvedBinding {
            task_id,
            anchor: address.anchor,
            worktree: address.worktree,
        })
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn git_in(dir: &std::path::Path, args: &[&str]) {
        let output = std::process::Command::new("git")
            .args([
                "-c",
                "user.name=GitPulse",
                "-c",
                "user.email=gitpulse@test.local",
                "-c",
                "commit.gpgsign=false",
            ])
            .args(args)
            .current_dir(dir)
            .output()
            .expect("spawn git");
        assert!(
            output.status.success(),
            "git {args:?} failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
    }

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
