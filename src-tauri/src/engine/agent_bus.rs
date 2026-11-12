//! Lock-free/atomic multi-agent worktree coordination bus and 3-layer collision radar.
//!
//! Coordinates parallel coding agents working across sibling Git worktrees without
//! writing anything into working trees or Git refs. State lives exclusively under
//! `<git-common-dir>/gitpulse/lanes/`:
//! - `claims.json`: bounded registry of active agent lanes, sparse cones, and
//!   pre-edit file/symbol intent claims (`MAX_ACTIVE_LANES = 64`).
//! - `messages.json`: bounded monotonic ring of agent-to-agent handoff, contract-change,
//!   and blocker messages (`MAX_BUS_MESSAGES = 256`, `MAX_MESSAGE_BODY_BYTES = 16 KiB`).
//!
//! [`scan_parallel_agent_radar`] combines three orthogonal conflict detectors:
//! 1. **Intent Claim Overlaps**: pre-edit file/directory and symbol claim collisions.
//! 2. **Semantic `RoaringBitmap` + 1-Hop CSR Neighborhood Collisions**: detects both
//!    direct symbol-node collisions and cross-file caller/callee hazards (e.g. Agent A
//!    mutates `src/callee.rs` while Agent B edits its caller in `src/caller.rs`) over
//!    the zero-copy `CsrGraphReader`.
//! 3. **Quarantined ODB 3-Way `git merge-tree` Conflicts**: detects committed textual
//!    merge conflicts across worktree `HEAD`s in an isolated scratch object store.

use crate::codeintel::worktree_overlay::{
    compute_worktree_dirty_nodes_readonly, open_worktree_or_anchor_csr, DirtyNodeScan,
};
use crate::engine::git_cli::{resolve_git_common_dir, validate_repo, DEFAULT_TIMEOUT};
use crate::engine::object_quarantine::{MergeOutcome, ObjectQuarantine};
use crate::engine::worktree::{list_worktrees, WorktreeInfo};
use crate::ipc_error::CommandError;
use devmap_query::RoaringBitmap;
use serde::{Deserialize, Serialize};
use std::collections::{BTreeSet, HashMap};
use std::fs;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, OnceLock, PoisonError};
use std::time::{Instant, SystemTime, UNIX_EPOCH};

pub const MAX_ACTIVE_LANES: usize = 64;
pub const MAX_PEER_RECORDS: usize = 256;
pub const MAX_CLAIMED_ITEMS: usize = 128;
pub const MAX_BUS_MESSAGES: usize = 256;
pub const MAX_PINNED_OPEN_ASKS: usize = 64;
pub const MAX_MESSAGE_BODY_BYTES: usize = 16 * 1024;
pub const MAX_READ_BY_ENTRIES: usize = 64;
pub const DEFAULT_MAX_HOPS: u32 = 6;
pub const MAX_ODB_RADAR_PAIRS: usize = 32;
pub const MAX_RADAR_CONFLICTS_PER_LAYER: usize = 128;

const LANES_SUBDIR: &str = "gitpulse/lanes";
const CLAIMS_FILE: &str = "claims.json";
const MESSAGES_FILE: &str = "messages.json";
const PEERS_FILE: &str = "peers.json";

/// Registration and intent-claim record for one parallel agent lane.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct AgentLaneClaim {
    pub agent_id: String,
    pub branch: String,
    pub worktree_path: Option<String>,
    pub sparse_cone: Vec<String>,
    pub claimed_files: Vec<String>,
    pub claimed_symbols: Vec<String>,
    pub updated_at_ms: u64,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
struct ClaimsState {
    lanes: Vec<AgentLaneClaim>,
}

/// Persisted peer identity and session state in `<git-common-dir>/gitpulse/lanes/peers.json`.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct PeerRecord {
    pub peer_id: String,
    pub provider: String,
    pub surface: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provider_session_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub terminal_session_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub run_id: Option<String>,
    pub worktree_path: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub branch: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub role: Option<String>,
    pub turn_state: String,
    #[serde(default)]
    pub auto_wake: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pid: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub proc_start_ns: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub transcript_path: Option<String>,
    pub updated_at_ms: u64,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
struct PeersState {
    peers: Vec<PeerRecord>,
}

/// Reconciled peer view combining `peers.json` with live OS/lock/transcript truth.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ReconciledPeer {
    #[serde(flatten)]
    pub record: PeerRecord,
    /// `"live"` | `"resumable"` | `"exited"` | `"unknown"`
    pub liveness: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub liveness_reason: Option<String>,
}

/// Parameters to upsert a peer in `peers.json`.
#[derive(Debug, Clone, Default)]
pub struct PeerUpsert {
    pub peer_id: Option<String>,
    pub provider: String,
    pub surface: String,
    pub provider_session_id: Option<String>,
    pub terminal_session_id: Option<String>,
    pub run_id: Option<String>,
    pub worktree_path: String,
    pub branch: Option<String>,
    pub role: Option<String>,
    pub turn_state: Option<String>,
    pub auto_wake: Option<bool>,
    pub pid: Option<u32>,
    pub proc_start_ns: Option<u64>,
    pub transcript_path: Option<String>,
}

/// Fail-closed peer target resolution error.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PeerResolutionError {
    AmbiguousPeer {
        target: String,
        candidates: Vec<ReconciledPeer>,
    },
    NotFound {
        target: String,
    },
    Bus(CommandError),
}

impl PeerResolutionError {
    pub fn into_command_error(self) -> CommandError {
        match self {
            Self::AmbiguousPeer { target, candidates } => {
                let ids: Vec<String> = candidates
                    .iter()
                    .map(|c| {
                        format!(
                            "{} (provider={}, branch={}, session={})",
                            c.record.peer_id,
                            c.record.provider,
                            c.record.branch.as_deref().unwrap_or("-"),
                            c.record.provider_session_id.as_deref().unwrap_or("-")
                        )
                    })
                    .collect();
                CommandError::from(format!(
                    "ambiguous_peer: target '{target}' matched {} live sessions [{}]; pass an explicit peer_id or provider_session_id",
                    candidates.len(),
                    ids.join(", ")
                ))
            }
            Self::NotFound { target } => CommandError::from(format!(
                "peer_not_found: no live or resumable peer matched '{target}'"
            )),
            Self::Bus(err) => err,
        }
    }
}

/// Structured agent-to-agent coordination message stored under `<git-common-dir>/gitpulse/lanes/`.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct AgentBusMessage {
    pub seq: u64,
    pub from_agent: String,
    pub to_agent: Option<String>,
    pub kind: String,
    pub body: String,
    pub branch: Option<String>,
    pub created_at_ms: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub thread_key: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub msg_id: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub read_by: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub hop: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_hops: Option<u32>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
struct MessagesState {
    next_seq: u64,
    dropped_before_seq: u64,
    messages: Vec<AgentBusMessage>,
}

/// Bounded inbox page returned to an agent reading the coordination bus.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct AgentInboxPage {
    pub messages: Vec<AgentBusMessage>,
    pub latest_seq: u64,
    pub dropped_before_seq: u64,
}

/// Grouped conversation thread and structured ask status modeled on Vespin.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct AgentInboxThread {
    pub thread_key: String,
    pub title: String,
    pub status: String,
    pub ask_type: Option<String>,
    pub latest_msg: AgentBusMessage,
    pub messages: Vec<AgentBusMessage>,
    pub from_agent: String,
    pub to_agent: Option<String>,
    pub updated_at_ms: u64,
}

/// Pre-edit file or symbol claim collision between two parallel agent lanes.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ClaimConflict {
    pub agent_a: String,
    pub branch_a: String,
    pub agent_b: String,
    pub branch_b: String,
    pub overlapping_files: Vec<String>,
    pub overlapping_symbols: Vec<String>,
}

/// Cross-worktree semantic graph collision detected via `WorktreeOverlay` `RoaringBitmap`
/// and 1-hop `CsrGraphReader` caller/callee adjacency.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct SemanticLaneConflict {
    pub worktree_a: String,
    pub branch_a: Option<String>,
    pub worktree_b: String,
    pub branch_b: Option<String>,
    /// `"same_csr_node"` or `"caller_callee_1hop"`
    pub relation: String,
    pub direct_dirty_node_overlap: usize,
    pub caller_callee_linked_nodes: usize,
}

/// Quarantined 3-way committed merge conflict between two sibling worktrees.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct OdbPairConflict {
    pub worktree_a: String,
    pub branch_a: Option<String>,
    pub worktree_b: String,
    pub branch_b: Option<String>,
    /// `"conflict"` or `"unknown"`
    pub state: String,
    pub paths: Vec<String>,
    pub paths_total: usize,
    pub reason: Option<String>,
}

/// A radar layer that could not run for one worktree, with the reason. A worktree
/// listed here is in `worktrees_unscanned`, never in `worktrees_scanned`.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct RadarScanError {
    pub worktree: String,
    pub error: String,
}

/// Unified 3-layer parallel agent radar report.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ParallelAgentRadarReport {
    pub worktrees_scanned: usize,
    pub worktrees_unscanned: usize,
    pub lanes_registered: usize,
    pub csr_graph_available: bool,
    pub csr_generation: Option<u64>,
    pub claim_conflicts: Vec<ClaimConflict>,
    pub semantic_conflicts: Vec<SemanticLaneConflict>,
    pub odb_merge_conflicts: Vec<OdbPairConflict>,
    /// Any layer was capped: a conflict list, the ODB pair budget, or a
    /// worktree's changed-file list.
    pub truncated: bool,
    /// Why each unscanned worktree (or skipped layer) produced no answer. A
    /// worktree whose directory is missing is unscanned without an entry here.
    #[serde(default)]
    pub scan_errors: Vec<RadarScanError>,
}

/// Per-worktree dirty-node scan results, tallied so a failed scan is never counted as done.
#[derive(Default)]
struct OverlayScan {
    scanned: usize,
    unscanned: usize,
    truncated: bool,
    errors: Vec<RadarScanError>,
    overlays: Vec<(String, Option<String>, RoaringBitmap)>,
}

fn scan_worktree_overlays(
    worktrees: &[&WorktreeInfo],
    mut scan: impl FnMut(&WorktreeInfo) -> Result<DirtyNodeScan, CommandError>,
) -> OverlayScan {
    let mut out = OverlayScan::default();
    for wt in worktrees {
        if !Path::new(&wt.path).is_dir() {
            out.unscanned += 1;
            continue;
        }
        match scan(wt) {
            Ok(found) => {
                out.scanned += 1;
                out.truncated |= found.truncated;
                if !found.dirty_nodes.is_empty() {
                    out.overlays
                        .push((wt.path.clone(), wt.branch.clone(), found.dirty_nodes));
                }
            }
            Err(err) => {
                // The listing's `dirty_files` count says the worktree has changes, not
                // that its nodes were scanned; a failed scan is unscanned.
                out.unscanned += 1;
                out.errors.push(RadarScanError {
                    worktree: wt.path.clone(),
                    error: err.message().to_string(),
                });
            }
        }
    }
    out
}

fn now_epoch_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

// Both are read only by `acquire_bus_file_lock`'s unix arm.
#[cfg(unix)]
const BUS_LOCK_FILE: &str = ".bus.lock";
#[cfg(unix)]
const BUS_LOCK_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(10);

fn bus_lock_for(common_dir: &Path) -> Arc<Mutex<()>> {
    static LOCKS: OnceLock<Mutex<HashMap<PathBuf, Arc<Mutex<()>>>>> = OnceLock::new();
    let map_mutex = LOCKS.get_or_init(|| Mutex::new(HashMap::new()));
    let mut map = map_mutex.lock().unwrap_or_else(PoisonError::into_inner);
    Arc::clone(
        map.entry(common_dir.to_path_buf())
            .or_insert_with(|| Arc::new(Mutex::new(()))),
    )
}

/// An exclusive `flock(2)` held until dropped.
#[cfg(unix)]
pub(crate) struct AdvisoryFileLock {
    file: fs::File,
}

#[cfg(unix)]
impl Drop for AdvisoryFileLock {
    fn drop(&mut self) {
        use std::os::unix::io::AsRawFd;
        // SAFETY: `self.file` remains open across this call before closing on drop.
        unsafe {
            let _ = libc::flock(self.file.as_raw_fd(), libc::LOCK_UN);
        }
    }
}

#[cfg(unix)]
fn acquire_bus_file_lock(
    lanes_dir: &Path,
    create_dir: bool,
) -> Result<Option<AdvisoryFileLock>, CommandError> {
    use std::os::unix::fs::OpenOptionsExt;

    if create_dir {
        fs::create_dir_all(lanes_dir).map_err(|e| {
            CommandError::from(format!("cannot create {}: {e}", lanes_dir.display()))
        })?;
    }
    let lock_path = lanes_dir.join(BUS_LOCK_FILE);
    let file = match fs::OpenOptions::new()
        .read(true)
        .write(true)
        .create(create_dir)
        .truncate(false)
        .mode(0o600)
        .open(&lock_path)
    {
        Ok(f) => f,
        Err(e) if !create_dir && e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(e) => {
            return Err(CommandError::from(format!(
                "cannot open coordination lock {}: {e}",
                lock_path.display()
            )))
        }
    };
    lock_exclusive_within(file, &lock_path, BUS_LOCK_TIMEOUT, "coordination bus").map(Some)
}

/// Takes an exclusive `flock(2)` on the already-open `file`, retrying with
/// backoff (1–25 ms) until `timeout`. `what` names the lock in errors.
#[cfg(unix)]
fn lock_exclusive_within(
    file: fs::File,
    lock_path: &Path,
    timeout: std::time::Duration,
    what: &str,
) -> Result<AdvisoryFileLock, CommandError> {
    use std::os::unix::io::AsRawFd;
    let deadline = std::time::Instant::now() + timeout;
    let mut backoff_ms = 1u64;
    loop {
        // SAFETY: `file` is a valid open file descriptor owned by this stack frame.
        let rc = unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) };
        if rc == 0 {
            return Ok(AdvisoryFileLock { file });
        }
        let err = std::io::Error::last_os_error();
        if err.kind() == std::io::ErrorKind::Interrupted {
            continue;
        }
        let contended = err.kind() == std::io::ErrorKind::WouldBlock
            || err.raw_os_error() == Some(libc::EWOULDBLOCK)
            || err.raw_os_error() == Some(libc::EAGAIN);
        if !contended {
            return Err(CommandError::from(format!(
                "cannot lock {what} {}: {err}",
                lock_path.display()
            )));
        }
        if std::time::Instant::now() >= deadline {
            return Err(CommandError::from(format!(
                "timed out after {}s waiting for {what} lock {}",
                timeout.as_secs(),
                lock_path.display()
            )));
        }
        std::thread::sleep(std::time::Duration::from_millis(backoff_ms));
        backoff_ms = (backoff_ms * 2).min(25);
    }
}

/// An exclusive advisory lock on `lock_path`, created (mode 0600, with its
/// directory) when missing, waited for up to `timeout`. Another process
/// holding it is waited out; anything else is an error. `what` names the lock
/// in errors. Elsewhere than Unix there is no such lock and this is `None`.
#[cfg(unix)]
pub(crate) fn exclusive_file_lock(
    lock_path: &Path,
    timeout: std::time::Duration,
    what: &str,
) -> Result<Option<AdvisoryFileLock>, CommandError> {
    use std::os::unix::fs::OpenOptionsExt;
    if let Some(dir) = lock_path.parent() {
        fs::create_dir_all(dir)
            .map_err(|e| CommandError::from(format!("cannot create {}: {e}", dir.display())))?;
    }
    let file = fs::OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .mode(0o600)
        .open(lock_path)
        .map_err(|e| format!("cannot open {what} lock {}: {e}", lock_path.display()))?;
    lock_exclusive_within(file, lock_path, timeout, what).map(Some)
}

#[cfg(not(unix))]
pub(crate) fn exclusive_file_lock(
    _lock_path: &Path,
    _timeout: std::time::Duration,
    _what: &str,
) -> Result<Option<()>, CommandError> {
    Ok(None)
}

#[cfg(not(unix))]
fn acquire_bus_file_lock(lanes_dir: &Path, create_dir: bool) -> Result<Option<()>, CommandError> {
    if create_dir {
        fs::create_dir_all(lanes_dir).map_err(|e| {
            CommandError::from(format!("cannot create {}: {e}", lanes_dir.display()))
        })?;
    }
    Ok(None)
}

fn with_bus_lock<T>(
    lanes_dir: &Path,
    create_dir: bool,
    f: impl FnOnce() -> Result<T, CommandError>,
) -> Result<T, CommandError> {
    let lock = bus_lock_for(lanes_dir);
    let _in_proc = lock.lock().unwrap_or_else(PoisonError::into_inner);
    let _cross_proc = acquire_bus_file_lock(lanes_dir, create_dir)?;
    f()
}

fn lanes_dir_for_repo(repo_path: &str) -> Result<(PathBuf, PathBuf), CommandError> {
    let repo = validate_repo(repo_path)?;
    let common = resolve_git_common_dir(&repo)?;
    let dir = LANES_SUBDIR
        .split('/')
        .fold(common.clone(), |acc, part| acc.join(part));
    Ok((repo, dir))
}

fn read_json_bounded<T: for<'de> Deserialize<'de> + Default>(
    path: &Path,
    cap_bytes: u64,
) -> Result<T, CommandError> {
    let file = match fs::File::open(path) {
        Ok(f) => f,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(T::default()),
        Err(e) => {
            return Err(CommandError::from(format!(
                "cannot open {}: {e}",
                path.display()
            )))
        }
    };
    let mut buf = Vec::new();
    file.take(cap_bytes + 1)
        .read_to_end(&mut buf)
        .map_err(|e| CommandError::from(format!("cannot read {}: {e}", path.display())))?;
    if buf.len() as u64 > cap_bytes {
        return Err(CommandError::from(format!(
            "coordination state {} exceeds {} byte cap",
            path.display(),
            cap_bytes
        )));
    }
    if buf.is_empty() {
        return Ok(T::default());
    }
    serde_json::from_slice(&buf).map_err(|e| {
        CommandError::from(format!(
            "corrupt coordination state {}: {e}",
            path.display()
        ))
    })
}

fn sanitize_token(label: &str, val: &str, max_len: usize) -> Result<String, CommandError> {
    let trimmed = val.trim();
    if trimmed.is_empty() || trimmed.contains('\0') {
        return Err(CommandError::from(format!("{label} must not be empty")));
    }
    if trimmed.len() > max_len {
        return Err(CommandError::from(format!(
            "{label} exceeds maximum length of {max_len} bytes"
        )));
    }
    Ok(trimmed.to_string())
}

fn sanitize_claim_paths(items: &[String], label: &str) -> Result<Vec<String>, CommandError> {
    if items.len() > MAX_CLAIMED_ITEMS {
        return Err(CommandError::from(format!(
            "{label} exceeds maximum of {MAX_CLAIMED_ITEMS} items"
        )));
    }
    let mut set = BTreeSet::new();
    for raw in items {
        let trimmed = raw.trim();
        if trimmed.is_empty() || trimmed.contains('\0') {
            continue;
        }
        let slash_norm = trimmed.replace('\\', "/");
        let mut parts = Vec::new();
        for seg in slash_norm.split('/') {
            if seg.is_empty() || seg == "." {
                continue;
            }
            if seg == ".." {
                return Err(CommandError::from(format!(
                    "{label} must not contain '..' path traversal"
                )));
            }
            parts.push(seg);
        }
        if parts.is_empty() {
            continue;
        }
        let cleaned = parts.join("/");
        if cleaned.len() > 512 {
            return Err(CommandError::from(format!(
                "{label} entry exceeds 512 bytes"
            )));
        }
        set.insert(cleaned);
    }
    Ok(set.into_iter().collect())
}

fn sanitize_claim_symbols(items: &[String], label: &str) -> Result<Vec<String>, CommandError> {
    if items.len() > MAX_CLAIMED_ITEMS {
        return Err(CommandError::from(format!(
            "{label} exceeds maximum of {MAX_CLAIMED_ITEMS} items"
        )));
    }
    let mut set = BTreeSet::new();
    for raw in items {
        let cleaned = raw.trim();
        if cleaned.is_empty() || cleaned.contains('\0') {
            continue;
        }
        if cleaned.len() > 512 {
            return Err(CommandError::from(format!(
                "{label} entry exceeds 512 bytes"
            )));
        }
        set.insert(cleaned.to_string());
    }
    Ok(set.into_iter().collect())
}

/// Registers or updates an agent lane's active worktree, sparse cone, and pre-edit
/// file/symbol claims inside `<git-common-dir>/gitpulse/lanes/claims.json`, returning
/// both the persisted claim record and any immediate claim overlaps involving `agent_id`.
pub fn register_or_update_lane_with_conflicts(
    repo_path: &str,
    agent_id: &str,
    branch: &str,
    worktree_path: Option<&str>,
    sparse_cone: &[String],
    claimed_files: &[String],
    claimed_symbols: &[String],
) -> Result<(AgentLaneClaim, Vec<ClaimConflict>), CommandError> {
    let agent = sanitize_token("agent_id", agent_id, 128)?;
    let branch_clean = sanitize_token("branch", branch, 256)?;
    let cone_clean = sanitize_claim_paths(sparse_cone, "sparse_cone")?;
    let files_clean = sanitize_claim_paths(claimed_files, "claimed_files")?;
    let symbols_clean = sanitize_claim_symbols(claimed_symbols, "claimed_symbols")?;

    let (_repo, lanes_dir) = lanes_dir_for_repo(repo_path)?;
    with_bus_lock(&lanes_dir, true, || {
        let claims_path = lanes_dir.join(CLAIMS_FILE);
        let mut state: ClaimsState = read_json_bounded(&claims_path, 4 * 1024 * 1024)?;

        let record = AgentLaneClaim {
            agent_id: agent.clone(),
            branch: branch_clean,
            worktree_path: worktree_path
                .map(|s| s.trim().to_string())
                .filter(|s| !s.is_empty()),
            sparse_cone: cone_clean,
            claimed_files: files_clean,
            claimed_symbols: symbols_clean,
            updated_at_ms: now_epoch_ms(),
        };

        if let Some(existing) = state.lanes.iter_mut().find(|l| l.agent_id == agent) {
            *existing = record.clone();
        } else {
            if state.lanes.len() >= MAX_ACTIVE_LANES {
                return Err(CommandError::from(format!(
                    "parallel agent lane cap ({MAX_ACTIVE_LANES}) reached; release finished lanes first"
                )));
            }
            state.lanes.push(record.clone());
        }
        state.lanes.sort_by(|a, b| a.agent_id.cmp(&b.agent_id));

        let conflicts = compute_claim_conflicts(&state.lanes)
            .into_iter()
            .filter(|c| c.agent_a == agent || c.agent_b == agent)
            .collect();

        let bytes = serde_json::to_vec_pretty(&state)
            .map_err(|e| CommandError::from(format!("cannot serialize claims: {e}")))?;
        devmap_query::write_atomic(&claims_path, &bytes).map_err(|e| {
            CommandError::from(format!("cannot write {}: {e}", claims_path.display()))
        })?;

        Ok((record, conflicts))
    })
}

/// Registers or updates an agent lane's active worktree, sparse cone, and pre-edit
/// file/symbol claims inside `<git-common-dir>/gitpulse/lanes/claims.json`.
pub fn register_or_update_lane(
    repo_path: &str,
    agent_id: &str,
    branch: &str,
    worktree_path: Option<&str>,
    sparse_cone: &[String],
    claimed_files: &[String],
    claimed_symbols: &[String],
) -> Result<AgentLaneClaim, CommandError> {
    register_or_update_lane_with_conflicts(
        repo_path,
        agent_id,
        branch,
        worktree_path,
        sparse_cone,
        claimed_files,
        claimed_symbols,
    )
    .map(|(record, _)| record)
}

/// Removes an agent lane's claim record when its task completes or tears down.
pub fn release_lane(repo_path: &str, agent_id: &str) -> Result<bool, CommandError> {
    let agent = sanitize_token("agent_id", agent_id, 128)?;
    let (_repo, lanes_dir) = lanes_dir_for_repo(repo_path)?;
    with_bus_lock(&lanes_dir, false, || {
        let claims_path = lanes_dir.join(CLAIMS_FILE);
        if !claims_path.is_file() {
            return Ok(false);
        }
        let mut state: ClaimsState = read_json_bounded(&claims_path, 4 * 1024 * 1024)?;
        let before = state.lanes.len();
        state.lanes.retain(|l| l.agent_id != agent);
        if state.lanes.len() == before {
            return Ok(false);
        }
        let bytes = serde_json::to_vec_pretty(&state)
            .map_err(|e| CommandError::from(format!("cannot serialize claims: {e}")))?;
        devmap_query::write_atomic(&claims_path, &bytes).map_err(|e| {
            CommandError::from(format!("cannot write {}: {e}", claims_path.display()))
        })?;
        Ok(true)
    })
}

/// Releases any registered lane claims matching `worktree_path` or `branch`
/// during worktree removal / merge teardown so `MAX_ACTIVE_LANES` slots never leak.
pub(crate) fn release_lanes_matching(
    repo_path: &str,
    worktree_path: Option<&Path>,
    branch: Option<&str>,
) -> Result<usize, CommandError> {
    let branch_clean = branch.map(str::trim).filter(|b| !b.is_empty());
    let wt_candidates: Vec<PathBuf> = worktree_path
        .into_iter()
        .flat_map(|p| {
            let mut v = vec![p.to_path_buf()];
            if let Ok(canon) = fs::canonicalize(p) {
                if canon != p {
                    v.push(canon);
                }
            }
            v
        })
        .collect();
    if wt_candidates.is_empty() && branch_clean.is_none() {
        return Ok(0);
    }

    let (_repo, lanes_dir) = lanes_dir_for_repo(repo_path)?;
    with_bus_lock(&lanes_dir, false, || {
        let claims_path = lanes_dir.join(CLAIMS_FILE);
        if !claims_path.is_file() {
            return Ok(0);
        }
        let mut state: ClaimsState = read_json_bounded(&claims_path, 4 * 1024 * 1024)?;
        let before = state.lanes.len();
        state.lanes.retain(|lane| {
            if let Some(target_branch) = branch_clean {
                if lane.branch == target_branch {
                    return false;
                }
            }
            if let Some(lw) = lane.worktree_path.as_deref() {
                let lw_path = Path::new(lw);
                let lw_canon = fs::canonicalize(lw_path).ok();
                for cand in &wt_candidates {
                    if lw_path == cand || lw_canon.as_deref() == Some(cand.as_path()) {
                        return false;
                    }
                }
            }
            true
        });
        let removed = before.saturating_sub(state.lanes.len());
        if removed > 0 {
            let bytes = serde_json::to_vec_pretty(&state)
                .map_err(|e| CommandError::from(format!("cannot serialize claims: {e}")))?;
            devmap_query::write_atomic(&claims_path, &bytes).map_err(|e| {
                CommandError::from(format!("cannot write {}: {e}", claims_path.display()))
            })?;
        }
        Ok(removed)
    })
}

/// Lists all registered parallel agent lanes in this repository family.
pub fn list_lanes(repo_path: &str) -> Result<Vec<AgentLaneClaim>, CommandError> {
    let (_repo, lanes_dir) = lanes_dir_for_repo(repo_path)?;
    with_bus_lock(&lanes_dir, false, || {
        let state: ClaimsState = read_json_bounded(&lanes_dir.join(CLAIMS_FILE), 4 * 1024 * 1024)?;
        Ok(state.lanes)
    })
}

/// Returns `<git-common-dir>/gitpulse/lanes/messages.json` (ensuring `lanes/` exists
/// when `create_dir` is true) so MCP waiters can watch it via `notify`.
pub fn lanes_messages_path(repo_path: &str, create_dir: bool) -> Result<PathBuf, CommandError> {
    let (_repo, lanes_dir) = lanes_dir_for_repo(repo_path)?;
    if create_dir {
        fs::create_dir_all(&lanes_dir).map_err(|e| {
            CommandError::from(format!("cannot create {}: {e}", lanes_dir.display()))
        })?;
    }
    Ok(lanes_dir.join(MESSAGES_FILE))
}

/// Posts a bounded agent-to-agent coordination message with an optional thread key.
pub fn post_thread_message(
    repo_path: &str,
    from_agent: &str,
    to_agent: Option<&str>,
    thread_key: Option<&str>,
    kind: &str,
    body: &str,
    branch: Option<&str>,
) -> Result<AgentBusMessage, CommandError> {
    let is_human = matches!(
        from_agent.trim(),
        "developer" | "human" | "human-dev" | "user"
    );
    let hop = if is_human { 0 } else { 1 };
    post_peer_message(
        repo_path,
        from_agent,
        to_agent,
        thread_key,
        kind,
        body,
        branch,
        None,
        hop,
        DEFAULT_MAX_HOPS,
    )
}

/// Posts a bounded peer message with explicit `msg_id`, `hop`, and `max_hops`.
#[allow(clippy::too_many_arguments)]
pub fn post_peer_message(
    repo_path: &str,
    from_agent: &str,
    to_agent: Option<&str>,
    thread_key: Option<&str>,
    kind: &str,
    body: &str,
    branch: Option<&str>,
    explicit_msg_id: Option<&str>,
    hop: u32,
    max_hops: u32,
) -> Result<AgentBusMessage, CommandError> {
    let from = sanitize_token("from_agent", from_agent, 128)?;
    let to = match to_agent.map(str::trim).filter(|s| !s.is_empty()) {
        Some(target) => Some(sanitize_token("to_agent", target, 128)?),
        None => None,
    };
    let t_key = match thread_key.map(str::trim).filter(|s| !s.is_empty()) {
        Some(k) => Some(sanitize_token("thread_key", k, 80)?),
        None => None,
    };
    let msg_kind = sanitize_token("kind", kind, 64)?;
    let normalized_body = crate::agent_mailbox::normalize_crlf(body);
    let trimmed_body = normalized_body.trim();
    if trimmed_body.is_empty() || trimmed_body.contains('\0') {
        return Err(CommandError::from("message body must not be empty"));
    }
    if trimmed_body.len() > MAX_MESSAGE_BODY_BYTES {
        return Err(CommandError::from(format!(
            "message body exceeds maximum of {MAX_MESSAGE_BODY_BYTES} bytes"
        )));
    }
    let msg_branch = match branch.map(str::trim).filter(|s| !s.is_empty()) {
        Some(b) => Some(sanitize_token("branch", b, 256)?),
        None => None,
    };
    let caller_msg_id = match explicit_msg_id.map(str::trim).filter(|s| !s.is_empty()) {
        Some(id) => {
            if !crate::agent_mailbox::valid_request_id(id) {
                return Err(CommandError::from(
                    "msg_id must be 1 to 64 ASCII letters, digits, '-' or '_'",
                ));
            }
            Some(id.to_string())
        }
        None => None,
    };
    let effective_max_hops = max_hops.clamp(1, 32);

    let (_repo, lanes_dir) = lanes_dir_for_repo(repo_path)?;
    with_bus_lock(&lanes_dir, true, || {
        let messages_path = lanes_dir.join(MESSAGES_FILE);
        let mut state: MessagesState = read_json_bounded(&messages_path, 8 * 1024 * 1024)?;

        if let Some(ref want_id) = caller_msg_id {
            if let Some(existing) = state
                .messages
                .iter()
                .find(|m| m.msg_id.as_deref() == Some(want_id.as_str()))
            {
                if existing.body == trimmed_body && existing.from_agent == from {
                    return Ok(existing.clone());
                }
                return Err(CommandError::from(format!(
                    "msg_id '{want_id}' already exists with a different payload"
                )));
            }
        }

        let seq = state.next_seq.saturating_add(1);
        state.next_seq = seq;
        let created_at_ms = now_epoch_ms();
        let msg_id = caller_msg_id.unwrap_or_else(|| format!("msg-{seq}-{created_at_ms}"));

        let msg = AgentBusMessage {
            seq,
            from_agent: from,
            to_agent: to,
            kind: msg_kind,
            body: trimmed_body.to_string(),
            branch: msg_branch,
            created_at_ms,
            thread_key: t_key,
            msg_id: Some(msg_id),
            read_by: Vec::new(),
            hop: Some(hop),
            max_hops: Some(effective_max_hops),
        };
        state.messages.push(msg.clone());
        if state.messages.len() > MAX_BUS_MESSAGES {
            let mut open_ask_seq_by_thread: HashMap<String, u64> = HashMap::new();
            for existing in &state.messages {
                let key = existing
                    .thread_key
                    .clone()
                    .unwrap_or_else(|| format!("msg-{}", existing.seq));
                if existing.kind == "ask" {
                    open_ask_seq_by_thread.insert(key, existing.seq);
                } else if matches!(
                    existing.kind.as_str(),
                    "reply" | "answer" | "withdraw" | "withdrawn"
                ) {
                    open_ask_seq_by_thread.remove(&key);
                }
            }
            let mut pinned_vec: Vec<u64> = open_ask_seq_by_thread.into_values().collect();
            pinned_vec.sort_unstable();
            let pinned_seqs: BTreeSet<u64> = pinned_vec
                .into_iter()
                .rev()
                .take(MAX_PINNED_OPEN_ASKS)
                .collect();

            while state.messages.len() > MAX_BUS_MESSAGES {
                let evict_idx = state
                    .messages
                    .iter()
                    .position(|m| !pinned_seqs.contains(&m.seq))
                    .unwrap_or(0);
                let dropped = state.messages.remove(evict_idx);
                state.dropped_before_seq =
                    state.dropped_before_seq.max(dropped.seq.saturating_add(1));
            }
        }

        let bytes = serde_json::to_vec_pretty(&state)
            .map_err(|e| CommandError::from(format!("cannot serialize messages: {e}")))?;
        devmap_query::write_atomic(&messages_path, &bytes).map_err(|e| {
            CommandError::from(format!("cannot write {}: {e}", messages_path.display()))
        })?;

        Ok(msg)
    })
    .inspect(|persisted| {
        #[cfg(not(test))]
        crate::workbench::notifications::sync_graft_inbox_message(persisted);
        #[cfg(test)]
        let _ = persisted;
    })
}

/// Looks up one message by `msg_id` (used to hydrate `> 6 KiB` mailbox pointer envelopes).
pub fn get_message_by_id(
    repo_path: &str,
    msg_id: &str,
) -> Result<Option<AgentBusMessage>, CommandError> {
    let clean_id = sanitize_token("msg_id", msg_id, 64)?;
    let (_repo, lanes_dir) = lanes_dir_for_repo(repo_path)?;
    with_bus_lock(&lanes_dir, false, || {
        let state: MessagesState =
            read_json_bounded(&lanes_dir.join(MESSAGES_FILE), 8 * 1024 * 1024)?;
        Ok(state
            .messages
            .into_iter()
            .find(|m| m.msg_id.as_deref() == Some(clean_id.as_str())))
    })
}

/// Marks `msg_ids` (or `msg-<seq>` fallback) as read by `reader_id` in `messages.json`.
pub fn ack_messages(
    repo_path: &str,
    reader_id: &str,
    msg_ids: &[String],
) -> Result<usize, CommandError> {
    if msg_ids.is_empty() {
        return Ok(0);
    }
    let reader = sanitize_token("reader_id", reader_id, 128)?;
    let wanted: BTreeSet<String> = msg_ids
        .iter()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .collect();
    if wanted.is_empty() {
        return Ok(0);
    }
    let (_repo, lanes_dir) = lanes_dir_for_repo(repo_path)?;
    with_bus_lock(&lanes_dir, false, || {
        let messages_path = lanes_dir.join(MESSAGES_FILE);
        if !messages_path.is_file() {
            return Ok(0);
        }
        let mut state: MessagesState = read_json_bounded(&messages_path, 8 * 1024 * 1024)?;
        let mut updated = 0usize;
        for msg in &mut state.messages {
            let eff_id = msg
                .msg_id
                .clone()
                .unwrap_or_else(|| format!("msg-{}", msg.seq));
            if wanted.contains(&eff_id)
                && !msg.read_by.contains(&reader)
                && msg.read_by.len() < MAX_READ_BY_ENTRIES
            {
                msg.read_by.push(reader.clone());
                updated += 1;
            }
        }
        if updated > 0 {
            let bytes = serde_json::to_vec_pretty(&state)
                .map_err(|e| CommandError::from(format!("cannot serialize messages: {e}")))?;
            devmap_query::write_atomic(&messages_path, &bytes).map_err(|e| {
                CommandError::from(format!("cannot write {}: {e}", messages_path.display()))
            })?;
        }
        Ok(updated)
    })
}

/// Reads unread messages addressed to any of `peer_aliases` (plus optional broadcasts),
/// excluding messages sent by any of `peer_aliases` or already listed in `read_by`.
pub fn read_unread_for_peer(
    repo_path: &str,
    peer_aliases: &[String],
    since_seq: u64,
    thread_key: Option<&str>,
    include_broadcasts: bool,
) -> Result<AgentInboxPage, CommandError> {
    read_messages_for_peer(
        repo_path,
        peer_aliases,
        since_seq,
        thread_key,
        include_broadcasts,
        true,
    )
}

/// Reads messages addressed to any of `peer_aliases` (plus optional broadcasts),
/// optionally filtering out messages already acknowledged in `read_by`.
pub fn read_messages_for_peer(
    repo_path: &str,
    peer_aliases: &[String],
    since_seq: u64,
    thread_key: Option<&str>,
    include_broadcasts: bool,
    unread_only: bool,
) -> Result<AgentInboxPage, CommandError> {
    let aliases: BTreeSet<String> = peer_aliases
        .iter()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .collect();
    let t_filter = thread_key.map(str::trim).filter(|s| !s.is_empty());
    let (_repo, lanes_dir) = lanes_dir_for_repo(repo_path)?;
    with_bus_lock(&lanes_dir, false, || {
        let state: MessagesState =
            read_json_bounded(&lanes_dir.join(MESSAGES_FILE), 8 * 1024 * 1024)?;
        let latest_seq = state.next_seq;
        let filtered = state
            .messages
            .into_iter()
            .filter(|m| m.seq > since_seq)
            .filter(|m| match t_filter {
                Some(tk) => m.thread_key.as_deref() == Some(tk),
                None => true,
            })
            .filter(|m| !aliases.contains(&m.from_agent))
            .filter(|m| !unread_only || !m.read_by.iter().any(|r| aliases.contains(r)))
            .filter(|m| match &m.to_agent {
                Some(to) => aliases.is_empty() || aliases.contains(to),
                None => include_broadcasts,
            })
            .collect();
        Ok(AgentInboxPage {
            messages: filtered,
            latest_seq,
            dropped_before_seq: state.dropped_before_seq,
        })
    })
}

/* ── Peer Registry (`peers.json`) & Liveness Reconciliation ───────────────── */

pub(crate) fn canonical_provider(raw: &str) -> String {
    let lower = raw.trim().to_ascii_lowercase();
    match lower.as_str() {
        "antigravity" | "agy" => "agy".to_string(),
        other => other.to_string(),
    }
}

pub(crate) fn canonical_surface(raw: &str) -> String {
    match raw.trim().to_ascii_lowercase().as_str() {
        "" => "external_cli".to_string(),
        "hosted_terminal" | "terminal" => "terminal".to_string(),
        "antigravity_ide" | "ide" => "ide".to_string(),
        "managed" => "managed".to_string(),
        other => other.to_string(),
    }
}

pub(crate) fn providers_match(record_provider: &str, query_provider: &str) -> bool {
    canonical_provider(record_provider) == canonical_provider(query_provider)
}

/// Probes whether an Antigravity `<home>/presence/<conversationId>.lock` file is
/// actively held by a live Antigravity CLI or IDE process via non-blocking `flock`.
pub(crate) fn is_antigravity_presence_locked(homes: &[PathBuf], session_id: &str) -> bool {
    if !crate::workbench::terminal_command::is_canonical_uuid(session_id) {
        return false;
    }
    #[cfg(unix)]
    {
        use std::os::unix::io::AsRawFd;
        let lock_name = format!("{}.lock", session_id.to_ascii_lowercase());
        for home in homes {
            let candidate = home.join("presence").join(&lock_name);
            let Ok(file) = fs::File::open(&candidate) else {
                continue;
            };
            // SAFETY: `file` is an open descriptor owned by this scope; if we acquire
            // `LOCK_EX | LOCK_NB` we unlock it immediately before closing.
            let rc = unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) };
            if rc == 0 {
                unsafe {
                    let _ = libc::flock(file.as_raw_fd(), libc::LOCK_UN);
                }
            } else {
                let err = std::io::Error::last_os_error();
                if err.kind() == std::io::ErrorKind::WouldBlock
                    || err.raw_os_error() == Some(libc::EWOULDBLOCK)
                    || err.raw_os_error() == Some(libc::EAGAIN)
                {
                    return true;
                }
            }
        }
        false
    }
    #[cfg(not(unix))]
    {
        let _ = (homes, session_id);
        false
    }
}

/// Upserts a peer record in `<git-common-dir>/gitpulse/lanes/peers.json` under `.bus.lock`.
pub fn upsert_peer(repo_path: &str, update: PeerUpsert) -> Result<PeerRecord, CommandError> {
    let provider = sanitize_token("provider", &canonical_provider(&update.provider), 32)?;
    let surface = sanitize_token("surface", &canonical_surface(&update.surface), 32)?;
    let worktree_clean = if update.worktree_path.trim().is_empty() {
        repo_path.trim().to_string()
    } else {
        update.worktree_path.trim().to_string()
    };
    let provider_sid = update
        .provider_session_id
        .as_deref()
        .map(str::trim)
        .filter(|s| crate::workbench::terminal_command::is_canonical_uuid(s))
        .map(|s| s.to_ascii_lowercase());
    let term_sid = update
        .terminal_session_id
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(|s| sanitize_token("terminal_session_id", s, 128))
        .transpose()?;
    let run_id = update
        .run_id
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(|s| sanitize_token("run_id", s, 128))
        .transpose()?;
    let derived_peer_id = match update
        .peer_id
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
    {
        Some(explicit) => sanitize_token("peer_id", explicit, 128)?,
        None => {
            if let Some(ref sid) = provider_sid {
                format!("{provider}:{sid}")
            } else if let Some(ref tid) = term_sid {
                format!("{provider}:{tid}")
            } else if let Some(pid) = update.pid {
                format!("{provider}:pid-{pid}")
            } else {
                return Err(CommandError::from(
                    "peer registration requires peer_id, provider_session_id, terminal_session_id, or pid",
                ));
            }
        }
    };
    let role = update
        .role
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(|s| sanitize_token("role", s, 64))
        .transpose()?;
    let branch = update
        .branch
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(|s| sanitize_token("branch", s, 256))
        .transpose()?;
    let turn_state = update
        .turn_state
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .unwrap_or("running")
        .to_string();
    let transcript_path = update
        .transcript_path
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty() && !s.contains('\0') && s.len() <= 1024)
        .map(str::to_string);

    let (_repo, lanes_dir) = lanes_dir_for_repo(repo_path)?;
    with_bus_lock(&lanes_dir, true, || {
        let peers_path = lanes_dir.join(PEERS_FILE);
        let mut state: PeersState = read_json_bounded(&peers_path, 4 * 1024 * 1024)?;
        let now = now_epoch_ms();

        let existing_idx = state.peers.iter().position(|p| {
            p.peer_id == derived_peer_id
                || (provider_sid.is_some() && p.provider_session_id == provider_sid)
                || (term_sid.is_some()
                    && provider_sid.is_none()
                    && p.terminal_session_id == term_sid
                    && providers_match(&p.provider, &provider))
        });

        let record = if let Some(idx) = existing_idx {
            let prev = &state.peers[idx];
            let merged_peer_id = if update.peer_id.is_some() || provider_sid.is_some() {
                derived_peer_id.clone()
            } else {
                prev.peer_id.clone()
            };
            let next = PeerRecord {
                peer_id: merged_peer_id,
                provider: provider.clone(),
                surface: if update.surface.trim().is_empty() {
                    prev.surface.clone()
                } else {
                    surface
                },
                provider_session_id: provider_sid.or_else(|| prev.provider_session_id.clone()),
                terminal_session_id: term_sid.or_else(|| prev.terminal_session_id.clone()),
                run_id: run_id.or_else(|| prev.run_id.clone()),
                worktree_path: worktree_clean,
                branch: branch.or_else(|| prev.branch.clone()),
                role: role.or_else(|| prev.role.clone()),
                turn_state: if update.turn_state.is_some() {
                    turn_state
                } else {
                    prev.turn_state.clone()
                },
                auto_wake: update.auto_wake.unwrap_or(prev.auto_wake),
                pid: update.pid.or(prev.pid),
                proc_start_ns: update.proc_start_ns.or(prev.proc_start_ns),
                transcript_path: transcript_path.or_else(|| prev.transcript_path.clone()),
                updated_at_ms: now,
            };
            state.peers[idx] = next.clone();
            next
        } else {
            if state.peers.len() >= MAX_PEER_RECORDS {
                state.peers.sort_by_key(|p| p.updated_at_ms);
                state.peers.remove(0);
            }
            let next = PeerRecord {
                peer_id: derived_peer_id,
                provider,
                surface,
                provider_session_id: provider_sid,
                terminal_session_id: term_sid,
                run_id,
                worktree_path: worktree_clean,
                branch,
                role,
                turn_state,
                auto_wake: update.auto_wake.unwrap_or(false),
                pid: update.pid,
                proc_start_ns: update.proc_start_ns,
                transcript_path,
                updated_at_ms: now,
            };
            state.peers.push(next.clone());
            next
        };

        state.peers.sort_by(|a, b| a.peer_id.cmp(&b.peer_id));
        let bytes = serde_json::to_vec_pretty(&state)
            .map_err(|e| CommandError::from(format!("cannot serialize peers: {e}")))?;
        devmap_query::write_atomic(&peers_path, &bytes).map_err(|e| {
            CommandError::from(format!("cannot write {}: {e}", peers_path.display()))
        })?;
        Ok(record)
    })
}

/// Returns `provider_session_id` recorded in `peers.json` for `run_id` when present.
pub(crate) fn peer_session_id_for_run(repo_path: &str, run_id: &str) -> Option<String> {
    let (_repo, lanes_dir) = lanes_dir_for_repo(repo_path).ok()?;
    let state: PeersState = with_bus_lock(&lanes_dir, false, || {
        read_json_bounded(&lanes_dir.join(PEERS_FILE), 4 * 1024 * 1024)
    })
    .ok()?;
    state
        .peers
        .into_iter()
        .filter(|p| p.run_id.as_deref() == Some(run_id))
        .max_by_key(|p| p.updated_at_ms)
        .and_then(|p| p.provider_session_id)
}

/// Checks Verdict 1 Rule 3 for external Claude `Stop` `deliver`: returns true iff
/// `peers.json` holds a live record for `session_id` with `auto_wake == true`
/// and matching live `(pid, proc_start_ns)`.
pub(crate) fn is_external_peer_auto_wake_opted_in(repo_path: &str, session_id: &str) -> bool {
    let Ok(sid) = crate::agent_mailbox::session_key(session_id) else {
        return false;
    };
    let Ok((_repo, lanes_dir)) = lanes_dir_for_repo(repo_path) else {
        return false;
    };
    let Ok(state) = with_bus_lock(&lanes_dir, false, || {
        read_json_bounded::<PeersState>(&lanes_dir.join(PEERS_FILE), 4 * 1024 * 1024)
    }) else {
        return false;
    };
    state.peers.iter().any(|peer| {
        if !peer.auto_wake || peer.provider_session_id.as_deref() != Some(sid.as_str()) {
            return false;
        }
        let (Some(pid), Some(start_ns)) = (peer.pid, peer.proc_start_ns) else {
            return false;
        };
        matches!(
            crate::workbench::process_birth::running_since(pid, u128::from(start_ns)),
            crate::workbench::process_birth::Liveness::Alive
        )
    })
}

/// Lists and reconciles all peers across all worktrees of `repo_path`.
pub fn list_reconciled_peers(repo_path: &str) -> Result<Vec<ReconciledPeer>, CommandError> {
    let agy_homes = crate::workbench::conversation::antigravity_homes();
    list_reconciled_peers_with(
        repo_path,
        &agy_homes,
        &crate::workbench::process_birth::running_since,
        &crate::workbench::conversation::has_saved_transcript,
    )
}

pub(crate) fn list_reconciled_peers_with(
    repo_path: &str,
    agy_homes: &[PathBuf],
    alive_probe: &dyn Fn(u32, u128) -> crate::workbench::process_birth::Liveness,
    has_transcript: &dyn Fn(&str, &str) -> bool,
) -> Result<Vec<ReconciledPeer>, CommandError> {
    let live_obs = crate::insights::live_sessions::observe();
    list_reconciled_peers_from_observation(
        repo_path,
        agy_homes,
        &live_obs,
        alive_probe,
        has_transcript,
    )
}

pub(crate) fn list_reconciled_peers_from_observation(
    repo_path: &str,
    agy_homes: &[PathBuf],
    live_obs: &crate::insights::live_sessions::Observation,
    alive_probe: &dyn Fn(u32, u128) -> crate::workbench::process_birth::Liveness,
    has_transcript: &dyn Fn(&str, &str) -> bool,
) -> Result<Vec<ReconciledPeer>, CommandError> {
    let (_repo, lanes_dir) = lanes_dir_for_repo(repo_path)?;
    let stored: PeersState = with_bus_lock(&lanes_dir, false, || {
        read_json_bounded(&lanes_dir.join(PEERS_FILE), 4 * 1024 * 1024)
    })?;
    let worktrees = list_worktrees(repo_path).unwrap_or_default();
    let wt_paths: Vec<String> = worktrees.iter().map(|w| w.path.clone()).collect();
    let wt_branch_by_path: HashMap<String, Option<String>> = worktrees
        .iter()
        .map(|w| (w.path.clone(), w.branch.clone()))
        .collect();

    let live_facet = crate::insights::live_sessions::attribute(live_obs, &wt_paths);
    let mut live_by_pid: HashMap<u32, (&crate::insights::LiveSession, &str)> = HashMap::new();
    let mut live_by_sid: HashMap<String, (&crate::insights::LiveSession, &str)> = HashMap::new();
    for wt in &live_facet.worktrees {
        for sess in &wt.sessions {
            live_by_pid.insert(sess.pid, (sess, wt.path.as_str()));
            if let Some(ref sid) = sess.session_id {
                live_by_sid.insert(sid.to_ascii_lowercase(), (sess, wt.path.as_str()));
            }
        }
    }

    let mut out = Vec::new();
    let mut covered_pids = BTreeSet::new();
    let mut covered_sids = BTreeSet::new();

    for mut peer in stored.peers {
        peer.provider = canonical_provider(&peer.provider);
        peer.surface = canonical_surface(&peer.surface);
        if peer.branch.is_none() {
            if let Some(Some(br)) = wt_branch_by_path.get(&peer.worktree_path) {
                peer.branch = Some(br.clone());
            }
        }
        let sid_lower = peer
            .provider_session_id
            .as_deref()
            .map(|s| s.to_ascii_lowercase());
        if let Some(ref s) = sid_lower {
            covered_sids.insert(s.clone());
        }
        if let Some(pid) = peer.pid {
            covered_pids.insert(pid);
        }

        let mut is_alive = false;
        let mut unknown_reason: Option<String> = None;

        if let Some(ref s) = sid_lower {
            if live_by_sid.contains_key(s)
                || (providers_match(&peer.provider, "agy")
                    && is_antigravity_presence_locked(agy_homes, s))
            {
                is_alive = true;
            }
        }

        if !is_alive {
            if let (Some(pid), Some(start_ns)) = (peer.pid, peer.proc_start_ns) {
                if peer.turn_state != "exited" {
                    match alive_probe(pid, u128::from(start_ns)) {
                        crate::workbench::process_birth::Liveness::Alive => is_alive = true,
                        crate::workbench::process_birth::Liveness::Gone => {}
                        crate::workbench::process_birth::Liveness::Unknown(why) => {
                            unknown_reason = Some(why);
                        }
                    }
                }
            } else if let Some(pid) = peer.pid {
                if peer.turn_state != "exited" && live_by_pid.contains_key(&pid) {
                    is_alive = true;
                }
            }
        }

        let (liveness, liveness_reason) = if is_alive {
            ("live".to_string(), None)
        } else if let Some(why) = unknown_reason {
            peer.turn_state = "unknown".to_string();
            ("unknown".to_string(), Some(why))
        } else {
            peer.turn_state = "exited".to_string();
            let resumable = sid_lower
                .as_deref()
                .is_some_and(|s| has_transcript(&peer.provider, s));
            if resumable {
                ("resumable".to_string(), None)
            } else {
                ("exited".to_string(), None)
            }
        };

        out.push(ReconciledPeer {
            record: peer,
            liveness,
            liveness_reason,
        });
    }

    // Also include live process-scanned sessions in this repo that have not yet
    // written a `peers.json` record (for example a freshly opened CLI or IDE).
    for wt in &live_facet.worktrees {
        let branch = wt_branch_by_path.get(&wt.path).cloned().flatten();
        for sess in &wt.sessions {
            let sid_lower = sess.session_id.as_deref().map(|s| s.to_ascii_lowercase());
            if covered_pids.contains(&sess.pid)
                || sid_lower
                    .as_deref()
                    .is_some_and(|s| covered_sids.contains(s))
            {
                continue;
            }
            let provider = canonical_provider(&sess.kind);
            let term_sid = sess.terminal_session_id.clone().or_else(|| {
                sess.tree
                    .as_deref()
                    .and_then(|t| t.strip_prefix("session:"))
                    .map(str::to_string)
            });
            let surface = if term_sid.is_some() {
                "terminal".to_string()
            } else if sess.ide_hosted {
                "ide".to_string()
            } else if sess.run_id.is_some() {
                "managed".to_string()
            } else {
                "external_cli".to_string()
            };
            let peer_id = match sid_lower.as_deref() {
                Some(s) => format!("{provider}:{s}"),
                None => format!("{provider}:pid-{}", sess.pid),
            };
            out.push(ReconciledPeer {
                record: PeerRecord {
                    peer_id,
                    provider,
                    surface,
                    provider_session_id: sid_lower,
                    terminal_session_id: term_sid,
                    run_id: sess.run_id.clone(),
                    worktree_path: wt.path.clone(),
                    branch: branch.clone(),
                    role: None,
                    turn_state: if sess.status.is_empty() {
                        "running".into()
                    } else {
                        sess.status.clone()
                    },
                    auto_wake: false,
                    pid: Some(sess.pid),
                    proc_start_ns: None,
                    transcript_path: None,
                    updated_at_ms: now_epoch_ms(),
                },
                liveness: "live".into(),
                liveness_reason: None,
            });
        }
    }

    out.sort_by(|a, b| a.record.peer_id.cmp(&b.record.peer_id));
    Ok(out)
}

/// Resolves a target selector (`peer_id`, `provider_session_id`, `terminal_session_id`,
/// `run_id`, `role`, `branch`, or bare `provider`) against reconciled peers, failing
/// closed with [`PeerResolutionError::AmbiguousPeer`] whenever multiple live (or
/// resumable) sessions match.
pub fn resolve_target_peer(
    repo_path: &str,
    target: &str,
) -> Result<ReconciledPeer, PeerResolutionError> {
    let peers = list_reconciled_peers(repo_path).map_err(PeerResolutionError::Bus)?;
    resolve_target_from_list(&peers, target)
}

pub(crate) fn resolve_target_from_list(
    peers: &[ReconciledPeer],
    target: &str,
) -> Result<ReconciledPeer, PeerResolutionError> {
    let clean = target.trim();
    if clean.is_empty() {
        return Err(PeerResolutionError::NotFound {
            target: String::new(),
        });
    }
    let clean_lower = clean.to_ascii_lowercase();

    let exact_matches = |p: &ReconciledPeer| -> bool {
        p.record.peer_id == clean
            || p.record
                .provider_session_id
                .as_deref()
                .is_some_and(|s| s.eq_ignore_ascii_case(&clean_lower))
            || p.record.terminal_session_id.as_deref() == Some(clean)
            || p.record.run_id.as_deref() == Some(clean)
    };

    let exact_live: Vec<ReconciledPeer> = peers
        .iter()
        .filter(|p| p.liveness == "live" && exact_matches(p))
        .cloned()
        .collect();
    if exact_live.len() == 1 {
        return Ok(exact_live.into_iter().next().unwrap());
    }
    if exact_live.len() > 1 {
        return Err(PeerResolutionError::AmbiguousPeer {
            target: clean.to_string(),
            candidates: exact_live,
        });
    }

    let selector_matches = |p: &ReconciledPeer| -> bool {
        providers_match(&p.record.provider, clean)
            || p.record.role.as_deref() == Some(clean)
            || p.record.branch.as_deref() == Some(clean)
    };

    let live_candidates: Vec<ReconciledPeer> = peers
        .iter()
        .filter(|p| p.liveness == "live" && selector_matches(p))
        .cloned()
        .collect();
    if live_candidates.len() == 1 {
        return Ok(live_candidates.into_iter().next().unwrap());
    }
    if live_candidates.len() > 1 {
        return Err(PeerResolutionError::AmbiguousPeer {
            target: clean.to_string(),
            candidates: live_candidates,
        });
    }

    // Fall back to resumable conversations so `gitpulse_agent_send` can report
    // `delivery_outcome: "requires_resume"` instead of losing track of an exited peer.
    let resumable_exact: Vec<ReconciledPeer> = peers
        .iter()
        .filter(|p| p.liveness == "resumable" && exact_matches(p))
        .cloned()
        .collect();
    if resumable_exact.len() == 1 {
        return Ok(resumable_exact.into_iter().next().unwrap());
    }
    if resumable_exact.len() > 1 {
        return Err(PeerResolutionError::AmbiguousPeer {
            target: clean.to_string(),
            candidates: resumable_exact,
        });
    }

    let resumable_sel: Vec<ReconciledPeer> = peers
        .iter()
        .filter(|p| p.liveness == "resumable" && selector_matches(p))
        .cloned()
        .collect();
    if resumable_sel.len() == 1 {
        return Ok(resumable_sel.into_iter().next().unwrap());
    }
    if resumable_sel.len() > 1 {
        return Err(PeerResolutionError::AmbiguousPeer {
            target: clean.to_string(),
            candidates: resumable_sel,
        });
    }

    Err(PeerResolutionError::NotFound {
        target: clean.to_string(),
    })
}

/// Posts a bounded agent-to-agent coordination message (broadcast when `to_agent` is `None`).
pub fn post_message(
    repo_path: &str,
    from_agent: &str,
    to_agent: Option<&str>,
    kind: &str,
    body: &str,
    branch: Option<&str>,
) -> Result<AgentBusMessage, CommandError> {
    post_thread_message(repo_path, from_agent, to_agent, None, kind, body, branch)
}

/// Reads coordination messages addressed to `agent_id` (plus broadcasts where
/// `to_agent` is `None`), or all messages when `agent_id` is `None`, strictly
/// after `since_seq`, optionally filtered by `thread_key`.
pub fn read_thread_messages(
    repo_path: &str,
    thread_key: Option<&str>,
    agent_id: Option<&str>,
    since_seq: u64,
) -> Result<AgentInboxPage, CommandError> {
    let target_filter = match agent_id.map(str::trim).filter(|s| !s.is_empty()) {
        Some(a) => Some(sanitize_token("agent_id", a, 128)?),
        None => None,
    };
    let t_filter = thread_key.map(str::trim).filter(|s| !s.is_empty());
    let (_repo, lanes_dir) = lanes_dir_for_repo(repo_path)?;
    with_bus_lock(&lanes_dir, false, || {
        let state: MessagesState =
            read_json_bounded(&lanes_dir.join(MESSAGES_FILE), 8 * 1024 * 1024)?;
        let latest_seq = state.next_seq;
        let filtered = state
            .messages
            .into_iter()
            .filter(|m| m.seq > since_seq)
            .filter(|m| match &t_filter {
                Some(tk) => m.thread_key.as_deref() == Some(*tk),
                None => true,
            })
            .filter(|m| match (&target_filter, &m.to_agent) {
                (None, _) => true,
                (Some(_), None) => true, // broadcast reaches every agent
                (Some(want), Some(to)) => want == to || &m.from_agent == want,
            })
            .collect();

        Ok(AgentInboxPage {
            messages: filtered,
            latest_seq,
            dropped_before_seq: state.dropped_before_seq,
        })
    })
}

/// Reads coordination messages addressed to `agent_id` (plus broadcasts where
/// `to_agent` is `None`), or all messages when `agent_id` is `None`, strictly
/// after `since_seq`.
pub fn read_messages(
    repo_path: &str,
    agent_id: Option<&str>,
    since_seq: u64,
) -> Result<AgentInboxPage, CommandError> {
    read_thread_messages(repo_path, None, agent_id, since_seq)
}

/// Withdraws an open structured ask under `thread_key`.
pub fn withdraw_ask(
    repo_path: &str,
    from_agent: &str,
    thread_key: &str,
    reason: &str,
) -> Result<AgentBusMessage, CommandError> {
    let clean_thread_key = sanitize_token("thread_key", thread_key, 80)?;
    let clean_reason = sanitize_token("reason", reason, 600)?;
    let body = serde_json::json!({
        "status": "withdrawn",
        "reason": clean_reason,
    })
    .to_string();
    post_thread_message(
        repo_path,
        from_agent,
        None,
        Some(&clean_thread_key),
        "withdraw",
        &body,
        None,
    )
}

/// Posts a developer or agent reply/answer to a thread or structured ask under `thread_key`.
pub fn answer_ask(
    repo_path: &str,
    from_agent: &str,
    thread_key: &str,
    answer_body: &str,
) -> Result<AgentBusMessage, CommandError> {
    let clean_thread_key = sanitize_token("thread_key", thread_key, 80)?;
    post_thread_message(
        repo_path,
        from_agent,
        None,
        Some(&clean_thread_key),
        "reply",
        answer_body,
        None,
    )
}

/// Lists aggregated inbox threads grouped by `thread_key` (or message sequence),
/// resolving status, latest ask, and reply timeline modeled on Vespin.
pub fn list_inbox_threads(
    repo_path: &str,
    agent_id: Option<&str>,
    status_filter: Option<&str>,
) -> Result<Vec<AgentInboxThread>, CommandError> {
    let target_filter = match agent_id.map(str::trim).filter(|s| !s.is_empty()) {
        Some(a) => Some(sanitize_token("agent_id", a, 128)?),
        None => None,
    };
    let clean_status = status_filter
        .map(str::trim)
        .filter(|s| !s.is_empty() && *s != "all");
    let (_repo, lanes_dir) = lanes_dir_for_repo(repo_path)?;
    with_bus_lock(&lanes_dir, false, || {
        let state: MessagesState =
            read_json_bounded(&lanes_dir.join(MESSAGES_FILE), 8 * 1024 * 1024)?;

        let mut thread_map: HashMap<String, Vec<AgentBusMessage>> = HashMap::new();
        for msg in state.messages {
            let key = msg
                .thread_key
                .clone()
                .unwrap_or_else(|| format!("msg-{}", msg.seq));
            thread_map.entry(key).or_default().push(msg);
        }

        let mut threads = Vec::new();
        for (key, mut msgs) in thread_map {
            msgs.sort_by_key(|m| m.seq);

            if let Some(want) = &target_filter {
                let touches_agent = msgs.iter().any(|m| {
                    &m.from_agent == want
                        || m.to_agent.as_deref() == Some(want.as_str())
                        || m.to_agent.is_none()
                });
                if !touches_agent {
                    continue;
                }
            }

            let from_agent = match msgs.first() {
                Some(m) => m.from_agent.clone(),
                None => continue,
            };
            let to_agent = match msgs.first() {
                Some(m) => m.to_agent.clone(),
                None => continue,
            };
            let first_body = match msgs.first() {
                Some(m) => m.body.clone(),
                None => continue,
            };
            let latest = match msgs.last() {
                Some(m) => m.clone(),
                None => continue,
            };

            let mut status = "settled".to_string();
            let mut ask_type = None;
            let mut ask_title = None;

            for msg in &msgs {
                if msg.kind == "ask" {
                    status = "open".to_string();
                    if let Ok(val) = serde_json::from_str::<serde_json::Value>(&msg.body) {
                        if let Some(t) = val.get("ask_type").and_then(|v| v.as_str()) {
                            ask_type = Some(t.to_string());
                        }
                        if let Some(q) = val
                            .get("question")
                            .or_else(|| val.get("title"))
                            .or_else(|| val.get("prompt"))
                            .and_then(|v| v.as_str())
                        {
                            ask_title = Some(q.to_string());
                        }
                    }
                } else if msg.kind == "reply" || msg.kind == "answer" {
                    status = "answered".to_string();
                } else if msg.kind == "withdraw" || msg.kind == "withdrawn" {
                    status = "withdrawn".to_string();
                }
            }

            if let Some(want_st) = clean_status {
                if status != want_st {
                    continue;
                }
            }

            let title = ask_title.unwrap_or_else(|| {
                let b = first_body.trim();
                if b.starts_with('{') {
                    if let Ok(val) = serde_json::from_str::<serde_json::Value>(b) {
                        if let Some(q) = val
                            .get("question")
                            .or_else(|| val.get("title"))
                            .or_else(|| val.get("prompt"))
                            .and_then(|v| v.as_str())
                        {
                            return q.to_string();
                        }
                    }
                }
                if b.chars().count() > 60 {
                    let end = b
                        .char_indices()
                        .nth(57)
                        .map(|(idx, _)| idx)
                        .unwrap_or(b.len());
                    format!("{}…", &b[..end])
                } else if !b.is_empty() {
                    b.to_string()
                } else {
                    key.clone()
                }
            });

            threads.push(AgentInboxThread {
                thread_key: key,
                title,
                status,
                ask_type,
                latest_msg: latest.clone(),
                messages: msgs,
                from_agent,
                to_agent,
                updated_at_ms: latest.created_at_ms,
            });
        }

        threads.sort_by_key(|t| std::cmp::Reverse(t.updated_at_ms));
        Ok(threads)
    })
}

fn paths_overlap(a: &str, b: &str) -> bool {
    a == b || a.starts_with(&format!("{b}/")) || b.starts_with(&format!("{a}/"))
}

fn compute_claim_conflicts(lanes: &[AgentLaneClaim]) -> Vec<ClaimConflict> {
    let mut out = Vec::new();
    for i in 0..lanes.len() {
        for j in (i + 1)..lanes.len() {
            let la = &lanes[i];
            let lb = &lanes[j];
            let mut file_hits = BTreeSet::new();
            for fa in &la.claimed_files {
                for fb in &lb.claimed_files {
                    if paths_overlap(fa, fb) {
                        let narrower = if fa.len() >= fb.len() { fa } else { fb };
                        file_hits.insert(narrower.clone());
                    }
                }
            }
            let sym_b: BTreeSet<&str> = lb.claimed_symbols.iter().map(String::as_str).collect();
            let sym_hits: Vec<String> = la
                .claimed_symbols
                .iter()
                .filter(|s| sym_b.contains(s.as_str()))
                .cloned()
                .collect();

            if !file_hits.is_empty() || !sym_hits.is_empty() {
                out.push(ClaimConflict {
                    agent_a: la.agent_id.clone(),
                    branch_a: la.branch.clone(),
                    agent_b: lb.agent_id.clone(),
                    branch_b: lb.branch.clone(),
                    overlapping_files: file_hits.into_iter().collect(),
                    overlapping_symbols: sym_hits,
                });
            }
        }
    }
    out
}

fn compute_semantic_conflicts(
    overlays: &[(String, Option<String>, RoaringBitmap)],
    csr: &devmap_store::CsrGraphReader,
) -> Vec<SemanticLaneConflict> {
    let mut conflicts = Vec::new();
    for i in 0..overlays.len() {
        for j in (i + 1)..overlays.len() {
            let (wt_a, br_a, bits_a) = &overlays[i];
            let (wt_b, br_b, bits_b) = &overlays[j];
            if bits_a.is_empty() || bits_b.is_empty() {
                continue;
            }
            let vec_a = bits_a.to_vec();
            let vec_b = bits_b.to_vec();

            let mut direct_overlap = 0usize;
            let mut linked_1hop = 0usize;

            for &u in &vec_a {
                if bits_b.contains(u) {
                    direct_overlap += 1;
                }
                for &target in csr.outgoing_edges(u) {
                    if target != u && bits_b.contains(target) {
                        linked_1hop += 1;
                    }
                }
            }
            for &v in &vec_b {
                for &target in csr.outgoing_edges(v) {
                    if target != v && bits_a.contains(target) {
                        linked_1hop += 1;
                    }
                }
            }

            if direct_overlap > 0 || linked_1hop > 0 {
                let relation = if direct_overlap > 0 {
                    "same_csr_node"
                } else {
                    "caller_callee_1hop"
                };
                conflicts.push(SemanticLaneConflict {
                    worktree_a: wt_a.clone(),
                    branch_a: br_a.clone(),
                    worktree_b: wt_b.clone(),
                    branch_b: br_b.clone(),
                    relation: relation.to_string(),
                    direct_dirty_node_overlap: direct_overlap,
                    caller_callee_linked_nodes: linked_1hop,
                });
            }
        }
    }
    conflicts
}

/// Runs the 3-layer parallel agent collision radar across all worktrees and registered
/// lanes of `repo_path`:
/// 1. Pre-edit file & symbol claim overlaps
/// 2. Zero-copy `WorktreeOverlay` `RoaringBitmap` + 1-hop `CsrGraphReader` caller/callee collisions
/// 3. Quarantined ODB 3-way `git merge-tree` committed conflict checks
pub fn scan_parallel_agent_radar(
    repo_path: &str,
) -> Result<ParallelAgentRadarReport, CommandError> {
    let repo = validate_repo(repo_path)?;
    let lanes = list_lanes(repo_path)?;
    let mut claim_conflicts = compute_claim_conflicts(&lanes);
    let mut truncated = false;
    if claim_conflicts.len() > MAX_RADAR_CONFLICTS_PER_LAYER {
        claim_conflicts.truncate(MAX_RADAR_CONFLICTS_PER_LAYER);
        truncated = true;
    }

    let worktrees = list_worktrees(repo_path)?;
    let non_bare: Vec<_> = worktrees.iter().filter(|w| !w.is_bare).collect();

    let anchor_path = non_bare
        .iter()
        .find(|w| w.is_main)
        .map(|w| PathBuf::from(&w.path))
        .unwrap_or_else(|| repo.clone());

    let csr_opt = open_worktree_or_anchor_csr(&anchor_path);
    let csr_graph_available = csr_opt.is_some();
    let csr_generation = csr_opt.as_ref().map(|(_, g)| *g);

    let OverlayScan {
        scanned: worktrees_scanned,
        unscanned: worktrees_unscanned,
        truncated: overlays_truncated,
        errors: mut scan_errors,
        overlays,
    } = scan_worktree_overlays(&non_bare, |wt| {
        compute_worktree_dirty_nodes_readonly(&wt.path, Some(&anchor_path))
    });
    truncated |= overlays_truncated;

    let mut semantic_conflicts = match csr_opt.as_ref() {
        Some((csr, _)) => compute_semantic_conflicts(&overlays, csr),
        None => Vec::new(),
    };
    if semantic_conflicts.len() > MAX_RADAR_CONFLICTS_PER_LAYER {
        semantic_conflicts.truncate(MAX_RADAR_CONFLICTS_PER_LAYER);
        truncated = true;
    }

    let mut odb_merge_conflicts = Vec::new();
    let mut pairs_checked = 0usize;

    let quarantine = ObjectQuarantine::new(&anchor_path);
    if let Err(err) = &quarantine {
        scan_errors.push(RadarScanError {
            worktree: anchor_path.to_string_lossy().into_owned(),
            error: format!("ODB merge layer skipped: {err}"),
        });
    }
    if let Ok(quarantine) = quarantine {
        'outer: for i in 0..non_bare.len() {
            for j in (i + 1)..non_bare.len() {
                let wa = non_bare[i];
                let wb = non_bare[j];
                if wa.head.is_empty() || wb.head.is_empty() || wa.head == wb.head {
                    continue;
                }
                if pairs_checked >= MAX_ODB_RADAR_PAIRS {
                    truncated = true;
                    break 'outer;
                }
                pairs_checked += 1;
                match quarantine.merge_tree(
                    &anchor_path,
                    &wa.head,
                    &wb.head,
                    Instant::now() + DEFAULT_TIMEOUT,
                ) {
                    Ok(MergeOutcome::Clean { .. }) => {}
                    Ok(MergeOutcome::Conflicted {
                        paths, paths_total, ..
                    }) => {
                        odb_merge_conflicts.push(OdbPairConflict {
                            worktree_a: wa.path.clone(),
                            branch_a: wa.branch.clone(),
                            worktree_b: wb.path.clone(),
                            branch_b: wb.branch.clone(),
                            state: "conflict".to_string(),
                            paths,
                            paths_total,
                            reason: None,
                        });
                    }
                    Err(err) => {
                        odb_merge_conflicts.push(OdbPairConflict {
                            worktree_a: wa.path.clone(),
                            branch_a: wa.branch.clone(),
                            worktree_b: wb.path.clone(),
                            branch_b: wb.branch.clone(),
                            state: "unknown".to_string(),
                            paths: Vec::new(),
                            paths_total: 0,
                            reason: Some(err.to_string()),
                        });
                    }
                }
            }
        }
    } else if non_bare.len() > 1 {
        truncated = true;
    }

    Ok(ParallelAgentRadarReport {
        worktrees_scanned,
        worktrees_unscanned,
        lanes_registered: lanes.len(),
        csr_graph_available,
        csr_generation,
        claim_conflicts,
        semantic_conflicts,
        odb_merge_conflicts,
        truncated,
        scan_errors,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::codeintel::{provision_worktree_devmap_zero_copy, sync_worktree_overlay};
    use crate::procguard::LockedSpawn;
    use std::process::Command;

    fn git(repo: &Path, args: &[&str]) {
        let mut out_cmd = Command::new("git");
        crate::test_support::pin_git(&mut out_cmd);
        let out = out_cmd
            .current_dir(repo)
            .args(args)
            .output_locked()
            .expect("git spawn");
        assert!(
            out.status.success(),
            "git {args:?} failed: {}",
            String::from_utf8_lossy(&out.stderr)
        );
    }

    #[test]
    fn agent_bus_claims_and_messages_coordinate_across_sibling_worktrees_with_caps() {
        let anchor_dir = crate::codeintel::tests::repo_with_one_generation();
        let anchor = anchor_dir.path();
        crate::test_support::trust_repo(anchor);

        let wt_parent = tempfile::tempdir().unwrap();
        let wt_a = wt_parent.path().join("agent-a");
        let wt_b = wt_parent.path().join("agent-b");
        git(
            anchor,
            &["worktree", "add", "-b", "agent/a", wt_a.to_str().unwrap()],
        );
        git(
            anchor,
            &["worktree", "add", "-b", "agent/b", wt_b.to_str().unwrap()],
        );
        crate::test_support::trust_repo(&wt_a);
        crate::test_support::trust_repo(&wt_b);

        // Register claims from two DIFFERENT worktree paths; both resolve to `<common>/gitpulse/lanes/`!
        register_or_update_lane(
            wt_a.to_str().unwrap(),
            "claude-1",
            "agent/a",
            Some(wt_a.to_str().unwrap()),
            &["src".into()],
            &["src/auth".into()],
            &["verify_token".into()],
        )
        .unwrap();

        register_or_update_lane(
            wt_b.to_str().unwrap(),
            "codex-2",
            "agent/b",
            Some(wt_b.to_str().unwrap()),
            &["src".into()],
            &["src/auth/jwt.rs".into()],
            &["verify_token".into()],
        )
        .unwrap();

        let lanes = list_lanes(anchor.to_str().unwrap()).unwrap();
        assert_eq!(lanes.len(), 2);
        let claim_hits = compute_claim_conflicts(&lanes);
        assert_eq!(claim_hits.len(), 1);
        assert_eq!(claim_hits[0].overlapping_files, vec!["src/auth/jwt.rs"]);
        assert_eq!(claim_hits[0].overlapping_symbols, vec!["verify_token"]);

        // Post direct & broadcast messages from wt_a and read from wt_b; git status stays clean!
        let m1 = post_message(
            wt_a.to_str().unwrap(),
            "claude-1",
            Some("codex-2"),
            "interface_change",
            "Updated verify_token() to return Result<Claims, AuthError>",
            Some("agent/a"),
        )
        .unwrap();
        assert_eq!(m1.seq, 1);

        let _m2 = post_message(
            wt_a.to_str().unwrap(),
            "claude-1",
            Some("other-agent"),
            "private",
            "Not for codex-2",
            None,
        )
        .unwrap();

        let inbox_b = read_messages(wt_b.to_str().unwrap(), Some("codex-2"), 0).unwrap();
        assert_eq!(inbox_b.messages.len(), 1);
        assert_eq!(inbox_b.messages[0].seq, 1);

        // Adversarial validation: empty body and oversized body are rejected!
        assert!(post_message(
            wt_a.to_str().unwrap(),
            "claude-1",
            None,
            "note",
            "   ",
            None
        )
        .is_err());
        let huge = "x".repeat(MAX_MESSAGE_BODY_BYTES + 1);
        assert!(post_message(
            wt_a.to_str().unwrap(),
            "claude-1",
            None,
            "note",
            &huge,
            None
        )
        .is_err());

        // Release lane cleans up claim.
        assert!(release_lane(wt_a.to_str().unwrap(), "claude-1").unwrap());
        assert_eq!(list_lanes(anchor.to_str().unwrap()).unwrap().len(), 1);
    }

    fn bind_real_devmap() -> crate::devmap::cli::TestBinaryBinding {
        if let Some(home) = std::env::var_os("HOME").map(PathBuf::from) {
            for rel in [".local/bin/devmap", ".cargo/bin/devmap"] {
                let candidate = home.join(rel);
                if candidate.is_file() {
                    return crate::devmap::cli::bind_test_binary(
                        candidate.to_string_lossy().into_owned(),
                    );
                }
            }
        }
        let resolved = crate::devmap::cli::resolve_binary().expect("real devmap binary installed");
        crate::devmap::cli::bind_test_binary(resolved.path)
    }

    fn seed_clean_indexed_anchor_with_csr() -> tempfile::TempDir {
        let dir = tempfile::tempdir().unwrap();
        let anchor = dir.path();
        git(anchor, &["init", "-q", "-b", "main"]);
        fs::write(anchor.join(".gitignore"), ".devmap/\n.devcouncil/\n").unwrap();
        fs::create_dir_all(anchor.join("src")).unwrap();
        let caller_src = "pub fn probe_caller() { probe_callee(); }\n";
        let callee_src = "pub fn probe_callee() {}\n";
        fs::write(anchor.join("src/caller.rs"), caller_src).unwrap();
        fs::write(anchor.join("src/callee.rs"), callee_src).unwrap();
        git(
            anchor,
            &["add", ".gitignore", "src/caller.rs", "src/callee.rs"],
        );
        git(
            anchor,
            &[
                "-c",
                "user.name=t",
                "-c",
                "user.email=t@t",
                "commit",
                "-q",
                "-m",
                "seed caller and callee",
            ],
        );

        let canon_anchor = fs::canonicalize(anchor).unwrap();
        crate::test_support::trust_repo(&canon_anchor);
        crate::devmap::ensure_state_dir_excluded(&canon_anchor);

        let mut head_cmd = Command::new("git");
        crate::test_support::pin_git(&mut head_cmd);
        let head_out = head_cmd
            .current_dir(&canon_anchor)
            .args(["rev-parse", "HEAD"])
            .output_locked()
            .unwrap();
        let head_sha = String::from_utf8_lossy(&head_out.stdout).trim().to_string();

        let scanned = devmap_extract::scan_tree(&canon_anchor).unwrap();
        let hashes: std::collections::BTreeMap<String, u64> = scanned
            .file_hashes()
            .into_iter()
            .map(|(k, v)| (k.to_string(), v))
            .collect();
        let (grammar_ver, analyzer_ver) = devmap_extract::cache::current_payload_identity("rust");
        let engine_json = serde_json::to_string(&devmap_extract::ExtractionEngine::TreeSitter {
            grammar: "rust".to_string(),
            grammar_version: grammar_ver.parse::<u32>().unwrap_or(1),
        })
        .unwrap();

        let db = devmap_query::paths::store_path(&canon_anchor);
        fs::create_dir_all(db.parent().unwrap()).unwrap();
        let store = devmap_store::Store::open(&db).unwrap();
        store.bind_repo_root(&canon_anchor).unwrap();
        drop(store);

        let root_str = canon_anchor.to_string_lossy().into_owned();
        let conn = rusqlite::Connection::open(&db).unwrap();
        conn.execute(
            "INSERT INTO generations (id, created_at, head_sha, analysis_json, repo_root)
             VALUES (1, 0.0, ?1, ?2, ?3)",
            (
                &head_sha,
                r#"{"total_files":2,"total_symbols":2,"total_edges":1,"dead_symbols":[],"communities":[],"status":"Ok","unresolved_calls":0}"#,
                &root_str,
            ),
        )
        .unwrap();

        let primary = [
            ("src/caller.rs", "probe_caller", caller_src.len() as i64),
            ("src/callee.rs", "probe_callee", callee_src.len() as i64),
        ];
        for (idx, (path, sym, span_end)) in primary.iter().enumerate() {
            let id = (idx + 1) as i64;
            let raw_hash = *hashes.get(*path).expect("scanned hash");
            conn.execute("INSERT INTO paths (id, path) VALUES (?1, ?2)", (id, *path))
                .unwrap();
            let extraction_json = serde_json::json!({
                "file_path": *path,
                "language": "rust",
                "content_hash": raw_hash,
                "parse_outcome": "Clean",
                "symbols": [{
                    "name": *sym,
                    "qualified_name": format!("{path}::{sym}"),
                    "kind": "Function",
                    "span": { "start_byte": 0, "end_byte": *span_end as usize },
                    "is_exported": true
                }],
                "imports": [],
                "calls": []
            })
            .to_string();
            conn.execute(
                "INSERT INTO file_payloads
                   (payload_id, file_id, content_hash, language,
                    parse_outcome_json, engine_json, extraction_json,
                    entry_wiring_json, grammar_version, analyzer_version)
                 VALUES (?1, ?1, ?2, 'rust', '\"Clean\"', ?3, ?4, '[]', ?5, ?6)",
                (
                    id,
                    raw_hash as i64,
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
            let qname = format!("{path}::{sym}");
            conn.execute(
                "INSERT INTO payload_nodes
                   (payload_id, ord_in_file, name, qualified_name, kind,
                    span_start, span_end, is_exported)
                 VALUES (?1, 0, ?2, ?3, 'Function', 0, ?4, 1)",
                (id, *sym, &qname, *span_end),
            )
            .unwrap();
            conn.execute(
                "INSERT INTO nodes_fts (rowid, name, qualified_name, path)
                 VALUES (?1, ?2, ?3, ?4)",
                (id << 24, *sym, &qname, *path),
            )
            .unwrap();
        }
        conn.execute(
            "INSERT INTO edge_rows
               (edge_id, source_file_id, target_file_id,
                source_symbol, target_symbol, edge_kind, confidence,
                valid_from, valid_to)
             VALUES (0, 1, 2, 'probe_caller', 'probe_callee', 'Calls', 0.95, 1, NULL)",
            [],
        )
        .unwrap();
        drop(conn);

        let store = devmap_store::Store::open(&db).unwrap();
        let (_, edge_index) = store.generation_edges_with_id().unwrap().unwrap();
        let csr_path = devmap_store::Store::csr_sidecar_path(&db);
        edge_index.to_csr_writer().write_to_file(&csr_path).unwrap();
        drop(store);
        dir
    }

    #[test]
    fn radar_detects_cross_file_1hop_caller_callee_collision_over_zero_copy_csr() {
        let _devmap = bind_real_devmap();
        let anchor_dir = seed_clean_indexed_anchor_with_csr();
        let anchor = anchor_dir.path();

        let wt_parent = tempfile::tempdir().unwrap();
        let wt_a = wt_parent.path().join("lane-caller");
        let wt_b = wt_parent.path().join("lane-callee");
        git(
            anchor,
            &[
                "worktree",
                "add",
                "-b",
                "lane/caller",
                wt_a.to_str().unwrap(),
            ],
        );
        git(
            anchor,
            &[
                "worktree",
                "add",
                "-b",
                "lane/callee",
                wt_b.to_str().unwrap(),
            ],
        );
        crate::test_support::trust_repo(&wt_a);
        crate::test_support::trust_repo(&wt_b);

        // Provision BOTH worktrees in Stacked Zero-Copy mode (zero SQLite copies!).
        let pa = provision_worktree_devmap_zero_copy(anchor, &wt_a).unwrap();
        let pb = provision_worktree_devmap_zero_copy(anchor, &wt_b).unwrap();
        assert!(pa.zero_copy_base_ref && pb.zero_copy_base_ref);

        // Agent A edits ONLY `src/caller.rs`; Agent B edits ONLY `src/callee.rs`!
        // Textual diff overlap is ZERO — they touch completely different files!
        fs::write(
            wt_a.join("src/caller.rs"),
            "pub fn probe_caller() { probe_callee(123); }\n",
        )
        .unwrap();
        fs::write(
            wt_b.join("src/callee.rs"),
            "pub fn probe_callee(flag: bool) {}\n",
        )
        .unwrap();
        sync_worktree_overlay(wt_a.to_str().unwrap()).unwrap();
        sync_worktree_overlay(wt_b.to_str().unwrap()).unwrap();

        let radar = scan_parallel_agent_radar(anchor.to_str().unwrap()).expect("radar scan");
        assert!(radar.csr_graph_available, "{radar:?}");
        assert!(
            radar
                .semantic_conflicts
                .iter()
                .any(|c| c.relation == "caller_callee_1hop" && c.caller_callee_linked_nodes >= 1),
            "expected cross-file 1-hop caller/callee semantic conflict, got: {radar:?}"
        );
    }

    #[test]
    fn radar_is_strictly_read_only_and_detects_live_1hop_csr_without_writing_overlays() {
        // 1. Plain repository that has never initialized `.devmap`: `scan_parallel_agent_radar`
        //    must NEVER touch `.git/info/exclude` or create `.devmap`!
        let plain_dir = tempfile::tempdir().unwrap();
        let plain = plain_dir.path();
        git(plain, &["init", "-q", "-b", "main"]);
        fs::write(plain.join("hello.rs"), "pub fn hi() {}\n").unwrap();
        git(plain, &["add", "hello.rs"]);
        git(
            plain,
            &[
                "-c",
                "user.name=t",
                "-c",
                "user.email=t@t",
                "commit",
                "-q",
                "-m",
                "init",
            ],
        );
        crate::test_support::trust_repo(plain);

        let exclude_path = plain.join(".git/info/exclude");
        let exclude_before = fs::read(&exclude_path).unwrap_or_default();
        let plain_radar = scan_parallel_agent_radar(plain.to_str().unwrap()).unwrap();
        assert_eq!(plain_radar.worktrees_scanned, 1);
        assert!(
            !plain.join(".devmap").exists() && !plain.join(".devcouncil").exists(),
            "read-only radar must not create .devmap/.devcouncil directories"
        );
        let exclude_after = fs::read(&exclude_path).unwrap_or_default();
        assert_eq!(
            exclude_before, exclude_after,
            "read-only radar must not mutate .git/info/exclude"
        );

        // 2. Indexed anchor + stacked worktree with uncommitted edits and NO prior
        //    `sync_worktree_overlay` call: radar must compute live `dirty_nodes` in memory
        //    and detect 1-hop caller/callee collisions WITHOUT creating `delta.sqlite`!
        let anchor_dir = seed_clean_indexed_anchor_with_csr();
        let anchor = anchor_dir.path();

        let wt_parent = tempfile::tempdir().unwrap();
        let wt_a = wt_parent.path().join("ro-caller");
        let wt_b = wt_parent.path().join("ro-callee");
        git(
            anchor,
            &["worktree", "add", "-b", "ro/caller", wt_a.to_str().unwrap()],
        );
        git(
            anchor,
            &["worktree", "add", "-b", "ro/callee", wt_b.to_str().unwrap()],
        );
        crate::test_support::trust_repo(&wt_a);
        crate::test_support::trust_repo(&wt_b);
        provision_worktree_devmap_zero_copy(anchor, &wt_a).unwrap();
        // Leave `wt_b` completely unprovisioned (no `.devmap` directory at all) to also
        // exercise `fallback_anchor` resolution in `compute_worktree_dirty_nodes_readonly`.

        fs::write(
            wt_a.join("src/caller.rs"),
            "pub fn probe_caller() { probe_callee(99); }\n",
        )
        .unwrap();
        fs::write(
            wt_b.join("src/callee.rs"),
            "pub fn probe_callee(x: i32) {}\n",
        )
        .unwrap();

        let radar = scan_parallel_agent_radar(anchor.to_str().unwrap()).unwrap();
        assert!(
            !devmap_extract::paths::stack_delta_path(&wt_a).exists()
                && !wt_b.join(".devmap").exists()
                && !wt_b.join(".devcouncil").exists(),
            "read-only radar must not materialize delta.sqlite or .devmap onto disk"
        );
        assert!(
            radar
                .semantic_conflicts
                .iter()
                .any(|c| c.relation == "caller_callee_1hop" && c.caller_callee_linked_nodes >= 1),
            "expected in-memory 1-hop caller/callee collision without delta.sqlite on disk, got: {radar:?}"
        );
    }

    #[test]
    fn claim_paths_normalize_dot_slash_and_duplicate_slashes_and_reject_parent_traversal() {
        let dir = tempfile::tempdir().unwrap();
        let repo = dir.path();
        git(repo, &["init", "-q", "-b", "main"]);
        crate::test_support::trust_repo(repo);
        let repo_str = repo.to_str().unwrap();

        assert!(
            register_or_update_lane_with_conflicts(
                repo_str,
                "bad-agent",
                "feat/bad",
                None,
                &[],
                &["../secret.rs".into()],
                &[],
            )
            .is_err(),
            "parent traversal in claimed_files must be rejected"
        );

        register_or_update_lane_with_conflicts(
            repo_str,
            "agent-1",
            "feat/1",
            None,
            &["./packages//auth/".into()],
            &["./src/auth/jwt.rs".into()],
            &["auth::verify".into()],
        )
        .unwrap();

        let (claim2, conflicts) = register_or_update_lane_with_conflicts(
            repo_str,
            "agent-2",
            "feat/2",
            None,
            &[],
            &["src//auth//jwt.rs".into()],
            &["auth::verify".into()],
        )
        .unwrap();
        assert_eq!(claim2.claimed_files, vec!["src/auth/jwt.rs"]);
        assert_eq!(
            conflicts.len(),
            1,
            "normalized path claims must overlap: {conflicts:?}"
        );
        assert_eq!(conflicts[0].overlapping_files, vec!["src/auth/jwt.rs"]);
        assert_eq!(conflicts[0].overlapping_symbols, vec!["auth::verify"]);
    }

    #[test]
    fn radar_counts_a_worktree_whose_scan_failed_as_unscanned_and_says_why() {
        let anchor_dir = crate::codeintel::tests::repo_with_one_generation();
        let anchor = anchor_dir.path();
        crate::test_support::trust_repo(anchor);
        let _csr = crate::codeintel::open_repo_csr_graph(anchor.to_str().unwrap()).unwrap();
        let wt_parent = tempfile::tempdir().unwrap();
        let wt = wt_parent.path().join("broken-overlay");
        git(
            anchor,
            &[
                "worktree",
                "add",
                "-b",
                "radar/broken",
                wt.to_str().unwrap(),
            ],
        );
        crate::test_support::trust_repo(&wt);
        // Dirty, so the listing carries `dirty_files: Some(_)` -- the old code's
        // reason to count a failed scan as done.
        fs::write(wt.join("dirty.txt"), "x").unwrap();
        let lease_path = devmap_extract::paths::stack_lease_path(&wt);
        fs::create_dir_all(lease_path.parent().unwrap()).unwrap();
        fs::write(&lease_path, b"{ not json").unwrap();

        let radar = scan_parallel_agent_radar(anchor.to_str().unwrap()).unwrap();
        let wt_canon = wt.canonicalize().unwrap();
        let failed = radar
            .scan_errors
            .iter()
            .find(|e| {
                Path::new(&e.worktree).canonicalize().ok().as_deref() == Some(wt_canon.as_path())
            })
            .unwrap_or_else(|| panic!("scan failure must be surfaced: {radar:?}"));
        assert!(!failed.error.is_empty());
        assert!(radar.worktrees_unscanned >= 1, "{radar:?}");
    }

    fn listed_worktree_with_dirty_files(
    ) -> (tempfile::TempDir, tempfile::TempDir, Vec<WorktreeInfo>) {
        let anchor_dir = crate::codeintel::tests::repo_with_one_generation();
        let anchor = anchor_dir.path();
        crate::test_support::trust_repo(anchor);
        let wt_parent = tempfile::tempdir().unwrap();
        let wt = wt_parent.path().join("scan-seam");
        git(
            anchor,
            &["worktree", "add", "-b", "radar/seam", wt.to_str().unwrap()],
        );
        crate::test_support::trust_repo(&wt);
        fs::write(wt.join("dirty.txt"), "x").unwrap();
        let listed = list_worktrees(anchor.to_str().unwrap()).unwrap();
        (anchor_dir, wt_parent, listed)
    }

    #[test]
    fn scan_tally_never_counts_an_errored_scan_as_scanned_even_with_a_dirty_count() {
        let (_a, _p, listed) = listed_worktree_with_dirty_files();
        let scan_seam: Vec<&WorktreeInfo> =
            listed.iter().filter(|w| w.name == "scan-seam").collect();
        assert_eq!(scan_seam.len(), 1);
        assert!(
            scan_seam[0].dirty_files.is_some(),
            "premise: listing counted dirty files"
        );

        let tally = scan_worktree_overlays(&scan_seam, |_| Err(CommandError::from("boom")));
        assert_eq!(tally.scanned, 0);
        assert_eq!(tally.unscanned, 1);
        assert_eq!(tally.errors.len(), 1);
        assert!(tally.errors[0].error.contains("boom"));
    }

    #[test]
    fn scan_tally_carries_a_truncated_worktree_scan_into_the_radar_flag() {
        let (_a, _p, listed) = listed_worktree_with_dirty_files();
        let scan_seam: Vec<&WorktreeInfo> =
            listed.iter().filter(|w| w.name == "scan-seam").collect();
        let tally = scan_worktree_overlays(&scan_seam, |_| {
            Ok(DirtyNodeScan {
                dirty_nodes: RoaringBitmap::new(),
                truncated: true,
            })
        });
        assert_eq!(tally.scanned, 1);
        assert!(tally.truncated);
        let tally = scan_worktree_overlays(&scan_seam, |_| Ok(DirtyNodeScan::default()));
        assert!(!tally.truncated);
    }

    #[test]
    fn test_threaded_messages_and_inbox_lifecycle_with_vespin_asks() {
        let anchor_dir = crate::codeintel::tests::repo_with_one_generation();
        let anchor = anchor_dir.path().to_str().unwrap();
        crate::test_support::trust_repo(anchor_dir.path());

        // 1. Post a structured ask (decide) under thread "db-migration"
        let ask_body = serde_json::json!({
            "ask_type": "decide",
            "question": "Use SQLite or JSON?",
            "options": ["SQLite", "JSON"]
        })
        .to_string();

        let m1 = post_thread_message(
            anchor,
            "claude-worker-1",
            Some("orchestrator"),
            Some("db-migration"),
            "ask",
            &ask_body,
            Some("feat/migration"),
        )
        .unwrap();
        assert_eq!(m1.seq, 1);
        assert_eq!(m1.thread_key.as_deref(), Some("db-migration"));

        // 2. Read thread messages
        let thread_page = read_thread_messages(anchor, Some("db-migration"), None, 0).unwrap();
        assert_eq!(thread_page.messages.len(), 1);
        assert_eq!(
            thread_page.messages[0].thread_key.as_deref(),
            Some("db-migration")
        );

        // 3. List inbox threads: verify status is open and ask_type is decide
        let threads_open = list_inbox_threads(anchor, None, Some("open")).unwrap();
        assert_eq!(threads_open.len(), 1);
        assert_eq!(threads_open[0].thread_key, "db-migration");
        assert_eq!(threads_open[0].status, "open");
        assert_eq!(threads_open[0].ask_type.as_deref(), Some("decide"));
        assert_eq!(threads_open[0].title, "Use SQLite or JSON?");
        assert_eq!(threads_open[0].messages.len(), 1);

        // 4. Answer the ask
        let m2 = answer_ask(
            anchor,
            "human-dev",
            "db-migration",
            "Selected option: SQLite",
        )
        .unwrap();
        assert_eq!(m2.seq, 2);
        assert_eq!(m2.kind, "reply");
        assert_eq!(m2.thread_key.as_deref(), Some("db-migration"));

        // Verify thread status transitioned to answered
        let threads_answered = list_inbox_threads(anchor, None, Some("answered")).unwrap();
        assert_eq!(threads_answered.len(), 1);
        assert_eq!(threads_answered[0].status, "answered");
        assert_eq!(threads_answered[0].messages.len(), 2);

        // Filter open threads: should be 0 now
        let threads_open_now = list_inbox_threads(anchor, None, Some("open")).unwrap();
        assert_eq!(threads_open_now.len(), 0);

        // 5. Post another ask in a separate thread and withdraw it
        let ask2_body = serde_json::json!({
            "ask_type": "do",
            "question": "Run manual database seed",
            "steps": ["Create test tables", "Seed admin user"]
        })
        .to_string();

        let m3 = post_thread_message(
            anchor,
            "claude-worker-2",
            None,
            Some("db-seed"),
            "ask",
            &ask2_body,
            None,
        )
        .unwrap();
        assert_eq!(m3.seq, 3);

        let m4 = withdraw_ask(
            anchor,
            "claude-worker-2",
            "db-seed",
            "Resolved automatically via migration script",
        )
        .unwrap();
        assert_eq!(m4.seq, 4);
        assert_eq!(m4.kind, "withdraw");

        // Verify thread status is withdrawn
        let threads_withdrawn = list_inbox_threads(anchor, None, Some("withdrawn")).unwrap();
        assert_eq!(threads_withdrawn.len(), 1);
        assert_eq!(threads_withdrawn[0].thread_key, "db-seed");
        assert_eq!(threads_withdrawn[0].status, "withdrawn");

        // Verify total threads in "all"
        let all_threads = list_inbox_threads(anchor, None, None).unwrap();
        assert_eq!(all_threads.len(), 2);
    }

    #[test]
    fn test_inbox_threads_multibyte_utf8_truncation_without_panic() {
        let anchor_dir = crate::codeintel::tests::repo_with_one_generation();
        let anchor = anchor_dir.path().to_str().unwrap();
        crate::test_support::trust_repo(anchor_dir.path());

        // 1. Emoji 🚀 (4 bytes: 55..59)
        let body_emoji = format!(
            "{}🚀{}",
            "A".repeat(55),
            "tail_exceeding_sixty_characters_limit"
        );
        post_thread_message(
            anchor,
            "agent-1",
            None,
            Some("t-emoji"),
            "note",
            &body_emoji,
            None,
        )
        .unwrap();

        // 2. CJK character 中 (3 bytes: 56..59)
        let body_cjk = format!(
            "{}中{}",
            "B".repeat(56),
            "tail_exceeding_sixty_characters_limit"
        );
        post_thread_message(
            anchor,
            "agent-2",
            None,
            Some("t-cjk"),
            "note",
            &body_cjk,
            None,
        )
        .unwrap();

        // 3. Accented character é (2 bytes: 56..58)
        let body_accent = format!(
            "{}é{}",
            "C".repeat(56),
            "tail_exceeding_sixty_characters_limit"
        );
        post_thread_message(
            anchor,
            "agent-3",
            None,
            Some("t-accent"),
            "note",
            &body_accent,
            None,
        )
        .unwrap();

        // 4. Boundary cases: 60 chars and 61 chars
        let body_60 = "D".repeat(60);
        post_thread_message(
            anchor,
            "agent-4",
            None,
            Some("t-60"),
            "note",
            &body_60,
            None,
        )
        .unwrap();

        let body_61 = "E".repeat(61);
        post_thread_message(
            anchor,
            "agent-5",
            None,
            Some("t-61"),
            "note",
            &body_61,
            None,
        )
        .unwrap();

        let threads =
            list_inbox_threads(anchor, None, None).expect("must not panic on multi-byte UTF-8");
        assert_eq!(threads.len(), 5);

        let t_emoji = threads.iter().find(|t| t.thread_key == "t-emoji").unwrap();
        assert!(t_emoji.title.ends_with('…'));
        assert_eq!(t_emoji.title.chars().count(), 58);
        assert!(t_emoji.title.contains('🚀'));

        let t_cjk = threads.iter().find(|t| t.thread_key == "t-cjk").unwrap();
        assert!(t_cjk.title.ends_with('…'));
        assert_eq!(t_cjk.title.chars().count(), 58);
        assert!(t_cjk.title.contains('中'));

        let t_accent = threads.iter().find(|t| t.thread_key == "t-accent").unwrap();
        assert!(t_accent.title.ends_with('…'));
        assert_eq!(t_accent.title.chars().count(), 58);
        assert!(t_accent.title.contains('é'));

        let t_60 = threads.iter().find(|t| t.thread_key == "t-60").unwrap();
        assert_eq!(t_60.title, "D".repeat(60));

        let t_61 = threads.iter().find(|t| t.thread_key == "t-61").unwrap();
        assert_eq!(t_61.title, format!("{}…", "E".repeat(57)));
        assert_eq!(t_61.title.chars().count(), 58);
    }

    #[test]
    fn peer_registry_reconciles_antigravity_presence_flock_and_resumable_transcripts() {
        let anchor_dir = crate::codeintel::tests::repo_with_one_generation();
        let anchor = anchor_dir.path().to_str().unwrap();
        crate::test_support::trust_repo(anchor_dir.path());

        let agy_home = tempfile::tempdir().unwrap();
        let presence_dir = agy_home.path().join("presence");
        fs::create_dir_all(&presence_dir).unwrap();

        let conv_live = "11111111-2222-4333-8444-555555555555";
        let conv_stale = "66666666-7777-4888-9999-aaaaaaaaaaaa";

        let live_lock_path = presence_dir.join(format!("{conv_live}.lock"));
        let stale_lock_path = presence_dir.join(format!("{conv_stale}.lock"));
        let live_lock_file = fs::OpenOptions::new()
            .create(true)
            .truncate(true)
            .read(true)
            .write(true)
            .open(&live_lock_path)
            .unwrap();
        // Leave stale_lock_path on disk unlocked (exact Antigravity post-exit behavior).
        fs::write(&stale_lock_path, b"").unwrap();

        #[cfg(unix)]
        {
            use std::os::unix::io::AsRawFd;
            let rc =
                unsafe { libc::flock(live_lock_file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) };
            assert_eq!(rc, 0, "test holds active flock on conv_live");
        }

        let homes = vec![agy_home.path().to_path_buf()];
        assert!(is_antigravity_presence_locked(&homes, conv_live));
        assert!(
            !is_antigravity_presence_locked(&homes, conv_stale),
            "unlocked 0-byte presence file must not report live"
        );

        let p1 = upsert_peer(
            anchor,
            PeerUpsert {
                provider: "antigravity".into(),
                surface: "ide".into(),
                provider_session_id: Some(conv_live.into()),
                run_id: Some("run-agy-1".into()),
                worktree_path: anchor.into(),
                branch: Some("main".into()),
                role: Some("planner".into()),
                turn_state: Some("idle".into()),
                ..Default::default()
            },
        )
        .unwrap();
        assert_eq!(p1.provider, "agy");
        assert_eq!(p1.peer_id, format!("agy:{conv_live}"));
        assert_eq!(
            peer_session_id_for_run(anchor, "run-agy-1").as_deref(),
            Some(conv_live)
        );

        upsert_peer(
            anchor,
            PeerUpsert {
                provider: "agy".into(),
                surface: "terminal".into(),
                provider_session_id: Some(conv_stale.into()),
                worktree_path: anchor.into(),
                branch: Some("feat/stale".into()),
                turn_state: Some("idle".into()),
                ..Default::default()
            },
        )
        .unwrap();

        let reconciled = list_reconciled_peers_with(
            anchor,
            &homes,
            &|_pid, _birth| crate::workbench::process_birth::Liveness::Gone,
            &|_prov, sid| sid == conv_stale,
        )
        .unwrap();

        let r_live = reconciled
            .iter()
            .find(|p| p.record.provider_session_id.as_deref() == Some(conv_live))
            .expect("conv_live peer");
        assert_eq!(r_live.liveness, "live");

        let r_stale = reconciled
            .iter()
            .find(|p| p.record.provider_session_id.as_deref() == Some(conv_stale))
            .expect("conv_stale peer");
        assert_eq!(r_stale.liveness, "resumable");
        assert_eq!(r_stale.record.turn_state, "exited");

        drop(live_lock_file);
        let reconciled_after_exit = list_reconciled_peers_with(
            anchor,
            &homes,
            &|_pid, _birth| crate::workbench::process_birth::Liveness::Gone,
            &|_prov, _sid| false,
        )
        .unwrap();
        let r_exited = reconciled_after_exit
            .iter()
            .find(|p| p.record.provider_session_id.as_deref() == Some(conv_live))
            .unwrap();
        assert_eq!(r_exited.liveness, "exited");
    }

    #[test]
    fn resolve_target_from_list_fails_closed_on_ambiguous_provider_or_branch() {
        let sid_a = "aaaaaaaa-1111-4222-8333-444444444444";
        let sid_b = "bbbbbbbb-1111-4222-8333-444444444444";
        let peers = vec![
            ReconciledPeer {
                record: PeerRecord {
                    peer_id: format!("claude:{sid_a}"),
                    provider: "claude".into(),
                    surface: "terminal".into(),
                    provider_session_id: Some(sid_a.into()),
                    terminal_session_id: Some("term-1".into()),
                    run_id: None,
                    worktree_path: "/repo/wt-a".into(),
                    branch: Some("main".into()),
                    role: Some("reviewer".into()),
                    turn_state: "idle".into(),
                    auto_wake: false,
                    pid: Some(101),
                    proc_start_ns: Some(1000),
                    transcript_path: None,
                    updated_at_ms: 10,
                },
                liveness: "live".into(),
                liveness_reason: None,
            },
            ReconciledPeer {
                record: PeerRecord {
                    peer_id: format!("claude:{sid_b}"),
                    provider: "claude".into(),
                    surface: "external_cli".into(),
                    provider_session_id: Some(sid_b.into()),
                    terminal_session_id: None,
                    run_id: None,
                    worktree_path: "/repo/wt-b".into(),
                    branch: Some("main".into()),
                    role: Some("implementer".into()),
                    turn_state: "running".into(),
                    auto_wake: true,
                    pid: Some(102),
                    proc_start_ns: Some(2000),
                    transcript_path: None,
                    updated_at_ms: 20,
                },
                liveness: "live".into(),
                liveness_reason: None,
            },
        ];

        // Bare provider or shared branch MUST fail closed with AmbiguousPeer.
        match resolve_target_from_list(&peers, "claude") {
            Err(PeerResolutionError::AmbiguousPeer { candidates, .. }) => {
                assert_eq!(candidates.len(), 2);
            }
            other => panic!("expected AmbiguousPeer on 'claude', got {other:?}"),
        }
        match resolve_target_from_list(&peers, "main") {
            Err(PeerResolutionError::AmbiguousPeer { candidates, .. }) => {
                assert_eq!(candidates.len(), 2);
            }
            other => panic!("expected AmbiguousPeer on 'main', got {other:?}"),
        }

        // Explicit role, session UUID, or peer_id resolves deterministically.
        let by_role = resolve_target_from_list(&peers, "reviewer").unwrap();
        assert_eq!(by_role.record.provider_session_id.as_deref(), Some(sid_a));
        let by_sid = resolve_target_from_list(&peers, sid_b).unwrap();
        assert_eq!(by_sid.record.peer_id, format!("claude:{sid_b}"));
    }

    #[test]
    fn post_peer_message_hydrates_large_payload_idempotently_and_acks_unread() {
        let anchor_dir = crate::codeintel::tests::repo_with_one_generation();
        let anchor = anchor_dir.path().to_str().unwrap();
        crate::test_support::trust_repo(anchor_dir.path());

        let large_body = format!("header\r\n{}", "payload-line\n".repeat(700));
        assert!(large_body.len() > crate::agent_mailbox::INLINE_PREVIEW_BYTES);
        assert!(large_body.len() < MAX_MESSAGE_BODY_BYTES);

        let m1 = post_peer_message(
            anchor,
            "agy:11111111-2222-4333-8444-555555555555",
            Some("claude:aaaaaaaa-1111-4222-8333-444444444444"),
            Some("a2a-review"),
            "peer_message",
            &large_body,
            Some("feat/a2a"),
            Some("req_large_001"),
            1,
            6,
        )
        .unwrap();
        assert_eq!(m1.msg_id.as_deref(), Some("req_large_001"));
        assert_eq!(m1.hop, Some(1));
        assert_eq!(m1.max_hops, Some(6));
        assert!(!m1.body.contains('\r'), "CRLF normalized to LF");

        // Idempotent retry with identical msg_id + body + sender returns existing message.
        let m1_dup = post_peer_message(
            anchor,
            "agy:11111111-2222-4333-8444-555555555555",
            Some("claude:aaaaaaaa-1111-4222-8333-444444444444"),
            Some("a2a-review"),
            "peer_message",
            &large_body,
            Some("feat/a2a"),
            Some("req_large_001"),
            1,
            6,
        )
        .unwrap();
        assert_eq!(m1_dup.seq, m1.seq);

        // Conflicting payload on same msg_id fails closed.
        assert!(post_peer_message(
            anchor,
            "agy:11111111-2222-4333-8444-555555555555",
            Some("claude:aaaaaaaa-1111-4222-8333-444444444444"),
            Some("a2a-review"),
            "peer_message",
            "different body",
            Some("feat/a2a"),
            Some("req_large_001"),
            1,
            6,
        )
        .is_err());

        let hydrated = get_message_by_id(anchor, "req_large_001")
            .unwrap()
            .expect("full body stored in agent_bus");
        assert_eq!(hydrated.body, m1.body);

        let aliases = vec![
            "claude:aaaaaaaa-1111-4222-8333-444444444444".to_string(),
            "aaaaaaaa-1111-4222-8333-444444444444".to_string(),
        ];
        let unread = read_unread_for_peer(anchor, &aliases, 0, None, true).unwrap();
        assert_eq!(unread.messages.len(), 1);

        let acked = ack_messages(
            anchor,
            "claude:aaaaaaaa-1111-4222-8333-444444444444",
            &["req_large_001".to_string()],
        )
        .unwrap();
        assert_eq!(acked, 1);

        let unread_after = read_unread_for_peer(anchor, &aliases, 0, None, true).unwrap();
        assert!(unread_after.messages.is_empty());
    }

    #[test]
    fn reconcile_classifies_surface_and_canonicalizes_legacy_hook_surfaces() {
        let anchor_dir = crate::codeintel::tests::repo_with_one_generation();
        let anchor = anchor_dir.path().to_str().unwrap();
        crate::test_support::trust_repo(anchor_dir.path());

        // Legacy hook surface spellings canonicalize on upsert.
        let legacy_ide = upsert_peer(
            anchor,
            PeerUpsert {
                peer_id: None,
                provider: "antigravity".into(),
                surface: "antigravity_ide".into(),
                provider_session_id: Some("88888888-8888-4888-8888-888888888881".into()),
                terminal_session_id: None,
                run_id: None,
                worktree_path: anchor.into(),
                branch: Some("main".into()),
                role: None,
                turn_state: Some("idle".into()),
                auto_wake: None,
                pid: Some(99001),
                proc_start_ns: Some(100),
                transcript_path: Some("/tmp/ide.pb".into()),
            },
        )
        .unwrap();
        assert_eq!(legacy_ide.provider, "agy");
        assert_eq!(legacy_ide.surface, "ide");

        let legacy_term = upsert_peer(
            anchor,
            PeerUpsert {
                peer_id: None,
                provider: "antigravity".into(),
                surface: "hosted_terminal".into(),
                provider_session_id: Some("88888888-8888-4888-8888-888888888882".into()),
                terminal_session_id: Some("term-99".into()),
                run_id: None,
                worktree_path: anchor.into(),
                branch: Some("main".into()),
                role: None,
                turn_state: Some("idle".into()),
                auto_wake: None,
                pid: Some(99002),
                proc_start_ns: Some(100),
                transcript_path: None,
            },
        )
        .unwrap();
        assert_eq!(legacy_term.provider, "agy");
        assert_eq!(legacy_term.surface, "terminal");

        // Process-scanned sessions distinguish IDE, external_terminal task run, managed run, and external_cli.
        let obs = vec![crate::insights::live_sessions::KindObservation {
            kind: "antigravity".into(),
            ok: true,
            live: vec![
                // 1. Antigravity IDE (`ide_hosted: true`)
                crate::insights::LiveSession {
                    kind: "antigravity".into(),
                    pid: 88001,
                    entrypoint: String::new(),
                    status: "running".into(),
                    cwd: anchor.into(),
                    tree: None,
                    run_id: None,
                    parent_pid: None,
                    session_id: None,
                    proc_start: None,
                    registry_cwd: None,
                    ide_hosted: true,
                    terminal_session_id: None,
                },
                // 2. External-terminal task run (`GITPULSE_RUN_ID` + `GITPULSE_SESSION_ID` -> `tree: "run:r1"`, `terminal_session_id: Some("term-1")`)
                crate::insights::LiveSession {
                    kind: "antigravity".into(),
                    pid: 88002,
                    entrypoint: String::new(),
                    status: "running".into(),
                    cwd: anchor.into(),
                    tree: Some("run:r1".into()),
                    run_id: Some("r1".into()),
                    parent_pid: None,
                    session_id: None,
                    proc_start: None,
                    registry_cwd: None,
                    ide_hosted: false,
                    terminal_session_id: Some("term-1".into()),
                },
                // 3. Managed run (`run_id: Some("r2")`, no `terminal_session_id`)
                crate::insights::LiveSession {
                    kind: "antigravity".into(),
                    pid: 88003,
                    entrypoint: String::new(),
                    status: "running".into(),
                    cwd: anchor.into(),
                    tree: Some("run:r2".into()),
                    run_id: Some("r2".into()),
                    parent_pid: None,
                    session_id: None,
                    proc_start: None,
                    registry_cwd: None,
                    ide_hosted: false,
                    terminal_session_id: None,
                },
                // 4. Plain external `agy` CLI (`ide_hosted: false`, no launch ids)
                crate::insights::LiveSession {
                    kind: "antigravity".into(),
                    pid: 88004,
                    entrypoint: String::new(),
                    status: "running".into(),
                    cwd: anchor.into(),
                    tree: None,
                    run_id: None,
                    parent_pid: None,
                    session_id: None,
                    proc_start: None,
                    registry_cwd: None,
                    ide_hosted: false,
                    terminal_session_id: None,
                },
            ],
            ..Default::default()
        }];
        let reconciled = list_reconciled_peers_from_observation(
            anchor,
            &[],
            &obs,
            &|_, _| crate::workbench::process_birth::Liveness::Gone,
            &|_, _| false,
        )
        .unwrap();
        let by_pid = |pid: u32| {
            reconciled
                .iter()
                .find(|p| p.record.pid == Some(pid))
                .unwrap_or_else(|| panic!("missing pid {pid} in {reconciled:?}"))
        };
        assert_eq!(by_pid(88001).record.surface, "ide");
        assert_eq!(by_pid(88002).record.surface, "terminal");
        assert_eq!(
            by_pid(88002).record.terminal_session_id.as_deref(),
            Some("term-1")
        );
        assert_eq!(by_pid(88002).record.run_id.as_deref(), Some("r1"));
        assert_eq!(by_pid(88003).record.surface, "managed");
        assert_eq!(by_pid(88004).record.surface, "external_cli");
    }
}
