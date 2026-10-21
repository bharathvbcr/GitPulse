//! The sizes of the process's two shared thread pools, set once at startup.
//!
//! Both pools used to take their defaults, which are "one worker per logical
//! core" for tokio's async workers and for rayon, plus tokio's 512-thread
//! blocking pool. This app's async work is IPC dispatch and awaiting blocking
//! bodies, and its parallel work is a handful of bounded scans, so on a
//! 12-core laptop that was a dozen parked workers per pool, each with a stack,
//! each a candidate to be scheduled by an unrelated wakeup, and a blocking pool
//! that would grow to 512 threads before pushing back on a stall.
//!
//! * The async runtime gets [`async_workers`] workers (2 to 4) and at most
//!   [`MAX_BLOCKING_THREADS`] blocking threads. Every blocking body is also a
//!   child-process wait, and `engine::git_cli` admits only a bounded number of
//!   those, so the blocking pool is not what limits git; it exists to keep a
//!   runaway from taking the machine.
//! * Rayon's global pool gets [`rayon_threads`] threads, half the logical cores
//!   rounded up. Its callers (`insights`, the language scan) are bounded
//!   fan-outs that were never meant to take every core from the foreground.
//!
//! Rayon's workers start at `QOS_CLASS_UTILITY` (see [`UtilityScope`]), and a
//! tokio blocking thread takes the same class for the span of any
//! `Admission::Background` scope, so background work yields to the UI, WebKit
//! and the PTY threads on a loaded Mac.

use std::sync::OnceLock;

/// Most threads tokio may start for `spawn_blocking`.
pub(crate) const MAX_BLOCKING_THREADS: usize = 64;

/// The runtime tauri's `async_runtime` runs on. `async_runtime::set` takes a
/// handle and "cannot drop the underlying runtime", so it lives here.
static ASYNC_RUNTIME: OnceLock<tokio::runtime::Runtime> = OnceLock::new();

/// Async workers for a host with `cpus` logical cores: at least 2, so one
/// blocked task cannot hold up every other, and at most 4.
pub(crate) fn async_workers(cpus: usize) -> usize {
    cpus.clamp(2, 4)
}

/// Rayon threads for a host with `cpus` logical cores: half, rounded up, and
/// never fewer than one.
pub(crate) fn rayon_threads(cpus: usize) -> usize {
    cpus.div_ceil(2).max(1)
}

fn logical_cpus() -> usize {
    std::thread::available_parallelism().map_or(2, std::num::NonZeroUsize::get)
}

pub(crate) fn build_async_runtime(cpus: usize) -> std::io::Result<tokio::runtime::Runtime> {
    tokio::runtime::Builder::new_multi_thread()
        .worker_threads(async_workers(cpus))
        .max_blocking_threads(MAX_BLOCKING_THREADS)
        .thread_name("gitpulse-async")
        .enable_all()
        .build()
}

/// Gives tauri the sized runtime. It must be the first use of
/// `tauri::async_runtime` in the process: the first use of any of its
/// functions builds the default runtime, and `set` after that panics. Only
/// `run()` calls this, which is why a test or bench that builds a mock app
/// keeps the default.
///
/// A failure leaves tauri on its default runtime, which works; the returned
/// note says so, for the caller to log once logging exists (this runs before
/// it does).
pub(crate) fn install_async_runtime() -> Option<String> {
    match build_async_runtime(logical_cpus()) {
        Ok(runtime) => {
            let handle = runtime.handle().clone();
            if ASYNC_RUNTIME.set(runtime).is_ok() {
                tauri::async_runtime::set(handle);
            }
            None
        }
        Err(error) => Some(format!(
            "sized async runtime not built, using tauri's default: {error}"
        )),
    }
}

/// The calling thread's macOS QoS class while it runs work nobody is waiting
/// on, restored when dropped. Nothing on other platforms.
///
/// QoS is the scheduler's hint for which threads to run on the efficiency
/// cores and to starve first: `QOS_CLASS_UTILITY` is below the user-initiated
/// and default classes the UI, WebKit and the PTY threads run at. Rayon's
/// workers keep it for life ([`install_rayon_pool`]); the tokio blocking pool
/// is shared with user actions, so a blocking thread takes it only for the
/// span of one `Admission::Background` scope (`engine::git_cli::with_admission`)
/// and gives it back, never to leave a user's next click on a lowered thread.
///
/// `pthread_set_qos_class_self_np` and `pthread_get_qos_class_np` are
/// documented in the SDK's `<pthread/qos.h>`; classes in `<sys/qos.h>`. The
/// pair fails (EPERM) only for a thread that opted out of QoS with
/// `pthread_setschedparam`, which none of ours do; a failure leaves the thread
/// as it was and the guard restores nothing.
#[must_use = "the class is restored when the scope is dropped"]
pub(crate) struct UtilityScope {
    #[cfg(target_os = "macos")]
    restore: Option<(u32, libc::c_int)>,
    /// What [`ORIGIN`] held before this scope, put back on drop.
    #[cfg(target_os = "macos")]
    outer_origin: Option<(u32, libc::c_int)>,
}

#[cfg(target_os = "macos")]
thread_local! {
    /// The class the thread had before the outermost [`UtilityScope`] lowered
    /// it, while one is active. Work nested inside a background scope that is
    /// itself user-facing runs at this class ([`UserFacingScope`]) and never
    /// above it.
    static ORIGIN: std::cell::Cell<Option<(u32, libc::c_int)>> =
        const { std::cell::Cell::new(None) };
}

/// `QOS_CLASS_UTILITY` as the raw value [`current_thread_qos`] reports.
#[cfg(target_os = "macos")]
pub(crate) const QOS_UTILITY: u32 = libc::qos_class_t::QOS_CLASS_UTILITY as u32;

/// The calling thread's requested QoS class and relative priority, as the raw
/// `u32` the kernel reports. `libc::qos_class_t` is a closed Rust enum, and
/// reading a class it does not list (the SDK has others, e.g. maintenance) as
/// that enum would be undefined behaviour, so the value is only ever held raw.
#[cfg(target_os = "macos")]
pub(crate) fn current_thread_qos() -> Option<(u32, libc::c_int)> {
    let mut class: u32 = 0;
    let mut priority: libc::c_int = 0;
    // SAFETY: `qos_class_t` is `repr(u32)`, so a `u32` out-pointer has the
    // layout the call writes; both out-pointers are live locals and
    // `pthread_self` is the calling thread.
    let rc = unsafe {
        libc::pthread_get_qos_class_np(
            libc::pthread_self(),
            std::ptr::from_mut(&mut class).cast::<libc::qos_class_t>(),
            &mut priority,
        )
    };
    (rc == 0).then_some((class, priority))
}

/// Requests `class` (one of the five classes `pthread_set_qos_class_self_np`
/// accepts) for the calling thread; `false` when the call refused.
#[cfg(target_os = "macos")]
fn set_current_thread_qos(class: u32, priority: libc::c_int) -> bool {
    use libc::qos_class_t as Q;
    let class = match class {
        c if c == Q::QOS_CLASS_USER_INTERACTIVE as u32 => Q::QOS_CLASS_USER_INTERACTIVE,
        c if c == Q::QOS_CLASS_USER_INITIATED as u32 => Q::QOS_CLASS_USER_INITIATED,
        c if c == Q::QOS_CLASS_UTILITY as u32 => Q::QOS_CLASS_UTILITY,
        c if c == Q::QOS_CLASS_BACKGROUND as u32 => Q::QOS_CLASS_BACKGROUND,
        // DEFAULT, and the unspecified class a bare `std` thread reports,
        // which cannot be requested back: the scheduler treats it as DEFAULT.
        _ => Q::QOS_CLASS_DEFAULT,
    };
    // SAFETY: affects only the calling thread; no pointers.
    unsafe { libc::pthread_set_qos_class_self_np(class, priority) == 0 }
}

impl UtilityScope {
    /// Lowers the calling thread to UTILITY for the scope, remembering the
    /// class it had so a nested user-facing scope can lift it back.
    pub(crate) fn enter() -> Self {
        #[cfg(target_os = "macos")]
        {
            let outer_origin = ORIGIN.get();
            let current = current_thread_qos().filter(|(class, _)| *class != QOS_UTILITY);
            let restore = current.filter(|_| set_current_thread_qos(QOS_UTILITY, 0));
            if restore.is_some() {
                ORIGIN.set(current);
            }
            Self {
                restore,
                outer_origin,
            }
        }
        #[cfg(not(target_os = "macos"))]
        Self {}
    }

    /// Lowers the calling thread to UTILITY for the rest of its life. Nothing
    /// is remembered or restored: a pool worker exists only to run this
    /// pool's tasks.
    pub(crate) fn lower_for_life() {
        #[cfg(target_os = "macos")]
        set_current_thread_qos(QOS_UTILITY, 0);
    }
}

impl Drop for UtilityScope {
    fn drop(&mut self) {
        #[cfg(target_os = "macos")]
        if let Some((class, priority)) = self.restore {
            set_current_thread_qos(class, priority);
            ORIGIN.set(self.outer_origin);
        }
    }
}

/// Lifts a thread that a surrounding [`UtilityScope`] lowered back to the
/// class it had, for a user-facing scope nested inside background work, and
/// lowers it again on drop. A no-op on a thread no scope lowered, so it never
/// raises a thread above what it originally had.
#[must_use = "the thread is lowered again when the scope is dropped"]
pub(crate) struct UserFacingScope {
    #[cfg(target_os = "macos")]
    lifted: bool,
}

impl UserFacingScope {
    pub(crate) fn enter() -> Self {
        #[cfg(target_os = "macos")]
        {
            let lifted = ORIGIN
                .get()
                .is_some_and(|(class, priority)| set_current_thread_qos(class, priority));
            Self { lifted }
        }
        #[cfg(not(target_os = "macos"))]
        Self {}
    }
}

impl Drop for UserFacingScope {
    fn drop(&mut self) {
        #[cfg(target_os = "macos")]
        if self.lifted {
            set_current_thread_qos(QOS_UTILITY, 0);
        }
    }
}

/// Rayon's global pool, sized and named, with every worker started at
/// `QOS_CLASS_UTILITY` for life. Its callers (`insights`, the dead-branch
/// scan) are bounded fan-outs; a `par_iter` hops threads, so the admission
/// class of the caller does not follow the work and the class has to be the
/// pool's own.
fn rayon_builder(threads: usize) -> rayon::ThreadPoolBuilder {
    rayon::ThreadPoolBuilder::new()
        .num_threads(threads)
        .thread_name(|index| format!("gitpulse-rayon-{index}"))
        .start_handler(|_| {
            UtilityScope::lower_for_life();
        })
}

/// Sizes rayon's global pool. Rayon builds the global pool on first use, so
/// this reports, rather than resizes, if something already used it.
pub(crate) fn install_rayon_pool() -> Option<String> {
    let threads = rayon_threads(logical_cpus());
    rayon_builder(threads)
        .build_global()
        .err()
        .map(|error| format!("rayon pool not sized to {threads} threads: {error}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn async_workers_are_clamped_to_two_through_four() {
        for (cpus, expected) in [
            (0, 2),
            (1, 2),
            (2, 2),
            (3, 3),
            (4, 4),
            (5, 4),
            (12, 4),
            (256, 4),
        ] {
            assert_eq!(async_workers(cpus), expected, "{cpus} cpus");
        }
    }

    #[test]
    fn rayon_gets_half_the_cores_rounded_up_and_at_least_one() {
        for (cpus, expected) in [(0, 1), (1, 1), (2, 1), (3, 2), (8, 4), (11, 6), (12, 6)] {
            assert_eq!(rayon_threads(cpus), expected, "{cpus} cpus");
        }
    }

    #[test]
    fn the_built_runtime_has_the_clamped_workers() {
        for cpus in [1usize, 8] {
            let runtime = build_async_runtime(cpus).expect("runtime");
            assert_eq!(
                runtime.handle().metrics().num_workers(),
                async_workers(cpus),
                "{cpus} cpus"
            );
            let answer = runtime.block_on(async { 6 * 7 });
            assert_eq!(answer, 42);
        }
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn rayon_workers_run_at_utility_qos() {
        let pool = rayon_builder(2).build().expect("rayon pool");
        let classes: Vec<_> = pool.install(|| {
            use rayon::prelude::*;
            (0..8)
                .into_par_iter()
                .map(|_| current_thread_qos().map(|(class, _)| class))
                .collect()
        });
        assert!(!classes.is_empty());
        for class in classes {
            assert_eq!(class, Some(QOS_UTILITY));
        }
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn a_utility_scope_lowers_the_thread_and_gives_the_class_back() {
        std::thread::spawn(|| {
            let class = || current_thread_qos().expect("qos").0;
            assert_ne!(class(), QOS_UTILITY);
            {
                let _scope = UtilityScope::enter();
                assert_eq!(class(), QOS_UTILITY);
            }
            assert_ne!(class(), QOS_UTILITY);
            assert_ne!(class(), libc::qos_class_t::QOS_CLASS_BACKGROUND as u32);
        })
        .join()
        .expect("scope thread");
    }

    #[test]
    fn the_blocking_pool_is_bounded() {
        let runtime = build_async_runtime(4).expect("runtime");
        let handle = runtime.handle().clone();
        let started = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let tasks: Vec<_> = (0..MAX_BLOCKING_THREADS + 8)
            .map(|_| {
                let started = started.clone();
                handle.spawn_blocking(move || {
                    started.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                    std::thread::sleep(std::time::Duration::from_millis(1500));
                })
            })
            .collect();
        // However long a loaded host takes to start the pool's threads, the
        // limit is reached and then held while the first bodies still sleep.
        let began = std::time::Instant::now();
        while started.load(std::sync::atomic::Ordering::SeqCst) < MAX_BLOCKING_THREADS {
            assert!(
                began.elapsed()
                    < crate::test_support::coverage_relaxed(std::time::Duration::from_secs(10)),
                "the blocking pool never reached {MAX_BLOCKING_THREADS} threads"
            );
            std::thread::sleep(std::time::Duration::from_millis(2));
        }
        std::thread::sleep(std::time::Duration::from_millis(100));
        assert_eq!(
            started.load(std::sync::atomic::Ordering::SeqCst),
            MAX_BLOCKING_THREADS,
            "only {MAX_BLOCKING_THREADS} blocking bodies may run at once; the rest wait"
        );
        runtime.block_on(async {
            for task in tasks {
                task.await.expect("blocking task");
            }
        });
    }
}
