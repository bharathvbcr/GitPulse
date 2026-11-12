//! The frames the app and the terminal host exchange.
//!
//! A frame is a 4-byte big-endian header length, that many bytes of JSON (the
//! typed header), a 4-byte big-endian payload length, and that many raw bytes.
//! Output and keystrokes travel as the payload, never inside the JSON: the
//! webview hop stopped base64-encoding output (`sink.rs`), and this hop does
//! not start again.
//!
//! [`Request::Hello`] and [`Request::Retire`], and the reply to each, are the
//! only shapes frozen across protocol versions: they are what an app uses to
//! find out that a host speaks another version, and to ask an idle one to go.

use super::super::{
    AgentSessionMeta, SpawnRequest, TerminalContext, TerminalExitPayload, TerminalListing,
    TerminalLossPayload, TerminalSpawned,
};
use serde::{Deserialize, Serialize};
use std::io::{self, Read, Write};

/// What this build speaks. Bumped whenever a frame changes shape or meaning.
/// 3 added `Request::Transcript`; 4 added child-orchestration lineage,
/// `Telemetry`, `SetAgentMeta`, `OrchestrationSnapshot`, and `TranscriptTail::total_bytes`.
pub const PROTOCOL: u32 = 4;

/// Longest JSON header accepted. A spawn request (argv and environment are
/// bounded at 128 KiB each by `validate_pty_options`) is the largest a caller
/// can send, so the bound is generous and still bounded.
pub const MAX_HEADER_BYTES: usize = 512 * 1024;

/// Longest payload: one output message (`OUTPUT_CHUNK_BYTES`), one write's
/// worth of keystrokes (`MAX_PTY_INPUT_BYTES`), whichever is larger.
pub const MAX_PAYLOAD_BYTES: usize = 64 * 1024;

/// App → host. Every request gets exactly one reply.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "op", rename_all = "snake_case")]
pub enum Request {
    /// Frozen. Answered with [`Hello`].
    Hello {
        protocol: u32,
        build: String,
    },
    /// Frozen. A host with no sessions exits; one with sessions stays and
    /// says how many. Answered with [`Retired`].
    Retire,
    Spawn {
        request: SpawnRequest,
    },
    /// The payload is the bytes to type.
    Write {
        session: String,
    },
    Resize {
        session: String,
        rows: u16,
        cols: u16,
    },
    /// The view painted `bytes` of `session`'s output. Answered inline, with
    /// the window's refusal when it owes nothing that large.
    Ack {
        session: String,
        bytes: usize,
    },
    Detach {
        session: String,
    },
    DetachAll,
    Attach {
        session: String,
        rows: u16,
        cols: u16,
    },
    AttachTracked {
        run_id: String,
        rows: u16,
        cols: u16,
    },
    Kill {
        session: String,
    },
    /// Every session, closed as a quit that chose to stop them closes them.
    StopAll,
    List,
    Context {
        session: String,
    },
    ListeningPorts {
        session: String,
    },
    Background {
        sessions: Vec<String>,
    },
    Limit {
        limit: usize,
    },
    Stall {
        ms: u64,
    },
    /// Exits recorded and not yet forgotten: sessions that ended while no
    /// app was there to hear it, or before it processed what it heard.
    Receipts,
    /// The app has recorded this session's exit; its receipt can go.
    Forget {
        session: String,
    },
    Wakeups {
        session: String,
    },
    Size {
        session: String,
    },
    Hold {
        session: String,
    },
    Release {
        session: String,
    },
    /// The newest `max_bytes` of the session's saved output, read-only: live
    /// or finished, it never attaches. Answered with `terminal::TranscriptRead`.
    /// `redact` masks secrets in the host before the bytes leave it. `run_id`,
    /// when sent, asks for that run's output only: an id a later session
    /// reused is answered as such, not read as the run's. Left out on the wire
    /// when absent, and optional when received, so protocol 3 is unchanged.
    Transcript {
        session: String,
        max_bytes: usize,
        #[serde(default)]
        redact: bool,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        run_id: Option<String>,
    },
    /// Current OSC 7 / 133 / 7501 + loopback readiness snapshot for `session`.
    Telemetry {
        session: String,
    },
    /// Persists `SessionStart` metadata (`agent`, `conversation_id`, `artifact_dir`)
    /// inside `gitpulse-ptyd` so a reconnecting app adopts it intact.
    SetAgentMeta {
        session: String,
        meta: AgentSessionMeta,
    },
    /// Returns orchestration lineage, session token hashes (RAM-only), telemetry,
    /// and `AgentSessionMeta` for live sessions held by `gitpulse-ptyd`.
    OrchestrationSnapshot,
}

/// A request with the id its reply will carry.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Envelope {
    pub id: u64,
    #[serde(flatten)]
    pub request: Request,
}

/// Host → app.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum HostFrame {
    /// The answer to request `id`: `ok` or `error`, never both.
    Reply {
        id: u64,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        ok: Option<serde_json::Value>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        error: Option<String>,
    },
    /// The payload is output for `session`'s current view.
    Output {
        session: String,
    },
    /// `session` will send nothing more; its exit follows.
    End {
        session: String,
    },
    Loss {
        loss: TerminalLossPayload,
    },
    Notice {
        notice: WireNotice,
    },
    ScanDrops {
        dropped: u64,
    },
    Exit {
        ended: WireEnded,
    },
}

/// Answer to [`Request::Hello`]. Frozen.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Hello {
    pub protocol: u32,
    pub build: String,
    pub pid: u32,
    pub sessions: usize,
}

/// Answer to [`Request::Retire`]. Frozen.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Retired {
    pub retired: bool,
    pub sessions: usize,
}

/// A notice as it crosses the socket: `alerts::Notice` with its two closed
/// vocabularies (`event`, `channel`) as text, read back against them.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WireNotice {
    pub key: String,
    pub label: String,
    pub place: Option<String>,
    pub event: Option<String>,
    pub detail: Option<String>,
    pub is_agent: bool,
    pub channel: String,
    pub subject: Option<String>,
    pub subagent: bool,
    /// Which subagent, when one was named. Left off the wire when absent, so a
    /// peer that predates it (and refuses unknown fields) reads every notice
    /// that does not carry one exactly as before.
    ///
    /// That only holds while no sender sets it: today the host forwards only
    /// its own `SessionWatcher`'s notices, which never name a subagent, so a
    /// `Some` does not cross this socket. A host-side producer that starts
    /// setting it would make an app built before this field fail to parse the
    /// frame (`deny_unknown_fields`; the client logs and drops it), and so
    /// must bump `PROTOCOL` in the same change.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub agent_id: Option<String>,
}

/// The channels a terminal's own output can raise a notice on: the scanner's,
/// `osc7501` for Herdr agent status coordination, and `exit` for session end.
const TERMINAL_CHANNELS: [&str; 6] = ["bell", "osc9", "osc777", "osc99", "osc7501", "exit"];

impl WireNotice {
    pub fn from_notice(notice: &crate::alerts::Notice) -> Self {
        Self {
            key: notice.key.clone(),
            label: notice.label.clone(),
            place: notice.place.clone(),
            event: notice.event.map(str::to_owned),
            detail: notice.detail.clone(),
            is_agent: notice.is_agent,
            channel: notice.channel.to_owned(),
            subject: notice.subject.clone(),
            subagent: notice.subagent,
            agent_id: notice.agent_id.clone(),
        }
    }

    /// The notice, when every closed field names something this build knows.
    /// A terminal notice is the only kind a host sends.
    pub fn into_notice(self) -> Result<crate::alerts::Notice, String> {
        let event = match self.event.as_deref() {
            None => None,
            Some(name) => Some(
                crate::alerts::bridge::event(name)
                    .ok_or_else(|| format!("the terminal host named an unknown event {name:?}"))?
                    .name,
            ),
        };
        let channel = TERMINAL_CHANNELS
            .into_iter()
            .find(|known| *known == self.channel)
            .ok_or_else(|| {
                format!(
                    "the terminal host named an unknown channel {:?}",
                    self.channel
                )
            })?;
        if self
            .agent_id
            .as_deref()
            .is_some_and(|id| !crate::alerts::bridge::valid_key(id))
        {
            return Err("the terminal host named a malformed agent id".to_owned());
        }
        Ok(crate::alerts::Notice {
            key: self.key,
            origin: crate::alerts::Origin::Terminal,
            label: self.label,
            place: self.place,
            event,
            detail: self.detail,
            is_agent: self.is_agent,
            channel,
            subject: self.subject,
            subagent: self.subagent,
            agent_id: self.agent_id,
            fully_idle: None,
            provider_session_id: None,
        })
    }
}

/// `SessionEnded` as it crosses the socket.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WireEnded {
    pub payload: TerminalExitPayload,
    pub ended_by_gitpulse: bool,
    pub program: String,
    pub cwd: String,
    /// The task run the session belonged to, so an exit the app hears about
    /// after a restart can still be filed under it.
    pub run_id: Option<String>,
}

/// Answer to [`Request::Spawn`]: the session, and the identity of the process
/// it started, read in the host right after the fork while the child cannot
/// yet have been reaped (`process_birth`). The app records the start with it;
/// by the time the reply arrives the child may already be gone.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SpawnReply {
    pub spawned: TerminalSpawned,
    pub process: Option<(u32, String)>,
    /// Why `process` could not be read, when it could not.
    pub process_error: Option<String>,
}

/// A session the host holds, as `list` reports it.
pub type Listing = TerminalListing;
/// What `attach` and `spawn` answer.
pub type Spawned = TerminalSpawned;
/// What `context` answers.
pub type Context = TerminalContext;

/// Writes one frame in a single `write_all`, so frames written by different
/// threads under one lock never interleave.
pub fn write_frame(
    out: &mut impl Write,
    header: &impl Serialize,
    payload: &[u8],
) -> io::Result<()> {
    let header = serde_json::to_vec(header).map_err(io::Error::other)?;
    if header.len() > MAX_HEADER_BYTES || payload.len() > MAX_PAYLOAD_BYTES {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            format!(
                "a terminal host frame of {} + {} bytes is over its bound",
                header.len(),
                payload.len()
            ),
        ));
    }
    let mut frame = Vec::with_capacity(8 + header.len() + payload.len());
    frame.extend_from_slice(&(header.len() as u32).to_be_bytes());
    frame.extend_from_slice(&header);
    frame.extend_from_slice(&(payload.len() as u32).to_be_bytes());
    frame.extend_from_slice(payload);
    out.write_all(&frame)?;
    out.flush()
}

/// Reads one frame: its JSON header and its payload. `Ok(None)` is a clean
/// end of stream between frames; an end inside one is an error.
pub fn read_frame(input: &mut impl Read) -> io::Result<Option<(Vec<u8>, Vec<u8>)>> {
    let mut length = [0u8; 4];
    if !read_exact_or_eof(input, &mut length)? {
        return Ok(None);
    }
    let header_len = u32::from_be_bytes(length) as usize;
    if header_len == 0 || header_len > MAX_HEADER_BYTES {
        return Err(oversized("header", header_len));
    }
    let mut header = vec![0u8; header_len];
    input.read_exact(&mut header)?;
    input.read_exact(&mut length)?;
    let payload_len = u32::from_be_bytes(length) as usize;
    if payload_len > MAX_PAYLOAD_BYTES {
        return Err(oversized("payload", payload_len));
    }
    let mut payload = vec![0u8; payload_len];
    input.read_exact(&mut payload)?;
    Ok(Some((header, payload)))
}

fn oversized(what: &str, len: usize) -> io::Error {
    io::Error::new(
        io::ErrorKind::InvalidData,
        format!("a terminal host frame's {what} of {len} bytes is out of bounds"),
    )
}

/// Fills `buf`, or reports a clean end of stream before its first byte.
fn read_exact_or_eof(input: &mut impl Read, buf: &mut [u8]) -> io::Result<bool> {
    let mut filled = 0;
    while filled < buf.len() {
        match input.read(&mut buf[filled..]) {
            Ok(0) if filled == 0 => return Ok(false),
            Ok(0) => return Err(io::ErrorKind::UnexpectedEof.into()),
            Ok(n) => filled += n,
            Err(error) if error.kind() == io::ErrorKind::Interrupted => {}
            Err(error) => return Err(error),
        }
    }
    Ok(true)
}

/// Decodes a frame's header as `T`.
pub fn decode<T: for<'de> Deserialize<'de>>(header: &[u8]) -> io::Result<T> {
    serde_json::from_slice(header)
        .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_frame_round_trips_with_its_raw_payload() {
        let mut wire = Vec::new();
        let envelope = Envelope {
            id: 7,
            request: Request::Write {
                session: "term-1-a".into(),
            },
        };
        write_frame(&mut wire, &envelope, b"\x1b[A\xff").unwrap();
        write_frame(
            &mut wire,
            &Envelope {
                id: 8,
                request: Request::List,
            },
            b"",
        )
        .unwrap();
        let mut input = wire.as_slice();
        let (header, payload) = read_frame(&mut input).unwrap().unwrap();
        assert_eq!(decode::<Envelope>(&header).unwrap(), envelope);
        assert_eq!(payload, b"\x1b[A\xff");
        let (header, payload) = read_frame(&mut input).unwrap().unwrap();
        assert_eq!(decode::<Envelope>(&header).unwrap().request, Request::List);
        assert!(payload.is_empty());
        assert!(read_frame(&mut input).unwrap().is_none(), "a clean end");
    }

    /// The two frames another version must still understand.
    #[test]
    fn hello_and_retire_keep_their_frozen_shapes() {
        let hello = serde_json::to_value(Envelope {
            id: 1,
            request: Request::Hello {
                protocol: 1,
                build: "1.2.3".into(),
            },
        })
        .unwrap();
        assert_eq!(
            hello,
            serde_json::json!({"id": 1, "op": "hello", "protocol": 1, "build": "1.2.3"})
        );
        assert_eq!(
            serde_json::to_value(Envelope {
                id: 2,
                request: Request::Retire
            })
            .unwrap(),
            serde_json::json!({"id": 2, "op": "retire"})
        );
        let reply = serde_json::to_value(HostFrame::Reply {
            id: 1,
            ok: Some(
                serde_json::to_value(Hello {
                    protocol: 9,
                    build: "b".into(),
                    pid: 4,
                    sessions: 2,
                })
                .unwrap(),
            ),
            error: None,
        })
        .unwrap();
        assert_eq!(
            reply,
            serde_json::json!({"kind": "reply", "id": 1, "ok": {"protocol": 9, "build": "b", "pid": 4, "sessions": 2}})
        );
    }

    /// The transcript read is new in protocol 3, so a host and an app of
    /// different protocols never exchange it: the socket they would meet on is
    /// named for the protocol.
    #[test]
    fn the_transcript_request_has_a_pinned_shape_and_lives_on_protocol_three() {
        assert_eq!(PROTOCOL, 4);
        assert_ne!(
            crate::terminal::host::socket_name(PROTOCOL),
            crate::terminal::host::socket_name(3)
        );
        let request = serde_json::to_value(Envelope {
            id: 9,
            request: Request::Transcript {
                session: "term-1-a".into(),
                max_bytes: 4096,
                redact: true,
                run_id: None,
            },
        })
        .unwrap();
        assert_eq!(
            request,
            serde_json::json!({"id": 9, "op": "transcript", "session": "term-1-a", "max_bytes": 4096, "redact": true})
        );
        // A run id rides the same request when the caller has the run, and is
        // left out when it has not, so a request without one is the request
        // protocol 3 always had.
        let named = serde_json::to_value(Envelope {
            id: 9,
            request: Request::Transcript {
                session: "term-1-a".into(),
                max_bytes: 4096,
                redact: false,
                run_id: Some("run-7".into()),
            },
        })
        .unwrap();
        assert_eq!(
            named,
            serde_json::json!({"id": 9, "op": "transcript", "session": "term-1-a", "max_bytes": 4096, "redact": false, "run_id": "run-7"})
        );
        assert_eq!(
            serde_json::from_value::<Envelope>(named).unwrap().request,
            Request::Transcript {
                session: "term-1-a".into(),
                max_bytes: 4096,
                redact: false,
                run_id: Some("run-7".into())
            }
        );
        // `redact` is optional on the wire: left out it is off.
        let bare: Envelope = serde_json::from_value(
            serde_json::json!({"id": 1, "op": "transcript", "session": "term-1-a", "max_bytes": 10}),
        )
        .unwrap();
        assert_eq!(
            bare.request,
            Request::Transcript {
                session: "term-1-a".into(),
                max_bytes: 10,
                redact: false,
                run_id: None
            }
        );
    }

    #[test]
    fn a_transcript_answer_names_which_of_its_states_it_is() {
        use crate::terminal::{TranscriptRead, TranscriptTail};
        for (answer, state) in [
            (TranscriptRead::Missing, "missing"),
            (
                TranscriptRead::Unreadable {
                    reason: "denied".into(),
                },
                "unreadable",
            ),
            (
                TranscriptRead::Unavailable {
                    reason: "in-process".into(),
                },
                "unavailable",
            ),
            (
                TranscriptRead::Reused {
                    reason: "a later session reused this id".into(),
                },
                "reused",
            ),
            (
                TranscriptRead::Found(TranscriptTail {
                    data_b64: "aGk=".into(),
                    bytes: 2,
                    retained: 2,
                    total_bytes: 2,
                    truncated: false,
                    rotated: false,
                    clipped: false,
                    gap_markers: 0,
                    unrecorded_bytes: 0,
                    redacted: false,
                    live: false,
                    settled: true,
                    recording_error: None,
                    run_id: Some("run-7".into()),
                }),
                "found",
            ),
        ] {
            let wire = serde_json::to_value(&answer).unwrap();
            assert_eq!(wire["state"], state);
            assert_eq!(
                serde_json::from_value::<TranscriptRead>(wire).unwrap(),
                answer
            );
        }
    }

    #[test]
    fn a_truncated_or_oversized_frame_is_an_error_not_a_frame() {
        let mut wire = Vec::new();
        write_frame(
            &mut wire,
            &Envelope {
                id: 1,
                request: Request::List,
            },
            b"xyz",
        )
        .unwrap();
        for cut in 1..wire.len() {
            let mut input = &wire[..cut];
            assert!(read_frame(&mut input).is_err(), "cut at {cut}");
        }
        let mut huge = Vec::new();
        huge.extend_from_slice(&((MAX_HEADER_BYTES + 1) as u32).to_be_bytes());
        assert!(read_frame(&mut huge.as_slice()).is_err());
        let zero = 0u32.to_be_bytes();
        assert!(read_frame(&mut zero.as_slice()).is_err());
        assert!(write_frame(&mut Vec::new(), &1, &vec![0; MAX_PAYLOAD_BYTES + 1]).is_err());
    }

    #[test]
    fn a_notice_crosses_only_with_names_this_build_knows() {
        let notice = crate::alerts::Notice {
            key: "term-1-a".into(),
            origin: crate::alerts::Origin::Terminal,
            label: "Claude Code".into(),
            place: Some("repo".into()),
            event: Some("session_ended"),
            detail: Some("done".into()),
            is_agent: true,
            channel: "exit",
            subject: None,
            subagent: false,
            agent_id: None,
            fully_idle: None,
            provider_session_id: None,
        };
        let wire = WireNotice::from_notice(&notice);
        assert_eq!(wire.clone().into_notice().unwrap(), notice);
        let mut unknown = wire.clone();
        unknown.channel = "smoke-signal".into();
        assert!(unknown.into_notice().is_err());
        let mut unknown = wire;
        unknown.event = Some("not_an_event".into());
        assert!(unknown.into_notice().is_err());
    }

    /// The subagent's id survives the hop, stays off the wire when there is
    /// none (a peer that refuses unknown fields reads such a notice as it
    /// always did), and a malformed one is refused rather than carried on.
    #[test]
    fn a_subagent_id_crosses_the_socket_and_an_absent_one_leaves_no_trace() {
        let notice = crate::alerts::Notice {
            key: "term-1-a".into(),
            origin: crate::alerts::Origin::Terminal,
            label: "Claude Code".into(),
            place: None,
            event: Some("session_ended"),
            detail: None,
            is_agent: true,
            channel: "exit",
            subject: None,
            subagent: true,
            agent_id: Some("a1b2c3d4e5f6".into()),
            fully_idle: None,
            provider_session_id: None,
        };
        let wire = WireNotice::from_notice(&notice);
        let text = serde_json::to_string(&wire).unwrap();
        let back: WireNotice = serde_json::from_str(&text).unwrap();
        assert_eq!(back.into_notice().unwrap(), notice);

        let mut bare = wire.clone();
        bare.agent_id = None;
        let json = serde_json::to_value(&bare).unwrap();
        assert!(json.get("agent_id").is_none(), "{json}");
        let older: WireNotice = serde_json::from_value(json).unwrap();
        assert_eq!(older.agent_id, None);

        for bad in ["", "has space", "../x", &"x".repeat(97)] {
            let mut malformed = wire.clone();
            malformed.agent_id = Some(bad.into());
            assert!(malformed.into_notice().is_err(), "{bad:?}");
        }
    }
}
