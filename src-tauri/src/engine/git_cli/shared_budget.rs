//! A spawn-rate budget every GitPulse process of one user draws from.
//!
//! The app, every agent session's `gitpulse-mcp`, `gitpulsed` and the hooks
//! all run git through this engine, and each had a token bucket of its own —
//! so N processes could start N times the rate the gate exists to cap. This
//! bucket is a small record in the per-user config directory, read and
//! rewritten under an exclusive advisory lock. A process keeps its own bucket
//! as well, and a spawn needs a token from both, so the shared record can only
//! tighten the per-process limit, never loosen it. Only those shipped
//! binaries open it, each by opting in at the top of `main`
//! (`run_process_with_shared_spawn_budget`); a test or bench process that
//! links the same engine keeps to its own bucket.
//!
//! A record it cannot reach fails open, and says so, in exactly two cases
//! beyond one that cannot be opened safely (which leaves the process on its
//! own bucket with a logged reason). A lock another process or thread holds
//! past [`LOCK_PATIENCE`] lets this one decision fall back to the process
//! bucket, counted. Failing closed there would let one stopped process — a
//! debugger, a `SIGSTOP` — defer every read in every other GitPulse process.
//! And a record of a newer layout is its writer's: this one leaves it
//! untouched and falls back for the decision as for a held lock. Only a whole
//! [`RECORD_LEN`]-byte record can be a newer layout. (A grant whose write then
//! fails is the record unreachable for that decision and falls back likewise;
//! the process bucket still caps this process.)
//!
//! Everything else fails closed. A record it reaches but cannot decode —
//! torn at any length, a newer version word on a short buffer included,
//! foreign or of another layout — is rewritten as an EMPTY budget that
//! refills at the rate from now: it grants nothing, because a reset to a full
//! burst is a fresh burst for every process that meets it — which is how
//! layout 1, resetting to full, handed out 192-spawn bursts over a hundred
//! times in three hours. The warning names the bytes and the writer pid they
//! carry. And a denial stays a denial when the record cannot be rewritten
//! ([`Take::DeniedUnrecorded`]): the decision came from a record that was
//! reached, so a failed write must not turn it into an admit.
//!
//! The layout is versioned twice, in the header and in the file name, and the
//! name moves with the layout. Binaries of two layouts coexist until every
//! install is refreshed; sharing one record, each would read the other's as
//! undecodable and their resets would either free both fleets (a reset to
//! full) or starve one (a reset to empty). Two names keep them apart.
//!
//! Every thread of a process shares one handle, so one open file description,
//! and an advisory lock does not exclude within that: a second thread's
//! `try_lock` succeeds while the first is mid-update. The record is therefore
//! also held behind an in-process mutex, and every seek, read and write goes
//! through its guard. Without it the threads' updates interleaved on the
//! shared offset, tearing the record (the "held 23/25 unreadable byte(s)"
//! warnings) and, worse, reading zero bytes past the end, which looks like a
//! record never written.

use std::fs::{File, TryLockError};
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::sync::{Mutex, MutexGuard, TryLockError as GuardTryLockError};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

const MAGIC: [u8; 4] = *b"GPSB";
/// The record layout; [`FILE_NAME`] carries it too.
const VERSION: u32 = 2;
/// `MAGIC`, `VERSION`, tokens (u32), writer pid (u32), refill anchor (u64 ns).
const RECORD_LEN: usize = 24;
/// How long one decision waits for another process or thread to finish its
/// update. An update is a 24-byte read and write, so anything this long is a
/// holder that is stopped or wedged, not one that is busy.
const LOCK_PATIENCE: Duration = Duration::from_millis(25);
/// Moves with [`VERSION`]: a binary of another layout opens another record.
const FILE_NAME: &str = "spawn-budget.v2";

/// Where every GitPulse process of this user finds the shared record.
///
/// The config directory rather than `temp_dir()`: agent hosts commonly give
/// the processes they start a private `TMPDIR`, which would silently split
/// the app and its agents onto separate budgets again.
pub(super) fn default_path() -> Result<PathBuf, String> {
    crate::tool_config::default_config_dir()
        .map(|dir| dir.join(FILE_NAME))
        .ok_or_else(|| "no per-user config directory (HOME / APPDATA unset)".to_string())
}

pub(super) struct SharedBudget {
    /// The record, reachable only through [`SharedBudget::lock`], which
    /// excludes this process's other threads before taking the advisory lock
    /// that excludes other processes.
    file: Mutex<File>,
    path: PathBuf,
    /// Makes [`SharedBudget::write_state`] fail, as a full disk or a revoked
    /// record would. One handle's, never a global.
    #[cfg(test)]
    fail_writes: std::sync::atomic::AtomicBool,
}

/// What the shared record said about one spawn.
#[derive(Debug, PartialEq)]
pub(super) enum Take {
    /// A token was taken from the shared record.
    Granted,
    /// The shared budget is spent down to the caller's floor.
    Denied,
    /// Spent down to the floor, but the record could not be rewritten to say
    /// so (the reason). Still a denial: the record was reached and answered.
    DeniedUnrecorded(String),
    /// The record could not be consulted for this decision.
    Unavailable(String),
}

#[derive(Debug, Clone, Copy, PartialEq)]
struct State {
    tokens: u32,
    /// Wall-clock nanoseconds the refill has been credited up to. Wall clock
    /// because `Instant` means nothing in another process.
    at_ns: u64,
}

/// What a decoded record says.
#[derive(Debug, PartialEq)]
enum Record {
    /// This layout's record.
    Current(State),
    /// Another build's newer layout, which this one must not rewrite.
    Newer(u32),
    /// Torn, foreign, or an older layout.
    Unreadable,
}

/// The record for one update: the in-process guard, and the advisory lock
/// taken through it. Releases the lock and then the guard on every path out
/// (`drop` runs before the fields are dropped).
struct Locked<'a> {
    file: MutexGuard<'a, File>,
}

impl Drop for Locked<'_> {
    fn drop(&mut self) {
        // Closing the descriptor would release it too; the record stays open
        // for the next decision, so release explicitly. Nothing useful can be
        // done about a failure here, and the next `try_lock` reports it.
        let _ = self.file.unlock();
    }
}

impl SharedBudget {
    /// Opens (creating if absent) the record at `path`, refusing one that is
    /// not a regular file this user alone owns and can read.
    pub(super) fn open(path: &Path) -> Result<Self, String> {
        let file = crate::fs_entry::open_private_record(path)?;
        Ok(Self {
            file: Mutex::new(file),
            path: path.to_path_buf(),
            #[cfg(test)]
            fail_writes: std::sync::atomic::AtomicBool::new(false),
        })
    }

    pub(super) fn path(&self) -> &Path {
        &self.path
    }

    /// Takes one token if more than `floor` remain after refilling at
    /// `per_sec` up to `burst`.
    pub(super) fn take(&self, floor: u32, burst: u32, per_sec: u32) -> Take {
        self.take_at(floor, burst, per_sec, wall_ns())
    }

    fn take_at(&self, floor: u32, burst: u32, per_sec: u32, now_ns: u64) -> Take {
        let locked = match self.lock() {
            Ok(locked) => locked,
            Err(reason) => return Take::Unavailable(reason),
        };
        let mut state = match self.read_state(&locked, burst, now_ns) {
            Ok(state) => state,
            Err(reason) => return Take::Unavailable(reason),
        };
        refill(&mut state, burst, per_sec, now_ns);
        let granted = state.tokens > floor;
        if granted {
            state.tokens -= 1;
        }
        // Written even on a denial: the refill anchor moved. A failed write
        // changes only what the record says afterwards, not this decision: a
        // grant it could not record is the record unreachable (fail open, the
        // process bucket still caps this process), but a denial stays one.
        match (granted, self.write_state(&locked, state)) {
            (true, Ok(())) => Take::Granted,
            (false, Ok(())) => Take::Denied,
            (true, Err(reason)) => Take::Unavailable(reason),
            (false, Err(reason)) => Take::DeniedUnrecorded(reason),
        }
    }

    /// Takes the in-process guard and then the advisory lock, both within one
    /// [`LOCK_PATIENCE`]: a thread wedged inside an update is as bounded for
    /// its peers as a wedged process is.
    fn lock(&self) -> Result<Locked<'_>, String> {
        let started = Instant::now();
        let held = |holder: &str| {
            format!(
                "{holder} held {} for over {} ms",
                self.path.display(),
                LOCK_PATIENCE.as_millis()
            )
        };
        let file = loop {
            match self.file.try_lock() {
                Ok(file) => break file,
                // The state is the record, not the mutex, so a thread that
                // panicked while holding the guard leaves nothing to distrust.
                Err(GuardTryLockError::Poisoned(poisoned)) => break poisoned.into_inner(),
                Err(GuardTryLockError::WouldBlock) => {
                    if started.elapsed() >= LOCK_PATIENCE {
                        return Err(held("another thread of this GitPulse process"));
                    }
                    // The holder is a peer inside a microseconds-long update,
                    // or polling a held advisory lock; a millisecond's sleep
                    // per try would starve a burst of threads into spurious
                    // unavailability.
                    std::thread::sleep(Duration::from_micros(50));
                }
            }
        };
        loop {
            match file.try_lock() {
                Ok(()) => return Ok(Locked { file }),
                Err(TryLockError::WouldBlock) => {
                    if started.elapsed() >= LOCK_PATIENCE {
                        return Err(held("another GitPulse process"));
                    }
                    std::thread::sleep(Duration::from_millis(1));
                }
                Err(TryLockError::Error(e)) => {
                    return Err(format!("could not lock {}: {e}", self.path.display()))
                }
            }
        }
    }

    /// The record, or a full burst when there is none yet. Only this layout
    /// opens this file and every write leaves it [`RECORD_LEN`] long under
    /// both locks, so an empty file is one never written, not one torn.
    ///
    /// A record present but undecodable is rewritten as an empty budget and
    /// logged with its bytes and writer; refusing instead would turn one bad
    /// write into a permanent stop, and a full burst would be a gift. A newer
    /// layout's record is an error, so the decision falls back and nothing is
    /// written over it.
    fn read_state(&self, locked: &Locked<'_>, burst: u32, now_ns: u64) -> Result<State, String> {
        let mut file: &File = &locked.file;
        file.seek(SeekFrom::Start(0))
            .map_err(|e| format!("could not read {}: {e}", self.path.display()))?;
        // One byte more than a record, so a longer file reads as unreadable
        // rather than as a good record with trailing bytes.
        let mut buf = Vec::with_capacity(RECORD_LEN + 1);
        file.take(RECORD_LEN as u64 + 1)
            .read_to_end(&mut buf)
            .map_err(|e| format!("could not read {}: {e}", self.path.display()))?;
        if buf.is_empty() {
            return Ok(State {
                tokens: burst,
                at_ns: now_ns,
            });
        }
        match decode(&buf) {
            Record::Current(state) => Ok(State {
                tokens: state.tokens.min(burst),
                ..state
            }),
            Record::Newer(version) => Err(format!(
                "{} holds a layout-{version} record from a newer GitPulse (this one \
                 reads layout {VERSION}); left for its writer",
                self.path.display()
            )),
            Record::Unreadable => {
                log::warn!(target: "spawn_gate", "{}", unreadable_warning(&self.path, &buf));
                Ok(State {
                    tokens: 0,
                    at_ns: now_ns,
                })
            }
        }
    }

    fn write_state(&self, locked: &Locked<'_>, state: State) -> Result<(), String> {
        #[cfg(test)]
        if self.fail_writes.load(std::sync::atomic::Ordering::Relaxed) {
            return Err(format!(
                "could not write {}: injected failure",
                self.path.display()
            ));
        }
        let mut file: &File = &locked.file;
        stage_update(&mut file, &encode(state), || {
            locked.file.set_len(RECORD_LEN as u64)
        })
        .map_err(|e| format!("could not write {}: {e}", self.path.display()))
    }

    /// Makes every later write through this handle fail (or succeed again).
    #[cfg(test)]
    pub(super) fn fail_writes(&self, fail: bool) {
        self.fail_writes
            .store(fail, std::sync::atomic::Ordering::Relaxed);
    }

    #[cfg(test)]
    fn tokens_now(&self, burst: u32, per_sec: u32, now_ns: u64) -> u32 {
        let locked = self.lock().expect("lock");
        let mut state = self.read_state(&locked, burst, now_ns).expect("read");
        refill(&mut state, burst, per_sec, now_ns);
        state.tokens
    }
}

/// Refills like the process bucket — whole tokens, remainder kept on the
/// anchor — with two clamps the process bucket's monotonic clock never needs.
/// A wall clock stepped backwards re-anchors at `now` and mints nothing; one
/// stepped far forwards, or a record untouched for hours, refills to `burst`
/// and no further.
fn refill(state: &mut State, burst: u32, per_sec: u32, now_ns: u64) {
    if now_ns < state.at_ns {
        state.at_ns = now_ns;
        return;
    }
    let elapsed_ms = u128::from((now_ns - state.at_ns) / 1_000_000);
    let add = elapsed_ms.saturating_mul(u128::from(per_sec.max(1))) / 1000;
    if add == 0 {
        return;
    }
    if add >= u128::from(burst) {
        state.tokens = burst;
        state.at_ns = now_ns;
        return;
    }
    // `add < burst`, so this fits in u32.
    let add = add as u32;
    state.tokens = state.tokens.saturating_add(add).min(burst);
    let consumed_ms = u64::from(add).saturating_mul(1000) / u64::from(per_sec.max(1));
    state.at_ns = state
        .at_ns
        .saturating_add(consumed_ms.saturating_mul(1_000_000))
        .min(now_ns);
}

/// The record as this process writes it, signed with its pid so the reader of
/// a record that later fails to decode can say whose it was.
fn encode(state: State) -> [u8; RECORD_LEN] {
    let mut record = [0u8; RECORD_LEN];
    record[0..4].copy_from_slice(&MAGIC);
    record[4..8].copy_from_slice(&VERSION.to_le_bytes());
    record[8..12].copy_from_slice(&state.tokens.to_le_bytes());
    record[12..16].copy_from_slice(&std::process::id().to_le_bytes());
    record[16..24].copy_from_slice(&state.at_ns.to_le_bytes());
    record
}

/// The writes that put `record` over the previous one, as `(offset, bytes)` in
/// the order they reach the file. The result is the record's bytes exactly, so
/// every layout-2 binary reads it and writes over it as before; only the
/// order is staged.
///
/// One 24-byte `write` can be cut short (a full disk, a signal, a crash), and
/// a prefix of the new bytes over the old ones is a record no writer made:
/// the new token count under the old refill anchor decodes as current and
/// credits time the update had already counted, up to a whole burst. So the
/// first byte of the magic is blanked first and put back last. Until that
/// last single byte lands the record is undecodable, and an undecodable
/// record is read as an empty budget: a cut anywhere costs the next taker its
/// tokens (they refill at the rate), and can never give any.
fn commit_steps(record: &[u8; RECORD_LEN]) -> [(u64, &[u8]); 3] {
    [(0, &[0]), (1, &record[1..]), (0, &record[..1])]
}

/// Puts `record` over the previous one in [`commit_steps`] order, running
/// `truncate` between the body and the byte that makes the record valid. The
/// truncation can fail, so it comes after the record is blanked (a longer file
/// is undecodable already, and cutting it down to a valid looking prefix first
/// would make it decodable) and before that last byte, so nothing can fail
/// once it has landed. `write_all` finishes a short write or one a signal
/// cut; any error leaves the record undecodable or untouched.
fn stage_update<W: Write + Seek>(
    file: &mut W,
    record: &[u8; RECORD_LEN],
    truncate: impl FnOnce() -> std::io::Result<()>,
) -> std::io::Result<()> {
    let [blank, body, commit] = commit_steps(record);
    let mut put = |(at, bytes): (u64, &[u8])| -> std::io::Result<()> {
        file.seek(SeekFrom::Start(at))?;
        file.write_all(bytes)
    };
    put(blank)?;
    put(body)?;
    truncate()?;
    put(commit)
}

/// The little-endian u32 at `at`, when `buf` is long enough to hold it.
fn word_at(buf: &[u8], at: usize) -> Option<u32> {
    let bytes: [u8; 4] = buf.get(at..at.checked_add(4)?)?.try_into().ok()?;
    Some(u32::from_le_bytes(bytes))
}

/// The layout a record claims, when it carries this file's magic at all.
fn claimed_version(buf: &[u8]) -> Option<u32> {
    if buf.get(0..4)? != MAGIC {
        return None;
    }
    word_at(buf, 4)
}

fn decode(buf: &[u8]) -> Record {
    match claimed_version(buf) {
        // Only a whole record can be another layout's: a shorter or longer
        // buffer with a newer version word is torn, and says nothing.
        Some(version) if version > VERSION && buf.len() == RECORD_LEN => Record::Newer(version),
        Some(VERSION) if buf.len() == RECORD_LEN => {
            let (Some(tokens), Some(at_ns)) = (
                word_at(buf, 8),
                buf.get(16..24)
                    .and_then(|bytes| <[u8; 8]>::try_from(bytes).ok())
                    .map(u64::from_le_bytes),
            ) else {
                return Record::Unreadable;
            };
            Record::Current(State { tokens, at_ns })
        }
        _ => Record::Unreadable,
    }
}

/// The warning for an undecodable record: its bytes, the writer they name,
/// and the reader that reset it, so a flip-flop between two writers can be
/// attributed from the logs alone.
fn unreadable_warning(path: &Path, buf: &[u8]) -> String {
    format!(
        "{} held {} unreadable byte(s) [{}] ({}); reset to an empty budget by pid {}, \
         GitPulse {}",
        path.display(),
        buf.len(),
        hex(buf),
        writer_of(buf),
        std::process::id(),
        env!("CARGO_PKG_VERSION")
    )
}

/// Who an undecodable record says wrote it, as far as its bytes still say.
fn writer_of(buf: &[u8]) -> String {
    // An update [`commit_steps`] stages is blank in its first magic byte until
    // it lands whole: that is a cut write, not a foreign file.
    let cut = buf.first() == Some(&0) && buf.get(1..4) == Some(&MAGIC[1..]);
    let version = if cut {
        word_at(buf, 4)
    } else {
        claimed_version(buf)
    };
    let Some(version) = version else {
        return "not a GitPulse spawn-budget record; writer unknown".to_string();
    };
    let cut = if cut {
        "update cut before it committed, "
    } else {
        ""
    };
    match word_at(buf, 12) {
        None => format!("{cut}layout {version}, cut short before the writer pid"),
        // Layout 1 kept that word reserved and always wrote zero.
        Some(0) => format!("{cut}layout {version}, writer pid unrecorded"),
        Some(pid) => format!("{cut}layout {version}, writer pid {pid}"),
    }
}

/// Lowercase hex, two digits a byte. Callers pass at most `RECORD_LEN + 1`.
fn hex(bytes: &[u8]) -> String {
    const DIGITS: &[u8; 16] = b"0123456789abcdef";
    let mut out = String::with_capacity(bytes.len() * 2);
    for &byte in bytes {
        out.push(char::from(DIGITS[usize::from(byte >> 4)]));
        out.push(char::from(DIGITS[usize::from(byte & 0x0f)]));
    }
    out
}

fn wall_ns() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|elapsed| u64::try_from(elapsed.as_nanos()).unwrap_or(u64::MAX))
        // Before 1970: anchor at zero; the backwards-clock clamp handles it.
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    const S: u64 = 1_000_000_000;

    fn budget(dir: &tempfile::TempDir) -> SharedBudget {
        SharedBudget::open(&dir.path().join(FILE_NAME)).expect("open")
    }

    #[test]
    fn two_handles_on_one_record_share_one_burst() {
        let dir = tempfile::tempdir().unwrap();
        // Two opens are two open-file descriptions, which is exactly what two
        // processes hold: the advisory lock contends between them.
        let a = budget(&dir);
        let b = budget(&dir);
        let now = 1_000 * S;
        let mut granted = 0;
        for i in 0..20 {
            let handle = if i % 2 == 0 { &a } else { &b };
            if handle.take_at(0, 6, 1, now) == Take::Granted {
                granted += 1;
            }
        }
        assert_eq!(granted, 6, "one burst between both handles, not one each");
        assert_eq!(a.take_at(0, 6, 1, now + 2 * S), Take::Granted);
        assert_eq!(b.take_at(0, 6, 1, now + 2 * S), Take::Granted);
        assert_eq!(a.take_at(0, 6, 1, now + 2 * S), Take::Denied);
    }

    #[test]
    fn the_floor_is_read_from_the_shared_count() {
        let dir = tempfile::tempdir().unwrap();
        let a = budget(&dir);
        let b = budget(&dir);
        let now = 50 * S;
        for _ in 0..6 {
            assert_eq!(a.take_at(0, 8, 1, now), Take::Granted);
        }
        // Two left: a floor of two (the background reserve) refuses B even
        // though B itself has spent nothing.
        assert_eq!(b.take_at(2, 8, 1, now), Take::Denied);
        assert_eq!(b.take_at(0, 8, 1, now), Take::Granted);
    }

    /// A record in this file's layout but stamped `version`, as a binary of
    /// another layout would leave it, holding `tokens`.
    fn stamped(version: u32, tokens: u32, at_ns: u64) -> Vec<u8> {
        let mut record = vec![0u8; RECORD_LEN];
        record[0..4].copy_from_slice(&MAGIC);
        record[4..8].copy_from_slice(&version.to_le_bytes());
        record[8..12].copy_from_slice(&tokens.to_le_bytes());
        record[16..24].copy_from_slice(&at_ns.to_le_bytes());
        record
    }

    fn write_private(path: &Path, bytes: &[u8]) {
        std::fs::write(path, bytes).unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600)).unwrap();
        }
    }

    /// What a torn or foreign record grants is nothing: an undecodable record
    /// is rewritten as an EMPTY budget that refills at the rate, never a full
    /// burst. A reset to full is what let a torn record hand every GitPulse
    /// process a fresh 192-spawn burst, 138 times in three hours.
    #[test]
    fn an_undecodable_record_resets_to_an_empty_budget() {
        let burst = 192;
        let v1_full = stamped(1, burst, 9 * S);
        let older = stamped(0, burst, 9 * S);
        let mut torn_long = stamped(1, burst, 9 * S);
        torn_long.push(0x47);
        let torn_short = stamped(1, burst, 9 * S)[..RECORD_LEN - 1].to_vec();
        for junk in [
            &b"garbage"[..],
            &[0u8; RECORD_LEN][..],
            &[0xFFu8; RECORD_LEN][..],
            &[0xFFu8; RECORD_LEN + 7][..],
            &torn_long[..],
            &torn_short[..],
            &older[..],
            &v1_full[..],
        ] {
            let dir = tempfile::tempdir().unwrap();
            let path = dir.path().join(FILE_NAME);
            write_private(&path, junk);
            let shared = SharedBudget::open(&path).unwrap();
            assert_eq!(
                shared.take_at(0, burst, 8, 9 * S),
                Take::Denied,
                "{junk:02x?} granted a token"
            );
            assert!(
                shared.tokens_now(burst, 8, 9 * S) <= burst / 4,
                "{junk:02x?} left more than a quarter burst"
            );
            assert_eq!(shared.tokens_now(burst, 8, 9 * S), 0, "{junk:02x?}");
            assert_eq!(std::fs::metadata(&path).unwrap().len(), RECORD_LEN as u64);
            // And it refills at the rate from the reset, like any spent record.
            assert_eq!(shared.take_at(0, burst, 8, 10 * S), Take::Granted);
            assert_eq!(shared.tokens_now(burst, 8, 10 * S), 7);
        }
    }

    /// Another layout's writer rewriting the record between this one's
    /// decisions — the flip-flop the card's runtime evidence describes — can
    /// never lift what this process is granted above one burst plus the rate.
    #[test]
    fn a_flip_flopping_foreign_writer_cannot_mint_past_burst_plus_rate() {
        let dir = tempfile::tempdir().unwrap();
        let shared = budget(&dir);
        let (burst, per_sec) = (16u32, 2u32);
        let start = 1_000 * S;
        let step = S / 10;
        let steps = 600u64;
        let mut granted = 0u64;
        for i in 0..steps {
            let now = start + i * step;
            match i % 3 {
                0 => std::fs::write(shared.path(), stamped(1, burst, now)).unwrap(),
                1 => {
                    let mut torn = stamped(1, burst, now);
                    torn.push(0);
                    std::fs::write(shared.path(), torn).unwrap();
                }
                _ => {}
            }
            for _ in 0..5 {
                if shared.take_at(0, burst, per_sec, now) == Take::Granted {
                    granted += 1;
                }
            }
        }
        let elapsed_s = (steps - 1) * step / S;
        let bound = u64::from(burst) + u64::from(per_sec) * elapsed_s;
        assert!(
            granted <= bound,
            "granted {granted} over {elapsed_s} s; burst + rate * t is {bound}"
        );
    }

    /// The same flip-flop from a writer that does take the advisory lock, the
    /// way another GitPulse process does, running concurrently.
    #[test]
    fn a_concurrent_locking_foreign_writer_cannot_mint_past_burst_plus_rate() {
        let dir = tempfile::tempdir().unwrap();
        let shared = budget(&dir);
        let path = shared.path().to_path_buf();
        let (burst, per_sec) = (16u32, 2u32);
        let start = 1_000 * S;
        let step = S / 1000;
        let steps = 2_000u64;
        let stop = std::sync::atomic::AtomicBool::new(false);
        let granted = std::thread::scope(|scope| {
            scope.spawn(|| {
                let foreign = File::options().read(true).write(true).open(&path).unwrap();
                let mut torn = stamped(1, burst, start);
                torn.push(0);
                for round in 0..20_000u32 {
                    if stop.load(std::sync::atomic::Ordering::Relaxed) {
                        break;
                    }
                    foreign.lock().unwrap();
                    let bytes = if round % 2 == 0 {
                        &torn[..]
                    } else {
                        &torn[..RECORD_LEN]
                    };
                    let mut handle = &foreign;
                    handle.seek(SeekFrom::Start(0)).unwrap();
                    handle.write_all(bytes).unwrap();
                    foreign.set_len(bytes.len() as u64).unwrap();
                    foreign.unlock().unwrap();
                    std::thread::yield_now();
                }
            });
            let mut granted = 0u64;
            for i in 0..steps {
                if shared.take_at(0, burst, per_sec, start + i * step) == Take::Granted {
                    granted += 1;
                }
            }
            stop.store(true, std::sync::atomic::Ordering::Relaxed);
            granted
        });
        let elapsed_s = (steps * step).div_ceil(S);
        let bound = u64::from(burst) + u64::from(per_sec) * elapsed_s;
        assert!(
            granted <= bound,
            "granted {granted} over {elapsed_s} s; burst + rate * t is {bound}"
        );
    }

    /// Every thread of a process shares one `SharedBudget`, and so one open
    /// file description — which an advisory lock does not exclude within. At a
    /// frozen clock the threads together get exactly one burst, and the record
    /// is never left torn.
    #[test]
    fn threads_of_one_process_share_one_burst_and_never_tear_the_record() {
        let dir = tempfile::tempdir().unwrap();
        let shared = budget(&dir);
        let burst = 64u32;
        let now = 1_000 * S;
        let granted: u64 = std::thread::scope(|scope| {
            let workers: Vec<_> = (0..8)
                .map(|_| {
                    scope.spawn(|| {
                        let mut granted = 0u64;
                        for _ in 0..500 {
                            if shared.take_at(0, burst, 1, now) == Take::Granted {
                                granted += 1;
                            }
                        }
                        granted
                    })
                })
                .collect();
            workers.into_iter().map(|w| w.join().unwrap()).sum()
        });
        assert_eq!(granted, u64::from(burst), "threads minted past one burst");
        assert_eq!(
            std::fs::metadata(shared.path()).unwrap().len(),
            RECORD_LEN as u64
        );
        assert_eq!(shared.tokens_now(burst, 1, now), 0);
    }

    /// Whether the gate would let a spawn start on this answer: a grant, or a
    /// record it could not consult (fail open).
    fn admits(outcome: &Take) -> bool {
        matches!(outcome, Take::Granted | Take::Unavailable(_))
    }

    /// A record spent down to its floor at `now`, written by a handle that is
    /// then dropped.
    fn spent_record(dir: &tempfile::TempDir, burst: u32, now: u64) -> PathBuf {
        let shared = budget(dir);
        for _ in 0..burst {
            assert_eq!(shared.take_at(0, burst, 1, now), Take::Granted);
        }
        shared.path().to_path_buf()
    }

    /// The denial was decided in memory; the record then could not be
    /// rewritten. A denial that is not recorded is still a denial: reporting
    /// the record as unreachable instead lets the gate admit, and every
    /// process that meets the same failure spends past the shared budget.
    #[test]
    fn a_denial_stays_a_denial_when_the_record_cannot_be_rewritten() {
        let dir = tempfile::tempdir().unwrap();
        let now = 1_000 * S;
        let path = spent_record(&dir, 4, now);
        let shared = SharedBudget::open(&path).unwrap();
        shared.fail_writes(true);
        let before = std::fs::read(&path).unwrap();
        for floor in [0, 2] {
            let outcome = shared.take_at(floor, 4, 1, now);
            assert!(
                matches!(&outcome, Take::DeniedUnrecorded(reason) if reason.contains("could not write")),
                "floor {floor}: {outcome:?}"
            );
        }
        assert_eq!(std::fs::read(&path).unwrap(), before, "nothing was written");
        // Once the record is writable again the denial is an ordinary one.
        shared.fail_writes(false);
        assert_eq!(shared.take_at(0, 4, 1, now), Take::Denied);
    }

    /// A grant whose record write fails is the documented fail-open: the
    /// record could not be reached for this decision, the process bucket still
    /// caps this process, and the record keeps the tokens it had.
    #[test]
    fn a_grant_whose_record_cannot_be_rewritten_is_unavailable() {
        let dir = tempfile::tempdir().unwrap();
        let shared = budget(&dir);
        let now = 1_000 * S;
        assert_eq!(shared.take_at(0, 4, 1, now), Take::Granted);
        shared.fail_writes(true);
        let outcome = shared.take_at(0, 4, 1, now);
        assert!(
            matches!(&outcome, Take::Unavailable(reason) if reason.contains("could not write")),
            "{outcome:?}"
        );
        shared.fail_writes(false);
        assert_eq!(
            shared.tokens_now(4, 1, now),
            3,
            "the failed take spent nothing"
        );
    }

    /// The same denial through a real write error, not the injected one: a
    /// handle opened read-only fails `write_all` with EBADF.
    #[cfg(unix)]
    #[test]
    fn a_denial_on_a_read_only_handle_is_not_admitted() {
        let dir = tempfile::tempdir().unwrap();
        let now = 1_000 * S;
        let path = spent_record(&dir, 4, now);
        let shared = SharedBudget {
            file: Mutex::new(File::open(&path).unwrap()),
            path: path.clone(),
            fail_writes: std::sync::atomic::AtomicBool::new(false),
        };
        let before = std::fs::read(&path).unwrap();
        let outcome = shared.take_at(0, 4, 1, now);
        assert!(!admits(&outcome), "{outcome:?}");
        assert_eq!(std::fs::read(&path).unwrap(), before);
    }

    /// A buffer that carries the magic and a newer version word but is not a
    /// whole record is torn, not a newer layout: nothing can be said of what
    /// it holds, so it is unreadable.
    #[test]
    fn a_short_buffer_claiming_a_newer_version_is_unreadable_not_newer() {
        let full = stamped(VERSION + 1, 3, 9 * S);
        for len in 1..RECORD_LEN {
            assert_eq!(decode(&full[..len]), Record::Unreadable, "{len} bytes");
        }
        let mut long = full.clone();
        long.push(0);
        assert_eq!(decode(&long), Record::Unreadable, "one byte too long");
        assert_eq!(decode(&full), Record::Newer(VERSION + 1));
        assert_eq!(decode(b"GPSB\x03\x00\x00\x00"), Record::Unreadable);
    }

    /// The 8-byte magic-plus-newer-version file, with the reset write
    /// failing, must not admit; with it working, it is the empty reset.
    #[test]
    fn an_eight_byte_newer_stub_is_reset_empty_and_never_admits_unreset() {
        let now = 9 * S;
        let stub = b"GPSB\x03\x00\x00\x00";
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join(FILE_NAME);
        write_private(&path, stub);
        let shared = SharedBudget::open(&path).unwrap();
        shared.fail_writes(true);
        let outcome = shared.take_at(0, 192, 8, now);
        assert!(!admits(&outcome), "{outcome:?}");
        assert_eq!(std::fs::read(&path).unwrap(), stub, "unwritable: untouched");
        shared.fail_writes(false);
        assert_eq!(shared.take_at(0, 192, 8, now), Take::Denied);
        assert_eq!(shared.tokens_now(192, 8, now), 0);
        assert_eq!(std::fs::metadata(&path).unwrap().len(), RECORD_LEN as u64);
    }

    /// A file cut at every length: empty is a never-written record and a full
    /// burst; every other length (of either layout) is torn, grants nothing,
    /// and is not admitted when the reset cannot be persisted.
    #[test]
    fn a_file_truncated_at_every_length_is_a_burst_or_an_empty_reset() {
        let burst = 8;
        let now = 9 * S;
        for version in [VERSION, VERSION + 1] {
            let full = stamped(version, burst, now);
            for len in 0..RECORD_LEN {
                let dir = tempfile::tempdir().unwrap();
                let path = dir.path().join(FILE_NAME);
                write_private(&path, &full[..len]);
                let shared = SharedBudget::open(&path).unwrap();

                shared.fail_writes(true);
                let unwritable = shared.take_at(0, burst, 1, now);
                if len == 0 {
                    assert!(
                        matches!(unwritable, Take::Unavailable(_)),
                        "len 0 grant, record unreachable: {unwritable:?}"
                    );
                } else {
                    assert!(!admits(&unwritable), "v{version} len {len}: {unwritable:?}");
                }
                assert_eq!(std::fs::read(&path).unwrap(), &full[..len]);

                shared.fail_writes(false);
                let writable = shared.take_at(0, burst, 1, now);
                if len == 0 {
                    assert_eq!(writable, Take::Granted);
                    assert_eq!(shared.tokens_now(burst, 1, now), burst - 1);
                } else {
                    assert_eq!(writable, Take::Denied, "v{version} len {len}");
                    assert_eq!(shared.tokens_now(burst, 1, now), 0);
                }
                assert_eq!(std::fs::metadata(&path).unwrap().len(), RECORD_LEN as u64);
            }
        }
    }

    /// Two handles (two processes) on a record at its floor, one of which
    /// cannot write, hammered from several threads: nothing is admitted.
    #[test]
    fn concurrent_takers_on_a_spent_record_are_never_admitted_by_a_failed_write() {
        let dir = tempfile::tempdir().unwrap();
        let now = 1_000 * S;
        let path = spent_record(&dir, 6, now);
        let a = SharedBudget::open(&path).unwrap();
        let b = SharedBudget::open(&path).unwrap();
        b.fail_writes(true);
        let admitted: usize = std::thread::scope(|scope| {
            let workers: Vec<_> = (0..8)
                .map(|i| {
                    let handle = if i % 2 == 0 { &a } else { &b };
                    scope.spawn(move || {
                        (0..200)
                            .filter(|_| {
                                // Unavailable here is a held lock, not a
                                // write failure: the record is spent either way.
                                matches!(handle.take_at(0, 6, 1, now), Take::Granted)
                            })
                            .count()
                    })
                })
                .collect();
            workers.into_iter().map(|w| w.join().unwrap()).sum()
        });
        assert_eq!(admitted, 0);
    }

    /// An unrecorded denial persists neither tokens nor the refill anchor, so
    /// the next handle that can write credits the elapsed time exactly once:
    /// the failed handle's refill is recomputed, never double counted.
    #[test]
    fn an_unrecorded_denial_does_not_let_the_next_writer_refill_twice() {
        let dir = tempfile::tempdir().unwrap();
        let t0 = 1_000 * S;
        let path = spent_record(&dir, 4, t0);
        let a = SharedBudget::open(&path).unwrap();
        let b = SharedBudget::open(&path).unwrap();
        b.fail_writes(true);
        // Half a second at 1/s credits nothing; then 2.5 s in, two whole tokens.
        for at in [t0 + S / 2, t0 + 2 * S + S / 2] {
            assert!(matches!(b.take_at(2, 4, 1, at), Take::DeniedUnrecorded(_)));
        }
        let now = t0 + 3 * S;
        let granted = (0..10)
            .filter(|_| a.take_at(0, 4, 1, now) == Take::Granted)
            .count();
        assert_eq!(granted, 3, "three seconds of refill, once");
    }

    /// A record renamed over by a directory after this handle opened it: the
    /// handle still holds the original file, so decisions are unchanged.
    #[cfg(unix)]
    #[test]
    fn a_record_replaced_by_a_directory_does_not_change_an_open_handles_decisions() {
        let dir = tempfile::tempdir().unwrap();
        let now = 1_000 * S;
        let path = spent_record(&dir, 4, now);
        let shared = SharedBudget::open(&path).unwrap();
        std::fs::remove_file(&path).unwrap();
        std::fs::create_dir(&path).unwrap();
        assert_eq!(shared.take_at(0, 4, 1, now), Take::Denied);
        assert!(SharedBudget::open(&path).is_err(), "a directory is refused");
    }

    /// A read-only record is refused at open, so the gate stays on its own
    /// bucket with the reason, rather than meeting write errors per decision.
    #[cfg(unix)]
    #[test]
    fn a_read_only_record_is_refused_at_open() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join(FILE_NAME);
        write_private(&path, b"");
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o400)).unwrap();
        assert!(SharedBudget::open(&path).is_err());
    }

    /// A record from a newer build is that build's to rewrite. This one steps
    /// aside for the decision (the gate's counted fallback) and leaves every
    /// byte where it was, whether or not the write would have worked.
    #[test]
    fn a_newer_record_is_left_alone_and_the_decision_is_unavailable() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join(FILE_NAME);
        let newer = stamped(VERSION + 1, 3, 9 * S);
        write_private(&path, &newer);
        let shared = SharedBudget::open(&path).unwrap();
        let outcome = shared.take_at(0, 192, 8, 9 * S);
        assert!(
            matches!(&outcome, Take::Unavailable(reason) if reason.contains("newer")),
            "{outcome:?}"
        );
        assert_eq!(
            std::fs::read(&path).unwrap(),
            newer,
            "the newer record was clobbered"
        );
    }

    /// The file after only the first `cut` bytes of `record`'s update (the
    /// bytes of `commit_steps` in the order they are written) reached `old`.
    fn torn_after(old: &[u8], record: &[u8; RECORD_LEN], cut: usize) -> Vec<u8> {
        let mut file = old.to_vec();
        let mut left = cut;
        for (at, bytes) in commit_steps(record) {
            let n = left.min(bytes.len());
            let at = usize::try_from(at).unwrap();
            file[at..at + n].copy_from_slice(&bytes[..n]);
            left -= n;
        }
        file
    }

    fn stream_len(record: &[u8; RECORD_LEN]) -> usize {
        commit_steps(record).iter().map(|(_, b)| b.len()).sum()
    }

    /// The record file is shared by every installed binary, so how the update
    /// is staged must not change what a finished update leaves: byte for byte
    /// the record `encode` makes, over a valid record, over junk, and over a
    /// longer file once truncated.
    #[test]
    fn a_staged_update_leaves_exactly_the_encoded_record() {
        let record = encode(State {
            tokens: 77,
            at_ns: 123_456_789_012,
        });
        let len = stream_len(&record);
        for old in [
            vec![0u8; RECORD_LEN],
            vec![0xFFu8; RECORD_LEN],
            stamped(VERSION, 5, 9 * S),
            encode(State {
                tokens: 1,
                at_ns: 1,
            })
            .to_vec(),
        ] {
            assert_eq!(torn_after(&old, &record, len), record.to_vec());
        }
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join(FILE_NAME);
        let mut long = stamped(VERSION, 5, 9 * S);
        long.push(7);
        write_private(&path, &long);
        let shared = SharedBudget::open(&path).unwrap();
        assert_eq!(shared.take_at(0, 192, 8, 9 * S), Take::Denied);
        assert_eq!(std::fs::metadata(&path).unwrap().len(), RECORD_LEN as u64);
        assert!(matches!(
            decode(&std::fs::read(&path).unwrap()),
            Record::Current(State { tokens: 0, .. })
        ));
    }

    /// A write cut at every byte of its update, over a previous valid record:
    /// whatever the next taker finds, it holds no more than the finished
    /// update would have left it. Decoding a torn record as Current used to
    /// credit time the finished update had already counted (new token count
    /// under the previous refill anchor), bounded by the burst. A cut that
    /// leaves the record undecodable costs the next taker its tokens instead
    /// (an empty reset, refilling at the rate), which is the safe side.
    #[test]
    fn a_write_torn_at_any_byte_never_credits_what_the_record_did_not_have() {
        let (burst, per_sec) = (192u32, 8u32);
        let mut over_credits = Vec::new();
        let mut table = Vec::new();
        // The second anchor carries across a byte boundary between the
        // previous record and the new one, so a prefix of its bytes is a
        // refill anchor from neither.
        for (scenario, t0) in [
            ("plain", 1_000 * S + 123_456_789),
            ("carry", (1u64 << 40) - 2 * S),
        ] {
            let dir = tempfile::tempdir().unwrap();
            let shared = budget(&dir);
            for _ in 0..182 {
                assert_eq!(shared.take_at(0, burst, per_sec, t0), Take::Granted);
            }
            let old = std::fs::read(shared.path()).unwrap();
            let t1 = t0 + 10 * S;
            assert_eq!(shared.take_at(0, burst, per_sec, t1), Take::Granted);
            let new: [u8; RECORD_LEN] = std::fs::read(shared.path()).unwrap().try_into().unwrap();
            let full = shared.tokens_now(burst, per_sec, t1);
            assert_eq!(full, 10 + 80 - 1, "{scenario}: the finished update");
            for cut in 1..stream_len(&new) {
                let torn = torn_after(&old, &new, cut);
                let path = dir.path().join(format!("torn-{cut}"));
                write_private(&path, &torn);
                let found = SharedBudget::open(&path).unwrap();
                let held = found.tokens_now(burst, per_sec, t1);
                let verdict = match decode(&torn) {
                    Record::Current(_) => "Current",
                    Record::Newer(_) => "Newer",
                    Record::Unreadable => "Unreadable",
                };
                let over = i64::from(held) - i64::from(full);
                table.push(format!(
                    "{scenario:5} cut {cut:2}: {verdict:10} next taker holds {held:3} \
                     (finished update: {full}), over-credit {over:+}"
                ));
                if over > 0 {
                    over_credits.push(format!("{scenario} cut {cut}: {over:+}"));
                }
            }
        }
        println!("{}", table.join("\n"));
        assert!(
            over_credits.is_empty(),
            "torn records credited tokens the finished update did not leave: {over_credits:?}\n{}",
            table.join("\n")
        );
    }

    #[test]
    fn the_file_name_moves_with_the_layout() {
        assert_eq!(FILE_NAME, format!("spawn-budget.v{VERSION}"));
    }

    /// The warning is what attributes a flip-flop from the logs: the bytes
    /// themselves, the writer pid they carry, and the reader that reset them.
    #[test]
    fn a_record_names_its_writer_and_the_warning_shows_its_bytes() {
        let dir = tempfile::tempdir().unwrap();
        let shared = budget(&dir);
        assert_eq!(shared.take_at(0, 4, 1, S), Take::Granted);
        let written = std::fs::read(shared.path()).unwrap();
        assert_eq!(word_at(&written, 12), Some(std::process::id()));
        assert_eq!(
            decode(&written),
            Record::Current(State {
                tokens: 3,
                at_ns: S
            })
        );

        let mut torn = written.clone();
        torn.push(0xab);
        let warning = unreadable_warning(shared.path(), &torn);
        let me = std::process::id();
        for expected in [
            format!("{} held 25 unreadable byte(s) [", shared.path().display()),
            format!("[{}]", hex(&torn)),
            format!("(layout {VERSION}, writer pid {me})"),
            format!("reset to an empty budget by pid {me}, GitPulse "),
            env!("CARGO_PKG_VERSION").to_string(),
        ] {
            assert!(
                warning.contains(&expected),
                "{expected:?} missing from {warning}"
            );
        }
        assert!(hex(&torn).starts_with("47505342"), "the magic, in hex");
        assert!(hex(&torn).ends_with("ab"));
        assert_eq!(hex(&[0x00, 0x0f, 0x7f, 0xff]), "000f7fff");

        assert_eq!(
            writer_of(&stamped(1, 3, S)),
            "layout 1, writer pid unrecorded"
        );
        assert_eq!(
            writer_of(b"GPSB\x01\x00\x00\x00\x09"),
            "layout 1, cut short before the writer pid"
        );
        assert_eq!(
            writer_of(b"garbage"),
            "not a GitPulse spawn-budget record; writer unknown"
        );
        assert_eq!(
            writer_of(b""),
            "not a GitPulse spawn-budget record; writer unknown"
        );
    }

    /// A never-written record is the one case that starts full: only this
    /// layout opens this file, and it never leaves it shorter than a record.
    #[test]
    fn an_empty_file_is_a_new_record_and_starts_full() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join(FILE_NAME);
        write_private(&path, b"");
        let shared = SharedBudget::open(&path).unwrap();
        assert_eq!(shared.take_at(0, 4, 1, 9 * S), Take::Granted);
        assert_eq!(shared.tokens_now(4, 1, 9 * S), 3);
        assert_eq!(std::fs::metadata(&path).unwrap().len(), RECORD_LEN as u64);
    }

    #[test]
    fn a_clock_stepped_back_neither_mints_nor_strands_tokens() {
        let dir = tempfile::tempdir().unwrap();
        let shared = budget(&dir);
        let late = 10_000 * S;
        for _ in 0..4 {
            assert_eq!(shared.take_at(0, 4, 1, late), Take::Granted);
        }
        // An hour back: nothing minted, and the anchor moves to the new now.
        let early = late - 3_600 * S;
        assert_eq!(shared.take_at(0, 4, 1, early), Take::Denied);
        // So refill resumes from the stepped-back clock, not an hour hence.
        assert_eq!(shared.take_at(0, 4, 1, early + 2 * S), Take::Granted);
    }

    #[test]
    fn a_long_idle_record_refills_to_the_burst_and_no_further() {
        let dir = tempfile::tempdir().unwrap();
        let shared = budget(&dir);
        assert_eq!(shared.take_at(0, 5, 2, S), Take::Granted);
        assert_eq!(shared.tokens_now(5, 2, u64::MAX / 2), 5);
        // A record written by a build with a larger burst is clamped to ours.
        std::fs::write(
            shared.path(),
            encode(State {
                tokens: 1_000,
                at_ns: S,
            }),
        )
        .unwrap();
        assert_eq!(shared.tokens_now(5, 2, S), 5);
    }

    #[test]
    fn a_held_lock_makes_the_decision_unavailable_not_denied() {
        let dir = tempfile::tempdir().unwrap();
        let shared = budget(&dir);
        let holder = File::options()
            .read(true)
            .write(true)
            .open(shared.path())
            .unwrap();
        holder.lock().unwrap();
        let started = Instant::now();
        let outcome = shared.take_at(0, 4, 1, S);
        assert!(
            matches!(&outcome, Take::Unavailable(reason) if reason.contains("held")),
            "{outcome:?}"
        );
        assert!(
            started.elapsed() < crate::test_support::coverage_relaxed(Duration::from_secs(1)),
            "bounded wait"
        );
        holder.unlock().unwrap();
        assert_eq!(shared.take_at(0, 4, 1, S), Take::Granted);
    }

    /// A thread stuck inside the critical section must not park every other
    /// spawn decision: waiting on a peer thread is bounded by the same
    /// [`LOCK_PATIENCE`] as waiting on another process. The decision comes back
    /// `Unavailable` (fail open, counted), never as a hang and never as `Denied`.
    ///
    /// The helper thread reports through a channel and the test waits on it
    /// with a deadline, so a patience that stopped being enforced fails here
    /// instead of hanging the suite.
    #[test]
    fn a_wedged_thread_makes_other_threads_unavailable_within_the_patience() {
        let dir = tempfile::tempdir().unwrap();
        let shared = budget(&dir);
        let wedged = shared.lock().expect("lock");
        let (sender, receiver) = std::sync::mpsc::channel();
        let started = Instant::now();
        let answer = std::thread::scope(|scope| {
            scope.spawn(|| {
                let _ = sender.send(shared.take_at(0, 4, 1, S));
            });
            let answer = receiver.recv_timeout(crate::test_support::coverage_relaxed(
                Duration::from_secs(5),
            ));
            // Release the wedge whatever happened, so the helper can finish
            // and the scope can join it.
            drop(wedged);
            answer
        });
        let outcome =
            answer.expect("the decision never came back: the in-process wait is unbounded");
        assert!(
            matches!(&outcome, Take::Unavailable(reason) if reason.contains("another thread")),
            "{outcome:?}"
        );
        assert!(
            started.elapsed() < crate::test_support::coverage_relaxed(Duration::from_secs(1)),
            "bounded wait"
        );
        assert_eq!(
            shared.take_at(0, 4, 1, S),
            Take::Granted,
            "the record is usable again once the wedged holder lets go"
        );
    }

    #[cfg(unix)]
    #[test]
    fn an_unsafe_record_is_refused_at_open() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let open = dir.path().join("open");
        std::fs::write(&open, b"").unwrap();
        std::fs::set_permissions(&open, std::fs::Permissions::from_mode(0o666)).unwrap();
        let err = SharedBudget::open(&open)
            .err()
            .expect("world-writable refused");
        assert!(err.contains("accessible to other users"), "{err}");

        let target = dir.path().join("target");
        std::fs::write(&target, b"").unwrap();
        std::fs::set_permissions(&target, std::fs::Permissions::from_mode(0o600)).unwrap();
        let link = dir.path().join("link");
        std::os::unix::fs::symlink(&target, &link).unwrap();
        assert!(
            SharedBudget::open(&link).is_err(),
            "a symlink is not followed"
        );

        let sub = dir.path().join("sub");
        std::fs::create_dir(&sub).unwrap();
        assert!(SharedBudget::open(&sub).is_err(), "a directory is refused");
    }

    #[test]
    fn a_new_record_is_private_to_its_owner() {
        let dir = tempfile::tempdir().unwrap();
        let shared = SharedBudget::open(&dir.path().join("nested").join(FILE_NAME)).unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(shared.path())
                .unwrap()
                .permissions()
                .mode();
            assert_eq!(mode & 0o777, 0o600);
        }
        assert!(shared.path().ends_with(FILE_NAME));
    }

    // ---- adversarial: mixed binaries, faults, file shapes (ft-d41b8491) ----

    /// One update as the binary on `fc088f9d` made it: a single in-place
    /// 24-byte write and then the truncation, with no staging. `cut` stops the
    /// write after that many bytes, as a crash would. Reads through the same
    /// `read_state` as the new writer: the decoder did not change between the
    /// two builds.
    fn old_binary_take(
        shared: &SharedBudget,
        burst: u32,
        per_sec: u32,
        now: u64,
        cut: Option<usize>,
    ) {
        let locked = shared.lock().expect("lock");
        let mut state = shared.read_state(&locked, burst, now).expect("read");
        refill(&mut state, burst, per_sec, now);
        if state.tokens > 0 {
            state.tokens -= 1;
        }
        let record = encode(state);
        let mut file: &File = &locked.file;
        file.seek(SeekFrom::Start(0)).unwrap();
        match cut {
            None => {
                file.write_all(&record).unwrap();
                file.set_len(RECORD_LEN as u64).unwrap();
            }
            Some(n) => file.write_all(&record[..n]).unwrap(),
        }
    }

    /// The new binary's take, its update cut after `cut` bytes of
    /// [`commit_steps`]' stream.
    fn new_binary_take_cut(shared: &SharedBudget, burst: u32, per_sec: u32, now: u64, cut: usize) {
        let locked = shared.lock().expect("lock");
        let mut state = shared.read_state(&locked, burst, now).expect("read");
        refill(&mut state, burst, per_sec, now);
        if state.tokens > 0 {
            state.tokens -= 1;
        }
        let before = std::fs::read(shared.path()).unwrap();
        let torn = torn_after(&before, &encode(state), cut);
        let mut file: &File = &locked.file;
        file.seek(SeekFrom::Start(0)).unwrap();
        file.write_all(&torn).unwrap();
    }

    #[derive(Clone, Copy, Debug)]
    enum Op {
        New,
        Old,
        NewCut(usize),
        OldCut(usize),
    }

    /// Runs `ops` over a record holding 10 tokens, half a second apart, and
    /// returns the worst `(held - ideal)` seen after any op, where `ideal` is
    /// the bucket had every op (cut or not) finished cleanly, and the op it
    /// was seen after.
    fn worst_over_credit(ops: &[Op]) -> (i64, Option<usize>) {
        let (burst, per_sec) = (192u32, 8u32);
        let t0 = 1_000 * S + 123_456_789;
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join(FILE_NAME);
        write_private(
            &path,
            &encode(State {
                tokens: 10,
                at_ns: t0,
            }),
        );
        let shared = SharedBudget::open(&path).unwrap();
        let mut ideal = State {
            tokens: 10,
            at_ns: t0,
        };
        let mut worst = (i64::MIN, None);
        for (i, op) in ops.iter().enumerate() {
            let now = t0 + (i as u64 + 1) * S / 2;
            match *op {
                Op::New => {
                    shared.take_at(0, burst, per_sec, now);
                }
                Op::Old => old_binary_take(&shared, burst, per_sec, now, None),
                Op::NewCut(n) => new_binary_take_cut(&shared, burst, per_sec, now, n),
                Op::OldCut(n) => old_binary_take(&shared, burst, per_sec, now, Some(n)),
            }
            refill(&mut ideal, burst, per_sec, now);
            ideal.tokens = ideal.tokens.saturating_sub(1);
            let mut seen = ideal;
            refill(&mut seen, burst, per_sec, now);
            let held = i64::from(shared.tokens_now(burst, per_sec, now));
            let over = held - i64::from(seen.tokens);
            if over > worst.0 {
                worst = (over, Some(i));
            }
        }
        worst
    }

    /// An old and a new binary share `spawn-budget.v2` (the layout and the name
    /// did not move). Every sequence of three updates by either binary, the
    /// new one cut at any byte of its staged write, never leaves the record
    /// holding more than the clean run would have: a blanked magic reads as
    /// undecodable to the old decoder too, which resets to EMPTY (the
    /// decoder is unchanged on the base). The torn state the new writer can
    /// leave costs tokens only.
    #[test]
    fn old_and_new_binaries_on_one_record_never_over_credit_after_a_new_writers_cut() {
        let mut ops = vec![Op::New, Op::Old];
        ops.extend(
            (1..stream_len(&encode(State {
                tokens: 0,
                at_ns: 0,
            })))
                .map(Op::NewCut),
        );
        let mut worst = (i64::MIN, Vec::new());
        let mut checked = 0usize;
        for a in &ops {
            for b in &ops {
                for c in &ops {
                    let seq = [*a, *b, *c];
                    let (over, _) = worst_over_credit(&seq);
                    checked += 1;
                    if over > worst.0 {
                        worst = (over, seq.to_vec());
                    }
                }
            }
        }
        println!(
            "{checked} sequences; worst held-minus-ideal {} at {:?}",
            worst.0, worst.1
        );
        assert!(worst.0 <= 0, "over-credit {} after {:?}", worst.0, worst.1);
    }

    /// The one mixed-binary hazard the new writer cannot remove: the OLD
    /// binary's own cut one-shot write leaves a prefix of its bytes under the
    /// previous anchor, which any reader (new included) decodes as current.
    /// Printed per cut so the bound is on record; it is bounded by the burst
    /// and ends when the install is refreshed.
    #[test]
    fn an_old_binarys_cut_write_is_the_only_mixed_over_credit_and_stays_within_the_burst() {
        let mut table = Vec::new();
        let mut max = i64::MIN;
        for cut in 1..RECORD_LEN {
            let (over, _) = worst_over_credit(&[Op::OldCut(cut), Op::New]);
            table.push(format!(
                "old writer cut at byte {cut:2}: over-credit {over:+}"
            ));
            max = max.max(over);
        }
        println!("{}", table.join("\n"));
        assert!(max <= 192, "bounded by one burst: {max}");
    }

    /// A file the way a faulting disk or a signal-happy kernel shows it:
    /// writes of at most `chunk` bytes, an `Interrupted` before every write,
    /// and an error once `fail_after` bytes have landed.
    struct FaultFile {
        buf: Vec<u8>,
        pos: usize,
        chunk: usize,
        landed: usize,
        fail_after: Option<usize>,
        interrupt: bool,
        pending_interrupt: bool,
    }

    impl FaultFile {
        fn over(old: &[u8]) -> Self {
            Self {
                buf: old.to_vec(),
                pos: 0,
                chunk: usize::MAX,
                landed: 0,
                fail_after: None,
                interrupt: false,
                pending_interrupt: false,
            }
        }
    }

    impl Write for FaultFile {
        fn write(&mut self, data: &[u8]) -> std::io::Result<usize> {
            if self.interrupt {
                self.pending_interrupt = !self.pending_interrupt;
                if self.pending_interrupt {
                    return Err(std::io::ErrorKind::Interrupted.into());
                }
            }
            let mut n = data.len().min(self.chunk);
            if let Some(limit) = self.fail_after {
                if self.landed >= limit {
                    return Err(std::io::Error::other("injected ENOSPC"));
                }
                n = n.min(limit - self.landed);
            }
            if self.buf.len() < self.pos + n {
                self.buf.resize(self.pos + n, 0);
            }
            self.buf[self.pos..self.pos + n].copy_from_slice(&data[..n]);
            self.pos += n;
            self.landed += n;
            Ok(n)
        }

        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }

    impl Seek for FaultFile {
        fn seek(&mut self, to: SeekFrom) -> std::io::Result<u64> {
            let SeekFrom::Start(at) = to else {
                return Err(std::io::Error::other("only absolute seeks"));
            };
            self.pos = usize::try_from(at).unwrap();
            Ok(at)
        }
    }

    /// A write returning fewer bytes than asked, or `Interrupted`, is
    /// finished: the update lands exactly as `encode` made it.
    #[test]
    fn short_writes_and_interrupts_still_land_the_whole_record() {
        let old = encode(State {
            tokens: 3,
            at_ns: 5,
        });
        let new = encode(State {
            tokens: 90,
            at_ns: 77 * S,
        });
        for chunk in [1usize, 2, 7, 23, usize::MAX] {
            let mut file = FaultFile::over(&old);
            file.chunk = chunk;
            file.interrupt = true;
            stage_update(&mut file, &new, || Ok(())).expect("write_all retries");
            assert_eq!(file.buf, new.to_vec(), "chunk {chunk}");
        }
    }

    /// A write that fails after any number of bytes, or a truncation that
    /// fails, leaves the record either untouched (nothing landed) or
    /// undecodable: never a decodable mix of old and new, which is the only
    /// shape that could credit.
    #[test]
    fn a_write_failing_at_any_byte_leaves_the_old_record_or_an_undecodable_one() {
        let old = encode(State {
            tokens: 3,
            at_ns: 5 * S,
        });
        let new = encode(State {
            tokens: 90,
            at_ns: 5 * S + 255,
        });
        let total = stream_len(&new);
        let mut rows = Vec::new();
        for landed in 0..total {
            let mut file = FaultFile::over(&old);
            file.fail_after = Some(landed);
            assert!(stage_update(&mut file, &new, || Ok(())).is_err());
            let verdict = decode(&file.buf);
            let shape = if file.buf == old.to_vec() {
                "old record untouched"
            } else {
                assert_eq!(verdict, Record::Unreadable, "{landed} bytes landed");
                "undecodable"
            };
            rows.push(format!("fails after {landed:2} byte(s): {shape}"));
        }
        let mut file = FaultFile::over(&old);
        assert!(stage_update(&mut file, &new, || Err(std::io::Error::other("ftruncate"))).is_err());
        assert_eq!(decode(&file.buf), Record::Unreadable, "truncate failed");
        println!("{}", rows.join("\n"));
    }

    /// The combined effect of a failure after the blank step, composed from
    /// what each side does: the writer's own decision is a grant it could not
    /// record (`Unavailable`, fail open, pinned by
    /// `a_grant_whose_record_cannot_be_rewritten_is_unavailable`), and the
    /// record it leaves is undecodable, so every other process reads EMPTY and
    /// is refused until the rate refills it. One fail-open grant, then
    /// fail-closed for the fleet, never a refill to full.
    #[test]
    fn a_failure_after_the_blank_step_costs_the_fleet_its_tokens_not_a_refill() {
        let (burst, per_sec, now) = (192u32, 8u32, 1_000 * S);
        let dir = tempfile::tempdir().unwrap();
        let shared = budget(&dir);
        assert_eq!(shared.take_at(0, burst, per_sec, now), Take::Granted);
        let before = std::fs::read(shared.path()).unwrap();
        let next = encode(State {
            tokens: burst - 2,
            at_ns: now,
        });
        // The blank step landed and the body did not.
        write_private(shared.path(), &torn_after(&before, &next, 1));
        let peer = SharedBudget::open(shared.path()).unwrap();
        assert_eq!(peer.take_at(0, burst, per_sec, now), Take::Denied);
        assert_eq!(
            peer.take_at(0, burst, per_sec, now + 125_000_000),
            Take::Granted
        );
        assert!(peer.tokens_now(burst, per_sec, now + 125_000_000) <= 1);
    }

    /// A previous file of any length, a longer one cut down, a shorter one
    /// grown, ends one update as exactly one record.
    #[test]
    fn an_update_over_a_file_of_any_length_leaves_one_current_record() {
        for len in 0..=60usize {
            let dir = tempfile::tempdir().unwrap();
            let path = dir.path().join(FILE_NAME);
            let mut old = stamped(VERSION, 5, 9 * S);
            old.resize(len.max(1), 0xAB);
            old.truncate(len);
            write_private(&path, &old);
            let shared = SharedBudget::open(&path).unwrap();
            let outcome = shared.take_at(0, 192, 8, 9 * S);
            let bytes = std::fs::read(&path).unwrap();
            assert_eq!(bytes.len(), RECORD_LEN, "len {len}: {outcome:?}");
            let Record::Current(state) = decode(&bytes) else {
                panic!("len {len}: left {bytes:?}");
            };
            // Empty is a fresh burst, a whole record is itself (5, taken
            // once), every other length is torn and EMPTY.
            let expect = match len {
                0 => 191,
                RECORD_LEN => 4,
                _ => 0,
            };
            assert_eq!(state.tokens, expect, "len {len}");
        }
    }

    /// A record whose first magic byte is blank is a staged update that never
    /// committed, and the warning says so instead of calling it foreign.
    #[test]
    fn a_cut_staged_update_is_named_as_one_in_the_warning() {
        let record = encode(State {
            tokens: 9,
            at_ns: 9 * S,
        });
        let old = encode(State {
            tokens: 40,
            at_ns: 8 * S,
        });
        let torn = torn_after(&old, &record, 1 + (RECORD_LEN - 1));
        assert_eq!(torn[0], 0);
        let said = writer_of(&torn);
        let pid = std::process::id();
        assert_eq!(
            said,
            format!("update cut before it committed, layout {VERSION}, writer pid {pid}")
        );
        let mut foreign = torn.clone();
        foreign[1] = b'X';
        assert!(writer_of(&foreign).starts_with("not a GitPulse"));
    }

    /// The staged order assumes what a crash gives (writes reach the file in
    /// order) and that one sector persists whole. If a power loss could
    /// persist ANY subset of the update's changed bytes, a decodable mix
    /// exists whatever the order (the first magic byte is the same in the old
    /// and new record, so restoring it proves nothing), and only an `fsync`
    /// between steps would close it. Pinned so the limit is on record; the
    /// fsync costs three orders of magnitude per take (see the measurement).
    #[test]
    fn a_byte_granular_power_loss_could_mix_records_whatever_the_order() {
        let (burst, per_sec) = (192u32, 8u32);
        let t0 = 1_000 * S + 255;
        let old = encode(State {
            tokens: 10,
            at_ns: t0,
        });
        let new = encode(State {
            tokens: 100,
            at_ns: t0 + 10 * S,
        });
        let changed: Vec<usize> = (0..RECORD_LEN).filter(|&i| old[i] != new[i]).collect();
        assert!(changed.len() <= 12, "{changed:?}");
        let finished = {
            let mut s = State {
                tokens: 100,
                at_ns: t0 + 10 * S,
            };
            refill(&mut s, burst, per_sec, t0 + 10 * S);
            s.tokens
        };
        let mut over = 0usize;
        for subset in 0u32..(1 << changed.len()) {
            let mut mixed = old;
            for (bit, &at) in changed.iter().enumerate() {
                if subset >> bit & 1 == 1 {
                    mixed[at] = new[at];
                }
            }
            if let Record::Current(mut s) = decode(&mixed) {
                refill(&mut s, burst, per_sec, t0 + 10 * S);
                if s.tokens > finished {
                    over += 1;
                }
            }
        }
        println!(
            "{over} of {} byte subsets over-credit",
            1u32 << changed.len()
        );
        assert!(over > 0);
    }

    /// Wall-clock cost of one take with the staged update as written and with
    /// an `fsync` between the steps (what a power-loss ordering guarantee
    /// would cost). Measurement only: run with `--ignored --nocapture`.
    #[test]
    #[ignore = "measurement, not a check"]
    fn measure_the_cost_of_syncing_between_the_staged_steps() {
        let record = encode(State {
            tokens: 9,
            at_ns: 9,
        });
        let dir = tempfile::tempdir().unwrap();
        let file = std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(dir.path().join("m"))
            .unwrap();
        let mut best = [Duration::MAX; 2];
        for _ in 0..5 {
            for (slot, sync) in [false, true].into_iter().enumerate() {
                let started = Instant::now();
                for _ in 0..200 {
                    let mut handle: &File = &file;
                    let [blank, body, commit] = commit_steps(&record);
                    for (n, (at, bytes)) in [blank, body, commit].into_iter().enumerate() {
                        handle.seek(SeekFrom::Start(at)).unwrap();
                        handle.write_all(bytes).unwrap();
                        if sync && n < 2 {
                            file.sync_data().unwrap();
                        }
                    }
                }
                best[slot] = best[slot].min(started.elapsed() / 200);
            }
        }
        println!(
            "per take (min of 5x200): staged {:?}, synced between steps {:?}",
            best[0], best[1]
        );
    }
}
