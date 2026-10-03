//! A spawn-rate budget every GitPulse process of one user draws from.
//!
//! The app, every agent session's `gitpulse-mcp`, `gitpulsed` and the hooks
//! all run git through this engine, and each had a token bucket of its own —
//! so N processes could start N times the rate the gate exists to cap. This
//! bucket is a small record in the per-user config directory, read and
//! rewritten under an exclusive advisory lock. A process keeps its own bucket
//! as well, and a spawn needs a token from both, so the shared record can only
//! tighten the per-process limit, never loosen it.
//!
//! It fails open, and says so. A record that cannot be opened safely leaves
//! the process on its own bucket with a logged reason; a lock another process
//! holds past [`LOCK_PATIENCE`] lets this one decision fall back to the process
//! bucket, counted. Failing closed instead would let one stopped process — a
//! debugger, a `SIGSTOP` — defer every read in every other GitPulse process.

use std::fs::{File, OpenOptions, TryLockError};
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

const MAGIC: [u8; 4] = *b"GPSB";
const VERSION: u32 = 1;
/// `MAGIC`, `VERSION`, tokens (u32), reserved (u32), refill anchor (u64 ns).
const RECORD_LEN: usize = 24;
/// How long one decision waits for another process to finish its update.
/// An update is a 24-byte read and write, so anything this long is a holder
/// that is stopped or wedged, not one that is busy.
const LOCK_PATIENCE: Duration = Duration::from_millis(25);
const FILE_NAME: &str = "spawn-budget.v1";

/// Where every GitPulse process of this user finds the shared record.
///
/// The config directory rather than `temp_dir()`: agent hosts commonly give
/// the processes they start a private `TMPDIR`, which would silently split
/// the app and its agents onto separate budgets again.
// The unit-test build never opens the real record (see `shared_budget_path`).
#[cfg_attr(test, allow(dead_code))]
pub(super) fn default_path() -> Result<PathBuf, String> {
    crate::tool_config::default_config_dir()
        .map(|dir| dir.join(FILE_NAME))
        .ok_or_else(|| "no per-user config directory (HOME / APPDATA unset)".to_string())
}

pub(super) struct SharedBudget {
    file: File,
    path: PathBuf,
}

/// What the shared record said about one spawn.
#[derive(Debug, PartialEq)]
pub(super) enum Take {
    /// A token was taken from the shared record.
    Granted,
    /// The shared budget is spent down to the caller's floor.
    Denied,
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

/// Releases the advisory lock on every path out of an update.
struct Locked<'a>(&'a File);

impl Drop for Locked<'_> {
    fn drop(&mut self) {
        // Closing the descriptor would release it too; the record stays open
        // for the next decision, so release explicitly. Nothing useful can be
        // done about a failure here, and the next `try_lock` reports it.
        let _ = self.0.unlock();
    }
}

impl SharedBudget {
    /// Opens (creating if absent) the record at `path`, refusing one that is
    /// not a regular file this user alone owns and can read.
    pub(super) fn open(path: &Path) -> Result<Self, String> {
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir)
                .map_err(|e| format!("could not create {}: {e}", dir.display()))?;
        }
        let mut options = OpenOptions::new();
        options.read(true).write(true).create(true).truncate(false);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            // A symlink planted at the path would otherwise redirect the
            // writes; with O_NOFOLLOW the open fails instead.
            options.mode(0o600).custom_flags(libc::O_NOFOLLOW);
        }
        let file = options
            .open(path)
            .map_err(|e| format!("could not open {}: {e}", path.display()))?;
        verify_owner(&file, path)?;
        Ok(Self {
            file,
            path: path.to_path_buf(),
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
        let _locked = match self.lock() {
            Ok(locked) => locked,
            Err(reason) => return Take::Unavailable(reason),
        };
        let mut state = match self.read_state(burst, now_ns) {
            Ok(state) => state,
            Err(reason) => return Take::Unavailable(reason),
        };
        refill(&mut state, burst, per_sec, now_ns);
        let granted = state.tokens > floor;
        if granted {
            state.tokens -= 1;
        }
        // Written even on a denial: the refill anchor moved.
        if let Err(reason) = self.write_state(state) {
            return Take::Unavailable(reason);
        }
        if granted {
            Take::Granted
        } else {
            Take::Denied
        }
    }

    fn lock(&self) -> Result<Locked<'_>, String> {
        let started = Instant::now();
        loop {
            match self.file.try_lock() {
                Ok(()) => return Ok(Locked(&self.file)),
                Err(TryLockError::WouldBlock) => {
                    if started.elapsed() >= LOCK_PATIENCE {
                        return Err(format!(
                            "another GitPulse process held {} for over {} ms",
                            self.path.display(),
                            LOCK_PATIENCE.as_millis()
                        ));
                    }
                    std::thread::sleep(Duration::from_millis(1));
                }
                Err(TryLockError::Error(e)) => {
                    return Err(format!("could not lock {}: {e}", self.path.display()))
                }
            }
        }
    }

    /// The record, or a full burst when there is none yet. A record that is
    /// present but unreadable is replaced with a full burst and logged: the
    /// alternative, refusing, would turn one bad write into a permanent stop.
    fn read_state(&self, burst: u32, now_ns: u64) -> Result<State, String> {
        let fresh = State {
            tokens: burst,
            at_ns: now_ns,
        };
        let mut file = &self.file;
        file.seek(SeekFrom::Start(0))
            .map_err(|e| format!("could not read {}: {e}", self.path.display()))?;
        let mut buf = Vec::with_capacity(RECORD_LEN);
        file.take(RECORD_LEN as u64 + 1)
            .read_to_end(&mut buf)
            .map_err(|e| format!("could not read {}: {e}", self.path.display()))?;
        if buf.is_empty() {
            return Ok(fresh);
        }
        match decode(&buf) {
            Some(state) => Ok(State {
                tokens: state.tokens.min(burst),
                ..state
            }),
            None => {
                log::warn!(
                    target: "spawn_gate",
                    "{} held {} unreadable byte(s); reset to a full budget",
                    self.path.display(),
                    buf.len()
                );
                Ok(fresh)
            }
        }
    }

    fn write_state(&self, state: State) -> Result<(), String> {
        let mut file = &self.file;
        let record = encode(state);
        file.seek(SeekFrom::Start(0))
            .and_then(|_| file.write_all(&record))
            .and_then(|_| self.file.set_len(RECORD_LEN as u64))
            .map_err(|e| format!("could not write {}: {e}", self.path.display()))
    }

    #[cfg(test)]
    fn tokens_now(&self, burst: u32, per_sec: u32, now_ns: u64) -> u32 {
        let _locked = self.lock().expect("lock");
        let mut state = self.read_state(burst, now_ns).expect("read");
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

fn encode(state: State) -> [u8; RECORD_LEN] {
    let mut record = [0u8; RECORD_LEN];
    record[0..4].copy_from_slice(&MAGIC);
    record[4..8].copy_from_slice(&VERSION.to_le_bytes());
    record[8..12].copy_from_slice(&state.tokens.to_le_bytes());
    record[16..24].copy_from_slice(&state.at_ns.to_le_bytes());
    record
}

fn decode(buf: &[u8]) -> Option<State> {
    if buf.len() != RECORD_LEN || buf[0..4] != MAGIC {
        return None;
    }
    let word = |range: std::ops::Range<usize>| -> Option<[u8; 4]> { buf[range].try_into().ok() };
    if u32::from_le_bytes(word(4..8)?) != VERSION {
        return None;
    }
    Some(State {
        tokens: u32::from_le_bytes(word(8..12)?),
        at_ns: u64::from_le_bytes(buf[16..24].try_into().ok()?),
    })
}

fn wall_ns() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|elapsed| u64::try_from(elapsed.as_nanos()).unwrap_or(u64::MAX))
        // Before 1970: anchor at zero; the backwards-clock clamp handles it.
        .unwrap_or(0)
}

#[cfg(unix)]
fn verify_owner(file: &File, path: &Path) -> Result<(), String> {
    use std::os::unix::fs::MetadataExt;
    let meta = file
        .metadata()
        .map_err(|e| format!("could not inspect {}: {e}", path.display()))?;
    if !meta.is_file() {
        return Err(format!("{} is not a regular file", path.display()));
    }
    // SAFETY: geteuid has no preconditions and cannot fail.
    let me = unsafe { libc::geteuid() };
    if meta.uid() != me {
        return Err(format!(
            "{} is owned by uid {}, not this user (uid {me})",
            path.display(),
            meta.uid()
        ));
    }
    if meta.mode() & 0o077 != 0 {
        return Err(format!(
            "{} is accessible to other users (mode {:o})",
            path.display(),
            meta.mode() & 0o777
        ));
    }
    Ok(())
}

/// The per-user `APPDATA` directory is already private to its owner.
#[cfg(not(unix))]
fn verify_owner(file: &File, path: &Path) -> Result<(), String> {
    let meta = file
        .metadata()
        .map_err(|e| format!("could not inspect {}: {e}", path.display()))?;
    if meta.is_file() {
        Ok(())
    } else {
        Err(format!("{} is not a regular file", path.display()))
    }
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

    #[test]
    fn a_corrupt_or_short_record_resets_to_a_full_burst() {
        for junk in [
            &b"garbage"[..],
            &[0u8; RECORD_LEN][..],
            &[0xFFu8; RECORD_LEN + 7][..],
        ] {
            let dir = tempfile::tempdir().unwrap();
            let path = dir.path().join(FILE_NAME);
            std::fs::write(&path, junk).unwrap();
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600)).unwrap();
            }
            let shared = SharedBudget::open(&path).unwrap();
            assert_eq!(shared.take_at(0, 4, 1, 9 * S), Take::Granted);
            assert_eq!(shared.tokens_now(4, 1, 9 * S), 3);
            assert_eq!(std::fs::metadata(&path).unwrap().len(), RECORD_LEN as u64);
        }
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
        assert!(started.elapsed() < Duration::from_secs(1), "bounded wait");
        holder.unlock().unwrap();
        assert_eq!(shared.take_at(0, 4, 1, S), Take::Granted);
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
}
