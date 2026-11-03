//! A one-use mailbox from GitPulse to one Claude Code session.
//!
//! A person reads a CI failure in GitPulse and wants the agent that pushed the
//! branch to hear about it. A live Claude Code session has no door GitPulse can
//! knock on: its terminal is the person's, and typing into it from here would
//! race whatever the person is typing. Claude Code does have one documented
//! way to be woken from outside — a `Stop` hook marked `asyncRewake` that exits
//! with code 2 wakes it even when it is idle, and shows the hook's stderr to it
//! as a system reminder
//! (<https://code.claude.com/docs/en/hooks.md>, the `asyncRewake` row of the
//! command-hook fields). `gitpulse-hook deliver` is that hook; this module is
//! what it waits on and what the app writes to.
//!
//! ## Layout
//!
//! ```text
//! <GitPulse config dir>/agent-mailbox/            0700
//!   <claude session uuid>/                        0700, one per session
//!     p-<ms>-<request id>.msg                     0600, pending
//!     c-<ms>-<request id>.msg                     0600, claimed (kept briefly)
//!     w-<ms>-<pid>-<nonce>                        0600, a hook waiting here
//!     .tmp-<pid>-<nonce>                          0600, a write in progress
//! ```
//!
//! ## What is guaranteed, and by what
//!
//! * **A message is whole or absent.** It is written to a temporary name in the
//!   same directory and published with a no-replace rename
//!   ([`crate::fs_entry::rename_noreplace`]), so a reader never sees a prefix
//!   and a publish never overwrites.
//! * **A message is delivered once.** The claim is a rename of `p-…` to `c-…`.
//!   Of any number of processes racing for one message the kernel lets exactly
//!   one rename succeed; the others see the source gone. This is the whole
//!   reason a plugin's copy of the hook and a `--settings` copy of it can both
//!   run: Claude Code does not deduplicate them, and nothing here assumes
//!   it does. "Claimed" means a hook process took it, not that the agent read
//!   it: a hook killed between the claim and its write loses the message, and
//!   [`Mailbox::status`] says `claimed`, never `read`.
//! * **Everything is bounded.** [`MAX_MESSAGE_BYTES`] per message,
//!   [`MAX_PENDING`] per session, [`MAX_SESSIONS`] directories, a pending
//!   message expires after [`PENDING_TTL`], and at most
//!   [`MAX_WAITERS_PER_SESSION`] / [`MAX_WAITERS_TOTAL`] hook processes wait at
//!   once. A waiter beyond a cap is told to leave, which the hook turns into a
//!   plain exit 0.
//! * **Only this user's files are read.** Directories are created 0700 and
//!   files 0600 with `O_NOFOLLOW`; a directory or file that is a symlink, is
//!   not a regular file, is owned by another uid, or is open to group or other
//!   is refused, not used. There is no socket, so there is no peer to
//!   authenticate: the owner check on the object is the credential check.
//!
//! ## What it widens
//!
//! It is a new path by which same-user text reaches an agent's context. The
//! text is exactly what a person previewed and confirmed in GitPulse
//! (`workbench::send_to_agent`); nothing is sent automatically. It cannot run
//! anything or change what the agent may run: the agent receives a reminder and
//! decides, under its own permission mode, what to do with it. Another process
//! of the same user can already write to the agent's terminal; the mailbox
//! gives it no reach it did not have.

use std::fs::{File, OpenOptions};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

/// The longest message, in bytes of UTF-8. Below the 16000-byte ceiling an
/// agent prompt argument has (`agentPromptArgs`) and Manvi's 32 KiB follow-up
/// limit, so one text fits every route.
pub(crate) const MAX_MESSAGE_BYTES: usize = 8 * 1024;
/// Pending messages one session may hold.
pub(crate) const MAX_PENDING: usize = 8;
/// Session directories under the root.
pub(crate) const MAX_SESSIONS: usize = 64;
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
/// Hook processes waiting on any session.
pub(crate) const MAX_WAITERS_TOTAL: usize = 16;
/// Longest request id, as a file-name part.
const MAX_REQUEST_ID: usize = 64;
/// Directory entries one pass will look at.
const SCAN_LIMIT: usize = 256;
/// An empty session directory older than this is removed.
const EMPTY_SESSION_TTL: Duration = Duration::from_secs(60 * 60);

/// Serializes this process's deposits, so the pending count a deposit checked
/// is the one it adds to. Other processes only claim, never deposit.
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
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum Wait {
    Message(Claim),
    /// The limit passed with nothing for this session.
    TimedOut,
    /// This waiter is over a cap and did not wait.
    Declined(String),
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

/// Checks a message a person approved: nonblank, bounded, and free of control
/// characters other than line breaks and tabs.
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

fn valid_request_id(id: &str) -> bool {
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
    /// directory. The hook and the app resolve it the same way, which is what
    /// lets two processes that never talk meet in it.
    pub(crate) fn at_default() -> Result<Self, MailboxError> {
        crate::tool_config::default_config_dir()
            .map(|dir| Self::new(dir.join("agent-mailbox")))
            .ok_or_else(|| MailboxError::Io("There is no GitPulse configuration directory.".into()))
    }

    pub(crate) fn session_path(&self, session: &str) -> Result<PathBuf, MailboxError> {
        Ok(self.root.join(session_key(session)?))
    }

    /// Makes the root and the session's directory if needed.
    fn ensure_session(&self, session: &str, now: SystemTime) -> Result<PathBuf, MailboxError> {
        let dir = self.session_path(session)?;
        if let Some(parent) = self.root.parent() {
            std::fs::create_dir_all(parent).map_err(|e| io("could not make the mailbox", &e))?;
        }
        private_dir(&self.root)?;
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
            self.expire(&dir, now);
            let empty = std::fs::read_dir(&dir).is_ok_and(|mut d| d.next().is_none());
            let old = std::fs::metadata(&dir)
                .and_then(|m| m.modified())
                .is_ok_and(|m| age(now, m) > EMPTY_SESSION_TTL);
            if empty && old {
                let _ = std::fs::remove_dir(&dir);
            }
        }
    }

    /// Removes what has outlived its time. A message another process is about
    /// to claim is never racing this: the bounds are hours, the claim seconds.
    fn expire(&self, dir: &Path, now: SystemTime) {
        for (name, modified) in regular_files(dir) {
            let ttl = if name.starts_with("p-") {
                PENDING_TTL
            } else if name.starts_with("c-") {
                CLAIMED_TTL
            } else if name.starts_with(".tmp-") {
                TEMP_TTL
            } else if name.starts_with("w-") {
                WAITER_STALE
            } else {
                continue;
            };
            if age(now, modified) > ttl {
                let _ = std::fs::remove_file(dir.join(&name));
            }
        }
    }

    /// Puts one message in a session's mailbox.
    ///
    /// `request_id` makes a retry harmless: the same id with the same text
    /// answers the first deposit and writes nothing; the same id with other
    /// text is refused.
    pub(crate) fn deposit(
        &self,
        session: &str,
        request_id: &str,
        text: &str,
        now: SystemTime,
    ) -> Result<Deposit, MailboxError> {
        validate_message(text)?;
        if !valid_request_id(request_id) {
            return Err(MailboxError::Invalid(
                "A request id is 1 to 64 letters, digits, - or _.".into(),
            ));
        }
        let _one_at_a_time = DEPOSIT_LOCK
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let dir = self.ensure_session(session, now)?;
        self.expire(&dir, now);
        let files = regular_files(&dir);
        for (name, _) in &files {
            if let Some((_, stamp, id)) = parse_message_name(name) {
                if id == request_id {
                    let same = read_message(&dir.join(name)).is_ok_and(|held| held == text);
                    if !same {
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
        let created = millis(now);
        let temp = dir.join(format!(".tmp-{}-{:08x}", std::process::id(), nonce(now)));
        write_private(&temp, text.as_bytes())?;
        let target = dir.join(format!("p-{created:013}-{request_id}.msg"));
        if let Err(error) = crate::fs_entry::rename_noreplace(&temp, &target) {
            let _ = std::fs::remove_file(&temp);
            return Err(io("could not publish the message", &error));
        }
        Ok(Deposit {
            request_id: request_id.to_owned(),
            created_at_ms: created,
            expires_at_ms: created + millis_of(PENDING_TTL),
            duplicate: false,
        })
    }

    /// Takes the oldest pending message, if there is one, for this caller
    /// alone. Of several concurrent callers exactly one gets a given message.
    pub(crate) fn claim_next(
        &self,
        session: &str,
        now: SystemTime,
    ) -> Result<Option<Claim>, MailboxError> {
        let dir = self.session_path(session)?;
        match std::fs::symlink_metadata(&dir) {
            Ok(_) => private_dir(&dir)?,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(e) => return Err(io("could not look at the session's mailbox", &e)),
        }
        self.expire(&dir, now);
        let mut pending: Vec<String> = regular_files(&dir)
            .into_iter()
            .map(|(name, _)| name)
            .filter(|name| name.starts_with("p-") && parse_message_name(name).is_some())
            .collect();
        pending.sort();
        for name in pending {
            let claimed = format!("c-{}", &name[2..]);
            match crate::fs_entry::rename_noreplace(&dir.join(&name), &dir.join(&claimed)) {
                Ok(()) => {}
                // Another claimer's rename won; the message is theirs.
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => continue,
                Err(e) => return Err(io("could not claim a message", &e)),
            }
            let id = parse_message_name(&claimed)
                .map(|(_, _, id)| id.to_owned())
                .unwrap_or_default();
            match read_message(&dir.join(&claimed)) {
                Ok(text) => {
                    return Ok(Some(Claim {
                        request_id: id,
                        text,
                    }))
                }
                // A file that is not what a deposit writes is dropped, and the
                // next message is tried: one bad file must not hold a session's
                // mailbox shut.
                Err(error) => {
                    log::warn!(target: "mailbox", "dropped an unreadable message: {error}");
                    let _ = std::fs::remove_file(dir.join(&claimed));
                }
            }
        }
        Ok(None)
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

    /// Messages waiting in a session's mailbox.
    pub(crate) fn pending(&self, session: &str) -> usize {
        self.session_path(session).map_or(0, |dir| {
            regular_files(&dir)
                .iter()
                .filter(|(n, _)| n.starts_with("p-") && parse_message_name(n).is_some())
                .count()
        })
    }

    /// Whether a hook is waiting on this session right now, by the beats its
    /// waiters leave. A waiter that was killed stops beating and stops counting
    /// after [`WAITER_STALE`], without anything having to clean up after it.
    pub(crate) fn listening(&self, session: &str, now: SystemTime) -> bool {
        self.session_path(session).is_ok_and(|dir| {
            regular_files(&dir)
                .iter()
                .any(|(n, modified)| n.starts_with("w-") && age(now, *modified) <= WAITER_STALE)
        })
    }

    /// Live waiters as `(session, file name)`, oldest first.
    fn live_waiters(&self, now: SystemTime) -> Vec<(String, String)> {
        let mut found = Vec::new();
        for session in self.session_names() {
            for (name, modified) in regular_files(&self.root.join(&session)) {
                if name.starts_with("w-") && age(now, modified) <= WAITER_STALE {
                    found.push((session.clone(), name));
                }
            }
        }
        found.sort_by(|a, b| a.1.cmp(&b.1));
        found
    }

    /// Waits for a message for this session, up to `limit`, by looking every
    /// [`POLL`]. Takes the message the moment one is there, if this waiter is
    /// within the caps.
    ///
    /// The deadline is the caller's own and is not shared with any other
    /// budget: a waiter that is clamped to a short budget would give up on a
    /// session that is merely idle, which is the case it exists for.
    pub(crate) fn wait(&self, session: &str, limit: Duration) -> Result<Wait, MailboxError> {
        let started = Instant::now();
        let dir = self.ensure_session(session, SystemTime::now())?;
        let key = session_key(session)?;
        let waiter = match Waiter::register(self, &dir, &key) {
            Ok(waiter) => waiter,
            Err(MailboxError::Full(why)) => return Ok(Wait::Declined(why)),
            Err(other) => return Err(other),
        };
        let mut last_beat = Instant::now();
        loop {
            if let Some(claim) = self.claim_next(session, SystemTime::now())? {
                return Ok(Wait::Message(claim));
            }
            let spent = started.elapsed();
            if spent >= limit {
                return Ok(Wait::TimedOut);
            }
            if last_beat.elapsed() >= HEARTBEAT {
                waiter.beat();
                last_beat = Instant::now();
            }
            std::thread::sleep(POLL.min(limit - spent));
        }
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
    path: PathBuf,
    file: File,
}

impl Waiter {
    /// Registers, then checks its rank, so two processes arriving together
    /// cannot both think they were the last one in: whoever sorts past a cap
    /// leaves, and the same ones leave whichever looked first.
    fn register(mailbox: &Mailbox, dir: &Path, session: &str) -> Result<Self, MailboxError> {
        let now = SystemTime::now();
        let name = format!(
            "w-{:013}-{}-{:08x}",
            millis(now),
            std::process::id(),
            nonce(now)
        );
        let path = dir.join(&name);
        let file = create_private(&path)?;
        let waiter = Self { path, file };
        let live = mailbox.live_waiters(now);
        let here = live.iter().filter(|(s, _)| s == session);
        let session_rank = here.clone().position(|(_, n)| *n == name);
        let total_rank = live.iter().position(|(_, n)| *n == name);
        match (session_rank, total_rank) {
            (Some(s), Some(t)) if s < MAX_WAITERS_PER_SESSION && t < MAX_WAITERS_TOTAL => {
                Ok(waiter)
            }
            (Some(s), _) if s >= MAX_WAITERS_PER_SESSION => Err(MailboxError::Full(format!(
                "{MAX_WAITERS_PER_SESSION} hooks are already waiting on this session."
            ))),
            (_, Some(t)) if t >= MAX_WAITERS_TOTAL => Err(MailboxError::Full(format!(
                "{MAX_WAITERS_TOTAL} hooks are already waiting on sessions."
            ))),
            // Not seen among the live waiters: its own file could not be read
            // back, so it cannot be counted, and a waiter that cannot be
            // counted does not wait.
            _ => Err(MailboxError::Io(
                "could not register as a waiter in the mailbox".into(),
            )),
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
/// [`SCAN_LIMIT`] entries looked at. Anything else in the directory — a symlink
/// planted there, a directory, a file another user owns — is not listed, so no
/// caller can be steered by it.
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
