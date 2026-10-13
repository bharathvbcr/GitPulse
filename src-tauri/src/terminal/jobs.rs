//! Stopping the jobs of a terminal that is being closed.
//!
//! A close hangs up every process group of the terminal's session, gives them
//! [`super::HANGUP_GRACE`] to clean up (an agent CLI runs its SessionEnd hooks and
//! flushes its transcript on SIGHUP, which takes seconds), and kills the
//! groups still standing. It runs in three steps so that the one that waits is
//! the one that holds nothing:
//!
//! 1. [`hang_up`], under the shell's lock: list the session, signal it.
//! 2. [`wait_for_exit`], with no lock held: sleep on the jobs' exit.
//! 3. [`kill_stragglers`], under the lock again: list the session once more
//!    and kill what is left.
//!
//! The lock is what keeps the shell's pid, and so the session it leads, from
//! being reaped and reused while a signal is aimed at that session, so the
//! caller holds it for steps 1 and 3. Step 2 does not need it: nothing reaps
//! the shell while a close is in progress (see `ClosingGuard` in `mod.rs`), and
//! step 2 signals nobody. **What is signalled is decided only by listing the
//! session at steps 1 and 3.** The processes step 2 watches are never a signal
//! target, so waiting on their exit cannot widen who a close reaches.
use super::readiness::{ExitWatch, Watch};
use std::collections::{BTreeSet, HashSet};
use std::thread;
use std::time::{Duration, Instant};

/// The listing of a session's processes as `(pid, process group)`. A test
/// supplies its own to count how often the process table is read.
pub(super) type Members<'a> = &'a dyn Fn(i32) -> Result<Vec<(i32, i32)>, String>;

/// How often the session is listed when the exit of its jobs cannot be heard:
/// the kernel facility is missing, refused a pid, or a job is a zombie its
/// parent has not reaped (listed, but its exit has already been delivered).
const POLL: Duration = Duration::from_millis(20);

/// The jobs a hangup was sent to.
pub(super) struct Hung {
    sid: i32,
    pids: Vec<i32>,
}

/// The process groups of session `sid` other than the one this process is in.
/// Groups are found by session id, not read off the tty: the tty names one
/// foreground group, and `cmd &`, `nohup`, an agent's dev server, a `^Z`'d
/// editor all live in others. Only groups with a member in this session are
/// returned, and never this process's own.
fn listed(sid: i32, members: Members) -> Result<Vec<(i32, i32)>, String> {
    // SAFETY: getpgrp has no arguments and cannot fail.
    let own = unsafe { libc::getpgrp() };
    Ok(members(sid)?
        .into_iter()
        .filter(|(_, group)| *group != own)
        .collect())
}

fn groups_of(found: &[(i32, i32)]) -> BTreeSet<i32> {
    found.iter().map(|(_, group)| *group).collect()
}

fn signal(sid: i32, signo: libc::c_int, groups: &BTreeSet<i32>) {
    for group in groups {
        // The group has a member in the session this call owns (the leader
        // cannot be reaped while a close is in progress). The primitive still
        // refuses an id that is not an ordinary group, our own group or
        // session, and a group whose leader is now in another session.
        let _ = crate::procguard::signal::signal_group(
            *group,
            signo,
            crate::procguard::signal::Basis::Session(sid),
            "terminal close",
        );
    }
}

/// Hangs up every process group in the session `sid` (and continues it, so a
/// stopped job can act on the hangup). `None` when the session holds nothing
/// but its leader, or could not be listed; otherwise the jobs to wait for.
pub(super) fn hang_up(sid: i32, members: Members) -> Option<Hung> {
    let first = match listed(sid, members) {
        Ok(found) if found.is_empty() => return None,
        Ok(found) => found,
        Err(error) => {
            log::warn!(target: "terminal", "could not list the terminal's jobs: {error}");
            return None;
        }
    };
    let groups = groups_of(&first);
    signal(sid, libc::SIGHUP, &groups);
    signal(sid, libc::SIGCONT, &groups);
    Some(Hung {
        sid,
        pids: first.into_iter().map(|(pid, _)| pid).collect(),
    })
}

/// Waits up to `grace` for the jobs that were hung up, and any forked since,
/// to exit. `true` when the session then holds nothing but its leader.
///
/// Sleeps in the kernel on each job's exit rather than reading the process
/// table every lap. The table is read once the watched jobs have all gone, to
/// confirm and to find what they forked; a job that cannot be watched, or one
/// that is listed but already gone, is waited for the old way, one lap at a
/// time.
pub(super) fn wait_for_exit(hung: Hung, grace: Duration, members: Members) -> bool {
    let Hung { sid, pids } = hung;
    let deadline = Instant::now() + grace;
    let mut watch = ExitWatch::new();
    let mut registered: HashSet<i32> = HashSet::new();
    let mut pending = 0usize;
    let mut pids = pids;
    loop {
        let mut polled = false;
        for pid in pids.drain(..) {
            if !registered.insert(pid) {
                continue;
            }
            match watch.as_mut().map_or(Watch::Unavailable, |w| w.watch(pid)) {
                Watch::Watching => pending += 1,
                Watch::Gone => {}
                Watch::Unavailable => polled = true,
            }
        }
        while pending > 0 {
            let Some(left) = deadline.checked_duration_since(Instant::now()) else {
                break;
            };
            let heard = watch.as_mut().map(|w| w.wait(left));
            match heard {
                Some(Ok(exited)) => pending = pending.saturating_sub(exited),
                // A wait that failed cannot be trusted to hear the rest.
                Some(Err(_)) | None => {
                    polled = true;
                    break;
                }
            }
        }
        let now = Instant::now();
        if now >= deadline {
            return false;
        }
        match listed(sid, members) {
            Ok(found) if found.is_empty() => return true,
            Ok(found) => {
                pids = found.into_iter().map(|(pid, _)| pid).collect();
                // Everything listed is already registered and nothing is left
                // to hear: zombies awaiting their parent, or jobs that cannot
                // be watched. Ask again in a lap, as the loop always did.
                if polled || pids.iter().all(|pid| registered.contains(pid)) {
                    thread::sleep(POLL.min(deadline.saturating_duration_since(now)));
                }
            }
            Err(error) => {
                log::warn!(target: "terminal", "could not list the terminal's jobs: {error}");
                return false;
            }
        }
    }
}

/// Kills the groups still standing in the session `sid`. A job forked while
/// the first sweep ran is found by the next one; two passes settle a fork bomb
/// no worse than the kernel's own limits.
pub(super) fn kill_stragglers(sid: i32, members: Members) {
    for _ in 0..2 {
        match listed(sid, members) {
            Ok(left) if left.is_empty() => return,
            Ok(left) => signal(sid, libc::SIGKILL, &groups_of(&left)),
            Err(error) => {
                log::warn!(target: "terminal", "could not list the terminal's jobs: {error}");
                return;
            }
        }
        thread::sleep(POLL);
    }
}

/// All three steps in a row, for a caller (a test) that holds no lock of its
/// own. A close holds the shell's lock for the first and last only.
#[cfg(test)]
pub(super) fn stop_with(sid: i32, grace: Duration, members: Members) {
    let Some(hung) = hang_up(sid, members) else {
        return;
    };
    if !wait_for_exit(hung, grace, members) {
        kill_stragglers(sid, members);
    }
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use crate::procguard::LockedSpawn;
    use std::process::Command;
    use std::sync::atomic::{AtomicUsize, Ordering};

    /// The pid of a process that has exited and been reaped.
    fn gone_pid() -> i32 {
        let mut done = Command::new("true").spawn_locked().expect("spawn true");
        let pid = i32::try_from(done.id()).expect("pid");
        done.wait().expect("reap");
        pid
    }

    /// A job the process table still lists after its exit has been delivered
    /// (a zombie its parent has not reaped) can never be heard exiting again.
    /// The wait asks again a lap at a time; it neither spins on the table nor
    /// gives up on the job.
    #[test]
    fn a_job_still_listed_after_its_exit_is_asked_about_a_lap_at_a_time() {
        let pid = gone_pid();
        let scans = AtomicUsize::new(0);
        let started = Instant::now();
        let done = wait_for_exit(
            Hung {
                sid: 4242,
                pids: vec![pid],
            },
            Duration::from_secs(5),
            &|_| {
                let before = scans.fetch_add(1, Ordering::SeqCst);
                Ok(if before < 4 {
                    vec![(pid, 4243)]
                } else {
                    Vec::new()
                })
            },
        );
        let waited = started.elapsed();
        assert!(done, "the session emptied, so the wait is over");
        assert_eq!(scans.load(Ordering::SeqCst), 5);
        assert!(
            waited >= POLL * 3,
            "listed four times in {waited:?}: the laps were skipped"
        );
        assert!(waited < crate::test_support::coverage_relaxed(Duration::from_secs(2)));
    }

    #[test]
    fn a_job_that_stays_listed_runs_out_the_grace_and_the_wait_says_so() {
        let pid = gone_pid();
        let scans = AtomicUsize::new(0);
        let grace = Duration::from_millis(200);
        let started = Instant::now();
        let done = wait_for_exit(
            Hung {
                sid: 4242,
                pids: vec![pid],
            },
            grace,
            &|_| {
                scans.fetch_add(1, Ordering::SeqCst);
                Ok(vec![(pid, 4243)])
            },
        );
        assert!(!done, "a job is still there, so the caller must kill it");
        assert!(started.elapsed() >= grace);
        let scans = scans.load(Ordering::SeqCst);
        assert!(
            (2..=20).contains(&scans),
            "{scans} reads of the table in {grace:?}"
        );
    }

    /// A job forked while the wait was under way is found by the next listing
    /// and waited for too.
    #[test]
    fn a_job_forked_during_the_wait_is_waited_for() {
        let pid = gone_pid();
        let mut later = Command::new("sleep")
            .arg("0.3")
            .spawn_locked()
            .expect("sleep");
        let later_pid = i32::try_from(later.id()).expect("pid");
        let scans = AtomicUsize::new(0);
        let started = Instant::now();
        let done = wait_for_exit(
            Hung {
                sid: 4242,
                pids: vec![pid],
            },
            Duration::from_secs(5),
            &|_| {
                let before = scans.fetch_add(1, Ordering::SeqCst);
                Ok(if before == 0 {
                    vec![(later_pid, 4244)]
                } else {
                    Vec::new()
                })
            },
        );
        assert!(done);
        assert_eq!(scans.load(Ordering::SeqCst), 2);
        assert!(
            started.elapsed() >= Duration::from_millis(250),
            "the new job's exit was not waited for ({:?})",
            started.elapsed()
        );
        later.wait().expect("reap");
    }

    /// A listing that cannot be read is never "nobody is there".
    #[test]
    fn a_listing_that_fails_is_not_reported_as_an_empty_session() {
        let pid = gone_pid();
        let done = wait_for_exit(
            Hung {
                sid: 4242,
                pids: vec![pid],
            },
            Duration::from_secs(2),
            &|_| Err("process table unreadable".into()),
        );
        assert!(!done);
    }

    /// Nothing is signalled for a session that is empty or cannot be listed.
    #[test]
    fn hanging_up_an_empty_or_unreadable_session_signals_and_waits_for_nothing() {
        assert!(hang_up(4242, &|_| Ok(Vec::new())).is_none());
        assert!(hang_up(4242, &|_| Err("unreadable".into())).is_none());
    }

    /// This process's own group is never a target, whatever the listing says.
    #[test]
    fn the_listing_never_names_this_processs_own_group() {
        // SAFETY: getpgrp has no arguments and cannot fail.
        let own = unsafe { libc::getpgrp() };
        let found = listed(4242, &|_| Ok(vec![(10, own), (11, 4243)])).unwrap();
        assert_eq!(found, vec![(11, 4243)]);
    }
}
