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

use crate::agent_mailbox::{EnvelopeProvenance, Mailbox, MailboxError, PeerDeposit};
use crate::engine::agent_bus::{
    self, AgentBusMessage, ClaimConflict, PeerResolutionError, PeerUpsert,
};
use crate::ipc_error::CommandError;
use notify::{Config, RecommendedWatcher, RecursiveMode, Watcher};
use serde_json::{json, Value};
use std::collections::BTreeSet;
use std::path::Path;
#[cfg(target_os = "macos")]
use std::sync::atomic::AtomicU64;
use std::sync::atomic::{AtomicUsize, Ordering};
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
// Only the macOS branch of `reach_desktop_agent_deliver` mints a request id.
#[cfg(target_os = "macos")]
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

#[cfg(target_os = "macos")]
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

    if let Some(ref explicit_provider) = a.provider {
        agent_bus::require_agent_sender(explicit_provider).map_err(CommandError::into_message)?;
    }
    if let Some(ref explicit_id) = a.peer_id {
        agent_bus::require_agent_sender(explicit_id).map_err(CommandError::into_message)?;
    }
    if let Some(ref explicit_role) = a.role {
        agent_bus::require_role_not_steward(explicit_role).map_err(CommandError::into_message)?;
    }

    crate::harness::guard_command(&a.repo_path, &["git", "worktree", "list"])?;

    let worktree_pb =
        match crate::engine::worktree::resolve_worktree_family(&a.repo_path, &a.repo_path) {
            Ok(family) => family.worktree,
            Err(err) if err.origin() == Some(crate::ipc_error::IpcErrorKind::DeferredUnderLoad) => {
                let pair = crate::engine::worktree::admit_worktree_pair(&a.repo_path, &a.repo_path)
                    .map_err(CommandError::into_message)?;
                let registered = pair.best_effort_registered_checkouts();
                if !registered.iter().any(|p| p == pair.repo_path()) {
                    return Err(err.into_message());
                }
                pair.repo_path().to_path_buf()
            }
            Err(err) => return Err(err.into_message()),
        };
    let worktree_path = worktree_pb.to_string_lossy().into_owned();
    let branch = a.branch.clone().or_else(|| {
        crate::engine::worktree::list_worktrees_lite(&a.repo_path)
            .ok()
            .and_then(|wts| {
                wts.into_iter()
                    .find(|w| w.path == worktree_path)
                    .and_then(|w| w.branch)
            })
            .or_else(|| crate::engine::worktree::head_branch_from_metadata(&worktree_pb))
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
            .filter(|p| matches!(*p, "claude" | "agy" | "antigravity" | "codex"))
            .map(agent_bus::canonical_provider)
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
        .as_deref()
        .map(agent_bus::canonical_provider)
        .or(inferred_from_id)
        .or_else(|| explicit_match.map(|p| agent_bus::canonical_provider(&p.record.provider)));

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
                        .is_none_or(|prov| agent_bus::providers_match(&p.record.provider, prov))
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
        peers.retain(|p| agent_bus::providers_match(&p.record.provider, provider));
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

/// Puts the envelope in the target session's mailbox and says what happened, so the send
/// result never claims a delivery the mailbox did not take.
fn deposit_into_mailbox(
    session: Option<&str>,
    msg_id: &str,
    preview: &str,
    provenance: EnvelopeProvenance,
) -> Value {
    let report = |deposited: bool,
                  reason: Option<&str>,
                  detail: Option<String>,
                  duplicate: bool,
                  overflow_notice: bool| {
        json!({
            "deposited": deposited,
            "reason": reason,
            "detail": detail,
            "duplicate": duplicate,
            "overflow_notice": overflow_notice,
        })
    };
    let Some(sid) = session.filter(|s| crate::workbench::terminal_command::is_canonical_uuid(s))
    else {
        return report(
            false,
            Some("no_session"),
            Some(
                "The target has no Claude session id, so there is no mailbox to deposit into."
                    .into(),
            ),
            false,
            false,
        );
    };
    let mb = match Mailbox::at_default() {
        Ok(mb) => mb,
        Err(e) => {
            return report(
                false,
                Some("unavailable"),
                Some(e.to_string()),
                false,
                false,
            )
        }
    };
    match mb.deposit_peer_or_overflow(sid, msg_id, preview, provenance, SystemTime::now()) {
        Ok(PeerDeposit::Deposited(d)) => report(true, None, None, d.duplicate, false),
        Ok(PeerDeposit::Overflowed { notice }) => report(
            false,
            Some("full"),
            Some(match &notice {
                Ok(_) => "The session's mailbox is full of peer messages; the message is on the bus and an overflow notice points the recipient at gitpulse_agent_inbox.".to_string(),
                Err(e) => format!("The session's mailbox is full of peer messages and the overflow notice could not be left: {e}"),
            }),
            false,
            notice.is_ok(),
        ),
        Err(e @ MailboxError::Full(_)) => report(false, Some("full"), Some(e.to_string()), false, false),
        Err(e @ MailboxError::Invalid(_)) => {
            report(false, Some("refused"), Some(e.to_string()), false, false)
        }
        Err(e) => report(false, Some("unavailable"), Some(e.to_string()), false, false),
    }
}

/// Where a send goes: a registered peer, or the identity of an unregistered sender being
/// answered, which has no session and so no mailbox.
struct SendTarget {
    peer_id: String,
    branch: Option<String>,
    session_id: Option<String>,
    liveness: String,
}

pub(crate) fn handle_agent_send(a: super::AgentSendArgs) -> Result<Value, String> {
    crate::repository_trust::require(Path::new(&a.repo_path))
        .map_err(CommandError::into_message)?;
    crate::harness::guard_command(&a.repo_path, &["git", "worktree", "list"])?;

    // The message being answered is read first: it supplies the thread and the hop, and a
    // reply to a message that is not on the bus is refused instead of sent unthreaded.
    let answered = match a
        .in_reply_to
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
    {
        Some(id) => {
            if !crate::agent_mailbox::valid_request_id(id) {
                return Err("in_reply_to must be 1 to 64 ASCII letters, digits, '-' or '_'".into());
            }
            let found = agent_bus::get_message_by_id(&a.repo_path, id)
                .map_err(CommandError::into_message)?;
            Some(found.ok_or_else(|| {
                format!(
                    "in_reply_to '{id}' names no message on the bus; it may have been evicted, so send without in_reply_to or name a retained msg_id"
                )
            })?)
        }
        None => None,
    };
    let thread_key: Option<String> = a
        .thread_key
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_string)
        .or_else(|| answered.as_ref().and_then(|m| m.thread_key.clone()));

    let target = match agent_bus::resolve_target_peer(&a.repo_path, &a.to_peer) {
        Ok(peer) => SendTarget {
            peer_id: peer.record.peer_id,
            branch: peer.record.branch,
            session_id: peer.record.provider_session_id,
            liveness: peer.liveness,
        },
        // A sender that never registered cannot be found as a peer, but a reply to it must
        // still work: the message it sent is on the bus under that exact identity. Only an
        // exact match counts, and an ambiguous peer stays an error.
        Err(agent_bus::PeerResolutionError::NotFound { target }) => {
            match agent_bus::find_bus_sender(&a.repo_path, &a.to_peer)
                .map_err(CommandError::into_message)?
            {
                Some(identity) => SendTarget {
                    peer_id: identity,
                    branch: None,
                    session_id: None,
                    liveness: "unregistered".to_string(),
                },
                None => {
                    return Err(agent_bus::PeerResolutionError::NotFound { target }
                        .into_command_error()
                        .into_message())
                }
            }
        }
        Err(e) => return Err(e.into_command_error().into_message()),
    };

    // An MCP caller is always an agent: it is never the person using GitPulse, whatever
    // `from_peer` says. `post_peer_message` refuses the names reserved for a person.
    let origin = agent_bus::MessageOrigin::Agent;
    // The count only climbs: an explicit hop can raise it but never lower what the answered
    // message or the thread already reached, or a caller could restart the loop guard.
    let mut requested_hop = a.hop.unwrap_or(1).max(1);
    if let Some(m) = &answered {
        requested_hop = requested_hop.max(m.hop.unwrap_or(0).saturating_add(1));
    }
    if let Some(tk) = &thread_key {
        // A failed read must fail the send: guessing hop 1 would reset the ping-pong count.
        let last = agent_bus::read_thread_messages(&a.repo_path, Some(tk), None, 0)
            .map_err(CommandError::into_message)?
            .messages
            .last()
            .map(|m| m.hop.unwrap_or(0).saturating_add(1));
        requested_hop = requested_hop.max(last.unwrap_or(1));
    }
    let hop = origin.effective_hop(requested_hop);
    let max_hops = a.max_hops.unwrap_or(agent_bus::DEFAULT_MAX_HOPS);

    let root_repo = crate::engine::git_cli::find_git_root(Path::new(&a.repo_path))
        .map(|p| p.to_string_lossy().into_owned())
        .unwrap_or_else(|| a.repo_path.clone());

    let message = agent_bus::post_peer_message(
        &a.repo_path,
        origin,
        &a.from_peer,
        Some(&target.peer_id),
        thread_key.as_deref(),
        a.kind.as_deref().unwrap_or("note"),
        &a.body,
        target.branch.as_deref(),
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

    let (preview, truncated_in_mailbox) =
        crate::agent_mailbox::preview_for_mailbox(&message.body, &msg_id);
    let provenance = EnvelopeProvenance::PeerAgent {
        msg_id: msg_id.clone(),
        from_peer: message.from_agent.clone(),
        provider: sender_provider,
        branch: message.branch.clone().unwrap_or_default(),
        thread_key: message.thread_key.clone().unwrap_or_default(),
        hop,
        max_hops,
        truncated_in_bus: truncated_in_mailbox,
        repo_root: root_repo.clone(),
    };
    let mailbox = deposit_into_mailbox(target.session_id.as_deref(), &msg_id, &preview, provenance);
    let deposited = mailbox["deposited"].as_bool().unwrap_or(false);

    notify_in_process_waiters();

    // The mailbox is all the push paths read, so a message that never reached it can only
    // be surfaced by an inbox or wait call. A desktop answer wins only when the desktop used
    // a channel independent of the mailbox ("managed" run delivery or the "banner" event);
    // its "mailbox" outcomes, and replies from an older desktop with no channel, describe
    // the mailbox and say nothing when the deposit failed.
    let desktop_outcome = if hop < max_hops && target.liveness == "live" {
        match reach_desktop_agent_deliver(
            &root_repo,
            &target.peer_id,
            &msg_id,
            None,
            None,
            None,
            None,
            None,
        ) {
            Some(Ok(reply)) => {
                let independent = matches!(reply["channel"].as_str(), Some("managed" | "banner"));
                (independent || deposited).then(|| {
                    reply["delivery_outcome"]
                        .as_str()
                        .unwrap_or("queued_for_hook_boundary")
                        .to_string()
                })
            }
            _ => None,
        }
    } else {
        None
    };
    let delivery_outcome = if hop >= max_hops {
        "paused_hop_limit".to_string()
    } else if let Some(outcome) = desktop_outcome {
        outcome
    } else if !deposited {
        "bus_only".to_string()
    } else if target.liveness != "live" {
        if target.liveness == "resumable" {
            "requires_resume".to_string()
        } else {
            "queued_for_hook_boundary".to_string()
        }
    } else {
        let listening = target
            .session_id
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
        "to_peer": target.peer_id,
        "liveness": target.liveness,
        "delivery_outcome": delivery_outcome,
        "hop": hop,
        "max_hops": max_hops,
        "truncated_in_mailbox": truncated_in_mailbox,
        "mailbox": mailbox,
        "message": message,
    }))
}

struct DedupedInboxResult {
    messages: Vec<AgentBusMessage>,
    latest_seq: u64,
    dropped_before_seq: u64,
    pinned_below_dropped_before_seq: Vec<u64>,
    has_more: bool,
    acked_count: usize,
    /// Why acknowledging failed, when it did. The messages were already collected, so a failed
    /// acknowledgement is reported beside them instead of discarding them.
    ack_error: Option<String>,
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

    {
        let reconciled = agent_bus::list_reconciled_peers(repo_path)
            .map_err(|e| format!("peer_list_failed: {}", e.into_message()))?;
        let mut matching: Vec<_> = reconciled
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
        // A named session id is the disambiguator the refusal below asks for, so it narrows
        // several matches down to the peer that owns that session.
        if matching.len() > 1 && resolved_session_id.is_some() {
            matching.retain(|p| p.record.provider_session_id == resolved_session_id);
        }
        if matching.len() > 1 {
            let ids: Vec<&str> = matching.iter().map(|p| p.record.peer_id.as_str()).collect();
            return Err(format!(
                "ambiguous_peer: for_peer '{clean_peer}' matched {} peers [{}]; name one by peer_id or provider_session_id",
                matching.len(),
                ids.join(", ")
            ));
        }
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

    let alias_vec: Vec<String> = aliases.iter().cloned().collect();
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

    // Merge direct mailbox envelopes (e.g. UI Send-to-Agent, steward directives, or
    // pointer envelopes) and deduplicate by `msg_id` (`request_id`). Peek first and
    // claim only returned envelopes below so `thread_key` filters, `since_seq` bounds,
    // cross-repository scopes, and `limit` caps never consume unreturned mailbox messages.
    let t_filter = thread_key.map(str::trim).filter(|s| !s.is_empty());
    let caller_common = crate::harness::steward::canonical_common_dir(repo_path);
    let mut mailbox_req_by_msg_id = std::collections::HashMap::<String, String>::new();
    let mut ack_errors: Vec<String> = Vec::new();
    if let Some(ref sid) = resolved_session_id {
        let mb = Mailbox::at_default().map_err(|e| format!("mailbox_unavailable: {e}"))?;
        let now = SystemTime::now();
        let now_ms = now
            .duration_since(UNIX_EPOCH)
            .map_or(0, |d| u64::try_from(d.as_millis()).unwrap_or(0));
        let envelopes = mb
            .peek_pending(sid, crate::agent_mailbox::MAX_PENDING, now)
            .map_err(|e| format!("mailbox_read_failed: {e}"))?;
        for env in envelopes {
            let env_repo_root = match &env.provenance {
                EnvelopeProvenance::PeerAgent { repo_root, .. }
                | EnvelopeProvenance::SystemSteward { repo_root, .. }
                    if !repo_root.trim().is_empty() =>
                {
                    Some(repo_root.trim())
                }
                _ => None,
            };
            if let (Some(ref want_common), Some(recorded_root)) = (&caller_common, env_repo_root) {
                let matches_repo = crate::harness::steward::canonical_common_dir(recorded_root)
                    .as_ref()
                    == Some(want_common);
                if !matches_repo {
                    continue;
                }
            }

            let env_id = match &env.provenance {
                EnvelopeProvenance::PeerAgent { msg_id, .. }
                | EnvelopeProvenance::SystemSteward { msg_id, .. } => msg_id.clone(),
                EnvelopeProvenance::HumanConfirmed => env.claim.request_id.clone(),
            };
            let env_created_ms = if env.created_at_ms > 0 {
                env.created_at_ms
            } else {
                now_ms
            };
            if !seen_ids.contains(&env_id) {
                let (synthesized, from_bus, has_bus_seq) = match env.provenance {
                    EnvelopeProvenance::PeerAgent {
                        ref msg_id,
                        ref from_peer,
                        ref branch,
                        ref thread_key,
                        hop,
                        max_hops,
                        ..
                    } => {
                        match agent_bus::get_message_by_id(repo_path, msg_id)
                            .map_err(CommandError::into_message)?
                        {
                            Some(bus_msg) => (bus_msg, true, true),
                            None => (
                                AgentBusMessage {
                                    seq: bus_page.latest_seq,
                                    from_agent: from_peer.clone(),
                                    to_agent: Some(canonical_reader.clone()),
                                    kind: "note".to_string(),
                                    body: env.claim.text,
                                    branch: (!branch.is_empty()).then(|| branch.clone()),
                                    created_at_ms: env_created_ms,
                                    thread_key: (!thread_key.is_empty())
                                        .then(|| thread_key.clone()),
                                    msg_id: Some(env_id.clone()),
                                    read_by: Vec::new(),
                                    hop: Some(hop),
                                    max_hops: Some(max_hops),
                                },
                                false,
                                false,
                            ),
                        }
                    }
                    EnvelopeProvenance::SystemSteward {
                        ref msg_id,
                        bus_seq,
                        ..
                    } => {
                        match agent_bus::get_message_by_id(repo_path, msg_id)
                            .map_err(CommandError::into_message)?
                        {
                            Some(bus_msg) => (bus_msg, true, true),
                            None => (
                                AgentBusMessage {
                                    seq: if bus_seq > 0 {
                                        bus_seq
                                    } else {
                                        bus_page.latest_seq
                                    },
                                    from_agent: agent_bus::STEWARD_SENDER.to_string(),
                                    to_agent: None,
                                    kind: agent_bus::STEWARD_MESSAGE_KIND.to_string(),
                                    body: env.claim.text,
                                    branch: None,
                                    created_at_ms: env_created_ms,
                                    thread_key: Some(agent_bus::STEWARD_THREAD_KEY.to_string()),
                                    msg_id: Some(env_id.clone()),
                                    read_by: Vec::new(),
                                    hop: Some(0),
                                    max_hops: Some(1),
                                },
                                false,
                                bus_seq > 0,
                            ),
                        }
                    }
                    EnvelopeProvenance::HumanConfirmed => (
                        AgentBusMessage {
                            seq: bus_page.latest_seq,
                            from_agent: "developer".to_string(),
                            to_agent: Some(canonical_reader.clone()),
                            kind: "human".to_string(),
                            body: env.claim.text,
                            branch: None,
                            created_at_ms: env_created_ms,
                            thread_key: None,
                            msg_id: Some(env_id.clone()),
                            read_by: Vec::new(),
                            hop: Some(0),
                            max_hops: Some(agent_bus::DEFAULT_MAX_HOPS),
                        },
                        false,
                        false,
                    ),
                };
                if from_bus && synthesized.read_by.iter().any(|r| aliases.contains(r)) {
                    if ack {
                        if let Err(e) = mb.mark_claimed_by_id(sid, &env.claim.request_id, now) {
                            ack_errors.push(format!("mailbox claim {}: {e}", env.claim.request_id));
                        }
                    }
                    continue;
                }
                if (from_bus || has_bus_seq) && synthesized.seq <= since_seq {
                    continue;
                }
                if let Some(tk) = t_filter {
                    if synthesized.thread_key.as_deref() != Some(tk) {
                        continue;
                    }
                }
                mailbox_req_by_msg_id.insert(env_id.clone(), env.claim.request_id);
                seen_ids.insert(env_id);
                combined.push(synthesized);
            } else {
                mailbox_req_by_msg_id.insert(env_id, env.claim.request_id);
            }
        }
    }

    let has_more = combined.len() > limit;
    if has_more {
        combined.truncate(limit);
    }

    let mut acked_count = 0usize;
    if ack && !combined.is_empty() {
        let returned_ids: Vec<String> = combined.iter().filter_map(|m| m.msg_id.clone()).collect();
        let bus_acked = match agent_bus::ack_messages(repo_path, &canonical_reader, &returned_ids) {
            Ok(n) => n,
            Err(e) => {
                ack_errors.push(format!("bus ack: {}", e.into_message()));
                0
            }
        };
        let mut mb_only_acked = 0usize;
        if let Some(ref sid) = resolved_session_id {
            match Mailbox::at_default() {
                Ok(mb) => {
                    let now = SystemTime::now();
                    let mut claim = |id: &str| match mb.mark_claimed_by_id(sid, id, now) {
                        Ok(claimed) => claimed,
                        Err(e) => {
                            ack_errors.push(format!("mailbox claim {id}: {e}"));
                            false
                        }
                    };
                    for mid in &returned_ids {
                        let mut claimed = claim(mid);
                        if let Some(req_id) = mailbox_req_by_msg_id.get(mid) {
                            if req_id != mid {
                                claimed |= claim(req_id);
                            }
                        }
                        if claimed {
                            mb_only_acked += 1;
                        }
                    }
                }
                Err(e) => ack_errors.push(format!("mailbox unavailable: {e}")),
            }
        }
        acked_count = bus_acked.max(mb_only_acked);
    }

    Ok((
        DedupedInboxResult {
            messages: combined,
            latest_seq: bus_page.latest_seq,
            dropped_before_seq: bus_page.dropped_before_seq,
            pinned_below_dropped_before_seq: bus_page.pinned_below_dropped_before_seq,
            has_more,
            acked_count,
            ack_error: (!ack_errors.is_empty()).then(|| ack_errors.join("; ")),
        },
        resolved_session_id,
    ))
}

pub(crate) fn handle_agent_inbox(a: super::AgentInboxArgs) -> Result<Value, String> {
    crate::repository_trust::require(Path::new(&a.repo_path))
        .map_err(CommandError::into_message)?;
    // Reading is a peek unless the caller asks to acknowledge: any caller may name any peer in
    // for_peer, so a default acknowledgement would let one agent consume another's messages.
    let ack = a.ack.unwrap_or(false);
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
        "ack_error": res.ack_error,
        "latest_seq": res.latest_seq,
        "dropped_before_seq": res.dropped_before_seq,
        "pinned_below_dropped_before_seq": res.pinned_below_dropped_before_seq,
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
        return Ok(wait_response(
            &a,
            ack,
            initial.messages.is_empty(),
            waited_ms,
            effective_timeout_ms,
            initial,
        ));
    }

    let messages_path =
        agent_bus::lanes_messages_path(&a.repo_path, true).map_err(CommandError::into_message)?;
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
        return Ok(wait_response(
            &a,
            ack,
            false,
            waited_ms,
            effective_timeout_ms,
            after_arm,
        ));
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
            return Ok(wait_response(
                &a,
                ack,
                false,
                waited_ms,
                effective_timeout_ms,
                polled,
            ));
        }
    }

    let waited_ms = u64::try_from(started.elapsed().as_millis()).unwrap_or(effective_timeout_ms);
    Ok(wait_response(
        &a,
        ack,
        true,
        waited_ms,
        effective_timeout_ms,
        initial,
    ))
}

/// Every return of a wait carries the same envelope as an inbox read, so a waiter can tell a
/// capped page (`has_more`) or an evicted range (`dropped_before_seq`) from a complete one.
fn wait_response(
    a: &super::AgentWaitArgs,
    ack: bool,
    timed_out: bool,
    waited_ms: u64,
    effective_timeout_ms: u64,
    res: DedupedInboxResult,
) -> Value {
    json!({
        "ok": true,
        "repo_path": a.repo_path,
        "for_peer": a.for_peer,
        "timed_out": timed_out,
        "waited_ms": waited_ms,
        "effective_timeout_ms": effective_timeout_ms,
        "acked": ack,
        "acked_count": res.acked_count,
        "ack_error": res.ack_error,
        "latest_seq": res.latest_seq,
        "dropped_before_seq": res.dropped_before_seq,
        "pinned_below_dropped_before_seq": res.pinned_below_dropped_before_seq,
        "has_more": res.has_more,
        "messages": res.messages,
    })
}
