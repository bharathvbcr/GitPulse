//! Bidirectional Agent-to-Agent (A2A) MCP handlers (`gitpulse_agent_*`) and
//! unattended `gitpulse_kickoff_agent` `spawn` bridge.
//!
//! Coordinates Claude Code (`claude` hosted tab or external CLI), Antigravity
//! (`agy` hosted tab, external CLI, or Antigravity IDE), and Manvi Managed runs
//! across worktrees via:
//! - `<git-common-dir>/gitpulse/lanes/{peers.json, messages.json}` (under `.bus.lock`),
//! - `<config>/agent-mailbox/<provider_session_uuid>/` (under `0600` `flock`),
//! - `desktop::instance::Request::AgentDeliver` (`#[cfg(target_os = "macos")]` when
//!   a live GitPulse GUI owner is running).

use crate::agent_mailbox::{EnvelopeProvenance, Mailbox};
use crate::engine::agent_bus::{
    self, AgentBusMessage, ClaimConflict, PeerResolutionError, PeerUpsert,
};
use crate::ipc_error::CommandError;
use notify::{Config, RecommendedWatcher, RecursiveMode, Watcher};
use serde_json::{json, Value};
use std::collections::BTreeSet;
use std::path::Path;
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::sync::{mpsc, Condvar, Mutex, OnceLock};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

/// Environment variable overriding `gitpulse-mcp`'s per-request call budget.
pub const CALL_BUDGET_ENV: &str = "GITPULSE_MCP_CALL_TIMEOUT_MS";
/// Default MCP per-call watchdog budget (`60s`).
pub const DEFAULT_CALL_BUDGET_MS: u64 = 60_000;
/// Hard ceiling on `GITPULSE_MCP_CALL_TIMEOUT_MS` (`10m`).
pub const MAX_CALL_BUDGET_MS: u64 = 10 * 60 * 1_000;
/// Maximum concurrent `gitpulse_agent_wait` calls per MCP server process so
/// parked waiters can never starve `MAX_IN_FLIGHT` (`32`).
pub const MAX_WAIT_CALLS: usize = 4;
/// Hard ceiling on a single `gitpulse_agent_wait` call (`45s`), strictly inside
/// the `60s` default MCP call budget.
pub const MAX_WAIT_TIMEOUT_MS: u64 = 45_000;

static ACTIVE_WAIT_CALLS: AtomicUsize = AtomicUsize::new(0);
static RPC_REQ_SEQ: AtomicU64 = AtomicU64::new(1);

fn in_process_bus_waker() -> &'static (Mutex<u64>, Condvar) {
    static WAKER: OnceLock<(Mutex<u64>, Condvar)> = OnceLock::new();
    WAKER.get_or_init(|| (Mutex::new(0), Condvar::new()))
}

pub(crate) fn notify_in_process_waiters() {
    let (lock, cvar) = in_process_bus_waker();
    if let Ok(mut gen) = lock.lock() {
        *gen = gen.wrapping_add(1);
        cvar.notify_all();
    }
}

/// Effective per-call budget in milliseconds from `GITPULSE_MCP_CALL_TIMEOUT_MS`.
pub fn effective_call_budget_ms() -> u64 {
    std::env::var(CALL_BUDGET_ENV)
        .ok()
        .and_then(|raw| raw.parse::<u64>().ok())
        .filter(|ms| *ms > 0)
        .map_or(DEFAULT_CALL_BUDGET_MS, |ms| ms.min(MAX_CALL_BUDGET_MS))
}

/// Clamps requested `timeout_ms` to `min(requested_ms, 45_000, call_budget.saturating_sub(5_000))`.
pub fn clamp_wait_timeout_ms(requested_ms: Option<u64>, call_budget_ms: u64) -> u64 {
    let requested = requested_ms.unwrap_or(15_000);
    requested
        .min(MAX_WAIT_TIMEOUT_MS)
        .min(call_budget_ms.saturating_sub(5_000))
}

pub(crate) struct WaitCallSlot<'a> {
    counter: &'a AtomicUsize,
}

impl<'a> WaitCallSlot<'a> {
    pub(crate) fn acquire(counter: &'a AtomicUsize) -> Result<Self, String> {
        let mut cur = counter.load(Ordering::Relaxed);
        loop {
            if cur >= MAX_WAIT_CALLS {
                return Err(format!(
                    "wait_slots_saturated: at most {MAX_WAIT_CALLS} concurrent gitpulse_agent_wait calls are allowed per server"
                ));
            }
            match counter.compare_exchange_weak(cur, cur + 1, Ordering::AcqRel, Ordering::Relaxed) {
                Ok(_) => return Ok(Self { counter }),
                Err(actual) => cur = actual,
            }
        }
    }
}

impl Drop for WaitCallSlot<'_> {
    fn drop(&mut self) {
        self.counter.fetch_sub(1, Ordering::AcqRel);
    }
}

fn next_rpc_id(prefix: &str) -> String {
    let seq = RPC_REQ_SEQ.fetch_add(1, Ordering::Relaxed);
    let ms = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| u64::try_from(d.as_millis()).unwrap_or(0));
    format!("{prefix}-{}-{ms}-{seq}", std::process::id())
}

#[allow(clippy::too_many_arguments)]
pub(crate) fn reach_desktop_agent_deliver(
    repo_root: &str,
    peer_id: &str,
    msg_id: &str,
    spawn: Option<String>,
    provider: Option<String>,
    branch: Option<String>,
    prompt: Option<String>,
    task_id: Option<String>,
) -> Option<Result<Value, String>> {
    #[cfg(target_os = "macos")]
    {
        let profile = crate::browser::profile_dir()?;
        let owner = crate::desktop::instance::reach_owner(&profile, Duration::from_millis(800))?;
        let req = crate::desktop::instance::Request::AgentDeliver {
            id: next_rpc_id("agtdlv"),
            repo: repo_root.to_string(),
            peer_id: peer_id.to_string(),
            msg_id: msg_id.to_string(),
            spawn,
            provider,
            branch,
            prompt,
            task_id,
        };
        Some(owner.agent_deliver(&req, Duration::from_millis(3_200)))
    }
    #[cfg(not(target_os = "macos"))]
    {
        let _ = (
            repo_root, peer_id, msg_id, spawn, provider, branch, prompt, task_id,
        );
        None
    }
}

pub(crate) fn dispatch_kickoff_spawn(
    repo_path: &str,
    mode: &str,
    provider: &str,
    branch: &str,
    prompt: &str,
    task_id: &str,
    msg_id: &str,
) -> Result<String, String> {
    let root_repo = crate::engine::git_cli::find_git_root(Path::new(repo_path))
        .map(|p| p.to_string_lossy().into_owned())
        .unwrap_or_else(|| repo_path.to_string());
    match reach_desktop_agent_deliver(
        &root_repo,
        "kickoff",
        msg_id,
        Some(mode.to_string()),
        Some(provider.to_string()),
        Some(branch.to_string()),
        Some(prompt.to_string()),
        Some(task_id.to_string()),
    ) {
        Some(Ok(reply)) => Ok(reply["delivery_outcome"]
            .as_str()
            .unwrap_or("woken_immediately")
            .to_string()),
        Some(Err(err)) => Err(format!("desktop kickoff spawn failed: {err}")),
        None => Ok("owner_offline_queued".to_string()),
    }
}

pub(crate) fn handle_agent_register(a: super::AgentRegisterArgs) -> Result<Value, String> {
    crate::repository_trust::require(Path::new(&a.repo_path))
        .map_err(CommandError::into_message)?;
    crate::harness::guard_command(&a.repo_path, &["git", "worktree", "list"])?;

    let family = crate::engine::worktree::resolve_worktree_family(&a.repo_path, &a.repo_path)
        .map_err(CommandError::into_message)?;
    let worktree_path = family.worktree.to_string_lossy().into_owned();
    let branch = a.branch.clone().or_else(|| {
        crate::engine::worktree::list_worktrees(&a.repo_path)
            .ok()
            .and_then(|wts| {
                wts.into_iter()
                    .find(|w| w.path == worktree_path)
                    .and_then(|w| w.branch)
            })
    });

    let mut peer_id = a.peer_id.clone();
    let mut provider_session_id = a.provider_session_id.clone();
    let mut terminal_session_id = a.terminal_session_id.clone();
    let mut pid = a.pid;
    let mut proc_start_ns = pid.and_then(crate::workbench::process_birth::started_unix_nanos);

    let reconciled = agent_bus::list_reconciled_peers(&a.repo_path).unwrap_or_default();
    let inferred_from_id = peer_id.as_deref().and_then(|id| {
        id.split_once(':')
            .map(|(prefix, _)| prefix)
            .filter(|p| matches!(*p, "claude" | "agy" | "codex"))
            .map(str::to_owned)
    });
    let explicit_match = reconciled.iter().find(|p| {
        (peer_id.is_some() && Some(p.record.peer_id.as_str()) == peer_id.as_deref())
            || (provider_session_id.is_some()
                && p.record.provider_session_id.as_deref() == provider_session_id.as_deref())
            || (terminal_session_id.is_some()
                && p.record.terminal_session_id.as_deref() == terminal_session_id.as_deref())
            || (pid.is_some() && p.record.pid == pid)
    });

    let mut resolved_provider = a
        .provider
        .clone()
        .or(inferred_from_id)
        .or_else(|| explicit_match.map(|p| p.record.provider.clone()));

    if peer_id.is_none()
        && provider_session_id.is_none()
        && terminal_session_id.is_none()
        && pid.is_none()
    {
        let live_in_wt: Vec<_> = reconciled
            .into_iter()
            .filter(|p| {
                p.liveness == "live"
                    && p.record.worktree_path == worktree_path
                    && resolved_provider
                        .as_deref()
                        .is_none_or(|prov| p.record.provider == prov)
            })
            .collect();
        if live_in_wt.len() > 1 {
            return Err(PeerResolutionError::AmbiguousPeer {
                target: resolved_provider.unwrap_or_else(|| worktree_path.clone()),
                candidates: live_in_wt,
            }
            .into_command_error()
            .into_message());
        }
        if let Some(found) = live_in_wt.into_iter().next() {
            resolved_provider = Some(found.record.provider);
            peer_id = Some(found.record.peer_id);
            provider_session_id = found.record.provider_session_id;
            terminal_session_id = found.record.terminal_session_id;
            pid = found.record.pid;
            proc_start_ns = found
                .record
                .proc_start_ns
                .or_else(|| pid.and_then(crate::workbench::process_birth::started_unix_nanos));
        } else if let Some(ref prov) = resolved_provider {
            if let Some(ref role) = a.role {
                peer_id = Some(format!("{prov}:{role}"));
            } else {
                peer_id = Some(format!("{prov}:{}", branch.as_deref().unwrap_or("main")));
            }
        }
    }

    let Some(provider) = resolved_provider else {
        return Err(
            "missing_provider: pass provider ('claude', 'agy', or 'codex') when registering a peer without an active session in this worktree"
                .to_string(),
        );
    };

    let surface = a.surface.clone().unwrap_or_else(|| {
        if terminal_session_id.is_some() {
            "terminal".to_string()
        } else if a.run_id.is_some() {
            "managed".to_string()
        } else {
            "external_cli".to_string()
        }
    });
    let auto_wake = a.wake_mode.as_deref().map(|mode| mode == "auto");

    let peer = agent_bus::upsert_peer(
        &a.repo_path,
        PeerUpsert {
            peer_id,
            provider,
            surface,
            provider_session_id,
            terminal_session_id,
            run_id: a.run_id,
            worktree_path,
            branch: branch.clone(),
            role: a.role,
            turn_state: Some("running".to_string()),
            auto_wake,
            pid,
            proc_start_ns,
            transcript_path: None,
        },
    )
    .map_err(CommandError::into_message)?;

    let claim_conflicts: Vec<ClaimConflict> =
        if a.claimed_files.is_some() || a.claimed_symbols.is_some() {
            let claim_branch = peer
                .branch
                .clone()
                .or(branch)
                .unwrap_or_else(|| "main".to_string());
            let (_, conflicts) = agent_bus::register_or_update_lane_with_conflicts(
                &a.repo_path,
                &peer.peer_id,
                &claim_branch,
                Some(&peer.worktree_path),
                &[],
                a.claimed_files.as_deref().unwrap_or(&[]),
                a.claimed_symbols.as_deref().unwrap_or(&[]),
            )
            .map_err(CommandError::into_message)?;
            conflicts
        } else {
            Vec::new()
        };

    Ok(json!({
        "ok": true,
        "repo_path": a.repo_path,
        "peer": peer,
        "claim_conflicts": claim_conflicts,
    }))
}

pub(crate) fn handle_agent_peers(a: super::AgentPeersArgs) -> Result<Value, String> {
    crate::repository_trust::require(Path::new(&a.repo_path))
        .map_err(CommandError::into_message)?;
    let mut peers =
        agent_bus::list_reconciled_peers(&a.repo_path).map_err(CommandError::into_message)?;
    if let Some(ref provider) = a.provider {
        peers.retain(|p| &p.record.provider == provider);
    }
    if !a.include_exited.unwrap_or(false) {
        peers.retain(|p| p.liveness != "exited");
    }
    Ok(json!({
        "ok": true,
        "repo_path": a.repo_path,
        "total": peers.len(),
        "peers": peers,
    }))
}

pub(crate) fn handle_agent_send(a: super::AgentSendArgs) -> Result<Value, String> {
    crate::repository_trust::require(Path::new(&a.repo_path))
        .map_err(CommandError::into_message)?;
    crate::harness::guard_command(&a.repo_path, &["git", "worktree", "list"])?;

    let target = agent_bus::resolve_target_peer(&a.repo_path, &a.to_peer)
        .map_err(|e| e.into_command_error().into_message())?;

    let is_human = matches!(
        a.from_peer.trim(),
        "developer" | "human" | "human-dev" | "user"
    );
    let hop = if is_human {
        0
    } else if let Some(explicit) = a.hop {
        explicit
    } else if let Some(ref tk) = a.thread_key {
        agent_bus::read_thread_messages(&a.repo_path, Some(tk), None, 0)
            .ok()
            .and_then(|page| page.messages.last().map(|m| m.hop.unwrap_or(0) + 1))
            .unwrap_or(1)
    } else {
        1
    };
    let max_hops = a.max_hops.unwrap_or(agent_bus::DEFAULT_MAX_HOPS);

    let root_repo = crate::engine::git_cli::find_git_root(Path::new(&a.repo_path))
        .map(|p| p.to_string_lossy().into_owned())
        .unwrap_or_else(|| a.repo_path.clone());

    let message = agent_bus::post_peer_message(
        &a.repo_path,
        &a.from_peer,
        Some(&target.record.peer_id),
        a.thread_key.as_deref(),
        a.kind.as_deref().unwrap_or("note"),
        &a.body,
        target.record.branch.as_deref(),
        a.msg_id.as_deref(),
        hop,
        max_hops,
    )
    .map_err(CommandError::into_message)?;
    let msg_id = message
        .msg_id
        .clone()
        .unwrap_or_else(|| format!("msg-{}", message.seq));

    let sender_provider = a
        .from_peer
        .split_once(':')
        .map(|(p, _)| p.to_string())
        .unwrap_or_else(|| "peer".to_string());

    let mut truncated_in_mailbox = false;
    if let Some(ref sid) = target.record.provider_session_id {
        if crate::workbench::terminal_command::is_canonical_uuid(sid) {
            if let Ok(mb) = Mailbox::at_default() {
                let (preview, truncated) =
                    crate::agent_mailbox::preview_for_mailbox(&message.body, &msg_id);
                truncated_in_mailbox = truncated;
                let provenance = if is_human {
                    EnvelopeProvenance::HumanConfirmed
                } else {
                    EnvelopeProvenance::PeerAgent {
                        msg_id: msg_id.clone(),
                        from_peer: message.from_agent.clone(),
                        provider: sender_provider,
                        branch: message.branch.clone().unwrap_or_default(),
                        thread_key: message.thread_key.clone().unwrap_or_default(),
                        hop,
                        max_hops,
                        truncated_in_bus: truncated,
                        repo_root: root_repo.clone(),
                    }
                };
                let _ =
                    mb.deposit_with_provenance(sid, &msg_id, &preview, provenance, SystemTime::now());
            }
        }
    }

    notify_in_process_waiters();

    let delivery_outcome = if !is_human && hop >= max_hops {
        "paused_hop_limit".to_string()
    } else if target.liveness != "live" {
        if target.liveness == "resumable" {
            "requires_resume".to_string()
        } else {
            "queued_for_hook_boundary".to_string()
        }
    } else if let Some(Ok(reply)) = reach_desktop_agent_deliver(
        &root_repo,
        &target.record.peer_id,
        &msg_id,
        None,
        None,
        None,
        None,
        None,
    ) {
        reply["delivery_outcome"]
            .as_str()
            .unwrap_or("queued_for_hook_boundary")
            .to_string()
    } else {
        let listening = target
            .record
            .provider_session_id
            .as_deref()
            .and_then(|sid| {
                Mailbox::at_default()
                    .ok()
                    .map(|mb| mb.listening(sid, SystemTime::now()))
            })
            .unwrap_or(false);
        if listening {
            "woken_immediately".to_string()
        } else {
            "queued_for_hook_boundary".to_string()
        }
    };

    Ok(json!({
        "ok": true,
        "repo_path": a.repo_path,
        "msg_id": msg_id,
        "to_peer": target.record.peer_id,
        "liveness": target.liveness,
        "delivery_outcome": delivery_outcome,
        "hop": hop,
        "max_hops": max_hops,
        "truncated_in_mailbox": truncated_in_mailbox,
        "message": message,
    }))
}

struct DedupedInboxResult {
    messages: Vec<AgentBusMessage>,
    latest_seq: u64,
    dropped_before_seq: u64,
    has_more: bool,
    acked_count: usize,
}

#[allow(clippy::too_many_arguments)]
fn collect_deduped_peer_inbox(
    repo_path: &str,
    for_peer: &str,
    explicit_session_id: Option<&str>,
    since_seq: u64,
    thread_key: Option<&str>,
    include_broadcasts: bool,
    ack: bool,
    limit: usize,
) -> Result<(DedupedInboxResult, Option<String>), String> {
    let clean_peer = for_peer.trim();
    if clean_peer.is_empty() {
        return Err("for_peer must not be empty".into());
    }

    let mut aliases = BTreeSet::new();
    aliases.insert(clean_peer.to_string());
    let mut canonical_reader = clean_peer.to_string();
    let mut resolved_session_id = explicit_session_id
        .map(str::trim)
        .filter(|s| crate::workbench::terminal_command::is_canonical_uuid(s))
        .map(|s| s.to_ascii_lowercase());

    if let Ok(reconciled) = agent_bus::list_reconciled_peers(repo_path) {
        let matching: Vec<_> = reconciled
            .into_iter()
            .filter(|p| {
                p.record.peer_id == clean_peer
                    || p.record.role.as_deref() == Some(clean_peer)
                    || p.record
                        .provider_session_id
                        .as_deref()
                        .is_some_and(|s| s.eq_ignore_ascii_case(clean_peer))
                    || (resolved_session_id.is_some()
                        && p.record.provider_session_id == resolved_session_id)
            })
            .collect();
        if matching.len() == 1 {
            let m = &matching[0].record;
            canonical_reader = m.peer_id.clone();
            aliases.insert(m.peer_id.clone());
            if let Some(ref sid) = m.provider_session_id {
                aliases.insert(sid.clone());
                if resolved_session_id.is_none() {
                    resolved_session_id = Some(sid.clone());
                }
            }
            if let Some(ref role) = m.role {
                aliases.insert(role.clone());
            }
            if let Some(ref tid) = m.terminal_session_id {
                aliases.insert(tid.clone());
            }
            if let Some(ref rid) = m.run_id {
                aliases.insert(rid.clone());
            }
        }
    }
    if let Some(ref sid) = resolved_session_id {
        aliases.insert(sid.clone());
    }

    let alias_vec: Vec<String> = aliases.into_iter().collect();
    let bus_page = agent_bus::read_messages_for_peer(
        repo_path,
        &alias_vec,
        since_seq,
        thread_key,
        include_broadcasts,
        true,
    )
    .map_err(CommandError::into_message)?;

    let mut seen_ids = BTreeSet::new();
    let mut combined = Vec::new();
    for msg in bus_page.messages {
        let eff_id = msg
            .msg_id
            .clone()
            .unwrap_or_else(|| format!("msg-{}", msg.seq));
        if seen_ids.insert(eff_id.clone()) {
            let mut normalized = msg;
            if normalized.msg_id.is_none() {
                normalized.msg_id = Some(eff_id);
            }
            combined.push(normalized);
        }
    }

    // Merge direct mailbox envelopes (e.g. UI Send-to-Agent or pointer envelopes)
    // and deduplicate by `msg_id` (`request_id`).
    if let (Some(ref sid), Ok(mb)) = (&resolved_session_id, Mailbox::at_default()) {
        let now = SystemTime::now();
        let envelopes = if ack {
            mb.try_claim_batch(sid, limit.saturating_add(1), now)
                .unwrap_or_default()
        } else {
            mb.peek_pending(sid, limit.saturating_add(1), now)
                .unwrap_or_default()
        };
        for env in envelopes {
            let env_id = match &env.provenance {
                EnvelopeProvenance::PeerAgent { msg_id, .. } => msg_id.clone(),
                EnvelopeProvenance::HumanConfirmed => env.claim.request_id.clone(),
            };
            if !seen_ids.insert(env_id.clone()) {
                continue;
            }
            if let EnvelopeProvenance::PeerAgent {
                ref msg_id,
                ref from_peer,
                ref branch,
                ref thread_key,
                hop,
                max_hops,
                truncated_in_bus,
                ..
            } = env.provenance
            {
                let hydrated = if truncated_in_bus {
                    agent_bus::get_message_by_id(repo_path, msg_id)
                        .ok()
                        .flatten()
                } else {
                    None
                };
                if let Some(full_msg) = hydrated {
                    combined.push(full_msg);
                } else {
                    combined.push(AgentBusMessage {
                        seq: bus_page.latest_seq,
                        from_agent: from_peer.clone(),
                        to_agent: Some(canonical_reader.clone()),
                        kind: "note".to_string(),
                        body: env.claim.text,
                        branch: (!branch.is_empty()).then(|| branch.clone()),
                        created_at_ms: now
                            .duration_since(UNIX_EPOCH)
                            .map_or(0, |d| u64::try_from(d.as_millis()).unwrap_or(0)),
                        thread_key: (!thread_key.is_empty()).then(|| thread_key.clone()),
                        msg_id: Some(env_id),
                        read_by: Vec::new(),
                        hop: Some(hop),
                        max_hops: Some(max_hops),
                    });
                }
            } else {
                combined.push(AgentBusMessage {
                    seq: bus_page.latest_seq,
                    from_agent: "developer".to_string(),
                    to_agent: Some(canonical_reader.clone()),
                    kind: "human".to_string(),
                    body: env.claim.text,
                    branch: None,
                    created_at_ms: now
                        .duration_since(UNIX_EPOCH)
                        .map_or(0, |d| u64::try_from(d.as_millis()).unwrap_or(0)),
                    thread_key: None,
                    msg_id: Some(env_id),
                    read_by: Vec::new(),
                    hop: Some(0),
                    max_hops: Some(agent_bus::DEFAULT_MAX_HOPS),
                });
            }
        }
    }

    let has_more = combined.len() > limit;
    if has_more {
        combined.truncate(limit);
    }

    let mut acked_count = 0usize;
    if ack && !combined.is_empty() {
        let returned_ids: Vec<String> = combined
            .iter()
            .filter_map(|m| m.msg_id.clone())
            .collect();
        acked_count = agent_bus::ack_messages(repo_path, &canonical_reader, &returned_ids)
            .unwrap_or(0);
        if let (Some(ref sid), Ok(mb)) = (&resolved_session_id, Mailbox::at_default()) {
            let now = SystemTime::now();
            for mid in &returned_ids {
                if mb.mark_claimed_by_id(sid, mid, now).unwrap_or(false) && acked_count == 0 {
                    acked_count += 1;
                }
            }
        }
    }

    Ok((
        DedupedInboxResult {
            messages: combined,
            latest_seq: bus_page.latest_seq,
            dropped_before_seq: bus_page.dropped_before_seq,
            has_more,
            acked_count,
        },
        resolved_session_id,
    ))
}

pub(crate) fn handle_agent_inbox(a: super::AgentInboxArgs) -> Result<Value, String> {
    crate::repository_trust::require(Path::new(&a.repo_path))
        .map_err(CommandError::into_message)?;
    let ack = a.ack.unwrap_or(true);
    let limit = a.limit.map(|l| l as usize).unwrap_or(50);
    let (res, _) = collect_deduped_peer_inbox(
        &a.repo_path,
        &a.for_peer,
        a.provider_session_id.as_deref(),
        a.since_seq.unwrap_or(0),
        a.thread_key.as_deref(),
        a.include_broadcasts.unwrap_or(false),
        ack,
        limit,
    )?;
    Ok(json!({
        "ok": true,
        "repo_path": a.repo_path,
        "for_peer": a.for_peer,
        "acked": ack,
        "acked_count": res.acked_count,
        "latest_seq": res.latest_seq,
        "dropped_before_seq": res.dropped_before_seq,
        "has_more": res.has_more,
        "messages": res.messages,
    }))
}

pub(crate) fn handle_agent_wait(a: super::AgentWaitArgs) -> Result<Value, String> {
    handle_agent_wait_with_counter(a, &ACTIVE_WAIT_CALLS, effective_call_budget_ms())
}

pub(crate) fn handle_agent_wait_with_counter(
    a: super::AgentWaitArgs,
    counter: &AtomicUsize,
    call_budget_ms: u64,
) -> Result<Value, String> {
    crate::repository_trust::require(Path::new(&a.repo_path))
        .map_err(CommandError::into_message)?;
    let _slot = WaitCallSlot::acquire(counter)?;
    let effective_timeout_ms = clamp_wait_timeout_ms(a.timeout_ms, call_budget_ms);
    let ack = a.ack.unwrap_or(true);
    let limit = a.limit.map(|l| l as usize).unwrap_or(20);
    let since_seq = a.since_seq.unwrap_or(0);
    let include_broadcasts = a.include_broadcasts.unwrap_or(false);
    let started = Instant::now();

    let (initial, resolved_sid) = collect_deduped_peer_inbox(
        &a.repo_path,
        &a.for_peer,
        a.provider_session_id.as_deref(),
        since_seq,
        a.thread_key.as_deref(),
        include_broadcasts,
        ack,
        limit,
    )?;
    if !initial.messages.is_empty() || effective_timeout_ms == 0 {
        let waited_ms = u64::try_from(started.elapsed().as_millis()).unwrap_or(0);
        return Ok(json!({
            "ok": true,
            "repo_path": a.repo_path,
            "for_peer": a.for_peer,
            "timed_out": initial.messages.is_empty(),
            "waited_ms": waited_ms,
            "effective_timeout_ms": effective_timeout_ms,
            "acked": ack,
            "acked_count": initial.acked_count,
            "latest_seq": initial.latest_seq,
            "messages": initial.messages,
        }));
    }

    let messages_path = agent_bus::lanes_messages_path(&a.repo_path, true)
        .map_err(CommandError::into_message)?;
    let (fs_tx, fs_rx) = mpsc::sync_channel::<()>(8);
    let waker_tx = fs_tx.clone();
    let mut watcher = RecommendedWatcher::new(
        move |res: Result<notify::Event, notify::Error>| {
            if res.is_ok() {
                let _ = waker_tx.try_send(());
            }
        },
        Config::default(),
    )
    .ok();

    if let Some(ref mut w) = watcher {
        if let Some(lanes_dir) = messages_path.parent() {
            let watch_target = lanes_dir
                .canonicalize()
                .unwrap_or_else(|_| lanes_dir.to_path_buf());
            let _ = w.watch(&watch_target, RecursiveMode::NonRecursive);
        }
        if let (Some(ref sid), Ok(mb)) = (&resolved_sid, Mailbox::at_default()) {
            if let Ok(session_dir) = mb.session_path(sid) {
                if let Ok(canon) = session_dir.canonicalize() {
                    let _ = w.watch(&canon, RecursiveMode::NonRecursive);
                } else if let Some(parent) = session_dir.parent() {
                    if let Ok(canon_parent) = parent.canonicalize() {
                        let _ = w.watch(&canon_parent, RecursiveMode::NonRecursive);
                    }
                }
            }
        }
    }

    // Re-check immediately after arming the watcher so a write between the
    // initial check and `w.watch(...)` cannot be missed.
    let (after_arm, _) = collect_deduped_peer_inbox(
        &a.repo_path,
        &a.for_peer,
        a.provider_session_id.as_deref(),
        since_seq,
        a.thread_key.as_deref(),
        include_broadcasts,
        ack,
        limit,
    )?;
    if !after_arm.messages.is_empty() {
        let waited_ms = u64::try_from(started.elapsed().as_millis()).unwrap_or(0);
        return Ok(json!({
            "ok": true,
            "repo_path": a.repo_path,
            "for_peer": a.for_peer,
            "timed_out": false,
            "waited_ms": waited_ms,
            "effective_timeout_ms": effective_timeout_ms,
            "acked": ack,
            "acked_count": after_arm.acked_count,
            "latest_seq": after_arm.latest_seq,
            "messages": after_arm.messages,
        }));
    }

    let deadline = started + Duration::from_millis(effective_timeout_ms);
    let mut last_stat = std::fs::metadata(&messages_path)
        .ok()
        .and_then(|m| m.modified().ok().map(|t| (m.len(), t)));
    let (bus_lock, bus_cvar) = in_process_bus_waker();
    let mut seen_gen = bus_lock.lock().map(|g| *g).unwrap_or(0);

    loop {
        let now = Instant::now();
        if now >= deadline {
            break;
        }
        let rem = deadline.saturating_duration_since(now);
        // Wait up to 50ms per step on in-process condvar or fs_rx, with explicit
        // 500ms stat fallback on `lanes/messages.json`.
        let step = rem.min(Duration::from_millis(50));
        if fs_rx.try_recv().is_err() {
            if let Ok(guard) = bus_lock.lock() {
                if *guard == seen_gen {
                    let _ = bus_cvar.wait_timeout(guard, step);
                }
            } else {
                let _ = fs_rx.recv_timeout(step);
            }
        }
        while fs_rx.try_recv().is_ok() {}

        let current_gen = bus_lock.lock().map(|g| *g).unwrap_or(seen_gen);
        let current_stat = std::fs::metadata(&messages_path)
            .ok()
            .and_then(|m| m.modified().ok().map(|t| (m.len(), t)));
        let changed = current_gen != seen_gen
            || current_stat != last_stat
            || started.elapsed().as_millis() % 500 < 60;
        seen_gen = current_gen;
        last_stat = current_stat;

        if !changed && Instant::now() < deadline {
            // Also check `fs_rx` if an OS kqueue/FSEvents notification arrived.
            if fs_rx.recv_timeout(Duration::from_millis(1)).is_err() {
                continue;
            }
        }

        let (polled, _) = collect_deduped_peer_inbox(
            &a.repo_path,
            &a.for_peer,
            a.provider_session_id.as_deref(),
            since_seq,
            a.thread_key.as_deref(),
            include_broadcasts,
            ack,
            limit,
        )?;
        if !polled.messages.is_empty() {
            let waited_ms = u64::try_from(started.elapsed().as_millis()).unwrap_or(0);
            return Ok(json!({
                "ok": true,
                "repo_path": a.repo_path,
                "for_peer": a.for_peer,
                "timed_out": false,
                "waited_ms": waited_ms,
                "effective_timeout_ms": effective_timeout_ms,
                "acked": ack,
                "acked_count": polled.acked_count,
                "latest_seq": polled.latest_seq,
                "messages": polled.messages,
            }));
        }
    }

    let waited_ms = u64::try_from(started.elapsed().as_millis()).unwrap_or(effective_timeout_ms);
    Ok(json!({
        "ok": true,
        "repo_path": a.repo_path,
        "for_peer": a.for_peer,
        "timed_out": true,
        "waited_ms": waited_ms,
        "effective_timeout_ms": effective_timeout_ms,
        "acked": ack,
        "acked_count": 0,
        "latest_seq": initial.latest_seq,
        "messages": Vec::<AgentBusMessage>::new(),
    }))
}
