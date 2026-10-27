//! Best-effort copy of a delivered banner to Graft's local notification socket.
//!
//! Graft.app (the Mac side of the phone pairing) listens on a Unix socket and
//! forwards what it is given to the paired phone. This module offers it the
//! banners GitPulse has already shown. It is strictly off the delivery path:
//!
//! * the coordinator and the session notifier only *enqueue* — a bounded queue
//!   that drops when full — and never wait for the socket;
//! * one supervised thread does the socket work, each exchange inside a hard
//!   one-second budget (connect, write and read together);
//! * nothing is retried, nothing a refusal or a missing socket says reaches a
//!   delivery record, and the log names a code, never a title, text or key.
//!
//! The user's gate is Graft's own switch and per-program allow list, not a
//! GitPulse preference: an unknown program is refused `source-not-allowed` and
//! offered to the user in Graft's settings. Wire contract: Graft PROTOCOL.md
//! §10.13 "Local submission".
// Graft's socket is a Unix socket. On other targets `exchange` reports the
// peer absent without building any answer, so the answer types and parser are
// compiled but never constructed; they stay so both targets share one shape.
#![cfg_attr(not(unix), allow(dead_code))]
use serde_json::json;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU32, AtomicU64, Ordering};
use std::sync::{mpsc, OnceLock};
use std::time::Duration;

/// Names this program to Graft's allow list. Matches `[a-z0-9][a-z0-9._-]{0,31}`.
const SOURCE: &str = "gitpulse";
/// What the phone shows as the posting application.
const APP: &str = "GitPulse";
/// Environment override for the socket path.
const SOCKET_ENV: &str = "GRAFT_NOTIFY_SOCKET";
/// Under the user's home directory when the override is absent.
const DEFAULT_SOCKET: &str = "Library/Application Support/Graft/notify.sock";

/// Graft's caps, in UTF-8 bytes. Graft truncates too; cutting here first keeps
/// the request under [`MAX_REQUEST`] and the cut on a code-point boundary.
const MAX_APP: usize = 128;
const MAX_TITLE: usize = 256;
const MAX_TEXT: usize = 2048;
const MAX_KEY: usize = 128;
/// The whole request line, LF included.
const MAX_REQUEST: usize = 8192;
/// The longest answer read: `{"ok":false,"err":"source-not-allowed"}` is 40.
const MAX_ANSWER: usize = 512;

/// Connect, write and read together. A peer that stalls at any point costs the
/// worker this much and nothing else.
const BUDGET: Duration = Duration::from_secs(1);
/// Requests waiting for the worker. Graft rate-limits at 20 per 10 s per
/// program, so a longer line would only be refused later.
const QUEUE: usize = 16;

/// What to send. Display fields are capped when the request is encoded.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum Request {
    Post {
        key: String,
        title: String,
        text: String,
        silent: bool,
    },
    Remove {
        key: String,
    },
}

/// Graft's refusal codes, plus the two ways an exchange can fail before one.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Refusal {
    Disabled,
    SourceNotAllowed,
    RateLimited,
    BadRequest,
    TooLarge,
    /// A well-formed refusal naming a code this build does not know.
    Unrecognised,
}

impl Refusal {
    fn parse(code: &str) -> Self {
        match code {
            "disabled" => Self::Disabled,
            "source-not-allowed" => Self::SourceNotAllowed,
            "rate-limited" => Self::RateLimited,
            "bad-request" => Self::BadRequest,
            "too-large" => Self::TooLarge,
            _ => Self::Unrecognised,
        }
    }
    /// A fixed word per variant: what the log may say about a refusal.
    fn word(self) -> &'static str {
        match self {
            Self::Disabled => "disabled",
            Self::SourceNotAllowed => "source-not-allowed",
            Self::RateLimited => "rate-limited",
            Self::BadRequest => "bad-request",
            Self::TooLarge => "too-large",
            Self::Unrecognised => "unrecognised",
        }
    }
}

/// How one exchange ended.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Outcome {
    /// Graft answered `ok`; `sent` is whether it forwarded to the phone now.
    Accepted {
        sent: bool,
    },
    Refused(Refusal),
    /// No socket, or nobody listening on it: Graft is not running. Quiet.
    Absent,
    /// Connected, but the exchange ran out of its budget.
    TimedOut,
    /// An I/O error after connecting, or an unusable socket path.
    Io,
    /// An answer that is not one JSON object with a boolean `ok`.
    Garbled,
    /// The request could not be built (an invalid key, or too large to fit).
    Unsendable,
}

impl Outcome {
    /// The one word the log is allowed, or `None` for outcomes that are quiet
    /// by design (accepted, and Graft not running).
    fn word(self) -> Option<&'static str> {
        match self {
            Self::Accepted { .. } | Self::Absent => None,
            Self::Refused(refusal) => Some(refusal.word()),
            Self::TimedOut => Some("timeout"),
            Self::Io => Some("io-error"),
            Self::Garbled => Some("bad-answer"),
            Self::Unsendable => Some("unsendable"),
        }
    }
}

/// `s` cut to at most `max` bytes, on a code-point boundary.
fn cap(s: &str, max: usize) -> &str {
    if s.len() <= max {
        return s;
    }
    let mut end = max;
    while !s.is_char_boundary(end) {
        end -= 1;
    }
    &s[..end]
}

/// Graft's key grammar: `[A-Za-z0-9._:-]{1,128}`. Rejected there, never
/// truncated, so it is checked here instead of guessed at.
fn valid_key(key: &str) -> bool {
    !key.is_empty()
        && key.len() <= MAX_KEY
        && key
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'_' | b':' | b'-'))
}

/// The key for the banner `native` names. The identity the coordinator already
/// persists (`gitpulse.<profile>.event-N` for an activity banner,
/// `gitpulse.session.<session>` for a session banner) is already in Graft's
/// grammar, so it is used as it is: the same banner always yields the same key,
/// two banners never share one, and a `remove` finds what its `post` made.
/// Anything outside the grammar is not forwarded at all.
pub(super) fn key_for(native: &str) -> Option<String> {
    valid_key(native).then(|| native.to_owned())
}

/// The request line, LF included, or `None` when it cannot be made valid.
fn encode(request: &Request) -> Option<Vec<u8>> {
    match request {
        Request::Remove { key } => {
            if !valid_key(key) {
                return None;
            }
            line(&json!({"op":"remove","src":SOURCE,"key":key}))
        }
        Request::Post {
            key,
            title,
            text,
            silent,
        } => {
            if !valid_key(key) {
                return None;
            }
            let title = cap(title, MAX_TITLE);
            let mut text = cap(text, MAX_TEXT);
            loop {
                let value = json!({
                    "op":"post","src":SOURCE,"key":key,"app":cap(APP, MAX_APP),
                    "title":title,"text":text,"silent":silent,
                });
                match line(&value) {
                    Some(bytes) => return Some(bytes),
                    // JSON escaping can grow control characters six-fold, so a
                    // text within its own cap can still overflow the request.
                    // Shorten it rather than lose the banner.
                    None if !text.is_empty() => text = cap(text, text.len() / 2),
                    None => return None,
                }
            }
        }
    }
}

fn line(value: &serde_json::Value) -> Option<Vec<u8>> {
    let mut bytes = serde_json::to_vec(value).ok()?;
    bytes.push(b'\n');
    (bytes.len() <= MAX_REQUEST).then_some(bytes)
}

/// Graft's socket: the environment override, else the default under `$HOME`.
pub(super) fn socket_path() -> Option<PathBuf> {
    if let Some(path) = std::env::var_os(SOCKET_ENV).filter(|p| !p.is_empty()) {
        return Some(PathBuf::from(path));
    }
    let home = std::env::var_os("HOME").filter(|h| !h.is_empty())?;
    Some(Path::new(&home).join(DEFAULT_SOCKET))
}

/// Reads Graft's one-line answer into an [`Outcome`].
fn parse_answer(answer: &[u8]) -> Outcome {
    let Ok(value) = serde_json::from_slice::<serde_json::Value>(answer) else {
        return Outcome::Garbled;
    };
    match value.get("ok").and_then(serde_json::Value::as_bool) {
        Some(true) => Outcome::Accepted {
            sent: value
                .get("sent")
                .and_then(serde_json::Value::as_bool)
                .unwrap_or(false),
        },
        Some(false) => Outcome::Refused(
            value
                .get("err")
                .and_then(serde_json::Value::as_str)
                .map_or(Refusal::Unrecognised, Refusal::parse),
        ),
        None => Outcome::Garbled,
    }
}

#[cfg(unix)]
fn exchange(path: &Path, request: &[u8], budget: Duration) -> Outcome {
    use std::io::ErrorKind;
    use std::os::unix::net::UnixStream;

    let deadline = std::time::Instant::now() + budget;
    // std offers no connect timeout. On macOS a listener whose backlog is full
    // refuses at once rather than blocking, and a missing or dead socket fails
    // at once on every platform, so the budget below governs what follows.
    match UnixStream::connect(path) {
        Ok(stream) => converse(stream, request, deadline),
        Err(e) if matches!(e.kind(), ErrorKind::NotFound | ErrorKind::ConnectionRefused) => {
            Outcome::Absent
        }
        Err(_) => Outcome::Io,
    }
}

/// How an exchange that failed after connecting ended: out of its deadline, or
/// any other I/O error. `DeadlineStream` reports an expired timeout as
/// `TimedOut` on every platform, so that one kind is the deadline.
fn failed(error: &std::io::Error) -> Outcome {
    if error.kind() == std::io::ErrorKind::TimedOut {
        Outcome::TimedOut
    } else {
        Outcome::Io
    }
}

/// One request and its answer over a connected socket, inside one deadline.
///
/// The deadline, and its macOS tolerance, are `crate::deadline::DeadlineStream`'s,
/// the one owner of "every read and write of a socket gets what is left of one
/// deadline": a peer that accepts or sends a byte at a time cannot stretch the
/// exchange past the budget, and a peer that has already answered and closed
/// (macOS refuses the timeout with `EINVAL` on such a socket) still yields the
/// answer it left buffered, or end of file.
fn converse<S>(stream: S, request: &[u8], deadline: std::time::Instant) -> Outcome
where
    S: crate::deadline::Timed + std::io::Read + std::io::Write,
{
    use std::io::{ErrorKind, Read, Write};

    let mut stream = crate::deadline::DeadlineStream::new(stream, deadline);
    if let Err(e) = stream.write_all(request) {
        return failed(&e);
    }

    let mut answer = Vec::with_capacity(64);
    let mut chunk = [0u8; 128];
    loop {
        match stream.read(&mut chunk) {
            Ok(0) => break,
            Ok(n) => {
                answer.extend_from_slice(&chunk[..n]);
                if let Some(end) = answer.iter().position(|&b| b == b'\n') {
                    answer.truncate(end);
                    break;
                }
                if answer.len() > MAX_ANSWER {
                    return Outcome::Garbled;
                }
            }
            Err(e) if e.kind() == ErrorKind::Interrupted => {}
            Err(e) => return failed(&e),
        }
    }
    parse_answer(&answer)
}

/// Graft's socket is a Unix socket; elsewhere there is nothing to talk to.
#[cfg(not(unix))]
fn exchange(_path: &Path, _request: &[u8], _budget: Duration) -> Outcome {
    Outcome::Absent
}

/// Runs one request to completion on the calling thread.
fn forward_one(socket: Option<PathBuf>, request: &Request, budget: Duration) -> Outcome {
    let Some(bytes) = encode(request) else {
        return Outcome::Unsendable;
    };
    match socket {
        Some(path) => exchange(&path, &bytes, budget),
        None => Outcome::Absent,
    }
}

/// Which outcome words have been logged already, one bit each.
#[derive(Default)]
pub(super) struct LogOnce(AtomicU32);

impl LogOnce {
    /// Whether `word` is new, marking it as seen.
    fn first(&self, word: &str) -> bool {
        const WORDS: [&str; 9] = [
            "disabled",
            "source-not-allowed",
            "rate-limited",
            "bad-request",
            "too-large",
            "unrecognised",
            "timeout",
            "io-error",
            "bad-answer",
        ];
        let bit = WORDS
            .iter()
            .position(|w| *w == word)
            .map_or(1 << 31, |at| 1u32 << at);
        self.0.fetch_or(bit, Ordering::Relaxed) & bit == 0
    }

    /// Logs a failing outcome the first time its code is seen. The line holds
    /// the code and nothing the notification said.
    pub(super) fn report(&self, outcome: Outcome) {
        if let Some(word) = outcome.word() {
            if self.first(word) {
                log::warn!(target: "graft-notify", "forwarding to Graft was not accepted: {word}");
            }
        }
    }
}

/// A bounded queue and the one thread that drains it.
pub(super) struct Forwarder {
    sender: mpsc::SyncSender<Request>,
    dropped: AtomicU64,
}

impl Forwarder {
    /// Starts the worker. `socket` is read per request, so a Graft that
    /// starts, stops or moves later is found without restarting GitPulse.
    pub(super) fn start(
        socket: impl Fn() -> Option<PathBuf> + Send + 'static,
        budget: Duration,
        capacity: usize,
        report: impl Fn(Outcome) + Send + 'static,
    ) -> std::io::Result<Self> {
        let (sender, receiver) = mpsc::sync_channel::<Request>(capacity);
        crate::supervisor::spawn_once(
            "graft-notify-forward",
            "graft notification forwarding",
            move || {
                // Ends when every sender is gone, or when a panic is recorded
                // as a stopped subsystem and the queue goes with it.
                for request in receiver {
                    report(forward_one(socket(), &request, budget));
                }
            },
        )?;
        Ok(Self {
            sender,
            dropped: AtomicU64::new(0),
        })
    }

    /// Queues `request` without waiting. Returns `false` when it was dropped:
    /// the queue is full (Graft is slow or stalled) or the worker has stopped.
    pub(super) fn offer(&self, request: Request) -> bool {
        let queued = self.sender.try_send(request).is_ok();
        if !queued {
            self.dropped.fetch_add(1, Ordering::Relaxed);
        }
        queued
    }

    #[cfg(test)]
    pub(super) fn dropped(&self) -> u64 {
        self.dropped.load(Ordering::Relaxed)
    }
}

static FORWARDER: OnceLock<Option<Forwarder>> = OnceLock::new();

/// The process-wide forwarder, started on first use. `None` when its thread
/// could not start, which only means nothing is forwarded.
fn forwarder() -> Option<&'static Forwarder> {
    static LOGGED: OnceLock<LogOnce> = OnceLock::new();
    FORWARDER
        .get_or_init(|| {
            Forwarder::start(socket_path, BUDGET, QUEUE, |outcome| {
                LOGGED.get_or_init(LogOnce::default).report(outcome)
            })
            .map_err(|error| log::warn!(target: "graft-notify", "worker did not start: {error}"))
            .ok()
        })
        .as_ref()
}

/// Offers the banner GitPulse just had accepted by the OS to Graft.
/// `native` is the banner's identity; `sound` is the banner's own sound
/// setting, so a banner that was silent here is silent on the phone.
pub(super) fn post(native: &str, heading: &str, body: &str, sound: bool) {
    let (Some(key), Some(forwarder)) = (key_for(native), forwarder()) else {
        return;
    };
    forwarder.offer(Request::Post {
        key,
        title: heading.to_owned(),
        text: body.to_owned(),
        silent: !sound,
    });
}

/// Offers Graft the withdrawal of banners GitPulse has just taken down.
pub(super) fn remove(natives: &[String]) {
    let Some(forwarder) = forwarder() else {
        return;
    };
    for key in natives.iter().filter_map(|native| key_for(native)) {
        forwarder.offer(Request::Remove { key });
    }
}

#[cfg(all(test, unix))]
pub(crate) mod fake_server_tests;
#[cfg(all(test, unix))]
mod tests;
