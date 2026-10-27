//! Adversarial tests for lane W3-L2 (macOS QoS tiering). Test-only.

use crate::engine::git_cli::{
    band_outcomes, run_bounded, run_observed, with_admission, with_background_band,
    with_background_budget_wait, with_background_processes, Admission, OutputStream,
    ProcessObserver, MAX_OUTPUT_BYTES,
};
use crate::pools::{current_thread_qos, QOS_UTILITY};
use crate::procguard::LockedSpawn;
use std::panic::{catch_unwind, AssertUnwindSafe};
use std::process::Command;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{mpsc, Arc, Mutex};
use std::time::{Duration, Instant};

fn qos_class() -> u32 {
    current_thread_qos().expect("qos").0
}

/// Runs `f` on a fresh std thread, so the thread's starting QoS is the one a
/// bare `std` thread has.
fn on_fresh_thread<T: Send + 'static>(f: impl FnOnce() -> T + Send + 'static) -> T {
    std::thread::spawn(f).join().expect("thread")
}

#[test]
fn adv_restore_after_panic_returns_the_exact_starting_class() {
    on_fresh_thread(|| {
        let before = current_thread_qos().expect("qos");
        let caught = catch_unwind(AssertUnwindSafe(|| {
            with_admission(Admission::Background, || {
                assert_eq!(qos_class(), QOS_UTILITY);
                panic!("boom");
            })
        }));
        assert!(caught.is_err());
        assert_eq!(
            current_thread_qos().expect("qos"),
            before,
            "class after a panic differs from the class before"
        );
    });
}

#[test]
fn adv_restore_after_early_return_returns_the_exact_starting_class() {
    fn early() -> Result<(), &'static str> {
        with_admission(Admission::Background, || {
            assert_eq!(qos_class(), QOS_UTILITY);
            Err("early")?;
            Ok(())
        })
    }
    on_fresh_thread(|| {
        let before = current_thread_qos().expect("qos");
        assert!(early().is_err());
        assert_eq!(current_thread_qos().expect("qos"), before);
    });
}

/// A bare std thread reports an unspecified class. Restoring must hand back
/// what it had, not a different class.
#[test]
fn adv_restore_of_a_bare_thread_is_identical_not_merely_not_utility() {
    on_fresh_thread(|| {
        let before = current_thread_qos().expect("qos");
        with_admission(Admission::Background, || {});
        let after = current_thread_qos().expect("qos");
        eprintln!("bare thread before={before:?} after={after:?}");
        assert_eq!(before, after);
    });
}

/// Background inside Interactive inside Background.
#[test]
fn adv_nested_background_interactive_background() {
    on_fresh_thread(|| {
        let before = current_thread_qos().expect("qos");
        with_admission(Admission::Background, || {
            assert_eq!(qos_class(), QOS_UTILITY);
            with_admission(Admission::Interactive, || {
                // A user-initiated scope nested in a background one: is the
                // thread given back to a user-facing class for the span?
                eprintln!("interactive-in-background class={:#x}", qos_class());
                assert_ne!(
                    qos_class(),
                    QOS_UTILITY,
                    "Interactive scope runs on a UTILITY thread"
                );
                with_admission(Admission::Background, || {
                    assert_eq!(qos_class(), QOS_UTILITY);
                });
            });
            assert_eq!(qos_class(), QOS_UTILITY);
        });
        assert_eq!(current_thread_qos().expect("qos"), before);
    });
}

/// Interactive outside, Background inside, Interactive again: restores each.
#[test]
fn adv_nested_interactive_background_interactive() {
    on_fresh_thread(|| {
        let before = current_thread_qos().expect("qos");
        with_admission(Admission::Interactive, || {
            with_admission(Admission::Background, || {
                assert_eq!(qos_class(), QOS_UTILITY);
            });
            assert_eq!(current_thread_qos().expect("qos"), before);
        });
        assert_eq!(current_thread_qos().expect("qos"), before);
    });
}

/// A blocking-pool thread must never be left at UTILITY.
#[test]
fn adv_blocking_pool_thread_is_never_left_at_utility() {
    let runtime = crate::pools::build_async_runtime(4).expect("runtime");
    runtime.block_on(async {
        let mut tasks = Vec::new();
        for i in 0..200u32 {
            tasks.push(tokio::task::spawn_blocking(move || {
                if i % 3 == 0 {
                    // Early exits and panics inside the scope.
                    let _ = catch_unwind(AssertUnwindSafe(|| {
                        with_admission(Admission::Background, || {
                            if i % 2 == 0 {
                                panic!("pool panic")
                            }
                        })
                    }));
                    None
                } else {
                    Some(qos_class())
                }
            }));
        }
        let mut seen_utility = 0;
        for task in tasks {
            if task.await.expect("task") == Some(QOS_UTILITY) {
                seen_utility += 1;
            }
        }
        assert_eq!(seen_utility, 0, "a blocking thread leaked UTILITY");
    });
}

/// `with_background_band` is thread-local and balanced even across a panic.
#[test]
fn adv_band_scope_is_balanced_across_panic() {
    on_fresh_thread(|| {
        let caught = catch_unwind(AssertUnwindSafe(|| with_background_band(|| panic!("x"))));
        assert!(caught.is_err());
        let priority = child_priority(12, Admission::Reactive);
        assert!(priority >= 10, "band leaked past a panic: {priority}");
    });
}

fn probe(waits: u32) -> Command {
    let mut cmd = Command::new("/bin/sh");
    cmd.arg("-c").arg(format!(
        "i=0; while [ $i -lt {waits} ]; do \
         p=$(/bin/ps -o pri= -p $$ | tr -d ' '); \
         [ \"$p\" -lt 10 ] && break; sleep 0.05; i=$((i+1)); done; \
         /bin/ps -o pri= -p $$ | tr -d ' '"
    ));
    cmd
}

fn child_priority(waits: u32, class: Admission) -> i32 {
    let run = with_admission(class, || {
        with_background_budget_wait(true, || {
            run_bounded(probe(waits), "adv-probe", Duration::from_secs(120), None)
        })
    })
    .expect("probe");
    String::from_utf8_lossy(&run.stdout)
        .trim()
        .parse()
        .expect("pri")
}

/// `ps`'s priority and command name for `pid`, or `None` while `ps` cannot
/// answer or the process is gone.
fn ps_state(pid: u32) -> Option<(i32, String)> {
    let out = Command::new("/bin/ps")
        .args(["-o", "pri=,comm=", "-p", &pid.to_string()])
        .output_locked()
        .ok()?;
    let text = String::from_utf8_lossy(&out.stdout);
    let mut fields = text.split_whitespace();
    let pri = fields.next()?.parse().ok()?;
    Some((pri, fields.next()?.to_string()))
}

/// How many reads a pid outside the band gets to join it before its reading is
/// taken as it stands. A descendant is created outside the band and joins it
/// within milliseconds, so a few reads settle it; a count, not a duration, so
/// that a loaded host, whose reads are slower, waits correspondingly longer.
const SETTLE_READS: u32 = 100;

/// What the parent read of one child process once it had exec'd into `sleep`:
/// the priority the first read showed (`first`), the one it settled at (`pri`,
/// in the band unless the reading ran out of reads), and how many `ps` reads
/// that took. A descendant is created at the default priority and joins the
/// band a few milliseconds later, so `first` can be outside it.
#[derive(Clone, Debug)]
struct Reading {
    label: &'static str,
    pid: u32,
    first: i32,
    pri: i32,
    reads: u32,
}

/// A pid the child announces as `label=<pid>`, and the end of the command name
/// `ps` must show for it before it is read (`sh`: still the shell; `sleep`:
/// exec'd, or a forked descendant that has exec'd).
type Wanted = (&'static str, &'static str);

/// The pid a child printed as `label=<pid>` on a line of its own.
fn announced(text: &str, label: &str) -> Option<u32> {
    text.split_inclusive('\n')
        .filter(|line| line.ends_with('\n'))
        .find_map(|line| {
            line.trim_end()
                .strip_prefix(label)?
                .strip_prefix('=')?
                .parse()
                .ok()
        })
}

/// Reads, from this process, the scheduler priority of the pids a demoted
/// child announces on stdout.
///
/// The band is applied by the parent right after the spawn and before the wait
/// loop begins, so by the time a child's output reaches the loop its demotion
/// has already been decided. The old tests had the child watch itself with a
/// shell loop that forked `ps` and `tr` up to 1200 times, from a process the
/// band had just put at the lowest CPU and I/O priority: on a saturated host
/// the observer was the starved process, and the run ended at its 120 s
/// deadline. Here the reads come from a sampler thread that is started before
/// the admission scope (so it is not itself at UTILITY QoS), is told the pids
/// once every one is announced, and posts its readings for `cancelled` to see.
/// The 120 s run deadline is only a hang backstop: nothing asserted depends on
/// it.
struct BandWatch {
    wanted: &'static [Wanted],
    seen: String,
    announce: Option<mpsc::Sender<Vec<(Wanted, u32)>>>,
    stop: Arc<AtomicBool>,
    done: Arc<AtomicBool>,
    readings: Arc<Mutex<Vec<Reading>>>,
    last_read: Arc<Mutex<String>>,
    sampler: Option<std::thread::JoinHandle<()>>,
}

impl BandWatch {
    fn start(wanted: &'static [Wanted]) -> Self {
        let (announce, targets) = mpsc::channel::<Vec<(Wanted, u32)>>();
        let stop = Arc::new(AtomicBool::new(false));
        let done = Arc::new(AtomicBool::new(false));
        let readings = Arc::new(Mutex::new(Vec::new()));
        let last_read = Arc::new(Mutex::new(String::from("nothing was announced")));
        let sampler = {
            let (stop, done, readings, last_read) = (
                stop.clone(),
                done.clone(),
                readings.clone(),
                last_read.clone(),
            );
            std::thread::spawn(move || {
                let Ok(pids) = targets.recv() else { return };
                for ((label, image), pid) in pids {
                    let mut reads = 0;
                    let mut first = None;
                    let reading = loop {
                        if stop.load(Ordering::Acquire) {
                            return;
                        }
                        if let Some((pri, comm)) = ps_state(pid) {
                            reads += 1;
                            *last_read.lock().unwrap() =
                                format!("{label} pid={pid} pri={pri} comm={comm} reads={reads}");
                            // Once the pid runs the image the case is about:
                            // the shell itself for a child that never execs,
                            // `sleep` after an exec or for a forked
                            // descendant, so what is read is not the image
                            // before it.
                            if comm.ends_with(image) {
                                let first = *first.get_or_insert(pri);
                                if pri < 10 || reads >= SETTLE_READS {
                                    break Reading {
                                        label,
                                        pid,
                                        first,
                                        pri,
                                        reads,
                                    };
                                }
                            }
                        }
                        std::thread::sleep(Duration::from_millis(10));
                    };
                    readings.lock().unwrap().push(reading);
                }
                done.store(true, Ordering::Release);
            })
        };
        Self {
            wanted,
            seen: String::new(),
            announce: Some(announce),
            stop,
            done,
            readings,
            last_read,
            sampler: Some(sampler),
        }
    }
}

impl ProcessObserver for BandWatch {
    /// False until every wanted pid has been announced and read, so the run
    /// is also not cancelled while it is still queued for admission.
    fn cancelled(&self) -> bool {
        self.done.load(Ordering::Acquire)
    }

    fn output(&mut self, stream: OutputStream, bytes: &[u8]) {
        if !matches!(stream, OutputStream::Stdout) {
            return;
        }
        self.seen.push_str(&String::from_utf8_lossy(bytes));
        let pids: Vec<_> = self
            .wanted
            .iter()
            .filter_map(|wanted| announced(&self.seen, wanted.0).map(|pid| (*wanted, pid)))
            .collect();
        if pids.len() == self.wanted.len() {
            if let Some(announce) = self.announce.take() {
                let _ = announce.send(pids);
            }
        }
    }
}

impl Drop for BandWatch {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Release);
        self.announce.take();
        if let Some(sampler) = self.sampler.take() {
            let _ = sampler.join();
        }
    }
}

/// Runs `script` as a background-band child, feeding it `stdin` after the
/// demotion, and returns what the parent read of each pid in `wanted`.
fn watched_band_run(
    label: &str,
    script: &str,
    stdin: Option<&[u8]>,
    wanted: &'static [Wanted],
) -> Vec<Reading> {
    let mut watch = BandWatch::start(wanted);
    let mut cmd = Command::new("/bin/sh");
    cmd.arg("-c").arg(script);
    let (entered, failed) = band_outcomes();
    let run = with_background_processes(|| {
        with_background_budget_wait(true, || {
            run_observed(
                &mut cmd,
                label,
                Duration::from_secs(120),
                stdin,
                MAX_OUTPUT_BYTES,
                &mut watch,
            )
        })
    });
    let (entered_after, failed_after) = band_outcomes();
    let last = watch.last_read.lock().unwrap().clone();
    let run = run.unwrap_or_else(|e| panic!("{}; last read by the parent: {last}", e.message()));
    assert!(run.cancelled, "{label} ended on its own; last read: {last}");
    assert_eq!(
        failed_after, failed,
        "{label}: the OS refused a demotion; last read: {last}"
    );
    assert!(
        entered_after > entered,
        "{label}: no child entered the band; last read: {last}"
    );
    let readings = watch.readings.lock().unwrap().clone();
    assert_eq!(readings.len(), wanted.len(), "{label}: {readings:?}");
    readings
}

fn assert_in_band(readings: &[Reading], what: &str) {
    for reading in readings {
        assert!(
            reading.pri < 10,
            "{} (pid {}) {what}: pri {} on the first read, {} after {} reads",
            reading.label,
            reading.pid,
            reading.first,
            reading.pri,
            reading.reads
        );
    }
}

/// The timeline case: a demoted child that never execs (it stays the shell,
/// waiting on a foreground `sleep`) is at the background priority when its
/// parent reads it, however loaded the host is. The child waits for a line on
/// stdin, which the run feeds only after the demotion, so the demotion is
/// already applied when it prints its pid. Its sibling `adv_band_survives_exec`
/// is the other case: the same child after an exec.
#[test]
fn adv_flake_timeline_time_to_demotion() {
    let started = Instant::now();
    let readings = watched_band_run(
        "adv-timeline",
        "read go; echo self=$$; /bin/sleep 600",
        Some(b"go\n"),
        &[("self", "sh")],
    );
    eprintln!("TIMELINE {readings:?} wall={:?}", started.elapsed());
    assert_in_band(&readings, "never demoted");
}

/// PRODUCTION DEFECT ft-b597add5 (gp-b26-background-band-race), kept as a
/// repro: a child that execs before the parent's
/// `setpriority` lands is left at the default priority for good. The band is
/// applied after the spawn returns (`enter_background_band`), and a child that
/// replaces its image at once, as `sh -c 'exec tool'` does, can pass through
/// its exec while the demotion is in flight; it then reads pri 31 and still
/// reads 31 half a second later (2 of 80 runs of the module at load 15 to 40
/// on 2026-10-26; the same was seen 1 in 10 at load 72 as a shell that never
/// reached the band in 81 s). It also hits a child that waits for stdin before
/// it execs (`adv_band_survives_exec`, `adv_flake_timeline_time_to_demotion`
/// failed 5 of 80 runs of the module at load 10 to 50 with 18 hogs): the band
/// was never in effect at all. Alone these repros pass (0 of 180 children); the
/// other tests' concurrent spawns are what trigger it, so run them together:
/// `--lib adv_w3l2_tests:: -- --include-ignored` (3 of 6 runs failed).
#[test]
#[ignore = "break: a child that execs immediately can miss the background band for good"]
fn adv_break_child_that_execs_at_once_misses_the_band() {
    let mut missed = 0;
    for _ in 0..60 {
        let readings = watched_band_run(
            "adv-race",
            "echo self=$$; exec /bin/sleep 600",
            None,
            &[("self", "sleep")],
        );
        missed += readings.iter().filter(|r| r.pri >= 10).count();
    }
    assert_eq!(
        missed, 0,
        "{missed} of 60 children that exec at once never joined the band"
    );
}

/// Gap between `posix_spawn` returning and the demotion returning, on a thread
/// at UTILITY (what `with_admission(Background)` makes it) versus a default
/// thread, and every error `enter_background_band` surfaces. Under load the
/// gap is the window in which the child is NOT demoted.
#[test]
fn adv_demotion_gap_and_errors_on_a_utility_thread() {
    use crate::procguard::{enter_background_band, spawn};
    use std::process::Stdio;
    fn gaps(n: usize) -> (Vec<Duration>, usize) {
        let mut out = Vec::new();
        let mut errors = 0;
        for _ in 0..n {
            let mut cmd = Command::new("/bin/sleep");
            cmd.arg("5").stdin(Stdio::null()).stdout(Stdio::null());
            let (mut child, guard) = spawn(&mut cmd, "adv-gap").expect("spawn");
            let t = Instant::now();
            if enter_background_band(&child).is_err() {
                errors += 1;
            }
            out.push(t.elapsed());
            guard.kill_tree(&mut child);
            let _ = guard.reap(|| child.wait());
        }
        (out, errors)
    }
    let report = |name: &str, mut v: Vec<Duration>, errors: usize| {
        v.sort();
        eprintln!(
            "GAP {name}: n={} p50={:?} p95={:?} max={:?} errors={errors}",
            v.len(),
            v[v.len() / 2],
            v[v.len() * 95 / 100],
            v[v.len() - 1]
        );
        assert_eq!(errors, 0);
    };
    let (default_gaps, e1) = on_fresh_thread(|| gaps(100));
    report("default-thread", default_gaps, e1);
    let (utility_gaps, e2) =
        on_fresh_thread(|| with_admission(Admission::Background, || gaps(100)));
    report("utility-thread", utility_gaps, e2);
}

/// A demoted child keeps the band through its own exec. The child waits for a
/// line on stdin, which the run feeds only after the demotion, so the exec
/// comes after it by construction.
#[test]
fn adv_band_survives_exec() {
    let readings = watched_band_run(
        "adv-exec",
        "read go; echo self=$$; exec /bin/sleep 600",
        Some(b"go\n"),
        &[("self", "sleep")],
    );
    eprintln!("EXEC {readings:?}");
    assert_in_band(&readings, "left the band across exec");
}

/// PRODUCTION DEFECT ft-b597add5 (gp-b26-background-band-race), kept as a
/// repro: a grandchild forked by an already
/// demoted child is usually in the band (a few milliseconds after it is
/// created, as `first` shows) but sometimes never joins it. The child waits for
/// a line on stdin that is fed only after the demotion, so the fork is after it
/// by construction. Seen as `grand pid=... pri=31 ... reads=8115` for the whole
/// 120 s run in 1 of 40 runs of `adv_band_survives_exec_and_is_inherited_by_
/// grandchildren` (the test this one was split from) at load 15 to 20, and 4
/// of 60 as a first read of 31 or 46 before it settled. Like its sibling it
/// needs the module's other tests spawning beside it: `--lib adv_w3l2_tests::
/// -- --include-ignored`.
#[test]
#[ignore = "break: a grandchild of a demoted child can stay outside the background band"]
fn adv_break_grandchild_of_a_demoted_child_misses_the_band() {
    let mut missed = 0;
    for _ in 0..60 {
        let readings = watched_band_run(
            "adv-grand",
            "read go; /bin/sleep 600 & echo grand=$!; echo self=$$; exec /bin/sleep 600",
            Some(b"go\n"),
            &[("grand", "sleep"), ("self", "sleep")],
        );
        missed += readings.iter().filter(|r| r.pri >= 10).count();
    }
    assert_eq!(
        missed, 0,
        "{missed} of 60 demoted children's pids never joined the band"
    );
}
