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

use std::io::{self, BufReader, Read};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{mpsc, Arc, Mutex};
use std::time::{Duration, Instant};

use gitpulse_lib::mcp::{self, Accepted, Era, JsonRpcRequest, JsonRpcResponse, Ready};
use gitpulse_lib::ndjson;
use serde_json::{json, Value};

/// Largest single JSON-RPC message accepted, matching the harness sidecar's
/// frame cap. A legitimate request is kilobytes; anything near this is a
/// wedged or hostile peer, and the frame is dropped rather than buffered.
const MAX_FRAME_BYTES: usize = 4 * 1024 * 1024;

/// How long one request may run before the client is told it was abandoned.
///
/// This does not stop the work — a thread cannot be cancelled from outside, and
/// the git subprocesses underneath have their own 90 s ceilings. What it stops
/// is the *client* waiting: the answer goes out at the deadline and the server
/// keeps serving. Overridable so the stress tests do not have to wait a minute
/// to observe the behaviour.
const DEFAULT_CALL_BUDGET: Duration = Duration::from_secs(60);
const CALL_BUDGET_ENV: &str = "GITPULSE_MCP_CALL_TIMEOUT_MS";

/// Requests allowed to be executing at once. Past this the server says so
/// instead of spawning unbounded threads: a client that pipelines faster than
/// the disk can answer must be refused, not absorbed.
const MAX_IN_FLIGHT: usize = 32;

/// How often the watchdog looks for requests that have outlived their budget.
const WATCHDOG_TICK: Duration = Duration::from_millis(100);

/// Polling interval for the two places that wait on `in_flight`: shutdown
/// drain, and the concurrency ceiling. Both are off the hot path.
const SLOT_POLL: Duration = Duration::from_millis(2);

fn call_budget() -> Duration {
    std::env::var(CALL_BUDGET_ENV)
        .ok()
        .and_then(|raw| raw.parse::<u64>().ok())
        .filter(|ms| *ms > 0)
        .map(Duration::from_millis)
        .unwrap_or(DEFAULT_CALL_BUDGET)
}

/// The single writer. Every response goes through here so concurrent workers
/// cannot interleave bytes into the same line.
struct Wire {
    out: Result<gitpulse_lib::output::BoundedOutput, io::Error>,
    broken: AtomicBool,
}

impl Wire {
    fn new() -> Self {
        Self {
            out: gitpulse_lib::output::BoundedOutput::new(
                io::stdout(),
                "gitpulse-mcp-stdout",
                MAX_FRAME_BYTES,
                Duration::from_secs(1),
            ),
            broken: AtomicBool::new(false),
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
                self.broken.store(true, Ordering::Relaxed);
                return false;
            }
        };
        if let Err(error) = out.write(format!("{line}\n").as_bytes()) {
            log::warn!(target: "mcp", "stdout failed: {error}; stopping without retry");
            self.broken.store(true, Ordering::Relaxed);
            return false;
        }
        true
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

/// Answers any request past its deadline and drops it from the registry.
///
/// One thread for the whole process, rather than a timer per request.
fn spawn_watchdog(pending: Arc<Mutex<Vec<Pending>>>, wire: Arc<Wire>) {
    std::thread::spawn(move || loop {
        std::thread::sleep(WATCHDOG_TICK);
        if wire.broken.load(Ordering::Relaxed) {
            return;
        }
        let mut expired: Vec<JsonRpcResponse> = Vec::new();
        {
            let Ok(mut queue) = pending.lock() else {
                return;
            };
            let now = Instant::now();
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
        }
        for response in &expired {
            log::warn!(target: "mcp", "abandoned a request past its budget: {}", response.id);
            // The work keeps running; `in_flight` is released by the worker when
            // it eventually finishes, so an abandoned call still occupies a slot
            // and a storm of them is refused rather than piling up.
            let _ = wire.send(response);
        }
    });
}

/// Run one accepted request on its own thread, under a panic guard.
fn dispatch(
    ready: Ready,
    wire: Arc<Wire>,
    pending: Arc<Mutex<Vec<Pending>>>,
    in_flight: Arc<AtomicUsize>,
    budget: Duration,
) {
    let answered = Arc::new(AtomicBool::new(false));
    let timeout_response = ready.timed_out(budget);
    let panicked = ready.panicked();
    {
        let Ok(mut queue) = pending.lock() else {
            let _ = wire.send(&panicked);
            in_flight.fetch_sub(1, Ordering::AcqRel);
            return;
        };
        queue.push(Pending {
            deadline: Instant::now() + budget,
            answered: Arc::clone(&answered),
            timeout_response,
        });
    }

    let worker_answered = Arc::clone(&answered);
    let worker_in_flight = Arc::clone(&in_flight);
    let spawned = std::thread::Builder::new()
        .name("mcp-request".into())
        .spawn(move || {
            // Without this a panic anywhere under a tool call takes the whole
            // process with it, and the client sees a dead server rather than a
            // failed call. `install_panic_hook` only logs; it does not unwind.
            let outcome =
                std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| mcp::run(ready)));
            let response = match outcome {
                Ok(response) => response,
                Err(_) => panicked,
            };
            if worker_answered
                .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
                .is_ok()
            {
                wire.send(&response);
            }
            worker_in_flight.fetch_sub(1, Ordering::AcqRel);
        });

    if spawned.is_err() {
        // Out of threads. Say so rather than dropping the request silently.
        if answered
            .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
            .is_ok()
        {
            log::error!(target: "mcp", "could not spawn a worker thread");
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
        std::thread::Builder::new()
            .name("gitpulse-mcp-stdin".into())
            .spawn(move || {
                let stdin = io::stdin();
                let mut input = stdin.lock();
                let mut bytes = [0; 8192];
                loop {
                    let (message, done) = match input.read(&mut bytes) {
                        Ok(count) => (Ok(bytes[..count].to_vec()), count == 0),
                        Err(error) if error.kind() == io::ErrorKind::Interrupted => continue,
                        Err(error) => (Err(error), true),
                    };
                    if sender.send(message).is_err() || done {
                        break;
                    }
                }
            })?;
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
            match self.receiver.recv_timeout(WATCHDOG_TICK) {
                Ok(Ok(chunk)) if chunk.is_empty() => return Ok(0),
                Ok(Ok(chunk)) => self.buffered = io::Cursor::new(chunk),
                Ok(Err(error)) => return Err(error),
                Err(mpsc::RecvTimeoutError::Timeout) => continue,
                Err(mpsc::RecvTimeoutError::Disconnected) => {
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
    let pending: Arc<Mutex<Vec<Pending>>> = Arc::new(Mutex::new(Vec::new()));
    let in_flight = Arc::new(AtomicUsize::new(0));
    spawn_watchdog(Arc::clone(&pending), Arc::clone(&wire));

    let input = match HostInput::new(Arc::clone(&wire)) {
        Ok(input) => input,
        Err(error) => return Stop::StreamFault(format!("stdin worker could not start: {error}")),
    };
    let mut reader = BufReader::new(input);

    let stop = read_loop(&mut reader, &wire, &mut era, &pending, &in_flight, budget);
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
/// The grace is the call budget plus a tick, because a request that has already
/// outlived its budget was answered by the watchdog and its worker is only
/// still running to release its slot.
fn drain_in_flight(in_flight: &AtomicUsize, budget: Duration) {
    let deadline = Instant::now() + budget + WATCHDOG_TICK;
    while in_flight.load(Ordering::Acquire) > 0 && Instant::now() < deadline {
        std::thread::sleep(SLOT_POLL);
    }
}

fn read_loop<R: std::io::BufRead>(
    reader: &mut R,
    wire: &Arc<Wire>,
    era: &mut Era,
    pending: &Arc<Mutex<Vec<Pending>>>,
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
                    Arc::clone(pending),
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

fn main() {
    gitpulse_lib::logging::init();
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
    //
    // Not waiting on a worker is not the same as leaving its subprocess
    // running, though, and `process::exit` runs no destructor that would tell
    // the difference. So the children themselves are taken down first: this
    // returns immediately when none are registered, and otherwise costs one
    // `procguard::CHILD_GRACE` — the window in which `git` removes its own
    // `.git/index.lock` instead of leaving it for the user to find.
    let sweep = gitpulse_lib::procguard::reap_all(gitpulse_lib::procguard::CHILD_GRACE);
    if sweep.is_complete() {
        log::info!(target: "shutdown", "{}", sweep.describe());
    } else {
        log::warn!(target: "shutdown", "{}", sweep.describe());
    }
    gitpulse_lib::procguard::exit(code);
}
