//! Application settings, tools, grants, provenance, cleaner and update commands.

use super::*;

/// Checks whether a newer GitPulse release has been published.
///
/// Opt-in at the call site: the frontend invokes this only when the user has
/// enabled the preference or pressed "Check now". Nothing here reaches the
/// network on its own.
///
/// Infallible by design — every failure arrives as an [`UpdateCheck`] with
/// `checked: false` and a reason, so a transport error can never be rendered
/// as "you are up to date".
///
/// `async fn` + [`off_thread`]: a sync body under `(async)` still runs on an
/// async runtime worker, and this one is a network `git ls-remote` that may
/// take its full timeout — holding a worker every other command needs.
#[tauri::command(async)]
pub async fn cmd_check_app_update() -> crate::updates::UpdateCheck {
    off_thread(|| Ok(crate::updates::check_for_update()))
        .await
        .unwrap_or_else(crate::updates::UpdateCheck::failed)
}

/// MCP 2.0 / Agent Plugins 1.0 installer facts: binary path, plugin manifests, tool catalog.
/// Off the main thread: it stats, canonicalizes and reads files, and a plain
/// `#[tauri::command]` runs on the GUI thread.
#[tauri::command(async)]
pub async fn cmd_mcp_info() -> Result<crate::insights::McpInfo, String> {
    off_thread(|| Ok(crate::insights::mcp_info())).await
}

/// Probe whether `devmap` / `manvi` are installed, which path answered, and
/// whether a sibling checkout can feed an in-app install.
#[tauri::command(async)]
pub async fn cmd_external_tools_status() -> Result<crate::tool_install::ToolsStatus, String> {
    off_thread(|| Ok(crate::tool_install::status_all())).await
}

/// Install or force-update `devmap` or `manvi` via the install ladder.
#[tauri::command(async)]
pub async fn cmd_external_tool_install(
    tool: crate::tool_install::ExternalTool,
    rung: Option<crate::tool_install::InstallRung>,
) -> Result<crate::tool_install::InstallOutcome, String> {
    off_thread(move || Ok(crate::tool_install::install_with_rung(tool, rung))).await
}

/// Cancel an in-flight [`cmd_external_tool_install`]. Best-effort: kills the
/// install process group when one is running.
#[tauri::command]
pub fn cmd_external_tool_install_cancel() {
    crate::tool_install::request_cancel();
}

/// Remove a GitPulse-owned binary (app bin dir only) and forget the saved path.
#[tauri::command(async)]
pub async fn cmd_external_tool_uninstall(
    tool: crate::tool_install::ExternalTool,
) -> Result<String, String> {
    off_thread(move || crate::tool_install::uninstall_tool(tool)).await
}

/// Hide or restore a tool without deleting it. Env overrides still win.
#[tauri::command(async)]
pub async fn cmd_external_tool_set_disabled(
    tool: crate::tool_install::ExternalTool,
    disabled: bool,
) -> Result<crate::tool_config::ToolConfigView, String> {
    off_thread(move || {
        crate::tool_install::set_tool_disabled(tool, disabled)?;
        crate::tool_config::view()
    })
    .await
}

#[tauri::command(async)]
pub async fn cmd_tool_config_get() -> Result<crate::tool_config::ToolConfigView, String> {
    off_thread(crate::tool_config::view).await
}

#[tauri::command(async)]
pub async fn cmd_tool_config_save(
    config: crate::tool_config::ToolConfig,
) -> Result<crate::tool_config::ToolConfigView, String> {
    off_thread(move || {
        crate::tool_config::save_confirmed(&config, &crate::desktop::confirm::native)?;
        crate::tool_config::view()
    })
    .await
}

/// What agent-session notifications did and did not do, plus the settings
/// that governed it.
///
/// One command answers both because the settings panel has to show them
/// together: "enabled, and nothing has been delivered" reads very differently
/// from "enabled, and nine were suppressed because you were watching".
#[tauri::command(async)]
pub async fn cmd_session_alerts() -> Result<SessionAlertsView, String> {
    off_thread(|| {
        Ok(SessionAlertsView {
            settings: crate::tool_config::session_alerts(),
            status: crate::alerts::status(),
            // Named here rather than assembled in the UI so the two spellings
            // of "which platform can do this" cannot drift apart.
            bridge_supported: cfg!(unix),
        })
    })
    .await
}

#[derive(serde::Serialize)]
pub struct SessionAlertsView {
    pub settings: crate::tool_config::SessionAlertSettings,
    pub status: crate::alerts::SessionAlertStatus,
    pub bridge_supported: bool,
}

/// What a terminal tab starts an agent CLI with, and which modes exist.
///
/// The mode list travels with the settings rather than being written down in
/// the frontend: a chooser that offered a mode this build cannot expand would
/// be offering a launch that fails at spawn.
#[tauri::command(async)]
pub async fn cmd_agent_defaults() -> Result<AgentDefaultsView, String> {
    off_thread(|| Ok(agent_defaults_view())).await
}

#[derive(serde::Serialize)]
pub struct AgentDefaultsView {
    pub defaults: crate::tool_config::AgentDefaults,
    /// Every permission mode, least authority first.
    pub modes: Vec<String>,
    /// Launchers that accept one. Strictly smaller than the tab strip's list:
    /// `shell` and `manvi` have no policy, and offering them a mode would
    /// produce a refusal at spawn rather than a setting.
    pub launchers: Vec<String>,
    /// The model fields each launcher takes, derived from the flag table so
    /// the panel never offers a control a launch would refuse.
    pub model_fields: std::collections::BTreeMap<String, Vec<String>>,
    /// Reasoning-effort levels, least first.
    pub effort_levels: Vec<String>,
    /// Launchers whose models GitPulse can list (`cmd_agent_models`), and
    /// how: `listed` by the CLI itself, or `known` from its aliases and the
    /// user's settings because the CLI publishes no list.
    pub model_listing: std::collections::BTreeMap<String, String>,
}

/// The stored defaults read back, with the vocabularies that describe them.
/// One builder for both the read and the save reply, so the two cannot differ.
pub(super) fn agent_defaults_view() -> AgentDefaultsView {
    use crate::workbench::terminal_command as command;
    AgentDefaultsView {
        defaults: crate::tool_config::agent_defaults(),
        modes: command::PERMISSION_MODES
            .iter()
            .map(|mode| (*mode).to_owned())
            .collect(),
        launchers: command::permission_launchers(),
        model_fields: command::model_launchers(),
        effort_levels: command::EFFORT_LEVELS
            .iter()
            .map(|level| (*level).to_owned())
            .collect(),
        model_listing: crate::workbench::agent_models::listing_launchers(),
    }
}

/// The models `launcher` can be started with, as its CLI says — offered as
/// suggestions by the model settings; the stored value stays free text.
///
/// Antigravity is asked with `agy models` (network, the user's sign-in, run
/// from the temporary directory, bounded, cached for ten minutes unless
/// `refresh`). Claude Code publishes no list, so its answer is its aliases
/// plus the models the user's own Claude settings name. A listing that failed
/// carries `error` beside an empty list rather than reading as no models.
#[tauri::command(async)]
pub async fn cmd_agent_models(
    launcher: String,
    refresh: Option<bool>,
) -> Result<crate::workbench::agent_models::AgentModelCatalog, String> {
    off_thread(move || {
        crate::workbench::agent_models::models_for(launcher.trim(), refresh.unwrap_or(false))
    })
    .await
}

#[tauri::command(async)]
pub async fn cmd_agent_defaults_save(
    terminals: State<'_, crate::terminal::TerminalSessions>,
    defaults: crate::tool_config::AgentDefaults,
) -> Result<AgentDefaultsView, String> {
    let terminals = terminals.inner().clone();
    off_thread(move || {
        crate::tool_config::set_agent_defaults(defaults)?;
        // Applied to the live registry here, so the next session obeys the
        // saved limit without a restart. Read back rather than taken from
        // the input, so the registry holds exactly what was stored.
        terminals.apply_stored_limit();
        Ok(agent_defaults_view())
    })
    .await
}

#[tauri::command(async)]
pub async fn cmd_session_alerts_save(
    settings: crate::tool_config::SessionAlertSettings,
) -> Result<SessionAlertsView, String> {
    off_thread(move || {
        let bridge = settings.hook_bridge && settings.enabled;
        crate::tool_config::set_session_alerts(settings)?;
        // Applied here rather than at the next launch: turning the bridge off
        // has to actually remove the socket, or "off" would mean "still bound,
        // ignoring you".
        crate::alerts::bridge::apply(bridge);
        Ok(SessionAlertsView {
            settings: crate::tool_config::session_alerts(),
            status: crate::alerts::status(),
            bridge_supported: cfg!(unix),
        })
    })
    .await
}

/// The two Lappi switches, with what recording has done this process.
///
/// Both default to off. The view travels with the switches for the same
/// reason session alerts do: "recording on, nothing written" and "recording
/// on, stopped at the store cap" look the same from a switch alone.
#[tauri::command(async)]
pub async fn cmd_lappi_settings() -> Result<crate::lappi::LappiView, String> {
    off_thread(|| Ok(crate::lappi::view())).await
}

#[tauri::command(async)]
pub async fn cmd_lappi_settings_save(
    settings: crate::tool_config::LappiSettings,
) -> Result<crate::lappi::LappiView, String> {
    off_thread(move || {
        crate::tool_config::set_lappi_settings(settings)?;
        Ok(crate::lappi::view())
    })
    .await
}

/// Host load as this app sees it: the sensor state with the last-known
/// pressure and its age, the readings, and — when this process writes the
/// snapshot — its sessions, launched trees, memguard and orphans
/// (`docs/LOAD_GOVERNOR.md`).
#[tauri::command(async)]
pub async fn cmd_host_load_status() -> Result<crate::sysload::HostLoadStatus, String> {
    off_thread(|| Ok(crate::sysload::status())).await
}

/// Sends SIGTERM to a foreign orphan Health listed, after the person clicked
/// Terminate on it and then answered Yes in a native dialog the webview cannot
/// click. Every condition is re-checked on the live process before and after
/// that dialog (`sysload::terminate_orphan`); anything changed refuses.
/// `start_us` is microseconds since the epoch, well inside a JavaScript
/// number's exact range.
#[tauri::command(async)]
pub async fn cmd_host_load_terminate_orphan(pid: i32, start_us: u64) -> Result<String, String> {
    off_thread(move || {
        crate::sysload::terminate_orphan(pid, start_us, &crate::desktop::confirm::native)
    })
    .await
}

/// A person's "Admit anyway" on a refused agent launch: after a Yes in a
/// native dialog that page script cannot click, launches pass for two
/// minutes (`sysload::admit_anyway`). There is no agent path to this.
#[tauri::command(async)]
pub async fn cmd_host_load_admit_anyway() -> Result<String, String> {
    off_thread(|| crate::sysload::admit_anyway(&crate::desktop::confirm::native)).await
}

/// The host-load kill switch, as stored.
#[tauri::command(async)]
pub async fn cmd_host_load_settings() -> Result<crate::tool_config::HostLoadSettings, String> {
    off_thread(|| Ok(crate::tool_config::host_load_settings())).await
}

/// Stores the kill switch; the sampler publishes it on its next tick.
#[tauri::command(async)]
pub async fn cmd_host_load_settings_save(
    settings: crate::tool_config::HostLoadSettings,
) -> Result<crate::tool_config::HostLoadSettings, String> {
    off_thread(move || {
        crate::tool_config::set_host_load_settings(settings)?;
        Ok(crate::tool_config::host_load_settings())
    })
    .await
}

/// Whether this repository posts verification results to GitHub. Off until
/// turned on; an unreadable setting is an error, never a yes.
#[tauri::command(async)]
pub async fn cmd_verify_status_enabled(repo_path: String) -> Result<bool, String> {
    off_thread(move || crate::workbench::verify_status::enabled_for(&repo_path)).await
}

#[tauri::command(async)]
pub async fn cmd_verify_status_save(repo_path: String, enabled: bool) -> Result<(), String> {
    off_thread(move || crate::workbench::verify_status::set_enabled(&repo_path, enabled)).await
}

/// Posts a task's verification verdict to GitHub as a commit status (and
/// optionally a PR comment) through `gh`. Only a person's explicit action
/// reaches this: it is a desktop command, and no MCP tool calls it. Refused
/// unless the repository opted in; `dry_run` returns what would be sent.
#[tauri::command(async)]
pub async fn cmd_verify_status_post(
    repo_path: String,
    task_id: String,
    comment: bool,
    dry_run: bool,
    workbench: State<'_, crate::workbench::WorkbenchState>,
) -> Result<crate::workbench::verify_status::PostedVerifyStatus, String> {
    let workbench = workbench.inner().clone();
    off_thread(move || {
        crate::workbench::verify_status::post_via_app(
            &workbench, &repo_path, &task_id, comment, dry_run,
        )
    })
    .await
}

/// One agent attempt's review, for its task panel (`docs/AGENT_OUTPUT_REVIEW.md`).
#[tauri::command(async)]
pub async fn cmd_attempt_review(
    run_id: String,
    workbench: State<'_, crate::workbench::WorkbenchState>,
) -> Result<crate::workbench::review::AttemptReview, String> {
    let workbench = workbench.inner().clone();
    off_thread(move || crate::workbench::review::attempt_review(&workbench, &run_id, None)).await
}

/// Records a person's decision on an ended attempt's current commit range:
/// `approve`, `request_changes`, `deny`, or `merge_unreviewed` (which needs a
/// note saying why).
#[tauri::command(async)]
pub async fn cmd_attempt_review_record(
    run_id: String,
    decision: String,
    note: Option<String>,
    workbench: State<'_, crate::workbench::WorkbenchState>,
) -> Result<crate::workbench::review::AttemptReview, String> {
    let workbench = workbench.inner().clone();
    off_thread(move || {
        crate::workbench::review::attempt_review(
            &workbench,
            &run_id,
            Some((&decision, note.as_deref())),
        )
    })
    .await
}

/// Turns the review gate on or off for the repository `repo_path` belongs to.
#[tauri::command(async)]
pub async fn cmd_review_gate_save(repo_path: String, enabled: bool) -> Result<(), String> {
    off_thread(move || {
        let dir = crate::harness::review::common_dir(std::path::Path::new(&repo_path))?;
        crate::tool_config::set_review_gate(Some(&dir), Some(enabled))
    })
    .await
}

/// Reports which terminal sessions the user can actually see.
///
/// Plural because a split terminal shows two at once. Only the renderer knows
/// which tabs are on screen; whether the *window* has focus is read natively
/// at decision time, so a renderer that stops running — the case in which a
/// notification matters most — cannot leave a session looking attended.
#[tauri::command(async)]
pub async fn cmd_session_alerts_visible(session_ids: Vec<String>) -> Result<(), String> {
    crate::alerts::set_visible_sessions(session_ids);
    Ok(())
}

/// What every agent session stands asking for right now.
///
/// The renderer hears attention as events, and an event emitted while no page
/// was listening — before the listener bound, or across a window reload — is
/// gone. A page reads this once it is listening, so an agent that asked for
/// permission while the window reloaded is still shown asking.
#[tauri::command(async)]
pub async fn cmd_session_attention_standing(
) -> Result<Vec<crate::alerts::StandingAttention>, String> {
    Ok(crate::alerts::standing())
}

/// The user typed into a session that was asking for something.
///
/// Tells the notifier, which owns what stands: without this, an answered
/// question was replayed to the next page, and its banner stayed in the
/// notification centre asking it.
#[tauri::command(async)]
pub async fn cmd_session_attention_answered(session_id: String) -> Result<(), String> {
    if crate::alerts::answered(&session_id) {
        Ok(())
    } else {
        Err("Not a terminal session id.".into())
    }
}

#[tauri::command(async)]
pub async fn cmd_tool_ladder(
    tool: crate::tool_install::ExternalTool,
) -> Result<crate::tool_install::LadderAssessment, String> {
    off_thread(move || Ok(crate::tool_install::assess_ladder(tool))).await
}

#[tauri::command(async)]
pub async fn cmd_tool_preflight(
    tool: crate::tool_install::ExternalTool,
) -> Result<crate::tool_install::PreflightReport, String> {
    off_thread(move || Ok(crate::tool_install::preflight(tool))).await
}

#[tauri::command(async)]
pub async fn cmd_tool_verify(
    tool: crate::tool_install::ExternalTool,
) -> Result<crate::tool_install::VerifyReport, String> {
    off_thread(move || Ok(crate::tool_install::verify_connected(tool))).await
}

#[tauri::command(async)]
pub async fn cmd_onboarding_clone_source(
    tool: crate::tool_install::ExternalTool,
    parent_dir: String,
) -> Result<crate::tool_install::CloneSourceOutcome, String> {
    off_thread(move || Ok(crate::tool_install::clone_source(tool, &parent_dir))).await
}

#[tauri::command]
pub fn cmd_tool_capability_refresh() {
    crate::tool_capability::invalidate_all();
}

/// The harness's grant ledger for this repository.
///
/// Read-only, and a file read rather than a protocol op: the serve plane holds
/// no grant ledger, so an op there could only ever answer "none". Revocation is
/// deliberately absent — it mutates state Manvi owns, and a second writer could
/// interleave with the harness's own serialised writes.
#[tauri::command(async)]
pub async fn cmd_grants_view(repo_path: String) -> Result<crate::grants::GrantView, String> {
    off_thread(move || Ok(crate::grants::view(&repo_path))).await
}

/// Discovers the model servers running on this machine.
///
/// Separate from `cmd_ai_status`, which reports the *selected* model. This
/// answers "what could I select", which the AI settings had no way to ask: the
/// harness's capability probe requires a base URL and a model, which is the
/// answer rather than the question.
#[tauri::command(async)]
pub async fn cmd_local_scan() -> Result<crate::harness::ScanResult, String> {
    off_thread(crate::ai::scan_local_servers).await
}

// --- provenance freshness ---------------------------------------------

/// Provenance freshness for one commit.
///
/// A verification note is a claim about a tree at a moment. This says how far
/// the world has moved since — and, when it cannot say, says that instead. The
/// `distance`/`confidence` pair is `null` rather than zero on every failure:
/// zero is the strongest claim the type can make, and handing it out because
/// git could not answer is how a stale badge comes to read as a fresh one.
#[tauri::command(async)]
pub async fn cmd_provenance_freshness(
    repo_path: String,
    commit_sha: String,
    base_branch: Option<String>,
) -> Result<crate::engine::ProvenanceFreshness, String> {
    let repo = validate_repo(&repo_path)?;
    off_thread(move || {
        Ok(crate::engine::provenance::compute_freshness(
            &repo.to_string_lossy(),
            &commit_sha,
            base_branch.as_deref(),
        ))
    })
    .await
}

/// Provenance freshness for many revisions at once, in input order.
///
/// The branch list and the pull-request list each want a badge per row, and a
/// per-row round trip would be a subprocess per branch. The batch resolves
/// every revision in one `git cat-file`, reads each notes ref once, and only
/// then measures — and only the commits that carry a note, because a commit
/// nobody verified has no verification whose age could decay.
///
/// Accepts ref names as well as shas: pull requests arrive as `head_ref`.
#[tauri::command(async)]
pub async fn cmd_provenance_freshness_batch(
    repo_path: String,
    revisions: Vec<String>,
    base_branch: Option<String>,
) -> Result<Vec<crate::engine::ProvenanceFreshness>, String> {
    let repo = validate_repo(&repo_path)?;
    off_thread(move || {
        Ok(crate::engine::provenance::freshness_batch(
            &repo.to_string_lossy(),
            &revisions,
            base_branch.as_deref(),
        ))
    })
    .await
}

#[tauri::command]
pub async fn cmd_cleaner_state() -> Result<crate::storage::hygiene::global::CleanerState, String> {
    off_thread(|| crate::storage::hygiene::global::service()?.state()).await
}

#[tauri::command]
pub async fn cmd_cleaner_save(
    config: crate::storage::hygiene::global::CleanerConfig,
) -> Result<crate::storage::hygiene::global::CleanerState, String> {
    off_thread(move || crate::storage::hygiene::global::service()?.save(config)).await
}

#[tauri::command]
pub async fn cmd_cleaner_scan() -> Result<crate::storage::hygiene::global::CleanerInventory, String>
{
    off_thread(|| crate::storage::hygiene::global::service()?.inventory()).await
}

#[tauri::command]
pub async fn cmd_cleaner_run(
    revision: u64,
) -> Result<crate::storage::hygiene::global::CleanerState, String> {
    off_thread(move || {
        crate::storage::hygiene::global::service()?.start(
            false,
            revision,
            crate::storage::hygiene::global::current_time(),
        )
    })
    .await
}

#[tauri::command]
pub async fn cmd_cleaner_cancel() -> Result<crate::storage::hygiene::global::CleanerState, String> {
    off_thread(|| crate::storage::hygiene::global::service()?.cancel()).await
}
