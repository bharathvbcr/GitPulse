//! Unified single-owner control-plane service trait (`ControlPlaneService`)
//! behind `control-plane.lock` (Opus R4).
//!
//! Both Waylode Desktop (local / Free mode) and `wayloded --relay` (paid
//! remote-control mode) host alerts + [`ApprovalBroker`] through this single
//! interface, owned strictly by whoever holds `control-plane.lock`. There are
//! zero duplicate socket-ownership paths:
//!
//! - When Desktop owns `control-plane.lock`, it runs [`InProcessControlPlane`]
//!   and binds `agent-notify.sock` + `approval-broker.sock`.
//! - When `wayloded --relay` owns `control-plane.lock`, it runs the exact
//!   same [`InProcessControlPlane`], and Desktop consumes read-only state via
//!   [`SocketControlPlaneClient`].
//! - Contract tests below exercise the shared [`ControlPlaneService`] surface
//!   against both implementations.

use serde::{Deserialize, Serialize};
use std::fs::{File, OpenOptions};
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::sync::Arc;

use super::approval_broker::{
    ApprovalBroker, ApprovalDecision, ApprovalTicketSummary, BrokerError, BrokerStatus,
    ResolveOutcome, SignedApprovalDecision,
};
use super::{HubStateRead, HubStateSnapshot, SessionAlertStatus, StandingAttention};

/// Single-owner lock file inside Waylode's config directory (`0600` in `0700`).
pub const LOCK_NAME: &str = "control-plane.lock";

/// Which process currently owns `control-plane.lock` and the local control sockets.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ControlPlaneOwnerMode {
    Desktop,
    Daemon,
}

/// Metadata written inside `control-plane.lock` by the active lock holder.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ControlPlaneOwner {
    pub pid: u32,
    pub start_us: u64,
    pub mode: ControlPlaneOwnerMode,
    pub broker_socket: String,
}

/// Advisory `flock`-backed ownership lease on `control-plane.lock`.
/// Dropping releases the lock cleanly.
pub struct ControlPlaneLock {
    path: PathBuf,
    owner: ControlPlaneOwner,
    #[cfg_attr(not(unix), allow(dead_code))]
    file: File,
}

impl ControlPlaneLock {
    /// Attempts to claim single ownership of `config_dir/control-plane.lock`
    /// without blocking. Returns `Ok(None)` when another live process already
    /// holds the lock.
    pub fn try_acquire(
        config_dir: &Path,
        mode: ControlPlaneOwnerMode,
        broker_socket: &Path,
    ) -> Result<Option<Self>, String> {
        #[cfg(unix)]
        crate::local_socket::private_dir(config_dir)?;
        #[cfg(not(unix))]
        std::fs::create_dir_all(config_dir)
            .map_err(|e| format!("could not create {}: {e}", config_dir.display()))?;

        let path = config_dir.join(LOCK_NAME);
        let mut opts = OpenOptions::new();
        opts.read(true).write(true).create(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            opts.mode(0o600).custom_flags(libc::O_NOFOLLOW);
        }
        let mut file = opts
            .open(&path)
            .map_err(|e| format!("could not open {}: {e}", path.display()))?;

        #[cfg(unix)]
        {
            use std::os::unix::fs::MetadataExt;
            use std::os::unix::io::AsRawFd;
            let meta = file
                .metadata()
                .map_err(|e| format!("could not inspect {}: {e}", path.display()))?;
            // SAFETY: `geteuid` has no preconditions and cannot fail.
            let me = unsafe { libc::geteuid() };
            if !meta.is_file() || meta.uid() != me || meta.mode() & 0o077 != 0 {
                return Err(format!(
                    "{} is not a private 0600 lock file of this user",
                    path.display()
                ));
            }
            // SAFETY: `file` is an open descriptor owned by this function.
            let rc = unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) };
            if rc != 0 {
                let err = std::io::Error::last_os_error();
                if err.raw_os_error() == Some(libc::EWOULDBLOCK)
                    || err.kind() == std::io::ErrorKind::WouldBlock
                {
                    return Ok(None);
                }
                return Err(format!("could not flock {}: {err}", path.display()));
            }
        }

        let pid = std::process::id();
        let start_us = crate::sysload::process_start_us(pid).unwrap_or(0);
        let owner = ControlPlaneOwner {
            pid,
            start_us,
            mode,
            broker_socket: broker_socket.display().to_string(),
        };
        let encoded = serde_json::to_vec(&owner)
            .map_err(|e| format!("could not serialize control-plane owner: {e}"))?;
        file.set_len(0)
            .and_then(|()| file.seek(SeekFrom::Start(0)))
            .and_then(|_| file.write_all(&encoded))
            .and_then(|()| file.sync_all())
            .map_err(|e| format!("could not write {}: {e}", path.display()))?;

        Ok(Some(Self { path, owner, file }))
    }

    pub fn owner(&self) -> &ControlPlaneOwner {
        &self.owner
    }

    pub fn path(&self) -> &Path {
        &self.path
    }
}

impl Drop for ControlPlaneLock {
    fn drop(&mut self) {
        #[cfg(unix)]
        {
            use std::os::unix::io::AsRawFd;
            // SAFETY: `self.file` remains valid until after `drop` returns.
            let _ = unsafe { libc::flock(self.file.as_raw_fd(), libc::LOCK_UN) };
        }
    }
}

/// Inspects the recorded owner in `config_dir/control-plane.lock`, if readable
/// and valid. Does not acquire the lock.
pub fn inspect_owner(config_dir: &Path) -> Option<ControlPlaneOwner> {
    let path = config_dir.join(LOCK_NAME);
    let file = crate::fs_entry::open_private_record_readonly(&path).ok()??;
    let mut buf = Vec::new();
    file.take(8 * 1024).read_to_end(&mut buf).ok()?;
    serde_json::from_slice(&buf).ok()
}

/// Unified control-plane service surface shared by Waylode Desktop and
/// `wayloded --relay` (Opus R4).
pub trait ControlPlaneService: Send + Sync {
    /// Current session-alerts worker and bridge counters.
    fn alerts_status(&self) -> SessionAlertStatus;

    /// Standing attention items across all tracked sessions.
    fn standing_attention(&self) -> Vec<StandingAttention>;

    /// Latest live hook-derived session snapshot (including `active_subagents`
    /// and `fully_idle`), when available.
    fn session_snapshot(&self) -> Option<HubStateSnapshot>;

    /// Read-only approval broker health and presence state.
    fn broker_status(&self) -> Result<BrokerStatus, BrokerError>;

    /// Lists all currently pending presence-gated approval tickets.
    fn pending_approvals(&self) -> Result<Vec<ApprovalTicketSummary>, BrokerError>;

    /// Resolves an open ticket from an in-process Desktop UI click.
    /// Socket clients MUST refuse this operation (Opus P0-3 / #15).
    fn resolve_approval_in_process(
        &self,
        ticket_id: &str,
        decision: ApprovalDecision,
        reason: Option<String>,
    ) -> Result<ResolveOutcome, BrokerError>;

    /// Resolves an open ticket with a cryptographically verified P-256 mobile
    /// signature.
    fn resolve_approval_signed(
        &self,
        signed: &SignedApprovalDecision,
    ) -> Result<ResolveOutcome, BrokerError>;

    /// Records a `HumanConfirmed` mailbox issuance strictly in the owner
    /// process's memory (never via a Unix socket op).
    fn record_human_issuance(
        &self,
        session_id: &str,
        request_id: &str,
        text: &str,
        created_at_ms: u64,
    ) -> Result<u64, BrokerError>;

    /// Read-only check verifying a `HumanConfirmed` mailbox envelope.
    fn verify_human_issuance(&self, session_id: &str, request_id: &str, text_sha256: &str) -> bool;

    /// Updates whether a paired paid remote device is currently connected.
    fn set_remote_approval_active(&self, active: bool) -> Result<(), BrokerError>;

    /// Immediately releases all open approval holds back to native prompts.
    fn release_all_holds(&self, reason: &str) -> Result<(), BrokerError>;
}

/// In-process control-plane implementation held by whoever owns `control-plane.lock`.
pub struct InProcessControlPlane {
    lock: ControlPlaneLock,
    broker: Arc<ApprovalBroker>,
    snapshot_path: Option<PathBuf>,
}

impl InProcessControlPlane {
    pub fn new(
        lock: ControlPlaneLock,
        broker: Arc<ApprovalBroker>,
        snapshot_path: Option<PathBuf>,
    ) -> Self {
        Self {
            lock,
            broker,
            snapshot_path,
        }
    }

    pub fn owner(&self) -> &ControlPlaneOwner {
        self.lock.owner()
    }

    pub fn broker(&self) -> &Arc<ApprovalBroker> {
        &self.broker
    }
}

impl ControlPlaneService for InProcessControlPlane {
    fn alerts_status(&self) -> SessionAlertStatus {
        super::status()
    }

    fn standing_attention(&self) -> Vec<StandingAttention> {
        super::standing()
    }

    fn session_snapshot(&self) -> Option<HubStateSnapshot> {
        match super::read_state_snapshot_at(self.snapshot_path.as_deref()) {
            HubStateRead::Ok(snap) => Some(snap),
            HubStateRead::Unavailable(_) => None,
        }
    }

    fn broker_status(&self) -> Result<BrokerStatus, BrokerError> {
        Ok(self.broker.status())
    }

    fn pending_approvals(&self) -> Result<Vec<ApprovalTicketSummary>, BrokerError> {
        Ok(self.broker.pending_tickets())
    }

    fn resolve_approval_in_process(
        &self,
        ticket_id: &str,
        decision: ApprovalDecision,
        reason: Option<String>,
    ) -> Result<ResolveOutcome, BrokerError> {
        Ok(self.broker.resolve_in_process(ticket_id, decision, reason))
    }

    fn resolve_approval_signed(
        &self,
        signed: &SignedApprovalDecision,
    ) -> Result<ResolveOutcome, BrokerError> {
        self.broker.resolve_signed(signed)
    }

    fn record_human_issuance(
        &self,
        session_id: &str,
        request_id: &str,
        text: &str,
        created_at_ms: u64,
    ) -> Result<u64, BrokerError> {
        Ok(self
            .broker
            .record_human_issuance(session_id, request_id, text, created_at_ms))
    }

    fn verify_human_issuance(&self, session_id: &str, request_id: &str, text_sha256: &str) -> bool {
        self.broker
            .verify_human_issuance(session_id, request_id, text_sha256)
    }

    fn set_remote_approval_active(&self, active: bool) -> Result<(), BrokerError> {
        self.broker.set_remote_approval_active(active);
        Ok(())
    }

    fn release_all_holds(&self, reason: &str) -> Result<(), BrokerError> {
        self.broker.release_all_holds(reason);
        Ok(())
    }
}

/// Read-only socket-backed client consumed when another process (e.g.
/// `wayloded --relay`) holds `control-plane.lock`.
///
/// Enforces Opus #15: mutating/resolution calls are rejected on the socket
/// client and never sent over the local Unix socket.
#[cfg(unix)]
pub struct SocketControlPlaneClient {
    broker_socket: PathBuf,
    snapshot_path: Option<PathBuf>,
}

#[cfg(unix)]
impl SocketControlPlaneClient {
    pub fn new(broker_socket: PathBuf, snapshot_path: Option<PathBuf>) -> Self {
        Self {
            broker_socket,
            snapshot_path,
        }
    }
}

#[cfg(unix)]
impl ControlPlaneService for SocketControlPlaneClient {
    fn alerts_status(&self) -> SessionAlertStatus {
        super::status()
    }

    fn standing_attention(&self) -> Vec<StandingAttention> {
        super::standing()
    }

    fn session_snapshot(&self) -> Option<HubStateSnapshot> {
        match super::read_state_snapshot_at(self.snapshot_path.as_deref()) {
            HubStateRead::Ok(snap) => Some(snap),
            HubStateRead::Unavailable(_) => None,
        }
    }

    fn broker_status(&self) -> Result<BrokerStatus, BrokerError> {
        super::approval_broker::query_status_via_socket(&self.broker_socket)
            .map_err(BrokerError::Io)
    }

    fn pending_approvals(&self) -> Result<Vec<ApprovalTicketSummary>, BrokerError> {
        super::approval_broker::list_pending_tickets_via_socket(&self.broker_socket)
            .map_err(BrokerError::Io)
    }

    fn resolve_approval_in_process(
        &self,
        _ticket_id: &str,
        _decision: ApprovalDecision,
        _reason: Option<String>,
    ) -> Result<ResolveOutcome, BrokerError> {
        Err(BrokerError::Io(
            "in-process approval resolution is forbidden over the Unix socket (Opus P0-3 / #15)"
                .into(),
        ))
    }

    fn resolve_approval_signed(
        &self,
        _signed: &SignedApprovalDecision,
    ) -> Result<ResolveOutcome, BrokerError> {
        Err(BrokerError::Io(
            "signed approval resolution must be verified in-process by the control-plane owner"
                .into(),
        ))
    }

    fn record_human_issuance(
        &self,
        _session_id: &str,
        _request_id: &str,
        _text: &str,
        _created_at_ms: u64,
    ) -> Result<u64, BrokerError> {
        Err(BrokerError::Io(
            "recording human issuance is forbidden over the Unix socket (Opus #15)".into(),
        ))
    }

    fn verify_human_issuance(&self, session_id: &str, request_id: &str, text_sha256: &str) -> bool {
        super::approval_broker::verify_human_issuance_via_socket(
            &self.broker_socket,
            session_id,
            request_id,
            text_sha256,
        )
    }

    fn set_remote_approval_active(&self, _active: bool) -> Result<(), BrokerError> {
        Err(BrokerError::Io(
            "remote_approval_active is owned by the control-plane lock holder".into(),
        ))
    }

    fn release_all_holds(&self, _reason: &str) -> Result<(), BrokerError> {
        Err(BrokerError::Io(
            "release_all_holds is owned by the control-plane lock holder".into(),
        ))
    }
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use crate::agent_mailbox::HumanIssuanceLedger;
    use crate::alerts::approval_broker::{
        ApprovalBrokerServer, MockPresenceProvider, RegisterApprovalRequest,
        TrustedApprovalKeyStore, UserPresence, SOCKET_NAME,
    };
    use serde_json::json;
    use std::time::Duration;

    fn assert_shared_read_contract(
        service: &dyn ControlPlaneService,
        expected_host: &str,
        expected_ticket_id: &str,
        human_session: &str,
        human_req: &str,
        human_hash: &str,
    ) {
        let status = service.broker_status().expect("broker status");
        assert_eq!(status.host_id, expected_host);
        assert!(status.remote_approval_active);
        assert_eq!(status.pending_tickets, 1);

        let tickets = service.pending_approvals().expect("pending approvals");
        assert_eq!(tickets.len(), 1);
        assert_eq!(tickets[0].ticket_id, expected_ticket_id);

        assert!(service.verify_human_issuance(human_session, human_req, human_hash));
        assert!(!service.verify_human_issuance(
            human_session,
            human_req,
            &crate::agent_mailbox::text_sha256_hex("forged")
        ));
    }

    #[test]
    fn control_plane_lock_enforces_single_owner_and_both_implementations_satisfy_contract() {
        let dir = tempfile::Builder::new().prefix("gp-cp-").tempdir().unwrap();
        let base = std::fs::canonicalize(dir.path()).unwrap();
        let sock = base.join(SOCKET_NAME);

        let lock = ControlPlaneLock::try_acquire(&base, ControlPlaneOwnerMode::Desktop, &sock)
            .unwrap()
            .expect("first owner acquires lock");
        assert_eq!(lock.owner().mode, ControlPlaneOwnerMode::Desktop);

        // Second process / caller cannot steal ownership while lock is held.
        let second =
            ControlPlaneLock::try_acquire(&base, ControlPlaneOwnerMode::Daemon, &sock).unwrap();
        assert!(second.is_none(), "control-plane.lock must be single-owner");
        let recorded = inspect_owner(&base).expect("owner metadata readable");
        assert_eq!(recorded.pid, std::process::id());
        assert_eq!(recorded.mode, ControlPlaneOwnerMode::Desktop);

        let presence = Arc::new(MockPresenceProvider::new(UserPresence::Away));
        let ledger = Arc::new(HumanIssuanceLedger::new());
        let keys = Arc::new(TrustedApprovalKeyStore::new());
        crate::alerts::approval_broker::configure_test_trust_anchor(&keys);
        let broker = Arc::new(
            ApprovalBroker::new("host-cp-1", base.join("agent-mailbox"), presence, keys)
                .with_repo_trust_checker(Arc::new(
                    crate::alerts::approval_broker::MockRepoTrustChecker::new(true),
                ))
                .with_human_ledger(ledger)
                .with_hold_ceiling(Duration::from_secs(30))
                .with_presence_poll_interval(Duration::from_millis(10)),
        );
        let _server = ApprovalBrokerServer::bind_and_start(sock.clone(), broker.clone())
            .expect("start broker server");

        let in_proc = InProcessControlPlane::new(lock, broker.clone(), None);
        let socket_client = SocketControlPlaneClient::new(sock.clone(), None);

        in_proc.set_remote_approval_active(true).unwrap();
        let msg = "human confirmed text";
        let hash = crate::agent_mailbox::text_sha256_hex(msg);
        in_proc
            .record_human_issuance("sess-1", "req-1", msg, 1000)
            .unwrap();

        // Spawn one pending approval waiter.
        let broker_wait = broker.clone();
        let waiter = std::thread::spawn(move || {
            broker_wait.register_and_wait_in_process(
                &RegisterApprovalRequest {
                    session_id: "sess-1".into(),
                    tool_name: "Bash".into(),
                    tool_input: json!({"command": "cargo test"}),
                    cwd: "/repo".into(),
                    tool_summary: "Bash: cargo test".into(),
                    wait_ms: Some(5_000),
                },
                || false,
            )
        });

        assert!(crate::test_support::wait_until(
            Duration::from_secs(2),
            || in_proc.pending_approvals().map_or(0, |t| t.len()) == 1
        ));
        let ticket_id = in_proc.pending_approvals().unwrap()[0].ticket_id.clone();

        // Both implementations answer the exact same read contract.
        assert_shared_read_contract(&in_proc, "host-cp-1", &ticket_id, "sess-1", "req-1", &hash);
        assert_shared_read_contract(
            &socket_client,
            "host-cp-1",
            &ticket_id,
            "sess-1",
            "req-1",
            &hash,
        );

        // Socket client MUST refuse in-process resolve and human issuance writes.
        assert!(socket_client
            .resolve_approval_in_process(&ticket_id, ApprovalDecision::AllowOnce, None)
            .is_err());
        assert!(socket_client
            .record_human_issuance("sess-1", "req-forged", "pwn", 2000)
            .is_err());

        // Owner resolves in-process cleanly.
        let outcome = in_proc
            .resolve_approval_in_process(&ticket_id, ApprovalDecision::AllowOnce, None)
            .unwrap();
        assert_eq!(
            outcome.status,
            crate::alerts::approval_broker::ResolveStatus::Applied
        );
        let _ = waiter.join().unwrap();
    }
}
