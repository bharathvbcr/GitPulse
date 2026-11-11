//! When the 6 s status poll may skip its `git status`, and when it may not.
//!
//! The rule is deliberately dumb. A kept status read may be returned again only
//! if the recursive stream delivered NO event of any kind since that read began:
//! not on an ignored path, not inside `.git`, not one the event filter would
//! drop, not a rescan flag, not a backend error. Every callback invocation moves
//! the epoch. No path is ever classified as "cannot matter" here (three
//! attempts to reason that way, from ignore rules, were each broken by a
//! rename, a pathspec-looking directory name or a listing artefact), so a build
//! churning an ignored directory makes every poll read, exactly as before this
//! existed, and an idle worktree skips.
//!
//! The epoch is taken BEFORE a read starts and the answer is kept under that
//! number, so an event that lands while the read runs leaves it older than the
//! epoch the watch reports next: a read in flight never covers an event.
//!
//! # Where it runs
//!
//! The skip is owned by `target_os = "macos"`: only there is the root watch
//! recursive ([`super::filter::ROOT_WATCH_MODE`]). On Linux and Windows
//! [`PollSkip::begin`] answers [`Outcome::Unavailable`] every time and the poll
//! reads as it always did. The module itself compiles everywhere; its unit
//! tests that arm a watch need a root identity (an inode), so they run on
//! `unix`, the cases that need none run everywhere, and the one case about a
//! platform with no identity runs where `unix` is off.
//!
//! # Unknown is never clean
//!
//! [`PollSkip::begin`] says nothing (the caller reads, as it always did)
//! unless the root watch is recursive (macOS only; Linux and Windows are
//! non-recursive, see [`super::filter::ROOT_WATCH_MODE`]), the watch has not
//! ended, the worktree root is still the directory that was registered (a moved
//! root sends no event; its inode is the only witness) and the watch is not
//! convicted. A kept read older than [`KEPT_STATUS_MAX_AGE`] is not served,
//! and a read with more than [`MAX_KEPT_ROWS`] rows is not kept at all.
//!
//! # A watch has to earn its trust
//!
//! A stream can stay registered and deliver nothing (this host's `fseventsd`
//! has wedged). Nothing in an epoch can show that, so the cap forces a real
//! read, and if that read returns a different answer from the kept one while
//! the epoch has not moved, something changed that no event announced: the
//! watch is *suspect*. If no event arrives within [`SUSPECT_GRACE`] it is
//! *convicted*: the kept read is discarded and every poll reads from then on,
//! until the watch is re-registered (a new `PollSkip`). "A later read differs
//! and the epoch moved" is not evidence that the stream delivers (one unrelated
//! event satisfies it), so nothing short of re-registration lifts a conviction.
//!
//! Only the DETERMINISTIC part of two reads is compared: paths and status
//! codes. Line counts, warnings and anything else a budget or a timeout can
//! change between two reads of an unchanged tree are not evidence of a missed
//! event (an untracked-file scan that stops at its file budget counts
//! differently when the cache is warm), and comparing them convicted a healthy
//! watch.

use crate::engine::git_reader::FileStatus;
use std::path::Path;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::time::{Duration, Instant};

/// A kept read older than this is not served. A silently dead stream (or a
/// global git setting no worktree watch can see) leaves the answer stale for at
/// most this long; the real read at the cap is also what convicts it.
pub(crate) const KEPT_STATUS_MAX_AGE: Duration = Duration::from_secs(60);
/// How long a suspect watch is given for an event to arrive before it is
/// convicted. The stream's own latency is milliseconds.
pub(crate) const SUSPECT_GRACE: Duration = Duration::from_secs(3);
/// A larger answer is not kept (and so never skipped): it bounds what a watch
/// holds, and a worktree that busy is rarely idle.
pub(crate) const MAX_KEPT_ROWS: usize = 4096;

/// The directory a watch was registered on, by identity (device, inode): a
/// root that moved or was replaced sends no event. Only unix has one; the
/// non-recursive platforms never get this far.
type RootId = (u64, u64);

#[cfg(unix)]
fn root_id(path: &Path) -> Option<RootId> {
    use std::os::unix::fs::MetadataExt;
    let metadata = std::fs::metadata(path).ok()?;
    Some((metadata.dev(), metadata.ino()))
}

#[cfg(not(unix))]
fn root_id(_path: &Path) -> Option<RootId> {
    None
}

/// Why a watch cannot vouch for a worktree at all.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Why {
    /// A bare repository: there is no worktree root to watch.
    NoRoot,
    /// The root watch is not recursive (Linux and Windows).
    NotRecursive,
    /// The watch was shut down, parked or evicted.
    Ended,
    /// The platform gave no identity for the root at registration (no inode).
    NoIdentity,
    /// The directory at the root is no longer the one that was registered.
    RootMoved,
}

/// What one poll decision came to.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Outcome {
    /// Answered from the kept read; no git process was spawned.
    Skipped,
    /// Read because an event arrived since the kept read began, or nothing was
    /// kept yet.
    ReadAfterEvents,
    /// Read because the kept read reached the age cap.
    ReadAged,
    /// Read because the watch is suspect (a cap read disagreed; see above).
    ReadSuspect,
    /// Read because the watch is convicted, until it is re-registered.
    ReadConvicted,
    /// Read because this watch cannot vouch for the worktree.
    Unavailable(Why),
}

const OUTCOMES: usize = 10;

impl Outcome {
    const fn slot(self) -> usize {
        match self {
            Outcome::Skipped => 0,
            Outcome::ReadAfterEvents => 1,
            Outcome::ReadAged => 2,
            Outcome::ReadSuspect => 3,
            Outcome::ReadConvicted => 4,
            Outcome::Unavailable(Why::NoRoot) => 5,
            Outcome::Unavailable(Why::NotRecursive) => 6,
            Outcome::Unavailable(Why::Ended) => 7,
            Outcome::Unavailable(Why::NoIdentity) => 8,
            Outcome::Unavailable(Why::RootMoved) => 9,
        }
    }
}

/// Poll decisions by outcome. One per watch (its own log text) and one for the
/// process (the line [`summary`] feeds to the existing periodic gate log).
struct Counts([AtomicU64; OUTCOMES]);

impl Counts {
    const fn new() -> Self {
        Self([const { AtomicU64::new(0) }; OUTCOMES])
    }

    fn add(&self, outcome: Outcome) {
        self.0[outcome.slot()].fetch_add(1, Ordering::Relaxed);
    }

    fn get(&self, outcome: Outcome) -> u64 {
        self.0[outcome.slot()].load(Ordering::Relaxed)
    }

    /// One line that tells the four states apart: `skipped` above zero is a
    /// skip that is alive and firing; `skipped=0` with `read_after_events`
    /// climbing is a skip that is alive but whose events never stop;
    /// `read_convicted` is a conviction; any `unavailable_*` names why this
    /// watch never could.
    fn line(&self) -> String {
        format!(
            "[poll-skip] skipped={} read_after_events={} read_aged={} read_suspect={} read_convicted={} unavailable_no_root={} unavailable_not_recursive={} unavailable_ended={} unavailable_no_identity={} unavailable_root_moved={}",
            self.get(Outcome::Skipped),
            self.get(Outcome::ReadAfterEvents),
            self.get(Outcome::ReadAged),
            self.get(Outcome::ReadSuspect),
            self.get(Outcome::ReadConvicted),
            self.get(Outcome::Unavailable(Why::NoRoot)),
            self.get(Outcome::Unavailable(Why::NotRecursive)),
            self.get(Outcome::Unavailable(Why::Ended)),
            self.get(Outcome::Unavailable(Why::NoIdentity)),
            self.get(Outcome::Unavailable(Why::RootMoved)),
        )
    }
}

static TOTALS: Counts = Counts::new();

/// The process-wide counts since start.
pub(crate) fn summary() -> String {
    TOTALS.line()
}

/// Remembers the last line it let through, so reading the diagnostics twice
/// with nothing new in between records the counters once.
#[derive(Default)]
pub(crate) struct Changed(Mutex<Option<String>>);

impl Changed {
    /// True when `line` differs from the last one let through.
    pub(crate) fn fresh(&self, line: &str) -> bool {
        let mut last = self.0.lock().unwrap_or_else(PoisonError::into_inner);
        if last.as_deref() == Some(line) {
            return false;
        }
        *last = Some(line.to_string());
        true
    }
}

static LAST_RECORDED: Changed = Changed(Mutex::new(None));

/// The process-wide counters as one line, for the diagnostics export, once per
/// change. `None` when no poll has been decided yet (an untouched process
/// logs nothing, so an empty log still reads as empty) or nothing changed.
pub(crate) fn summary_if_changed() -> Option<String> {
    let line = summary();
    if TOTALS.0.iter().all(|c| c.load(Ordering::Relaxed) == 0) {
        return None;
    }
    LAST_RECORDED.fresh(&line).then_some(line)
}

/// The part of a status row that is a function of the tree alone.
fn deterministic(rows: &[FileStatus]) -> Vec<(&str, Option<&str>, &str, bool, bool)> {
    rows.iter()
        .map(|row| {
            (
                row.path.as_str(),
                row.old_path.as_deref(),
                row.status_code.as_str(),
                row.is_staged,
                row.is_conflicted,
            )
        })
        .collect()
}

struct Kept {
    epoch: u64,
    at: Instant,
    rows: Arc<Vec<FileStatus>>,
}

struct Suspicion {
    epoch: u64,
    since: Instant,
}

#[derive(Default)]
struct Trust {
    kept: Option<Kept>,
    suspect: Option<Suspicion>,
    convicted: bool,
}

/// What a poll decision hands the caller.
pub(crate) struct Begin {
    /// The kept answer, when the poll may be answered without a read.
    pub(crate) served: Option<Arc<Vec<FileStatus>>>,
    /// The epoch a real read is kept under, `None` when nothing may be kept.
    pub(crate) keep_under: Option<u64>,
}

pub(crate) struct PollSkip {
    /// The root watch is recursive. Where it is not, nothing can show a
    /// worktree unchanged and the poll always reads.
    recursive: bool,
    has_root: bool,
    epoch: AtomicU64,
    ended: AtomicBool,
    root: std::path::PathBuf,
    root_id: Option<RootId>,
    trust: Mutex<Trust>,
    /// An answer too large to keep was reported once; see [`MAX_KEPT_ROWS`].
    oversize_reported: AtomicBool,
    max_age_ms: AtomicU64,
    grace_ms: AtomicU64,
    counts: Counts,
}

impl PollSkip {
    pub(crate) fn new(root: Option<&Path>, recursive: bool) -> Self {
        Self {
            recursive,
            has_root: root.is_some(),
            epoch: AtomicU64::new(0),
            ended: AtomicBool::new(false),
            root: root.map(Path::to_path_buf).unwrap_or_default(),
            root_id: root.and_then(root_id),
            trust: Mutex::new(Trust::default()),
            oversize_reported: AtomicBool::new(false),
            max_age_ms: AtomicU64::new(KEPT_STATUS_MAX_AGE.as_millis() as u64),
            grace_ms: AtomicU64::new(SUSPECT_GRACE.as_millis() as u64),
            counts: Counts::new(),
        }
    }

    /// The callback ran: SOMETHING happened (or was lost). Called for every
    /// invocation, before any filtering.
    pub(crate) fn event(&self) {
        self.epoch.fetch_add(1, Ordering::AcqRel);
    }

    /// The watch is over; nothing it said before may be relied on after.
    pub(crate) fn end(&self) {
        self.ended.store(true, Ordering::Release);
    }

    pub(crate) fn epoch_now(&self) -> u64 {
        self.epoch.load(Ordering::Acquire)
    }

    fn lock(&self) -> MutexGuard<'_, Trust> {
        self.trust.lock().unwrap_or_else(PoisonError::into_inner)
    }

    #[cfg(all(test, unix))]
    pub(crate) fn set_timings(&self, max_age: Duration, grace: Duration) {
        self.max_age_ms
            .store(max_age.as_millis() as u64, Ordering::Release);
        self.grace_ms
            .store(grace.as_millis() as u64, Ordering::Release);
    }

    #[cfg(all(test, unix))]
    pub(crate) fn is_convicted(&self) -> bool {
        self.lock().convicted
    }

    /// Why this watch cannot vouch for the worktree, if it cannot.
    fn unavailable(&self) -> Option<Why> {
        if !self.has_root {
            Some(Why::NoRoot)
        } else if !self.recursive {
            Some(Why::NotRecursive)
        } else if self.ended.load(Ordering::Acquire) {
            Some(Why::Ended)
        } else if self.root_id.is_none() {
            Some(Why::NoIdentity)
        } else if root_id(&self.root) != self.root_id {
            Some(Why::RootMoved)
        } else {
            None
        }
    }

    /// The epoch a read starting now is kept under, or why the watch vouches
    /// for nothing and the caller must read and keep nothing.
    fn cover(&self, now: Instant) -> Result<u64, Outcome> {
        if let Some(why) = self.unavailable() {
            return Err(Outcome::Unavailable(why));
        }
        let mut trust = self.lock();
        if trust.convicted {
            return Err(Outcome::ReadConvicted);
        }
        let epoch = self.epoch_now();
        if let Some(suspicion) = &trust.suspect {
            if suspicion.epoch != epoch {
                // An event arrived: the stream delivers.
                trust.suspect = None;
            } else if now.saturating_duration_since(suspicion.since)
                >= Duration::from_millis(self.grace_ms.load(Ordering::Acquire))
            {
                trust.convicted = true;
                trust.suspect = None;
                trust.kept = None;
                log::warn!(
                    target: "watcher",
                    "{}: a status read returned a different answer from the previous one although the watch saw no event; the status poll now reads every time for this worktree until the watch is re-registered (the cause may be a change no worktree watch can see, such as a global git setting, rather than a lost event); poll decisions so far: {}",
                    self.root.display(),
                    self.counts.line()
                );
                return Err(Outcome::ReadConvicted);
            }
        }
        Ok(epoch)
    }

    #[cfg(all(test, unix))]
    pub(crate) fn cover_at(&self, now: Instant) -> Option<u64> {
        self.cover(now).ok()
    }

    /// The kept read, if it was taken at `epoch`, no event has arrived since,
    /// and it is younger than the cap. A suspect watch serves nothing.
    fn serve_kept(&self, epoch: u64, now: Instant) -> Result<Arc<Vec<FileStatus>>, Outcome> {
        let trust = self.lock();
        if trust.convicted {
            return Err(Outcome::ReadConvicted);
        }
        if trust.suspect.is_some() {
            return Err(Outcome::ReadSuspect);
        }
        let Some(kept) = trust.kept.as_ref() else {
            return Err(Outcome::ReadAfterEvents);
        };
        // The live epoch is read here, under the lock and after the decision to
        // serve was taken: an event that landed in between must not be covered.
        if kept.epoch != epoch || self.epoch_now() != epoch {
            return Err(Outcome::ReadAfterEvents);
        }
        let max_age = Duration::from_millis(self.max_age_ms.load(Ordering::Acquire));
        if now.saturating_duration_since(kept.at) >= max_age {
            return Err(Outcome::ReadAged);
        }
        Ok(Arc::clone(&kept.rows))
    }

    #[cfg(all(test, unix))]
    pub(crate) fn serve(&self, epoch: u64, now: Instant) -> Option<Arc<Vec<FileStatus>>> {
        self.serve_kept(epoch, now).ok()
    }

    /// One poll decision. `may_reuse` is false for a refresh that followed an
    /// event, which is a read by definition and is not counted as a poll.
    pub(crate) fn begin(&self, now: Instant, may_reuse: bool) -> Begin {
        let covered = self.cover(now);
        let mut served = None;
        if may_reuse {
            let outcome = match covered.and_then(|epoch| self.serve_kept(epoch, now)) {
                Ok(rows) => {
                    served = Some(rows);
                    Outcome::Skipped
                }
                Err(outcome) => outcome,
            };
            self.count(outcome);
        }
        Begin {
            served,
            keep_under: covered.ok(),
        }
    }

    fn count(&self, outcome: Outcome) {
        self.counts.add(outcome);
        TOTALS.add(outcome);
    }

    #[cfg(all(test, unix))]
    pub(crate) fn counted(&self, outcome: Outcome) -> u64 {
        self.counts.get(outcome)
    }

    /// Records the answer of a real read that began when `begin` said
    /// `epoch`. A read that disagrees with the kept one while the epoch has not
    /// moved makes the watch suspect.
    pub(crate) fn settle(&self, epoch: u64, rows: &[FileStatus], now: Instant) {
        let mut trust = self.lock();
        if let Some(previous) = &trust.kept {
            if previous.epoch == epoch
                && self.epoch_now() == epoch
                && trust.suspect.is_none()
                && deterministic(&previous.rows) != deterministic(rows)
            {
                trust.suspect = Some(Suspicion { epoch, since: now });
            }
        }
        if rows.len() > MAX_KEPT_ROWS && !self.oversize_reported.swap(true, Ordering::AcqRel) {
            log::info!(
                target: "watcher",
                "{}: a status answer of {} rows is too large to keep; the status poll reads every time for this worktree while it stays that large; poll decisions so far: {}",
                self.root.display(),
                rows.len(),
                self.counts.line()
            );
        }
        trust.kept = (rows.len() <= MAX_KEPT_ROWS).then(|| Kept {
            epoch,
            at: now,
            rows: Arc::new(rows.to_vec()),
        });
    }
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;

    fn row(path: &str) -> FileStatus {
        FileStatus {
            path: path.to_string(),
            old_path: None,
            status_code: " M".to_string(),
            is_staged: false,
            is_conflicted: false,
            additions: 1,
            deletions: 0,
            staged_additions: None,
            staged_deletions: None,
            unstaged_additions: None,
            unstaged_deletions: None,
            warnings: Vec::new(),
        }
    }

    fn armed() -> (tempfile::TempDir, PollSkip) {
        let temp = tempfile::TempDir::new().unwrap();
        let root = temp.path().canonicalize().unwrap();
        let skip = PollSkip::new(Some(&root), true);
        (temp, skip)
    }

    /// One read at `now`, kept; returns the epoch it was kept under.
    fn read_and_keep(skip: &PollSkip, rows: &[FileStatus], now: Instant) -> u64 {
        let epoch = skip.cover_at(now).expect("armed");
        skip.settle(epoch, rows, now);
        epoch
    }

    #[test]
    fn a_quiet_watch_serves_the_kept_read_and_any_event_ends_it() {
        let (_temp, skip) = armed();
        let t0 = Instant::now();
        let epoch = read_and_keep(&skip, &[row("a.txt")], t0);
        let served = skip.serve(epoch, t0).expect("nothing happened");
        assert_eq!(served[0].path, "a.txt");
        skip.event();
        let next = skip.cover_at(t0).unwrap();
        assert!(
            skip.serve(next, t0).is_none(),
            "an event ends the kept read"
        );
    }

    /// The race the epoch exists for: an event that lands while a read runs
    /// must not be covered by that read.
    #[test]
    fn a_read_in_flight_when_an_event_arrives_does_not_cover_it() {
        let (_temp, skip) = armed();
        let t0 = Instant::now();
        let epoch = skip.cover_at(t0).unwrap();
        skip.event();
        skip.settle(epoch, &[row("a.txt")], t0);
        let next = skip.cover_at(t0).unwrap();
        assert_ne!(next, epoch);
        assert!(skip.serve(next, t0).is_none());
    }

    #[test]
    fn a_kept_read_older_than_the_cap_is_not_served() {
        let (_temp, skip) = armed();
        let t0 = Instant::now();
        let epoch = read_and_keep(&skip, &[row("a.txt")], t0);
        assert!(skip
            .serve(epoch, t0 + KEPT_STATUS_MAX_AGE - Duration::from_millis(1))
            .is_some());
        assert!(skip.serve(epoch, t0 + KEPT_STATUS_MAX_AGE).is_none());
    }

    /// Unknown is never clean: a watch that has ended vouches for nothing.
    #[test]
    fn an_ended_watch_vouches_for_nothing() {
        let (_temp, ended) = armed();
        let now = Instant::now();
        assert!(ended.cover_at(now).is_some());
        ended.end();
        assert_eq!(ended.cover_at(now), None);
        assert_eq!(ended.begin(now, true).keep_under, None);
        assert_eq!(ended.counted(Outcome::Unavailable(Why::Ended)), 1);
    }

    /// The live-epoch guard in `serve`, which no test pinned (deleting it left the suite green),
    /// was untested: the decision to serve is taken on the epoch `cover_at`
    /// returned, and an event can land between that and the serve. Without
    /// the live-epoch comparison the kept read, whose epoch still equals the
    /// stale number, would be served over the event.
    #[test]
    fn an_event_between_the_decision_and_the_serve_is_not_covered() {
        let (_temp, skip) = armed();
        let t0 = Instant::now();
        let epoch = read_and_keep(&skip, &[row("a.txt")], t0);
        let decided = skip.cover_at(t0).expect("armed");
        assert_eq!(
            decided, epoch,
            "nothing happened yet: the decision is to serve"
        );
        skip.event();
        assert!(
            skip.serve(decided, t0).is_none(),
            "an event landed after the decision; the kept read must not cover it"
        );
    }

    /// The counters tell the four states apart.
    #[test]
    fn the_counters_tell_firing_from_event_bound_from_convicted() {
        let (_temp, skip) = armed();
        let t0 = Instant::now();
        // First poll: nothing kept, a real read.
        let first = skip.begin(t0, true);
        assert!(first.served.is_none());
        skip.settle(first.keep_under.unwrap(), &[row("a.txt")], t0);
        assert_eq!(skip.counted(Outcome::ReadAfterEvents), 1);
        // Alive and firing.
        assert!(skip.begin(t0, true).served.is_some());
        assert!(skip.begin(t0, true).served.is_some());
        assert_eq!(skip.counted(Outcome::Skipped), 2);
        // Events keep arriving: every poll reads.
        for _ in 0..3 {
            skip.event();
            let begun = skip.begin(t0, true);
            assert!(begun.served.is_none());
            skip.settle(begun.keep_under.unwrap(), &[row("a.txt")], t0);
        }
        assert_eq!(skip.counted(Outcome::ReadAfterEvents), 4);
        assert_eq!(skip.counted(Outcome::Skipped), 2);
        // The age cap.
        let aged = skip.begin(t0 + KEPT_STATUS_MAX_AGE, true);
        assert!(aged.served.is_none());
        assert_eq!(skip.counted(Outcome::ReadAged), 1);
        // A refresh that followed an event is a read, not a poll decision.
        let before = skip
            .counts
            .0
            .iter()
            .map(|c| c.load(Ordering::Relaxed))
            .sum::<u64>();
        skip.begin(t0, false);
        let after = skip
            .counts
            .0
            .iter()
            .map(|c| c.load(Ordering::Relaxed))
            .sum::<u64>();
        assert_eq!(before, after, "may_reuse=false is not counted");
        // Suspect, then convicted.
        let (_temp, skip) = armed();
        convict(&skip, t0);
        skip.begin(t0 + KEPT_STATUS_MAX_AGE + SUSPECT_GRACE, true);
        assert!(skip.counted(Outcome::ReadConvicted) >= 1);
        let line = skip.counts.line();
        assert!(
            line.contains("read_convicted=") && !line.contains("read_convicted=0"),
            "{line}"
        );
    }

    /// A suspect watch says so in the counters before it is convicted.
    #[test]
    fn a_suspect_watch_counts_as_suspect() {
        let (_temp, skip) = armed();
        let t0 = Instant::now();
        let epoch = read_and_keep(&skip, &[row("a.txt")], t0);
        skip.settle(epoch, &[row("b.txt")], t0);
        skip.begin(t0, true);
        assert_eq!(skip.counted(Outcome::ReadSuspect), 1);
    }

    /// A root that moved away sends no event; its identity is the witness.
    #[cfg(unix)]
    #[test]
    fn a_root_that_moved_or_was_replaced_vouches_for_nothing() {
        let temp = tempfile::TempDir::new().unwrap();
        let base = temp.path().canonicalize().unwrap();
        let root = base.join("repo");
        std::fs::create_dir(&root).unwrap();
        let skip = PollSkip::new(Some(&root), true);
        let now = Instant::now();
        assert!(skip.cover_at(now).is_some());
        std::fs::rename(&root, base.join("moved")).unwrap();
        assert_eq!(skip.cover_at(now), None, "the root is gone");
        std::fs::create_dir(&root).unwrap();
        assert_eq!(
            skip.cover_at(now),
            None,
            "a different directory stands there"
        );
    }

    #[test]
    fn a_read_over_the_row_cap_is_not_kept() {
        let (_temp, skip) = armed();
        let t0 = Instant::now();
        let rows: Vec<FileStatus> = (0..=MAX_KEPT_ROWS).map(|i| row(&format!("f{i}"))).collect();
        let epoch = read_and_keep(&skip, &rows, t0);
        assert!(skip.serve(epoch, t0).is_none());
    }

    fn convict(skip: &PollSkip, t0: Instant) {
        let epoch = read_and_keep(skip, &[row("a.txt")], t0);
        // A real read at the cap disagrees, and no event arrives.
        skip.settle(
            epoch,
            &[row("a.txt"), row("b.txt")],
            t0 + KEPT_STATUS_MAX_AGE,
        );
        assert!(!skip.is_convicted(), "suspect, not yet convicted");
        assert!(
            skip.serve(epoch, t0).is_none(),
            "a suspect watch serves nothing"
        );
        assert_eq!(
            skip.cover_at(t0 + KEPT_STATUS_MAX_AGE + SUSPECT_GRACE),
            None
        );
        assert!(skip.is_convicted());
    }

    #[test]
    fn a_silent_stream_is_convicted_when_a_read_disagrees_and_no_event_follows() {
        let (_temp, skip) = armed();
        convict(&skip, Instant::now());
    }

    /// A conviction is final for this registration: one unrelated event and a
    /// changed answer satisfy "the epoch moved and the read differs" on a
    /// stream that is still dead.
    #[test]
    fn a_conviction_does_not_lift_on_an_unrelated_event_or_a_changed_answer() {
        let (_temp, skip) = armed();
        let t0 = Instant::now();
        convict(&skip, t0);
        skip.event();
        let later = t0 + KEPT_STATUS_MAX_AGE * 2;
        assert_eq!(skip.cover_at(later), None);
        skip.settle(skip.epoch_now(), &[row("z.txt")], later);
        assert_eq!(skip.cover_at(later + SUSPECT_GRACE), None);
        assert!(skip.is_convicted());
    }

    /// An event inside the grace period shows the stream delivers.
    #[test]
    fn an_event_during_the_grace_period_clears_the_suspicion() {
        let (_temp, skip) = armed();
        let t0 = Instant::now();
        let epoch = read_and_keep(&skip, &[row("a.txt")], t0);
        skip.settle(epoch, &[row("b.txt")], t0);
        skip.event();
        assert!(skip.cover_at(t0 + SUSPECT_GRACE * 2).is_some());
        assert!(!skip.is_convicted());
    }

    /// Finding C: only paths and status codes are evidence. An untracked-file
    /// scan that stops at its budget reports different line counts and
    /// warnings for an unchanged tree; that convicted a healthy watch.
    #[test]
    fn line_counts_and_warnings_are_not_evidence_of_a_missed_event() {
        let (_temp, skip) = armed();
        let t0 = Instant::now();
        let epoch = read_and_keep(&skip, &[row("a.txt")], t0);
        let mut again = row("a.txt");
        again.additions = 99;
        again.unstaged_additions = Some(99);
        again.warnings.push("untracked scan budget spent".into());
        skip.settle(epoch, &[again], t0 + KEPT_STATUS_MAX_AGE);
        assert!(skip
            .cover_at(t0 + KEPT_STATUS_MAX_AGE + SUSPECT_GRACE * 5)
            .is_some());
        assert!(!skip.is_convicted());
    }
}

/// The cases that need no root identity run everywhere; they are the whole
/// story on Linux and Windows, where the root watch is not recursive.
#[cfg(test)]
mod portable_tests {
    use super::*;

    fn root() -> (tempfile::TempDir, std::path::PathBuf) {
        let temp = tempfile::TempDir::new().unwrap();
        let root = temp.path().canonicalize().unwrap();
        (temp, root)
    }

    /// Where the skip is unavailable the poll reads every time and says why.
    #[test]
    fn a_non_recursive_watch_reads_every_poll_and_says_so() {
        let (_temp, root) = root();
        let skip = PollSkip::new(Some(&root), false);
        let now = Instant::now();
        for _ in 0..2 {
            let begun = skip.begin(now, true);
            assert!(begun.served.is_none() && begun.keep_under.is_none());
        }
        assert_eq!(skip.counts.get(Outcome::Unavailable(Why::NotRecursive)), 2);
        assert_eq!(skip.counts.get(Outcome::Skipped), 0);
        let line = skip.counts.line();
        assert!(line.contains("unavailable_not_recursive=2"), "{line}");
        assert!(line.contains("skipped=0"), "{line}");
    }

    #[test]
    fn a_watch_with_no_worktree_root_reads_every_poll_and_says_so() {
        let skip = PollSkip::new(None, true);
        let now = Instant::now();
        assert!(skip.begin(now, true).keep_under.is_none());
        assert_eq!(skip.counts.get(Outcome::Unavailable(Why::NoRoot)), 1);
    }

    /// Reading the diagnostics twice with nothing new records the counters once.
    #[test]
    fn the_counters_are_recorded_once_per_change() {
        let seen = Changed::default();
        assert!(seen.fresh("a"));
        assert!(!seen.fresh("a"));
        assert!(seen.fresh("b"));
    }

    /// The process-wide line is the same text over the process totals.
    #[test]
    fn the_summary_line_names_every_state() {
        let line = summary();
        for key in [
            "skipped=",
            "read_after_events=",
            "read_aged=",
            "read_suspect=",
            "read_convicted=",
            "unavailable_no_root=",
            "unavailable_not_recursive=",
            "unavailable_ended=",
            "unavailable_no_identity=",
            "unavailable_root_moved=",
        ] {
            assert!(line.contains(key), "{line}");
        }
        assert!(line.starts_with("[poll-skip] "));
    }
}

/// Where the platform gives no root identity (Windows) a recursive watch still
/// cannot vouch: the poll reads every time. Compiled only there, so it is
/// checked for Windows by clippy from the Mac and run by Windows CI.
#[cfg(all(test, not(unix)))]
mod non_unix_tests {
    use super::*;

    #[test]
    fn without_a_root_identity_every_poll_reads_and_says_why() {
        let temp = tempfile::TempDir::new().unwrap();
        let root = temp.path().canonicalize().unwrap();
        let skip = PollSkip::new(Some(&root), true);
        let now = Instant::now();
        assert!(skip.cover(now).is_err());
        for _ in 0..2 {
            assert!(skip.begin(now, true).served.is_none());
        }
        assert_eq!(skip.counts.get(Outcome::Unavailable(Why::NoIdentity)), 2);
    }
}
