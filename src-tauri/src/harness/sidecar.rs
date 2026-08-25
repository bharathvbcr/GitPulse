//! The `manvi serve` sidecar: one long-lived child process, NDJSON over stdio.
//!
//! Three properties are load-bearing here.
//!
//! * **The user's repository is never touched.** Every MANVI command first
//!   "prepares the repository it is standing in" — it creates a state
//!   directory and appends managed rules to `.gitignore`. A Git client that
//!   silently rewrote the `.gitignore` of every repository the user opened
//!   would be indefensible, so the child is spawned in a scratch directory of
//!   ours with `MANVI_HARNESS_INIT_ENABLED=false` and `MANVI_STATE_DIR`
//!   pointed away from the repository. The repository reaches the harness only
//!   as the `root` parameter of a policy call, which is read, never written.
//! * **A call that could not run is never reported as one that passed.** A
//!   missing binary, a dead child, a timeout and a refusal are four different
//!   outcomes and stay four different outcomes all the way to the UI.
//! * **Dispatch is serial**, because the harness's own dispatch is serial: it
//!   writes exactly one response line per request and handles them in order.
//!   Holding the lock across the round trip is what makes a stray line
//!   attributable to the request that provoked it.

use std::collections::VecDeque;
use std::io::{BufRead, BufReader, Write};
use std::path::PathBuf;
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError};
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant};

use serde::Serialize;
use serde_json::Value;

use super::protocol::*;

/// How long a single request may take before the child is considered wedged.
pub const DEFAULT_CALL_TIMEOUT: Duration = Duration::from_secs(15);
/// Handshake budget. A cold `manvi serve` has to resolve its configuration
/// before it answers, so this is deliberately looser than a warm call.
const HELLO_TIMEOUT: Duration = Duration::from_secs(20);
/// How long a failed start is remembered before another spawn is attempted.
/// Without it, a machine with no `manvi` installed would fork a process per
/// status poll.
const RESPAWN_BACKOFF: Duration = Duration::from_secs(30);
/// Lines of the child's stderr kept for diagnostics.
const STDERR_TAIL_LINES: usize = 40;
/// Refuse to send a line the harness would refuse anyway (its cap is 8 MiB).
/// Ours is lower on purpose: a request this large is a defect in our prompt
/// budgeting, and finding it here is cheaper than finding it as an E_TOO_LARGE.
const MAX_REQUEST_BYTES: usize = 4 * 1024 * 1024;

/// Why the harness is not answering, in terms a user can act on.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HarnessError {
    /// No `manvi` binary was found.
    NotInstalled(String),
    /// The binary exists but the sidecar could not be started or handshaken.
    Unavailable(String),
    /// The call went out and the child did not answer in time.
    Timeout(String),
    /// The harness answered with `ok:false`.
    Refused(WireError),
    /// The harness answered something this client could not decode.
    Protocol(String),
}

impl HarnessError {
    pub fn message(&self) -> String {
        match self {
            HarnessError::NotInstalled(m) => m.clone(),
            HarnessError::Unavailable(m) => m.clone(),
            HarnessError::Timeout(m) => m.clone(),
            HarnessError::Refused(e) => e.to_string(),
            HarnessError::Protocol(m) => m.clone(),
        }
    }

    pub fn code(&self) -> &'static str {
        match self {
            HarnessError::NotInstalled(_) => "not_installed",
            HarnessError::Unavailable(_) => "unavailable",
            HarnessError::Timeout(_) => "timeout",
            HarnessError::Refused(_) => "refused",
            HarnessError::Protocol(_) => "protocol",
        }
    }
}

impl std::fmt::Display for HarnessError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.message())
    }
}

/// A running sidecar and the handshake it answered with.
struct Sidecar {
    child: Child,
    stdin: ChildStdin,
    lines: Receiver<String>,
    stderr: std::sync::Arc<Mutex<VecDeque<String>>>,
    hello: HelloResult,
    binary: String,
    next_id: u64,
}

impl Drop for Sidecar {
    fn drop(&mut self) {
        // Closing stdin is the documented clean shutdown; killing is the
        // fallback for a child that ignores EOF.
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

impl Sidecar {
    fn call(
        &mut self,
        op: &str,
        params: Option<Value>,
        timeout: Duration,
    ) -> Result<Value, HarnessError> {
        self.next_id += 1;
        let id = self.next_id.to_string();
        let line = serde_json::to_string(&Request {
            id: id.clone(),
            op: op.to_string(),
            params,
        })
        .map_err(|e| HarnessError::Protocol(format!("could not encode {} request: {}", op, e)))?;

        if line.len() > MAX_REQUEST_BYTES {
            return Err(HarnessError::Protocol(format!(
                "{} request is {} bytes, past this client's {} byte cap",
                op,
                line.len(),
                MAX_REQUEST_BYTES
            )));
        }

        self.stdin
            .write_all(line.as_bytes())
            .and_then(|_| self.stdin.write_all(b"\n"))
            .and_then(|_| self.stdin.flush())
            .map_err(|e| HarnessError::Unavailable(format!("sidecar stdin closed: {}", e)))?;

        let deadline = Instant::now() + timeout;
        loop {
            let remaining = deadline.saturating_duration_since(Instant::now());
            if remaining.is_zero() {
                return Err(HarnessError::Timeout(format!(
                    "{} did not answer within {:?}{}",
                    op,
                    timeout,
                    self.stderr_hint()
                )));
            }
            match self.lines.recv_timeout(remaining) {
                Ok(raw) => match classify_line(&raw, &id) {
                    LineOutcome::Ignore => {
                        // Noise, a log line, or another call's answer: none of
                        // these fault a healthy sidecar, so keep waiting
                        // within the same deadline.
                        continue;
                    }
                    LineOutcome::Answer(response) => {
                        if !response.ok {
                            return Err(HarnessError::Refused(response.error.unwrap_or(
                                WireError {
                                    code: "E_INTERNAL".into(),
                                    message: format!("{} failed without an error body", op),
                                    retryable: false,
                                },
                            )));
                        }
                        return Ok(response.result.unwrap_or(Value::Null));
                    }
                    LineOutcome::Fault(message) => {
                        // Addressed to this very call yet undecodable: the
                        // protocol itself is broken and retrying the read
                        // cannot help.
                        return Err(HarnessError::Protocol(message));
                    }
                },
                Err(RecvTimeoutError::Timeout) => continue,
                Err(RecvTimeoutError::Disconnected) => {
                    return Err(HarnessError::Unavailable(format!(
                        "sidecar exited during {}{}",
                        op,
                        self.stderr_hint()
                    )))
                }
            }
        }
    }

    fn stderr_hint(&self) -> String {
        let tail = self
            .stderr
            .lock()
            .map(|t| t.iter().cloned().collect::<Vec<_>>())
            .unwrap_or_default();
        match tail.last() {
            Some(last) if !last.is_empty() => {
                format!(" (last diagnostic: {})", truncate(last, 200))
            }
            _ => String::new(),
        }
    }
}

/// What one raw sidecar stdout line means for the call waiting on it.
#[derive(Debug)]
enum LineOutcome {
    /// Log chatter, an undecodable line, or a response to some other call:
    /// none of these implicate the protocol, so the caller keeps waiting
    /// within its deadline instead of faulting a healthy sidecar.
    Ignore,
    /// The answer to this call.
    Answer(Response),
    /// A line addressed to this very call yet unparseable: the protocol
    /// itself is broken and the sidecar must be dropped.
    Fault(String),
}

/// Pure decision core of the read loop, so the ignore-vs-fault policy is
/// unit-testable without spawning a child.
fn classify_line(raw: &str, expected_id: &str) -> LineOutcome {
    match serde_json::from_str::<Response>(raw) {
        Ok(response) => {
            if response.id == expected_id {
                LineOutcome::Answer(response)
            } else {
                // Serial dispatch means a foreign id is either a leftover from
                // an earlier call or harness noise; it is not ours to act on.
                LineOutcome::Ignore
            }
        }
        Err(e) => {
            // Only treat the line as a breach when it carries OUR id: that is
            // the harness answering us in a shape we cannot read. Anything
            // else (plain text, partial JSON, other ids) stays noise.
            let addressed_to_us = serde_json::from_str::<Value>(raw)
                .ok()
                .and_then(|v| v.get("id").and_then(Value::as_str).map(String::from))
                .is_some_and(|id| id == expected_id);
            if addressed_to_us {
                LineOutcome::Fault(format!(
                    "sidecar answered {} with an undecodable response ({}): {}",
                    expected_id,
                    e,
                    truncate(raw, 200)
                ))
            } else {
                LineOutcome::Ignore
            }
        }
    }
}

fn truncate(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        return s.to_string();
    }
    s.chars().take(max).collect::<String>() + "…"
}

/// Where the sidecar is allowed to keep its state: never inside the user's
/// repository.
fn scratch_dir() -> PathBuf {
    let dir = std::env::temp_dir().join("gitpulse-harness");
    let _ = std::fs::create_dir_all(&dir);
    dir
}

/// Resolves the `manvi` binary: explicit override, then `PATH`, then the
/// conventional user-local install directory.
pub fn resolve_binary() -> Option<String> {
    if let Ok(explicit) = std::env::var("GITPULSE_MANVI_BIN") {
        let path = PathBuf::from(&explicit);
        if path.is_file() {
            return Some(explicit);
        }
        return None;
    }
    if let Ok(path_var) = std::env::var("PATH") {
        for dir in std::env::split_paths(&path_var) {
            let candidate = dir.join("manvi");
            if candidate.is_file() {
                return Some(candidate.to_string_lossy().into_owned());
            }
        }
    }
    if let Some(home) = std::env::var_os("HOME") {
        let candidate = PathBuf::from(home).join(".local/bin/manvi");
        if candidate.is_file() {
            return Some(candidate.to_string_lossy().into_owned());
        }
    }
    None
}

fn spawn() -> Result<Sidecar, HarnessError> {
    let binary = resolve_binary().ok_or_else(|| {
        HarnessError::NotInstalled(
            "no `manvi` binary on PATH, in ~/.local/bin, or named by GITPULSE_MANVI_BIN".into(),
        )
    })?;

    let dir = scratch_dir();
    let mut child = Command::new(&binary)
        .args(["serve", "--posture", "host"])
        .current_dir(&dir)
        // Both of these keep the harness out of the working tree: the first
        // stops it preparing a repository at all, the second keeps its state
        // out of whatever directory it did start in.
        .env("MANVI_HARNESS_INIT_ENABLED", "false")
        .env("MANVI_STATE_DIR", dir.join("state"))
        .env("NO_COLOR", "1")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| {
            HarnessError::Unavailable(format!("could not start `{} serve`: {}", binary, e))
        })?;

    let stdin = child
        .stdin
        .take()
        .ok_or_else(|| HarnessError::Unavailable("sidecar has no stdin".into()))?;
    let stdout = child
        .stdout
        .take()
        .ok_or_else(|| HarnessError::Unavailable("sidecar has no stdout".into()))?;
    let stderr = child
        .stderr
        .take()
        .ok_or_else(|| HarnessError::Unavailable("sidecar has no stderr".into()))?;

    let (tx, rx) = mpsc::channel::<String>();
    std::thread::spawn(move || {
        for line in BufReader::new(stdout).lines().map_while(Result::ok) {
            if line.trim().is_empty() {
                continue;
            }
            if tx.send(line).is_err() {
                return;
            }
        }
    });

    let tail = std::sync::Arc::new(Mutex::new(VecDeque::with_capacity(STDERR_TAIL_LINES)));
    let tail_writer = tail.clone();
    std::thread::spawn(move || {
        for line in BufReader::new(stderr).lines().map_while(Result::ok) {
            if let Ok(mut buf) = tail_writer.lock() {
                if buf.len() == STDERR_TAIL_LINES {
                    buf.pop_front();
                }
                buf.push_back(line);
            }
        }
    });

    let mut sidecar = Sidecar {
        child,
        stdin,
        lines: rx,
        stderr: tail,
        hello: HelloResult::default(),
        binary: binary.clone(),
        next_id: 0,
    };

    let raw = sidecar.call(
        OP_HELLO,
        Some(serde_json::json!({"protocol": PROTOCOL_VERSION, "host": "gitpulse"})),
        HELLO_TIMEOUT,
    )?;
    let hello: HelloResult = serde_json::from_value(raw)
        .map_err(|e| HarnessError::Protocol(format!("could not decode hello: {}", e)))?;

    if hello.protocol != PROTOCOL_VERSION {
        return Err(HarnessError::Protocol(format!(
            "sidecar speaks protocol {} and this build speaks {}; refusing rather than \
             mis-reading a policy decision",
            hello.protocol, PROTOCOL_VERSION
        )));
    }
    sidecar.hello = hello;
    Ok(sidecar)
}

/// The process-wide sidecar, started on first use and restarted after a fault.
struct Slot {
    sidecar: Option<Sidecar>,
    last_error: Option<HarnessError>,
    backoff_until: Option<Instant>,
}

fn slot() -> &'static Mutex<Slot> {
    static SLOT: OnceLock<Mutex<Slot>> = OnceLock::new();
    SLOT.get_or_init(|| {
        Mutex::new(Slot {
            sidecar: None,
            last_error: None,
            backoff_until: None,
        })
    })
}

impl Slot {
    fn ensure(&mut self) -> Result<&mut Sidecar, HarnessError> {
        if self.sidecar.is_none() {
            if let Some(until) = self.backoff_until {
                if Instant::now() < until {
                    return Err(self.last_error.clone().unwrap_or_else(|| {
                        HarnessError::Unavailable("harness unavailable".into())
                    }));
                }
            }
            match spawn() {
                Ok(s) => {
                    self.sidecar = Some(s);
                    self.last_error = None;
                    self.backoff_until = None;
                }
                Err(e) => {
                    self.last_error = Some(e.clone());
                    self.backoff_until = Some(Instant::now() + RESPAWN_BACKOFF);
                    return Err(e);
                }
            }
        }
        Ok(self.sidecar.as_mut().expect("sidecar just ensured"))
    }
}

/// Issues one operation, starting the sidecar if it is not running.
///
/// A transport fault drops the child so the next call starts a fresh one; a
/// refusal by the harness leaves it running, because the harness is fine and
/// the request was not.
pub fn call(op: &str, params: Option<Value>, timeout: Duration) -> Result<Value, HarnessError> {
    let mut guard = slot()
        .lock()
        .map_err(|_| HarnessError::Unavailable("harness lock poisoned".into()))?;
    let sidecar = guard.ensure()?;
    match sidecar.call(op, params, timeout) {
        Ok(v) => Ok(v),
        Err(e) => {
            if matches!(
                e,
                HarnessError::Timeout(_) | HarnessError::Unavailable(_) | HarnessError::Protocol(_)
            ) {
                guard.sidecar = None;
                guard.last_error = Some(e.clone());
                guard.backoff_until = Some(Instant::now() + RESPAWN_BACKOFF);
            }
            Err(e)
        }
    }
}

/// Types a call's result, so every caller does not repeat the decode.
pub fn call_typed<T: serde::de::DeserializeOwned>(
    op: &str,
    params: impl Serialize,
    timeout: Duration,
) -> Result<T, HarnessError> {
    let params = serde_json::to_value(params)
        .map_err(|e| HarnessError::Protocol(format!("could not encode {} params: {}", op, e)))?;
    let raw = call(op, Some(params), timeout)?;
    serde_json::from_value(raw)
        .map_err(|e| HarnessError::Protocol(format!("could not decode {} result: {}", op, e)))
}

/// What the sidecar answered at handshake, without provoking a spawn beyond
/// the first.
pub fn handshake() -> Result<(String, HelloResult), HarnessError> {
    let mut guard = slot()
        .lock()
        .map_err(|_| HarnessError::Unavailable("harness lock poisoned".into()))?;
    let sidecar = guard.ensure()?;
    Ok((sidecar.binary.clone(), sidecar.hello.clone()))
}

/// Forgets the backoff so the next call retries immediately. The UI's
/// "reconnect" affordance.
pub fn reset() {
    if let Ok(mut guard) = slot().lock() {
        guard.sidecar = None;
        guard.last_error = None;
        guard.backoff_until = None;
    }
}

/// True when this build knows how to ask for `op` and the running sidecar
/// serves it.
pub fn serves(hello: &HelloResult, op: &str) -> bool {
    hello.ops.iter().any(|o| o == op)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Regression: an undecodable stdout line used to return
    /// HarnessError::Protocol, which drops a healthy sidecar and starts the
    /// 30s respawn backoff. Noise must be ignored instead; only a line
    /// addressed to this call that cannot decode is a protocol fault.
    #[test]
    fn undecodable_noise_is_ignored_not_faulted() {
        for noise in [
            "",
            "   ",
            "manvi: starting up",
            "[2026-08-25] notice",
            "{not json at all",
            r#"{"id": "999", "broken"#,
            // A valid response belonging to some other call.
            r#"{"id":"other","ok":true,"result":{"x":1}}"#,
        ] {
            assert!(
                matches!(classify_line(noise, "42"), LineOutcome::Ignore),
                "line {noise:?} must not fault the sidecar"
            );
        }
    }

    #[test]
    fn matching_id_yields_the_answer() {
        let line = r#"{"id":"7","ok":true,"result":{"allowed":true}}"#;
        match classify_line(line, "7") {
            LineOutcome::Answer(r) => {
                assert!(r.ok);
                assert_eq!(
                    r.result.unwrap_or(Value::Null)["allowed"],
                    Value::Bool(true)
                );
            }
            other => panic!("expected Answer, got {other:?}"),
        }
    }

    #[test]
    fn refused_answer_is_delivered_not_ignored() {
        let line = r#"{"id":"7","ok":false,"error":{"code":"E_BLOCKED","message":"no","retryable":false}}"#;
        match classify_line(line, "7") {
            LineOutcome::Answer(r) => {
                assert!(!r.ok);
                assert_eq!(r.error.unwrap().code, "E_BLOCKED");
            }
            other => panic!("expected Answer, got {other:?}"),
        }
    }

    #[test]
    fn malformed_response_with_our_id_is_a_fault() {
        // Carries our id, so it claims to answer us — but it lacks the
        // mandatory `ok` field. Retrying cannot help; this faults.
        let outcome = classify_line(r#"{"id":"9","result":null}"#, "9");
        assert!(
            matches!(outcome, LineOutcome::Fault(_)),
            "expected Fault, got {outcome:?}"
        );
    }
}
