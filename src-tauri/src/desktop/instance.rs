//! One GitPulse per profile on macOS, whichever copy of the app was launched.
//!
//! LaunchServices does not guarantee one instance of a bundle identifier. It
//! guarantees one instance of a bundle *path*: a machine with
//! `/Applications/GitPulse.app`, a build under `target/release/bundle`, and a
//! mounted disk image has three apps that are each happy to start. A banner
//! click is routed to whichever copy LaunchServices prefers, which need not be
//! the copy that is running — and once the running copy's bundle has been
//! deleted by a clean build it never can be. Each such click started a whole
//! second GitPulse: another window, another set of watchers and another
//! code-intelligence build over the same repositories and the same profile.
//!
//! So ownership is decided by the profile, not by LaunchServices:
//!
//! - `flock` on [`LOCK_FILE`] in the profile directory picks the owner. The
//!   kernel releases it when the owner dies however it dies, and two launches
//!   racing for it cannot both win — unlike "is the socket there", which two
//!   launches can both answer "no" to.
//! - A unix socket beside it is the owner's mailbox. A launch that loses the
//!   lock hands the owner what it was started to do and exits without ever
//!   building a window.
//!
//! The owner holding the lock is not by itself proof it is answering, and a
//! lock file can be deleted from under its holder. So a launch that *wins* the
//! lock still asks the mailbox first, and defers to a live answer there; and a
//! launch that *loses* it waits, bounded, for the mailbox — the owner may be
//! starting (lock taken, socket not yet bound) or quitting (socket gone, lock
//! not yet released). Neither wait ends in a second instance: an owner that
//! never answers is reported as [`Claim::Blocked`] and the launch gives up.

use std::collections::VecDeque;
use std::fs::{File, OpenOptions};
use std::io::{BufRead, BufReader, ErrorKind, Read, Write};
use std::os::unix::fs::{MetadataExt, OpenOptionsExt};
use std::os::unix::io::AsRawFd;
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};

pub const LOCK_FILE: &str = "instance.lock";
pub const SOCKET_FILE: &str = "instance.sock";

/// Bumped on any incompatible change to the frames below. A launch that meets
/// an owner speaking another version still never starts a second instance.
const PROTOCOL: u32 = 1;
/// Every frame is one JSON line no longer than this: room for [`MAX_PATHS`]
/// paths of [`MAX_PATH_BYTES`] even when every byte is a control character
/// JSON spells as six (`\u0001`). `forward` checks the encoded size anyway,
/// so a request this cannot carry fails at the sender.
const MAX_FRAME: usize = 128 * 1024;
const MAX_PATHS: usize = 16;
/// `PATH_MAX` on macOS.
const MAX_PATH_BYTES: usize = 1024;
/// The longest command line a hook may ask the app to judge. A longer one is
/// judged by the hook's own path, which has no frame to fit in.
const MAX_GATE_COMMAND_BYTES: usize = 48 * 1024;
/// Native notification identifiers are well under this; see
/// `alerts::native_session_key` and `workbench::notifications::owned`, which
/// validate them properly on the owner's side.
const MAX_NATIVE_ID: usize = 256;
/// Connections answered at once. A connection past this is closed unanswered
/// and its sender retries until its own deadline.
const MAX_HANDLERS: usize = 8;
/// Hook verdicts in flight at once, counted on top of [`MAX_HANDLERS`] so a
/// burst of agent hooks can never use up the handlers a relaunch needs. One
/// past this is answered "busy" and its hook judges the command itself.
const MAX_GATE_HANDLERS: usize = 16;
/// Requests held for an owner whose window is not up yet.
const MAX_QUEUED: usize = 32;
/// How long the owner gives one connection end to end. Longer than the
/// launch-intent capture on the sending side (`launch::capture`), which holds
/// its connection open while it learns what it was launched for.
pub const HANDLER_TIMEOUT: Duration = Duration::from_secs(4);
/// How long a sender waits for the owner's greeting on one attempt.
const HELLO_TIMEOUT: Duration = Duration::from_secs(2);
/// How long a launch that lost the lock keeps trying to reach the owner. Long
/// enough to outlast an owner's startup, a quit in progress, and a mailbox
/// saturated for one [`HANDLER_TIMEOUT`].
pub const CLAIM_DEADLINE: Duration = Duration::from_secs(8);
const RETRY: Duration = Duration::from_millis(50);
/// How often the accept loop wakes to notice [`Primary::shutdown`].
const ACCEPT_POLL: Duration = Duration::from_millis(500);

/// What a launch that found GitPulse already running asks of it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum Request {
    /// Open these repositories (Finder's "Open with", a dropped folder, a path
    /// on the command line), revealing the window on the way.
    Open { paths: Vec<String> },
    /// Come to the front: someone launched the app that is already running.
    Reveal,
    /// A banner was clicked, or its Snooze pressed, and macOS delivered the
    /// response to the launch rather than to the owner.
    Notification { id: String, snooze: bool },
    /// An agent hook asking for a verdict on a command it is about to let run.
    /// Answered on the connection, by whatever [`Primary::attach_gate`] put
    /// there, rather than handed to the window: see [`GateSink`].
    GateCommand {
        /// Echoed in the answer, so a reply that belongs to another request is
        /// refused by the hook rather than believed.
        id: String,
        cwd: String,
        command: String,
    },
}

impl Request {
    /// Bounds a frame from another process before anything acts on it. The
    /// sender is this user's own GitPulse, but a socket is a socket.
    pub fn validate(&self) -> Result<(), String> {
        match self {
            Request::Open { paths } => {
                if paths.is_empty() || paths.len() > MAX_PATHS {
                    return Err(format!(
                        "open names {} paths; 1..={MAX_PATHS} allowed",
                        paths.len()
                    ));
                }
                for path in paths {
                    if path.is_empty() || path.len() > MAX_PATH_BYTES {
                        return Err("open path is empty or too long".into());
                    }
                    if path.contains('\0') || !Path::new(path).is_absolute() {
                        return Err("open path must be absolute".into());
                    }
                }
                Ok(())
            }
            Request::Reveal => Ok(()),
            Request::Notification { id, .. } => {
                if id.is_empty() || id.len() > MAX_NATIVE_ID || id.chars().any(char::is_control) {
                    return Err("notification identity is empty, too long or not printable".into());
                }
                Ok(())
            }
            Request::GateCommand { id, cwd, command } => {
                if id.is_empty() || id.len() > MAX_NATIVE_ID || id.chars().any(char::is_control) {
                    return Err("gate request id is empty, too long or not printable".into());
                }
                if cwd.is_empty()
                    || cwd.len() > MAX_PATH_BYTES
                    || cwd.contains('\0')
                    || !Path::new(cwd).is_absolute()
                {
                    return Err("gate cwd must be an absolute path".into());
                }
                if command.is_empty() || command.len() > MAX_GATE_COMMAND_BYTES {
                    return Err("gate command is empty or too long".into());
                }
                Ok(())
            }
        }
    }
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Hello {
    v: u32,
    pid: u32,
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Envelope {
    v: u32,
    request: Request,
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Ack {
    accepted: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    reason: Option<String>,
    /// The request's `id` and the verdict, for the one request that has an
    /// answer beyond "accepted".
    #[serde(default, skip_serializing_if = "Option::is_none")]
    id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    reply: Option<serde_json::Value>,
}

impl Ack {
    fn accepted() -> Self {
        Self {
            accepted: true,
            reason: None,
            id: None,
            reply: None,
        }
    }

    fn refused(reason: String) -> Self {
        Self {
            accepted: false,
            reason: Some(reason),
            id: None,
            reply: None,
        }
    }
}

/// Where the lock and the mailbox live for a profile directory.
#[derive(Debug, Clone)]
pub struct Paths {
    pub lock: PathBuf,
    pub socket: PathBuf,
}

impl Paths {
    pub fn in_dir(dir: &Path) -> Self {
        Self {
            lock: dir.join(LOCK_FILE),
            socket: dir.join(SOCKET_FILE),
        }
    }

    /// A socket path the kernel would truncate cannot be bound or reached. The
    /// lock still decides ownership; only forwarding is lost.
    fn socket_fits(&self) -> bool {
        self.socket.as_os_str().len() <= crate::local_socket::max_path_bytes()
    }
}

/// The outcome of [`claim`].
pub enum Claim {
    /// This process owns the profile. Keep it alive for the process lifetime:
    /// dropping it releases the lock.
    Primary(Primary),
    /// Another GitPulse owns the profile and is answering. Hand it the
    /// request with [`Secondary::forward`] and exit.
    Secondary(Secondary),
    /// Another GitPulse holds the lock but never answered within the deadline.
    /// Starting anyway is exactly the harm this module exists to prevent, so
    /// the launch gives up; `pid` is the holder's, for the log and for
    /// bringing it forward.
    Blocked { pid: Option<u32>, reason: String },
    /// The guard could not run at all (the profile directory is unwritable,
    /// say). Reported distinctly from `Primary` so that "unchecked" is never
    /// logged as "checked and alone".
    Unavailable(String),
}

/// Decides whether this process is the profile's GitPulse.
///
/// Returns within `deadline` plus one handshake in every case.
pub fn claim(dir: &Path, deadline: Duration) -> Claim {
    if let Err(error) = std::fs::create_dir_all(dir) {
        return Claim::Unavailable(format!("cannot create {}: {error}", dir.display()));
    }
    // Every launch must name the same socket, and the socket is bound only on
    // a path with no symlink in it (`local_socket`). Resolving the directory
    // once gives both, even when `$HOME` itself passes through a symlink.
    let dir = match dir.canonicalize() {
        Ok(dir) => dir,
        Err(error) => {
            return Claim::Unavailable(format!("cannot resolve {}: {error}", dir.display()))
        }
    };
    let paths = Paths::in_dir(&dir);
    let started = Instant::now();
    loop {
        let file = match open_lock(&paths.lock) {
            Ok(file) => file,
            Err(error) => {
                return Claim::Unavailable(format!("cannot open {}: {error}", paths.lock.display()))
            }
        };
        match try_lock(&file) {
            Ok(true) => {
                // The path may have been replaced between our open and our
                // lock; a lock on an inode nobody else can find decides nothing.
                if !same_file(&file, &paths.lock) {
                    if started.elapsed() >= deadline {
                        return Claim::Unavailable(format!(
                            "{} kept being replaced while it was locked",
                            paths.lock.display()
                        ));
                    }
                    continue;
                }
                // Holding the lock is one signal. A live answer on the mailbox
                // is the other, and it wins: it means the owner's lock file was
                // deleted from under it and we locked a fresh one.
                if paths.socket_fits() {
                    match Secondary::connect(&paths.socket) {
                        Ok(secondary) => return Claim::Secondary(secondary),
                        Err(Reach::Incompatible { pid, reason }) => {
                            return Claim::Blocked {
                                pid: Some(pid),
                                reason,
                            }
                        }
                        Err(Reach::Unreachable(_)) => {}
                    }
                }
                return Claim::Primary(Primary::start(file, paths));
            }
            Ok(false) => {
                if !paths.socket_fits() {
                    return Claim::Blocked {
                        pid: read_pid(&paths.lock),
                        reason: format!(
                            "the mailbox path {} is too long to reach",
                            paths.socket.display()
                        ),
                    };
                }
                match Secondary::connect(&paths.socket) {
                    Ok(secondary) => return Claim::Secondary(secondary),
                    Err(Reach::Incompatible { pid, reason }) => {
                        return Claim::Blocked {
                            pid: Some(pid),
                            reason,
                        }
                    }
                    Err(Reach::Unreachable(reason)) => {
                        if started.elapsed() >= deadline {
                            return Claim::Blocked {
                                pid: read_pid(&paths.lock),
                                reason,
                            };
                        }
                    }
                }
            }
            Err(error) => {
                return Claim::Unavailable(format!("cannot lock {}: {error}", paths.lock.display()))
            }
        }
        std::thread::sleep(RETRY);
    }
}

fn open_lock(path: &Path) -> std::io::Result<File> {
    // `std` opens with O_CLOEXEC, so the lock never leaks into a git child
    // that could outlive this process and keep the profile "owned".
    OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .mode(0o600)
        .open(path)
}

/// `Ok(true)` locked, `Ok(false)` held elsewhere.
fn try_lock(file: &File) -> std::io::Result<bool> {
    loop {
        // SAFETY: a valid descriptor owned by `file` for the whole call.
        let rc = unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) };
        if rc == 0 {
            return Ok(true);
        }
        let error = std::io::Error::last_os_error();
        match error.kind() {
            ErrorKind::WouldBlock => return Ok(false),
            ErrorKind::Interrupted => continue,
            _ => return Err(error),
        }
    }
}

fn same_file(file: &File, path: &Path) -> bool {
    match (file.metadata(), std::fs::metadata(path)) {
        (Ok(held), Ok(named)) => held.dev() == named.dev() && held.ino() == named.ino(),
        _ => false,
    }
}

fn read_pid(lock: &Path) -> Option<u32> {
    let mut text = String::new();
    File::open(lock)
        .ok()?
        .take(32)
        .read_to_string(&mut text)
        .ok()?;
    text.trim().parse().ok()
}

/// Reads one newline-terminated frame of at most [`MAX_FRAME`] bytes.
fn read_frame(reader: &mut impl BufRead) -> Result<Option<String>, String> {
    let mut line = Vec::new();
    let read = reader
        .take(MAX_FRAME as u64 + 1)
        .read_until(b'\n', &mut line)
        .map_err(|error| format!("read: {error}"))?;
    if read == 0 {
        return Ok(None);
    }
    if line.last() != Some(&b'\n') {
        return Err(if line.len() > MAX_FRAME {
            "frame exceeds its size limit".into()
        } else {
            "frame ended without a newline".into()
        });
    }
    line.pop();
    String::from_utf8(line)
        .map(Some)
        .map_err(|_| "frame is not UTF-8".into())
}

fn write_frame(stream: &mut UnixStream, value: &impl Serialize) -> Result<(), String> {
    let mut bytes = serde_json::to_vec(value).map_err(|error| error.to_string())?;
    bytes.push(b'\n');
    stream
        .write_all(&bytes)
        .and_then(|()| stream.flush())
        .map_err(|error| format!("write: {error}"))
}

/// Only this user's processes may hand the owner work. The profile directory
/// is already private to the user; this holds if it is not. A peer whose uid
/// could not be read is refused.
pub fn peer_allowed(peer_uid: Option<u32>, own_uid: u32) -> bool {
    peer_uid == Some(own_uid)
}

/// Why [`Secondary::connect`] did not produce a usable connection.
enum Reach {
    /// Nobody answered: no owner yet, one starting, or one leaving.
    Unreachable(String),
    /// An owner answered, in a protocol this build cannot speak.
    Incompatible { pid: u32, reason: String },
}

/// A launch that found an owner answering, with the connection still open.
pub struct Secondary {
    stream: UnixStream,
    reader: BufReader<UnixStream>,
    pid: u32,
}

impl Secondary {
    fn connect(socket: &Path) -> Result<Self, Reach> {
        Self::connect_within(socket, HELLO_TIMEOUT)
    }

    /// [`Self::connect`] with a greeting deadline of the caller's choosing. A
    /// peer that is not this user is not an owner: the profile directory is
    /// private, so one that answers there is an impostor or a misconfiguration,
    /// and either way nothing may be handed to it.
    fn connect_within(socket: &Path, hello: Duration) -> Result<Self, Reach> {
        let unreachable = |error: String| Reach::Unreachable(error);
        let stream = UnixStream::connect(socket)
            .map_err(|error| unreachable(format!("connect: {error}")))?;
        // SAFETY: `getuid` reads this process's real user id and cannot fail.
        let own_uid = unsafe { libc::getuid() };
        let peer = crate::local_socket::peer_uid(&stream);
        if !peer_allowed(peer, own_uid) {
            return Err(unreachable(format!(
                "the owner is uid {peer:?}, not this user"
            )));
        }
        stream
            .set_read_timeout(Some(hello))
            .and_then(|()| stream.set_write_timeout(Some(hello)))
            .map_err(|error| unreachable(format!("timeouts: {error}")))?;
        let mut reader = BufReader::new(
            stream
                .try_clone()
                .map_err(|error| unreachable(error.to_string()))?,
        );
        let line = read_frame(&mut reader)
            .map_err(unreachable)?
            .ok_or_else(|| unreachable("the owner closed without a greeting".into()))?;
        let hello: Hello = serde_json::from_str(&line)
            .map_err(|error| unreachable(format!("greeting: {error}")))?;
        // An owner on another protocol version is still an owner: it answered.
        if hello.v != PROTOCOL {
            return Err(Reach::Incompatible {
                pid: hello.pid,
                reason: format!(
                    "owner {} speaks protocol {}, this launch {PROTOCOL}",
                    hello.pid, hello.v
                ),
            });
        }
        Ok(Self {
            stream,
            reader,
            pid: hello.pid,
        })
    }

    /// The owner's process id, from its greeting.
    pub fn owner_pid(&self) -> u32 {
        self.pid
    }

    /// Hands the owner `request`. The connection was opened by [`claim`] and is
    /// held while the caller works out what to send, which the owner allows
    /// for up to [`HANDLER_TIMEOUT`].
    pub fn forward(self, request: &Request) -> Result<(), String> {
        self.exchange(request, HANDLER_TIMEOUT).map(|_| ())
    }

    /// Asks the owner for a verdict on a command: the answer is the owner's
    /// reply to [`Request::GateCommand`], returned only when the owner echoed
    /// this request's id. Anything else — a refusal, a missing or foreign id, a
    /// missing reply — is an error, and the caller judges the command itself.
    pub fn gate(self, request: &Request, within: Duration) -> Result<serde_json::Value, String> {
        let Request::GateCommand { id, .. } = request else {
            return Err("not a gate request".into());
        };
        let ack = self.exchange(request, within)?;
        if ack.id.as_deref() != Some(id.as_str()) {
            return Err("the answer names another request".into());
        }
        ack.reply
            .ok_or_else(|| "the answer carries no verdict".into())
    }

    fn exchange(mut self, request: &Request, within: Duration) -> Result<Ack, String> {
        request.validate()?;
        self.stream
            .set_read_timeout(Some(within))
            .and_then(|()| self.stream.set_write_timeout(Some(within)))
            .map_err(|error| format!("timeouts: {error}"))?;
        let envelope = Envelope {
            v: PROTOCOL,
            request: request.clone(),
        };
        let encoded = serde_json::to_vec(&envelope).map_err(|error| error.to_string())?;
        if encoded.len() > MAX_FRAME {
            return Err(format!(
                "request is {} bytes; frames carry {MAX_FRAME}",
                encoded.len()
            ));
        }
        write_frame(&mut self.stream, &envelope)?;
        let line = read_frame(&mut self.reader)?.ok_or("the owner closed without an answer")?;
        let ack: Ack = serde_json::from_str(&line).map_err(|error| format!("answer: {error}"))?;
        if ack.accepted {
            Ok(ack)
        } else {
            Err(ack.reason.unwrap_or_else(|| "refused".into()))
        }
    }
}

/// How a hook reaches the running app: `dir` is the profile directory its
/// owner claimed. `None` when there is no owner to ask — the ordinary case when
/// GitPulse is closed, which is why this never claims anything.
pub fn reach_owner(dir: &Path, hello: Duration) -> Option<Secondary> {
    let paths = Paths::in_dir(&dir.canonicalize().ok()?);
    if !paths.socket_fits() {
        return None;
    }
    Secondary::connect_within(&paths.socket, hello).ok()
}

/// Judges one command for a hook: `(cwd, command)` to the verdict's JSON.
pub type GateSink = Arc<dyn Fn(&str, &str) -> Result<serde_json::Value, String> + Send + Sync>;

type Sink = Arc<dyn Fn(Request) + Send + Sync>;

#[derive(Default)]
struct Inbox {
    sink: Option<Sink>,
    queued: VecDeque<Request>,
}

struct Shared {
    closing: AtomicBool,
    active: AtomicUsize,
    gating: AtomicUsize,
    gate: Mutex<Option<GateSink>>,
    inbox: Mutex<Inbox>,
    own_uid: u32,
}

impl Shared {
    fn new(own_uid: u32) -> Self {
        Self {
            closing: AtomicBool::new(false),
            active: AtomicUsize::new(0),
            gating: AtomicUsize::new(0),
            gate: Mutex::new(None),
            inbox: Mutex::new(Inbox::default()),
            own_uid,
        }
    }
}

/// The profile's owner. Holds the lock for as long as it lives.
pub struct Primary {
    _lock: File,
    paths: Paths,
    /// The bound socket's identity, so shutdown removes only our own.
    socket_identity: Option<(u64, u64)>,
    shared: Arc<Shared>,
    mailbox_error: Option<String>,
}

impl Primary {
    fn start(mut lock: File, paths: Paths) -> Self {
        // For the log of a launch that could not reach us, and nothing else.
        let _ = lock
            .set_len(0)
            .and_then(|()| write!(lock, "{}", std::process::id()))
            .and_then(|()| lock.flush());
        let shared = Arc::new(Shared::new(
            // SAFETY: `getuid` reads this process's real user id and cannot
            // fail — the same id the hook bridge compares peers against.
            unsafe { libc::getuid() },
        ));
        let mut primary = Self {
            _lock: lock,
            paths,
            socket_identity: None,
            shared,
            mailbox_error: None,
        };
        if let Err(error) = primary.open_mailbox() {
            primary.mailbox_error = Some(error);
        }
        primary
    }

    fn open_mailbox(&mut self) -> Result<(), String> {
        // We hold the lock, so a socket at this path is a dead owner's, which
        // this replaces; anything else there is left alone and reported.
        let listener =
            crate::local_socket::bind_private(&self.paths.socket, "single-instance mailbox")?;
        let meta = std::fs::metadata(&self.paths.socket)
            .map_err(|error| format!("mailbox vanished: {error}"))?;
        self.socket_identity = Some((meta.dev(), meta.ino()));
        let shared = self.shared.clone();
        crate::supervisor::spawn_once("instance-mailbox", "single-instance mailbox", move || {
            accept_loop(listener, shared)
        })
        .map(|_| ())
        .map_err(|error| format!("cannot start the mailbox: {error}"))
    }

    /// Why launches cannot reach this owner, when they cannot. The lock still
    /// holds them off; they just cannot say what they wanted.
    pub fn mailbox_error(&self) -> Option<&str> {
        self.mailbox_error.as_deref()
    }

    /// Where forwarded requests go from now on. Anything that arrived before
    /// this was queued, and is delivered here first, in arrival order.
    pub fn attach(&self, sink: impl Fn(Request) + Send + Sync + 'static) {
        let sink: Sink = Arc::new(sink);
        let queued = {
            let mut inbox = self
                .shared
                .inbox
                .lock()
                .unwrap_or_else(PoisonError::into_inner);
            inbox.sink = Some(sink.clone());
            std::mem::take(&mut inbox.queued)
        };
        for request in queued {
            sink(request);
        }
    }

    /// Where hooks' command verdicts are answered from now on. Until this is
    /// called a [`Request::GateCommand`] is refused, and its hook judges the
    /// command itself.
    pub fn attach_gate(&self, sink: GateSink) {
        *self
            .shared
            .gate
            .lock()
            .unwrap_or_else(PoisonError::into_inner) = Some(sink);
    }

    /// Stops answering and takes the mailbox down, keeping the lock. A launch
    /// from here on waits for the lock instead, which the kernel releases when
    /// this process is gone — so a relaunch during a slow quit becomes the next
    /// owner rather than handing its request to an owner that is leaving.
    pub fn shutdown(&self) {
        if self.shared.closing.swap(true, Ordering::AcqRel) {
            return;
        }
        let ours = std::fs::symlink_metadata(&self.paths.socket)
            .ok()
            .map(|meta| (meta.dev(), meta.ino()));
        if ours.is_some() && ours == self.socket_identity {
            let _ = std::fs::remove_file(&self.paths.socket);
        }
    }
}

/// An owner that stops owning — a launch that deferred after winning the lock,
/// or a test — takes its mailbox down with it, so the next owner never meets a
/// socket nobody answers, and the accept thread ends at its next poll.
impl Drop for Primary {
    fn drop(&mut self) {
        self.shutdown();
    }
}

fn accept_loop(listener: UnixListener, shared: Arc<Shared>) {
    while !shared.closing.load(Ordering::Acquire) {
        if !crate::local_socket::wait_readable(listener.as_raw_fd(), ACCEPT_POLL) {
            continue;
        }
        // Accepting creates a descriptor that is made close-on-exec only
        // afterwards; the inheritance lock keeps a concurrent git spawn from
        // copying it in between. The listener is non-blocking, so the lock is
        // held for one syscall, never across a wait.
        let accepted = crate::procguard::with_inheritance_lock(|| listener.accept());
        let stream = match accepted {
            Ok((stream, _)) => stream,
            Err(error)
                if matches!(error.kind(), ErrorKind::WouldBlock | ErrorKind::Interrupted) =>
            {
                continue
            }
            Err(error) => {
                log::warn!(target: "desktop", "instance mailbox accept failed: {error}");
                std::thread::sleep(RETRY);
                continue;
            }
        };
        if shared.active.fetch_add(1, Ordering::AcqRel) >= MAX_HANDLERS + MAX_GATE_HANDLERS {
            shared.active.fetch_sub(1, Ordering::AcqRel);
            continue;
        }
        // The slot is released by the guard's drop, so a handler that panics
        // gives it back like one that returns. Eight panics would otherwise
        // fill MAX_HANDLERS and silently drop every later relaunch.
        let slot = HandlerSlot(shared.clone());
        let spawned = crate::supervisor::spawn_once(
            "instance-mailbox-conn",
            "single-instance mailbox",
            move || handle(stream, &slot.0),
        );
        if let Err(error) = spawned {
            // The closure, and the slot it owned, were dropped by the failed
            // spawn, which released the count already.
            log::warn!(target: "desktop", "instance mailbox could not answer a launch: {error}");
        }
    }
}

/// One of the [`MAX_HANDLERS`] mailbox slots, released on drop.
struct HandlerSlot(Arc<Shared>);

impl Drop for HandlerSlot {
    fn drop(&mut self) {
        self.0.active.fetch_sub(1, Ordering::AcqRel);
    }
}

fn handle(mut stream: UnixStream, shared: &Shared) {
    if let Err(ended) = answer(&mut stream, shared) {
        let (level, line) = ended.log_line();
        log::log!(target: "desktop", level, "{line}");
    }
}

/// How a conversation ended when it did not serve its peer.
#[derive(Debug, PartialEq, Eq)]
enum Ended {
    /// The peer connected, never sent a byte and left. Hooks do this to ask
    /// whether the owner is alive, many times an hour; nothing was refused
    /// because nothing was asked.
    Probe(String),
    /// Everything else, including a peer that sent part of a frame, a
    /// stranger's uid, and a peer that stalled while still connected.
    Refused(String),
}

impl Ended {
    /// The level and line this ending is logged with. A probe is routine and
    /// is not a warning; a refusal is, and keeps the line it has always had.
    fn log_line(&self) -> (log::Level, String) {
        match self {
            Self::Probe(reason) => (
                log::Level::Debug,
                format!("instance mailbox probe left before its first frame: {reason}"),
            ),
            Self::Refused(reason) => (
                log::Level::Warn,
                format!("instance mailbox refused a launch: {reason}"),
            ),
        }
    }

    /// A failure with nothing of the peer's first frame read: a [`Probe`]
    /// only if the peer is provably gone and sent nothing. When that cannot be
    /// established it is a refusal, so an unexplained failure stays loud.
    ///
    /// [`Probe`]: Ended::Probe
    fn before_first_frame(stream: &UnixStream, reason: String) -> Self {
        if crate::local_socket::peer_hung_up_silently(stream) {
            Self::Probe(reason)
        } else {
            Self::Refused(reason)
        }
    }
}

impl From<String> for Ended {
    fn from(reason: String) -> Self {
        Self::Refused(reason)
    }
}

fn answer(stream: &mut UnixStream, shared: &Shared) -> Result<(), Ended> {
    // Inherited from the non-blocking listener on the BSDs.
    if let Err(error) = stream
        .set_nonblocking(false)
        .and_then(|()| stream.set_read_timeout(Some(HANDLER_TIMEOUT)))
        .and_then(|()| stream.set_write_timeout(Some(HANDLER_TIMEOUT)))
    {
        return Err(Ended::before_first_frame(
            stream,
            format!("socket setup: {error}"),
        ));
    }
    let uid = crate::local_socket::peer_uid(stream);
    if !peer_allowed(uid, shared.own_uid) {
        return Err(Ended::Refused(format!("connection from uid {uid:?}")));
    }
    if shared.closing.load(Ordering::Acquire) {
        return Ok(());
    }
    write_frame(
        stream,
        &Hello {
            v: PROTOCOL,
            pid: std::process::id(),
        },
    )
    .map_err(|reason| Ended::before_first_frame(stream, reason))?;
    let mut reader = BufReader::new(stream.try_clone().map_err(|error| error.to_string())?);
    // A launch that only wanted to know whether we are alive closes here. An
    // error with nothing buffered is also before the first frame; once a byte
    // is buffered, anything that goes wrong is a frame cut short.
    match reader.fill_buf() {
        Ok([]) => return Ok(()),
        Ok(_) => {}
        Err(error) => {
            return Err(Ended::before_first_frame(stream, format!("read: {error}")));
        }
    }
    let Some(line) = read_frame(&mut reader)? else {
        return Ok(());
    };
    let request = serde_json::from_str::<Envelope>(&line)
        .map_err(|error| format!("malformed request: {error}"))
        .and_then(|envelope| {
            if envelope.v != PROTOCOL {
                return Err(format!("protocol {} is not {PROTOCOL}", envelope.v));
            }
            envelope.request.validate()?;
            Ok(envelope.request)
        });
    let request = match request {
        Ok(request) => request,
        Err(reason) => {
            write_frame(stream, &Ack::refused(reason.clone()))?;
            return Err(Ended::Refused(reason));
        }
    };
    if let Request::GateCommand { id, cwd, command } = &request {
        return answer_gate(stream, shared, id, cwd, command).map_err(Ended::Refused);
    }
    let verdict = admit(shared, request);
    let ack = match &verdict {
        Ok(_) => Ack::accepted(),
        Err(reason) => Ack::refused(reason.clone()),
    };
    write_frame(stream, &ack)?;
    match verdict {
        Ok(Some((sink, request))) => {
            sink(request);
            Ok(())
        }
        Ok(None) => Ok(()),
        Err(reason) => Err(Ended::Refused(reason)),
    }
}

/// Answers a hook's [`Request::GateCommand`] on its connection. The sink's
/// error is a refusal, not a verdict: the hook reads it as "judge this
/// yourself", which is the path that has always decided.
fn answer_gate(
    stream: &mut UnixStream,
    shared: &Shared,
    id: &str,
    cwd: &str,
    command: &str,
) -> Result<(), String> {
    let slot = GateSlot::take(shared);
    // Cloned out so the lock is not held while the verdict is worked out.
    let sink = shared
        .gate
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .clone();
    let outcome = if shared.closing.load(Ordering::Acquire) {
        Err("GitPulse is quitting".to_string())
    } else if slot.is_none() {
        Err("the gate is busy".to_string())
    } else {
        match sink {
            Some(sink) => sink(cwd, command),
            None => Err("the gate is not attached".to_string()),
        }
    };
    let ack = match outcome {
        Ok(reply) => Ack {
            accepted: true,
            reason: None,
            id: Some(id.to_string()),
            reply: Some(reply),
        },
        Err(reason) => Ack::refused(reason),
    };
    write_frame(stream, &ack)
}

/// One of the [`MAX_GATE_HANDLERS`] verdicts in flight, released on drop.
struct GateSlot<'a>(&'a Shared);

impl<'a> GateSlot<'a> {
    fn take(shared: &'a Shared) -> Option<Self> {
        if shared.gating.fetch_add(1, Ordering::AcqRel) >= MAX_GATE_HANDLERS {
            shared.gating.fetch_sub(1, Ordering::AcqRel);
            return None;
        }
        Some(Self(shared))
    }
}

impl Drop for GateSlot<'_> {
    fn drop(&mut self) {
        self.0.gating.fetch_sub(1, Ordering::AcqRel);
    }
}

/// Queues `request`, or returns the sink to deliver it to once acknowledged.
fn admit(shared: &Shared, request: Request) -> Result<Option<(Sink, Request)>, String> {
    let mut inbox = shared.inbox.lock().unwrap_or_else(PoisonError::into_inner);
    if shared.closing.load(Ordering::Acquire) {
        return Err("GitPulse is quitting".into());
    }
    if let Some(sink) = &inbox.sink {
        return Ok(Some((sink.clone(), request)));
    }
    if inbox.queued.len() >= MAX_QUEUED {
        return Err("GitPulse is still starting and its queue is full".into());
    }
    inbox.queued.push_back(request);
    Ok(None)
}

#[cfg(test)]
#[path = "instance_tests.rs"]
mod tests;
