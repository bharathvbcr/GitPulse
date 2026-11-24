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
use crate::engine::git_cli::{resolve_git_common_dir, validate_repo};
use crate::engine::object_quarantine::{MergeOutcome, ObjectQuarantine};
use crate::engine::process::DEFAULT_TIMEOUT;
use crate::engine::worktree::{
    admit_worktree_pair, head_branch_from_metadata, list_worktrees, list_worktrees_lite,
    WorktreeInfo,
};
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
/// Most unread direct messages kept past the row cap for one recipient, so one chatty
/// pair cannot push a quiet peer's message out.
pub const MAX_PINNED_UNREAD_PER_RECIPIENT: usize = 8;
/// Most unread direct messages kept past the row cap across all recipients.
pub const MAX_PINNED_UNREAD_DIRECT: usize = 64;
pub const MAX_READ_BY_ENTRIES: usize = 64;
pub const DEFAULT_MAX_HOPS: u32 = 6;
pub const MAX_ODB_RADAR_PAIRS: usize = 32;
pub const MAX_RADAR_CONFLICTS_PER_LAYER: usize = 128;

const LANES_SUBDIR: &str = "gitpulse/lanes";
const CLAIMS_FILE: &str = "claims.json";
const MESSAGES_FILE: &str = "messages.json";
/// Largest `messages.json` any write leaves behind and any read accepts.
const MESSAGES_FILE_CAP: u64 = 8 * 1024 * 1024;
/// Largest `messages.json` a write will load so an over-cap file (written by an older build
/// that counted body bytes, not stored bytes) can be compacted instead of failing forever.
const MESSAGES_WRITE_CEILING: u64 = 4 * MESSAGES_FILE_CAP;
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
    read_json_bounded_hint(path, cap_bytes, "")
}

/// [`read_json_bounded`] whose over-cap error ends with `hint`, so a reader that cannot
/// repair the file can say who will.
fn read_json_bounded_hint<T: for<'de> Deserialize<'de> + Default>(
    path: &Path,
    cap_bytes: u64,
    hint: &str,
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
            "coordination state {} exceeds {} byte cap{hint}",
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
    require_agent_sender(&agent)?;
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

/// Canonical bus sender identity reserved exclusively for GitPulse's in-process
/// Manvi system steward (`harness::steward`, [`MessageOrigin::SystemSteward`]).
pub const STEWARD_SENDER: &str = "manvi-steward";
/// Canonical bus thread key for Manvi system steward operational directives.
pub const STEWARD_THREAD_KEY: &str = "manvi-system-steward";
/// Canonical bus message kind for Manvi system steward operational directives.
pub const STEWARD_MESSAGE_KIND: &str = "system-advisory";

/// Who a bus message comes from, decided by the entry point that accepted it and never by
/// the sender name the caller supplied.
///
/// Only the app's own Tauri commands (a person using the UI) pass [`MessageOrigin::Person`],
/// and only the app's in-process steward (`harness::steward`) passes
/// [`MessageOrigin::SystemSteward`]. Every MCP tool, CLI and hook passes
/// [`MessageOrigin::Agent`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MessageOrigin {
    /// A person acting in the GitPulse UI. Hop 0, and exempt from the hop limit.
    Person,
    /// GitPulse's in-process Manvi system steward (`harness::steward`). Hop 0,
    /// and the only origin permitted to post under [`STEWARD_SENDER`] or
    /// [`STEWARD_MESSAGE_KIND`].
    SystemSteward,
    /// An agent, whatever name it gives itself. Hop at least 1, and the hop limit applies.
    Agent,
}

impl MessageOrigin {
    /// The hop this origin may carry: a person's or system steward's message is hop 0,
    /// an agent's is never below 1.
    #[must_use]
    pub fn effective_hop(self, requested: u32) -> u32 {
        match self {
            Self::Person | Self::SystemSteward => 0,
            Self::Agent => requested.max(1),
        }
    }
}

/// Sender names the UI uses for a person. An agent may not post under one, because the
/// receiving agent and the inbox view read them as "the person using GitPulse".
fn is_reserved_person_name(name: &str) -> bool {
    let name = name.trim();
    ["developer", "human", "human-dev", "user"]
        .iter()
        .any(|reserved| name.eq_ignore_ascii_case(reserved))
}

/// Sender names reserved for GitPulse's Manvi system steward, including `prefix:id`
/// peer forms, so no untrusted agent can spoof a host-governor directive on the bus.
fn is_reserved_steward_name(name: &str) -> bool {
    let lower = name.trim().to_ascii_lowercase();
    let head = lower.split(':').next().unwrap_or(lower.as_str());
    matches!(
        head,
        "manvi-steward" | "system-steward" | "manvi-system-steward" | "gitpulse-steward"
    )
}

/// The sender name GitPulse itself uses for notices it leaves in a mailbox (the overflow
/// notice). The mailbox trusts it, so `require_agent_sender` keeps agents from using it.
pub const GITPULSE_SENDER_NAME: &str = "gitpulse";

/// Only the exact name is reserved, not `gitpulse:<x>`, because peer ids may use that form.
fn is_reserved_gitpulse_name(name: &str) -> bool {
    name.trim().eq_ignore_ascii_case(GITPULSE_SENDER_NAME)
}

/// Message kinds reserved for [`MessageOrigin::SystemSteward`].
fn is_reserved_steward_kind(kind: &str) -> bool {
    let lower = kind.trim().to_ascii_lowercase();
    matches!(lower.as_str(), "system-advisory" | "system_steward")
}

/// Thread keys reserved for [`MessageOrigin::SystemSteward`] so untrusted agents
/// cannot inject messages into or withdraw the system steward thread.
fn is_reserved_steward_thread_key(thread_key: &str) -> bool {
    let lower = thread_key.trim().to_ascii_lowercase();
    matches!(lower.as_str(), "manvi-system-steward" | "system-steward")
}

/// Refuses a sender name reserved for a person or GitPulse's Manvi system steward.
/// Every agent entry point calls this before it acts on the name, and
/// [`post_peer_message`] calls it again for the posts it owns.
pub fn require_agent_sender(from_agent: &str) -> Result<(), CommandError> {
    if is_reserved_person_name(from_agent) {
        return Err(CommandError::from(
            "from_agent is a name reserved for the person using GitPulse; an agent must send as its own peer id",
        ));
    }
    if is_reserved_steward_name(from_agent) {
        return Err(CommandError::from(
            "from_agent is reserved for GitPulse's Manvi system steward; an agent must send as its own peer id",
        ));
    }
    if is_reserved_gitpulse_name(from_agent) {
        return Err(CommandError::from(
            "from_agent is reserved for GitPulse itself; an agent must send as its own peer id",
        ));
    }
    Ok(())
}

/// A peer's role is a description ("developer", "reviewer"), never shown as a
/// sender, so the person's names stay usable; only the steward's are refused,
/// so no peer can present itself as the host governor.
pub fn require_role_not_steward(role: &str) -> Result<(), CommandError> {
    if is_reserved_steward_name(role) {
        return Err(CommandError::from(
            "role is reserved for GitPulse's Manvi system steward",
        ));
    }
    Ok(())
}

/// Posts a bounded agent-to-agent coordination message with an optional thread key.
#[allow(clippy::too_many_arguments)]
pub fn post_thread_message(
    repo_path: &str,
    origin: MessageOrigin,
    from_agent: &str,
    to_agent: Option<&str>,
    thread_key: Option<&str>,
    kind: &str,
    body: &str,
    branch: Option<&str>,
) -> Result<AgentBusMessage, CommandError> {
    post_peer_message(
        repo_path,
        origin,
        from_agent,
        to_agent,
        thread_key,
        kind,
        body,
        branch,
        None,
        1,
        DEFAULT_MAX_HOPS,
    )
}

/// Posts a bounded peer message with explicit `msg_id`, `hop`, and `max_hops`.
///
/// `origin` fixes the hop (see [`MessageOrigin::effective_hop`]), and an
/// [`MessageOrigin::Agent`] post is refused when `from_agent` is a name reserved for a person.
#[allow(clippy::too_many_arguments)]
pub fn post_peer_message(
    repo_path: &str,
    origin: MessageOrigin,
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
    if origin == MessageOrigin::Agent {
        require_agent_sender(from_agent)?;
        if is_reserved_steward_kind(kind) {
            return Err(CommandError::from(
                "kind is reserved for GitPulse's Manvi system steward",
            ));
        }
        if let Some(tk) = thread_key {
            if is_reserved_steward_thread_key(tk) {
                return Err(CommandError::from(
                    "thread_key is reserved for GitPulse's Manvi system steward",
                ));
            }
        }
    }
    let hop = origin.effective_hop(hop);
    let from = sanitize_token("from_agent", from_agent, 128)?;
    if origin == MessageOrigin::SystemSteward && from != STEWARD_SENDER {
        return Err(CommandError::from(format!(
            "SystemSteward origin requires from_agent = '{STEWARD_SENDER}'"
        )));
    }
    let to = match to_agent.map(str::trim).filter(|s| !s.is_empty()) {
        Some(target) => Some(sanitize_token("to_agent", target, 128)?),
        None => None,
    };
    let t_key = match thread_key.map(str::trim).filter(|s| !s.is_empty()) {
        Some(k) => Some(sanitize_token("thread_key", k, 80)?),
        None => None,
    };
    let msg_kind = sanitize_token("kind", kind, 64)?;
    if origin == MessageOrigin::SystemSteward && msg_kind != STEWARD_MESSAGE_KIND {
        return Err(CommandError::from(format!(
            "SystemSteward origin requires kind = '{STEWARD_MESSAGE_KIND}'"
        )));
    }
    let normalized_body = crate::agent_mailbox::normalize_crlf(body);
    let trimmed_body = normalized_body.trim();
    if trimmed_body.is_empty() || trimmed_body.contains('\0') {
        return Err(CommandError::from("message body must not be empty"));
    }
    if crate::agent_mailbox::has_disallowed_control(trimmed_body) {
        return Err(CommandError::from(
            "message body cannot contain control characters other than line breaks and tabs",
        ));
    }
    // The file stores the body JSON-escaped (a quote or backslash is two bytes), so the cap
    // counts the stored form: counting raw bytes let a quote-heavy body double its footprint.
    let stored_len = serde_json::to_string(trimmed_body)
        .map_err(|e| CommandError::from(format!("cannot encode message body: {e}")))?
        .len()
        .saturating_sub(2);
    if stored_len > MAX_MESSAGE_BODY_BYTES {
        return Err(CommandError::from(format!(
            "message body exceeds maximum of {MAX_MESSAGE_BODY_BYTES} bytes: {} bytes raw, {stored_len} bytes as stored (JSON-escaped)",
            trimmed_body.len()
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
        let mut state: MessagesState = read_json_bounded(&messages_path, MESSAGES_WRITE_CEILING)?;

        if let Some(ref want_id) = caller_msg_id {
            if let Some(existing) = state
                .messages
                .iter()
                .find(|m| m.msg_id.as_deref() == Some(want_id.as_str()))
            {
                // A retry is the same message only when every field that routes or threads it
                // matches. A reused id aimed at another recipient, kind or thread would otherwise
                // be reported as the first message while the caller believes it went elsewhere.
                let mut differs: Vec<&str> = Vec::new();
                if existing.body != trimmed_body {
                    differs.push("body");
                }
                if existing.from_agent != from {
                    differs.push("from_agent");
                }
                if existing.to_agent != to {
                    differs.push("to_agent");
                }
                if existing.kind != msg_kind {
                    differs.push("kind");
                }
                if existing.thread_key != t_key {
                    differs.push("thread_key");
                }
                if differs.is_empty() {
                    return Ok(existing.clone());
                }
                return Err(CommandError::from(format!(
                    "msg_id '{want_id}' already exists with a different payload (mismatched: {})",
                    differs.join(", ")
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
        let bytes = compact_messages_for_write(&mut state, &messages_path, Some(seq))?;
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

/// Reads `messages.json` for a path that cannot repair it. An over-cap file is refused, and
/// the error says the next write (a post or an ack) compacts it.
fn read_messages_readonly(path: &Path) -> Result<MessagesState, CommandError> {
    read_json_bounded_hint(
        path,
        MESSAGES_FILE_CAP,
        "; the next post or ack compacts it",
    )
}

/// Seqs of the rows an eviction must keep while it can: open asks (newest
/// `MAX_PINNED_OPEN_ASKS`) and unread direct messages younger than the mailbox's pending
/// TTL (newest `MAX_PINNED_UNREAD_PER_RECIPIENT` per recipient, newest
/// `MAX_PINNED_UNREAD_DIRECT` overall).
fn pinned_message_seqs(messages: &[AgentBusMessage], now_ms: u64) -> BTreeSet<u64> {
    let mut open_ask_seq_by_thread: HashMap<String, u64> = HashMap::new();
    for existing in messages {
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
    let mut open_asks: Vec<u64> = open_ask_seq_by_thread.into_values().collect();
    open_asks.sort_unstable();
    let mut pinned: BTreeSet<u64> = open_asks
        .into_iter()
        .rev()
        .take(MAX_PINNED_OPEN_ASKS)
        .collect();

    let ttl_ms = u64::try_from(crate::agent_mailbox::PENDING_TTL.as_millis()).unwrap_or(u64::MAX);
    let mut per_recipient: HashMap<&str, usize> = HashMap::new();
    let mut unread: Vec<u64> = Vec::new();
    for msg in messages.iter().rev() {
        let Some(to) = msg.to_agent.as_deref() else {
            continue;
        };
        if !msg.read_by.is_empty() || now_ms.saturating_sub(msg.created_at_ms) >= ttl_ms {
            continue;
        }
        let taken = per_recipient.entry(to).or_insert(0);
        if *taken < MAX_PINNED_UNREAD_PER_RECIPIENT {
            *taken += 1;
            unread.push(msg.seq);
        }
    }
    // `unread` is newest first, so the overall cap keeps the newest.
    pinned.extend(unread.into_iter().take(MAX_PINNED_UNREAD_DIRECT));
    pinned
}

/// Seqs in the order rows are given up: every unpinned row oldest first, then pinned rows
/// oldest first, so a pinned row only goes once nothing unpinned is left.
fn eviction_order(messages: &[AgentBusMessage], now_ms: u64) -> Vec<u64> {
    let pinned = pinned_message_seqs(messages, now_ms);
    let (mut order, held): (Vec<u64>, Vec<u64>) = messages
        .iter()
        .map(|m| m.seq)
        .partition(|seq| !pinned.contains(seq));
    order.extend(held);
    order
}

/// Removes the rows with these seqs and raises `dropped_before_seq` past each. `next_seq`
/// is never touched, so a dropped seq is never reused.
fn drop_message_seqs(state: &mut MessagesState, seqs: &[u64]) {
    let gone: BTreeSet<u64> = seqs.iter().copied().collect();
    state.messages.retain(|m| {
        if gone.contains(&m.seq) {
            state.dropped_before_seq = state.dropped_before_seq.max(m.seq.saturating_add(1));
            false
        } else {
            true
        }
    });
}

/// Trims `state` to the row cap, then to the file cap, and returns the bytes to write, so no
/// write ever leaves `messages.json` over what a reader accepts. A file an older build left
/// over the cap is healed here by dropping rows in eviction order.
fn compact_messages_for_write(
    state: &mut MessagesState,
    messages_path: &Path,
    keep_seq: Option<u64>,
) -> Result<Vec<u8>, CommandError> {
    let now_ms = now_epoch_ms();
    // The row being posted is never a candidate: an Ok from a post means the message is on
    // the bus, so a state that cannot hold it fails the post instead.
    let order_for = |messages: &[AgentBusMessage]| {
        let mut order = eviction_order(messages, now_ms);
        order.retain(|seq| Some(*seq) != keep_seq);
        order
    };
    if state.messages.len() > MAX_BUS_MESSAGES {
        let excess = state.messages.len() - MAX_BUS_MESSAGES;
        let order = order_for(&state.messages);
        drop_message_seqs(state, &order[..excess.min(order.len())]);
    }
    let serialize = |state: &MessagesState| {
        serde_json::to_vec_pretty(state)
            .map_err(|e| CommandError::from(format!("cannot serialize messages: {e}")))
    };
    let mut bytes = serialize(state)?;
    if bytes.len() as u64 <= MESSAGES_FILE_CAP {
        return Ok(bytes);
    }

    let rows_before = state.messages.len();
    let order = order_for(&state.messages);
    let row_bytes: HashMap<u64, usize> = state
        .messages
        .iter()
        .map(|m| {
            let len = serde_json::to_string_pretty(m).map_or(0, |s| s.len());
            (m.seq, len)
        })
        .collect();
    let mut next = 0usize;
    while bytes.len() as u64 > MESSAGES_FILE_CAP {
        let excess = bytes.len() - MESSAGES_FILE_CAP as usize;
        let start = next;
        let mut freed = 0usize;
        while freed < excess && next < order.len() {
            freed += row_bytes.get(&order[next]).copied().unwrap_or(0).max(1);
            next += 1;
        }
        if next == start {
            return Err(CommandError::from(match keep_seq {
                Some(_) => format!(
                    "the new message does not fit in {} under its {MESSAGES_FILE_CAP} byte cap even with every other message evicted",
                    messages_path.display()
                ),
                None => format!(
                    "cannot compact {} under its {MESSAGES_FILE_CAP} byte cap",
                    messages_path.display()
                ),
            }));
        }
        drop_message_seqs(state, &order[start..next]);
        bytes = serialize(state)?;
    }
    log::warn!(
        target: "agent_bus",
        "dropped {} of {rows_before} rows from {} to bring it under its {MESSAGES_FILE_CAP} byte cap",
        rows_before - state.messages.len(),
        messages_path.display()
    );
    Ok(bytes)
}

/// Looks up one message by `msg_id` (used to hydrate `> 6 KiB` mailbox pointer envelopes).
pub fn get_message_by_id(
    repo_path: &str,
    msg_id: &str,
) -> Result<Option<AgentBusMessage>, CommandError> {
    let clean_id = sanitize_token("msg_id", msg_id, 64)?;
    let (_repo, lanes_dir) = lanes_dir_for_repo(repo_path)?;
    with_bus_lock(&lanes_dir, false, || {
        let state: MessagesState = read_messages_readonly(&lanes_dir.join(MESSAGES_FILE))?;
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
        let mut state: MessagesState = read_json_bounded(&messages_path, MESSAGES_WRITE_CEILING)?;
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
        let rows_before = state.messages.len();
        let bytes = compact_messages_for_write(&mut state, &messages_path, None)?;
        if updated > 0 || state.messages.len() != rows_before {
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
        let state: MessagesState = read_messages_readonly(&lanes_dir.join(MESSAGES_FILE))?;
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
                None => {
                    include_broadcasts
                        || (m.from_agent == STEWARD_SENDER
                            && m.kind == STEWARD_MESSAGE_KIND
                            && m.hop == Some(0))
                }
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
    require_agent_sender(&provider)?;
    require_agent_sender(&derived_peer_id)?;
    if let Some(ref r) = role {
        require_role_not_steward(r)?;
    }
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
    let (repo, lanes_dir) = lanes_dir_for_repo(repo_path)?;
    let repo_common = resolve_git_common_dir(&repo).ok();
    let stored: PeersState = with_bus_lock(&lanes_dir, false, || {
        read_json_bounded(&lanes_dir.join(PEERS_FILE), 4 * 1024 * 1024)
    })?;
    let worktrees = list_worktrees_lite(repo_path).unwrap_or_default();
    let mut wt_paths: Vec<String> = Vec::new();
    let mut wt_seen: BTreeSet<String> = BTreeSet::new();
    let mut wt_branch_by_path: HashMap<String, Option<String>> = HashMap::new();
    for wt in &worktrees {
        if wt_seen.insert(wt.path.clone()) {
            wt_paths.push(wt.path.clone());
        }
        wt_branch_by_path.insert(wt.path.clone(), wt.branch.clone());
    }
    // When `git worktree list` is shed under `Tier::Severe`, discover the primary
    // checkout and every registered linked worktree from `<common_dir>/worktrees/<entry>/gitdir`
    // and `<private_git_dir>/HEAD` with zero subprocess spawns so active sessions in
    // sibling linked worktrees are never invisible during critical pressure.
    if wt_paths.is_empty() {
        let fallback_checkouts = admit_worktree_pair(repo_path, repo_path)
            .map(|pair| pair.best_effort_checkouts())
            .unwrap_or_else(|_| vec![repo.clone()]);
        for checkout in fallback_checkouts {
            let path_str = checkout.to_string_lossy().into_owned();
            if wt_seen.insert(path_str.clone()) {
                wt_paths.push(path_str.clone());
            }
            wt_branch_by_path
                .entry(path_str)
                .or_insert_with(|| head_branch_from_metadata(&checkout));
        }
    }
    // Also retain any stored peer checkout that belongs to this same common Git dir.
    if let Some(ref common) = repo_common {
        for peer in &stored.peers {
            let candidate = Path::new(peer.worktree_path.trim());
            let Ok(canonical) = fs::canonicalize(candidate) else {
                continue;
            };
            if resolve_git_common_dir(&canonical).ok().as_ref() == Some(common) {
                let path_str = canonical.to_string_lossy().into_owned();
                if wt_seen.insert(path_str.clone()) {
                    wt_paths.push(path_str.clone());
                }
                wt_branch_by_path
                    .entry(path_str)
                    .or_insert_with(|| head_branch_from_metadata(&canonical));
            }
        }
    }
    if wt_paths.is_empty() {
        wt_paths.push(repo.to_string_lossy().into_owned());
    }

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

/// The `from_agent` of a message still on the bus that is exactly `target` (after trim), so a
/// reply can reach a sender that never registered as a peer. Never a prefix or case match:
/// a guessed identity would deliver to the wrong agent.
pub fn find_bus_sender(repo_path: &str, target: &str) -> Result<Option<String>, CommandError> {
    let clean = target.trim();
    if clean.is_empty() {
        return Ok(None);
    }
    let (_repo, lanes_dir) = lanes_dir_for_repo(repo_path)?;
    with_bus_lock(&lanes_dir, false, || {
        let state: MessagesState = read_messages_readonly(&lanes_dir.join(MESSAGES_FILE))?;
        Ok(state
            .messages
            .iter()
            .find(|m| m.from_agent == clean)
            .map(|m| m.from_agent.clone()))
    })
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
    origin: MessageOrigin,
    from_agent: &str,
    to_agent: Option<&str>,
    kind: &str,
    body: &str,
    branch: Option<&str>,
) -> Result<AgentBusMessage, CommandError> {
    post_thread_message(
        repo_path, origin, from_agent, to_agent, None, kind, body, branch,
    )
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
        let state: MessagesState = read_messages_readonly(&lanes_dir.join(MESSAGES_FILE))?;
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
    origin: MessageOrigin,
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
        origin,
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
    origin: MessageOrigin,
    from_agent: &str,
    thread_key: &str,
    answer_body: &str,
) -> Result<AgentBusMessage, CommandError> {
    let clean_thread_key = sanitize_token("thread_key", thread_key, 80)?;
    post_thread_message(
        repo_path,
        origin,
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
        let state: MessagesState = read_messages_readonly(&lanes_dir.join(MESSAGES_FILE))?;

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
            MessageOrigin::Agent,
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
            MessageOrigin::Agent,
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
            MessageOrigin::Agent,
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
            MessageOrigin::Agent,
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
        crate::test_support::stamp_current_resolver_epoch(&conn);

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
            MessageOrigin::Agent,
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
            MessageOrigin::Person,
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
            MessageOrigin::Agent,
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
            MessageOrigin::Agent,
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
            MessageOrigin::Agent,
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
            MessageOrigin::Agent,
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
            MessageOrigin::Agent,
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
            MessageOrigin::Agent,
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
            MessageOrigin::Agent,
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
    fn a_peer_may_describe_itself_as_a_developer_but_never_as_the_steward() {
        let anchor_dir = crate::codeintel::tests::repo_with_one_generation();
        let anchor = anchor_dir.path().to_str().unwrap();
        crate::test_support::trust_repo(anchor_dir.path());
        let peer = |role: &str| PeerUpsert {
            provider: "claude".into(),
            surface: "terminal".into(),
            provider_session_id: Some("0f0e0d0c-0b0a-4909-8807-060504030201".into()),
            worktree_path: anchor.into(),
            role: Some(role.into()),
            ..Default::default()
        };
        let registered = upsert_peer(anchor, peer("developer")).unwrap();
        assert_eq!(registered.role.as_deref(), Some("developer"));
        for reserved in ["system-steward", "Manvi-Steward:worker"] {
            assert!(
                upsert_peer(anchor, peer(reserved)).is_err(),
                "role {reserved} must be refused"
            );
        }
        // The sender identity itself still may not be a person's name.
        assert!(require_agent_sender("developer").is_err());
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
    fn origin_not_sender_name_decides_the_hop_and_who_may_use_a_person_name() {
        let anchor_dir = crate::codeintel::tests::repo_with_one_generation();
        let anchor = anchor_dir.path().to_str().unwrap();
        crate::test_support::trust_repo(anchor_dir.path());

        // The app UI posts as a person: hop 0, whatever the argument asks for.
        let person = post_peer_message(
            anchor,
            MessageOrigin::Person,
            "developer",
            Some("claude:peer"),
            None,
            "note",
            "from the app",
            None,
            None,
            5,
            6,
        )
        .unwrap();
        assert_eq!(person.hop, Some(0));

        // An agent never gets a person's name, in any case or spacing...
        for name in [
            "developer",
            "human",
            "human-dev",
            "user",
            " USER ",
            "Developer",
        ] {
            let refused = post_thread_message(
                anchor,
                MessageOrigin::Agent,
                name,
                Some("claude:peer"),
                None,
                "note",
                "forged",
                None,
            );
            assert!(refused.is_err(), "an agent must not post as {name:?}");
        }
        // GitPulse's own sender name is reserved too, in any case or spacing, because the
        // mailbox trusts it for the overflow notice.
        for name in ["gitpulse", " GitPulse "] {
            let refused = post_peer_message(
                anchor,
                MessageOrigin::Agent,
                name,
                Some("claude:peer"),
                None,
                "note",
                "forged",
                None,
                Some("bus-overflow-deadbeef"),
                1,
                6,
            );
            assert!(refused.is_err(), "an agent must not post as {name:?}");
        }
        assert!(require_agent_sender("gitpulse:worker").is_ok());
        // ...and its hop is at least 1 even when it names 0.
        let agent = post_peer_message(
            anchor,
            MessageOrigin::Agent,
            "claude:coder",
            Some("claude:peer"),
            None,
            "note",
            "from an agent",
            None,
            None,
            0,
            6,
        )
        .unwrap();
        assert_eq!(agent.hop, Some(1));
    }

    #[test]
    fn a_reused_msg_id_is_refused_when_recipient_kind_or_thread_differs_and_exact_retry_is_idempotent(
    ) {
        let anchor_dir = crate::codeintel::tests::repo_with_one_generation();
        let anchor = anchor_dir.path().to_str().unwrap();
        crate::test_support::trust_repo(anchor_dir.path());

        let send = |to: &str, thread: Option<&str>, kind: &str| {
            post_peer_message(
                anchor,
                MessageOrigin::Agent,
                "agy:lead",
                Some(to),
                thread,
                kind,
                "same text",
                None,
                Some("req_reuse_001"),
                1,
                6,
            )
        };
        let first = send("claude:peer-b", Some("t-1"), "note").unwrap();

        // The same id and body aimed at another recipient is refused, not reported as B's row.
        let other_recipient = send("claude:peer-c", Some("t-1"), "note")
            .expect_err("recipient")
            .into_message();
        assert!(
            other_recipient.contains("to_agent"),
            "refusal must name the recipient: {other_recipient}"
        );
        assert!(send("claude:peer-b", Some("t-1"), "ask")
            .expect_err("kind")
            .into_message()
            .contains("kind"));
        assert!(send("claude:peer-b", Some("t-2"), "note")
            .expect_err("thread")
            .into_message()
            .contains("thread_key"));
        assert!(send("claude:peer-b", None, "note")
            .expect_err("dropped thread")
            .into_message()
            .contains("thread_key"));

        // An exact retry still returns the original row, and nothing extra was stored.
        let retry = send("claude:peer-b", Some("t-1"), "note").unwrap();
        assert_eq!(retry.seq, first.seq);
        assert_eq!(read_messages(anchor, None, 0).unwrap().messages.len(), 1);
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
            MessageOrigin::Agent,
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
            MessageOrigin::Agent,
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
            MessageOrigin::Agent,
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
                    start_us: None,
                    is_caller: false,
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
                    start_us: None,
                    is_caller: false,
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
                    start_us: None,
                    is_caller: false,
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
                    start_us: None,
                    is_caller: false,
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

    #[test]
    fn reconcile_discovers_sibling_linked_worktree_sessions_under_severe_governor_without_git_spawns(
    ) {
        use crate::procguard::LockedSpawn;
        use crate::sysload::{inject_governor_with_history, GovernorInput, Level};

        let tmp = tempfile::tempdir().unwrap();
        let root = crate::engine::process::canonicalize_plain(tmp.path()).unwrap();
        let main_repo = root.join("main-repo");
        let sibling_wt = root.join("sibling-lane");
        fs::create_dir_all(&main_repo).unwrap();

        for args in [
            vec!["init", "-q", "-b", "main"],
            vec!["config", "user.name", "Bus Test"],
            vec!["config", "user.email", "bus@example.com"],
            vec!["commit", "-q", "--allow-empty", "-m", "init"],
        ] {
            assert!(std::process::Command::new("git")
                .args(&args)
                .current_dir(&main_repo)
                .status_locked()
                .unwrap()
                .success());
        }
        assert!(std::process::Command::new("git")
            .args([
                "worktree",
                "add",
                "-q",
                "-b",
                "feat/sibling",
                sibling_wt.to_str().unwrap(),
            ])
            .current_dir(&main_repo)
            .status_locked()
            .unwrap()
            .success());

        // Also append an `[include]` stanza to `.git/config` so `metadata_checkouts()`
        // returns `None`, exercising `best_effort_checkouts()`'s direct `<common>/worktrees/<entry>/gitdir`
        // + `HEAD` fallback!
        let cfg_path = main_repo.join(".git/config");
        let mut cfg_text = fs::read_to_string(&cfg_path).unwrap();
        cfg_text.push_str("\n[include]\n\tpath = nonexistent-optional.gitconfig\n");
        fs::write(&cfg_path, cfg_text).unwrap();

        crate::test_support::trust_repo(&main_repo);
        crate::test_support::trust_repo(&sibling_wt);

        // Inject a REAL Severe governor (`Level::Critical`) so `list_worktrees_lite` is shed!
        let _gov = inject_governor_with_history(std::iter::repeat_n(
            GovernorInput::test_pressure(Level::Critical),
            10,
        ));
        assert!(
            list_worktrees_lite(main_repo.to_str().unwrap()).is_err(),
            "Severe governor must shed git worktree list subprocesses"
        );

        let sibling_str = sibling_wt.to_string_lossy().into_owned();
        let obs = vec![crate::insights::live_sessions::KindObservation {
            kind: "codex".into(),
            ok: true,
            live: vec![crate::insights::LiveSession {
                kind: "codex".into(),
                pid: 99123,
                entrypoint: String::new(),
                status: "working".into(),
                cwd: sibling_str.clone(),
                tree: None,
                run_id: None,
                parent_pid: None,
                session_id: None,
                proc_start: None,
                registry_cwd: None,
                start_us: None,
                is_caller: false,
                ide_hosted: false,
                terminal_session_id: None,
            }],
            ..Default::default()
        }];

        let reconciled = list_reconciled_peers_from_observation(
            main_repo.to_str().unwrap(),
            &[],
            &obs,
            &|_, _| crate::workbench::process_birth::Liveness::Gone,
            &|_, _| false,
        )
        .expect("reconcile succeeds with zero git spawns under Tier::Severe");

        let found = reconciled
            .iter()
            .find(|p| p.record.pid == Some(99123))
            .unwrap_or_else(|| panic!("missing sibling worktree session in {reconciled:?}"));
        assert_eq!(found.liveness, "live");
        assert_eq!(found.record.worktree_path, sibling_str);
        assert_eq!(found.record.branch.as_deref(), Some("feat/sibling"));

        // Also verify `best_effort_registered_checkouts()` discovers both `main_repo`
        // and `sibling_wt` with zero git spawns under `Tier::Severe` (even with
        // `[include]` in `.git/config`), while excluding an unregistered hand-written
        // gitfile, AND `handle_agent_register` rejects reserved steward provider names
        // before touching the worktree gate!
        let forged_wt = tmp.path().join("forged-wt");
        fs::create_dir_all(&forged_wt).unwrap();
        let real_gitfile = fs::read_to_string(sibling_wt.join(".git")).unwrap();
        fs::write(forged_wt.join(".git"), real_gitfile).unwrap();
        crate::test_support::trust_repo(&forged_wt);
        let canon_forged = crate::engine::process::canonicalize_plain(&forged_wt).unwrap();
        let forged_pair = crate::engine::worktree::admit_worktree_pair(
            forged_wt.to_str().unwrap(),
            forged_wt.to_str().unwrap(),
        )
        .unwrap();
        let verified_checkouts = forged_pair.best_effort_registered_checkouts();
        assert!(
            verified_checkouts
                .iter()
                .any(|p| p.to_string_lossy() == sibling_str),
            "verified linked worktree must be discovered from <common>/worktrees/*/gitdir"
        );
        assert!(
            !verified_checkouts.contains(&canon_forged),
            "unregistered hand-written gitfile must NOT appear in best_effort_registered_checkouts"
        );

        let spoof_err = crate::mcp::parse::<crate::mcp::AgentRegisterArgs>(
            "gitpulse_agent_register",
            &serde_json::json!({
                "repo_path": sibling_str,
                "provider": "manvi-steward",
                "role": "worker",
            }),
        )
        .and_then(crate::mcp::a2a::handle_agent_register)
        .expect_err(
            "reserved steward provider must fail before guard_command even under Severe load",
        );
        assert!(
            spoof_err.contains("Manvi system steward"),
            "unexpected error: {spoof_err}"
        );
    }

    fn bus_row(seq: u64, to: Option<&str>, created_at_ms: u64, body: String) -> AgentBusMessage {
        AgentBusMessage {
            seq,
            from_agent: "agy:filler".to_string(),
            to_agent: to.map(str::to_string),
            kind: "note".to_string(),
            body,
            branch: None,
            created_at_ms,
            thread_key: None,
            msg_id: Some(format!("seed-{seq}")),
            read_by: Vec::new(),
            hop: Some(1),
            max_hops: Some(6),
        }
    }

    /// A trusted repo whose `messages.json` already holds `rows`, as an older build or a busy
    /// bus would have left it.
    fn repo_with_seeded_bus(rows: Vec<AgentBusMessage>) -> tempfile::TempDir {
        let dir = crate::codeintel::tests::repo_with_one_generation();
        crate::test_support::trust_repo(dir.path());
        let repo = dir.path().to_str().unwrap();
        let state = MessagesState {
            next_seq: rows.iter().map(|m| m.seq).max().unwrap_or(0),
            dropped_before_seq: 0,
            messages: rows,
        };
        fs::write(
            lanes_messages_path(repo, true).unwrap(),
            serde_json::to_vec_pretty(&state).unwrap(),
        )
        .unwrap();
        dir
    }

    /// Posts one broadcast so the write path runs its eviction, and returns what is stored.
    fn post_then_stored(dir: &tempfile::TempDir) -> MessagesState {
        let repo = dir.path().to_str().unwrap();
        post_thread_message(
            repo,
            MessageOrigin::Agent,
            "agy:lead",
            None,
            None,
            "note",
            "trigger",
            None,
        )
        .unwrap();
        read_messages_readonly(&lanes_messages_path(repo, false).unwrap()).unwrap()
    }

    fn stored_seqs(state: &MessagesState) -> BTreeSet<u64> {
        state.messages.iter().map(|m| m.seq).collect()
    }

    /// `count` broadcast rows numbered from `first_seq`; broadcasts are never pinned.
    fn broadcasts(first_seq: u64, count: u64, now: u64) -> Vec<AgentBusMessage> {
        (first_seq..first_seq + count)
            .map(|seq| bus_row(seq, None, now, "filler".to_string()))
            .collect()
    }

    #[test]
    fn a_chatty_recipient_keeps_only_its_newest_eight_unread_pinned_and_a_quiet_one_survives() {
        let now = now_epoch_ms();
        let mut rows = vec![bus_row(1, Some("claude:quiet"), now, "quiet".into())];
        rows.extend((2..=10).map(|seq| bus_row(seq, Some("claude:chatty"), now, "chatty".into())));
        rows.extend(broadcasts(11, 250, now));
        let state = post_then_stored(&repo_with_seeded_bus(rows));
        let kept = stored_seqs(&state);

        assert_eq!(state.messages.len(), MAX_BUS_MESSAGES);
        assert!(
            kept.contains(&1),
            "the quiet peer's only unread message must survive"
        );
        assert!(
            (3..=10).all(|seq| kept.contains(&seq)),
            "newest 8 of the chatty recipient are pinned"
        );
        assert!(
            !kept.contains(&2),
            "the ninth-newest unread is an ordinary row and goes first"
        );
    }

    #[test]
    fn at_most_sixty_four_unread_direct_messages_are_pinned_across_recipients() {
        let now = now_epoch_ms();
        // Nine recipients with eight unread messages each: 72 candidates for 64 pins.
        let mut rows: Vec<AgentBusMessage> = (1..=72u64)
            .map(|seq| {
                let to = format!("claude:peer-{}", (seq - 1) / 8);
                bus_row(seq, Some(&to), now, "unread".into())
            })
            .collect();
        rows.extend(broadcasts(73, 300, now));
        let kept = stored_seqs(&post_then_stored(&repo_with_seeded_bus(rows)));

        assert!(
            (9..=72).all(|seq| kept.contains(&seq)),
            "the newest 64 are pinned"
        );
        assert!(
            (1..=8).all(|seq| !kept.contains(&seq)),
            "the oldest beyond the cap are evicted"
        );
    }

    #[test]
    fn an_unread_direct_message_older_than_the_pending_ttl_is_not_pinned() {
        let now = now_epoch_ms();
        let stale =
            now - u64::try_from(crate::agent_mailbox::PENDING_TTL.as_millis()).unwrap() - 1_000;
        let mut rows = vec![
            bus_row(1, Some("claude:late"), stale, "stale".into()),
            bus_row(2, Some("claude:late"), now, "fresh".into()),
        ];
        rows.extend(broadcasts(3, 255, now));
        let kept = stored_seqs(&post_then_stored(&repo_with_seeded_bus(rows)));

        assert!(
            !kept.contains(&1),
            "an expired unread message is the oldest ordinary row"
        );
        assert!(kept.contains(&2), "a fresh unread message is pinned");
        assert!(
            !kept.contains(&3),
            "the next oldest ordinary row goes after it"
        );
        assert!(kept.contains(&4));
    }

    #[test]
    fn a_direct_message_somebody_has_read_is_not_pinned() {
        let now = now_epoch_ms();
        let mut read = bus_row(1, Some("claude:late"), now, "read".into());
        read.read_by.push("claude:late".to_string());
        let mut rows = vec![read, bus_row(2, Some("claude:late"), now, "unread".into())];
        rows.extend(broadcasts(3, 255, now));
        let kept = stored_seqs(&post_then_stored(&repo_with_seeded_bus(rows)));

        assert!(!kept.contains(&1), "a read message is an ordinary row");
        assert!(kept.contains(&2), "the unread one beside it is pinned");
        assert!(!kept.contains(&3));
    }

    #[test]
    fn size_healing_keeps_pinned_rows_while_unpinned_rows_can_pay_and_never_moves_next_seq() {
        let now = now_epoch_ms();
        // 256 rows of 40 KB is over the 8 MiB cap, as an older build could leave it.
        let big = || "x".repeat(40_000);
        let mut rows = vec![
            bus_row(1, Some("claude:quiet"), now, big()),
            bus_row(2, Some("claude:quiet"), now, big()),
        ];
        rows.extend((3..=256).map(|seq| bus_row(seq, None, now, big())));
        let dir = repo_with_seeded_bus(rows);
        let path = lanes_messages_path(dir.path().to_str().unwrap(), false).unwrap();
        assert!(fs::metadata(&path).unwrap().len() > MESSAGES_FILE_CAP);
        let state = post_then_stored(&dir);
        let kept = stored_seqs(&state);

        assert!(fs::metadata(&path).unwrap().len() <= MESSAGES_FILE_CAP);
        assert!(
            kept.contains(&1) && kept.contains(&2),
            "pinned rows outlast unpinned ones"
        );
        assert_eq!(state.next_seq, 257, "healing never moves next_seq");
        assert!(state.dropped_before_seq > 3);
        assert!(kept.contains(&257), "the new post is kept");
    }

    #[test]
    fn a_post_into_an_over_cap_file_of_pinned_rows_keeps_the_new_row_or_fails() {
        let now = now_epoch_ms();
        let big = || "x".repeat(72_000);
        // 64 unread directs and 64 open asks: every row is pinned and together they are over
        // the cap, so the only unpinned row is the broadcast being posted.
        let mut rows: Vec<AgentBusMessage> = (1..=64u64)
            .map(|seq| {
                let to = format!("claude:peer-{}", (seq - 1) / 8);
                bus_row(seq, Some(&to), now, big())
            })
            .collect();
        rows.extend((65..=128u64).map(|seq| {
            let mut ask = bus_row(seq, None, now, big());
            ask.kind = "ask".to_string();
            ask.thread_key = Some(format!("ask-{seq}"));
            ask
        }));
        let state = post_then_stored(&repo_with_seeded_bus(rows));

        assert!(
            state.messages.iter().any(|m| m.body == "trigger"),
            "the post returned Ok, so its row must be stored"
        );
        assert!(
            state.messages.iter().all(|m| m.seq > 1) || state.dropped_before_seq > 1,
            "the oldest pinned rows paid instead"
        );
    }

    #[test]
    fn a_state_that_cannot_hold_the_new_row_fails_the_post() {
        let now = now_epoch_ms();
        let mut state = MessagesState {
            next_seq: 1,
            dropped_before_seq: 0,
            messages: vec![bus_row(
                1,
                None,
                now,
                "x".repeat(usize::try_from(MESSAGES_FILE_CAP).unwrap()),
            )],
        };
        let err = compact_messages_for_write(&mut state, Path::new("messages.json"), Some(1))
            .expect_err("a row larger than the cap cannot be kept");
        assert!(err
            .into_message()
            .contains("even with every other message evicted"));
    }

    #[test]
    fn size_healing_evicts_every_unpinned_row_before_any_pinned_one() {
        let now = now_epoch_ms();
        let big = || "x".repeat(72_000);
        // 64 unread directs and 64 open asks, each large enough that together they alone
        // exceed the cap, then 128 tiny ordinary rows.
        let mut rows: Vec<AgentBusMessage> = (1..=64u64)
            .map(|seq| {
                let to = format!("claude:peer-{}", (seq - 1) / 8);
                bus_row(seq, Some(&to), now, big())
            })
            .collect();
        rows.extend((65..=128u64).map(|seq| {
            let mut ask = bus_row(seq, None, now, big());
            ask.kind = "ask".to_string();
            ask.thread_key = Some(format!("ask-{seq}"));
            ask
        }));
        rows.extend(broadcasts(129, 128, now));
        let dir = repo_with_seeded_bus(rows);
        let state = post_then_stored(&dir);
        let kept = stored_seqs(&state);

        assert!(
            (129..=256).all(|seq| !kept.contains(&seq)),
            "every ordinary row goes before a pinned one is touched"
        );
        let survivors: Vec<u64> = (1..=128).filter(|seq| kept.contains(seq)).collect();
        assert!(
            survivors.len() < 128,
            "pinned rows are the last resort and were needed"
        );
        let first = survivors[0];
        assert!(first > 1, "the oldest pinned row goes first");
        assert!(
            survivors.iter().copied().eq(first..=128),
            "pinned rows go oldest first"
        );
        assert_eq!(state.next_seq, 257);
    }
}
