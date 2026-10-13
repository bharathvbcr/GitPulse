//! The one place GitPulse sends a signal to a process.
//!
//! A signal aimed at the wrong number is the worst thing this app can do: a
//! `kill(-1, …)`, a `killpg(0, …)` or a `killpg(1, …)` reaches the user's other
//! processes, and a stale pid reaches a stranger. Every kill, hangup and
//! terminate in the app therefore goes through [`signal_group`] or
//! [`signal_process`], and neither will deliver a signal that these rules do
//! not allow:
//!
//! * the id is greater than 1. 0, 1, negative values and `u32` values that wrap
//!   negative as a `pid_t` (`-1` is "every process I may signal") are refused;
//! * it is never this process, its own process group, or a session this process
//!   belongs to (so an app launched from a terminal cannot hang up the shell
//!   that launched it);
//! * a group is signalled only on a [`Basis`] that says why it is ours:
//!   a group leader [`super::spawn`] created, or a group inside a terminal
//!   session this process created with `setsid`. A leader that the OS now
//!   reports in somebody else's session or group is a reused pid and is
//!   refused;
//! * a single process is signalled only while its start time still matches the
//!   one the caller recorded, so a pid that was recycled after the caller last
//!   looked is never signalled.
//!
//! Every delivered signal is logged with the caller's `who`, so the log answers
//! "what ended that process, and why". Signal 0 (an existence probe) delivers
//! nothing and is neither logged nor refused beyond the id rules.
//!
//! The checks that need the OS are separated from the decision
//! ([`vet_group`], [`vet_process`]) so every refusal is tested without
//! signalling anything.

use std::io;

/// What makes a process group this process's to signal.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Basis {
    /// The group's id is the pid of a child [`super::spawn`] made a group
    /// leader, and its owner has not reaped it.
    Spawned,
    /// The group has a member in terminal session `sid`, which this process
    /// created and whose leader it still owns.
    Session(i32),
}

/// This process's own ids: the things no signal may ever reach.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Me {
    pub pid: i32,
    pub pgrp: i32,
    pub sid: i32,
}

impl Me {
    pub fn current() -> Me {
        // SAFETY: getpid, getpgrp and getsid(0) take no pointers and read only
        // this process's own ids.
        unsafe {
            Me {
                pid: libc::getpid(),
                pgrp: libc::getpgrp(),
                sid: libc::getsid(0),
            }
        }
    }
}

fn refusal(why: impl Into<String>) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidInput, why.into())
}

/// Whether `id` could be an ordinary process or group id. 0 and negative values
/// address "my group" and "everything"; 1 is `launchd`/`init`.
fn plain_id(id: i32, what: &str) -> Result<(), io::Error> {
    if id <= 1 {
        return Err(refusal(format!("refusing to signal {what} {id}")));
    }
    Ok(())
}

/// The decision behind [`signal_group`]: `Ok` when `pgid` may be signalled on
/// `basis`. `pgid_of` and `sid_of` answer for a live process id, and `None`
/// for one that no longer exists, which is not a refusal: a group can outlive
/// its leader, and `killpg` itself reports a group that is gone.
pub fn vet_group(
    pgid: i32,
    basis: Basis,
    me: Me,
    pgid_of: &dyn Fn(i32) -> Option<i32>,
    sid_of: &dyn Fn(i32) -> Option<i32>,
) -> io::Result<()> {
    plain_id(pgid, "process group")?;
    if pgid == me.pgrp || pgid == me.pid {
        return Err(refusal(format!(
            "refusing to signal this process's own group {pgid}"
        )));
    }
    match basis {
        Basis::Spawned => {
            if let Some(group) = pgid_of(pgid) {
                if group != pgid {
                    return Err(refusal(format!(
                        "pid {pgid} is in group {group}, not a group leader this process created"
                    )));
                }
            }
            // A group leader cannot `setsid`, so a spawned child's leader is
            // always in our session; anything else is a reused pid.
            if let Some(session) = sid_of(pgid) {
                if session != me.sid {
                    return Err(refusal(format!(
                        "pid {pgid} is in session {session}, not a child of this process"
                    )));
                }
            }
        }
        Basis::Session(sid) => {
            plain_id(sid, "session")?;
            if sid == me.sid {
                return Err(refusal(format!(
                    "refusing to signal inside this process's own session {sid}"
                )));
            }
            if let Some(session) = sid_of(pgid) {
                if session != sid {
                    return Err(refusal(format!(
                        "group {pgid} is in session {session}, not terminal session {sid}"
                    )));
                }
            }
        }
    }
    Ok(())
}

/// The decision behind [`signal_process`].
pub fn vet_process(
    pid: i32,
    started_us: u64,
    me: Me,
    start_of: &dyn Fn(i32) -> Option<u64>,
) -> io::Result<()> {
    plain_id(pid, "process")?;
    if pid == me.pid {
        return Err(refusal("refusing to signal this process"));
    }
    if started_us == 0 {
        return Err(refusal(
            "no start time was recorded, so the pid cannot be told from a reused one",
        ));
    }
    match start_of(pid) {
        Some(now) if now == started_us => Ok(()),
        Some(_) => Err(refusal(format!(
            "pid {pid} now belongs to a different process"
        ))),
        None => Err(refusal(format!("pid {pid} is no longer running"))),
    }
}

fn os_pgid(pid: i32) -> Option<i32> {
    // SAFETY: getpgid takes a numeric id; a process that is gone answers -1.
    match unsafe { libc::getpgid(pid) } {
        -1 => None,
        group => Some(group),
    }
}

fn os_sid(pid: i32) -> Option<i32> {
    // SAFETY: getsid takes a numeric id; a process that is gone answers -1.
    match unsafe { libc::getsid(pid) } {
        -1 => None,
        session => Some(session),
    }
}

/// Sends `signo` to process group `pgid`, if `basis` says it is ours.
///
/// `Ok(true)` when the signal reached a live member, `Ok(false)` when the group
/// has nothing left to signal, and an `InvalidInput` error, with nothing
/// signalled, when the id or basis is refused.
///
/// `ESRCH` and `EPERM` both mean "nothing left to signal". `EPERM` is what
/// macOS answers for a group whose only member is an unreaped zombie — the
/// normal state between a child exiting and its owner calling `wait` — and
/// reading it as "somebody else's group" would need a recycled pid, which
/// [`vet_group`] refuses before the syscall.
pub fn signal_group(pgid: i32, signo: libc::c_int, basis: Basis, who: &str) -> io::Result<bool> {
    if let Err(error) = vet_group(pgid, basis, Me::current(), &os_pgid, &os_sid) {
        log::warn!(target: "procguard", "signal {signo} to group {pgid} refused for {who}: {error}");
        return Err(error);
    }
    if signo != 0 {
        log::info!(target: "procguard", "signal {signo} to group {pgid} for {who}");
    }
    // SAFETY: `pgid` passed `vet_group`: it is neither 0, 1, negative, nor this
    // process's own group, and is one this process has reason to own.
    if unsafe { libc::killpg(pgid, signo) } == 0 {
        return Ok(true);
    }
    let error = io::Error::last_os_error();
    match error.raw_os_error() {
        Some(libc::ESRCH) | Some(libc::EPERM) => Ok(false),
        _ => Err(error),
    }
}

/// Sends `signo` to the single process `pid`, only while `start_of(pid)` still
/// returns `started_us`: the start time the caller recorded when it decided this
/// process was the one to signal.
pub fn signal_process(
    pid: i32,
    started_us: u64,
    start_of: &dyn Fn(i32) -> Option<u64>,
    signo: libc::c_int,
    who: &str,
) -> io::Result<()> {
    if let Err(error) = vet_process(pid, started_us, Me::current(), start_of) {
        log::warn!(target: "procguard", "signal {signo} to pid {pid} refused for {who}: {error}");
        return Err(error);
    }
    if signo != 0 {
        log::info!(target: "procguard", "signal {signo} to pid {pid} for {who}");
    }
    // SAFETY: `pid` passed `vet_process`: greater than 1, not this process, and
    // still the process whose start time the caller recorded.
    if unsafe { libc::kill(pid, signo) } == 0 {
        Ok(())
    } else {
        Err(io::Error::last_os_error())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const ME: Me = Me {
        pid: 500,
        pgrp: 400,
        sid: 300,
    };
    const NOBODY: &dyn Fn(i32) -> Option<i32> = &|_| None;

    fn vet(pgid: i32, basis: Basis) -> io::Result<()> {
        vet_group(pgid, basis, ME, NOBODY, NOBODY)
    }

    /// The ids that reach more than one process, or the wrong one, are never
    /// delivered — including the `u32` values that wrap negative as a `pid_t`.
    #[test]
    fn ids_that_address_more_than_one_process_are_refused() {
        for id in [0, 1, -1, -2, i32::MIN] {
            for basis in [Basis::Spawned, Basis::Session(77)] {
                let error = vet(id, basis).expect_err(&format!("group {id}"));
                assert_eq!(error.kind(), io::ErrorKind::InvalidInput);
            }
            assert!(vet_process(id, 9, ME, &|_| Some(9)).is_err(), "pid {id}");
        }
        for wrapped in [u32::MAX, u32::MAX - 1, 0x8000_0000] {
            let id = wrapped as i32;
            assert!(id < 0);
            assert!(vet(id, Basis::Spawned).is_err(), "u32 {wrapped} as pid_t");
        }
    }

    #[test]
    fn this_process_and_its_group_and_its_session_are_refused() {
        assert!(vet(ME.pid, Basis::Spawned).is_err());
        assert!(vet(ME.pgrp, Basis::Spawned).is_err());
        assert!(vet(ME.pgrp, Basis::Session(77)).is_err());
        assert!(vet(600, Basis::Session(ME.sid)).is_err(), "own session");
        assert!(vet(600, Basis::Session(1)).is_err(), "session 1");
        assert!(vet(600, Basis::Session(0)).is_err(), "session 0");
        assert!(vet_process(ME.pid, 9, ME, &|_| Some(9)).is_err());
    }

    /// A group the caller did not create is not signalled: a pid that is now a
    /// member of someone else's group, or leads someone else's session.
    #[test]
    fn a_group_this_process_did_not_create_is_refused() {
        // 600 is a process in group 123: not a leader spawn made.
        let in_other_group = |pid: i32| (pid == 600).then_some(123);
        assert!(vet_group(600, Basis::Spawned, ME, &in_other_group, NOBODY).is_err());
        // 600 leads its group but in a session that is not ours.
        let own_group = |pid: i32| (pid == 600).then_some(600);
        let foreign_session = |pid: i32| (pid == 600).then_some(901);
        assert!(vet_group(600, Basis::Spawned, ME, &own_group, &foreign_session).is_err());
        // A terminal group must lie inside the session being closed.
        assert!(vet_group(600, Basis::Session(77), ME, NOBODY, &foreign_session).is_err());
    }

    #[test]
    fn a_group_that_is_ours_passes_and_a_vanished_leader_is_not_a_refusal() {
        let leader = |pid: i32| (pid == 600).then_some(600);
        let ours = |pid: i32| (pid == 600).then_some(ME.sid);
        assert!(vet_group(600, Basis::Spawned, ME, &leader, &ours).is_ok());
        let in_session = |pid: i32| (pid == 600).then_some(77);
        assert!(vet_group(600, Basis::Session(77), ME, NOBODY, &in_session).is_ok());
        // The leader is gone but the group may not be: left to `killpg`.
        assert!(vet_group(600, Basis::Spawned, ME, NOBODY, NOBODY).is_ok());
        assert!(vet_group(600, Basis::Session(77), ME, NOBODY, NOBODY).is_ok());
    }

    /// A pid whose start time moved was recycled after the caller last looked.
    #[test]
    fn a_process_whose_start_time_moved_is_refused() {
        assert!(vet_process(600, 10, ME, &|_| Some(10)).is_ok());
        let reused = vet_process(600, 10, ME, &|_| Some(11)).expect_err("reused pid");
        assert!(reused.to_string().contains("different process"), "{reused}");
        assert!(vet_process(600, 10, ME, &|_| None).is_err(), "gone");
        assert!(vet_process(600, 0, ME, &|_| Some(0)).is_err(), "unrecorded");
    }

    /// The real syscall path refuses without signalling: these would end the
    /// test runner (or its whole group) if the guard let them through, so they
    /// are asserted from the safe side only.
    #[test]
    fn the_syscall_wrappers_refuse_the_ids_that_would_reach_everything() {
        for id in [0, 1, -1, i32::MIN] {
            assert!(signal_group(id, libc::SIGKILL, Basis::Spawned, "test").is_err());
            assert!(signal_group(id, libc::SIGKILL, Basis::Session(2), "test").is_err());
            assert!(signal_process(id, 1, &|_| Some(1), libc::SIGKILL, "test").is_err());
        }
        let me = Me::current();
        for target in [me.pid, me.pgrp] {
            assert!(signal_group(target, libc::SIGKILL, Basis::Spawned, "test").is_err());
        }
        assert!(signal_process(me.pid, 1, &|_| Some(1), libc::SIGKILL, "test").is_err());
    }
}
