//! QoS tier selection and background-band outcomes for child spawns.
//!
//! `QosTier` is an internal process-runner parameter (not serialized over IPC).

use super::gate::Admission;
use std::cell::Cell;

/// Explicit or class-derived scheduling tier for a spawned child process.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum QosTier {
    Standard,
    Background,
}

impl QosTier {
    /// Resolves an explicit QoS override, or falls back to the thread-local
    /// background-band / admission class default.
    pub(crate) fn current(explicit: Option<QosTier>, class: Admission) -> Self {
        explicit.unwrap_or_else(|| {
            if runs_in_background_band(class) {
                Self::Background
            } else {
                Self::Standard
            }
        })
    }
}

thread_local! {
    /// Depth of [`with_background_band`] scopes on this thread.
    static BAND_FORCED: Cell<u32> = const { Cell::new(0) };
}

/// Runs `body` with every child it spawns in the background band whatever the
/// admission class, for work that is background by what it is: `devmap build`
/// is an index rebuild that has run at 50-78% CPU and 1.3 GB, and a user
/// asking for one still does not want their typing to wait behind it.
/// Admission is untouched, so the gate still admits it as its caller's class.
pub(crate) fn with_background_band<T>(body: impl FnOnce() -> T) -> T {
    BAND_FORCED.set(BAND_FORCED.get() + 1);
    let _leave = std::mem::DropGuard::new((), |()| {
        BAND_FORCED.set(BAND_FORCED.get() - 1);
    });
    body()
}

#[cfg(all(test, target_os = "macos"))]
thread_local! {
    /// `(entered, failed)` for the spawns made on this thread, see
    /// [`band_outcomes`].
    static BAND_OUTCOMES: Cell<(u64, u64)> = const { Cell::new((0, 0)) };
}

/// Counts what the spawn of a background-band child did, so a refused band is
/// visible to a test as well as in the error the caller gets. A refusal is not
/// logged here: it is the spawn's failure, and travels to the caller as one.
#[cfg(all(test, target_os = "macos"))]
pub(super) fn count_band_spawn<T>(spawned: &std::io::Result<T>) {
    let (entered, failed) = BAND_OUTCOMES.get();
    match spawned {
        Ok(_) => BAND_OUTCOMES.set((entered + 1, failed)),
        Err(error) if crate::procguard::band_refusal(error) => {
            BAND_OUTCOMES.set((entered, failed + 1));
        }
        Err(_) => {}
    }
}

/// `(entered, failed)`: background-band children that started in the band, and
/// spawns that failed because the band could not be entered, counted for the
/// calling thread. Per thread, not per process, so a test that makes the band
/// fail cannot show up in another test's difference.
#[cfg(all(test, target_os = "macos"))]
pub(crate) fn band_outcomes() -> (u64, u64) {
    BAND_OUTCOMES.get()
}

/// Whether a child spawned under `class` on this thread goes into the macOS
/// background band: exactly the children of [`Admission::Background`], plus
/// those inside [`with_background_band`]. A user action (`Interactive`) and a
/// read someone is waiting on (`Reactive`) are never starved, so a mis-classed
/// write is slow at worst, never throttled.
fn runs_in_background_band(class: Admission) -> bool {
    class == Admission::Background || BAND_FORCED.get() > 0
}
