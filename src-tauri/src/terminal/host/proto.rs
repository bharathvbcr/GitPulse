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
    SpawnRequest, TerminalContext, TerminalExitPayload, TerminalListing, TerminalLossPayload,
    TerminalSpawned,
};
use serde::{Deserialize, Serialize};
use std::io::{self, Read, Write};

/// What this build speaks. Bumped whenever a frame changes shape or meaning.
pub const PROTOCOL: u32 = 1;

/// Longest JSON header accepted. A spawn request (argv and environment are
/// bounded at 128 KiB each by `validate_pty_options`) is the largest a caller
/// can send, so the bound is generous and still bounded.
pub const MAX_HEADER_BYTES: usize = 512 * 1024;

/// Longest payload: one output message (`OUTPUT_CHUNK_BYTES`), one write's
/// worth of keystrokes (`MAX_PTY_INPUT_BYTES`), whichever is larger.
pub const MAX_PAYLOAD_BYTES: usize = 64 * 1024;

/// App → host. Every request but [`Request::Ack`] gets exactly one reply.
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
    /// Never answered: sent per acknowledgement, inline.
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
    LaneThreads,
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
}

/// The channels a terminal's own output can raise a notice on: the scanner's,
/// and `exit` for the end of a session.
const TERMINAL_CHANNELS: [&str; 5] = ["bell", "osc9", "osc777", "osc99", "exit"];

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
    match read_exact_or_eof(input, &mut length)? {
        false => return Ok(None),
        true => {}
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
}
