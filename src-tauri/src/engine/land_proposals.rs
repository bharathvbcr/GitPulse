//! Conflict proposal persistence in the Git object database (`refs/gitpulse/proposals/`),
//! conflict-file verdict caching (`ConflictFileKey`), bounded TTL/cap sweeping (`ProposalSweeper`),
//! and the tree-OID verification safety gate (`decide_land`).
//!
//! # Security & Durability Invariants
//!
//! - **Namespace**: Proposals live strictly under
//!   `refs/gitpulse/proposals/<sha256(branch)[:16]>/<source_commit_oid>`, outside
//!   `refs/heads/` and `refs/tags/`.
//! - **Quarantine isolation**: Every proposal commit is constructed inside a
//!   dedicated [`ObjectQuarantine`] (never a failed land's discarded quarantine)
//!   with ONLY the proposed file blobs at their repo paths, 2 parents
//!   `[tip, source_commit_oid]`, and server `now` timestamps (inherited
//!   `GIT_AUTHOR_DATE`/`GIT_COMMITTER_DATE` are stripped by `git_cli`).
//! - **Trailer injection safety**: `Resolver:` and `Requested-By:` trailer values
//!   are validated as printable single-line UTF-8 (rejecting `\n`, `\r`, `\t`,
//!   NUL, C0/C1 controls, `U+0085`, `U+2028`, `U+2029`); proposal commits
//!   ASCII-escape invalid values while resolution commits hard-reject them.
//! - **CAS safety**: Ref writes and sweeper/drop deletions always pass exact
//!   expected old OIDs (`update-ref --no-deref`), leaving concurrent newer
//!   overwrites intact and ignoring already-absent refs on post-land drop.
//! - **Auto-land safety gate (`decide_land`)**: Model-originated resolutions
//!   NEVER auto-land without exact `verified_tree_oid == candidate_tree_oid`;
//!   held resolutions (e.g. dual-model disagreement) NEVER auto-land even when
//!   verified.

use crate::engine::git_cli::{
    git_captured, git_captured_with_stdin, git_spawn_quarantined_streaming, git_text,
    resolve_git_common_dir, without_lazy_fetch,
};
use crate::engine::git_writer::{
    absent_oid_for_len, is_full_oid, validate_ref_name, SNAPSHOT_IDENTITY_ARGS,
};
use crate::engine::object_quarantine::ObjectQuarantine;
use crate::engine::process::{DEFAULT_TIMEOUT, MAX_OUTPUT_BYTES};
use crate::ipc_error::CommandError;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, HashMap, HashSet};
use std::ffi::OsStr;
use std::io::{Read, Write};
use std::path::{Component, Path, PathBuf};
use std::sync::{Arc, Condvar, Mutex, OnceLock};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

/// Reserved ref prefix under which conflict proposals are persisted.
pub const PROPOSAL_REF_PREFIX: &str = "refs/gitpulse/proposals/";
/// Default proposal retention window: 7 days (168 hours).
pub const PROPOSAL_TTL_SECS: u64 = 7 * 24 * 3600;
/// Maximum retained proposals per target branch (newest kept).
pub const PROPOSAL_MAX_PER_BRANCH: usize = 20;
/// Default wall-clock budget for a coalesced proposal sweep.
pub const PROPOSAL_SWEEP_TIMEOUT: Duration = Duration::from_secs(5);
/// Maximum size of a single proposed file blob (16 MiB).
pub const MAX_PROPOSAL_FILE_BYTES: usize = 16 * 1024 * 1024;
/// Maximum files permitted in a single proposal package.
pub const MAX_PROPOSAL_FILES: usize = 512;

const BRANCH_HASH_HEX_LEN: usize = 16;
const MAX_TRAILER_BYTES: usize = 1024;
const MAX_STDERR_BYTES: usize = 64 * 1024;

// ── Ref Naming & Validation ──────────────────────────────────────────────────

/// Computes the 16-char lowercase hex SHA-256 prefix for a branch name.
/// Strips an optional `refs/heads/` prefix so `"main"` and `"refs/heads/main"`
/// hash to the same namespace.
pub fn branch_proposal_hash(branch: &str) -> Result<String, CommandError> {
    let normalized = normalize_branch_name(branch)?;
    let digest = Sha256::digest(normalized.as_bytes());
    let mut hex = String::with_capacity(BRANCH_HASH_HEX_LEN);
    for byte in &digest[..BRANCH_HASH_HEX_LEN / 2] {
        use std::fmt::Write as _;
        let _ = write!(&mut hex, "{byte:02x}");
    }
    Ok(hex)
}

fn normalize_branch_name(branch: &str) -> Result<&str, CommandError> {
    let trimmed = branch.strip_prefix("refs/heads/").unwrap_or(branch);
    if trimmed.is_empty()
        || trimmed == "HEAD"
        || trimmed.starts_with('-')
        || trimmed.contains('\0')
        || trimmed.chars().any(char::is_control)
    {
        return Err(format!("Invalid branch name for proposal ref: {branch:?}").into());
    }
    validate_ref_name(&format!("refs/heads/{trimmed}"))?;
    Ok(trimmed)
}

/// True iff `oid` is a full 40-char (SHA-1) or 64-char (SHA-256) lowercase hex OID.
pub fn is_full_lowercase_oid(oid: &str) -> bool {
    matches!(oid.len(), 40 | 64) && oid.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f'))
}

/// Constructs and validates `refs/gitpulse/proposals/<sha256(branch)[:16]>/<source_commit_oid>`.
pub fn proposal_ref_name(branch: &str, source_commit_oid: &str) -> Result<String, CommandError> {
    if !is_full_lowercase_oid(source_commit_oid) {
        return Err(format!(
            "Proposal source commit OID must be full lowercase hex, got {source_commit_oid:?}"
        )
        .into());
    }
    let hash16 = branch_proposal_hash(branch)?;
    let ref_name = format!("{PROPOSAL_REF_PREFIX}{hash16}/{source_commit_oid}");
    validate_proposal_ref(&ref_name)?;
    Ok(ref_name)
}

/// Validates that `ref_name` strictly matches
/// `refs/gitpulse/proposals/<16-hex>/<40-or-64-hex>` and passes Git ref rules.
pub fn validate_proposal_ref(ref_name: &str) -> Result<(), CommandError> {
    validate_ref_name(ref_name)?;
    if !is_valid_proposal_ref(ref_name) {
        return Err(format!(
            "Invalid proposal ref {ref_name:?}: expected {PROPOSAL_REF_PREFIX}<16-hex>/<oid>"
        )
        .into());
    }
    Ok(())
}

/// Strict predicate for `refs/gitpulse/proposals/<16-lowercase-hex>/<40|64-lowercase-hex>`.
pub fn is_valid_proposal_ref(ref_name: &str) -> bool {
    let Some(rest) = ref_name.strip_prefix(PROPOSAL_REF_PREFIX) else {
        return false;
    };
    let Some((hash16, oid)) = rest.split_once('/') else {
        return false;
    };
    if oid.contains('/') {
        return false;
    }
    hash16.len() == BRANCH_HASH_HEX_LEN
        && hash16
            .bytes()
            .all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f'))
        && is_full_lowercase_oid(oid)
}

// ── Trailer Validation & Escaping ────────────────────────────────────────────

fn is_forbidden_trailer_char(c: char) -> bool {
    c.is_control()
        || matches!(
            c,
            '\n' | '\r' | '\t' | '\0' | '\u{0085}' | '\u{2028}' | '\u{2029}'
        )
}

/// Validates a trailer value as non-empty printable single-line UTF-8.
/// Rejects `\n`, `\r`, `\t`, NUL, all C0/C1 control characters, `U+0085`,
/// `U+2028`, and `U+2029`.
pub fn validate_trailer_value(value: &str) -> Result<&str, CommandError> {
    if value.trim().is_empty() {
        return Err("Trailer value must not be empty".into());
    }
    if value.len() > MAX_TRAILER_BYTES {
        return Err(format!("Trailer value exceeds {MAX_TRAILER_BYTES} bytes").into());
    }
    if value.chars().any(is_forbidden_trailer_char) {
        return Err("Trailer value contains forbidden control or line-separator characters".into());
    }
    Ok(value)
}

/// Hard-rejects invalid trailer values for resolution commits (`Graft-Resolved-By`, etc.).
pub fn validate_resolution_trailer_value(value: &str) -> Result<&str, CommandError> {
    validate_trailer_value(value)
}

/// Formats a trailer value for a proposal commit: valid printable single-line
/// values are kept verbatim; invalid values are escaped as printable single-line
/// quoted ASCII (analogous to Go's `strconv.QuoteToASCII`).
pub fn sanitize_proposal_trailer_value(value: &str) -> Result<String, CommandError> {
    if value.trim().is_empty() {
        return Err("Trailer value must not be empty".into());
    }
    if value.len() > MAX_TRAILER_BYTES * 4 {
        return Err("Trailer value exceeds maximum length".into());
    }
    if validate_trailer_value(value).is_ok() {
        return Ok(value.to_string());
    }
    let escaped: String = value.chars().flat_map(char::escape_default).collect();
    let bounded: String = escaped.chars().take(MAX_TRAILER_BYTES).collect();
    Ok(format!("\"{bounded}\""))
}

// ── ConflictFileKey & Verdict Cache ──────────────────────────────────────────

/// Content-addressed key for one conflicted file's 3-way merge inputs.
///
/// Deliberately excludes `tip` commit OID, `merge_base` commit OID, and commit
/// messages so cached resolutions survive unrelated commits landing on `main`.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
pub struct ConflictFileKey {
    pub path: String,
    pub base_oid: String,
    pub ours_oid: String,
    pub theirs_oid: String,
    pub base_mode: String,
    pub ours_mode: String,
    pub theirs_mode: String,
}

impl ConflictFileKey {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        path: impl Into<String>,
        base_oid: impl Into<String>,
        ours_oid: impl Into<String>,
        theirs_oid: impl Into<String>,
        base_mode: impl Into<String>,
        ours_mode: impl Into<String>,
        theirs_mode: impl Into<String>,
    ) -> Self {
        Self {
            path: path.into(),
            base_oid: base_oid.into(),
            ours_oid: ours_oid.into(),
            theirs_oid: theirs_oid.into(),
            base_mode: base_mode.into(),
            ours_mode: ours_mode.into(),
            theirs_mode: theirs_mode.into(),
        }
    }
}

/// Stage entry parsed from `git merge-tree --write-tree --no-messages -z`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct MergeStageEntry {
    pub stage: u8,
    pub mode: String,
    pub oid: String,
}

/// Extracted conflict-file metadata from a quarantined `merge-tree --write-tree`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ExtractedConflictFile {
    pub key: ConflictFileKey,
    pub stages: Vec<MergeStageEntry>,
    pub conflict_blob_bytes: Option<Vec<u8>>,
    pub conflict_blob_mode: Option<String>,
}

/// Quarantined 3-way merge inspection output (zero main-ODB or working-tree mutations).
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct QuarantinedMergeInspection {
    pub clean: bool,
    pub merged_tree_oid: String,
    pub conflicts: Vec<ExtractedConflictFile>,
}

/// Origin of a conflict resolution verdict.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum ResolutionOrigin {
    /// Deterministic fast-rule / explicit human resolution.
    Rule,
    /// Model-originated resolution (must always pass exact tree-OID verification).
    Model,
}

/// Outcome of resolving a single conflicted file.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum FileResolutionOutcome {
    Resolved,
    Proposed,
    Unresolved,
}

/// Per-file resolution verdict stored in [`ConflictVerdictCache`] or [`LandAnswer`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FileResolutionVerdict {
    pub key: ConflictFileKey,
    pub(crate) outcome: FileResolutionOutcome,
    pub(crate) origin: ResolutionOrigin,
    /// Non-empty when resolution is held (e.g. dual-model disagreement).
    pub hold_reason: Option<String>,
    /// Optional resolved blob mode and bytes cached for reuse.
    pub resolved_mode: Option<String>,
    pub resolved_bytes: Option<Vec<u8>>,
}

impl FileResolutionVerdict {
    pub fn resolved_by_rule(
        key: ConflictFileKey,
        resolved_mode: Option<String>,
        resolved_bytes: Option<Vec<u8>>,
    ) -> Self {
        Self {
            key,
            outcome: FileResolutionOutcome::Resolved,
            origin: ResolutionOrigin::Rule,
            hold_reason: None,
            resolved_mode,
            resolved_bytes,
        }
    }

    pub fn proposed_by_model(
        key: ConflictFileKey,
        resolved_mode: Option<String>,
        resolved_bytes: Option<Vec<u8>>,
    ) -> Self {
        Self {
            key,
            outcome: FileResolutionOutcome::Proposed,
            origin: ResolutionOrigin::Model,
            hold_reason: None,
            resolved_mode,
            resolved_bytes,
        }
    }

    pub fn resolved_by_model(
        key: ConflictFileKey,
        resolved_mode: Option<String>,
        resolved_bytes: Option<Vec<u8>>,
    ) -> Self {
        Self {
            key,
            outcome: FileResolutionOutcome::Resolved,
            origin: ResolutionOrigin::Model,
            hold_reason: None,
            resolved_mode,
            resolved_bytes,
        }
    }

    #[cfg_attr(not(test), allow(dead_code))]
    pub(crate) fn held(
        key: ConflictFileKey,
        origin: ResolutionOrigin,
        reason: impl Into<String>,
    ) -> Self {
        Self {
            key,
            outcome: FileResolutionOutcome::Proposed,
            origin,
            hold_reason: Some(reason.into()),
            resolved_mode: None,
            resolved_bytes: None,
        }
    }

    #[cfg_attr(not(test), allow(dead_code))]
    pub(crate) fn unresolved(key: ConflictFileKey, origin: ResolutionOrigin) -> Self {
        Self {
            key,
            outcome: FileResolutionOutcome::Unresolved,
            origin,
            hold_reason: None,
            resolved_mode: None,
            resolved_bytes: None,
        }
    }
}

/// Reusable conflict resolution and tree-OID verification cache across tip moves.
#[derive(Debug, Default, Clone)]
pub struct ConflictVerdictCache {
    verdicts: HashMap<ConflictFileKey, FileResolutionVerdict>,
    current_keys: HashSet<ConflictFileKey>,
    verified_trees: HashSet<String>,
}

impl ConflictVerdictCache {
    pub fn new() -> Self {
        Self::default()
    }

    /// Records a file verdict keyed by its [`ConflictFileKey`].
    pub fn insert_verdict(&mut self, verdict: FileResolutionVerdict) {
        self.verdicts.insert(verdict.key.clone(), verdict);
    }

    /// Updates the set of active conflict keys for the latest branch tip so
    /// stale verdicts for files modified on `main` are ignored while unchanged
    /// keys remain reusable.
    pub fn set_current(&mut self, keys: &[ConflictFileKey]) {
        self.current_keys.clear();
        self.current_keys.extend(keys.iter().cloned());
    }

    /// Returns the cached verdict iff `key` is active in `current_keys` (or when
    /// `set_current` was not restricted, wait: if `set_current` was called, only
    /// current keys are returned; callers always call `set_current` for the new tip).
    pub fn get_current(&self, key: &ConflictFileKey) -> Option<&FileResolutionVerdict> {
        if !self.current_keys.contains(key) {
            return None;
        }
        self.verdicts.get(key)
    }

    /// Looks up a cached verdict directly by `ConflictFileKey` regardless of `current_keys`.
    pub fn get(&self, key: &ConflictFileKey) -> Option<&FileResolutionVerdict> {
        self.verdicts.get(key)
    }

    /// Records that `tree_oid` (exact Git tree object ID, never a commit SHA)
    /// passed verification.
    pub fn mark_verified_tree(&mut self, tree_oid: &str) -> Result<(), CommandError> {
        if !is_full_lowercase_oid(tree_oid) {
            return Err(format!("Invalid tree OID for verification cache: {tree_oid:?}").into());
        }
        self.verified_trees.insert(tree_oid.to_string());
        Ok(())
    }

    pub fn is_tree_verified(&self, tree_oid: &str) -> bool {
        is_full_lowercase_oid(tree_oid) && self.verified_trees.contains(tree_oid)
    }
}

// ── decide_land Safety Gate ──────────────────────────────────────────────────

/// Full resolution answer evaluated by [`decide_land`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LandAnswer {
    pub files: Vec<FileResolutionVerdict>,
}

/// Inputs to the [`decide_land`] safety gate.
#[derive(Debug, Clone, Copy)]
pub struct LandDecisionInput<'a> {
    pub answer: Option<&'a LandAnswer>,
    pub candidate_tree_oid: &'a str,
    pub verified_tree_oid: Option<&'a str>,
    pub verify_required: bool,
}

/// Reason [`decide_land`] blocked auto-landing.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum LandBlockReason {
    MissingAnswer,
    HeldDisagreement { path: String, reason: String },
    UnverifiedTree,
    TreeMismatch { candidate: String, verified: String },
    InvalidCandidateTree,
    UnresolvedFile { path: String },
    ProposedWithoutProposalFlag { path: String },
}

/// Decision returned by [`decide_land`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LandDecision {
    pub allowed: bool,
    pub proposes: bool,
    pub(crate) reason: Option<LandBlockReason>,
}

impl LandDecision {
    pub fn reason_message(&self) -> Option<String> {
        self.reason.as_ref().map(|r| match r {
            LandBlockReason::MissingAnswer => "no conflict resolution answer provided".to_string(),
            LandBlockReason::HeldDisagreement { path, reason } => {
                format!("resolution held for {path}: {reason}")
            }
            LandBlockReason::UnverifiedTree => {
                "resolved tree has not passed verification".to_string()
            }
            LandBlockReason::TreeMismatch {
                candidate,
                verified,
            } => format!("verified tree {verified} does not match candidate tree {candidate}"),
            LandBlockReason::InvalidCandidateTree => {
                "candidate tree OID is missing or invalid".to_string()
            }
            LandBlockReason::UnresolvedFile { path } => {
                format!("conflict remains unresolved for {path}")
            }
            LandBlockReason::ProposedWithoutProposalFlag { path } => {
                format!("file {path} has proposed outcome without proposal gate")
            }
        })
    }
}

fn answer_has_holds(answer: &LandAnswer) -> Option<(&str, &str)> {
    answer.files.iter().find_map(|f| {
        f.hold_reason
            .as_deref()
            .map(str::trim)
            .filter(|r| !r.is_empty())
            .map(|r| (f.key.path.as_str(), r))
    })
}

/// Fail-safe classification: ANY file not explicitly marked `ResolutionOrigin::Rule`
/// (or having `FileResolutionOutcome::Proposed`) counts as model-originated and
/// requires exact tree-OID verification.
fn answer_proposes(answer: &LandAnswer) -> bool {
    answer
        .files
        .iter()
        .any(|f| f.origin != ResolutionOrigin::Rule || f.outcome == FileResolutionOutcome::Proposed)
}

/// Safety gate governing whether a conflict resolution may auto-land:
///
/// 1. `answer == None` -> blocked.
/// 2. Any held file (non-empty `hold_reason`, e.g., dual-model disagreement) ->
///    ALWAYS blocked, even when `verified_tree_oid == Some(candidate_tree_oid)`.
/// 3. Fail-safe `proposes`: any file not explicitly `OriginRule` (or marked
///    `Proposed`) requires `verified_tree_oid == Some(candidate_tree_oid)`.
/// 4. When `proposes || verify_required`, exact tree-OID equality is mandatory.
/// 5. Every file must be `Resolved` or (only when `proposes`) `Proposed`.
pub fn decide_land(input: &LandDecisionInput<'_>) -> LandDecision {
    let Some(answer) = input.answer else {
        return LandDecision {
            allowed: false,
            proposes: false,
            reason: Some(LandBlockReason::MissingAnswer),
        };
    };

    if let Some((path, reason)) = answer_has_holds(answer) {
        return LandDecision {
            allowed: false,
            proposes: answer_proposes(answer),
            reason: Some(LandBlockReason::HeldDisagreement {
                path: path.to_string(),
                reason: reason.to_string(),
            }),
        };
    }

    let proposes = answer_proposes(answer);

    if proposes || input.verify_required {
        if !is_full_lowercase_oid(input.candidate_tree_oid) {
            return LandDecision {
                allowed: false,
                proposes,
                reason: Some(LandBlockReason::InvalidCandidateTree),
            };
        }
        let Some(verified) = input.verified_tree_oid.filter(|v| !v.is_empty()) else {
            return LandDecision {
                allowed: false,
                proposes,
                reason: Some(LandBlockReason::UnverifiedTree),
            };
        };
        if !is_full_lowercase_oid(verified) || verified != input.candidate_tree_oid {
            return LandDecision {
                allowed: false,
                proposes,
                reason: Some(LandBlockReason::TreeMismatch {
                    candidate: input.candidate_tree_oid.to_string(),
                    verified: verified.to_string(),
                }),
            };
        }
    }

    for file in &answer.files {
        match file.outcome {
            FileResolutionOutcome::Resolved => {}
            FileResolutionOutcome::Proposed if proposes => {}
            FileResolutionOutcome::Proposed => {
                return LandDecision {
                    allowed: false,
                    proposes,
                    reason: Some(LandBlockReason::ProposedWithoutProposalFlag {
                        path: file.key.path.clone(),
                    }),
                };
            }
            FileResolutionOutcome::Unresolved => {
                return LandDecision {
                    allowed: false,
                    proposes,
                    reason: Some(LandBlockReason::UnresolvedFile {
                        path: file.key.path.clone(),
                    }),
                };
            }
        }
    }

    LandDecision {
        allowed: true,
        proposes,
        reason: None,
    }
}

// ── Proposal Data Types ──────────────────────────────────────────────────────

/// A single proposed file to persist inside an ODB proposal commit.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProposedFileInput {
    pub path: String,
    /// Git file mode: `"100644"`, `"100755"`, or `"120000"`.
    pub mode: String,
    pub bytes: Vec<u8>,
}

/// Request to create and persist an ODB conflict proposal ref.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CreateProposalRequest {
    pub branch: String,
    pub tip_oid: String,
    pub source_commit_oid: String,
    pub resolver: String,
    pub requested_by: String,
    pub files: Vec<ProposedFileInput>,
    /// Optional explicit CAS expected old OID. When `None`, reads the current
    /// exact ref (or all-zeros when absent) before atomic CAS update.
    pub expected_old_oid: Option<String>,
}

/// Summary metadata of one stored proposal ref.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProposalSummary {
    pub ref_name: String,
    pub branch_hash: String,
    pub source_commit_oid: String,
    pub proposal_commit_oid: String,
    pub tree_oid: String,
    pub tip_oid: String,
    pub committer_unix: i64,
    pub resolver: String,
    pub requested_by: String,
}

/// Result of listing proposals, including count of skipped non-commit/symbolic refs.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProposalListReport {
    pub proposals: Vec<ProposalSummary>,
    pub skipped: usize,
}

/// Full hydrated ODB proposal package including proposed file contents.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProposalPackage {
    pub summary: ProposalSummary,
    pub files: Vec<ProposedFileEntry>,
}

/// One proposed file stored inside a proposal commit's sparse tree.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProposedFileEntry {
    pub path: String,
    pub mode: String,
    pub blob_oid: String,
    pub bytes: Vec<u8>,
}

/// Result of one [`ProposalSweeper`] pass.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct SweepReport {
    pub deleted_expired: usize,
    pub deleted_excess: usize,
    pub kept: usize,
    pub skipped: usize,
    pub cas_conflicts: usize,
    pub already_gone: usize,
}

// ── Quarantined Git Plumbing Helpers ─────────────────────────────────────────

fn quarantine_alternates(repo: &Path) -> Result<std::ffi::OsString, CommandError> {
    let objects = resolve_git_common_dir(repo)?.join("objects");
    if !objects.is_absolute() || !objects.is_dir() {
        return Err(format!(
            "{} is not the repository's object directory",
            objects.display()
        )
        .into());
    }
    if cfg!(windows) && objects.to_string_lossy().contains(';') {
        return Err(format!(
            "alternates path {} holds the list separator ';'",
            objects.display()
        )
        .into());
    }
    std::env::join_paths([&objects])
        .map_err(|e| format!("Failed to construct alternates path: {e}").into())
}

fn read_bounded_stream<R: Read>(mut reader: R, cap: usize) -> Result<(Vec<u8>, bool), String> {
    let mut buf = Vec::new();
    let mut chunk = [0u8; 8192];
    loop {
        match reader.read(&mut chunk) {
            Ok(0) => return Ok((buf, false)),
            Ok(n) => {
                if buf.len().saturating_add(n) > cap {
                    let take = cap.saturating_sub(buf.len());
                    buf.extend_from_slice(&chunk[..take]);
                    return Ok((buf, true));
                }
                buf.extend_from_slice(&chunk[..n]);
            }
            Err(e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
            Err(e) => return Err(e.to_string()),
        }
    }
}

fn run_quarantined_with_stdin(
    repo: &Path,
    quarantine: &ObjectQuarantine,
    alternates: &OsStr,
    args: &[&str],
    stdin_bytes: &[u8],
    timeout: Duration,
) -> Result<Vec<u8>, CommandError> {
    let mut child =
        git_spawn_quarantined_streaming(repo, args, quarantine.path(), alternates, timeout)?;
    let Some(mut stdin) = child.take_stdin() else {
        return Err("Quarantined git command missing stdin pipe".into());
    };
    let Some(stdout) = child.take_stdout() else {
        return Err("Quarantined git command missing stdout pipe".into());
    };
    let stderr = child.take_stderr();

    let (write_res, stdout_res, stderr_res) = std::thread::scope(|scope| {
        let writer = scope.spawn(move || -> Result<(), String> {
            let res = stdin.write_all(stdin_bytes).and_then(|()| stdin.flush());
            drop(stdin);
            res.map_err(|e| e.to_string())
        });
        let err_reader = scope.spawn(move || -> Vec<u8> {
            match stderr {
                Some(pipe) => read_bounded_stream(pipe, MAX_STDERR_BYTES)
                    .map(|(b, _)| b)
                    .unwrap_or_default(),
                None => Vec::new(),
            }
        });
        let out_res = read_bounded_stream(stdout, MAX_OUTPUT_BYTES);
        let write_res = writer
            .join()
            .unwrap_or_else(|_| Err("stdin writer panicked".to_string()));
        let err_bytes = err_reader.join().unwrap_or_default();
        (write_res, out_res, err_bytes)
    });

    let status = child
        .wait_and_reap()
        .map_err(|e| format!("Failed waiting for quarantined git command: {e}"))?;
    let (out_bytes, truncated) =
        stdout_res.map_err(|e| format!("Failed reading quarantined git stdout: {e}"))?;
    if truncated {
        return Err("Quarantined git output exceeded cap".into());
    }
    if !status.success() {
        let err_text = String::from_utf8_lossy(&stderr_res).trim().to_string();
        let write_note = write_res.err().unwrap_or_default();
        return Err(format!(
            "Quarantined git {:?} failed (exit {:?}): {} {}",
            args.first().copied().unwrap_or("git"),
            status.code(),
            err_text,
            write_note
        )
        .trim()
        .to_string()
        .into());
    }
    write_res.map_err(|e| format!("Failed writing stdin to quarantined git command: {e}"))?;
    Ok(out_bytes)
}

// ── Path & Tree Building ─────────────────────────────────────────────────────

fn validate_proposed_path(path: &str) -> Result<Vec<&str>, CommandError> {
    if path.is_empty() || path.contains('\0') || path.starts_with('/') || path.ends_with('/') {
        return Err(format!("Invalid proposed file path: {path:?}").into());
    }
    let mut segments = Vec::new();
    for component in Path::new(path).components() {
        match component {
            Component::Normal(name) => {
                if crate::engine::git_cli::is_git_metadata_component(name) {
                    return Err(format!("Git metadata path cannot be proposed: {path:?}").into());
                }
                let text = name
                    .to_str()
                    .ok_or_else(|| format!("Proposed path is not valid UTF-8: {path:?}"))?;
                if text.is_empty() || text == "." || text == ".." || text.contains('\\') {
                    return Err(format!("Invalid proposed path segment in {path:?}").into());
                }
                segments.push(text);
            }
            Component::CurDir
            | Component::ParentDir
            | Component::RootDir
            | Component::Prefix(_) => {
                return Err(format!("Proposed path must be normalized relative: {path:?}").into());
            }
        }
    }
    if segments.is_empty() {
        return Err(format!("Empty proposed path: {path:?}").into());
    }
    Ok(segments)
}

#[derive(Default)]
struct SparseTreeNode {
    files: BTreeMap<String, (String, String)>,
    dirs: BTreeMap<String, SparseTreeNode>,
}

impl SparseTreeNode {
    fn insert(
        &mut self,
        segments: &[&str],
        mode: String,
        blob_oid: String,
        full_path: &str,
    ) -> Result<(), CommandError> {
        let Some((head, tail)) = segments.split_first() else {
            return Err(format!("Empty path segments for {full_path:?}").into());
        };
        if tail.is_empty() {
            if self.dirs.contains_key(*head) || self.files.contains_key(*head) {
                return Err(format!(
                    "Duplicate or file/directory colliding proposed path: {full_path:?}"
                )
                .into());
            }
            self.files.insert((*head).to_string(), (mode, blob_oid));
            Ok(())
        } else {
            if self.files.contains_key(*head) {
                return Err(
                    format!("File/directory collision in proposed path: {full_path:?}").into(),
                );
            }
            let child = self.dirs.entry((*head).to_string()).or_default();
            child.insert(tail, mode, blob_oid, full_path)
        }
    }

    fn write_tree_quarantined(
        &self,
        repo: &Path,
        quarantine: &ObjectQuarantine,
        alternates: &OsStr,
        created_oids: &mut Vec<String>,
    ) -> Result<String, CommandError> {
        let mut mktree_input = Vec::new();
        for (name, (mode, blob_oid)) in &self.files {
            mktree_input.extend_from_slice(format!("{mode} blob {blob_oid}\t{name}\0").as_bytes());
        }
        for (name, subtree) in &self.dirs {
            let subtree_oid =
                subtree.write_tree_quarantined(repo, quarantine, alternates, created_oids)?;
            mktree_input
                .extend_from_slice(format!("040000 tree {subtree_oid}\t{name}\0").as_bytes());
        }
        let out = run_quarantined_with_stdin(
            repo,
            quarantine,
            alternates,
            &["mktree", "-z"],
            &mktree_input,
            DEFAULT_TIMEOUT,
        )?;
        let tree_oid = String::from_utf8_lossy(&out).trim().to_string();
        if !is_full_lowercase_oid(&tree_oid) {
            return Err(format!("mktree returned invalid tree OID: {tree_oid:?}").into());
        }
        created_oids.push(tree_oid.clone());
        Ok(tree_oid)
    }
}

fn verify_commit_oid(repo: &Path, oid: &str, label: &str) -> Result<(), CommandError> {
    if !is_full_lowercase_oid(oid) {
        return Err(format!("{label} must be a full lowercase commit OID, got {oid:?}").into());
    }
    let spec = format!("{oid}^{{commit}}");
    let resolved = git_text(repo, &["rev-parse", "--verify", "--end-of-options", &spec])?;
    if resolved.trim() != oid {
        return Err(format!(
            "{label} {oid} did not resolve to itself (got {:?})",
            resolved.trim()
        )
        .into());
    }
    Ok(())
}

/// Reads an exact ref target without Git DWIM fallback. Returns `None` if absent.
/// Rejects symbolic refs and non-commit targets.
fn read_exact_proposal_ref(
    repo: &Path,
    ref_name: &str,
) -> Result<Option<(String, i64)>, CommandError> {
    validate_proposal_ref(ref_name)?;
    let raw = git_text(
        repo,
        &[
            "for-each-ref",
            "--format=%(refname)%00%(objectname)%00%(objecttype)%00%(symref)%00%(committerdate:unix)",
            ref_name,
        ],
    )?;
    for line in raw.lines() {
        if line.is_empty() {
            continue;
        }
        let mut parts = line.split('\0');
        let found_ref = parts.next().unwrap_or_default();
        if found_ref != ref_name {
            continue;
        }
        let oid = parts.next().unwrap_or_default().trim();
        let obj_type = parts.next().unwrap_or_default().trim();
        let symref = parts.next().unwrap_or_default().trim();
        let committer_unix_str = parts.next().unwrap_or_default().trim();
        if !symref.is_empty() {
            return Err(format!("Refusing symbolic proposal ref {ref_name} -> {symref}").into());
        }
        if obj_type != "commit" {
            return Err(format!(
                "Proposal ref {ref_name} targets non-commit object type {obj_type:?}"
            )
            .into());
        }
        if !is_full_lowercase_oid(oid) {
            return Err(format!("Proposal ref {ref_name} holds invalid OID {oid:?}").into());
        }
        let committer_unix = committer_unix_str
            .parse::<i64>()
            .map_err(|_| format!("Invalid committer timestamp on {ref_name}"))?;
        return Ok(Some((oid.to_string(), committer_unix)));
    }
    Ok(None)
}

// ── Create / Read / List / Drop Proposals ────────────────────────────────────

/// Builds a proposal commit in a **separate [`ObjectQuarantine`]** holding ONLY
/// the proposed file blobs at their repo paths, 2 parents `[tip_oid, source_commit_oid]`,
/// server `now` timestamps, and validated/escaped single-line trailers (`Resolver:`,
/// `Requested-By:`), migrates the objects to the repository's ODB, and writes
/// `refs/gitpulse/proposals/<sha256(branch)[:16]>/<source_commit_oid>` atomically
/// via CAS (`update-ref --no-deref`).
pub fn create_proposal(
    repo: &Path,
    request: &CreateProposalRequest,
) -> Result<ProposalSummary, CommandError> {
    if request.files.is_empty() {
        return Err("Proposal must contain at least one proposed file".into());
    }
    if request.files.len() > MAX_PROPOSAL_FILES {
        return Err(format!("Proposal exceeds maximum file count ({MAX_PROPOSAL_FILES})").into());
    }
    verify_commit_oid(repo, &request.tip_oid, "tip_oid")?;
    verify_commit_oid(repo, &request.source_commit_oid, "source_commit_oid")?;

    let ref_name = proposal_ref_name(&request.branch, &request.source_commit_oid)?;
    let branch_hash = branch_proposal_hash(&request.branch)?;
    let resolver = sanitize_proposal_trailer_value(&request.resolver)?;
    let requested_by = sanitize_proposal_trailer_value(&request.requested_by)?;

    let mut parsed_files = Vec::with_capacity(request.files.len());
    for file in &request.files {
        if !matches!(file.mode.as_str(), "100644" | "100755" | "120000") {
            return Err(format!(
                "Unsupported Git mode {:?} for proposed file {:?}",
                file.mode, file.path
            )
            .into());
        }
        if file.bytes.len() > MAX_PROPOSAL_FILE_BYTES {
            return Err(format!(
                "Proposed file {:?} exceeds {MAX_PROPOSAL_FILE_BYTES} bytes",
                file.path
            )
            .into());
        }
        let segments = validate_proposed_path(&file.path)?;
        parsed_files.push((file, segments));
    }

    // Read observed current ref before quarantine/migration if caller did not supply CAS expected_old.
    let expected_old = match &request.expected_old_oid {
        Some(expected) => {
            if !is_full_lowercase_oid(expected)
                && expected != absent_oid_for_len(request.source_commit_oid.len())
            {
                return Err(format!("Invalid expected_old_oid: {expected:?}").into());
            }
            expected.clone()
        }
        None => match read_exact_proposal_ref(repo, &ref_name)? {
            Some((oid, _)) => oid,
            None => absent_oid_for_len(request.source_commit_oid.len()).to_string(),
        },
    };

    // Build all proposal objects inside a dedicated ObjectQuarantine.
    let quarantine = ObjectQuarantine::new(repo).map_err(|e| CommandError::plain(e.to_string()))?;
    let alternates = quarantine_alternates(repo)?;
    let mut created_oids: Vec<String> = Vec::new();
    let mut root_node = SparseTreeNode::default();

    for (file, segments) in &parsed_files {
        let blob_out = run_quarantined_with_stdin(
            repo,
            &quarantine,
            &alternates,
            &["hash-object", "-w", "--no-filters", "--stdin"],
            &file.bytes,
            DEFAULT_TIMEOUT,
        )?;
        let blob_oid = String::from_utf8_lossy(&blob_out).trim().to_string();
        if !is_full_lowercase_oid(&blob_oid) {
            return Err(format!("hash-object returned invalid OID: {blob_oid:?}").into());
        }
        created_oids.push(blob_oid.clone());
        root_node.insert(segments, file.mode.clone(), blob_oid, &file.path)?;
    }

    let tree_oid =
        root_node.write_tree_quarantined(repo, &quarantine, &alternates, &mut created_oids)?;

    let message = format!(
        "gitpulse: proposals for landing {} onto {}\n\nResolver: {resolver}\nRequested-By: {requested_by}\n",
        request.source_commit_oid, request.tip_oid
    );

    let mut commit_args: Vec<&str> = SNAPSHOT_IDENTITY_ARGS.to_vec();
    commit_args.extend(["commit-tree", &tree_oid, "-p", &request.tip_oid]);
    if request.source_commit_oid != request.tip_oid {
        commit_args.extend(["-p", &request.source_commit_oid]);
    }
    commit_args.extend(["-F", "-"]);

    // `git_spawn_quarantined_streaming` strips GIT_AUTHOR_DATE and
    // GIT_COMMITTER_DATE via `pin_git_environment`, so Git records server `now`.
    let commit_out = run_quarantined_with_stdin(
        repo,
        &quarantine,
        &alternates,
        &commit_args,
        message.as_bytes(),
        DEFAULT_TIMEOUT,
    )?;
    let proposal_commit_oid = String::from_utf8_lossy(&commit_out).trim().to_string();
    if !is_full_lowercase_oid(&proposal_commit_oid) {
        return Err(format!("commit-tree returned invalid OID: {proposal_commit_oid:?}").into());
    }
    created_oids.push(proposal_commit_oid.clone());

    // Migrate objects from quarantine into the repository ODB before CAS update-ref.
    let mut oid_list = String::new();
    for oid in &created_oids {
        oid_list.push_str(oid);
        oid_list.push('\n');
    }
    let pack_bytes = run_quarantined_with_stdin(
        repo,
        &quarantine,
        &alternates,
        &["pack-objects", "--stdout", "-q"],
        oid_list.as_bytes(),
        DEFAULT_TIMEOUT,
    )?;
    let unpack_run = git_captured_with_stdin(repo, &["unpack-objects", "-q"], &pack_bytes)?
        .require_complete("git unpack-objects")?;
    if !unpack_run.success {
        return Err(format!(
            "Failed to migrate proposal objects from quarantine: {}",
            String::from_utf8_lossy(&unpack_run.stderr).trim()
        )
        .into());
    }
    drop(quarantine);

    // Atomic CAS ref update.
    let update_run = git_captured(
        repo,
        &[
            "update-ref",
            "--no-deref",
            &ref_name,
            &proposal_commit_oid,
            &expected_old,
        ],
    )?
    .require_complete("git update-ref")?;
    if !update_run.success {
        return Err(format!(
            "CAS ref update failed for {ref_name} (expected {expected_old}): {}",
            String::from_utf8_lossy(&update_run.stderr).trim()
        )
        .into());
    }

    let (_, committer_unix) = read_exact_proposal_ref(repo, &ref_name)?
        .ok_or_else(|| format!("Proposal ref {ref_name} missing immediately after write"))?;

    Ok(ProposalSummary {
        ref_name,
        branch_hash,
        source_commit_oid: request.source_commit_oid.clone(),
        proposal_commit_oid,
        tree_oid,
        tip_oid: request.tip_oid.clone(),
        committer_unix,
        resolver,
        requested_by,
    })
}

fn parse_proposal_commit(
    repo: &Path,
    ref_name: &str,
    proposal_commit_oid: &str,
    committer_unix: i64,
) -> Result<ProposalSummary, CommandError> {
    let raw = git_text(repo, &["cat-file", "commit", proposal_commit_oid])?;
    let (header, body) = raw
        .split_once("\n\n")
        .ok_or_else(|| format!("Proposal commit {proposal_commit_oid} has no message body"))?;

    let mut tree_oid: Option<String> = None;
    let mut parents: Vec<String> = Vec::new();
    for line in header.lines() {
        if let Some(rest) = line.strip_prefix("tree ") {
            tree_oid = Some(rest.trim().to_string());
        } else if let Some(rest) = line.strip_prefix("parent ") {
            parents.push(rest.trim().to_string());
        }
    }
    let tree_oid = tree_oid
        .filter(|oid| is_full_lowercase_oid(oid))
        .ok_or_else(|| format!("Proposal commit {proposal_commit_oid} missing valid tree"))?;
    let tip_oid = parents
        .first()
        .filter(|oid| is_full_lowercase_oid(oid))
        .cloned()
        .ok_or_else(|| format!("Proposal commit {proposal_commit_oid} missing tip parent"))?;

    let rest = ref_name
        .strip_prefix(PROPOSAL_REF_PREFIX)
        .ok_or_else(|| format!("Invalid proposal ref {ref_name:?}"))?;
    let (branch_hash, source_commit_oid_from_ref) = rest
        .split_once('/')
        .ok_or_else(|| format!("Invalid proposal ref {ref_name:?}"))?;

    let source_commit_oid = parents
        .get(1)
        .filter(|oid| is_full_lowercase_oid(oid))
        .cloned()
        .unwrap_or_else(|| source_commit_oid_from_ref.to_string());

    let mut resolver = String::new();
    let mut requested_by = String::new();
    for line in body.lines() {
        if let Some(val) = line.strip_prefix("Resolver: ") {
            if resolver.is_empty() {
                resolver = val.trim().to_string();
            }
        } else if let Some(val) = line.strip_prefix("Requested-By: ") {
            if requested_by.is_empty() {
                requested_by = val.trim().to_string();
            }
        }
    }

    Ok(ProposalSummary {
        ref_name: ref_name.to_string(),
        branch_hash: branch_hash.to_string(),
        source_commit_oid,
        proposal_commit_oid: proposal_commit_oid.to_string(),
        tree_oid,
        tip_oid,
        committer_unix,
        resolver,
        requested_by,
    })
}

/// Reads a proposal package by `(branch, source_commit_oid)`.
/// Returns `Ok(None)` when the proposal ref does not exist.
pub fn read_proposal(
    repo: &Path,
    branch: &str,
    source_commit_oid: &str,
) -> Result<Option<ProposalPackage>, CommandError> {
    let ref_name = proposal_ref_name(branch, source_commit_oid)?;
    read_proposal_by_ref(repo, &ref_name)
}

/// Reads a proposal package by exact full proposal ref (`refs/gitpulse/proposals/<hash16>/<oid>`).
pub fn read_proposal_by_ref(
    repo: &Path,
    ref_name: &str,
) -> Result<Option<ProposalPackage>, CommandError> {
    let Some((proposal_commit_oid, committer_unix)) = read_exact_proposal_ref(repo, ref_name)?
    else {
        return Ok(None);
    };
    let summary = parse_proposal_commit(repo, ref_name, &proposal_commit_oid, committer_unix)?;

    let ls_run = git_captured(repo, &["ls-tree", "-r", "-z", &summary.tree_oid])?
        .require_complete("git ls-tree")?;
    if !ls_run.success {
        return Err(format!(
            "Failed reading proposal tree {}: {}",
            summary.tree_oid,
            String::from_utf8_lossy(&ls_run.stderr).trim()
        )
        .into());
    }

    let mut files = Vec::new();
    for record in ls_run.stdout.split(|&b| b == 0).filter(|r| !r.is_empty()) {
        if files.len() >= MAX_PROPOSAL_FILES {
            return Err(format!(
                "Proposal tree {} exceeds maximum file count ({MAX_PROPOSAL_FILES})",
                summary.tree_oid
            )
            .into());
        }
        let Some(tab) = record.iter().position(|&b| b == b'\t') else {
            return Err("Malformed ls-tree record in proposal tree".into());
        };
        let meta = std::str::from_utf8(&record[..tab])
            .map_err(|e| format!("Invalid UTF-8 in proposal ls-tree header: {e}"))?;
        let path = std::str::from_utf8(&record[tab + 1..])
            .map_err(|e| format!("Invalid UTF-8 in proposal file path: {e}"))?;
        let mut fields = meta.split_whitespace();
        let mode = fields.next().unwrap_or_default();
        let obj_type = fields.next().unwrap_or_default();
        let blob_oid = fields.next().unwrap_or_default();
        if obj_type != "blob" || !is_full_lowercase_oid(blob_oid) {
            return Err(format!(
                "Invalid proposal tree entry for {path:?}: type={obj_type:?}, oid={blob_oid:?}"
            )
            .into());
        }
        validate_proposed_path(path)?;
        let size_text = git_text(repo, &["cat-file", "-s", blob_oid])?;
        let size: usize = size_text
            .trim()
            .parse()
            .map_err(|_| format!("Invalid blob size for {blob_oid}"))?;
        if size > MAX_PROPOSAL_FILE_BYTES {
            return Err(format!(
                "Proposal blob {blob_oid} ({path}) exceeds {MAX_PROPOSAL_FILE_BYTES} bytes"
            )
            .into());
        }
        let bytes = crate::engine::git_cli::git(repo, &["cat-file", "blob", blob_oid])?;
        files.push(ProposedFileEntry {
            path: path.to_string(),
            mode: mode.to_string(),
            blob_oid: blob_oid.to_string(),
            bytes,
        });
    }

    Ok(Some(ProposalPackage { summary, files }))
}

/// Lists proposals under `refs/gitpulse/proposals/` (or for a single `branch`
/// when `Some`), sorted newest first. Skips symbolic refs or non-commit targets
/// cleanly and counts them in `skipped`.
pub fn list_proposals_report(
    repo: &Path,
    branch: Option<&str>,
) -> Result<ProposalListReport, CommandError> {
    let prefix = match branch {
        Some(b) => format!("{PROPOSAL_REF_PREFIX}{}/", branch_proposal_hash(b)?),
        None => PROPOSAL_REF_PREFIX.to_string(),
    };
    let raw = git_text(
        repo,
        &[
            "for-each-ref",
            "--format=%(refname)%00%(objectname)%00%(objecttype)%00%(symref)%00%(committerdate:unix)",
            &prefix,
        ],
    )?;

    let mut proposals = Vec::new();
    let mut skipped = 0usize;
    for line in raw.lines() {
        if line.is_empty() {
            continue;
        }
        let mut parts = line.split('\0');
        let ref_name = parts.next().unwrap_or_default();
        let oid = parts.next().unwrap_or_default().trim();
        let obj_type = parts.next().unwrap_or_default().trim();
        let symref = parts.next().unwrap_or_default().trim();
        let committer_unix_str = parts.next().unwrap_or_default().trim();

        if !symref.is_empty()
            || obj_type != "commit"
            || !is_valid_proposal_ref(ref_name)
            || !is_full_lowercase_oid(oid)
        {
            skipped += 1;
            continue;
        }
        let Ok(committer_unix) = committer_unix_str.parse::<i64>() else {
            skipped += 1;
            continue;
        };
        match parse_proposal_commit(repo, ref_name, oid, committer_unix) {
            Ok(summary) => proposals.push(summary),
            Err(_) => skipped += 1,
        }
    }

    proposals.sort_by(|a, b| {
        b.committer_unix
            .cmp(&a.committer_unix)
            .then_with(|| a.ref_name.cmp(&b.ref_name))
    });
    Ok(ProposalListReport { proposals, skipped })
}

/// Lists proposals under `refs/gitpulse/proposals/` (or for `branch`), newest first.
pub fn list_proposals(
    repo: &Path,
    branch: Option<&str>,
) -> Result<Vec<ProposalSummary>, CommandError> {
    Ok(list_proposals_report(repo, branch)?.proposals)
}

/// Idempotently drops a proposal ref on successful land via CAS (`OldOID = observed,
/// NewOID = ZeroOID`). Returns `Ok(true)` if deleted, `Ok(false)` if already absent
/// (or concurrently deleted by a sweep). Refuses symbolic/non-commit refs or a
/// concurrent overwrite to a different OID.
pub fn drop_proposal(
    repo: &Path,
    branch: &str,
    source_commit_oid: &str,
) -> Result<bool, CommandError> {
    let ref_name = proposal_ref_name(branch, source_commit_oid)?;
    drop_proposal_by_ref(repo, &ref_name)
}

/// Idempotent CAS drop of an exact proposal ref (`refs/gitpulse/proposals/<hash16>/<oid>`).
pub fn drop_proposal_by_ref(repo: &Path, ref_name: &str) -> Result<bool, CommandError> {
    let Some((observed_oid, _)) = read_exact_proposal_ref(repo, ref_name)? else {
        return Ok(false);
    };
    let run = git_captured(
        repo,
        &["update-ref", "-d", "--no-deref", ref_name, &observed_oid],
    )?
    .require_complete("git update-ref -d")?;
    if run.success {
        return Ok(true);
    }
    // Idempotent if a concurrent sweeper deleted it before our delete landed.
    if read_exact_proposal_ref(repo, ref_name)?.is_none() {
        return Ok(false);
    }
    Err(format!(
        "CAS drop failed for {ref_name} (expected {observed_oid}): {}",
        String::from_utf8_lossy(&run.stderr).trim()
    )
    .into())
}

// ── Coalescing TTL/Cap ProposalSweeper ───────────────────────────────────────

#[derive(Default)]
struct BranchSweepCoordState {
    arrivals: u64,
    covered: u64,
    running: bool,
    last_error: Option<String>,
}

struct BranchSweepCoordinator {
    state: Mutex<BranchSweepCoordState>,
    cond: Condvar,
}

impl BranchSweepCoordinator {
    fn new() -> Self {
        Self {
            state: Mutex::new(BranchSweepCoordState::default()),
            cond: Condvar::new(),
        }
    }
}

type BranchSweepMap = Mutex<HashMap<(PathBuf, String), Arc<BranchSweepCoordinator>>>;

fn coordinator_for(repo: &Path, branch_hash: &str) -> Arc<BranchSweepCoordinator> {
    static COORDS: OnceLock<BranchSweepMap> = OnceLock::new();
    let canon = std::fs::canonicalize(repo).unwrap_or_else(|_| repo.to_path_buf());
    let map = COORDS.get_or_init(|| Mutex::new(HashMap::new()));
    let mut guard = map
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    guard
        .entry((canon, branch_hash.to_string()))
        .or_insert_with(|| Arc::new(BranchSweepCoordinator::new()))
        .clone()
}

/// Bounded TTL + per-branch cap sweeper for `refs/gitpulse/proposals/<hash16>/`.
#[derive(Debug, Clone)]
pub struct ProposalSweeper {
    pub ttl: Duration,
    pub max_per_branch: usize,
    pub timeout: Duration,
}

impl Default for ProposalSweeper {
    fn default() -> Self {
        Self {
            ttl: Duration::from_secs(PROPOSAL_TTL_SECS),
            max_per_branch: PROPOSAL_MAX_PER_BRANCH,
            timeout: PROPOSAL_SWEEP_TIMEOUT,
        }
    }
}

impl ProposalSweeper {
    pub fn new(ttl: Duration, max_per_branch: usize, timeout: Duration) -> Self {
        Self {
            ttl,
            max_per_branch,
            timeout,
        }
    }

    /// Coalesced sweep for `branch` using the current wall-clock time.
    /// Guarantees that any caller returning `Ok` was covered by a sweep that
    /// started listing *after* the caller arrived.
    pub fn sweep_branch(&self, repo: &Path, branch: &str) -> Result<SweepReport, CommandError> {
        let now_unix = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or(Duration::ZERO)
            .as_secs() as i64;
        self.sweep_branch_coalesced_at(repo, branch, now_unix)
    }

    /// Coalesced sweep at an explicit unix timestamp (in seconds).
    pub fn sweep_branch_coalesced_at(
        &self,
        repo: &Path,
        branch: &str,
        now_unix: i64,
    ) -> Result<SweepReport, CommandError> {
        let branch_hash = branch_proposal_hash(branch)?;
        let coord = coordinator_for(repo, &branch_hash);
        let deadline = Instant::now() + self.timeout;

        let ticket = {
            let mut state = coord
                .state
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            state.arrivals = state.arrivals.saturating_add(1);
            let ticket = state.arrivals;
            while state.running {
                if state.covered >= ticket {
                    return match &state.last_error {
                        None => Ok(SweepReport::default()),
                        Some(err) => Err(CommandError::plain(err.clone())),
                    };
                }
                let remaining = deadline.saturating_duration_since(Instant::now());
                if remaining.is_zero() {
                    return Err("Proposal sweep timed out waiting for active sweeper".into());
                }
                let (next_state, wait_res) = coord
                    .cond
                    .wait_timeout(state, remaining)
                    .unwrap_or_else(std::sync::PoisonError::into_inner);
                state = next_state;
                if state.covered >= ticket {
                    return match &state.last_error {
                        None => Ok(SweepReport::default()),
                        Some(err) => Err(CommandError::plain(err.clone())),
                    };
                }
                if wait_res.timed_out() && state.running {
                    return Err("Proposal sweep timed out waiting for active sweeper".into());
                }
            }
            state.running = true;
            state.last_error = None;
            ticket
        };

        let target_watermark = {
            let state = coord
                .state
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            state.arrivals.max(ticket)
        };

        let result = self.sweep_branch_at(repo, branch, now_unix);

        let mut state = coord
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        state.running = false;
        match &result {
            Ok(_) => {
                state.covered = state.covered.max(target_watermark);
                state.last_error = None;
            }
            Err(err) => {
                state.last_error = Some(err.message().to_string());
            }
        }
        coord.cond.notify_all();
        result
    }

    /// Direct sweep for `branch` at explicit `now_unix` seconds.
    pub fn sweep_branch_at(
        &self,
        repo: &Path,
        branch: &str,
        now_unix: i64,
    ) -> Result<SweepReport, CommandError> {
        self.sweep_branch_with_hook(repo, branch, now_unix, |_, _| {})
    }

    /// Sweeps expired and over-cap proposals for `branch` at `now_unix`,
    /// invoking `before_delete(ref_name, observed_oid)` right before each CAS delete
    /// (used in tests to inject a concurrent overwrite race).
    pub(crate) fn sweep_branch_with_hook(
        &self,
        repo: &Path,
        branch: &str,
        now_unix: i64,
        mut before_delete: impl FnMut(&str, &str),
    ) -> Result<SweepReport, CommandError> {
        let branch_hash = branch_proposal_hash(branch)?;
        let prefix = format!("{PROPOSAL_REF_PREFIX}{branch_hash}/");
        let raw = git_text(
            repo,
            &[
                "for-each-ref",
                "--format=%(refname)%00%(objectname)%00%(objecttype)%00%(symref)%00%(committerdate:unix)",
                &prefix,
            ],
        )?;

        struct Candidate {
            ref_name: String,
            oid: String,
            committer_unix: i64,
        }

        let mut report = SweepReport::default();
        let mut survivors: Vec<Candidate> = Vec::new();
        let ttl_secs = self.ttl.as_secs();

        for line in raw.lines() {
            if line.is_empty() {
                continue;
            }
            let mut parts = line.split('\0');
            let ref_name = parts.next().unwrap_or_default();
            let oid = parts.next().unwrap_or_default().trim();
            let obj_type = parts.next().unwrap_or_default().trim();
            let symref = parts.next().unwrap_or_default().trim();
            let committer_unix_str = parts.next().unwrap_or_default().trim();

            if !symref.is_empty()
                || obj_type != "commit"
                || !is_valid_proposal_ref(ref_name)
                || !is_full_lowercase_oid(oid)
            {
                report.skipped += 1;
                continue;
            }
            let Ok(committer_unix) = committer_unix_str.parse::<i64>() else {
                report.skipped += 1;
                continue;
            };

            let expired =
                now_unix > committer_unix && (now_unix - committer_unix) as u64 > ttl_secs;
            if expired {
                before_delete(ref_name, oid);
                match cas_delete_proposal_ref(repo, ref_name, oid)? {
                    CasDeleteOutcome::Deleted => report.deleted_expired += 1,
                    CasDeleteOutcome::AlreadyGone => report.already_gone += 1,
                    CasDeleteOutcome::Moved => report.cas_conflicts += 1,
                }
            } else {
                survivors.push(Candidate {
                    ref_name: ref_name.to_string(),
                    oid: oid.to_string(),
                    committer_unix,
                });
            }
        }

        // Keep the newest `max_per_branch`; CAS-delete excess oldest proposals.
        survivors.sort_by(|a, b| {
            b.committer_unix
                .cmp(&a.committer_unix)
                .then_with(|| b.oid.cmp(&a.oid))
                .then_with(|| a.ref_name.cmp(&b.ref_name))
        });

        let keep_count = self.max_per_branch.min(survivors.len());
        report.kept = keep_count;
        for excess in survivors.into_iter().skip(keep_count) {
            before_delete(&excess.ref_name, &excess.oid);
            match cas_delete_proposal_ref(repo, &excess.ref_name, &excess.oid)? {
                CasDeleteOutcome::Deleted => report.deleted_excess += 1,
                CasDeleteOutcome::AlreadyGone => report.already_gone += 1,
                CasDeleteOutcome::Moved => report.cas_conflicts += 1,
            }
        }

        Ok(report)
    }
}

enum CasDeleteOutcome {
    Deleted,
    AlreadyGone,
    Moved,
}

/// CAS-safe delete equivalent to `OldOID = observed_oid, NewOID = ZeroOID`.
/// Leaves any newer concurrent overwrite intact if the ref already moved.
fn cas_delete_proposal_ref(
    repo: &Path,
    ref_name: &str,
    observed_oid: &str,
) -> Result<CasDeleteOutcome, CommandError> {
    let zero_oid = absent_oid_for_len(observed_oid.len());
    let stdin = format!("update {ref_name}\0{zero_oid}\0{observed_oid}\0");
    let run = git_captured_with_stdin(
        repo,
        &["update-ref", "--no-deref", "--stdin", "-z"],
        stdin.as_bytes(),
    )?
    .require_complete("git update-ref --stdin")?;
    if run.success {
        return Ok(CasDeleteOutcome::Deleted);
    }
    match read_exact_proposal_ref(repo, ref_name) {
        Ok(None) => Ok(CasDeleteOutcome::AlreadyGone),
        Ok(Some((current_oid, _))) if current_oid != observed_oid => Ok(CasDeleteOutcome::Moved),
        Ok(Some(_)) => Err(format!(
            "Failed deleting proposal ref {ref_name}: {}",
            String::from_utf8_lossy(&run.stderr).trim()
        )
        .into()),
        Err(_) => Ok(CasDeleteOutcome::Moved),
    }
}

// ── Quarantined Merge-Tree Conflict Inspection ───────────────────────────────

/// Inspects a 3-way merge `(ours_commit, theirs_commit)` inside an isolated
/// [`ObjectQuarantine`], extracting [`ConflictFileKey`]s, conflict stages, and
/// conflict-marked file bytes without mutating the repository's ODB, index, or
/// working tree.
pub(crate) fn inspect_merge_conflicts_quarantined(
    repo: &Path,
    ours_commit: &str,
    theirs_commit: &str,
) -> Result<QuarantinedMergeInspection, CommandError> {
    if !is_full_oid(ours_commit) || !is_full_oid(theirs_commit) {
        return Err("Merge operands must be full commit OIDs".into());
    }
    let quarantine = ObjectQuarantine::new(repo).map_err(|e| CommandError::plain(e.to_string()))?;
    let alternates = quarantine_alternates(repo)?;
    let deadline = Instant::now() + DEFAULT_TIMEOUT;

    let run = without_lazy_fetch(|| {
        quarantine.run(
            repo,
            &[
                "merge-tree",
                "--write-tree",
                "--no-messages",
                "-z",
                ours_commit,
                theirs_commit,
            ],
            deadline,
        )
    })
    .map_err(|e| CommandError::plain(e.to_string()))?;

    if !matches!(run.status_code, 0 | 1) {
        return Err(format!(
            "git merge-tree exited {}: {}",
            run.status_code,
            String::from_utf8_lossy(&run.stderr).trim()
        )
        .into());
    }

    let mut fields = run.stdout.split(|&b| b == 0);
    let tree_raw = fields.next().unwrap_or_default();
    let merged_tree_oid = String::from_utf8_lossy(tree_raw).trim().to_string();
    if !is_full_lowercase_oid(&merged_tree_oid) {
        return Err(format!("merge-tree returned invalid tree OID {merged_tree_oid:?}").into());
    }

    if run.status_code == 0 {
        return Ok(QuarantinedMergeInspection {
            clean: true,
            merged_tree_oid,
            conflicts: Vec::new(),
        });
    }

    let mut per_path: BTreeMap<String, Vec<MergeStageEntry>> = BTreeMap::new();
    for field in fields {
        if field.is_empty() {
            break;
        }
        let Some(tab) = field.iter().position(|&b| b == b'\t') else {
            return Err("Malformed stage field in merge-tree output".into());
        };
        let header = std::str::from_utf8(&field[..tab])
            .map_err(|e| format!("Invalid UTF-8 in merge-tree stage header: {e}"))?;
        let path = std::str::from_utf8(&field[tab + 1..])
            .map_err(|e| format!("Invalid UTF-8 in merge-tree conflicted path: {e}"))?;
        let mut parts = header.split(' ');
        let mode = parts.next().unwrap_or_default();
        let oid = parts.next().unwrap_or_default();
        let stage: u8 = parts
            .next()
            .unwrap_or_default()
            .parse()
            .map_err(|_| "Invalid conflict stage number in merge-tree output")?;
        if !(1..=3).contains(&stage) || !is_full_lowercase_oid(oid) {
            return Err(format!("Invalid stage entry in merge-tree output: {header}").into());
        }
        per_path
            .entry(path.to_string())
            .or_default()
            .push(MergeStageEntry {
                stage,
                mode: mode.to_string(),
                oid: oid.to_string(),
            });
    }

    let mut conflicts = Vec::with_capacity(per_path.len());
    for (path, mut stages) in per_path {
        stages.sort_by_key(|s| s.stage);
        let find_stage = |num: u8| -> (&str, &str) {
            stages
                .iter()
                .find(|s| s.stage == num)
                .map(|s| (s.oid.as_str(), s.mode.as_str()))
                .unwrap_or(("", ""))
        };
        let (base_oid, base_mode) = find_stage(1);
        let (ours_oid, ours_mode) = find_stage(2);
        let (theirs_oid, theirs_mode) = find_stage(3);
        let key = ConflictFileKey::new(
            path.clone(),
            base_oid,
            ours_oid,
            theirs_oid,
            base_mode,
            ours_mode,
            theirs_mode,
        );

        let (conflict_blob_mode, conflict_blob_bytes) = read_blob_from_quarantined_tree(
            repo,
            &quarantine,
            &alternates,
            &merged_tree_oid,
            &path,
        )?;

        conflicts.push(ExtractedConflictFile {
            key,
            stages,
            conflict_blob_bytes,
            conflict_blob_mode,
        });
    }

    Ok(QuarantinedMergeInspection {
        clean: false,
        merged_tree_oid,
        conflicts,
    })
}

fn read_blob_from_quarantined_tree(
    repo: &Path,
    quarantine: &ObjectQuarantine,
    alternates: &OsStr,
    tree_oid: &str,
    rel_path: &str,
) -> Result<(Option<String>, Option<Vec<u8>>), CommandError> {
    let deadline = Instant::now() + DEFAULT_TIMEOUT;
    let literal = format!(":(literal){rel_path}");
    let ls = quarantine
        .run(repo, &["ls-tree", "-z", tree_oid, "--", &literal], deadline)
        .map_err(|e| CommandError::plain(e.to_string()))?;
    if !ls.success {
        return Err(format!(
            "Failed reading merged tree entry for {rel_path}: {}",
            String::from_utf8_lossy(&ls.stderr).trim()
        )
        .into());
    }
    let Some(record) = ls.stdout.split(|&b| b == 0).find(|r| !r.is_empty()) else {
        return Ok((None, None));
    };
    let Some(tab) = record.iter().position(|&b| b == b'\t') else {
        return Err("Malformed ls-tree output for merged tree".into());
    };
    let meta = std::str::from_utf8(&record[..tab])
        .map_err(|e| format!("Invalid UTF-8 in ls-tree header: {e}"))?;
    let mut parts = meta.split_whitespace();
    let mode = parts.next().unwrap_or_default().to_string();
    let obj_type = parts.next().unwrap_or_default();
    let oid = parts.next().unwrap_or_default();
    if obj_type != "blob" || !is_full_lowercase_oid(oid) {
        return Ok((Some(mode), None));
    }
    let bytes = run_quarantined_with_stdin(
        repo,
        quarantine,
        alternates,
        &["cat-file", "blob", oid],
        b"",
        DEFAULT_TIMEOUT,
    )?;
    if bytes.len() > MAX_PROPOSAL_FILE_BYTES {
        return Err(format!(
            "Merged conflict blob for {rel_path} exceeds {MAX_PROPOSAL_FILE_BYTES} bytes"
        )
        .into());
    }
    Ok((Some(mode), Some(bytes)))
}

/// Extracts [`ConflictFileKey`] entries for `(ours_commit, theirs_commit)` in a
/// quarantined `merge-tree` without mutating the repository or working tree.
pub fn extract_conflict_keys(
    repo: &Path,
    ours_commit: &str,
    theirs_commit: &str,
) -> Result<Vec<ConflictFileKey>, CommandError> {
    let inspection = inspect_merge_conflicts_quarantined(repo, ours_commit, theirs_commit)?;
    Ok(inspection.conflicts.into_iter().map(|c| c.key).collect())
}

// ── Tests ────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn init_test_repo() -> (tempfile::TempDir, PathBuf) {
        let dir = tempfile::TempDir::new().unwrap();
        let repo = dir.path().canonicalize().unwrap();
        crate::test_support::git_in(&repo, &["init", "-b", "main"]);
        crate::test_support::git_in(&repo, &["config", "user.name", "Test"]);
        crate::test_support::git_in(&repo, &["config", "user.email", "test@example.com"]);
        crate::test_support::git_in(&repo, &["config", "commit.gpgsign", "false"]);
        crate::test_support::trust_repo(&repo);
        (dir, repo)
    }

    fn commit_file(repo: &Path, path: &str, content: &str, msg: &str) -> String {
        if let Some(parent) = repo.join(path).parent() {
            fs::create_dir_all(parent).unwrap();
        }
        fs::write(repo.join(path), content).unwrap();
        crate::test_support::git_in(repo, &["add", path]);
        crate::test_support::git_in(repo, &["commit", "-m", msg]);
        git_text(repo, &["rev-parse", "HEAD"])
            .unwrap()
            .trim()
            .to_string()
    }

    fn setup_conflict_pair(repo: &Path) -> (String, String, String) {
        let base = commit_file(repo, "shared.txt", "line1\nbase\nline3\n", "base");
        crate::test_support::git_in(repo, &["checkout", "-b", "feat"]);
        let feat = commit_file(repo, "shared.txt", "line1\ntheirs\nline3\n", "feat");
        crate::test_support::git_in(repo, &["checkout", "main"]);
        let tip = commit_file(repo, "shared.txt", "line1\nours\nline3\n", "ours");
        (base, tip, feat)
    }

    #[test]
    fn create_list_read_and_drop_proposal_ref() {
        let (_dir, repo) = init_test_repo();
        let (_base, tip, feat) = setup_conflict_pair(&repo);

        // Create proposal with nested path and a trailer containing newline injection attempt.
        let summary = create_proposal(
            &repo,
            &CreateProposalRequest {
                branch: "main".to_string(),
                tip_oid: tip.clone(),
                source_commit_oid: feat.clone(),
                resolver: "dual-model".to_string(),
                requested_by: "agent-a\nFake-Trailer: injected".to_string(),
                files: vec![
                    ProposedFileInput {
                        path: "shared.txt".to_string(),
                        mode: "100644".to_string(),
                        bytes: b"line1\nresolved\nline3\n".to_vec(),
                    },
                    ProposedFileInput {
                        path: "nested/dir/extra.txt".to_string(),
                        mode: "100755".to_string(),
                        bytes: b"#!/bin/sh\necho ok\n".to_vec(),
                    },
                ],
                expected_old_oid: None,
            },
        )
        .expect("create_proposal");

        let expected_hash = branch_proposal_hash("main").unwrap();
        assert_eq!(expected_hash.len(), 16);
        assert_eq!(
            summary.ref_name,
            format!("refs/gitpulse/proposals/{expected_hash}/{feat}")
        );
        assert!(is_valid_proposal_ref(&summary.ref_name));
        assert_eq!(summary.tip_oid, tip);
        assert_eq!(summary.source_commit_oid, feat);
        assert_eq!(summary.resolver, "dual-model");
        assert!(
            !summary.requested_by.contains('\n'),
            "newline must be escaped: {:?}",
            summary.requested_by
        );

        // Verify 2 parents [tip, source_commit_oid], sparse tree (ONLY proposed files), and no raw injected trailer.
        let commit_raw =
            git_text(&repo, &["cat-file", "commit", &summary.proposal_commit_oid]).unwrap();
        let parent_lines: Vec<&str> = commit_raw
            .lines()
            .filter(|l| l.starts_with("parent "))
            .collect();
        assert_eq!(
            parent_lines,
            vec![format!("parent {tip}"), format!("parent {feat}")]
        );
        assert!(
            !commit_raw.lines().any(|l| l.starts_with("Fake-Trailer:")),
            "injected trailer leaked into commit:\n{commit_raw}"
        );

        // List proposals for branch and globally.
        let listed = list_proposals(&repo, Some("main")).unwrap();
        assert_eq!(listed.len(), 1);
        assert_eq!(listed[0].proposal_commit_oid, summary.proposal_commit_oid);
        assert_eq!(list_proposals(&repo, None).unwrap().len(), 1);

        // Read proposal package back.
        let pkg = read_proposal(&repo, "main", &feat)
            .unwrap()
            .expect("proposal must exist");
        assert_eq!(pkg.summary.proposal_commit_oid, summary.proposal_commit_oid);
        assert_eq!(pkg.files.len(), 2);
        assert_eq!(pkg.files[0].path, "nested/dir/extra.txt");
        assert_eq!(pkg.files[0].mode, "100755");
        assert_eq!(pkg.files[1].path, "shared.txt");
        assert_eq!(pkg.files[1].bytes, b"line1\nresolved\nline3\n");

        // Idempotent drop on land.
        assert!(drop_proposal(&repo, "main", &feat).unwrap());
        assert!(!drop_proposal(&repo, "main", &feat).unwrap());
        assert!(read_proposal(&repo, "main", &feat).unwrap().is_none());
    }

    #[test]
    fn trailer_validation_rejects_control_and_line_separators() {
        for bad in [
            "",
            "   ",
            "a\nb",
            "a\rb",
            "a\tb",
            "a\0b",
            "a\u{0085}b",
            "a\u{2028}b",
            "a\u{2029}b",
            "a\x1bb",
            "a\x7fb",
        ] {
            assert!(
                validate_trailer_value(bad).is_err(),
                "expected rejection for {bad:?}"
            );
            assert!(validate_resolution_trailer_value(bad).is_err());
        }
        assert_eq!(
            validate_trailer_value("alice (gpt-5)").unwrap(),
            "alice (gpt-5)"
        );
    }

    #[test]
    fn conflict_file_key_reused_after_unrelated_commit_lands_on_main() {
        let (_dir, repo) = init_test_repo();
        let (_base, tip1, feat) = setup_conflict_pair(&repo);

        let keys_before = extract_conflict_keys(&repo, &tip1, &feat).unwrap();
        assert_eq!(keys_before.len(), 1);
        let key1 = keys_before[0].clone();

        let mut cache = ConflictVerdictCache::new();
        cache.insert_verdict(FileResolutionVerdict::proposed_by_model(
            key1.clone(),
            Some("100644".to_string()),
            Some(b"line1\nresolved\nline3\n".to_vec()),
        ));

        // Land an unrelated commit onto main (tip1 -> tip2).
        let tip2 = commit_file(&repo, "unrelated.txt", "unrelated change\n", "unrelated");
        assert_ne!(tip1, tip2);

        let keys_after = extract_conflict_keys(&repo, &tip2, &feat).unwrap();
        assert_eq!(keys_after.len(), 1);
        let key2 = keys_after[0].clone();

        // Key is identical despite tip move, so cached verdict is reused!
        assert_eq!(key1, key2);
        cache.set_current(&keys_after);
        let reused = cache.get_current(&key2).expect("verdict must be reused");
        assert_eq!(
            reused.resolved_bytes.as_deref(),
            Some(b"line1\nresolved\nline3\n".as_slice())
        );

        // If a commit on main DOES touch shared.txt (tip2 -> tip3), key changes and old verdict is ignored.
        let tip3 = commit_file(
            &repo,
            "shared.txt",
            "line1\nours-v2\nline3\n",
            "touch shared",
        );
        let keys_changed = extract_conflict_keys(&repo, &tip3, &feat).unwrap();
        assert_eq!(keys_changed.len(), 1);
        assert_ne!(keys_changed[0], key1);
        cache.set_current(&keys_changed);
        assert!(cache.get_current(&key1).is_none());
        assert!(cache.get_current(&keys_changed[0]).is_none());
    }

    #[test]
    fn sweeper_enforces_max_per_branch_and_ttl_with_cas_safety() {
        let (_dir, repo) = init_test_repo();
        let (_base, tip, _feat) = setup_conflict_pair(&repo);

        // Create 22 distinct source commits and proposals for branch "main".
        let mut commits = Vec::new();
        for i in 0..22 {
            crate::test_support::git_in(&repo, &["checkout", "-q", "-b", &format!("p-{i}"), &tip]);
            let c = commit_file(
                &repo,
                "shared.txt",
                &format!("line1\nv{i}\nline3\n"),
                &format!("c{i}"),
            );
            commits.push(c);
        }
        crate::test_support::git_in(&repo, &["checkout", "-q", "main"]);

        let mut refs_and_times = Vec::new();
        for (i, c) in commits.iter().enumerate() {
            let summary = create_proposal(
                &repo,
                &CreateProposalRequest {
                    branch: "main".to_string(),
                    tip_oid: tip.clone(),
                    source_commit_oid: c.clone(),
                    resolver: "model".to_string(),
                    requested_by: format!("agent-{i}"),
                    files: vec![ProposedFileInput {
                        path: "shared.txt".to_string(),
                        mode: "100644".to_string(),
                        bytes: format!("resolved-{i}\n").into_bytes(),
                    }],
                    expected_old_oid: None,
                },
            )
            .unwrap();
            refs_and_times.push(summary);
        }
        assert_eq!(list_proposals(&repo, Some("main")).unwrap().len(), 22);

        // 1. Test CAS-safe delete during sweep: overwrite the first excess target concurrently
        // right before the sweeper deletes it, and assert the newer overwrite survives!
        let sweeper = ProposalSweeper::default();
        let now_within_ttl = refs_and_times.last().unwrap().committer_unix + 60;
        let mut raced_ref: Option<String> = None;
        let mut raced_new_oid: Option<String> = None;

        let report = sweeper
            .sweep_branch_with_hook(&repo, "main", now_within_ttl, |ref_name, _observed_oid| {
                if raced_ref.is_none() {
                    let src_oid = ref_name.rsplit('/').next().unwrap().to_string();
                    let updated = create_proposal(
                        &repo,
                        &CreateProposalRequest {
                            branch: "main".to_string(),
                            tip_oid: tip.clone(),
                            source_commit_oid: src_oid,
                            resolver: "concurrent-overwriter".to_string(),
                            requested_by: "racer".to_string(),
                            files: vec![ProposedFileInput {
                                path: "shared.txt".to_string(),
                                mode: "100644".to_string(),
                                bytes: b"fresh-concurrent-overwrite\n".to_vec(),
                            }],
                            expected_old_oid: None,
                        },
                    )
                    .unwrap();
                    raced_ref = Some(ref_name.to_string());
                    raced_new_oid = Some(updated.proposal_commit_oid);
                }
            })
            .unwrap();

        assert_eq!(
            report.cas_conflicts, 1,
            "expected 1 CAS conflict: {report:?}"
        );
        assert_eq!(
            report.deleted_excess, 1,
            "expected 1 other excess deleted: {report:?}"
        );
        assert_eq!(report.kept, 20);

        // The concurrently overwritten ref MUST remain intact with its new commit OID!
        let raced_ref = raced_ref.unwrap();
        let pkg = read_proposal_by_ref(&repo, &raced_ref)
            .unwrap()
            .expect("concurrent overwrite must survive CAS delete");
        assert_eq!(Some(pkg.summary.proposal_commit_oid), raced_new_oid);
        assert_eq!(pkg.files[0].bytes, b"fresh-concurrent-overwrite\n");

        // Second sweep (without race) settles to exact MaxPerBranch = 20.
        let report2 = sweeper
            .sweep_branch_at(&repo, "main", now_within_ttl)
            .unwrap();
        assert_eq!(report2.deleted_excess, 1);
        assert_eq!(report2.kept, 20);
        assert_eq!(list_proposals(&repo, Some("main")).unwrap().len(), 20);

        // 2. Test 7d TTL expiry: at min_created + 7d - 1s nothing expires; at max_created + 7d + 1s all 20 expire.
        let remaining_proposals = list_proposals(&repo, Some("main")).unwrap();
        let min_created = remaining_proposals
            .iter()
            .map(|p| p.committer_unix)
            .min()
            .unwrap();
        let max_created = remaining_proposals
            .iter()
            .map(|p| p.committer_unix)
            .max()
            .unwrap();
        let just_before_ttl = min_created + PROPOSAL_TTL_SECS as i64 - 1;
        let pre_ttl = sweeper
            .sweep_branch_at(&repo, "main", just_before_ttl)
            .unwrap();
        assert_eq!(pre_ttl.deleted_expired, 0);
        assert_eq!(list_proposals(&repo, Some("main")).unwrap().len(), 20);

        let past_ttl = max_created + PROPOSAL_TTL_SECS as i64 + 1;
        let post_ttl = sweeper.sweep_branch_at(&repo, "main", past_ttl).unwrap();
        assert_eq!(post_ttl.deleted_expired, 20);
        assert_eq!(list_proposals(&repo, Some("main")).unwrap().len(), 0);
    }

    #[test]
    fn decide_land_enforces_tree_oid_and_hold_invariants() {
        let key = ConflictFileKey::new(
            "a.txt",
            "1".repeat(40),
            "2".repeat(40),
            "3".repeat(40),
            "100644",
            "100644",
            "100644",
        );
        let tree_a = "a".repeat(40);
        let tree_b = "b".repeat(40);

        // 1. Missing answer -> blocked.
        let d = decide_land(&LandDecisionInput {
            answer: None,
            candidate_tree_oid: &tree_a,
            verified_tree_oid: Some(&tree_a),
            verify_required: false,
        });
        assert!(!d.allowed);

        // 2. Model-originated proposal WITHOUT verified_tree -> blocked.
        let model_answer = LandAnswer {
            files: vec![FileResolutionVerdict::proposed_by_model(
                key.clone(),
                None,
                None,
            )],
        };
        let d = decide_land(&LandDecisionInput {
            answer: Some(&model_answer),
            candidate_tree_oid: &tree_a,
            verified_tree_oid: None,
            verify_required: false,
        });
        assert!(!d.allowed);
        assert!(d.proposes);

        // 3. Model-originated resolution with MISMATCHED verified_tree -> blocked.
        let d = decide_land(&LandDecisionInput {
            answer: Some(&model_answer),
            candidate_tree_oid: &tree_a,
            verified_tree_oid: Some(&tree_b),
            verify_required: false,
        });
        assert!(!d.allowed);

        // 4. Model-originated resolution with EXACT matching tree_oid -> allowed.
        let d = decide_land(&LandDecisionInput {
            answer: Some(&model_answer),
            candidate_tree_oid: &tree_a,
            verified_tree_oid: Some(&tree_a),
            verify_required: false,
        });
        assert!(d.allowed);
        assert!(d.proposes);

        // 5. Held resolution (dual-model disagreement) -> NEVER auto-lands EVEN IF verified tree matches!
        let held_answer = LandAnswer {
            files: vec![FileResolutionVerdict::held(
                key.clone(),
                ResolutionOrigin::Model,
                "dual-model disagreement",
            )],
        };
        let d = decide_land(&LandDecisionInput {
            answer: Some(&held_answer),
            candidate_tree_oid: &tree_a,
            verified_tree_oid: Some(&tree_a),
            verify_required: false,
        });
        assert!(!d.allowed);

        // 6. Rule-originated resolved with verify_required=false -> allowed without verification.
        let rule_answer = LandAnswer {
            files: vec![FileResolutionVerdict::resolved_by_rule(
                key.clone(),
                None,
                None,
            )],
        };
        let d = decide_land(&LandDecisionInput {
            answer: Some(&rule_answer),
            candidate_tree_oid: &tree_a,
            verified_tree_oid: None,
            verify_required: false,
        });
        assert!(d.allowed);
        assert!(!d.proposes);

        // 7. Rule-originated resolved with verify_required=true requires matching tree OID.
        let d = decide_land(&LandDecisionInput {
            answer: Some(&rule_answer),
            candidate_tree_oid: &tree_a,
            verified_tree_oid: None,
            verify_required: true,
        });
        assert!(!d.allowed);

        // 8. Unresolved file -> blocked even with matching tree OID.
        let unresolved_answer = LandAnswer {
            files: vec![FileResolutionVerdict::unresolved(
                key,
                ResolutionOrigin::Rule,
            )],
        };
        let d = decide_land(&LandDecisionInput {
            answer: Some(&unresolved_answer),
            candidate_tree_oid: &tree_a,
            verified_tree_oid: Some(&tree_a),
            verify_required: true,
        });
        assert!(!d.allowed);
    }
}
