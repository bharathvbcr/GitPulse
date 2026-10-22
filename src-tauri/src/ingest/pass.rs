//! The family-wide transcript pass.
//!
//! Agent transcripts (Claude Code's and Codex's, one [`TranscriptSource`] each)
//! belong to the machine, not to a worktree, yet
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
//!   removed, or that this pass has no slot for, is recorded under that
//!   worktree's own path (and the task that was bound to it when each call ran), hosted by the live root around
//!   it (whose cursor keeps it exactly-once), never folded into the
//!   repository; the owner's own cursor moves with it, so one re-created at the
//!   same path, or run on its own, does not read those bytes again;
//! * every participant's cursor advances in one transaction.
//!
//! Nothing about what a worktree *means* by its cursor changed. A participant
//! that has never read a file starts at 0 under its own frozen baseline floor,
//! a file that shrank is read again from 0, and a pass cut short leaves each
//! cursor exactly at the last whole line it consumed, so the table, the
//! `WRITER_VERSION` and every row already in it are untouched.

use std::collections::{HashMap, HashSet};

use super::lines::LineReader;
use super::source::{self, TranscriptSource};
use super::{transcript, CatchUp, Walk};
use crate::ledger;
use crate::ledger::bindings::BindingHistory;

/// Worktrees one pass serves with a cursor read of their own. Far above a real
/// family. A worktree past it is not dropped: its calls are still recorded under
/// its own path and task, on the transaction of the deepest root that has a
/// slot, and its cursor moves with them (`rehosted_calls` counts them), so a
/// pass of its own owes nothing it already has.
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
    /// The fingerprint stored with each offset, where there is one: what tells
    /// the file this cursor stopped in from one rewritten past that offset.
    fingerprints: HashMap<String, String>,
    baseline: String,
    baseline_ms: u64,
    /// Its bind and unbind rows, read once: each call is stamped with the task
    /// bound when the call happened, not the one bound when the pass ran.
    history: BindingHistory,
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
    /// The file was rewritten under this cursor: the bytes before the offset
    /// no longer match the fingerprint stored with it. The read starts over from
    /// 0 and the cursor is rewritten even if it lands on the same offset.
    rotated: bool,
}

/// The first `head` bytes of the file at `path`, and the `tail` bytes that end
/// at `offset`.
fn read_fingerprint_bytes(
    path: &std::path::Path,
    offset: u64,
    head: usize,
    tail: usize,
) -> std::io::Result<(Vec<u8>, Vec<u8>)> {
    use std::io::{Read, Seek};
    #[cfg(test)]
    FINGERPRINT_OPENS.with(|opens| opens.set(opens.get() + 1));
    let before = offset
        .checked_sub(tail as u64)
        .ok_or(std::io::ErrorKind::UnexpectedEof)?;
    let mut file = std::fs::File::open(path)?;
    let mut head_bytes = vec![0u8; head];
    file.read_exact(&mut head_bytes)?;
    let mut tail_bytes = vec![0u8; tail];
    file.seek(std::io::SeekFrom::Start(before))?;
    file.read_exact(&mut tail_bytes)?;
    Ok((head_bytes, tail_bytes))
}

/// The fingerprint to store with a cursor that now stands at `offset` of the
/// file at `path` ([`ledger::cursor_fingerprint`]), read from the file as it is
/// now. `None` when it cannot be read or `offset` is 0: the cursor is then
/// stored with none and checked by length alone, as it was before they existed.
fn fingerprint_at(
    path: &std::path::Path,
    offset: u64,
    stat: Option<ledger::FileStat>,
) -> Option<String> {
    let n = usize::try_from(offset.min(ledger::CURSOR_FINGERPRINT_BYTES as u64)).ok()?;
    let (head, tail) = read_fingerprint_bytes(path, offset, n, n).ok()?;
    ledger::cursor_fingerprint(&head, &tail, stat)
}

/// A file's length and mtime in nanoseconds, as a fingerprint records them;
/// `None` when the filesystem will not say, which leaves the cursor's
/// fingerprint without a stat and so re-hashed on every pass.
fn file_stat(meta: &std::fs::Metadata) -> Option<ledger::FileStat> {
    let mtime = meta
        .modified()
        .ok()?
        .duration_since(std::time::UNIX_EPOCH)
        .ok()?;
    Some((meta.len(), u64::try_from(mtime.as_nanos()).ok()?))
}

// Files the fingerprint check has opened on this thread: what the "an
// unchanged transcript costs a stat" tests count.
#[cfg(test)]
thread_local! {
    pub(super) static FINGERPRINT_OPENS: std::cell::Cell<u64> = const { std::cell::Cell::new(0) };
}

/// Whether the bytes of the file at `path` that `fingerprint` covers are still
/// the ones it was taken from.
///
/// False only for a positive mismatch (different bytes, or a file that ends
/// before the offset). A fingerprint this build cannot read, or a file that
/// cannot be read right now, is not evidence of a rewrite: reading again
/// from the start would record every line twice, and the open that follows
/// reports a file that really cannot be read.
///
/// `now` is the file's stat as the pass already read it. A fingerprint that
/// recorded a stat and finds the same one is a match with no open: the file has
/// not been written since the cursor was. Any difference (a rewrite that kept
/// the size but moved the mtime included) re-hashes the bytes. One without a
/// stat always re-hashes.
///
/// A mismatch is not yet a rotation: the cursor's tail window (the bytes just
/// before its offset, which the fingerprint holds a digest of) is searched for in
/// the new file. A file that kept the old lines (a record prepended, an early
/// line edited, anything appended) holds it, and the cursor resumes after it;
/// only a file that does not is read again from the start.
fn standing_of(
    path: &std::path::Path,
    now: Option<ledger::FileStat>,
    offset: u64,
    fingerprint: &str,
) -> Standing {
    let Some(parts) = ledger::parse_cursor_fingerprint(fingerprint) else {
        return Standing::Holds;
    };
    if parts.stat.is_some() && parts.stat == now {
        return Standing::Holds;
    }
    match read_fingerprint_bytes(path, offset, parts.head, parts.tail) {
        Ok((head, tail)) if ledger::cursor_fingerprint_holds(fingerprint, &parts, &head, &tail) => {
            Standing::Holds
        }
        Ok(_) => rebase(path, offset, &parts).map_or(Standing::Rotated, Standing::Rebased),
        Err(e) if e.kind() == std::io::ErrorKind::UnexpectedEof => {
            rebase(path, offset, &parts).map_or(Standing::Rotated, Standing::Rebased)
        }
        Err(_) => Standing::Holds,
    }
}

/// How far into a file the search for a moved cursor looks, in bytes. Past it
/// the cursor is called rotated and read from the start: a duplicate row is
/// recoverable, an unbounded read of a transcript that may be hundreds of
/// megabytes is not.
const REBASE_SEARCH_BYTES: u64 = 1 << 20;

/// Where a cursor that stood at `offset` stands in the file at `path` now: the
/// end of the line-ending window of `parts.tail` bytes whose digest is the
/// fingerprint's, nearest `offset` within the first [`REBASE_SEARCH_BYTES`].
/// `None` for a fingerprint with no tail digest or a window not found.
fn rebase(path: &std::path::Path, offset: u64, parts: &ledger::FingerprintParts) -> Option<u64> {
    use std::io::Read;
    let wanted = parts.tail_hash?;
    let width = parts.tail;
    let mut buffer = Vec::new();
    std::fs::File::open(path)
        .ok()?
        .take(REBASE_SEARCH_BYTES + width as u64)
        .read_to_end(&mut buffer)
        .ok()?;
    // A cursor stands at a line's end, so the window ends in a newline: only
    // those positions are hashed.
    (width..=buffer.len())
        .filter(|end| {
            buffer[end - 1] == b'\n'
                && ledger::cursor_tail_hash(&buffer[end - width..*end]) == wanted
        })
        .map(|end| end as u64)
        .min_by_key(|end| end.abs_diff(offset))
}

/// Where a cursor stands against the file as it is now.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Standing {
    /// The bytes before it are the ones it was taken from (or it cannot be
    /// checked): it is read from its offset.
    Holds,
    /// The file changed before the cursor but still holds the lines the cursor
    /// had consumed, now ending at this offset: it resumes there.
    Rebased(u64),
    /// The file is not the one the cursor stopped in: it is read from the start.
    Rotated,
}

/// [`standing_of`] for one file, asked once per distinct cursor: every
/// participant of a family usually holds the same offset.
struct Probe<'a> {
    path: &'a std::path::Path,
    /// The file's stat as the pass read it before anything else.
    now: Option<ledger::FileStat>,
    answers: HashMap<(u64, String), Standing>,
}

impl Probe<'_> {
    fn standing_at(&mut self, offset: u64, fingerprint: &str) -> Standing {
        // The common answer, with no allocation: a fingerprint that recorded a
        // stat and finds it unchanged.
        if let Some(parts) = ledger::parse_cursor_fingerprint(fingerprint) {
            if parts.stat.is_some() && parts.stat == self.now {
                return Standing::Holds;
            }
        }
        let (path, now) = (self.path, self.now);
        *self
            .answers
            .entry((offset, fingerprint.to_string()))
            .or_insert_with(|| standing_of(path, now, offset, fingerprint))
    }

    /// Where `participant`'s cursor on `file_key` stands against this file: no
    /// fingerprint (a cursor from before they existed) is taken as holding.
    fn standing(&mut self, participant: &Participant, file_key: &str) -> Standing {
        match (
            participant.offsets.get(file_key),
            participant.fingerprints.get(file_key),
        ) {
            (Some(offset), Some(fingerprint)) => self.standing_at(*offset, fingerprint),
            _ => Standing::Holds,
        }
    }
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
    ingest_family(ledger_repo, trigger, siblings, true, expired)
}

/// [`ingest_family_until`] for a directory that is not a Git checkout, whose
/// family is the trigger alone. No removed worktree is recognised: there is no
/// worktree layout to read one from. A Git checkout whose family cannot be
/// resolved never comes here; its pass is skipped (see `catch_up_with`), since
/// calls recorded for the trigger alone would be recorded again once the
/// family can be seen. The result says so (`family_unresolved`).
pub(super) fn ingest_unresolved_family_until(
    ledger_repo: &str,
    trigger: &str,
    expired: &mut dyn FnMut(&CatchUp) -> bool,
) -> CatchUp {
    let mut out = ingest_family(ledger_repo, trigger, &[], false, expired);
    out.family_unresolved = true;
    out
}

/// `resolved` is whether `siblings` is the whole family; only then can a path
/// under `.claude/worktrees` that no member owns be read as a removed worktree.
fn ingest_family(
    ledger_repo: &str,
    trigger: &str,
    siblings: &[String],
    resolved: bool,
    expired: &mut dyn FnMut(&CatchUp) -> bool,
) -> CatchUp {
    let mut out = CatchUp::default();
    // Before anything reads the ledger: a repository that was moved carries a
    // ledger whose rows name its old path, which every read below refuses until
    // the ledger is taken over (or, for another repository's ledger, never).
    match ledger::settle_identity(ledger_repo) {
        Ok(reanchored) => out.reanchored_from = reanchored.unwrap_or_default(),
        Err(e) => {
            out.error = format!("the ledger's repository could not be confirmed: {e}");
            return out;
        }
    }
    let mut active: Vec<(&'static dyn TranscriptSource, std::path::PathBuf)> = Vec::new();
    // What could not be read of a source's root, for `error`.
    let mut gaps: Vec<String> = Vec::new();
    // A root that is absent where it should not be: the listing is then not the
    // whole corpus, whatever the roots that are there say.
    let mut missing_root = false;
    // Whether a saved offset may be forgotten at all this pass. An optional
    // source's root problems only report: whether they hold anything back is
    // `track_roots`'s call, from what the source has held before.
    let mut blocks_forget = false;
    // An optional source's default root that is not there (ENOENT, nothing more)
    // is ordinary: "never installed" and "gone since the last pass" are told apart
    // by the source's marker, below.
    let mut any_root = false;
    for source in source::sources() {
        let Some(root) = source.root() else { continue };
        any_root = true;
        let name = source.name();
        let shown = root.path.display();
        let note = source.reports_unwalkable_root();
        // A root that is not an absolute path would be read against whatever
        // directory the app happens to be in.
        if note && !root.path.is_absolute() {
            missing_root = true;
            blocks_forget |= !source.optional();
            gaps.push(format!(
                "the {name} transcript root {shown} is not an absolute path, so its transcripts were not read"
            ));
            continue;
        }
        match std::fs::metadata(&root.path) {
            Ok(meta) if meta.is_dir() => active.push((source, root.path)),
            Ok(_) => {
                missing_root = true;
                blocks_forget |= !source.optional();
                if note {
                    gaps.push(format!(
                        "the {name} transcript root {shown} is not a directory, so its transcripts were not read"
                    ));
                }
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                if root.explicit || !source.optional() {
                    missing_root = true;
                    blocks_forget |= !source.optional();
                    if root.explicit && note {
                        gaps.push(format!(
                            "the {name} transcript root {shown} does not exist, so its transcripts were not read"
                        ));
                    }
                }
            }
            // Present, or not, but not to be told: a permission, a component
            // that is a file. Never "absent": an unreachable root is not a root
            // with nothing in it.
            Err(e) => {
                missing_root = true;
                blocks_forget |= !source.optional();
                if note {
                    gaps.push(format!(
                        "the {name} transcript root {shown} could not be read ({}), so its transcripts were not read",
                        e.kind()
                    ));
                }
            }
        }
    }
    if !any_root {
        out.error = super::NO_TRANSCRIPT_ROOT.into();
        return out;
    }
    if active.is_empty() {
        // No transcripts is the ordinary case for a machine that has never run
        // an agent. Not an error, unless a root was named and is not there.
        if !gaps.is_empty() {
            out.walk_complete = Some(false);
            out.error = gaps.join("; ");
        }
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
    let mut owners = Owners::default();

    // Every optional source has one marker cursor per worktree, in the cursor
    // table beside the file cursors, never listed by a walk and never forgotten.
    // Its fingerprint text holds the roots (newline-separated, the latest eight)
    // that held transcripts of that source. See `track_roots`.
    let markers: HashSet<String> = source::sources()
        .iter()
        .filter(|source| source.optional())
        .map(|source| seen_key(*source))
        .collect();

    // Each source is walked on its own, under its own file cap and entry
    // budget: a source with a huge corpus cannot starve another of its listing.
    let mut files: Vec<(&'static dyn TranscriptSource, std::path::PathBuf)> = Vec::new();
    let mut read_roots: Vec<(&'static dyn TranscriptSource, &std::path::Path, usize)> = Vec::new();
    let mut walk = Walk::none();
    for (source, root) in &active {
        let mut listed_here = Vec::new();
        let walked = super::collect_jsonl_until(root, &mut listed_here, 0, &mut || expired(&out));
        let counted_unlisted =
            walked.unlisted_depth > 0 || walked.unlisted_cap > 0 || walked.unlisted_lower_bound;
        if source.reports_unwalkable_root() && !walked.complete && !walked.cut && !counted_unlisted
        {
            gaps.push(format!(
                "the {} transcript root {} could not be read in full",
                source.name(),
                root.display()
            ));
        }
        walk.absorb(&walked);
        read_roots.push((*source, root.as_path(), listed_here.len()));
        files.extend(listed_here.into_iter().map(|path| (*source, path)));
    }
    track_roots(
        &mut participants,
        &active,
        &read_roots,
        &mut gaps,
        &mut missing_root,
        &mut blocks_forget,
    );
    let walks_complete = walk.complete;
    walk.complete &= !missing_root;
    if active.len() > 1 {
        files = single_owner(files, &active);
    }
    if walk.cut {
        out.truncated = true;
    }
    out.transcripts_listed = i64::try_from(files.len()).unwrap_or(i64::MAX);
    out.unlisted_depth = walk.unlisted_depth;
    out.unlisted_cap = walk.unlisted_cap;
    out.transcripts_unlisted = walk.unlisted_depth.saturating_add(walk.unlisted_cap);
    out.unlisted_lower_bound = walk.unlisted_lower_bound;
    out.refused_outside_corpus = walk.refused_outside_corpus;
    out.walk_complete = Some(walk.complete);
    let mut listed = markers;
    let mut resume_budget = source::RESUME_PASS_BYTES;
    for (source, path) in files {
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
        let stat = file_stat(&meta);
        let mut probe = Probe {
            path: &path,
            now: stat,
            answers: HashMap::new(),
        };
        let owed = owed_on(
            &participants,
            &file_key,
            len,
            modified_ms,
            source.legacy_floor(),
            &mut probe,
        );
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
        let Some(mut parser) = source.open(&path, start, &mut resume_budget) else {
            // Its start cannot be replayed within this pass's budget: nothing is
            // read and no cursor moves, so the next pass tries again. Counted.
            out.skipped_lines += 1;
            continue;
        };

        let mut drafts: Vec<Vec<ledger::Draft>> = owed.iter().map(|_| Vec::new()).collect();
        let mut touched: HashSet<usize> = HashSet::new();
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
            let parsed = parser.parse(text);
            out.skipped_lines += parsed.skipped;
            for call in parsed.calls {
                // One owner per call, and the row is always the owner's:
                // its path, its task. A removed worktree is found by
                // its path alone, and only when the family is known (an
                // unresolved one may hold live worktrees this pass
                // cannot see). A worktree this pass has no room for
                // owns its rows just the same. Either is "hosted": the
                // bytes ride the transaction of the deepest root that
                // has a slot, whose cursor keeps them exactly-once.
                let departed = if resolved {
                    transcript::departed_root(&call, &roots)
                } else {
                    None
                };
                let (hosted_owner_root, live_host) = match &departed {
                    Some(departed) => (Some((departed.root, false)), departed.host),
                    None => match transcript::attribution_root(&call, &roots) {
                        Some(root) if by_path.contains_key(root) => (None, root),
                        Some(root) => (Some((root, true)), root),
                        None => continue,
                    },
                };
                let (index, rehosted) = match by_path.get(live_host) {
                    Some(&index) => (index, false),
                    None => {
                        let owner = hosted_owner_root.map_or(live_host, |(root, _)| root);
                        match transcript::deepest_root(owner, paths)
                            .and_then(|root| by_path.get(root))
                        {
                            Some(&index) => (index, true),
                            None => continue,
                        }
                    }
                };
                // A host that owes nothing here has already consumed it.
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
                // Compared as instants: a transcript's own spelling may
                // carry an offset, which does not order as text. A time
                // that cannot be read is not skipped; a duplicate row
                // is recoverable, a lost one is not.
                if owes.floored
                    && participant.baseline_ms > 0
                    && ledger::ids::parse_event_time(&call.ts_utc)
                        .is_some_and(|at| at <= participant.baseline_ms)
                {
                    continue;
                }
                let (worktree, history) = match hosted_owner_root {
                    Some((root, live)) => {
                        match hosted_owner(ledger_repo, root, live, &mut owners, &mut out) {
                            Some(owner) => {
                                // Consumed in a pass of its own, or
                                // hosted by an earlier one.
                                if consumed_by(
                                    &owners.hosted[owner],
                                    &file_key,
                                    line.offset,
                                    len,
                                    &mut probe,
                                ) {
                                    continue;
                                }
                                touched.insert(owner);
                                (root, &owners.hosted[owner].history)
                            }
                            // Past the bound on untrusted names: the
                            // row is still the owner's and the host's
                            // cursor still keeps it exactly-once, but
                            // the owner's own cursor is not touched,
                            // and its task is not looked up.
                            None => {
                                out.departed_overflow += 1;
                                (root, &NO_HISTORY)
                            }
                        }
                    }
                    None => (participant.path.as_str(), &participant.history),
                };
                drafts[slot].push(super::transcript_draft(
                    &call,
                    source.actor_id(),
                    ledger_repo,
                    worktree,
                    history,
                ));
                out.rehosted_calls += i64::from(rehosted);
            }
        }
        #[cfg(test)]
        BYTES_READ.with(|read| read.set(read.get() + reader.bytes_read()));

        let end = start + consumed;
        // Taken from the file as it stands once the lines are read, and only
        // when a cursor is to be written at all.
        // The stat recorded with it is the one taken before the read: a file
        // written during or after the read then differs on the next pass and is
        // re-hashed, never taken for unchanged.
        let fingerprint = fingerprint_at(&path, end, stat);
        let mut has_events = false;
        for (owes, drafts) in owed.iter().zip(drafts) {
            // A pass cut before it reached this participant's cursor moved
            // nothing for it; recording `end` would send the cursor backwards.
            if end < owes.start {
                continue;
            }
            // A rotated cursor is rewritten even where the new file ends at the
            // old offset, or it would read as rotated again on every pass.
            if (owes.from != Some(end) || owes.rotated) && !(owes.from.is_none() && end == 0) {
                has_events |= !drafts.is_empty();
                participants[owes.participant]
                    .pending
                    .push(ledger::TranscriptAdvance {
                        file_key: file_key.clone(),
                        from: owes.from,
                        to: end,
                        fingerprint: fingerprint.clone(),
                        drafts,
                    });
            }
        }
        // A removed worktree moves with the root that hosted its rows, so one
        // re-created at the same path resumes after them instead of reading
        // them again as its own. A cursor past the file is stale: it was
        // rewritten shorter, and is read again from the start.
        for owner in touched {
            let owner = &mut owners.hosted[owner];
            let current = owner.offsets.get(&file_key).copied();
            // Or rewritten under it: its fingerprint no longer matches.
            let rotated = probe.standing(owner, &file_key) != Standing::Holds;
            if rotated || current.is_none_or(|current| current < end || current > len) {
                owner.pending.push(ledger::TranscriptAdvance {
                    file_key: file_key.clone(),
                    from: current,
                    to: end,
                    fingerprint: fingerprint.clone(),
                    drafts: Vec::new(),
                });
                owner.offsets.insert(file_key.clone(), end);
                match &fingerprint {
                    Some(fingerprint) => {
                        owner
                            .fingerprints
                            .insert(file_key.clone(), fingerprint.clone());
                    }
                    None => {
                        owner.fingerprints.remove(&file_key);
                    }
                }
            }
        }
        // Events are committed as soon as their transcript is read, so a
        // deadline never discards them. Offset-only advances are batched: a
        // first read of a corpus holding nothing for these worktrees is
        // thousands of them.
        let pending: usize = participants
            .iter()
            .chain(&owners.hosted)
            .map(|p| p.pending.len())
            .sum();
        if (has_events || pending >= ADVANCE_BATCH)
            && !commit(ledger_repo, &mut participants, &mut owners.hosted, &mut out)
        {
            return out;
        }
        if out.truncated {
            break;
        }
    }
    if !commit(ledger_repo, &mut participants, &mut owners.hosted, &mut out) {
        return out;
    }
    // Forget transcripts that are gone, but only on a pass that saw the whole
    // corpus: a file missing from a partial listing may still exist, and
    // forgetting its offset would replay it. Hosted owners hold cursors too,
    // for the files this pass met them in.
    let forgets = walks_complete && !blocks_forget && !out.truncated;
    if forgets {
        for participant in participants.into_iter().chain(owners.hosted) {
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
    // Files the walk saw and did not list were not read: the pass is a floor,
    // however it ended, and says how much it left and why. After the forget
    // decision above, which a walk that left any behind never reaches.
    if out.transcripts_unlisted > 0 || out.unlisted_lower_bound {
        out.truncated = true;
        let at_least = if out.unlisted_lower_bound {
            "at least "
        } else {
            ""
        };
        let note = format!(
            "catch-up truncated: the transcript walk left {at_least}{} file(s) unlisted and unread \
             ({} below the depth bound, {} past the file cap), {} listed",
            out.transcripts_unlisted, out.unlisted_depth, out.unlisted_cap, out.transcripts_listed
        );
        out.error = if out.error.is_empty() {
            note
        } else {
            format!("{}; {note}", out.error)
        };
    }
    // A root that was named and could not be read is a gap in the history, and
    // is said whether or not anything else went wrong.
    for gap in gaps {
        out.error = if out.error.is_empty() {
            gap
        } else {
            format!("{}; {gap}", out.error)
        };
    }
    out
}

/// How many roots a marker remembers.
const MAX_MARKER_ROOTS: usize = 8;

/// Keeps each optional source's marker, and holds forgetting back when a root
/// that held transcripts is not among the roots read now.
///
/// Cursors are keyed by a hash of the file's path, so a cursor cannot be told
/// from another source's. A root that held transcripts and is not read this pass
/// (an unmounted volume, a `CODEX_HOME` pointed elsewhere and back) may hold
/// files the pass cannot list; forgetting their cursors would replay them as
/// duplicate rows when the root returns. So a root recorded in a marker and
/// absent now stops every forget this pass, with the root and the reason in
/// `gaps`. A root that held nothing is never recorded, and never blocks.
fn track_roots(
    participants: &mut [Participant],
    active: &[(&'static dyn TranscriptSource, std::path::PathBuf)],
    read_roots: &[(&'static dyn TranscriptSource, &std::path::Path, usize)],
    gaps: &mut Vec<String>,
    missing_root: &mut bool,
    blocks_forget: &mut bool,
) {
    for source in source::sources().iter().filter(|s| s.optional()) {
        let key = seen_key(*source);
        let mut recorded: Vec<String> = Vec::new();
        for participant in participants.iter() {
            for root in participant
                .fingerprints
                .get(&key)
                .map(String::as_str)
                .unwrap_or_default()
                .lines()
            {
                if !recorded.iter().any(|known| known == root) {
                    recorded.push(root.to_string());
                }
            }
        }
        for root in &recorded {
            let read_now = active.iter().any(|(s, path)| {
                s.actor_id() == source.actor_id() && path.to_string_lossy() == root.as_str()
            });
            if read_now {
                continue;
            }
            *missing_root = true;
            *blocks_forget = true;
            if !gaps.iter().any(|gap| gap.contains(root.as_str())) {
                gaps.push(format!(
                    "the {} transcript root {root} held transcripts earlier and is not readable now, so no saved offset was forgotten",
                    source.name()
                ));
            }
        }
        let mut roots = recorded.clone();
        for (read, path, files) in read_roots {
            let shown = path.to_string_lossy();
            if read.actor_id() == source.actor_id()
                && *files > 0
                && !roots.iter().any(|known| known == shown.as_ref())
            {
                roots.push(shown.into_owned());
            }
        }
        if roots.len() == recorded.len() {
            continue;
        }
        let keep = roots.len().saturating_sub(MAX_MARKER_ROOTS);
        let text = roots[keep..].join("\n");
        for participant in participants.iter_mut() {
            let from = participant.offsets.get(&key).copied();
            participant.offsets.insert(key.clone(), 1);
            participant.fingerprints.insert(key.clone(), text.clone());
            participant.pending.push(ledger::TranscriptAdvance {
                file_key: key.clone(),
                from,
                to: 1,
                fingerprint: Some(text.clone()),
                drafts: Vec::new(),
            });
        }
    }
}

type Listed = Vec<(&'static dyn TranscriptSource, std::path::PathBuf)>;

/// Gives every listed file exactly one owning source.
///
/// Two sources list the same file when their roots overlap (one directory named
/// as both, or one root inside the other). Reading it with both would record
/// its lines twice or, worse, let the wrong parser's cursor advance win. The
/// owner is the source that recognises the file by its first line, else the one
/// whose root is deeper (the more specific claim), else the first listed.
/// Files are matched by identity, so two spellings of one path are one file.
/// Without overlapping roots nothing is looked at.
fn single_owner(
    files: Listed,
    active: &[(&'static dyn TranscriptSource, std::path::PathBuf)],
) -> Listed {
    let canonical: Vec<Option<std::path::PathBuf>> = active
        .iter()
        .map(|(_, root)| std::fs::canonicalize(root).ok())
        .collect();
    let overlap = canonical.iter().enumerate().any(|(i, a)| {
        canonical
            .iter()
            .enumerate()
            .any(|(j, b)| i != j && matches!((a, b), (Some(a), Some(b)) if a.starts_with(b)))
    });
    if !overlap {
        return files;
    }
    let depth = |source: &dyn TranscriptSource| {
        active
            .iter()
            .zip(&canonical)
            .find(|((s, _), _)| s.actor_id() == source.actor_id())
            .and_then(|(_, root)| root.as_ref())
            .map_or(0, |root| root.components().count())
    };
    let mut at: HashMap<super::DirId, usize> = HashMap::new();
    let mut kept: Listed = Vec::new();
    for (source, path) in files {
        let id = std::fs::metadata(&path)
            .ok()
            .and_then(|meta| super::dir_id(&meta, &path));
        let Some(id) = id else {
            kept.push((source, path));
            continue;
        };
        let Some(&held_at) = at.get(&id) else {
            at.insert(id, kept.len());
            kept.push((source, path));
            continue;
        };
        let held = kept[held_at].0;
        if held.actor_id() == source.actor_id() {
            continue;
        }
        let (claims, held_claims) = (source.recognises(&path), held.recognises(&path));
        let preferred = if claims == held_claims {
            depth(source) > depth(held)
        } else {
            claims
        };
        if preferred {
            kept[held_at] = (source, path);
        }
    }
    kept
}

/// The cursor key that records that `source` has been read for a worktree. It
/// names no file, so it is never listed by a walk, and the pass keeps it.
fn seen_key(source: &dyn TranscriptSource) -> String {
    ledger::transcript_key(std::path::Path::new(&format!(
        "gitpulse-transcript-source-read:{}",
        source.actor_id()
    )))
}

/// The answer for a removed worktree's rows the pass did not look up: no task,
/// `unresolved`, and never the host's.
static NO_HISTORY: BindingHistory = BindingHistory::unreadable();

/// New removed worktrees one pass enters into [`Owners`]. Their names come from
/// transcript content, so the cursors and ledger reads they cost are bounded;
/// past it a row is still recorded under its owner's path and only the owner's
/// own cursor is skipped, counted in `CatchUp::departed_overflow`.
const MAX_DEPARTED_OWNERS: usize = 256;

/// The worktrees whose rows this pass records on another's transaction: removed
/// ones, and live ones with no slot. Each is entered once, with its own cursors
/// as they stood when first met. Kept apart from `participants`: an owner reads
/// no file, only has bytes hosted for it.
#[derive(Default)]
struct Owners {
    hosted: Vec<Participant>,
    index: HashMap<String, usize>,
    departed: usize,
}

/// The owner at `root`, entered into `owners` with its cursors read once per
/// pass, and its binding history, read the way a participant's
/// is. `None` for a removed worktree past [`MAX_DEPARTED_OWNERS`]. A ledger that
/// cannot be read is reported in `out.error` and answers "nothing consumed": a
/// repeated row is recoverable, a lost one is not.
fn hosted_owner(
    ledger_repo: &str,
    root: &str,
    live: bool,
    owners: &mut Owners,
    out: &mut CatchUp,
) -> Option<usize> {
    if let Some(&known) = owners.index.get(root) {
        return Some(known);
    }
    if !live && owners.departed >= MAX_DEPARTED_OWNERS {
        return None;
    }
    let (offsets, fingerprints) = match ledger::transcript_progress_many(ledger_repo, &[root]) {
        Ok(mut progress) => progress
            .pop()
            .map(|p| (p.offsets, p.fingerprints))
            .unwrap_or_default(),
        Err(e) => {
            out.error = format!("an owner's transcript progress could not be read: {e}");
            (HashMap::new(), HashMap::new())
        }
    };
    owners.hosted.push(Participant {
        path: root.to_string(),
        offsets,
        fingerprints,
        baseline: String::new(),
        baseline_ms: 0,
        history: load_history(ledger_repo, root, out),
        pending: Vec::new(),
    });
    owners.departed += usize::from(!live);
    owners
        .index
        .insert(root.to_string(), owners.hosted.len() - 1);
    Some(owners.hosted.len() - 1)
}

/// The bind and unbind history of `worktree`, for stamping the calls it owns.
/// Live and removed worktrees alike resolve from their own path's rows: a bind
/// row exists only for a path that was authenticated when it was bound. One
/// that cannot be read is reported in `out.error` and decides nothing, so its
/// calls are `unresolved` rather than given a task they may not have had.
fn load_history(ledger_repo: &str, worktree: &str, out: &mut CatchUp) -> BindingHistory {
    match BindingHistory::load(ledger_repo, worktree) {
        Ok(history) => history,
        Err(e) => {
            out.error = format!("a worktree's task binding history could not be read: {e}");
            BindingHistory::unreadable()
        }
    }
}

/// Whether `owner` already consumed the line at `offset` of `file_key`, in a
/// pass of its own or hosted by an earlier one, so recording it again would
/// double it. A cursor past the file's `len` is stale (the file was rewritten
/// shorter) and consumed nothing of it.
/// A cursor whose fingerprint no longer matches (the file was rewritten past
/// it, however long the new one is) consumed nothing of this file either.
fn consumed_by(
    owner: &Participant,
    file_key: &str,
    offset: u64,
    len: u64,
    probe: &mut Probe<'_>,
) -> bool {
    let Some(consumed) = owner.offsets.get(file_key) else {
        return false;
    };
    match probe.standing(owner, file_key) {
        Standing::Holds => *consumed > offset && *consumed <= len,
        // The file moved the lines the cursor had consumed: those before where
        // they now end are consumed, and nothing after.
        Standing::Rebased(resume) => offset < resume,
        Standing::Rotated => false,
    }
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
                    fingerprints: progress.fingerprints,
                    baseline_ms: super::iso_to_millis(&baseline),
                    baseline,
                    history: load_history(ledger_repo, path, out),
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
    legacy_floor: bool,
    probe: &mut Probe<'_>,
) -> Vec<Owed> {
    participants
        .iter()
        .enumerate()
        .filter_map(|(index, participant)| {
            let from = participant.offsets.get(file_key).copied();
            // A file as long as the cursor, or longer, is not thereby the file
            // the cursor stopped in: the bytes before it must still be the ones
            // its fingerprint was taken from. Only then is "at the end" or "past
            // the cursor" a statement about this file.
            let standing = if from.is_some_and(|done| done <= len) {
                probe.standing(participant, file_key)
            } else {
                Standing::Holds
            };
            let rotated = standing != Standing::Holds;
            // `floored` applies only to a transcript this worktree has never
            // read under offsets: calls at or before the frozen baseline were
            // the old watermark's to record, and reading them again would
            // duplicate them.
            let (start, floored) = match from {
                Some(_) if standing == Standing::Rotated => (0, false),
                Some(_) if rotated => match standing {
                    Standing::Rebased(resume) => (resume, false),
                    _ => (0, false),
                },
                Some(done) if done == len => return None,
                Some(done) if done < len => (done, false),
                // Shorter than what was read: rewritten rather than appended
                // to. Read it again — a duplicate row is recoverable, a lost
                // one is not.
                Some(_) => (0, false),
                // Untouched since the baseline: the old watermark already read it.
                // Only for a source the old watermark described: a source it
                // never read has nothing before the line to be skipped.
                None if legacy_floor
                    && participant.baseline_ms > 0
                    && !super::written_since(modified_ms, participant.baseline_ms) =>
                {
                    return None
                }
                None => (0, legacy_floor && !participant.baseline.is_empty()),
            };
            Some(Owed {
                participant: index,
                from,
                start,
                floored,
                rotated,
            })
        })
        .collect()
}

/// Commits every participant's `pending` advances in one transaction, emptying
/// them. False, with the error in `out`, when the ledger refused it — the pass
/// stops there rather than read further into events it could not keep.
fn commit(
    ledger_repo: &str,
    participants: &mut [Participant],
    hosted: &mut [Participant],
    out: &mut CatchUp,
) -> bool {
    let advances: Vec<(&str, Vec<ledger::TranscriptAdvance>)> = participants
        .iter_mut()
        .chain(hosted.iter_mut())
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
