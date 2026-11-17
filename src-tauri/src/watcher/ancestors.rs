//! A kqueue on the split ancestors of a narrowed registration (macOS).
//!
//! A narrowed registration registers the entries of the worktree root (and of
//! every other split ancestor, see [`super::scope`]) one by one, so a NEW entry
//! there lies under no registered path and the stream says nothing. This
//! thread watches the ancestor directories themselves with
//! `EVFILT_VNODE` (`NOTE_WRITE | NOTE_DELETE | NOTE_RENAME | NOTE_REVOKE`) and,
//! when one fires, asks the registration to compare the entry names with the
//! ones it registered. Same names: nothing (an editor's temporary file came
//! and went, and the rename over the real file is delivered by that file's own
//! path registration). Different names, or an ancestor deleted or renamed: the
//! registration plans again.
//!
//! One thread per watch. It holds a `Weak` to the registration, so it never
//! keeps a watch alive, and ends when the registration is gone, when it is
//! told to (an `EVFILT_USER` event), or when the kqueue fails. Every file
//! descriptor it opens is closed when it stops looking at that directory.

use super::debouncer::Registration;
use std::ffi::CString;
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd, RawFd};
use std::os::unix::ffi::OsStrExt;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, PoisonError, Weak};
use std::time::{Duration, Instant};

/// How long a burst of vnode events is allowed to settle before the entry
/// names are compared: an atomic save's temporary file is gone by then.
const SETTLE_QUIET: Duration = Duration::from_millis(60);
const SETTLE_MAX: Duration = Duration::from_millis(300);
/// `EVFILT_USER` ident the stop and reconfigure signals use.
const WAKE_IDENT: libc::uintptr_t = 0;
const EVENT_BATCH: usize = 16;

struct Shared {
    kq: OwnedFd,
    stop: AtomicBool,
    /// (generation, directories). The thread reopens its descriptors when the
    /// generation moves.
    dirs: Mutex<(u64, Vec<PathBuf>)>,
}

impl Shared {
    fn kevent(&self, change: &libc::kevent) -> bool {
        // SAFETY: `kq` is an open kqueue for as long as `self` lives; one
        // change is read from a valid pointer and no event list is written.
        unsafe {
            libc::kevent(
                self.kq.as_raw_fd(),
                change,
                1,
                std::ptr::null_mut(),
                0,
                std::ptr::null(),
            ) >= 0
        }
    }

    fn wake(&self) {
        let trigger = libc::kevent {
            ident: WAKE_IDENT,
            filter: libc::EVFILT_USER,
            flags: 0,
            fflags: libc::NOTE_TRIGGER,
            data: 0,
            udata: std::ptr::null_mut(),
        };
        // A failed trigger leaves the thread asleep until its next event; the
        // stop flag is still set, and the registration it holds is a `Weak`.
        let _ = self.kevent(&trigger);
    }
}

/// The handle a registration keeps; dropping it stops the thread.
pub(crate) struct AncestorWatch {
    shared: Arc<Shared>,
    thread: Option<std::thread::JoinHandle<()>>,
}

impl AncestorWatch {
    /// Opens a kqueue and starts watching `dirs`. Fails (so the caller registers
    /// the whole worktree) when the kqueue or the thread cannot be made.
    pub(crate) fn start(
        registration: Weak<Registration>,
        dirs: Vec<PathBuf>,
    ) -> Result<Self, String> {
        // SAFETY: `kqueue` takes no arguments and returns a new descriptor or -1.
        let raw = unsafe { libc::kqueue() };
        if raw < 0 {
            return Err(format!(
                "could not open a kqueue: {}",
                std::io::Error::last_os_error()
            ));
        }
        // SAFETY: `raw` was just returned by `kqueue` and nothing else owns it.
        let kq = unsafe { OwnedFd::from_raw_fd(raw) };
        // Closed with the kqueue; not inherited by a spawned git.
        // SAFETY: `fcntl` on an owned open descriptor.
        unsafe { libc::fcntl(kq.as_raw_fd(), libc::F_SETFD, libc::FD_CLOEXEC) };
        let shared = Arc::new(Shared {
            kq,
            stop: AtomicBool::new(false),
            dirs: Mutex::new((1, dirs)),
        });
        let wake = libc::kevent {
            ident: WAKE_IDENT,
            filter: libc::EVFILT_USER,
            flags: libc::EV_ADD | libc::EV_CLEAR,
            fflags: 0,
            data: 0,
            udata: std::ptr::null_mut(),
        };
        if !shared.kevent(&wake) {
            return Err(format!(
                "could not register the kqueue wake event: {}",
                std::io::Error::last_os_error()
            ));
        }
        let thread_shared = Arc::clone(&shared);
        let thread =
            crate::supervisor::spawn_once("gitpulse-fs-ancestors", "watch ancestors", move || {
                run(thread_shared, registration)
            })
            .map_err(|e| format!("could not start the ancestor watch: {e}"))?;
        Ok(Self {
            shared,
            thread: Some(thread),
        })
    }

    /// The directories to watch now; the thread reopens its descriptors.
    pub(crate) fn set_dirs(&self, dirs: Vec<PathBuf>) {
        {
            let mut guard = self
                .shared
                .dirs
                .lock()
                .unwrap_or_else(PoisonError::into_inner);
            if guard.1 == dirs {
                return;
            }
            guard.0 += 1;
            guard.1 = dirs;
        }
        self.shared.wake();
    }

    /// Tells the thread to end. Does not wait for it.
    pub(crate) fn stop(&self) {
        self.shared.stop.store(true, Ordering::Release);
        self.shared.wake();
    }

    /// Waits up to `within` for the thread to end, and not at all when called
    /// from that thread. A thread still busy (it may be inside a git query) is
    /// left to finish on its own: it holds only a `Weak` and the stop flag is set.
    pub(crate) fn join(&mut self, within: Duration) {
        let Some(thread) = self.thread.take() else {
            return;
        };
        if thread.thread().id() == std::thread::current().id() {
            return;
        }
        let deadline = Instant::now() + within;
        while !thread.is_finished() {
            if Instant::now() >= deadline {
                log::debug!(target: "watcher", "the ancestor watch was still busy at shutdown");
                return;
            }
            std::thread::sleep(Duration::from_millis(5));
        }
        let _ = thread.join();
    }
}

impl Drop for AncestorWatch {
    fn drop(&mut self) {
        self.stop();
    }
}

/// One watched directory. The descriptor closes with it, which removes its
/// kevent.
struct Dir {
    _fd: OwnedFd,
}

/// Opens `dirs` and registers each with the kqueue. The second value is true
/// when one could not be opened (it is gone), which is itself a change.
fn open_all(shared: &Shared, dirs: &[PathBuf]) -> (Vec<Dir>, bool) {
    let mut open = Vec::with_capacity(dirs.len());
    let mut missing = false;
    for dir in dirs {
        let Ok(path) = CString::new(dir.as_os_str().as_bytes()) else {
            missing = true;
            continue;
        };
        // SAFETY: `path` is a NUL-terminated string that outlives the call.
        let raw: RawFd = unsafe { libc::open(path.as_ptr(), libc::O_EVTONLY | libc::O_CLOEXEC) };
        if raw < 0 {
            missing = true;
            continue;
        }
        // SAFETY: `raw` was just returned by `open` and nothing else owns it.
        let fd = unsafe { OwnedFd::from_raw_fd(raw) };
        let watch = libc::kevent {
            ident: raw as libc::uintptr_t,
            filter: libc::EVFILT_VNODE,
            flags: libc::EV_ADD | libc::EV_CLEAR,
            fflags: libc::NOTE_WRITE | libc::NOTE_DELETE | libc::NOTE_RENAME | libc::NOTE_REVOKE,
            data: 0,
            udata: std::ptr::null_mut(),
        };
        if shared.kevent(&watch) {
            open.push(Dir { _fd: fd });
        } else {
            missing = true;
        }
    }
    (open, missing)
}

/// Waits for events. `None` is a failure of the kqueue itself; `Some(n)` is the
/// number of events read (0 on timeout), with `vnode` set when any was a
/// directory event.
fn wait(shared: &Shared, timeout: Option<Duration>) -> Option<(usize, bool)> {
    let mut events: [libc::kevent; EVENT_BATCH] = [libc::kevent {
        ident: 0,
        filter: 0,
        flags: 0,
        fflags: 0,
        data: 0,
        udata: std::ptr::null_mut(),
    }; EVENT_BATCH];
    let spec = timeout.map(|timeout| libc::timespec {
        tv_sec: timeout.as_secs() as libc::time_t,
        tv_nsec: timeout.subsec_nanos() as libc::c_long,
    });
    let spec_ptr = spec
        .as_ref()
        .map_or(std::ptr::null(), |spec| spec as *const libc::timespec);
    // SAFETY: `events` has room for `EVENT_BATCH` entries, the kqueue is open,
    // and `spec_ptr` is null or points at a live `timespec`.
    let got = unsafe {
        libc::kevent(
            shared.kq.as_raw_fd(),
            std::ptr::null(),
            0,
            events.as_mut_ptr(),
            EVENT_BATCH as libc::c_int,
            spec_ptr,
        )
    };
    if got < 0 {
        return if std::io::Error::last_os_error().kind() == std::io::ErrorKind::Interrupted {
            Some((0, false))
        } else {
            None
        };
    }
    let got = got as usize;
    let vnode = events[..got]
        .iter()
        .any(|event| event.filter == libc::EVFILT_VNODE);
    Some((got, vnode))
}

fn run(shared: Arc<Shared>, registration: Weak<Registration>) {
    let mut generation = 0u64;
    let mut open: Vec<Dir> = Vec::new();
    // Names to compare once: after every (re)open, and when a directory is gone.
    let mut report = false;
    loop {
        if shared.stop.load(Ordering::Acquire) {
            return;
        }
        let wanted = {
            let guard = shared.dirs.lock().unwrap_or_else(PoisonError::into_inner);
            (guard.0 != generation).then(|| (guard.0, guard.1.clone()))
        };
        if let Some((next, dirs)) = wanted {
            open.clear();
            let (reopened, _missing) = open_all(&shared, &dirs);
            open = reopened;
            generation = next;
            // The layout was read before these descriptors existed: an entry
            // made in between fired no event. Compare the names once now (one
            // readdir per ancestor, no spawn when nothing changed); a directory
            // that could not be opened is the same comparison.
            report = true;
        }
        let pending = registration.upgrade().and_then(|r| r.pending_at());
        let timeout = if report {
            Some(Duration::ZERO)
        } else {
            pending.map(|at| at.saturating_duration_since(Instant::now()))
        };
        let Some((got, vnode)) = wait(&shared, timeout) else {
            log::warn!(target: "watcher", "the ancestor watch's kqueue failed; it stops");
            return;
        };
        if shared.stop.load(Ordering::Acquire) {
            return;
        }
        let mut changed = vnode || std::mem::take(&mut report);
        if vnode {
            // Let a burst finish: a temporary file that came and went is not
            // a change, and must not be seen half-way.
            let started = Instant::now();
            while started.elapsed() < SETTLE_MAX {
                match wait(&shared, Some(SETTLE_QUIET)) {
                    Some((0, _)) => break,
                    Some((_, more)) => changed |= more,
                    None => return,
                }
            }
        }
        let Some(registration) = registration.upgrade() else {
            return;
        };
        if changed {
            registration.ancestors_changed();
        }
        if got == 0 || changed {
            registration.run_pending();
        }
    }
}
