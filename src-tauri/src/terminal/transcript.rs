//! What a saved terminal transcript read answers.
//!
//! The terminal host (`host::transcripts`) appends every session's output to a
//! bounded log on disk; this is the shape of reading it back. It is defined
//! outside `host` because the answer reaches the renderer through a command
//! that exists on every platform, while only a unix host can keep a transcript.
//!
//! Five answers, and none of them stands in for another: a transcript that was
//! never kept (`missing`) is not one that could not be read (`unreadable`), is
//! not one that belongs to a different session than the run asked about
//! (`reused`), and neither is the app saying it has no host to ask
//! (`unavailable`).

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
    /// The read named a run, and what the host saved under this terminal id is
    /// not that run's: a later session reused the id (terminal ids are
    /// `term-<pid>-<count>`, so they repeat across reboots), or the transcript
    /// there was recorded for no run. The run's own output was not kept.
    /// Never shown as the run's output.
    Reused { reason: String },
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
    /// The start of the session's output is not in `data_b64`: `rotated` or
    /// `clipped`.
    pub truncated: bool,
    /// Earlier output was rotated off the disk to keep the transcript within
    /// its size limit, so it is gone, not merely not shown.
    pub rotated: bool,
    /// The read was cut to its byte bound: older output is still saved but is
    /// not in `data_b64`. Both this and `rotated` can hold.
    pub clipped: bool,
    /// Places in `data_b64` where the host could not keep up and dropped
    /// output, each marked in the text itself.
    pub gap_markers: u32,
    /// Bytes the session printed that the host dropped and that no marker in
    /// `data_b64` accounts for: dropped while the writer was behind and not yet
    /// marked (the marker is written ahead of the next chunk that fits, or when
    /// the session ends), or dropped after the host began shutting down, which
    /// will never be marked. `0` means none were pending when the host answered.
    /// A marker written for the same bytes while the read ran can name them
    /// again; they are never counted as zero.
    pub unrecorded_bytes: u64,
    /// Secrets were masked (`ledger::redact`) before this left the host.
    pub redacted: bool,
    /// The session is still running, so more output may follow.
    pub live: bool,
    /// The host had written everything it had been given when it answered.
    /// `false` means the tail may lag the session by what was still queued.
    pub settled: bool,
    /// Why the saved output may stop short of what the session printed: a
    /// write failed (disk full, a permission change), or the host that wrote it
    /// ended without closing it, so whether its end is missing is unknown.
    /// `None` only for a session still being recorded without error, or one
    /// whose end was written down.
    pub recording_error: Option<String>,
    /// The run the session was spawned for, as the host recorded it; `None`
    /// when it was spawned for none. A read that named a run only ever returns
    /// that run's.
    pub run_id: Option<String>,
}
