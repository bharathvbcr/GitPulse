//! Code intelligence, DevMap, workspace, DevCouncil, insights and fleet commands.

use super::*;
use crate::codeintel::WalkBounds;
use crate::ipc_error::CommandError;

// --- code intelligence (devmap in-process) ----------------------------

/// Status and metadata of the repository's code intelligence graph.
#[tauri::command(async)]
pub async fn cmd_codeintel_status(
    repo_path: String,
) -> Result<crate::codeintel::CodeintelStatus, IpcError> {
    off_thread(move || Ok(crate::codeintel::status(&repo_path))).await
}

/// Symbol search across indexed code files.
#[tauri::command(async)]
pub async fn cmd_codeintel_search(
    repo_path: String,
    query: String,
    token_budget: Option<u32>,
) -> Result<crate::codeintel::CodeintelResponse<crate::codeintel::CodeintelSymbolHit>, IpcError> {
    off_thread(move || Ok(crate::codeintel::search(&repo_path, &query, token_budget))).await
}

/// Bounded symbols-in-file span read for one path at a named head SHA.
#[tauri::command(async)]
pub async fn cmd_codeintel_symbols_for_file(
    repo_path: String,
    file_path: String,
    head_sha: String,
) -> Result<crate::codeintel::CodeintelFileSymbols, IpcError> {
    off_thread(move || {
        Ok(crate::codeintel::symbols_for_file(
            &repo_path, &file_path, &head_sha,
        ))
    })
    .await
}

/// Blast radius and impact graph for a symbol or file.
#[tauri::command(async)]
pub async fn cmd_codeintel_impact(
    repo_path: String,
    target: String,
    token_budget: Option<u32>,
    cancel_token: Option<String>,
) -> Result<crate::codeintel::CodeintelResponse<crate::codeintel::CodeintelEdge>, IpcError> {
    off_thread(move || {
        let cancel = crate::codeintel::begin_cancellable_query(cancel_token.as_deref());
        let result = crate::codeintel::impact_at_rung(
            &repo_path,
            &target,
            token_budget,
            None,
            WalkBounds::default(),
            Some(cancel),
        );
        crate::codeintel::finish_cancellable_query(cancel_token.as_deref());
        Ok(result)
    })
    .await
}

/// Which commits between `since` and the indexed head could have caused a
/// symptom.
///
/// Returns the scope beside the ranked list, and it is not optional detail: an
/// empty list means "nothing in the window touched the dependency cone", which
/// is a finding, and the scope is what lets the caller tell that from an answer
/// that was cut short. `walk_incomplete` carries the refusals when the list is
/// a lower bound.
///
/// Runs off-thread: it shells out to `git blame` once per file in the cone, so
/// it is bounded but not instant.
#[tauri::command(async)]
pub async fn cmd_codeintel_suspects(
    repo_path: String,
    symptom: String,
    since: String,
    depth: Option<u32>,
) -> Result<crate::codeintel::CodeintelSuspectsPayload, IpcError> {
    off_thread(move || {
        Ok(crate::codeintel::suspects(
            &repo_path, &symptom, &since, depth,
        ))
    })
    .await
}

/// Dead code analysis across the repository.
#[tauri::command(async)]
pub async fn cmd_codeintel_dead_symbols(
    repo_path: String,
    token_budget: Option<u32>,
) -> Result<crate::codeintel::CodeintelResponse<crate::codeintel::CodeintelDeadSymbol>, IpcError> {
    off_thread(move || Ok(crate::codeintel::dead_symbols(&repo_path, token_budget))).await
}

/// File dependencies for a path.
#[tauri::command(async)]
pub async fn cmd_codeintel_dependencies(
    repo_path: String,
    file_path: String,
    token_budget: Option<u32>,
    min_rung: Option<String>,
) -> Result<crate::codeintel::CodeintelResponse<crate::codeintel::CodeintelEdge>, IpcError> {
    off_thread(move || {
        Ok(crate::codeintel::dependencies_at_rung(
            &repo_path,
            &file_path,
            token_budget,
            min_rung.as_deref(),
            WalkBounds::default(),
            None,
        ))
    })
    .await
}

/// Trace a path between two symbols.
#[tauri::command(async)]
pub async fn cmd_codeintel_trace(
    repo_path: String,
    from: String,
    to: String,
    token_budget: Option<u32>,
    min_rung: Option<String>,
) -> Result<crate::codeintel::CodeintelResponse<crate::codeintel::CodeintelEdge>, IpcError> {
    off_thread(move || {
        Ok(crate::codeintel::trace_between_at_rung(
            &repo_path,
            &from,
            &to,
            token_budget,
            min_rung.as_deref(),
            WalkBounds::default(),
            None,
        ))
    })
    .await
}

/// Callers and callees for one or more targets.
#[tauri::command(async)]
pub async fn cmd_codeintel_neighbors(
    repo_path: String,
    targets: Vec<String>,
    token_budget: Option<u32>,
    min_rung: Option<String>,
) -> Result<Vec<crate::codeintel::CodeintelNeighbors>, IpcError> {
    off_thread(move || {
        crate::codeintel::neighbors(
            &repo_path,
            &targets,
            token_budget,
            min_rung.as_deref(),
            WalkBounds::default(),
        )
    })
    .await
}

/// Explore a symbol: definitions plus blast radius.
#[tauri::command(async)]
pub async fn cmd_codeintel_explore(
    repo_path: String,
    query: String,
    token_budget: Option<u32>,
    limit: Option<u32>,
) -> Result<crate::codeintel::CodeintelExplore, IpcError> {
    off_thread(move || {
        Ok(crate::codeintel::explore(
            &repo_path,
            &query,
            token_budget,
            limit,
            WalkBounds::default(),
        ))
    })
    .await
}

/// Affected test files for a seed set (fail-closed metadata included).
#[tauri::command(async)]
pub async fn cmd_codeintel_affected_tests(
    repo_path: String,
    targets: Vec<String>,
    token_budget: Option<u32>,
    max_depth: Option<usize>,
) -> Result<crate::codeintel::CodeintelAffectedTests, IpcError> {
    off_thread(move || {
        Ok(crate::codeintel::affected_tests(
            &repo_path,
            &targets,
            token_budget,
            WalkBounds {
                max_depth,
                min_confidence: None,
            },
        ))
    })
    .await
}

/// Duplicate-code groups.
#[tauri::command(async)]
pub async fn cmd_codeintel_clones(
    repo_path: String,
    token_budget: Option<u32>,
) -> Result<crate::codeintel::CodeintelClones, IpcError> {
    off_thread(move || Ok(crate::codeintel::clones(&repo_path, token_budget))).await
}

/// Layered blast radius for one target (do not combine with min_rung).
#[tauri::command(async)]
pub async fn cmd_codeintel_impact_layered(
    repo_path: String,
    target: String,
    token_budget: Option<u32>,
    cancel_token: Option<String>,
) -> Result<crate::codeintel::CodeintelLayeredImpact, IpcError> {
    off_thread(move || {
        let cancel = crate::codeintel::begin_cancellable_query(cancel_token.as_deref());
        let result = crate::codeintel::impact_layered_with_cancel(
            &repo_path,
            &target,
            token_budget,
            Some(cancel),
        );
        crate::codeintel::finish_cancellable_query(cancel_token.as_deref());
        Ok(result)
    })
    .await
}

/// Layered blast radius composed over many changed-file seeds.
#[tauri::command(async)]
pub async fn cmd_codeintel_impact_layered_many(
    repo_path: String,
    targets: Vec<String>,
    token_budget: Option<u32>,
    cancel_token: Option<String>,
) -> Result<Vec<crate::codeintel::CodeintelLayeredImpact>, IpcError> {
    off_thread(move || {
        let cancel = crate::codeintel::begin_cancellable_query(cancel_token.as_deref());
        let result = crate::codeintel::impact_layered_many_with_cancel(
            &repo_path,
            &targets,
            token_budget,
            Some(cancel),
        );
        crate::codeintel::finish_cancellable_query(cancel_token.as_deref());
        Ok(result)
    })
    .await
}

/// Impact with an optional resolution-rung floor.
#[tauri::command(async)]
pub async fn cmd_codeintel_impact_at_rung(
    repo_path: String,
    target: String,
    token_budget: Option<u32>,
    min_rung: Option<String>,
    cancel_token: Option<String>,
) -> Result<crate::codeintel::CodeintelResponse<crate::codeintel::CodeintelEdge>, IpcError> {
    off_thread(move || {
        let cancel = crate::codeintel::begin_cancellable_query(cancel_token.as_deref());
        let result = crate::codeintel::impact_at_rung(
            &repo_path,
            &target,
            token_budget,
            min_rung.as_deref(),
            WalkBounds::default(),
            Some(cancel),
        );
        crate::codeintel::finish_cancellable_query(cancel_token.as_deref());
        Ok(result)
    })
    .await
}

/// Trip an in-flight codeintel walk (file switch / dismiss). Timeout also
/// trips the same flag; this is the user-driven half.
#[tauri::command(async)]
pub async fn cmd_codeintel_cancel(cancel_token: String) -> Result<bool, IpcError> {
    Ok(crate::codeintel::cancel_query(&cancel_token))
}

/// Build the code map via the installed `devmap` CLI.
#[tauri::command(async)]
pub async fn cmd_devmap_build(repo_path: String) -> Result<crate::devmap::BuildOutcome, IpcError> {
    off_thread(move || {
        crate::devmap::clear_live_echo_cooldown(&repo_path);
        crate::devmap::build(&repo_path).map_err(CommandError::from)
    })
    .await
}

/// Incremental refresh via the installed `devmap` CLI.
#[tauri::command(async)]
pub async fn cmd_devmap_refresh(
    repo_path: String,
) -> Result<crate::devmap::BuildOutcome, IpcError> {
    off_thread(move || {
        crate::devmap::clear_live_echo_cooldown(&repo_path);
        crate::devmap::refresh(&repo_path).map_err(CommandError::from)
    })
    .await
}

/// Watcher-driven live index: refresh only when stale, schema-ok, and idle.
///
/// `repo_changed` should be true when this follows a `repo-changed` event —
/// that folds the working-tree dirty signal into effective freshness so a
/// store with an empty pending queue still rebuilds after a settled write.
///
/// Never waits on a build: an authorized build runs on its own thread and
/// this answers `started`; the next call for the repository collects the
/// outcome. The blocking-pool thread is held only for the status probe.
#[tauri::command(async)]
pub async fn cmd_devmap_maybe_refresh(
    repo_path: String,
    repo_changed: Option<bool>,
) -> Result<crate::devmap::LiveRefreshOutcome, IpcError> {
    off_thread_background(move || {
        Ok(crate::devmap::live::start_refresh(
            &repo_path,
            repo_changed.unwrap_or(true),
        ))
    })
    .await
}

/// `devmap status --json` via the CLI.
#[tauri::command(async)]
pub async fn cmd_devmap_status(repo_path: String) -> Result<crate::devmap::CliStatus, IpcError> {
    off_thread(move || Ok(crate::devmap::cli_status(&repo_path))).await
}

/// Preview what an unsaved buffer would break (`devmap preview`).
#[tauri::command(async)]
pub async fn cmd_devmap_preview(
    repo_path: String,
    file_path: String,
    content: String,
) -> Result<crate::devmap::PreviewFileResult, IpcError> {
    // Refused before the buffer is moved into the worker or copied again.
    super::payload::check_content(
        "preview file content",
        &content,
        super::payload::MAX_PREVIEW_FILE_BYTES,
    )?;
    off_thread(move || Ok(crate::devmap::preview(&repo_path, &file_path, &content))).await
}

/// Preview many changed files; cancellable between files.
///
/// `cancel_token` names the request: `cmd_codeintel_cancel` with the same
/// token stops the batch before its next file (the caller abandoned it), and
/// the 30 s query backstop stops it regardless. The result then says
/// `cancelled`, never a complete-looking list.
///
/// The count and byte caps run here, before `preview_many` clones any entry.
#[tauri::command(async)]
pub async fn cmd_devmap_preview_many(
    repo_path: String,
    files: Vec<(String, String)>,
    cancel_token: Option<String>,
) -> Result<crate::devmap::PreviewOutcome, IpcError> {
    super::payload::check_preview_batch(&files)?;
    off_thread(move || Ok(preview_many_cancellable(&repo_path, &files, cancel_token))).await
}

/// [`crate::devmap::preview_many`] with its cancel flag bound to `cancel_token`
/// in the shared query registry.
fn preview_many_cancellable(
    repo_path: &str,
    files: &[(String, String)],
    cancel_token: Option<String>,
) -> crate::devmap::PreviewOutcome {
    let cancel = crate::codeintel::begin_cancellable_query(cancel_token.as_deref());
    let outcome = crate::devmap::preview_many(repo_path, files, || cancel.is_cancelled());
    crate::codeintel::finish_cancellable_query(cancel_token.as_deref());
    outcome
}

/// Read `.devcouncil/repo_map.json` into GitPulse's typed navigator mirror.
#[tauri::command(async)]
pub async fn cmd_devmap_repo_map(
    repo_path: String,
) -> Result<crate::devmap::RepoMapLoad, IpcError> {
    off_thread(move || Ok(crate::devmap::load_repo_map(&repo_path))).await
}

/// Code-graph canvas payload via `devmap_query::viz::build_payload`.
///
/// Reads on-disk `code_graph.json`, ranks nodes by degree, and caps at
/// `max_nodes` (default 1500). The payload always reports
/// `counts.nodes_truncated` — a capped picture is never the whole graph.
#[tauri::command(async)]
pub async fn cmd_devmap_viz(
    repo_path: String,
    symbols: Option<bool>,
    max_nodes: Option<usize>,
) -> Result<crate::devmap::GraphVizLoad, IpcError> {
    off_thread(move || {
        Ok(crate::devmap::load_code_graph_viz(
            &repo_path, symbols, max_nodes,
        ))
    })
    .await
}

/// Subsystem map-preview payload via `map_preview::build_preview_payload`.
#[tauri::command(async)]
pub async fn cmd_devmap_map_preview(
    repo_path: String,
) -> Result<crate::devmap::GraphVizLoad, IpcError> {
    off_thread(move || Ok(crate::devmap::load_map_preview(&repo_path))).await
}

// --- multi-repo workspace registry (devmap workspace.json) ------------

/// Register one repository in the workspace registry rooted at `registry_root`.
#[tauri::command(async)]
pub async fn cmd_workspace_register(
    registry_root: String,
    repo_path: String,
    name: Option<String>,
) -> Result<crate::workspace_registry::WorkspaceRegisterResult, IpcError> {
    off_thread(move || {
        crate::workspace_registry::register(&registry_root, &repo_path, name.as_deref())
    })
    .await
}

/// Remove a repository from the workspace registry by name.
#[tauri::command(async)]
pub async fn cmd_workspace_unregister(
    registry_root: String,
    name: String,
) -> Result<crate::workspace_registry::WorkspaceUnregisterResult, IpcError> {
    off_thread(move || crate::workspace_registry::unregister(&registry_root, &name)).await
}

/// List repositories in the workspace registry.
#[tauri::command(async)]
pub async fn cmd_workspace_list(
    registry_root: String,
) -> Result<crate::workspace_registry::WorkspaceSnapshot, IpcError> {
    off_thread(move || crate::workspace_registry::list(&registry_root)).await
}

/// Initialize one opened repository for DevCouncil: ignore hygiene for the
/// DevMap state directory, then the workspace registry.
///
/// Writes only inside `.git/info/exclude` and the resolved state directory, so
/// nothing it does can appear in a diff or a commit. Agent guides, editor rules
/// and MCP files are not touched here — those are `cmd_devmap_integrate`.
#[tauri::command(async)]
pub async fn cmd_devcouncil_init(
    repo_path: String,
    open_repos: Vec<String>,
) -> Result<crate::devmap::InitReport, IpcError> {
    off_thread(move || crate::devmap::initialize_repository(&repo_path, &open_repos)).await
}

/// What of DevCouncil is installed, plus the installation warnings `devmap
/// doctor` already measures.
///
/// `repo_path` is only where the doctor probe runs — it is read-only and
/// repository-independent. Without a trusted repository the component
/// inventory is still returned and the doctor section says why it is absent,
/// rather than presenting a warning-free report nothing checked.
#[tauri::command(async)]
pub async fn cmd_devcouncil_suite_status(
    repo_path: Option<String>,
) -> Result<crate::tool_install::components::SuiteReport, IpcError> {
    off_thread(move || {
        Ok(crate::tool_install::components::suite_report(
            repo_path.as_deref(),
        ))
    })
    .await
}

/// What registering DevMap with one agent host would change in this
/// repository. Writes nothing.
#[tauri::command(async)]
pub async fn cmd_devmap_integration_preview(
    repo_path: String,
    host: crate::devmap::IntegrationHost,
) -> Result<crate::devmap::IntegrationPlan, IpcError> {
    off_thread(move || Ok(crate::devmap::preview_integration(&repo_path, host))).await
}

/// The same preview for every host at once, for the "is this repository set up
/// for agents" strip. Still writes nothing, so it is safe on repository open.
#[tauri::command(async)]
pub async fn cmd_devmap_integration_survey(
    repo_path: String,
) -> Result<Vec<crate::devmap::IntegrationPlan>, IpcError> {
    off_thread(move || Ok(crate::devmap::survey_integrations(&repo_path))).await
}

/// Write the integration assets for one host.
///
/// This edits tracked files in the repository and a machine-wide MCP
/// registration in the user's home directory, so it must only ever be reached
/// from an explicit user action that has seen `cmd_devmap_integration_preview`.
#[tauri::command(async)]
pub async fn cmd_devmap_integration_apply(
    repo_path: String,
    host: crate::devmap::IntegrationHost,
) -> Result<crate::devmap::IntegrationPlan, IpcError> {
    off_thread(move || Ok(crate::devmap::apply_integration(&repo_path, host))).await
}

/// Replace the registry so it matches the open-tab set (plus the registry host).
#[tauri::command(async)]
pub async fn cmd_workspace_sync(
    registry_root: String,
    repo_paths: Vec<String>,
) -> Result<crate::workspace_registry::WorkspaceSnapshot, IpcError> {
    off_thread(move || crate::workspace_registry::sync_open_tabs(&registry_root, &repo_paths)).await
}

/// Cross-repo symbol search. `semantic: true` uses TF-IDF name ranking — not AI.
#[tauri::command(async)]
pub async fn cmd_workspace_search(
    registry_root: String,
    query: String,
    token_budget: Option<u32>,
    semantic: Option<bool>,
) -> Result<crate::workspace_registry::WorkspaceSearchResult, IpcError> {
    off_thread(move || {
        crate::workspace_registry::search(
            &registry_root,
            &query,
            token_budget,
            semantic.unwrap_or(false),
        )
    })
    .await
}

/// Declared cross-repo import links among registered repositories.
#[tauri::command(async)]
pub async fn cmd_workspace_link_candidates(
    registry_root: String,
) -> Result<crate::workspace_registry::WorkspaceLinksResult, IpcError> {
    off_thread(move || crate::workspace_registry::link_candidates(&registry_root)).await
}

/// One-shot Work-view snapshot: worktrees, agent sessions, collisions, ledger, code graph.
#[tauri::command(async)]
pub async fn cmd_insights_snapshot(
    repo_path: String,
) -> Result<crate::insights::InsightsSnapshot, IpcError> {
    off_thread(move || Ok(crate::insights::snapshot(&repo_path))).await
}

/// Files with uncommitted changes in more than one worktree, and in any
/// worktree two live agent sessions share.
#[tauri::command(async)]
pub async fn cmd_collision_risk(
    repo_path: String,
) -> Result<crate::insights::CollisionRisk, IpcError> {
    off_thread(move || Ok(crate::insights::collision_risk(&repo_path))).await
}

/// The cheap facet for a whole workspace of repositories, in one round trip.
///
/// Two `git` spawns per repository plus a read-only look at each repository's
/// own ledger. Deliberately narrower than `cmd_insights_snapshot`, which
/// probes every worktree and is priced for one repository on screen rather
/// than two dozen. Per-repository failures ride in their own facet.
///
/// `window_days` sets the commit window. It costs no extra `git` — the walk is
/// bounded by commit count, not by date — and is clamped rather than refused,
/// with the window actually used reported back on every facet.
#[tauri::command(async)]
pub async fn cmd_fleet_snapshot(
    repo_paths: Vec<String>,
    window_days: Option<u32>,
) -> Result<crate::insights::FleetSnapshot, IpcError> {
    off_thread(move || Ok(crate::insights::fleet_snapshot(&repo_paths, window_days))).await
}

/// Records one expensive scan's result in that repository's own ledger.
///
/// Called after a Fleet scan lands, so the number survives a restart and can
/// be shown with its age. Families are independent: the fields this call
/// leaves null keep whatever was recorded before.
#[tauri::command(async)]
pub async fn cmd_fleet_record_metrics(
    repo_path: String,
    metrics: crate::ledger::FleetMetricsInput,
) -> Result<(), IpcError> {
    off_thread(move || {
        let repo = validate_repo(&repo_path)?;
        crate::ledger::save_fleet_metrics(&repo.to_string_lossy(), &metrics)
            .map_err(|e| e.to_string())
            .map_err(CommandError::from)
    })
    .await
}

#[cfg(test)]
mod payload_cap_tests {
    use super::*;
    use crate::commands::payload::{MAX_CONTENT_BYTES, MAX_PREVIEW_FILE_BYTES};

    /// The over-cap request is refused with a size error before any worker
    /// runs: a repository that does not exist would otherwise be the error.
    #[test]
    fn devmap_preview_refuses_an_oversized_buffer_at_the_boundary() {
        let huge = "x".repeat(MAX_PREVIEW_FILE_BYTES + 1);
        let err = tauri::async_runtime::block_on(cmd_devmap_preview(
            "/no/such/repo".into(),
            "src/lib.rs".into(),
            huge,
        ))
        .expect_err("over the cap");
        assert!(err.contains("too large"), "{err}");
    }

    #[test]
    fn preview_many_refuses_seventeen_files_with_one_huge_body_before_any_work() {
        let mut files: Vec<(String, String)> = (0..16)
            .map(|i| (format!("src/f{i}.rs"), "fn x() {}\n".into()))
            .collect();
        files.push(("src/huge.rs".into(), "x".repeat(MAX_PREVIEW_FILE_BYTES + 1)));
        let err = tauri::async_runtime::block_on(cmd_devmap_preview_many(
            "/no/such/repo".into(),
            files,
            None,
        ))
        .expect_err("over the cap");
        assert!(err.contains("too large"), "{err}");
    }

    #[test]
    fn write_file_content_refuses_an_oversized_buffer_at_the_boundary() {
        let huge = "x".repeat(MAX_CONTENT_BYTES + 1);
        let err = tauri::async_runtime::block_on(super::super::cmd_write_file_content(
            "/no/such/repo".into(),
            "src/lib.rs".into(),
            huge,
            None,
            None,
        ))
        .expect_err("over the cap");
        assert!(err.message().contains("too large"), "{err}");
    }

    /// A fake `devmap` whose `preview` takes a moment and logs each file it
    /// is asked about, so a test can stop the batch while it is mid-file.
    #[cfg(unix)]
    fn slow_logging_devmap(dir: &std::path::Path, log: &std::path::Path) -> std::path::PathBuf {
        use std::os::unix::fs::PermissionsExt;
        let path = dir.join("devmap");
        let script = format!(
            "#!/bin/sh
if [ \"$1\" = \"preview\" ]; then
  cat >/dev/null
  echo \"$*\" >> '{log}'
  sleep 1
  printf '%s\\n' '{{\"file_path\":\"x\",\"parse_status\":\"Clean\",\"delta_available\":true,\"file_is_indexed\":true,\"compared_against\":\"disk\",\"symbols\":[],\"bodies_not_compared\":0,\"ambiguous_callers\":0}}'
  exit 0
fi
exit 2
",
            log = log.display()
        );
        std::fs::write(&path, script).unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
        path
    }

    /// Cancelling the request's token mid-batch stops it after the file in
    /// flight: three files, the first running when the cancel lands, one
    /// `devmap` invocation, and an outcome that says `cancelled`.
    #[cfg(unix)]
    #[test]
    fn cancelling_the_token_stops_preview_many_within_one_file() {
        let _lock = crate::harness::sidecar::test_serial();
        crate::devmap::cli::clear_preview_debounces();
        let repo = crate::test_support::git_repo();
        let fixture = tempfile::TempDir::new().unwrap();
        let log = fixture.path().join("calls.log");
        let bin = slow_logging_devmap(fixture.path(), &log);
        let _bound = crate::devmap::cli::bind_test_binary(bin.to_string_lossy().into_owned());
        let files: Vec<(String, String)> = (0..3)
            .map(|i| (format!("src/f{i}.rs"), "fn x() {}\n".into()))
            .collect();
        let repo_path = repo.path().to_string_lossy().into_owned();

        let outcome = std::thread::scope(|scope| {
            let worker = scope.spawn(|| {
                preview_many_cancellable(&repo_path, &files, Some("preview-cancel-test".into()))
            });
            // The first file is running once its call is logged; the token is
            // registered before that.
            let started = std::time::Instant::now();
            while std::fs::read_to_string(&log).map_or(true, |text| text.lines().count() == 0) {
                assert!(
                    started.elapsed()
                        < crate::test_support::env::coverage_relaxed(
                            std::time::Duration::from_secs(30)
                        ),
                    "no preview started"
                );
                std::thread::sleep(std::time::Duration::from_millis(20));
            }
            assert!(crate::codeintel::cancel_query("preview-cancel-test"));
            worker.join().unwrap()
        });
        crate::devmap::cli::clear_preview_debounces();

        assert!(outcome.cancelled, "{:?}", outcome.reason);
        let calls = std::fs::read_to_string(&log).unwrap().lines().count();
        assert_eq!(calls, 1, "the batch stopped after the file in flight");
    }
}
