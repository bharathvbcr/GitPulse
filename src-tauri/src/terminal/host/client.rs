//! The app's side of the terminal host.
//!
//! [`HostClient`] is what `TerminalSessions` holds when the host backend is
//! selected. Every `terminal::` function routes here; the callers do not know.
//! It owns the connection (starting the host when nothing answers), a proxy per
//! session the host holds (where its output goes in this app, and its run
//! observer), and the work only the app can do: the run's bookkeeping, the
//! incident record and the Tauri events.
//!
//! When the app starts it connects, adopts every session the host still holds,
//! and records every exit that happened while it was gone (`Request::Receipts`),
//! before the workbench reconciles its runs: an agent that finished while the
//! app was down gets its real exit status, and one still running is not
//! released as abandoned.

use super::proto::{
    self, Envelope, Hello, HostFrame, Request, Retired, SpawnReply, WireEnded, PROTOCOL,
};
use crate::terminal::executor::Executor;
use crate::terminal::sink::OutputSlot;
use crate::terminal::{
    MasterHold, OutputSink, SessionEnded, SessionEvents, SessionObserver, SpawnRequest,
    TerminalContext, TerminalListing, TerminalLossPayload, TerminalSpawned, SHED_NOTICE,
};
use serde::de::DeserializeOwned;
use std::collections::{HashMap, VecDeque};
use std::io::BufReader;
use std::os::unix::net::UnixStream;
use std::panic::{catch_unwind, AssertUnwindSafe};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::sync::{mpsc, Arc, Mutex, MutexGuard, PoisonError, Weak};
use std::time::{Duration, Instant};

/// What a call that lost its connection is told.
const LOST: &str = "The connection to the terminal host was lost; the terminal kept running there.";
/// The longest an ordinary request waits for its answer.
const CALL_DEADLINE: Duration = Duration::from_secs(10);
/// A spawn checks the repository and its trust and forks.
const SPAWN_DEADLINE: Duration = Duration::from_secs(30);
/// A close waits out a hangup grace, and stopping every session waits for all.
const KILL_DEADLINE: Duration = Duration::from_secs(15);
const STOP_ALL_DEADLINE: Duration = Duration::from_secs(30);
/// How long the app waits for a host it started to listen.
const START_DEADLINE: Duration = Duration::from_secs(5);
/// How long a closed session's exit may take to be recorded here once the
/// host has reaped it.
const EXIT_RECORD_DEADLINE: Duration = Duration::from_secs(5);
/// How often expired calls are failed.
const SWEEP_EVERY: Duration = Duration::from_millis(250);
/// A probe of another protocol's host.
const PROBE_DEADLINE: Duration = Duration::from_secs(2);
/// Session ids whose exit was recorded, kept to recognise a second report of it.
const REMEMBERED_EXITS: usize = 1024;

/// Where the host lives and how to start it.
#[derive(Debug, Clone)]
pub struct HostConfig {
    /// The host directory: socket, lock, receipts, log.
    pub dir: PathBuf,
    /// The host executable. `None` is the one beside this app (or
    /// `GITPULSE_PTYD`).
    pub binary: Option<PathBuf>,
    /// Passed to a host this app starts; `None` is the host's default.
    pub idle_exit: Option<Duration>,
    /// Where a host this app starts writes its durable log
    /// (`logging::LOG_DIR_ENV`). `None` is the user's log directory, as for
    /// every other GitPulse binary; a test names its own, so a test host
    /// never writes into the real log.
    pub log_dir: Option<PathBuf>,
}

impl HostConfig {
    /// The app's own host, in its config directory.
    pub fn for_app() -> Result<Self, String> {
        Ok(Self {
            dir: super::default_dir().ok_or("Application config directory unavailable")?,
            binary: None,
            idle_exit: None,
            log_dir: None,
        })
    }
}

/// Runs a waiter with the answer to its call.
type Waiter = Box<dyn FnOnce(Result<serde_json::Value, String>) + Send>;
/// Makes the run observer for a task run whose own observer died with an
/// earlier app.
pub(crate) type Adopter = Arc<dyn Fn(&str) -> Option<Arc<dyn SessionObserver>> + Send + Sync>;

/// One open connection to the host.
struct Connection {
    writer: Mutex<UnixStream>,
    control: UnixStream,
    next_id: AtomicU64,
    pending: Mutex<HashMap<u64, (Instant, Waiter)>>,
    closed: AtomicBool,
    /// The host's pid, from its hello; 0 until it has answered.
    host_pid: std::sync::atomic::AtomicU32,
    /// Frames the reader has queued for delivery, and how many the delivery
    /// thread has finished. A reply is resolved on the reader, so a call
    /// whose answer must not overtake what the host sent before it (an
    /// attach, whose replay comes first) waits for delivery to catch up.
    queued: AtomicU64,
    delivered: Mutex<u64>,
    caught_up: std::sync::Condvar,
    /// The delivery thread, which never waits on itself.
    delivery: std::sync::OnceLock<std::thread::ThreadId>,
}

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

impl Connection {
    fn send(
        &self,
        request: Request,
        payload: &[u8],
        waiter: Option<(Duration, Waiter)>,
    ) -> Result<(), String> {
        if self.closed.load(Ordering::Acquire) {
            return Err(LOST.into());
        }
        let id = self.next_id.fetch_add(1, Ordering::Relaxed);
        if let Some((within, waiter)) = waiter {
            lock(&self.pending).insert(id, (Instant::now() + within, waiter));
        }
        let mut writer = lock(&self.writer);
        if let Err(error) = proto::write_frame(&mut *writer, &Envelope { id, request }, payload) {
            drop(writer);
            lock(&self.pending).remove(&id);
            self.close();
            return Err(format!("{LOST} ({error})"));
        }
        Ok(())
    }

    fn call(
        &self,
        request: Request,
        payload: &[u8],
        within: Duration,
    ) -> Result<serde_json::Value, String> {
        let (answer, answered) = mpsc::sync_channel(1);
        self.send(
            request,
            payload,
            Some((
                within,
                Box::new(move |result| {
                    let _ = answer.send(result);
                }),
            )),
        )?;
        // The sweeper fails the call at its deadline; this bound is only for
        // a sweeper that is itself gone.
        answered
            .recv_timeout(within + SWEEP_EVERY * 4)
            .unwrap_or_else(|_| Err(LOST.into()))
    }

    /// [`Connection::call`], answered only once everything the host sent
    /// before its reply has been delivered.
    fn call_in_order(
        &self,
        request: Request,
        payload: &[u8],
        within: Duration,
    ) -> Result<serde_json::Value, String> {
        let answer = self.call(request, payload, within)?;
        if self.delivery.get() != Some(&std::thread::current().id()) {
            let target = self.queued.load(Ordering::Acquire);
            let deadline = Instant::now() + within;
            let mut delivered = lock(&self.delivered);
            while *delivered < target && !self.is_closed() {
                let left = deadline.saturating_duration_since(Instant::now());
                if left.is_zero() {
                    return Err("The terminal host answered, but what it sent first was not delivered in time.".into());
                }
                delivered = self
                    .caught_up
                    .wait_timeout(delivered, left.min(SWEEP_EVERY))
                    .unwrap_or_else(PoisonError::into_inner)
                    .0;
            }
        }
        Ok(answer)
    }

    fn mark_delivered(&self) {
        *lock(&self.delivered) += 1;
        self.caught_up.notify_all();
    }

    fn resolve(&self, id: u64, result: Result<serde_json::Value, String>) {
        let waiter = lock(&self.pending).remove(&id);
        if let Some((_, waiter)) = waiter {
            waiter(result);
        }
    }

    /// Fails every call past its deadline. Returns whether to keep sweeping.
    fn sweep(&self) -> bool {
        let now = Instant::now();
        let expired: Vec<Waiter> = {
            let mut pending = lock(&self.pending);
            let ids: Vec<u64> = pending
                .iter()
                .filter(|(_, (deadline, _))| *deadline <= now)
                .map(|(id, _)| *id)
                .collect();
            ids.into_iter()
                .filter_map(|id| pending.remove(&id).map(|(_, waiter)| waiter))
                .collect()
        };
        for waiter in expired {
            waiter(Err("The terminal host did not answer in time.".into()));
        }
        !self.closed.load(Ordering::Acquire)
    }

    fn close(&self) {
        if self.closed.swap(true, Ordering::AcqRel) {
            return;
        }
        let _ = self.control.shutdown(std::net::Shutdown::Both);
        let pending: Vec<Waiter> = lock(&self.pending).drain().map(|(_, (_, w))| w).collect();
        for waiter in pending {
            waiter(Err(LOST.into()));
        }
    }

    fn is_closed(&self) -> bool {
        self.closed.load(Ordering::Acquire)
    }
}

/// A session the host holds, as this app knows it.
struct Proxy {
    /// Where its output goes in this app: the view's channel, or nowhere.
    output: Arc<OutputSlot>,
    observer: Option<Arc<dyn SessionObserver>>,
    run_id: Option<String>,
    /// A view in this app shows it.
    viewed: bool,
    /// Its start has not been recorded yet; an exit that arrives first waits.
    starting: bool,
    deferred_exit: Option<WireEnded>,
    /// Sending its output to the view is failing (logged once per run).
    failing: Arc<AtomicBool>,
    /// The host's end marker has been passed to the view.
    end_sent: bool,
    /// This session's keystrokes and resizes, in order, one at a time — the
    /// app's own lanes, as in-process: a full queue is refused at once, and a
    /// call dropped before its turn is skipped rather than sent.
    input: Executor,
    geometry: Executor,
}

impl Proxy {
    fn new(
        output: OutputSink,
        observer: Option<Arc<dyn SessionObserver>>,
        run_id: Option<String>,
        viewed: bool,
        starting: bool,
    ) -> Self {
        let (input, geometry) = crate::terminal::session_lanes();
        Self {
            output: Arc::new(OutputSlot::new(output)),
            observer,
            run_id,
            viewed,
            starting,
            deferred_exit: None,
            failing: Arc::default(),
            end_sent: false,
            input,
            geometry,
        }
    }
}

/// A sink that sends nowhere: a session with no view in this app.
fn nowhere() -> OutputSink {
    OutputSink::from_fn(|_, _| Ok(()))
}

struct Inner {
    config: HostConfig,
    conn: Mutex<Option<Arc<Connection>>>,
    connecting: Mutex<()>,
    proxies: Mutex<HashMap<String, Proxy>>,
    events: Mutex<Option<Arc<dyn SessionEvents>>>,
    adopter: Mutex<Option<Adopter>>,
    /// Sessions whose exit this app has recorded, newest last.
    ended: Mutex<VecDeque<String>>,
    /// Every session the host holds has a proxy here: set once a connection
    /// has listed them. Until then, whether a run's terminal is held is not
    /// known, and is answered as held.
    adopted: AtomicBool,
    limit: AtomicUsize,
    stall_ms: AtomicU64,
    /// The app is quitting and has let the host go: no reconnecting.
    leaving: AtomicBool,
    /// What is wrong with the host, said plainly, when something is.
    trouble: Mutex<Vec<String>>,
}

/// The app's handle on the terminal host. Cheap to clone.
#[derive(Clone)]
pub struct HostClient {
    inner: Arc<Inner>,
}

impl HostClient {
    pub fn new(config: HostConfig) -> Self {
        Self {
            inner: Arc::new(Inner {
                config,
                conn: Mutex::default(),
                connecting: Mutex::default(),
                proxies: Mutex::default(),
                events: Mutex::default(),
                adopter: Mutex::default(),
                ended: Mutex::default(),
                adopted: AtomicBool::new(false),
                limit: AtomicUsize::new(0),
                stall_ms: AtomicU64::new(0),
                leaving: AtomicBool::new(false),
                trouble: Mutex::default(),
            }),
        }
    }

    pub fn config(&self) -> &HostConfig {
        &self.inner.config
    }

    /// Where session events go in this app (the Tauri events, the incident
    /// record). Set from every spawn and once at startup.
    pub(crate) fn set_events(&self, events: Arc<dyn SessionEvents>) {
        *lock(&self.inner.events) = Some(events);
    }

    /// How a task run whose observer died with an earlier app is observed.
    pub(crate) fn set_adopter(&self, adopter: Adopter) {
        *lock(&self.inner.adopter) = Some(adopter);
    }

    /// What is wrong with the host, if anything: version skew, a host that
    /// would not start. Empty when nothing is known to be wrong.
    pub fn trouble(&self) -> Vec<String> {
        lock(&self.inner.trouble).clone()
    }

    /// Connects (starting the host if nothing answers), adopts what it holds
    /// and records what ended while this app was away.
    pub fn connect(&self) -> Result<(), String> {
        self.inner.ensure_connected().map(|_| ())
    }

    /// The app is quitting and leaving every session running: the connection
    /// closes, the host detaches every view and keeps every process.
    pub fn disconnect(&self) {
        self.inner.leaving.store(true, Ordering::Release);
        if let Some(conn) = lock(&self.inner.conn).take() {
            conn.close();
        }
    }

    fn conn(&self) -> Result<Arc<Connection>, String> {
        self.inner.ensure_connected()
    }

    fn call<T: DeserializeOwned>(&self, request: Request, within: Duration) -> Result<T, String> {
        let value = self.conn()?.call(request, &[], within)?;
        serde_json::from_value(value)
            .map_err(|e| format!("The terminal host answered in a form this app cannot read: {e}"))
    }

    pub(crate) fn spawn(
        &self,
        events: Arc<dyn SessionEvents>,
        request: SpawnRequest,
        output: OutputSink,
        observer: Option<Arc<dyn SessionObserver>>,
    ) -> Result<super::super::Started, String> {
        self.set_events(events);
        let conn = self.conn()?;
        let session = request.session_id.clone();
        // The run's claim names the session before the process exists, as it
        // does in-process.
        if let Some(observer) = &observer {
            observer.before_spawn(&session)?;
        }
        // Before the request: output, and even the exit of a program that
        // ends at once, can arrive before the reply.
        lock(&self.inner.proxies).insert(
            session.clone(),
            Proxy::new(output, observer.clone(), request.run_id.clone(), true, true),
        );
        let reply = conn
            .call(Request::Spawn { request }, &[], SPAWN_DEADLINE)
            .and_then(|value| {
                serde_json::from_value::<SpawnReply>(value).map_err(|e| {
                    format!("The terminal host answered in a form this app cannot read: {e}")
                })
            });
        let reply = match reply {
            Ok(reply) => reply,
            Err(error) => {
                let proxy = lock(&self.inner.proxies).remove(&session);
                if let Some(observer) = &observer {
                    observer.spawn_failed(&session, &error);
                }
                // A process that started after all (the answer was lost) still
                // reports its exit; with no proxy it is filed by its run.
                if let Some(ended) = proxy.and_then(|p| p.deferred_exit) {
                    self.inner.record_exit(ended);
                }
                return Err(error);
            }
        };
        let process = match (reply.process, reply.process_error) {
            (Some(process), _) => Ok(process),
            (None, error) => Err(error.unwrap_or_else(|| "PTY did not report a process ID".into())),
        };
        let start_error = observer
            .as_ref()
            .and_then(|observer| observer.started(&session, process.clone()).err());
        // Started: an exit that overtook the reply is recorded now.
        let deferred = {
            let mut proxies = lock(&self.inner.proxies);
            proxies.get_mut(&session).and_then(|proxy| {
                proxy.starting = false;
                proxy.deferred_exit.take()
            })
        };
        if let Some(ended) = deferred {
            self.inner.record_exit(ended);
        } else if let Some(error) = start_error.as_ref() {
            if let Err(close) = self.kill(&session) {
                log::error!(target: "terminal", "could not stop a task session after bookkeeping failure: {close}");
            }
            log::warn!(target: "terminal", "a task session was stopped: its start could not be recorded: {error}");
        }
        if let Some(error) = start_error {
            return Err(format!("Task process bookkeeping failed; the child was asked to stop. Inspect the run before another attempt: {error}"));
        }
        Ok(super::super::Started {
            spawned: reply.spawned,
            process,
        })
    }

    /// Types `bytes` into `session`, answered once the host's lane has
    /// delivered them. Called on the session's input lane.
    pub fn write_blocking(&self, session: &str, bytes: &[u8]) -> Result<(), String> {
        self.conn()?
            .call(
                Request::Write {
                    session: session.to_owned(),
                },
                bytes,
                CALL_DEADLINE,
            )
            .map(|_| ())
    }

    /// Resizes `session`'s PTY. Called on the session's geometry lane.
    pub fn resize_blocking(&self, session: &str, rows: u16, cols: u16) -> Result<(), String> {
        self.call::<serde_json::Value>(
            Request::Resize {
                session: session.to_owned(),
                rows,
                cols,
            },
            CALL_DEADLINE,
        )
        .map(|_| ())
    }

    /// Repays credit for output the view has painted, and says when the
    /// window refused it.
    pub fn ack(&self, session: &str, bytes: usize) -> Result<(), String> {
        // An acknowledgement after the connection went is for a window the
        // host has already reset when it detached the session; there is
        // nothing left to repay.
        let Some(conn) = self.inner.current() else {
            return Ok(());
        };
        conn.call(
            Request::Ack {
                session: session.to_owned(),
                bytes,
            },
            &[],
            CALL_DEADLINE,
        )
        .map(|_| ())
    }

    /// The lane that carries `session`'s keystrokes.
    pub(crate) fn input_lane(&self, session: &str) -> Result<Executor, String> {
        lock(&self.inner.proxies)
            .get(session)
            .map(|p| p.input.clone())
            .ok_or_else(|| format!("Terminal session '{session}' not found"))
    }

    /// The lane that carries `session`'s resizes.
    pub(crate) fn geometry_lane(&self, session: &str) -> Result<Executor, String> {
        lock(&self.inner.proxies)
            .get(session)
            .map(|p| p.geometry.clone())
            .ok_or_else(|| format!("Terminal session '{session}' not found"))
    }

    /// Threads alive on this app's lanes for the sessions it knows.
    pub fn lane_threads(&self) -> usize {
        lock(&self.inner.proxies)
            .values()
            .map(|p| p.input.workers() + p.geometry.workers())
            .sum()
    }

    /// The host process's id, once connected. For tests that measure what
    /// the process holding the PTYs gives back.
    pub fn host_pid(&self) -> Option<u32> {
        self.inner
            .current()
            .map(|conn| conn.host_pid.load(Ordering::Acquire))
            .filter(|pid| *pid > 0)
    }

    pub fn list(&self) -> Result<Vec<TerminalListing>, String> {
        self.call(Request::List, CALL_DEADLINE)
    }

    pub fn context(&self, session: &str) -> Result<TerminalContext, String> {
        self.call(
            Request::Context {
                session: session.to_owned(),
            },
            CALL_DEADLINE,
        )
    }

    pub fn detach(&self, session: &str) -> Result<bool, String> {
        if let Some(proxy) = lock(&self.inner.proxies).get_mut(session) {
            proxy.viewed = false;
        }
        self.call(
            Request::Detach {
                session: session.to_owned(),
            },
            CALL_DEADLINE,
        )
    }

    pub fn detach_all(&self) -> Result<usize, String> {
        for proxy in lock(&self.inner.proxies).values_mut() {
            proxy.viewed = false;
        }
        self.call(Request::DetachAll, CALL_DEADLINE)
    }

    /// Hands a detached session to `output`. The ring's replay is the first
    /// thing the host sends it, so the slot is switched before the request.
    pub fn attach(
        &self,
        session: &str,
        rows: u16,
        cols: u16,
        output: OutputSink,
    ) -> Result<TerminalSpawned, String> {
        let conn = self.conn()?;
        let slot = {
            let mut proxies = lock(&self.inner.proxies);
            let proxy = proxies
                .get_mut(session)
                .ok_or_else(|| format!("Terminal session '{session}' has ended"))?;
            if proxy.viewed {
                return Err("This terminal is already shown in a tab.".into());
            }
            proxy.viewed = true;
            proxy.output.clone()
        };
        let previous = slot.current();
        slot.replace(output);
        let attached = conn
            .call_in_order(
                Request::Attach {
                    session: session.to_owned(),
                    rows,
                    cols,
                },
                &[],
                CALL_DEADLINE,
            )
            .and_then(|value| serde_json::from_value(value).map_err(|e| e.to_string()));
        if attached.is_err() {
            slot.replace(previous);
            if let Some(proxy) = lock(&self.inner.proxies).get_mut(session) {
                proxy.viewed = false;
            }
        }
        attached
    }

    pub fn attach_tracked(
        &self,
        run_id: &str,
        rows: u16,
        cols: u16,
        output: OutputSink,
    ) -> Option<TerminalSpawned> {
        let conn = self.conn().ok()?;
        let (session, slot) = {
            let mut proxies = lock(&self.inner.proxies);
            let (id, proxy) = proxies
                .iter_mut()
                .find(|(_, p)| p.run_id.as_deref() == Some(run_id) && !p.starting)?;
            proxy.viewed = true;
            (id.clone(), proxy.output.clone())
        };
        let previous = slot.current();
        slot.replace(output);
        let attached = conn
            .call_in_order(
                Request::AttachTracked {
                    run_id: run_id.to_owned(),
                    rows,
                    cols,
                },
                &[],
                CALL_DEADLINE,
            )
            .and_then(|value| {
                serde_json::from_value::<Option<TerminalSpawned>>(value).map_err(|e| e.to_string())
            });
        match attached {
            Ok(Some(spawned)) if spawned.id == session => Some(spawned),
            Ok(other) => {
                slot.replace(previous);
                other
            }
            Err(error) => {
                slot.replace(previous);
                log::warn!(target: "terminal", "a task terminal could not be taken over: {error}");
                None
            }
        }
    }

    /// Closes a session and waits until its exit has been recorded here, as
    /// the in-process close waits for its reader.
    pub fn kill(&self, session: &str) -> Result<(), String> {
        self.call::<serde_json::Value>(
            Request::Kill {
                session: session.to_owned(),
            },
            KILL_DEADLINE,
        )?;
        self.inner
            .wait_until(EXIT_RECORD_DEADLINE, |proxies| {
                !proxies.contains_key(session)
            })
            .then_some(())
            .ok_or_else(|| "Terminal process has not finished cleanup; retry Close".into())
    }

    /// Stops every session the host holds and waits for their exits.
    pub fn stop_all(&self) -> Result<(), String> {
        self.call::<serde_json::Value>(Request::StopAll, STOP_ALL_DEADLINE)?;
        if self
            .inner
            .wait_until(EXIT_RECORD_DEADLINE, HashMap::is_empty)
        {
            Ok(())
        } else {
            let left = lock(&self.inner.proxies).len();
            Err(format!(
                "{left} terminal processes have not finished cleanup"
            ))
        }
    }

    /// Whether a session of `run_id` is held. Answered "held" until the host
    /// has been asked: a run is never released on a list that was not read.
    pub fn tracks_run(&self, run_id: &str) -> bool {
        if !self.inner.adopted.load(Ordering::Acquire) {
            return true;
        }
        lock(&self.inner.proxies)
            .values()
            .any(|p| p.run_id.as_deref() == Some(run_id))
    }

    /// Whether this app holds a proxy for `session`, which is every session
    /// the host held when it was last listed, plus any this app is starting
    /// (a proxy exists before the spawn's reply) and any ending (it goes at
    /// `record_exit`). `None` until the host has been asked: whether it holds
    /// the session is not known then. Answered from the snapshot alone, with
    /// no request to the host, no reconnect and no host start, so it never
    /// waits on a host that is slow or gone; the snapshot lags only by the
    /// host's `Exit` frames, and, while the connection is down, by what ended
    /// meanwhile until the next `adopt` reconciles it.
    pub fn holds_session(&self, session: &str) -> Option<bool> {
        if !self.inner.adopted.load(Ordering::Acquire) {
            return None;
        }
        Some(lock(&self.inner.proxies).contains_key(session))
    }

    pub fn set_background(&self, sessions: &[String]) {
        if let Some(conn) = self.inner.current() {
            if let Err(error) = conn.send(
                Request::Background {
                    sessions: sessions.to_vec(),
                },
                &[],
                None,
            ) {
                log::warn!(target: "terminal", "hidden tabs were not reported to the terminal host: {error}");
            }
        }
    }

    pub fn set_limit(&self, limit: usize) {
        self.inner.limit.store(limit, Ordering::Release);
        if let Some(conn) = self.inner.current() {
            let _ = conn.send(Request::Limit { limit }, &[], None);
        }
    }

    pub fn set_stall(&self, stall: Duration) {
        let ms = stall.as_millis() as u64;
        self.inner.stall_ms.store(ms, Ordering::Release);
        if let Some(conn) = self.inner.current() {
            let _ = conn.send(Request::Stall { ms }, &[], None);
        }
    }

    pub fn wakeups(&self, session: &str) -> Result<Option<u64>, String> {
        self.call(
            Request::Wakeups {
                session: session.to_owned(),
            },
            CALL_DEADLINE,
        )
    }

    pub fn size(&self, session: &str) -> Result<(u16, u16), String> {
        self.call(
            Request::Size {
                session: session.to_owned(),
            },
            CALL_DEADLINE,
        )
    }

    pub fn hold(&self, session: &str) -> Result<MasterHold, String> {
        self.call::<serde_json::Value>(
            Request::Hold {
                session: session.to_owned(),
            },
            CALL_DEADLINE,
        )?;
        Ok(MasterHold::remote(self.clone(), session.to_owned()))
    }

    pub(crate) fn release(&self, session: &str) {
        if let Err(error) = self.call::<serde_json::Value>(
            Request::Release {
                session: session.to_owned(),
            },
            CALL_DEADLINE,
        ) {
            log::warn!(target: "terminal", "a held terminal master was not released: {error}");
        }
    }

    /// Sessions this app holds a proxy for.
    pub fn known_sessions(&self) -> usize {
        lock(&self.inner.proxies).len()
    }
}

/// The last handle is gone: the connection closes, so the host detaches every
/// view and, holding no session and no app, can exit.
impl Drop for Inner {
    fn drop(&mut self) {
        if let Some(conn) = lock(&self.conn).take() {
            conn.close();
        }
    }
}

impl Inner {
    fn current(&self) -> Option<Arc<Connection>> {
        lock(&self.conn).clone().filter(|c| !c.is_closed())
    }

    fn events(&self) -> Option<Arc<dyn SessionEvents>> {
        lock(&self.events).clone()
    }

    fn adopt_observer(&self, run_id: &str) -> Option<Arc<dyn SessionObserver>> {
        let adopter = lock(&self.adopter).clone()?;
        adopter(run_id)
    }

    fn note_trouble(&self, trouble: String) {
        log::warn!(target: "terminal", "{trouble}");
        let mut list = lock(&self.trouble);
        if !list.contains(&trouble) {
            list.push(trouble);
            if list.len() > 8 {
                list.remove(0);
            }
        }
    }

    /// Waits until `done` holds of the proxies, or `within` passes.
    fn wait_until(&self, within: Duration, done: impl Fn(&HashMap<String, Proxy>) -> bool) -> bool {
        let deadline = Instant::now() + within;
        loop {
            if done(&lock(&self.proxies)) {
                return true;
            }
            if Instant::now() >= deadline {
                return false;
            }
            std::thread::sleep(Duration::from_millis(10));
        }
    }

    fn ensure_connected(self: &Arc<Self>) -> Result<Arc<Connection>, String> {
        if let Some(conn) = self.current() {
            return Ok(conn);
        }
        if self.leaving.load(Ordering::Acquire) {
            return Err("GitPulse is quitting and has let the terminal host go.".into());
        }
        let _connecting = lock(&self.connecting);
        if let Some(conn) = self.current() {
            return Ok(conn);
        }
        let socket = self.config.dir.join(super::socket_name(PROTOCOL));
        let stream = match UnixStream::connect(&socket) {
            Ok(stream) => stream,
            Err(error)
                if matches!(
                    error.kind(),
                    std::io::ErrorKind::NotFound | std::io::ErrorKind::ConnectionRefused
                ) =>
            {
                self.start_host()?;
                self.wait_for_socket(&socket)?
            }
            Err(error) => {
                let trouble = format!(
                    "The terminal host at {} could not be reached: {error}",
                    socket.display()
                );
                self.note_trouble(trouble.clone());
                return Err(trouble);
            }
        };
        let conn = self.open(stream)?;
        *lock(&self.conn) = Some(conn.clone());
        let limit = self.limit.load(Ordering::Acquire);
        if limit > 0 {
            let _ = conn.send(Request::Limit { limit }, &[], None);
        }
        let ms = self.stall_ms.load(Ordering::Acquire);
        if ms > 0 {
            let _ = conn.send(Request::Stall { ms }, &[], None);
        }
        if let Err(error) = self.adopt(&conn) {
            let trouble = format!("The terminal host's sessions could not be listed: {error}");
            self.note_trouble(trouble.clone());
            return Err(trouble);
        }
        self.probe_other_protocols();
        Ok(conn)
    }

    /// Opens a connection on `stream`: its reader, its deadline sweeper, and
    /// the introduction.
    fn open(self: &Arc<Self>, stream: UnixStream) -> Result<Arc<Connection>, String> {
        stream
            .set_write_timeout(Some(super::server::WRITE_DEADLINE))
            .map_err(|e| e.to_string())?;
        let conn = Arc::new(Connection {
            writer: Mutex::new(stream.try_clone().map_err(|e| e.to_string())?),
            control: stream.try_clone().map_err(|e| e.to_string())?,
            next_id: AtomicU64::new(1),
            pending: Mutex::default(),
            closed: AtomicBool::new(false),
            host_pid: std::sync::atomic::AtomicU32::new(0),
            queued: AtomicU64::new(0),
            delivered: Mutex::new(0),
            caught_up: std::sync::Condvar::new(),
            delivery: std::sync::OnceLock::new(),
        });
        let reader = {
            let (inner, conn) = (Arc::downgrade(self), conn.clone());
            crate::supervisor::spawn_one_of_many(
                "terminal-host-reader",
                "terminal host reader",
                move || read_loop(inner, conn, BufReader::new(stream)),
            )
        };
        if let Err(error) = reader {
            conn.close();
            return Err(format!(
                "The terminal host connection could not start: {error}"
            ));
        }
        {
            let sweeping = conn.clone();
            let swept = crate::supervisor::spawn_one_of_many(
                "terminal-host-sweep",
                "terminal host deadlines",
                move || {
                    while sweeping.sweep() {
                        std::thread::sleep(SWEEP_EVERY);
                    }
                },
            );
            if let Err(error) = swept {
                conn.close();
                return Err(format!(
                    "The terminal host connection could not start: {error}"
                ));
            }
        }
        let hello = conn
            .call(
                Request::Hello {
                    protocol: PROTOCOL,
                    build: crate::updates::CURRENT_VERSION.to_owned(),
                },
                &[],
                CALL_DEADLINE,
            )
            .and_then(|value| serde_json::from_value::<Hello>(value).map_err(|e| e.to_string()));
        let hello = match hello {
            Ok(hello) => hello,
            Err(error) => {
                conn.close();
                let trouble = format!("The terminal host refused this app: {error}");
                self.note_trouble(trouble.clone());
                return Err(trouble);
            }
        };
        if hello.protocol != PROTOCOL {
            conn.close();
            let trouble = format!(
                "The terminal host (pid {}, build {}) speaks protocol {}, and this app speaks {PROTOCOL}; it was not used.",
                hello.pid, hello.build, hello.protocol
            );
            self.note_trouble(trouble.clone());
            return Err(trouble);
        }
        conn.host_pid.store(hello.pid, Ordering::Release);
        if hello.build != crate::updates::CURRENT_VERSION {
            log::info!(target: "terminal", "terminal host build {} (pid {}) serves app build {}", hello.build, hello.pid, crate::updates::CURRENT_VERSION);
        }
        Ok(conn)
    }

    /// Every session the host holds gets a proxy; every exit recorded while
    /// this app was away is recorded now; a proxy whose session the host no
    /// longer holds, and that no receipt accounts for, is ended.
    fn adopt(self: &Arc<Self>, conn: &Arc<Connection>) -> Result<(), String> {
        let listed: Vec<TerminalListing> =
            serde_json::from_value(conn.call(Request::List, &[], CALL_DEADLINE)?)
                .map_err(|e| e.to_string())?;
        let reattach: Vec<String> = {
            let mut proxies = lock(&self.proxies);
            let mut reattach = Vec::new();
            for listing in &listed {
                match proxies.get_mut(&listing.id) {
                    // A view here still shows it: the connection was lost and
                    // the host detached it. It is taken over again below.
                    Some(proxy) if proxy.viewed && listing.detached && !listing.ending => {
                        proxy.viewed = false;
                        reattach.push(listing.id.clone());
                    }
                    Some(_) => {}
                    None => {
                        let observer = listing
                            .run_id
                            .as_deref()
                            .and_then(|run| self.adopt_observer(run));
                        proxies.insert(
                            listing.id.clone(),
                            Proxy::new(nowhere(), observer, listing.run_id.clone(), false, false),
                        );
                    }
                }
            }
            reattach
        };
        self.adopted.store(true, Ordering::Release);
        let receipts: Vec<WireEnded> =
            serde_json::from_value(conn.call(Request::Receipts, &[], CALL_DEADLINE)?)
                .map_err(|e| e.to_string())?;
        if !receipts.is_empty() {
            log::info!(target: "terminal", "{} terminal session(s) ended while this app was not connected; recording them", receipts.len());
        }
        for ended in receipts {
            self.record_exit(ended);
        }
        let gone: Vec<String> = lock(&self.proxies)
            .iter()
            .filter(|(id, p)| !p.starting && !listed.iter().any(|l| &l.id == *id))
            .map(|(id, _)| id.clone())
            .collect();
        for id in gone {
            let run_id = lock(&self.proxies).get(&id).and_then(|p| p.run_id.clone());
            self.record_exit(WireEnded {
                payload: crate::terminal::TerminalExitPayload {
                    id,
                    exit_code: None,
                    signal: String::new(),
                    error: Some(super::receipts::HOST_ENDED.into()),
                    reaped: false,
                    ended_by_gitpulse: false,
                },
                ended_by_gitpulse: false,
                program: String::new(),
                cwd: String::new(),
                run_id,
            });
        }
        for id in reattach {
            self.reattach(conn, &id);
        }
        Ok(())
    }

    /// Shows a session again in the view that showed it before the connection
    /// was lost: the host replays its ring into that view.
    fn reattach(&self, conn: &Arc<Connection>, session: &str) {
        let size = conn
            .call(
                Request::Size {
                    session: session.to_owned(),
                },
                &[],
                CALL_DEADLINE,
            )
            .and_then(|v| serde_json::from_value::<(u16, u16)>(v).map_err(|e| e.to_string()));
        let (rows, cols) = match size {
            Ok(size) => size,
            Err(error) => {
                log::warn!(target: "terminal", "a terminal could not be shown again after the host reconnected: {error}");
                return;
            }
        };
        if let Some(proxy) = lock(&self.proxies).get_mut(session) {
            proxy.viewed = true;
        }
        if let Some(events) = self.events() {
            events.loss(TerminalLossPayload {
                id: session.to_owned(),
                message: "The connection to the terminal host was lost and restored; the screen was redrawn from what it kept.".into(),
            });
        }
        if let Err(error) = conn.call(
            Request::Attach {
                session: session.to_owned(),
                rows,
                cols,
            },
            &[],
            CALL_DEADLINE,
        ) {
            log::warn!(target: "terminal", "a terminal could not be shown again after the host reconnected: {error}");
            if let Some(proxy) = lock(&self.proxies).get_mut(session) {
                proxy.viewed = false;
            }
        }
    }

    /// Records one session's exit: its run's receipt, the incident record and
    /// `terminal-exit`, then lets the host forget it. Called once per session;
    /// a second report of the same exit only clears the host's copy.
    fn record_exit(&self, ended: WireEnded) {
        let id = ended.payload.id.clone();
        {
            let mut proxies = lock(&self.proxies);
            if let Some(proxy) = proxies.get_mut(&id).filter(|p| p.starting) {
                proxy.deferred_exit = Some(ended);
                return;
            }
        }
        let already = lock(&self.ended).contains(&id);
        if !already {
            let (observer, slot) = {
                let proxies = lock(&self.proxies);
                let proxy = proxies.get(&id);
                (
                    proxy.and_then(|p| p.observer.clone()),
                    proxy.filter(|p| !p.end_sent).map(|p| p.output.clone()),
                )
            };
            let observer = observer.or_else(|| {
                ended
                    .run_id
                    .as_deref()
                    .and_then(|run| self.adopt_observer(run))
            });
            // A panic in the bookkeeping must not keep the exit from being
            // announced or the proxy from going.
            if let Some(observer) = observer {
                if let Err(panic) =
                    catch_unwind(AssertUnwindSafe(|| observer.finished(&ended.payload)))
                {
                    crate::supervisor::note_stopped("terminal observer panic", panic.as_ref());
                }
            }
            {
                let mut remembered = lock(&self.ended);
                remembered.push_back(id.clone());
                while remembered.len() > REMEMBERED_EXITS {
                    remembered.pop_front();
                }
            }
            lock(&self.proxies).remove(&id);
            // The host sent its own end marker if the session's view was
            // live; a session ended while the connection was down never did.
            if let Some(slot) = slot {
                let _ = slot.current().end(&id);
            }
            log::info!(
                target: "terminal",
                "terminal session ended in the host: exit_code={:?} signal={:?} reaped={} error={:?}",
                ended.payload.exit_code, ended.payload.signal, ended.payload.reaped, ended.payload.error
            );
            match self.events() {
                Some(events) => events.exit(SessionEnded {
                    payload: ended.payload,
                    ended_by_gitpulse: ended.ended_by_gitpulse,
                    program: ended.program,
                    cwd: ended.cwd,
                }),
                None => {
                    log::warn!(target: "terminal", "a terminal exit had nowhere to be announced")
                }
            }
        } else {
            lock(&self.proxies).remove(&id);
        }
        if let Some(conn) = self.current() {
            if let Err(error) = conn.send(Request::Forget { session: id }, &[], None) {
                log::warn!(target: "terminal", "a recorded exit stays in the host for the next connect: {error}");
            }
        }
    }

    fn output(&self, conn: &Connection, session: &str, bytes: &[u8]) {
        let target = lock(&self.proxies)
            .get(session)
            .map(|p| (p.output.clone(), p.failing.clone()));
        let delivered = match &target {
            Some((slot, failing)) => match slot.current().bytes(session, bytes) {
                Ok(()) => {
                    failing.store(false, Ordering::Release);
                    true
                }
                Err(error) => {
                    if !failing.swap(true, Ordering::AcqRel) {
                        log::warn!(target: "terminal", "terminal output could not be sent to its view, skipping output: {error}");
                        if let Some(events) = self.events() {
                            events.loss(TerminalLossPayload {
                                id: session.to_owned(),
                                message: SHED_NOTICE.into(),
                            });
                        }
                    }
                    false
                }
            },
            None => false,
        };
        // Output no view here will acknowledge is repaid at once, so the
        // host's reader does not wait out a stall for a view that is not
        // there.
        if !delivered {
            let _ = conn.send(
                Request::Ack {
                    session: session.to_owned(),
                    bytes: bytes.len(),
                },
                &[],
                None,
            );
        }
    }

    /// One frame a session sent, on the delivery thread, in the order sent.
    fn deliver(self: &Arc<Self>, conn: &Connection, frame: HostFrame, payload: &[u8]) {
        match frame {
            HostFrame::Output { session } => self.output(conn, &session, payload),
            HostFrame::End { session } => self.end(&session),
            HostFrame::Loss { loss } => {
                if let Some(events) = self.events() {
                    events.loss(loss);
                }
            }
            HostFrame::Notice { notice } => match notice.into_notice() {
                Ok(notice) => crate::alerts::offer(notice),
                Err(error) => log::warn!(target: "terminal", "{error}"),
            },
            HostFrame::ScanDrops { dropped } => crate::alerts::record_scan_drops(dropped),
            // Off this thread too: a run's receipt is a store write, and the
            // output of every other session waits behind delivery.
            HostFrame::Exit { ended } => {
                let inner = self.clone();
                if let Err(error) = crate::supervisor::spawn_one_of_many(
                    "terminal-host-exit",
                    "terminal host exit",
                    move || inner.record_exit(ended),
                ) {
                    log::error!(target: "terminal", "a terminal exit could not be recorded now; the host keeps it for the next connect: {error}");
                }
            }
            HostFrame::Reply { id, .. } => {
                log::warn!(target: "terminal", "reply {id} reached delivery; it is answered on the reader");
            }
        }
    }

    fn end(&self, session: &str) {
        let slot = lock(&self.proxies).get_mut(session).map(|p| {
            p.end_sent = true;
            p.output.clone()
        });
        if let Some(slot) = slot {
            if let Err(error) = slot.current().end(session) {
                log::warn!(target: "terminal", "could not mark the end of a terminal's output: {error}");
            }
        }
    }

    /// The connection ended. Calls in flight fail; the sessions are the
    /// host's, and are found again by reconnecting, unless the app is leaving.
    fn lost(self: &Arc<Self>, conn: &Arc<Connection>) {
        conn.close();
        {
            let mut current = lock(&self.conn);
            if current.as_ref().is_some_and(|c| Arc::ptr_eq(c, conn)) {
                *current = None;
            } else {
                return;
            }
        }
        if self.leaving.load(Ordering::Acquire) {
            return;
        }
        log::warn!(target: "terminal", "the connection to the terminal host was lost; reconnecting");
        let inner = self.clone();
        let spawned = crate::supervisor::spawn_one_of_many(
            "terminal-host-reconnect",
            "terminal host reconnect",
            move || {
                for attempt in 0..3 {
                    if attempt > 0 {
                        std::thread::sleep(Duration::from_secs(1));
                    }
                    match inner.ensure_connected() {
                        Ok(_) => return,
                        Err(error) => {
                            log::warn!(target: "terminal", "terminal host reconnect failed: {error}")
                        }
                    }
                }
            },
        );
        if let Err(error) = spawned {
            log::warn!(target: "terminal", "terminal host reconnect did not start: {error}");
        }
    }

    /// Starts `gitpulse-ptyd` in a session of its own, so nothing aimed at
    /// this app's process group or session reaches it, and nothing this app's
    /// quit sweeps (`procguard::reap_all`) tracks it.
    fn start_host(&self) -> Result<(), String> {
        let binary = match &self.config.binary {
            Some(binary) => binary.clone(),
            None => crate::insights::bundled_binary(super::BINARY_ENV, super::BINARY).map_err(
                |error| {
                    let trouble = format!("The terminal host could not start: {error}");
                    self.note_trouble(trouble.clone());
                    trouble
                },
            )?,
        };
        crate::local_socket::private_dir(&self.config.dir)?;
        let log_path = self.config.dir.join("ptyd.log");
        let log = {
            use std::os::unix::fs::OpenOptionsExt;
            std::fs::OpenOptions::new()
                .create(true)
                .write(true)
                .truncate(true)
                .mode(0o600)
                .custom_flags(libc::O_NOFOLLOW)
                .open(&log_path)
                .map_err(|e| format!("{}: {e}", log_path.display()))?
        };
        let mut command = std::process::Command::new(&binary);
        command.arg("--dir").arg(&self.config.dir);
        if let Some(dir) = &self.config.log_dir {
            command.env(crate::logging::LOG_DIR_ENV, dir);
        }
        if let Some(idle) = self.config.idle_exit {
            command
                .arg("--idle-exit-ms")
                .arg(idle.as_millis().to_string());
        }
        command
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(log);
        {
            use std::os::unix::process::CommandExt;
            // SAFETY: `setsid` is async-signal-safe and touches nothing but
            // the child's own session, between fork and exec.
            unsafe {
                command.pre_exec(|| {
                    if libc::setsid() == -1 {
                        return Err(std::io::Error::last_os_error());
                    }
                    Ok(())
                });
            }
        }
        // Under the inheritance lock, and not registered with procguard: its
        // quit sweep must never reach the host.
        let mut child = crate::procguard::LockedSpawn::spawn_locked(&mut command).map_err(|e| {
            format!(
                "The terminal host ({}) could not start: {e}",
                binary.display()
            )
        })?;
        log::info!(target: "terminal", "started the terminal host (pid {})", child.id());
        // Reaped when it ends, so a host that exits while this app runs is not
        // left a zombie. The app never signals it.
        let waited = crate::supervisor::spawn_one_of_many(
            "terminal-host-wait",
            "terminal host wait",
            move || match child.wait() {
                Ok(status) => log::info!(target: "terminal", "the terminal host exited: {status}"),
                Err(error) => {
                    log::warn!(target: "terminal", "could not wait for the terminal host: {error}")
                }
            },
        );
        if let Err(error) = waited {
            log::warn!(target: "terminal", "the terminal host will not be reaped by this app: {error}");
        }
        Ok(())
    }

    fn wait_for_socket(&self, socket: &std::path::Path) -> Result<UnixStream, String> {
        let deadline = Instant::now() + START_DEADLINE;
        loop {
            match UnixStream::connect(socket) {
                Ok(stream) => return Ok(stream),
                Err(error) if Instant::now() >= deadline => {
                    let log = std::fs::read_to_string(self.config.dir.join("ptyd.log"))
                        .unwrap_or_default();
                    let said: String = log.lines().rev().take(3).collect::<Vec<_>>().join(" / ");
                    let trouble = format!(
                        "The terminal host did not start listening within {}s ({error}){}",
                        START_DEADLINE.as_secs(),
                        if said.is_empty() {
                            String::new()
                        } else {
                            format!(": {said}")
                        }
                    );
                    self.note_trouble(trouble.clone());
                    return Err(trouble);
                }
                Err(_) => std::thread::sleep(Duration::from_millis(25)),
            }
        }
    }

    /// Hosts of other protocol versions in the host directory: one with no
    /// sessions is asked to go; one with sessions is left running and reported,
    /// because this version cannot show them and must not pretend there are
    /// none.
    fn probe_other_protocols(&self) {
        let Ok(entries) = std::fs::read_dir(&self.config.dir) else {
            return;
        };
        for entry in entries.flatten().take(64) {
            let name = entry.file_name();
            let Some(protocol) = name.to_str().and_then(super::socket_protocol) else {
                continue;
            };
            if protocol == PROTOCOL {
                continue;
            }
            let path = entry.path();
            match probe(
                &path,
                Request::Hello {
                    protocol: PROTOCOL,
                    build: crate::updates::CURRENT_VERSION.to_owned(),
                },
            )
            .and_then(|value| serde_json::from_value::<Hello>(value).map_err(|e| e.to_string()))
            {
                Ok(hello) if hello.sessions == 0 => {
                    match probe(&path, Request::Retire).and_then(|value| {
                        serde_json::from_value::<Retired>(value).map_err(|e| e.to_string())
                    }) {
                        Ok(Retired { retired: true, .. }) => {
                            log::info!(target: "terminal", "an idle terminal host of protocol {protocol} (build {}) was asked to exit", hello.build)
                        }
                        Ok(Retired { sessions, .. }) => {
                            self.note_trouble(skew(protocol, &hello.build, hello.pid, sessions))
                        }
                        Err(error) => {
                            log::warn!(target: "terminal", "an idle terminal host of protocol {protocol} did not retire: {error}")
                        }
                    }
                }
                Ok(hello) => {
                    self.note_trouble(skew(protocol, &hello.build, hello.pid, hello.sessions))
                }
                // Nothing listening: a socket a dead host left. Not ours to
                // delete; its own next start replaces it.
                Err(error) => {
                    log::debug!(target: "terminal", "no terminal host answers at {}: {error}", path.display())
                }
            }
        }
    }
}

fn skew(protocol: u32, build: &str, pid: u32, sessions: usize) -> String {
    format!(
        "A terminal host from another GitPulse version (protocol {protocol}, build {build}, pid {pid}) is still running {sessions} session(s). This version speaks protocol {PROTOCOL} and cannot show them; they keep running until they end or that host is stopped."
    )
}

/// One request on a connection of its own, for a host this app will not use.
fn probe(path: &std::path::Path, request: Request) -> Result<serde_json::Value, String> {
    let mut stream = UnixStream::connect(path).map_err(|e| e.to_string())?;
    stream
        .set_read_timeout(Some(PROBE_DEADLINE))
        .map_err(|e| e.to_string())?;
    stream
        .set_write_timeout(Some(PROBE_DEADLINE))
        .map_err(|e| e.to_string())?;
    proto::write_frame(&mut stream, &Envelope { id: 1, request }, &[])
        .map_err(|e| e.to_string())?;
    let (header, _) = proto::read_frame(&mut stream)
        .map_err(|e| e.to_string())?
        .ok_or("the host closed the connection without answering")?;
    // Read loosely: a host of another protocol frames replies its own way
    // beyond the frozen fields.
    let reply: serde_json::Value = proto::decode(&header).map_err(|e| e.to_string())?;
    if let Some(error) = reply.get("error").and_then(|e| e.as_str()) {
        return Err(error.to_owned());
    }
    reply
        .get("ok")
        .cloned()
        .ok_or_else(|| "the host's answer carried nothing".into())
}

/// Reads the host's frames. Replies are resolved here and nothing else is
/// done here: everything a session sends — output, its end, a loss, a notice,
/// its exit — goes, in order, to the connection's delivery thread. A listener
/// that answers output with a call to the host (an acknowledgement, a write)
/// then waits on a reply this thread is free to read; done here, it would wait
/// on itself.
fn read_loop(inner: Weak<Inner>, conn: Arc<Connection>, mut reader: BufReader<UnixStream>) {
    let (deliver, deliveries) = mpsc::channel::<(HostFrame, Vec<u8>)>();
    let delivering = {
        let (inner, conn) = (inner.clone(), conn.clone());
        crate::supervisor::spawn_one_of_many(
            "terminal-host-delivery",
            "terminal host delivery",
            move || {
                let _ = conn.delivery.set(std::thread::current().id());
                for (frame, payload) in deliveries {
                    let Some(inner) = inner.upgrade() else {
                        break;
                    };
                    inner.deliver(&conn, frame, &payload);
                    conn.mark_delivered();
                }
            },
        )
    };
    if let Err(error) = delivering {
        log::error!(target: "terminal", "the terminal host connection could not deliver output: {error}");
        if let Some(inner) = inner.upgrade() {
            inner.lost(&conn);
        } else {
            conn.close();
        }
        return;
    }
    loop {
        let frame = match proto::read_frame(&mut reader) {
            Ok(Some(frame)) => frame,
            Ok(None) => break,
            Err(error) => {
                if !conn.is_closed() {
                    log::warn!(target: "terminal", "the terminal host connection failed: {error}");
                }
                break;
            }
        };
        let (header, payload) = frame;
        match proto::decode::<HostFrame>(&header) {
            Ok(HostFrame::Reply { id, ok, error }) => conn.resolve(
                id,
                match error {
                    Some(error) => Err(error),
                    None => Ok(ok.unwrap_or(serde_json::Value::Null)),
                },
            ),
            Ok(frame) => {
                conn.queued.fetch_add(1, Ordering::AcqRel);
                if deliver.send((frame, payload)).is_err() {
                    break;
                }
            }
            Err(error) => {
                log::warn!(target: "terminal", "the terminal host sent a frame this app cannot read; disconnecting: {error}");
                break;
            }
        }
    }
    // What was read is still delivered; the delivery thread ends after it.
    drop(deliver);
    if let Some(inner) = inner.upgrade() {
        inner.lost(&conn);
    } else {
        conn.close();
    }
}

/// Whether the registry holds the session `key`, asked without waiting on
/// anything: a host is answered from this app's own snapshot
/// ([`HostClient::holds_session`]), and an in-process registry from its own
/// map, a local lock with no connection behind it. `None` when the registry
/// cannot tell (a host not yet listed, or an unreadable list): the caller
/// decides what that means.
///
/// Its natural home is beside `tracks_run` in `terminal/mod.rs`; it lives here
/// because a descendant module can read that file's private `host` field.
pub(crate) fn registry_holds(state: &crate::terminal::TerminalSessions, key: &str) -> Option<bool> {
    match &state.host {
        Some(host) => host.holds_session(key),
        None => crate::terminal::list_sessions(state)
            .ok()
            .map(|listed| listed.iter().any(|session| session.id == key)),
    }
}

#[cfg(test)]
#[path = "client_tests.rs"]
mod tests;
