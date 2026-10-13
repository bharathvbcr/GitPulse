//! The family-wide transcript pass.
//!
//! Claude and Codex transcripts belong to the machine, not to a worktree, yet
//! progress is recorded per worktree (`transcript_cursors (worktree_key,
//! file_key, byte_offset)`) and used to be replayed per worktree: with eighteen
//! worktrees one corpus was read and parsed up to eighteen times. One pass now
//! serves every worktree of a repository:
//!
//! * each corpus file is read once, from the lowest offset any participant
//!   still owes it;
//! * each line is parsed once, then each call in it goes to exactly one
//!   participant: the deepest worktree it lies under (worktrees nest under
//!   `<repo>/.claude/worktrees`), if that participant's own cursor is at or
//!   below the line's byte offset. A call from a worktree that has since been
//!   removed is recorded under that worktree's own path, hosted by the live
//!   root around it, never folded into the repository;
//! * every participant's cursor advances in one transaction.
//!
//! Nothing about what a worktree *means* by its cursor changed. A participant
//! that has never read a file starts at 0 under its own frozen baseline floor,
//! a file that shrank is read again from 0, and a pass cut short leaves each
//! cursor exactly at the last whole line it consumed, so the table, the
//! `WRITER_VERSION` and every row already in it are untouched.

use std::collections::{HashMap, HashSet};

use super::lines::LineReader;
use super::{transcript, CatchUp};
use crate::ledger;

/// Worktrees one pass serves. Far above a real family; past it the rest wait
/// for a pass of their own, which reads exactly what they are owed.
pub(super) const MAX_PARTICIPANTS: usize = 64;

/// Offset-only advances held before one commit.
const ADVANCE_BATCH: usize = 512;

// Bytes the pass has pulled from transcript files on this thread: what the
// read-once tests count.
#[cfg(test)]
thread_local! {
    pub(super) static BYTES_READ: std::cell::Cell<u64> = const { std::cell::Cell::new(0) };
}

/// One worktree being served by the pass.
struct Participant {
    path: String,
    offsets: HashMap<String, u64>,
    baseline: String,
    baseline_ms: u64,
    task_id: Option<String>,
    pending: Vec<ledger::TranscriptAdvance>,
}

/// Where a participant stands on one file.
struct Owed {
    participant: usize,
    /// The offset this participant's cursor holds; `None` before its first read.
    from: Option<u64>,
    /// Where its reading of the file starts.
    start: u64,
    /// Whether its baseline floor applies (only to a file it never read).
    floored: bool,
}

/// Runs one pass for `trigger` and the `siblings` that share its ledger,
/// stopped when `expired` says so.
///
/// `expired` sees the pass's progress so far. Production budgets ignore it and
/// read the clock; a test stops the pass at a chosen point of progress, which a
/// wall-clock deadline cannot do deterministically.
pub(super) fn ingest_family_until(
    ledger_repo: &str,
    trigger: &str,
    siblings: &[String],
    expired: &mut dyn FnMut(&CatchUp) -> bool,
) -> CatchUp {
    ingest_family_with(
        ledger_repo,
        trigger,
        siblings,
        expired,
        &transcript::path_exists,
    )
}

/// [`ingest_family_until`] with the filesystem's answer to "does this path
/// exist" supplied, which is all that tells a removed worktree from a
/// subdirectory of a live one.
pub(super) fn ingest_family_with(
    ledger_repo: &str,
    trigger: &str,
    siblings: &[String],
    expired: &mut dyn FnMut(&CatchUp) -> bool,
    exists: &dyn Fn(&str) -> std::io::Result<bool>,
) -> CatchUp {
    let mut out = CatchUp::default();
    let Some(root) = super::transcript_root() else {
        out.error = "no home directory, so transcripts cannot be located".into();
        return out;
    };
    if !root.is_dir() {
        // No transcripts is the ordinary case for a machine that has never run
        // an agent. Not an error.
        return out;
    }

    // Every checkout of the family decides which of them owns a call, however
    // many of them this pass has room to serve: a worktree left out of the
    // pass must still keep its calls from the shallower root around it.
    let mut roots: Vec<&str> = vec![trigger];
    let mut seen: HashSet<&str> = HashSet::from([trigger]);
    for sibling in siblings {
        if seen.insert(sibling.as_str()) {
            roots.push(sibling);
        }
    }
    let paths = &roots[..roots.len().min(MAX_PARTICIPANTS)];
    let mut participants = match participants(ledger_repo, paths, &mut out, expired) {
        Some(participants) => participants,
        None => return out,
    };
    let by_path: HashMap<&str, usize> = paths
        .iter()
        .enumerate()
        .map(|(index, path)| (*path, index))
        .collect();
    let mut resolver = transcript::DepartedResolver::new(exists);
    // What each departed worktree's own cursor consumed, loaded once per pass.
    let mut departed_cursors: HashMap<String, HashMap<String, u64>> = HashMap::new();

    let mut files = Vec::new();
    let walk = super::collect_jsonl_until(&root, &mut files, 0, &mut || expired(&out));
    if walk.cut {
        out.truncated = true;
    }
    out.transcripts_listed = i64::try_from(files.len()).unwrap_or(i64::MAX);
    let mut listed = HashSet::new();
    for path in files {
        if expired(&out) {
            out.truncated = true;
            break;
        }
        let file_key = ledger::transcript_key(&path);
        listed.insert(file_key.clone());
        let Ok(meta) = std::fs::metadata(&path) else {
            out.skipped_lines += 1;
            continue;
        };
        let len = meta.len();
        let modified_ms = super::modified_ms(&meta);
        let owed = owed_on(&participants, &file_key, len, modified_ms);
        // Nothing appended since every participant's last read. This is what
        // keeps a corpus no agent touched from being re-read every call.
        let Some(start) = owed.iter().map(|o| o.start).min() else {
            continue;
        };
        let file = match std::fs::File::open(&path) {
            Ok(file) => file,
            Err(_) => {
                // A transcript being written right now can fail a read; the
                // next pass picks it up. Counted so the gap is visible.
                out.skipped_lines += 1;
                continue;
            }
        };
        let mut reader = match LineReader::seek(file, start, len) {
            Ok(reader) => reader,
            Err(_) => {
                out.skipped_lines += 1;
                continue;
            }
        };
        out.transcripts += 1;

        let mut drafts: Vec<Vec<ledger::Draft>> = owed.iter().map(|_| Vec::new()).collect();
        let mut consumed = 0u64;
        loop {
            if expired(&out) {
                out.truncated = true;
                break;
            }
            let line = match reader.next_line() {
                Ok(Some(line)) => line,
                Ok(None) => break,
                Err(_) => {
                    // What was read stays read; the rest is owed to the next pass.
                    out.skipped_lines += 1;
                    break;
                }
            };
            consumed += line.len;
            let Some(body) = line.body else {
                // Longer than the reader keeps: counted, never silently lost.
                out.skipped_lines += 1;
                continue;
            };
            let Ok(text) = std::str::from_utf8(body) else {
                out.skipped_lines += 1;
                continue;
            };
            let text = text.trim_end_matches('\r');
            if text.trim().is_empty() {
                continue;
            }
            match transcript::parse_record(text) {
                transcript::LineOutcome::NoCalls => {}
                transcript::LineOutcome::Unreadable => out.skipped_lines += 1,
                transcript::LineOutcome::Calls(calls) => {
                    for call in calls {
                        // One owner per call. A removed worktree owns its calls
                        // under its own path, hosted by the live root around it
                        // so that root's cursor keeps them exactly-once.
                        let departed = resolver.departed(&call, &roots);
                        let (host, departed_root) = match &departed {
                            Some(departed) => (departed.host, Some(departed.root.as_str())),
                            None => match transcript::attribution_root(&call, &roots) {
                                Some(root) => (root, None),
                                None => continue,
                            },
                        };
                        // A host this pass has no room for, or one that owes
                        // nothing here, is not replaced by a shallower root.
                        let Some(&index) = by_path.get(host) else {
                            continue;
                        };
                        let Some(slot) = owed.iter().position(|o| o.participant == index) else {
                            continue;
                        };
                        let owes = &owed[slot];
                        // Bytes before a participant's own cursor are ones
                        // it has already consumed.
                        if line.offset < owes.start {
                            continue;
                        }
                        let participant = &participants[index];
                        if owes.floored
                            && !participant.baseline.is_empty()
                            && call.ts_utc.as_str() <= participant.baseline.as_str()
                        {
                            continue;
                        }
                        let (worktree, task_id) = match departed_root {
                            Some(root) => {
                                if departed_consumed(
                                    ledger_repo,
                                    root,
                                    &file_key,
                                    line.offset,
                                    len,
                                    &mut departed_cursors,
                                    &mut out,
                                ) {
                                    continue;
                                }
                                (root, None)
                            }
                            None => (participant.path.as_str(), participant.task_id.clone()),
                        };
                        drafts[slot].push(super::transcript_draft(
                            &call,
                            ledger_repo,
                            worktree,
                            &task_id,
                        ));
                    }
                }
            }
        }
        #[cfg(test)]
        BYTES_READ.with(|read| read.set(read.get() + reader.bytes_read()));
        out.unverified_departed = resolver.unverified;

        let end = start + consumed;
        let mut has_events = false;
        for (owes, drafts) in owed.iter().zip(drafts) {
            // A pass cut before it reached this participant's cursor moved
            // nothing for it; recording `end` would send the cursor backwards.
            if end < owes.start {
                continue;
            }
            if owes.from != Some(end) && !(owes.from.is_none() && end == 0) {
                has_events |= !drafts.is_empty();
                participants[owes.participant]
                    .pending
                    .push(ledger::TranscriptAdvance {
                        file_key: file_key.clone(),
                        from: owes.from,
                        to: end,
                        drafts,
                    });
            }
        }
        // Events are committed as soon as their transcript is read, so a
        // deadline never discards them. Offset-only advances are batched: a
        // first read of a corpus holding nothing for these worktrees is
        // thousands of them.
        let pending: usize = participants.iter().map(|p| p.pending.len()).sum();
        if (has_events || pending >= ADVANCE_BATCH)
            && !commit(ledger_repo, &mut participants, &mut out)
        {
            return out;
        }
        if out.truncated {
            break;
        }
    }
    if !commit(ledger_repo, &mut participants, &mut out) {
        return out;
    }
    // Forget transcripts that are gone, but only on a pass that saw the whole
    // corpus: a file missing from a partial listing may still exist, and
    // forgetting its offset would replay it.
    if walk.complete && !out.truncated {
        for participant in participants {
            let gone: Vec<String> = participant
                .offsets
                .into_keys()
                .filter(|key| !listed.contains(key))
                .collect();
            if let Err(e) = ledger::forget_transcripts(ledger_repo, &participant.path, &gone) {
                out.error = format!("vanished transcripts could not be forgotten: {e}");
            }
        }
    }
    out
}

/// Whether a worktree that has since been removed already consumed the line at
/// `offset` of `file_key` in a pass of its own, so recording it again from the
/// host's cursor would double it. A cursor past the file's `len` is stale (the
/// file was rewritten shorter) and consumed nothing of it. Its cursors are read once per pass and only
/// ever read: nothing is written for a path that no longer exists.
///
/// A ledger that cannot be read is reported in `out.error` and answers "not
/// consumed": a repeated row is recoverable, a lost one is not.
fn departed_consumed(
    ledger_repo: &str,
    root: &str,
    file_key: &str,
    offset: u64,
    len: u64,
    cursors: &mut HashMap<String, HashMap<String, u64>>,
    out: &mut CatchUp,
) -> bool {
    if !cursors.contains_key(root) {
        let offsets = match ledger::transcript_progress_many(ledger_repo, &[root]) {
            Ok(mut progress) => progress.pop().map(|p| p.offsets).unwrap_or_default(),
            Err(e) => {
                out.error =
                    format!("a removed worktree's transcript progress could not be read: {e}");
                HashMap::new()
            }
        };
        cursors.insert(root.to_string(), offsets);
    }
    cursors
        .get(root)
        .and_then(|offsets| offsets.get(file_key))
        .is_some_and(|consumed| *consumed > offset && *consumed <= len)
}

/// Every participant's progress, with the baseline floor frozen for any that
/// has none yet. `None`, with the reason in `out`, when the pass cannot go on.
fn participants(
    ledger_repo: &str,
    paths: &[&str],
    out: &mut CatchUp,
    expired: &mut dyn FnMut(&CatchUp) -> bool,
) -> Option<Vec<Participant>> {
    let progress = match ledger::transcript_progress_many(ledger_repo, paths) {
        Ok(progress) => progress,
        Err(e) => {
            out.error = format!("transcript progress could not be read: {e}");
            return None;
        }
    };
    let unset: Vec<&str> = paths
        .iter()
        .zip(&progress)
        .filter(|(_, progress)| progress.baseline.is_none())
        .map(|(path, _)| *path)
        .collect();
    let mut frozen: HashMap<&str, String> = HashMap::new();
    if !unset.is_empty() {
        // One scan of the ledger for every worktree that needs its line drawn,
        // not one per worktree.
        let watermarks = match super::legacy_watermarks(ledger_repo, &unset, &mut || expired(out)) {
            Ok(Some(watermarks)) => watermarks,
            Ok(None) => {
                out.truncated = true;
                return None;
            }
            Err(e) => {
                out.error = format!("transcript baseline could not be set: {e}");
                return None;
            }
        };
        for (path, since) in unset.iter().zip(watermarks) {
            match ledger::freeze_transcript_baseline(ledger_repo, path, &since) {
                Ok(baseline) => {
                    frozen.insert(*path, baseline);
                }
                Err(e) => {
                    out.error = format!("transcript baseline could not be set: {e}");
                    return None;
                }
            }
        }
    }
    Some(
        paths
            .iter()
            .zip(progress)
            .map(|(path, progress)| {
                let baseline = progress
                    .baseline
                    .or_else(|| frozen.remove(path))
                    .unwrap_or_default();
                Participant {
                    path: (*path).to_string(),
                    offsets: progress.offsets,
                    baseline_ms: super::iso_to_millis(&baseline),
                    baseline,
                    task_id: crate::ledger::bindings::resolve(ledger_repo, path)
                        .ok()
                        .flatten(),
                    pending: Vec::new(),
                }
            })
            .collect(),
    )
}

/// What each participant still owes one file of `len` bytes last written at
/// `modified_ms`; participants with nothing owed are absent.
fn owed_on(
    participants: &[Participant],
    file_key: &str,
    len: u64,
    modified_ms: Option<u64>,
) -> Vec<Owed> {
    participants
        .iter()
        .enumerate()
        .filter_map(|(index, participant)| {
            let from = participant.offsets.get(file_key).copied();
            // `floored` applies only to a transcript this worktree has never
            // read under offsets: calls at or before the frozen baseline were
            // the old watermark's to record, and reading them again would
            // duplicate them.
            let (start, floored) = match from {
                Some(done) if done == len => return None,
                Some(done) if done < len => (done, false),
                // Shorter than what was read: rewritten rather than appended
                // to. Read it again — a duplicate row is recoverable, a lost
                // one is not.
                Some(_) => (0, false),
                // Untouched since the baseline: the old watermark already read it.
                None if participant.baseline_ms > 0
                    && !super::written_since(modified_ms, participant.baseline_ms) =>
                {
                    return None
                }
                None => (0, !participant.baseline.is_empty()),
            };
            Some(Owed {
                participant: index,
                from,
                start,
                floored,
            })
        })
        .collect()
}

/// Commits every participant's `pending` advances in one transaction, emptying
/// them. False, with the error in `out`, when the ledger refused it — the pass
/// stops there rather than read further into events it could not keep.
fn commit(ledger_repo: &str, participants: &mut [Participant], out: &mut CatchUp) -> bool {
    let advances: Vec<(&str, Vec<ledger::TranscriptAdvance>)> = participants
        .iter_mut()
        .filter(|participant| !participant.pending.is_empty())
        .map(|participant| {
            (
                participant.path.as_str(),
                std::mem::take(&mut participant.pending),
            )
        })
        .collect();
    if advances.is_empty() {
        return true;
    }
    match ledger::advance_transcripts_family(ledger_repo, advances) {
        Ok(recorded) => {
            out.recorded += recorded;
            true
        }
        Err(e) => {
            out.error = format!("transcript events could not be recorded: {e}");
            false
        }
    }
}
