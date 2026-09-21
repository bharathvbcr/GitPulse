//! Process-global environment overrides that cannot outlive the test.
//!
//! # Why this exists
//!
//! An environment variable is one slot shared by every thread in the test
//! binary. Eighteen call sites used to install an override and undo it on the
//! last line of the test body:
//!
//! ```ignore
//! std::env::set_var("GITPULSE_MANVI_BIN", fake.display().to_string());
//! let msg = resolve_binary_absence();
//! std::env::remove_var("GITPULSE_MANVI_BIN");
//! assert!(msg.contains("not a file"));
//! ```
//!
//! which works exactly until the body fails before reaching the last line —
//! and the body is a test, so failing is the one thing it is built to do. The
//! panic skips the restore, unwinding drops whatever `TempDir` the override
//! pointed into, and the value stays installed for every later test in the
//! process. That is not a hypothetical: the sidecar's fake-binary override
//! leaked exactly this way, and one slow test became nineteen failures across
//! terminal, `tool_install`, workbench and firebase, each reading
//! `could not start …/profile-sidecar: No such file or directory` and each
//! passing when run on its own, which is what makes the class so expensive to
//! diagnose — the damage never surfaces where it is caused.
//!
//! [`bind_env`] is the fix in the shape this crate already uses for the two
//! binary overrides (`harness::sidecar::bind_test_binary` and
//! `devmap::cli::bind_test_binary`): undoing the override is a `Drop`,
//! so it runs on the failing path and the successful one alike, and there is no
//! last line to forget.
//!
//! # What it restores
//!
//! The *previous value*, not "nothing". The hand-rolled sites mostly ended in
//! `remove_var`, which is only accidentally correct: it is right when the
//! variable started out unset and silently destroys a real one otherwise. A
//! developer with `GITPULSE_DEVMAP_BIN` exported lost it for the rest of the
//! run. `Drop` here puts back exactly what was there, including absence.
//!
//! # Why a serial is required to get one
//!
//! Restoring reliably is only half the contract. Two tests that set the same
//! variable in parallel interleave regardless of how carefully each one cleans
//! up, and the loser reads the winner's value — or, worse, reads the
//! developer's real environment and asserts against whatever happens to be on
//! the machine. So the only way to obtain a binding is to hand it a serial
//! guard, which makes "I forgot to serialize" a compile error rather than a
//! race that shows up once a fortnight in CI. The binding borrows that guard,
//! so the guard cannot be released while the override is still installed.
//!
//! # Why this file is included twice
//!
//! `gitpulsed` is a separate bin crate and cannot see `gitpulse_lib`'s
//! `#[cfg(test)]` items, but it overrides `GITPULSE_TRANSCRIPT_ROOT` the same
//! way and leaks it the same way. It includes this file with `#[path]` — the
//! idiom `tests/common/process_trust.rs` already uses across two integration
//! tests — so the guarantee is one implementation rather than two that drift.

// Each including crate uses a subset of the API: `gitpulsed` only ever sets a
// variable, so `remove` and `invalidating` are dead code there. The alternative
// is a second, smaller copy of this file, which is the duplication this module
// exists to remove.
#![allow(dead_code)]

use std::ffi::{OsStr, OsString};

/// Proof that the holder has serialized against everything that reads the
/// variable it is about to overwrite.
///
/// Deliberately not implemented for arbitrary types: it is implemented for the
/// mutex guards this crate's tests actually use as serials, plus
/// `harness::sidecar::SidecarTestGuard` (implemented in the parent module,
/// which can name it — this file is also compiled into a crate that cannot).
///
/// It proves *a* serial is held, not that it is the right one for this
/// variable, which is the same bound `harness::sidecar::bind_test_binary`
/// gives. Picking the correct serial stays a judgement each call site makes and
/// documents; what this removes is the case where none was taken at all.
pub(crate) trait EnvSerial {}

impl EnvSerial for std::sync::MutexGuard<'_, ()> {}

/// Environment overrides installed for the whole process, undone on drop.
///
/// Obtained only from [`bind_env`]. The lifetime is the serial's: the borrow
/// keeps the guard alive for at least as long as the override it authorises.
pub(crate) struct EnvBinding<'serial> {
    /// Key and the value it had before this binding touched it, in the order
    /// they were touched. Restored in reverse, so a key overridden twice ends
    /// up back at the value it had before the *first* override.
    saved: Vec<(OsString, Option<OsString>)>,
    /// Caches to drop once the variables are back. Run on the way in as well,
    /// for the reason given on [`EnvBinding::invalidating`].
    invalidations: Vec<Box<dyn Fn()>>,
    _serial: std::marker::PhantomData<&'serial ()>,
}

/// Begin overriding the environment, serialized behind `serial`.
///
/// The returned binding starts out having changed nothing; [`EnvBinding::set`]
/// and [`EnvBinding::remove`] install the overrides.
pub(crate) fn bind_env<S: EnvSerial + ?Sized>(_serial: &S) -> EnvBinding<'_> {
    EnvBinding {
        saved: Vec::new(),
        invalidations: Vec::new(),
        _serial: std::marker::PhantomData,
    }
}

impl EnvBinding<'_> {
    /// Set `key` to `value` until this binding is dropped.
    pub(crate) fn set(mut self, key: impl AsRef<OsStr>, value: impl AsRef<OsStr>) -> Self {
        let key = key.as_ref();
        self.save(key);
        // SAFETY: serialized behind the guard `bind_env` required, and undone
        // by this binding's `Drop` on the unwinding path as well as the normal
        // one.
        unsafe { std::env::set_var(key, value) };
        self
    }

    /// Unset `key` until this binding is dropped.
    ///
    /// The mirror of [`set`](Self::set), and needed just as often: several
    /// tests here assert what happens with *no* override installed, which is a
    /// claim about the developer's environment unless the variable is cleared
    /// for the duration — and clearing it is the same process-global mutation,
    /// with the same obligation to put it back.
    pub(crate) fn remove(mut self, key: impl AsRef<OsStr>) -> Self {
        let key = key.as_ref();
        self.save(key);
        // SAFETY: as in `set`.
        unsafe { std::env::remove_var(key) };
        self
    }

    /// Run `invalidate` now and again once the variables are restored.
    ///
    /// Both ends, because a cached read is as process-global as the variable it
    /// came from: a value cached before the override answers from the old
    /// environment, and one cached during it answers the *next* test from an
    /// environment that no longer exists. `bind_test_binary` drops the sidecar
    /// slot on the way in for precisely this reason; this is the same rule for
    /// the caches that sit in front of these variables.
    ///
    /// Call it after the [`set`](Self::set) calls it belongs to — the builder
    /// runs in the order it is written.
    pub(crate) fn invalidating(mut self, invalidate: impl Fn() + 'static) -> Self {
        invalidate();
        self.invalidations.push(Box::new(invalidate));
        self
    }

    fn save(&mut self, key: &OsStr) {
        self.saved.push((key.to_os_string(), std::env::var_os(key)));
    }
}

impl Drop for EnvBinding<'_> {
    fn drop(&mut self) {
        for (key, value) in self.saved.drain(..).rev() {
            match value {
                // SAFETY: still serialized — the binding borrows the guard, so
                // the guard outlives this drop.
                Some(value) => unsafe { std::env::set_var(&key, value) },
                None => unsafe { std::env::remove_var(&key) },
            }
        }
        // After the restore, never before: an invalidation that ran first would
        // repopulate the cache from the environment this binding is about to
        // take away.
        for invalidate in &self.invalidations {
            invalidate();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use std::sync::atomic::{AtomicUsize, Ordering};

    /// This module's own serial. The tests below are the only readers and
    /// writers of the probe variable, and they take this before touching it —
    /// the rule the module exists to enforce applies to the module's own tests.
    static PROBE_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

    const PROBE: &str = "GITPULSE_TEST_ENV_BINDING_PROBE";

    fn serial() -> std::sync::MutexGuard<'static, ()> {
        PROBE_LOCK
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    /// Regression (a panic leaks a process-global override): the whole point of
    /// the type. A test that fails while holding an override must leave the
    /// environment exactly as it found it, because the next test to read that
    /// variable has no way to tell an override from the truth — and will fail
    /// somewhere else entirely, looking like an unrelated flake.
    ///
    /// Asserted for both starting states, because "restore" means two different
    /// operations: putting a value back, and taking one away.
    #[test]
    fn a_failing_test_cannot_leave_its_env_override_installed() {
        let serial = serial();
        // SAFETY: serialized behind `serial`; both arms below restore it.
        unsafe { std::env::remove_var(PROBE) };

        let invalidated = std::sync::Arc::new(AtomicUsize::new(0));

        // Starting absent: the override must be gone afterwards, not empty.
        let counter = std::sync::Arc::clone(&invalidated);
        let outcome = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let _env = bind_env(&serial)
                .set(PROBE, "installed")
                .invalidating(move || {
                    counter.fetch_add(1, Ordering::SeqCst);
                });
            assert_eq!(
                std::env::var(PROBE).as_deref(),
                Ok("installed"),
                "the fixture must install an override before it fails"
            );
            panic!("simulated failure while holding the environment override");
        }));
        assert!(outcome.is_err(), "the fixture must actually have failed");
        assert_eq!(
            std::env::var_os(PROBE),
            None,
            "the override outlived the test that installed it; every later test \
             reading this variable would answer from a value nobody meant it to see"
        );
        assert_eq!(
            invalidated.load(Ordering::SeqCst),
            2,
            "the cache must be dropped on the way in and again on the way out, \
             including when the way out is a panic"
        );

        // Starting present: restoring is putting the old value back, and a
        // binding that only knew how to `remove_var` would pass the case above
        // while destroying a real setting here.
        // SAFETY: serialized behind `serial`; removed at the end of the test.
        unsafe { std::env::set_var(PROBE, "pre-existing") };
        let outcome = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let _env = bind_env(&serial).set(PROBE, "installed").remove(PROBE);
            assert_eq!(std::env::var_os(PROBE), None);
            panic!("simulated failure while holding the environment override");
        }));
        assert!(outcome.is_err(), "the fixture must actually have failed");
        assert_eq!(
            std::env::var(PROBE).as_deref(),
            Ok("pre-existing"),
            "restoring must put the previous value back, not unset the variable"
        );

        // SAFETY: serialized behind `serial`, which is still held here.
        unsafe { std::env::remove_var(PROBE) };
    }
}
