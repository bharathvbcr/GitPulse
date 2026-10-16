//! The unix sockets GitPulse listens on, and the one way to bind them.
//!
//! Two endpoints live in the profile directory: the agent hook bridge
//! (`alerts::bridge`) and the single-instance mailbox (`desktop::instance`).
//! Both accept work from any process that can reach them, so both need the
//! same defences, and they are kept here so a hardening of one is a hardening
//! of both:
//!
//! * the path must fit `sun_path`, and saying so beats a bare `ENAMETOOLONG`;
//! * the directory is `0700`, and refused if any component is a symlink;
//! * a stale socket from a killed process is replaced, but a regular file
//!   someone else put at the path is never deleted;
//! * the socket is `0600` inside that `0700` directory, so no other user can
//!   reach it even in the instant between `bind` and `chmod` — and the
//!   process-wide umask is never touched, which would hand every directory
//!   another thread created meanwhile a mode nobody can create files in;
//! * a peer's uid is read on every connection, and a platform that cannot
//!   report it is not bound at all — a check that cannot run must not be
//!   reported as a check that passed.

use std::os::fd::OwnedFd;
use std::os::unix::fs::PermissionsExt;
use std::os::unix::io::{AsRawFd, RawFd};
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::Path;
use std::time::Duration;

/// The longest socket path this platform can bind.
///
/// `sun_path` is a fixed array — 104 bytes on macOS, 108 on Linux — and
/// `bind` reports a path one byte too long as `ENAMETOOLONG`, which reads as a
/// filesystem problem rather than as "your home directory's name is long".
pub fn max_path_bytes() -> usize {
    // SAFETY: reading the length of a field of a zeroed `sockaddr_un` is a
    // compile-time constant expressed through the libc type; nothing is
    // dereferenced.
    let probe = unsafe { std::mem::zeroed::<libc::sockaddr_un>() };
    probe.sun_path.len().saturating_sub(1)
}

/// Whether [`peer_uid`] can answer on this platform at all.
pub fn peer_identity_is_checkable() -> bool {
    cfg!(any(
        target_os = "macos",
        target_os = "ios",
        target_os = "freebsd",
        target_os = "openbsd",
        target_os = "netbsd",
        target_os = "dragonfly",
        target_os = "linux",
        target_os = "android"
    ))
}

/// Binds a private, non-blocking listener at `path`. `what` names the
/// endpoint in errors ("notification socket").
pub fn bind_private(path: &Path, what: &str) -> Result<UnixListener, String> {
    if !peer_identity_is_checkable() {
        return Err(format!(
            "This platform cannot report a socket peer's user, so the {what} stays closed."
        ));
    }
    let limit = max_path_bytes();
    if path.as_os_str().len() > limit {
        return Err(format!(
            "The {what} path is {} bytes and this system allows {limit}: {}",
            path.as_os_str().len(),
            path.display()
        ));
    }
    let dir = path
        .parent()
        .ok_or_else(|| format!("The {what} path has no directory: {}", path.display()))?
        .to_path_buf();
    for ancestor in dir.ancestors() {
        if ancestor.try_exists().map_err(|e| e.to_string())? {
            crate::storage::hygiene::tree::no_symlinks(ancestor)?;
            break;
        }
    }
    std::fs::create_dir_all(&dir).map_err(|e| format!("{}: {e}", dir.display()))?;
    crate::storage::hygiene::tree::no_symlinks(&dir)?;
    std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o700))
        .map_err(|e| format!("{}: {e}", dir.display()))?;

    // A stale socket from a killed process would make `bind` fail with
    // EADDRINUSE forever. Remove only a socket, never a regular file someone
    // else put at this path.
    match std::fs::symlink_metadata(path) {
        Ok(meta) => {
            use std::os::unix::fs::FileTypeExt;
            if !meta.file_type().is_socket() {
                return Err(format!(
                    "{} exists and is not a socket; the {what} stays closed.",
                    path.display()
                ));
            }
            std::fs::remove_file(path).map_err(|e| format!("{}: {e}", path.display()))?;
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
        Err(e) => return Err(format!("{}: {e}", path.display())),
    }

    // The directory above is `0700` before the socket exists, so no other
    // user can reach the socket in the instant before it is made `0600`.
    // Narrowing the umask instead would be process-wide: a directory any other
    // thread created in that instant would come out `0600` — no search bit,
    // nothing creatable inside it. The descriptor is made close-on-exec only
    // after `socket()` returns, so it is created under the inheritance lock a
    // concurrent spawn would otherwise copy it through.
    let listener = crate::procguard::with_inheritance_lock(|| UnixListener::bind(path))
        .map_err(|e| format!("{}: {e}", path.display()))?;
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))
        .map_err(|e| format!("{}: {e}", path.display()))?;
    listener
        .set_nonblocking(true)
        .map_err(|e| format!("{}: {e}", path.display()))?;
    Ok(listener)
}

/// The uid on the other end of a connected socket, or `None` when it cannot be
/// established. `None` is always a refusal.
pub fn peer_uid(stream: &UnixStream) -> Option<u32> {
    let fd = stream.as_raw_fd();
    #[cfg(any(
        target_os = "macos",
        target_os = "ios",
        target_os = "freebsd",
        target_os = "openbsd",
        target_os = "netbsd",
        target_os = "dragonfly"
    ))]
    {
        let mut uid: libc::uid_t = 0;
        let mut gid: libc::gid_t = 0;
        // SAFETY: `fd` is a live connected socket owned by `stream`, and both
        // out-pointers address initialised locals for the call's duration.
        let ok = unsafe { libc::getpeereid(fd, &mut uid, &mut gid) } == 0;
        return ok.then_some(uid);
    }
    #[cfg(any(target_os = "linux", target_os = "android"))]
    {
        let mut cred = libc::ucred {
            pid: 0,
            uid: 0,
            gid: 0,
        };
        let mut len = std::mem::size_of::<libc::ucred>() as libc::socklen_t;
        // SAFETY: `fd` is a live connected socket; the buffer and its length
        // describe one `ucred` and are valid for the call.
        let ok = unsafe {
            libc::getsockopt(
                fd,
                libc::SOL_SOCKET,
                libc::SO_PEERCRED,
                std::ptr::addr_of_mut!(cred).cast(),
                &mut len,
            )
        } == 0;
        return (ok && len as usize == std::mem::size_of::<libc::ucred>()).then_some(cred.uid);
    }
    #[allow(unreachable_code)]
    {
        let _ = fd;
        None
    }
}

/// Whether the other end has closed without ever sending a byte.
///
/// A liveness probe connects and closes. Whatever the owner then attempts on
/// that connection fails with `EPIPE`, a reset, or on macOS an `EINVAL` from
/// `setsockopt` on the departed socket — none of which says anything about the
/// peer except that it left. What does is this: the socket reads end-of-file
/// (or a reset) with nothing queued ahead of it.
///
/// Peeks without consuming and never blocks, whatever the descriptor's own
/// mode, so a silent peer that is still connected answers `false` at once.
/// Anything it cannot establish is `false`: callers treat that as a real
/// failure, never as a probe.
pub fn peer_hung_up_silently(stream: &UnixStream) -> bool {
    let mut byte = 0u8;
    // SAFETY: `fd` is a live socket owned by `stream`, and the buffer is one
    // initialised byte that outlives the call. `MSG_PEEK` leaves the queue as
    // it was; `MSG_DONTWAIT` makes this call non-blocking on its own.
    let read = unsafe {
        libc::recv(
            stream.as_raw_fd(),
            std::ptr::addr_of_mut!(byte).cast(),
            1,
            libc::MSG_PEEK | libc::MSG_DONTWAIT,
        )
    };
    match read {
        0 => true,
        n if n > 0 => false,
        _ => matches!(
            std::io::Error::last_os_error().raw_os_error(),
            Some(libc::ECONNRESET | libc::ECONNABORTED | libc::EPIPE | libc::ENOTCONN)
        ),
    }
}

/// Waits for a listener to have a connection ready, or for the timeout.
///
/// `poll` rather than a sleep between non-blocking `accept` calls: the
/// difference is the latency of every connection.
pub fn wait_readable(fd: RawFd, timeout: Duration) -> bool {
    let mut descriptor = libc::pollfd {
        fd,
        events: libc::POLLIN,
        revents: 0,
    };
    let millis = i32::try_from(timeout.as_millis()).unwrap_or(i32::MAX);
    // SAFETY: one initialised `pollfd` describing a descriptor this thread
    // owns for the duration of the call.
    unsafe { libc::poll(&mut descriptor, 1, millis) > 0 }
}

/// What [`wait_readable_or_woken`] found ready.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct Ready {
    /// The listener has a connection to accept.
    pub listener: bool,
    /// The wake descriptor has bytes: someone asked the waiter to look again.
    pub woken: bool,
}

/// Blocks, with no timeout, until `fd` is readable or `wake` is.
///
/// For a thread whose only reasons to run are a connection and being told to
/// stop. `wake` is the read end of a pipe the stopper writes to; a timeout
/// would only ever find the stop late. An interrupted wait returns with
/// nothing ready, and the caller's loop asks again.
pub fn wait_readable_or_woken(fd: RawFd, wake: RawFd) -> Ready {
    let mut descriptors = [
        libc::pollfd {
            fd,
            events: libc::POLLIN,
            revents: 0,
        },
        libc::pollfd {
            fd: wake,
            events: libc::POLLIN,
            revents: 0,
        },
    ];
    // SAFETY: two initialised `pollfd`s describing descriptors this thread
    // holds open for the duration of the call; -1 waits indefinitely.
    let ready = unsafe { libc::poll(descriptors.as_mut_ptr(), 2, -1) };
    if ready <= 0 {
        return Ready::default();
    }
    // POLLHUP and POLLERR count: a listener that errored must be looked at by
    // `accept`, and a closed wake pipe means nobody can ever wake us. So does
    // POLLNVAL, a descriptor closed under the waiter: reporting nothing would
    // return at once, every time, and turn the caller's loop into a spin;
    // `accept` on it fails and the loop ends.
    let any = libc::POLLIN | libc::POLLHUP | libc::POLLERR | libc::POLLNVAL;
    Ready {
        listener: descriptors[0].revents & any != 0,
        woken: descriptors[1].revents & any != 0,
    }
}

/// The two ends of a wake pipe: how a thread blocked in
/// [`wait_readable_or_woken`] is told to look again.
///
/// One owner for every accept loop that waits with no timeout. The waiter polls
/// [`Self::reader_fd`]; whoever retires it calls [`Self::notify`].
pub(crate) struct Wake {
    reader: OwnedFd,
    writer: OwnedFd,
}

impl Wake {
    /// Fails if the pipe cannot be made. Callers must not fall back to polling
    /// on a timeout, which is the cost this type exists to remove.
    pub(crate) fn new() -> std::io::Result<Self> {
        let (reader, writer) = crate::procguard::wake_pipe()?;
        Ok(Self { reader, writer })
    }

    /// The descriptor to hand to [`wait_readable_or_woken`].
    pub(crate) fn reader_fd(&self) -> RawFd {
        self.reader.as_raw_fd()
    }

    /// Makes the reader readable. Never blocks: the pipe is non-blocking, and a
    /// full pipe already is readable.
    pub(crate) fn notify(&self) {
        let byte = 1u8;
        // SAFETY: one byte from a live local to a descriptor `self` keeps
        // open; the result is deliberately ignored (see above).
        unsafe { libc::write(self.writer.as_raw_fd(), std::ptr::addr_of!(byte).cast(), 1) };
    }

    /// Empties the pipe so the next wait blocks again.
    pub(crate) fn drain(&self) {
        let mut buffer = [0u8; 64];
        loop {
            // SAFETY: reads into a live local buffer of the length given, from
            // a non-blocking descriptor `self` keeps open.
            let read = unsafe {
                libc::read(
                    self.reader.as_raw_fd(),
                    buffer.as_mut_ptr().cast(),
                    buffer.len(),
                )
            };
            if read < buffer.len() as isize {
                return;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_wait_ends_when_either_descriptor_is_readable_and_says_which() {
        use std::io::Write;
        let (wake_reader, mut wake_writer) = UnixStream::pair().unwrap();
        let (listener_side, mut peer) = UnixStream::pair().unwrap();
        let fds = (listener_side.as_raw_fd(), wake_reader.as_raw_fd());
        wake_writer.write_all(b"w").unwrap();
        assert_eq!(
            wait_readable_or_woken(fds.0, fds.1),
            Ready {
                listener: false,
                woken: true
            }
        );
        peer.write_all(b"x").unwrap();
        assert_eq!(
            wait_readable_or_woken(fds.0, fds.1),
            Ready {
                listener: true,
                woken: true
            }
        );
    }

    #[test]
    fn a_wait_with_nothing_ready_blocks_until_it_is_woken() {
        use std::io::Write;
        let (wake_reader, mut wake_writer) = UnixStream::pair().unwrap();
        let (listener_side, _peer) = UnixStream::pair().unwrap();
        let fds = (listener_side.as_raw_fd(), wake_reader.as_raw_fd());
        let waiter = std::thread::spawn(move || {
            let _keep = (&listener_side, &wake_reader);
            let started = std::time::Instant::now();
            (wait_readable_or_woken(fds.0, fds.1), started.elapsed())
        });
        std::thread::sleep(Duration::from_millis(300));
        wake_writer.write_all(b"w").unwrap();
        let (ready, waited) = waiter.join().unwrap();
        assert!(ready.woken && !ready.listener, "{ready:?}");
        assert!(
            waited >= Duration::from_millis(250),
            "returned after {waited:?} with nothing ready"
        );
    }

    #[test]
    fn a_peer_is_hung_up_silently_only_when_it_left_without_sending() {
        use std::io::{Read, Write};
        // Connected and quiet: not hung up, and the call must not wait for it.
        let (server, mut client) = UnixStream::pair().unwrap();
        assert!(!peer_hung_up_silently(&server), "a live, silent peer");
        // Sent something and left: the bytes are what matters, and are kept.
        client.write_all(b"x").unwrap();
        drop(client);
        assert!(!peer_hung_up_silently(&server), "a peer that sent a byte");
        let mut server = server;
        let mut byte = [0u8; 1];
        server.read_exact(&mut byte).unwrap();
        assert_eq!(&byte, b"x", "the peek consumed the byte");
        // Now drained and gone: end of file with nothing ahead of it.
        assert!(peer_hung_up_silently(&server), "a peer that left");
        // Gone before it ever wrote.
        let (server, client) = UnixStream::pair().unwrap();
        drop(client);
        assert!(peer_hung_up_silently(&server), "a peer that never wrote");
        // A blocking descriptor is not waited on either.
        let (server, _client) = UnixStream::pair().unwrap();
        server.set_nonblocking(false).unwrap();
        assert!(!peer_hung_up_silently(&server));
    }

    /// Binding must not change how anything else in the process creates
    /// files. Directories made on other threads while sockets are bound keep
    /// the process umask's mode — a narrowed umask turned them into `0600`
    /// directories that nothing could be created in.
    #[test]
    fn binding_leaves_other_threads_file_modes_alone() {
        let root = tempfile::tempdir().unwrap().keep().canonicalize().unwrap();
        let made = root.join("made");
        std::fs::create_dir(&made).unwrap();
        let expected = std::fs::metadata(&made).unwrap().permissions().mode() & 0o777;
        let stop = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let maker = {
            let (stop, made) = (stop.clone(), made.clone());
            std::thread::spawn(move || {
                let mut wrong = Vec::new();
                let mut n = 0;
                while !stop.load(std::sync::atomic::Ordering::Relaxed) {
                    let dir = made.join(n.to_string());
                    std::fs::create_dir(&dir).unwrap();
                    let mode = std::fs::metadata(&dir).unwrap().permissions().mode() & 0o777;
                    if mode != expected {
                        wrong.push(mode);
                    }
                    n += 1;
                }
                (n, wrong)
            })
        };
        let sockets = root.join("s");
        for n in 0..300 {
            let path = sockets.join(format!("{n}.sock"));
            drop(bind_private(&path, "test socket").unwrap());
            let mode = std::fs::metadata(&path).unwrap().permissions().mode() & 0o777;
            assert_eq!(mode, 0o600, "socket {n} is not private");
        }
        stop.store(true, std::sync::atomic::Ordering::Relaxed);
        let (made_count, wrong) = maker.join().unwrap();
        let _ = std::fs::remove_dir_all(&root);
        assert!(
            made_count > 0,
            "fixture: no directories were made alongside the binds"
        );
        assert!(
            wrong.is_empty(),
            "{} of {made_count} directories got another mode: {:?}",
            wrong.len(),
            &wrong[..wrong.len().min(5)]
        );
        assert_eq!(
            std::fs::metadata(&sockets)
                .map(|m| m.permissions().mode() & 0o777)
                .ok(),
            None,
            "fixture cleaned up"
        );
    }

    #[test]
    fn notifying_a_full_pipe_never_blocks_and_one_drain_leaves_it_quiet() {
        let wake = Wake::new().unwrap();
        let started = std::time::Instant::now();
        for _ in 0..100_000 {
            wake.notify();
        }
        assert!(
            started.elapsed() < Duration::from_secs(5),
            "100k notifies took {:?}",
            started.elapsed()
        );
        let (listener_side, _peer) = UnixStream::pair().unwrap();
        let ready = wait_readable_or_woken(listener_side.as_raw_fd(), wake.reader_fd());
        assert!(ready.woken && !ready.listener, "{ready:?}");
        wake.drain();
        // Drained to empty: a second wait must block, so give it a waker.
        let reader = wake.reader_fd();
        let fd = listener_side.as_raw_fd();
        let waiter = std::thread::spawn(move || {
            let started = std::time::Instant::now();
            (wait_readable_or_woken(fd, reader), started.elapsed())
        });
        std::thread::sleep(Duration::from_millis(200));
        wake.notify();
        let (ready, waited) = waiter.join().unwrap();
        assert!(ready.woken, "{ready:?}");
        assert!(
            waited >= Duration::from_millis(150),
            "a drained pipe still read as readable ({waited:?})"
        );
    }

    /// Dropping the writer makes the reader permanently POLLHUP, so a loop that
    /// kept polling the reader would spin. This pins what the wait reports, and
    /// why `Wake` must keep both ends alive for as long as anyone polls it.
    #[test]
    fn a_writer_dropped_under_a_poller_reads_as_woken_forever() {
        let Wake { reader, writer } = Wake::new().unwrap();
        drop(writer);
        let (listener_side, _peer) = UnixStream::pair().unwrap();
        for _ in 0..3 {
            let started = std::time::Instant::now();
            let ready = wait_readable_or_woken(listener_side.as_raw_fd(), reader.as_raw_fd());
            assert!(ready.woken, "{ready:?}");
            assert!(started.elapsed() < Duration::from_millis(100));
        }
    }

    /// POLLNVAL (a descriptor closed under the waiter) must be reported as
    /// something to look at; an empty answer makes every caller's loop spin.
    #[test]
    fn a_closed_listener_descriptor_is_reported_not_swallowed() {
        let wake = Wake::new().unwrap();
        // A descriptor number no thread has open: a number closed a moment ago
        // can be handed to another test's socket, and the wait would block on it.
        let fd: RawFd = 900_000;
        let ready = wait_readable_or_woken(fd, wake.reader_fd());
        assert!(
            ready.listener,
            "POLLNVAL on the listener was swallowed: {ready:?}"
        );
    }
}
