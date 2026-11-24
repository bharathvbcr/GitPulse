//! Protocol 4, frozen: the frames an app of a later protocol needs to talk to
//! a still-running host of protocol 4 about the sessions it holds.
//!
//! When an update bumps `proto::PROTOCOL` the new app starts a host of its own
//! (`ptyd-v{N}.sock`), and every session a protocol-4 host still holds is
//! invisible to it. This module is how it sees them anyway. It is a pinned
//! copy of the v4 wire shapes for a core subset, and it deliberately imports
//! nothing from `proto.rs` or from the live terminal types (`TerminalListing`,
//! `TranscriptRead`): those move on with every protocol, and this copy must
//! not move with them. Edit it only to fix a mistake against what a v4 host
//! really sends, never to follow a later protocol; `compat_v4_tests.rs` pins
//! every shape against literal JSON and, while `PROTOCOL` is 4, against
//! `proto.rs` itself.
//!
//! The subset is what stage one of `legacy` uses: `Hello` (become the
//! connected app), `List`, `Kill`, and the read-only `Transcript`. Everything a
//! v4 host sends that is not a reply (`output`, `end`, `loss`, `notice`,
//! `scan_drops`, `exit`) decodes as [`HostFrame::Other`] and is skipped: the
//! legacy client never attaches, so none of it is addressed to a view.
//!
//! A later protocol that needs its own legacy dialect adds `compat_v5.rs` the
//! same way and an entry in `legacy::COMPAT_PROTOCOLS`.

use serde::{Deserialize, Serialize};
use std::io::{self, Read, Write};

/// The protocol this copy speaks.
pub const PROTOCOL: u32 = 4;

/// Longest JSON header a v4 host sends or accepts.
pub const MAX_HEADER_BYTES: usize = 512 * 1024;

/// Longest payload a v4 host sends or accepts.
pub const MAX_PAYLOAD_BYTES: usize = 64 * 1024;

/// The largest transcript tail a v4 host answers a read with.
pub const MAX_TRANSCRIPT_TAIL_BYTES: usize = 128 * 1024;

/// App → host, the subset. Serializes exactly as v4's `proto::Request` does.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "op", rename_all = "snake_case")]
pub enum Request {
    Hello {
        protocol: u32,
        build: String,
    },
    Kill {
        session: String,
    },
    List,
    Transcript {
        session: String,
        max_bytes: usize,
        #[serde(default)]
        redact: bool,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        run_id: Option<String>,
    },
}

/// A request with the id its reply will carry.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Envelope {
    pub id: u64,
    #[serde(flatten)]
    pub request: Request,
}

/// Host → app, the subset: a reply, or any other frame, which is skipped.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum HostFrame {
    Reply {
        id: u64,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        ok: Option<serde_json::Value>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        error: Option<String>,
    },
    /// `output`, `end`, `loss`, `notice`, `scan_drops` or `exit`.
    #[serde(other)]
    Other,
}

/// Answer to `Hello`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Hello {
    pub protocol: u32,
    pub build: String,
    pub pid: u32,
    pub sessions: usize,
}

/// A session the host holds, as v4's `list` reports it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Listing {
    pub id: String,
    pub shell: String,
    pub cwd: String,
    pub repo: String,
    pub launcher: Option<String>,
    pub run_id: Option<String>,
    pub detached: bool,
    #[serde(default)]
    pub ending: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parent_session_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    #[serde(default)]
    pub quiet_ms: Option<u64>,
}

/// Answer to `Transcript`, as v4 sends it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum TranscriptRead {
    Found(TranscriptTail),
    Missing,
    Unreadable { reason: String },
    Reused { reason: String },
    Unavailable { reason: String },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TranscriptTail {
    pub data_b64: String,
    pub bytes: usize,
    pub retained: u64,
    #[serde(default)]
    pub total_bytes: u64,
    pub truncated: bool,
    pub rotated: bool,
    pub clipped: bool,
    pub gap_markers: u32,
    pub unrecorded_bytes: u64,
    pub redacted: bool,
    pub live: bool,
    pub settled: bool,
    pub recording_error: Option<String>,
    pub run_id: Option<String>,
}

/// Writes one frame in a single `write_all`: a 4-byte big-endian header
/// length, the JSON header, a 4-byte big-endian payload length, the payload.
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
                "a protocol 4 frame of {} + {} bytes is over its bound",
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
/// end of stream between frames; an end inside one, or a length over its
/// bound, is an error.
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
        format!("a protocol 4 frame's {what} of {len} bytes is out of bounds"),
    )
}

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
#[path = "compat_v4_tests.rs"]
mod tests;
