//! Secret scan of what the next commit would add, and the commit gate on it.
//!
//! Reads the index against `HEAD` the way `git commit` will record it: the
//! added lines of every text file, and the whole staged blob of every file git
//! calls binary (its bytes are not in the patch). Removed lines are not read —
//! they are already in history, and stopping a commit that deletes a key would
//! only keep the key around longer.
//!
//! Fails closed. A git command that fails, times out or overflows its output
//! cap, a patch the parser cannot account for line by line, and a blob that
//! could not be read all refuse the commit with the reason. Only a person, in
//! the commit view, can let a refused commit through ([`Policy::HumanOverride`]).

use super::{
    note_scan_completed, parse_patch, parse_raw, BlobLines, RawEntry, SkipReason, Tally, ENGINE,
};
use crate::engine::git_cli::{git, git_observed, git_with_stdin};
use crate::engine::process::{
    OutputStream, ProcessObserver, DEFAULT_TIMEOUT, MAX_DRAINED_OUTPUT_BYTES,
};
use crate::ipc_error::{CommandError, IpcErrorKind};
use std::path::Path;
use std::time::Instant;

/// Largest staged binary blob read in the one shared `cat-file --batch`.
/// Larger blobs are streamed one at a time instead.
pub const BATCH_BLOB_BYTES: u64 = 8 * 1024 * 1024;
/// Bytes of binary blobs read in the shared batch. Kept under the runner's
/// 64 MiB output cap so the batch itself cannot overflow; blobs past it are
/// streamed.
pub const BATCH_BUDGET_BYTES: u64 = 48 * 1024 * 1024;
/// Bytes of binary blobs streamed per scan, never held in memory: at the
/// engine's ~26 MB/s measured on a loaded host, about ten seconds. A blob
/// past what is left is skipped, and the skip refuses the commit like a
/// finding does.
pub const STREAM_BUDGET_BYTES: u64 = 256 * 1024 * 1024;
// One streamed blob's cap is its size plus one, so the whole budget must fit
// what the runner lets a draining read declare.
const _: () = assert!(STREAM_BUDGET_BYTES < MAX_DRAINED_OUTPUT_BYTES as u64);

/// Options every diff here runs with, so a user's diff configuration cannot
/// change what the scan reads: no external diff driver, no textconv filter,
/// no colour codes, renames detected the same way in both listings.
const DIFF_OPTIONS: [&str; 5] = [
    "--cached",
    "--no-ext-diff",
    "--no-textconv",
    "--no-color",
    "-M",
];

/// A scan that could not reach a verdict. Fixed wording: git's own stderr can
/// quote file contents, so none of it is forwarded.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ScanFailure(pub String);

/// A completed scan of the staged changes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StagedScan {
    pub tally: Tally,
    pub duration_ms: u64,
}

fn diff_args<'a>(extra: &[&'a str], pathspec: &'a [String]) -> Vec<&'a str> {
    let mut args = vec!["diff"];
    args.extend_from_slice(&DIFF_OPTIONS);
    args.extend_from_slice(extra);
    if !pathspec.is_empty() {
        args.push("--");
        args.extend(pathspec.iter().map(String::as_str));
    }
    args
}

/// Scan what the next commit would add, limited to `pathspec` when it is not
/// empty. Pass the exact pathspec strings the commit itself is given, so the
/// scan and the commit cannot select different files.
pub fn scan_staged(repo: &Path, pathspec: &[String]) -> Result<StagedScan, ScanFailure> {
    let started = Instant::now();
    let raw = git(repo, &diff_args(&["--raw", "-z", "--no-abbrev"], pathspec))
        .map_err(|_| ScanFailure("git could not list the staged changes".into()))?;
    let entries = parse_raw(&raw).ok_or_else(|| {
        ScanFailure("the staged-change listing was not in the expected form".into())
    })?;
    let patch = git(repo, &diff_args(&["-p", "-U0"], pathspec))
        .map_err(|_| ScanFailure("git could not produce the staged diff".into()))?;
    let sections = parse_patch(&patch).map_err(|e| ScanFailure(e.describe().into()))?;
    // Both listings come from one diff queue, so they name the same files in
    // the same order. If they disagree, which lines belong to which file is
    // unknown, and so is the verdict.
    if sections.len() != entries.len() {
        return Err(ScanFailure(format!(
            "the staged diff named {} files and the listing {}",
            sections.len(),
            entries.len()
        )));
    }

    let mut tally = Tally::default();
    let mut blobs: Vec<&RawEntry> = Vec::new();
    for (section, entry) in sections.iter().zip(&entries) {
        if section.binary {
            if entry.has_blob() {
                blobs.push(entry);
            }
            continue;
        }
        if section.added.is_empty() {
            continue;
        }
        tally.inputs_scanned += 1;
        for (line_no, range) in &section.added {
            tally.scan_line(&entry.path, *line_no, &patch[range.clone()]);
        }
    }
    if !blobs.is_empty() {
        scan_blobs(repo, &blobs, &mut tally)?;
    }
    note_scan_completed();
    Ok(StagedScan {
        tally,
        duration_ms: u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX),
    })
}

/// Read and scan staged binary blobs by object id, within the per-blob cap
/// and the scan's budget. Every blob left unread is a typed skip.
pub(super) fn scan_blobs(
    repo: &Path,
    blobs: &[&RawEntry],
    tally: &mut Tally,
) -> Result<(), ScanFailure> {
    let mut request = Vec::new();
    for entry in blobs {
        request.extend_from_slice(entry.dst_oid.as_bytes());
        request.push(b'\n');
    }
    let checked = git_with_stdin(repo, &["cat-file", "--batch-check"], &request)
        .map_err(|_| ScanFailure("git could not size the staged binary files".into()))?;
    let lines: Vec<&[u8]> = checked
        .split(|b| *b == b'\n')
        .filter(|l| !l.is_empty())
        .collect();
    if lines.len() != blobs.len() {
        return Err(ScanFailure(
            "git sized a different number of staged binary files than were asked for".into(),
        ));
    }
    let mut wanted: Vec<&RawEntry> = Vec::new();
    let mut streamed: Vec<(&RawEntry, u64)> = Vec::new();
    let mut batch_left = BATCH_BUDGET_BYTES;
    let mut stream_left = STREAM_BUDGET_BYTES;
    for (entry, line) in blobs.iter().zip(lines) {
        let text = String::from_utf8_lossy(line);
        let mut parts = text.split(' ');
        let (Some(oid), Some(kind), Some(size)) = (parts.next(), parts.next(), parts.next()) else {
            tally.skip(&entry.path, SkipReason::Unreadable, None);
            continue;
        };
        let size: Option<u64> = size.parse().ok();
        match (oid == entry.dst_oid, kind, size) {
            (true, "blob", Some(size)) if size <= BATCH_BLOB_BYTES && size <= batch_left => {
                batch_left -= size;
                wanted.push(entry);
            }
            (true, "blob", Some(size)) if size <= stream_left => {
                stream_left -= size;
                streamed.push((entry, size));
            }
            (true, "blob", Some(size)) if size > STREAM_BUDGET_BYTES => {
                tally.skip(&entry.path, SkipReason::Oversized, Some(size));
            }
            (true, "blob", Some(size)) => {
                tally.skip(&entry.path, SkipReason::BudgetExhausted, Some(size));
            }
            _ => tally.skip(&entry.path, SkipReason::Unreadable, size),
        }
    }
    for (entry, size) in streamed {
        stream_blob(repo, entry, size, tally);
    }
    if wanted.is_empty() {
        return Ok(());
    }
    let mut request = Vec::new();
    for entry in &wanted {
        request.extend_from_slice(entry.dst_oid.as_bytes());
        request.push(b'\n');
    }
    let out = git_with_stdin(repo, &["cat-file", "--batch"], &request)
        .map_err(|_| ScanFailure("git could not read the staged binary files".into()))?;
    let mut at = 0usize;
    for entry in wanted {
        let Some(header_end) = out[at..].iter().position(|b| *b == b'\n').map(|n| at + n) else {
            return Err(ScanFailure(
                "git returned fewer staged binary files than asked for".into(),
            ));
        };
        let header = String::from_utf8_lossy(&out[at..header_end]);
        let size: Option<usize> = header.rsplit(' ').next().and_then(|s| s.parse().ok());
        let body_start = header_end + 1;
        match size {
            Some(size) if header.starts_with(&entry.dst_oid) && body_start + size <= out.len() => {
                tally.scan_blob(&entry.path, &out[body_start..body_start + size]);
                // The body is followed by one newline.
                at = body_start + size + 1;
            }
            _ => {
                return Err(ScanFailure(
                    "git returned a staged binary file in an unexpected form".into(),
                ));
            }
        }
    }
    Ok(())
}

/// Feeds a streamed blob's bytes to the line splitter as git writes them, so
/// the run keeps none of them.
struct StreamedBlob<'a> {
    lines: BlobLines<'a>,
    read: u64,
}

impl ProcessObserver for StreamedBlob<'_> {
    fn output(&mut self, stream: OutputStream, bytes: &[u8]) {
        if matches!(stream, OutputStream::Stdout) {
            self.read += bytes.len() as u64;
            self.lines.feed(bytes);
        }
    }

    fn drains_stdout(&self) -> bool {
        true
    }
}

/// Scan one staged blob of `size` bytes as `git cat-file blob` writes it.
/// Anything short of the whole blob, read to a clean exit, is a skip: the
/// lines that did arrive are still scanned and their hits kept, but the blob
/// does not count as covered.
fn stream_blob(repo: &Path, entry: &RawEntry, size: u64, tally: &mut Tally) {
    // One byte over the declared size, so a stream longer than git said is
    // seen as such rather than cut to fit.
    let cap = usize::try_from(size)
        .unwrap_or(usize::MAX)
        .saturating_add(1);
    let mut blob = StreamedBlob {
        lines: BlobLines::new(tally, &entry.path),
        read: 0,
    };
    let run = git_observed(
        repo,
        &["cat-file", "blob", &entry.dst_oid],
        DEFAULT_TIMEOUT,
        cap,
        &mut blob,
    );
    let read = blob.read;
    blob.lines.finish();
    let whole = matches!(&run, Ok(run) if run.success && run.incomplete.is_none()) && read == size;
    if !whole {
        tally.skip(&entry.path, SkipReason::Unreadable, Some(size));
    }
}

/// Who may let a refused commit through.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Policy {
    /// Scan, and refuse on any finding, skip or failed scan.
    Enforce,
    /// A person saw the refusal in the commit view and chose to commit
    /// anyway. Reachable only from that button: no MCP tool or agent path
    /// constructs it.
    HumanOverride,
}

/// Rows named in the refusal's text; the detail carries the full lists.
const NAMED_IN_MESSAGE: usize = 5;

/// The commit gate. `Ok` means the staged changes were scanned and are clean,
/// or a person overrode the gate.
pub fn guard_commit(repo: &Path, pathspec: &[String], policy: Policy) -> Result<(), CommandError> {
    if policy == Policy::HumanOverride {
        log::warn!(
            target: "leak_scan",
            "commit proceeds past the secret scan on a person's explicit override"
        );
        return Ok(());
    }
    match scan_staged(repo, pathspec) {
        Ok(scan) if scan.tally.is_clean() => {
            log::info!(
                target: "leak_scan",
                "staged changes scanned clean by {ENGINE}: {} inputs, {} lines, {} bytes in {} ms",
                scan.tally.inputs_scanned,
                scan.tally.lines_scanned,
                scan.tally.bytes_scanned,
                scan.duration_ms
            );
            Ok(())
        }
        Ok(scan) => Err(refusal(Gate::Commit, Some(&scan.tally), None)),
        Err(failure) => Err(refusal(Gate::Commit, None, Some(&failure))),
    }
}

/// A path as it may appear in a refusal: file names are text an agent wrote,
/// so they pass the same redaction as everything else that is displayed.
fn shown(path: &str) -> String {
    dc_redact::redact_secrets(path)
}

/// Which git write a refusal stops. The scan and its report are the same for
/// both; what was stopped, and what fixing it takes, are not.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Gate {
    /// A commit of the staged changes ([`guard_commit`]).
    Commit,
    /// A push of the commits the remote does not have
    /// ([`super::range::guard_push`]).
    Push,
}

impl Gate {
    fn wording(self) -> GateWording {
        match self {
            Self::Commit => GateWording {
                name: "commit",
                stopped: "Commit blocked by the secret scan. Nothing was committed.",
                unread: "The staged changes could not be scanned",
                remedy: "A credential committed to history stays there after the file is fixed: remove it from the staged changes.",
                inputs: "staged file",
            },
            Self::Push => GateWording {
                name: "push",
                stopped: "Push blocked by the secret scan. Nothing was pushed.",
                unread: "The commits this push would send could not be scanned",
                remedy: "Each of these is in a commit the push would send, and a later commit that deletes it does not take it out of history: rewrite the commits that added it (an interactive rebase, for example) before pushing.",
                inputs: "input the push would send",
            },
        }
    }
}

struct GateWording {
    name: &'static str,
    stopped: &'static str,
    unread: &'static str,
    remedy: &'static str,
    inputs: &'static str,
}

/// The typed refusal. Names shapes, paths and line numbers — never a value
/// or the line it sits on.
pub fn refusal(gate: Gate, tally: Option<&Tally>, failure: Option<&ScanFailure>) -> CommandError {
    let words = gate.wording();
    let mut message = String::from(words.stopped);
    if let Some(failure) = failure {
        message.push_str(&format!("\n{}: {}.", words.unread, failure.0));
    }
    let empty = Tally::default();
    let tally = tally.unwrap_or(&empty);
    if tally.hits_total > 0 {
        message.push_str(&format!(
            "\n{} added line{} look{} like a credential:",
            tally.hits_total,
            if tally.hits_total == 1 { "" } else { "s" },
            if tally.hits_total == 1 { "s" } else { "" },
        ));
        for hit in tally.hits.iter().take(NAMED_IN_MESSAGE) {
            message.push_str(&format!(
                "\n  {} at {}:{}",
                hit.rule,
                shown(&hit.path),
                hit.line
            ));
        }
        let unnamed = tally.hits_total.saturating_sub(NAMED_IN_MESSAGE as u64);
        if unnamed > 0 {
            message.push_str(&format!("\n  and {unnamed} more"));
        }
        message.push('\n');
        message.push_str(words.remedy);
    }
    if tally.skipped_total > 0 {
        message.push_str(&format!(
            "\n{} {}{} could not be scanned:",
            tally.skipped_total,
            words.inputs,
            if tally.skipped_total == 1 { "" } else { "s" },
        ));
        for skip in tally.skipped.iter().take(NAMED_IN_MESSAGE) {
            message.push_str(&format!(
                "\n  {} ({})",
                shown(&skip.path),
                skip.reason.describe()
            ));
        }
        let unnamed = tally.skipped_total.saturating_sub(NAMED_IN_MESSAGE as u64);
        if unnamed > 0 {
            message.push_str(&format!("\n  and {unnamed} more"));
        }
    }
    let hits: Vec<serde_json::Value> = tally
        .hits
        .iter()
        .map(|h| serde_json::json!({ "rule": h.rule, "path": shown(&h.path), "line": h.line }))
        .collect();
    let skipped: Vec<serde_json::Value> = tally
        .skipped
        .iter()
        .map(
            |s| serde_json::json!({ "path": shown(&s.path), "reason": s.reason, "bytes": s.bytes }),
        )
        .collect();
    let detail = serde_json::json!({
        "gate": words.name,
        "engine": ENGINE,
        "hits": hits,
        "hits_total": tally.hits_total,
        "skipped": skipped,
        "skipped_total": tally.skipped_total,
        "failure": failure.map(|f| f.0.clone()),
    });
    CommandError::typed_with_detail(IpcErrorKind::SecretScanBlocked, message, detail)
}
