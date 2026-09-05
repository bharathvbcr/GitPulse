//! Newline-delimited JSON framing, bounded.
//!
//! Two places in this codebase read NDJSON off a pipe they do not control: the
//! MANVI sidecar's stdout, and `gitpulse-mcp`'s stdin. Both need the same
//! guarantee — *never buffer more than `max` bytes for one frame* — and only
//! one of them had it. The MCP server used `BufRead::lines()`, which grows a
//! `String` until it finds a newline: 600 MiB of newline-free input took the
//! process from 10 MiB to 616 MiB of resident memory, measured, with no
//! newline ever required.
//!
//! `lines()` has a second failure this replaces. It yields `Err` on the first
//! non-UTF-8 byte, and a reader that treats that as end-of-stream stops
//! serving — one stray byte killed the MCP server and it exited 0, so the
//! supervising client recorded a clean shutdown rather than a fault. Here the
//! two outcomes are distinct variants, and neither is EOF.

use std::io::BufRead;

/// Why a frame could not be delivered. `TooLong` and `NotUtf8` are faults in
/// one frame, not in the stream: the reader has already consumed through the
/// newline, so a caller can report the frame and keep going.
#[derive(Debug)]
pub enum FrameError {
    /// The frame exceeded `max` bytes. Contains the cap, for the message.
    TooLong(usize),
    /// The frame was not valid UTF-8.
    NotUtf8,
    /// The underlying stream failed. Unlike the other two, there is nothing to
    /// resynchronise to.
    Io(std::io::Error),
}

impl FrameError {
    /// True when the stream can still be read after this error.
    pub fn is_recoverable(&self) -> bool {
        matches!(self, Self::TooLong(_) | Self::NotUtf8)
    }

    pub fn message(&self) -> String {
        match self {
            Self::TooLong(max) => format!("message exceeds the {max} byte frame cap"),
            Self::NotUtf8 => "message is not valid UTF-8".to_string(),
            Self::Io(error) => format!("stream error: {error}"),
        }
    }
}

/// Read one newline-terminated frame without ever buffering more than `max`
/// bytes.
///
/// The buffer is fed chunk by chunk from `fill_buf`, so a peer streaming
/// gigabytes without a newline cannot balloon memory: past the cap the bytes
/// are consumed and dropped rather than accumulated, and the reader still
/// stops at the newline so the *next* frame starts clean.
///
/// - `Ok(Some(line))` — a complete in-budget frame, trailing newline stripped.
///   A blank line yields `Ok(Some(""))`; it is an empty frame, not an ending.
/// - `Ok(None)` — end of stream, and only that
/// - `Err(..)` — see [`FrameError`]
pub fn read_frame<R: BufRead>(reader: &mut R, max: usize) -> Result<Option<String>, FrameError> {
    let mut out: Vec<u8> = Vec::with_capacity(4096);
    let mut overflowed = false;
    let mut saw_data = false;
    loop {
        let available = match reader.fill_buf() {
            Ok(bytes) => bytes,
            Err(error) if error.kind() == std::io::ErrorKind::Interrupted => continue,
            Err(error) => return Err(FrameError::Io(error)),
        };
        if available.is_empty() {
            break;
        }
        saw_data = true;
        match available.iter().position(|&b| b == b'\n') {
            Some(pos) => {
                let chunk = &available[..=pos];
                if !overflowed && out.len() + chunk.len() <= max {
                    out.extend_from_slice(chunk);
                } else {
                    overflowed = true;
                }
                reader.consume(pos + 1);
                break;
            }
            None => {
                if !overflowed && out.len() + available.len() > max {
                    // Drop what was accumulated: the frame is already a
                    // write-off, and holding it would defeat the cap.
                    overflowed = true;
                    out.clear();
                } else if !overflowed {
                    out.extend_from_slice(available);
                }
                let len = available.len();
                reader.consume(len);
            }
        }
    }
    if !saw_data && out.is_empty() {
        return Ok(None);
    }
    if overflowed {
        return Err(FrameError::TooLong(max));
    }
    if out.is_empty() {
        return Ok(None);
    }
    while out.last() == Some(&b'\n') {
        out.pop();
    }
    // A trailing CR is what a Windows client's `\r\n` leaves behind. Dropping
    // it here means every reader does not have to.
    while out.last() == Some(&b'\r') {
        out.pop();
    }
    // A blank line is an empty frame, NOT end of stream. Collapsing the two
    // would let one stray newline look like a closed pipe, which is the same
    // class of bug as the invalid-UTF-8 shutdown above — and the sidecar's
    // caller already depends on the distinction.
    String::from_utf8(out)
        .map(Some)
        .map_err(|_| FrameError::NotUtf8)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::BufReader;

    fn read_all(input: &[u8], max: usize) -> Vec<Result<Option<String>, FrameError>> {
        let mut reader = BufReader::new(input);
        let mut out = Vec::new();
        loop {
            let frame = read_frame(&mut reader, max);
            let done = matches!(frame, Ok(None)) || matches!(&frame, Err(e) if !e.is_recoverable());
            out.push(frame);
            if done {
                return out;
            }
            assert!(out.len() < 64, "reader did not terminate");
        }
    }

    #[test]
    fn frames_are_split_on_newlines_and_the_newline_is_stripped() {
        let frames = read_all(b"one\ntwo\nthree\n", 1024);
        let values: Vec<_> = frames
            .iter()
            .filter_map(|f| f.as_ref().ok().and_then(|o| o.clone()))
            .collect();
        assert_eq!(values, vec!["one", "two", "three"]);
    }

    #[test]
    fn a_final_frame_without_a_trailing_newline_is_still_delivered() {
        let frames = read_all(b"only", 1024);
        assert_eq!(frames[0].as_ref().unwrap().as_deref(), Some("only"));
    }

    #[test]
    fn a_windows_line_ending_does_not_leave_a_carriage_return_in_the_frame() {
        let frames = read_all(b"{\"a\":1}\r\n", 1024);
        assert_eq!(frames[0].as_ref().unwrap().as_deref(), Some("{\"a\":1}"));
    }

    #[test]
    fn an_oversized_frame_is_reported_and_the_next_frame_still_arrives() {
        // The whole point of the cap: the stream survives the bad frame.
        let mut input = vec![b'x'; 5000];
        input.push(b'\n');
        input.extend_from_slice(b"after\n");
        let frames = read_all(&input, 1024);
        assert!(matches!(frames[0], Err(FrameError::TooLong(1024))));
        assert!(frames[0].as_ref().unwrap_err().is_recoverable());
        assert_eq!(frames[1].as_ref().unwrap().as_deref(), Some("after"));
    }

    #[test]
    fn a_frame_with_no_newline_at_all_cannot_grow_past_the_cap() {
        // 4 MiB of input against a 1 KiB cap. Without the chunked drop this is
        // where the measured 616 MiB came from.
        let input = vec![b'x'; 4 * 1024 * 1024];
        let mut reader = BufReader::new(&input[..]);
        assert!(matches!(
            read_frame(&mut reader, 1024),
            Err(FrameError::TooLong(1024))
        ));
    }

    #[test]
    fn invalid_utf8_is_a_recoverable_frame_error_not_end_of_stream() {
        // The regression: `lines()` returned Err here and the MCP server
        // treated it as EOF, discarding every request behind it.
        let frames = read_all(b"good\n\xff\xfe\nafter\n", 1024);
        assert_eq!(frames[0].as_ref().unwrap().as_deref(), Some("good"));
        assert!(matches!(frames[1], Err(FrameError::NotUtf8)));
        assert!(frames[1].as_ref().unwrap_err().is_recoverable());
        assert_eq!(frames[2].as_ref().unwrap().as_deref(), Some("after"));
    }

    #[test]
    fn a_blank_line_is_an_empty_frame_not_end_of_stream() {
        // The distinction the caller depends on: if a blank line returned
        // `Ok(None)`, one stray newline would read as a closed pipe and every
        // message behind it would be discarded.
        let frames = read_all(b"\n\n{\"a\":1}\n", 1024);
        let values: Vec<_> = frames
            .iter()
            .filter_map(|f| f.as_ref().ok().and_then(|o| o.clone()))
            .collect();
        assert_eq!(values, vec!["", "", "{\"a\":1}"]);
    }

    #[test]
    fn an_empty_stream_is_end_of_stream_immediately() {
        let mut reader = BufReader::new(&b""[..]);
        assert!(read_frame(&mut reader, 1024).unwrap().is_none());
    }

    #[test]
    fn a_frame_exactly_at_the_cap_is_delivered() {
        // Off-by-one at the boundary: the frame is `max` bytes including its
        // newline, which is what the accumulator compares.
        let mut input = vec![b'x'; 1023];
        input.push(b'\n');
        let mut reader = BufReader::new(&input[..]);
        assert_eq!(read_frame(&mut reader, 1024).unwrap().unwrap().len(), 1023);
    }

    #[test]
    fn multibyte_characters_survive_a_chunk_boundary() {
        // `fill_buf` can split a UTF-8 sequence across chunks; decoding is done
        // once at the end for exactly this reason.
        let text = "héllo wörld ✓".repeat(500);
        let input = format!("{text}\n").into_bytes();
        let mut reader = BufReader::with_capacity(7, &input[..]);
        assert_eq!(read_frame(&mut reader, 1 << 20).unwrap().unwrap(), text);
    }
}
