//! Adversarial tests for lane W3-L2 (macOS QoS tiering). Test-only.

use crate::engine::git_cli::{
    run_bounded, with_admission, with_background_band, with_background_budget_wait,
    with_background_processes, Admission,
};
use crate::pools::{current_thread_qos, QOS_UTILITY};
use std::panic::{catch_unwind, AssertUnwindSafe};
use std::process::Command;
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

/// Records, once per 25 ms, the priority of the child and how long the parent
/// took to demote it. Prints the timeline: used to tell "demotion landed late"
/// from "demotion never happened".
#[test]
fn adv_flake_timeline_time_to_demotion() {
    let script = "t=0; while [ $t -lt 1200 ]; do \
        p=$(/bin/ps -o pri= -p $$ | tr -d ' '); \
        if [ \"$p\" -lt 10 ]; then echo demoted_after_samples=$t pri=$p; exit 0; fi; \
        sleep 0.025; t=$((t+1)); done; echo NEVER pri=$p";
    let mut cmd = Command::new("/bin/sh");
    cmd.arg("-c").arg(script);
    let started = Instant::now();
    let run = with_admission(Admission::Background, || {
        with_background_budget_wait(true, || {
            run_bounded(cmd, "adv-timeline", Duration::from_secs(120), None)
        })
    })
    .expect("run");
    let out = String::from_utf8_lossy(&run.stdout).to_string();
    eprintln!("TIMELINE {} wall={:?}", out.trim(), started.elapsed());
    assert!(out.contains("demoted"), "never demoted: {out}");
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

/// Grandchildren and exec inherit the band from a demoted child.
#[test]
fn adv_band_survives_exec_and_is_inherited_by_grandchildren() {
    let mut cmd = Command::new("/bin/sh");
    cmd.arg("-c").arg(
        "i=0; while [ $i -lt 1200 ]; do p=$(/bin/ps -o pri= -p $$ | tr -d ' '); \
         [ \"$p\" -lt 10 ] && break; sleep 0.05; i=$((i+1)); done; \
         /bin/sh -c 'echo grand=$(/bin/ps -o pri= -p $$ | tr -d \" \")'; \
         exec /bin/sh -c 'echo execd=$(/bin/ps -o pri= -p $$ | tr -d \" \")'",
    );
    let run = with_background_processes(|| {
        with_background_budget_wait(true, || {
            run_bounded(cmd, "adv-inherit", Duration::from_secs(120), None)
        })
    })
    .expect("run");
    let out = String::from_utf8_lossy(&run.stdout).to_string();
    eprintln!("INHERIT {}", out.replace('\n', " "));
    for part in out.split_whitespace() {
        let n: i32 = part.split('=').nth(1).unwrap().parse().unwrap();
        assert!(n < 10, "{part}");
    }
}
