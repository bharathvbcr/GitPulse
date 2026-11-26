//! `waylode-ptyd` single-client takeover handoff (Opus R5).
//!
//! `waylode-ptyd` enforces strictly **one** connected app at a time
//! (`LinkState::current` in `terminal/host/server.rs`). Its `HANDOFF_GRACE`
//! (`250ms`) is a *post-FIN drain*, not a dual-client takeover window: if
//! Desktop sends `Request::Hello` before the daemon's connection has closed and
//! drained, `ptyd` refuses with `"Another Waylode window is connected to this
//! terminal host."`.
//!
//! Therefore the takeover sequence is:
//! 1. Desktop sends `TakeoverPtyd` to `wayloded --relay`.
//! 2. Daemon detaches (`DetachAll`, best-effort) and immediately shuts down &
//!    closes its `ptyd` Unix socket, returning [`TakeoverAck`].
//! 3. Desktop waits out the post-FIN drain (`HANDOFF_GRACE = 250ms` default)
//!    and runs bounded retry `Request::Hello` until `ptyd` accepts the new link.

#[cfg(unix)]
use crate::terminal::host::proto::{self, Envelope, Hello, HostFrame, Request, PROTOCOL};
use serde::{Deserialize, Serialize};
#[cfg(unix)]
use std::io::BufReader;
#[cfg(unix)]
use std::net::Shutdown;
#[cfg(unix)]
use std::os::unix::net::UnixStream;
#[cfg(unix)]
use std::path::Path;
use std::sync::{Arc, Mutex};
use std::time::Duration;
#[cfg(unix)]
use std::time::Instant;

/// Mirrors `HANDOFF_GRACE` in `src-tauri/src/terminal/host/server.rs`.
pub const HANDOFF_GRACE: Duration = Duration::from_millis(250);

/// Default bounded retry policy for Desktop `Request::Hello` after `TakeoverPtyd`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct HandoffRetryPolicy {
    /// Delay after daemon closes its socket to let `ptyd` drain FIN before
    /// the first `Hello` attempt. Default is [`HANDOFF_GRACE`] (`250ms`).
    pub post_fin_drain: Duration,
    /// Pause between retries when `ptyd` replies that the prior link is still
    /// draining.
    pub retry_interval: Duration,
    /// Maximum number of `Hello` attempts.
    pub max_attempts: usize,
    /// Hard wall-clock ceiling for the entire takeover + `Hello` sequence.
    pub total_deadline: Duration,
    /// Per-attempt socket read/write timeout.
    pub io_timeout: Duration,
}

impl Default for HandoffRetryPolicy {
    fn default() -> Self {
        Self {
            post_fin_drain: HANDOFF_GRACE,
            retry_interval: Duration::from_millis(50),
            max_attempts: 20,
            total_deadline: Duration::from_secs(3),
            io_timeout: Duration::from_secs(2),
        }
    }
}

/// Control-plane message sent by Desktop to ask `wayloded --relay` to release
/// its `waylode-ptyd` socket.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TakeoverPtydRequest {
    pub desktop_pid: u32,
}

/// Daemon's acknowledgement that it has detached and closed its `ptyd` socket.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TakeoverAck {
    pub released_prior_link: bool,
    pub handoff_grace_ms: u64,
    pub detach_warning: Option<String>,
}

/// Daemon-side holder of the single `waylode-ptyd` link when Desktop is closed.
#[derive(Default)]
pub struct PtydHandoffCoordinator {
    state: Arc<Mutex<DaemonPtydState>>,
}

#[derive(Default)]
struct DaemonPtydState {
    desktop_active: bool,
    #[cfg(unix)]
    link: Option<UnixStream>,
    #[cfg(unix)]
    next_id: u64,
}

impl PtydHandoffCoordinator {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn is_desktop_active(&self) -> bool {
        self.state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .desktop_active
    }

    #[cfg(unix)]
    pub fn holds_ptyd_link(&self) -> bool {
        self.state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .link
            .is_some()
    }

    /// Marks Desktop as disconnected so the daemon may attach to `ptyd` to keep
    /// sessions alive past `ptyd`'s 30s idle exit and stream read-only PTY data.
    pub fn on_desktop_disconnected(&self) {
        let mut guard = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        guard.desktop_active = false;
    }

    /// Connects the daemon to `ptyd-v<P>.sock` and performs `Request::Hello`.
    /// Refuses if Desktop currently owns `ptyd`.
    #[cfg(unix)]
    pub fn daemon_attach_ptyd(
        &self,
        socket_path: &Path,
        build_version: &str,
        timeout: Duration,
    ) -> Result<Hello, String> {
        {
            let guard = self
                .state
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            if guard.desktop_active {
                return Err("desktop currently owns waylode-ptyd; daemon will not compete".into());
            }
            if guard.link.is_some() {
                return Err("daemon already holds a waylode-ptyd link".into());
            }
        }

        let (stream, hello) = connect_and_hello_once(socket_path, build_version, timeout)?;
        let mut guard = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if guard.desktop_active {
            // Desktop arrived while we were handshaking — release immediately!
            let _ = stream.shutdown(Shutdown::Both);
            return Err("desktop took over waylode-ptyd while daemon was attaching".into());
        }
        guard.link = Some(stream);
        guard.next_id = 2;
        Ok(hello)
    }

    /// Handles `TakeoverPtyd` from Desktop:
    /// 1. Marks Desktop as active so daemon will not reconnect.
    /// 2. Best-effort sends `Request::DetachAll` on the daemon's `ptyd` socket.
    /// 3. Immediately calls `shutdown(Shutdown::Both)` and drops the socket so
    ///    `ptyd` observes FIN right away and begins its post-FIN drain.
    #[cfg(unix)]
    pub fn handle_takeover(&self, _req: TakeoverPtydRequest) -> TakeoverAck {
        let (prior, detach_id) = {
            let mut guard = self
                .state
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            guard.desktop_active = true;
            let id = guard.next_id.max(2);
            guard.next_id = id + 1;
            (guard.link.take(), id)
        };

        let mut detach_warning = None;
        let released_prior_link = if let Some(mut stream) = prior {
            let _ = stream.set_write_timeout(Some(Duration::from_millis(50)));
            let envelope = Envelope {
                id: detach_id,
                request: Request::DetachAll,
            };
            if let Err(err) = proto::write_frame(&mut stream, &envelope, &[]) {
                detach_warning = Some(format!("best-effort DetachAll write failed: {err}"));
            }
            let _ = stream.shutdown(Shutdown::Both);
            drop(stream);
            true
        } else {
            false
        };

        TakeoverAck {
            released_prior_link,
            handoff_grace_ms: HANDOFF_GRACE.as_millis() as u64,
            detach_warning,
        }
    }
}

/// Successful result of Desktop taking over `waylode-ptyd` from the daemon.
#[cfg(unix)]
#[derive(Debug)]
pub struct TakeoverConnectResult {
    pub stream: UnixStream,
    pub hello: Hello,
    /// Number of `Hello` attempts needed (1 = first try succeeded; >1 = retried
    /// through a post-FIN drain race).
    pub attempts: usize,
    pub elapsed: Duration,
}

/// Executes the full Opus R5 Desktop takeover sequence:
/// 1. Invokes `request_takeover` (`TakeoverPtyd` over `control-plane.sock` or
///    in-process coordinator) so the daemon detaches and closes its `ptyd` link.
/// 2. Waits `policy.post_fin_drain` (default `HANDOFF_GRACE = 250ms`) for
///    `ptyd` to finish its post-FIN drain.
/// 3. Runs bounded retry `Request::Hello` against `socket_path`, retrying any
///    `"Another Waylode window is connected"` (older hosts: `GitPulse`) / drain-window refusal until
///    accepted or limits expire.
#[cfg(unix)]
pub fn desktop_takeover_and_connect(
    socket_path: &Path,
    build_version: &str,
    policy: HandoffRetryPolicy,
    request_takeover: impl FnOnce() -> Result<TakeoverAck, String>,
) -> Result<TakeoverConnectResult, String> {
    let started = Instant::now();
    let ack = request_takeover()?;

    if ack.released_prior_link && !policy.post_fin_drain.is_zero() {
        std::thread::sleep(policy.post_fin_drain);
    }

    let deadline = started + policy.total_deadline;
    let max_attempts = policy.max_attempts.max(1);
    let mut last_error = String::new();

    for attempt in 1..=max_attempts {
        match connect_and_hello_once(socket_path, build_version, policy.io_timeout) {
            Ok((stream, hello)) => {
                return Ok(TakeoverConnectResult {
                    stream,
                    hello,
                    attempts: attempt,
                    elapsed: started.elapsed(),
                });
            }
            Err(err) => {
                let retryable = is_drain_race_error(&err);
                last_error = err;
                if !retryable || attempt == max_attempts || Instant::now() >= deadline {
                    break;
                }
                let remaining = deadline.saturating_duration_since(Instant::now());
                std::thread::sleep(policy.retry_interval.min(remaining));
            }
        }
    }

    Err(format!(
        "desktop ptyd takeover failed after bounded retries ({last_error})"
    ))
}

/// Returns true when a `Hello` failure reflects the `ptyd` post-FIN drain race
/// rather than a permanent protocol mismatch or missing socket.
pub fn is_drain_race_error(err: &str) -> bool {
    let lower = err.to_ascii_lowercase();
    lower.contains("another waylode window is connected")
        || lower.contains("another gitpulse window is connected")
        || lower.contains("already connected")
        || lower.contains("unexpected eof")
        || lower.contains("connection reset")
        || lower.contains("broken pipe")
}

/// Connects to `socket_path`, sends `Request::Hello`, and validates the reply.
#[cfg(unix)]
pub fn connect_and_hello_once(
    socket_path: &Path,
    build_version: &str,
    timeout: Duration,
) -> Result<(UnixStream, Hello), String> {
    let mut stream = UnixStream::connect(socket_path)
        .map_err(|e| format!("connect {}: {e}", socket_path.display()))?;
    stream
        .set_read_timeout(Some(timeout))
        .map_err(|e| format!("set read timeout: {e}"))?;
    stream
        .set_write_timeout(Some(timeout))
        .map_err(|e| format!("set write timeout: {e}"))?;

    let envelope = Envelope {
        id: 1,
        request: Request::Hello {
            protocol: PROTOCOL,
            build: build_version.to_string(),
        },
    };
    proto::write_frame(&mut stream, &envelope, &[])
        .map_err(|e| format!("write hello frame: {e}"))?;

    let mut reader = BufReader::new(
        stream
            .try_clone()
            .map_err(|e| format!("clone ptyd stream: {e}"))?,
    );
    let Some((header, _payload)) =
        proto::read_frame(&mut reader).map_err(|e| format!("read hello reply: {e}"))?
    else {
        return Err("unexpected EOF while awaiting ptyd hello reply".into());
    };
    let frame: HostFrame =
        proto::decode(&header).map_err(|e| format!("decode hello reply: {e}"))?;
    match frame {
        HostFrame::Reply {
            id: 1,
            ok: Some(val),
            error: None,
        } => {
            let hello: Hello =
                serde_json::from_value(val).map_err(|e| format!("parse Hello payload: {e}"))?;
            if hello.protocol != PROTOCOL {
                return Err(format!(
                    "ptyd protocol mismatch: expected {PROTOCOL}, got {}",
                    hello.protocol
                ));
            }
            let _ = stream.set_read_timeout(None);
            Ok((stream, hello))
        }
        HostFrame::Reply {
            error: Some(reason),
            ..
        } => Err(reason),
        other => Err(format!("unexpected ptyd frame during hello: {other:?}")),
    }
}
