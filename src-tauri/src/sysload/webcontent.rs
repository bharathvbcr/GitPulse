//! A bounded ring of the WebKit content processes' footprints, so a content
//! process the OS or WebKit kills leaves its last readings behind.
//!
//! WebContent processes are XPC services launched under launchd (their parent
//! is pid 1), not children of the app, so they are found by executable path
//! (`…/com.apple.WebKit.WebContent`), not by ancestry. Another app's content
//! processes match too; the ring keeps them all and the dump narrows to the
//! pid the unified log names when it names one.
//!
//! Sampled from the app sampler's tick (`sampler::run`), every
//! [`SAMPLE_EVERY`], only while host-load sampling is enabled and only in the
//! app process. By reading the code (inferred, not observed with a hidden
//! window) the tick is a plain thread on a fixed grid, not tied to window
//! visibility, and holds an App Nap opt-out while enabled, so a hidden window
//! should keep being sampled; a tick that runs late just leaves a longer gap,
//! which the dump shows as its own `T-` offsets.

use std::collections::VecDeque;
#[cfg(target_os = "macos")]
use std::sync::Mutex;
use std::time::{Duration, Instant};

/// Executable-path marker of a WebKit content process.
#[cfg(any(target_os = "macos", test))]
pub(crate) const MARKER: &str = "com.apple.WebKit.WebContent";
/// Samples kept: 20 x 5 s is the last 100 s.
#[cfg(any(target_os = "macos", test))]
pub(crate) const RING_TICKS: usize = 20;
/// Content processes recorded per sample. Every app's are in the table (17
/// were running on the host this was written on), so the bound sits well above
/// that; more are counted, not recorded.
#[cfg(any(target_os = "macos", test))]
pub(crate) const MAX_PROCS: usize = 64;
/// Gap between samples.
pub(crate) const SAMPLE_EVERY: Duration = Duration::from_secs(5);
/// Lines a dump carries.
#[cfg(any(target_os = "macos", test))]
pub(crate) const MAX_DUMP_LINES: usize = 40;

/// One content process at one sample. `None` where the OS would not say.
#[cfg(any(target_os = "macos", test))]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct ProcSample {
    pub pid: i32,
    pub footprint: Option<u64>,
    pub cpu_ns: Option<u64>,
}

#[cfg(any(target_os = "macos", test))]
#[derive(Debug, Clone)]
struct Tick {
    at: Instant,
    procs: Vec<ProcSample>,
    /// Content processes beyond [`MAX_PROCS`] that were not recorded.
    unrecorded: usize,
}

/// The ring and its health counters.
#[cfg(any(target_os = "macos", test))]
#[derive(Clone)]
pub(crate) struct Ring {
    ticks: VecDeque<Tick>,
    /// Samples taken, ever.
    taken: u64,
    /// Samples that could not be taken, and the last reason.
    failed: u64,
    last_failure: Option<String>,
}

/// A formatted dump: lines oldest first, and what to know about them.
#[cfg(any(target_os = "macos", test))]
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Dump {
    pub lines: Vec<String>,
    pub note: String,
}

#[cfg(any(target_os = "macos", test))]
impl Ring {
    pub(crate) const fn new() -> Self {
        Self {
            ticks: VecDeque::new(),
            taken: 0,
            failed: 0,
            last_failure: None,
        }
    }

    pub(crate) fn record(&mut self, at: Instant, mut procs: Vec<ProcSample>) {
        let unrecorded = procs.len().saturating_sub(MAX_PROCS);
        procs.truncate(MAX_PROCS);
        self.ticks.push_back(Tick {
            at,
            procs,
            unrecorded,
        });
        while self.ticks.len() > RING_TICKS {
            self.ticks.pop_front();
        }
        self.taken += 1;
    }

    pub(crate) fn record_failure(&mut self, reason: String) {
        self.failed += 1;
        self.last_failure = Some(reason);
    }

    pub(crate) fn len(&self) -> usize {
        self.ticks.len()
    }

    /// The last samples as `pid X, footprint Y MB, cpu Z s at T-Ns`, oldest
    /// first, for `pid` when given and sampled, else every process.
    pub(crate) fn dump(&self, now: Instant, pid: Option<u32>) -> Dump {
        let health = match &self.last_failure {
            Some(reason) => format!(
                "; {} of {} sample(s) failed, last: {reason}",
                self.failed,
                self.taken + self.failed
            ),
            None => String::new(),
        };
        if self.ticks.is_empty() {
            return Dump {
                lines: Vec::new(),
                note: format!(
                    "no WebContent samples exist: the sampler had not ticked, host-load \
                     sampling was off, or this is not macOS{health}"
                ),
            };
        }
        let wanted = pid.and_then(|p| i32::try_from(p).ok());
        let seen = wanted.is_some_and(|w| {
            self.ticks
                .iter()
                .any(|t| t.procs.iter().any(|p| p.pid == w))
        });
        let filter = if seen { wanted } else { None };
        let mut lines = Vec::new();
        let mut unrecorded = 0;
        for tick in &self.ticks {
            unrecorded = unrecorded.max(tick.unrecorded);
            let age = now.saturating_duration_since(tick.at).as_secs();
            for p in tick
                .procs
                .iter()
                .filter(|p| filter.is_none_or(|w| p.pid == w))
            {
                lines.push(format!(
                    "pid {}, footprint {}, cpu {} at T-{age}s",
                    p.pid,
                    p.footprint.map_or_else(
                        || "unreadable".to_string(),
                        |b| format!("{:.1} MB", b as f64 / (1024.0 * 1024.0))
                    ),
                    p.cpu_ns.map_or_else(
                        || "unreadable".to_string(),
                        |ns| format!("{:.1} s", ns as f64 / 1e9)
                    ),
                ));
            }
        }
        let capped = lines.len().saturating_sub(MAX_DUMP_LINES);
        lines.drain(..capped);
        let scope = match (pid, seen) {
            (Some(p), true) => format!("pid {p} only"),
            (Some(p), false) => {
                format!("pid {p} was never sampled; every sampled WebContent process shown")
            }
            (None, _) => "every sampled WebContent process".to_string(),
        };
        let extra = if unrecorded > 0 {
            format!("; up to {unrecorded} more content process(es) per sample were not recorded")
        } else {
            String::new()
        };
        let cut = if capped > 0 {
            format!("; the oldest {capped} line(s) were dropped")
        } else {
            String::new()
        };
        Dump {
            lines,
            note: format!(
                "{scope}, {} sample(s) of {}s apart{extra}{cut}{health}",
                self.len(),
                SAMPLE_EVERY.as_secs()
            ),
        }
    }
}

#[cfg(target_os = "macos")]
static RING: Mutex<Ring> = Mutex::new(Ring::new());

#[cfg(target_os = "macos")]
fn ring() -> std::sync::MutexGuard<'static, Ring> {
    RING.lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

/// A copy of the ring as it is now, so a dump later still shows what was
/// there at the moment of interest, not what the next ticks added.
#[cfg(target_os = "macos")]
pub(crate) fn snapshot() -> Ring {
    ring().clone()
}

/// Content processes among `pids`, each read with `usage`. A process `usage`
/// cannot read is kept as a sample with `None` readings, never dropped.
#[cfg(any(target_os = "macos", test))]
pub(crate) fn collect(
    pids: &[i32],
    path_of: impl Fn(i32) -> Option<String>,
    usage: impl Fn(i32) -> Option<(u64, u64)>,
) -> Vec<ProcSample> {
    pids.iter()
        .copied()
        .filter(|pid| *pid > 0)
        .filter(|pid| path_of(*pid).is_some_and(|path| path.contains(MARKER)))
        .map(|pid| {
            let read = usage(pid);
            ProcSample {
                pid,
                footprint: read.map(|(f, _)| f),
                cpu_ns: read.map(|(_, c)| c),
            }
        })
        .collect()
}

/// Reads every WebContent process now and adds the sample to the ring.
#[cfg(target_os = "macos")]
pub(crate) fn sample_now() {
    let sample = super::procscan::all_pids().map(|pids| {
        collect(
            &pids,
            super::procscan::exec_path_of,
            super::procscan::footprint_cpu_of,
        )
    });
    let mut ring = ring();
    match sample {
        Ok(procs) => ring.record(Instant::now(), procs),
        Err(reason) => ring.record_failure(reason),
    }
}

#[cfg(not(target_os = "macos"))]
pub(crate) fn sample_now() {}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample(pid: i32, mb: u64, cpu_s: u64) -> ProcSample {
        ProcSample {
            pid,
            footprint: Some(mb * 1024 * 1024),
            cpu_ns: Some(cpu_s * 1_000_000_000),
        }
    }

    #[test]
    fn the_ring_never_holds_more_than_its_bound() {
        let mut ring = Ring::new();
        let start = Instant::now();
        for step in 0..(RING_TICKS as u64 * 3) {
            ring.record(start + Duration::from_secs(step), vec![sample(1, step, 1)]);
        }
        assert_eq!(ring.len(), RING_TICKS);
        // The newest survive: the first line left is sample 40, not 0.
        let dump = ring.dump(start + Duration::from_secs(100), Some(1));
        assert!(
            dump.lines[0].contains("footprint 40.0 MB"),
            "{:?}",
            dump.lines
        );
    }

    #[test]
    fn one_sample_records_a_bounded_number_of_processes() {
        let mut ring = Ring::new();
        let procs = (1..=(MAX_PROCS as i32 + 5))
            .map(|p| sample(p, 1, 1))
            .collect();
        ring.record(Instant::now(), procs);
        assert_eq!(ring.ticks[0].procs.len(), MAX_PROCS);
        let dump = ring.dump(Instant::now(), None);
        // The dump has its own, lower bound.
        assert_eq!(dump.lines.len(), MAX_DUMP_LINES);
        assert!(
            dump.note.contains("oldest 24 line(s) were dropped"),
            "{}",
            dump.note
        );
        assert!(
            dump.note.contains("5 more content process"),
            "{}",
            dump.note
        );
    }

    #[test]
    fn a_dump_reads_pid_footprint_cpu_and_seconds_before() {
        let mut ring = Ring::new();
        let t0 = Instant::now();
        ring.record(t0, vec![sample(1591, 812, 45), sample(2000, 5, 1)]);
        let dump = ring.dump(t0 + Duration::from_secs(5), Some(1591));
        assert_eq!(
            dump.lines,
            ["pid 1591, footprint 812.0 MB, cpu 45.0 s at T-5s"]
        );
        assert!(dump.note.starts_with("pid 1591 only"), "{}", dump.note);
    }

    #[test]
    fn a_pid_never_sampled_says_so_and_shows_the_others() {
        let mut ring = Ring::new();
        let t0 = Instant::now();
        ring.record(t0, vec![sample(7, 1, 1)]);
        let dump = ring.dump(t0, Some(1591));
        assert_eq!(dump.lines.len(), 1);
        assert!(
            dump.note.contains("pid 1591 was never sampled"),
            "{}",
            dump.note
        );
    }

    #[test]
    fn an_empty_ring_is_not_a_clean_bill() {
        let mut ring = Ring::new();
        let none = ring.dump(Instant::now(), Some(1));
        assert!(none.lines.is_empty());
        assert!(
            none.note.contains("no WebContent samples exist"),
            "{}",
            none.note
        );
        ring.record_failure("process table unreadable".into());
        let failed = ring.dump(Instant::now(), None);
        assert!(
            failed.note.contains("process table unreadable"),
            "{}",
            failed.note
        );
    }

    #[test]
    fn an_unreadable_process_is_unreadable_never_zero() {
        let procs = collect(
            &[10, 11],
            |_| Some("/x/com.apple.WebKit.WebContent.xpc/Contents/MacOS/c".into()),
            |pid| (pid == 10).then_some((2 * 1024 * 1024, 3_000_000_000)),
        );
        let mut ring = Ring::new();
        ring.record(Instant::now(), procs);
        let dump = ring.dump(Instant::now(), None);
        assert!(
            dump.lines[1].contains("footprint unreadable"),
            "{:?}",
            dump.lines
        );
    }

    #[test]
    fn only_content_processes_are_collected() {
        let procs = collect(
            &[-1, 0, 5, 6],
            |pid| {
                Some(if pid == 6 {
                    "/System/Library/Frameworks/WebKit.framework/XPCServices/com.apple.WebKit.WebContent.xpc/Contents/MacOS/com.apple.WebKit.WebContent".into()
                } else {
                    "/bin/zsh".into()
                })
            },
            |_| Some((1, 1)),
        );
        assert_eq!(procs.iter().map(|p| p.pid).collect::<Vec<_>>(), [6]);
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn the_real_sampler_path_records_a_sample_or_a_failure() {
        let (taken, failed) = {
            let ring = ring();
            (ring.taken, ring.failed)
        };
        sample_now();
        let ring = ring();
        assert_eq!(
            (ring.taken + ring.failed),
            taken + failed + 1,
            "a tick is either a sample (even an empty one) or a recorded failure"
        );
    }
}
