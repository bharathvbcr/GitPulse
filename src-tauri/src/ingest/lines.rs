//! Reading a transcript's unread tail one complete line at a time, in bounded
//! memory.
//!
//! Catch-up used to read the whole unread span of a transcript into one `Vec`
//! before looking at a single line. A cold pass starts every transcript at 0,
//! so peak memory was the largest transcript on the machine (images and base64
//! tool output make 400 MB sessions), allocated before the pass's deadline was
//! consulted once. This reader holds one chunk plus one line, whatever the file
//! size, and a line longer than the cap is counted and skipped rather than
//! buffered.

use std::io::Read;

/// Bytes requested from the file per read.
pub(super) const CHUNK: usize = 1 << 20;

/// The longest line kept in memory. A longer one is reported as
/// [`Line::body`]` == None` and its bytes are discarded as they stream by.
pub(super) const MAX_LINE: usize = 8 << 20;

/// One complete, newline-terminated line of the span being read.
pub(super) struct Line<'a> {
    /// Byte offset of the line's first byte in the file.
    pub offset: u64,
    /// Bytes the line occupies including its terminating newline: what the
    /// transcript's cursor advances by.
    pub len: u64,
    /// The line without its newline, or `None` when it exceeded the cap and
    /// was not kept.
    pub body: Option<&'a [u8]>,
}

/// Streams the complete lines of the first `span` bytes of `source`.
///
/// The span is the length observed before the read began, so a transcript that
/// keeps growing under the reader is not chased. A trailing fragment with no
/// newline yet may still be being written: it is not returned, and its bytes
/// are not part of any line's `len`, so the caller's cursor stops before it.
pub(super) struct LineReader<R> {
    source: R,
    chunk: usize,
    max_line: usize,
    /// Bytes of the span not yet read from `source`.
    unread: u64,
    buffer: Vec<u8>,
    /// Start of the unconsumed bytes in `buffer`.
    head: usize,
    /// File offset of `buffer[head]`.
    offset: u64,
    /// While inside an over-long line: the bytes of it already thrown away.
    discarded: Option<u64>,
    #[cfg(test)]
    high_water: usize,
    #[cfg(test)]
    read_total: u64,
}

impl LineReader<std::fs::File> {
    /// A reader over the bytes of `file` from `start` up to `len` (the length
    /// observed before the pass began).
    pub fn seek(mut file: std::fs::File, start: u64, len: u64) -> std::io::Result<Self> {
        use std::io::Seek;
        file.seek(std::io::SeekFrom::Start(start))?;
        Ok(Self::new(file, start, len.saturating_sub(start)))
    }
}

impl<R: Read> LineReader<R> {
    /// A reader over `source`, positioned at `start` in the file, for `span`
    /// bytes.
    pub fn new(source: R, start: u64, span: u64) -> Self {
        Self::with_limits(source, start, span, CHUNK, MAX_LINE)
    }

    pub fn with_limits(source: R, start: u64, span: u64, chunk: usize, max_line: usize) -> Self {
        Self {
            source,
            chunk: chunk.max(1),
            max_line: max_line.max(1),
            unread: span,
            buffer: Vec::new(),
            head: 0,
            offset: start,
            discarded: None,
            #[cfg(test)]
            high_water: 0,
            #[cfg(test)]
            read_total: 0,
        }
    }

    /// The most bytes this reader has held at once: the bound the memory test
    /// asserts, independent of the size of the file.
    #[cfg(test)]
    pub fn high_water(&self) -> usize {
        self.high_water
    }

    /// Bytes pulled from the source so far.
    #[cfg(test)]
    pub fn bytes_read(&self) -> u64 {
        self.read_total
    }

    /// The next complete line, or `None` once the span holds no more.
    pub fn next_line(&mut self) -> std::io::Result<Option<Line<'_>>> {
        loop {
            let unconsumed = &self.buffer[self.head..];
            if let Some(newline) = unconsumed.iter().position(|b| *b == b'\n') {
                let taken = newline + 1;
                let offset = self.offset;
                let start = self.head;
                self.head += taken;
                self.offset += taken as u64;
                return Ok(Some(match self.discarded.take() {
                    Some(already) => Line {
                        offset: offset - already,
                        len: already + taken as u64,
                        body: None,
                    },
                    // The whole line arrived in one chunk, but it is past the
                    // cap all the same.
                    None if newline > self.max_line => Line {
                        offset,
                        len: taken as u64,
                        body: None,
                    },
                    None => Line {
                        offset,
                        len: taken as u64,
                        body: Some(&self.buffer[start..start + newline]),
                    },
                }));
            }
            // No newline in what is held. Past the cap the line is not worth
            // keeping: drop the bytes and remember how many, so the line is
            // still accounted for when its end arrives.
            if unconsumed.len() > self.max_line {
                let dropped = unconsumed.len() as u64;
                self.offset += dropped;
                *self.discarded.get_or_insert(0) += dropped;
                self.buffer.clear();
                self.head = 0;
            } else if self.head > 0 {
                self.buffer.drain(..self.head);
                self.head = 0;
            }
            if self.unread == 0 {
                return Ok(None);
            }
            let want = self
                .chunk
                .min(usize::try_from(self.unread).unwrap_or(usize::MAX));
            let filled = self.buffer.len();
            self.buffer.resize(filled + want, 0);
            let read = loop {
                match self.source.read(&mut self.buffer[filled..]) {
                    Ok(read) => break read,
                    Err(error) if error.kind() == std::io::ErrorKind::Interrupted => {}
                    Err(error) => {
                        self.buffer.truncate(filled);
                        return Err(error);
                    }
                }
            };
            self.buffer.truncate(filled + read);
            #[cfg(test)]
            {
                self.high_water = self.high_water.max(self.buffer.len());
                self.read_total += read as u64;
            }
            if read == 0 {
                // The file ended before the span did (it was truncated under
                // us): whatever is held is a fragment.
                self.unread = 0;
                return Ok(None);
            }
            self.unread -= read as u64;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn collect(
        text: &[u8],
        start: u64,
        chunk: usize,
        cap: usize,
    ) -> Vec<(u64, u64, Option<String>)> {
        let mut reader = LineReader::with_limits(text, start, text.len() as u64, chunk, cap);
        let mut lines = Vec::new();
        while let Some(line) = reader.next_line().unwrap() {
            lines.push((
                line.offset,
                line.len,
                line.body.map(|b| String::from_utf8(b.to_vec()).unwrap()),
            ));
        }
        lines
    }

    #[test]
    fn lines_carry_their_offsets_whatever_the_chunking() {
        let text = b"alpha\nbe\n\ngamma\n";
        let expected = vec![
            (100, 6, Some("alpha".to_string())),
            (106, 3, Some("be".to_string())),
            (109, 1, Some(String::new())),
            (110, 6, Some("gamma".to_string())),
        ];
        for chunk in [1, 2, 3, 5, 7, 64] {
            assert_eq!(collect(text, 100, chunk, 1024), expected, "chunk {chunk}");
        }
    }

    #[test]
    fn a_trailing_fragment_is_left_for_the_next_pass() {
        let lines = collect(b"one\ntwo\nthr", 0, 4, 1024);
        assert_eq!(lines.len(), 2);
        assert_eq!(lines.iter().map(|l| l.1).sum::<u64>(), 8);
    }

    #[test]
    fn an_oversize_line_is_counted_but_not_kept() {
        let mut text = b"ok\n".to_vec();
        text.extend(std::iter::repeat_n(b'x', 5000));
        text.extend_from_slice(b"\nafter\n");
        for chunk in [7, 64, 4096, 65_536] {
            let lines = collect(&text, 0, chunk, 100);
            assert_eq!(lines.len(), 3, "chunk {chunk}");
            assert_eq!(lines[0], (0, 3, Some("ok".to_string())));
            assert_eq!(lines[1], (3, 5001, None), "chunk {chunk}");
            assert_eq!(lines[2], (5004, 6, Some("after".to_string())));
        }
    }

    #[test]
    fn an_unfinished_oversize_line_is_not_consumed() {
        let mut text = b"ok\n".to_vec();
        text.extend(std::iter::repeat_n(b'x', 5000));
        let lines = collect(&text, 0, 64, 100);
        assert_eq!(lines.len(), 1, "a line still being written must be left");
    }

    /// The shipped limits, on a transcript of realistic lines forty times the
    /// chunk: the reader never holds more than one chunk past a line.
    #[test]
    fn the_shipped_limits_hold_one_chunk_of_a_large_transcript() {
        let line = "z".repeat(2047) + "\n";
        let text = line.repeat(20_000); // ~40 MiB
        let mut reader = LineReader::new(text.as_bytes(), 0, text.len() as u64);
        let mut count = 0usize;
        while reader.next_line().unwrap().is_some() {
            count += 1;
        }
        assert_eq!(count, 20_000);
        assert!(
            reader.high_water() <= CHUNK + line.len(),
            "held {} bytes of a {} byte transcript",
            reader.high_water(),
            text.len()
        );
    }

    #[test]
    fn memory_is_bounded_by_the_chunk_and_the_cap_not_by_the_file() {
        // Eight thousand lines of 600 bytes: a 4.8 MB stand-in for a transcript
        // far larger than anything the reader may hold.
        let line = "y".repeat(599) + "\n";
        let text = line.repeat(8000);
        let (chunk, cap) = (4096, 2048);
        let mut reader = LineReader::with_limits(text.as_bytes(), 0, text.len() as u64, chunk, cap);
        let mut count = 0;
        while reader.next_line().unwrap().is_some() {
            count += 1;
        }
        assert_eq!(count, 8000);
        assert!(
            reader.high_water() <= chunk + cap + 1,
            "held {} bytes of a {} byte file",
            reader.high_water(),
            text.len()
        );
    }
}
