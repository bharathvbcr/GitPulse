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
//! # Unknown is never clean
//!
//! [`PollSkip::cover_at`] says nothing (the caller reads, as it always did)
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

pub(crate) struct PollSkip {
    /// The root watch is recursive. Where it is not, nothing can show a
    /// worktree unchanged and the poll always reads.
    recursive: bool,
    epoch: AtomicU64,
    ended: AtomicBool,
    root: std::path::PathBuf,
    root_id: Option<RootId>,
    trust: Mutex<Trust>,
    /// An answer too large to keep was reported once; see [`MAX_KEPT_ROWS`].
    oversize_reported: AtomicBool,
    max_age_ms: AtomicU64,
    grace_ms: AtomicU64,
}

impl PollSkip {
    pub(crate) fn new(root: Option<&Path>, recursive: bool) -> Self {
        Self {
            recursive: recursive && root.is_some(),
            epoch: AtomicU64::new(0),
            ended: AtomicBool::new(false),
            root: root.map(Path::to_path_buf).unwrap_or_default(),
            root_id: root.and_then(root_id),
            trust: Mutex::new(Trust::default()),
            oversize_reported: AtomicBool::new(false),
            max_age_ms: AtomicU64::new(KEPT_STATUS_MAX_AGE.as_millis() as u64),
            grace_ms: AtomicU64::new(SUSPECT_GRACE.as_millis() as u64),
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

    #[cfg(test)]
    pub(crate) fn set_timings(&self, max_age: Duration, grace: Duration) {
        self.max_age_ms
            .store(max_age.as_millis() as u64, Ordering::Release);
        self.grace_ms
            .store(grace.as_millis() as u64, Ordering::Release);
    }

    #[cfg(test)]
    pub(crate) fn is_convicted(&self) -> bool {
        self.lock().convicted
    }

    fn available(&self) -> bool {
        self.recursive
            && !self.ended.load(Ordering::Acquire)
            && self.root_id.is_some()
            && root_id(&self.root) == self.root_id
    }

    /// The epoch a read starting now is kept under, or `None` when the watch
    /// vouches for nothing and the caller must read and keep nothing.
    pub(crate) fn cover_at(&self, now: Instant) -> Option<u64> {
        if !self.available() {
            return None;
        }
        let mut trust = self.lock();
        if trust.convicted {
            return None;
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
                    "{}: a status read returned a different answer from the previous one although the watch saw no event; the status poll now reads every time for this worktree until the watch is re-registered (the cause may be a change no worktree watch can see, such as a global git setting, rather than a lost event)",
                    self.root.display()
                );
                return None;
            }
        }
        Some(epoch)
    }

    /// The kept read, if it was taken at `epoch`, no event has arrived since,
    /// and it is younger than the cap. A suspect watch serves nothing.
    pub(crate) fn serve(&self, epoch: u64, now: Instant) -> Option<Arc<Vec<FileStatus>>> {
        let trust = self.lock();
        if trust.suspect.is_some() || trust.convicted {
            return None;
        }
        let kept = trust.kept.as_ref()?;
        let max_age = Duration::from_millis(self.max_age_ms.load(Ordering::Acquire));
        (kept.epoch == epoch
            && self.epoch_now() == epoch
            && now.saturating_duration_since(kept.at) < max_age)
            .then(|| Arc::clone(&kept.rows))
    }

    /// Records the answer of a real read that began when `cover_at` said
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
                "{}: a status answer of {} rows is too large to keep; the status poll reads every time for this worktree while it stays that large",
                self.root.display(),
                rows.len()
            );
        }
        trust.kept = (rows.len() <= MAX_KEPT_ROWS).then(|| Kept {
            epoch,
            at: now,
            rows: Arc::new(rows.to_vec()),
        });
    }
}

#[cfg(test)]
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
        assert!(skip.serve(next, t0).is_none(), "an event ends the kept read");
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

    /// Unknown is never clean, one case each.
    #[test]
    fn an_unarmed_watch_vouches_for_nothing() {
        let temp = tempfile::TempDir::new().unwrap();
        let root = temp.path().canonicalize().unwrap();
        let now = Instant::now();
        // Linux and Windows: the root watch is not recursive.
        assert_eq!(PollSkip::new(Some(&root), false).cover_at(now), None);
        // No worktree root (a bare repository).
        assert_eq!(PollSkip::new(None, true).cover_at(now), None);
        // A watch that has ended.
        let ended = PollSkip::new(Some(&root), true);
        assert!(ended.cover_at(now).is_some());
        ended.end();
        assert_eq!(ended.cover_at(now), None);
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
        assert_eq!(skip.cover_at(now), None, "a different directory stands there");
    }

    #[test]
    fn a_read_over_the_row_cap_is_not_kept() {
        let (_temp, skip) = armed();
        let t0 = Instant::now();
        let rows: Vec<FileStatus> = (0..=MAX_KEPT_ROWS)
            .map(|i| row(&format!("f{i}")))
            .collect();
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
