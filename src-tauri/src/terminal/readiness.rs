//! What a PTY reader sleeps on while its session is idle.
//!
//! The reader used to poll the master for 100 ms and ask `waitpid` whether the
//! shell had exited on every lap: ten wakeups a second per session for as long
//! as it lived, nothing in them but a question the kernel answers by itself.
//! A session now sleeps in one kernel wait that ends on exactly the two things
//! it is waiting for: the master has something to read (or hung up), or the
//! shell exited. macOS uses a kqueue (`EVFILT_READ` on the master,
//! `EVFILT_PROC`/`NOTE_EXIT` on the child), Linux an epoll set holding the
//! master and a pidfd.
//!
//! A readiness wait is a hint, never a verdict. Whether the shell exited is
//! decided by `try_wait` (see [`Readiness::take_exit_hint`]), because the
//! exit notification can be missed and must not be the only road to that
//! answer: a child that exited before the watch was registered is reported
//! as `ESRCH`, a registration can fail outright, and a pid can be reused.
//! Every such case falls back to a slow safety net ([`SAFETY_NET`]), or to the
//! legacy 100 ms lap when exits cannot be watched at all ([`POLL_LAP`]).
use std::io;
use std::os::fd::{OwnedFd, RawFd};
use std::time::Duration;

/// How long a session whose exit is being watched sleeps before asking anyway.
/// It exists for the exit notification that never comes; a healthy session
/// never reaches it with output and exit both silent for this long without
/// the answer being "still running", so the cost of the net is one wakeup per
/// session per this interval.
pub(super) const SAFETY_NET: Duration = Duration::from_secs(300);

/// The wait when exits cannot be watched: the legacy lap.
pub(super) const POLL_LAP: Duration = Duration::from_millis(100);

/// How a wait is set up. [`Options::default`] is production.
#[derive(Clone, Copy)]
pub(super) struct Options {
    /// Sleep this long at most when the exit is being watched.
    pub safety_net: Duration,
    /// Register the child's exit. `false` is the condition under which the
    /// kernel notification was lost: the wait then knows only the safety net.
    pub watch_exit: bool,
    /// The exit is registered and then never heard: the kernel's notification
    /// lost in transit, which the safety net exists for. Tests only.
    #[cfg(test)]
    pub deaf: bool,
}

impl Default for Options {
    fn default() -> Self {
        Self {
            safety_net: SAFETY_NET,
            watch_exit: true,
            #[cfg(test)]
            deaf: false,
        }
    }
}

enum Backend {
    #[cfg(target_os = "macos")]
    Kqueue { kq: OwnedFd },
    #[cfg(target_os = "linux")]
    Epoll { ep: OwnedFd, _pidfd: OwnedFd },
    /// Anything else, or a platform facility that refused to start.
    Poll,
}

/// One session's idle wait.
pub(super) struct Readiness {
    backend: Backend,
    master: RawFd,
    /// The child's exit is registered, so a quiet session needs no lap.
    watching_exit: bool,
    safety_net: Duration,
    /// The next `try_wait` may find the shell gone: it has not been asked since
    /// the watch was registered, or the last wait ended on an exit, a timer or
    /// a lap rather than on output.
    exit_hint: bool,
}

impl Readiness {
    pub(super) fn new(master: RawFd, child: Option<u32>) -> Self {
        Self::with_options(master, child, Options::default())
    }

    pub(super) fn with_options(master: RawFd, child: Option<u32>, options: Options) -> Self {
        let child = child.filter(|_| options.watch_exit);
        // A deaf watch registers nothing and still believes it is listening.
        #[cfg(test)]
        let deaf = options.deaf;
        #[cfg(not(test))]
        let deaf = false;
        let (backend, mut watching_exit) = match open(master, child.filter(|_| !deaf)) {
            Ok(opened) => opened,
            Err(error) => {
                log::warn!(
                    target: "terminal",
                    "terminal readiness unavailable; the reader will poll: {error}"
                );
                (Backend::Poll, false)
            }
        };
        if deaf && child.is_some() && !matches!(backend, Backend::Poll) {
            watching_exit = true;
        }
        Self {
            backend,
            master,
            watching_exit,
            safety_net: options.safety_net,
            exit_hint: true,
        }
    }

    /// Whether an idle session's exit is delivered by the kernel (rather than
    /// found by a lap).
    #[cfg(test)]
    pub(super) fn watching_exit(&self) -> bool {
        self.watching_exit
    }

    /// Whether the caller should ask the child if it has exited, clearing the
    /// hint. True before the first wait (the child may have exited before it
    /// was watched) and after every wait that did not end on output alone.
    pub(super) fn take_exit_hint(&mut self) -> bool {
        std::mem::take(&mut self.exit_hint)
    }

    /// Sleeps until the master is readable or has hung up, the child exits, or
    /// the safety net (or, when exits cannot be watched, the lap) elapses.
    /// An interruption returns early and is not an error. The tty is
    /// level-triggered: output the caller has not read ends the next wait at
    /// once, so call this only once the master has answered `WouldBlock`.
    pub(super) fn wait(&mut self) -> io::Result<()> {
        let timeout = if self.watching_exit {
            self.safety_net
        } else {
            POLL_LAP
        };
        let woke = match &self.backend {
            #[cfg(target_os = "macos")]
            Backend::Kqueue { kq } => kqueue_wait(kq, timeout)?,
            #[cfg(target_os = "linux")]
            Backend::Epoll { ep, .. } => epoll_wait(ep, timeout)?,
            Backend::Poll => super::input::wait_for_output_within(self.master, timeout)
                .map(|output| Woke {
                    output,
                    exited: false,
                })?,
        };
        // Output alone proves nothing about the child; anything else (an exit,
        // a timer, a lap, an interruption) is a reason to ask.
        // Unwatched, there is no exit to hear about: every wake asks, as the
        // reader always did.
        if woke.exited || !woke.output || !self.watching_exit {
            self.exit_hint = true;
        }
        Ok(())
    }
}

/// What ended a wait.
struct Woke {
    output: bool,
    exited: bool,
}

#[cfg(target_os = "macos")]
fn open(master: RawFd, child: Option<u32>) -> io::Result<(Backend, bool)> {
    use std::os::fd::FromRawFd;
    // `kqueue()` returns a descriptor without close-on-exec; the two calls
    // are not atomic, so a concurrent spawn could inherit it. Spawns hold the
    // inheritance lock across their fork.
    let kq = crate::procguard::with_inheritance_lock(|| {
        // SAFETY: kqueue takes no arguments; the result is checked below.
        let kq = unsafe { libc::kqueue() };
        if kq < 0 {
            return Err(io::Error::last_os_error());
        }
        // SAFETY: kq is a fresh descriptor this call owns.
        let owned = unsafe { OwnedFd::from_raw_fd(kq) };
        // SAFETY: setting FD_CLOEXEC on a descriptor this call owns.
        if unsafe { libc::fcntl(kq, libc::F_SETFD, libc::FD_CLOEXEC) } < 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(owned)
    })?;
    use std::os::fd::AsRawFd;
    // Level-triggered: output not yet read keeps the master ready.
    kevent_change(
        kq.as_raw_fd(),
        master as libc::uintptr_t,
        libc::EVFILT_READ,
        libc::EV_ADD | libc::EV_ENABLE,
        0,
    )?;
    let watching = match child {
        Some(pid) => match kevent_change(
            kq.as_raw_fd(),
            pid as libc::uintptr_t,
            libc::EVFILT_PROC,
            libc::EV_ADD | libc::EV_ENABLE | libc::EV_ONESHOT,
            libc::NOTE_EXIT,
        ) {
            Ok(()) => true,
            // The child is already gone (and reaped); the first `try_wait`
            // finds that out. Not a failure to watch: nothing is left to watch.
            Err(error) if error.raw_os_error() == Some(libc::ESRCH) => true,
            Err(error) => {
                log::warn!(
                    target: "terminal",
                    "could not watch a terminal child's exit; the reader will poll: {error}"
                );
                false
            }
        },
        None => false,
    };
    Ok((Backend::Kqueue { kq }, watching))
}

#[cfg(target_os = "macos")]
fn kevent_change(
    kq: RawFd,
    ident: libc::uintptr_t,
    filter: i16,
    flags: u16,
    fflags: u32,
) -> io::Result<()> {
    let change = libc::kevent {
        ident,
        filter,
        flags,
        fflags,
        data: 0,
        udata: std::ptr::null_mut(),
    };
    // SAFETY: one initialized change record, no event list.
    let rc = unsafe { libc::kevent(kq, &change, 1, std::ptr::null_mut(), 0, std::ptr::null()) };
    if rc < 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(())
}

#[cfg(target_os = "macos")]
fn kqueue_wait(kq: &OwnedFd, timeout: Duration) -> io::Result<Woke> {
    use std::os::fd::AsRawFd;
    let limit = libc::timespec {
        tv_sec: timeout.as_secs().min(i64::MAX as u64) as libc::time_t,
        tv_nsec: libc::c_long::from(timeout.subsec_nanos() as i32),
    };
    // SAFETY: an all-zero kevent is a valid (empty) record.
    let mut events: [libc::kevent; 2] = unsafe { std::mem::zeroed() };
    // SAFETY: the event list is two initialized records and `limit` outlives
    // the call.
    let ready = unsafe {
        libc::kevent(
            kq.as_raw_fd(),
            std::ptr::null(),
            0,
            events.as_mut_ptr(),
            events.len() as libc::c_int,
            &limit,
        )
    };
    if ready < 0 {
        let error = io::Error::last_os_error();
        if error.kind() == io::ErrorKind::Interrupted {
            return Ok(Woke {
                output: false,
                exited: false,
            });
        }
        return Err(error);
    }
    let mut woke = Woke {
        output: false,
        exited: false,
    };
    for event in &events[..ready as usize] {
        if event.flags & libc::EV_ERROR != 0 {
            return Err(io::Error::from_raw_os_error(event.data as i32));
        }
        match event.filter {
            // Includes `EV_EOF`: the read that follows reports the hangup.
            libc::EVFILT_READ => woke.output = true,
            libc::EVFILT_PROC if event.fflags & libc::NOTE_EXIT != 0 => woke.exited = true,
            _ => {}
        }
    }
    Ok(woke)
}

#[cfg(target_os = "linux")]
fn open(master: RawFd, child: Option<u32>) -> io::Result<(Backend, bool)> {
    use std::os::fd::{AsRawFd, FromRawFd};
    // SAFETY: epoll_create1 takes flags only; the result is checked below.
    let ep = unsafe { libc::epoll_create1(libc::EPOLL_CLOEXEC) };
    if ep < 0 {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: ep is a fresh descriptor this call owns.
    let ep = unsafe { OwnedFd::from_raw_fd(ep) };
    epoll_add(ep.as_raw_fd(), master, MASTER)?;
    let Some(pid) = child else {
        return Ok((Backend::Poll, false));
    };
    // SAFETY: pidfd_open(pid, 0) takes no pointers; the result is checked.
    let pidfd = unsafe { libc::syscall(libc::SYS_pidfd_open, pid as libc::pid_t, 0) };
    if pidfd < 0 {
        let error = io::Error::last_os_error();
        // Already gone and reaped: the first `try_wait` says so, so the lap
        // that follows is never reached. Any other refusal (a kernel before
        // 5.3, a seccomp filter) is worth a line.
        if error.raw_os_error() != Some(libc::ESRCH) {
            log::warn!(
                target: "terminal",
                "could not watch a terminal child's exit; the reader will poll: {error}"
            );
        }
        return Ok((Backend::Poll, false));
    }
    // SAFETY: pidfd_open returned a descriptor this call owns (close-on-exec).
    let pidfd = unsafe { OwnedFd::from_raw_fd(pidfd as RawFd) };
    epoll_add(ep.as_raw_fd(), pidfd.as_raw_fd(), CHILD)?;
    Ok((Backend::Epoll { ep, _pidfd: pidfd }, true))
}

#[cfg(target_os = "linux")]
const MASTER: u64 = 0;
#[cfg(target_os = "linux")]
const CHILD: u64 = 1;

#[cfg(target_os = "linux")]
fn epoll_add(ep: RawFd, fd: RawFd, tag: u64) -> io::Result<()> {
    let mut event = libc::epoll_event {
        events: libc::EPOLLIN as u32,
        u64: tag,
    };
    // SAFETY: one initialized event; ep and fd are open descriptors.
    if unsafe { libc::epoll_ctl(ep, libc::EPOLL_CTL_ADD, fd, &mut event) } < 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(())
}

#[cfg(target_os = "linux")]
fn epoll_wait(ep: &OwnedFd, timeout: Duration) -> io::Result<Woke> {
    use std::os::fd::AsRawFd;
    let millis = timeout.as_nanos().div_ceil(1_000_000).min(i32::MAX as u128) as libc::c_int;
    let mut events = [libc::epoll_event { events: 0, u64: 0 }; 2];
    // SAFETY: the event list is two initialized records.
    let ready = unsafe {
        libc::epoll_wait(
            ep.as_raw_fd(),
            events.as_mut_ptr(),
            events.len() as libc::c_int,
            millis,
        )
    };
    if ready < 0 {
        let error = io::Error::last_os_error();
        if error.kind() == io::ErrorKind::Interrupted {
            return Ok(Woke {
                output: false,
                exited: false,
            });
        }
        return Err(error);
    }
    let mut woke = Woke {
        output: false,
        exited: false,
    };
    for event in &events[..ready as usize] {
        let tag = event.u64;
        if tag == MASTER {
            woke.output = true;
        } else if tag == CHILD {
            woke.exited = true;
        }
    }
    Ok(woke)
}

#[cfg(not(any(target_os = "macos", target_os = "linux")))]
fn open(_master: RawFd, _child: Option<u32>) -> io::Result<(Backend, bool)> {
    Ok((Backend::Poll, false))
}

#[cfg(all(test, any(target_os = "macos", target_os = "linux")))]
mod tests {
    use super::*;
    use portable_pty::{native_pty_system, CommandBuilder, PtySize};
    use std::time::Instant;

    fn pty() -> portable_pty::PtyPair {
        native_pty_system()
            .openpty(PtySize {
                rows: 24,
                cols: 80,
                pixel_width: 0,
                pixel_height: 0,
            })
            .expect("openpty")
    }

    fn shell(pair: &portable_pty::PtyPair, script: &str) -> Box<dyn portable_pty::Child + Send + Sync> {
        let mut command = CommandBuilder::new("/bin/sh");
        command.args(["-c", script]);
        pair.slave.spawn_command(command).expect("spawn")
    }

    fn nonblocking(fd: RawFd) {
        // SAFETY: fcntl on a descriptor the test owns.
        unsafe {
            let flags = libc::fcntl(fd, libc::F_GETFL);
            assert!(libc::fcntl(fd, libc::F_SETFL, flags | libc::O_NONBLOCK) >= 0);
        }
    }

    /// The facility has to work on a PTY master, which is not a given on macOS.
    #[test]
    fn a_wait_ends_when_the_master_has_output_without_waiting_out_the_net() {
        let pair = pty();
        let fd = pair.master.as_raw_fd().unwrap();
        nonblocking(fd);
        let mut child = shell(&pair, "sleep 0.3; echo hi; exec sleep 5");
        let mut ready = Readiness::new(fd, child.process_id());
        assert!(ready.watching_exit());
        assert!(ready.take_exit_hint(), "a fresh watch asks once");
        let started = Instant::now();
        ready.wait().unwrap();
        assert!(
            started.elapsed() < Duration::from_secs(3),
            "the wait outlived the output: {:?}",
            started.elapsed()
        );
        assert!(
            !ready.take_exit_hint(),
            "output alone is not a reason to ask whether the child exited"
        );
        let _ = child.kill();
    }

    #[test]
    fn a_wait_ends_when_the_child_exits_with_no_output() {
        let pair = pty();
        let fd = pair.master.as_raw_fd().unwrap();
        nonblocking(fd);
        let mut child = shell(&pair, "sleep 0.3");
        let mut ready = Readiness::new(fd, child.process_id());
        ready.take_exit_hint();
        let started = Instant::now();
        // macOS revokes the tty when the session leader exits, so the master
        // may report the hangup before the exit; either way the sleep ends
        // with the child's death, and the ask that follows finds it.
        let mut asked = false;
        while !asked && started.elapsed() < Duration::from_secs(3) {
            ready.wait().unwrap();
            asked = ready.take_exit_hint();
        }
        assert!(asked, "the exit never ended the sleep");
        assert!(child.try_wait().unwrap().is_some());
    }

    /// The registration races the child's exit: one that is already over, and
    /// even already reaped, must still end in "ask", never in a sleep.
    #[test]
    fn a_child_that_exited_before_the_watch_is_found_by_the_first_ask() {
        let pair = pty();
        let fd = pair.master.as_raw_fd().unwrap();
        nonblocking(fd);
        let mut child = shell(&pair, "exit 0");
        let pid = child.process_id();
        child.wait().unwrap();
        let mut ready = Readiness::new(fd, pid);
        assert!(ready.take_exit_hint());
    }

    /// The notification this module exists to avoid polling for can be lost.
    /// A deaf watch believes it is listening and is told nothing: the safety
    /// net, not the exit, is what ends the sleep, and it ends it in a question
    /// whose answer is "gone".
    #[cfg(target_os = "macos")]
    #[test]
    fn a_lost_exit_notification_is_caught_by_the_safety_net() {
        let pair = pty();
        let fd = pair.master.as_raw_fd().unwrap();
        nonblocking(fd);
        // Not on the PTY, whose master would report the hangup that macOS
        // raises when a session leader exits, and so end the sleep itself.
        let mut child = std::process::Command::new("/bin/sh")
            .args(["-c", "sleep 0.2"])
            .spawn()
            .unwrap();
        let mut ready = Readiness::with_options(
            fd,
            Some(child.id()),
            Options {
                safety_net: Duration::from_millis(700),
                watch_exit: true,
                deaf: true,
            },
        );
        assert!(ready.watching_exit(), "positive control: it trusts its watch");
        ready.take_exit_hint();
        let started = Instant::now();
        ready.wait().unwrap();
        assert!(
            started.elapsed() >= Duration::from_millis(600),
            "the wait ended on something other than the net: {:?}",
            started.elapsed()
        );
        assert!(ready.take_exit_hint());
        assert!(child.try_wait().unwrap().is_some(), "the ask finds the exit");
    }

    /// Without a watch there is no net to trust: the lap finds the exit.
    #[test]
    fn an_unwatched_exit_is_found_by_the_lap() {
        let pair = pty();
        let fd = pair.master.as_raw_fd().unwrap();
        nonblocking(fd);
        let mut child = shell(&pair, "sleep 0.2");
        let mut ready = Readiness::with_options(
            fd,
            child.process_id(),
            Options {
                watch_exit: false,
                ..Options::default()
            },
        );
        assert!(!ready.watching_exit());
        let deadline = Instant::now() + Duration::from_secs(5);
        let mut exited = false;
        while Instant::now() < deadline && !exited {
            ready.take_exit_hint();
            ready.wait().unwrap();
            exited = ready.take_exit_hint() && child.try_wait().unwrap().is_some();
        }
        assert!(exited, "an exit nobody was told about was never found");
    }

    /// With the exit watched, a quiet session has no timer shorter than the net.
    #[test]
    fn a_watched_quiet_session_sleeps_for_the_whole_net() {
        let pair = pty();
        let fd = pair.master.as_raw_fd().unwrap();
        nonblocking(fd);
        let mut child = shell(&pair, "exec sleep 5");
        let mut ready = Readiness::with_options(
            fd,
            child.process_id(),
            Options {
                safety_net: Duration::from_millis(700),
                ..Options::default()
            },
        );
        assert!(ready.watching_exit());
        ready.take_exit_hint();
        let started = Instant::now();
        ready.wait().unwrap();
        let slept = started.elapsed();
        assert!(
            slept >= Duration::from_millis(600),
            "a quiet watched session woke after {slept:?}, well before its net"
        );
        assert!(ready.take_exit_hint(), "the net ends in a question");
        let _ = child.kill();
    }
}
