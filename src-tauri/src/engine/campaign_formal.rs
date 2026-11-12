//! Formal state-machine model, runtime invariant verifier, and read-only audit
//! surface for multi-agent `GitPulse` worktree campaigns:
//! 1. Hierarchical single-writer file/directory lease disjointness,
//! 2. Append-only SHA-256 hash-chained `BULLETIN` monotonicity, and
//! 3. Sole-committer `Integrator` target-branch authority.

use crate::engine::git_cli::{git_captured, git_with_stdin, resolve_git_common_dir, validate_repo};
use crate::ipc_error::CommandError;
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

/// Maximum size of a persisted `campaign.json` state file (4 MiB).
const MAX_CAMPAIGN_STATE_BYTES: u64 = 4 * 1024 * 1024;

/// Byte length of `<git-common-dir>/gitpulse/campaign.key` (256-bit CSPRNG HMAC key).
pub const CAMPAIGN_KEY_LEN: usize = 32;

/// Git object-database state-anchor ref protecting against `campaign.json` rollbacks and wipes.
pub const CAMPAIGN_STATE_REF: &str = "refs/gitpulse/campaign/state";

/// Maximum ODB state-anchor chain hops walked during read-only audit verification.
const MAX_ODB_ANCHOR_CHAIN_HOPS: usize = 4096;

/// Maximum items returned per campaign audit collection over MCP/IPC.
pub const MAX_CAMPAIGN_AUDIT_ITEMS: usize = 256;

/// Default maximum `BULLETIN` entries returned by [`audit_repo_campaign`].
pub const DEFAULT_CAMPAIGN_BULLETIN_LIMIT: usize = 100;

/// Role assigned to an agent lane within a multi-worktree campaign.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub enum LaneRole {
    Worker,
    Integrator,
}

/// Access mode for a path or directory subtree lease.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub enum LeaseMode {
    Read,
    Write,
}

/// Canonical repository-relative scope held by a lease.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub enum LeaseScope {
    FileExact(String),
    DirSubtree(String),
}

/// Deterministic error taxonomy for campaign safety violations.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum CampaignViolation {
    InvalidRepoRelPath(String),
    UnknownLane(String),
    DuplicateLane(String),
    EmptyLeaseRequest,
    LeaseConflict {
        requester: String,
        holder: String,
        requested: LeaseScope,
        existing: LeaseScope,
    },
    BulletinSequenceMismatch {
        expected: u64,
        got: u64,
    },
    BulletinHashChainBroken {
        seq: u64,
    },
    BulletinHistoryTruncatedOrRewritten {
        index: usize,
    },
    WorkerDirectCommitForbidden {
        lane: String,
    },
    OutOfLeaseModification {
        lane: String,
        file: String,
    },
    StaleBaseRevision {
        expected_oid: String,
        got_oid: String,
    },
    UnverifiedIntegration {
        proposal_id: u64,
    },
    UnknownOrConsumedProposal {
        proposal_id: u64,
    },
    InvalidRevisionOid(String),
    TargetBranchCycle {
        new_oid: String,
    },
}

/// Validates a Git revision / commit OID string, rejecting empty strings,
/// leading/trailing or embedded whitespace, and ASCII control characters.
pub fn validate_revision_oid(raw: &str) -> Result<(), CampaignViolation> {
    if raw.is_empty()
        || raw
            .bytes()
            .any(|b| b <= 0x20 || b == 0x7f || b.is_ascii_whitespace())
    {
        return Err(CampaignViolation::InvalidRevisionOid(raw.to_string()));
    }
    Ok(())
}

/// Canonicalizes and validates a repository-relative path, rejecting empty
/// strings, absolute paths, `..` traversal components, `.` components, NUL /
/// control characters, and Windows backslashes.
pub fn normalize_repo_rel_path(raw: &str) -> Result<String, CampaignViolation> {
    let trimmed = raw.trim_end_matches('/');
    if raw.is_empty() || raw.starts_with('/') || raw.contains('\\') {
        return Err(CampaignViolation::InvalidRepoRelPath(raw.to_string()));
    }
    if raw.bytes().any(|b| b < 0x20 || b == 0x7f) {
        return Err(CampaignViolation::InvalidRepoRelPath(raw.to_string()));
    }
    if trimmed.is_empty() {
        return Err(CampaignViolation::InvalidRepoRelPath(raw.to_string()));
    }
    let mut parts = Vec::new();
    for seg in trimmed.split('/') {
        if seg.is_empty() || seg == "." || seg == ".." {
            return Err(CampaignViolation::InvalidRepoRelPath(raw.to_string()));
        }
        parts.push(seg);
    }
    Ok(parts.join("/"))
}

#[inline]
fn has_dir_prefix(path: &str, dir: &str) -> bool {
    let pb = path.as_bytes();
    let db = dir.as_bytes();
    pb.len() > db.len() && pb[db.len()] == b'/' && &pb[..db.len()] == db
}

impl LeaseScope {
    pub fn file(raw: &str) -> Result<Self, CampaignViolation> {
        Ok(Self::FileExact(normalize_repo_rel_path(raw)?))
    }

    pub fn dir(raw: &str) -> Result<Self, CampaignViolation> {
        Ok(Self::DirSubtree(normalize_repo_rel_path(raw)?))
    }

    pub fn covers_file(&self, canonical_file: &str) -> bool {
        match self {
            Self::FileExact(f) => f.as_bytes() == canonical_file.as_bytes(),
            Self::DirSubtree(d) => {
                canonical_file.as_bytes() == d.as_bytes() || has_dir_prefix(canonical_file, d)
            }
        }
    }
}

/// Hierarchical scope intersection predicate: catches exact file collisions,
/// file-inside-directory containment, and nested directory subtrees.
pub fn scopes_overlap(a: &LeaseScope, b: &LeaseScope) -> bool {
    match (a, b) {
        (LeaseScope::FileExact(fa), LeaseScope::FileExact(fb)) => fa.as_bytes() == fb.as_bytes(),
        (LeaseScope::FileExact(f), dir @ LeaseScope::DirSubtree(_))
        | (dir @ LeaseScope::DirSubtree(_), LeaseScope::FileExact(f)) => dir.covers_file(f),
        (LeaseScope::DirSubtree(da), LeaseScope::DirSubtree(db)) => {
            da.as_bytes() == db.as_bytes() || has_dir_prefix(da, db) || has_dir_prefix(db, da)
        }
    }
}

/// Two lease modes conflict whenever either side requests exclusive `Write`.
pub fn modes_conflict(m1: LeaseMode, m2: LeaseMode) -> bool {
    matches!((m1, m2), (LeaseMode::Write, _) | (_, LeaseMode::Write))
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LeaseGrant {
    pub lane: String,
    pub mode: LeaseMode,
    pub scope: LeaseScope,
}

/// Pure-Rust FIPS 180-4 SHA-256 implementation for zero-dependency cryptographic
/// `BULLETIN` hash-chaining.
pub fn sha256_digest(chunks: &[&[u8]]) -> [u8; 32] {
    const K: [u32; 64] = [
        0x428a2f98, 0x71374491, 0xb5c0fbcf, 0xe9b5dba5, 0x3956c25b, 0x59f111f1, 0x923f82a4,
        0xab1c5ed5, 0xd807aa98, 0x12835b01, 0x243185be, 0x550c7dc3, 0x72be5d74, 0x80deb1fe,
        0x9bdc06a7, 0xc19bf174, 0xe49b69c1, 0xefbe4786, 0x0fc19dc6, 0x240ca1cc, 0x2de92c6f,
        0x4a7484aa, 0x5cb0a9dc, 0x76f988da, 0x983e5152, 0xa831c66d, 0xb00327c8, 0xbf597fc7,
        0xc6e00bf3, 0xd5a79147, 0x06ca6351, 0x14292967, 0x27b70a85, 0x2e1b2138, 0x4d2c6dfc,
        0x53380d13, 0x650a7354, 0x766a0abb, 0x81c2c92e, 0x92722c85, 0xa2bfe8a1, 0xa81a664b,
        0xc24b8b70, 0xc76c51a3, 0xd192e819, 0xd6990624, 0xf40e3585, 0x106aa070, 0x19a4c116,
        0x1e376c08, 0x2748774c, 0x34b0bcb5, 0x391c0cb3, 0x4ed8aa4a, 0x5b9cca4f, 0x682e6ff3,
        0x748f82ee, 0x78a5636f, 0x84c87814, 0x8cc70208, 0x90befffa, 0xa4506ceb, 0xbef9a3f7,
        0xc67178f2,
    ];
    let mut state: [u32; 8] = [
        0x6a09e667, 0xbb67ae85, 0x3c6ef372, 0xa54ff53a, 0x510e527f, 0x9b05688c, 0x1f83d9ab,
        0x5be0cd19,
    ];
    let mut msg = Vec::new();
    for c in chunks {
        msg.extend_from_slice(c);
    }
    let bit_len = (msg.len() as u64) * 8;
    msg.push(0x80);
    while (msg.len() % 64) != 56 {
        msg.push(0x00);
    }
    msg.extend_from_slice(&bit_len.to_be_bytes());

    for block in msg.as_chunks::<64>().0 {
        let mut w = [0u32; 64];
        for (i, word) in block.as_chunks::<4>().0.iter().enumerate() {
            w[i] = u32::from_be_bytes(*word);
        }
        for i in 16..64 {
            let s0 = w[i - 15].rotate_right(7) ^ w[i - 15].rotate_right(18) ^ (w[i - 15] >> 3);
            let s1 = w[i - 2].rotate_right(17) ^ w[i - 2].rotate_right(19) ^ (w[i - 2] >> 10);
            w[i] = w[i - 16]
                .wrapping_add(s0)
                .wrapping_add(w[i - 7])
                .wrapping_add(s1);
        }
        let [mut a, mut b, mut c, mut d, mut e, mut f, mut g, mut h] = state;
        for i in 0..64 {
            let s1 = e.rotate_right(6) ^ e.rotate_right(11) ^ e.rotate_right(25);
            let ch = (e & f) ^ ((!e) & g);
            let temp1 = h
                .wrapping_add(s1)
                .wrapping_add(ch)
                .wrapping_add(K[i])
                .wrapping_add(w[i]);
            let s0 = a.rotate_right(2) ^ a.rotate_right(13) ^ a.rotate_right(22);
            let maj = (a & b) ^ (a & c) ^ (b & c);
            let temp2 = s0.wrapping_add(maj);

            h = g;
            g = f;
            f = e;
            e = d.wrapping_add(temp1);
            d = c;
            c = b;
            b = a;
            a = temp1.wrapping_add(temp2);
        }
        state[0] = state[0].wrapping_add(a);
        state[1] = state[1].wrapping_add(b);
        state[2] = state[2].wrapping_add(c);
        state[3] = state[3].wrapping_add(d);
        state[4] = state[4].wrapping_add(e);
        state[5] = state[5].wrapping_add(f);
        state[6] = state[6].wrapping_add(g);
        state[7] = state[7].wrapping_add(h);
    }

    let mut out = [0u8; 32];
    for (i, word) in state.iter().enumerate() {
        out[i * 4..(i + 1) * 4].copy_from_slice(&word.to_be_bytes());
    }
    out
}

/// Encodes a 32-byte SHA-256 digest as lowercase 64-char hex.
pub fn hex_sha256(digest: &[u8; 32]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut out = String::with_capacity(64);
    for &b in digest {
        out.push(HEX[(b >> 4) as usize] as char);
        out.push(HEX[(b & 0x0f) as usize] as char);
    }
    out
}

/// Decodes an exact 64-character ASCII hex string into a 32-byte digest/tag.
pub fn decode_hex_32(hex: &str) -> Result<[u8; 32], String> {
    if hex.len() != 64 {
        return Err(format!(
            "expected 64-char hex digest, got {} chars",
            hex.len()
        ));
    }
    let bytes = hex.as_bytes();
    let mut out = [0u8; 32];
    let nibble = |b: u8| -> Result<u8, String> {
        match b {
            b'0'..=b'9' => Ok(b - b'0'),
            b'a'..=b'f' => Ok(b - b'a' + 10),
            b'A'..=b'F' => Ok(b - b'A' + 10),
            _ => Err(format!("invalid hex byte 0x{b:02x}")),
        }
    };
    for i in 0..32 {
        let hi = nibble(bytes[2 * i])?;
        let lo = nibble(bytes[2 * i + 1])?;
        out[i] = (hi << 4) | lo;
    }
    Ok(out)
}

/// RFC 2104 / FIPS 198-1 HMAC-SHA256 implemented directly over [`sha256_digest`]
/// (64-byte SHA-256 block size, 32-byte MAC output).
pub fn hmac_sha256(key: &[u8], chunks: &[&[u8]]) -> [u8; 32] {
    let mut k0 = [0u8; 64];
    if key.len() > 64 {
        let hashed = sha256_digest(&[key]);
        k0[..32].copy_from_slice(&hashed);
    } else {
        k0[..key.len()].copy_from_slice(key);
    }
    let mut ipad = [0u8; 64];
    let mut opad = [0u8; 64];
    for i in 0..64 {
        ipad[i] = k0[i] ^ 0x36;
        opad[i] = k0[i] ^ 0x5c;
    }
    let mut inner_chunks = Vec::with_capacity(chunks.len() + 1);
    inner_chunks.push(ipad.as_slice());
    inner_chunks.extend_from_slice(chunks);
    let inner_digest = sha256_digest(&inner_chunks);
    sha256_digest(&[&opad, &inner_digest])
}

/// Constant-time equality over 32-byte HMAC-SHA256 authentication tags.
pub fn ct_eq_32(a: &[u8; 32], b: &[u8; 32]) -> bool {
    let mut acc = 0u8;
    for i in 0..32 {
        acc |= std::hint::black_box(a[i] ^ b[i]);
    }
    std::hint::black_box(acc) == 0
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BulletinEntry {
    pub seq: u64,
    pub author_lane: String,
    pub kind: String,
    pub body: String,
    pub prev_hash: [u8; 32],
    pub entry_hash: [u8; 32],
}

impl BulletinEntry {
    pub fn compute_hash(
        seq: u64,
        author_lane: &str,
        kind: &str,
        body: &str,
        prev_hash: &[u8; 32],
    ) -> [u8; 32] {
        let seq_bytes = seq.to_be_bytes();
        let author_len = (author_lane.len() as u32).to_be_bytes();
        let kind_len = (kind.len() as u32).to_be_bytes();
        let body_len = (body.len() as u32).to_be_bytes();
        sha256_digest(&[
            b"gitpulse-bulletin-v1\0",
            &seq_bytes,
            prev_hash,
            &author_len,
            author_lane.as_bytes(),
            &kind_len,
            kind.as_bytes(),
            &body_len,
            body.as_bytes(),
        ])
    }

    /// Computes a domain-separated HMAC-SHA256 authentication tag over all fields of `self`.
    pub fn compute_hmac(&self, key: &[u8; CAMPAIGN_KEY_LEN]) -> [u8; 32] {
        let seq_bytes = self.seq.to_be_bytes();
        let author_len = (self.author_lane.len() as u32).to_be_bytes();
        let kind_len = (self.kind.len() as u32).to_be_bytes();
        let body_len = (self.body.len() as u32).to_be_bytes();
        hmac_sha256(
            key,
            &[
                b"gitpulse-bulletin-hmac-v1\0",
                &seq_bytes,
                &self.prev_hash,
                &self.entry_hash,
                &author_len,
                self.author_lane.as_bytes(),
                &kind_len,
                self.kind.as_bytes(),
                &body_len,
                self.body.as_bytes(),
            ],
        )
    }
}

/// Append-only campaign `BULLETIN` log with hash-chain integrity and prefix
/// monotonicity verification.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct BulletinLog {
    entries: Vec<BulletinEntry>,
}

impl BulletinLog {
    pub fn entries(&self) -> &[BulletinEntry] {
        &self.entries
    }

    pub fn head_hash(&self) -> [u8; 32] {
        self.entries
            .last()
            .map(|e| e.entry_hash)
            .unwrap_or([0u8; 32])
    }

    pub fn append(&mut self, author_lane: &str, kind: &str, body: &str) -> BulletinEntry {
        let seq = (self.entries.len() as u64) + 1;
        let prev_hash = self.head_hash();
        let entry_hash = BulletinEntry::compute_hash(seq, author_lane, kind, body, &prev_hash);
        let entry = BulletinEntry {
            seq,
            author_lane: author_lane.to_string(),
            kind: kind.to_string(),
            body: body.to_string(),
            prev_hash,
            entry_hash,
        };
        self.entries.push(entry.clone());
        entry
    }

    /// Verifies internal hash-chain integrity from genesis (`seq = 1`, `prev_hash = [0; 32]`).
    pub fn verify_integrity(&self) -> Result<(), CampaignViolation> {
        let mut expected_prev = [0u8; 32];
        for (idx, entry) in self.entries.iter().enumerate() {
            let expected_seq = (idx as u64) + 1;
            if entry.seq != expected_seq {
                return Err(CampaignViolation::BulletinSequenceMismatch {
                    expected: expected_seq,
                    got: entry.seq,
                });
            }
            let recomputed = BulletinEntry::compute_hash(
                entry.seq,
                &entry.author_lane,
                &entry.kind,
                &entry.body,
                &expected_prev,
            );
            if entry.prev_hash != expected_prev || entry.entry_hash != recomputed {
                return Err(CampaignViolation::BulletinHashChainBroken { seq: entry.seq });
            }
            expected_prev = entry.entry_hash;
        }
        Ok(())
    }

    /// Proves append-only monotonicity (`older` is an exact prefix of `self`, and
    /// `self` has valid hash-chain integrity).
    pub fn verify_extension_of(&self, older: &BulletinLog) -> Result<(), CampaignViolation> {
        self.verify_integrity()?;
        if self.entries.len() < older.entries.len() {
            return Err(CampaignViolation::BulletinHistoryTruncatedOrRewritten {
                index: self.entries.len(),
            });
        }
        for (idx, prev_entry) in older.entries.iter().enumerate() {
            if &self.entries[idx] != prev_entry {
                return Err(CampaignViolation::BulletinHistoryTruncatedOrRewritten { index: idx });
            }
        }
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PatchProposal {
    pub id: u64,
    pub worker_lane: String,
    pub base_oid: String,
    pub touched_files: BTreeSet<String>,
    pub integrated: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TargetCommitRecord {
    pub proposal_id: u64,
    pub worker_lane: String,
    pub committer_lane: String,
    pub committer_role: LaneRole,
    pub parent_oid: String,
    pub new_oid: String,
    pub touched_files: BTreeSet<String>,
    pub verification_passed: bool,
}

impl TargetCommitRecord {
    /// Computes a domain-separated HMAC-SHA256 authentication tag over all fields of `self`.
    pub fn compute_hmac(&self, key: &[u8; CAMPAIGN_KEY_LEN]) -> [u8; 32] {
        let mut payload = Vec::with_capacity(128);
        payload.extend_from_slice(&self.proposal_id.to_be_bytes());
        payload.extend_from_slice(&(self.worker_lane.len() as u32).to_be_bytes());
        payload.extend_from_slice(self.worker_lane.as_bytes());
        payload.extend_from_slice(&(self.committer_lane.len() as u32).to_be_bytes());
        payload.extend_from_slice(self.committer_lane.as_bytes());
        payload.push(match self.committer_role {
            LaneRole::Worker => 0,
            LaneRole::Integrator => 1,
        });
        payload.extend_from_slice(&(self.parent_oid.len() as u32).to_be_bytes());
        payload.extend_from_slice(self.parent_oid.as_bytes());
        payload.extend_from_slice(&(self.new_oid.len() as u32).to_be_bytes());
        payload.extend_from_slice(self.new_oid.as_bytes());
        payload.push(u8::from(self.verification_passed));
        payload.extend_from_slice(&(self.touched_files.len() as u32).to_be_bytes());
        for f in &self.touched_files {
            payload.extend_from_slice(&(f.len() as u32).to_be_bytes());
            payload.extend_from_slice(f.as_bytes());
        }
        hmac_sha256(key, &[b"gitpulse-target-commit-hmac-v1\0", &payload])
    }
}

/// Formal campaign controller enforcing single-writer leases, append-only
/// `BULLETIN` coordination, and sole-committer `Integrator` target-branch merges.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CampaignController {
    genesis_oid: String,
    lanes: BTreeMap<String, LaneRole>,
    leases: Vec<LeaseGrant>,
    bulletin: BulletinLog,
    target_oid: String,
    seen_target_oids: BTreeSet<String>,
    next_proposal_id: u64,
    proposals: BTreeMap<u64, PatchProposal>,
    target_history: Vec<TargetCommitRecord>,
}

impl CampaignController {
    pub fn new(genesis_oid: impl Into<String>) -> Self {
        let g = genesis_oid.into();
        let mut seen = BTreeSet::new();
        if validate_revision_oid(&g).is_ok() {
            seen.insert(g.clone());
        }
        Self {
            genesis_oid: g.clone(),
            lanes: BTreeMap::new(),
            leases: Vec::new(),
            bulletin: BulletinLog::default(),
            target_oid: g,
            seen_target_oids: seen,
            next_proposal_id: 1,
            proposals: BTreeMap::new(),
            target_history: Vec::new(),
        }
    }

    pub fn try_new(genesis_oid: impl Into<String>) -> Result<Self, CampaignViolation> {
        let g = genesis_oid.into();
        validate_revision_oid(&g)?;
        Ok(Self::new(g))
    }

    pub fn genesis_oid(&self) -> &str {
        &self.genesis_oid
    }

    pub fn target_oid(&self) -> &str {
        &self.target_oid
    }

    pub fn lanes(&self) -> &BTreeMap<String, LaneRole> {
        &self.lanes
    }

    pub fn bulletin(&self) -> &BulletinLog {
        &self.bulletin
    }

    pub fn active_leases(&self) -> &[LeaseGrant] {
        &self.leases
    }

    pub fn proposals(&self) -> &BTreeMap<u64, PatchProposal> {
        &self.proposals
    }

    pub fn target_history(&self) -> &[TargetCommitRecord] {
        &self.target_history
    }

    pub fn register_lane(&mut self, lane: &str, role: LaneRole) -> Result<(), CampaignViolation> {
        if lane.trim().is_empty() {
            return Err(CampaignViolation::UnknownLane(lane.to_string()));
        }
        if self.lanes.contains_key(lane) {
            return Err(CampaignViolation::DuplicateLane(lane.to_string()));
        }
        self.lanes.insert(lane.to_string(), role);
        Ok(())
    }

    /// Atomically acquires a batch of leases for `lane`: either every requested
    /// scope is disjoint from conflicting foreign leases, or zero leases are added.
    pub fn acquire_leases(
        &mut self,
        lane: &str,
        mode: LeaseMode,
        scopes: Vec<LeaseScope>,
    ) -> Result<(), CampaignViolation> {
        if !self.lanes.contains_key(lane) {
            return Err(CampaignViolation::UnknownLane(lane.to_string()));
        }
        if scopes.is_empty() {
            return Err(CampaignViolation::EmptyLeaseRequest);
        }
        for req_scope in &scopes {
            for existing in &self.leases {
                if existing.lane == lane {
                    continue;
                }
                if modes_conflict(mode, existing.mode) && scopes_overlap(req_scope, &existing.scope)
                {
                    return Err(CampaignViolation::LeaseConflict {
                        requester: lane.to_string(),
                        holder: existing.lane.clone(),
                        requested: req_scope.clone(),
                        existing: existing.scope.clone(),
                    });
                }
            }
        }
        for scope in scopes {
            let grant = LeaseGrant {
                lane: lane.to_string(),
                mode,
                scope,
            };
            if !self.leases.contains(&grant) {
                self.leases.push(grant);
            }
        }
        Ok(())
    }

    pub fn release_lane_leases(&mut self, lane: &str) -> Result<usize, CampaignViolation> {
        if !self.lanes.contains_key(lane) {
            return Err(CampaignViolation::UnknownLane(lane.to_string()));
        }
        let before = self.leases.len();
        self.leases.retain(|g| g.lane != lane);
        Ok(before - self.leases.len())
    }

    pub fn post_bulletin(
        &mut self,
        author_lane: &str,
        kind: &str,
        body: &str,
    ) -> Result<BulletinEntry, CampaignViolation> {
        if !self.lanes.contains_key(author_lane) {
            return Err(CampaignViolation::UnknownLane(author_lane.to_string()));
        }
        Ok(self.bulletin.append(author_lane, kind, body))
    }

    fn lane_holds_write_for(&self, lane: &str, canonical_file: &str) -> bool {
        self.leases.iter().any(|g| {
            g.lane == lane && g.mode == LeaseMode::Write && g.scope.covers_file(canonical_file)
        })
    }

    /// Stages a patch proposal from a worker worktree. Every touched file must
    /// be covered by `worker_lane`'s active `Write` lease, and `base_oid` must
    /// match `target_oid`.
    pub fn stage_proposal(
        &mut self,
        worker_lane: &str,
        base_oid: &str,
        raw_files: &[&str],
    ) -> Result<u64, CampaignViolation> {
        if !self.lanes.contains_key(worker_lane) {
            return Err(CampaignViolation::UnknownLane(worker_lane.to_string()));
        }
        if raw_files.is_empty() {
            return Err(CampaignViolation::EmptyLeaseRequest);
        }
        validate_revision_oid(&self.target_oid)?;
        validate_revision_oid(base_oid)?;
        if base_oid != self.target_oid {
            return Err(CampaignViolation::StaleBaseRevision {
                expected_oid: self.target_oid.clone(),
                got_oid: base_oid.to_string(),
            });
        }
        let mut canonical_files = BTreeSet::new();
        for raw in raw_files {
            let norm = normalize_repo_rel_path(raw)?;
            if !self.lane_holds_write_for(worker_lane, &norm) {
                return Err(CampaignViolation::OutOfLeaseModification {
                    lane: worker_lane.to_string(),
                    file: norm,
                });
            }
            canonical_files.insert(norm);
        }
        let id = self.next_proposal_id;
        self.next_proposal_id += 1;
        self.proposals.insert(
            id,
            PatchProposal {
                id,
                worker_lane: worker_lane.to_string(),
                base_oid: base_oid.to_string(),
                touched_files: canonical_files,
                integrated: false,
            },
        );
        Ok(id)
    }

    /// Sole-committer integration gate: ONLY `LaneRole::Integrator` may advance
    /// the target branch, and ONLY when `verification_passed` holds, `expected_parent_oid`
    /// matches `target_oid`, and the worker still holds write leases over every
    /// touched path. Auto-releases the worker's leases on commit.
    pub fn commit_target_branch(
        &mut self,
        committer_lane: &str,
        proposal_id: u64,
        expected_parent_oid: &str,
        new_oid: &str,
        verification_passed: bool,
    ) -> Result<TargetCommitRecord, CampaignViolation> {
        let Some(&role) = self.lanes.get(committer_lane) else {
            return Err(CampaignViolation::UnknownLane(committer_lane.to_string()));
        };
        if role != LaneRole::Integrator {
            return Err(CampaignViolation::WorkerDirectCommitForbidden {
                lane: committer_lane.to_string(),
            });
        }
        if !verification_passed {
            return Err(CampaignViolation::UnverifiedIntegration { proposal_id });
        }
        validate_revision_oid(&self.target_oid)?;
        validate_revision_oid(expected_parent_oid)?;
        validate_revision_oid(new_oid)?;
        if expected_parent_oid != self.target_oid {
            return Err(CampaignViolation::StaleBaseRevision {
                expected_oid: self.target_oid.clone(),
                got_oid: expected_parent_oid.to_string(),
            });
        }
        if new_oid == self.target_oid || self.seen_target_oids.contains(new_oid) {
            return Err(CampaignViolation::TargetBranchCycle {
                new_oid: new_oid.to_string(),
            });
        }
        let Some(prop) = self.proposals.get(&proposal_id).cloned() else {
            return Err(CampaignViolation::UnknownOrConsumedProposal { proposal_id });
        };
        if prop.integrated {
            return Err(CampaignViolation::UnknownOrConsumedProposal { proposal_id });
        }
        if prop.base_oid != self.target_oid {
            return Err(CampaignViolation::StaleBaseRevision {
                expected_oid: self.target_oid.clone(),
                got_oid: prop.base_oid,
            });
        }
        for f in &prop.touched_files {
            if !self.lane_holds_write_for(&prop.worker_lane, f) {
                return Err(CampaignViolation::OutOfLeaseModification {
                    lane: prop.worker_lane.clone(),
                    file: f.clone(),
                });
            }
        }

        if let Some(stored) = self.proposals.get_mut(&proposal_id) {
            stored.integrated = true;
        }
        let rec = TargetCommitRecord {
            proposal_id,
            worker_lane: prop.worker_lane.clone(),
            committer_lane: committer_lane.to_string(),
            committer_role: role,
            parent_oid: self.target_oid.clone(),
            new_oid: new_oid.to_string(),
            touched_files: prop.touched_files,
            verification_passed: true,
        };
        self.target_oid = new_oid.to_string();
        self.seen_target_oids.insert(new_oid.to_string());
        self.leases.retain(|g| g.lane != prop.worker_lane);
        self.bulletin.append(
            committer_lane,
            "integrated",
            &format!("proposal={proposal_id} oid={new_oid}"),
        );
        self.target_history.push(rec.clone());
        Ok(rec)
    }

    /// Global campaign state invariant checker invoked at every step of bounded
    /// model checking.
    pub fn check_global_invariants(&self, genesis_oid: &str) -> Result<(), String> {
        // 1. Pairwise write-lease disjointness across distinct lanes.
        for i in 0..self.leases.len() {
            for j in (i + 1)..self.leases.len() {
                let a = &self.leases[i];
                let b = &self.leases[j];
                if a.lane != b.lane
                    && modes_conflict(a.mode, b.mode)
                    && scopes_overlap(&a.scope, &b.scope)
                {
                    return Err(format!("conflicting active leases: {a:?} vs {b:?}"));
                }
            }
        }
        // 2. Bulletin hash-chain integrity.
        self.bulletin
            .verify_integrity()
            .map_err(|e| format!("bulletin integrity broken: {e:?}"))?;

        // 3. Sole-committer Integrator authority & unbroken, acyclic parent_oid -> new_oid chain.
        validate_revision_oid(genesis_oid).map_err(|e| format!("invalid genesis_oid: {e:?}"))?;
        if self.genesis_oid != genesis_oid {
            return Err(format!(
                "controller genesis_oid {} mismatches expected {genesis_oid}",
                self.genesis_oid
            ));
        }
        let mut visited_oids = BTreeSet::new();
        visited_oids.insert(genesis_oid.to_string());
        let mut cur_oid = genesis_oid;
        for rec in &self.target_history {
            if rec.committer_role != LaneRole::Integrator {
                return Err(format!("non-integrator commit in history: {rec:?}"));
            }
            if !rec.verification_passed {
                return Err(format!("unverified commit in history: {rec:?}"));
            }
            validate_revision_oid(&rec.new_oid)
                .map_err(|e| format!("invalid commit new_oid in history: {e:?}"))?;
            if rec.parent_oid != cur_oid {
                return Err(format!(
                    "target branch lineage gap: expected parent {cur_oid}, got {}",
                    rec.parent_oid
                ));
            }
            if !visited_oids.insert(rec.new_oid.clone()) {
                return Err(format!(
                    "target branch ABA cycle detected in history: {} repeated",
                    rec.new_oid
                ));
            }
            cur_oid = &rec.new_oid;
        }
        if self.target_oid != cur_oid {
            return Err(format!(
                "target_oid {} diverged from history tip {}",
                self.target_oid, cur_oid
            ));
        }
        if self.seen_target_oids != visited_oids {
            return Err(format!(
                "seen_target_oids {:?} diverged from lineage {:?}",
                self.seen_target_oids, visited_oids
            ));
        }
        Ok(())
    }

    /// Loads a persisted `CampaignController` from `path`, enforcing `campaign.key`
    /// hygiene (`0600`, non-symlink, owner UID), domain-separated HMAC-SHA256
    /// authentication over the envelope + every `BulletinEntry` + every
    /// `TargetCommitRecord`, global safety invariants, and Git ODB state-anchor
    /// parity (`refs/gitpulse/campaign/state`) when `path` resides inside a Git repository.
    pub fn load_verified(path: &Path) -> Result<Self, String> {
        let (ctrl, _) = Self::load_verified_with_anchor(path)?;
        Ok(ctrl)
    }

    fn load_verified_with_anchor(path: &Path) -> Result<(Self, Option<String>), String> {
        verify_parent_dir_not_symlink(path)?;
        let bytes = read_regular_bounded_file(path, MAX_CAMPAIGN_STATE_BYTES)?;
        let key_path = campaign_key_path_for(path);
        let key = load_verified_campaign_key(&key_path)?;

        let envelope: AuthenticatedCampaignEnvelope =
            serde_json::from_slice(&bytes).map_err(|e| {
                format!(
                    "UnauthenticatedOrMalformedCampaignState: {} is missing authenticated HMAC envelope or malformed JSON ({e})",
                    path.display()
                )
            })?;
        if envelope.schema_version != 1 {
            return Err(format!(
                "UnsupportedCampaignEnvelopeVersion: {} has schema_version {}, expected 1",
                path.display(),
                envelope.schema_version
            ));
        }

        if let Err(hmac_err) = envelope.verify_hmac(&key) {
            if let Err(inv_err) = envelope
                .controller
                .check_global_invariants(&envelope.controller.genesis_oid)
            {
                return Err(format!(
                    "{hmac_err}; structural invariant failure: {inv_err}"
                ));
            }
            return Err(hmac_err);
        }

        envelope
            .controller
            .check_global_invariants(&envelope.controller.genesis_oid)?;

        let mut anchor_oid_out = None;
        if let Some((trust_root, common)) = enclosing_git_common_dir(path) {
            let Some(common_str) = common.to_str() else {
                return Err("Git common directory path is not valid UTF-8".to_string());
            };
            let state_sha256 = hex_sha256(&sha256_digest(&[&bytes]));
            let oid = verify_odb_state_anchor(
                &trust_root,
                &common,
                common_str,
                &key,
                &state_sha256,
                &envelope.envelope_hmac_sha256,
            )?;
            anchor_oid_out = Some(oid);
        }

        Ok((envelope.controller, anchor_oid_out))
    }

    /// Atomically persists `self` to `path` (`0600`), minting or validating a strict `0600`
    /// CSPRNG key at `<dir>/campaign.key`, sealing every `BulletinEntry`,
    /// `TargetCommitRecord`, outer envelope, and ODB state anchor with domain-separated
    /// HMAC-SHA256, and CAS-anchoring the state chain in Git's ODB at `refs/gitpulse/campaign/state`.
    pub fn save_verified_atomic(&self, path: &Path) -> Result<(), String> {
        self.check_global_invariants(&self.genesis_oid)?;
        let parent = ensure_campaign_parent_dir(path)?;
        let _ = parent;

        let git_ctx = enclosing_git_common_dir(path);
        let existing_anchor = if let Some((ref trust_root, ref common)) = git_ctx {
            let Some(common_str) = common.to_str() else {
                return Err("Git common directory path is not valid UTF-8".to_string());
            };
            read_odb_state_anchor(trust_root, common_str)?
        } else {
            None
        };

        let state_exists = match std::fs::symlink_metadata(path) {
            Ok(meta) => {
                if meta.file_type().is_symlink() || !meta.file_type().is_file() {
                    return Err(format!(
                        "refusing non-regular campaign state {}",
                        path.display()
                    ));
                }
                true
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => false,
            Err(e) => {
                return Err(format!(
                    "cannot stat campaign state {}: {e}",
                    path.display()
                ));
            }
        };

        if !state_exists && existing_anchor.is_some() {
            return Err(format!(
                "CampaignStateDeletedWhileOdbAnchorExists: refusing to create {} because {} already exists in Git ODB",
                path.display(),
                CAMPAIGN_STATE_REF
            ));
        }

        if state_exists {
            let older = Self::load_verified(path)?;
            if older.genesis_oid != self.genesis_oid {
                return Err(format!(
                    "refusing to overwrite campaign genesis {} with {}",
                    older.genesis_oid, self.genesis_oid
                ));
            }
            self.bulletin()
                .verify_extension_of(older.bulletin())
                .map_err(|e| format!("bulletin monotonicity violation on save: {e:?}"))?;
            if self.target_history.len() < older.target_history.len()
                || self.target_history[..older.target_history.len()] != older.target_history[..]
            {
                return Err(
                    "TargetHistoryTruncatedOrRewritten: target_history is not an append-only extension of persisted state"
                        .to_string(),
                );
            }
        }

        let key_path = campaign_key_path_for(path);
        let key_exists = match std::fs::symlink_metadata(&key_path) {
            Ok(_) => true,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => false,
            Err(e) => {
                return Err(format!(
                    "cannot stat campaign key {}: {e}",
                    key_path.display()
                ));
            }
        };

        let key = if key_exists {
            load_verified_campaign_key(&key_path)?
        } else if !state_exists && existing_anchor.is_none() {
            create_campaign_key_exclusive(&key_path)?
        } else {
            return Err(format!(
                "MissingCampaignKey: {} is absent while campaign state already exists",
                key_path.display()
            ));
        };

        let envelope = AuthenticatedCampaignEnvelope::seal(self.clone(), &key)?;
        let encoded = serde_json::to_vec_pretty(&envelope)
            .map_err(|e| format!("cannot serialize authenticated campaign state: {e}"))?;
        if (encoded.len() as u64) > MAX_CAMPAIGN_STATE_BYTES {
            return Err(format!(
                "serialized campaign state ({} bytes) exceeds {} byte cap",
                encoded.len(),
                MAX_CAMPAIGN_STATE_BYTES
            ));
        }

        if let Some((ref trust_root, ref common)) = git_ctx {
            let Some(common_str) = common.to_str() else {
                return Err("Git common directory path is not valid UTF-8".to_string());
            };
            let state_sha256 = hex_sha256(&sha256_digest(&[&encoded]));
            let (prev_oid, next_seq) = match existing_anchor {
                Some((ref oid, ref prev_anc)) => {
                    prev_anc.verify_hmac(&key)?;
                    let seq = prev_anc.chain_seq.checked_add(1).ok_or_else(|| {
                        "CampaignOdbAnchorSequenceOverflow: chain_seq overflow".to_string()
                    })?;
                    (Some(oid.clone()), seq)
                }
                None => (None, 1u64),
            };
            let anchor = CampaignOdbStateAnchor::seal(
                next_seq,
                prev_oid.clone(),
                state_sha256,
                envelope.envelope_hmac_sha256.clone(),
                self.genesis_oid.clone(),
                self.target_oid.clone(),
                self.bulletin.entries().len(),
                self.target_history.len(),
                &key,
            )?;
            write_and_cas_odb_state_anchor(trust_root, common_str, &anchor, prev_oid.as_deref())?;
        }

        write_private_atomic(path, &encoded)
    }
}

/// Persisted `campaign.json` envelope authenticating the controller payload and every
/// individual `BulletinEntry` and `TargetCommitRecord` via domain-separated HMAC-SHA256.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AuthenticatedCampaignEnvelope {
    pub schema_version: u32,
    pub controller: CampaignController,
    pub bulletin_entry_hmacs: Vec<String>,
    pub target_commit_hmacs: Vec<String>,
    pub envelope_hmac_sha256: String,
}

impl AuthenticatedCampaignEnvelope {
    pub fn seal(
        controller: CampaignController,
        key: &[u8; CAMPAIGN_KEY_LEN],
    ) -> Result<Self, String> {
        let bulletin_entry_hmacs: Vec<String> = controller
            .bulletin()
            .entries()
            .iter()
            .map(|e| hex_sha256(&e.compute_hmac(key)))
            .collect();
        let target_commit_hmacs: Vec<String> = controller
            .target_history()
            .iter()
            .map(|r| hex_sha256(&r.compute_hmac(key)))
            .collect();
        let canonical =
            serde_json::to_vec(&(&controller, &bulletin_entry_hmacs, &target_commit_hmacs))
                .map_err(|e| format!("cannot serialize campaign HMAC payload: {e}"))?;
        let env_tag = hmac_sha256(key, &[b"gitpulse-campaign-envelope-hmac-v1\0", &canonical]);
        Ok(Self {
            schema_version: 1,
            controller,
            bulletin_entry_hmacs,
            target_commit_hmacs,
            envelope_hmac_sha256: hex_sha256(&env_tag),
        })
    }

    pub fn verify_hmac(&self, key: &[u8; CAMPAIGN_KEY_LEN]) -> Result<(), String> {
        let canonical = serde_json::to_vec(&(
            &self.controller,
            &self.bulletin_entry_hmacs,
            &self.target_commit_hmacs,
        ))
        .map_err(|e| format!("cannot serialize campaign HMAC payload: {e}"))?;
        let expected_env = hmac_sha256(key, &[b"gitpulse-campaign-envelope-hmac-v1\0", &canonical]);
        let claimed_env = decode_hex_32(&self.envelope_hmac_sha256)
            .map_err(|e| format!("CampaignEnvelopeHmacMalformed: {e}"))?;
        if !ct_eq_32(&expected_env, &claimed_env) {
            return Err(
                "CampaignEnvelopeHmacMismatch: campaign.json failed constant-time HMAC-SHA256 verification against campaign.key"
                    .to_string(),
            );
        }

        let entries = self.controller.bulletin().entries();
        if self.bulletin_entry_hmacs.len() != entries.len() {
            return Err(format!(
                "BulletinHmacCountMismatch: {} HMAC tags for {} BULLETIN entries",
                self.bulletin_entry_hmacs.len(),
                entries.len()
            ));
        }
        for (idx, (entry, claimed_hex)) in entries
            .iter()
            .zip(self.bulletin_entry_hmacs.iter())
            .enumerate()
        {
            let claimed = decode_hex_32(claimed_hex)
                .map_err(|e| format!("BulletinHmacMalformed at index {idx}: {e}"))?;
            let expected = entry.compute_hmac(key);
            if !ct_eq_32(&expected, &claimed) {
                return Err(format!(
                    "BulletinHmacMismatch: BULLETIN entry seq={} failed constant-time HMAC-SHA256 verification",
                    entry.seq
                ));
            }
        }

        let commits = self.controller.target_history();
        if self.target_commit_hmacs.len() != commits.len() {
            return Err(format!(
                "TargetCommitHmacCountMismatch: {} HMAC tags for {} target commits",
                self.target_commit_hmacs.len(),
                commits.len()
            ));
        }
        for (idx, (rec, claimed_hex)) in commits
            .iter()
            .zip(self.target_commit_hmacs.iter())
            .enumerate()
        {
            let claimed = decode_hex_32(claimed_hex)
                .map_err(|e| format!("TargetCommitHmacMalformed at index {idx}: {e}"))?;
            let expected = rec.compute_hmac(key);
            if !ct_eq_32(&expected, &claimed) {
                return Err(format!(
                    "TargetCommitHmacMismatch: target commit proposal_id={} failed constant-time HMAC-SHA256 verification",
                    rec.proposal_id
                ));
            }
        }
        Ok(())
    }
}

/// Returns the sibling `<git-common-dir>/gitpulse/campaign.key` path for `state_path`.
pub fn campaign_key_path_for(state_path: &Path) -> PathBuf {
    state_path.with_file_name("campaign.key")
}

fn verify_parent_dir_not_symlink(path: &Path) -> Result<PathBuf, String> {
    let Some(parent) = path.parent() else {
        return Err(format!(
            "campaign path {} has no parent directory",
            path.display()
        ));
    };
    if let Ok(meta) = std::fs::symlink_metadata(parent) {
        if meta.file_type().is_symlink() || !meta.file_type().is_dir() {
            return Err(format!(
                "CampaignParentSymlinkOrNonDir: refusing non-directory or symlink parent {}",
                parent.display()
            ));
        }
    }
    Ok(parent.to_path_buf())
}

fn ensure_campaign_parent_dir(path: &Path) -> Result<PathBuf, String> {
    let parent = verify_parent_dir_not_symlink(path)?;
    if !parent.exists() {
        #[cfg(unix)]
        {
            use std::os::unix::fs::DirBuilderExt;
            let mut b = std::fs::DirBuilder::new();
            b.recursive(true).mode(0o700);
            b.create(&parent)
                .map_err(|e| format!("cannot create campaign dir {}: {e}", parent.display()))?;
        }
        #[cfg(not(unix))]
        {
            std::fs::create_dir_all(&parent)
                .map_err(|e| format!("cannot create campaign dir {}: {e}", parent.display()))?;
        }
    }
    verify_parent_dir_not_symlink(path)
}

#[cfg(unix)]
fn generate_csprng_key() -> Result<[u8; CAMPAIGN_KEY_LEN], String> {
    use std::io::Read;
    let mut key = [0u8; CAMPAIGN_KEY_LEN];
    let mut f = std::fs::File::open("/dev/urandom")
        .map_err(|e| format!("cannot open /dev/urandom for campaign.key CSPRNG: {e}"))?;
    f.read_exact(&mut key)
        .map_err(|e| format!("short read from /dev/urandom for campaign.key CSPRNG: {e}"))?;
    Ok(key)
}

#[cfg(windows)]
fn generate_csprng_key() -> Result<[u8; CAMPAIGN_KEY_LEN], String> {
    #[link(name = "bcrypt")]
    extern "system" {
        fn BCryptGenRandom(
            h_algorithm: *mut core::ffi::c_void,
            pb_buffer: *mut u8,
            cb_buffer: u32,
            dw_flags: u32,
        ) -> i32;
    }
    const BCRYPT_USE_SYSTEM_PREFERRED_RNG: u32 = 0x0000_0002;
    let mut key = [0u8; CAMPAIGN_KEY_LEN];
    // SAFETY: `key` is a valid mutable 32-byte stack buffer and null `h_algorithm`
    // is explicitly permitted when `BCRYPT_USE_SYSTEM_PREFERRED_RNG` is set.
    let status = unsafe {
        BCryptGenRandom(
            core::ptr::null_mut(),
            key.as_mut_ptr(),
            CAMPAIGN_KEY_LEN as u32,
            BCRYPT_USE_SYSTEM_PREFERRED_RNG,
        )
    };
    if status != 0 {
        return Err(format!(
            "BCryptGenRandom failed with NTSTATUS 0x{status:08x}"
        ));
    }
    Ok(key)
}

fn create_campaign_key_exclusive(key_path: &Path) -> Result<[u8; CAMPAIGN_KEY_LEN], String> {
    ensure_campaign_parent_dir(key_path)?;
    let key = generate_csprng_key()?;
    use std::io::Write;
    let mut opts = std::fs::OpenOptions::new();
    opts.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        opts.mode(0o600).custom_flags(libc::O_NOFOLLOW);
    }
    let mut f = opts.open(key_path).map_err(|e| {
        format!(
            "cannot exclusively create campaign key {}: {e}",
            key_path.display()
        )
    })?;
    f.write_all(&key)
        .map_err(|e| format!("cannot write campaign key {}: {e}", key_path.display()))?;
    f.sync_all()
        .map_err(|e| format!("cannot fsync campaign key {}: {e}", key_path.display()))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        let meta = f.metadata().map_err(|e| {
            format!(
                "cannot stat created campaign key {}: {e}",
                key_path.display()
            )
        })?;
        if meta.mode() & 0o077 != 0 {
            return Err(format!(
                "InsecureCampaignKeyPermissions: created {} with mode {:04o}; expected 0600",
                key_path.display(),
                meta.mode() & 0o777
            ));
        }
    }
    Ok(key)
}

fn load_verified_campaign_key(key_path: &Path) -> Result<[u8; CAMPAIGN_KEY_LEN], String> {
    verify_parent_dir_not_symlink(key_path)?;
    let sym_meta = std::fs::symlink_metadata(key_path).map_err(|e| {
        if e.kind() == std::io::ErrorKind::NotFound {
            format!(
                "MissingCampaignKey: required 32-byte HMAC key {} does not exist",
                key_path.display()
            )
        } else {
            format!("cannot stat campaign key {}: {e}", key_path.display())
        }
    })?;
    if sym_meta.file_type().is_symlink() || !sym_meta.file_type().is_file() {
        return Err(format!(
            "CampaignKeySymlinkOrNonRegular: refusing symlink or non-regular campaign key {}",
            key_path.display()
        ));
    }

    let mut opts = std::fs::OpenOptions::new();
    opts.read(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        opts.custom_flags(libc::O_NOFOLLOW);
    }
    let mut f = opts.open(key_path).map_err(|e| {
        format!(
            "CampaignKeyOpenFailed: cannot open {}: {e}",
            key_path.display()
        )
    })?;
    let fd_meta = f
        .metadata()
        .map_err(|e| format!("cannot stat open campaign key {}: {e}", key_path.display()))?;
    if !fd_meta.file_type().is_file() {
        return Err(format!(
            "CampaignKeySymlinkOrNonRegular: open descriptor for {} is not a regular file",
            key_path.display()
        ));
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        if (sym_meta.dev(), sym_meta.ino()) != (fd_meta.dev(), fd_meta.ino()) {
            return Err(format!(
                "CampaignKeySwappedDuringOpen: inode changed while opening {}",
                key_path.display()
            ));
        }
        if fd_meta.mode() & 0o077 != 0 {
            return Err(format!(
                "InsecureCampaignKeyPermissions: {} has mode {:04o}; group/other bits must be zero (0600)",
                key_path.display(),
                fd_meta.mode() & 0o777
            ));
        }
        // SAFETY: `geteuid` reads this process's effective UID and has no preconditions.
        let euid = unsafe { libc::geteuid() };
        if fd_meta.uid() != euid {
            return Err(format!(
                "ForeignCampaignKeyOwner: {} is owned by uid {}, expected process euid {}",
                key_path.display(),
                fd_meta.uid(),
                euid
            ));
        }
    }
    if fd_meta.len() != CAMPAIGN_KEY_LEN as u64 {
        return Err(format!(
            "InvalidCampaignKeyLength: {} has {} bytes, expected {}",
            key_path.display(),
            fd_meta.len(),
            CAMPAIGN_KEY_LEN
        ));
    }
    use std::io::Read;
    let mut key = [0u8; CAMPAIGN_KEY_LEN];
    f.read_exact(&mut key).map_err(|e| {
        format!(
            "cannot read 32-byte campaign key {}: {e}",
            key_path.display()
        )
    })?;
    Ok(key)
}

fn read_regular_bounded_file(path: &Path, max_bytes: u64) -> Result<Vec<u8>, String> {
    let sym_meta = std::fs::symlink_metadata(path)
        .map_err(|e| format!("cannot stat campaign state {}: {e}", path.display()))?;
    if sym_meta.file_type().is_symlink() || !sym_meta.file_type().is_file() {
        return Err(format!(
            "CampaignStateSymlinkOrNonRegular: refusing non-regular campaign state {}",
            path.display()
        ));
    }
    if sym_meta.len() > max_bytes {
        return Err(format!(
            "campaign state {} exceeds {} byte cap",
            path.display(),
            max_bytes
        ));
    }
    let mut opts = std::fs::OpenOptions::new();
    opts.read(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        opts.custom_flags(libc::O_NOFOLLOW);
    }
    let f = opts
        .open(path)
        .map_err(|e| format!("cannot open campaign state {}: {e}", path.display()))?;
    let fd_meta = f
        .metadata()
        .map_err(|e| format!("cannot stat open campaign state {}: {e}", path.display()))?;
    if !fd_meta.file_type().is_file() {
        return Err(format!(
            "CampaignStateSymlinkOrNonRegular: open descriptor for {} is not a regular file",
            path.display()
        ));
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        if (sym_meta.dev(), sym_meta.ino()) != (fd_meta.dev(), fd_meta.ino()) {
            return Err(format!(
                "CampaignStateSwappedDuringOpen: inode changed while opening {}",
                path.display()
            ));
        }
        // SAFETY: `geteuid` reads this process's effective UID and has no preconditions.
        let euid = unsafe { libc::geteuid() };
        if fd_meta.uid() != euid {
            return Err(format!(
                "ForeignCampaignStateOwner: {} is owned by uid {}, expected process euid {}",
                path.display(),
                fd_meta.uid(),
                euid
            ));
        }
    }
    use std::io::Read;
    let mut bytes = Vec::with_capacity(fd_meta.len().min(max_bytes) as usize);
    f.take(max_bytes + 1)
        .read_to_end(&mut bytes)
        .map_err(|e| format!("cannot read campaign state {}: {e}", path.display()))?;
    if (bytes.len() as u64) > max_bytes {
        return Err(format!(
            "campaign state {} exceeds {} byte cap",
            path.display(),
            max_bytes
        ));
    }
    Ok(bytes)
}

fn write_private_atomic(path: &Path, bytes: &[u8]) -> Result<(), String> {
    static TMP_SEQ: AtomicU64 = AtomicU64::new(0);
    let parent = ensure_campaign_parent_dir(path)?;
    let stem = path
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("campaign.json");
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    let seq = TMP_SEQ.fetch_add(1, Ordering::Relaxed);
    let tmp_path = parent.join(format!(".{stem}.tmp.{}.{nanos}.{seq}", std::process::id()));

    use std::io::Write;
    let mut opts = std::fs::OpenOptions::new();
    opts.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        opts.mode(0o600).custom_flags(libc::O_NOFOLLOW);
    }
    let write_res = (|| -> Result<(), String> {
        let mut f = opts.open(&tmp_path).map_err(|e| {
            format!(
                "cannot create temp campaign state {}: {e}",
                tmp_path.display()
            )
        })?;
        f.write_all(bytes).map_err(|e| {
            format!(
                "cannot write temp campaign state {}: {e}",
                tmp_path.display()
            )
        })?;
        f.sync_all().map_err(|e| {
            format!(
                "cannot fsync temp campaign state {}: {e}",
                tmp_path.display()
            )
        })?;
        Ok(())
    })();
    if let Err(err) = write_res {
        let _ = std::fs::remove_file(&tmp_path);
        return Err(err);
    }
    if let Err(e) = std::fs::rename(&tmp_path, path) {
        let _ = std::fs::remove_file(&tmp_path);
        return Err(format!(
            "cannot atomically rename {} -> {}: {e}",
            tmp_path.display(),
            path.display()
        ));
    }
    #[cfg(unix)]
    {
        if let Ok(dir_fd) = std::fs::File::open(&parent) {
            let _ = dir_fd.sync_all();
        }
    }
    Ok(())
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
struct CampaignOdbStateAnchor {
    schema_version: u32,
    chain_seq: u64,
    prev_anchor_oid: Option<String>,
    state_sha256: String,
    envelope_hmac_sha256: String,
    genesis_oid: String,
    target_oid: String,
    bulletin_len: usize,
    target_commits_len: usize,
    anchor_hmac_sha256: String,
}

impl CampaignOdbStateAnchor {
    #[allow(clippy::too_many_arguments)]
    fn seal(
        chain_seq: u64,
        prev_anchor_oid: Option<String>,
        state_sha256: String,
        envelope_hmac_sha256: String,
        genesis_oid: String,
        target_oid: String,
        bulletin_len: usize,
        target_commits_len: usize,
        key: &[u8; CAMPAIGN_KEY_LEN],
    ) -> Result<Self, String> {
        let canonical = serde_json::to_vec(&(
            1u32,
            chain_seq,
            &prev_anchor_oid,
            &state_sha256,
            &envelope_hmac_sha256,
            &genesis_oid,
            &target_oid,
            bulletin_len,
            target_commits_len,
        ))
        .map_err(|e| format!("cannot serialize campaign ODB anchor HMAC payload: {e}"))?;
        let tag = hmac_sha256(
            key,
            &[b"gitpulse-campaign-odb-anchor-hmac-v1\0", &canonical],
        );
        Ok(Self {
            schema_version: 1,
            chain_seq,
            prev_anchor_oid,
            state_sha256,
            envelope_hmac_sha256,
            genesis_oid,
            target_oid,
            bulletin_len,
            target_commits_len,
            anchor_hmac_sha256: hex_sha256(&tag),
        })
    }

    fn verify_hmac(&self, key: &[u8; CAMPAIGN_KEY_LEN]) -> Result<(), String> {
        let canonical = serde_json::to_vec(&(
            self.schema_version,
            self.chain_seq,
            &self.prev_anchor_oid,
            &self.state_sha256,
            &self.envelope_hmac_sha256,
            &self.genesis_oid,
            &self.target_oid,
            self.bulletin_len,
            self.target_commits_len,
        ))
        .map_err(|e| format!("cannot serialize campaign ODB anchor HMAC payload: {e}"))?;
        let expected = hmac_sha256(
            key,
            &[b"gitpulse-campaign-odb-anchor-hmac-v1\0", &canonical],
        );
        let claimed = decode_hex_32(&self.anchor_hmac_sha256)
            .map_err(|e| format!("CampaignOdbAnchorHmacMalformed: {e}"))?;
        if !ct_eq_32(&expected, &claimed) {
            return Err(
                "CampaignOdbAnchorHmacMismatch: refs/gitpulse/campaign/state anchor failed constant-time HMAC-SHA256 verification against campaign.key"
                    .to_string(),
            );
        }
        Ok(())
    }
}

fn enclosing_git_common_dir(state_path: &Path) -> Option<(PathBuf, PathBuf)> {
    let git_dir = state_path.parent()?.parent()?;
    if !git_dir.join("HEAD").is_file() || !git_dir.join("objects").is_dir() {
        return None;
    }
    let trust_root = if git_dir.file_name().and_then(|s| s.to_str()) == Some(".git") {
        git_dir.parent().unwrap_or(git_dir).to_path_buf()
    } else {
        git_dir.to_path_buf()
    };
    Some((trust_root, git_dir.to_path_buf()))
}

fn read_odb_anchor_blob(
    trust_root: &Path,
    common_str: &str,
    blob_oid: &str,
) -> Result<CampaignOdbStateAnchor, String> {
    if !is_git_hex_oid(blob_oid) {
        return Err(format!(
            "CorruptCampaignOdbStateAnchor: invalid anchor blob OID {blob_oid:?}"
        ));
    }
    let cat = git_captured(
        trust_root,
        &[
            "--no-optional-locks",
            "--git-dir",
            common_str,
            "cat-file",
            "blob",
            blob_oid,
        ],
    )
    .map_err(|e| {
        format!(
            "cannot read Git ODB campaign anchor {blob_oid}: {}",
            e.into_message()
        )
    })?;
    if !cat.success {
        return Err(format!(
            "DanglingCampaignOdbStateAnchor: anchor blob {blob_oid} missing from Git ODB"
        ));
    }
    let anchor: CampaignOdbStateAnchor = serde_json::from_slice(&cat.stdout).map_err(|e| {
        format!("CorruptCampaignOdbStateAnchor: malformed anchor blob {blob_oid} ({e})")
    })?;
    if anchor.schema_version != 1 {
        return Err(format!(
            "UnsupportedCampaignOdbStateAnchorVersion: {} in {blob_oid}",
            anchor.schema_version
        ));
    }
    Ok(anchor)
}

fn read_odb_state_anchor(
    trust_root: &Path,
    common_str: &str,
) -> Result<Option<(String, CampaignOdbStateAnchor)>, String> {
    let rev = git_captured(
        trust_root,
        &[
            "--no-optional-locks",
            "--git-dir",
            common_str,
            "rev-parse",
            "--verify",
            "--quiet",
            CAMPAIGN_STATE_REF,
        ],
    )
    .map_err(|e| {
        format!(
            "cannot query Git ODB ref {CAMPAIGN_STATE_REF}: {}",
            e.into_message()
        )
    })?;
    if !rev.success {
        return Ok(None);
    }
    let ref_oid = String::from_utf8_lossy(&rev.stdout).trim().to_string();
    let anchor = read_odb_anchor_blob(trust_root, common_str, &ref_oid)?;
    Ok(Some((ref_oid, anchor)))
}

fn verify_odb_state_anchor(
    trust_root: &Path,
    common: &Path,
    common_str: &str,
    key: &[u8; CAMPAIGN_KEY_LEN],
    expected_state_sha256: &str,
    expected_envelope_hmac: &str,
) -> Result<String, String> {
    let Some((tip_oid, tip_anchor)) = read_odb_state_anchor(trust_root, common_str)? else {
        return Err(format!(
            "MissingCampaignOdbStateAnchor: campaign.json exists on disk without {CAMPAIGN_STATE_REF} in Git ODB"
        ));
    };
    tip_anchor.verify_hmac(key)?;
    let tip_sha = decode_hex_32(&tip_anchor.state_sha256)
        .map_err(|e| format!("CorruptCampaignOdbStateAnchor: {e}"))?;
    let want_sha = decode_hex_32(expected_state_sha256)
        .map_err(|e| format!("CorruptCampaignStateDigest: {e}"))?;
    let tip_hmac = decode_hex_32(&tip_anchor.envelope_hmac_sha256)
        .map_err(|e| format!("CorruptCampaignOdbStateAnchor: {e}"))?;
    let want_hmac = decode_hex_32(expected_envelope_hmac)
        .map_err(|e| format!("CorruptCampaignEnvelopeHmac: {e}"))?;
    if !ct_eq_32(&tip_sha, &want_sha) || !ct_eq_32(&tip_hmac, &want_hmac) {
        return Err(format!(
            "CampaignStateRollbackOrOdbMismatch: campaign.json (sha256={expected_state_sha256}) does not match {CAMPAIGN_STATE_REF} anchor {tip_oid} (expected sha256={})",
            tip_anchor.state_sha256
        ));
    }

    // Verify bounded, acyclic, HMAC-authenticated ODB blob-chain ancestry back to genesis (`chain_seq == 1`, `prev_anchor_oid: None`).
    let mut visited = BTreeSet::new();
    visited.insert(tip_oid.clone());
    let mut cur_seq = tip_anchor.chain_seq;
    let mut cur_bulletin_len = tip_anchor.bulletin_len;
    let mut cur_commits_len = tip_anchor.target_commits_len;
    let mut cur_prev = tip_anchor.prev_anchor_oid.clone();
    let mut hops = 0usize;
    while let Some(prev_oid) = cur_prev {
        hops += 1;
        if hops > MAX_ODB_ANCHOR_CHAIN_HOPS {
            return Err(format!(
                "CampaignOdbAnchorChainTooDeep: exceeded {MAX_ODB_ANCHOR_CHAIN_HOPS} hops"
            ));
        }
        if !visited.insert(prev_oid.clone()) {
            return Err(format!(
                "CyclicCampaignOdbAnchorChain: cycle detected at {prev_oid}"
            ));
        }
        let prev_anchor = read_odb_anchor_blob(trust_root, common_str, &prev_oid)?;
        prev_anchor.verify_hmac(key)?;
        if prev_anchor.genesis_oid != tip_anchor.genesis_oid {
            return Err(format!(
                "CampaignOdbAnchorGenesisMismatch: anchor {prev_oid} genesis {} != tip genesis {}",
                prev_anchor.genesis_oid, tip_anchor.genesis_oid
            ));
        }
        if prev_anchor.chain_seq.checked_add(1) != Some(cur_seq) {
            return Err(format!(
                "CampaignOdbAnchorSequenceBroken: expected prev chain_seq + 1 == {cur_seq}, got {} at {prev_oid}",
                prev_anchor.chain_seq
            ));
        }
        if cur_bulletin_len < prev_anchor.bulletin_len
            || cur_commits_len < prev_anchor.target_commits_len
        {
            return Err(format!(
                "CampaignOdbAnchorHistoryRegressed: anchor {prev_oid} had (bulletin={}, commits={}) > child ({cur_bulletin_len}, {cur_commits_len})",
                prev_anchor.bulletin_len, prev_anchor.target_commits_len
            ));
        }
        cur_seq = prev_anchor.chain_seq;
        cur_bulletin_len = prev_anchor.bulletin_len;
        cur_commits_len = prev_anchor.target_commits_len;
        cur_prev = prev_anchor.prev_anchor_oid;
    }
    if cur_seq != 1 {
        return Err(format!(
            "CampaignOdbAnchorGenesisSequenceInvalid: root anchor chain_seq={}, expected 1",
            cur_seq
        ));
    }

    // Verify `<git-common-dir>/logs/refs/gitpulse/campaign/state` reflog continuity so
    // `git update-ref refs/gitpulse/campaign/state <older_oid>` + `campaign.json` replay is convicted!
    let reflog_path = common.join("logs").join(CAMPAIGN_STATE_REF);
    let reflog_bytes =
        read_regular_bounded_file(&reflog_path, MAX_CAMPAIGN_STATE_BYTES).map_err(|e| {
            format!(
                "MissingOrInvalidCampaignOdbReflog: required reflog {} unreadable ({e})",
                reflog_path.display()
            )
        })?;
    let reflog_text = String::from_utf8_lossy(&reflog_bytes);
    let mut last_new_oid: Option<&str> = None;
    let mut reflog_candidates: BTreeSet<String> = BTreeSet::new();
    for line in reflog_text.lines() {
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }
        let header = trimmed.split('\t').next().unwrap_or(trimmed);
        let mut cols = header.split_whitespace();
        let Some(old_col) = cols.next() else {
            continue;
        };
        let Some(new_col) = cols.next() else {
            continue;
        };
        last_new_oid = Some(new_col);
        for cand in [old_col, new_col] {
            if is_git_hex_oid(cand)
                && !cand.bytes().all(|b| b == b'0')
                && !visited.contains(cand)
                && reflog_candidates.len() < MAX_ODB_ANCHOR_CHAIN_HOPS
            {
                reflog_candidates.insert(cand.to_string());
            }
        }
    }
    if last_new_oid != Some(tip_oid.as_str()) {
        return Err(format!(
            "CampaignStateReflogTipMismatch: {CAMPAIGN_STATE_REF} ({tip_oid}) diverges from reflog tip {:?}",
            last_new_oid
        ));
    }
    for cand_oid in reflog_candidates {
        if let Ok(cand_anchor) = read_odb_anchor_blob(trust_root, common_str, &cand_oid) {
            if cand_anchor.verify_hmac(key).is_ok()
                && cand_anchor.genesis_oid == tip_anchor.genesis_oid
                && (cand_anchor.chain_seq > tip_anchor.chain_seq
                    || (cand_anchor.chain_seq == tip_anchor.chain_seq && cand_oid != tip_oid))
            {
                return Err(format!(
                    "CampaignStateRollbackDetected: {CAMPAIGN_STATE_REF} ({tip_oid}, chain_seq={}) rolled back or forked from previously anchored state {cand_oid} (chain_seq={})",
                    tip_anchor.chain_seq, cand_anchor.chain_seq
                ));
            }
        }
    }

    Ok(tip_oid)
}

fn write_and_cas_odb_state_anchor(
    trust_root: &Path,
    common_str: &str,
    anchor: &CampaignOdbStateAnchor,
    expected_old_oid: Option<&str>,
) -> Result<String, String> {
    let anchor_bytes = serde_json::to_vec_pretty(anchor)
        .map_err(|e| format!("cannot serialize campaign ODB anchor: {e}"))?;
    let raw_oid = git_with_stdin(
        trust_root,
        &["--git-dir", common_str, "hash-object", "-w", "--stdin"],
        &anchor_bytes,
    )
    .map_err(|e| {
        format!(
            "git hash-object -w failed for campaign anchor: {}",
            e.into_message()
        )
    })?;
    let new_oid = String::from_utf8_lossy(&raw_oid).trim().to_string();
    if !is_git_hex_oid(&new_oid) {
        return Err(format!(
            "git hash-object -w returned invalid OID {new_oid:?}"
        ));
    }
    let zero_oid;
    let cas_old = match expected_old_oid {
        Some(old) => old,
        None => {
            zero_oid = "0".repeat(new_oid.len());
            &zero_oid
        }
    };
    let upd = git_captured(
        trust_root,
        &[
            "--git-dir",
            common_str,
            "update-ref",
            "--create-reflog",
            CAMPAIGN_STATE_REF,
            &new_oid,
            cas_old,
        ],
    )
    .map_err(|e| {
        format!(
            "CAS git update-ref {CAMPAIGN_STATE_REF} failed: {}",
            e.into_message()
        )
    })?;
    if !upd.success {
        return Err(format!(
            "CampaignOdbCasConflict: git update-ref {CAMPAIGN_STATE_REF} -> {new_oid} (expected {cas_old}) failed: {}",
            String::from_utf8_lossy(&upd.stderr).trim()
        ));
    }
    Ok(new_oid)
}

/// Registered lane record in a [`CampaignAuditReport`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CampaignLaneAudit {
    pub lane: String,
    pub role: LaneRole,
}

/// Active single-writer or shared-reader lease record in a [`CampaignAuditReport`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CampaignLeaseAudit {
    pub lane: String,
    pub mode: LeaseMode,
    pub scope_kind: String,
    pub path: String,
}

/// Verified hash-chained `BULLETIN` entry in a [`CampaignAuditReport`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CampaignBulletinAudit {
    pub seq: u64,
    pub author_lane: String,
    pub kind: String,
    pub body: String,
    pub prev_hash_sha256: String,
    pub entry_hash_sha256: String,
}

/// Staged or integrated worker proposal in a [`CampaignAuditReport`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CampaignProposalAudit {
    pub id: u64,
    pub worker_lane: String,
    pub base_oid: String,
    pub touched_files: Vec<String>,
    pub integrated: bool,
}

/// Sole-committer `Integrator` target-branch commit record in a [`CampaignAuditReport`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CampaignTargetCommitAudit {
    pub proposal_id: u64,
    pub worker_lane: String,
    pub committer_lane: String,
    pub committer_role: LaneRole,
    pub parent_oid: String,
    pub new_oid: String,
    pub touched_files: Vec<String>,
    pub verification_passed: bool,
}

/// Structured read-only audit report for `<git-common-dir>/gitpulse/campaign.json`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CampaignAuditReport {
    pub ok: bool,
    pub repo_path: String,
    pub state_path: String,
    pub initialized: bool,
    pub invariants_verified: bool,
    pub sole_committer_enforced: bool,
    pub key_permissions_valid: bool,
    pub hmac_authenticated: bool,
    pub odb_anchor_verified: bool,
    pub odb_state_ref_oid: Option<String>,
    pub verification_error: Option<String>,
    pub genesis_oid: Option<String>,
    pub target_oid: Option<String>,
    pub head_oid: Option<String>,
    pub head_matches_target: Option<bool>,
    pub bulletin_head_sha256: Option<String>,
    pub lanes: Vec<CampaignLaneAudit>,
    pub active_leases: Vec<CampaignLeaseAudit>,
    pub bulletin_entries: Vec<CampaignBulletinAudit>,
    pub bulletin_total: usize,
    pub bulletin_returned: usize,
    pub bulletin_truncated: bool,
    pub proposals: Vec<CampaignProposalAudit>,
    pub proposals_total: usize,
    pub target_commits: Vec<CampaignTargetCommitAudit>,
    pub target_commits_total: usize,
    pub truncated: bool,
}

fn is_git_hex_oid(raw: &str) -> bool {
    matches!(raw.len(), 40 | 64) && raw.bytes().all(|b| b.is_ascii_hexdigit())
}

fn resolve_canonical_commit_oid(
    repo: &Path,
    common_str: &str,
    rev: &str,
) -> Result<Option<String>, String> {
    if rev.is_empty() || rev.starts_with('-') {
        return Ok(None);
    }
    let spec = format!("{rev}^{{commit}}");
    let run = git_captured(
        repo,
        &[
            "--no-optional-locks",
            "--git-dir",
            common_str,
            "rev-parse",
            "--verify",
            "--quiet",
            &spec,
        ],
    )
    .map_err(|e| format!("cannot query Git ODB for {rev}: {}", e.into_message()))?;
    if !run.success {
        return Ok(None);
    }
    let oid = String::from_utf8_lossy(&run.stdout).trim().to_string();
    if !is_git_hex_oid(&oid) {
        return Ok(None);
    }
    Ok(Some(oid))
}

fn verify_campaign_against_git_odb(
    repo: &Path,
    common: &Path,
    ctrl: &CampaignController,
) -> (Option<String>, Option<bool>, Option<String>) {
    let Some(common_str) = common.to_str() else {
        return (
            None,
            None,
            Some("Git common directory path is not valid UTF-8".to_string()),
        );
    };
    let live_head = match resolve_canonical_commit_oid(repo, common_str, "HEAD") {
        Err(msg) => return (None, None, Some(msg)),
        Ok(None) => {
            return (
                None,
                Some(false),
                Some(
                    "LiveTargetBranchUnborn: primary repository HEAD has no commit in Git ODB"
                        .to_string(),
                ),
            );
        }
        Ok(Some(oid)) => oid,
    };

    let genesis = ctrl.genesis_oid();
    if !is_git_hex_oid(genesis) {
        return (
            Some(live_head),
            Some(false),
            Some(format!(
                "UnanchoredGenesisOid: genesis_oid {genesis:?} is not a canonical Git commit OID"
            )),
        );
    }
    match resolve_canonical_commit_oid(repo, common_str, genesis) {
        Err(msg) => return (Some(live_head), Some(false), Some(msg)),
        Ok(Some(resolved)) if resolved.eq_ignore_ascii_case(genesis) => {}
        Ok(_) => {
            return (
                Some(live_head),
                Some(false),
                Some(format!(
                    "DanglingGenesisOid: genesis_oid {genesis} does not exist as a commit in Git ODB"
                )),
            );
        }
    }

    for rec in ctrl.target_history() {
        for (label, oid) in [("parent_oid", &rec.parent_oid), ("new_oid", &rec.new_oid)] {
            if !is_git_hex_oid(oid) {
                return (
                    Some(live_head),
                    Some(false),
                    Some(format!(
                        "UnanchoredTargetHistoryOid: {label} {oid:?} in proposal {} is not a canonical Git commit OID",
                        rec.proposal_id
                    )),
                );
            }
            match resolve_canonical_commit_oid(repo, common_str, oid) {
                Err(msg) => return (Some(live_head), Some(false), Some(msg)),
                Ok(Some(resolved)) if resolved.eq_ignore_ascii_case(oid) => {}
                Ok(_) => {
                    return (
                        Some(live_head),
                        Some(false),
                        Some(format!(
                            "DanglingTargetHistoryOid: {label} {oid} in proposal {} does not exist as a commit in Git ODB",
                            rec.proposal_id
                        )),
                    );
                }
            }
        }

        let anc = match git_captured(
            repo,
            &[
                "--no-optional-locks",
                "--git-dir",
                common_str,
                "merge-base",
                "--is-ancestor",
                &rec.parent_oid,
                &rec.new_oid,
            ],
        ) {
            Ok(run) => run,
            Err(e) => {
                return (
                    Some(live_head),
                    Some(false),
                    Some(format!(
                        "cannot verify Git ODB ancestry: {}",
                        e.into_message()
                    )),
                );
            }
        };
        if !anc.success {
            return (
                Some(live_head),
                Some(false),
                Some(format!(
                    "NonLinearTargetHistory: parent_oid {} is not an ancestor of new_oid {} in Git ODB (proposal {})",
                    rec.parent_oid, rec.new_oid, rec.proposal_id
                )),
            );
        }
    }

    if !live_head.eq_ignore_ascii_case(ctrl.target_oid()) {
        return (
            Some(live_head.clone()),
            Some(false),
            Some(format!(
                "UnauthorizedTargetBranchDrift: primary repository HEAD {} diverges from sole-committer Integrator target_oid {}",
                live_head,
                ctrl.target_oid()
            )),
        );
    }

    (Some(live_head), Some(true), None)
}

/// Resolves the canonical repository path and `<git-common-dir>/gitpulse/campaign.json`
/// without creating any directories or files on disk.
pub fn resolve_campaign_state_path(repo_path: &str) -> Result<(PathBuf, PathBuf), CommandError> {
    let repo = validate_repo(repo_path)?;
    let common = resolve_git_common_dir(&repo)?;
    let state_path = common.join("gitpulse").join("campaign.json");
    Ok((repo, state_path))
}

/// Audits `<git-common-dir>/gitpulse/campaign.json` in read-only mode, verifying
/// all three formal campaign invariants plus live Git ODB / primary `HEAD` parity:
/// 1. Pairwise hierarchical single-writer lease disjointness across lanes,
/// 2. Genesis-to-head FIPS 180-4 SHA-256 `BULLETIN` hash-chain integrity, and
/// 3. Sole-committer `Integrator` target-branch authority, acyclic Git ODB commit
///    ancestry, and exact parity between `target_oid` and the primary repository's
///    live `HEAD`.
///
/// Creates nothing on disk. When no `campaign.json` exists yet, `ok` is `true`
/// while `initialized`, `invariants_verified`, and `sole_committer_enforced` are
/// `false` so an uninitialized repo is never conflated with a verified campaign.
/// When `campaign.json` exists and fails state-machine or live Git ODB / `HEAD`
/// verification, `ok`, `invariants_verified`, and `sole_committer_enforced` are
/// `false` with `verification_error` naming the cause.
pub fn audit_repo_campaign(
    repo_path: &str,
    since_seq: Option<u64>,
    limit: Option<u32>,
) -> Result<CampaignAuditReport, CommandError> {
    let repo = validate_repo(repo_path)?;
    let common = resolve_git_common_dir(&repo)?;
    let state_path = common.join("gitpulse").join("campaign.json");
    let repo_str = repo.to_string_lossy().into_owned();
    let state_str = state_path.to_string_lossy().into_owned();

    match std::fs::symlink_metadata(&state_path) {
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            let key_path = campaign_key_path_for(&state_path);
            let key_present = std::fs::symlink_metadata(&key_path).is_ok();
            let key_ok = load_verified_campaign_key(&key_path).is_ok();
            let odb_probe = common
                .to_str()
                .ok_or_else(|| "Git common directory path is not valid UTF-8".to_string())
                .and_then(|cs| read_odb_state_anchor(&repo, cs));
            match odb_probe {
                Err(err_msg) => {
                    return Ok(CampaignAuditReport {
                        ok: false,
                        repo_path: repo_str,
                        state_path: state_str,
                        initialized: true,
                        invariants_verified: false,
                        sole_committer_enforced: false,
                        key_permissions_valid: key_ok,
                        hmac_authenticated: false,
                        odb_anchor_verified: false,
                        odb_state_ref_oid: None,
                        verification_error: Some(err_msg),
                        genesis_oid: None,
                        target_oid: None,
                        head_oid: None,
                        head_matches_target: None,
                        bulletin_head_sha256: None,
                        lanes: Vec::new(),
                        active_leases: Vec::new(),
                        bulletin_entries: Vec::new(),
                        bulletin_total: 0,
                        bulletin_returned: 0,
                        bulletin_truncated: false,
                        proposals: Vec::new(),
                        proposals_total: 0,
                        target_commits: Vec::new(),
                        target_commits_total: 0,
                        truncated: false,
                    });
                }
                Ok(Some((anchor_oid, _))) => {
                    return Ok(CampaignAuditReport {
                        ok: false,
                        repo_path: repo_str,
                        state_path: state_str,
                        initialized: true,
                        invariants_verified: false,
                        sole_committer_enforced: false,
                        key_permissions_valid: key_ok,
                        hmac_authenticated: false,
                        odb_anchor_verified: false,
                        odb_state_ref_oid: Some(anchor_oid.clone()),
                        verification_error: Some(format!(
                            "CampaignStateDeletedWhileAnchored: {} was removed while {CAMPAIGN_STATE_REF} ({anchor_oid}) exists in Git ODB",
                            state_path.display()
                        )),
                        genesis_oid: None,
                        target_oid: None,
                        head_oid: None,
                        head_matches_target: None,
                        bulletin_head_sha256: None,
                        lanes: Vec::new(),
                        active_leases: Vec::new(),
                        bulletin_entries: Vec::new(),
                        bulletin_total: 0,
                        bulletin_returned: 0,
                        bulletin_truncated: false,
                        proposals: Vec::new(),
                        proposals_total: 0,
                        target_commits: Vec::new(),
                        target_commits_total: 0,
                        truncated: false,
                    });
                }
                Ok(None) if key_present => {
                    return Ok(CampaignAuditReport {
                        ok: false,
                        repo_path: repo_str,
                        state_path: state_str,
                        initialized: true,
                        invariants_verified: false,
                        sole_committer_enforced: false,
                        key_permissions_valid: key_ok,
                        hmac_authenticated: false,
                        odb_anchor_verified: false,
                        odb_state_ref_oid: None,
                        verification_error: Some(format!(
                            "CampaignStateDeletedWhileKeyPresent: {} is missing while {} exists",
                            state_path.display(),
                            key_path.display()
                        )),
                        genesis_oid: None,
                        target_oid: None,
                        head_oid: None,
                        head_matches_target: None,
                        bulletin_head_sha256: None,
                        lanes: Vec::new(),
                        active_leases: Vec::new(),
                        bulletin_entries: Vec::new(),
                        bulletin_total: 0,
                        bulletin_returned: 0,
                        bulletin_truncated: false,
                        proposals: Vec::new(),
                        proposals_total: 0,
                        target_commits: Vec::new(),
                        target_commits_total: 0,
                        truncated: false,
                    });
                }
                Ok(None) => {}
            }
            return Ok(CampaignAuditReport {
                ok: true,
                repo_path: repo_str,
                state_path: state_str,
                initialized: false,
                invariants_verified: false,
                sole_committer_enforced: false,
                key_permissions_valid: false,
                hmac_authenticated: false,
                odb_anchor_verified: false,
                odb_state_ref_oid: None,
                verification_error: None,
                genesis_oid: None,
                target_oid: None,
                head_oid: None,
                head_matches_target: None,
                bulletin_head_sha256: None,
                lanes: Vec::new(),
                active_leases: Vec::new(),
                bulletin_entries: Vec::new(),
                bulletin_total: 0,
                bulletin_returned: 0,
                bulletin_truncated: false,
                proposals: Vec::new(),
                proposals_total: 0,
                target_commits: Vec::new(),
                target_commits_total: 0,
                truncated: false,
            });
        }
        Err(e) => {
            return Ok(CampaignAuditReport {
                ok: false,
                repo_path: repo_str,
                state_path: state_str,
                initialized: true,
                invariants_verified: false,
                sole_committer_enforced: false,
                key_permissions_valid: false,
                hmac_authenticated: false,
                odb_anchor_verified: false,
                odb_state_ref_oid: None,
                verification_error: Some(format!(
                    "cannot stat campaign state {}: {e}",
                    state_path.display()
                )),
                genesis_oid: None,
                target_oid: None,
                head_oid: None,
                head_matches_target: None,
                bulletin_head_sha256: None,
                lanes: Vec::new(),
                active_leases: Vec::new(),
                bulletin_entries: Vec::new(),
                bulletin_total: 0,
                bulletin_returned: 0,
                bulletin_truncated: false,
                proposals: Vec::new(),
                proposals_total: 0,
                target_commits: Vec::new(),
                target_commits_total: 0,
                truncated: false,
            });
        }
        Ok(_) => {}
    }

    let key_ok = load_verified_campaign_key(&campaign_key_path_for(&state_path)).is_ok();
    let (ctrl, odb_state_ref_oid) = match CampaignController::load_verified_with_anchor(&state_path)
    {
        Ok(pair) => pair,
        Err(reason) => {
            return Ok(CampaignAuditReport {
                ok: false,
                repo_path: repo_str,
                state_path: state_str,
                initialized: true,
                invariants_verified: false,
                sole_committer_enforced: false,
                key_permissions_valid: key_ok,
                hmac_authenticated: false,
                odb_anchor_verified: false,
                odb_state_ref_oid: None,
                verification_error: Some(reason),
                genesis_oid: None,
                target_oid: None,
                head_oid: None,
                head_matches_target: None,
                bulletin_head_sha256: None,
                lanes: Vec::new(),
                active_leases: Vec::new(),
                bulletin_entries: Vec::new(),
                bulletin_total: 0,
                bulletin_returned: 0,
                bulletin_truncated: false,
                proposals: Vec::new(),
                proposals_total: 0,
                target_commits: Vec::new(),
                target_commits_total: 0,
                truncated: false,
            });
        }
    };

    let mut lanes_truncated = false;
    let mut lanes: Vec<CampaignLaneAudit> = ctrl
        .lanes()
        .iter()
        .map(|(lane, &role)| CampaignLaneAudit {
            lane: lane.clone(),
            role,
        })
        .collect();
    if lanes.len() > MAX_CAMPAIGN_AUDIT_ITEMS {
        lanes.truncate(MAX_CAMPAIGN_AUDIT_ITEMS);
        lanes_truncated = true;
    }

    let mut leases_truncated = false;
    let mut active_leases: Vec<CampaignLeaseAudit> = ctrl
        .active_leases()
        .iter()
        .map(|g| {
            let (scope_kind, path) = match &g.scope {
                LeaseScope::FileExact(p) => ("FileExact".to_string(), p.clone()),
                LeaseScope::DirSubtree(p) => ("DirSubtree".to_string(), p.clone()),
            };
            CampaignLeaseAudit {
                lane: g.lane.clone(),
                mode: g.mode,
                scope_kind,
                path,
            }
        })
        .collect();
    if active_leases.len() > MAX_CAMPAIGN_AUDIT_ITEMS {
        active_leases.truncate(MAX_CAMPAIGN_AUDIT_ITEMS);
        leases_truncated = true;
    }

    let min_seq = since_seq.unwrap_or(0);
    let bulletin_cap = limit
        .map(|l| (l as usize).clamp(1, MAX_CAMPAIGN_AUDIT_ITEMS))
        .unwrap_or(DEFAULT_CAMPAIGN_BULLETIN_LIMIT);
    let all_bulletin = ctrl.bulletin().entries();
    let bulletin_total = all_bulletin.len();
    let filtered_bulletin: Vec<&BulletinEntry> =
        all_bulletin.iter().filter(|e| e.seq > min_seq).collect();
    let bulletin_truncated = filtered_bulletin.len() > bulletin_cap;
    let bulletin_entries: Vec<CampaignBulletinAudit> = filtered_bulletin
        .into_iter()
        .take(bulletin_cap)
        .map(|e| CampaignBulletinAudit {
            seq: e.seq,
            author_lane: e.author_lane.clone(),
            kind: e.kind.clone(),
            body: e.body.clone(),
            prev_hash_sha256: hex_sha256(&e.prev_hash),
            entry_hash_sha256: hex_sha256(&e.entry_hash),
        })
        .collect();
    let bulletin_returned = bulletin_entries.len();

    let proposals_total = ctrl.proposals().len();
    let mut proposals: Vec<CampaignProposalAudit> = ctrl
        .proposals()
        .values()
        .map(|p| CampaignProposalAudit {
            id: p.id,
            worker_lane: p.worker_lane.clone(),
            base_oid: p.base_oid.clone(),
            touched_files: p.touched_files.iter().cloned().collect(),
            integrated: p.integrated,
        })
        .collect();
    let proposals_truncated = proposals.len() > MAX_CAMPAIGN_AUDIT_ITEMS;
    if proposals_truncated {
        proposals.truncate(MAX_CAMPAIGN_AUDIT_ITEMS);
    }

    let target_commits_total = ctrl.target_history().len();
    let mut target_commits: Vec<CampaignTargetCommitAudit> = ctrl
        .target_history()
        .iter()
        .map(|c| CampaignTargetCommitAudit {
            proposal_id: c.proposal_id,
            worker_lane: c.worker_lane.clone(),
            committer_lane: c.committer_lane.clone(),
            committer_role: c.committer_role,
            parent_oid: c.parent_oid.clone(),
            new_oid: c.new_oid.clone(),
            touched_files: c.touched_files.iter().cloned().collect(),
            verification_passed: c.verification_passed,
        })
        .collect();
    let commits_truncated = target_commits.len() > MAX_CAMPAIGN_AUDIT_ITEMS;
    if commits_truncated {
        target_commits.truncate(MAX_CAMPAIGN_AUDIT_ITEMS);
    }

    let (head_oid, head_matches_target, odb_error) =
        verify_campaign_against_git_odb(&repo, &common, &ctrl);
    let odb_ok = odb_error.is_none();

    let truncated = lanes_truncated
        || leases_truncated
        || bulletin_truncated
        || proposals_truncated
        || commits_truncated;

    Ok(CampaignAuditReport {
        ok: odb_ok,
        repo_path: repo_str,
        state_path: state_str,
        initialized: true,
        invariants_verified: odb_ok,
        sole_committer_enforced: odb_ok,
        key_permissions_valid: true,
        hmac_authenticated: true,
        odb_anchor_verified: odb_state_ref_oid.is_some(),
        odb_state_ref_oid,
        verification_error: odb_error,
        genesis_oid: Some(ctrl.genesis_oid().to_string()),
        target_oid: Some(ctrl.target_oid().to_string()),
        head_oid,
        head_matches_target,
        bulletin_head_sha256: Some(hex_sha256(&ctrl.bulletin().head_hash())),
        lanes,
        active_leases,
        bulletin_entries,
        bulletin_total,
        bulletin_returned,
        bulletin_truncated,
        proposals,
        proposals_total,
        target_commits,
        target_commits_total,
        truncated,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sha256_and_hmac_sha256_match_nist_and_ietf_rfc_4231_vectors() {
        // Empty string: e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855
        let empty = sha256_digest(&[b""]);
        assert_eq!(
            &empty[..4],
            &[0xe3, 0xb0, 0xc4, 0x42],
            "SHA-256 empty vector prefix mismatch"
        );
        assert_eq!(
            hex_sha256(&empty),
            "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"
        );
        // "abc": ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad
        let abc = sha256_digest(&[b"abc"]);
        assert_eq!(
            hex_sha256(&abc),
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );

        // RFC 4231 Test Case 1: Key = 20 bytes of 0x0b, Data = "Hi There"
        let tc1 = hmac_sha256(&[0x0bu8; 20], &[b"Hi There"]);
        assert_eq!(
            hex_sha256(&tc1),
            "b0344c61d8db38535ca8afceaf0bf12b881dc200c9833da726e9376c2e32cff7"
        );

        // RFC 4231 Test Case 2: Key = "Jefe", Data = "what do ya want for nothing?"
        let tc2 = hmac_sha256(b"Jefe", &[b"what do ya want for ", b"nothing?"]);
        assert_eq!(
            hex_sha256(&tc2),
            "5bdcc146bf60754e6a042426089575c75a003f089d2739839dec58b964ec3843"
        );

        // RFC 4231 Test Case 6: Key = 131 bytes of 0xaa (> 64-byte block size!)
        let tc6 = hmac_sha256(
            &[0xaau8; 131],
            &[b"Test Using Larger Than Block-Size Key - Hash Key First"],
        );
        assert_eq!(
            hex_sha256(&tc6),
            "60e431591ee0b67f0d8a26aacbf5b77f8e0bc6213728c5140546040f0ee37f54"
        );
        assert!(ct_eq_32(&tc6, &tc6));
        let mut flipped = tc6;
        flipped[31] ^= 0x01;
        assert!(!ct_eq_32(&tc6, &flipped));
    }

    #[test]
    fn disproofs_catch_subtree_overlap_path_traversal_bulletin_tamper_and_worker_commit() {
        let dir_core = LeaseScope::dir("crates/core").expect("valid dir");
        let file_inside = LeaseScope::file("crates/core/src/lib.rs").expect("valid file");
        let sibling_prefix = LeaseScope::file("crates/core_ext/src/lib.rs").expect("valid sibling");
        assert!(scopes_overlap(&dir_core, &file_inside));
        assert!(scopes_overlap(&file_inside, &dir_core));
        assert!(
            !scopes_overlap(&dir_core, &sibling_prefix),
            "sibling prefix crates/core_ext must not collide with crates/core"
        );

        for bad in [
            "",
            "/etc/passwd",
            "../outside.rs",
            "src/../main.rs",
            "./src/lib.rs",
            "src//lib.rs",
            "src\\lib.rs",
            "src/lib\0.rs",
        ] {
            assert!(
                normalize_repo_rel_path(bad).is_err(),
                "malformed path {bad:?} must be rejected"
            );
        }

        let mut log = BulletinLog::default();
        log.append("worker-a", "handoff", "claim src/a.rs");
        let snap1 = log.clone();
        log.append("worker-b", "blocker", "waiting on schema");
        assert!(log.verify_extension_of(&snap1).is_ok());

        let truncated = BulletinLog::default();
        assert!(matches!(
            truncated.verify_extension_of(&snap1),
            Err(CampaignViolation::BulletinHistoryTruncatedOrRewritten { index: 0 })
        ));

        let mut rewritten = log.clone();
        rewritten.entries[0].body = "forged body".to_string();
        assert!(
            rewritten.verify_integrity().is_err(),
            "tampered entry body must break SHA-256 chain"
        );
        let mut forged_rehashed = BulletinLog::default();
        forged_rehashed.append("worker-a", "handoff", "forged body");
        forged_rehashed.append("worker-b", "blocker", "waiting on schema");
        assert!(forged_rehashed.verify_integrity().is_ok());
        assert!(matches!(
            forged_rehashed.verify_extension_of(&snap1),
            Err(CampaignViolation::BulletinHistoryTruncatedOrRewritten { index: 0 })
        ));

        let mut ctrl = CampaignController::new("oid-0");
        ctrl.register_lane("worker-a", LaneRole::Worker)
            .expect("register worker-a");
        ctrl.register_lane("worker-b", LaneRole::Worker)
            .expect("register worker-b");
        ctrl.register_lane("integrator", LaneRole::Integrator)
            .expect("register integrator");

        ctrl.acquire_leases(
            "worker-a",
            LeaseMode::Write,
            vec![LeaseScope::dir("src/auth").expect("scope")],
        )
        .expect("worker-a lease");

        let conflict = ctrl.acquire_leases(
            "worker-b",
            LeaseMode::Write,
            vec![LeaseScope::file("src/auth/token.rs").expect("scope")],
        );
        assert!(matches!(
            conflict,
            Err(CampaignViolation::LeaseConflict { .. })
        ));

        let out_of_lease = ctrl.stage_proposal("worker-a", "oid-0", &["src/billing/charge.rs"]);
        assert!(matches!(
            out_of_lease,
            Err(CampaignViolation::OutOfLeaseModification { .. })
        ));

        let prop_id = ctrl
            .stage_proposal("worker-a", "oid-0", &["src/auth/token.rs"])
            .expect("valid proposal");
        let worker_commit = ctrl.commit_target_branch("worker-a", prop_id, "oid-0", "oid-1", true);
        assert!(matches!(
            worker_commit,
            Err(CampaignViolation::WorkerDirectCommitForbidden { .. })
        ));

        let unverified = ctrl.commit_target_branch("integrator", prop_id, "oid-0", "oid-1", false);
        assert!(matches!(
            unverified,
            Err(CampaignViolation::UnverifiedIntegration { .. })
        ));

        let rec = ctrl
            .commit_target_branch("integrator", prop_id, "oid-0", "oid-1", true)
            .expect("verified integration must succeed");
        assert_eq!(rec.committer_role, LaneRole::Integrator);
        assert!(
            ctrl.active_leases().is_empty(),
            "worker-a lease must auto-release upon integration"
        );
        ctrl.check_global_invariants("oid-0")
            .expect("global invariants hold");
    }

    #[test]
    fn audit_repo_campaign_reports_absent_verified_and_tampered_states_honestly() {
        let dir = tempfile::tempdir().expect("tempdir");
        crate::test_support::git_in(dir.path(), &["init", "-q"]);
        let repo = dir.path().to_string_lossy().into_owned();

        // 1. Uninitialized repository: creates nothing on disk and never claims verified.
        let absent = audit_repo_campaign(&repo, None, None).expect("absent audit");
        assert!(absent.ok);
        assert!(!absent.initialized);
        assert!(!absent.invariants_verified);
        assert!(!absent.sole_committer_enforced);
        assert!(!Path::new(&absent.state_path).exists());

        // 2. Verified campaign anchored to real Git ODB commits with worker lease,
        // BULLETIN, and sole-committer Integrator commit.
        crate::test_support::git_in(
            dir.path(),
            &["commit", "--allow-empty", "-m", "genesis commit"],
        );
        let oid_genesis = crate::engine::git_cli::git_text(dir.path(), &["rev-parse", "HEAD"])
            .expect("genesis HEAD")
            .trim()
            .to_string();
        crate::test_support::git_in(
            dir.path(),
            &["commit", "--allow-empty", "-m", "integrated commit"],
        );
        let oid_next = crate::engine::git_cli::git_text(dir.path(), &["rev-parse", "HEAD"])
            .expect("integrated HEAD")
            .trim()
            .to_string();

        let (_, state_path) = resolve_campaign_state_path(&repo).expect("state path");
        let mut ctrl = CampaignController::try_new(&oid_genesis).expect("genesis");
        ctrl.register_lane("worker-1", LaneRole::Worker)
            .expect("worker-1");
        ctrl.register_lane("worker-2", LaneRole::Worker)
            .expect("worker-2");
        ctrl.register_lane("integrator", LaneRole::Integrator)
            .expect("integrator");
        ctrl.acquire_leases(
            "worker-1",
            LeaseMode::Write,
            vec![LeaseScope::dir("src/auth").expect("scope")],
        )
        .expect("lease 1");
        ctrl.acquire_leases(
            "worker-2",
            LeaseMode::Write,
            vec![LeaseScope::file("src/ui/app.ts").expect("scope")],
        )
        .expect("lease 2");
        ctrl.post_bulletin("worker-1", "handoff", "auth token verifier ready")
            .expect("bulletin 1");
        let pid = ctrl
            .stage_proposal("worker-1", &oid_genesis, &["src/auth/verify.rs"])
            .expect("proposal");
        ctrl.commit_target_branch("integrator", pid, &oid_genesis, &oid_next, true)
            .expect("integrate");
        ctrl.save_verified_atomic(&state_path).expect("save");

        let verified = audit_repo_campaign(&repo, None, None).expect("verified audit");
        assert!(verified.ok);
        assert!(verified.initialized);
        assert!(verified.invariants_verified);
        assert!(verified.sole_committer_enforced);
        assert_eq!(verified.target_oid.as_deref(), Some(oid_next.as_str()));
        assert_eq!(verified.head_oid.as_deref(), Some(oid_next.as_str()));
        assert_eq!(verified.head_matches_target, Some(true));
        assert_eq!(verified.lanes.len(), 3);
        assert_eq!(verified.active_leases.len(), 1);
        assert_eq!(verified.active_leases[0].lane, "worker-2");
        assert_eq!(verified.bulletin_total, 2);
        assert_eq!(verified.target_commits.len(), 1);
        assert_eq!(
            verified.target_commits[0].committer_role,
            LaneRole::Integrator
        );

        // Incremental BULLETIN read (since_seq=1) returns only entry #2.
        let incremental = audit_repo_campaign(&repo, Some(1), Some(1)).expect("incremental");
        assert_eq!(incremental.bulletin_returned, 1);
        assert_eq!(incremental.bulletin_entries[0].seq, 2);
        assert!(!incremental.bulletin_truncated);

        // 3. Tamper with persisted BULLETIN body on disk -> audit convicts hash-chain corruption!
        let raw = std::fs::read_to_string(&state_path).expect("read state");
        let forged = raw.replace("auth token verifier ready", "FORGED BULLETIN PAYLOAD");
        assert_ne!(raw, forged);
        std::fs::write(&state_path, forged).expect("write tampered");

        let tampered = audit_repo_campaign(&repo, None, None).expect("tampered audit");
        assert!(!tampered.ok);
        assert!(tampered.initialized);
        assert!(!tampered.invariants_verified);
        assert!(!tampered.sole_committer_enforced);
        assert!(
            tampered
                .verification_error
                .as_deref()
                .unwrap_or_default()
                .contains("BulletinHashChainBroken"),
            "unexpected error: {:?}",
            tampered.verification_error
        );
    }

    #[test]
    fn audit_repo_campaign_convicts_unkeyed_rewrite_loosened_key_perms_symlink_and_rollback() {
        let dir = tempfile::tempdir().expect("tempdir");
        crate::test_support::git_in(dir.path(), &["init", "-q"]);
        crate::test_support::git_in(
            dir.path(),
            &["commit", "--allow-empty", "-m", "genesis commit"],
        );
        let oid_genesis = crate::engine::git_cli::git_text(dir.path(), &["rev-parse", "HEAD"])
            .expect("genesis HEAD")
            .trim()
            .to_string();
        let repo = dir.path().to_string_lossy().into_owned();
        let (_, state_path) = resolve_campaign_state_path(&repo).expect("state path");

        let mut ctrl = CampaignController::try_new(&oid_genesis).expect("genesis");
        ctrl.register_lane("worker-1", LaneRole::Worker)
            .expect("worker-1");
        ctrl.register_lane("integrator", LaneRole::Integrator)
            .expect("integrator");
        ctrl.post_bulletin("worker-1", "status", "step 1")
            .expect("bulletin 1");
        ctrl.save_verified_atomic(&state_path).expect("save 1");

        let key_path = state_path.with_file_name("campaign.key");
        assert!(
            key_path.exists(),
            "save_verified_atomic must create 32-byte 0600 campaign.key alongside campaign.json"
        );

        let snap_v1 = std::fs::read(&state_path).expect("read v1");

        // Advance campaign state to v2 (bulletin seq=2) -> updates refs/gitpulse/campaign/state.
        ctrl.post_bulletin("worker-1", "status", "step 2")
            .expect("bulletin 2");
        ctrl.save_verified_atomic(&state_path).expect("save 2");
        let snap_v2 = std::fs::read(&state_path).expect("read v2");
        let ok_v2 = audit_repo_campaign(&repo, None, None).expect("audit v2");
        assert!(ok_v2.ok && ok_v2.invariants_verified);
        assert_eq!(ok_v2.bulletin_total, 2);

        // Attack A: Wholesale unkeyed campaign.json rewrite from genesis (valid SHA-256 chain + valid HEAD OID)!
        let mut forged_genesis = CampaignController::try_new(&oid_genesis).expect("forged");
        forged_genesis
            .register_lane("rogue-integrator", LaneRole::Integrator)
            .expect("register rogue");
        forged_genesis
            .post_bulletin("rogue-integrator", "forged", "rehashed from genesis")
            .expect("post forged");
        std::fs::write(
            &state_path,
            serde_json::to_vec_pretty(&forged_genesis).expect("serialize forged"),
        )
        .expect("write unkeyed genesis rewrite");
        let unkeyed = audit_repo_campaign(&repo, None, None).expect("audit unkeyed");
        assert!(
            !unkeyed.ok && !unkeyed.invariants_verified,
            "wholesale unkeyed campaign.json rewrite from genesis must fail closed: {unkeyed:?}"
        );

        // Attack B: Rollback campaign.json on disk to validly-HMAC-signed v1 snapshot!
        std::fs::write(&state_path, &snap_v1).expect("rollback to v1");
        let rolled_back = audit_repo_campaign(&repo, None, None).expect("audit rollback");
        assert!(
            !rolled_back.ok && !rolled_back.invariants_verified,
            "state rollback to v1 must be convicted via refs/gitpulse/campaign/state ODB anchor: {rolled_back:?}"
        );

        // Restore valid v2 bytes and check it passes again before testing key hygiene attacks.
        std::fs::write(&state_path, &snap_v2).expect("restore v2");
        let restored = audit_repo_campaign(&repo, None, None).expect("audit restored v2");
        assert!(restored.ok && restored.invariants_verified);

        // Attack C (Unix): Loosened campaign.key permissions (0644) must fail closed!
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&key_path, std::fs::Permissions::from_mode(0o644))
                .expect("chmod 0644");
            let loose = audit_repo_campaign(&repo, None, None).expect("audit loose key");
            assert!(
                !loose.ok && !loose.invariants_verified,
                "loosened 0644 campaign.key must fail closed: {loose:?}"
            );
            std::fs::set_permissions(&key_path, std::fs::Permissions::from_mode(0o600))
                .expect("restore 0600");
            assert!(
                audit_repo_campaign(&repo, None, None)
                    .expect("re-audit 0600")
                    .ok
            );

            // Attack D (Unix): Symlink swap on campaign.key must fail closed!
            let moved_key = state_path.with_file_name("campaign.key.real");
            std::fs::rename(&key_path, &moved_key).expect("move key");
            std::os::unix::fs::symlink(&moved_key, &key_path).expect("symlink key");
            let sym_key = audit_repo_campaign(&repo, None, None).expect("audit symlinked key");
            assert!(
                !sym_key.ok && !sym_key.invariants_verified,
                "symlinked campaign.key must fail closed: {sym_key:?}"
            );
            std::fs::remove_file(&key_path).expect("rm symlink key");
            std::fs::rename(&moved_key, &key_path).expect("restore real key");
            assert!(
                audit_repo_campaign(&repo, None, None)
                    .expect("re-audit real key")
                    .ok
            );

            // Attack D2 (Unix): Symlink swap on campaign.json itself must fail closed!
            let moved_state = state_path.with_file_name("campaign.json.real");
            std::fs::rename(&state_path, &moved_state).expect("move state");
            std::os::unix::fs::symlink(&moved_state, &state_path).expect("symlink state");
            let sym_state = audit_repo_campaign(&repo, None, None).expect("audit symlinked state");
            assert!(
                !sym_state.ok && !sym_state.invariants_verified,
                "symlinked campaign.json must fail closed: {sym_state:?}"
            );
            std::fs::remove_file(&state_path).expect("rm symlink state");
            std::fs::rename(&moved_state, &state_path).expect("restore real state");
            assert!(
                audit_repo_campaign(&repo, None, None)
                    .expect("re-audit real state")
                    .ok
            );
        }

        // Attack E: Deleting campaign.json + campaign.key while refs/gitpulse/campaign/state exists in Git ODB must convict!
        std::fs::remove_file(&state_path).expect("delete campaign.json");
        std::fs::remove_file(&key_path).expect("delete campaign.key");
        let wiped = audit_repo_campaign(&repo, None, None).expect("audit wiped state");
        assert!(
            !wiped.ok && wiped.initialized && !wiped.invariants_verified,
            "deleting campaign.json while refs/gitpulse/campaign/state exists must fail closed: {wiped:?}"
        );
    }

    #[test]
    fn odb_anchor_hmac_git_update_ref_rollback_and_private_state_mode_are_enforced() {
        let dir = tempfile::tempdir().expect("tempdir");
        crate::test_support::git_in(dir.path(), &["init", "-q"]);
        crate::test_support::git_in(
            dir.path(),
            &["commit", "--allow-empty", "-m", "genesis commit"],
        );
        let oid_genesis = crate::engine::git_cli::git_text(dir.path(), &["rev-parse", "HEAD"])
            .expect("genesis HEAD")
            .trim()
            .to_string();
        let repo = dir.path().to_string_lossy().into_owned();
        let (_, state_path) = resolve_campaign_state_path(&repo).expect("state path");

        let mut ctrl = CampaignController::try_new(&oid_genesis).expect("genesis");
        ctrl.register_lane("worker-1", LaneRole::Worker).unwrap();
        ctrl.register_lane("integrator", LaneRole::Integrator)
            .unwrap();
        ctrl.post_bulletin("worker-1", "status", "v1").unwrap();
        ctrl.save_verified_atomic(&state_path).expect("save v1");

        #[cfg(unix)]
        {
            use std::os::unix::fs::MetadataExt;
            let state_mode = std::fs::symlink_metadata(&state_path).unwrap().mode() & 0o777;
            assert_eq!(
                state_mode, 0o600,
                "campaign.json must be persisted with 0600 permissions, got {:04o}",
                state_mode
            );
        }

        let snap_v1 = std::fs::read(&state_path).expect("snap v1");
        let ref_v1 =
            crate::engine::git_cli::git_text(dir.path(), &["rev-parse", CAMPAIGN_STATE_REF])
                .expect("ref v1")
                .trim()
                .to_string();

        ctrl.post_bulletin("worker-1", "status", "v2").unwrap();
        ctrl.save_verified_atomic(&state_path).expect("save v2");
        let ok_v2 = audit_repo_campaign(&repo, None, None).expect("audit v2");
        assert!(ok_v2.ok && ok_v2.key_permissions_valid && ok_v2.odb_anchor_verified);

        // Attack 1: Coordinated rollback of BOTH `campaign.json` AND `refs/gitpulse/campaign/state` back to v1!
        crate::test_support::git_in(dir.path(), &["update-ref", CAMPAIGN_STATE_REF, &ref_v1]);
        std::fs::write(&state_path, &snap_v1).expect("restore snap v1");
        let rolled = audit_repo_campaign(&repo, None, None).expect("audit coordinated rollback");
        assert!(
            !rolled.ok && !rolled.invariants_verified && !rolled.odb_anchor_verified,
            "coordinated rollback of campaign.json + refs/gitpulse/campaign/state to v1 must be convicted: {rolled:?}"
        );

        // Attack 2: Unkeyed synthetic ODB anchor blob forged without campaign.key HMAC must be rejected!
        let fake_anchor = serde_json::json!({
            "schema_version": 1,
            "chain_seq": 1,
            "prev_anchor_oid": null,
            "state_sha256": hex_sha256(&sha256_digest(&[&snap_v1])),
            "envelope_hmac_sha256": serde_json::from_slice::<serde_json::Value>(&snap_v1).unwrap()["envelope_hmac_sha256"],
            "genesis_oid": oid_genesis,
            "target_oid": oid_genesis,
            "bulletin_len": 1,
            "target_commits_len": 0,
            "anchor_hmac_sha256": "00".repeat(32)
        });
        let forged_oid_raw = git_with_stdin(
            dir.path(),
            &["hash-object", "-w", "--stdin"],
            serde_json::to_vec(&fake_anchor).unwrap().as_slice(),
        )
        .unwrap();
        let forged_oid = String::from_utf8_lossy(&forged_oid_raw).trim().to_string();
        crate::test_support::git_in(dir.path(), &["update-ref", CAMPAIGN_STATE_REF, &forged_oid]);
        let forged_audit = audit_repo_campaign(&repo, None, None).expect("audit forged anchor");
        assert!(
            !forged_audit.ok && !forged_audit.odb_anchor_verified,
            "unkeyed forged ODB anchor blob must fail HMAC verification: {forged_audit:?}"
        );
    }
}
