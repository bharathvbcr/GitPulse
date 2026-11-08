//! Worktree lifecycle commands: list, add, remove, lock, merge teardown, routes, summaries.

use super::*;
use crate::engine::worktree::MergeTeardownError;
use crate::ipc_error::CommandError;

// ---------------------------------------------------------------------------
// Linked worktrees: how agents parallelize, so they are first-class here.
// ---------------------------------------------------------------------------

/// `depth` is what the caller reads, and absent means `listing`: one
/// `git worktree list`, every count "not scanned". Each level past it costs
/// git children per worktree — `dirty` one `git status` each, `full` adds a
/// diff of every changed worktree, divergence from main and routes — so a
/// caller asks for what it renders. The default used to be `full`, and a
/// caller that read only heads or dirty counts paid for all of it: 18
/// worktrees were 37 children per listing through an 8-a-second spawn budget.
#[tauri::command(async)]
pub async fn cmd_list_worktrees(
    repo_path: String,
    depth: Option<crate::engine::worktree::ScanDepth>,
) -> Result<Vec<WorktreeInfo>, IpcError> {
    // Past `listing` it spawns git per worktree (capped), so it belongs on
    // the blocking pool like every other reader.
    let depth = depth.unwrap_or(crate::engine::worktree::ScanDepth::Listing);
    off_thread(move || crate::engine::worktree::list_worktrees_scanned(&repo_path, depth)).await
}

/// Creates a linked worktree, after the command gate has judged the exact
/// `git worktree add` line this would run.
#[tauri::command(async)]
#[allow(clippy::too_many_arguments)]
pub async fn cmd_add_worktree(
    repo_path: String,
    target_path: String,
    new_branch: Option<String>,
    start_point: Option<String>,
    detach: bool,
    cow_caches: Option<bool>,
    run_hooks: Option<bool>,
    zero_copy_devmap: Option<bool>,
    seed_devmap: Option<bool>,
    sparse_paths: Option<Vec<String>>,
) -> Result<Guarded<crate::engine::worktree::WorktreeCreated>, IpcError> {
    off_thread(move || {
        add_worktree_guarded(
            &repo_path,
            &target_path,
            new_branch.as_deref(),
            start_point.as_deref(),
            detach,
            cow_caches.unwrap_or(false),
            run_hooks.unwrap_or(false),
            devmap_mode(zero_copy_devmap, seed_devmap),
            sparse_paths.as_deref(),
            &|repo, argv| guard(repo, argv),
        )
    })
    .await
}

/// The DevMap state a new worktree is asked for. The zero-copy pointer, when
/// asked for, wins; otherwise `seed_devmap` asks for a CoW-cloned store; a
/// caller that names neither gets none, as before either field existed.
fn devmap_mode(
    zero_copy_devmap: Option<bool>,
    seed_devmap: Option<bool>,
) -> crate::engine::worktree::DevmapMode {
    use crate::engine::worktree::DevmapMode;
    if zero_copy_devmap.unwrap_or(false) {
        DevmapMode::ZeroCopy
    } else if seed_devmap.unwrap_or(false) {
        DevmapMode::Copy
    } else {
        DevmapMode::None
    }
}

#[allow(clippy::too_many_arguments)]
pub(crate) fn add_worktree_guarded(
    repo_path: &str,
    target_path: &str,
    new_branch: Option<&str>,
    start_point: Option<&str>,
    detach: bool,
    cow_caches: bool,
    run_hooks: bool,
    devmap: crate::engine::worktree::DevmapMode,
    sparse_paths: Option<&[String]>,
    gate: WorktreeGate<'_>,
) -> Result<Guarded<crate::engine::worktree::WorktreeCreated>, CommandError> {
    // GitPulse's task-worktree container has one creator, the task
    // provisioner, which keeps it out of `git status` first.
    crate::engine::worktree::refuse_gitpulse_lane_target(target_path)?;
    let has_sparse = sparse_paths
        .unwrap_or(&[])
        .iter()
        .any(|s| !s.trim().is_empty());
    // The plain add is judged here, as one line. A sparse add is a sequence
    // (add, sparse-checkout, checkout, and an undo on failure), so the engine
    // hands each git write to the same gate as it runs; the verdict reported is
    // the first one, the add's.
    let mut policy = None;
    if !has_sparse {
        let argv_owned = crate::engine::worktree::add_worktree_argv_configured(
            target_path,
            new_branch,
            start_point,
            detach,
            false,
        );
        let refs: Vec<&str> = argv_owned.iter().map(String::as_str).collect();
        policy = Some(gate(repo_path, &refs)?);
    }
    let mut judge = |args: &[&str]| -> Result<(), CommandError> {
        let argv: Vec<&str> = std::iter::once("git").chain(args.iter().copied()).collect();
        let verdict = gate(repo_path, &argv)?;
        policy.get_or_insert(verdict);
        Ok(())
    };
    let created = crate::engine::worktree::add_worktree_configured(
        repo_path,
        target_path,
        new_branch,
        start_point,
        detach,
        cow_caches,
        // Off unless asked for; and even then only hooks a person
        // approved for this exact file run.
        run_hooks,
        devmap,
        sparse_paths,
        &mut judge,
    )?;
    // A seeded store is the main checkout's generation; bring it to this
    // worktree's own tree (the pointer leaves no local store to build).
    if matches!(
        created.devmap,
        crate::engine::worktree::DevmapOutcome::Provisioned { zero_copy: false }
    ) {
        crate::devmap::live::refresh_seeded_worktree(
            &created.path,
            crate::devmap::live::SeededRefresh::Background,
        );
    }
    Ok(Guarded {
        policy: policy.ok_or_else(|| {
            CommandError::plain("The worktree was created without being judged by the policy gate.")
        })?,
        output: created,
    })
}

/// The repository's `.gitpulse/hooks` file as the approval dialog shows it:
/// its exact text, what it would run, and whether that exact text is approved.
/// `None` when the repository has no such file. Reads only.
#[tauri::command(async)]
pub async fn cmd_preview_worktree_hooks(
    repo_path: String,
) -> Result<Option<crate::engine::worktree_hooks::WorktreeHooksPreview>, IpcError> {
    off_thread(move || {
        let repo = crate::engine::git_cli::validate_repo(&repo_path)?;
        crate::engine::preview_worktree_hooks(&repo)
    })
    .await
}

/// Records consent to run the hooks file whose text is `content` — the text
/// the person was shown. Refused if the file on disk is no longer exactly that.
#[tauri::command(async)]
pub async fn cmd_approve_worktree_hooks(
    repo_path: String,
    content: String,
) -> Result<(), IpcError> {
    off_thread(move || {
        let repo = crate::engine::git_cli::validate_repo(&repo_path)?;
        crate::engine::approve_worktree_hooks(&repo, &content)
    })
    .await
}

/// Removes a linked worktree, gated like every other destructive write.
#[tauri::command(async)]
pub async fn cmd_remove_worktree(
    repo_path: String,
    target_path: String,
    force: bool,
    workbench: State<'_, crate::workbench::WorkbenchState>,
) -> Result<Guarded<crate::engine::worktree::WorktreeRemoval>, IpcError> {
    let workbench = workbench.inner().clone();
    off_thread(move || {
        remove_worktree_guarded(
            &workbench,
            &repo_path,
            &target_path,
            force,
            &|repo, argv| guard(repo, argv),
        )
    })
    .await
}

/// The command gate as a worktree teardown is handed it: the repository and
/// the full argv (program first). The command supplies [`guard`]; a test
/// supplies the harness gate without the user-action mark.
pub(crate) type WorktreeGate<'a> =
    &'a dyn Fn(&str, &[&str]) -> Result<crate::harness::PolicyVerdict, String>;

/// Body of [`cmd_remove_worktree`].
pub(crate) fn remove_worktree_guarded(
    workbench: &crate::workbench::WorkbenchState,
    repo_path: &str,
    target_path: &str,
    force: bool,
    gate: WorktreeGate<'_>,
) -> Result<Guarded<crate::engine::worktree::WorktreeRemoval>, CommandError> {
    // A task run's agent, a dock tab's agent or any process may be working in
    // it; removal would pull its files out from under it.
    workbench.refuse_if_in_use(target_path)?;
    let argv_owned = crate::engine::worktree::remove_worktree_argv(target_path, force);
    let refs: Vec<&str> = argv_owned.iter().map(String::as_str).collect();
    let policy = gate(repo_path, &refs)?;
    let output = crate::engine::worktree::remove_worktree(repo_path, target_path, force)?;
    Ok(Guarded { policy, output })
}

#[tauri::command(async)]
pub async fn cmd_lock_worktree(
    repo_path: String,
    target_path: String,
    reason: Option<String>,
) -> Result<Guarded<()>, IpcError> {
    off_thread(move || {
        let argv_owned =
            crate::engine::worktree::lock_worktree_argv(&target_path, reason.as_deref());
        let refs: Vec<&str> = argv_owned.iter().map(String::as_str).collect();
        let policy = guard(&repo_path, &refs)?;
        crate::engine::worktree::lock_worktree(&repo_path, &target_path, reason.as_deref())?;
        Ok(Guarded { policy, output: () })
    })
    .await
}

#[tauri::command(async)]
pub async fn cmd_unlock_worktree(
    repo_path: String,
    target_path: String,
) -> Result<Guarded<()>, IpcError> {
    off_thread(move || {
        let argv_owned = crate::engine::worktree::unlock_worktree_argv(&target_path);
        let refs: Vec<&str> = argv_owned.iter().map(String::as_str).collect();
        let policy = guard(&repo_path, &refs)?;
        crate::engine::worktree::unlock_worktree(&repo_path, &target_path)?;
        Ok(Guarded { policy, output: () })
    })
    .await
}

#[tauri::command(async)]
pub async fn cmd_prune_worktree(repo_path: String) -> Result<Guarded<()>, IpcError> {
    off_thread(move || {
        let argv_owned = crate::engine::worktree::prune_worktree_argv();
        let refs: Vec<&str> = argv_owned.iter().map(String::as_str).collect();
        let policy = guard(&repo_path, &refs)?;
        crate::engine::worktree::prune_worktree(&repo_path)?;
        Ok(Guarded { policy, output: () })
    })
    .await
}

/// Merges a worktree branch into target_branch (defaulting to main), tears down
/// the worktree cleanly, and prunes the merged branch.
///
/// `expected_tip` is the branch commit the caller verified: when it is given
/// and the branch is anywhere else once the merge holds its lock, nothing is
/// merged. The error is the typed [`MergeTeardownError`] (`{code, message,
/// ...}`), so a caller can tell a conflict from a refusal.
///
/// `require_write_tree`: the wave sets this. Absent or false keeps the
/// in-checkout fallback when this git cannot write a merge tree. When the
/// wave sets it, `task_id` is the card whose candidate is verified before
/// the target ref moves. The panel omits both.
#[tauri::command(async)]
#[allow(clippy::too_many_arguments)]
pub async fn cmd_worktree_merge_teardown(
    repo_path: String,
    worktree_path: String,
    target_branch: Option<String>,
    squash: Option<bool>,
    expected_tip: Option<String>,
    require_write_tree: Option<bool>,
    task_id: Option<String>,
    workbench: State<'_, crate::workbench::WorkbenchState>,
) -> Result<Guarded<crate::engine::worktree::MergeTeardownResult>, MergeTeardownError> {
    let workbench = workbench.inner().clone();
    // The pool carries `String` errors; the typed one rides inside the `Ok`.
    off_thread(move || {
        Ok(merge_teardown_guarded(
            &workbench,
            &repo_path,
            &worktree_path,
            target_branch.as_deref(),
            squash.unwrap_or(false),
            expected_tip.as_deref(),
            require_write_tree.unwrap_or(false),
            task_id.as_deref(),
            &|repo, argv| guard(repo, argv),
        ))
    })
    .await
    .map_err(|e| e.message)?
}

/// Body of [`cmd_worktree_merge_teardown`].
#[allow(clippy::too_many_arguments)]
pub(crate) fn merge_teardown_guarded(
    workbench: &crate::workbench::WorkbenchState,
    repo_path: &str,
    worktree_path: &str,
    target_branch: Option<&str>,
    is_squash: bool,
    expected_tip: Option<&str>,
    require_write_tree: bool,
    task_id: Option<&str>,
    judge: WorktreeGate<'_>,
) -> Result<Guarded<crate::engine::worktree::MergeTeardownResult>, MergeTeardownError> {
    // Before the merge, not only before the removal: merging a branch an
    // agent is still committing to takes half its work.
    workbench.refuse_if_in_use(worktree_path)?;
    // Each step is judged as it runs, with what runs (see the engine's
    // doc). The verdict reported is the merge's: the first mutation, and
    // the one the rest depend on.
    let mut policy = None;
    let mut gate = |args: &[&str]| -> Result<(), CommandError> {
        let argv: Vec<&str> = std::iter::once("git").chain(args.iter().copied()).collect();
        let verdict = judge(repo_path, &argv)?;
        policy.get_or_insert(verdict);
        Ok(())
    };
    let result = if require_write_tree {
        let task = task_id.unwrap_or("").to_string();
        let host = workbench.clone();
        let mut verify = move |scratch: &std::path::Path, candidate: &str| {
            crate::workbench::verification_driver::judge_candidate(
                &host,
                &task,
                scratch,
                candidate,
                &mut std::thread::sleep,
            )
        };
        crate::engine::worktree::merge_and_teardown_verified(
            repo_path,
            worktree_path,
            target_branch,
            is_squash,
            expected_tip,
            true,
            Some(&mut verify),
            &mut gate,
        )?
    } else {
        crate::engine::worktree::merge_and_teardown_requiring(
            repo_path,
            worktree_path,
            target_branch,
            is_squash,
            expected_tip,
            false,
            &mut gate,
        )?
    };
    Ok(Guarded {
        policy: policy.ok_or_else(|| {
            MergeTeardownError::from(
                "The merge ran without being judged by the policy gate.".to_string(),
            )
        })?,
        output: result,
    })
}

/// Reflink-copies the repository's ignored dependency caches into one of its
/// worktrees. A cache that could not be copied whole fails the call.
#[tauri::command(async)]
pub async fn cmd_worktree_cow_sync(
    repo_path: String,
    target_path: String,
) -> Result<Vec<crate::engine::cow_clone::ReflinkResult>, IpcError> {
    off_thread(move || {
        let repo = crate::engine::validate_repo(&repo_path)?;
        // The target comes from the webview. Only a checkout git lists as a
        // worktree of this repository receives caches — one `git worktree
        // list`, no scan of any worktree. Any other existing directory used to
        // be filled with the anchor's caches and links.
        let family = crate::engine::worktree::resolve_worktree_family(&repo_path, &target_path)?;
        crate::engine::cow_clone::reflink_ignored_caches(
            &repo,
            &family.worktree,
            &crate::engine::cow_clone::CacheSelection::default(),
        )
    })
    .await
}

/// Discovers live portless and localhost dev server routes for a worktree.
#[tauri::command(async)]
pub async fn cmd_worktree_routes(
    worktree_path: String,
    branch: Option<String>,
) -> Result<Vec<crate::engine::portless::WorktreeRouteInfo>, IpcError> {
    off_thread(move || {
        Ok(crate::engine::portless::detect_worktree_routes(
            &worktree_path,
            branch.as_deref(),
        ))
    })
    .await
}

/// Provides a high-density 1-line diff & status summary of a worktree.
#[tauri::command(async)]
pub async fn cmd_worktree_ai_summary(
    repo_path: String,
    worktree_path: String,
    branch: Option<String>,
) -> Result<String, IpcError> {
    off_thread(move || worktree_ai_summary(&repo_path, &worktree_path, branch.as_deref())).await
}

/// Body of [`cmd_worktree_ai_summary`].
pub(super) fn worktree_ai_summary(
    repo_path: &str,
    worktree_path: &str,
    branch: Option<&str>,
) -> Result<String, CommandError> {
    let wt = std::path::Path::new(worktree_path);
    if !wt.exists() {
        return Err("Worktree path does not exist".into());
    }
    // Each measurement below reads a failed git as "absent", and with all
    // three absent the summary used to say "Worktree clean". A read that
    // produced no answer fails the summary instead, with its own cause —
    // a deferral keeps its marker, so the call is asked again later.
    let failures = crate::engine::git_cli::process_failures();
    let diff_stat = crate::engine::worktree::measure_diff_stat(wt);
    let repo = crate::engine::validate_repo(repo_path).ok();
    let divergence = repo
        .as_ref()
        .and_then(|r| crate::engine::worktree::measure_main_divergence(r, branch));
    let log_out = git_text(wt, &["log", "-1", "--pretty=%s"]).unwrap_or_default();
    if crate::engine::git_cli::process_failures() != failures {
        return Err(crate::engine::git_cli::last_process_failure()
            .unwrap_or_else(|| "a git read for the worktree summary produced no answer".into()));
    }
    let last_msg = log_out.trim();

    let mut parts = Vec::new();
    if let Some(stat) = diff_stat {
        if stat.files_changed > 0 {
            parts.push(format!(
                "HEAD±: {} files (+{}, -{})",
                stat.files_changed, stat.insertions, stat.deletions
            ));
        } else {
            parts.push("HEAD±: clean".to_string());
        }
    }
    if let Some(div) = divergence {
        if div.ahead > 0 || div.behind > 0 {
            parts.push(format!("base↕: +{} -{}", div.ahead, div.behind));
        } else {
            parts.push("base↕: synced".to_string());
        }
    }
    if !last_msg.is_empty() {
        parts.push(format!("tip: \"{last_msg}\""));
    }
    if parts.is_empty() {
        Ok("Worktree clean".to_string())
    } else {
        Ok(parts.join(" · "))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::RefCell;

    #[test]
    fn add_worktree_guarded_supports_zero_copy_devmap_and_sparse_cone() {
        let sandbox = tempfile::tempdir().expect("tempdir");
        let raw_anchor = sandbox.path().join("anchor");
        std::fs::create_dir_all(raw_anchor.join("pkg/api")).expect("mkdir api");
        std::fs::create_dir_all(raw_anchor.join("pkg/other")).expect("mkdir other");
        std::fs::write(raw_anchor.join(".gitignore"), ".devmap/\n.devcouncil/\n")
            .expect("write gitignore");
        std::fs::write(raw_anchor.join("pkg/api/lib.rs"), "pub fn api() {}\n").expect("write api");
        std::fs::write(raw_anchor.join("pkg/other/lib.rs"), "pub fn other() {}\n")
            .expect("write other");

        crate::test_support::git_in(&raw_anchor, &["init", "-q", "-b", "main"]);
        crate::test_support::git_in(&raw_anchor, &["config", "user.name", "Test"]);
        crate::test_support::git_in(&raw_anchor, &["config", "user.email", "test@example.com"]);
        crate::test_support::git_in(&raw_anchor, &["add", "."]);
        crate::test_support::git_in(&raw_anchor, &["commit", "-q", "-m", "add packages"]);
        let repo_dir = std::fs::canonicalize(&raw_anchor).expect("canonicalize anchor");
        crate::test_support::trust_repo(&repo_dir);
        crate::devmap::ensure_state_dir_excluded(&repo_dir);

        let head_run =
            crate::engine::git_cli::git_captured(&repo_dir, &["rev-parse", "HEAD"]).unwrap();
        let head_sha = String::from_utf8_lossy(&head_run.stdout).trim().to_string();
        let scanned = devmap_extract::scan_tree(&repo_dir).expect("scan_tree");
        let db = devmap_query::paths::store_path(&repo_dir);
        std::fs::create_dir_all(db.parent().unwrap()).expect("mkdir codeintel");
        let store = devmap_store::Store::open(&db).expect("open store");
        store.bind_repo_root(&repo_dir).expect("bind repo root");
        drop(store);
        let conn = rusqlite::Connection::open(&db).expect("open sqlite");
        for (idx, (path, raw_hash)) in scanned.file_hashes().into_iter().enumerate() {
            let id = (idx + 1) as i64;
            let lang = devmap_extract::detect_language(std::path::Path::new(path));
            let (grammar_ver, analyzer_ver) = devmap_extract::cache::current_payload_identity(lang);
            let engine_json =
                serde_json::to_string(&devmap_extract::ExtractionEngine::TreeSitter {
                    grammar: lang.to_string(),
                    grammar_version: grammar_ver.parse::<u32>().unwrap_or(1),
                })
                .unwrap();
            let extraction_json = serde_json::json!({
                "file_path": path,
                "language": lang,
                "content_hash": raw_hash,
                "parse_outcome": "Clean",
                "symbols": [],
                "imports": [],
                "calls": []
            })
            .to_string();
            conn.execute("INSERT INTO paths (id, path) VALUES (?1, ?2)", (id, path))
                .unwrap();
            conn.execute(
                "INSERT INTO file_payloads
                   (payload_id, file_id, content_hash, language,
                    parse_outcome_json, engine_json, extraction_json,
                    entry_wiring_json, grammar_version, analyzer_version)
                 VALUES (?1, ?1, ?2, ?3, '\"Clean\"', ?4, ?5, '[]', ?6, ?7)",
                (
                    id,
                    raw_hash as i64,
                    lang,
                    &engine_json,
                    &extraction_json,
                    &grammar_ver,
                    &analyzer_ver,
                ),
            )
            .unwrap();
            conn.execute(
                "INSERT INTO file_rows (file_id, payload_id, valid_from, valid_to)
                 VALUES (?1, ?1, 1, NULL)",
                [id],
            )
            .unwrap();
        }
        conn.execute(
            "INSERT INTO generations (id, created_at, head_sha, analysis_json, repo_root)
             VALUES (1, 0.0, ?1, ?2, ?3)",
            (
                &head_sha,
                r#"{"total_files":2,"total_symbols":0,"total_edges":0,"dead_symbols":[],"communities":[],"status":"Ok","unresolved_calls":0}"#,
                repo_dir.to_str().unwrap(),
            ),
        )
        .unwrap();
        drop(conn);

        let repo_str = repo_dir.to_str().expect("repo_str");
        let judged: RefCell<Vec<Vec<String>>> = RefCell::new(Vec::new());
        let gate = |repo: &str, argv: &[&str]| {
            judged
                .borrow_mut()
                .push(argv.iter().map(|s| (*s).to_string()).collect());
            crate::harness::guard_command(repo, argv)
        };

        // 1. Refuses `.gitpulse/worktrees/...` before invoking the gate.
        let illegal_target = repo_dir.join(".gitpulse/worktrees/illegal");
        let refused = add_worktree_guarded(
            repo_str,
            illegal_target.to_str().expect("illegal"),
            Some("feat/illegal"),
            None,
            false,
            false,
            false,
            crate::engine::worktree::DevmapMode::ZeroCopy,
            None,
            &gate,
        );
        assert!(refused.is_err());
        assert!(judged.borrow().is_empty());

        // 2. Full checkout with `zero_copy_devmap = true` writes `stack_lease.json` without copying `devmap.sqlite`.
        let zc_target = sandbox.path().join("wt-zc");
        let zc_created = add_worktree_guarded(
            repo_str,
            zc_target.to_str().expect("zc"),
            Some("feat/zc"),
            None,
            false,
            false,
            false,
            crate::engine::worktree::DevmapMode::ZeroCopy,
            None,
            &gate,
        )
        .expect("zc add_worktree_guarded");
        assert_eq!(
            judged.borrow()[0],
            vec![
                "git",
                "worktree",
                "add",
                "-b",
                "feat/zc",
                zc_target.to_str().expect("zc"),
            ]
        );
        let zc_wt = std::path::Path::new(&zc_created.output.path);
        crate::test_support::trust_repo(zc_wt);
        assert!(!devmap_query::paths::store_path(zc_wt).exists());
        assert!(devmap_query::paths::stack_lease_path(zc_wt).is_file());

        // 3. Sparse cone checkout judges `--no-checkout` and materializes only `pkg/api`.
        let sparse_target = sandbox.path().join("wt-sparse");
        let sparse_paths = vec!["pkg/api".to_string()];
        let sparse_created = add_worktree_guarded(
            repo_str,
            sparse_target.to_str().expect("sparse"),
            Some("feat/sparse"),
            None,
            false,
            false,
            false,
            crate::engine::worktree::DevmapMode::ZeroCopy,
            Some(&sparse_paths),
            &gate,
        )
        .expect("sparse add_worktree_guarded");
        assert_eq!(
            judged.borrow()[1],
            vec![
                "git",
                "worktree",
                "add",
                "--no-checkout",
                "-b",
                "feat/sparse",
                sparse_target.to_str().expect("sparse"),
            ]
        );
        let sp_wt = std::path::Path::new(&sparse_created.output.path);
        crate::test_support::trust_repo(sp_wt);
        assert!(sp_wt.join("pkg/api/lib.rs").is_file());
        assert!(!sp_wt.join("pkg/other/lib.rs").exists());
        assert!(devmap_query::paths::stack_lease_path(sp_wt).is_file());

        // 4. Sparse cone checkout without the lease (`DevmapMode::None`; `Copy`
        //    behaves the same here) still provisions a local CoW `devmap.sqlite`:
        //    the sparse path seeds either way, and only `ZeroCopy` writes
        //    `stack_lease.json` instead.
        let sparse_local_target = sandbox.path().join("wt-sparse-local-devmap");
        let sparse_local_created = add_worktree_guarded(
            repo_str,
            sparse_local_target.to_str().expect("sparse_local"),
            Some("feat/sparse-local"),
            None,
            false,
            false,
            false,
            crate::engine::worktree::DevmapMode::None,
            Some(&sparse_paths),
            &gate,
        )
        .expect("sparse local devmap add_worktree_guarded");
        let sp_local_wt = std::path::Path::new(&sparse_local_created.output.path);
        crate::test_support::trust_repo(sp_local_wt);
        assert!(sp_local_wt.join("pkg/api/lib.rs").is_file());
        assert!(!sp_local_wt.join("pkg/other/lib.rs").exists());
        assert!(
            !devmap_query::paths::stack_lease_path(sp_local_wt).exists(),
            "zero_copy_devmap=false must not write stack_lease.json"
        );
        assert!(
            devmap_query::paths::store_path(sp_local_wt).is_file(),
            "zero_copy_devmap=false with sparse_paths must provision local devmap.sqlite"
        );
    }

    /// What the Worktrees panel asks for by default: a full checkout whose
    /// DevMap store is CoW-cloned from the main checkout and then built for
    /// this worktree's own tree.
    #[cfg(unix)]
    #[test]
    fn seed_devmap_clones_the_store_and_starts_the_worktrees_own_build() {
        use std::os::unix::fs::PermissionsExt;
        let bin_dir = tempfile::tempdir().expect("tempdir");
        let log = bin_dir.path().join("devmap-argv.log");
        let bin = bin_dir.path().join("devmap");
        std::fs::write(
            &bin,
            format!(
                "#!/bin/sh\n\
                 printf '%s %s\\n' \"$PWD\" \"$*\" >> '{log}'\n\
                 if [ \"$1\" = status ]; then printf '%s\\n' '{{\"is_fresh\":false,\"schema_outdated\":false}}'; exit 0; fi\n\
                 if [ \"$1\" = build ]; then printf '%s\\n' '{{\"ok\":true}}'; exit 0; fi\n\
                 exit 2\n",
                log = log.display()
            ),
        )
        .expect("write stub");
        std::fs::set_permissions(&bin, std::fs::Permissions::from_mode(0o755)).expect("chmod");
        let _devmap = crate::devmap::cli::bind_test_binary(bin.to_string_lossy().into_owned());

        let sandbox = tempfile::tempdir().expect("tempdir");
        let anchor_fixture = crate::codeintel::tests::repo_with_one_generation();
        let repo_dir = anchor_fixture.path().to_path_buf();
        crate::test_support::git_in(&repo_dir, &["config", "user.name", "Test"]);
        crate::test_support::git_in(&repo_dir, &["config", "user.email", "test@example.com"]);
        crate::test_support::git_in(&repo_dir, &["add", "src"]);
        crate::test_support::git_in(&repo_dir, &["commit", "-m", "init"]);
        crate::test_support::trust_repo(&repo_dir);

        assert_eq!(
            devmap_mode(None, Some(true)),
            crate::engine::worktree::DevmapMode::Copy
        );
        assert_eq!(
            devmap_mode(Some(true), Some(true)),
            crate::engine::worktree::DevmapMode::ZeroCopy
        );
        assert_eq!(
            devmap_mode(None, None),
            crate::engine::worktree::DevmapMode::None
        );

        let target = sandbox.path().join("wt-seeded");
        let created = add_worktree_guarded(
            repo_dir.to_str().expect("repo"),
            target.to_str().expect("target"),
            Some("feat/seeded"),
            None,
            false,
            false,
            false,
            devmap_mode(None, Some(true)),
            None,
            &|repo, argv| crate::harness::guard_command(repo, argv),
        )
        .expect("seeded add_worktree_guarded");
        let wt = std::path::Path::new(&created.output.path)
            .canonicalize()
            .expect("canonical worktree");
        assert!(devmap_query::paths::store_path(&wt).is_file());
        assert!(!devmap_query::paths::stack_lease_path(&wt).exists());

        let deadline = std::time::Instant::now()
            + crate::test_support::env::coverage_relaxed(std::time::Duration::from_secs(10));
        loop {
            let seen = std::fs::read_to_string(&log).unwrap_or_default();
            if seen.lines().any(|line| {
                line.split_once(' ').is_some_and(|(cwd, argv)| {
                    std::path::Path::new(cwd).canonicalize().ok().as_deref() == Some(wt.as_path())
                        && argv.split_whitespace().next() == Some("build")
                })
            }) {
                break;
            }
            assert!(
                std::time::Instant::now() < deadline,
                "no devmap build was started in {}; devmap saw:\n{seen}",
                wt.display()
            );
            std::thread::sleep(std::time::Duration::from_millis(25));
        }
    }
}
