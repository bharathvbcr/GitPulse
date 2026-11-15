//! Build leases (load-governor card 1d): how many heavy builds run at once on
//! this Mac, who holds the slots, and who waits.
//!
//! # Why a lease and not a lock a shell script holds
//!
//! On 2026-10-17 four to seven release builds from separate agent worktrees
//! starved the window server, and on 2026-11-14 sixteen runs queued for two
//! slots of a shell shim while the host was healthy. The shim cannot see what
//! GitPulse sees: which session a build belongs to, the governor's verdict, or
//! who is waiting. This module is the one owner of that state, and
//! [`crate::hooks::lease_wrapper`] is the program a build goes through.
//!
//! # State, all under `<config>/GitPulse/leases/`
//!
//! * `slot-1` .. `slot-16` are lock files. A holder takes a POSIX record lock
//!   (`fcntl(F_SETLK)`) on its slot and then `exec`s the build, so the lock is
//!   owned by the build's own process: a record lock is **not inherited by a
//!   child**, so a daemon a build leaves behind (a compile server, a watcher)
//!   cannot keep the slot after the build has exited, and the kernel drops the
//!   lock when the process dies, crashes included. `F_GETLK` names the holder.
//! * `slot-N.json` is display only (written tmp + rename): who holds the slot,
//!   for which session, since when. **A record without a lock is never a
//!   holder**, and a lock without a record is a holder with no name.
//! * `queue/<mono_ns>-<pid>.json` is a waiter's ticket, locked by the waiter.
//!   A ticket nobody locks is dead and is deleted by any reader.
//!
//! # The one trap in POSIX record locks
//!
//! A process loses **every** lock it holds on a file when it closes **any**
//! descriptor for that file. So a process that holds a slot (or a ticket) must
//! never open that file again, and [`Own`] is how every reader here is told
//! which slot and ticket to leave alone. A second descriptor opened and closed
//! by the holder itself would silently free its own slot. For the same reason
//! a lock held by the calling process is invisible to its own `F_GETLK`, and a
//! test that needs a slot held starts another process to hold it.
//!
//! # Order
//!
//! A waiter's rank is `(priority, slots its session already holds, enqueue
//! time, pid)`: someone in a GitPulse terminal before a background task run
//! before an observed session, a session that holds none before one that holds
//! one, and then first come first served. A waiter takes a free slot only when
//! its rank is below the number of free slots, so the order is the order of
//! service even when two slots free at once.
//!
//! # How many slots
//!
//! The setting, lowered by the governor's published verdict ([`capacity`]):
//! half under an elevated tier (at least one), none under a severe tier or a
//! verdict that does not admit launches. With no readable verdict the setting
//! applies and [`Capacity::note`] says why it was not lowered: a reading that
//! could not be taken is never presented as a healthy one.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use super::{PublishedVerdict, Tier};

/// The directory under the per-user config directory.
pub const DIR_NAME: &str = "leases";

/// The sub-directory of waiters' tickets.
pub const QUEUE_DIR: &str = "queue";

/// How many slot files exist. More than [`MAX_SLOTS`], so a build that waited
/// too long (an overflow) still has a file to hold.
pub const SLOT_FILES: usize = 16;

/// The most slots the setting may ask for.
pub const MAX_SLOTS: u32 = 8;

/// Slots when nothing is set: what the owner's shell shim allows on a healthy
/// machine.
pub const DEFAULT_SLOTS: u32 = 4;

/// Seconds a build waits before it starts without a slot, when the verdict
/// does not refuse.
pub const DEFAULT_WAIT_MAX_S: u64 = 1_800;

/// Seconds a build waits while the verdict refuses, before it gives up.
pub const DEFAULT_WAIT_HARD_S: u64 = 14_400;

/// The most tickets one read looks at; the rest are not counted as dead.
pub const MAX_QUEUE_SCAN: usize = 256;

/// The largest record a reader accepts.
pub const RECORD_MAX_BYTES: usize = 8 * 1024;

/// The longest working directory kept in a record.
const CWD_CHARS: usize = 400;

/// Who a build is for, most urgent first.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Priority {
    /// A session in a GitPulse terminal tab: a person is watching it.
    Terminal,
    /// A task run GitPulse launched.
    Run,
    /// A session GitPulse only observes.
    Observed,
}

/// What a slot or a ticket says about its build. Display only: nothing here
/// decides whether a slot is held.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Record {
    pub pid: i32,
    pub start_us: u64,
    /// The session the build belongs to: `terminal:<id>`, `run:<id>`,
    /// `agent:<pid>-<start>` or `process:<pid>-<start>`.
    pub session_key: String,
    pub priority: Priority,
    pub terminal_session: Option<String>,
    pub run_id: Option<String>,
    pub cwd: String,
    /// `cargo build`, never the whole command line.
    pub label: String,
    pub enqueued_mono_ns: u64,
    pub granted_mono_ns: Option<u64>,
    pub tier_at_grant: Option<Tier>,
    /// Why no verdict was read at grant, when none was.
    pub verdict_unavailable_reason: Option<String>,
    /// Started without a slot after waiting too long.
    pub overflow: bool,
}

impl Record {
    /// This record with its free text cut to what a reader will show.
    pub fn bounded(mut self) -> Self {
        self.cwd = self.cwd.chars().take(CWD_CHARS).collect();
        self.label = self.label.chars().take(64).collect();
        self.session_key = self.session_key.chars().take(160).collect();
        self
    }
}

/// A slot some process holds.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Holder {
    pub slot: usize,
    pub pid: i32,
    /// `None` is a lock with no record, or a record another process wrote.
    pub record: Option<Record>,
}

/// A live ticket.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Waiter {
    pub name: String,
    pub record: Record,
}

/// The holders and the live queue at one moment.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
pub struct Board {
    pub holders: Vec<Holder>,
    /// In service order.
    pub waiters: Vec<Waiter>,
    /// Slot files and tickets that could not be read: counted, never read as
    /// "free".
    pub unreadable: Vec<String>,
}

/// What the governor's verdict is, as far as this module is concerned.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Load {
    Verdict(PublishedVerdict),
    /// No verdict could be read, and why.
    Unknown(String),
}

/// How many builds may run now, and why.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Capacity {
    pub configured: u32,
    pub slots: u32,
    /// The verdict forbids a new grant: a waiter must not start without a
    /// slot either.
    pub refuses: bool,
    pub tier: Option<Tier>,
    /// What lowered the count, or why it was not lowered when it could not be.
    pub note: Option<String>,
}

/// The slots the setting asks for, lowered by what the governor published.
pub fn capacity(configured: u32, load: &Load) -> Capacity {
    let configured = configured.clamp(1, MAX_SLOTS);
    match load {
        Load::Unknown(why) => Capacity {
            configured,
            slots: configured,
            refuses: false,
            tier: None,
            note: Some(format!("slots not lowered for load: {why}")),
        },
        Load::Verdict(verdict) if !verdict.admit || verdict.tier >= Tier::Severe => Capacity {
            configured,
            slots: 0,
            refuses: true,
            tier: Some(verdict.tier),
            note: Some(if verdict.admit {
                "no new builds while the host load is severe".to_string()
            } else {
                "no new builds while the governor is not admitting launches".to_string()
            }),
        },
        Load::Verdict(verdict) if verdict.tier >= Tier::Elevated => {
            let slots = (configured / 2).max(1);
            Capacity {
                configured,
                slots,
                refuses: false,
                tier: Some(verdict.tier),
                note: (slots < configured)
                    .then(|| format!("halved from {configured} while the host load is elevated")),
            }
        }
        Load::Verdict(verdict) => Capacity {
            configured,
            slots: configured,
            refuses: false,
            tier: Some(verdict.tier),
            note: None,
        },
    }
}

/// Whether the waiter at `rank` (0 is next) may take a slot while `held` are
/// held: only a waiter ahead of the number of free slots may, so two slots
/// freeing at once serve the first two waiters and nobody else.
pub fn may_take(rank: usize, held: usize, capacity: &Capacity) -> bool {
    let free = (capacity.slots as usize).saturating_sub(held);
    rank < free
}

/// Sorts `waiters` into service order: priority, then how many slots the
/// waiter's session already holds (fewest first), then enqueue time, then pid.
pub fn order(waiters: &mut [Waiter], holders: &[Holder]) {
    let held_by = |session: &str| {
        holders
            .iter()
            .filter(|h| h.record.as_ref().is_some_and(|r| r.session_key == session))
            .count()
    };
    waiters.sort_by_cached_key(|w| {
        (
            w.record.priority,
            held_by(&w.record.session_key),
            w.record.enqueued_mono_ns,
            w.record.pid,
        )
    });
}

/// How a session is named and how urgent it is, from what a build's
/// environment says. `ancestor` is the nearest agent process above the build,
/// which the caller looked up only when the environment named nothing.
pub fn session_of(
    terminal_session: Option<&str>,
    run_id: Option<&str>,
    ancestor: Option<(i32, u64)>,
    parent: (i32, u64),
) -> (String, Priority) {
    let plain =
        |s: &&str| !s.is_empty() && s.len() <= 128 && s.chars().all(|c| c.is_ascii_graphic());
    if let Some(id) = terminal_session.filter(plain) {
        return (format!("terminal:{id}"), Priority::Terminal);
    }
    if let Some(id) = run_id.filter(plain) {
        return (format!("run:{id}"), Priority::Run);
    }
    match ancestor {
        Some((pid, start)) => (format!("agent:{pid}-{start}"), Priority::Observed),
        None => (
            format!("process:{}-{}", parent.0, parent.1),
            Priority::Observed,
        ),
    }
}

/// `<config>/leases`.
pub fn lease_dir(config_dir: &Path) -> PathBuf {
    config_dir.join(DIR_NAME)
}

/// `<config>/lease-bin`: the folder of `cargo` and `go` links an agent's PATH
/// starts with.
pub fn bin_dir(config_dir: &Path) -> PathBuf {
    config_dir.join("lease-bin")
}

fn slot_name(slot: usize) -> String {
    format!("slot-{slot}")
}

fn parse_record(bytes: &[u8]) -> Option<Record> {
    if bytes.len() > RECORD_MAX_BYTES {
        return None;
    }
    serde_json::from_slice::<Record>(bytes)
        .ok()
        .map(Record::bounded)
}

#[cfg(unix)]
pub use unix::{Dir, Grant, Own, Ticket};

#[cfg(unix)]
mod unix {
    use std::fs::{File, OpenOptions};
    use std::io::{self, Write};
    use std::os::fd::AsRawFd;
    use std::os::unix::fs::{MetadataExt, OpenOptionsExt};
    use std::path::{Path, PathBuf};

    use crate::fs_entry;

    use super::{
        order, parse_record, slot_name, Board, Holder, Record, Waiter, MAX_QUEUE_SCAN, QUEUE_DIR,
        SLOT_FILES,
    };

    /// What a lock probe saw.
    #[derive(Debug, PartialEq, Eq)]
    enum Probe {
        Free,
        Held(i32),
    }

    fn flock(kind: i32) -> libc::flock {
        // SAFETY: `flock` is plain data, valid when zeroed.
        let mut lock: libc::flock = unsafe { std::mem::zeroed() };
        lock.l_type = kind as libc::c_short;
        lock.l_whence = libc::SEEK_SET as libc::c_short;
        lock.l_start = 0;
        lock.l_len = 1;
        lock
    }

    /// Takes the write lock on byte 0. `Ok(false)` when another process holds it.
    fn try_lock(file: &File) -> io::Result<bool> {
        let mut lock = flock(libc::F_WRLCK as i32);
        // SAFETY: the descriptor is open for the life of `file`, and `lock`
        // is a valid `flock` the kernel only reads.
        let result = unsafe { libc::fcntl(file.as_raw_fd(), libc::F_SETLK, &mut lock) };
        if result == 0 {
            return Ok(true);
        }
        let error = io::Error::last_os_error();
        match error.raw_os_error() {
            Some(libc::EAGAIN) | Some(libc::EACCES) => Ok(false),
            _ => Err(error),
        }
    }

    /// Who holds a conflicting lock. A read lock is asked about, because the
    /// descriptor may be read-only and a holder holds the write lock, which
    /// conflicts with it. The calling process's own locks never conflict.
    fn probe(file: &File) -> io::Result<Probe> {
        let mut lock = flock(libc::F_RDLCK as i32);
        // SAFETY: as in `try_lock`; the kernel writes the answer into `lock`.
        let result = unsafe { libc::fcntl(file.as_raw_fd(), libc::F_GETLK, &mut lock) };
        if result != 0 {
            return Err(io::Error::last_os_error());
        }
        if lock.l_type == libc::F_UNLCK as libc::c_short {
            Ok(Probe::Free)
        } else {
            Ok(Probe::Held(lock.l_pid))
        }
    }

    /// Keeps `file` open across `exec`, which is what keeps the lock: a lock
    /// is dropped when the descriptor it was taken on is closed, and `exec`
    /// closes every close-on-exec descriptor.
    fn keep_across_exec(file: &File) -> io::Result<()> {
        // SAFETY: F_SETFD on an open descriptor.
        let result = unsafe { libc::fcntl(file.as_raw_fd(), libc::F_SETFD, 0) };
        if result == 0 {
            Ok(())
        } else {
            Err(io::Error::last_os_error())
        }
    }

    /// The slot and ticket the calling process holds, which no reader may open.
    #[derive(Debug, Default, Clone)]
    pub struct Own {
        pub slot: Option<usize>,
        pub ticket: Option<String>,
    }

    /// A waiter's place in the queue, held for as long as this value lives.
    #[derive(Debug)]
    pub struct Ticket {
        pub name: String,
        path: PathBuf,
        file: File,
    }

    impl Ticket {
        /// Takes this waiter out of the queue.
        pub fn withdraw(self) {
            let _ = std::fs::remove_file(&self.path);
        }

        pub fn own(&self) -> Own {
            Own {
                slot: None,
                ticket: Some(self.name.clone()),
            }
        }
    }

    /// A slot this process holds: the lock lives as long as `file` is open,
    /// which the wrapper arranges to be until the build it runs has exited.
    #[derive(Debug)]
    pub struct Grant {
        pub slot: usize,
        pub record: Record,
        file: File,
    }

    impl Grant {
        /// Leaves the descriptor open through `exec` and returns its number.
        pub fn keep_for_exec(&self) -> io::Result<i32> {
            keep_across_exec(&self.file)?;
            Ok(self.file.as_raw_fd())
        }
    }

    /// The lease directory.
    #[derive(Debug, Clone)]
    pub struct Dir {
        root: PathBuf,
    }

    fn private_dir(dir: &Path) -> Result<(), String> {
        crate::hooks::advisory_once::private_dir(dir)
            .map_err(|why| format!("cannot use the lease directory {}: {why}", dir.display()))
    }

    impl Dir {
        /// Opens (making it when it is absent) the lease directory under
        /// `root`, which must be a real directory this user alone can write.
        pub fn open(root: &Path) -> Result<Self, String> {
            if let Some(parent) = root.parent() {
                std::fs::create_dir_all(parent)
                    .map_err(|e| format!("cannot create {}: {e}", parent.display()))?;
            }
            private_dir(root)?;
            private_dir(&root.join(QUEUE_DIR))?;
            Ok(Self {
                root: root.to_path_buf(),
            })
        }

        pub fn root(&self) -> &Path {
            &self.root
        }

        fn slot_path(&self, slot: usize) -> PathBuf {
            self.root.join(slot_name(slot))
        }

        fn queue(&self) -> PathBuf {
            self.root.join(QUEUE_DIR)
        }

        /// The holders and the live queue, in service order. A dead ticket is
        /// deleted here. The calling process's own slot and ticket are never
        /// opened (see the module docs); the caller accounts for them.
        ///
        /// The queue is read before the slots, on purpose. A waiter that takes
        /// a slot locks it and only then removes its ticket, so a reader that
        /// sees the ticket gone sees the slot held; read the other way round
        /// it could see neither, and serve one waiter too many.
        pub fn board(&self, own: &Own) -> Board {
            let mut board = Board::default();
            self.read_queue(own, &mut board);
            self.read_slots(own, &mut board);
            order(&mut board.waiters, &board.holders);
            board
        }

        fn read_slots(&self, own: &Own, board: &mut Board) {
            for slot in 1..=SLOT_FILES {
                if own.slot == Some(slot) {
                    continue;
                }
                let path = self.slot_path(slot);
                let file = match fs_entry::open_private_record_readonly(&path) {
                    Ok(Some(file)) => file,
                    Ok(None) => continue,
                    Err(why) => {
                        board.unreadable.push(why);
                        continue;
                    }
                };
                match probe(&file) {
                    Ok(Probe::Free) => {}
                    Ok(Probe::Held(pid)) => {
                        let record = fs_entry::read_bounded_nofollow(
                            &self.root.join(format!("{}.json", slot_name(slot))),
                            super::RECORD_MAX_BYTES,
                        )
                        .ok()
                        .and_then(|bytes| parse_record(&bytes))
                        // A record another holder wrote is not this holder's.
                        .filter(|record| record.pid == pid);
                        board.holders.push(Holder { slot, pid, record });
                    }
                    Err(error) => board
                        .unreadable
                        .push(format!("could not probe {}: {error}", path.display())),
                }
            }
        }

        fn read_queue(&self, own: &Own, board: &mut Board) {
            let Ok(entries) = std::fs::read_dir(self.queue()) else {
                board
                    .unreadable
                    .push(format!("could not read {}", self.queue().display()));
                return;
            };
            for entry in entries.take(MAX_QUEUE_SCAN).flatten() {
                let name = entry.file_name().to_string_lossy().into_owned();
                if own.ticket.as_deref() == Some(name.as_str()) || !name.ends_with(".json") {
                    continue;
                }
                let path = entry.path();
                let file = match fs_entry::open_private_record_readonly(&path) {
                    Ok(Some(file)) => file,
                    Ok(None) => continue,
                    Err(why) => {
                        board.unreadable.push(why);
                        continue;
                    }
                };
                match probe(&file) {
                    // Nobody holds the lock: the waiter died.
                    Ok(Probe::Free) => {
                        let _ = std::fs::remove_file(&path);
                    }
                    Ok(Probe::Held(_)) => {
                        use std::io::Read;
                        let mut bytes = Vec::new();
                        let read = (&file)
                            .take(super::RECORD_MAX_BYTES as u64 + 1)
                            .read_to_end(&mut bytes);
                        // Locked, but its record is not written yet: it shows
                        // on the next read.
                        if let (Ok(_), Some(record)) = (read, parse_record(&bytes)) {
                            board.waiters.push(Waiter { name, record });
                        }
                    }
                    Err(error) => board
                        .unreadable
                        .push(format!("could not probe {}: {error}", path.display())),
                }
            }
        }

        /// Puts `record` in the queue. The ticket is locked by this process
        /// from the moment it exists.
        pub fn enqueue(&self, record: &Record) -> Result<Ticket, String> {
            let body = serde_json::to_vec(record).map_err(|e| e.to_string())?;
            for attempt in 0..3u64 {
                let name = format!(
                    "{:020}-{}.json",
                    record.enqueued_mono_ns.saturating_add(attempt),
                    record.pid
                );
                let path = self.queue().join(&name);
                let mut file = match OpenOptions::new()
                    .read(true)
                    .write(true)
                    .create_new(true)
                    .mode(0o600)
                    .custom_flags(libc::O_NOFOLLOW)
                    .open(&path)
                {
                    Ok(file) => file,
                    // The same instant, by a process with the same pid: a
                    // leftover of an earlier run. Take the next name.
                    Err(error) if error.kind() == io::ErrorKind::AlreadyExists => continue,
                    Err(error) => {
                        return Err(format!(
                            "could not create a ticket in {}: {error}",
                            path.display()
                        ))
                    }
                };
                if !try_lock(&file).map_err(|e| format!("could not lock a ticket: {e}"))? {
                    return Err("another process holds this process's own ticket".into());
                }
                file.write_all(&body)
                    .map_err(|e| format!("could not write a ticket: {e}"))?;
                // A reader that saw the file in the instant before the lock
                // took it for dead and deleted it: this ticket is then in no
                // queue, so make another.
                let same = std::fs::metadata(&path)
                    .map(|meta| {
                        file.metadata()
                            .map(|mine| mine.ino() == meta.ino() && mine.dev() == meta.dev())
                            .unwrap_or(false)
                    })
                    .unwrap_or(false);
                if same {
                    return Ok(Ticket { name, path, file });
                }
            }
            Err("a ticket was deleted three times as it was made".into())
        }

        /// Whether `ticket`'s file is still in the queue. A waiter that finds
        /// it gone queues again rather than waiting in no line.
        pub fn still_queued(&self, ticket: &Ticket) -> bool {
            match (std::fs::metadata(&ticket.path), ticket.file.metadata()) {
                (Ok(path), Ok(file)) => path.ino() == file.ino() && path.dev() == file.dev(),
                _ => false,
            }
        }

        /// Tries to take `slot`. `Ok(None)` when another process holds it (the
        /// race was lost: ask again after the next read), `Err` when the file
        /// cannot be used at all.
        pub fn take(&self, slot: usize, record: &Record) -> Result<Option<Grant>, String> {
            let path = self.slot_path(slot);
            let file = fs_entry::open_private_record(&path)?;
            match try_lock(&file) {
                Ok(true) => {}
                Ok(false) => return Ok(None),
                Err(error) => return Err(format!("could not lock {}: {error}", path.display())),
            }
            self.write_record(slot, record)?;
            Ok(Some(Grant {
                slot,
                record: record.clone(),
                file,
            }))
        }

        /// The lowest slot file nobody holds, among the ones `board` saw and
        /// the calling process's own.
        pub fn lowest_free(board: &Board, own: &Own) -> Option<usize> {
            (1..=SLOT_FILES).find(|slot| {
                own.slot != Some(*slot) && !board.holders.iter().any(|h| h.slot == *slot)
            })
        }

        fn write_record(&self, slot: usize, record: &Record) -> Result<(), String> {
            let body = serde_json::to_vec(record).map_err(|e| e.to_string())?;
            let tmp = self
                .root
                .join(format!("{}.json.{}.tmp", slot_name(slot), record.pid));
            let target = self.root.join(format!("{}.json", slot_name(slot)));
            let _ = std::fs::remove_file(&tmp);
            let mut file = OpenOptions::new()
                .write(true)
                .create_new(true)
                .mode(0o600)
                .custom_flags(libc::O_NOFOLLOW)
                .open(&tmp)
                .map_err(|e| format!("could not write {}: {e}", tmp.display()))?;
            file.write_all(&body)
                .and_then(|()| file.flush())
                .map_err(|e| format!("could not write {}: {e}", tmp.display()))?;
            std::fs::rename(&tmp, &target)
                .map_err(|e| format!("could not publish {}: {e}", target.display()))
        }
    }
}

#[cfg(test)]
#[path = "lease_tests.rs"]
mod tests;
