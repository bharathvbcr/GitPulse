//! A client for the terminal host of the *previous* protocol.
//!
//! An update that bumps `PROTOCOL` starts a new host beside the old one, and
//! the old one keeps every session it holds: nothing in the new app could see,
//! read or stop them. [`LegacyHost`] speaks the frozen subset in
//! [`compat_v4`] (and, for later bumps, the dialect modules listed in
//! [`COMPAT_PROTOCOLS`]) so the app can list those sessions, read the tail of
//! their saved output, and stop them.
//!
//! This is stage one: list, kill and read a transcript. It does not attach,
//! type or resize; a legacy session is shown and stopped, not driven.
//!
//! The app is the host's one connected app while a [`LegacyHost`] exists
//! (`server.rs`: "one app at a time"), so a handle is opened for the work and
//! dropped. A host whose own app is still connected refuses the connection;
//! that is reported as [`LegacyError::Refused`], never as an empty host.
//!
//! Every wait is bounded by an absolute deadline per call, including a host
//! that answers one byte at a time. A call that fails mid-exchange leaves the
//! stream with an unread reply on it, so the handle refuses everything after
//! ([`LegacyError::Broken`]) rather than pair a later request with an earlier
//! answer.

use super::compat_v4 as v4;
use serde::Serialize;
use std::io::{self, Read};
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

/// The protocols this build can speak to as a previous host. Every entry has a
/// dialect module (`compat_v4`); a protocol bump that keeps sessions alive
/// adds its predecessor here.
pub const COMPAT_PROTOCOLS: &[u32] = &[v4::PROTOCOL];

/// How long `Hello` and a read (`List`, `Transcript`) may take; `Kill` gets
/// half as long again, because the host waits for the process to go.
const CALL_DEADLINE: Duration = Duration::from_secs(10);
/// Frames that are not the reply, skipped while waiting for one. A host sends
/// one per session event; a stream of them with no reply is not a host
/// answering.
const MAX_SKIPPED_FRAMES: usize = 1024;
/// Socket names `discover` looks at in one directory.
const MAX_SOCKETS: usize = 64;
/// Longest session id accepted from a caller; the host's own ids are
/// `term-<pid>-<n>`.
const MAX_SESSION_ID_BYTES: usize = 256;

/// Whether a host of `protocol` can be spoken to as a legacy host.
pub fn has_compat(protocol: u32) -> bool {
    COMPAT_PROTOCOLS.contains(&protocol)
}

/// Why a legacy host could not be used. Each says what went wrong; none of
/// them means "the host holds nothing".
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "kind", content = "reason", rename_all = "snake_case")]
pub enum LegacyError {
    /// This build has no dialect for the protocol.
    NoCompat(u32),
    /// Nothing answers at the socket: a file a dead host left, or no file.
    Unreachable(String),
    /// The host is there and turned this app away: another window holds it.
    Refused(String),
    /// The host did not answer within the deadline.
    TimedOut(String),
    /// The host closed the connection, or answered with something that is not
    /// a protocol-4 frame, or answered another request, or spoke another
    /// protocol than the socket's name says.
    Protocol(String),
    /// The host understood and said no.
    Host(String),
    /// An earlier call on this handle left it out of step.
    Broken,
    /// The caller asked for something this client will not send.
    Invalid(String),
}

impl std::fmt::Display for LegacyError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::NoCompat(protocol) => write!(
                f,
                "this version has no way to talk to a terminal host of protocol {protocol}"
            ),
            Self::Unreachable(why) => write!(f, "the older terminal host is not reachable: {why}"),
            Self::Refused(why) => write!(f, "the older terminal host refused this app: {why}"),
            Self::TimedOut(what) => write!(f, "the older terminal host did not answer: {what}"),
            Self::Protocol(why) => write!(
                f,
                "the older terminal host sent something unexpected: {why}"
            ),
            Self::Host(why) => write!(f, "the older terminal host said: {why}"),
            Self::Broken => write!(
                f,
                "an earlier request to the older terminal host failed; reconnect"
            ),
            Self::Invalid(why) => write!(f, "{why}"),
        }
    }
}

impl std::error::Error for LegacyError {}

/// What a legacy host said about itself in its `Hello` answer.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct LegacyHostInfo {
    pub protocol: u32,
    pub build: String,
    pub pid: u32,
    /// Sessions the host held when it answered.
    pub sessions: usize,
}

/// A session a legacy host holds. The v4 listing has no start time, so none
/// is reported here.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct LegacySession {
    pub id: String,
    /// The program the session runs (`shell`).
    pub program: String,
    pub cwd: String,
    pub repo: String,
    pub launcher: Option<String>,
    pub run_id: Option<String>,
    pub title: Option<String>,
    pub parent_session_id: Option<String>,
    /// No view holds it.
    pub detached: bool,
    /// Its terminal closed or it was told to stop; it has not been reaped.
    pub ending: bool,
    /// Milliseconds since it last wrote; `None` when the host could not say.
    pub quiet_ms: Option<u64>,
}

impl From<v4::Listing> for LegacySession {
    fn from(listing: v4::Listing) -> Self {
        Self {
            id: listing.id,
            program: listing.shell,
            cwd: listing.cwd,
            repo: listing.repo,
            launcher: listing.launcher,
            run_id: listing.run_id,
            title: listing.title,
            parent_session_id: listing.parent_session_id,
            detached: listing.detached,
            ending: listing.ending,
            quiet_ms: listing.quiet_ms,
        }
    }
}

/// What the app can say about one other-protocol host it found.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum LegacyStatus {
    /// The host was reached and listed what it holds.
    Listed {
        info: LegacyHostInfo,
        sessions: Vec<LegacySession>,
    },
    /// This build has no dialect for the host's protocol: it cannot be used.
    NoCompat,
    /// A socket with no host behind it.
    Unreachable { reason: String },
    /// Another app is connected to it.
    Refused { reason: String },
    /// It was reached and could not be used: slow, garbled, or an error.
    Failed { reason: String },
}

impl From<LegacyError> for LegacyStatus {
    fn from(error: LegacyError) -> Self {
        match error {
            LegacyError::NoCompat(_) => Self::NoCompat,
            LegacyError::Unreachable(reason) => Self::Unreachable { reason },
            LegacyError::Refused(reason) => Self::Refused { reason },
            other => Self::Failed {
                reason: other.to_string(),
            },
        }
    }
}

/// One other-protocol host found in the host directory.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct LegacyHostReport {
    pub protocol: u32,
    pub socket: PathBuf,
    pub status: LegacyStatus,
}

/// What `discover` found.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct LegacyScan {
    pub hosts: Vec<LegacyHostReport>,
    /// The directory held more sockets than were looked at.
    pub truncated: bool,
}

/// Looks at every host socket of a protocol other than `current` in `dir`
/// and, for each, reports what could be learned. A host whose protocol this
/// build has no dialect for is reported as such, without being connected to.
pub fn discover(dir: &Path, current: u32) -> LegacyScan {
    let mut sockets: Vec<(u32, PathBuf)> = match std::fs::read_dir(dir) {
        Ok(entries) => entries
            .flatten()
            .filter_map(|entry| {
                let protocol = entry
                    .file_name()
                    .to_str()
                    .and_then(super::socket_protocol)?;
                (protocol != current).then(|| (protocol, entry.path()))
            })
            .collect(),
        Err(_) => Vec::new(),
    };
    sockets.sort();
    let truncated = sockets.len() > MAX_SOCKETS;
    sockets.truncate(MAX_SOCKETS);
    let hosts = sockets
        .into_iter()
        .map(|(protocol, socket)| LegacyHostReport {
            protocol,
            status: inspect(dir, protocol),
            socket,
        })
        .collect();
    LegacyScan { hosts, truncated }
}

/// Connects to the host of `protocol` in `dir` and lists its sessions.
pub fn inspect(dir: &Path, protocol: u32) -> LegacyStatus {
    inspect_with(dir, protocol, CALL_DEADLINE)
}

fn inspect_with(dir: &Path, protocol: u32, deadline: Duration) -> LegacyStatus {
    let listed = LegacyHost::connect_with(dir, protocol, deadline).and_then(|mut host| {
        let sessions = host.list()?;
        Ok(LegacyStatus::Listed {
            info: host.info.clone(),
            sessions,
        })
    });
    listed.unwrap_or_else(LegacyStatus::from)
}

/// A connection to a host of an earlier protocol.
pub struct LegacyHost {
    stream: UnixStream,
    info: LegacyHostInfo,
    next_id: u64,
    broken: bool,
    deadline: Duration,
}

/// Reads from a socket until an absolute instant, however slowly the peer
/// sends: each read is given only the time that is left.
struct UntilDeadline<'a> {
    stream: &'a UnixStream,
    end: Instant,
}

impl Read for UntilDeadline<'_> {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        let left = self.end.saturating_duration_since(Instant::now());
        if left.is_zero() {
            return Err(io::ErrorKind::TimedOut.into());
        }
        // macOS refuses a socket option (EINVAL) on a socket whose peer has
        // already hung up, which is exactly a host that sent its refusal and
        // closed. The read below then returns what it sent, or the end, at
        // once, so the timeout it would have bounded is not needed.
        let _ = self.stream.set_read_timeout(Some(left));
        let mut stream = self.stream;
        stream.read(buf)
    }
}

impl LegacyHost {
    /// Connects to the host of `protocol` in `dir` as the app it expects, and
    /// introduces this app to it.
    pub fn connect(dir: &Path, protocol: u32) -> Result<Self, LegacyError> {
        Self::connect_with(dir, protocol, CALL_DEADLINE)
    }

    fn connect_with(dir: &Path, protocol: u32, deadline: Duration) -> Result<Self, LegacyError> {
        if !has_compat(protocol) {
            return Err(LegacyError::NoCompat(protocol));
        }
        let path = dir.join(super::socket_name(protocol));
        let stream = UnixStream::connect(&path)
            .map_err(|error| LegacyError::Unreachable(format!("{}: {error}", path.display())))?;
        let mut host = Self {
            stream,
            info: LegacyHostInfo {
                protocol,
                build: String::new(),
                pid: 0,
                sessions: 0,
            },
            next_id: 1,
            broken: false,
            deadline,
        };
        let hello = host.call(
            v4::Request::Hello {
                protocol,
                build: crate::updates::CURRENT_VERSION.to_owned(),
            },
            deadline,
        );
        // A host that is serving another app says so in place of its hello.
        let hello = hello.map_err(|error| match error {
            LegacyError::Host(why) => LegacyError::Refused(why),
            other => other,
        })?;
        let hello: v4::Hello = serde_json::from_value(hello)
            .map_err(|error| LegacyError::Protocol(format!("its hello was unreadable: {error}")))?;
        if hello.protocol != protocol {
            return Err(LegacyError::Protocol(format!(
                "the socket is named for protocol {protocol} but the host speaks {}",
                hello.protocol
            )));
        }
        host.info = LegacyHostInfo {
            protocol,
            build: hello.build,
            pid: hello.pid,
            sessions: hello.sessions,
        };
        Ok(host)
    }

    /// What the host said about itself when this app introduced itself.
    pub fn info(&self) -> &LegacyHostInfo {
        &self.info
    }

    /// Every session the host holds, live or ending.
    pub fn list(&mut self) -> Result<Vec<LegacySession>, LegacyError> {
        let value = self.call(v4::Request::List, self.deadline)?;
        let listings: Vec<v4::Listing> = serde_json::from_value(value)
            .map_err(|error| self.fail(format!("its session list was unreadable: {error}")))?;
        Ok(listings.into_iter().map(LegacySession::from).collect())
    }

    /// Stops `session`: the host closes it as the app's own close does, and
    /// answers once the process is gone.
    pub fn kill(&mut self, session: &str) -> Result<(), LegacyError> {
        check_session(session)?;
        self.call(
            v4::Request::Kill {
                session: session.to_owned(),
            },
            self.deadline * 3 / 2,
        )
        .map(|_| ())
    }

    /// The newest `max_bytes` (at most 128 KiB) of `session`'s saved output,
    /// read-only: it never attaches. `redact` masks secrets in the host.
    pub fn transcript_tail(
        &mut self,
        session: &str,
        max_bytes: usize,
        redact: bool,
    ) -> Result<crate::terminal::TranscriptRead, LegacyError> {
        check_session(session)?;
        let value = self.call(
            v4::Request::Transcript {
                session: session.to_owned(),
                max_bytes: max_bytes.clamp(1, v4::MAX_TRANSCRIPT_TAIL_BYTES),
                redact,
                run_id: None,
            },
            self.deadline,
        )?;
        let read: v4::TranscriptRead = serde_json::from_value(value)
            .map_err(|error| self.fail(format!("its transcript was unreadable: {error}")))?;
        Ok(current_transcript(read))
    }

    /// Records that the exchange is out of step and says why.
    fn fail(&mut self, why: String) -> LegacyError {
        self.broken = true;
        LegacyError::Protocol(why)
    }

    /// One request and its reply, within `deadline` of now.
    fn call(
        &mut self,
        request: v4::Request,
        deadline: Duration,
    ) -> Result<serde_json::Value, LegacyError> {
        if self.broken {
            return Err(LegacyError::Broken);
        }
        let id = self.next_id;
        self.next_id += 1;
        let end = Instant::now() + deadline;
        // Refused as in `UntilDeadline::read` when the peer is already gone;
        // the write then fails or the read reports the end, both at once.
        let _ = self.stream.set_write_timeout(Some(deadline));
        let mut stream = &self.stream;
        let sent = v4::write_frame(&mut stream, &v4::Envelope { id, request }, &[]);
        if let Err(error) = sent {
            self.broken = true;
            return Err(io_error("sending the request", error));
        }
        let mut skipped = 0;
        loop {
            let mut input = UntilDeadline {
                stream: &self.stream,
                end,
            };
            let frame = match v4::read_frame(&mut input) {
                Ok(Some((header, _payload))) => header,
                Ok(None) => {
                    self.broken = true;
                    return Err(LegacyError::Protocol(
                        "the host closed the connection without answering".into(),
                    ));
                }
                Err(error) => {
                    self.broken = true;
                    return Err(io_error("waiting for the answer", error));
                }
            };
            match v4::decode::<v4::HostFrame>(&frame) {
                Ok(v4::HostFrame::Reply { id: got, ok, error }) if got == id => {
                    return match error {
                        Some(error) => Err(LegacyError::Host(error)),
                        None => Ok(ok.unwrap_or(serde_json::Value::Null)),
                    };
                }
                Ok(v4::HostFrame::Reply { id: got, .. }) => {
                    return Err(self.fail(format!("it answered request {got}, not {id}")));
                }
                Ok(v4::HostFrame::Other) => {
                    skipped += 1;
                    if skipped > MAX_SKIPPED_FRAMES {
                        return Err(self.fail(format!(
                            "{MAX_SKIPPED_FRAMES} frames went by with no answer"
                        )));
                    }
                }
                Err(error) => {
                    return Err(self.fail(format!("a frame was not protocol 4: {error}")));
                }
            }
        }
    }
}

fn check_session(session: &str) -> Result<(), LegacyError> {
    if session.is_empty()
        || session.len() > MAX_SESSION_ID_BYTES
        || session.chars().any(char::is_control)
    {
        return Err(LegacyError::Invalid(
            "a session id is 1 to 256 characters with no control characters".into(),
        ));
    }
    Ok(())
}

fn io_error(doing: &str, error: io::Error) -> LegacyError {
    match error.kind() {
        io::ErrorKind::TimedOut | io::ErrorKind::WouldBlock => {
            LegacyError::TimedOut(format!("gave up {doing}"))
        }
        _ => LegacyError::Protocol(format!("{doing}: {error}")),
    }
}

/// The v4 transcript answer in the shape this build's app reads. This is the
/// one seam where the frozen wire shape meets the live type: a later change to
/// `TranscriptRead` or `TranscriptTail` stops compiling here, not on the wire.
fn current_transcript(read: v4::TranscriptRead) -> crate::terminal::TranscriptRead {
    use crate::terminal::{TranscriptRead as Now, TranscriptTail as Tail};
    match read {
        v4::TranscriptRead::Found(tail) => Now::Found(Tail {
            data_b64: tail.data_b64,
            bytes: tail.bytes,
            retained: tail.retained,
            total_bytes: tail.total_bytes,
            truncated: tail.truncated,
            rotated: tail.rotated,
            clipped: tail.clipped,
            gap_markers: tail.gap_markers,
            unrecorded_bytes: tail.unrecorded_bytes,
            redacted: tail.redacted,
            live: tail.live,
            settled: tail.settled,
            recording_error: tail.recording_error,
            run_id: tail.run_id,
        }),
        v4::TranscriptRead::Missing => Now::Missing,
        v4::TranscriptRead::Unreadable { reason } => Now::Unreadable { reason },
        v4::TranscriptRead::Reused { reason } => Now::Reused { reason },
        v4::TranscriptRead::Unavailable { reason } => Now::Unavailable { reason },
    }
}

#[cfg(test)]
#[path = "legacy_tests.rs"]
mod tests;
