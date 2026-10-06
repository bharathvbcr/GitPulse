//! One exchange with a running Lappi agent, read as one of six closed outcomes.
//!
//! The reference client is `qd_runtime::oneshot::ask_over_socket` in
//! Lappi-decision; this replicates its transport (one request line out, one
//! reply line back, one deadline for the whole exchange) and adds the caller
//! side `docs/caller-contract.md` §2 asks for: every reply, and every way of
//! not getting one, maps to exactly one [`Outcome`]. There is no in-process
//! fallback (§1): no agent means [`Outcome::Unavailable`], and the caller keeps
//! its own path.

use std::io::{self, BufRead};
use std::path::{Path, PathBuf};
use std::time::Duration;

use serde_json::{Map, Value};

/// The agent's request-line cap, mirrored.
///
/// Mirrors `MAX_PAYLOAD_BYTES` in Lappi-decision `crates/qd-runtime/src/wire.rs`
/// (1_572_864 at line 64 on 2026-10-06). A line over it is never sent: the agent
/// would refuse it and close the connection (`docs/caller-contract.md` §1).
/// The reply is read bounded to the same cap.
pub const MAX_PAYLOAD_BYTES: usize = 1_572_864;

/// The only `schema_version` this client speaks.
pub const SCHEMA_VERSION: u64 = 1;

/// Overrides the socket path, ahead of the default.
pub const SOCKET_ENV: &str = "LAPPI_SOCKET";

/// The default socket, relative to `$HOME` (`qd serve` and `qd-metal-serve`
/// both default to it).
pub const SOCKET_RELATIVE_TO_HOME: &str = "Library/Caches/qd/qd.sock";

/// A refusal or error `kind`, and a `backend` name, are bounded before they are
/// believed: they reach a caller record, whose checker caps them at 256.
const MAX_KIND_CHARS: usize = 64;
const MAX_BACKEND_CHARS: usize = 256;

/// Why there was no reply to read (`docs/caller-contract.md` §2, `unavailable`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UnavailableReason {
    SocketNotFound,
    ConnectRefused,
    Deadline,
    ClosedWithoutReply,
    ReplyOverCap,
    ReplyUnparseable,
}

impl UnavailableReason {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::SocketNotFound => "socket_not_found",
            Self::ConnectRefused => "connect_refused",
            Self::Deadline => "deadline",
            Self::ClosedWithoutReply => "closed_without_reply",
            Self::ReplyOverCap => "reply_over_cap",
            Self::ReplyUnparseable => "reply_unparseable",
        }
    }
}

/// Why GitPulse chose not to send (`docs/caller-contract.md` §2, `not_asked`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NotAskedReason {
    /// The "Ask Lappi on ambiguous commit types" setting is off.
    Disabled,
    /// The classifier already typed the change: not a decision point.
    ClassifierTyped,
    /// The staged patch was cut: it is not "the staged unified diff".
    PatchTruncated,
    /// The request line would be over [`MAX_PAYLOAD_BYTES`].
    OverPayloadCap,
    /// No random record id could be drawn, so no `example_id` could be named.
    NoRecordId,
    /// The request could not be serialised.
    RequestNotBuilt,
    /// This platform offers no Unix socket.
    Unsupported,
}

impl NotAskedReason {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Disabled => "disabled",
            Self::ClassifierTyped => "classifier_typed",
            Self::PatchTruncated => "patch_truncated",
            Self::OverPayloadCap => "over_payload_cap",
            Self::NoRecordId => "no_record_id",
            Self::RequestNotBuilt => "request_not_built",
            Self::Unsupported => "unsupported_platform",
        }
    }
}

/// The six readings, closed. A caller matches all of them.
#[derive(Debug, Clone, PartialEq)]
pub enum Outcome {
    /// `status: ok`, not degraded, at least one slot not `noul`.
    ModelAnswered {
        backend: String,
        slots: Map<String, Value>,
    },
    /// `status: ok` with every slot `noul`, or `degraded: true` (a reference
    /// backend's answer is not a model's).
    ModelAbstained {
        backend: String,
        slots: Map<String, Value>,
    },
    /// `status: refused`.
    RequestRefused {
        kind: String,
    },
    /// `status: error`.
    BackendFailed {
        kind: String,
    },
    Unavailable {
        reason: UnavailableReason,
    },
    NotAsked {
        reason: NotAskedReason,
    },
}

impl Outcome {
    /// The reading's wire spelling (`caller_record::READINGS`).
    pub fn reading(&self) -> &'static str {
        match self {
            Self::ModelAnswered { .. } => "model_answered",
            Self::ModelAbstained { .. } => "model_abstained",
            Self::RequestRefused { .. } => "request_refused",
            Self::BackendFailed { .. } => "backend_failed",
            Self::Unavailable { .. } => "unavailable",
            Self::NotAsked { .. } => "not_asked",
        }
    }

    pub fn asked(&self) -> bool {
        !matches!(self, Self::NotAsked { .. })
    }

    /// The `kind` a caller record names: a refusal's or error's kind, or why
    /// the agent was unavailable. `None` for an answer and for `not_asked`.
    pub fn kind(&self) -> Option<&str> {
        match self {
            Self::RequestRefused { kind } | Self::BackendFailed { kind } => Some(kind.as_str()),
            Self::Unavailable { reason } => Some(reason.as_str()),
            Self::ModelAnswered { .. } | Self::ModelAbstained { .. } | Self::NotAsked { .. } => {
                None
            }
        }
    }

    /// The answer's backend and slots, for an answer only.
    pub fn answer(&self) -> Option<(&str, &Map<String, Value>)> {
        match self {
            Self::ModelAnswered { backend, slots } | Self::ModelAbstained { backend, slots } => {
                Some((backend.as_str(), slots))
            }
            _ => None,
        }
    }

    /// The value a model chose for a `choice` slot. Only a real answer has one:
    /// an abstention, a degraded answer and every non-answer return `None`.
    pub fn chosen(&self, slot: &str) -> Option<&str> {
        let Self::ModelAnswered { slots, .. } = self else {
            return None;
        };
        let answer = slots.get(slot)?.as_object()?;
        if answer.get("noul").and_then(Value::as_bool) != Some(false) {
            return None;
        }
        answer.get("value")?.as_str()
    }
}

/// `LAPPI_SOCKET`, else `$HOME/Library/Caches/qd/qd.sock`, else `None`.
///
/// Never a relative fallback: with `$HOME` unset there is no socket to name.
pub fn socket_path() -> Option<PathBuf> {
    if let Some(explicit) = std::env::var_os(SOCKET_ENV) {
        if !explicit.is_empty() {
            return Some(PathBuf::from(explicit));
        }
    }
    let home = std::env::var_os("HOME")?;
    if home.is_empty() {
        return None;
    }
    Some(PathBuf::from(home).join(SOCKET_RELATIVE_TO_HOME))
}

/// Send one request line to the agent at `socket` and read its one reply.
///
/// `timeout` bounds the whole exchange: connect, write and the complete reply
/// line share one deadline, so an agent that drips its reply a byte at a time
/// cannot hold the caller past it. `UnixStream::connect` itself is a blocking
/// call std cannot interrupt; its time is charged to the deadline, and a
/// connect that used it all fails `deadline` before anything is written. On
/// macOS a listener with a full backlog refuses at once rather than blocking.
#[cfg(unix)]
pub fn ask(socket: &Path, line: &[u8], timeout: Duration) -> Outcome {
    use std::io::{BufReader, Write};
    use std::os::unix::net::UnixStream;
    use std::time::Instant;

    if line.len() > MAX_PAYLOAD_BYTES {
        return Outcome::NotAsked {
            reason: NotAskedReason::OverPayloadCap,
        };
    }
    // std refuses a zero socket timeout, but only after the connect: refuse
    // before it, so the agent never sees a connection that sends nothing.
    if timeout.is_zero() {
        return Outcome::Unavailable {
            reason: UnavailableReason::Deadline,
        };
    }
    let deadline = Instant::now() + timeout;
    let stream = match UnixStream::connect(socket) {
        Ok(stream) => stream,
        Err(error) => {
            return Outcome::Unavailable {
                reason: connect_reason(&error),
            }
        }
    };
    let mut stream = DeadlineStream { stream, deadline };

    // One write of the framed line: a request and its newline are one message.
    let mut framed = Vec::with_capacity(line.len() + 1);
    framed.extend_from_slice(line);
    framed.push(b'\n');
    // A failed write is not yet the outcome: an agent over its connection cap
    // writes `overloaded` the moment it accepts and closes, so the request can
    // meet a closed socket while that reply sits unread. Read it; only a socket
    // with nothing to read reports the write's failure.
    let written = stream.write_all(&framed).and_then(|()| stream.flush());

    let mut reader = BufReader::new(stream);
    match (read_reply_line(&mut reader, MAX_PAYLOAD_BYTES), written) {
        (Ok(reply), _) => parse_reply(&reply),
        (Err(_), Err(error)) => Outcome::Unavailable {
            reason: exchange_reason(&error),
        },
        (Err(reason), Ok(())) => Outcome::Unavailable { reason },
    }
}

/// No Unix socket on this platform, so nothing is sent.
#[cfg(not(unix))]
pub fn ask(_socket: &Path, _line: &[u8], _timeout: Duration) -> Outcome {
    Outcome::NotAsked {
        reason: NotAskedReason::Unsupported,
    }
}

/// A `UnixStream` whose every read and write is bounded by one deadline.
///
/// `set_read_timeout` bounds a single `read(2)`; an agent that sends one byte
/// just inside it resets it each time. Here each operation is given only what
/// is left of the deadline, and one that would start with nothing left fails
/// `TimedOut` without touching the socket. The same shape as the reference
/// client's `DeadlineStream` (Lappi-decision `crates/qd-runtime/src/oneshot.rs`).
#[cfg(unix)]
struct DeadlineStream {
    stream: std::os::unix::net::UnixStream,
    deadline: std::time::Instant,
}

#[cfg(unix)]
impl DeadlineStream {
    fn remaining(&self) -> io::Result<Duration> {
        let left = self
            .deadline
            .saturating_duration_since(std::time::Instant::now());
        if left.is_zero() {
            return Err(io::Error::new(
                io::ErrorKind::TimedOut,
                "the Lappi exchange ran past its deadline",
            ));
        }
        Ok(left)
    }

    /// macOS reports an expired socket timeout as `EAGAIN` (`WouldBlock`),
    /// Linux as `TimedOut`; both mean the deadline passed.
    fn timed_out(error: io::Error) -> io::Error {
        if error.kind() == io::ErrorKind::WouldBlock {
            io::Error::new(
                io::ErrorKind::TimedOut,
                "the Lappi exchange ran past its deadline",
            )
        } else {
            error
        }
    }
}

/// Apply a timeout, tolerating the one failure that is not a failure: macOS
/// refuses `SO_RCVTIMEO`/`SO_SNDTIMEO` with `EINVAL` (`InvalidInput`) on a
/// socket the peer has already shut down, which is exactly when an agent has
/// written its reply and closed. On such a socket a read returns the buffered
/// bytes or EOF at once and a write fails at once, so carrying on cannot block,
/// and the timeout already set by this exchange is never past the deadline.
/// Mirrors Lappi-decision `crates/qd-runtime/src/deadline.rs`.
#[cfg(unix)]
fn apply_timeout(result: io::Result<()>) -> io::Result<()> {
    match result {
        Err(error) if error.kind() == io::ErrorKind::InvalidInput => Ok(()),
        other => other,
    }
}

#[cfg(unix)]
impl io::Read for DeadlineStream {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        let left = self.remaining()?;
        apply_timeout(self.stream.set_read_timeout(Some(left)))?;
        self.stream.read(buf).map_err(Self::timed_out)
    }
}

#[cfg(unix)]
impl io::Write for DeadlineStream {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        let left = self.remaining()?;
        apply_timeout(self.stream.set_write_timeout(Some(left)))?;
        self.stream.write(buf).map_err(Self::timed_out)
    }

    fn flush(&mut self) -> io::Result<()> {
        self.stream.flush()
    }
}

#[cfg(unix)]
fn connect_reason(error: &io::Error) -> UnavailableReason {
    match error.kind() {
        io::ErrorKind::NotFound => UnavailableReason::SocketNotFound,
        io::ErrorKind::TimedOut | io::ErrorKind::WouldBlock => UnavailableReason::Deadline,
        // Refused, permission denied, a path that is not a socket: something is
        // there and would not take the connection.
        _ => UnavailableReason::ConnectRefused,
    }
}

fn exchange_reason(error: &io::Error) -> UnavailableReason {
    match error.kind() {
        io::ErrorKind::TimedOut | io::ErrorKind::WouldBlock => UnavailableReason::Deadline,
        // A reset, a broken pipe or EOF mid-write: the agent went away before
        // a whole reply line arrived.
        _ => UnavailableReason::ClosedWithoutReply,
    }
}

/// Read one reply line, bounded to `cap` bytes before its newline.
///
/// Unlike the reference `read_one_line`, this keeps whether the newline was
/// seen: a connection closed before one is `closed_without_reply` even when
/// some bytes arrived (`docs/caller-contract.md` §1), never a short reply that
/// then fails to parse.
fn read_reply_line<R: BufRead>(reader: R, cap: usize) -> Result<Vec<u8>, UnavailableReason> {
    let mut buf = Vec::new();
    let mut limited = reader.take(cap as u64 + 1);
    limited
        .read_until(b'\n', &mut buf)
        .map_err(|error| exchange_reason(&error))?;
    if buf.last() == Some(&b'\n') {
        buf.pop();
        if buf.len() > cap {
            return Err(UnavailableReason::ReplyOverCap);
        }
        return Ok(buf);
    }
    if buf.len() > cap {
        Err(UnavailableReason::ReplyOverCap)
    } else {
        Err(UnavailableReason::ClosedWithoutReply)
    }
}

/// Read one reply line as an outcome. Anything not exactly one of the three
/// documented envelopes is `unavailable` / `reply_unparseable`, never a guess.
pub fn parse_reply(line: &[u8]) -> Outcome {
    parse_envelope(line).unwrap_or(Outcome::Unavailable {
        reason: UnavailableReason::ReplyUnparseable,
    })
}

fn parse_envelope(line: &[u8]) -> Option<Outcome> {
    let value: Value = serde_json::from_slice(line).ok()?;
    let envelope = value.as_object()?;
    if envelope.get("schema_version")?.as_u64()? != SCHEMA_VERSION {
        return None;
    }
    match envelope.get("status")?.as_str()? {
        "ok" => parse_answer(envelope),
        "refused" => {
            exact_keys(
                envelope,
                &["status", "schema_version", "refusal", "message"],
            )?;
            envelope.get("message")?.as_str()?;
            let kind = typed_kind(envelope.get("refusal")?)?;
            Some(Outcome::RequestRefused { kind })
        }
        "error" => {
            exact_keys(envelope, &["status", "schema_version", "error", "message"])?;
            envelope.get("message")?.as_str()?;
            let kind = typed_kind(envelope.get("error")?)?;
            Some(Outcome::BackendFailed { kind })
        }
        _ => None,
    }
}

fn parse_answer(envelope: &Map<String, Value>) -> Option<Outcome> {
    exact_keys(
        envelope,
        &["status", "schema_version", "backend", "degraded", "slots"],
    )?;
    let backend = envelope.get("backend")?.as_str()?;
    if backend.is_empty() || backend.chars().count() > MAX_BACKEND_CHARS {
        return None;
    }
    let degraded = envelope.get("degraded")?.as_bool()?;
    let slots = envelope.get("slots")?.as_object()?;
    if slots.is_empty() {
        return None;
    }
    let mut every_noul = true;
    for slot in slots.values() {
        let slot = slot.as_object()?;
        exact_keys(
            slot,
            &["value", "conformal_set", "score", "noul", "degraded"],
        )?;
        let noul = slot.get("noul")?.as_bool()?;
        // `value` is absent if and only if `noul` (`docs/schema-api.md`).
        if slot.get("value")?.is_null() != noul {
            return None;
        }
        // `degraded` is mirrored from the envelope, never per-slot.
        if slot.get("degraded")?.as_bool()? != degraded {
            return None;
        }
        let score = slot.get("score")?.as_f64()?;
        if !(0.0..=1.0).contains(&score) {
            return None;
        }
        match slot.get("conformal_set")? {
            Value::Null | Value::Array(_) => {}
            _ => return None,
        }
        every_noul &= noul;
    }
    let backend = backend.to_string();
    let slots = slots.clone();
    if degraded || every_noul {
        Some(Outcome::ModelAbstained { backend, slots })
    } else {
        Some(Outcome::ModelAnswered { backend, slots })
    }
}

/// `{"kind": "<identifier>", ...}` → the identifier.
fn typed_kind(value: &Value) -> Option<String> {
    let kind = value.as_object()?.get("kind")?.as_str()?;
    let shaped = !kind.is_empty()
        && kind.chars().count() <= MAX_KIND_CHARS
        && kind
            .chars()
            .all(|ch| ch.is_ascii_lowercase() || ch.is_ascii_digit() || ch == '_' || ch == '-');
    shaped.then(|| kind.to_string())
}

fn exact_keys(object: &Map<String, Value>, expected: &[&str]) -> Option<()> {
    let known = object.keys().all(|key| expected.contains(&key.as_str()));
    let complete = expected.iter().all(|key| object.contains_key(*key));
    (known && complete).then_some(())
}

#[cfg(test)]
mod tests {
    use super::*;

    const ANSWER_FIX: &str = r#"{"status":"ok","schema_version":1,"backend":"qd-metal/qwen3.5-2b-base/tessl","degraded":false,"slots":{"commit_type":{"value":"fix","conformal_set":["fix"],"score":0.8,"noul":false,"degraded":false}}}"#;
    const ANSWER_NOUL: &str = r#"{"status":"ok","schema_version":1,"backend":"qd-metal/qwen3.5-2b-base/tessl","degraded":false,"slots":{"commit_type":{"value":null,"conformal_set":null,"score":0.01,"noul":true,"degraded":false}}}"#;
    const ANSWER_DEGRADED: &str = r#"{"status":"ok","schema_version":1,"backend":"reference-deterministic-v1","degraded":true,"slots":{"commit_type":{"value":"feat","conformal_set":["feat"],"score":0.7,"noul":false,"degraded":true}}}"#;
    const REFUSED: &str = r#"{"status":"refused","schema_version":1,"refusal":{"kind":"task_not_trained","task":"gitpulse.commit_type","available":["code.defect_class"]},"message":"not trained"}"#;
    const ERROR: &str = r#"{"status":"error","schema_version":1,"error":{"kind":"overloaded","limit":8},"message":"busy"}"#;

    #[test]
    fn each_documented_envelope_reads_as_its_outcome() {
        let answered = parse_reply(ANSWER_FIX.as_bytes());
        assert_eq!(answered.reading(), "model_answered");
        assert_eq!(answered.chosen("commit_type"), Some("fix"));
        assert_eq!(answered.kind(), None);

        let abstained = parse_reply(ANSWER_NOUL.as_bytes());
        assert_eq!(abstained.reading(), "model_abstained");
        assert_eq!(abstained.chosen("commit_type"), None);

        assert_eq!(
            parse_reply(REFUSED.as_bytes()),
            Outcome::RequestRefused {
                kind: "task_not_trained".into()
            }
        );
        assert_eq!(
            parse_reply(ERROR.as_bytes()),
            Outcome::BackendFailed {
                kind: "overloaded".into()
            }
        );
    }

    #[test]
    fn a_degraded_answer_is_an_abstention_and_offers_no_choice() {
        let outcome = parse_reply(ANSWER_DEGRADED.as_bytes());
        assert_eq!(outcome.reading(), "model_abstained");
        assert_eq!(outcome.chosen("commit_type"), None);
        let (backend, _) = outcome
            .answer()
            .expect("an abstention still carries its answer");
        assert_eq!(backend, "reference-deterministic-v1");
    }

    #[test]
    fn anything_off_the_documented_shapes_is_unparseable_not_a_guess() {
        let unparseable = Outcome::Unavailable {
            reason: UnavailableReason::ReplyUnparseable,
        };
        let cases = [
            "not json",
            "[]",
            "{}",
            r#"{"status":"maybe","schema_version":1}"#,
            r#"{"status":"ok","schema_version":2,"backend":"b","degraded":false,"slots":{}}"#,
            // An answer with no slots answers nothing.
            r#"{"status":"ok","schema_version":1,"backend":"b","degraded":false,"slots":{}}"#,
            // value present while noul: one fact spelled two ways.
            r#"{"status":"ok","schema_version":1,"backend":"b","degraded":false,"slots":{"commit_type":{"value":"fix","conformal_set":null,"score":0.5,"noul":true,"degraded":false}}}"#,
            // value absent while not noul.
            r#"{"status":"ok","schema_version":1,"backend":"b","degraded":false,"slots":{"commit_type":{"value":null,"conformal_set":null,"score":0.5,"noul":false,"degraded":false}}}"#,
            // An unknown envelope key is refused, as the runtime refuses one.
            r#"{"status":"ok","schema_version":1,"backend":"b","degraded":false,"slots":{"commit_type":{"value":"fix","conformal_set":null,"score":0.5,"noul":false,"degraded":false}},"extra":1}"#,
            // Per-slot degraded disagreeing with the envelope.
            r#"{"status":"ok","schema_version":1,"backend":"b","degraded":false,"slots":{"commit_type":{"value":"fix","conformal_set":null,"score":0.5,"noul":false,"degraded":true}}}"#,
            // A refusal whose kind is not an identifier.
            r#"{"status":"refused","schema_version":1,"refusal":{"kind":"Not An Id"},"message":"m"}"#,
            r#"{"status":"refused","schema_version":1,"refusal":{},"message":"m"}"#,
            r#"{"status":"error","schema_version":1,"error":{"kind":"x"}}"#,
        ];
        for case in cases {
            assert_eq!(parse_reply(case.as_bytes()), unparseable, "{case}");
        }
    }

    #[test]
    fn a_close_before_the_newline_is_closed_without_reply_even_with_bytes() {
        let partial: &[u8] = br#"{"status":"ok""#;
        assert_eq!(
            read_reply_line(partial, 64),
            Err(UnavailableReason::ClosedWithoutReply)
        );
        let nothing: &[u8] = b"";
        assert_eq!(
            read_reply_line(nothing, 64),
            Err(UnavailableReason::ClosedWithoutReply)
        );
        let over: &[u8] = &[b'x'; 65];
        assert_eq!(
            read_reply_line(over, 64),
            Err(UnavailableReason::ReplyOverCap)
        );
        let exact: Vec<u8> = [vec![b'y'; 64], vec![b'\n']].concat();
        assert_eq!(read_reply_line(exact.as_slice(), 64), Ok(vec![b'y'; 64]));
    }

    #[test]
    fn the_mirrored_cap_is_the_runtimes() {
        // A cross-repository pin against Lappi-decision's
        // crates/qd-runtime/src/wire.rs, run when LAPPI_DECISION_DIR names a
        // checkout. Without one it says so rather than passing quietly.
        let Some(root) = std::env::var_os("LAPPI_DECISION_DIR") else {
            eprintln!("payload-cap cross-check NOT RUN: LAPPI_DECISION_DIR is unset");
            return;
        };
        let wire = Path::new(&root).join("crates/qd-runtime/src/wire.rs");
        let text = std::fs::read_to_string(&wire)
            .unwrap_or_else(|error| panic!("{} unreadable: {error}", wire.display()));
        assert!(
            text.contains(&format!(
                "pub const MAX_PAYLOAD_BYTES: usize = {};",
                "1_572_864"
            )),
            "wire.rs no longer declares MAX_PAYLOAD_BYTES = 1_572_864; re-mirror it"
        );
        assert_eq!(MAX_PAYLOAD_BYTES, 1_572_864);
    }
}
