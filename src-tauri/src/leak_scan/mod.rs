//! In-process secret scanning over the credential table every gate shares.
//!
//! The detector is `dc_redact::find_secret`, the same per-line rule that
//! DevCouncil's `dc-verify` secret gate blocks commits on and that the ledger
//! redacts with. This module adds no shapes of its own: a second table is how
//! a key one gate stops slips through another (see `Cargo.toml` on
//! `dc-redact`). What it owns is everything around the detector — which bytes
//! are read, how they are bounded, and an account of every input it could not
//! read, so a scan that missed something can never be reported as clean.
//!
//! Kingfisher stays the opt-in deep scan of the whole working tree
//! (`crate::secrets`); this is the everyday path that has to be fast, local
//! and dependency-free.
//!
//! The module is pure over bytes. [`staged`] is the one place that runs git.

pub mod staged;

use serde::Serialize;
use std::ops::Range;
use std::sync::atomic::{AtomicU64, Ordering};

/// The detector, the version of the vendored table and the DevCouncil commit
/// it was vendored from, named in every report so a finding can be traced to
/// the rules that produced it. The commit is there because the table changes
/// without the version moving. Kept equal to `vendored/dc-redact/Cargo.toml`
/// and `vendored/VENDOR.json` by test.
pub const ENGINE: &str = "dc-redact 0.2.5 (devcouncil@b9b2314d)";

/// Rows carried in a refusal. A diff full of fixtures can match thousands of
/// lines; past this the count still says how many there were.
pub const MAX_REPORTED_HITS: usize = 200;
/// Skipped inputs carried in a refusal, for the same reason.
pub const MAX_REPORTED_SKIPS: usize = 200;

/// A credential shape found on one line. Deliberately without the value or
/// the line: the report must not become a second copy of the secret.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Hit {
    /// The dc-redact shape name, e.g. `github personal access token`.
    pub rule: &'static str,
    /// Repo-relative, as git spelled it.
    pub path: String,
    /// 1-based line in the new version of the file.
    pub line: u32,
}

/// Why an input was not read. Each one is a hole in the scan, and a hole is
/// reported, never folded into "clean".
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum SkipReason {
    /// Larger than the per-file cap.
    Oversized,
    /// Within the per-file cap, but the scan's byte budget was already spent.
    BudgetExhausted,
    /// Git could not produce its bytes.
    Unreadable,
}

impl SkipReason {
    pub fn describe(self) -> &'static str {
        match self {
            Self::Oversized => "larger than the scan reads",
            Self::BudgetExhausted => "past the scan's byte budget",
            Self::Unreadable => "could not be read",
        }
    }
}

/// One input the scan did not read.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Skip {
    pub path: String,
    pub reason: SkipReason,
    /// The input's size when git reported one.
    pub bytes: Option<u64>,
}

/// What one scan read and found.
///
/// `hits_total` and `skipped_total` count everything; the vectors hold at
/// most [`MAX_REPORTED_HITS`] / [`MAX_REPORTED_SKIPS`] rows.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
pub struct Tally {
    pub hits: Vec<Hit>,
    pub hits_total: u64,
    pub skipped: Vec<Skip>,
    pub skipped_total: u64,
    /// Inputs (files, diff sections, blobs) whose bytes were read.
    pub inputs_scanned: u64,
    pub lines_scanned: u64,
    pub bytes_scanned: u64,
}

impl Tally {
    /// Clean means: something was examined or nothing needed to be, nothing
    /// matched, and nothing was skipped. A skip is never clean.
    pub fn is_clean(&self) -> bool {
        self.hits_total == 0 && self.skipped_total == 0
    }

    /// Scan one line. `line` may carry a trailing `\r`; bytes that are not
    /// UTF-8 are read as U+FFFD, which no credential shape contains — every
    /// shape in the table is ASCII.
    pub fn scan_line(&mut self, path: &str, line_no: u32, line: &[u8]) {
        let line = line.strip_suffix(b"\r").unwrap_or(line);
        self.lines_scanned += 1;
        self.bytes_scanned += line.len() as u64;
        self.detect(path, line_no, line);
    }

    /// Run the detector over `bytes` as one line and record a hit. True when
    /// it matched.
    fn detect(&mut self, path: &str, line_no: u32, bytes: &[u8]) -> bool {
        let text = String::from_utf8_lossy(bytes);
        let Some(found) = dc_redact::find_secret(&text) else {
            return false;
        };
        self.hits_total += 1;
        if self.hits.len() < MAX_REPORTED_HITS {
            self.hits.push(Hit {
                rule: found.name,
                path: path.to_string(),
                line: line_no,
            });
        }
        true
    }

    /// Scan a whole file's bytes, numbering lines from 1. The same splitter
    /// a streamed blob goes through, fed in one piece.
    pub fn scan_blob(&mut self, path: &str, bytes: &[u8]) {
        let mut lines = BlobLines::new(self, path);
        lines.feed(bytes);
        lines.finish();
    }

    pub fn skip(&mut self, path: &str, reason: SkipReason, bytes: Option<u64>) {
        self.skipped_total += 1;
        if self.skipped.len() < MAX_REPORTED_SKIPS {
            self.skipped.push(Skip {
                path: path.to_string(),
                reason,
                bytes,
            });
        }
    }
}

/// Most bytes of one line handed to the detector at once. Binary files can be
/// one "line" tens of megabytes long; a longer line is read in windows of
/// this size, each repeating the last [`WINDOW_OVERLAP`] bytes of the one
/// before, so memory stays bounded and a credential across a window edge is
/// still whole in one window.
pub const LINE_WINDOW: usize = 64 * 1024;
/// Bytes each window repeats from the previous one. Longer than any shape in
/// the table, PEM header lines included.
pub const WINDOW_OVERLAP: usize = 1024;

/// A file's bytes split into numbered lines as they arrive, in any number of
/// pieces. The result does not depend on where the pieces were cut: windows
/// are placed from the start of each line, never from a chunk edge. A line
/// counts once in `lines_scanned`, and its bytes once in `bytes_scanned`,
/// however many windows read it, and gives at most one hit, as a short line
/// does.
pub struct BlobLines<'a> {
    tally: &'a mut Tally,
    path: &'a str,
    line_no: u32,
    /// The current line's bytes not yet read by a complete window: everything
    /// since the last window's start plus its overlap.
    pending: Vec<u8>,
    /// Bytes of the current line already counted in `bytes_scanned`.
    counted: usize,
    /// The current line already gave a hit.
    hit: bool,
}

impl<'a> BlobLines<'a> {
    pub fn new(tally: &'a mut Tally, path: &'a str) -> Self {
        tally.inputs_scanned += 1;
        Self {
            tally,
            path,
            line_no: 1,
            pending: Vec::new(),
            counted: 0,
            hit: false,
        }
    }

    pub fn feed(&mut self, mut bytes: &[u8]) {
        while let Some(newline) = bytes.iter().position(|b| *b == b'\n') {
            let (line, rest) = (&bytes[..newline], &bytes[newline + 1..]);
            if self.pending.is_empty() && self.counted == 0 && line.len() <= LINE_WINDOW {
                // The common case: a whole short line in one piece, read in
                // place without copying.
                self.tally.scan_line(self.path, self.line_no, line);
            } else {
                self.take(line);
                self.end_line();
            }
            self.line_no = self.line_no.saturating_add(1);
            bytes = rest;
        }
        self.take(bytes);
    }

    /// Read the last line, which has no newline after it (empty when the file
    /// ends with one).
    pub fn finish(mut self) {
        self.end_line();
    }

    /// Add part of the current line, reading every window it completes.
    fn take(&mut self, bytes: &[u8]) {
        self.pending.extend_from_slice(bytes);
        while self.pending.len() > LINE_WINDOW {
            if !self.hit {
                self.hit = self
                    .tally
                    .detect(self.path, self.line_no, &self.pending[..LINE_WINDOW]);
            }
            let step = LINE_WINDOW - WINDOW_OVERLAP;
            self.counted += step;
            self.pending.drain(..step);
        }
    }

    /// Read what is left of the current line and count it once.
    fn end_line(&mut self) {
        let rest = self.pending.strip_suffix(b"\r").unwrap_or(&self.pending);
        if !self.hit {
            self.tally.detect(self.path, self.line_no, rest);
        }
        self.tally.lines_scanned += 1;
        self.tally.bytes_scanned += (self.counted + rest.len()) as u64;
        self.pending.clear();
        self.counted = 0;
        self.hit = false;
    }
}

/// Scans that ran to a verdict, clean or not, since the process started. The
/// counter is the proof that the in-house path ran rather than none at all.
static SCANS_COMPLETED: AtomicU64 = AtomicU64::new(0);

pub fn scans_completed() -> u64 {
    SCANS_COMPLETED.load(Ordering::SeqCst)
}

pub(crate) fn note_scan_completed() {
    SCANS_COMPLETED.fetch_add(1, Ordering::SeqCst);
}

/// One file's section of a `git diff -p -U0` patch.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Section {
    /// Added lines: (1-based new-side line number, byte range in the patch
    /// without the leading `+`).
    pub added: Vec<(u32, Range<usize>)>,
    /// Git printed `Binary files … differ` instead of hunks, so the added
    /// bytes are not in the patch and have to be read from the blob.
    pub binary: bool,
}

/// Why a patch could not be read. Any of these fails the scan: a section the
/// parser misread is a section the scan did not cover.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PatchError {
    /// A hunk line before any section header.
    OrphanLine,
    /// An `@@` line this parser cannot read.
    BadHunkHeader,
    /// A hunk held a different number of added lines than its header said.
    HunkCountMismatch,
    /// A line inside a hunk that is none of `+`, `-`, ` `, `\`, `@@`, or the
    /// next section's header.
    UnexpectedHunkLine,
}

impl PatchError {
    pub fn describe(&self) -> &'static str {
        match self {
            Self::OrphanLine => "the diff had content before any file header",
            Self::BadHunkHeader => "the diff had a hunk header the scan could not read",
            Self::HunkCountMismatch => "a hunk's line count did not match its header",
            Self::UnexpectedHunkLine => "the diff had a line the scan could not classify",
        }
    }
}

/// A line that opens a new file section. `* Unmerged path` is git's whole
/// section for a conflicted entry under `--cached`.
fn opens_section(line: &[u8]) -> bool {
    line.starts_with(b"diff --git ")
        || line.starts_with(b"diff --cc ")
        || line.starts_with(b"diff --combined ")
        || line.starts_with(b"* Unmerged path ")
}

/// `@@ -a[,b] +c[,d] @@…` → (c, d). `d` defaults to 1.
fn hunk_new_side(line: &[u8]) -> Option<(u32, u32)> {
    let text = std::str::from_utf8(line).ok()?;
    let rest = text.strip_prefix("@@ -")?;
    let plus = rest.find(" +")?;
    let after = &rest[plus + 2..];
    let end = after.find(' ')?;
    let spec = &after[..end];
    let (start, count) = match spec.split_once(',') {
        Some((s, c)) => (s.parse().ok()?, c.parse().ok()?),
        None => (spec.parse().ok()?, 1),
    };
    after[end..].starts_with(" @@").then_some((start, count))
}

/// Parse a `git diff -p -U0 --no-color` patch into one [`Section`] per file,
/// in git's order.
///
/// Strict on purpose: every hunk's added-line count is checked against its
/// header, so a patch this parser misreads fails the scan instead of quietly
/// skipping the lines it lost.
pub fn parse_patch(patch: &[u8]) -> Result<Vec<Section>, PatchError> {
    let mut sections: Vec<Section> = Vec::new();
    // Inside a hunk: (next new-side line number, added lines still expected).
    let mut hunk: Option<(u32, u32)> = None;
    let mut offset = 0usize;
    let mut lines = patch.split(|b| *b == b'\n').peekable();
    while let Some(line) = lines.next() {
        let start = offset;
        offset += line.len() + 1;
        // The split yields an empty tail after a final newline.
        if line.is_empty() && lines.peek().is_none() {
            break;
        }
        if opens_section(line) {
            if let Some((_, remaining)) = hunk.take() {
                if remaining != 0 {
                    return Err(PatchError::HunkCountMismatch);
                }
            }
            sections.push(Section::default());
            continue;
        }
        let Some(section) = sections.last_mut() else {
            return Err(PatchError::OrphanLine);
        };
        if line.starts_with(b"@@ ") {
            if let Some((_, remaining)) = hunk.take() {
                if remaining != 0 {
                    return Err(PatchError::HunkCountMismatch);
                }
            }
            let (new_start, count) = hunk_new_side(line).ok_or(PatchError::BadHunkHeader)?;
            hunk = Some((new_start, count));
            continue;
        }
        match hunk.as_mut() {
            None => {
                // Extended header: index, mode, rename, ---/+++, Binary files.
                if line.starts_with(b"Binary files ") && line.ends_with(b" differ") {
                    section.binary = true;
                }
            }
            Some((next, remaining)) => match line.first() {
                Some(b'+') => {
                    if *remaining == 0 {
                        return Err(PatchError::HunkCountMismatch);
                    }
                    section.added.push((*next, start + 1..start + line.len()));
                    *next = next.saturating_add(1);
                    *remaining -= 1;
                }
                Some(b' ') => {
                    if *remaining == 0 {
                        return Err(PatchError::HunkCountMismatch);
                    }
                    *next = next.saturating_add(1);
                    *remaining -= 1;
                }
                Some(b'-') | Some(b'\\') => {}
                _ => return Err(PatchError::UnexpectedHunkLine),
            },
        }
    }
    if let Some((_, remaining)) = hunk {
        if remaining != 0 {
            return Err(PatchError::HunkCountMismatch);
        }
    }
    Ok(sections)
}

/// One entry of `git diff --raw -z --no-abbrev`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RawEntry {
    /// Status letter: `A`, `M`, `D`, `R`, `C`, `T`, `U`, …
    pub status: u8,
    /// The new side's mode, octal as git prints it (`100644`, `160000`, …).
    pub dst_mode: String,
    /// The new side's blob id; all zeros when there is none.
    pub dst_oid: String,
    /// The new side's path (the destination of a rename or copy).
    pub path: String,
}

impl RawEntry {
    /// A regular file or symlink whose staged bytes exist to be read: not a
    /// deletion, not a submodule commit, not an unmerged placeholder.
    pub fn has_blob(&self) -> bool {
        self.status != b'D'
            && self.status != b'U'
            && self.dst_mode != "160000"
            && !self.dst_oid.bytes().all(|b| b == b'0')
    }
}

/// Parse `git diff --raw -z --no-abbrev` output. `None` when it is not in
/// that shape, which fails the scan.
pub fn parse_raw(raw: &[u8]) -> Option<Vec<RawEntry>> {
    let mut entries = Vec::new();
    let mut fields = raw.split(|b| *b == 0);
    while let Some(meta) = fields.next() {
        if meta.is_empty() {
            // Trailing NUL.
            continue;
        }
        let meta = std::str::from_utf8(meta).ok()?;
        let meta = meta.strip_prefix(':')?;
        let parts: Vec<&str> = meta.split(' ').collect();
        if parts.len() != 5 {
            return None;
        }
        let status = *parts[4].as_bytes().first()?;
        let first = fields.next()?;
        // Renames and copies carry the source path, then the destination.
        let path = if matches!(status, b'R' | b'C') {
            fields.next()?
        } else {
            first
        };
        entries.push(RawEntry {
            status,
            dst_mode: parts[1].to_string(),
            dst_oid: parts[3].to_string(),
            path: String::from_utf8_lossy(path).into_owned(),
        });
    }
    Some(entries)
}

#[cfg(test)]
mod corpus_tests;
#[cfg(test)]
mod staged_tests;
#[cfg(test)]
mod tests;
