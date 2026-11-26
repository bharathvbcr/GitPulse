//! A one-use cross-process mailbox from GitPulse to a Claude Code or Antigravity session.
//!
//! ## Layout
//!
//! ```text
//! <GitPulse config dir>/agent-mailbox/            0700
//!   .lock                                         0600, advisory flock for root dir mutations
//!   .locks/<session uuid>.lock                    0600, advisory flock per session
//!   .hw/<session uuid>                            0600, highest envelope stamp issued (stamps only climb)
//!   .meta/<session uuid>/<request id>.json        0600, EnvelopeProvenance sidecar
//!   .epochs/<session uuid>                        0600, monotonic turn_epoch counter
//!   .continuations/<session uuid>                 0600, per-turn continuation counter
//!   <session uuid>/                               0700, one per session
//!     p-<ms>-<request id>.msg                     0600, pending
//!     c-<ms>-<request id>.msg                     0600, claimed (kept briefly)
//!     w-<ms>-wh-<pid>-<nonce>                     0600, hosted hook waiting here
//!     w-<ms>-we-<pid>-<nonce>                     0600, external hook waiting here
//!     .tmp-<pid>-<nonce>                          0600, a write in progress
//! ```
//!
//! ## What is guaranteed, and by what
//!
//! * **A message is whole or absent.** It is written to a temporary name in the
//!   same directory and published with a no-replace rename
//!   ([`crate::fs_entry::rename_noreplace`]), after its [`EnvelopeProvenance`]
//!   sidecar is synced, under an advisory `flock`.
//! * **A message is delivered once.** The claim is a rename of `p-…` to `c-…`.
//!   Of any number of processes racing for one message the kernel lets exactly
//!   one rename succeed; the others see the source gone.
//! * **Everything is bounded.** [`MAX_MESSAGE_BYTES`] per message,
//!   [`INLINE_PREVIEW_BYTES`] before pointer truncation to `agent_bus`,
//!   [`MAX_PENDING`] per session, [`MAX_SESSIONS`] directories, a pending
//!   message expires after [`PENDING_TTL`], and at most
//!   [`MAX_WAITERS_PER_SESSION`] / [`MAX_WAITERS_TOTAL`] hook processes wait at
//!   once, partitioned between hosted (`wh-`, guaranteed [`MAX_WAITERS_HOSTED`])
//!   and external (`we-`, capped at [`MAX_WAITERS_EXTERNAL`]) pools.
//! * **Only this user's files are read.** Directories are created 0700 and
//!   files 0600 with `O_NOFOLLOW`; symlinks, non-regular files, foreign owners,
//!   or group/other permissions are refused.

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::borrow::Cow;
use std::collections::HashMap;
use std::fs::{File, OpenOptions};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

/// The longest message stored directly in a mailbox file, in bytes of UTF-8.
pub(crate) const MAX_MESSAGE_BYTES: usize = 8 * 1024;
/// Maximum peer payload stored inline before writing a preview + `msg_id`
/// pointer to `agent_bus`.
pub(crate) const INLINE_PREVIEW_BYTES: usize = 6 * 1024;
/// Largest provenance sidecar accepted on read.
const MAX_META_BYTES: u64 = 4 * 1024;
/// Pending messages one session may hold.
pub(crate) const MAX_PENDING: usize = 8;
/// Session directories under the root.
pub(crate) const MAX_SESSIONS: usize = 256;
/// How long a message waits for a hook to take it.
pub(crate) const PENDING_TTL: Duration = Duration::from_secs(2 * 60 * 60);
/// The largest envelope stamp: stamps are 13 digits in a message file name.
const MAX_STAMP: u64 = 9_999_999_999_999;
/// How long a claimed message is kept, so the sender can see that it was.
pub(crate) const CLAIMED_TTL: Duration = Duration::from_secs(10 * 60);
/// How long an unfinished write is left alone.
const TEMP_TTL: Duration = Duration::from_secs(60);
/// A waiter whose file has not been touched for this long is gone.
pub(crate) const WAITER_STALE: Duration = Duration::from_secs(10);
/// How often a waiter shows it is alive.
pub(crate) const HEARTBEAT: Duration = Duration::from_secs(2);
/// How often a waiter looks for a message. A directory read, not a busy loop.
pub(crate) const POLL: Duration = Duration::from_millis(250);
/// Why a wait that ended with only refused envelopes pending says it was declined.
const PAUSED_AT_HOP_LIMIT: &str =
    "pending peer message paused at hop limit; leaving unclaimed for manual review";
/// Hook processes waiting on one session.
pub(crate) const MAX_WAITERS_PER_SESSION: usize = 2;
/// Guaranteed minimum places reserved for GitPulse-hosted terminal sessions.
pub(crate) const MAX_WAITERS_HOSTED: usize = 48;
/// Maximum places external sessions may occupy across the machine.
pub(crate) const MAX_WAITERS_EXTERNAL: usize = 16;
/// Hook processes waiting on any session (`MAX_WAITERS_HOSTED + MAX_WAITERS_EXTERNAL`).
pub(crate) const MAX_WAITERS_TOTAL: usize = 64;
/// Maximum consecutive Antigravity `PostInvocation` / `Stop` auto-continuations
/// allowed within a single user turn.
pub(crate) const MAX_CONTINUATIONS: u32 = 3;
/// Longest request id, as a file-name part.
const MAX_REQUEST_ID: usize = 64;
/// Directory entries one pass will look at.
const SCAN_LIMIT: usize = 1024;
/// An empty session directory older than this is removed.
const EMPTY_SESSION_TTL: Duration = Duration::from_secs(60 * 60);

/// Maximum in-memory `HumanConfirmed` issuance records retained per process.
/// Never written to a `0600` file on disk: any same-UID agent process can read
/// and write `0600` files, so trust lives solely in the control-plane owner
/// process's memory (Opus P0-3 / #15).
const MAX_HUMAN_ISSUANCES: usize = MAX_SESSIONS * MAX_PENDING;
/// Lifetime of an in-memory human issuance record (`PENDING_TTL + CLAIMED_TTL`).
const HUMAN_ISSUANCE_TTL: Duration = Duration::from_secs(2 * 60 * 60 + 10 * 60);

/// Serializes in-process callers alongside advisory `flock` across processes.
static DEPOSIT_LOCK: Mutex<()> = Mutex::new(());

/// Lowercase hex SHA-256 of normalized message bytes.
pub(crate) fn text_sha256_hex(text: &str) -> String {
    let digest = Sha256::digest(text.as_bytes());
    let mut hex = String::with_capacity(64);
    for byte in digest {
        use std::fmt::Write as _;
        let _ = write!(&mut hex, "{byte:02x}");
    }
    hex
}

fn const_time_eq(a: &[u8], b: &[u8]) -> bool {
    if a.len() != b.len() {
        return false;
    }
    let mut diff = 0u8;
    for (&x, &y) in a.iter().zip(b.iter()) {
        diff |= x ^ y;
    }
    diff == 0
}

/// Untrusted `PeerAgent` provenance used whenever a `HumanConfirmed` sidecar
/// fails issuance verification (forged sidecar, tampered body, cross-session
/// replay, or owner restart). Never fails open.
pub(crate) fn downgraded_peer_provenance(request_id: &str) -> EnvelopeProvenance {
    EnvelopeProvenance::PeerAgent {
        msg_id: request_id.to_owned(),
        from_peer: "unverified-local-mailbox".to_owned(),
        provider: "unknown".to_owned(),
        branch: String::new(),
        thread_key: String::new(),
        hop: 0,
        max_hops: 1,
        truncated_in_bus: false,
        repo_root: String::new(),
    }
}

#[derive(Debug, Clone)]
struct HumanIssuanceEntry {
    text_sha256: String,
    expires_at_ms: u64,
    seq: u64,
}

#[derive(Debug, Default)]
struct HumanIssuanceState {
    next_seq: u64,
    entries: HashMap<(PathBuf, String, String), HumanIssuanceEntry>,
}

/// In-memory issuance ledger for `EnvelopeProvenance::HumanConfirmed` messages.
///
/// Only the control-plane owner process populates this ledger (strictly in-process,
/// never via a Unix socket write op and never backed by an agent-writable `0600`
/// file). Hook processes verify envelopes via a read-only broker socket query;
/// any unknown, tampered, expired, or post-restart envelope is downgraded to
/// untrusted `PeerAgent` XML framing.
#[derive(Debug, Default)]
pub(crate) struct HumanIssuanceLedger {
    state: Mutex<HumanIssuanceState>,
}

impl HumanIssuanceLedger {
    pub(crate) fn new() -> Self {
        Self::default()
    }

    pub(crate) fn record_scoped(
        &self,
        scope: &Path,
        session_uuid: &str,
        request_id: &str,
        text: &str,
        created_at_ms: u64,
    ) -> u64 {
        let normalized = normalize_crlf(text);
        let text_sha256 = text_sha256_hex(normalized.as_ref());
        let session_key = session_uuid.to_ascii_lowercase();
        let ttl_ms = millis_of(HUMAN_ISSUANCE_TTL);
        let now_ms = millis(SystemTime::now()).max(created_at_ms);
        let expires_at_ms = now_ms.saturating_add(ttl_ms);
        let mut guard = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        guard.entries.retain(|_, entry| entry.expires_at_ms >= now_ms);
        let key = (scope.to_path_buf(), session_key, request_id.to_owned());
        if let Some(existing) = guard.entries.get(&key) {
            if const_time_eq(existing.text_sha256.as_bytes(), text_sha256.as_bytes()) {
                return existing.seq;
            }
        }
        if guard.entries.len() >= MAX_HUMAN_ISSUANCES && !guard.entries.contains_key(&key) {
            if let Some(oldest) = guard
                .entries
                .iter()
                .min_by_key(|(_, entry)| entry.seq)
                .map(|(k, _)| k.clone())
            {
                guard.entries.remove(&oldest);
            }
        }
        guard.next_seq = guard.next_seq.saturating_add(1);
        let seq = guard.next_seq;
        guard.entries.insert(
            key,
            HumanIssuanceEntry {
                text_sha256,
                expires_at_ms,
                seq,
            },
        );
        seq
    }

    pub(crate) fn verify_scoped(
        &self,
        scope: &Path,
        session_uuid: &str,
        request_id: &str,
        text_sha256: &str,
    ) -> bool {
        let now_ms = millis(SystemTime::now());
        let key = (
            scope.to_path_buf(),
            session_uuid.to_ascii_lowercase(),
            request_id.to_owned(),
        );
        let guard = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let Some(entry) = guard.entries.get(&key) else {
            return false;
        };
        entry.expires_at_ms >= now_ms
            && const_time_eq(entry.text_sha256.as_bytes(), text_sha256.as_bytes())
    }
}

/// Process-wide in-memory human-issuance ledger held by the control-plane owner.
pub(crate) fn process_human_issuance_ledger() -> &'static HumanIssuanceLedger {
    static LEDGER: OnceLock<HumanIssuanceLedger> = OnceLock::new();
    LEDGER.get_or_init(HumanIssuanceLedger::new)
}

/// Verifies whether a `HumanConfirmed` mailbox envelope was genuinely issued
/// by the control-plane owner process for this `(session_uuid, request_id, text_sha256)`.
pub(crate) trait HumanIssuanceVerifier: Send + Sync {
    fn verify_human_issuance(
        &self,
        mailbox_root: &Path,
        session_uuid: &str,
        request_id: &str,
        text_sha256: &str,
    ) -> bool;
}

/// Verifier that checks an explicit in-memory [`HumanIssuanceLedger`].
#[cfg_attr(not(test), allow(dead_code))]
pub(crate) struct LedgerHumanVerifier {
    ledger: Arc<HumanIssuanceLedger>,
}

#[cfg_attr(not(test), allow(dead_code))]
impl LedgerHumanVerifier {
    pub(crate) fn new(ledger: Arc<HumanIssuanceLedger>) -> Self {
        Self { ledger }
    }
}

impl HumanIssuanceVerifier for LedgerHumanVerifier {
    fn verify_human_issuance(
        &self,
        mailbox_root: &Path,
        session_uuid: &str,
        request_id: &str,
        text_sha256: &str,
    ) -> bool {
        self.ledger
            .verify_scoped(mailbox_root, session_uuid, request_id, text_sha256)
    }
}

/// Verifier that rejects every `HumanConfirmed` claim (simulates owner restart
/// or unreachable broker socket — never fails open).
#[cfg(test)]
pub(crate) struct RejectAllHumanVerifier;

#[cfg(test)]
impl HumanIssuanceVerifier for RejectAllHumanVerifier {
    fn verify_human_issuance(
        &self,
        _mailbox_root: &Path,
        _session_uuid: &str,
        _request_id: &str,
        _text_sha256: &str,
    ) -> bool {
        false
    }
}

/// Why a mailbox call did not do what was asked.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum MailboxError {
    /// The input is not acceptable: a bad session id, request id or text.
    Invalid(String),
    /// A cap was reached.
    Full(String),
    /// Something on disk is not what this user alone made.
    Insecure(String),
    /// The file system said no.
    Io(String),
}

impl std::fmt::Display for MailboxError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Invalid(m) | Self::Full(m) | Self::Insecure(m) | Self::Io(m) => f.write_str(m),
        }
    }
}

impl std::error::Error for MailboxError {}

fn io(context: &str, error: &std::io::Error) -> MailboxError {
    MailboxError::Io(format!("{context}: {error}"))
}

/// Who originated a mailbox envelope, governing whether delivery frames it as
/// human-confirmed instructions, host-governor Manvi steward guidance, or
/// untrusted peer-agent output.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub(crate) enum EnvelopeProvenance {
    HumanConfirmed,
    PeerAgent {
        msg_id: String,
        from_peer: String,
        provider: String,
        #[serde(default)]
        branch: String,
        #[serde(default)]
        thread_key: String,
        hop: u32,
        max_hops: u32,
        #[serde(default)]
        truncated_in_bus: bool,
        #[serde(default)]
        repo_root: String,
    },
    SystemSteward {
        msg_id: String,
        model: String,
        mode: String,
        tier: String,
        binding: String,
        #[serde(default)]
        repo_root: String,
        #[serde(default)]
        bus_seq: u64,
    },
}

fn same_steward_repo(existing_root: &str, new_root: &str) -> bool {
    let a = existing_root.trim();
    let b = new_root.trim();
    if a == b {
        return true;
    }
    if a.is_empty() || b.is_empty() {
        return false;
    }
    match (
        crate::harness::steward::canonical_common_dir(a),
        crate::harness::steward::canonical_common_dir(b),
    ) {
        (Some(ca), Some(cb)) => ca == cb,
        _ => false,
    }
}

/// Which waiter quota pool a `Stop` `deliver` process belongs to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum WaitPool {
    /// Spawned inside a GitPulse terminal (`GITPULSE_SESSION_ID` set).
    Hosted,
    /// Opted-in external CLI session in a trusted repository.
    External,
}

impl WaitPool {
    fn tag(self) -> &'static str {
        match self {
            Self::Hosted => "wh",
            Self::External => "we",
        }
    }
}

/// What became of a peer deposit made through [`Mailbox::deposit_peer_or_overflow`].
#[derive(Debug)]
pub(crate) enum PeerDeposit {
    /// The envelope is in the mailbox (or was already, for a retried send).
    Deposited(Deposit),
    /// The peer cap was reached, so the envelope was not stored. `notice` is what became
    /// of the overflow notice that tells the recipient to read the rest from the bus.
    Overflowed {
        notice: Result<Deposit, MailboxError>,
    },
}

/// Pending slots peer-agent envelopes may hold; the last slot of [`MAX_PENDING`] is kept for
/// the one overflow notice per repository, so a flood of peer messages can still say it overflowed.
const MAX_PEER_PENDING: usize = MAX_PENDING - 1;

const OVERFLOW_ID_PREFIX: &str = "bus-overflow-";
const OVERFLOW_FROM_PEER: &str = crate::engine::agent_bus::GITPULSE_SENDER_NAME;

/// The request id of a repository's overflow notice: stable across calls and processes, so a
/// second overflow finds the first notice instead of adding another.
fn overflow_notice_id(repo_root: &str) -> String {
    let identity = crate::harness::steward::canonical_common_dir(repo_root)
        .unwrap_or_else(|| repo_root.to_owned());
    // FNV-1a: `DefaultHasher` is not guaranteed stable between builds, and two builds
    // sharing a mailbox must agree on the id.
    let mut hash: u32 = 0x811c_9dc5;
    for byte in identity.bytes() {
        hash ^= u32::from(byte);
        hash = hash.wrapping_mul(0x0100_0193);
    }
    format!("{OVERFLOW_ID_PREFIX}{hash:08x}")
}

fn is_overflow_notice(request_id: &str, provenance: &EnvelopeProvenance) -> bool {
    request_id.starts_with(OVERFLOW_ID_PREFIX)
        && matches!(provenance, EnvelopeProvenance::PeerAgent { from_peer, .. } if from_peer == OVERFLOW_FROM_PEER)
}

/// What became of a deposit.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Deposit {
    pub(crate) request_id: String,
    pub(crate) created_at_ms: u64,
    pub(crate) expires_at_ms: u64,
    /// The request id had already been deposited with this text; nothing was
    /// written a second time.
    pub(crate) duplicate: bool,
}

/// A message a hook took.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Claim {
    pub(crate) request_id: String,
    pub(crate) text: String,
}

/// A claimed message paired with its verified provenance and deposit timestamp.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ClaimedEnvelope {
    pub(crate) claim: Claim,
    pub(crate) provenance: EnvelopeProvenance,
    pub(crate) created_at_ms: u64,
    /// `true` ONLY when `provenance == EnvelopeProvenance::HumanConfirmed` AND
    /// the envelope matched a control-plane human issuance record for
    /// `(session_uuid, request_id, sha256(text))`. Defense in depth so
    /// `deliver::render_message` can never emit `human_reminder` for an
    /// unverified envelope.
    pub(crate) human_verified: bool,
}

/// Where one deposited message stands.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Delivery {
    /// Waiting for a hook to take it.
    Pending,
    /// A hook took it. Not a statement that the agent read it.
    Claimed,
    /// Not there: never deposited, expired, or already cleaned up.
    Absent,
}

/// The outcome of waiting for a message.
#[cfg(test)]
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum Wait {
    Message(Claim),
    /// The limit passed with nothing for this session.
    TimedOut,
    /// This waiter is over a cap or displaced and did not wait.
    Declined(String),
}

/// The outcome of waiting for a message with provenance.
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum EnvelopeWait {
    Message(ClaimedEnvelope),
    TimedOut,
    Declined(String),
}

/// Normalizes CRLF (`\r\n`) line endings to LF (`\n`) while leaving bare `\r`
/// untouched so [`validate_message`] continues to reject carriage-return
/// overwrite sequences.
pub(crate) fn normalize_crlf(text: &str) -> Cow<'_, str> {
    if text.contains("\r\n") {
        Cow::Owned(text.replace("\r\n", "\n"))
    } else {
        Cow::Borrowed(text)
    }
}

/// Truncates peer bodies exceeding [`INLINE_PREVIEW_BYTES`] on a UTF-8 char
/// boundary and appends a pointer to the canonical `agent_bus` record.
pub(crate) fn preview_for_mailbox(body: &str, msg_id: &str) -> (String, bool) {
    let normalized = normalize_crlf(body);
    if normalized.len() <= INLINE_PREVIEW_BYTES {
        return (normalized.into_owned(), false);
    }
    let suffix = format!(
        "\n\n[... truncated for mailbox preview; full payload ({} bytes) stored in agent_bus under msg_id={msg_id} ...]",
        normalized.len()
    );
    let budget = INLINE_PREVIEW_BYTES.saturating_sub(suffix.len());
    let mut cut = budget.min(normalized.len());
    while cut > 0 && !normalized.is_char_boundary(cut) {
        cut -= 1;
    }
    let mut preview = String::with_capacity(cut + suffix.len());
    preview.push_str(&normalized[..cut]);
    preview.push_str(&suffix);
    (preview, true)
}

/// A session id as a directory name: a canonical UUID, lower-cased.
pub(crate) fn session_key(raw: &str) -> Result<String, MailboxError> {
    if crate::workbench::terminal_command::is_canonical_uuid(raw) {
        Ok(raw.to_ascii_lowercase())
    } else {
        Err(MailboxError::Invalid(
            "a mailbox is named by a Claude Code session id".into(),
        ))
    }
}

/// True when `text` holds a control character other than a line break or tab. The one
/// definition of that rule, shared by the mailbox and the coordination bus so a body one
/// accepts is never refused by the other.
pub(crate) fn has_disallowed_control(text: &str) -> bool {
    text.chars()
        .any(|c| c.is_control() && !matches!(c, '\n' | '\t'))
}

/// Checks a message: nonblank, bounded, and free of control characters other
/// than line breaks and tabs.
pub(crate) fn validate_message(text: &str) -> Result<(), MailboxError> {
    if text.trim().is_empty() {
        return Err(MailboxError::Invalid("A message cannot be blank.".into()));
    }
    if text.len() > MAX_MESSAGE_BYTES {
        return Err(MailboxError::Invalid(format!(
            "A message is at most {MAX_MESSAGE_BYTES} bytes; this one is {}.",
            text.len()
        )));
    }
    if has_disallowed_control(text) {
        return Err(MailboxError::Invalid(
            "A message cannot contain control characters other than line breaks and tabs.".into(),
        ));
    }
    Ok(())
}

pub(crate) fn valid_request_id(id: &str) -> bool {
    !id.is_empty()
        && id.len() <= MAX_REQUEST_ID
        && id
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-')
}

fn millis(time: SystemTime) -> u64 {
    time.duration_since(UNIX_EPOCH)
        .map_or(0, |d| u64::try_from(d.as_millis()).unwrap_or(u64::MAX))
}

fn age(now: SystemTime, then: SystemTime) -> Duration {
    now.duration_since(then).unwrap_or_default()
}

/// `p-<ms>-<id>.msg` or `c-<ms>-<id>.msg` taken apart.
fn parse_message_name(name: &str) -> Option<(char, u64, &str)> {
    let (kind, rest) = (name.chars().next()?, name.get(2..)?);
    if !matches!(kind, 'p' | 'c') || name.as_bytes().get(1) != Some(&b'-') {
        return None;
    }
    let rest = rest.strip_suffix(".msg")?;
    let (stamp, id) = rest.split_once('-')?;
    if stamp.len() != 13 || !valid_request_id(id) {
        return None;
    }
    Some((kind, stamp.parse().ok()?, id))
}

/// Parses pool membership from `w-<013-ms>-wh-...` / `w-<013-ms>-we-...`,
/// `wh-<013-ms>-...` / `we-<013-ms>-...`, or legacy `w-<013-ms>-<pid>-<nonce>`
/// (which defaults to [`WaitPool::Hosted`]).
fn parse_waiter_pool(name: &str) -> Option<WaitPool> {
    if name.starts_with("wh-") {
        return Some(WaitPool::Hosted);
    }
    if name.starts_with("we-") {
        return Some(WaitPool::External);
    }
    if !name.starts_with("w-") {
        return None;
    }
    match name.get(16..19) {
        Some("wh-") => Some(WaitPool::Hosted),
        Some("we-") => Some(WaitPool::External),
        _ => Some(WaitPool::Hosted),
    }
}

fn is_waiter_name(name: &str) -> bool {
    parse_waiter_pool(name).is_some()
}

/// Cross-process exclusive lock on a private lock file: `flock` on Unix,
/// `LockFileEx` on Windows, both through [`File::lock`]. The lock is held by
/// this open handle, so a second handle (another thread, another process)
/// blocks until this one is dropped. The lock file is never read or written, so
/// Windows' mandatory byte-range lock gets in nobody's way.
struct AdvisoryLock {
    file: File,
}

impl AdvisoryLock {
    fn acquire(path: &Path) -> Result<Self, MailboxError> {
        Ok(Self::open_locked(path, true)?.expect("a blocking lock is always taken"))
    }

    /// Takes the lock only if it is free right now; `Ok(None)` means another holder has it.
    fn try_acquire(path: &Path) -> Result<Option<Self>, MailboxError> {
        Self::open_locked(path, false)
    }

    fn open_locked(path: &Path, wait: bool) -> Result<Option<Self>, MailboxError> {
        let mut options = OpenOptions::new();
        options.read(true).write(true).create(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600).custom_flags(libc::O_NOFOLLOW);
        }
        let file = options
            .open(path)
            .map_err(|e| io("could not open a mailbox lock", &e))?;
        let meta = file
            .metadata()
            .map_err(|e| io("could not inspect a mailbox lock", &e))?;
        if !meta.is_file() || !owned_privately(&meta) {
            return Err(MailboxError::Insecure(format!(
                "{} is not a private lock file of this user",
                path.display()
            )));
        }
        // A signal can interrupt the blocking `flock`; retry `EINTR` on both
        // blocking and non-blocking acquisitions across Unix and Windows.
        if wait {
            loop {
                match file.lock() {
                    Ok(()) => break,
                    Err(e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
                    Err(e) => return Err(io("could not lock the mailbox", &e)),
                }
            }
        } else {
            loop {
                match file.try_lock() {
                    Ok(()) => break,
                    Err(std::fs::TryLockError::WouldBlock) => return Ok(None),
                    Err(std::fs::TryLockError::Error(e))
                        if e.kind() == std::io::ErrorKind::Interrupted =>
                    {
                        continue
                    }
                    Err(std::fs::TryLockError::Error(e)) => {
                        return Err(io("could not lock the mailbox", &e))
                    }
                }
            }
        }
        Ok(Some(Self { file }))
    }
}

impl Drop for AdvisoryLock {
    fn drop(&mut self) {
        let _ = self.file.unlock();
    }
}

/// The mailbox rooted at one directory.
#[derive(Clone)]
pub(crate) struct Mailbox {
    root: PathBuf,
    /// Optional explicit in-memory issuance ledger (defaults to
    /// [`process_human_issuance_ledger`]).
    ledger: Option<Arc<HumanIssuanceLedger>>,
    /// Optional explicit verifier for `HumanConfirmed` envelopes (defaults to
    /// checking the in-process ledger; `at_default()` also checks the live
    /// control-plane broker over its read-only socket query).
    verifier: Option<Arc<dyn HumanIssuanceVerifier>>,
    /// Whether this mailbox is the default config-dir mailbox eligible to query
    /// `approval-broker.sock` for cross-process `HumanConfirmed` verification.
    query_broker_socket: bool,
}

impl std::fmt::Debug for Mailbox {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Mailbox")
            .field("root", &self.root)
            .field("query_broker_socket", &self.query_broker_socket)
            .finish()
    }
}

impl Mailbox {
    #[cfg_attr(not(test), allow(dead_code))]
    pub(crate) fn new(root: PathBuf) -> Self {
        Self {
            root,
            ledger: None,
            verifier: None,
            query_broker_socket: false,
        }
    }

    /// Constructs a mailbox backed by an explicit issuance verifier (used by
    /// tests and broker-connected hooks).
    #[cfg_attr(not(test), allow(dead_code))]
    pub(crate) fn with_verifier(root: PathBuf, verifier: Arc<dyn HumanIssuanceVerifier>) -> Self {
        Self {
            root,
            ledger: None,
            verifier: Some(verifier),
            query_broker_socket: false,
        }
    }

    /// Constructs a mailbox backed by an isolated in-memory issuance ledger
    /// and matching verifier.
    #[cfg(test)]
    pub(crate) fn with_isolated_ledger(root: PathBuf, ledger: Arc<HumanIssuanceLedger>) -> Self {
        let verifier = Arc::new(LedgerHumanVerifier::new(ledger.clone()));
        Self {
            root,
            ledger: Some(ledger),
            verifier: Some(verifier),
            query_broker_socket: false,
        }
    }

    /// The mailbox beside the notification socket, in GitPulse's configuration
    /// directory.
    pub(crate) fn at_default() -> Result<Self, MailboxError> {
        crate::tool_config::default_config_dir()
            .map(|dir| Self {
                root: dir.join("agent-mailbox"),
                ledger: None,
                verifier: None,
                query_broker_socket: true,
            })
            .ok_or_else(|| MailboxError::Io("There is no GitPulse configuration directory.".into()))
    }

    fn record_human_issuance(&self, session_uuid: &str, request_id: &str, text: &str, created: u64) {
        match &self.ledger {
            Some(ledger) => {
                ledger.record_scoped(&self.root, session_uuid, request_id, text, created);
            }
            None => {
                process_human_issuance_ledger()
                    .record_scoped(&self.root, session_uuid, request_id, text, created);
            }
        }
    }

    fn verify_human_issuance(&self, session_uuid: &str, request_id: &str, text_sha256: &str) -> bool {
        if let Some(verifier) = &self.verifier {
            return verifier.verify_human_issuance(&self.root, session_uuid, request_id, text_sha256);
        }
        if let Some(ledger) = &self.ledger {
            return ledger.verify_scoped(&self.root, session_uuid, request_id, text_sha256);
        }
        if process_human_issuance_ledger()
            .verify_scoped(&self.root, session_uuid, request_id, text_sha256)
        {
            return true;
        }
        if self.query_broker_socket {
            #[cfg(unix)]
            {
                return crate::alerts::approval_broker::verify_human_issuance_via_default_socket(
                    session_uuid,
                    request_id,
                    text_sha256,
                );
            }
        }
        false
    }

    /// Authenticates `provenance` for `(session_uuid, request_id, text)`.
    ///
    /// Any `HumanConfirmed` sidecar that does not match an in-memory issuance
    /// record from the control-plane owner process is downgraded to untrusted
    /// `PeerAgent` framing (`human_verified = false`). Never fails open.
    fn authenticate_provenance(
        &self,
        session_uuid: &str,
        request_id: &str,
        text: &str,
        provenance: EnvelopeProvenance,
    ) -> (EnvelopeProvenance, bool) {
        if provenance == EnvelopeProvenance::HumanConfirmed {
            let digest = text_sha256_hex(text);
            if self.verify_human_issuance(session_uuid, request_id, &digest) {
                (EnvelopeProvenance::HumanConfirmed, true)
            } else {
                log::warn!(
                    target: "mailbox",
                    "downgrading unverified HumanConfirmed envelope (session={session_uuid}, request_id={request_id}) to untrusted PeerAgent framing"
                );
                (downgraded_peer_provenance(request_id), false)
            }
        } else {
            (provenance, false)
        }
    }

    pub(crate) fn session_path(&self, session: &str) -> Result<PathBuf, MailboxError> {
        Ok(self.root.join(session_key(session)?))
    }

    fn ensure_root(&self) -> Result<(), MailboxError> {
        if let Some(parent) = self.root.parent() {
            std::fs::create_dir_all(parent).map_err(|e| io("could not make the mailbox", &e))?;
        }
        private_dir(&self.root)
    }

    fn lock_root(&self) -> Result<AdvisoryLock, MailboxError> {
        self.ensure_root()?;
        AdvisoryLock::acquire(&self.root.join(".lock"))
    }

    fn lock_session(&self, session_uuid: &str) -> Result<AdvisoryLock, MailboxError> {
        self.ensure_root()?;
        let locks_dir = self.root.join(".locks");
        private_dir(&locks_dir)?;
        AdvisoryLock::acquire(&locks_dir.join(format!("{session_uuid}.lock")))
    }

    /// [`Self::lock_session`] that gives up instead of waiting: `Ok(None)` when another holder has
    /// the session's lock.
    fn try_lock_session(&self, session_uuid: &str) -> Result<Option<AdvisoryLock>, MailboxError> {
        self.ensure_root()?;
        let locks_dir = self.root.join(".locks");
        private_dir(&locks_dir)?;
        AdvisoryLock::try_acquire(&locks_dir.join(format!("{session_uuid}.lock")))
    }

    fn meta_path(&self, session_uuid: &str, request_id: &str) -> PathBuf {
        self.root
            .join(".meta")
            .join(session_uuid)
            .join(format!("{request_id}.json"))
    }

    fn write_provenance(
        &self,
        session_uuid: &str,
        request_id: &str,
        provenance: &EnvelopeProvenance,
        now: SystemTime,
    ) -> Result<(), MailboxError> {
        let meta_root = self.root.join(".meta");
        private_dir(&meta_root)?;
        let session_meta = meta_root.join(session_uuid);
        private_dir(&session_meta)?;
        let bytes = serde_json::to_vec(provenance)
            .map_err(|e| MailboxError::Invalid(format!("invalid envelope provenance: {e}")))?;
        let temp = session_meta.join(format!(".tmp-{}-{:08x}", std::process::id(), nonce(now)));
        write_private(&temp, &bytes)?;
        let target = session_meta.join(format!("{request_id}.json"));
        let _ = std::fs::remove_file(&target);
        if let Err(error) = crate::fs_entry::rename_noreplace(&temp, &target) {
            let _ = std::fs::remove_file(&temp);
            return Err(io("could not publish envelope provenance", &error));
        }
        Ok(())
    }

    fn read_provenance(
        &self,
        session_uuid: &str,
        request_id: &str,
    ) -> Result<EnvelopeProvenance, MailboxError> {
        let path = self.meta_path(session_uuid, request_id);
        let mut options = OpenOptions::new();
        options.read(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.custom_flags(libc::O_NOFOLLOW);
        }
        let file = match options.open(&path) {
            Ok(file) => file,
            // Every deposit writes a sidecar, human ones included, so a missing one means the
            // envelope's origin is unknown and it must not be read as a person's. Envelopes
            // deposited before sidecars existed (59178b6c) lived at most `PENDING_TTL` plus
            // `CLAIMED_TTL`, so none survives to be migrated; an older build that still
            // deposits without a sidecar has its envelopes dropped, not trusted.
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                return Err(MailboxError::Invalid(
                    "envelope provenance is missing; its origin is unknown".into(),
                ));
            }
            Err(e) => return Err(io("could not open envelope provenance", &e)),
        };
        let meta = file
            .metadata()
            .map_err(|e| io("could not inspect envelope provenance", &e))?;
        if !meta.is_file() || !owned_privately(&meta) {
            return Err(MailboxError::Insecure(format!(
                "{} is not a private provenance file of this user",
                path.display()
            )));
        }
        if meta.len() > MAX_META_BYTES {
            return Err(MailboxError::Invalid(
                "Envelope provenance exceeds maximum size.".into(),
            ));
        }
        let mut text = String::new();
        file.take(MAX_META_BYTES + 1)
            .read_to_string(&mut text)
            .map_err(|e| io("could not read envelope provenance", &e))?;
        serde_json::from_str(&text)
            .map_err(|e| MailboxError::Invalid(format!("corrupt envelope provenance: {e}")))
    }

    fn remove_provenance(&self, session_uuid: &str, request_id: &str) {
        let _ = std::fs::remove_file(self.meta_path(session_uuid, request_id));
    }

    /// Makes the root and the session's directory if needed.
    fn ensure_session(&self, session: &str, now: SystemTime) -> Result<PathBuf, MailboxError> {
        let dir = self.session_path(session)?;
        let _root_lock = self.lock_root()?;
        match std::fs::symlink_metadata(&dir) {
            Ok(_) => {}
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                self.prune_sessions(now);
                if self.session_names().len() >= MAX_SESSIONS {
                    return Err(MailboxError::Full(format!(
                        "The mailbox already holds {MAX_SESSIONS} sessions."
                    )));
                }
            }
            Err(e) => return Err(io("could not look at the session's mailbox", &e)),
        }
        private_dir(&dir)?;
        Ok(dir)
    }

    fn session_names(&self) -> Vec<String> {
        let Ok(entries) = std::fs::read_dir(&self.root) else {
            return Vec::new();
        };
        entries
            .take(SCAN_LIMIT)
            .flatten()
            .filter_map(|e| e.file_name().into_string().ok())
            .filter(|name| session_key(name).is_ok())
            .collect()
    }

    /// Removes the `.tmp-*` files a crashed counter write left behind, once they are a minute
    /// old (a live write finishes in milliseconds).
    fn remove_stale_counter_temps(&self, now: SystemTime) {
        for subdir in [".hw", ".epochs", ".continuations", ".stop_continued"] {
            let Ok(entries) = std::fs::read_dir(self.root.join(subdir)) else {
                continue;
            };
            for entry in entries.take(SCAN_LIMIT).flatten() {
                let stale = entry.file_name().to_string_lossy().starts_with(".tmp-")
                    && entry
                        .metadata()
                        .and_then(|meta| meta.modified())
                        .is_ok_and(|modified| age(now, modified) > Duration::from_secs(60));
                if stale {
                    let _ = std::fs::remove_file(entry.path());
                }
            }
        }
    }

    /// Removes expired files from every session and empty sessions that have
    /// sat unused. Best effort: a stale file costs bytes, not correctness.
    fn prune_sessions(&self, now: SystemTime) {
        self.remove_stale_counter_temps(now);
        for name in self.session_names() {
            let dir = self.root.join(&name);
            // Under the session's lock, like every other path that removes a message and its
            // sidecar: unlocked, the sidecar removal here could delete one a concurrent deposit
            // of the same request id had just written for its new message. No path takes a
            // session lock and then the root lock, so root -> session cannot deadlock.
            // Pruning is best effort and holds the root lock, so a session whose lock is
            // held (or cannot be taken) is skipped rather than waited on: one stalled holder
            // must not stall every deposit and waiter registration behind the root lock.
            let Ok(Some(_session_lock)) = self.try_lock_session(&name) else {
                continue;
            };
            self.expire_session(&name, &dir, now);
            let empty = std::fs::read_dir(&dir).is_ok_and(|mut d| d.next().is_none());
            let old = std::fs::metadata(&dir)
                .and_then(|m| m.modified())
                .is_ok_and(|m| age(now, m) > EMPTY_SESSION_TTL);
            if empty && old && std::fs::remove_dir(&dir).is_ok() {
                let _ = std::fs::remove_file(self.root.join(".locks").join(format!("{name}.lock")));
                let _ = std::fs::remove_file(self.root.join(".hw").join(&name));
                let _ = std::fs::remove_file(self.root.join(".epochs").join(&name));
                let _ = std::fs::remove_file(self.root.join(".continuations").join(&name));
                let _ = std::fs::remove_file(self.root.join(".stop_continued").join(&name));
                let _ = std::fs::remove_dir_all(self.root.join(".meta").join(&name));
            }
        }
    }

    /// Removes what has outlived its time.
    fn expire_session(&self, session_uuid: &str, dir: &Path, now: SystemTime) {
        for (name, modified) in regular_files(dir) {
            let ttl = if name.starts_with("p-") {
                PENDING_TTL
            } else if name.starts_with("c-") {
                CLAIMED_TTL
            } else if name.starts_with(".tmp-") {
                TEMP_TTL
            } else if is_waiter_name(&name) {
                WAITER_STALE
            } else {
                continue;
            };
            if age(now, modified) > ttl && std::fs::remove_file(dir.join(&name)).is_ok() {
                if let Some((_, _, id)) = parse_message_name(&name) {
                    self.remove_provenance(session_uuid, id);
                }
            }
        }
    }

    /// The creation stamp for the next envelope of a session: the caller's clock, but strictly
    /// above every stamp this session has issued or holds (a persisted high-water mark and the
    /// files present). Callers stamp before they wait for the lock, and a clock can step back, so
    /// the raw clock does not follow publish order; a reader that has seen stamp T must never
    /// find a later publish below it. Must be called with the session lock held.
    ///
    /// The mark is read through the counter reader (no symlinks, a private file of this user; only
    /// a missing file means none yet). An unparseable mark falls back to the stamps of the files
    /// present, never to zero, and is rewritten. A stamp is 13 digits in a file name, so one past
    /// [`MAX_STAMP`] is refused rather than minting a name that cannot be parsed back.
    fn next_stamp(
        &self,
        session_key: &str,
        files: &[(String, SystemTime)],
        now: SystemTime,
    ) -> Result<u64, MailboxError> {
        let persisted = match self.read_counter_text(".hw", session_key)? {
            None => 0,
            Some(text) => text.trim().parse::<u64>().unwrap_or_else(|_| {
                log::warn!(target: "mailbox", "unreadable stamp high-water mark for a session; using the stamps of its files");
                0
            }),
        };
        let held = files
            .iter()
            .filter_map(|(name, _)| parse_message_name(name).map(|(_, stamp, _)| stamp))
            .max()
            .unwrap_or(0);
        let stamp = millis(now).max(persisted.max(held).saturating_add(1));
        if stamp > MAX_STAMP {
            return Err(MailboxError::Invalid(
                "This session's mailbox stamps are exhausted, so it cannot take another message."
                    .into(),
            ));
        }
        self.write_u64_counter(".hw", session_key, stamp, now)?;
        Ok(stamp)
    }

    /// Puts one human-confirmed message in a session's mailbox.
    pub(crate) fn deposit(
        &self,
        session: &str,
        request_id: &str,
        text: &str,
        now: SystemTime,
    ) -> Result<Deposit, MailboxError> {
        self.deposit_with_provenance(
            session,
            request_id,
            text,
            EnvelopeProvenance::HumanConfirmed,
            now,
        )
    }

    /// Puts one message with explicit provenance in a session's mailbox.
    ///
    /// Normalizes `\r\n` -> `\n` prior to validation and storage, and holds a
    /// cross-process advisory `flock` across duplicate check, pending-cap check,
    /// provenance sidecar write, and no-replace publish.
    pub(crate) fn deposit_with_provenance(
        &self,
        session: &str,
        request_id: &str,
        text: &str,
        provenance: EnvelopeProvenance,
        now: SystemTime,
    ) -> Result<Deposit, MailboxError> {
        let normalized = normalize_crlf(text);
        validate_message(&normalized)?;
        if !valid_request_id(request_id) {
            return Err(MailboxError::Invalid(
                "A request id is 1 to 64 letters, digits, - or _.".into(),
            ));
        }
        let key = session_key(session)?;
        let _in_proc = DEPOSIT_LOCK
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let dir = self.ensure_session(&key, now)?;
        let _session_lock = self.lock_session(&key)?;
        self.expire_session(&key, &dir, now);
        let files = regular_files(&dir);
        let overflow_notice = is_overflow_notice(request_id, &provenance);
        for (name, _) in &files {
            if let Some((kind, stamp, id)) = parse_message_name(name) {
                if id == request_id {
                    if overflow_notice && kind == 'c' {
                        // The recipient already took an earlier notice. A new overflow needs a
                        // fresh one, so the claimed copy must not make it a duplicate.
                        std::fs::remove_file(dir.join(name))
                            .map_err(|e| io("could not clear the claimed overflow notice", &e))?;
                        self.remove_provenance(&key, id);
                        continue;
                    }
                    let same_text =
                        read_message(&dir.join(name)).is_ok_and(|held| held == normalized.as_ref());
                    let same_prov = self
                        .read_provenance(&key, request_id)
                        .is_ok_and(|held| held == provenance);
                    if !same_text || !same_prov {
                        return Err(MailboxError::Invalid(
                            "That request id already carried a different message.".into(),
                        ));
                    }
                    if provenance == EnvelopeProvenance::HumanConfirmed {
                        self.record_human_issuance(&key, request_id, normalized.as_ref(), stamp);
                    }
                    return Ok(Deposit {
                        request_id: request_id.to_owned(),
                        created_at_ms: stamp,
                        expires_at_ms: stamp + millis_of(PENDING_TTL),
                        duplicate: true,
                    });
                }
            }
        }
        let mut pending = 0usize;
        let mut peer_pending = 0usize;
        let counts_as_peer =
            matches!(provenance, EnvelopeProvenance::PeerAgent { .. }) && !overflow_notice;
        let steward_repo = match &provenance {
            EnvelopeProvenance::SystemSteward { repo_root, .. } => Some(repo_root.as_str()),
            _ => None,
        };
        for (name, _) in &files {
            if !name.starts_with("p-") {
                continue;
            }
            if let Some(new_repo) = steward_repo {
                if let Some((_, _, id)) = parse_message_name(name) {
                    if id != request_id
                        && matches!(
                            self.read_provenance(&key, id),
                            Ok(EnvelopeProvenance::SystemSteward { ref repo_root, .. })
                                if same_steward_repo(repo_root, new_repo)
                        )
                    {
                        // Supersede older unclaimed steward advisories for this repository so
                        // periodic host directives never consume more than one mailbox slot.
                        let _ = std::fs::remove_file(dir.join(name));
                        self.remove_provenance(&key, id);
                        continue;
                    }
                }
            }
            pending += 1;
            if counts_as_peer {
                if let Some((_, _, id)) = parse_message_name(name) {
                    if matches!(
                        self.read_provenance(&key, id),
                        Ok(ref held @ EnvelopeProvenance::PeerAgent { .. })
                            if !is_overflow_notice(id, held)
                    ) {
                        peer_pending += 1;
                    }
                }
            }
        }
        if counts_as_peer && peer_pending >= MAX_PEER_PENDING {
            return Err(MailboxError::Full(format!(
                "This session already has {MAX_PEER_PENDING} peer messages waiting for it."
            )));
        }
        if pending >= MAX_PENDING {
            return Err(MailboxError::Full(format!(
                "This session already has {MAX_PENDING} messages waiting for it."
            )));
        }
        // Assigned here, under the session lock, so the stamp order is the publish order a
        // cursor-reading peeker sees (see `next_stamp`).
        let created = self.next_stamp(&key, &files, now)?;
        self.write_provenance(&key, request_id, &provenance, now)?;
        let temp = dir.join(format!(".tmp-{}-{:08x}", std::process::id(), nonce(now)));
        if let Err(err) = write_private(&temp, normalized.as_bytes()) {
            self.remove_provenance(&key, request_id);
            return Err(err);
        }
        let target = dir.join(format!("p-{created:013}-{request_id}.msg"));
        if let Err(error) = crate::fs_entry::rename_noreplace(&temp, &target) {
            let _ = std::fs::remove_file(&temp);
            self.remove_provenance(&key, request_id);
            return Err(io("could not publish the message", &error));
        }
        if provenance == EnvelopeProvenance::HumanConfirmed {
            self.record_human_issuance(&key, request_id, normalized.as_ref(), created);
        }
        Ok(Deposit {
            request_id: request_id.to_owned(),
            created_at_ms: created,
            expires_at_ms: created + millis_of(PENDING_TTL),
            duplicate: false,
        })
    }

    /// Deposits one peer-agent envelope, or, when the peer cap is reached, leaves the
    /// repository's single overflow notice in the reserved last slot.
    ///
    /// The notice tells the recipient that more peer messages are on the bus and how to read
    /// them. Only `PeerAgent` provenance is held to the peer cap; any other provenance is
    /// deposited as `deposit_with_provenance` would.
    pub(crate) fn deposit_peer_or_overflow(
        &self,
        session: &str,
        request_id: &str,
        text: &str,
        provenance: EnvelopeProvenance,
        now: SystemTime,
    ) -> Result<PeerDeposit, MailboxError> {
        let (repo_root, max_hops) = match &provenance {
            EnvelopeProvenance::PeerAgent {
                repo_root,
                max_hops,
                ..
            } => (repo_root.clone(), *max_hops),
            _ => {
                return self
                    .deposit_with_provenance(session, request_id, text, provenance, now)
                    .map(PeerDeposit::Deposited)
            }
        };
        match self.deposit_with_provenance(session, request_id, text, provenance, now) {
            Err(MailboxError::Full(_)) => {}
            other => return other.map(PeerDeposit::Deposited),
        }
        let for_peer = session.to_ascii_lowercase();
        let notice_text = format!(
            "More peer messages are waiting on the GitPulse agent bus for this session in {repo_root}. \
             Call gitpulse_agent_inbox with repo_path={repo_root} and for_peer={for_peer} to read them; \
             pass ack=true to mark what it reads as read, or page with next_since_seq (as since_seq) and mailbox_cursor_ms (as since_mailbox_ms)."
        );
        let notice_id = overflow_notice_id(&repo_root);
        let notice = EnvelopeProvenance::PeerAgent {
            msg_id: notice_id.clone(),
            from_peer: OVERFLOW_FROM_PEER.to_owned(),
            provider: OVERFLOW_FROM_PEER.to_owned(),
            branch: String::new(),
            thread_key: String::new(),
            hop: 0,
            max_hops,
            truncated_in_bus: false,
            repo_root,
        };
        Ok(PeerDeposit::Overflowed {
            notice: self.deposit_with_provenance(session, &notice_id, &notice_text, notice, now),
        })
    }

    /// Takes the oldest pending message, if there is one, for this caller alone.
    #[cfg(test)]
    pub(crate) fn claim_next(
        &self,
        session: &str,
        now: SystemTime,
    ) -> Result<Option<Claim>, MailboxError> {
        Ok(self
            .try_claim_batch(session, 1, now)?
            .pop()
            .map(|env| env.claim))
    }

    /// Whether `session` has a pending (`p-*`) envelope on disk, answered
    /// without the session's `flock`, so the peer-inbox hook that runs on every
    /// tool call does not take a cross-process lock to learn there is nothing
    /// to read.
    ///
    /// It looks at the same first [`SCAN_LIMIT`] entries the claim does, and
    /// counts a `p-*` name the claim might still skip (a foreign owner, a
    /// symlink), so it can only say yes too often, never no when the claim
    /// would have found one. Expired envelopes are left for the next claim to
    /// remove; nothing is lost by not expiring them here.
    pub(crate) fn has_unclaimed(&self, session: &str) -> Result<bool, MailboxError> {
        let key = session_key(session)?;
        let dir = self.root.join(&key);
        let entries = match std::fs::read_dir(&dir) {
            Ok(entries) => entries,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(false),
            Err(e) => return Err(io("could not look at the session's mailbox", &e)),
        };
        Ok(entries.take(SCAN_LIMIT).flatten().any(|entry| {
            entry
                .file_name()
                .to_str()
                .is_some_and(|name| name.starts_with("p-") && parse_message_name(name).is_some())
        }))
    }

    /// Non-blocking batch claim of up to `max_items` oldest pending envelopes
    /// under the session's cross-process advisory `flock`.
    #[cfg(test)]
    pub(crate) fn try_claim_batch(
        &self,
        session: &str,
        max_items: usize,
        now: SystemTime,
    ) -> Result<Vec<ClaimedEnvelope>, MailboxError> {
        self.try_claim_batch_filtered(session, max_items, now, |_| true)
    }

    /// Non-blocking batch claim of up to `max_items` oldest pending envelopes
    /// whose provenance passes `accept`. Envelopes rejected by `accept` stay
    /// pending on disk for manual/boundary inspection.
    pub(crate) fn try_claim_batch_filtered<F>(
        &self,
        session: &str,
        max_items: usize,
        now: SystemTime,
        accept: F,
    ) -> Result<Vec<ClaimedEnvelope>, MailboxError>
    where
        F: Fn(&EnvelopeProvenance) -> bool,
    {
        self.claim_batch_reporting_refused(session, max_items, now, accept)
            .map(|(claimed, _)| claimed)
    }

    /// [`Self::try_claim_batch_filtered`] that also reports whether `accept` refused at least one
    /// pending envelope, decided under the same lock acquisition as the claim.
    fn claim_batch_reporting_refused<F>(
        &self,
        session: &str,
        max_items: usize,
        now: SystemTime,
        accept: F,
    ) -> Result<(Vec<ClaimedEnvelope>, bool), MailboxError>
    where
        F: Fn(&EnvelopeProvenance) -> bool,
    {
        if max_items == 0 {
            return Ok((Vec::new(), false));
        }
        let key = session_key(session)?;
        let dir = self.root.join(&key);
        match std::fs::symlink_metadata(&dir) {
            Ok(_) => private_dir(&dir)?,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok((Vec::new(), false)),
            Err(e) => return Err(io("could not look at the session's mailbox", &e)),
        }
        let _session_lock = self.lock_session(&key)?;
        self.expire_session(&key, &dir, now);
        let mut pending: Vec<String> = regular_files(&dir)
            .into_iter()
            .map(|(name, _)| name)
            .filter(|name| name.starts_with("p-") && parse_message_name(name).is_some())
            .collect();
        pending.sort();
        let mut claimed_batch = Vec::new();
        let mut refused = false;
        for name in pending {
            if claimed_batch.len() >= max_items {
                break;
            }
            if let Some((_, _, id)) = parse_message_name(&name) {
                if let (Ok(text), Ok(prov)) =
                    (read_message(&dir.join(&name)), self.read_provenance(&key, id))
                {
                    let (effective, _) = self.authenticate_provenance(&key, id, &text, prov);
                    if !accept(&effective) {
                        refused = true;
                        continue;
                    }
                }
            }
            if !rename_to_claimed(&dir, &name, now)? {
                continue;
            }
            let claimed = format!("c-{}", &name[2..]);
            let (created_at_ms, id) = parse_message_name(&claimed)
                .map(|(_, stamp, id)| (stamp, id.to_owned()))
                .unwrap_or_default();
            match (
                read_message(&dir.join(&claimed)),
                self.read_provenance(&key, &id),
            ) {
                (Ok(text), Ok(provenance)) => {
                    let (provenance, human_verified) =
                        self.authenticate_provenance(&key, &id, &text, provenance);
                    claimed_batch.push(ClaimedEnvelope {
                        claim: Claim {
                            request_id: id,
                            text,
                        },
                        provenance,
                        created_at_ms,
                        human_verified,
                    });
                }
                (Err(error), _) | (_, Err(error)) => {
                    log::warn!(target: "mailbox", "dropped an unreadable envelope: {error}");
                    let _ = std::fs::remove_file(dir.join(&claimed));
                    self.remove_provenance(&key, &id);
                }
            }
        }
        Ok((claimed_batch, refused))
    }

    /// Non-destructively reads up to `max_items` oldest pending envelopes
    /// without renaming `p-*` -> `c-*` (used when `gitpulse_agent_inbox` is
    /// called with `ack: false`).
    pub(crate) fn peek_pending(
        &self,
        session: &str,
        max_items: usize,
        now: SystemTime,
    ) -> Result<Vec<ClaimedEnvelope>, MailboxError> {
        Ok(self
            .peek_named(session, max_items, now, |_| true)?
            .into_iter()
            .map(|(_, envelope)| envelope)
            .collect())
    }

    /// The oldest `max_items` pending envelopes whose provenance `accept`
    /// takes, each with the `p-*` file it was read from. Nothing is renamed.
    ///
    /// An envelope that cannot be read (its text or its provenance sidecar is
    /// gone or unusable) is skipped and logged rather than counted against
    /// `max_items`: it stays on disk until it expires, and cannot hide the
    /// readable messages behind it.
    fn peek_named<F>(
        &self,
        session: &str,
        max_items: usize,
        now: SystemTime,
        accept: F,
    ) -> Result<Vec<(String, ClaimedEnvelope)>, MailboxError>
    where
        F: Fn(&EnvelopeProvenance) -> bool,
    {
        if max_items == 0 {
            return Ok(Vec::new());
        }
        let key = session_key(session)?;
        let dir = self.root.join(&key);
        match std::fs::symlink_metadata(&dir) {
            Ok(_) => private_dir(&dir)?,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
            Err(e) => return Err(io("could not look at the session's mailbox", &e)),
        }
        let _session_lock = self.lock_session(&key)?;
        self.expire_session(&key, &dir, now);
        let mut pending: Vec<String> = regular_files(&dir)
            .into_iter()
            .map(|(name, _)| name)
            .filter(|name| name.starts_with("p-") && parse_message_name(name).is_some())
            .collect();
        pending.sort();
        let mut out = Vec::new();
        for name in pending {
            if out.len() >= max_items {
                break;
            }
            let Some((_, created_at_ms, id)) = parse_message_name(&name) else {
                continue;
            };
            match (
                read_message(&dir.join(&name)),
                self.read_provenance(&key, id),
            ) {
                (Ok(text), Ok(provenance)) => {
                    let (provenance, human_verified) =
                        self.authenticate_provenance(&key, id, &text, provenance);
                    if accept(&provenance) {
                        out.push((
                            name.clone(),
                            ClaimedEnvelope {
                                claim: Claim {
                                    request_id: id.to_owned(),
                                    text,
                                },
                                provenance,
                                created_at_ms,
                                human_verified,
                            },
                        ));
                    }
                }
                (Err(error), _) | (_, Err(error)) => {
                    log::warn!(target: "mailbox", "left an unreadable pending message in place: {error}");
                }
            }
        }
        Ok(out)
    }

    /// First half of a two-phase delivery: reads the oldest pending envelopes
    /// that `accept` takes and builds, for each, whatever `build` makes of it
    /// (the text that will be shown), WITHOUT consuming any of them. Nothing
    /// leaves `p-*` until [`PendingDelivery::commit`], so a caller that is cut
    /// off while it builds (a budget that ran out, a crash) has lost nothing.
    pub(crate) fn prepare_delivery<T, F>(
        &self,
        session: &str,
        max_items: usize,
        now: SystemTime,
        accept: F,
        mut build: impl FnMut(&ClaimedEnvelope) -> T,
    ) -> Result<PendingDelivery<T, F>, MailboxError>
    where
        F: Fn(&EnvelopeProvenance) -> bool,
    {
        let key = session_key(session)?;
        let items = self
            .peek_named(session, max_items, now, &accept)?
            .into_iter()
            .map(|(file, envelope)| {
                let built = build(&envelope);
                PendingItem {
                    file,
                    envelope,
                    built,
                }
            })
            .collect();
        Ok(PendingDelivery {
            mailbox: self.clone(),
            key,
            items,
            accept,
        })
    }

    /// Atomically marks a specific pending `request_id` (`msg_id`) as claimed
    /// (`p-*` -> `c-*`) when acknowledged directly via MCP (`gitpulse_agent_wait`
    /// or `gitpulse_agent_inbox`), preventing later hook double-delivery.
    pub(crate) fn mark_claimed_by_id(
        &self,
        session: &str,
        request_id: &str,
        now: SystemTime,
    ) -> Result<bool, MailboxError> {
        if !valid_request_id(request_id) {
            return Err(MailboxError::Invalid(
                "A request id is 1 to 64 letters, digits, - or _.".into(),
            ));
        }
        let key = session_key(session)?;
        let dir = self.root.join(&key);
        match std::fs::symlink_metadata(&dir) {
            Ok(_) => private_dir(&dir)?,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(false),
            Err(e) => return Err(io("could not look at the session's mailbox", &e)),
        }
        let _session_lock = self.lock_session(&key)?;
        self.expire_session(&key, &dir, now);
        for (name, _) in regular_files(&dir) {
            if let Some(('p', _, id)) = parse_message_name(&name) {
                if id == request_id {
                    return rename_to_claimed(&dir, &name, now);
                }
            }
        }
        Ok(false)
    }

    /// Checks whether unread pending messages exist for `session` without
    /// claiming them (used by `cmd_terminal_deliver_peer` so modal prompts do
    /// not drop messages).
    pub(crate) fn has_pending(&self, session: &str, now: SystemTime) -> Result<bool, MailboxError> {
        let key = session_key(session)?;
        let dir = self.root.join(&key);
        match std::fs::symlink_metadata(&dir) {
            Ok(_) => private_dir(&dir)?,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(false),
            Err(e) => return Err(io("could not look at the session's mailbox", &e)),
        }
        let _session_lock = self.lock_session(&key)?;
        self.expire_session(&key, &dir, now);
        Ok(regular_files(&dir)
            .iter()
            .any(|(n, _)| n.starts_with("p-") && parse_message_name(n).is_some()))
    }

    fn read_u64_counter(&self, subdir: &str, session_uuid: &str) -> Result<u64, MailboxError> {
        Ok(self
            .read_counter_text(subdir, session_uuid)?
            .and_then(|text| text.trim().parse::<u64>().ok())
            .unwrap_or(0))
    }

    /// The text of a counter file: `None` only when it does not exist. Opened without following
    /// symlinks and checked to be a regular file private to this user.
    fn read_counter_text(
        &self,
        subdir: &str,
        session_uuid: &str,
    ) -> Result<Option<String>, MailboxError> {
        let path = self.root.join(subdir).join(session_uuid);
        let mut options = OpenOptions::new();
        options.read(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.custom_flags(libc::O_NOFOLLOW);
        }
        let file = match options.open(&path) {
            Ok(file) => file,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(e) => return Err(io("could not open mailbox counter", &e)),
        };
        let meta = file
            .metadata()
            .map_err(|e| io("could not inspect mailbox counter", &e))?;
        if !meta.is_file() || !owned_privately(&meta) {
            return Err(MailboxError::Insecure(format!(
                "{} is not a private counter file of this user",
                path.display()
            )));
        }
        let mut buf = String::new();
        file.take(64)
            .read_to_string(&mut buf)
            .map_err(|e| io("could not read mailbox counter", &e))?;
        Ok(Some(buf))
    }

    fn write_u64_counter(
        &self,
        subdir: &str,
        session_uuid: &str,
        value: u64,
        now: SystemTime,
    ) -> Result<(), MailboxError> {
        let dir = self.root.join(subdir);
        private_dir(&dir)?;
        let temp = dir.join(format!(".tmp-{}-{:08x}", std::process::id(), nonce(now)));
        write_private(&temp, value.to_string().as_bytes())?;
        let target = dir.join(session_uuid);
        let _ = std::fs::remove_file(&target);
        if let Err(error) = crate::fs_entry::rename_noreplace(&temp, &target) {
            let _ = std::fs::remove_file(&temp);
            return Err(io("could not update mailbox counter", &error));
        }
        Ok(())
    }

    /// Increments `turn_epoch` on a new user turn (`UserPromptSubmit` /
    /// `PreInvocation`), displacing any lingering `Stop` waiters from earlier
    /// turns and resetting the per-turn continuation counter to `0`.
    pub(crate) fn bump_turn_epoch(&self, session: &str) -> Result<u64, MailboxError> {
        let key = session_key(session)?;
        let _session_lock = self.lock_session(&key)?;
        let now = SystemTime::now();
        let next = self.read_u64_counter(".epochs", &key)?.saturating_add(1);
        self.write_u64_counter(".epochs", &key, next, now)?;
        self.write_u64_counter(".continuations", &key, 0, now)?;
        self.write_u64_counter(".stop_continued", &key, 0, now)?;
        Ok(next)
    }

    /// Starts an Antigravity `PreInvocation(invocationNum == 1)` turn: if the
    /// immediately preceding `Stop` returned `decision: "continue"`, consumes
    /// `.stop_continued` without resetting `.continuations` or bumping `.epochs`
    /// and returns `Ok(false)`. Otherwise bumps `.epochs`, resets `.continuations`
    /// to `0`, and returns `Ok(true)`.
    pub(crate) fn begin_user_turn(&self, session: &str) -> Result<bool, MailboxError> {
        let key = session_key(session)?;
        let _session_lock = self.lock_session(&key)?;
        let now = SystemTime::now();
        if self.read_u64_counter(".stop_continued", &key)? > 0 {
            self.write_u64_counter(".stop_continued", &key, 0, now)?;
            return Ok(false);
        }
        let next = self.read_u64_counter(".epochs", &key)?.saturating_add(1);
        self.write_u64_counter(".epochs", &key, next, now)?;
        self.write_u64_counter(".continuations", &key, 0, now)?;
        self.write_u64_counter(".stop_continued", &key, 0, now)?;
        Ok(true)
    }

    /// Reads the current per-turn continuation count for Antigravity hooks.
    pub(crate) fn continuation_count(&self, session: &str) -> Result<u32, MailboxError> {
        let key = session_key(session)?;
        let raw = self.read_u64_counter(".continuations", &key)?;
        Ok(u32::try_from(raw).unwrap_or(u32::MAX))
    }

    /// Increments and returns the per-turn continuation count under lock.
    pub(crate) fn bump_continuation(&self, session: &str) -> Result<u32, MailboxError> {
        let key = session_key(session)?;
        let _session_lock = self.lock_session(&key)?;
        let now = SystemTime::now();
        let next = self
            .read_u64_counter(".continuations", &key)?
            .saturating_add(1);
        self.write_u64_counter(".continuations", &key, next, now)?;
        Ok(u32::try_from(next).unwrap_or(u32::MAX))
    }

    /// Increments the per-turn continuation count for `Stop:continue` and marks
    /// `.stop_continued = 1` so the immediate follow-up `PreInvocation(1)` does
    /// not wipe the cap.
    pub(crate) fn bump_continuation_from_stop(&self, session: &str) -> Result<u32, MailboxError> {
        let key = session_key(session)?;
        let _session_lock = self.lock_session(&key)?;
        let now = SystemTime::now();
        let next = self
            .read_u64_counter(".continuations", &key)?
            .saturating_add(1);
        self.write_u64_counter(".continuations", &key, next, now)?;
        self.write_u64_counter(".stop_continued", &key, 1, now)?;
        Ok(u32::try_from(next).unwrap_or(u32::MAX))
    }

    /// Clears `.stop_continued` when a step or normal `Stop` finishes without
    /// `Stop:continue`.
    pub(crate) fn clear_stop_continue(&self, session: &str) -> Result<(), MailboxError> {
        let key = session_key(session)?;
        let _session_lock = self.lock_session(&key)?;
        self.write_u64_counter(".stop_continued", &key, 0, SystemTime::now())
    }

    /// Where a deposited message stands.
    pub(crate) fn status(&self, session: &str, request_id: &str) -> Delivery {
        let Ok(dir) = self.session_path(session) else {
            return Delivery::Absent;
        };
        for (name, _) in regular_files(&dir) {
            if let Some((kind, _, id)) = parse_message_name(&name) {
                if id == request_id {
                    return if kind == 'p' {
                        Delivery::Pending
                    } else {
                        Delivery::Claimed
                    };
                }
            }
        }
        Delivery::Absent
    }

    /// Messages waiting in a session's mailbox (test helper).
    #[cfg(test)]
    pub(crate) fn pending(&self, session: &str) -> usize {
        self.session_path(session).map_or(0, |dir| {
            regular_files(&dir)
                .iter()
                .filter(|(n, _)| n.starts_with("p-") && parse_message_name(n).is_some())
                .count()
        })
    }

    /// Whether a hook is waiting on this session right now, by the beats its
    /// waiters leave.
    pub(crate) fn listening(&self, session: &str, now: SystemTime) -> bool {
        self.session_path(session).is_ok_and(|dir| {
            regular_files(&dir)
                .iter()
                .any(|(n, modified)| is_waiter_name(n) && age(now, *modified) <= WAITER_STALE)
        })
    }

    /// Live waiters as `(session, file name)`, oldest first.
    fn live_waiters(&self, now: SystemTime) -> Vec<(String, String)> {
        let mut found = Vec::new();
        for session in self.session_names() {
            for (name, modified) in regular_files(&self.root.join(&session)) {
                if is_waiter_name(&name) && age(now, modified) <= WAITER_STALE {
                    found.push((session.clone(), name));
                }
            }
        }
        found.sort_by(|a, b| a.1.cmp(&b.1));
        found
    }

    /// Waits for a plain [`Claim`] under the global [`MAX_WAITERS_TOTAL`] cap.
    #[cfg(test)]
    pub(crate) fn wait(&self, session: &str, limit: Duration) -> Result<Wait, MailboxError> {
        match self.wait_with_cap(session, limit, WaitPool::Hosted, MAX_WAITERS_TOTAL, |_| {
            true
        })? {
            EnvelopeWait::Message(env) => Ok(Wait::Message(env.claim)),
            EnvelopeWait::TimedOut => Ok(Wait::TimedOut),
            EnvelopeWait::Declined(why) => Ok(Wait::Declined(why)),
        }
    }

    /// Waits for a [`ClaimedEnvelope`] accepted by `accept` under the global
    /// [`MAX_WAITERS_TOTAL`] cap.
    #[cfg(test)]
    pub(crate) fn wait_filtered<F>(
        &self,
        session: &str,
        limit: Duration,
        accept: F,
    ) -> Result<EnvelopeWait, MailboxError>
    where
        F: Fn(&EnvelopeProvenance) -> bool,
    {
        self.wait_with_cap(session, limit, WaitPool::Hosted, MAX_WAITERS_TOTAL, accept)
    }

    /// Waits in `pool` for a [`ClaimedEnvelope`], enforcing partitioned
    /// hosted/external quotas and `turn_epoch` displacement.
    #[cfg_attr(not(test), allow(dead_code))]
    pub(crate) fn wait_in_pool(
        &self,
        session: &str,
        limit: Duration,
        pool: WaitPool,
    ) -> Result<EnvelopeWait, MailboxError> {
        self.wait_in_pool_filtered(session, limit, pool, |_| true)
    }

    /// Waits in `pool` for an envelope accepted by `accept`. An envelope `accept` refuses (for
    /// example a peer message that has reached `max_hops`) is never claimed: it stays pending
    /// for manual/boundary inspection, and the waiter looks past it to the first acceptable
    /// envelope and keeps waiting while only refused ones are pending, as it would on an empty
    /// mailbox. Only when the limit passes with a refused envelope still pending does it return
    /// `Declined` ("paused") rather than `TimedOut`.
    pub(crate) fn wait_in_pool_filtered<F>(
        &self,
        session: &str,
        limit: Duration,
        pool: WaitPool,
        accept: F,
    ) -> Result<EnvelopeWait, MailboxError>
    where
        F: Fn(&EnvelopeProvenance) -> bool,
    {
        let pool_cap = match pool {
            WaitPool::Hosted => MAX_WAITERS_HOSTED,
            WaitPool::External => MAX_WAITERS_EXTERNAL,
        };
        self.wait_with_cap(session, limit, pool, pool_cap, accept)
    }

    fn wait_with_cap<F>(
        &self,
        session: &str,
        limit: Duration,
        pool: WaitPool,
        pool_cap: usize,
        accept: F,
    ) -> Result<EnvelopeWait, MailboxError>
    where
        F: Fn(&EnvelopeProvenance) -> bool,
    {
        let started = Instant::now();
        let key = session_key(session)?;
        let dir = self.ensure_session(&key, SystemTime::now())?;
        let mut waiter = match Waiter::register_in_pool_with_cap(self, &dir, &key, pool, pool_cap) {
            Ok(waiter) => waiter,
            Err(MailboxError::Full(why)) => return Ok(EnvelopeWait::Declined(why)),
            Err(other) => return Err(other),
        };
        let mut last_beat = Instant::now();
        loop {
            if let Some(reason) = waiter.displaced_reason()? {
                return Ok(EnvelopeWait::Declined(reason));
            }
            // One lock acquisition both claims the first acceptable envelope (skipping any
            // `accept` refuses, such as a peer message at its hop limit) and notes whether a
            // refused one is pending, so the two cannot disagree.
            let (mut claimed, refused) =
                self.claim_batch_reporting_refused(&key, 1, SystemTime::now(), &accept)?;
            if let Some(envelope) = claimed.pop() {
                return Ok(EnvelopeWait::Message(envelope));
            }
            let spent = started.elapsed();
            if spent >= limit {
                // Refused envelopes do not end the wait early (an acceptable one may still
                // arrive), but one still pending at the limit is reported as paused.
                return Ok(if refused {
                    EnvelopeWait::Declined(PAUSED_AT_HOP_LIMIT.into())
                } else {
                    EnvelopeWait::TimedOut
                });
            }
            if last_beat.elapsed() >= HEARTBEAT {
                waiter.beat();
                last_beat = Instant::now();
            }
            std::thread::sleep(POLL.min(limit - spent));
        }
    }
}

/// Renames one pending `p-*` file to its `c-*` name. `Ok(false)` means the file
/// was no longer there: another consumer took it first, or it expired.
///
/// A rename keeps the file's modification time, which is when the message was deposited, and
/// [`CLAIMED_TTL`] is measured from it. A message that sat pending for most of its window
/// would lose its claimed copy almost at once, and an idempotent retry of its request id would
/// then deliver it a second time. So the claimed file is restamped with the claim time.
fn rename_to_claimed(
    dir: &Path,
    pending_name: &str,
    now: SystemTime,
) -> Result<bool, MailboxError> {
    let claimed = format!("c-{}", &pending_name[2..]);
    let claimed_path = dir.join(claimed);
    match crate::fs_entry::rename_noreplace(&dir.join(pending_name), &claimed_path) {
        Ok(()) => {
            restamp(&claimed_path, now);
            Ok(true)
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(false),
        Err(e) => Err(io("could not claim a message", &e)),
    }
}

/// Sets a claimed file's modification time to the claim time. Best effort: failing leaves the
/// deposit time, which only shortens how long a retry is recognised.
fn restamp(path: &Path, at: SystemTime) {
    let mut options = OpenOptions::new();
    options.write(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.custom_flags(libc::O_NOFOLLOW);
    }
    if let Err(error) = options.open(path).and_then(|file| file.set_modified(at)) {
        log::warn!(target: "mailbox", "could not restamp a claimed message: {error}");
    }
}

struct PendingItem<T> {
    file: String,
    envelope: ClaimedEnvelope,
    built: T,
}

/// Envelopes read and prepared for delivery but not yet consumed; see
/// [`Mailbox::prepare_delivery`]. Dropping it consumes nothing.
pub(crate) struct PendingDelivery<T, F> {
    mailbox: Mailbox,
    key: String,
    items: Vec<PendingItem<T>>,
    accept: F,
}

impl<T, F> PendingDelivery<T, F>
where
    F: Fn(&EnvelopeProvenance) -> bool,
{
    /// Whether there was nothing to deliver.
    pub(crate) fn is_empty(&self) -> bool {
        self.items.is_empty()
    }

    /// Second half of the delivery: consumes (`p-*` -> `c-*`) exactly the
    /// envelopes prepared, and hands back each one this call took, with what was
    /// built for it. Call it only when the built text is about to be printed.
    ///
    /// Under the session lock each envelope's provenance is read again, and the
    /// envelope is taken only if it is still the one that was prepared and
    /// `accept` still takes it: the filter that chose what to prepare also gates
    /// what is consumed. An envelope another consumer already took, or that
    /// expired, is not returned, so two consumers prepared from the same
    /// envelope deliver it once between them.
    ///
    /// An error before anything was taken is returned and leaves every envelope
    /// pending. An error after some were taken returns those, and leaves the
    /// rest pending.
    pub(crate) fn commit(self) -> Result<Vec<(ClaimedEnvelope, T)>, MailboxError> {
        let Self {
            mailbox,
            key,
            items,
            accept,
        } = self;
        if items.is_empty() {
            return Ok(Vec::new());
        }
        let dir = mailbox.root.join(&key);
        match std::fs::symlink_metadata(&dir) {
            Ok(_) => private_dir(&dir)?,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
            Err(e) => return Err(io("could not look at the session's mailbox", &e)),
        }
        let _session_lock = mailbox.lock_session(&key)?;
        let now = SystemTime::now();
        let mut won = Vec::new();
        for item in items {
            let unchanged = match (
                read_message(&dir.join(&item.file)),
                mailbox.read_provenance(&key, &item.envelope.claim.request_id),
            ) {
                (Ok(held_text), Ok(raw_prov)) => {
                    let (held_prov, held_verified) = mailbox.authenticate_provenance(
                        &key,
                        &item.envelope.claim.request_id,
                        &held_text,
                        raw_prov,
                    );
                    held_text == item.envelope.claim.text
                        && held_prov == item.envelope.provenance
                        && held_verified == item.envelope.human_verified
                        && accept(&held_prov)
                }
                _ => false,
            };
            if !unchanged {
                continue;
            }
            match rename_to_claimed(&dir, &item.file, now) {
                Ok(true) => won.push((item.envelope, item.built)),
                Ok(false) => {}
                Err(error) if won.is_empty() => return Err(error),
                Err(error) => {
                    log::warn!(target: "mailbox", "stopped claiming after one message: {error}");
                    break;
                }
            }
        }
        Ok(won)
    }
}

fn millis_of(duration: Duration) -> u64 {
    u64::try_from(duration.as_millis()).unwrap_or(u64::MAX)
}

fn nonce(now: SystemTime) -> u32 {
    let nanos = now
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.subsec_nanos());
    let counter = {
        static NEXT: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);
        NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
    };
    nanos ^ counter.rotate_left(16) ^ std::process::id()
}

/// A hook waiting on a session. Its file is its registration; dropping it
/// leaves.
struct Waiter {
    mailbox: Mailbox,
    session_uuid: String,
    registered_epoch: u64,
    path: PathBuf,
    dir: PathBuf,
    name: String,
    file: File,
}

impl Waiter {
    #[cfg(test)]
    fn register(mailbox: &Mailbox, dir: &Path) -> Result<Self, MailboxError> {
        let session_uuid = dir
            .file_name()
            .and_then(|n| n.to_str())
            .map(str::to_owned)
            .unwrap_or_default();
        Self::register_in_pool_with_cap(
            mailbox,
            dir,
            &session_uuid,
            WaitPool::Hosted,
            MAX_WAITERS_TOTAL,
        )
    }

    #[cfg(test)]
    fn register_in_pool(
        mailbox: &Mailbox,
        dir: &Path,
        session_uuid: &str,
        pool: WaitPool,
    ) -> Result<Self, MailboxError> {
        let pool_cap = match pool {
            WaitPool::Hosted => MAX_WAITERS_HOSTED,
            WaitPool::External => MAX_WAITERS_EXTERNAL,
        };
        Self::register_in_pool_with_cap(mailbox, dir, session_uuid, pool, pool_cap)
    }

    fn register_in_pool_with_cap(
        mailbox: &Mailbox,
        dir: &Path,
        session_uuid: &str,
        pool: WaitPool,
        pool_cap: usize,
    ) -> Result<Self, MailboxError> {
        let _root_lock = mailbox.lock_root()?;
        let registered_epoch = mailbox.read_u64_counter(".epochs", session_uuid)?;
        let now = SystemTime::now();
        let name = format!(
            "w-{:013}-{}-{}-{:08x}",
            millis(now),
            pool.tag(),
            std::process::id(),
            nonce(now)
        );
        let path = dir.join(&name);
        let file = create_private(&path)?;
        let waiter = Self {
            mailbox: mailbox.clone(),
            session_uuid: session_uuid.to_owned(),
            registered_epoch,
            path,
            dir: dir.to_path_buf(),
            name: name.clone(),
            file,
        };
        let live = mailbox.live_waiters(now);
        if !live.iter().any(|(_, n)| *n == name) {
            return Err(MailboxError::Io(
                "could not register as a waiter in the mailbox".into(),
            ));
        }

        // Compute the staying waiters (newest MAX_WAITERS_PER_SESSION per session,
        // since live_waiters is sorted oldest-first).
        let mut by_session: std::collections::HashMap<&str, Vec<&str>> =
            std::collections::HashMap::new();
        for (sess, wname) in &live {
            by_session
                .entry(sess.as_str())
                .or_default()
                .push(wname.as_str());
        }
        let mut staying_total = 0usize;
        let mut staying_hosted = 0usize;
        let mut staying_external = 0usize;
        for names in by_session.values() {
            let keep_from = names.len().saturating_sub(MAX_WAITERS_PER_SESSION);
            for wname in &names[keep_from..] {
                staying_total += 1;
                match parse_waiter_pool(wname) {
                    Some(WaitPool::External) => staying_external += 1,
                    _ => staying_hosted += 1,
                }
            }
        }

        let pool_used = match pool {
            WaitPool::Hosted => staying_hosted,
            WaitPool::External => staying_external,
        };
        if pool_used > pool_cap {
            let label = match pool {
                WaitPool::Hosted => "hosted ",
                WaitPool::External => "external ",
            };
            return Err(MailboxError::Full(format!(
                "{pool_cap} {label}hooks are already waiting on sessions."
            )));
        }
        if staying_total > MAX_WAITERS_TOTAL {
            return Err(MailboxError::Full(format!(
                "{MAX_WAITERS_TOTAL} hooks are already waiting on sessions."
            )));
        }
        Ok(waiter)
    }

    #[cfg(test)]
    fn displaced(&mut self) -> Result<bool, MailboxError> {
        Ok(self.displaced_reason()?.is_some())
    }

    /// Returns `Some(reason)` when either a newer user turn bumped `turn_epoch`
    /// on this session or [`MAX_WAITERS_PER_SESSION`] newer waiters arrived.
    fn displaced_reason(&mut self) -> Result<Option<String>, MailboxError> {
        if !self.session_uuid.is_empty() {
            let current_epoch = self
                .mailbox
                .read_u64_counter(".epochs", &self.session_uuid)?;
            if current_epoch > self.registered_epoch {
                return Ok(Some(
                    "a newer turn started on this session; prior stop waiter displaced.".into(),
                ));
            }
        }
        let now = SystemTime::now();
        let (mut newer, mut present) = (0, false);
        for (name, modified) in regular_files(&self.dir) {
            if !is_waiter_name(&name) || age(now, modified) > WAITER_STALE {
                continue;
            }
            if name == self.name {
                present = true;
            } else if name > self.name {
                newer += 1;
            }
        }
        if !present {
            match std::fs::symlink_metadata(&self.path) {
                Ok(_) => {
                    let _ = self.file.set_modified(now);
                }
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                    self.file = create_private(&self.path)?;
                }
                Err(e) => return Err(io("could not look at a waiter's registration", &e)),
            }
        }
        if newer >= MAX_WAITERS_PER_SESSION {
            Ok(Some(format!(
                "{MAX_WAITERS_PER_SESSION} newer hooks are waiting on this session."
            )))
        } else {
            Ok(None)
        }
    }

    fn beat(&self) {
        let _ = self.file.set_modified(SystemTime::now());
    }
}

impl Drop for Waiter {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.path);
    }
}

/// Regular files directly in `dir` with their modification times, at most
/// [`SCAN_LIMIT`] entries looked at.
fn regular_files(dir: &Path) -> Vec<(String, SystemTime)> {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return Vec::new();
    };
    let mut out = Vec::new();
    for entry in entries.take(SCAN_LIMIT).flatten() {
        let Ok(name) = entry.file_name().into_string() else {
            continue;
        };
        let Ok(meta) = std::fs::symlink_metadata(entry.path()) else {
            continue;
        };
        if !meta.file_type().is_file() || !owned_privately(&meta) {
            continue;
        }
        out.push((name, meta.modified().unwrap_or(SystemTime::UNIX_EPOCH)));
    }
    out
}

#[cfg(unix)]
fn owned_privately(meta: &std::fs::Metadata) -> bool {
    use std::os::unix::fs::MetadataExt;
    // SAFETY: geteuid has no preconditions and cannot fail.
    meta.uid() == unsafe { libc::geteuid() } && meta.mode() & 0o077 == 0
}

#[cfg(not(unix))]
fn owned_privately(_meta: &std::fs::Metadata) -> bool {
    true
}

/// Makes `dir` if needed and confirms it is a real directory only this user
/// can use. One someone else planted, or a symlink, is refused.
fn private_dir(dir: &Path) -> Result<(), MailboxError> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::{DirBuilderExt, MetadataExt};
        match std::fs::DirBuilder::new().mode(0o700).create(dir) {
            Ok(()) => {}
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {}
            Err(e) => return Err(io("could not make a mailbox directory", &e)),
        }
        let meta = std::fs::symlink_metadata(dir)
            .map_err(|e| io("could not look at a mailbox directory", &e))?;
        // SAFETY: geteuid has no preconditions and cannot fail.
        let me = unsafe { libc::geteuid() };
        if !meta.is_dir() || meta.uid() != me || meta.mode() & 0o077 != 0 {
            return Err(MailboxError::Insecure(format!(
                "{} is not a private directory of this user",
                dir.display()
            )));
        }
        Ok(())
    }
    #[cfg(not(unix))]
    {
        std::fs::create_dir_all(dir).map_err(|e| io("could not make a mailbox directory", &e))
    }
}

/// Creates a new file only this user can read, never following a symlink at
/// the name.
fn create_private(path: &Path) -> Result<File, MailboxError> {
    let mut options = OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600).custom_flags(libc::O_NOFOLLOW);
    }
    options
        .open(path)
        .map_err(|e| io("could not create a mailbox file", &e))
}

fn write_private(path: &Path, bytes: &[u8]) -> Result<(), MailboxError> {
    let mut file = create_private(path)?;
    let written = file.write_all(bytes).and_then(|()| file.sync_all());
    if let Err(error) = written {
        let _ = std::fs::remove_file(path);
        return Err(io("could not write a message", &error));
    }
    Ok(())
}

/// Reads one message through a descriptor that is checked, not through a path
/// that was: a regular file this user owns, no larger than a message can be.
fn read_message(path: &Path) -> Result<String, MailboxError> {
    let mut options = OpenOptions::new();
    options.read(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.custom_flags(libc::O_NOFOLLOW);
    }
    let file = options
        .open(path)
        .map_err(|e| io("could not open a message", &e))?;
    let meta = file
        .metadata()
        .map_err(|e| io("could not look at a message", &e))?;
    if !meta.is_file() || !owned_privately(&meta) {
        return Err(MailboxError::Insecure(format!(
            "{} is not a private file of this user",
            path.display()
        )));
    }
    if meta.len() > MAX_MESSAGE_BYTES as u64 {
        return Err(MailboxError::Invalid(
            "A message file is larger than a message can be.".into(),
        ));
    }
    let mut text = String::new();
    file.take(MAX_MESSAGE_BYTES as u64 + 1)
        .read_to_string(&mut text)
        .map_err(|e| io("could not read a message", &e))?;
    validate_message(&text)?;
    Ok(text)
}

#[cfg(test)]
mod tests;
