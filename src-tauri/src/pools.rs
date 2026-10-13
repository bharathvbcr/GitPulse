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
//! Rayon's pool has no start handler here. A start handler is where a thread
//! would be put in a lower scheduling class, which is the QoS work held by the
//! governor lane; installing an empty one would only look like that work.

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

/// Sizes rayon's global pool. Rayon builds the global pool on first use, so
/// this reports, rather than resizes, if something already used it.
pub(crate) fn install_rayon_pool() -> Option<String> {
    let threads = rayon_threads(logical_cpus());
    rayon::ThreadPoolBuilder::new()
        .num_threads(threads)
        .thread_name(|index| format!("gitpulse-rayon-{index}"))
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
