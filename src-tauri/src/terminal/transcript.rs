//! What a saved terminal transcript read answers.
//!
//! The terminal host (`host::transcripts`) appends every session's output to a
//! bounded log on disk; this is the shape of reading it back. It is defined
//! outside `host` because the answer reaches the renderer through a command
//! that exists on every platform, while only a unix host can keep a transcript.
//!
//! Four answers, and none of them stands in for another: a transcript that was
//! never kept (`missing`) is not one that could not be read (`unreadable`), and
//! neither is the app saying it has no host to ask (`unavailable`).

use serde::{Deserialize, Serialize};

/// The largest tail one read returns. The tail crosses the host socket as
/// base64 inside a reply header (at most 512 KiB), so this is the bound.
pub const MAX_TRANSCRIPT_TAIL_BYTES: usize = 128 * 1024;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum TranscriptRead {
    /// The newest output of the session, oldest byte first.
    Found(TranscriptTail),
    /// No transcript exists for this session: it was never recorded, was
    /// evicted, or aged out.
    Missing,
    /// One exists, or should, and could not be read; `reason` says why.
    Unreadable { reason: String },
    /// This app has no terminal host to ask, so nothing is recorded anywhere
    /// it could look. Never sent by a host.
    Unavailable { reason: String },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TranscriptTail {
    /// The raw bytes of the tail as the terminal printed them (escape
    /// sequences included), base64.
    pub data_b64: String,
    /// How many bytes `data_b64` decodes to.
    pub bytes: usize,
    /// Bytes of the session's output that are still on disk. More than
    /// `bytes` when the tail was clipped.
    pub retained: u64,
    /// The start of the session's output is not in `data_b64`: the read was
    /// clipped to its bound, or earlier output was rotated away.
    pub truncated: bool,
    /// Places in `data_b64` where the host could not keep up and dropped
    /// output, each marked in the text itself.
    pub gap_markers: u32,
    /// Secrets were masked (`ledger::redact`) before this left the host.
    pub redacted: bool,
    /// The session is still running, so more output may follow.
    pub live: bool,
    /// The host had written everything it had been given when it answered.
    /// `false` means the tail may lag the session by what was still queued.
    pub settled: bool,
    /// Why recording stopped short, when a write failed (disk full, a
    /// permission change). Output after that point is not in the log.
    pub recording_error: Option<String>,
}
