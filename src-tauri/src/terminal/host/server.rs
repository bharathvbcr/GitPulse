//! `gitpulse-ptyd`: the process that holds the terminals.
//!
//! It runs the same engine the app runs in-process — `start_session`, the
//! reader, `OutputFlow`, `kill_session` — and answers one connected app over a
//! private unix socket. Because the PTY masters live here, an app that quits,
//! crashes or is killed takes no session with it: the host detaches every view,
//! keeps reading into each session's ring, writes down every exit, and waits for
//! an app to come back.
//!
//! One app at a time. A second one is refused while the first is connected,
//! with a reason; when the first one's connection ends — it quit, crashed, or
//! stopped reading for [`WRITE_DEADLINE`] — the next is welcome.

use super::proto::{
    self, Envelope, Hello, HostFrame, Request, Retired, WireEnded, WireNotice, PROTOCOL,
};
use super::receipts::Receipts;
use crate::terminal::{
    self as engine, OutputSink, Place, SessionEnded, SessionEvents, TerminalLossPayload,
    TerminalSessions,
};
use serde::Serialize;
use std::collections::HashMap;
use std::io::BufReader;
use std::os::unix::io::AsRawFd;
use std::os::unix::net::UnixStream;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::{Duration, Instant};

/// How long a write to the app may block before the app is taken to be gone.
/// A live app reads on a thread of its own, so only a stopped or wedged one
/// takes this long; its connection is closed and every session is detached,
/// which is what its death would have done.
pub const WRITE_DEADLINE: Duration = Duration::from_secs(5);
/// How long a new connection has to introduce itself.
const HELLO_DEADLINE: Duration = Duration::from_secs(5);
/// How often the accept loop looks up to check whether it should exit.
const ACCEPT_WAIT: Duration = Duration::from_millis(250);
/// The default for how long a host with no app and no sessions waits before it
/// exits.
pub const DEFAULT_IDLE_EXIT: Duration = Duration::from_secs(30);
/// Socket send buffer asked for: a few output messages, so a burst does not
/// make the reader wait on the app's read loop for each one.
const SEND_BUFFER_BYTES: libc::c_int = 1024 * 1024;

/// How `serve` was started.
#[derive(Debug, Clone)]
pub struct ServeConfig {
    /// The host directory: socket, lock and receipts.
    pub dir: PathBuf,
    pub idle_exit: Duration,
}

/// Why `serve` ended, as the binary's exit code.
pub mod exit_code {
    pub const IDLE: i32 = 0;
    pub const FAILED: i32 = 1;
    pub const OCCUPIED: i32 = 3;
}

/// The one app connection.
struct Conn {
    writer: Mutex<UnixStream>,
    /// Kept to shut the connection down from any thread, which ends its read
    /// loop as well.
    control: UnixStream,
    closed: AtomicBool,
}

impl Conn {
    fn new(stream: &UnixStream) -> std::io::Result<Self> {
        Ok(Self {
            writer: Mutex::new(stream.try_clone()?),
            control: stream.try_clone()?,
            closed: AtomicBool::new(false),
        })
    }

    fn send(&self, header: &HostFrame, payload: &[u8]) -> Result<(), String> {
        if self.closed.load(Ordering::Acquire) {
            return Err("the app's connection is closed".into());
        }
        let mut writer = self.writer.lock().unwrap_or_else(PoisonError::into_inner);
        proto::write_frame(&mut *writer, header, payload).map_err(|error| {
            drop(writer);
            self.close();
            format!("the app's connection failed: {error}")
        })
    }

    fn reply(&self, id: u64, result: Result<serde_json::Value, String>) {
        let frame = match result {
            Ok(ok) => HostFrame::Reply {
                id,
                ok: Some(ok),
                error: None,
            },
            Err(error) => HostFrame::Reply {
                id,
                ok: None,
                error: Some(error),
            },
        };
        if let Err(error) = self.send(&frame, &[]) {
            log::debug!(target: "terminal_host", "a reply was not delivered: {error}");
        }
    }

    fn close(&self) {
        if !self.closed.swap(true, Ordering::AcqRel) {
            let _ = self.control.shutdown(std::net::Shutdown::Both);
        }
    }

    fn is_closed(&self) -> bool {
        self.closed.load(Ordering::Acquire)
    }
}

/// Whichever app is connected now.
#[derive(Default)]
struct Link {
    current: Mutex<Option<Arc<Conn>>>,
}

impl Link {
    fn current(&self) -> Option<Arc<Conn>> {
        self.current
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone()
    }

    fn send(&self, header: &HostFrame, payload: &[u8]) -> Result<(), String> {
        self.current()
            .ok_or_else(|| "no GitPulse app is connected".to_string())?
            .send(header, payload)
    }
}

/// Every session's output goes to whichever app is connected. With none, the
/// engine has detached every session and holds instead of sending.
fn link_sink(link: Arc<Link>) -> OutputSink {
    OutputSink::from_fn(move |session, data| match data {
        Some(bytes) => link.send(
            &HostFrame::Output {
                session: session.to_owned(),
            },
            bytes,
        ),
        None => link.send(
            &HostFrame::End {
                session: session.to_owned(),
            },
            &[],
        ),
    })
}

/// One session's events, as frames to the app, and its exit on disk first.
struct HostEvents {
    link: Arc<Link>,
    receipts: Arc<Receipts>,
    run_id: Option<String>,
}

impl SessionEvents for HostEvents {
    fn loss(&self, loss: TerminalLossPayload) {
        let _ = self.link.send(&HostFrame::Loss { loss }, &[]);
    }

    fn notice(&self, notice: crate::alerts::Notice) {
        let notice = WireNotice::from_notice(&notice);
        let _ = self.link.send(&HostFrame::Notice { notice }, &[]);
    }

    fn scan_drops(&self, dropped: u64) {
        let _ = self.link.send(&HostFrame::ScanDrops { dropped }, &[]);
    }

    fn exit(&self, ended: SessionEnded) {
        let ended = WireEnded {
            payload: ended.payload,
            ended_by_gitpulse: ended.ended_by_gitpulse,
            program: ended.program,
            cwd: ended.cwd,
            run_id: self.run_id.clone(),
        };
        // On disk before anyone is told: an app that misses the frame (none
        // is connected, or it dies reading it) learns of it at its next start.
        if let Err(error) = self.receipts.exited(&ended) {
            log::error!(target: "terminal_host", "a session's exit could not be recorded: {error}");
        }
        if let Err(error) = self.link.send(&HostFrame::Exit { ended }, &[]) {
            log::info!(target: "terminal_host", "a session ended with no app to tell; its receipt is kept: {error}");
        }
    }
}

struct Host {
    engine: TerminalSessions,
    link: Arc<Link>,
    receipts: Arc<Receipts>,
    /// Test-only master holds (`Request::Hold`), released by `Release` or
    /// when the connection that took them ends.
    holds: Mutex<HashMap<String, engine::MasterHold>>,
    /// Set by a granted `Retire`: the accept loop ends.
    retire: AtomicBool,
}

impl Host {
    fn session_count(&self) -> usize {
        self.engine.lock_sessions().len()
    }

    fn sink(&self) -> OutputSink {
        link_sink(self.link.clone())
    }

    fn control(&self, request: Request) -> Result<serde_json::Value, String> {
        match request {
            Request::Spawn { request } => {
                let session = request.session_id.clone();
                // Refused before a receipt is written: a duplicate must not
                // touch the live session's file.
                if self.engine.lock_sessions().contains_key(&session) {
                    return Err(format!("Terminal session '{session}' already exists"));
                }
                let program = request
                    .program
                    .clone()
                    .filter(|p| !p.trim().is_empty())
                    .unwrap_or_else(engine::default_shell);
                let place = match &request.place {
                    Place::Repo { repo_path, .. } => repo_path.clone(),
                    Place::Task { cwd } => cwd.clone(),
                };
                self.receipts
                    .spawned(&session, request.run_id.as_deref(), &program, &place)?;
                let events = Arc::new(HostEvents {
                    link: self.link.clone(),
                    receipts: self.receipts.clone(),
                    run_id: request.run_id.clone(),
                });
                match engine::start_session(&self.engine, request, self.sink(), events, None) {
                    Ok(started) => {
                        let (process, process_error) = match started.process {
                            Ok(process) => (Some(process), None),
                            Err(error) => (None, Some(error)),
                        };
                        json(proto::SpawnReply {
                            spawned: started.spawned,
                            process,
                            process_error,
                        })
                    }
                    Err(error) => {
                        // Only a session whose reader never started has no
                        // exit coming; one that started reports its own.
                        if !self.engine.lock_sessions().contains_key(&session) {
                            self.receipts.discard(&session);
                        }
                        Err(error)
                    }
                }
            }
            Request::Detach { session } => json(engine::detach_session(&self.engine, &session)?),
            Request::DetachAll => json(engine::detach_all(&self.engine)),
            Request::Attach {
                session,
                rows,
                cols,
            } => json(engine::attach_session(
                &self.engine,
                &session,
                rows,
                cols,
                self.sink(),
            )?),
            Request::AttachTracked { run_id, rows, cols } => json(engine::attach_tracked_session(
                &self.engine,
                &run_id,
                rows,
                cols,
                self.sink(),
            )),
            Request::Kill { session } => json(engine::kill_session(&self.engine, &session)?),
            Request::StopAll => json(engine::shutdown_sessions(&self.engine)?),
            Request::List => json(engine::list_sessions(&self.engine)?),
            Request::Context { session } => json(engine::session_context(&self.engine, &session)?),
            Request::Receipts => json(self.receipts.pending()),
            Request::Forget { session } => json(self.receipts.forget(&session)?),
            Request::Wakeups { session } => json(engine::reader_wakeups(&self.engine, &session)),
            Request::Size { session } => json(engine::pty_size(&self.engine, &session)?),
            Request::Hold { session } => {
                let hold = engine::hold_master(&self.engine, &session)?;
                if let Some(earlier) = self
                    .holds
                    .lock()
                    .unwrap_or_else(PoisonError::into_inner)
                    .insert(session, hold)
                {
                    earlier.release();
                }
                json(())
            }
            Request::Release { session } => {
                let hold = self
                    .holds
                    .lock()
                    .unwrap_or_else(PoisonError::into_inner)
                    .remove(&session);
                if let Some(hold) = hold {
                    hold.release();
                }
                json(())
            }
            Request::Hello { .. }
            | Request::Retire
            | Request::Write { .. }
            | Request::Resize { .. }
            | Request::Ack { .. }
            | Request::Background { .. }
            | Request::Limit { .. }
            | Request::Stall { .. } => Err("not a control request".into()),
        }
    }

    /// One request: keystrokes and resizes on the session's own lanes, the
    /// cheap bookkeeping inline, everything else on the control pool, so a
    /// close waiting out its hangup grace never holds up typing.
    fn dispatch(self: &Arc<Self>, conn: &Arc<Conn>, id: u64, request: Request, payload: Vec<u8>) {
        match request {
            Request::Hello { .. } | Request::Retire => conn.reply(
                id,
                Err("this connection has already introduced itself".into()),
            ),
            // Inline: a counter update under a short lock. Answered, so an
            // acknowledgement the window refuses is refused to the view, as
            // in-process.
            Request::Ack { session, bytes } => {
                conn.reply(
                    id,
                    engine::acknowledge_output(&self.engine, &session, bytes)
                        .map(|()| serde_json::Value::Null),
                );
            }
            Request::Background { sessions } => {
                engine::set_background_sessions(&self.engine, &sessions);
                conn.reply(id, json(()));
            }
            Request::Limit { limit } => {
                self.engine.set_session_limit(limit);
                conn.reply(id, json(self.engine.session_limit()));
            }
            Request::Stall { ms } => {
                self.engine
                    .set_output_stall_timeout(Duration::from_millis(ms));
                conn.reply(id, json(()));
            }
            Request::Write { session } => {
                let lane = match self.engine.input_lane(&session) {
                    Ok(lane) => lane,
                    Err(error) => return conn.reply(id, Err(error)),
                };
                let (host, reply_to) = (self.clone(), conn.clone());
                let queued = lane.spawn("terminal_write", move || {
                    let written = engine::write_pty_bytes(&host.engine, &session, &payload);
                    reply_to.reply(id, written.clone().map(|()| serde_json::Value::Null));
                    written
                });
                if let Err(error) = queued {
                    conn.reply(id, Err(error));
                }
            }
            Request::Resize {
                session,
                rows,
                cols,
            } => {
                let lane = match self.engine.geometry_lane(&session) {
                    Ok(lane) => lane,
                    Err(error) => return conn.reply(id, Err(error)),
                };
                let (host, reply_to) = (self.clone(), conn.clone());
                let queued = lane.spawn("terminal_resize", move || {
                    let resized = engine::resize_session(&host.engine, &session, rows, cols);
                    reply_to.reply(id, resized.clone().map(|()| serde_json::Value::Null));
                    resized
                });
                if let Err(error) = queued {
                    conn.reply(id, Err(error));
                }
            }
            request => {
                let operation = operation_label(&request);
                let (host, reply_to) = (self.clone(), conn.clone());
                let queued = self.engine.control.spawn(operation, move || {
                    let result = host.control(request);
                    let outcome = result.as_ref().map(|_| ()).map_err(Clone::clone);
                    reply_to.reply(id, result);
                    outcome
                });
                if let Err(error) = queued {
                    conn.reply(id, Err(error));
                }
            }
        }
    }

    /// The connection ended. If it was the current one, every session loses its
    /// view, exactly as a page reload would, and keeps running.
    fn disconnected(&self, conn: &Arc<Conn>) {
        conn.close();
        let mut current = self
            .link
            .current
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        if current.as_ref().is_some_and(|c| Arc::ptr_eq(c, conn)) {
            *current = None;
            drop(current);
            let detached = engine::detach_all(&self.engine);
            let holds =
                std::mem::take(&mut *self.holds.lock().unwrap_or_else(PoisonError::into_inner));
            for (_, hold) in holds {
                hold.release();
            }
            log::info!(target: "terminal_host", "the app disconnected; {detached} session(s) left running");
        }
    }
}

fn operation_label(request: &Request) -> &'static str {
    match request {
        Request::Spawn { .. } => "host_spawn",
        Request::Kill { .. } => "host_kill",
        Request::StopAll => "host_stop_all",
        Request::Attach { .. } | Request::AttachTracked { .. } => "host_attach",
        Request::Context { .. } => "host_context",
        Request::List => "host_list",
        _ => "host_control",
    }
}

fn json(value: impl Serialize) -> Result<serde_json::Value, String> {
    serde_json::to_value(value).map_err(|e| e.to_string())
}

/// Serves one connection until it ends.
fn serve_connection(host: Arc<Host>, stream: UnixStream) {
    if let Err(error) = prepare(&stream) {
        log::warn!(target: "terminal_host", "a connection could not be set up: {error}");
        return;
    }
    let conn = match Conn::new(&stream) {
        Ok(conn) => Arc::new(conn),
        Err(error) => {
            log::warn!(target: "terminal_host", "a connection could not be set up: {error}");
            return;
        }
    };
    let mut reader = BufReader::new(stream);
    if !introduce(&host, &conn, &mut reader) {
        conn.close();
        return;
    }
    loop {
        let frame = match proto::read_frame(&mut reader) {
            Ok(Some(frame)) => frame,
            Ok(None) => break,
            Err(error) => {
                log::warn!(target: "terminal_host", "the app's connection ended: {error}");
                break;
            }
        };
        match proto::decode::<Envelope>(&frame.0) {
            Ok(envelope) => host.dispatch(&conn, envelope.id, envelope.request, frame.1),
            Err(error) => {
                // A frame this host cannot read means the two do not speak the
                // same protocol after all; carrying on would guess.
                log::warn!(target: "terminal_host", "an unreadable request ended the connection: {error}");
                break;
            }
        }
    }
    host.disconnected(&conn);
}

/// The first frame: `hello` (become the connected app) or `retire`.
fn introduce(host: &Arc<Host>, conn: &Arc<Conn>, reader: &mut BufReader<UnixStream>) -> bool {
    let _ = reader.get_ref().set_read_timeout(Some(HELLO_DEADLINE));
    let first = proto::read_frame(reader)
        .ok()
        .flatten()
        .and_then(|(header, _)| proto::decode::<Envelope>(&header).ok());
    let _ = reader.get_ref().set_read_timeout(None);
    let Some(Envelope { id, request }) = first else {
        return false;
    };
    match request {
        Request::Hello { protocol, build } => {
            let mut current = host
                .link
                .current
                .lock()
                .unwrap_or_else(PoisonError::into_inner);
            if current.as_ref().is_some_and(|c| !c.is_closed()) {
                drop(current);
                conn.reply(
                    id,
                    Err("Another GitPulse window is connected to this terminal host.".into()),
                );
                return false;
            }
            conn.reply(
                id,
                json(Hello {
                    protocol: PROTOCOL,
                    build: crate::updates::CURRENT_VERSION.to_owned(),
                    pid: std::process::id(),
                    sessions: host.session_count(),
                }),
            );
            if protocol != PROTOCOL {
                log::warn!(target: "terminal_host", "an app speaking protocol {protocol} (build {build}) was turned away; this host speaks {PROTOCOL}");
                return false;
            }
            if build != crate::updates::CURRENT_VERSION {
                log::info!(target: "terminal_host", "app build {build} connected to host build {}", crate::updates::CURRENT_VERSION);
            }
            *current = Some(conn.clone());
            true
        }
        Request::Retire => {
            let sessions = host.session_count();
            let connected = host.link.current().is_some_and(|c| !c.is_closed());
            let retired = sessions == 0 && !connected;
            if retired {
                host.retire.store(true, Ordering::Release);
            }
            conn.reply(id, json(Retired { retired, sessions }));
            false
        }
        _ => {
            conn.reply(id, Err("the first request must be hello".into()));
            false
        }
    }
}

fn prepare(stream: &UnixStream) -> std::io::Result<()> {
    stream.set_nonblocking(false)?;
    stream.set_write_timeout(Some(WRITE_DEADLINE))?;
    // SAFETY: a valid descriptor, a correctly sized int option.
    let rc = unsafe {
        libc::setsockopt(
            stream.as_raw_fd(),
            libc::SOL_SOCKET,
            libc::SO_SNDBUF,
            (&SEND_BUFFER_BYTES as *const libc::c_int).cast(),
            std::mem::size_of::<libc::c_int>() as libc::socklen_t,
        )
    };
    if rc != 0 {
        log::debug!(target: "terminal_host", "the send buffer was left at its default: {}", std::io::Error::last_os_error());
    }
    Ok(())
}

/// Runs the host until it is idle long enough, retired, or signalled. Returns
/// the process exit code.
pub fn serve(config: ServeConfig) -> i32 {
    if let Err(error) = crate::local_socket::private_dir(&config.dir) {
        eprintln!("gitpulse-ptyd: {error}");
        return exit_code::FAILED;
    }
    let _lock = match crate::daemon_lock::acquire_named(
        &config.dir,
        &super::lock_name(PROTOCOL),
        crate::updates::CURRENT_VERSION,
    ) {
        crate::daemon_lock::Acquire::Held(lock) => lock,
        crate::daemon_lock::Acquire::Occupied(owner) => {
            eprintln!(
                "gitpulse-ptyd: another terminal host (pid {:?}, build {:?}) already serves protocol {PROTOCOL} in {}",
                owner.pid,
                owner.version,
                config.dir.display()
            );
            return exit_code::OCCUPIED;
        }
        crate::daemon_lock::Acquire::Unavailable(error) => {
            eprintln!("gitpulse-ptyd: {error}");
            return exit_code::FAILED;
        }
    };
    let receipts = match Receipts::open(config.dir.join(super::RECEIPTS_DIR)) {
        Ok(receipts) => Arc::new(receipts),
        Err(error) => {
            eprintln!("gitpulse-ptyd: {error}");
            return exit_code::FAILED;
        }
    };
    let recovered = receipts.recover(&std::collections::HashSet::new());
    if recovered > 0 {
        log::warn!(target: "terminal_host", "{recovered} session(s) outlived by an earlier host were recorded as ended with it");
    }
    let socket = config.dir.join(super::socket_name(PROTOCOL));
    let listener = match crate::local_socket::bind_private(&socket, "terminal host socket") {
        Ok(listener) => listener,
        Err(error) => {
            eprintln!("gitpulse-ptyd: {error}");
            return exit_code::FAILED;
        }
    };
    let host = Arc::new(Host {
        engine: TerminalSessions::default(),
        link: Arc::default(),
        receipts,
        holds: Mutex::default(),
        retire: AtomicBool::new(false),
    });
    {
        let host = host.clone();
        crate::procguard::set_shutdown_hook(move || {
            log::info!(target: "terminal_host", "signalled: stopping every session");
            if let Err(error) = engine::shutdown_sessions(&host.engine) {
                log::warn!(target: "terminal_host", "terminal shutdown incomplete: {error}");
            }
        });
    }
    log::info!(target: "terminal_host", "terminal host (protocol {PROTOCOL}, build {}) listening", crate::updates::CURRENT_VERSION);
    // SAFETY: `getuid` reads this process's real user id and cannot fail.
    let own = unsafe { libc::getuid() };
    let mut idle_since = Some(Instant::now());
    let code = loop {
        if host.retire.load(Ordering::Acquire) {
            log::info!(target: "terminal_host", "retired by a newer app");
            break exit_code::IDLE;
        }
        let idle = host.link.current().is_none() && host.session_count() == 0;
        match (idle, idle_since) {
            (false, _) => idle_since = None,
            (true, None) => idle_since = Some(Instant::now()),
            (true, Some(since)) if since.elapsed() >= config.idle_exit => {
                log::info!(target: "terminal_host", "no app and no sessions for {:?}; exiting", config.idle_exit);
                break exit_code::IDLE;
            }
            (true, Some(_)) => {}
        }
        if !crate::local_socket::wait_readable(listener.as_raw_fd(), ACCEPT_WAIT) {
            continue;
        }
        match crate::procguard::with_inheritance_lock(|| listener.accept()) {
            Ok((stream, _)) => {
                if crate::local_socket::peer_uid(&stream) != Some(own) {
                    log::warn!(target: "terminal_host", "a connection from another user was refused");
                    continue;
                }
                let host = host.clone();
                if let Err(error) = crate::supervisor::spawn_one_of_many(
                    "terminal-host-connection",
                    "terminal host connection",
                    move || serve_connection(host, stream),
                ) {
                    log::warn!(target: "terminal_host", "a connection could not be served: {error}");
                }
            }
            Err(error)
                if matches!(
                    error.kind(),
                    std::io::ErrorKind::WouldBlock | std::io::ErrorKind::Interrupted
                ) => {}
            Err(error) => {
                log::error!(target: "terminal_host", "the terminal host stopped accepting: {error}");
                break exit_code::FAILED;
            }
        }
    };
    drop(listener);
    // Only a socket, and only ours: a successor binding the path after this
    // host decided to go must keep it, and nothing but this host binds it
    // while the lock is held.
    let _ = std::fs::remove_file(&socket);
    code
}
