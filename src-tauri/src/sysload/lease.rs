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
//! applies and [`LeaseCapacity::note`] says why it was not lowered: a reading that
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
pub struct LeaseRecord {
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

impl LeaseRecord {
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
pub struct LeaseHolder {
    pub slot: usize,
    pub pid: i32,
    /// `None` is a lock with no record, or a record another process wrote.
    pub record: Option<LeaseRecord>,
}

/// A live ticket.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct LeaseWaiter {
    pub name: String,
    pub record: LeaseRecord,
}

/// The holders and the live queue at one moment.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
pub struct Board {
    pub holders: Vec<LeaseHolder>,
    /// In service order.
    pub waiters: Vec<LeaseWaiter>,
    /// Slot files and tickets that could not be read: counted, never read as
    /// "free".
    pub unreadable: Vec<String>,
    /// The slot numbers among them. A slot nobody can read may be held, so it
    /// counts against the capacity and is never offered as free.
    pub unreadable_slots: Vec<usize>,
}

impl Board {
    /// Slots in use: the ones with a holder and the ones that could not be
    /// read.
    pub fn occupied(&self) -> usize {
        self.holders.len() + self.unreadable_slots.len()
    }
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
pub struct LeaseCapacity {
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
pub fn capacity(configured: u32, load: &Load) -> LeaseCapacity {
    let configured = configured.clamp(1, MAX_SLOTS);
    match load {
        Load::Unknown(why) => LeaseCapacity {
            configured,
            slots: configured,
            refuses: false,
            tier: None,
            note: Some(format!("slots not lowered for load: {why}")),
        },
        Load::Verdict(verdict) if !verdict.admit || verdict.tier >= Tier::Severe => LeaseCapacity {
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
            LeaseCapacity {
                configured,
                slots,
                refuses: false,
                tier: Some(verdict.tier),
                note: (slots < configured)
                    .then(|| format!("halved from {configured} while the host load is elevated")),
            }
        }
        Load::Verdict(verdict) => LeaseCapacity {
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
pub fn may_take(rank: usize, held: usize, capacity: &LeaseCapacity) -> bool {
    let free = (capacity.slots as usize).saturating_sub(held);
    rank < free
}

/// Sorts `waiters` into service order: priority, then how many slots the
/// waiter's session already holds (fewest first), then enqueue time, then pid.
pub fn order(waiters: &mut [LeaseWaiter], holders: &[LeaseHolder]) {
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
    // A task run is a run even when it runs in a PTY of its own: only a
    // person's terminal has a terminal session id and no run id.
    if let Some(id) = run_id.filter(plain) {
        return (format!("run:{id}"), Priority::Run);
    }
    if let Some(id) = terminal_session.filter(plain) {
        return (format!("terminal:{id}"), Priority::Terminal);
    }
    match ancestor {
        Some((pid, start)) => (format!("agent:{pid}-{start}"), Priority::Observed),
        None => (
            format!("process:{}-{}", parent.0, parent.1),
            Priority::Observed,
        ),
    }
}

/// Everything Health shows about build leases, in one read. A field that
/// could not be read carries why; none of them is ever a zero standing in for
/// "unknown".
#[derive(Debug, Clone, Serialize)]
pub struct LeaseStatus {
    /// Leases are switched on in Settings.
    pub on: bool,
    /// The setting, before the governor lowers it.
    pub configured: u32,
    /// How many may run now, and why that many.
    pub capacity: LeaseCapacity,
    pub holders: Vec<LeaseHolder>,
    /// In service order.
    pub queue: Vec<LeaseWaiter>,
    /// Slot files and tickets that could not be read.
    pub unreadable: Vec<String>,
    /// Why the lease directory could not be used at all.
    pub error: Option<String>,
    /// Why the `cargo` and `go` links an agent's PATH starts with are not in
    /// place, or `None` when they are.
    pub links_missing: Option<String>,
    /// Builds that ran unleased in the last 15 minutes, counted from markers.
    pub misses: Option<u32>,
    /// The marker directory held more entries than one read looks at, so
    /// `misses` is a floor.
    pub misses_truncated: bool,
    pub misses_error: Option<String>,
    /// Heavy builds running that hold no slot (and have no holder above them):
    /// something that never went through the wrapper.
    pub unleased_running: Option<u32>,
    pub unleased_error: Option<String>,
    /// The monotonic clock the records are dated on, to age them.
    pub now_mono_ns: u64,
}

/// Counts the leased builds among `rows` (`pid`, `ppid`, whether its argv is a
/// leased build) that hold no slot, are not a wrapper waiting for one, and are
/// not the child of another counted build. `covered` holds the pids of slot
/// holders and of waiting wrappers; a descendant of one is covered too. A
/// proxy that starts the real tool as a child (rustup's `cargo`) is one build.
pub fn count_unleased(rows: &[(i32, i32, bool)], covered: &std::collections::HashSet<i32>) -> u32 {
    let parent: std::collections::HashMap<i32, i32> =
        rows.iter().map(|&(pid, ppid, _)| (pid, ppid)).collect();
    let leased: std::collections::HashSet<i32> = rows
        .iter()
        .filter(|(_, _, leased)| *leased)
        .map(|&(pid, _, _)| pid)
        .collect();
    let mut count = 0;
    for &pid in &leased {
        let mut at = pid;
        let mut skip = false;
        for step in 0..32 {
            if covered.contains(&at) || (step > 0 && leased.contains(&at)) {
                skip = true;
                break;
            }
            match parent.get(&at) {
                Some(next) if *next > 1 && *next != at => at = *next,
                _ => break,
            }
        }
        if !skip {
            count += 1;
        }
    }
    count
}

/// Counts heavy builds in the process table that hold no slot. `Err` when the
/// table could not be read.
#[cfg(unix)]
pub fn unleased_running(holders: &[LeaseHolder], waiters: &[LeaseWaiter]) -> Result<u32, String> {
    let table = super::procscan::signal_table()?;
    let covered: std::collections::HashSet<i32> = holders
        .iter()
        .map(|h| h.pid)
        .chain(waiters.iter().map(|w| w.record.pid))
        .collect();
    let rows: Vec<(i32, i32, bool)> = table
        .rows
        .iter()
        .map(|row| {
            let leased = row.real_uid() == table.me_uid
                && row.argv.as_ref().is_some_and(|argv| {
                    argv.split_first()
                        .is_some_and(|(program, args)| crate::hooks::build_is_leased(program, args))
                });
            (row.pid, row.ppid, leased)
        })
        .collect();
    Ok(count_unleased(&rows, &covered))
}

#[cfg(not(unix))]
pub fn unleased_running(_holders: &[LeaseHolder], _waiters: &[LeaseWaiter]) -> Result<u32, String> {
    Err("build leases are not supported on this platform".into())
}

/// The lease state for Health: the setting, the governor's verdict, the board.
/// `scan_processes` false skips the process-table read behind
/// `unleased_running`.
pub fn status(
    config_dir: Option<&Path>,
    settings: &crate::tool_config::HostLoadLeases,
    load: &Load,
    misses: Result<(usize, bool), String>,
    scan_processes: bool,
) -> LeaseStatus {
    let capacity = capacity(settings.slots, load);
    let mut out = LeaseStatus {
        on: settings.mode == crate::tool_config::LeaseMode::On,
        configured: settings.slots,
        capacity,
        holders: Vec::new(),
        queue: Vec::new(),
        unreadable: Vec::new(),
        error: None,
        links_missing: None,
        misses: None,
        misses_truncated: false,
        misses_error: None,
        unleased_running: None,
        unleased_error: None,
        now_mono_ns: super::Clock::now().mono_ns,
    };
    match misses {
        Ok((n, truncated)) => {
            out.misses = Some(u32::try_from(n).unwrap_or(u32::MAX));
            out.misses_truncated = truncated;
        }
        Err(why) => out.misses_error = Some(why),
    }
    let Some(config) = config_dir else {
        out.error = Some("there is no per-user config directory (HOME unset)".into());
        return out;
    };
    let bin = bin_dir(config);
    let hook = std::env::current_exe()
        .ok()
        .and_then(|exe| exe.parent().map(|dir| dir.join("gitpulse-hook")));
    out.links_missing = links_problem(&bin, hook.as_deref());
    #[cfg(unix)]
    {
        match Dir::open(&lease_dir(config)) {
            Ok(dir) => {
                let board = dir.board(&Own::default());
                out.holders = board.holders;
                out.queue = board.waiters;
                out.unreadable = board.unreadable;
            }
            Err(why) => out.error = Some(why),
        }
    }
    #[cfg(not(unix))]
    {
        out.error = Some("build leases are not supported on this platform".into());
    }
    if scan_processes {
        match unleased_running(&out.holders, &out.queue) {
            Ok(n) => out.unleased_running = Some(n),
            Err(why) => out.unleased_error = Some(why),
        }
    } else {
        // The process table is the costly part of a read. A caller that asked
        // for the queue alone is told this was not read, not that it was zero.
        out.unleased_error = Some("not read: this was a queue-only read".into());
    }
    out
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

fn parse_record(bytes: &[u8]) -> Option<LeaseRecord> {
    if bytes.len() > RECORD_MAX_BYTES {
        return None;
    }
    serde_json::from_slice::<LeaseRecord>(bytes)
        .ok()
        .map(LeaseRecord::bounded)
}

/// The programs the app links into [`bin_dir`].
pub const LINKED_PROGRAMS: [&str; 2] = ["cargo", "go"];

/// Why the links in `bin` do not work, or `None` when every one resolves to a
/// file (a dangling link is not a link) and, where `hook` is known, to that
/// binary.
pub fn links_problem(bin: &Path, hook: Option<&Path>) -> Option<String> {
    let want = hook.map(|h| std::fs::canonicalize(h).unwrap_or_else(|_| h.to_path_buf()));
    for name in LINKED_PROGRAMS {
        let link = bin.join(name);
        if link.symlink_metadata().is_err() {
            return Some(format!("{} does not exist", link.display()));
        }
        let Ok(target) = std::fs::canonicalize(&link) else {
            return Some(format!(
                "{} points at something that does not exist",
                link.display()
            ));
        };
        if !target.is_file() {
            return Some(format!("{} does not point at a file", link.display()));
        }
        if want.as_ref().is_some_and(|want| *want != target) {
            return Some(format!(
                "{} points at {}, not at this GitPulse's gitpulse-hook",
                link.display(),
                target.display()
            ));
        }
    }
    None
}

/// `PATH` with `bin` first, once: an entry already there is moved to the
/// front, never duplicated.
pub fn path_with_bin_first(bin: &Path, existing: Option<&std::ffi::OsStr>) -> std::ffi::OsString {
    let mut entries: Vec<PathBuf> = vec![bin.to_path_buf()];
    if let Some(existing) = existing {
        entries.extend(std::env::split_paths(existing).filter(|entry| entry != bin));
    }
    std::env::join_paths(&entries).unwrap_or_else(|_| bin.as_os_str().to_os_string())
}

/// What a session GitPulse launches is given so its builds go through the
/// wrapper: `PATH` led by [`bin_dir`] and `GITPULSE_LEASE_BIN`. `None` for a
/// value means "remove it": a stale one inherited from a parent GitPulse must
/// not claim a gate this session does not have. Nothing is added unless the
/// links exist, so a session is never pointed at a folder with nothing in it.
pub fn launch_env(
    config_dir: Option<&Path>,
    existing_path: Option<&std::ffi::OsStr>,
    enabled: bool,
) -> Vec<(&'static str, Option<std::ffi::OsString>)> {
    let bin = config_dir.map(bin_dir);
    let usable = enabled
        && bin
            .as_deref()
            .is_some_and(|bin| links_problem(bin, None).is_none());
    match (usable, bin) {
        (true, Some(bin)) => vec![
            ("PATH", Some(path_with_bin_first(&bin, existing_path))),
            ("GITPULSE_LEASE_BIN", Some(bin.into_os_string())),
        ],
        _ => vec![("GITPULSE_LEASE_BIN", None)],
    }
}

/// Makes `<config>/lease-bin/{cargo,go}` symlinks to `hook`, replacing a
/// wrong or missing one by rename so a build never finds the folder half made.
/// `Err` says what could not be done; links already right are left alone.
#[cfg(unix)]
pub fn ensure_links(config_dir: &Path, hook: &Path) -> Result<(), String> {
    let bin = bin_dir(config_dir);
    std::fs::create_dir_all(config_dir)
        .map_err(|e| format!("cannot create {}: {e}", config_dir.display()))?;
    crate::hooks::advisory_once::private_dir(&bin)
        .map_err(|why| format!("cannot use {}: {why}", bin.display()))?;
    if !hook.is_file() {
        return Err(format!(
            "{} does not exist, so there is nothing for the cargo and go links to point at",
            hook.display()
        ));
    }
    for name in LINKED_PROGRAMS {
        let link = bin.join(name);
        if std::fs::read_link(&link).is_ok_and(|target| target == hook) {
            continue;
        }
        let tmp = bin.join(format!(".{name}.{}.tmp", std::process::id()));
        let _ = std::fs::remove_file(&tmp);
        std::os::unix::fs::symlink(hook, &tmp)
            .map_err(|e| format!("cannot link {}: {e}", tmp.display()))?;
        std::fs::rename(&tmp, &link).map_err(|e| {
            let _ = std::fs::remove_file(&tmp);
            format!("cannot replace {}: {e}", link.display())
        })?;
    }
    Ok(())
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
        order, parse_record, slot_name, Board, LeaseHolder, LeaseRecord, LeaseWaiter,
        MAX_QUEUE_SCAN, QUEUE_DIR, SLOT_FILES,
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
        pub record: LeaseRecord,
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
                        board.unreadable_slots.push(slot);
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
                        board.holders.push(LeaseHolder { slot, pid, record });
                    }
                    Err(error) => {
                        board
                            .unreadable
                            .push(format!("could not probe {}: {error}", path.display()));
                        board.unreadable_slots.push(slot);
                    }
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
                            board.waiters.push(LeaseWaiter { name, record });
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
        pub fn enqueue(&self, record: &LeaseRecord) -> Result<Ticket, String> {
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
        pub fn take(&self, slot: usize, record: &LeaseRecord) -> Result<Option<Grant>, String> {
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
            Self::free_slots(board, own).next()
        }

        /// Every slot file nobody holds and that could be read, lowest first.
        pub fn free_slots<'a>(board: &'a Board, own: &'a Own) -> impl Iterator<Item = usize> + 'a {
            (1..=SLOT_FILES).filter(move |slot| {
                own.slot != Some(*slot)
                    && !board.holders.iter().any(|h| h.slot == *slot)
                    && !board.unreadable_slots.contains(slot)
            })
        }

        fn write_record(&self, slot: usize, record: &LeaseRecord) -> Result<(), String> {
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
