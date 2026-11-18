//! GitPulse MCP Server.
//!
//! JSON-RPC over stdio. Protocol and tools live in `gitpulse_lib::mcp` so the
//! desktop app and this binary cannot advertise different catalogs; this file
//! is only the transport, and it exists to make four failure modes survivable.
//!
//! What a naive `for line in stdin.lock().lines()` loop did, all measured:
//!
//! * one non-UTF-8 byte ended the loop and the process exited **0**, so the
//!   supervising client recorded a clean shutdown and every queued request
//!   behind that byte was discarded;
//! * 600 MiB of newline-free input took the process from 10 MiB to 616 MiB
//!   resident, because `lines()` buffers until it finds a newline;
//! * one slow call blocked every other request — a trivial `tools/list` queued
//!   behind a stalled `gitpulse_status` got no answer in a 12-second window,
//!   and the worst case is minutes;
//! * `let _ = writeln!(..)` discarded the broken-pipe error, so a server whose
//!   client had gone away kept executing requests and spawning git for nobody.
//!
//! So: frames are read bounded ([`gitpulse_lib::ndjson`]), a bad frame is
//! answered and skipped rather than fatal, each request runs on its own thread
//! under a deadline and a panic guard, and a failed write ends the session.
//!
//! Not every failed write is the client's doing, and the wire tells them apart:
//!
//! * a response past [`mcp::MAX_FRAME_BYTES`] is the *request's* problem. It is
//!   replaced by an error for that id (`bytes` and `cap` in its data) and the
//!   session keeps serving. This used to end the session with exit 0 and no
//!   answer at all. The library budgets its results well below the frame, so
//!   this is the backstop for what a budget did not foresee. If the id alone
//!   makes that error too big, it goes out with `id: null`, as JSON-RPC says of
//!   an error whose request id cannot be used;
//! * a closed pipe (EPIPE) is the client gone: the session ends at once;
//! * a write that takes longer than `GITPULSE_MCP_WRITE_TIMEOUT_MS` (default
//!   10 s, it was a fixed 1 s) means the client stopped reading. A partial line
//!   may already be on the pipe, so the session ends rather than resume
//!   mid-line. A reader that pauses for a second or two survives. While a write
//!   is stalled, the read loop and the deadline watchdog (which answer through
//!   the same single writer) wait behind it, for up to that window.
//!
//! `GITPULSE_MCP_MAX_RESPONSE_BYTES` lowers the response cap, for tests and
//! diagnosis only: it is clamped to the frame and never raises it.

use std::io::{self, BufReader, Read};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{mpsc, Arc, Condvar, Mutex};
use std::time::{Duration, Instant};

use gitpulse_lib::mcp::{self, Accepted, Era, JsonRpcRequest, JsonRpcResponse, Ready};
use gitpulse_lib::ndjson;
use serde_json::{json, Value};

/// Largest single JSON-RPC message accepted: [`mcp::MAX_FRAME_BYTES`], the
/// figure every result budget in the library is derived from. A legitimate
/// request is kilobytes; anything near this is a wedged or hostile peer, and the
/// frame is dropped rather than buffered.
const MAX_FRAME_BYTES: usize = mcp::MAX_FRAME_BYTES;

/// A test and diagnostic knob: lowers the size past which a *response* is
/// replaced by an error for its request (see [`Wire::send`]). It can only
/// lower the backstop (a larger value is clamped to [`MAX_FRAME_BYTES`]) and
/// never touches the result budgets, which stay derived from the real frame.
const MAX_RESPONSE_ENV: &str = "GITPULSE_MCP_MAX_RESPONSE_BYTES";

/// How long a response may take to leave the process before the client is
/// judged stalled. A reader that merely pauses (a busy host, a debugger)
/// recovers inside this; one that never reads does not, and the session ends.
const DEFAULT_WRITE_TIMEOUT: Duration = Duration::from_secs(10);
const WRITE_TIMEOUT_ENV: &str = "GITPULSE_MCP_WRITE_TIMEOUT_MS";

/// How long one request may run before the client is told it was abandoned.
///
/// This does not stop the work — a thread cannot be cancelled from outside, and
/// the git subprocesses underneath have their own 90 s ceilings. What it stops
/// is the *client* waiting: the answer goes out at the deadline and the server
/// keeps serving. Overridable so the stress tests do not have to wait a minute
/// to observe the behaviour.
const DEFAULT_CALL_BUDGET: Duration = Duration::from_millis(mcp::DEFAULT_CALL_BUDGET_MS);
const CALL_BUDGET_ENV: &str = mcp::CALL_BUDGET_ENV;

/// Requests allowed to be executing at once. Past this the server says so
/// instead of spawning unbounded threads: a client that pipelines faster than
/// the disk can answer must be refused, not absorbed. Blocking peer waits
/// (`gitpulse_agent_wait`) are further capped at [`mcp::MAX_WAIT_CALLS`] (`4`)
/// so parked waiters can never starve ordinary tool calls.
const MAX_IN_FLIGHT: usize = 32;

/// Longest the watchdog sleeps with nothing pending, so a broken wire still
/// ends its thread eventually. Everything else wakes it: a new request
/// notifies it, and a pending one bounds its sleep by its own deadline.
///
/// It used to wake every 100 ms whatever was pending, as did the input loop.
/// One server per agent session, and 74 of them were alive on one host after
/// three days — about 1,500 timer wakeups a second spent watching nothing.
const WATCHDOG_IDLE_RECHECK: Duration = Duration::from_secs(30);

/// Grace past the budget for the shutdown drain, so a worker the watchdog
/// already answered for has time to release its slot.
const DRAIN_GRACE: Duration = Duration::from_millis(100);

/// Polling interval for the two places that wait on `in_flight`: shutdown
/// drain, and the concurrency ceiling. Both are off the hot path.
const SLOT_POLL: Duration = Duration::from_millis(2);

fn call_budget() -> Duration {
    millis_from_env(CALL_BUDGET_ENV).unwrap_or(DEFAULT_CALL_BUDGET)
}

fn write_timeout() -> Duration {
    millis_from_env(WRITE_TIMEOUT_ENV).unwrap_or(DEFAULT_WRITE_TIMEOUT)
}

/// Longest either window may be set to. A value past it is clamped, not
/// honoured: an unbounded write window is a session hung on a reader that
/// stopped reading, and an unbounded call budget is a request nobody answers.
const MAX_ENV_WINDOW: Duration = Duration::from_secs(10 * 60);

/// A positive millisecond count from the environment, at most
/// [`MAX_ENV_WINDOW`]; zero and anything unparseable are unset.
fn millis_from_env(name: &str) -> Option<Duration> {
    std::env::var(name)
        .ok()
        .and_then(|raw| raw.parse::<u64>().ok())
        .filter(|ms| *ms > 0)
        .map(|ms| Duration::from_millis(ms).min(MAX_ENV_WINDOW))
}

/// The largest response line written, [`MAX_RESPONSE_ENV`] included.
fn response_cap() -> usize {
    std::env::var(MAX_RESPONSE_ENV)
        .ok()
        .and_then(|raw| raw.parse::<usize>().ok())
        .filter(|bytes| *bytes > 0)
        .map_or(MAX_FRAME_BYTES, |bytes| bytes.min(MAX_FRAME_BYTES))
}

/// The single writer. Every response goes through here so concurrent workers
/// cannot interleave bytes into the same line.
struct Wire {
    out: Result<gitpulse_lib::output::BoundedOutput, io::Error>,
    /// Largest line (newline included) this wire writes. A response past it is
    /// replaced by an error for its own request rather than written.
    cap: usize,
    broken: AtomicBool,
    /// The input loop's channel, poked when the wire breaks so a reader
    /// blocked waiting for the host wakes at once instead of polling the flag.
    input_wake: Mutex<Option<mpsc::SyncSender<io::Result<Vec<u8>>>>>,
}

impl Wire {
    fn new() -> Self {
        Self::to(io::stdout())
    }

    fn to(writer: impl io::Write + Send + 'static) -> Self {
        Self::with_limits(writer, response_cap(), write_timeout())
    }

    fn with_limits(writer: impl io::Write + Send + 'static, cap: usize, timeout: Duration) -> Self {
        Self {
            out: gitpulse_lib::output::BoundedOutput::new(
                writer,
                "gitpulse-mcp-stdout",
                cap,
                timeout,
            ),
            cap,
            broken: AtomicBool::new(false),
            input_wake: Mutex::new(None),
        }
    }

    /// Records that nothing more can be written, then wakes the input loop.
    ///
    /// The flag is set first, so a reader that misses the poke (its channel
    /// was full) finds it on its next pass instead of blocking for good.
    fn mark_broken(&self) {
        self.broken.store(true, Ordering::Release);
        if let Ok(guard) = self.input_wake.lock() {
            if let Some(sender) = guard.as_ref() {
                let _ = sender.try_send(Err(io::Error::new(
                    io::ErrorKind::BrokenPipe,
                    "stdout unavailable; input cancelled",
                )));
            }
        }
    }

    /// Write one response. Returns false once the pipe is gone.
    ///
    /// The error is acted on rather than discarded: Rust ignores SIGPIPE, so a
    /// dropped write is invisible unless it is checked, and a server that keeps
    /// working after its client has closed the pipe is burning CPU and spawning
    /// git processes whose output nobody will ever read.
    fn send(&self, response: &JsonRpcResponse) -> bool {
        if self.broken.load(Ordering::Relaxed) {
            return false;
        }
        // Serializing a response built only from `Value`s cannot fail, but
        // saying so in a fallback beats an `unwrap` that would take the process
        // down if that ever stopped being true.
        let line = match serde_json::to_string(response) {
            Ok(line) => line,
            Err(error) => {
                log::error!(target: "mcp", "response could not be serialized: {error}");
                serde_json::to_string(&mcp::err(
                    response.id.clone(),
                    -32603,
                    "response could not be serialized",
                    None,
                ))
                .unwrap_or_else(|_| {
                    r#"{"jsonrpc":"2.0","id":null,"error":{"code":-32603,"message":"internal error"}}"#
                        .to_string()
                })
            }
        };
        let out = match &self.out {
            Ok(out) => out,
            Err(error) => {
                log::error!(target: "mcp", "stdout worker could not start: {error}");
                self.mark_broken();
                return false;
            }
        };
        // A response too big to write is the request's problem, not the
        // wire's: answer that id with an error that says how big it was, and
        // keep serving. The compare-and-swap that let this response through
        // is already spent, so the request is still answered exactly once.
        let size = line.len() + 1;
        let line = if size > self.cap {
            match self.refusal(&response.id, size) {
                Some(refusal) => refusal,
                None => return true,
            }
        } else {
            line
        };
        let mut written = out.write(format!("{line}\n").as_bytes());
        if written
            .as_ref()
            .is_err_and(|error| error.kind() == io::ErrorKind::InvalidInput)
        {
            // The output's own bound disagreed with ours. It refuses before it
            // writes a byte, so the sink is still usable: say so to the
            // request instead of ending the session.
            match self.refusal(&response.id, size) {
                Some(refusal) => written = out.write(format!("{refusal}\n").as_bytes()),
                None => return true,
            }
        }
        match written {
            Ok(()) => true,
            Err(error) => {
                self.fail(&error);
                false
            }
        }
    }

    /// The error that stands in for a response of `bytes` that cannot be
    /// written, as a line; `None` when even that does not fit, in which case the
    /// request goes unanswered and the session keeps running.
    ///
    /// The error names the request. When the id alone makes the error too big
    /// (a client may send an id of any size up to the frame), the error is
    /// sent with `id: null` instead, as JSON-RPC prescribes for an error whose
    /// request id cannot be determined: the client learns that a response was
    /// refused, and the session is not left silent.
    fn refusal(&self, id: &Value, bytes: usize) -> Option<String> {
        let shown: String = id.to_string().chars().take(64).collect();
        log::warn!(
            target: "mcp",
            "response for request {shown} is {bytes} bytes, past the {} byte cap; answering with an error",
            self.cap
        );
        for answering in [id, &Value::Null] {
            let refusal = mcp::err(
                answering.clone(),
                -32603,
                format!("response is {bytes} bytes; the frame cap is {}", self.cap),
                Some(json!({ "bytes": bytes, "cap": self.cap })),
            );
            if let Ok(line) = serde_json::to_string(&refusal) {
                if line.len() < self.cap {
                    return Some(line);
                }
            }
        }
        log::error!(
            target: "mcp",
            "the error for oversized request {shown} does not fit the {} byte cap either; dropped",
            self.cap
        );
        None
    }

    /// Ends the session after a write that may have left a partial line on the
    /// pipe: the stream cannot be resynchronized, so nothing more is written.
    fn fail(&self, error: &io::Error) {
        match error.kind() {
            io::ErrorKind::TimedOut | io::ErrorKind::WouldBlock => log::warn!(
                target: "mcp",
                "client stalled: {error}; ending the session (raise {WRITE_TIMEOUT_ENV} for a slower reader)"
            ),
            _ => log::warn!(target: "mcp", "stdout failed: {error}; stopping without retry"),
        }
        self.mark_broken();
    }
}

/// One request the watchdog is holding a deadline for.
struct Pending {
    deadline: Instant,
    /// Set by whichever of the worker and the watchdog answers first, so the
    /// other stays silent. A client must never see two responses for one id.
    answered: Arc<AtomicBool>,
    timeout_response: JsonRpcResponse,
}

/// The requests the watchdog holds deadlines for, and what wakes it.
#[derive(Default)]
struct Watchlist {
    queue: Mutex<Vec<Pending>>,
    /// Notified on every new entry, so the watchdog re-plans its sleep.
    changed: Condvar,
}

impl Watchlist {
    /// Registers `entry`; false when the lock is poisoned and it was not.
    fn push(&self, entry: Pending) -> bool {
        let Ok(mut queue) = self.queue.lock() else {
            return false;
        };
        queue.push(entry);
        drop(queue);
        self.changed.notify_one();
        true
    }
}

/// Answers any request past its deadline and drops it from the registry.
///
/// One thread for the whole process, rather than a timer per request. It
/// sleeps until the earliest unanswered deadline — or, with none, until a new
/// request arrives — so an idle server costs no wakeups and an overdue
/// request is answered at its deadline rather than up to a tick late.
fn spawn_watchdog(watchlist: Arc<Watchlist>, wire: Arc<Wire>) {
    let _ = gitpulse_lib::supervisor::spawn_service("mcp-watchdog", move || {
        let Ok(mut queue) = watchlist.queue.lock() else {
            return;
        };
        loop {
            if wire.broken.load(Ordering::Acquire) {
                return;
            }
            let now = Instant::now();
            let mut expired: Vec<JsonRpcResponse> = Vec::new();
            queue.retain(|entry| {
                if entry.answered.load(Ordering::Acquire) {
                    return false;
                }
                if now < entry.deadline {
                    return true;
                }
                // The worker may be answering right now; only one of us wins.
                if entry
                    .answered
                    .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
                    .is_ok()
                {
                    expired.push(entry.timeout_response.clone());
                }
                false
            });
            if !expired.is_empty() {
                // Never write to the wire holding the lock a dispatch needs.
                drop(queue);
                for response in &expired {
                    log::warn!(target: "mcp", "abandoned a request past its budget: {}", response.id);
                    // The work keeps running; `in_flight` is released by the
                    // worker when it eventually finishes, so an abandoned call
                    // still occupies a slot and a storm of them is refused
                    // rather than piling up.
                    let _ = wire.send(response);
                }
                queue = match watchlist.queue.lock() {
                    Ok(queue) => queue,
                    Err(_) => return,
                };
                continue;
            }
            let sleep = queue
                .iter()
                .map(|entry| entry.deadline.saturating_duration_since(now))
                .min()
                .unwrap_or(WATCHDOG_IDLE_RECHECK)
                .min(WATCHDOG_IDLE_RECHECK);
            queue = match watchlist.changed.wait_timeout(queue, sleep) {
                Ok((queue, _)) => queue,
                Err(_) => return,
            };
        }
    });
}

/// Run one accepted request on its own thread, under a panic guard.
fn dispatch(
    ready: Ready,
    wire: Arc<Wire>,
    watchlist: Arc<Watchlist>,
    in_flight: Arc<AtomicUsize>,
    budget: Duration,
) {
    dispatch_with(
        |work| {
            gitpulse_lib::supervisor::spawn_once("mcp-request", "mcp request worker", work)
                .map(drop)
        },
        ready,
        wire,
        watchlist,
        in_flight,
        budget,
    );
}

/// [`dispatch`], with the thread start passed in so a failed start — the
/// process is out of threads — can be exercised.
fn dispatch_with(
    spawn: impl FnOnce(Box<dyn FnOnce() + Send>) -> io::Result<()>,
    ready: Ready,
    wire: Arc<Wire>,
    watchlist: Arc<Watchlist>,
    in_flight: Arc<AtomicUsize>,
    budget: Duration,
) {
    let answered = Arc::new(AtomicBool::new(false));
    let timeout_response = ready.timed_out(budget);
    let panicked = ready.panicked();
    let unstarted = ready.unstarted();
    if !watchlist.push(Pending {
        deadline: Instant::now() + budget,
        answered: Arc::clone(&answered),
        timeout_response,
    }) {
        let _ = wire.send(&panicked);
        in_flight.fetch_sub(1, Ordering::AcqRel);
        return;
    }

    let worker_answered = Arc::clone(&answered);
    let worker_in_flight = Arc::clone(&in_flight);
    let worker_wire = Arc::clone(&wire);
    let spawned = spawn(Box::new(move || {
        // Without this a panic anywhere under a tool call takes the whole
        // process with it, and the client sees a dead server rather than a
        // failed call. `install_panic_hook` only logs; it does not unwind.
        let outcome = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| mcp::run(ready)));
        let response = match outcome {
            Ok(response) => response,
            Err(_) => panicked,
        };
        if worker_answered
            .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
            .is_ok()
        {
            worker_wire.send(&response);
        }
        worker_in_flight.fetch_sub(1, Ordering::AcqRel);
    }));

    if let Err(error) = spawned {
        // Out of threads. The closure that held the request's answers is gone
        // with the failed spawn, and the watchdog skips an answered entry, so
        // the reply must be sent here or the client waits for nothing.
        if answered
            .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
            .is_ok()
        {
            log::error!(target: "mcp", "could not spawn a worker thread: {error}");
            let _ = wire.send(&unstarted);
        }
        in_flight.fetch_sub(1, Ordering::AcqRel);
    }
}

/// Recover the `id` from a message that failed to deserialize.
///
/// Without this the error response carries `id: null` and the client's pending
/// entry for the real id never resolves — it hangs until its own timeout. The
/// code matters too: well-formed JSON that merely fails the request schema is
/// `-32600 Invalid Request`, not `-32700 Parse error`.
fn malformed(line: &str) -> Option<JsonRpcResponse> {
    match serde_json::from_str::<Value>(line) {
        Err(error) => Some(mcp::err(
            Value::Null,
            -32700,
            format!("Parse error: {error}"),
            None,
        )),
        Ok(value) => {
            // A batch is an array. JSON-RPC batching is not part of MCP, and
            // answering with `id: null` would leave every id in the batch
            // unresolved, so say what happened.
            if value.is_array() {
                return Some(mcp::err(
                    Value::Null,
                    -32600,
                    "Invalid Request: JSON-RPC batching is not supported; send one message per line",
                    None,
                ));
            }
            let id = value.get("id").cloned().unwrap_or(Value::Null);
            // A message with no `id` that also fails to deserialize is a
            // malformed notification. JSON-RPC forbids answering a
            // notification, and an unsolicited `id: null` error would confuse a
            // client more than silence.
            if id.is_null() && value.get("id").is_none() {
                log::warn!(target: "mcp", "dropped a malformed notification");
                return None;
            }
            Some(mcp::err(
                id,
                -32600,
                "Invalid Request: message does not match the JSON-RPC request schema",
                Some(json!({
                    "expected": ["jsonrpc", "id", "method"],
                })),
            ))
        }
    }
}

/// Why the read loop stopped. Reported through the exit code so a supervisor
/// can tell a clean shutdown from a fault — the distinction the old loop lost
/// by exiting 0 on a bad byte.
enum Stop {
    /// stdin reached end of file: the client's documented shutdown signal.
    EndOfInput,
    /// stdout could not be written: the client is gone.
    ClientGone,
    /// stdin itself failed.
    StreamFault(String),
}

/// One bounded reader worker lets a failed output cancel the main input loop
/// even when the host never closes stdin. Healthy idle sessions have no idle
/// deadline; cancellation is polled between bounded waits for input chunks.
struct HostInput {
    receiver: mpsc::Receiver<io::Result<Vec<u8>>>,
    buffered: io::Cursor<Vec<u8>>,
    wire: Arc<Wire>,
}

impl HostInput {
    fn new(wire: Arc<Wire>) -> io::Result<Self> {
        let (sender, receiver) = mpsc::sync_channel(2);
        if let Ok(mut wake) = wire.input_wake.lock() {
            *wake = Some(sender.clone());
        }
        gitpulse_lib::supervisor::spawn_once(
            "gitpulse-mcp-stdin",
            "mcp stdin reader",
            move || {
                // `Wire` keeps a sender clone to wake the reader, so the
                // channel cannot report this worker's death by disconnecting.
                // Whatever ends the loop — EOF, an error, a panic — the reader
                // must still get a last message, or it blocks for good.
                //
                // A blocking `send`, not `try_send`: with the channel full the
                // poke would be dropped and the reader, once it drained the
                // queued chunks, would wait forever. Blocking cannot deadlock —
                // it waits only while the reader has chunks left to take, and
                // fails at once if the reader is gone.
                struct LastWord(Option<mpsc::SyncSender<io::Result<Vec<u8>>>>);
                impl Drop for LastWord {
                    fn drop(&mut self) {
                        if let Some(sender) = self.0.take() {
                            let _ = sender.send(Err(io::Error::new(
                                io::ErrorKind::BrokenPipe,
                                "stdin worker stopped without EOF",
                            )));
                        }
                    }
                }
                let mut last_word = LastWord(Some(sender.clone()));
                let stdin = io::stdin();
                let mut input = stdin.lock();
                let mut bytes = [0; 8192];
                loop {
                    let (message, done) = match input.read(&mut bytes) {
                        Ok(count) => (Ok(bytes[..count].to_vec()), count == 0),
                        Err(error) if error.kind() == io::ErrorKind::Interrupted => continue,
                        Err(error) => (Err(error), true),
                    };
                    let delivered = sender.send(message).is_ok();
                    if delivered && done {
                        // The terminal message (EOF or the error) is out.
                        last_word.0 = None;
                    }
                    if !delivered || done {
                        break;
                    }
                }
            },
        )?;
        Ok(Self {
            receiver,
            buffered: io::Cursor::new(Vec::new()),
            wire,
        })
    }
}

impl Read for HostInput {
    fn read(&mut self, bytes: &mut [u8]) -> io::Result<usize> {
        if bytes.is_empty() {
            return Ok(0);
        }
        loop {
            if self.wire.broken.load(Ordering::Relaxed) {
                return Err(io::Error::new(
                    io::ErrorKind::BrokenPipe,
                    "stdout unavailable; input cancelled",
                ));
            }
            let count = self.buffered.read(bytes)?;
            if count > 0 {
                return Ok(count);
            }
            // Blocks until the host sends something or the wire breaks:
            // `Wire::mark_broken` sends into this same channel. The flag check
            // above covers the poke it could not deliver to a full channel.
            match self.receiver.recv() {
                Ok(Ok(chunk)) if chunk.is_empty() => return Ok(0),
                Ok(Ok(chunk)) => self.buffered = io::Cursor::new(chunk),
                Ok(Err(error)) => return Err(error),
                Err(mpsc::RecvError) => {
                    return Err(io::Error::new(
                        io::ErrorKind::BrokenPipe,
                        "stdin worker stopped without EOF",
                    ))
                }
            }
        }
    }
}

fn serve(wire: Arc<Wire>) -> Stop {
    let mut era = Era::Unknown;
    let budget = call_budget();
    let watchlist = Arc::new(Watchlist::default());
    let in_flight = Arc::new(AtomicUsize::new(0));
    spawn_watchdog(Arc::clone(&watchlist), Arc::clone(&wire));

    let input = match HostInput::new(Arc::clone(&wire)) {
        Ok(input) => input,
        Err(error) => return Stop::StreamFault(format!("stdin worker could not start: {error}")),
    };
    let mut reader = BufReader::new(input);

    let stop = read_loop(&mut reader, &wire, &mut era, &watchlist, &in_flight, budget);
    if wire.broken.load(Ordering::Relaxed) {
        return Stop::ClientGone;
    }
    // Closing stdin means "no more requests", not "discard the ones you took".
    // Exiting straight away killed every worker mid-flight, so a client that
    // pipelined a request and closed the pipe never got its answer — which the
    // stress suite caught as fourteen missing responses.
    drain_in_flight(&in_flight, budget);
    stop
}

/// Wait for `in_flight` to fall below the ceiling. False if it never did.
fn wait_for_slot(in_flight: &AtomicUsize, budget: Duration) -> bool {
    let deadline = Instant::now() + budget;
    loop {
        let current = in_flight.load(Ordering::Acquire);
        if current < MAX_IN_FLIGHT
            && in_flight
                .compare_exchange(current, current + 1, Ordering::AcqRel, Ordering::Acquire)
                .is_ok()
        {
            return true;
        }
        if Instant::now() >= deadline {
            return false;
        }
        std::thread::sleep(SLOT_POLL);
    }
}

/// Give accepted requests a bounded chance to finish before the process exits.
///
/// The grace is the call budget plus [`DRAIN_GRACE`], because a request that has already
/// outlived its budget was answered by the watchdog and its worker is only
/// still running to release its slot.
fn drain_in_flight(in_flight: &AtomicUsize, budget: Duration) {
    let deadline = Instant::now() + budget + DRAIN_GRACE;
    while in_flight.load(Ordering::Acquire) > 0 && Instant::now() < deadline {
        std::thread::sleep(SLOT_POLL);
    }
}

fn read_loop<R: std::io::BufRead>(
    reader: &mut R,
    wire: &Arc<Wire>,
    era: &mut Era,
    watchlist: &Arc<Watchlist>,
    in_flight: &Arc<AtomicUsize>,
    budget: Duration,
) -> Stop {
    loop {
        let line = match ndjson::read_frame(reader, MAX_FRAME_BYTES) {
            Ok(Some(line)) => line,
            Ok(None) => return Stop::EndOfInput,
            // A frame too long or not UTF-8 is one bad message, not a dead
            // stream: answer it and read the next one.
            Err(error) if error.is_recoverable() => {
                let response = mcp::err(
                    Value::Null,
                    -32700,
                    format!("Parse error: {}", error.message()),
                    None,
                );
                if !wire.send(&response) {
                    return Stop::ClientGone;
                }
                continue;
            }
            Err(error) => return Stop::StreamFault(error.message()),
        };

        if line.trim().is_empty() {
            continue;
        }

        let request: JsonRpcRequest = match serde_json::from_str(&line) {
            Ok(request) => request,
            Err(_) => {
                match malformed(&line) {
                    Some(response) if !wire.send(&response) => return Stop::ClientGone,
                    _ => {}
                }
                continue;
            }
        };

        match mcp::accept(request, era) {
            Accepted::Silent => {}
            Accepted::Answered(response) => {
                if !wire.send(&response) {
                    return Stop::ClientGone;
                }
            }
            Accepted::Ready(ready) => {
                // At capacity, wait for a slot rather than refusing outright:
                // a client pipelining a burst of cheap reads is doing nothing
                // wrong, and blocking here is ordinary flow control — its
                // writes back up, which is what a full pipe is for. The wait is
                // bounded so a genuinely wedged server still answers.
                if !wait_for_slot(in_flight, budget) {
                    let response = mcp::err(
                        ready.id().clone(),
                        -32603,
                        format!(
                            "server has been at its {MAX_IN_FLIGHT} concurrent request ceiling \
                             for {} ms; retry",
                            budget.as_millis()
                        ),
                        Some(json!({ "method": ready.method(), "maxInFlight": MAX_IN_FLIGHT })),
                    );
                    if !wire.send(&response) {
                        return Stop::ClientGone;
                    }
                    continue;
                }
                dispatch(
                    ready,
                    Arc::clone(wire),
                    Arc::clone(watchlist),
                    Arc::clone(in_flight),
                    budget,
                );
                // Workers write their own answers, so the main loop would never
                // see a broken pipe on its own. Without this check a server
                // whose client has gone keeps reading and dispatching forever.
                if wire.broken.load(Ordering::Relaxed) {
                    return Stop::ClientGone;
                }
            }
        }
    }
}

const USAGE: &str = "\
gitpulse-mcp — GitPulse MCP server

USAGE:
    gitpulse-mcp
    gitpulse-mcp tools [--json]
    gitpulse-mcp describe <tool>
    gitpulse-mcp call <tool> [key=value ...] [--json <object|-|@file>] [--write]
                      [--compact] [--timeout-ms N]
    gitpulse-mcp read <uri> [--compact] [--timeout-ms N]

With no arguments it speaks JSON-RPC (MCP) over stdin and stdout until stdin
closes. An MCP host starts it that way; see `npm run mcp:install`.

The other forms answer one request and exit, for an agent whose MCP connection
is not loaded or has dropped. They run the same tools, argument checks, trust
gates and policy as the server, in this process: `tools` lists the catalog,
`describe` prints one tool's schema, `call` runs one tool and prints its
JSON result on stdout, and `read` prints one gitpulse:// resource
(gitpulse://server/manifest lists the rest). They do not get past load: a read
deferred under load is deferred here too.

    gitpulse-mcp call gitpulse_insights repo_path=\"$PWD\"
    gitpulse-mcp call gitpulse_list_tasks repo_path=/abs/repo limit=20
    echo '{\"repo_path\":\"/abs/repo\",\"title\":\"...\"}' | gitpulse-mcp call gitpulse_add_task --json - --write
    gitpulse-mcp read gitpulse://server/manifest

A key=value is typed by the tool's schema: a string property takes the text as
it is, any other type takes JSON (limit=20, include=true, paths='[\"a\"]').
--json takes a whole argument object inline, from stdin (-) or a file (@path);
a key given twice, in either form, is refused. A tool that changes state (its
readOnlyHint is false) runs only with --write.

EXIT STATUS (call, read):
    0  the tool answered and no part of the answer says it is incomplete
       (still read each facet: this checks the flags below, nothing more)
    1  the tool reported an error, including an argument it refused, or it
       panicked
    2  the command line was wrong: unknown command, tool, option, key or
       resource, or a write without --write
    3  the answer says it is incomplete: ok: false, <facet>_ok: false,
       deferred: true or truncated: true on any object in it, or the result
       budget cut it. The partial result is still printed; stderr names
       where (worktrees.ok, agents.live.ok, ...)
    4  no answer within --timeout-ms (default: GITPULSE_MCP_CALL_TIMEOUT_MS,
       60000, for a read; 600000 for a write, which stopping midway would
       leave half done). The tool is stopped and its git children taken
       down; nothing is printed on stdout, and a write's outcome is unknown:
       read the state back before retrying

Stdout carries only the answer; warnings go to stderr. A terminal tool
(gitpulse_terminal_*) acts for the GitPulse terminal it runs in
(GITPULSE_TERMINAL_SESSION). Outside one, each call is a new caller
(mcp-<pid>) that cannot reach a child an earlier call left open, so only a
self-contained call (exec with keep_open=false, open_file) is useful there.

OPTIONS:
    -h, --help       Print this message
    -V, --version    Print the version

ENVIRONMENT:
    GITPULSE_MCP_CALL_TIMEOUT_MS      Per-request deadline in ms (default 60000,
                                      at most 600000; zero or junk = default),
                                      and the default --timeout-ms of call/read
    GITPULSE_MCP_WRITE_TIMEOUT_MS     How long a response may take to leave the
                                      process before the client is judged stalled
                                      and the session ends (default 10000). The
                                      read loop and deadline watchdog can wait
                                      this long behind a stalled write. At most
                                      600000 (ten minutes); zero or junk = default.
    GITPULSE_MCP_MAX_RESPONSE_BYTES   Test/diagnostic knob: lowers the response
                                      cap below the 4 MiB frame; never raises it.
";

/// What the command line asked for. Hosts pass no arguments, so any argument
/// is a person at a terminal, and serving them would wait on a stdin nobody
/// writes to — a `gitpulse-mcp --help` that never returns.
#[derive(Debug, PartialEq, Eq)]
enum Invocation {
    Serve,
    Help,
    Version,
    /// A one-shot command ([`CLI_COMMANDS`]), with its whole argument list.
    Command(Vec<String>),
    Unknown(String),
}

fn invocation(args: &[String]) -> Invocation {
    match args {
        [] => Invocation::Serve,
        [only] if only == "--help" || only == "-h" => Invocation::Help,
        [only] if only == "--version" || only == "-V" => Invocation::Version,
        [first, ..] if CLI_COMMANDS.contains(&first.as_str()) => Invocation::Command(args.to_vec()),
        _ => Invocation::Unknown(args.join(" ")),
    }
}

/// `--version`: the version line, unchanged for whatever reads it, then the
/// one-shot commands this build answers. `mcp:doctor` reads the second line:
/// the version does not move between releases, so it is the only way to tell
/// an installed binary that predates a command the skills tell agents to run.
fn version_text() -> String {
    format!(
        "{} {}\ncommands: {}\n",
        mcp::SERVER_NAME,
        mcp::server_version(),
        CLI_COMMANDS.join(", ")
    )
}

/// Answers a command line that is not a host's. Returns `None` to serve, or
/// the arguments of a one-shot command for [`run_command`], which runs only
/// after the process is set up as the server would be: logging, descriptor
/// headroom and the signal handlers that take a tool's `git` children down
/// with it when the caller's own timeout kills this process.
fn answer_command_line() -> Option<Vec<String>> {
    use std::io::Write;
    let args: Vec<String> = std::env::args().skip(1).collect();
    let text = match invocation(&args) {
        Invocation::Serve => return None,
        Invocation::Command(args) => return Some(args),
        Invocation::Help => USAGE.to_string(),
        Invocation::Version => version_text(),
        Invocation::Unknown(given) => {
            eprintln!(
                "gitpulse-mcp: unexpected argument {given:?}; a host passes none (see --help)"
            );
            std::process::exit(2);
        }
    };
    let mut out = io::stdout().lock();
    let written = out.write_all(text.as_bytes()).and_then(|()| out.flush());
    std::process::exit(if written.is_ok() { 0 } else { 1 });
}

fn main() {
    // Agent and scheduled work: shed before the app the user is looking at is
    // deferred, on every thread this process starts. First, so no thread has
    // already been given the app's default.
    gitpulse_lib::engine::process::run_process_as_background();
    // A caller (an agent) is blocked on every read this server answers, so its
    // background spawns wait out a low budget, bounded, rather than being shed
    // at 0.000s. Still above the reserve: see the function's doc comment for
    // why this is not Reactive.
    gitpulse_lib::engine::process::run_process_awaiting_background_budget();
    // And from the per-user spawn budget every GitPulse process shares, before
    // any spawn builds the gate.
    gitpulse_lib::engine::process::run_process_with_shared_spawn_budget();
    // Both switches above only set a flag. Everything below logs, spawns or
    // arms a handler, and none of it applies to printing a usage line.
    let command = answer_command_line();
    gitpulse_lib::logging::init();
    if command.is_some() {
        // A one-shot caller reads stderr beside the answer, and the setup
        // narration below is for a long-lived server's log. Warnings and
        // errors still reach both the log file and stderr.
        log::set_max_level(log::LevelFilter::Warn);
    }
    gitpulse_lib::logging::install_panic_hook();
    // Same descriptor headroom the GUI takes: this server answers agent
    // traffic through the same git engine, and a launchd-inherited limit
    // of 256 fails its spawns exactly the same way.
    log::info!(target: "setup", "{}", gitpulse_lib::limits::raise_open_file_limit().describe());
    // An agent host that stops this server sends SIGTERM. Without a handler
    // the default action ends the process instantly and every `git` a tool
    // call had in flight keeps running against the user's repository, holding
    // `.git/index.lock`, attributed to a server that no longer exists.
    // Reported either way, so a guarantee that could not be armed is in the
    // log rather than assumed.
    let signals = gitpulse_lib::procguard::install_signal_handlers();
    if signals.is_armed() {
        log::info!(target: "setup", "{}", signals.describe());
    } else {
        log::warn!(target: "setup", "{}", signals.describe());
    }

    if let Some(args) = command {
        exit_reaping(run_command(&args));
    }

    let wire = Arc::new(Wire::new());
    let code = match serve(Arc::clone(&wire)) {
        Stop::EndOfInput => {
            log::info!(target: "mcp", "stdin closed; exiting");
            0
        }
        Stop::ClientGone => {
            log::warn!(target: "mcp", "stdout is closed; the client went away");
            0
        }
        Stop::StreamFault(why) => {
            log::error!(target: "mcp", "stdin failed: {why}");
            1
        }
    };
    // Exit without waiting on abandoned workers. They hold no lock the client
    // can observe, and blocking here would let one wedged git subprocess turn
    // the documented "close stdin and wait" shutdown into a forced kill.
    exit_reaping(code);
}

/// Ends the process with `code` after taking down any child still registered.
///
/// Not waiting on a worker is not the same as leaving its subprocess
/// running, and `process::exit` runs no destructor that would tell the
/// difference. So the children themselves are taken down first: this returns
/// immediately when none are registered, and otherwise costs one
/// `procguard::CHILD_GRACE` — the window in which `git` removes its own
/// `.git/index.lock` instead of leaving it for the user to find.
fn exit_reaping(code: i32) -> ! {
    let sweep = gitpulse_lib::procguard::reap_all(gitpulse_lib::procguard::CHILD_GRACE);
    if sweep.is_complete() {
        log::info!(target: "shutdown", "{}", sweep.describe());
    } else {
        log::warn!(target: "shutdown", "{}", sweep.describe());
    }
    gitpulse_lib::procguard::exit(code);
}

// ---------------------------------------------------------------------------
// One-shot commands: `tools`, `describe`, `call`.
//
// An agent whose MCP connection is not loaded, was dropped by its host, or is
// wedged still has a shell. These answer one request from it and exit. They
// add no tool and no rule of their own: `call` is a `tools/call` request
// handed to [`mcp::process_request`], so argument validation, the result
// budget, unknown-tool handling, repository trust and policy gates are the
// server's own, in this process.
// ---------------------------------------------------------------------------

/// The first arguments that select a one-shot command instead of serving.
const CLI_COMMANDS: [&str; 4] = ["tools", "describe", "call", "read"];

/// Exit statuses of the one-shot commands. Distinct, so a caller chaining
/// `&&` never reads "could not look" as "looked and found nothing".
mod exit_status {
    /// The tool answered completely.
    pub const OK: i32 = 0;
    /// The tool reported an error, including an argument it refused.
    pub const TOOL_ERROR: i32 = 1;
    /// The command line was wrong: unknown command, tool, option or key.
    pub const USAGE: i32 = 2;
    /// The tool answered and said the answer is incomplete, on any object in
    /// it (`incomplete_parts`). The partial answer is printed.
    pub const INCOMPLETE: i32 = 3;
    /// No answer within the deadline. The tool was stopped, so a write's
    /// outcome is unknown.
    pub const TIMED_OUT: i32 = 4;
}

#[derive(Debug, PartialEq)]
enum CliCommand {
    Tools {
        json: bool,
    },
    Describe {
        tool: String,
    },
    Call {
        tool: String,
        arguments: Value,
        compact: bool,
        timeout: Duration,
    },
    Read {
        uri: String,
        compact: bool,
        timeout: Duration,
    },
}

/// What a one-shot command prints and how it exits.
#[derive(Debug, PartialEq)]
struct CliOutput {
    code: i32,
    stdout: String,
    stderr: String,
}

impl CliOutput {
    fn usage(message: impl std::fmt::Display) -> Self {
        CliOutput {
            code: exit_status::USAGE,
            stdout: String::new(),
            stderr: format!("gitpulse-mcp: {message} (see --help)\n"),
        }
    }
}

fn run_command(args: &[String]) -> i32 {
    use std::io::Write;
    let catalog = mcp::tools();
    let output = match parse_command(args, &catalog, &read_argument_source) {
        Ok(command) => execute(command, &catalog),
        Err(message) => CliOutput::usage(message),
    };
    // Stdout first: it is the answer. A reader that closed it early (`| head`)
    // gets a failing status rather than a silent success.
    let mut stdout = io::stdout().lock();
    let wrote = stdout
        .write_all(output.stdout.as_bytes())
        .and_then(|()| stdout.flush());
    drop(stdout);
    if !output.stderr.is_empty() {
        let mut stderr = io::stderr().lock();
        let _ = stderr.write_all(output.stderr.as_bytes());
    }
    match wrote {
        Ok(()) => output.code,
        Err(error) => {
            eprintln!("gitpulse-mcp: could not write the result to stdout: {error}");
            exit_status::TOOL_ERROR.max(output.code)
        }
    }
}

/// Reads `--json -` (stdin) or `--json @path`, bounded by the frame a server
/// would accept, so a runaway pipe is refused rather than buffered.
fn read_argument_source(source: &str) -> Result<String, String> {
    let limit = mcp::MAX_FRAME_BYTES;
    let reader: Box<dyn Read> = if source == "-" {
        Box::new(io::stdin())
    } else {
        let path = source.strip_prefix('@').unwrap_or(source);
        Box::new(std::fs::File::open(path).map_err(|e| format!("--json {source}: {e}"))?)
    };
    let mut bytes = Vec::new();
    reader
        .take(limit as u64 + 1)
        .read_to_end(&mut bytes)
        .map_err(|e| format!("--json {source}: {e}"))?;
    if bytes.len() > limit {
        return Err(format!("--json {source} is larger than {limit} bytes"));
    }
    String::from_utf8(bytes).map_err(|_| format!("--json {source} is not UTF-8"))
}

fn parse_command(
    args: &[String],
    catalog: &[Value],
    read: &dyn Fn(&str) -> Result<String, String>,
) -> Result<CliCommand, String> {
    let Some((command, rest)) = args.split_first() else {
        return Err("no command given".into());
    };
    match command.as_str() {
        "tools" => match rest {
            [] => Ok(CliCommand::Tools { json: false }),
            [flag] if flag == "--json" => Ok(CliCommand::Tools { json: true }),
            _ => Err(format!(
                "`tools` takes only --json, got {:?}",
                rest.join(" ")
            )),
        },
        "describe" => match rest {
            [tool] => find_tool(catalog, tool).map(|_| CliCommand::Describe { tool: tool.clone() }),
            _ => Err("`describe` takes exactly one tool name".into()),
        },
        "call" => parse_call(rest, catalog, read),
        "read" => parse_read(rest),
        other => Err(format!("unknown command {other:?}")),
    }
}

fn find_tool<'a>(catalog: &'a [Value], name: &str) -> Result<&'a Value, String> {
    catalog
        .iter()
        .find(|tool| tool["name"] == name)
        .ok_or_else(|| format!("unknown tool {name:?}; `gitpulse-mcp tools` lists them"))
}

/// A tool runs without `--write` only when it declares itself read-only.
/// Anything else (`false`, or no annotation at all) is treated as a write.
fn is_read_only(tool: &Value) -> bool {
    tool["annotations"]["readOnlyHint"] == true
}

fn parse_call(
    rest: &[String],
    catalog: &[Value],
    read: &dyn Fn(&str) -> Result<String, String>,
) -> Result<CliCommand, String> {
    let Some((name, rest)) = rest.split_first() else {
        return Err("`call` needs a tool name: gitpulse-mcp call <tool> [key=value ...]".into());
    };
    if name.starts_with('-') {
        return Err(format!(
            "`call` needs a tool name before options, got {name:?}"
        ));
    }
    let tool = find_tool(catalog, name)?;
    let mut arguments = serde_json::Map::new();
    let mut write = false;
    let mut compact = false;
    let mut json_given = false;
    let mut timeout = None;
    let mut rest = rest.iter();
    while let Some(arg) = rest.next() {
        match arg.as_str() {
            "--write" => write = true,
            "--compact" => compact = true,
            "--timeout-ms" => set_timeout(&mut timeout, rest.next())?,
            "--json" => {
                if json_given {
                    return Err("--json given twice".into());
                }
                json_given = true;
                let source = rest.next().ok_or(
                    "--json needs a value: a JSON object, - for stdin, or @path for a file",
                )?;
                let text = if source == "-" || source.starts_with('@') {
                    read(source)?
                } else {
                    source.clone()
                };
                let parsed: Value =
                    serde_json::from_str(&text).map_err(|e| format!("--json is not JSON: {e}"))?;
                let Value::Object(object) = parsed else {
                    return Err("--json must be a JSON object".into());
                };
                for (key, value) in object {
                    insert_once(&mut arguments, key, value)?;
                }
            }
            flag if flag.starts_with('-') => {
                return Err(format!("unknown option {flag:?} for `call`"));
            }
            pair => {
                let Some((key, raw)) = pair.split_once('=') else {
                    return Err(format!("expected key=value, got {pair:?}"));
                };
                let value = typed_value(tool, key, raw)?;
                insert_once(&mut arguments, key.to_owned(), value)?;
            }
        }
    }
    if !is_read_only(tool) && !write {
        return Err(format!(
            "{name} changes state (it is not marked readOnlyHint); pass --write to run it"
        ));
    }
    Ok(CliCommand::Call {
        tool: name.clone(),
        arguments: Value::Object(arguments),
        compact,
        timeout: timeout.unwrap_or_else(|| default_timeout(tool)),
    })
}

/// A read waits the server's own per-request budget. A write waits the
/// longest window there is: stopping one midway (a worktree half created, a
/// merge half done) is worse than waiting for it, and the server, which
/// answers at its budget, still lets the work finish.
fn default_timeout(tool: &Value) -> Duration {
    if is_read_only(tool) {
        call_budget()
    } else {
        MAX_ENV_WINDOW
    }
}

/// `read <uri> [--compact] [--timeout-ms N]`. The URI is the server's to
/// judge: one it does not address comes back as the caller's error.
fn parse_read(rest: &[String]) -> Result<CliCommand, String> {
    let mut uri = None;
    let mut compact = false;
    let mut timeout = None;
    let mut rest = rest.iter();
    while let Some(arg) = rest.next() {
        match arg.as_str() {
            "--compact" => compact = true,
            "--timeout-ms" => set_timeout(&mut timeout, rest.next())?,
            flag if flag.starts_with('-') => {
                return Err(format!("unknown option {flag:?} for `read`"));
            }
            given if uri.is_none() => uri = Some(given.to_owned()),
            extra => return Err(format!("`read` takes one URI, got another: {extra:?}")),
        }
    }
    Ok(CliCommand::Read {
        uri: uri.ok_or("`read` needs a URI: gitpulse-mcp read gitpulse://server/manifest")?,
        compact,
        timeout: timeout.unwrap_or_else(call_budget),
    })
}

/// `--timeout-ms N`: a positive whole number of milliseconds, at most
/// [`MAX_ENV_WINDOW`], given once. Refused rather than clamped, so the
/// deadline a caller asked for is the one that runs.
fn set_timeout(slot: &mut Option<Duration>, raw: Option<&String>) -> Result<(), String> {
    if slot.is_some() {
        return Err("--timeout-ms given twice".into());
    }
    let max = MAX_ENV_WINDOW.as_millis();
    let raw = raw.ok_or("--timeout-ms needs a number of milliseconds")?;
    match raw.parse::<u64>() {
        Ok(ms) if ms > 0 && u128::from(ms) <= max => {
            *slot = Some(Duration::from_millis(ms));
            Ok(())
        }
        _ => Err(format!(
            "--timeout-ms takes a whole number of milliseconds from 1 to {max}, got {raw:?}"
        )),
    }
}

fn insert_once(
    arguments: &mut serde_json::Map<String, Value>,
    key: String,
    value: Value,
) -> Result<(), String> {
    if arguments.contains_key(&key) {
        return Err(format!("argument {key:?} given twice"));
    }
    arguments.insert(key, value);
    Ok(())
}

/// Types a `key=value` by the property's declared schema type: a string
/// property takes the text as it is (so `title=42` is the string "42"), and
/// every other type takes JSON. A key the tool does not declare is refused
/// here, with the keys it does take, rather than left for validation.
fn typed_value(tool: &Value, key: &str, raw: &str) -> Result<Value, String> {
    let properties = &tool["inputSchema"]["properties"];
    let Some(schema) = properties.get(key) else {
        let mut accepted: Vec<&str> = properties
            .as_object()
            .map(|p| p.keys().map(String::as_str).collect())
            .unwrap_or_default();
        accepted.sort_unstable();
        return Err(format!(
            "{} takes no argument {key:?}; it takes: {}",
            tool["name"].as_str().unwrap_or("the tool"),
            accepted.join(", ")
        ));
    };
    match schema["type"].as_str() {
        Some("string") => Ok(Value::String(raw.to_owned())),
        declared => serde_json::from_str(raw).map_err(|_| {
            format!(
                "{key} takes {}; {raw:?} is not JSON (quote strings inside arrays and objects, or use --json)",
                declared.unwrap_or("a JSON value")
            )
        }),
    }
}

fn execute(command: CliCommand, catalog: &[Value]) -> CliOutput {
    match command {
        CliCommand::Tools { json } => CliOutput {
            code: exit_status::OK,
            stdout: if json {
                pretty(&Value::Array(catalog.to_vec()))
            } else {
                tool_lines(catalog)
            },
            stderr: String::new(),
        },
        CliCommand::Describe { tool } => match find_tool(catalog, &tool) {
            Ok(entry) => CliOutput {
                code: exit_status::OK,
                stdout: pretty(entry),
                stderr: String::new(),
            },
            Err(message) => CliOutput::usage(message),
        },
        CliCommand::Call {
            tool,
            arguments,
            compact,
            timeout,
        } => {
            let request = cli_request(
                "tools/call",
                json!({ "name": tool, "arguments": arguments }),
            );
            match run_bounded(request, timeout, mcp::run) {
                Bounded::Answered(response) => call_outcome(&response, compact),
                Bounded::TimedOut => timed_out_outcome(&tool, timeout),
            }
        }
        CliCommand::Read {
            uri,
            compact,
            timeout,
        } => {
            let request = cli_request("resources/read", json!({ "uri": uri }));
            match run_bounded(request, timeout, mcp::run) {
                Bounded::Answered(response) => read_outcome(&response, compact),
                Bounded::TimedOut => timed_out_outcome(&uri, timeout),
            }
        }
    }
}

/// One line per tool: name, `read` or `write`, and the first line of its
/// description.
fn tool_lines(catalog: &[Value]) -> String {
    let width = catalog
        .iter()
        .filter_map(|t| t["name"].as_str())
        .map(str::len)
        .max()
        .unwrap_or(0);
    let mut out = String::new();
    for tool in catalog {
        let name = tool["name"].as_str().unwrap_or("");
        let access = if is_read_only(tool) { "read " } else { "write" };
        let summary = tool["description"]
            .as_str()
            .and_then(|d| d.lines().next())
            .unwrap_or("");
        out.push_str(&format!("{name:<width$}  {access}  {summary}\n"));
    }
    out
}

fn pretty(value: &Value) -> String {
    let mut text = serde_json::to_string_pretty(value).unwrap_or_else(|_| value.to_string());
    text.push('\n');
    text
}

/// One request for the server's own request path.
///
/// A one-shot command has no session to negotiate. The legacy era serves a
/// request without per-request `_meta`, as it serves a client that sent
/// `initialize`, and answers a tool call with its result as compact JSON text.
fn cli_request(method: &str, params: Value) -> JsonRpcRequest {
    JsonRpcRequest {
        jsonrpc: "2.0".into(),
        id: Some(json!(1)),
        method: method.into(),
        params,
    }
}

/// What [`run_bounded`] got back.
#[derive(Debug)]
enum Bounded {
    /// The server's answer, or the answer for a request that panicked or
    /// could not start.
    Answered(JsonRpcResponse),
    /// Nothing within the budget. The caller exits, which stops the work.
    TimedOut,
}

/// Runs one request as the server does — header checks with [`mcp::accept`],
/// the body on a worker thread under a panic guard — and waits at most
/// `budget` for it. `run` is [`mcp::run`] outside tests.
///
/// Unlike the server, nothing is left running past the budget for long: the
/// command returns, and its process exits through [`exit_reaping`], which takes
/// the tool's `git` children down with it.
fn run_bounded<F>(request: JsonRpcRequest, budget: Duration, run: F) -> Bounded
where
    F: FnOnce(Ready) -> JsonRpcResponse + Send + 'static,
{
    let mut era = Era::Legacy;
    let ready = match mcp::accept(request, &mut era) {
        Accepted::Ready(ready) => ready,
        Accepted::Answered(response) => return Bounded::Answered(response),
        Accepted::Silent => {
            return Bounded::Answered(JsonRpcResponse {
                jsonrpc: "2.0",
                id: json!(1),
                result: None,
                error: Some(json!({ "code": -32603, "message": "the request was not answered" })),
            })
        }
    };
    let panicked = ready.panicked();
    let unstarted = ready.unstarted();
    let (sender, receiver) = mpsc::sync_channel(1);
    let worker_panicked = panicked.clone();
    let spawned = std::thread::Builder::new()
        .name("gitpulse-mcp-call".into())
        .spawn(move || {
            let outcome = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| run(ready)));
            // The receiver is gone only when the budget already ran out.
            let _ = sender.send(outcome.unwrap_or(worker_panicked));
        });
    if let Err(error) = spawned {
        log::error!(target: "mcp", "could not spawn a worker thread: {error}");
        return Bounded::Answered(unstarted);
    }
    match receiver.recv_timeout(budget) {
        Ok(response) => Bounded::Answered(response),
        Err(mpsc::RecvTimeoutError::Timeout) => Bounded::TimedOut,
        // The worker sends on every path it returns by, so a dropped sender
        // means it died some other way: report it as the panic it was.
        Err(mpsc::RecvTimeoutError::Disconnected) => Bounded::Answered(panicked),
    }
}

/// Exit 4, with what the caller must do about a write that may have happened.
fn timed_out_outcome(target: &str, budget: Duration) -> CliOutput {
    CliOutput {
        code: exit_status::TIMED_OUT,
        stdout: String::new(),
        stderr: format!(
            "gitpulse-mcp: {target} did not answer within {} ms (--timeout-ms, default {CALL_BUDGET_ENV}); it was stopped and its git children taken down. A write's outcome is unknown: read the state back before retrying\n",
            budget.as_millis()
        ),
    }
}

/// A JSON-RPC error. -32600..-32602 are the request's fault (an unknown
/// tool or resource, arguments that are not an object); anything else is the
/// server's, a panic included.
fn protocol_error_outcome(error: &Value, command: &str) -> CliOutput {
    let message = error["message"].as_str().unwrap_or("protocol error");
    let caller_fault = matches!(error["code"].as_i64(), Some(-32602..=-32600));
    CliOutput {
        code: if caller_fault {
            exit_status::USAGE
        } else {
            exit_status::TOOL_ERROR
        },
        stdout: String::new(),
        stderr: format!("gitpulse-mcp {command}: {message}\n"),
    }
}

fn call_outcome(response: &JsonRpcResponse, compact: bool) -> CliOutput {
    if let Some(error) = &response.error {
        return protocol_error_outcome(error, "call");
    }
    let result = response.result.as_ref().unwrap_or(&Value::Null);
    let text = result["content"][0]["text"].as_str().unwrap_or("");
    if result["isError"] == true {
        return CliOutput {
            code: exit_status::TOOL_ERROR,
            stdout: String::new(),
            stderr: format!("gitpulse-mcp call: {text}\n"),
        };
    }
    payload_outcome(text, compact, "call")
}

/// A `resources/read` answer: the one document's text, judged as a tool's
/// result is. Several contents are printed as the array they came as.
fn read_outcome(response: &JsonRpcResponse, compact: bool) -> CliOutput {
    if let Some(error) = &response.error {
        return protocol_error_outcome(error, "read");
    }
    let contents = &response.result.as_ref().unwrap_or(&Value::Null)["contents"];
    match contents.as_array().map(Vec::as_slice) {
        Some([only]) if only["text"].is_string() => {
            payload_outcome(only["text"].as_str().unwrap_or(""), compact, "read")
        }
        _ => payload_outcome(&contents.to_string(), compact, "read"),
    }
}

/// Prints a result's text: JSON pretty (or compact) with its incompleteness
/// judged, anything else as it came.
fn payload_outcome(text: &str, compact: bool, command: &str) -> CliOutput {
    let Ok(payload) = serde_json::from_str::<Value>(text) else {
        return CliOutput {
            code: exit_status::OK,
            stdout: format!("{text}\n"),
            stderr: String::new(),
        };
    };
    let incomplete = incomplete_parts(&payload);
    CliOutput {
        code: if incomplete.is_empty() {
            exit_status::OK
        } else {
            exit_status::INCOMPLETE
        },
        stdout: if compact {
            format!("{payload}\n")
        } else {
            pretty(&payload)
        },
        stderr: if incomplete.is_empty() {
            String::new()
        } else {
            format!(
                "gitpulse-mcp {command}: the answer is incomplete ({}); read its error fields before relying on it\n",
                incomplete.join(", ")
            )
        },
    }
}

/// Where a result says it is not whole, as dotted paths: `ok: false` or a
/// `<facet>_ok: false` (`sessions_ok`), `deferred: true` or
/// `truncated: true`, on the result or on any object inside it, and the
/// result budget's own cut marker. Facets nest (`gitpulse_insights` reports
/// `agents.live.ok` inside a result that otherwise succeeded), so every
/// object is read. Arrays hold rows, which are data rather than facets, and
/// are not read.
fn incomplete_parts(payload: &Value) -> Vec<String> {
    /// Deeper than any result nests; bounds the walk on a hostile payload.
    const MAX_DEPTH: usize = 16;
    /// What `mcp::budget` adds when it had to cut a result to fit.
    const BUDGET_CUT: &str = "result_truncation";

    fn walk(value: &Value, path: &str, depth: usize, parts: &mut Vec<String>) {
        let Some(object) = value.as_object() else {
            return;
        };
        for (key, field) in object {
            let at = if path.is_empty() {
                key.clone()
            } else {
                format!("{path}.{key}")
            };
            let says_partial = match key.as_str() {
                "deferred" | "truncated" => *field == true,
                BUDGET_CUT => !field.is_null(),
                "ok" => *field == false,
                other => other.ends_with("_ok") && *field == false,
            };
            if says_partial {
                parts.push(at.clone());
            }
            if depth < MAX_DEPTH {
                walk(field, &at, depth + 1, parts);
            }
        }
    }

    let mut parts = Vec::new();
    walk(payload, "", 0, &mut parts);
    parts
}

/// The library's environment-override guard, compiled into this crate too: the
/// library's own is `#[cfg(test)]` and `pub(crate)`, so it does not exist from
/// here (as in `gitpulsed`, which includes it the same way).
#[cfg(test)]
#[path = "../test_support/env.rs"]
mod test_env;

#[cfg(test)]
mod tests {
    use super::*;

    /// A deferred and retried insights read, with its wait for budget at every
    /// spawn, still ends inside the per-call budget the watchdog enforces.
    #[test]
    fn a_deferred_and_retried_read_fits_the_call_budget() {
        let worst = mcp::WORST_CASE_DEFERRED_READ;
        assert!(
            worst < DEFAULT_CALL_BUDGET,
            "{worst:?} vs {DEFAULT_CALL_BUDGET:?}"
        );
    }

    /// A writer the test can read back after the wire's worker wrote to it.
    #[derive(Clone, Default)]
    struct Captured(Arc<Mutex<Vec<u8>>>);

    impl io::Write for Captured {
        fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
            self.0.lock().unwrap().extend_from_slice(bytes);
            Ok(bytes.len())
        }
        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }

    fn result_of(id: Value, result: Value) -> JsonRpcResponse {
        JsonRpcResponse {
            jsonrpc: "2.0",
            id,
            result: Some(result),
            error: None,
        }
    }

    #[test]
    fn a_5_mib_response_is_answered_with_an_error_and_the_wire_stays_open() {
        let captured = Captured::default();
        let wire = Wire::to(captured.clone());
        let huge = result_of(json!(9), json!({ "blob": "x".repeat(5 * 1024 * 1024) }));
        assert!(
            wire.send(&huge),
            "an oversized response must be answered, not end the session"
        );
        assert!(
            !wire.broken.load(Ordering::Acquire),
            "an oversized response is the request's problem, not the wire's"
        );
        assert!(wire.send(&result_of(json!(10), json!({ "small": true }))));

        let text = String::from_utf8(captured.0.lock().unwrap().clone()).unwrap();
        let lines: Vec<&str> = text.lines().collect();
        assert_eq!(
            lines.len(),
            2,
            "one answer per send, got {} lines",
            lines.len()
        );
        let first: Value = serde_json::from_str(lines[0]).unwrap();
        assert_eq!(first["id"], 9, "the error names the request it answers");
        assert_eq!(first["error"]["code"], -32603, "{first}");
        let bytes = first["error"]["data"]["bytes"].as_u64().expect("bytes");
        let cap = first["error"]["data"]["cap"].as_u64().expect("cap");
        assert!(bytes > cap, "{bytes} should exceed {cap}");
        let second: Value = serde_json::from_str(lines[1]).unwrap();
        assert_eq!(second["id"], 10);
        assert_eq!(second["result"]["small"], true);
    }

    #[test]
    fn the_cap_is_on_the_whole_line_and_an_error_that_cannot_fit_is_dropped() {
        let response = result_of(json!(1), json!({ "blob": "x".repeat(100) }));
        let line = serde_json::to_string(&response).unwrap().len() + 1;

        let fits = Captured::default();
        let wire = Wire::with_limits(fits.clone(), line, Duration::from_secs(1));
        assert!(wire.send(&response));
        assert_eq!(
            fits.0.lock().unwrap().len(),
            line,
            "exactly at the cap is written"
        );

        let over = Captured::default();
        let wire = Wire::with_limits(over.clone(), line + 200, Duration::from_secs(1));
        assert!(wire.send(&result_of(json!(1), json!({ "blob": "x".repeat(500) }))));
        let text = String::from_utf8(over.0.lock().unwrap().clone()).unwrap();
        let refusal: Value = serde_json::from_str(text.trim()).unwrap();
        assert_eq!(refusal["error"]["data"]["cap"], line + 200, "{refusal}");

        let tiny = Captured::default();
        let wire = Wire::with_limits(tiny.clone(), 10, Duration::from_secs(1));
        assert!(
            wire.send(&response),
            "an unanswerable id must not end the session"
        );
        assert!(
            tiny.0.lock().unwrap().is_empty(),
            "nothing fits in ten bytes"
        );
        assert!(!wire.broken.load(Ordering::Acquire));
    }

    /// Runs `probe` with `name` set to each of `values`, then restores it.
    ///
    /// Through the repo's one sanctioned form (`bind_env`, serialized and
    /// restored on drop, also when a probe panics): raw `set_var` in a test is
    /// what `hermetic_contract` refuses.
    fn with_env(name: &str, values: &[&str], mut probe: impl FnMut(&str)) {
        let serial = crate::test_env::env_serial();
        for value in values {
            let _binding = crate::test_env::bind_env(&serial).set(name, value);
            probe(value);
        }
    }

    /// Attack 3: the test-only cap may only lower the cap.
    #[test]
    fn the_test_only_response_cap_can_never_raise_the_cap() {
        with_env(
            MAX_RESPONSE_ENV,
            &[
                "4194305",
                "99999999999999",
                "18446744073709551615",
                "18446744073709551616",
                "+99999999",
                "-1",
                "0",
                "abc",
                " 1",
                "1e9",
                "",
            ],
            |value| {
                assert!(
                    response_cap() <= MAX_FRAME_BYTES,
                    "{value:?} raised the cap to {}",
                    response_cap()
                );
            },
        );
    }

    /// Attack 3: an unbounded write window is a hung session. 0 and garbage fall
    /// back to the default; a huge value must be clamped, not honoured.
    #[test]
    fn the_write_timeout_is_bounded_whatever_the_environment_says() {
        const CEILING: Duration = Duration::from_secs(10 * 60);
        with_env(
            WRITE_TIMEOUT_ENV,
            &[
                "0",
                "-1",
                "abc",
                "",
                "600000",
                "86400000",
                "99999999999",
                "18446744073709551615",
            ],
            |value| {
                let window = write_timeout();
                assert!(!window.is_zero(), "{value:?} gave a zero write window");
                assert!(
                    window <= CEILING,
                    "{value:?} gave a {window:?} write window: a reader that stops reading hangs the session that long"
                );
            },
        );
    }

    /// Attack 3, same helper: the per-request deadline is unbounded too.
    #[test]
    fn the_call_budget_is_bounded_whatever_the_environment_says() {
        const CEILING: Duration = Duration::from_secs(60 * 60);
        with_env(
            CALL_BUDGET_ENV,
            &["86400000", "18446744073709551615"],
            |value| {
                let window = call_budget();
                assert!(
                    window <= CEILING,
                    "{value:?} gave a {window:?} request deadline"
                );
            },
        );
    }

    /// Attack 1: cap-1, cap and cap+1 on the whole line, newline included.
    #[test]
    fn the_whole_line_is_measured_at_cap_minus_one_cap_and_cap_plus_one() {
        let response = result_of(json!(1), json!({ "blob": "x".repeat(600) }));
        let size = serde_json::to_string(&response).unwrap().len() + 1;
        for (cap, written) in [(size + 1, true), (size, true), (size - 1, false)] {
            let sink = Captured::default();
            let wire = Wire::with_limits(sink.clone(), cap, Duration::from_secs(1));
            assert!(wire.send(&response), "cap {cap}");
            let text = String::from_utf8(sink.0.lock().unwrap().clone()).unwrap();
            assert!(
                text.ends_with('\n') && text.lines().count() == 1,
                "cap {cap}"
            );
            assert!(text.len() <= cap, "cap {cap}: wrote {} bytes", text.len());
            let line: Value = serde_json::from_str(text.trim()).unwrap();
            assert_eq!(
                line.get("result").is_some(),
                written,
                "cap {cap}: {line:.120}"
            );
            if !written {
                assert_eq!(line["error"]["data"]["bytes"], size);
            }
        }
    }

    /// Attack 4: a reader that closes mid-frame ends the session, and nothing is
    /// written after the failure.
    #[test]
    fn a_reader_that_closes_mid_frame_ends_the_session_and_nothing_more_is_written() {
        struct Breaks(Arc<Mutex<usize>>);
        impl io::Write for Breaks {
            fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
                let mut written = self.0.lock().unwrap();
                if *written >= 10 {
                    return Err(io::Error::from(io::ErrorKind::BrokenPipe));
                }
                let take = bytes.len().min(10 - *written);
                *written += take;
                Ok(take)
            }
            fn flush(&mut self) -> io::Result<()> {
                Ok(())
            }
        }
        let count = Arc::new(Mutex::new(0));
        let wire = Wire::with_limits(Breaks(count.clone()), 4096, Duration::from_secs(1));
        assert!(!wire.send(&result_of(json!(1), json!({ "a": "b".repeat(200) }))));
        assert!(wire.broken.load(Ordering::Acquire));
        assert!(!wire.send(&result_of(json!(2), json!({}))));
        assert_eq!(*count.lock().unwrap(), 10);
    }

    /// Attack 4: a reader that takes one byte per interval is judged stalled once
    /// the window passes, the session ends promptly, and the detached writer
    /// finishes its line without a second frame ever following it.
    #[test]
    fn a_one_byte_at_a_time_reader_is_judged_stalled_and_no_second_frame_follows() {
        #[derive(Clone, Default)]
        struct Trickle(Arc<Mutex<Vec<u8>>>);
        impl io::Write for Trickle {
            fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
                std::thread::sleep(Duration::from_millis(40));
                self.0.lock().unwrap().push(bytes[0]);
                Ok(1)
            }
            fn flush(&mut self) -> io::Result<()> {
                Ok(())
            }
        }
        let sink = Trickle::default();
        let wire = Wire::with_limits(sink.clone(), 4096, Duration::from_millis(200));
        let started = Instant::now();
        assert!(!wire.send(&result_of(json!(1), json!({ "a": "b".repeat(60) }))));
        assert!(
            started.elapsed() < Duration::from_secs(2),
            "{:?}",
            started.elapsed()
        );
        assert!(!wire.send(&result_of(json!(2), json!({ "second": true }))));
        std::thread::sleep(Duration::from_secs(5));
        let text = String::from_utf8(sink.0.lock().unwrap().clone()).unwrap();
        assert_eq!(text.lines().count(), 1, "{text}");
        assert!(!text.contains("second"));
    }

    #[test]
    fn a_request_id_that_alone_overflows_the_refusal_is_answered_with_a_null_id() {
        let sink = Captured::default();
        let wire = Wire::with_limits(sink.clone(), 512, Duration::from_secs(1));
        let id = json!("i".repeat(600));
        assert!(wire.send(&result_of(id, json!({ "blob": "x".repeat(5000) }))));
        let text = String::from_utf8(sink.0.lock().unwrap().clone()).unwrap();
        let line: Value = serde_json::from_str(text.trim()).expect("one JSON-RPC line");
        assert_eq!(line["id"], Value::Null, "{line}");
        assert_eq!(line["error"]["code"], -32603);
        assert!(line["error"]["data"]["bytes"].as_u64().unwrap() > 5000);
        assert!(text.len() <= 512);
        assert!(!wire.broken.load(Ordering::Acquire));
    }

    #[test]
    fn a_writer_that_stalls_past_the_write_window_ends_the_session() {
        struct Stalled;
        impl io::Write for Stalled {
            fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
                std::thread::sleep(Duration::from_secs(3));
                Ok(bytes.len())
            }
            fn flush(&mut self) -> io::Result<()> {
                Ok(())
            }
        }
        let wire = Wire::with_limits(Stalled, 1024, Duration::from_millis(150));
        let started = Instant::now();
        assert!(
            !wire.send(&result_of(json!(1), json!({}))),
            "a stalled client ends the session"
        );
        assert!(started.elapsed() < Duration::from_secs(2));
        assert!(wire.broken.load(Ordering::Acquire));
        assert!(
            !wire.send(&result_of(json!(2), json!({}))),
            "nothing is written after a possibly partial line"
        );
    }

    #[test]
    fn a_request_whose_thread_cannot_start_is_still_answered_once() {
        let request: JsonRpcRequest = serde_json::from_value(json!({
            "jsonrpc": "2.0", "id": 7, "method": "tools/list"
        }))
        .unwrap();
        let Accepted::Ready(ready) = mcp::accept(request, &mut Era::Legacy) else {
            panic!("a legacy tools/list is dispatched");
        };
        let captured = Captured::default();
        let wire = Arc::new(Wire::to(captured.clone()));
        let watchlist = Arc::new(Watchlist::default());
        let in_flight = Arc::new(AtomicUsize::new(1));
        dispatch_with(
            |_| Err(io::Error::other("no threads left")),
            ready,
            Arc::clone(&wire),
            Arc::clone(&watchlist),
            Arc::clone(&in_flight),
            Duration::from_secs(30),
        );

        let start = Instant::now();
        while captured.0.lock().unwrap().is_empty() && start.elapsed() < Duration::from_secs(5) {
            std::thread::sleep(Duration::from_millis(10));
        }
        let text = String::from_utf8(captured.0.lock().unwrap().clone()).unwrap();
        let lines: Vec<&str> = text.lines().collect();
        assert_eq!(lines.len(), 1, "exactly one answer, got {text:?}");
        let answer: Value = serde_json::from_str(lines[0]).unwrap();
        assert_eq!(answer["id"], 7, "the answer names the request");
        assert_eq!(answer["error"]["data"]["ran"], false, "{answer}");
        assert_eq!(in_flight.load(Ordering::Acquire), 0, "the slot is released");
        let queue = watchlist.queue.lock().unwrap();
        assert!(
            queue
                .iter()
                .all(|entry| entry.answered.load(Ordering::Acquire)),
            "the watchdog must not answer it a second time"
        );
    }
}

#[cfg(test)]
mod cli_tests {
    use super::*;

    fn args(list: &[&str]) -> Vec<String> {
        list.iter().map(|s| (*s).to_owned()).collect()
    }

    fn no_reader(source: &str) -> Result<String, String> {
        panic!("nothing should be read, asked for {source}")
    }

    fn parse(list: &[&str]) -> Result<CliCommand, String> {
        parse_command(&args(list), &mcp::tools(), &no_reader)
    }

    fn response(result: Value) -> JsonRpcResponse {
        JsonRpcResponse {
            jsonrpc: "2.0",
            id: json!(1),
            result: Some(result),
            error: None,
        }
    }

    /// A tool call through the real server path, with room to finish.
    fn call_tool(tool: &str, arguments: Value) -> JsonRpcResponse {
        let request = cli_request(
            "tools/call",
            json!({ "name": tool, "arguments": arguments }),
        );
        match run_bounded(request, Duration::from_secs(120), mcp::run) {
            Bounded::Answered(response) => response,
            Bounded::TimedOut => panic!("{tool} did not answer in 120 s"),
        }
    }

    fn text_result(payload: &Value) -> JsonRpcResponse {
        response(json!({
            "content": [{ "type": "text", "text": payload.to_string() }],
            "isError": false
        }))
    }

    #[test]
    fn one_shot_commands_are_routed_and_other_arguments_still_refused() {
        assert_eq!(
            invocation(&args(&["call", "gitpulse_status"])),
            Invocation::Command(args(&["call", "gitpulse_status"]))
        );
        assert_eq!(
            invocation(&args(&["tools"])),
            Invocation::Command(args(&["tools"]))
        );
        assert_eq!(
            invocation(&args(&["describe", "x"])),
            Invocation::Command(args(&["describe", "x"]))
        );
        assert_eq!(
            invocation(&args(&["read", "gitpulse://server/manifest"])),
            Invocation::Command(args(&["read", "gitpulse://server/manifest"]))
        );
        // Only the first argument selects a command.
        assert_eq!(
            invocation(&args(&["--json", "call"])),
            Invocation::Unknown("--json call".into())
        );
        assert_eq!(
            invocation(&args(&["serve"])),
            Invocation::Unknown("serve".into())
        );
        assert_eq!(invocation(&[]), Invocation::Serve);
    }

    #[test]
    fn a_read_tool_runs_without_write_and_a_write_tool_does_not() {
        let catalog = mcp::tools();
        let status = find_tool(&catalog, "gitpulse_status").unwrap();
        assert!(is_read_only(status), "{status}");
        let add = find_tool(&catalog, "gitpulse_add_task").unwrap();
        assert!(!is_read_only(add), "{add}");

        assert_eq!(
            parse(&["call", "gitpulse_status", "repo_path=/r"]),
            Ok(CliCommand::Call {
                tool: "gitpulse_status".into(),
                arguments: json!({ "repo_path": "/r" }),
                compact: false,
                timeout: call_budget()
            })
        );
        let refused = parse(&["call", "gitpulse_add_task", "repo_path=/r", "title=t"]).unwrap_err();
        assert!(refused.contains("--write"), "{refused}");
        assert!(parse(&[
            "call",
            "gitpulse_add_task",
            "repo_path=/r",
            "title=t",
            "--write"
        ])
        .is_ok());
    }

    /// Every tool without `readOnlyHint: true` needs --write, including one
    /// that carries no annotation at all: the default is the cautious one.
    #[test]
    fn a_tool_without_a_read_only_claim_is_a_write() {
        assert!(!is_read_only(&json!({ "name": "x" })));
        assert!(!is_read_only(
            &json!({ "name": "x", "annotations": { "readOnlyHint": "true" } })
        ));
        for tool in mcp::tools() {
            let name = tool["name"].as_str().unwrap();
            let without = parse_command(
                &args(&["call", name]),
                std::slice::from_ref(&tool),
                &no_reader,
            );
            if is_read_only(&tool) {
                assert!(without.is_ok(), "{name}: {without:?}");
            } else {
                assert!(without.unwrap_err().contains("--write"), "{name}");
            }
        }
    }

    #[test]
    fn key_value_arguments_are_typed_by_the_schema() {
        let catalog = mcp::tools();
        // A string property keeps digits and JSON-looking text as text.
        let Ok(CliCommand::Call { arguments, .. }) =
            parse(&["call", "gitpulse_status", "repo_path=42"])
        else {
            panic!("parse failed")
        };
        assert_eq!(arguments, json!({ "repo_path": "42" }));

        // An integer property takes JSON, and text that is not JSON is refused
        // with the type it wanted rather than sent as a string.
        let sessions = find_tool(&catalog, "gitpulse_sessions").unwrap();
        assert_eq!(
            sessions["inputSchema"]["properties"]["limit"]["type"],
            "integer"
        );
        assert_eq!(typed_value(sessions, "limit", "5"), Ok(json!(5)));
        let refused = typed_value(sessions, "limit", "five").unwrap_err();
        assert!(refused.contains("integer"), "{refused}");

        // A key the tool does not take names the keys it does.
        let refused = typed_value(sessions, "lmit", "5").unwrap_err();
        assert!(
            refused.contains("limit") && refused.contains("repo_path"),
            "{refused}"
        );

        // A value may itself contain '='.
        let Ok(CliCommand::Call { arguments, .. }) =
            parse(&["call", "gitpulse_status", "repo_path=/a=b"])
        else {
            panic!("parse failed")
        };
        assert_eq!(arguments, json!({ "repo_path": "/a=b" }));
    }

    #[test]
    fn json_arguments_come_inline_from_stdin_or_a_file_and_never_twice() {
        let tools = mcp::tools();
        let reader = |source: &str| -> Result<String, String> {
            assert!(source == "-" || source == "@brief.json", "{source}");
            Ok(r#"{"repo_path":"/r","limit":3}"#.to_owned())
        };
        for source in ["-", "@brief.json"] {
            let parsed = parse_command(
                &args(&["call", "gitpulse_sessions", "--json", source]),
                &tools,
                &reader,
            );
            assert_eq!(
                parsed,
                Ok(CliCommand::Call {
                    tool: "gitpulse_sessions".into(),
                    arguments: json!({ "repo_path": "/r", "limit": 3 }),
                    compact: false,
                    timeout: call_budget()
                })
            );
        }
        let inline = parse(&[
            "call",
            "gitpulse_status",
            "--json",
            r#"{"repo_path":"/r"}"#,
            "--compact",
        ]);
        assert_eq!(
            inline,
            Ok(CliCommand::Call {
                tool: "gitpulse_status".into(),
                arguments: json!({ "repo_path": "/r" }),
                compact: true,
                timeout: call_budget()
            })
        );

        // The same key from both forms, or twice in one, is refused: neither
        // copy silently wins.
        for list in [
            &[
                "call",
                "gitpulse_status",
                "--json",
                r#"{"repo_path":"/a"}"#,
                "repo_path=/b",
            ][..],
            &["call", "gitpulse_status", "repo_path=/a", "repo_path=/a"],
        ] {
            let refused = parse(list).unwrap_err();
            assert!(refused.contains("twice"), "{list:?}: {refused}");
        }
        for (list, wanted) in [
            (&["call", "gitpulse_status", "--json", "[1]"][..], "object"),
            (&["call", "gitpulse_status", "--json", "{"], "not JSON"),
            (&["call", "gitpulse_status", "--json"], "needs a value"),
            (
                &["call", "gitpulse_status", "--json", "{}", "--json", "{}"],
                "twice",
            ),
        ] {
            let refused = parse(list).unwrap_err();
            assert!(refused.contains(wanted), "{list:?}: {refused}");
        }
    }

    #[test]
    fn a_malformed_command_line_is_refused_before_anything_runs() {
        for (list, wanted) in [
            (&["call"][..], "needs a tool name"),
            (&["call", "--write"], "needs a tool name"),
            (&["call", "gitpulse_nope"], "unknown tool"),
            (&["call", "gitpulse_status", "--yes"], "unknown option"),
            (&["call", "gitpulse_status", "repo_path"], "key=value"),
            (&["describe"], "exactly one"),
            (&["describe", "gitpulse_nope"], "unknown tool"),
            (&["tools", "--all"], "only --json"),
        ] {
            let refused = parse(list).unwrap_err();
            assert!(refused.contains(wanted), "{list:?}: {refused}");
        }
    }

    #[test]
    fn the_read_source_is_bounded_and_named_in_its_errors() {
        let missing = read_argument_source("@/no/such/gitpulse-cli-brief.json").unwrap_err();
        assert!(
            missing.contains("/no/such/gitpulse-cli-brief.json"),
            "{missing}"
        );
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("big.json");
        std::fs::write(&path, vec![b' '; mcp::MAX_FRAME_BYTES + 1]).unwrap();
        let refused = read_argument_source(&format!("@{}", path.display())).unwrap_err();
        assert!(refused.contains("larger than"), "{refused}");
    }

    #[test]
    fn exit_status_tells_a_complete_answer_from_a_partial_one_and_from_an_error() {
        let complete = call_outcome(&text_result(&json!({ "ok": true })), true);
        assert_eq!(complete.code, exit_status::OK);
        assert_eq!(complete.stdout, "{\"ok\":true}\n");
        assert!(complete.stderr.is_empty());

        // A top-level refusal, a deferral, a capped scan, a budget cut, and a
        // facet that could not scan anywhere inside a result that otherwise
        // succeeded are all partial, and each is named by its path.
        for (payload, named) in [
            (json!({ "ok": false, "error": "e" }), "ok"),
            (
                json!({ "deferred": true, "retry_after_ms": 125 }),
                "deferred",
            ),
            (json!({ "items": [], "truncated": true }), "truncated"),
            (
                json!({ "items": [], "result_truncation": { "budget_bytes": 1 } }),
                "result_truncation",
            ),
            (json!({ "items": [], "sessions_ok": false }), "sessions_ok"),
            (
                json!({ "worktrees": { "ok": false, "error": "e" }, "ledger": { "ok": true } }),
                "worktrees.ok",
            ),
            (
                json!({ "ok": true, "agents": { "ok": true, "live": { "ok": false } } }),
                "agents.live.ok",
            ),
        ] {
            let partial = call_outcome(&text_result(&payload), false);
            assert_eq!(partial.code, exit_status::INCOMPLETE, "{payload}");
            assert!(
                !partial.stdout.is_empty(),
                "the partial answer is still printed"
            );
            assert!(
                partial.stderr.contains(named),
                "{payload}: {}",
                partial.stderr
            );
        }
        // Rows in a list are data, not facets; and flags that say "whole"
        // are not flags.
        let rows = call_outcome(
            &text_result(
                &json!({ "items": [{ "ok": false }], "ok": true, "truncated": false, "sessions_ok": true }),
            ),
            false,
        );
        assert_eq!(rows.code, exit_status::OK, "{rows:?}");

        let refused = call_outcome(
            &response(
                json!({ "content": [{ "type": "text", "text": "bad limit" }], "isError": true }),
            ),
            false,
        );
        assert_eq!(refused.code, exit_status::TOOL_ERROR);
        assert!(refused.stdout.is_empty());
        assert!(refused.stderr.contains("bad limit"));

        let unknown = JsonRpcResponse {
            jsonrpc: "2.0",
            id: json!(1),
            result: None,
            error: Some(json!({ "code": -32602, "message": "Unknown tool: x" })),
        };
        assert_eq!(call_outcome(&unknown, false).code, exit_status::USAGE);
        let internal = JsonRpcResponse {
            error: Some(json!({ "code": -32603, "message": "boom" })),
            ..unknown
        };
        assert_eq!(call_outcome(&internal, false).code, exit_status::TOOL_ERROR);
    }

    /// The call goes through the server's own request path: the schema refusal
    /// a host would get is the refusal the command line gets.
    #[test]
    fn a_call_is_validated_by_the_server_path() {
        let refused = call_outcome(&call_tool("gitpulse_status", json!({})), false);
        assert_eq!(refused.code, exit_status::TOOL_ERROR, "{refused:?}");
        assert!(refused.stderr.contains("repo_path"), "{}", refused.stderr);
        let unknown = call_outcome(&call_tool("gitpulse_nope", json!({})), false);
        assert_eq!(unknown.code, exit_status::USAGE, "{unknown:?}");
    }

    /// Every example the help text and the skills show parses, names only keys
    /// the tool takes, and carries every key its schema requires. Nothing runs.
    #[test]
    fn every_documented_example_parses_with_its_required_arguments() {
        let catalog = mcp::tools();
        let brief = |source: &str| -> Result<String, String> {
            match source {
                "-" => Ok(r#"{"repo_path":"/abs/repo","title":"..."}"#.to_owned()),
                "@claim.json" => Ok(
                    r#"{"repo_path":"/abs/repo","agent_id":"a","branch":"b","claimed_files":["src/lib.rs"]}"#
                        .to_owned(),
                ),
                other => panic!("no example reads {other}"),
            }
        };
        let examples: [&[&str]; 13] = [
            &["call", "gitpulse_insights", "repo_path=/abs/repo"],
            &[
                "call",
                "gitpulse_list_tasks",
                "repo_path=/abs/repo",
                "limit=20",
            ],
            &[
                "call",
                "gitpulse_list_tasks",
                "repo_path=/abs/repo",
                "limit=50",
            ],
            &["call", "gitpulse_add_task", "--json", "-", "--write"],
            &["call", "gitpulse_add_task", "--write", "--json", "-"],
            &[
                "call",
                "gitpulse_collision_risk",
                "repo_path=/abs/repo",
                "path=src/lib.rs",
                "--compact",
            ],
            &[
                "call",
                "gitpulse_complete_task",
                "--write",
                "repo_path=/abs/repo",
                "task_id=ft-0",
                "status=done",
                "summary=...",
            ],
            &["describe", "gitpulse_insights"],
            &["read", "gitpulse://server/manifest"],
            &[
                "call",
                "gitpulse_collision_risk",
                "repo_path=/abs/repo",
                "path=src/lib.rs",
            ],
            &["call", "gitpulse_worktree_radar", "repo_path=/abs/repo"],
            &[
                "call",
                "gitpulse_active_changes",
                "repo_path=/abs/repo",
                "limit=50",
            ],
            &[
                "call",
                "gitpulse_worktree_claim",
                "--write",
                "--json",
                "@claim.json",
            ],
        ];
        for example in examples {
            let parsed = parse_command(&args(example), &catalog, &brief)
                .unwrap_or_else(|e| panic!("{example:?}: {e}"));
            let CliCommand::Call {
                tool, arguments, ..
            } = parsed
            else {
                continue;
            };
            let schema = &find_tool(&catalog, &tool).unwrap()["inputSchema"];
            for required in schema["required"].as_array().into_iter().flatten() {
                let key = required.as_str().unwrap();
                assert!(arguments.get(key).is_some(), "{example:?} lacks {key}");
            }
            if let Some(status) = arguments.get("status") {
                let allowed = &schema["properties"]["status"]["enum"];
                assert!(
                    allowed.as_array().is_some_and(|a| a.contains(status)),
                    "{example:?}: {status} not in {allowed}"
                );
            }
        }
        assert!(parse(&["tools"]).is_ok());
    }

    /// A call that outlives its budget is reported as timed out, not waited
    /// on; one that panics is a tool error, and the process is still there to
    /// say so.
    #[test]
    fn a_call_past_its_deadline_times_out_and_a_panic_is_a_tool_error() {
        let request = || {
            cli_request(
                "tools/call",
                json!({ "name": "gitpulse_status", "arguments": {} }),
            )
        };
        let started = Instant::now();
        let slow = run_bounded(request(), Duration::from_millis(20), |ready| {
            std::thread::sleep(Duration::from_secs(5));
            ready.panicked()
        });
        assert!(matches!(slow, Bounded::TimedOut), "{slow:?}");
        assert!(
            started.elapsed() < Duration::from_secs(4),
            "it waited for the worker"
        );

        let timed_out = timed_out_outcome("gitpulse_status", Duration::from_millis(20));
        assert_eq!(timed_out.code, exit_status::TIMED_OUT);
        assert!(timed_out.stdout.is_empty());
        assert!(timed_out.stderr.contains("20 ms"), "{}", timed_out.stderr);
        assert!(
            timed_out.stderr.contains("outcome is unknown"),
            "{}",
            timed_out.stderr
        );

        let Bounded::Answered(panicked) =
            run_bounded(request(), Duration::from_secs(5), |_| panic!("injected"))
        else {
            panic!("a panic was reported as a timeout")
        };
        let outcome = call_outcome(&panicked, false);
        assert_eq!(outcome.code, exit_status::TOOL_ERROR, "{outcome:?}");
        assert!(outcome.stderr.contains("panicked"), "{}", outcome.stderr);
    }

    #[test]
    fn timeout_ms_is_a_positive_bounded_number_given_once() {
        let Ok(CliCommand::Call { timeout, .. }) = parse(&[
            "call",
            "gitpulse_status",
            "repo_path=/r",
            "--timeout-ms",
            "1500",
        ]) else {
            panic!("parse failed")
        };
        assert_eq!(timeout, Duration::from_millis(1500));
        let Ok(CliCommand::Read { timeout, .. }) = parse(&[
            "read",
            "gitpulse://server/manifest",
            "--timeout-ms",
            "600000",
        ]) else {
            panic!("parse failed")
        };
        assert_eq!(timeout, MAX_ENV_WINDOW);
        // A read waits the server's budget by default; a write the longest
        // window, since stopping it midway would leave it half done.
        let Ok(CliCommand::Call { timeout, .. }) =
            parse(&["call", "gitpulse_status", "repo_path=/r"])
        else {
            panic!("parse failed")
        };
        assert_eq!(timeout, call_budget());
        let Ok(CliCommand::Call { timeout, .. }) = parse(&[
            "call",
            "gitpulse_add_task",
            "repo_path=/r",
            "title=t",
            "--write",
        ]) else {
            panic!("parse failed")
        };
        assert_eq!(timeout, MAX_ENV_WINDOW);
        for bad in [
            &["call", "gitpulse_status", "--timeout-ms", "0"][..],
            &["call", "gitpulse_status", "--timeout-ms", "abc"],
            &["call", "gitpulse_status", "--timeout-ms", "-5"],
            &["call", "gitpulse_status", "--timeout-ms", "600001"],
            &["call", "gitpulse_status", "--timeout-ms"],
            &[
                "call",
                "gitpulse_status",
                "--timeout-ms",
                "5",
                "--timeout-ms",
                "6",
            ],
            &["read", "gitpulse://server/manifest", "--timeout-ms", "0"],
        ] {
            let refused = parse(bad).unwrap_err();
            assert!(refused.contains("--timeout-ms"), "{bad:?}: {refused}");
        }
    }

    #[test]
    fn read_takes_one_uri_and_its_own_options() {
        assert_eq!(
            parse(&["read", "gitpulse://server/manifest", "--compact"]),
            Ok(CliCommand::Read {
                uri: "gitpulse://server/manifest".into(),
                compact: true,
                timeout: call_budget()
            })
        );
        for bad in [
            &["read"][..],
            &["read", "--compact"],
            &["read", "gitpulse://a", "gitpulse://b"],
            &["read", "gitpulse://a", "--write"],
        ] {
            assert!(parse(bad).is_err(), "{bad:?}");
        }
    }

    /// `read` goes through the server's resource path: the manifest is there,
    /// and a URI the server does not address is the caller's error.
    #[test]
    fn read_answers_from_the_server_resource_path() {
        let catalog = mcp::tools();
        let read = |uri: &str| {
            execute(
                CliCommand::Read {
                    uri: uri.into(),
                    compact: true,
                    timeout: Duration::from_secs(120),
                },
                &catalog,
            )
        };
        let manifest = read("gitpulse://server/manifest");
        assert_eq!(manifest.code, exit_status::OK, "{manifest:?}");
        let parsed: Value = serde_json::from_str(&manifest.stdout).expect("JSON on stdout");
        assert!(parsed.is_object(), "{parsed}");
        let unknown = read("gitpulse://no-such-resource");
        assert_eq!(unknown.code, exit_status::USAGE, "{unknown:?}");
        assert!(unknown.stdout.is_empty());
        assert!(
            unknown.stderr.starts_with("gitpulse-mcp read:"),
            "{}",
            unknown.stderr
        );
    }

    /// The doctor reads the second line; the first stays what it was.
    #[test]
    fn the_version_names_every_one_shot_command() {
        let text = version_text();
        let mut lines = text.lines();
        assert_eq!(
            lines.next(),
            Some(format!("{} {}", mcp::SERVER_NAME, mcp::server_version()).as_str())
        );
        assert_eq!(lines.next(), Some("commands: tools, describe, call, read"));
        assert_eq!(lines.next(), None);
    }

    #[test]
    fn tools_lists_every_tool_with_its_access() {
        let catalog = mcp::tools();
        let lines = tool_lines(&catalog);
        assert_eq!(lines.lines().count(), catalog.len());
        let status = lines
            .lines()
            .find(|l| l.starts_with("gitpulse_status "))
            .unwrap();
        assert!(status.contains(" read "), "{status}");
        let add = lines
            .lines()
            .find(|l| l.starts_with("gitpulse_add_task "))
            .unwrap();
        assert!(add.contains(" write "), "{add}");
    }
}
