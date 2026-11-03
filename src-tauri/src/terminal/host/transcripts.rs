//! Each session's output, on disk, so it can be read after the session is gone.
//!
//! The ring (`terminal::ring`) keeps the newest 512 KiB in memory for a view
//! that takes a session over, and the receipts (`receipts.rs`) keep how a
//! session ended. Neither keeps what it printed: a host that dies, or a reboot,
//! lost the output of a run that finished while no one watched. This keeps it,
//! as `<host dir>/transcripts/<session id>.log`, `0600` in a `0700` directory,
//! opened `O_NOFOLLOW` and named only by a well-formed session id, exactly the
//! rules the receipts follow.
//!
//! **The reader never waits on the disk.** The ring copies every chunk to a
//! [`Tap`] inside `Ring::push`, which runs with the ring's lock held, so the tap
//! only appends to an in-memory queue bounded in bytes. A single supervised
//! writer thread drains it. When the queue is full the chunk is dropped, its
//! size is remembered, and a `[gitpulse: N bytes of output were not recorded]`
//! line is written ahead of the next chunk that fits (or when the session ends),
//! so a gap is in the text, never silent.
//!
//! **Bounds.** A session's log is two segments, `<id>.log` (current) and
//! `<id>.log.1` (previous), each at most [`Limits::segment_bytes`]; when the
//! current one is full it becomes the previous one and the old previous one is
//! deleted. Every segment after the session's first begins with
//! [`CONTINUED`], which is how a read knows the start of the output is gone.
//! The directory as a whole is held to [`Limits::total_bytes`] by deleting whole
//! sessions, least recently written first, never one still running; if running
//! sessions alone exceed it their previous segments go next. Transcripts older
//! than [`Limits::max_age`] are deleted when the host starts.
//!
//! **Lifecycle.** A transcript is created when its session is armed (just
//! before the spawn), closed and synced when the session's ring is dropped,
//! and removed when the session never started (`discard`, beside the receipt's).
//! It is *not* removed when the app `forget`s the receipt: forgetting means the
//! app has recorded the exit, which is long before anyone reopens the run's
//! output. Eviction and age are what end a finished session's transcript.
//!
//! **A transcript is a run's, not only an id's.** Terminal ids are
//! `term-<pid>-<count>` and repeat across reboots. A session spawned for a run
//! writes the run id to `<id>.log.meta`; a new session under an id deletes the old
//! files only if they were recorded for the same run or for none, and otherwise
//! keeps them as `<id>.<tag of the run>.*` (one per run, counted toward the
//! directory limits under that name). A read that names a run is answered from
//! that run's files or as [`TranscriptRead::Reused`], never from another
//! session's.
//!
//! **How it ended is on disk too.** What a host knows only in memory is gone
//! when it is, and a log that simply stops reads like a log that is complete.
//! So closing a session appends [`ENDED`] to its newest segment, and a write
//! that fails leaves `<id>.log.err` holding the reason. A read strips the
//! marker like [`CONTINUED`]; one that finds a finished session with neither
//! the marker nor a note says its end may be missing (the host that wrote it
//! died, or was killed, mid-session) instead of answering as if it were whole.

use crate::terminal::ring::Tap;
use crate::terminal::{TranscriptRead, TranscriptTail, MAX_TRANSCRIPT_TAIL_BYTES};
use base64::engine::general_purpose::STANDARD as BASE64;
use base64::Engine;
use std::collections::{HashMap, HashSet, VecDeque};
use std::fs::{File, OpenOptions};
use std::io::Write;
use std::os::unix::fs::{FileExt, MetadataExt, OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Condvar, Mutex, MutexGuard, PoisonError};
use std::thread::JoinHandle;
use std::time::{Duration, SystemTime};

/// The first line of every segment after a session's first.
const CONTINUED: &[u8] = b"[gitpulse: transcript continues]\r\n";
/// The last bytes of a session's newest segment once it was closed normally.
const ENDED: &[u8] = b"\r\n[gitpulse: transcript ends]\r\n";
/// What a read says of a finished session whose segment lacks [`ENDED`] and
/// whose recording left no note: the host that wrote it did not get to close it.
const UNCLOSED: &str =
    "the host that wrote this did not close it, so the end of the output may be missing";
/// Most bytes of a failure note, written or read.
const NOTE_MAX: usize = 2048;
/// Most bytes of a run id the host keeps beside a transcript, and compares. An
/// id is clipped the same way wherever it is read, so a longer one still names
/// one run.
const RUN_ID_MAX: usize = 256;
/// How much older output a masking read looks at beyond the bytes it returns,
/// so a name or a key's first line just before the window still marks what
/// follows as secret.
const REDACT_LOOKBACK: usize = 64 * 1024;
/// What a gap marker's text ends with; the count of these in a tail is
/// [`TranscriptTail::gap_markers`].
const GAP_TAIL: &[u8] = b" bytes of output were not recorded]";
/// What each queued operation is counted as in addition to its bytes, so a
/// flood of tiny chunks cannot grow the queue past its bound.
const OP_COST: usize = 64;
/// How long a read waits for the writer to catch up with what it was given.
const SETTLE_DEADLINE: Duration = Duration::from_secs(2);
/// How long an idle writer holding unsynced output waits before syncing it.
const SYNC_EVERY: Duration = Duration::from_secs(5);
/// Most entries one directory scan looks at.
const MAX_SCANNED: usize = 16 * 1024;
/// How often a read starts over when a rotation lands between its two opens.
const READ_ATTEMPTS: usize = 3;
const MIN_SEGMENT_BYTES: u64 = 256;

/// How much a host keeps.
#[derive(Debug, Clone, Copy)]
pub(super) struct Limits {
    /// Most bytes in one segment; a session keeps at most two.
    pub segment_bytes: u64,
    /// Most bytes in the directory, running sessions aside.
    pub total_bytes: u64,
    /// Most bytes (and, at [`OP_COST`] each, operations) waiting for the writer.
    pub queue_bytes: usize,
    /// How long a finished session's transcript is kept.
    pub max_age: Duration,
}

impl Default for Limits {
    fn default() -> Self {
        Self {
            segment_bytes: 4 * 1024 * 1024,
            total_bytes: 256 * 1024 * 1024,
            queue_bytes: 4 * 1024 * 1024,
            max_age: Duration::from_secs(14 * 24 * 60 * 60),
        }
    }
}

#[derive(Debug)]
pub(super) enum ArmError {
    /// The session is already being recorded.
    Duplicate,
    /// The store has stopped.
    Closed,
}

enum Op {
    /// A new session, with the run it was spawned for. A transcript left under
    /// its id is replaced, unless it was another run's: that one is kept
    /// under a name of its own.
    Open(String, Option<String>),
    Data(String, Vec<u8>),
    Gap(String, u64),
    /// The session ended: sync and release its file.
    Close(String),
    /// The session never started.
    Discard(String),
}

impl Op {
    fn session(&self) -> &str {
        match self {
            Op::Open(session, _)
            | Op::Data(session, _)
            | Op::Gap(session, _)
            | Op::Close(session)
            | Op::Discard(session) => session,
        }
    }
}

/// The operations the writer took in one go. However it ends, they all count
/// as finished, so a read waiting on them is released; if the writer panicked
/// part-way, the sessions whose operations it did not apply are marked failed,
/// not left looking whole.
struct Batch<'a> {
    inner: &'a Inner,
    ops: VecDeque<Op>,
    total: u64,
    current: Option<String>,
}

impl<'a> Batch<'a> {
    fn take(inner: &'a Inner, queue: &mut Queue) -> Self {
        let ops: VecDeque<Op> = std::mem::take(&mut queue.ops);
        queue.cost = 0;
        Self {
            inner,
            total: ops.len() as u64,
            ops,
            current: None,
        }
    }

    fn next(&mut self) -> Option<Op> {
        let op = self.ops.pop_front()?;
        self.current = Some(op.session().to_owned());
        Some(op)
    }
}

impl Drop for Batch<'_> {
    fn drop(&mut self) {
        if std::thread::panicking() {
            let mut lost: Vec<String> = self.current.take().into_iter().collect();
            lost.extend(self.ops.iter().map(|op| op.session().to_owned()));
            for session in lost {
                self.inner.fail(
                    &session,
                    "the transcript writer stopped unexpectedly, so later output is not here"
                        .to_owned(),
                );
            }
        }
        let mut queue = lock(&self.inner.queue);
        // A marker the writer will never append no longer holds its bytes back
        // from a read: the session is marked failed above, which a read says.
        for op in self.ops.drain(..) {
            if let Op::Gap(session, dropped) = op {
                queue.marked(&session, dropped);
            }
        }
        queue.done += self.total;
        drop(queue);
        self.inner.settled.notify_all();
    }
}

#[derive(Default)]
struct Queue {
    ops: VecDeque<Op>,
    /// Bytes (and per-op costs) of `ops`, which the bound is on.
    cost: usize,
    /// Bytes dropped per session that no gap marker has been queued for: held
    /// back until a chunk fits or the session ends, and, once the writer is
    /// stopping, for good. A read reports it (`TranscriptTail::unrecorded_bytes`).
    gaps: HashMap<String, u64>,
    /// Bytes per session whose gap marker has left `gaps` and is not yet in the
    /// file: queued behind other operations, or taken by the writer and not yet
    /// appended. Added where the marker is queued, released by the writer only
    /// once it has appended the marker (or can no longer), so a byte is always
    /// in `gaps`, here, or in the file's markers. A read reports `gaps` and this.
    marking: HashMap<String, u64>,
    /// Sessions armed and not yet closed.
    recording: HashSet<String>,
    /// Operations ever queued, and ever finished by the writer.
    queued: u64,
    done: u64,
    shutdown: bool,
}

/// One session's open log, owned by the writer.
struct Log {
    file: File,
    len: u64,
    dirty: bool,
}

struct Inner {
    dir: PathBuf,
    limits: Limits,
    queue: Mutex<Queue>,
    /// The writer waits here for operations.
    work: Condvar,
    /// Readers wait here for the writer to finish what was queued.
    settled: Condvar,
    /// Open logs. Only the writer takes this lock.
    files: Mutex<HashMap<String, Log>>,
    /// Why a session's recording stopped, for the sessions it has. The same
    /// reason is written to the session's `.log.err` for a later host.
    failures: Mutex<HashMap<String, String>>,
    /// A session whose next data write panics, for the test of what that costs.
    #[cfg(test)]
    panic_on: Mutex<Option<String>>,
    /// Held by a test to stop the writer before it writes a gap marker, with
    /// the marker's operation already taken from the queue.
    #[cfg(test)]
    gap_hold: Mutex<()>,
}

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

/// The host's transcript store.
pub(super) struct Transcripts {
    inner: Arc<Inner>,
    writer: Mutex<Option<JoinHandle<()>>>,
}

/// The copy one session's ring takes. Dropping it (with the ring) ends the
/// session's transcript.
struct SessionTap {
    inner: Arc<Inner>,
    session: String,
}

impl Tap for SessionTap {
    fn chunk(&self, data: &[u8]) {
        self.inner.push_data(&self.session, data);
    }
}

impl Drop for SessionTap {
    fn drop(&mut self) {
        self.inner.close(&self.session);
    }
}

impl Transcripts {
    /// The store in `dir`, created private and refused if it is a link, with
    /// what the limits no longer allow already deleted.
    pub(super) fn open(dir: PathBuf, limits: Limits) -> Result<Self, String> {
        crate::local_socket::private_dir(&dir)?;
        let limits = Limits {
            segment_bytes: limits.segment_bytes.max(MIN_SEGMENT_BYTES),
            ..limits
        };
        let inner = Arc::new(Inner {
            dir,
            limits,
            queue: Mutex::default(),
            work: Condvar::new(),
            settled: Condvar::new(),
            files: Mutex::default(),
            failures: Mutex::default(),
            #[cfg(test)]
            panic_on: Mutex::default(),
            #[cfg(test)]
            gap_hold: Mutex::default(),
        });
        inner.sweep(&HashSet::new())?;
        let worker = inner.clone();
        let writer = crate::supervisor::spawn_service("terminal-transcripts", move || worker.run())
            .map_err(|e| format!("could not start the transcript writer: {e}"))?;
        Ok(Self {
            inner,
            writer: Mutex::new(Some(writer)),
        })
    }

    /// Starts recording `session`, spawned for `run_id` when it has one. The
    /// tap goes to the session's ring.
    pub(super) fn arm(
        &self,
        session: &str,
        run_id: Option<&str>,
    ) -> Result<Arc<dyn Tap>, ArmError> {
        let mut queue = lock(&self.inner.queue);
        if queue.shutdown {
            return Err(ArmError::Closed);
        }
        if !queue.recording.insert(session.to_owned()) {
            return Err(ArmError::Duplicate);
        }
        queue.gaps.remove(session);
        queue.push(
            Op::Open(
                session.to_owned(),
                run_id.map(|id| clip_run_id(id).to_owned()),
            ),
            0,
        );
        drop(queue);
        self.inner.work.notify_one();
        Ok(Arc::new(SessionTap {
            inner: self.inner.clone(),
            session: session.to_owned(),
        }))
    }

    /// The session never started: nothing it recorded is worth keeping.
    pub(super) fn discard(&self, session: &str) {
        let mut queue = lock(&self.inner.queue);
        if queue.recording.contains(session) || queue.shutdown {
            return;
        }
        queue.push(Op::Discard(session.to_owned()), 0);
        drop(queue);
        self.inner.work.notify_one();
    }

    /// Waits until everything queued so far is on disk, or `within` passes.
    pub(super) fn flush(&self, within: Duration) -> bool {
        self.inner.settle(within)
    }

    /// The newest `max_bytes` of `session`'s output. With `run_id`, only that
    /// run's: an id a later session reused answers [`TranscriptRead::Reused`]
    /// (or the run's own kept transcript, when it was kept).
    pub(super) fn read(
        &self,
        session: &str,
        max_bytes: usize,
        redact: bool,
        run_id: Option<&str>,
    ) -> TranscriptRead {
        self.inner.read(session, max_bytes, redact, run_id)
    }

    /// Writes out what is queued and stops the writer.
    pub(super) fn shutdown(&self) {
        lock(&self.inner.queue).shutdown = true;
        self.inner.work.notify_all();
        let writer = lock(&self.writer).take();
        if let Some(writer) = writer {
            let _ = writer.join();
        }
    }
}

impl Drop for Transcripts {
    fn drop(&mut self) {
        self.shutdown();
    }
}

impl Queue {
    fn push(&mut self, op: Op, cost: usize) {
        self.cost += cost;
        self.queued += 1;
        self.ops.push_back(op);
    }

    /// Queues the marker for the bytes `session` has had dropped since its last
    /// one, moving them from `gaps` to `marking` in the same critical section.
    fn mark_gap(&mut self, session: &str, cost: usize) {
        if let Some(dropped) = self.gaps.remove(session) {
            let marking = self.marking.entry(session.to_owned()).or_insert(0);
            *marking = marking.saturating_add(dropped);
            self.push(Op::Gap(session.to_owned(), dropped), cost);
        }
    }

    /// The marker for `dropped` bytes is in the file, or never will be.
    fn marked(&mut self, session: &str, dropped: u64) {
        if let Some(marking) = self.marking.get_mut(session) {
            *marking = marking.saturating_sub(dropped);
            if *marking == 0 {
                self.marking.remove(session);
            }
        }
    }

    /// Dropped bytes of `session` that are not in its file's markers yet.
    fn unrecorded(&self, session: &str) -> u64 {
        let pending = self.gaps.get(session).copied().unwrap_or(0);
        let queued = self.marking.get(session).copied().unwrap_or(0);
        pending.saturating_add(queued)
    }
}

/// Releases a gap marker's bytes from [`Queue::marking`] when the writer is
/// done with it, including when it panics part-way.
struct Marked<'a> {
    inner: &'a Inner,
    session: &'a str,
    dropped: u64,
}

impl Drop for Marked<'_> {
    fn drop(&mut self) {
        lock(&self.inner.queue).marked(self.session, self.dropped);
    }
}

impl Inner {
    fn path(&self, session: &str) -> PathBuf {
        self.dir.join(format!("{session}.log"))
    }

    fn previous(&self, session: &str) -> PathBuf {
        self.dir.join(format!("{session}.log.1"))
    }

    /// Why this session's recording stopped, when it did.
    fn note(&self, session: &str) -> PathBuf {
        self.dir.join(format!("{session}.log.err"))
    }

    /// The run this session was spawned for, when it was spawned for one.
    fn meta(&self, session: &str) -> PathBuf {
        self.dir.join(format!("{session}.log.meta"))
    }

    /// Every file a session (or a kept earlier run's stem) can have, the run's
    /// record first: moved in this order, a partly moved transcript still has
    /// the record that says whose it is.
    fn files_of(&self, stem: &str) -> [PathBuf; 4] {
        [
            self.meta(stem),
            self.path(stem),
            self.previous(stem),
            self.note(stem),
        ]
    }

    // -- the reader's side: never waits on the disk -------------------------

    fn push_data(&self, session: &str, data: &[u8]) {
        if data.is_empty() {
            return;
        }
        let owned = data.to_vec();
        let cost = data.len() + OP_COST;
        let mut queue = lock(&self.queue);
        // Once the writer is stopping nothing queued would be written, and no
        // later chunk will carry a marker for these bytes either. They are
        // counted all the same, in the one place a read looks, so it can say
        // they are missing instead of leaving the text to read as complete.
        if queue.shutdown || queue.cost + cost > self.limits.queue_bytes {
            let dropped = queue.gaps.entry(session.to_owned()).or_insert(0);
            *dropped = dropped.saturating_add(data.len() as u64);
            return;
        }
        queue.mark_gap(session, OP_COST);
        queue.push(Op::Data(session.to_owned(), owned), cost);
        drop(queue);
        self.work.notify_one();
    }

    fn close(&self, session: &str) {
        let mut queue = lock(&self.queue);
        queue.recording.remove(session);
        if queue.shutdown {
            // Never written, so the file keeps no end marker: a later host
            // reads that as a session its host did not get to close. Bytes
            // dropped and not yet marked stay counted: no gap marker will
            // ever be written for them, and a read still reports them.
            return;
        }
        queue.mark_gap(session, 0);
        queue.push(Op::Close(session.to_owned()), 0);
        drop(queue);
        self.work.notify_one();
    }

    fn settle(&self, within: Duration) -> bool {
        let deadline = std::time::Instant::now() + within;
        let mut queue = lock(&self.queue);
        let target = queue.queued;
        while queue.done < target {
            let left = deadline.saturating_duration_since(std::time::Instant::now());
            if left.is_zero() {
                return false;
            }
            queue = self
                .settled
                .wait_timeout(queue, left)
                .unwrap_or_else(PoisonError::into_inner)
                .0;
        }
        true
    }

    // -- the writer ----------------------------------------------------------

    /// The writer's body: runs until shut down and drained. Safe to run again
    /// after a panic, since everything it keeps is in `self`.
    fn run(&self) {
        loop {
            // Read before the queue is taken: the queue's lock is the
            // reader's, and nothing here may hold it while waiting on a file.
            let dirty = lock(&self.files).values().any(|log| log.dirty);
            let mut queue = lock(&self.queue);
            if queue.ops.is_empty() {
                if queue.shutdown {
                    drop(queue);
                    self.close_all();
                    return;
                }
                if dirty {
                    let (woken, timeout) = self
                        .work
                        .wait_timeout(queue, SYNC_EVERY)
                        .unwrap_or_else(PoisonError::into_inner);
                    queue = woken;
                    if timeout.timed_out() && queue.ops.is_empty() {
                        drop(queue);
                        self.sync_dirty();
                        continue;
                    }
                } else {
                    queue = self
                        .work
                        .wait(queue)
                        .unwrap_or_else(PoisonError::into_inner);
                }
                if queue.ops.is_empty() {
                    continue;
                }
            }
            let mut batch = Batch::take(self, &mut queue);
            drop(queue);
            while let Some(op) = batch.next() {
                self.apply(op);
            }
        }
    }

    fn apply(&self, op: Op) {
        #[cfg(test)]
        if let Op::Data(session, _) = &op {
            if lock(&self.panic_on).as_deref() == Some(session.as_str()) {
                panic!("a test made the transcript writer panic on {session}");
            }
        }
        #[cfg(test)]
        if let Op::Gap(..) = &op {
            drop(lock(&self.gap_hold));
        }
        match op {
            Op::Open(session, run_id) => {
                lock(&self.failures).remove(&session);
                let kept = self.retire_previous(&session, run_id.as_deref());
                let log = self.create(&session);
                let made = log.is_ok();
                self.install(&session, log);
                if let (true, Some(run_id)) = (made, &run_id) {
                    // Without it a read cannot tell this run's output from a
                    // later session's under the same id, so the recording
                    // is marked failed rather than left to look identified.
                    if let Err(error) = self.write_meta(&session, run_id) {
                        self.fail(&session, error);
                    }
                }
                if kept {
                    self.enforce();
                }
            }
            Op::Data(session, bytes) => self.append(&session, &bytes),
            Op::Gap(session, dropped) => {
                // Released only after the append: a read between the two
                // names the bytes twice, never zero times.
                let _release = Marked {
                    inner: self,
                    session: &session,
                    dropped,
                };
                self.append(&session, gap_marker(dropped).as_bytes());
            }
            Op::Close(session) => {
                let log = lock(&self.files).remove(&session);
                if let Some(mut log) = log {
                    // A recording that failed already says why, in its note;
                    // an end marker would claim a close it did not have.
                    if !self.failed(&session) {
                        if let Err(error) = self.write_ended(&session, &mut log) {
                            self.fail(&session, error);
                        }
                    }
                    if let Err(error) = log.file.sync_data() {
                        log::warn!(target: "terminal_host", "a transcript could not be synced as its session ended: {error}");
                    }
                }
                self.enforce();
            }
            Op::Discard(session) => {
                if !lock(&self.files).contains_key(&session) {
                    self.remove_files(&session);
                    lock(&self.failures).remove(&session);
                }
            }
        }
    }

    fn install(&self, session: &str, log: Result<Log, String>) {
        match log {
            Ok(log) => {
                lock(&self.files).insert(session.to_owned(), log);
            }
            Err(error) => self.fail(session, error),
        }
    }

    fn fail(&self, session: &str, reason: String) {
        let first = lock(&self.failures)
            .insert(session.to_owned(), reason.clone())
            .is_none();
        if first {
            log::error!(target: "terminal_host", "a session's transcript stopped being recorded: {reason}");
            // For a host that starts later and has no memory of this one.
            if let Err(error) = self.write_note(session, &reason) {
                log::warn!(target: "terminal_host", "could not keep a note of why a transcript stopped: {error}");
            }
        }
    }

    fn failed(&self, session: &str) -> bool {
        lock(&self.failures).contains_key(session)
    }

    /// A segment file: appended to, `0600`, never through a link, and ours.
    ///
    /// `O_NONBLOCK` is for the open alone: a named pipe planted by the same
    /// user would otherwise block it for ever. It does nothing to a regular
    /// file, and a pipe is refused by the check below.
    fn create_file(path: &Path) -> Result<File, String> {
        let mut options = OpenOptions::new();
        options
            .append(true)
            .create(true)
            .mode(0o600)
            .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK);
        let file = options
            .open(path)
            .map_err(|e| format!("{}: {e}", path.display()))?;
        let meta = file
            .metadata()
            .map_err(|e| format!("{}: {e}", path.display()))?;
        // SAFETY: `getuid` reads this process's real user id and cannot fail.
        if !meta.is_file() || meta.uid() != unsafe { libc::getuid() } {
            return Err(format!("{} is not a file this host owns", path.display()));
        }
        if meta.permissions().mode() & 0o777 != 0o600 {
            file.set_permissions(std::fs::Permissions::from_mode(0o600))
                .map_err(|e| format!("{}: {e}", path.display()))?;
        }
        Ok(file)
    }

    /// Writes [`ENDED`] as the last bytes of the session's newest segment,
    /// under the segment size like any other bytes: when it would not fit the
    /// segment rotates first.
    fn write_ended(&self, session: &str, log: &mut Log) -> Result<(), String> {
        let room = self.limits.segment_bytes.saturating_sub(log.len);
        if room < ENDED.len() as u64 {
            self.rotate(session, log)?;
        }
        log.file
            .write_all(ENDED)
            .map_err(|e| format!("{}: {e}", self.path(session).display()))?;
        log.len += ENDED.len() as u64;
        log.dirty = true;
        Ok(())
    }

    /// Records why `session` stopped being recorded, beside its log.
    fn write_note(&self, session: &str, reason: &str) -> Result<(), String> {
        Self::write_small(&self.note(session), reason, NOTE_MAX)
    }

    /// Records which run `session` was spawned for, beside its log.
    fn write_meta(&self, session: &str, run_id: &str) -> Result<(), String> {
        Self::write_small(&self.meta(session), run_id, RUN_ID_MAX)
    }

    /// Replaces the small file at `path` with the first `max` bytes of `text`
    /// (cut on a character boundary): `0600`, never through a link, and ours.
    fn write_small(path: &Path, text: &str, max: usize) -> Result<(), String> {
        let mut options = OpenOptions::new();
        options
            .write(true)
            .create(true)
            .truncate(true)
            .mode(0o600)
            .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK);
        let mut file = options
            .open(path)
            .map_err(|e| format!("{}: {e}", path.display()))?;
        let meta = file
            .metadata()
            .map_err(|e| format!("{}: {e}", path.display()))?;
        // SAFETY: `getuid` reads this process's real user id and cannot fail.
        if !meta.is_file() || meta.uid() != unsafe { libc::getuid() } {
            return Err(format!("{} is not a file this host owns", path.display()));
        }
        let mut end = text.len().min(max);
        while !text.is_char_boundary(end) {
            end -= 1;
        }
        file.write_all(&text.as_bytes()[..end])
            .and_then(|()| file.sync_data())
            .map_err(|e| format!("{}: {e}", path.display()))
    }

    /// The text of the small file at `path`, at most `max` bytes: `None` when
    /// there is no such file, and an error saying why when there is one that
    /// cannot be read (a link, a file of another user, a failed read).
    fn read_small(path: &Path, max: usize) -> Result<Option<String>, String> {
        let mut options = OpenOptions::new();
        options
            .read(true)
            .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK);
        let file = match options.open(path) {
            Ok(file) => file,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(error) => return Err(error.to_string()),
        };
        let meta = file.metadata().map_err(|e| e.to_string())?;
        // SAFETY: `getuid` reads this process's real user id and cannot fail.
        if !meta.is_file() || meta.uid() != unsafe { libc::getuid() } {
            return Err("it is not a file this host wrote".to_owned());
        }
        let mut bytes = Vec::new();
        std::io::Read::read_to_end(&mut std::io::Read::take(&file, max as u64), &mut bytes)
            .map_err(|e| e.to_string())?;
        Ok(Some(String::from_utf8_lossy(&bytes).into_owned()))
    }

    /// The reason an earlier host left for `session`'s recording stopping, if
    /// it left one. A note that is there and cannot be read is reported as that,
    /// not as no note.
    fn read_note(&self, session: &str) -> Option<String> {
        let path = self.note(session);
        match Self::read_small(&path, NOTE_MAX) {
            Ok(note) => note,
            Err(why) => Some(unreadable_note(&path, &why)),
        }
    }

    /// The run `stem`'s transcript was recorded for: `None` when it was not
    /// spawned for one, an error when the record is there and unreadable.
    fn read_meta(&self, stem: &str) -> Result<Option<String>, String> {
        let path = self.meta(stem);
        Self::read_small(&path, RUN_ID_MAX).map_err(|why| format!("{}: {why}", path.display()))
    }

    /// Whether any segment of `stem` is on disk.
    fn holds(&self, stem: &str) -> bool {
        [self.path(stem), self.previous(stem)]
            .iter()
            .any(|path| std::fs::symlink_metadata(path).is_ok())
    }

    /// Clears the way for a new session under `session`. What an earlier
    /// session left there is deleted, unless it was recorded for a different
    /// run than the new one: terminal ids are `term-<pid>-<count>`, so after a
    /// reboot a new session can carry an id an earlier run's output is filed
    /// under, and that output is still the earlier run's to read. It is kept
    /// under a name of its own (at most one per run), subject to the directory's
    /// limits like any other. Returns whether something was kept.
    fn retire_previous(&self, session: &str, run_id: Option<&str>) -> bool {
        let any = self
            .files_of(session)
            .iter()
            .any(|path| std::fs::symlink_metadata(path).is_ok());
        if !any {
            return false;
        }
        match self.read_meta(session) {
            Ok(Some(earlier)) if Some(earlier.as_str()) != run_id => {
                match self.keep_under_own_name(session, &earlier) {
                    Ok(()) => return true,
                    Err(error) => {
                        log::warn!(target: "terminal_host", "an earlier run's transcript could not be kept, so a new session replaces it: {error}")
                    }
                }
            }
            Ok(_) => {}
            Err(error) => {
                log::warn!(target: "terminal_host", "an earlier transcript's run could not be read, so a new session replaces it: {error}")
            }
        }
        self.remove_files(session);
        false
    }

    /// Moves every file of `session` to the stem kept for `run_id`, replacing
    /// what was kept for that run before.
    fn keep_under_own_name(&self, session: &str, run_id: &str) -> Result<(), String> {
        let stem = kept_stem(session, run_id);
        self.remove_files(&stem);
        for (from, to) in self.files_of(session).into_iter().zip(self.files_of(&stem)) {
            match std::fs::rename(&from, &to) {
                Ok(()) => {}
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                Err(error) => {
                    return Err(format!("{} to {}: {error}", from.display(), to.display()))
                }
            }
        }
        Ok(())
    }

    fn create(&self, session: &str) -> Result<Log, String> {
        let path = self.path(session);
        let file = Self::create_file(&path)?;
        let len = file.metadata().map_err(|e| e.to_string())?.len();
        Ok(Log {
            file,
            len,
            dirty: false,
        })
    }

    fn remove_files(&self, session: &str) {
        for path in self.files_of(session) {
            match std::fs::remove_file(&path) {
                Ok(()) => {}
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                Err(error) => {
                    log::warn!(target: "terminal_host", "could not remove {}: {error}", path.display())
                }
            }
        }
    }

    fn append(&self, session: &str, mut bytes: &[u8]) {
        if self.failed(session) {
            return;
        }
        let mut rotated = false;
        {
            let mut files = lock(&self.files);
            if !files.contains_key(session) {
                match self.create(session) {
                    Ok(log) => {
                        files.insert(session.to_owned(), log);
                    }
                    Err(error) => {
                        drop(files);
                        self.fail(session, error);
                        return;
                    }
                }
            }
            let Some(log) = files.get_mut(session) else {
                return;
            };
            while !bytes.is_empty() {
                let room = self.limits.segment_bytes.saturating_sub(log.len);
                if room == 0 {
                    match self.rotate(session, log) {
                        Ok(()) => rotated = true,
                        Err(error) => {
                            drop(files);
                            self.fail(session, error);
                            return;
                        }
                    }
                    continue;
                }
                let take = usize::try_from(room).map_or(bytes.len(), |r| r.min(bytes.len()));
                if let Err(error) = log.file.write_all(&bytes[..take]) {
                    let path = self.path(session);
                    drop(files);
                    self.fail(session, format!("{}: {error}", path.display()));
                    return;
                }
                log.len += take as u64;
                log.dirty = true;
                bytes = &bytes[take..];
            }
        }
        if rotated {
            self.enforce();
        }
    }

    /// The current segment becomes the previous one, replacing it, and a new
    /// current one starts with [`CONTINUED`].
    fn rotate(&self, session: &str, log: &mut Log) -> Result<(), String> {
        if let Err(error) = log.file.sync_data() {
            log::warn!(target: "terminal_host", "a transcript segment could not be synced as it was closed: {error}");
        }
        let (current, previous) = (self.path(session), self.previous(session));
        std::fs::rename(&current, &previous).map_err(|e| format!("{}: {e}", current.display()))?;
        let mut file = Self::create_file(&current)?;
        file.write_all(CONTINUED)
            .map_err(|e| format!("{}: {e}", current.display()))?;
        *log = Log {
            file,
            len: CONTINUED.len() as u64,
            dirty: true,
        };
        Ok(())
    }

    fn sync_dirty(&self) {
        for log in lock(&self.files).values_mut().filter(|log| log.dirty) {
            match log.file.sync_data() {
                Ok(()) => log.dirty = false,
                Err(error) => {
                    log::warn!(target: "terminal_host", "a transcript could not be synced: {error}")
                }
            }
        }
    }

    fn close_all(&self) {
        let logs: Vec<Log> = lock(&self.files).drain().map(|(_, log)| log).collect();
        for log in logs {
            if let Err(error) = log.file.sync_data() {
                log::warn!(target: "terminal_host", "a transcript could not be synced as the host stopped: {error}");
            }
        }
    }

    // -- retention -----------------------------------------------------------

    /// What the directory holds, per session, from at most `cap` entries. The
    /// flag is whether there were more: a total counted from a capped scan is
    /// short, and the caller must not treat it as the directory's size.
    fn scan_up_to(&self, cap: usize) -> Result<(HashMap<String, Held>, bool), String> {
        let entries =
            std::fs::read_dir(&self.dir).map_err(|e| format!("{}: {e}", self.dir.display()))?;
        let mut held: HashMap<String, Held> = HashMap::new();
        let mut capped = false;
        for (seen, entry) in entries.enumerate() {
            if seen >= cap {
                capped = true;
                break;
            }
            let entry = entry.map_err(|e| format!("{}: {e}", self.dir.display()))?;
            let name = entry.file_name();
            let Some(name) = name.to_str() else { continue };
            let (session, previous) = if let Some(session) = name.strip_suffix(".log.1") {
                (session, true)
            } else if let Some(session) = name
                .strip_suffix(".log.err")
                .or_else(|| name.strip_suffix(".log.meta"))
                .or_else(|| name.strip_suffix(".log"))
            {
                (session, false)
            } else {
                continue;
            };
            // A session's own id, or the stem of an earlier run's transcript
            // kept after a later session took the id.
            if !is_stem(session) {
                continue;
            }
            // `DirEntry::metadata` does not follow a link.
            let Ok(meta) = entry.metadata() else { continue };
            if !meta.is_file() {
                continue;
            }
            let held = held.entry(session.to_owned()).or_default();
            held.bytes += meta.len();
            held.newest = held
                .newest
                .max(meta.modified().unwrap_or(SystemTime::UNIX_EPOCH));
            if previous {
                held.previous = Some(entry.path());
            }
            held.paths.push(entry.path());
        }
        Ok((held, capped))
    }

    /// Deletes what the age and size limits no longer allow, except `live`.
    fn sweep(&self, live: &HashSet<String>) -> Result<(), String> {
        let (mut held, capped) = self.scan_up_to(MAX_SCANNED)?;
        if capped {
            log::warn!(target: "terminal_host", "the transcript directory holds more than {MAX_SCANNED} entries; limits are enforced on the {MAX_SCANNED} read, so it may stay over them");
        }
        let now = SystemTime::now();
        held.retain(|session, entry| {
            let expired = !live.contains(session)
                && now
                    .duration_since(entry.newest)
                    .is_ok_and(|age| age > self.limits.max_age);
            if expired {
                entry.remove();
            }
            !expired
        });
        let mut total: u64 = held.values().map(|entry| entry.bytes).sum();
        if total <= self.limits.total_bytes {
            return Ok(());
        }
        let mut finished: Vec<(&String, &Held)> =
            held.iter().filter(|(s, _)| !live.contains(*s)).collect();
        finished.sort_by_key(|(_, entry)| entry.newest);
        for (_, entry) in finished {
            if total <= self.limits.total_bytes {
                return Ok(());
            }
            total = total.saturating_sub(entry.bytes);
            entry.remove();
        }
        // Only running sessions are left, and they alone are over: their
        // previous segments are the oldest output there is.
        let mut running: Vec<&Held> = held
            .iter()
            .filter(|(s, _)| live.contains(*s))
            .map(|(_, entry)| entry)
            .collect();
        running.sort_by_key(|entry| entry.newest);
        for entry in running {
            if total <= self.limits.total_bytes {
                break;
            }
            if let Some(previous) = &entry.previous {
                let freed = std::fs::metadata(previous).map_or(0, |m| m.len());
                if std::fs::remove_file(previous).is_ok() {
                    total = total.saturating_sub(freed);
                }
            }
        }
        if total > self.limits.total_bytes {
            log::warn!(target: "terminal_host", "running sessions' transcripts hold {total} bytes, over the {} byte limit; they shrink as they rotate", self.limits.total_bytes);
        }
        Ok(())
    }

    fn enforce(&self) {
        let live: HashSet<String> = lock(&self.files).keys().cloned().collect();
        if let Err(error) = self.sweep(&live) {
            log::warn!(target: "terminal_host", "transcript limits could not be enforced: {error}");
        }
    }

    // -- reading -------------------------------------------------------------

    /// Which transcript answers a read of `session`, and the run it was
    /// recorded for. Without `expected` it is whatever the id holds. With one it
    /// is that run's: the id's own transcript when it was recorded for it, else
    /// the copy kept when a later session took the id. Anything else under the
    /// id is not this run's, and is answered as such (or, when what is there
    /// cannot be told apart from a transcript still being opened, as unreadable)
    /// instead of being shown as the run's output.
    fn choose(
        &self,
        session: &str,
        expected: Option<&str>,
        settled: bool,
    ) -> Result<(String, Option<String>), TranscriptRead> {
        let unreadable = |reason: String| TranscriptRead::Unreadable {
            reason: format!("which run this transcript belongs to could not be read: {reason}"),
        };
        let current = self.read_meta(session).map_err(unreadable)?;
        let Some(want) = expected.map(clip_run_id) else {
            return Ok((session.to_owned(), current));
        };
        if current.as_deref() == Some(want) {
            return Ok((session.to_owned(), current));
        }
        let kept = kept_stem(session, want);
        if self.read_meta(&kept).map_err(unreadable)?.as_deref() == Some(want) {
            return Ok((kept, Some(want.to_owned())));
        }
        if !self.holds(session) {
            // Nothing under the id at all: the ordinary missing / could-not-be
            // -recorded answer, not a claim about another session.
            return Ok((session.to_owned(), current));
        }
        if current.is_none() {
            // A transcript with no run on record is a session spawned without
            // one, or one whose opening was not finished or whose record was not
            // written; only the first is another session's.
            let failure = lock(&self.failures)
                .get(session)
                .cloned()
                .or_else(|| self.read_note(session));
            if let Some(error) = failure {
                return Err(TranscriptRead::Unreadable {
                    reason: format!(
                        "whether this transcript is this run's could not be confirmed: {error}"
                    ),
                });
            }
            if !settled {
                return Err(TranscriptRead::Unreadable {
                    reason: "the host had not finished opening this session's transcript, so whether it is this run's could not be confirmed".to_owned(),
                });
            }
        }
        Err(TranscriptRead::Reused {
            reason: match current {
                Some(_) => "a later session reused this terminal id, so the output saved under it is not this run's".to_owned(),
                None => "no run was recorded for the output saved under this terminal id, so it is not shown as this run's".to_owned(),
            },
        })
    }

    fn read(
        &self,
        session: &str,
        max_bytes: usize,
        redact: bool,
        expected: Option<&str>,
    ) -> TranscriptRead {
        if !crate::terminal::is_session_id(session) {
            return TranscriptRead::Unreadable {
                reason: format!("{session:?} is not a session id"),
            };
        }
        let settled = self.settle(SETTLE_DEADLINE);
        let (stem, run_id) = match self.choose(session, expected, settled) {
            Ok(chosen) => chosen,
            Err(answer) => return answer,
        };
        let session = stem.as_str();
        // One look at the queue for both facts, taken before the files are
        // read: a count and a liveness from two moments could disagree. Bytes
        // counted here that a gap marker reaches the file for before the read
        // below are named twice, never zero times.
        let (live, unrecorded_bytes) = {
            let queue = lock(&self.queue);
            (queue.recording.contains(session), queue.unrecorded(session))
        };
        // This host's memory first; a host that started later has only the
        // note the earlier one left. A running session has no earlier host.
        let remembered = lock(&self.failures).get(session).cloned();
        let failure = match remembered {
            Some(error) => Some(error),
            None if live => None,
            None => self.read_note(session),
        };
        let max = max_bytes.clamp(1, MAX_TRANSCRIPT_TAIL_BYTES);
        // Masking needs what came before the window: the name a value follows,
        // the first line of a key. It looks that much further back, then clips.
        let want = if redact {
            max.saturating_add(REDACT_LOOKBACK)
        } else {
            max
        };
        let raw = match self.read_tail(session, want) {
            Ok(Some(raw)) => raw,
            Ok(None) => {
                return match failure {
                    Some(error) => TranscriptRead::Unreadable {
                        reason: format!("this session's output could not be recorded: {error}"),
                    },
                    None => TranscriptRead::Missing,
                }
            }
            Err(reason) => return TranscriptRead::Unreadable { reason },
        };
        // A new session under the id may have taken it between the check above
        // and the read: the bytes are the run's only if the record still says so.
        if let Some(want) = expected.map(clip_run_id) {
            match self.read_meta(session) {
                Ok(now) if now.as_deref() == Some(want) => {}
                Ok(_) => {
                    return TranscriptRead::Reused {
                        reason: "a later session took this terminal id while its output was being read, so what was read is not shown as this run's".to_owned(),
                    }
                }
                Err(reason) => {
                    return TranscriptRead::Unreadable {
                        reason: format!(
                            "which run this transcript belongs to could not be read: {reason}"
                        ),
                    }
                }
            }
        }
        let mut clipped = raw.clipped;
        let bytes = if redact {
            let (bytes, cut) = redacted(&raw.bytes, max);
            clipped |= cut;
            bytes
        } else {
            raw.bytes
        };
        let gap_markers = count(&bytes, GAP_TAIL);
        // A finished session whose newest segment was never closed and left no
        // note: its host died or was killed. Whether anything is missing is
        // unknown, which is not the same as nothing missing.
        let recording_error =
            failure.or_else(|| (!live && !raw.ended).then(|| UNCLOSED.to_owned()));
        TranscriptRead::Found(TranscriptTail {
            data_b64: BASE64.encode(&bytes),
            bytes: bytes.len(),
            retained: raw.retained,
            truncated: raw.rotated || clipped,
            rotated: raw.rotated,
            clipped,
            gap_markers,
            unrecorded_bytes,
            redacted: redact,
            live,
            settled,
            recording_error,
            run_id,
        })
    }

    /// The newest `max` bytes across both segments, with each continuation
    /// line and the end marker taken out; `None` when there is neither segment.
    fn read_tail(&self, session: &str, max: usize) -> Result<Option<RawTail>, String> {
        self.read_tail_between(session, max, &mut || {})
    }

    /// [`Self::read_tail`], calling `between` after the older segment is open
    /// and before the newer one is: where a rotation can slip in.
    fn read_tail_between(
        &self,
        session: &str,
        max: usize,
        between: &mut dyn FnMut(),
    ) -> Result<Option<RawTail>, String> {
        let (previous_path, current_path) = (self.previous(session), self.path(session));
        for _ in 0..READ_ATTEMPTS {
            let previous = Segment::open(&previous_path)?;
            between();
            let current = Segment::open(&current_path)?;
            // A rotation between the two opens leaves the older name on what
            // was the newest segment, and a read of the pair would skip it.
            if Segment::is_current(&previous, &previous_path)?
                && Segment::is_current(&current, &current_path)?
            {
                let segments: Vec<Segment> = previous.into_iter().chain(current).collect();
                return Self::tail_of(&segments, max);
            }
        }
        Err(format!(
            "{} was rotated again and again while it was being read",
            current_path.display()
        ))
    }

    fn tail_of(segments: &[Segment], max: usize) -> Result<Option<RawTail>, String> {
        let (Some(first), Some(last)) = (segments.first(), segments.last()) else {
            return Ok(None);
        };
        let rotated = first.continued;
        let retained: u64 = segments.iter().map(|s| s.effective).sum();
        let take = retained.min(max as u64);
        let start = retained - take;
        let mut bytes = Vec::with_capacity(take as usize);
        let mut at = 0u64;
        for segment in segments {
            let (from, to) = (at.max(start), (at + segment.effective).min(retained));
            if from < to {
                let mut part = vec![0u8; (to - from) as usize];
                segment
                    .file
                    .read_exact_at(&mut part, segment.skip + (from - at))
                    .map_err(|e| format!("{}: {e}", segment.path.display()))?;
                bytes.extend_from_slice(&part);
            }
            at += segment.effective;
        }
        Ok(Some(RawTail {
            bytes,
            retained,
            rotated,
            clipped: start > 0,
            ended: last.ended,
        }))
    }
}

struct RawTail {
    bytes: Vec<u8>,
    retained: u64,
    /// The start of the session's output was rotated off the disk.
    rotated: bool,
    /// Older saved output was left out to fit the bound asked for.
    clipped: bool,
    /// The newest segment ends with [`ENDED`]: its session was closed.
    ended: bool,
}

/// One segment opened to be read.
struct Segment {
    file: File,
    path: PathBuf,
    /// Which file this is, to tell it from whatever the name points to later.
    device: u64,
    inode: u64,
    /// Bytes of continuation line at its head, which the read leaves out.
    skip: u64,
    /// Bytes it contributes to the output.
    effective: u64,
    continued: bool,
    /// It ends with [`ENDED`], which the read leaves out.
    ended: bool,
}

impl Segment {
    fn open(path: &Path) -> Result<Option<Self>, String> {
        // `O_NONBLOCK` keeps a named pipe planted here from holding the open
        // for ever; it does nothing to a regular file, and the check below
        // refuses anything that is not one.
        let mut options = OpenOptions::new();
        options
            .read(true)
            .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK);
        let file = match options.open(path) {
            Ok(file) => file,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(error) => {
                // `O_NOFOLLOW` on a link is ELOOP; say what it means.
                let why = if error.raw_os_error() == Some(libc::ELOOP) {
                    "it is a symbolic link, which the host never follows".to_owned()
                } else {
                    error.to_string()
                };
                return Err(format!("{}: {why}", path.display()));
            }
        };
        let meta = file
            .metadata()
            .map_err(|e| format!("{}: {e}", path.display()))?;
        // SAFETY: `getuid` reads this process's real user id and cannot fail.
        if !meta.is_file() || meta.uid() != unsafe { libc::getuid() } {
            return Err(format!("{} is not a file this host wrote", path.display()));
        }
        let len = meta.len();
        let mut head = vec![0u8; CONTINUED.len().min(len as usize)];
        file.read_exact_at(&mut head, 0)
            .map_err(|e| format!("{}: {e}", path.display()))?;
        let continued = head == CONTINUED;
        let skip = if continued { CONTINUED.len() as u64 } else { 0 };
        let mut ended = false;
        if len - skip >= ENDED.len() as u64 {
            let mut last = vec![0u8; ENDED.len()];
            file.read_exact_at(&mut last, len - ENDED.len() as u64)
                .map_err(|e| format!("{}: {e}", path.display()))?;
            ended = last == ENDED;
        }
        let marker = if ended { ENDED.len() as u64 } else { 0 };
        Ok(Some(Self {
            file,
            path: path.to_owned(),
            device: meta.dev(),
            inode: meta.ino(),
            skip,
            effective: len - skip - marker,
            continued,
            ended,
        }))
    }

    /// Whether `path` still names the file `opened` is (or still names no
    /// file, when none was opened). A file unlinked since it was opened is
    /// still a whole, consistent snapshot, so that counts.
    fn is_current(opened: &Option<Self>, path: &Path) -> Result<bool, String> {
        match (std::fs::symlink_metadata(path), opened) {
            (Ok(meta), Some(segment)) => {
                Ok(meta.dev() == segment.device && meta.ino() == segment.inode)
            }
            (Ok(_), None) => Ok(false),
            (Err(error), _) if error.kind() == std::io::ErrorKind::NotFound => Ok(true),
            (Err(error), _) => Err(format!("{}: {error}", path.display())),
        }
    }
}

/// One session's files on disk.
struct Held {
    bytes: u64,
    newest: SystemTime,
    previous: Option<PathBuf>,
    paths: Vec<PathBuf>,
}

impl Default for Held {
    fn default() -> Self {
        Self {
            bytes: 0,
            newest: SystemTime::UNIX_EPOCH,
            previous: None,
            paths: Vec::new(),
        }
    }
}

impl Held {
    fn remove(&self) {
        for path in &self.paths {
            if let Err(error) = std::fs::remove_file(path) {
                if error.kind() != std::io::ErrorKind::NotFound {
                    log::warn!(target: "terminal_host", "could not remove {}: {error}", path.display());
                }
            }
        }
    }
}

/// `run_id` cut to [`RUN_ID_MAX`] bytes on a character boundary.
fn clip_run_id(run_id: &str) -> &str {
    let mut end = run_id.len().min(RUN_ID_MAX);
    while !run_id.is_char_boundary(end) {
        end -= 1;
    }
    &run_id[..end]
}

/// A fixed-width name for a run id (FNV-1a), to file its kept transcript under.
/// It only names a file: a read trusts the run id recorded inside, so two ids
/// that shared a name could cost a kept transcript but never be confused.
fn run_tag(run_id: &str) -> String {
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    for byte in run_id.bytes() {
        hash ^= u64::from(byte);
        hash = hash.wrapping_mul(0x0100_0000_01b3);
    }
    format!("{hash:016x}")
}

/// The stem an earlier run's transcript is kept under after a later session
/// took `session`'s id: `<session>.<tag of the run>`.
fn kept_stem(session: &str, run_id: &str) -> String {
    format!("{session}.{}", run_tag(clip_run_id(run_id)))
}

/// Whether `stem` names a session's files: a session id, or a kept earlier
/// run's stem ([`kept_stem`]).
fn is_stem(stem: &str) -> bool {
    match stem.split_once('.') {
        None => crate::terminal::is_session_id(stem),
        Some((session, tag)) => {
            crate::terminal::is_session_id(session)
                && tag.len() == 16
                && tag
                    .bytes()
                    .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
        }
    }
}

fn gap_marker(dropped: u64) -> String {
    format!(
        "\r\n[gitpulse: {dropped}{}\r\n",
        String::from_utf8_lossy(GAP_TAIL)
    )
}

fn count(haystack: &[u8], needle: &[u8]) -> u32 {
    let mut found = 0u32;
    let mut from = 0;
    while let Some(at) = haystack[from..]
        .windows(needle.len())
        .position(|window| window == needle)
    {
        found = found.saturating_add(1);
        from += at + needle.len();
    }
    found
}

fn unreadable_note(path: &Path, why: &str) -> String {
    format!(
        "a note on why recording stopped is at {} but could not be read: {why}",
        path.display()
    )
}

/// Where the first private-key marker of the named kind (`-----begin`,
/// `-----end`) is in `lower` (ASCII-lowercased text): its start, and the end of
/// its closing dashes. The marker has to sit on one line, as the redaction's
/// own pattern requires.
fn key_marker(lower: &str, tag: &str) -> Option<(usize, usize)> {
    const KEY: &str = "private key";
    let mut at = 0;
    while let Some(found) = lower[at..].find(tag) {
        let start = at + found;
        let line_end = lower[start..]
            .find(['\r', '\n'])
            .map_or(lower.len(), |n| start + n);
        let line = &lower[start..line_end];
        if let Some(k) = line.find(KEY) {
            let after = &line[k + KEY.len()..];
            let after = after.strip_prefix(" block").unwrap_or(after);
            if after.starts_with("-----") {
                return Some((start, line_end - after.len() + "-----".len()));
            }
        }
        at = start + tag.len();
    }
    None
}

/// `text` with a private key's body masked when the text starts inside it: the
/// first thing in it is the key's closing line, with no opening one before.
/// The redaction masks a block from its opening line, which this window may
/// not hold; without this its body would pass as plain text. A body longer
/// than the lookback, closing line and all outside the window, is not seen.
fn mask_leading_key_body(text: &str) -> std::borrow::Cow<'_, str> {
    let lower = text.to_ascii_lowercase();
    let Some((end_start, end_to)) = key_marker(&lower, "-----end") else {
        return std::borrow::Cow::Borrowed(text);
    };
    if key_marker(&lower, "-----begin").is_some_and(|(begin, _)| begin < end_start) {
        return std::borrow::Cow::Borrowed(text);
    }
    std::borrow::Cow::Owned(format!("<private key redacted>{}", &text[end_to..]))
}

/// `bytes` with secrets masked by the ledger's redaction, clipped to its
/// newest `max` bytes on a character boundary. The flag says it was clipped.
/// `bytes` should reach back past the `max` it is clipped to (see
/// [`REDACT_LOOKBACK`]), so what the redaction needs to recognise a secret is
/// there when the secret is.
fn redacted(bytes: &[u8], max: usize) -> (Vec<u8>, bool) {
    let lossy = String::from_utf8_lossy(bytes);
    let text = crate::ledger::redact::text(&mask_leading_key_body(&lossy));
    if text.len() <= max {
        return (text.into_bytes(), false);
    }
    let mut cut = text.len() - max;
    while !text.is_char_boundary(cut) {
        cut += 1;
    }
    (text.as_bytes()[cut..].to_vec(), true)
}

#[cfg(test)]
#[path = "transcripts_tests.rs"]
mod tests;
