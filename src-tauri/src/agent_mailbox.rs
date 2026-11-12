//! A one-use cross-process mailbox from GitPulse to a Claude Code or Antigravity session.
//!
//! ## Layout
//!
//! ```text
//! <GitPulse config dir>/agent-mailbox/            0700
//!   .lock                                         0600, advisory flock for root dir mutations
//!   .locks/<session uuid>.lock                    0600, advisory flock per session
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
use std::borrow::Cow;
use std::fs::{File, OpenOptions};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::sync::Mutex;
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

/// Serializes in-process callers alongside advisory `flock` across processes.
static DEPOSIT_LOCK: Mutex<()> = Mutex::new(());

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
/// human-confirmed instructions or untrusted peer-agent output.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub(crate) enum EnvelopeProvenance {
    #[default]
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

/// A claimed message paired with its verified provenance.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ClaimedEnvelope {
    pub(crate) claim: Claim,
    pub(crate) provenance: EnvelopeProvenance,
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
    if text
        .chars()
        .any(|c| c.is_control() && !matches!(c, '\n' | '\t'))
    {
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

/// Cross-process exclusive advisory lock on a `0600` `O_NOFOLLOW` lock file.
struct AdvisoryLock {
    file: File,
}

impl AdvisoryLock {
    fn acquire(path: &Path) -> Result<Self, MailboxError> {
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
        #[cfg(unix)]
        {
            use std::os::unix::io::AsRawFd;
            loop {
                // SAFETY: `file` is an open descriptor we own for the duration of the call.
                let rc = unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX) };
                if rc == 0 {
                    break;
                }
                let err = std::io::Error::last_os_error();
                if err.kind() == std::io::ErrorKind::Interrupted {
                    continue;
                }
                return Err(io("could not lock the mailbox", &err));
            }
        }
        Ok(Self { file })
    }
}

impl Drop for AdvisoryLock {
    fn drop(&mut self) {
        #[cfg(unix)]
        {
            use std::os::unix::io::AsRawFd;
            // SAFETY: `self.file` remains valid until after `drop` returns.
            let _ = unsafe { libc::flock(self.file.as_raw_fd(), libc::LOCK_UN) };
        }
    }
}

/// The mailbox rooted at one directory.
#[derive(Debug, Clone)]
pub(crate) struct Mailbox {
    root: PathBuf,
}

impl Mailbox {
    pub(crate) fn new(root: PathBuf) -> Self {
        Self { root }
    }

    /// The mailbox beside the notification socket, in GitPulse's configuration
    /// directory.
    pub(crate) fn at_default() -> Result<Self, MailboxError> {
        crate::tool_config::default_config_dir()
            .map(|dir| Self::new(dir.join("agent-mailbox")))
            .ok_or_else(|| MailboxError::Io("There is no GitPulse configuration directory.".into()))
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
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                return Ok(EnvelopeProvenance::HumanConfirmed);
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

    /// Removes expired files from every session and empty sessions that have
    /// sat unused. Best effort: a stale file costs bytes, not correctness.
    fn prune_sessions(&self, now: SystemTime) {
        for name in self.session_names() {
            let dir = self.root.join(&name);
            self.expire_session(&name, &dir, now);
            let empty = std::fs::read_dir(&dir).is_ok_and(|mut d| d.next().is_none());
            let old = std::fs::metadata(&dir)
                .and_then(|m| m.modified())
                .is_ok_and(|m| age(now, m) > EMPTY_SESSION_TTL);
            if empty && old && std::fs::remove_dir(&dir).is_ok() {
                let _ = std::fs::remove_file(self.root.join(".locks").join(format!("{name}.lock")));
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
        for (name, _) in &files {
            if let Some((_, stamp, id)) = parse_message_name(name) {
                if id == request_id {
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
                    return Ok(Deposit {
                        request_id: request_id.to_owned(),
                        created_at_ms: stamp,
                        expires_at_ms: stamp + millis_of(PENDING_TTL),
                        duplicate: true,
                    });
                }
            }
        }
        let pending = files.iter().filter(|(n, _)| n.starts_with("p-")).count();
        if pending >= MAX_PENDING {
            return Err(MailboxError::Full(format!(
                "This session already has {MAX_PENDING} messages waiting for it."
            )));
        }
        self.write_provenance(&key, request_id, &provenance, now)?;
        let created = millis(now);
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
        Ok(Deposit {
            request_id: request_id.to_owned(),
            created_at_ms: created,
            expires_at_ms: created + millis_of(PENDING_TTL),
            duplicate: false,
        })
    }

    /// Takes the oldest pending message, if there is one, for this caller alone.
    #[cfg(test)]
    pub(crate) fn claim_next(
        &self,
        session: &str,
        now: SystemTime,
    ) -> Result<Option<Claim>, MailboxError> {
        Ok(self.claim_envelope_next(session, now)?.map(|env| env.claim))
    }

    /// Takes the oldest pending envelope along with its [`EnvelopeProvenance`].
    pub(crate) fn claim_envelope_next(
        &self,
        session: &str,
        now: SystemTime,
    ) -> Result<Option<ClaimedEnvelope>, MailboxError> {
        let mut batch = self.try_claim_batch(session, 1, now)?;
        Ok(batch.pop())
    }

    /// Non-blocking batch claim of up to `max_items` oldest pending envelopes
    /// under the session's cross-process advisory `flock`.
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
        let mut claimed_batch = Vec::new();
        for name in pending {
            if claimed_batch.len() >= max_items {
                break;
            }
            if let Some((_, _, id)) = parse_message_name(&name) {
                if let Ok(prov) = self.read_provenance(&key, id) {
                    if !accept(&prov) {
                        continue;
                    }
                }
            }
            let claimed = format!("c-{}", &name[2..]);
            match crate::fs_entry::rename_noreplace(&dir.join(&name), &dir.join(&claimed)) {
                Ok(()) => {}
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => continue,
                Err(e) => return Err(io("could not claim a message", &e)),
            }
            let id = parse_message_name(&claimed)
                .map(|(_, _, id)| id.to_owned())
                .unwrap_or_default();
            match (
                read_message(&dir.join(&claimed)),
                self.read_provenance(&key, &id),
            ) {
                (Ok(text), Ok(provenance)) => {
                    claimed_batch.push(ClaimedEnvelope {
                        claim: Claim {
                            request_id: id,
                            text,
                        },
                        provenance,
                    });
                }
                (Err(error), _) | (_, Err(error)) => {
                    log::warn!(target: "mailbox", "dropped an unreadable envelope: {error}");
                    let _ = std::fs::remove_file(dir.join(&claimed));
                    self.remove_provenance(&key, &id);
                }
            }
        }
        Ok(claimed_batch)
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
        for name in pending.into_iter().take(max_items) {
            let Some((_, _, id)) = parse_message_name(&name) else {
                continue;
            };
            if let (Ok(text), Ok(provenance)) = (
                read_message(&dir.join(&name)),
                self.read_provenance(&key, id),
            ) {
                out.push(ClaimedEnvelope {
                    claim: Claim {
                        request_id: id.to_owned(),
                        text,
                    },
                    provenance,
                });
            }
        }
        Ok(out)
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
                    let claimed = format!("c-{}", &name[2..]);
                    match crate::fs_entry::rename_noreplace(&dir.join(&name), &dir.join(&claimed)) {
                        Ok(()) => return Ok(true),
                        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(false),
                        Err(e) => return Err(io("could not mark message claimed", &e)),
                    }
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
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(0),
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
        Ok(buf.trim().parse::<u64>().unwrap_or(0))
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

    /// Waits in `pool` for an envelope accepted by `accept`. When the oldest
    /// pending envelope fails `accept` (for example a peer message that has
    /// reached `max_hops`), the waiter declines without claiming it so the
    /// message stays pending for manual/boundary inspection instead of looping.
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
            if let Some(reason) = self.peek_rejected_by(&key, &dir, SystemTime::now(), &accept)? {
                return Ok(EnvelopeWait::Declined(reason));
            }
            if let Some(envelope) = self.claim_envelope_next(&key, SystemTime::now())? {
                return Ok(EnvelopeWait::Message(envelope));
            }
            let spent = started.elapsed();
            if spent >= limit {
                return Ok(EnvelopeWait::TimedOut);
            }
            if last_beat.elapsed() >= HEARTBEAT {
                waiter.beat();
                last_beat = Instant::now();
            }
            std::thread::sleep(POLL.min(limit - spent));
        }
    }

    fn peek_rejected_by<F>(
        &self,
        session_uuid: &str,
        dir: &Path,
        now: SystemTime,
        accept: &F,
    ) -> Result<Option<String>, MailboxError>
    where
        F: Fn(&EnvelopeProvenance) -> bool,
    {
        let _session_lock = self.lock_session(session_uuid)?;
        self.expire_session(session_uuid, dir, now);
        let mut pending: Vec<String> = regular_files(dir)
            .into_iter()
            .map(|(name, _)| name)
            .filter(|name| name.starts_with("p-") && parse_message_name(name).is_some())
            .collect();
        pending.sort();
        let Some(oldest) = pending.first() else {
            return Ok(None);
        };
        let Some((_, _, id)) = parse_message_name(oldest) else {
            return Ok(None);
        };
        let prov = self.read_provenance(session_uuid, id)?;
        if !accept(&prov) {
            return Ok(Some(
                "pending peer message paused at hop limit; leaving unclaimed for manual review"
                    .into(),
            ));
        }
        Ok(None)
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
