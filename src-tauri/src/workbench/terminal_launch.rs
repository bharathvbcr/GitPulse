//! Native preparation for a task-linked terminal attempt. The canonical store
//! owns the snapshot and capacity transaction; this adapter supplies observed
//! checkout identity. Preparation neither spawns a process nor grants access.

use super::{query, WorkbenchError, WorkbenchState};
use crate::engine::git_cli::{git_captured, resolve_git_common_dir, resolve_git_dir, resolve_repo};
use serde::Deserialize;
use serde_json::{json, Value};
use std::path::Path;

/// The revision every prepared attempt has. The store refuses to revise a
/// preparation ("a launch attempt is immutable; prepare a new run ID"), so a
/// claim or a managed preparation always names this one, and naming it — not
/// the row's current revision — is what keeps a retried request identical
/// to the original after the claim moved the row on.
pub(super) const PREPARED_REVISION: u64 = 1;

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Preparation {
    id: String,
    request_id: String,
    task_id: String,
    source_revision: u64,
    repository_id: String,
    repository_revision: u64,
    provider: String,
    permission_mode: String,
    #[serde(default)]
    acknowledge_bypass: bool,
    repo_path: String,
    /// Give this attempt a fresh linked worktree branched from `repo_path`,
    /// instead of running in `repo_path` itself.
    #[serde(default)]
    worktree: bool,
    /// This launch's model, laid over the saved default for the provider;
    /// the result is recorded on the attempt and is what its launch applies.
    #[serde(default)]
    model_choice: Option<crate::tool_config::ModelChoice>,
}

fn parse(input: &str) -> Result<Preparation, WorkbenchError> {
    if input.len() > 16_384 || !input.trim_start().starts_with('{') {
        return Err(WorkbenchError::new(
            "invalid_input",
            "Terminal preparation must be a JSON object of at most 16 KiB.",
        ));
    }
    // Struct deserialization rejects duplicate and unknown fields. Validate all
    // user-controlled metadata before filesystem probing or database creation.
    let request: Preparation = serde_json::from_str(input)
        .map_err(|e| WorkbenchError::new("invalid_input", e.to_string()))?;
    let ids = [
        &request.id,
        &request.request_id,
        &request.task_id,
        &request.repository_id,
    ];
    if ids.iter().any(|id| {
        id.is_empty()
            || id.len() > 128
            || !id
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-')
    }) || [request.source_revision, request.repository_revision]
        .iter()
        .any(|r| *r == 0 || *r > 9_007_199_254_740_991)
        || !super::terminal_command::is_terminal_provider(&request.provider)
        || ![
            "inspect",
            "ask",
            "edit",
            "auto_review",
            "preapproved",
            "bypass",
        ]
        .contains(&request.permission_mode.as_str())
        || (request.permission_mode == "bypass") != request.acknowledge_bypass
        || !valid_path(&request.repo_path)
    {
        return Err(WorkbenchError::new("invalid_input", "Terminal preparation requires valid identities, exact revisions, a supported mode and an absolute checkout path. Bypass acknowledgment applies only to bypass mode."));
    }
    Ok(request)
}

fn valid_path(path: &str) -> bool {
    path.len() <= 4096 && Path::new(path).is_absolute() && !path.chars().any(char::is_control)
}

fn unavailable(message: impl Into<String>) -> WorkbenchError {
    WorkbenchError::new("repository_unavailable", message)
}

fn native_path(path: &Path) -> Result<String, WorkbenchError> {
    path.to_str()
        .filter(|s| valid_path(s))
        .map(String::from)
        .ok_or_else(|| unavailable("The resolved checkout path is unsupported or exceeds 4 KiB."))
}

fn observed(repo: &Path, args: &[&str]) -> Result<(i32, String), WorkbenchError> {
    let output = git_captured(repo, args)
        .and_then(|r| r.require_complete("Terminal checkout probe"))
        .map_err(unavailable)?;
    if output.cancelled {
        return Err(unavailable("Checkout validation was cancelled."));
    }
    let text = String::from_utf8(output.stdout)
        .map_err(|_| unavailable("Git returned an invalid Unicode checkout identity."))?;
    // Remove Git's line ending, not meaningful whitespace in a native path.
    Ok((
        output.status_code,
        text.trim_end_matches(['\r', '\n']).to_owned(),
    ))
}

fn head(repo: &Path) -> Result<Option<String>, WorkbenchError> {
    let (status, oid) = observed(repo, &["rev-parse", "--verify", "--quiet", "HEAD^{commit}"])?;
    if status == 0 && [40, 64].contains(&oid.len()) && oid.bytes().all(|b| b.is_ascii_hexdigit()) {
        return Ok(Some(oid));
    }
    if status == 1 {
        // A failed HEAD lookup alone does not establish an unborn branch. Its
        // symbolic branch must be valid and demonstrably absent from the refs.
        let (symbolic_status, branch) = observed(repo, &["symbolic-ref", "--quiet", "HEAD"])?;
        if symbolic_status == 0 && branch.starts_with("refs/heads/") {
            let (ref_status, _) = observed(repo, &["show-ref", "--verify", "--quiet", &branch])?;
            if ref_status == 1 {
                return Ok(None);
            }
        }
    }
    Err(unavailable(
        "Cannot establish a commit or an unborn branch for the selected checkout.",
    ))
}

#[derive(Deserialize, Debug, PartialEq, Eq)]
struct Checkout {
    cwd: String,
    git_dir: String,
    git_common_dir: String,
    head_oid: Option<String>,
    head_ref: Option<String>,
}

/// The observed identity of the checkout holding `repo_path`, which may be
/// the checkout's root or a directory inside it. Everything Git is asked is
/// asked of the root, and the root is what must be trusted; `cwd` is the
/// directory itself, canonical, where the agent will start.
fn checkout(repo_path: &str) -> Result<Checkout, WorkbenchError> {
    let (dir, root, _) = crate::terminal::checkout_root(repo_path).map_err(unavailable)?;
    let resolved = resolve_repo(&native_path(&root)?).map_err(unavailable)?;
    if resolved.is_bare {
        return Err(unavailable(
            "A terminal task requires a working checkout; this repository is bare.",
        ));
    }
    let root = Path::new(&resolved.path);
    let cwd_text = native_path(&dir)?;
    let (status, top) = observed(root, &["rev-parse", "--show-toplevel"])?;
    if status != 0
        || Path::new(&top)
            .canonicalize()
            .map_err(|e| unavailable(e.to_string()))?
            != root
    {
        return Err(unavailable(
            "Git does not report this directory's checkout as its working tree.",
        ));
    }
    let git_dir = native_path(&resolve_git_dir(root).map_err(unavailable)?)?;
    let git_common_dir = native_path(&resolve_git_common_dir(root).map_err(unavailable)?)?;
    let head_oid = head(root)?;
    let (status, branch) = observed(root, &["symbolic-ref", "--quiet", "HEAD"])?;
    let head_ref = match status {
        0 if branch.starts_with("refs/heads/") => Some(branch),
        1 if head_oid.is_some() => None,
        _ => return Err(unavailable("Cannot establish the checkout branch.")),
    };
    Ok(Checkout {
        cwd: cwd_text,
        git_dir,
        git_common_dir,
        head_oid,
        head_ref,
    })
}

/// `runs.checkout_identity`: the Git identity of the checkout holding a
/// directory, by the same probe a preparation records.
///
/// The handoff form asks which attempt already holds the checkout it is about
/// to launch in. The store refuses by `git_dir`, and an attempt's recorded
/// directory may be a subfolder of its checkout, so only this key lets the
/// form agree with the store before Launch. Read-only, and gated exactly as
/// `runs.prepare_terminal` is: it runs Git only in a trusted checkout.
pub(super) fn checkout_identity(input: &str) -> Result<Value, WorkbenchError> {
    #[derive(Deserialize)]
    #[serde(deny_unknown_fields)]
    struct Ask {
        repo_path: String,
    }
    if input.len() > 8_192 || !input.trim_start().starts_with('{') {
        return Err(WorkbenchError::new(
            "invalid_input",
            "A checkout identity request must be a JSON object of at most 8 KiB.",
        ));
    }
    let ask: Ask = serde_json::from_str(input)
        .map_err(|e| WorkbenchError::new("invalid_input", e.to_string()))?;
    if !valid_path(&ask.repo_path) {
        return Err(WorkbenchError::new(
            "invalid_input",
            "A checkout identity needs an absolute path of at most 4 KiB.",
        ));
    }
    let observed = checkout(&ask.repo_path)?;
    Ok(json!({
        "ok": true,
        "cwd": observed.cwd,
        "git_dir": observed.git_dir,
        "git_common_dir": observed.git_common_dir,
    }))
}

pub(super) fn prepare(state: &WorkbenchState, input: &str) -> Result<Value, WorkbenchError> {
    prepare_kind(state, input, "external_terminal")
}

pub(super) fn prepare_managed(
    state: &WorkbenchState,
    input: &str,
) -> Result<Value, WorkbenchError> {
    prepare_kind(state, input, "managed")
}

/// Refuses a managed provider the *installed* harness has no adapter for,
/// before any attempt is stored.
///
/// Three source lists already agree about which providers may take this lane —
/// the renderer, this workbench, and the harness's own `ManagedProviders`. All
/// three describe source. None of them describes the binary on this machine,
/// and that is the gap this closes: a harness built before Claude Code gained
/// an adapter passes every source check, then refuses the launch in words
/// written for the providers it *does* have ("requires a fresh Codex
/// attempt"), after the attempt has already taken the repository's capacity.
///
/// A harness that did not publish its adapter set is *not* refused. Older
/// builds drive Codex perfectly well, and turning "could not check" into "no"
/// would break them; the launch proceeds and any failure is attributed
/// honestly instead (see `managed_run::launch_with`). A handshake that could
/// not be reached at all is likewise not a refusal — a busy or restarting
/// sidecar is a transport condition, not a verdict about adapters.
fn managed_adapter_gate(state: &WorkbenchState, provider: &str) -> Result<(), WorkbenchError> {
    use crate::harness::protocol::ManagedAdapter;
    let Ok(hello) = state.worker_handshake() else {
        return Ok(());
    };
    match hello.managed_adapter(provider) {
        ManagedAdapter::Present | ManagedAdapter::Unknown { .. } => Ok(()),
        ManagedAdapter::Absent { published } => {
            // Bounded before rendering. `published` is whatever the harness
            // sent, and this string ends up in a toast: a build advertising
            // thousands of adapters — or a corrupted line — must not become a
            // megabyte of error text on its way to the screen.
            const SHOWN: usize = 8;
            let has = if published.is_empty() {
                "no managed adapters".to_owned()
            } else {
                let names = published
                    .iter()
                    .take(SHOWN)
                    .map(|p| p.chars().take(32).collect::<String>())
                    .collect::<Vec<_>>()
                    .join(", ");
                match published.len().checked_sub(SHOWN) {
                    Some(rest) if rest > 0 => format!("only: {names} (and {rest} more)"),
                    _ => format!("only: {names}"),
                }
            };
            Err(WorkbenchError::new(
                "unsupported_operation",
                format!(
                    "The installed Manvi has no managed adapter for {provider}; it reports {has}. \
                     Update Manvi, or hand this task to {provider} in a terminal instead."
                ),
            ))
        }
    }
}

fn prepare_kind(state: &WorkbenchState, input: &str, kind: &str) -> Result<Value, WorkbenchError> {
    let input = parse(input)?;
    if kind == "managed"
        && (!super::terminal_command::is_managed_provider(&input.provider) || input.id.len() > 100)
    {
        return Err(WorkbenchError::new(
            "unsupported_operation",
            "Managed launches require Codex or Claude Code and an attempt identity of at most 100 characters.",
        ));
    }
    // Held across the whole build, hook included, so a retry of this attempt
    // never meets a tree that is still being set up — and so a cancel cannot
    // reclaim it halfway. Taken before the lookup below, so a retry during
    // setup is told so rather than handed an attempt still being made.
    let _place = if input.worktree {
        Some(
            state
                .0
                .preparations
                .try_enter(&input.id)
                .map_err(|_| setting_up())?,
        )
    } else {
        None
    };
    // A retry of a preparation the store already committed is answered with
    // that attempt, before anything the host reads now — limits, the saved
    // model, HEAD — can make the request differ from the one it committed.
    if let Some(prepared) = own_prepared(state, &input, kind)? {
        return Ok(prepared);
    }
    if kind == "managed" {
        managed_adapter_gate(state, &input.provider)?;
    }
    let model_choice = recorded_model_choice(&input, kind)?;
    // Two retries can both find nothing above and both send; the store
    // commits one and refuses the other, whose host-read fields differ, as
    // an idempotency conflict. That loser's attempt is the winner's.
    let in_place = || {
        prepare_in(state, &input, kind, &input.repo_path, &model_choice).or_else(|error| {
            if error.code != "idempotency_conflict" {
                return Err(error);
            }
            own_prepared(state, &input, kind)?.ok_or(error)
        })
    };
    if !input.worktree && kind != "managed" {
        return in_place();
    }
    let (_, root, inside) =
        crate::terminal::checkout_root(&input.repo_path).map_err(unavailable)?;
    if kind == "managed" {
        if let Some(inside) = inside {
            // Manvi is handed one directory as the provider's workspace and
            // sandbox; nothing here shows it accepts one below the checkout
            // root, so this is refused rather than tried.
            return Err(WorkbenchError::new(
                "unsupported_operation",
                format!(
                    "A managed agent works in its whole checkout, not in {inside}. Choose the checkout itself, or hand this task to an agent in a terminal to start it there."
                ),
            ));
        }
        if !input.worktree {
            return in_place();
        }
    }
    // Nothing prepared under this id (`own_prepared` answered every other
    // case), so this attempt needs room and a tree of its own.
    ensure_room(state)?;
    let task = state
        .with_store(|store| query(store, "items.get", &json!({"id":input.task_id}).to_string()))?;
    let title = task["item"]["title"].as_str().unwrap_or_default();
    let provisioned =
        super::agent_worktree::provision(&native_path(&root)?, &input.id, title, false)?;
    // The same folder, in the new tree: an attempt chosen for a package of a
    // monorepo works in that package of its own worktree. A folder the new
    // branch does not have (untracked here) is refused by `checkout`, and
    // the tree goes with the refusal.
    let cwd = match &inside {
        Some(inside) => native_path(&Path::new(&provisioned.path).join(inside))?,
        None => provisioned.path.clone(),
    };
    let prepared = prepare_in(state, &input, kind, &cwd, &model_choice);
    if let Err(refusal) = prepared {
        // The attempt does not exist, so neither may the worktree made for it.
        return Err(match super::agent_worktree::discard(&provisioned) {
            Ok(()) => refusal,
            Err(cleanup) => WorkbenchError::new(
                &refusal.code,
                format!(
                    "{} The worktree made for it could not be removed and is still at {} on branch {}: {cleanup}",
                    refusal.message, provisioned.path, provisioned.branch
                ),
            ),
        });
    }
    prepared
}

/// The attempt this request already committed, when one exists under its id.
///
/// An attempt id is minted fresh for one preparation and the store never
/// prepares an id twice ("a launch attempt is immutable"), so a run under
/// this id was made by this preparation — provided every field the renderer
/// owns agrees, which [`replay_verdict`] checks. `None` when there is none.
fn own_prepared(
    state: &WorkbenchState,
    input: &Preparation,
    kind: &str,
) -> Result<Option<Value>, WorkbenchError> {
    match state.with_store(|store| query(store, "runs.get", &json!({"id":input.id}).to_string())) {
        Ok(saved) => {
            let now = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|elapsed| elapsed.as_secs())
                .unwrap_or_default();
            replay_verdict(&saved, input, kind, now).map(Some)
        }
        Err(error) if error.code == "not_found" => Ok(None),
        Err(error) => Err(error),
    }
}

/// What a retry is answered with, given the attempt saved under its id.
///
/// The saved attempt in the shape a preparation answers with (no brief), when
/// every renderer-owned field matches and it is still launchable. A mismatch
/// is a different request under a reused id, and is refused rather than
/// answered with someone else's attempt; an attempt that has moved on or
/// expired (at `expires_at`, as the store counts it) is not handed back as
/// launchable. Host-read fields — limits, model, checkout — are not compared:
/// they are what may honestly have changed since.
fn replay_verdict(
    saved: &Value,
    input: &Preparation,
    kind: &str,
    now: u64,
) -> Result<Value, WorkbenchError> {
    let item = &saved["item"];
    let same = item["task_id"] == input.task_id
        && item["source_revision"] == input.source_revision
        && item["repository_id"] == input.repository_id
        && item["provider"] == input.provider
        && item["permission_mode"] == input.permission_mode
        && item["bypass_acknowledged"] == input.acknowledge_bypass
        && item["kind"] == kind;
    if !same {
        return Err(WorkbenchError::new(
            "invalid_state",
            "This attempt ID already names a different preparation. Prepare a new attempt.",
        ));
    }
    if item["state"] != "prepared" {
        // Building a tree, or answering "prepared", for an attempt that has
        // ended would only offer a launch the store will refuse.
        return Err(WorkbenchError::new(
            "launch_consumed",
            format!(
                "This attempt is already {}. Prepare a new attempt.",
                item["state"].as_str().unwrap_or("over")
            ),
        ));
    }
    if item["expires_at"]
        .as_u64()
        .is_none_or(|expires| expires <= now)
    {
        return Err(WorkbenchError::new(
            "launch_consumed",
            "This attempt expired before its preparation was confirmed. Prepare a new attempt.",
        ));
    }
    let mut item = item.clone();
    if let Some(fields) = item.as_object_mut() {
        fields.remove("brief");
    }
    Ok(json!({"ok": true, "item": item}))
}

/// The model choice this attempt records: the saved default with this
/// launch's override laid over it, in the form the store keeps.
///
/// A pure function of the request and the saved default, so `prepare_kind`
/// answers it before building anything — a worktree's setup hook can take
/// minutes, and a refusal after it threw that work away. A managed agent's
/// model is Manvi's to set, not these flags: an override there would be
/// recorded on the attempt and never applied.
fn recorded_model_choice(input: &Preparation, kind: &str) -> Result<Value, WorkbenchError> {
    if kind == "managed" {
        if input.model_choice.is_some() {
            return Err(WorkbenchError::new(
                "unsupported_operation",
                "A model for one launch applies to agents started in a terminal; a managed agent uses Manvi's model.",
            ));
        }
        return Ok(Value::Null);
    }
    let defaults = crate::tool_config::agent_defaults();
    let choice = super::terminal_command::launch_model_choice(
        &input.provider,
        defaults.model_for(&input.provider),
        input.model_choice.as_ref(),
    )
    .map_err(|message| WorkbenchError::new("invalid_input", message))?;
    match choice {
        Some(choice) => super::terminal_command::model_choice_record(&choice)
            .map_err(|message| WorkbenchError::new("invalid_input", message)),
        None => Ok(Value::Null),
    }
}

fn prepare_in(
    state: &WorkbenchState,
    input: &Preparation,
    kind: &str,
    repo_path: &str,
    model_choice: &Value,
) -> Result<Value, WorkbenchError> {
    let checkout = checkout(repo_path)?;
    let body = json!({
        "model_choice": model_choice,
        "id":input.id, "request_id":input.request_id, "expected_revision":0,
        "kind":kind,
        "task_id":input.task_id, "source_revision":input.source_revision,
        "repository_id":input.repository_id, "repository_revision":input.repository_revision,
        "provider":input.provider, "permission_mode":input.permission_mode,
        "acknowledge_bypass":input.acknowledge_bypass, "cwd":checkout.cwd,
        "git_dir":checkout.git_dir, "git_common_dir":checkout.git_common_dir,
        "head_oid":checkout.head_oid, "head_ref":checkout.head_ref,
        // The user's choice, read at each launch so a change in Settings
        // applies to the next one. The store owns the bound and the count.
        "max_active_runs": state.live_runs(),
    });
    let prepare = || state.with_store(|store| query(store, "runs.prepare", &body.to_string()));
    capacity_hint(match prepare() {
        // The slot may be held by an attempt whose owner died. Release what
        // the evidence allows, once, and ask again with the identical request;
        // a refusal is rolled back, so the same request_id is still unused.
        Err(error) if matches!(error.code.as_str(), "checkout_busy" | "capacity_reached") => {
            match super::reconcile::sweep(state) {
                Ok(released) if released > 0 => prepare(),
                Ok(_) => Err(error),
                Err(sweep) => {
                    log::warn!(target: "workbench", "run reconciliation before a launch failed: {}: {}", sweep.code, sweep.message);
                    Err(error)
                }
            }
        }
        other => other,
    })
}

fn setting_up() -> WorkbenchError {
    WorkbenchError::new(
        "busy",
        "This attempt is still setting up its worktree; the repository's setup hook can take several minutes. Wait for it to finish rather than starting it again.",
    )
}

/// Refuses a preparation the profile has no room for, before a worktree is
/// built for it: the store only counts at the very end, after `git worktree
/// add` and the whole `post_create` hook. Read-only, with the same one
/// release of provably stranded attempts the store refusal gets; the store
/// still decides, since this count can only be low.
fn ensure_room(state: &WorkbenchState) -> Result<(), WorkbenchError> {
    let limit = state.live_runs();
    if super::reconcile::live_count(state)? < u64::from(limit) {
        return Ok(());
    }
    match super::reconcile::sweep(state) {
        Ok(released) if released > 0 => {
            if super::reconcile::live_count(state)? < u64::from(limit) {
                return Ok(());
            }
        }
        Ok(_) => {}
        Err(sweep) => {
            log::warn!(target: "workbench", "run reconciliation before a launch failed: {}: {}", sweep.code, sweep.message);
        }
    }
    capacity_hint(Err(WorkbenchError::new(
        "capacity_reached",
        // The store's sentence (dc-store `runs::prepare`), word for word; a
        // test holds the two together.
        format!(
            "{limit} runs are already prepared, active or unresolved; finish or reconcile one before launching another"
        ),
    )))
    .map(|_| ())
}

/// `runs.cancel`, and then the worktree made for the attempt, when it was
/// never claimed: the store's cancel is unchanged and decides first; only
/// once it succeeded is the tree given back (`agent_worktree::reclaim`,
/// never forced). What happened to it is the optional `worktree` field
/// beside `item`; it is absent when the attempt had no worktree of its own.
pub(super) fn cancel(state: &WorkbenchState, input: &str) -> Result<Value, WorkbenchError> {
    let id = serde_json::from_str::<Value>(input)
        .ok()
        .and_then(|request| request["id"].as_str().map(str::to_owned))
        .filter(|id| !id.is_empty() && id.len() <= 128);
    let Some(id) = id else {
        // Malformed: the store's own validation says how.
        return state.with_store(|store| query(store, "runs.cancel", input));
    };
    let _place = state
        .0
        .preparations
        .try_enter(&id)
        .map_err(|_| setting_up())?;
    let before = state
        .with_store(|store| query(store, "runs.get", &json!({"id":id}).to_string()))
        .ok();
    let mut cancelled = state.with_store(|store| query(store, "runs.cancel", input))?;
    let unclaimed = before.is_some_and(|before| {
        before["item"]["state"] == "prepared" && before["item"]["owner_id"].is_null()
    });
    if unclaimed && cancelled["item"]["state"] == "cancelled" {
        let open_files = |path: &Path| state.open_files(path);
        if let Some(outcome) = super::agent_worktree::reclaim(&cancelled["item"], &open_files) {
            log::info!(target: "workbench", "cancelled attempt {id}: {}", outcome.describe());
            cancelled["worktree"] = outcome.to_json();
        }
    }
    Ok(cancelled)
}

/// Says where the limit is set when the profile is full. The store's own
/// sentence names the number in force but not that the reader chose it, so a
/// refusal at the default read as a wall rather than a setting.
fn capacity_hint(result: Result<Value, WorkbenchError>) -> Result<Value, WorkbenchError> {
    result.map_err(|error| {
        if error.code != "capacity_reached" {
            return error;
        }
        let max = crate::tool_config::MAX_LIVE_RUNS;
        WorkbenchError::new(
            &error.code,
            format!(
                "{} To run more at once, raise \"Agents running at once\" in Settings → Agents (up to {max}).",
                error.message
            ),
        )
    })
}

pub(super) fn revalidate(saved: &Value) -> Result<(), WorkbenchError> {
    let recorded: Checkout = serde_json::from_value(saved["item"].clone())
        .map_err(|e| WorkbenchError::new("protocol_error", e.to_string()))?;
    if saved["item"].get("head_ref").is_none() || checkout(&recorded.cwd)? != recorded {
        return Err(WorkbenchError::new("checkout_changed", "The checkout, Git directories, branch or commit changed after preparation. Prepare a new attempt."));
    }
    Ok(())
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Claim {
    id: String,
    request_id: String,
    expected_revision: u64,
    owner_id: String,
    session_id: String,
}

pub(super) fn claim(state: &WorkbenchState, input: &str) -> Result<Value, WorkbenchError> {
    if input.len() > 4096 || !input.trim_start().starts_with('{') {
        return Err(WorkbenchError::new(
            "invalid_input",
            "A launch claim must be a JSON object of at most 4 KiB.",
        ));
    }
    let claim: Claim = serde_json::from_str(input)
        .map_err(|e| WorkbenchError::new("invalid_input", e.to_string()))?;
    if [
        &claim.id,
        &claim.request_id,
        &claim.owner_id,
        &claim.session_id,
    ]
    .iter()
    .any(|id| {
        id.is_empty()
            || id.len() > 128
            || !id
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-')
    }) || claim.expected_revision == 0
        || claim.expected_revision > 9_007_199_254_740_991
    {
        return Err(WorkbenchError::new(
            "invalid_input",
            "A launch claim requires valid identities and an exact revision.",
        ));
    }
    let saved =
        state.with_store(|store| query(store, "runs.get", &json!({"id":claim.id}).to_string()))?;
    if saved["item"]["state"] == "prepared" {
        // Native launch requires an observed branch field. Older, manually
        // supplied records cannot silently imply a detached checkout.
        revalidate(&saved)?;
    }
    // Preserve the exact request identity and the canonical one-use receipt.
    // CAS also closes source/revision races while filesystem probing ran.
    // Filesystem probes are observations, not locks against external git tools.
    state.with_store(|store| query(store, "runs.claim", input))
}

#[cfg(test)]
mod tests {
    use super::super::{Inner, WorkbenchState};
    use super::{replay_verdict, Preparation};
    use crate::engine::git_cli::{git_global, git_text};
    use serde_json::{json, Value};
    use std::path::Path;
    use std::sync::Arc;

    /// Nothing has files open anywhere: the open-file scan is `lsof`, which a
    /// Linux CI image may not have, and its verdict is not what these tests
    /// are about. `an_open_worktree_is_kept_when_its_attempt_is_cancelled`
    /// drives the other answer.
    fn nothing_open(_: &Path) -> Result<(), String> {
        Ok(())
    }

    fn host(path: &Path) -> WorkbenchState {
        WorkbenchState(Arc::new(Inner {
            path: Some(path.into()),
            open_files: Some(nothing_open),
            ..Inner::default()
        }))
    }

    fn limited(path: &Path, live_runs: u32) -> WorkbenchState {
        WorkbenchState(Arc::new(Inner {
            path: Some(path.into()),
            live_runs: Some(live_runs),
            open_files: Some(nothing_open),
            ..Inner::default()
        }))
    }

    fn init(root: &Path) {
        git_global(&["init", root.to_str().unwrap()]).unwrap();
        crate::test_support::trust_repo(root);
    }

    fn commit(root: &Path) {
        git_text(
            root,
            &[
                "-c",
                "user.name=Workbench Test",
                "-c",
                "user.email=workbench@example.invalid",
                "-c",
                "commit.gpgsign=false",
                "commit",
                "--allow-empty",
                "-m",
                "fixture",
            ],
        )
        .unwrap();
    }

    fn prepare(root: &Path) -> Value {
        json!({"id":"run","request_id":"prepare","task_id":"task","source_revision":1,"repository_id":"repo","repository_revision":1,"provider":"codex","permission_mode":"inspect","acknowledge_bypass":false,"repo_path":root})
    }

    fn seed(state: &WorkbenchState, root: &Path) {
        state
            .register(root.to_str().unwrap(), "repo", "register")
            .unwrap();
        state.request("items.put", &json!({"id":"task","request_id":"task","expected_revision":0,"title":"Preserve E42","description":"Exact saved instructions","repository_ids":["repo"],"primary_repository_id":"repo"}).to_string()).unwrap();
    }

    #[test]
    fn native_preparation_binds_real_worktree_identity_and_preserves_source_after_restart() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("repo with spaces");
        let linked = dir.path().join(if cfg!(unix) {
            "linked 'quoted' worktree "
        } else {
            "linked 'quoted' worktree"
        });
        let clone = dir.path().join("different clone");
        init(&root);
        commit(&root);
        git_text(
            &root,
            &["worktree", "add", "--detach", linked.to_str().unwrap()],
        )
        .unwrap();
        git_global(&[
            "clone",
            "--local",
            root.to_str().unwrap(),
            clone.to_str().unwrap(),
        ])
        .unwrap();
        crate::test_support::trust_repo(&linked);
        crate::test_support::trust_repo(&clone);
        let db = dir.path().join("profile.sqlite");
        let state = host(&db);
        seed(&state, &root);
        let wrong = state
            .request("runs.prepare_terminal", &prepare(&clone).to_string())
            .unwrap_err();
        assert_eq!(wrong.code, "repository_mismatch");
        assert_eq!(state.request("runs.list", "{}").unwrap()["total"], 0);
        let input = prepare(&linked).to_string();
        let result = state.request("runs.prepare_terminal", &input).unwrap();
        assert_eq!(result["item"]["state"], "prepared");
        assert_eq!(result["item"]["cwd"], json!(linked.canonicalize().unwrap()));
        assert_eq!(
            result["item"]["git_common_dir"],
            json!(root.join(".git").canonicalize().unwrap())
        );
        assert_ne!(result["item"]["git_dir"], result["item"]["git_common_dir"]);
        assert_eq!(
            result["item"]["head_oid"],
            git_text(&linked, &["rev-parse", "HEAD"]).unwrap().trim()
        );
        // A retry is answered with the attempt itself. Not the receipt's
        // envelope: its `sequence` announced a change that a retry does not
        // make, and the renderer reads only `ok` and `item`.
        let replayed = state.request("runs.prepare_terminal", &input).unwrap();
        assert_eq!(
            (&replayed["ok"], &replayed["item"]),
            (&result["ok"], &result["item"])
        );
        assert_eq!(
            state.request("items.get", r#"{"id":"task"}"#).unwrap()["item"]["revision"],
            1
        );
        assert!(state.0.worker.lock().unwrap().is_none());
        drop(state);
        let restored = host(&db).request("runs.get", r#"{"id":"run"}"#).unwrap();
        assert_eq!(
            restored["item"]["brief"]["task"]["description"],
            "Exact saved instructions"
        );
    }

    #[test]
    fn native_preparation_rejects_malformed_input_before_filesystem_or_profile_io() {
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("absent/profile.sqlite");
        let state = host(&db);
        let base = prepare(&dir.path().join("missing repo"));
        for input in [
            "null".into(),
            "[]".into(),
            "{".into(),
            " ".repeat(16_385),
            base.to_string()
                .replacen("\"id\":\"run\"", "\"id\":\"run\",\"id\":\"again\"", 1),
        ] {
            assert_eq!(
                state
                    .request("runs.prepare_terminal", &input)
                    .unwrap_err()
                    .code,
                "invalid_input",
                "{input}"
            );
        }
        for (key, value) in [
            ("extra", json!(true)),
            ("id", json!("../../run")),
            ("source_revision", json!(0)),
            ("source_revision", json!(9_007_199_254_740_992_u64)),
            ("source_revision", json!(1.5)),
            ("repository_revision", json!(0)),
            ("provider", json!("shell")),
            ("permission_mode", json!("unknown")),
            ("permission_mode", json!("bypass")),
            ("acknowledge_bypass", json!(true)),
            ("repo_path", json!("relative")),
            ("repo_path", json!("/bad\npath")),
            ("repo_path", json!("/bad\0path")),
        ] {
            let mut input = base.clone();
            input[key] = value;
            assert_eq!(
                state
                    .request("runs.prepare_terminal", &input.to_string())
                    .unwrap_err()
                    .code,
                "invalid_input",
                "{input}"
            );
        }
        assert!(!db.parent().unwrap().exists());
        assert!(state.0.worker.lock().unwrap().is_none());
    }

    #[test]
    fn grok_and_antigravity_prepare_as_terminal_providers() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("repo");
        init(&root);
        commit(&root);
        let state = host(&dir.path().join("profile.sqlite"));
        seed(&state, &root);
        for provider in ["grok", "agy"] {
            let mut input = prepare(&root);
            input["id"] = json!(format!("run-{provider}"));
            input["request_id"] = json!(format!("prepare-{provider}"));
            input["provider"] = json!(provider);
            let result = state
                .request("runs.prepare_terminal", &input.to_string())
                .unwrap_or_else(|e| panic!("{provider}: {} {}", e.code, e.message));
            assert_eq!(result["item"]["provider"], provider);
            assert_eq!(result["item"]["kind"], "external_terminal");
            // One prepared run per checkout: cancel before the next provider.
            state
                .request(
                    "runs.cancel",
                    &json!({
                        "id": format!("run-{provider}"),
                        "request_id": format!("cancel-{provider}"),
                        "expected_revision": 1
                    })
                    .to_string(),
                )
                .unwrap();
        }
    }

    /// The managed lane admits exactly the providers with a harness adapter.
    ///
    /// Both directions matter and they fail differently: a provider wrongly
    /// admitted here has its attempt stored and its repository capacity taken
    /// before the harness refuses it, and a provider wrongly excluded is an
    /// adapter nobody can reach.
    #[test]
    fn managed_preparation_admits_every_provider_with_an_adapter_and_no_others() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("repo");
        init(&root);
        commit(&root);
        let state = host(&dir.path().join("profile.sqlite"));
        seed(&state, &root);
        for provider in ["codex", "claude", "grok", "agy", "shell", ""] {
            let mut input = prepare(&root);
            input["id"] = json!(format!("managed-{provider}"));
            input["request_id"] = json!(format!("prepare-managed-{provider}"));
            input["provider"] = json!(provider);
            let result = state.request("runs.prepare_managed", &input.to_string());
            let expected = super::super::terminal_command::is_managed_provider(provider);
            assert_eq!(
                result.is_ok(),
                expected,
                "provider {provider:?} admitted={} expected={expected}",
                result.is_ok()
            );
            match result {
                Ok(row) => {
                    assert_eq!(row["item"]["kind"], "managed");
                    assert_eq!(row["item"]["provider"], provider);
                    // One prepared run per checkout: cancel before the next.
                    state
                        .request(
                            "runs.cancel",
                            &json!({
                                "id": format!("managed-{provider}"),
                                "request_id": format!("cancel-managed-{provider}"),
                                "expected_revision": 1
                            })
                            .to_string(),
                        )
                        .unwrap();
                }
                // Two gates refuse, at different depths: a provider GitPulse
                // cannot launch at all fails `parse` as invalid input, and a
                // launchable provider without a managed adapter fails the
                // managed check. Either is a refusal before anything is stored.
                Err(error) => assert!(
                    matches!(
                        error.code.as_str(),
                        "unsupported_operation" | "invalid_input"
                    ),
                    "{provider:?} was refused as {}",
                    error.code
                ),
            }
        }
    }

    #[test]
    fn native_preparation_distinguishes_unborn_head_from_broken_or_unavailable_checkout() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("unborn");
        init(&root);
        let state = host(&dir.path().join("profile.sqlite"));
        seed(&state, &root);
        let result = state
            .request("runs.prepare_terminal", &prepare(&root).to_string())
            .unwrap();
        assert!(result["item"]["head_oid"].is_null());
        state
            .request(
                "runs.cancel",
                r#"{"id":"run","request_id":"cancel","expected_revision":1}"#,
            )
            .unwrap();
        std::fs::write(root.join(".git/HEAD"), "invalid head\n").unwrap();
        // A fresh attempt: "run" was cancelled, and a retry of it is refused
        // as consumed before any checkout is probed.
        let mut fresh = prepare(&root);
        fresh["id"] = json!("run-broken");
        fresh["request_id"] = json!("prepare-broken");
        assert_eq!(
            state
                .request("runs.prepare_terminal", &fresh.to_string())
                .unwrap_err()
                .code,
            "repository_unavailable"
        );
        let bare = dir.path().join("bare");
        git_global(&["init", "--bare", bare.to_str().unwrap()]).unwrap();
        crate::test_support::trust_repo(&bare);
        assert_eq!(
            state
                .request(
                    "runs.prepare_terminal",
                    &{
                        let mut bare_attempt = prepare(&bare);
                        bare_attempt["id"] = json!("run-bare");
                        bare_attempt["request_id"] = json!("prepare-bare");
                        bare_attempt
                    }
                    .to_string()
                )
                .unwrap_err()
                .code,
            "repository_unavailable"
        );
        assert_eq!(
            state
                .request(
                    "runs.prepare_terminal",
                    &{
                        let mut missing = prepare(&dir.path().join("missing"));
                        missing["id"] = json!("run-missing");
                        missing["request_id"] = json!("prepare-missing");
                        missing
                    }
                    .to_string()
                )
                .unwrap_err()
                .code,
            "repository_unavailable"
        );
        assert_eq!(state.request("runs.list", "{}").unwrap()["total"], 1);
    }

    #[test]
    fn native_claim_refuses_checkout_drift_without_consuming_its_claim() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("repo");
        init(&root);
        commit(&root);
        let state = host(&dir.path().join("profile.sqlite"));
        seed(&state, &root);
        state
            .request("runs.prepare_terminal", &prepare(&root).to_string())
            .unwrap();
        let original_branch = git_text(&root, &["symbolic-ref", "--short", "HEAD"]).unwrap();
        let claim = r#"{"id":"run","request_id":"claim","expected_revision":1,"owner_id":"host","session_id":"session"}"#;
        git_text(&root, &["switch", "-c", "same-commit-other-branch"]).unwrap();
        assert_eq!(
            state.request("runs.claim", claim).unwrap_err().code,
            "checkout_changed"
        );
        commit(&root);
        assert_eq!(
            state.request("runs.claim", claim).unwrap_err().code,
            "checkout_changed"
        );
        assert_eq!(
            state.request("runs.get", r#"{"id":"run"}"#).unwrap()["item"]["state"],
            "prepared"
        );
        // A rejected probe does not consume the request ID. Restore the exact
        // disposable checkout and prove that this claim can be consumed once.
        git_text(&root, &["switch", original_branch.trim()]).unwrap();
        assert_eq!(
            state.request("runs.claim", claim).unwrap()["item"]["state"],
            "starting"
        );
        assert_eq!(
            state.request("runs.claim", claim).unwrap_err().code,
            "claim_consumed"
        );
    }

    #[test]
    fn native_claim_refuses_replaced_checkout_and_changed_task() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("repo");
        init(&root);
        commit(&root);
        let state = host(&dir.path().join("profile.sqlite"));
        seed(&state, &root);
        state
            .request("runs.prepare_terminal", &prepare(&root).to_string())
            .unwrap();
        let claim = r#"{"id":"run","request_id":"claim","expected_revision":1,"owner_id":"host","session_id":"session"}"#;
        std::fs::rename(&root, dir.path().join("moved")).unwrap();
        assert_eq!(
            state.request("runs.claim", claim).unwrap_err().code,
            "repository_unavailable"
        );
        init(&root);
        assert_eq!(
            state.request("runs.claim", claim).unwrap_err().code,
            "checkout_changed"
        );
        std::fs::rename(&root, dir.path().join("replacement")).unwrap();
        std::fs::rename(dir.path().join("moved"), &root).unwrap();
        crate::test_support::trust_repo(&root);
        state.request("items.put", &json!({"id":"task","request_id":"edit","expected_revision":1,"title":"Changed instructions","repository_ids":["repo"],"primary_repository_id":"repo"}).to_string()).unwrap();
        assert_eq!(
            state.request("runs.claim", claim).unwrap_err().code,
            "revision_conflict"
        );
        assert_eq!(
            state.request("runs.get", r#"{"id":"run"}"#).unwrap()["item"]["state"],
            "prepared"
        );
    }

    fn with_worktree(root: &Path, id: &str) -> Value {
        let mut input = prepare(root);
        input["id"] = json!(id);
        input["request_id"] = json!(format!("prepare-{id}"));
        input["worktree"] = json!(true);
        input
    }

    fn porcelain(root: &Path) -> String {
        git_text(root, &["status", "--porcelain", "--untracked-files=all"]).unwrap()
    }

    /// The limit is the one the store enforces: two at once refuses a third —
    /// saying where to raise it, and removing the worktree made for the
    /// refused attempt — and a host passing a higher limit admits the third
    /// into the same profile.
    #[test]
    fn the_users_agents_at_once_limit_is_what_the_store_enforces() {
        let dir = tempfile::tempdir().unwrap();
        let profile = dir.path().join("profile.sqlite");
        let limited = |n: u32| {
            WorkbenchState(Arc::new(Inner {
                path: Some(profile.clone()),
                live_runs: Some(n),
                ..Inner::default()
            }))
        };
        let root = dir.path().join("repo");
        init(&root);
        commit(&root);
        let state = limited(2);
        seed(&state, &root);
        state
            .request("runs.prepare_terminal", &prepare(&root).to_string())
            .unwrap();
        state
            .request(
                "runs.prepare_terminal",
                &with_worktree(&root, "f00dcafe-1").to_string(),
            )
            .unwrap();
        let full = state
            .request(
                "runs.prepare_terminal",
                &with_worktree(&root, "beefbeef-2").to_string(),
            )
            .unwrap_err();
        assert_eq!(full.code, "capacity_reached");
        assert!(
            full.message.starts_with("2 runs are already"),
            "{}",
            full.message
        );
        assert!(
            full.message.contains("Settings → Agents (up to 64)"),
            "{}",
            full.message
        );
        assert!(
            !root
                .canonicalize()
                .unwrap()
                .join(".gitpulse/worktrees/preserve-e42-beefbeef")
                .exists(),
            "a refused attempt left its worktree behind"
        );
        limited(3)
            .request(
                "runs.prepare_terminal",
                &with_worktree(&root, "beefbeef-2").to_string(),
            )
            .unwrap();
    }

    #[test]
    fn a_second_task_in_a_busy_checkout_runs_in_its_own_ignored_worktree() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("repo");
        init(&root);
        commit(&root);
        let state = host(&dir.path().join("profile.sqlite"));
        seed(&state, &root);
        state
            .request("runs.prepare_terminal", &prepare(&root).to_string())
            .unwrap();
        // The main checkout is taken; the same checkout again is refused...
        let mut again = prepare(&root);
        again["id"] = json!("again");
        again["request_id"] = json!("prepare-again");
        assert_eq!(
            state
                .request("runs.prepare_terminal", &again.to_string())
                .unwrap_err()
                .code,
            "checkout_busy"
        );
        // ...and a fresh worktree is admitted beside it, concurrently.
        let run = state
            .request(
                "runs.prepare_terminal",
                &with_worktree(&root, "f00dcafe-1").to_string(),
            )
            .unwrap();
        let cwd = run["item"]["cwd"].as_str().unwrap();
        let expected = root
            .canonicalize()
            .unwrap()
            .join(".gitpulse/worktrees/preserve-e42-f00dcafe");
        assert_eq!(Path::new(cwd), expected);
        assert_eq!(
            git_text(Path::new(cwd), &["branch", "--show-current"])
                .unwrap()
                .trim(),
            "gitpulse/preserve-e42-f00dcafe"
        );
        // The main checkout's status does not show the agent's tree. (The
        // gate's own ledger may appear here; it is not this change's.)
        let status = porcelain(&root);
        assert!(!status.contains(".gitpulse"), "{status}");
        let exclude = std::fs::read_to_string(root.join(".git/info/exclude")).unwrap();
        assert!(exclude.contains("/.gitpulse/worktrees/"));
        // A third task gets a third tree; two live runs, one repository.
        state
            .request(
                "runs.prepare_terminal",
                &with_worktree(&root, "beefbeef-2").to_string(),
            )
            .unwrap();
        assert_eq!(state.request("runs.list", "{}").unwrap()["total"], 3);
    }

    #[test]
    fn a_refused_attempt_takes_its_worktree_and_branch_with_it() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("repo");
        init(&root);
        commit(&root);
        let state = host(&dir.path().join("profile.sqlite"));
        seed(&state, &root);
        let mut stale = with_worktree(&root, "abcdef12-x");
        stale["source_revision"] = json!(9);
        let error = state
            .request("runs.prepare_terminal", &stale.to_string())
            .unwrap_err();
        assert_ne!(error.code, "worktree_unavailable", "{}", error.message);
        assert!(!root
            .join(".gitpulse/worktrees/preserve-e42-abcdef12")
            .exists());
        assert_eq!(
            git_text(&root, &["branch", "--list", "gitpulse/*"])
                .unwrap()
                .trim(),
            ""
        );
        assert_eq!(
            git_text(&root, &["worktree", "list", "--porcelain"])
                .unwrap()
                .matches("worktree ")
                .count(),
            1
        );
        assert_eq!(state.request("runs.list", "{}").unwrap()["total"], 0);
    }

    #[test]
    fn a_replayed_worktree_preparation_reuses_its_tree_and_its_receipt() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("repo");
        init(&root);
        commit(&root);
        let state = host(&dir.path().join("profile.sqlite"));
        seed(&state, &root);
        let input = with_worktree(&root, "cafe0123-r").to_string();
        let first = state.request("runs.prepare_terminal", &input).unwrap();
        // A lost reply is retried with the identical request.
        let second = state.request("runs.prepare_terminal", &input).unwrap();
        assert_eq!(first["item"], second["item"]);
        assert_eq!(state.request("runs.list", "{}").unwrap()["total"], 1);
        assert_eq!(
            git_text(&root, &["worktree", "list", "--porcelain"])
                .unwrap()
                .matches("worktree ")
                .count(),
            2
        );
    }

    /// A retry of an unanswered preparation returns the attempt the store
    /// committed, though values the host reads at call time have moved.
    ///
    /// The host builds the store request from the renderer's fields plus
    /// `max_active_runs`, the saved model and the checkout probe, and the
    /// store replays a receipt only for an identical request — so a retry
    /// after any of those changed was refused as "request_id was already used
    /// for a different request", and the committed attempt held its slot.
    #[cfg(unix)]
    #[test]
    fn a_retry_returns_the_committed_attempt_after_host_values_moved() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("repo");
        init(&root);
        commit(&root);
        let db = dir.path().join("profile.sqlite");
        let state = limited(&db, 2);
        seed(&state, &root);
        for input in [prepare(&root), with_worktree(&root, "cafe0123-d")] {
            let input = input.to_string();
            let first = state.request("runs.prepare_terminal", &input).unwrap();
            // The reply was lost; meanwhile the limit changed and HEAD moved.
            commit(&root);
            let retried = limited(&db, 3)
                .request("runs.prepare_terminal", &input)
                .unwrap_or_else(|e| panic!("{}: {}", e.code, e.message));
            assert_eq!(first["item"], retried["item"]);
            assert!(
                retried["item"].get("brief").is_none(),
                "a replay answered in a different shape: {retried}"
            );
            state.request("runs.cancel", &json!({"id": first["item"]["id"], "request_id": format!("cancel-{}", first["item"]["id"].as_str().unwrap()), "expected_revision": 1}).to_string()).unwrap();
        }
        assert_eq!(state.request("runs.list", "{}").unwrap()["total"], 2);
    }

    /// The same, when the saved model default changed before the retry: the
    /// attempt keeps the choice it recorded, and a default that would now be
    /// refused does not refuse a replay of an attempt that already exists.
    #[test]
    fn a_retry_keeps_the_model_its_attempt_recorded() {
        let serial = crate::tool_config::lock_config_env();
        let dir = tempfile::tempdir().unwrap();
        let _env = crate::test_support::env::bind_env(&serial)
            .set(
                crate::tool_config::TOOL_CONFIG_ENV,
                dir.path().join("tools.json"),
            )
            .invalidating(crate::tool_config::invalidate_cache);
        let root = dir.path().join("repo");
        init(&root);
        commit(&root);
        let state = host(&dir.path().join("profile.sqlite"));
        seed(&state, &root);
        let mut input = prepare(&root);
        input["provider"] = json!("claude");
        input["model_choice"] = json!({"model": "opus"});
        let first = state
            .request("runs.prepare_terminal", &input.to_string())
            .unwrap();
        let mut defaults = crate::tool_config::AgentDefaults::default();
        defaults.models.insert(
            "claude".into(),
            crate::tool_config::ModelChoice {
                effort: Some("max".into()),
                ..Default::default()
            },
        );
        crate::tool_config::set_agent_defaults(defaults).unwrap();
        let retried = state
            .request("runs.prepare_terminal", &input.to_string())
            .unwrap_or_else(|e| panic!("{}: {}", e.code, e.message));
        assert_eq!(retried["item"]["model_choice"], json!({"model": "opus"}));
        assert_eq!(first["item"], retried["item"]);
    }

    /// An attempt id names one preparation: a different request under it is
    /// refused, never answered with the other request's attempt, and an
    /// attempt that has moved on is not handed back as launchable.
    #[test]
    fn an_attempt_id_is_never_handed_to_a_different_or_finished_request() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("repo");
        init(&root);
        commit(&root);
        let state = host(&dir.path().join("profile.sqlite"));
        seed(&state, &root);
        state
            .request("runs.prepare_terminal", &prepare(&root).to_string())
            .unwrap();
        for (field, value) in [
            ("provider", json!("claude")),
            ("permission_mode", json!("ask")),
            ("source_revision", json!(2)),
            ("task_id", json!("other")),
        ] {
            let mut other = prepare(&root);
            other[field] = value;
            let refused = state
                .request("runs.prepare_terminal", &other.to_string())
                .unwrap_err();
            assert!(
                matches!(
                    refused.code.as_str(),
                    "invalid_state" | "invalid_input" | "not_found"
                ),
                "{field}: {} {}",
                refused.code,
                refused.message
            );
        }
        assert_eq!(state.request("runs.list", "{}").unwrap()["total"], 1);
        state
            .request(
                "runs.cancel",
                &json!({"id":"run","request_id":"cancel","expected_revision":1}).to_string(),
            )
            .unwrap();
        let consumed = state
            .request("runs.prepare_terminal", &prepare(&root).to_string())
            .unwrap_err();
        assert_eq!(consumed.code, "launch_consumed", "{}", consumed.message);
    }

    #[test]
    fn the_replay_verdict_reads_only_what_the_renderer_owns() {
        let input: Preparation = serde_json::from_value(json!({"id":"run","request_id":"r","task_id":"task","source_revision":3,"repository_id":"repo","repository_revision":1,"provider":"codex","permission_mode":"inspect","acknowledge_bypass":false,"repo_path":"/w"})).unwrap();
        let saved = |state: &str, expires: u64| json!({"ok":true,"item":{"id":"run","task_id":"task","source_revision":3,"repository_id":"repo","provider":"codex","permission_mode":"inspect","bypass_acknowledged":false,"kind":"external_terminal","state":state,"expires_at":expires,"head_oid":"anything","brief":{"markdown":"x"}}});
        let ok =
            replay_verdict(&saved("prepared", 1_000), &input, "external_terminal", 999).unwrap();
        assert!(ok["item"].get("brief").is_none() && ok["ok"] == json!(true));
        // Expired at exactly its expiry, as the store counts it.
        assert_eq!(
            replay_verdict(
                &saved("prepared", 1_000),
                &input,
                "external_terminal",
                1_000
            )
            .unwrap_err()
            .code,
            "launch_consumed"
        );
        assert_eq!(
            replay_verdict(&saved("starting", 1_000), &input, "external_terminal", 0)
                .unwrap_err()
                .code,
            "launch_consumed"
        );
        assert_eq!(
            replay_verdict(&saved("prepared", 1_000), &input, "managed", 0)
                .unwrap_err()
                .code,
            "invalid_state"
        );
        // A record missing a field it must match is not a match.
        let mut partial = saved("prepared", 1_000);
        partial["item"].as_object_mut().unwrap().remove("provider");
        assert_eq!(
            replay_verdict(&partial, &input, "external_terminal", 0)
                .unwrap_err()
                .code,
            "invalid_state"
        );
        // Fuzz: change any one renderer-owned field and the verdict refuses.
        for field in [
            "task_id",
            "source_revision",
            "repository_id",
            "provider",
            "permission_mode",
            "bypass_acknowledged",
            "kind",
        ] {
            for value in [json!(null), json!("x"), json!(0), json!(true), json!(4)] {
                let mut moved = saved("prepared", 1_000);
                if moved["item"][field] == value {
                    continue;
                }
                moved["item"][field] = value.clone();
                assert!(
                    replay_verdict(&moved, &input, "external_terminal", 0).is_err(),
                    "{field}={value} was accepted"
                );
            }
        }
    }

    /// Concurrent replays of one preparation, from hosts that read different
    /// limits, answer with one attempt and create exactly one.
    #[test]
    fn concurrent_replays_create_one_attempt() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("repo");
        init(&root);
        commit(&root);
        let db = dir.path().join("profile.sqlite");
        let state = limited(&db, 2);
        seed(&state, &root);
        let input = prepare(&root).to_string();
        let answers: Vec<_> = std::thread::scope(|scope| {
            let workers: Vec<_> = (0..8u32)
                .map(|n| {
                    let host = if n % 2 == 0 {
                        state.clone()
                    } else {
                        limited(&db, 2 + n)
                    };
                    let input = input.clone();
                    scope.spawn(move || host.request("runs.prepare_terminal", &input))
                })
                .collect();
            workers.into_iter().map(|w| w.join().unwrap()).collect()
        });
        let items: Vec<_> = answers
            .iter()
            .filter_map(|a| a.as_ref().ok())
            .map(|a| a["item"].clone())
            .collect();
        for refused in answers.iter().filter_map(|a| a.as_ref().err()) {
            // Contention may refuse a racer; it must never be a second attempt or a conflict.
            assert_ne!(refused.code, "idempotency_conflict", "{}", refused.message);
        }
        assert!(!items.is_empty());
        assert!(
            items.windows(2).all(|w| w[0] == w[1]),
            "replays answered with different attempts"
        );
        assert_eq!(state.request("runs.list", "{}").unwrap()["total"], 1);
    }

    /// The form's "is this checkout taken" is asked by the key the store
    /// refuses on, which only the host can observe.
    #[cfg(unix)]
    #[test]
    fn a_checkout_identity_names_the_git_directory_a_preparation_records() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("repo");
        init(&root);
        commit(&root);
        std::fs::create_dir_all(root.join("pkg/web")).unwrap();
        std::fs::write(root.join("pkg/web/keep"), "").unwrap();
        git_text(&root, &["add", "pkg"]).unwrap();
        commit(&root);
        let state = host(&dir.path().join("profile.sqlite"));
        seed(&state, &root);
        let sub = root.join("pkg/web");
        let prepared = state
            .request("runs.prepare_terminal", &prepare(&sub).to_string())
            .unwrap();
        for path in [&root, &sub] {
            let identity = state
                .request(
                    "runs.checkout_identity",
                    &json!({"repo_path": path}).to_string(),
                )
                .unwrap();
            assert_eq!(
                identity["git_dir"],
                prepared["item"]["git_dir"],
                "{}",
                path.display()
            );
            assert_eq!(
                identity["git_common_dir"],
                prepared["item"]["git_common_dir"]
            );
        }
        // A worktree nested inside the checkout is a checkout of its own.
        let nested = state
            .request(
                "runs.prepare_terminal",
                &with_worktree(&root, "cafe0123-n").to_string(),
            )
            .unwrap();
        let identity = state
            .request(
                "runs.checkout_identity",
                &json!({"repo_path": nested["item"]["cwd"]}).to_string(),
            )
            .unwrap();
        assert_eq!(identity["git_dir"], nested["item"]["git_dir"]);
        assert_ne!(identity["git_dir"], prepared["item"]["git_dir"]);
        // Refused as input: relative, oversized, unknown fields, not an object.
        for input in [
            json!({"repo_path":"repo"}).to_string(),
            json!({"repo_path": format!("/{}", "x".repeat(4_200))}).to_string(),
            json!({"repo_path":"/r","extra":1}).to_string(),
            "[]".into(),
            "x".repeat(9_000),
        ] {
            assert_eq!(
                state
                    .request("runs.checkout_identity", &input)
                    .unwrap_err()
                    .code,
                "invalid_input",
                "{}",
                &input[..input.len().min(60)]
            );
        }
        // Git is not run in a checkout nobody trusted.
        let stranger = dir.path().join("stranger");
        git_global(&["init", stranger.to_str().unwrap()]).unwrap();
        assert_eq!(
            state
                .request(
                    "runs.checkout_identity",
                    &json!({"repo_path": stranger}).to_string()
                )
                .unwrap_err()
                .code,
            "repository_unavailable"
        );
        assert_eq!(state.request("runs.list", "{}").unwrap()["total"], 2);
    }

    /// Commits a `post_create` hook, so a provisioned worktree runs it.
    fn post_create(root: &Path, command: &str) {
        std::fs::create_dir_all(root.join(".gitpulse")).unwrap();
        let hooks = json!({"worktree":{"post_create":[command]}}).to_string();
        std::fs::write(root.join(".gitpulse/hooks.json"), &hooks).unwrap();
        // Hooks run only once approved; these tests are about what a hook
        // that runs does, so the file is approved as written.
        crate::engine::approve_worktree_hooks(root, &hooks).unwrap();
        git_text(root, &["add", ".gitpulse/hooks.json"]).unwrap();
        commit(root);
    }

    fn lane(root: &Path, name: &str) -> std::path::PathBuf {
        root.canonicalize()
            .unwrap()
            .join(".gitpulse/worktrees")
            .join(name)
    }

    fn gitpulse_branches(root: &Path) -> String {
        git_text(
            root,
            &[
                "for-each-ref",
                "--format=%(refname:short)",
                "refs/heads/gitpulse/",
            ],
        )
        .unwrap()
        .trim()
        .to_owned()
    }

    fn cancel(state: &WorkbenchState, id: &str) -> Value {
        state
            .request(
                "runs.cancel",
                &json!({"id":id,"request_id":format!("cancel-{id}"),"expected_revision":1})
                    .to_string(),
            )
            .unwrap()
    }

    /// A full profile is refused before anything is built: the worktree, its
    /// branch and the repository's whole setup hook used to run first, only
    /// to be torn down again when the store said no. The refusal is the
    /// store's own sentence, word for word.
    #[cfg(unix)]
    #[test]
    fn a_full_profile_refuses_before_building_a_worktree() {
        let dir = tempfile::tempdir().unwrap();
        let sentinel = dir.path().join("setup-ran");
        let root = dir.path().join("repo");
        init(&root);
        commit(&root);
        post_create(&root, &format!("touch '{}'", sentinel.display()));
        let other = dir.path().join("other");
        git_text(
            &root,
            &["worktree", "add", "--detach", other.to_str().unwrap()],
        )
        .unwrap();
        crate::test_support::trust_repo(&other);
        let state = limited(&dir.path().join("profile.sqlite"), 1);
        seed(&state, &root);
        state
            .request("runs.prepare_terminal", &prepare(&root).to_string())
            .unwrap();
        // The store's refusal, for a checkout that needs no worktree.
        let mut elsewhere = prepare(&other);
        elsewhere["id"] = json!("elsewhere");
        elsewhere["request_id"] = json!("prepare-elsewhere");
        let store = state
            .request("runs.prepare_terminal", &elsewhere.to_string())
            .unwrap_err();
        assert_eq!(store.code, "capacity_reached");
        let refused = state
            .request(
                "runs.prepare_terminal",
                &with_worktree(&root, "f00dcafe-1").to_string(),
            )
            .unwrap_err();
        assert_eq!(refused.code, "capacity_reached");
        assert_eq!(refused.message, store.message);
        assert!(
            !sentinel.exists(),
            "the setup hook ran for an attempt the profile had no room for"
        );
        assert!(!lane(&root, "preserve-e42-f00dcafe").exists());
        assert_eq!(gitpulse_branches(&root), "");
    }

    /// A model choice the launch cannot record is refused before the
    /// repository's setup hook runs, not after.
    ///
    /// The choice was resolved inside `prepare_in`, after the worktree was
    /// added and its hook had run — minutes of dependency installs, then a
    /// refusal and a discarded tree. It is a pure function of the request and
    /// the saved default, so it is answered first.
    #[cfg(unix)]
    #[test]
    fn a_model_choice_the_launch_cannot_record_refuses_before_building_a_worktree() {
        let dir = tempfile::tempdir().unwrap();
        let sentinel = dir.path().join("setup-ran");
        let root = dir.path().join("repo");
        init(&root);
        commit(&root);
        post_create(&root, &format!("touch '{}'", sentinel.display()));
        let state = limited(&dir.path().join("profile.sqlite"), 2);
        seed(&state, &root);
        let arn = |n: u8| {
            format!("arn:aws:bedrock:us-east-1:123456789012:inference-profile/us.anthropic.claude-model-{n}")
        };
        for (id, provider, choice, says) in [
            // Codex takes a model and nothing else.
            ("f00dcafe-1", "codex", json!({"effort":"high"}), "effort"),
            // Each id fits; joined, as the store records them, they do not.
            (
                "f00dcafe-2",
                "claude",
                json!({"fallback":[arn(1), arn(2)]}),
                "128",
            ),
        ] {
            let mut input = with_worktree(&root, id);
            input["provider"] = json!(provider);
            input["model_choice"] = choice;
            let refused = state
                .request("runs.prepare_terminal", &input.to_string())
                .unwrap_err();
            assert_eq!(
                refused.code, "invalid_input",
                "{provider}: {}",
                refused.message
            );
            assert!(
                refused.message.contains(says),
                "{provider}: {}",
                refused.message
            );
        }
        assert!(
            !sentinel.exists(),
            "the setup hook ran for a launch that was always going to be refused"
        );
        assert_eq!(gitpulse_branches(&root), "");
    }

    /// A retry that arrives while the first preparation is still running the
    /// repository's setup hook is told so. It used to find the half-built
    /// tree, adopt it as "already made", and admit the attempt — and when the
    /// first preparation's hook then failed, that tree was removed under the
    /// agent the retry had started in it.
    #[cfg(unix)]
    #[test]
    fn a_retry_during_worktree_setup_is_refused_rather_than_handed_the_half_built_tree() {
        let dir = tempfile::tempdir().unwrap();
        let started = dir.path().join("setup-started");
        let root = dir.path().join("repo");
        init(&root);
        commit(&root);
        post_create(&root, &format!("touch '{}'; sleep 3", started.display()));
        let state = host(&dir.path().join("profile.sqlite"));
        seed(&state, &root);
        let input = with_worktree(&root, "c0c0a123-q").to_string();
        std::thread::scope(|scope| {
            let first = scope.spawn(|| state.request("runs.prepare_terminal", &input));
            let deadline = std::time::Instant::now() + std::time::Duration::from_secs(60);
            while !started.exists() {
                assert!(
                    std::time::Instant::now() < deadline,
                    "the setup hook never started"
                );
                std::thread::sleep(std::time::Duration::from_millis(20));
            }
            let retry = state.request("runs.prepare_terminal", &input);
            let first = first.join().unwrap();
            let refusal =
                retry.expect_err("a retry was handed a worktree whose setup was still running");
            assert_eq!(refusal.code, "busy");
            assert!(
                refusal.message.contains("still setting up its worktree"),
                "{}",
                refusal.message
            );
            assert_eq!(first.unwrap()["item"]["state"], "prepared");
        });
    }

    /// A worktree whose setup never finished — GitPulse quit during the hook —
    /// is set up again before an agent is given it. It used to be adopted as
    /// this attempt's finished tree, so the agent started without the
    /// dependencies the hook installs.
    #[cfg(unix)]
    #[test]
    fn a_worktree_left_half_built_is_set_up_again_before_an_agent_gets_it() {
        let dir = tempfile::tempdir().unwrap();
        let ran = dir.path().join("setup-ran");
        let root = dir.path().join("repo");
        init(&root);
        commit(&root);
        post_create(&root, &format!("touch '{}'", ran.display()));
        // What a preparation that died during its hook leaves behind.
        let path = lane(&root, "preserve-e42-deadc0de");
        git_text(
            &root,
            &[
                "worktree",
                "add",
                "-b",
                "gitpulse/preserve-e42-deadc0de",
                path.to_str().unwrap(),
            ],
        )
        .unwrap();
        std::fs::write(path.join("half-installed"), "x").unwrap();
        let state = host(&dir.path().join("profile.sqlite"));
        seed(&state, &root);
        let run = state
            .request(
                "runs.prepare_terminal",
                &with_worktree(&root, "deadc0de-h").to_string(),
            )
            .unwrap();
        assert_eq!(Path::new(run["item"]["cwd"].as_str().unwrap()), path);
        assert!(
            ran.exists(),
            "the agent was given a tree whose setup never ran"
        );
        assert!(!path.join("half-installed").exists());
    }

    /// Cancelling an attempt that was never claimed gives back the worktree
    /// and branch made for it. Both used to stay forever, one more per
    /// cancel-and-retry.
    #[test]
    fn cancelling_an_unclaimed_attempt_removes_the_worktree_made_for_it() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("repo");
        init(&root);
        commit(&root);
        let state = host(&dir.path().join("profile.sqlite"));
        seed(&state, &root);
        let run = state
            .request(
                "runs.prepare_terminal",
                &with_worktree(&root, "abad1dea-c").to_string(),
            )
            .unwrap();
        let cwd = run["item"]["cwd"].as_str().unwrap().to_owned();
        assert!(Path::new(&cwd).is_dir());
        let cancelled = cancel(&state, "abad1dea-c");
        assert_eq!(cancelled["item"]["state"], "cancelled");
        assert!(
            !Path::new(&cwd).exists(),
            "the cancelled attempt's worktree stayed"
        );
        assert_eq!(gitpulse_branches(&root), "");
        assert_eq!(cancelled["worktree"]["removed"], true, "{cancelled}");
        assert_eq!(cancelled["worktree"]["path"], json!(cwd));
    }

    /// Work in the tree is never thrown away to tidy up: a cancelled attempt
    /// whose worktree has changes keeps it, and says why.
    #[test]
    fn cancelling_keeps_a_worktree_that_holds_changes() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("repo");
        init(&root);
        commit(&root);
        let state = host(&dir.path().join("profile.sqlite"));
        seed(&state, &root);
        let run = state
            .request(
                "runs.prepare_terminal",
                &with_worktree(&root, "5afe0001-k").to_string(),
            )
            .unwrap();
        let cwd = Path::new(run["item"]["cwd"].as_str().unwrap()).to_path_buf();
        std::fs::write(cwd.join("notes.txt"), "mine").unwrap();
        let cancelled = cancel(&state, "5afe0001-k");
        assert_eq!(cancelled["item"]["state"], "cancelled");
        assert_eq!(
            std::fs::read_to_string(cwd.join("notes.txt")).unwrap(),
            "mine"
        );
        assert_eq!(gitpulse_branches(&root), "gitpulse/preserve-e42-5afe0001");
        assert_eq!(cancelled["worktree"]["removed"], false, "{cancelled}");
        assert!(
            cancelled["worktree"]["kept_because"]
                .as_str()
                .is_some_and(|why| !why.is_empty()),
            "{cancelled}"
        );
    }

    /// A subdirectory of the checkout is recorded as the attempt's directory,
    /// with the root's identity; claiming it revalidates the same way. A
    /// managed agent is refused one, and a worktree attempt works in the same
    /// folder of its own tree — whose cancel still gives that tree back.
    #[test]
    fn a_subdirectory_is_prepared_with_its_checkouts_identity() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("repo");
        init(&root);
        std::fs::create_dir_all(root.join("pkg/web")).unwrap();
        std::fs::write(root.join("pkg/web/index.js"), "x\n").unwrap();
        git_text(&root, &["add", "pkg"]).unwrap();
        commit(&root);
        let state = host(&dir.path().join("profile.sqlite"));
        seed(&state, &root);
        let folder = root.join("pkg/web");
        let prepared = state
            .request("runs.prepare_terminal", &prepare(&folder).to_string())
            .unwrap();
        assert_eq!(
            prepared["item"]["cwd"],
            json!(folder.canonicalize().unwrap())
        );
        assert_eq!(
            prepared["item"]["git_dir"],
            json!(root.join(".git").canonicalize().unwrap())
        );
        // One agent per working tree, wherever in it: the root is busy now.
        let mut again = prepare(&root);
        again["id"] = json!("again");
        again["request_id"] = json!("prepare-again");
        assert_eq!(
            state
                .request("runs.prepare_terminal", &again.to_string())
                .unwrap_err()
                .code,
            "checkout_busy"
        );
        let claim = r#"{"id":"run","request_id":"claim","expected_revision":1,"owner_id":"host","session_id":"session"}"#;
        assert_eq!(
            state.request("runs.claim", claim).unwrap()["item"]["state"],
            "starting"
        );

        let mut managed = with_worktree(&folder, "d00dfeed-m");
        managed["provider"] = json!("codex");
        managed["worktree"] = json!(false);
        let refused = state
            .request("runs.prepare_managed", &managed.to_string())
            .unwrap_err();
        assert_eq!(refused.code, "unsupported_operation");
        assert!(refused.message.contains("pkg/web"), "{}", refused.message);

        let lane_run = state
            .request(
                "runs.prepare_terminal",
                &with_worktree(&folder, "feed0123-w").to_string(),
            )
            .unwrap();
        let tree = lane(&root, "preserve-e42-feed0123");
        assert_eq!(
            Path::new(lane_run["item"]["cwd"].as_str().unwrap()),
            tree.join("pkg/web")
        );
        let cancelled = cancel(&state, "feed0123-w");
        assert_eq!(cancelled["worktree"]["removed"], true, "{cancelled}");
        assert!(!tree.exists());
    }

    /// A clean tree that something still has open — a shell started in it, an
    /// editor — is kept, and the scan's answer is the reason given.
    #[test]
    fn an_open_worktree_is_kept_when_its_attempt_is_cancelled() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("repo");
        init(&root);
        commit(&root);
        let state = WorkbenchState(Arc::new(Inner {
            path: Some(dir.path().join("profile.sqlite")),
            open_files: Some(|_| Err("pid 4242 has its working directory here".into())),
            ..Inner::default()
        }));
        seed(&state, &root);
        let run = state
            .request(
                "runs.prepare_terminal",
                &with_worktree(&root, "0be00be0-o").to_string(),
            )
            .unwrap();
        let cancelled = cancel(&state, "0be00be0-o");
        assert!(Path::new(run["item"]["cwd"].as_str().unwrap()).is_dir());
        assert_eq!(cancelled["worktree"]["removed"], false);
        assert!(
            cancelled["worktree"]["kept_because"]
                .as_str()
                .unwrap()
                .contains("pid 4242"),
            "{cancelled}"
        );
        // A cancel of an attempt that never had its own worktree reports none.
        state
            .request("runs.prepare_terminal", &prepare(&root).to_string())
            .unwrap();
        let plain = cancel(&state, "run");
        assert_eq!(plain["item"]["state"], "cancelled");
        assert!(plain.get("worktree").is_none(), "{plain}");
    }

    /// A preparation that expired unclaimed gives its worktree back when
    /// GitPulse next reconciles; one still within its five minutes keeps it.
    #[test]
    fn an_expired_unclaimed_attempt_gives_its_worktree_back_on_reconciliation() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("repo");
        init(&root);
        commit(&root);
        let state = host(&dir.path().join("profile.sqlite"));
        seed(&state, &root);
        let mut cwds = Vec::new();
        for id in ["e1e1e1e1-x", "f2f2f2f2-y"] {
            let run = state
                .request(
                    "runs.prepare_terminal",
                    &with_worktree(&root, id).to_string(),
                )
                .unwrap();
            cwds.push(run["item"]["cwd"].as_str().unwrap().to_owned());
        }
        state
            .with_store(|store| {
                store
                    .connection()
                    .execute(
                        "UPDATE work_runs SET body=json_set(body,'$.expires_at',1) WHERE id='e1e1e1e1-x'",
                        [],
                    )
                    .map_err(|e| super::super::WorkbenchError::new("store_error", e.to_string()))
            })
            .unwrap();
        state.reconcile_stale_runs().unwrap();
        assert!(
            !Path::new(&cwds[0]).exists(),
            "the expired attempt's worktree stayed"
        );
        assert!(
            Path::new(&cwds[1]).is_dir(),
            "a live preparation lost its worktree"
        );
        assert_eq!(gitpulse_branches(&root), "gitpulse/preserve-e42-f2f2f2f2");
    }

    #[test]
    fn a_stranger_directory_at_the_worktree_path_is_never_adopted() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("repo");
        init(&root);
        commit(&root);
        let state = host(&dir.path().join("profile.sqlite"));
        seed(&state, &root);
        let squatter = root.join(".gitpulse/worktrees/preserve-e42-deadbeef");
        std::fs::create_dir_all(&squatter).unwrap();
        std::fs::write(squatter.join("keep.txt"), "mine").unwrap();
        let error = state
            .request(
                "runs.prepare_terminal",
                &with_worktree(&root, "deadbeef-s").to_string(),
            )
            .unwrap_err();
        assert_eq!(error.code, "worktree_unavailable");
        assert_eq!(
            std::fs::read_to_string(squatter.join("keep.txt")).unwrap(),
            "mine"
        );
    }

    /// The handoff form's model for one launch is laid over the saved default
    /// and recorded on the attempt — the record is what its launch applies
    /// (`terminal_run::spawn`), not whatever the default is by then.
    #[test]
    fn a_launch_records_the_saved_default_with_its_own_override() {
        let serial = crate::tool_config::lock_config_env();
        let dir = tempfile::tempdir().unwrap();
        let _env = crate::test_support::env::bind_env(&serial)
            .set(
                crate::tool_config::TOOL_CONFIG_ENV,
                dir.path().join("tools.json"),
            )
            .invalidating(crate::tool_config::invalidate_cache);
        let mut defaults = crate::tool_config::AgentDefaults::default();
        defaults.models.insert(
            "claude".into(),
            crate::tool_config::ModelChoice {
                model: Some("sonnet".into()),
                effort: Some("medium".into()),
                ..Default::default()
            },
        );
        crate::tool_config::set_agent_defaults(defaults).unwrap();

        let root = dir.path().join("repo");
        init(&root);
        commit(&root);
        let state = host(&dir.path().join("profile.sqlite"));
        seed(&state, &root);
        let mut input = prepare(&root);
        input["provider"] = json!("claude");
        input["model_choice"] = json!({"model": "opus"});
        let run = state
            .request("runs.prepare_terminal", &input.to_string())
            .unwrap();
        assert_eq!(
            run["item"]["model_choice"],
            json!({"model": "opus", "effort": "medium"})
        );

        // An override the launcher cannot take is refused before an attempt exists.
        let mut bad = prepare(&root);
        bad["id"] = json!("run-bad");
        bad["request_id"] = json!("prepare-bad");
        bad["provider"] = json!("codex");
        bad["model_choice"] = json!({"effort": "high"});
        let refused = state
            .request("runs.prepare_terminal", &bad.to_string())
            .unwrap_err();
        assert_eq!(refused.code, "invalid_input", "{}", refused.message);
        // A field the form does not know is refused by the parser, not dropped.
        bad["model_choice"] = json!({"temperature": "1"});
        assert_eq!(
            state
                .request("runs.prepare_terminal", &bad.to_string())
                .unwrap_err()
                .code,
            "invalid_input"
        );
        assert_eq!(state.request("runs.list", "{}").unwrap()["total"], 1);
    }
}
