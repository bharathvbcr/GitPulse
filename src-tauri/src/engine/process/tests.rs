use super::*;
use crate::ipc_error::CommandError;
#[cfg(unix)]
use crate::procguard::LockedSpawn;
use crate::sysload::Binding;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::Command;
#[cfg(unix)]
use std::process::Stdio;
use std::sync::{mpsc, Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

/// Characterizes the retired channel handoff in regression tests. Production
/// owns captured prefixes independently of worker completion on every platform.
#[cfg(test)]
fn collect_drained_deadline(rx: &mpsc::Receiver<Drained>, deadline: Instant) -> Drained {
    let remaining = deadline.saturating_duration_since(Instant::now());
    rx.recv_timeout(remaining).unwrap_or_else(|e| Drained {
        // Emphatically not `Default`: an empty `Vec` with no stop reason says
        // "the child produced nothing", which is a real and common answer. A
        // drain that never delivered produced *unknown*, and the two must not
        // arrive as the same value.
        stop: Some(Stop::Undelivered(format!(
            "output reader did not finish: {e}"
        ))),
        ..Drained::default()
    })
}

#[cfg(test)]
fn drain_capped<R: Read>(pipe: Option<R>, max_bytes: usize) -> Drained {
    let mut out = Drained::default();
    let Some(mut pipe) = pipe else {
        return out;
    };
    let mut tmp = [0u8; 16_384];
    loop {
        match pipe.read(&mut tmp) {
            Ok(0) => break,
            Ok(n) => out.append(&tmp[..n], max_bytes),
            Err(e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
            // Not end-of-stream. Breaking here and handing back the prefix as
            // if the child had simply said less is how a half-read `git show`
            // reached a parser as a complete record and came back as "failed
            // to parse commit metadata format" — a read that broke reported as
            // a read that finished.
            Err(e) => {
                out.stop = Some(Stop::Broken(e.to_string()));
                break;
            }
        }
    }
    out
}

/// F-10. Decoding captured output that is valid UTF-8 hands the buffer
/// over: the text is over the allocation the process read into, not a
/// second copy of it. The old expression is asserted to copy, so this
/// test discriminates.
#[test]
fn decoding_valid_captured_output_does_not_copy_the_buffer() {
    let bytes = "commit \u{e9}\u{4e16}\n".repeat(4096).into_bytes();
    let at = bytes.as_ptr();
    let old = String::from_utf8_lossy(&bytes).into_owned();
    assert_ne!(old.as_ptr(), at, "the previous decode copied");
    let text = super::text_from_bytes(bytes);
    assert_eq!(text.as_ptr(), at, "valid UTF-8 was copied again");
    assert_eq!(text, old);
}

/// Invalid bytes still decode the way `from_utf8_lossy` does, once.
#[test]
fn decoding_invalid_captured_output_replaces_the_bad_bytes() {
    let bytes = b"ok \xff\xfe tail \xe4\xb8".to_vec();
    let expected = String::from_utf8_lossy(&bytes).into_owned();
    assert_eq!(super::text_from_bytes(bytes), expected);
    assert!(expected.contains('\u{fffd}'));
    assert_eq!(super::text_from_bytes(Vec::new()), "");
}

#[test]
fn background_priority_restores_after_nested_work_errors_and_panic() {
    use super::{current_admission, Admission};
    assert_eq!(current_admission(), Admission::Reactive);
    let result = std::panic::catch_unwind(|| {
        super::with_background_processes(|| {
            assert_eq!(current_admission(), Admission::Background);
            let error: Result<(), &str> = super::with_background_processes(|| Err("expected"));
            assert_eq!(error, Err("expected"));
            assert_eq!(current_admission(), Admission::Background);
            panic!("expected priority unwind");
        })
    });
    assert!(result.is_err());
    assert_eq!(current_admission(), Admission::Reactive);
    super::with_background_processes(|| {
        std::thread::spawn(|| assert_eq!(current_admission(), Admission::Reactive))
            .join()
            .unwrap();
    });
    assert_eq!(current_admission(), Admission::Reactive);
}

/// The command guard promotes a mutation for the rest of its command and
/// no further: the runner's scope ends the promotion, panics included, so
/// a pooled thread never carries a user-action class into the next task.
#[test]
fn a_guarded_mutation_is_interactive_until_its_command_scope_ends() {
    use super::{current_admission, mark_interactive, with_admission, Admission};
    with_admission(Admission::Reactive, || {
        assert_eq!(current_admission(), Admission::Reactive);
        mark_interactive();
        assert_eq!(current_admission(), Admission::Interactive);
    });
    assert_eq!(current_admission(), Admission::Reactive);
    let unwound = std::panic::catch_unwind(|| {
        with_admission(Admission::Reactive, || {
            mark_interactive();
            panic!("mutation failed");
        })
    });
    assert!(unwound.is_err());
    assert_eq!(current_admission(), Admission::Reactive);
}

/// The launch failure: refresh traffic across ~20 repositories spent the
/// rate budget, and the user's own actions were refused with it. A user
/// action is limited by concurrency alone, and spends no token a refresh
/// would need.
#[test]
fn a_user_action_is_admitted_after_refreshes_spend_every_token() {
    use super::{with_admission, Admission};
    let gate = super::SpawnGate::with_rate(8, 2, 1);
    let quick = || Instant::now() + std::time::Duration::from_millis(30);
    let burst: Vec<_> = (0..2)
        .map(|_| gate.acquire(quick()).expect("burst"))
        .collect();
    drop(burst);
    assert!(
        gate.acquire(quick()).is_none(),
        "the refresh budget is spent"
    );
    let actions: Vec<_> = (0..4)
        .map(|_| {
            with_admission(Admission::Interactive, || gate.acquire(quick()))
                .expect("a user action must not wait for a rate token")
        })
        .collect();
    assert_eq!(gate.state.lock().unwrap().tokens, 0);
    drop(actions);
    let counters = gate.counters();
    assert_eq!(counters[Admission::Interactive.index()].admitted, 4);
    assert_eq!(counters[Admission::Interactive.index()].refused, 0);
    assert_eq!(counters[Admission::Reactive.index()].admitted, 2);
    assert_eq!(counters[Admission::Reactive.index()].refused, 1);

    // Still concurrency-limited: a user action is not a way past the
    // descriptor budget the gate exists for.
    let full: Vec<_> = (0..8)
        .map(|_| with_admission(Admission::Interactive, || gate.acquire(quick())).expect("slot"))
        .collect();
    assert!(with_admission(Admission::Interactive, || gate.acquire(quick())).is_none());
    drop(full);
}

/// Background work goes first: once the budget is at the reserve kept for
/// refreshes, it is refused at once rather than queued, while a refresh
/// is still admitted from the reserve.
#[test]
fn background_is_shed_at_the_reserve_while_a_refresh_still_runs() {
    use super::{with_admission, with_background_processes, Admission, Refusal};
    let gate = super::SpawnGate::with_rate(16, 8, 1);
    assert_eq!(gate.background_reserve(), 2);
    let far = || Instant::now() + std::time::Duration::from_secs(10);
    for _ in 0..6 {
        drop(with_background_processes(|| gate.acquire(far())).expect("above reserve"));
    }
    let began = Instant::now();
    let shed = with_background_processes(|| gate.acquire_until(far(), &|| false).err());
    assert_eq!(shed, Some(Refusal::Shed));
    assert!(
        began.elapsed()
            < crate::test_support::coverage_relaxed(std::time::Duration::from_millis(500)),
        "shedding must not wait out the budget: {:?}",
        began.elapsed()
    );
    assert!(
        with_admission(Admission::Reactive, || gate.acquire(far())).is_some(),
        "the reserve is for refreshes"
    );
    assert_eq!(gate.counters()[Admission::Background.index()].shed, 1);
}

/// An agent's MCP read is background work that waits: at the reserve it is
/// paced by the refill, not shed at 0.000s for a condition that clears in
/// a fraction of a second. Shed work (the default, and the scheduled
/// daemon) is the control, in the same gate and state.
#[test]
fn awaiting_background_work_is_paced_by_the_refill_where_shed_work_is_refused() {
    use super::{with_background_budget_wait, with_background_processes, Admission, Refusal};
    let gate = super::SpawnGate::with_rate(16, 8, 8);
    assert_eq!(gate.background_reserve(), 2);
    let far = || Instant::now() + std::time::Duration::from_secs(10);
    for _ in 0..6 {
        drop(with_background_processes(|| gate.acquire(far())).expect("above reserve"));
    }
    // Control: at the reserve, shed work is refused at once.
    let shed = with_background_processes(|| gate.acquire_until(far(), &|| false).err());
    assert_eq!(shed, Some(Refusal::Shed));
    // The same spawn, waiting: the next token (125 ms at 8/s) is admitted.
    let began = Instant::now();
    let permit = with_background_budget_wait(true, || {
        with_background_processes(|| gate.acquire_until(far(), &|| false))
    });
    assert!(
        permit.is_ok(),
        "a waiting background read was refused: {:?}",
        permit.err()
    );
    assert!(
        began.elapsed()
            < crate::test_support::coverage_relaxed(std::time::Duration::from_millis(1_900)),
        "paced by the refill, not by the whole wait budget: {:?}",
        began.elapsed()
    );
    let counters = gate.counters()[Admission::Background.index()];
    assert_eq!(counters.shed, 1, "only the control was shed");
}

/// Waiting is bounded and never spends the reserve: with the budget empty
/// and a refill too slow to clear the reserve inside the wait budget, the
/// waiting read is refused after the same 2 s a reactive read gets, and a
/// reactive read still has its token.
#[test]
fn awaiting_background_work_waits_a_bounded_time_and_leaves_the_reserve() {
    use super::{with_admission, with_background_budget_wait, with_background_processes};
    use super::{Admission, Refusal};
    let gate = super::SpawnGate::with_rate(16, 64, 1);
    assert_eq!(gate.background_reserve(), 16);
    let far = || Instant::now() + std::time::Duration::from_secs(30);
    for _ in 0..64 {
        drop(with_admission(Admission::Reactive, || gate.acquire(far())).expect("burst"));
    }
    let began = Instant::now();
    let refusal = with_background_budget_wait(true, || {
        with_background_processes(|| gate.acquire_until(far(), &|| false).err())
    });
    assert!(
        matches!(refusal, Some(Refusal::Refused { .. })),
        "expected a bounded wait that ends refused, got {refusal:?}"
    );
    assert!(
        began.elapsed() >= std::time::Duration::from_millis(1_900),
        "gave up before the wait budget: {:?}",
        began.elapsed()
    );
    assert!(
        with_admission(Admission::Reactive, || gate.acquire(far())).is_some(),
        "the waiting read did not spend what the refill gave back: reactive still admitted"
    );
}

/// Under memory pressure (the load governor's verdict) the gate sheds its
/// own optional classes with the deferral marker the frontend retries on:
/// background at `warn`, background and reactive at `critical` or a
/// thrashing compressor. A user action is never shed, a read credited to
/// one is not shed either, and everything resumes once the pressure
/// clears. No spawn budget is spent on a shed spawn.
#[test]
fn memory_pressure_sheds_background_then_reactive_never_a_user_action_and_resumes() {
    use super::{with_admission, with_background_processes, Admission, Refusal};
    use crate::sysload::{GovernorInput, Level};
    let gate = super::SpawnGate::new(8);
    let far = || Instant::now() + std::time::Duration::from_secs(10);
    let background = || with_background_processes(|| gate.acquire_until(far(), &|| false).err());
    let reactive = || {
        with_admission(Admission::Reactive, || {
            gate.acquire_until(far(), &|| false).err()
        })
    };
    let interactive = || {
        with_admission(Admission::Interactive, || {
            gate.acquire_until(far(), &|| false).err()
        })
    };

    let calm = crate::sysload::inject_governor(GovernorInput::test_pressure(Level::Normal));
    assert_eq!(
        (background(), reactive(), interactive()),
        (None, None, None)
    );
    drop(calm);

    let warn = crate::sysload::inject_governor(GovernorInput::test_pressure(Level::Warn));
    assert_eq!(
        background(),
        Some(Refusal::Pressure {
            reactive: false,
            cause: Some(Binding::MemoryPressure)
        })
    );
    assert_eq!((reactive(), interactive()), (None, None));
    drop(warn);

    let critical = crate::sysload::inject_governor(GovernorInput::test_pressure(Level::Critical));
    assert_eq!(
        background(),
        Some(Refusal::Pressure {
            reactive: false,
            cause: Some(Binding::MemoryPressure)
        })
    );
    assert_eq!(
        reactive(),
        Some(Refusal::Pressure {
            reactive: true,
            cause: Some(Binding::MemoryPressure)
        })
    );
    assert_eq!(interactive(), None, "a click is never shed");
    let message = refusal_message(
        "git status",
        Refusal::Pressure {
            reactive: true,
            cause: Some(Binding::MemoryPressure),
        },
    );
    assert!(
        is_deferred_under_load(message.message()),
        "{message}",
        message = message.message()
    );
    assert!(
        message.contains("memory pressure"),
        "{message}",
        message = message.message()
    );
    let counters = gate.counters();
    assert_eq!(counters[Admission::Background.index()].shed, 2);
    assert_eq!(counters[Admission::Reactive.index()].shed, 1);
    drop(critical);

    // A thrashing compressor sheds like critical while the level lags.
    let thrash = crate::sysload::inject_governor(GovernorInput {
        compressor_thrash: true,
        ..GovernorInput::test_pressure(Level::Normal)
    });
    assert_eq!(
        reactive(),
        Some(Refusal::Pressure {
            reactive: true,
            cause: Some(Binding::CompressorThrash)
        })
    );
    drop(thrash);

    // A cause that is not memory is named, not called memory pressure.
    let swap = crate::sysload::inject_governor(GovernorInput {
        readings: crate::sysload::Readings {
            swap_used: crate::sysload::Reading::Value(60 << 30),
            ..crate::sysload::Readings::calm()
        },
        ..GovernorInput::test_pressure(Level::Normal)
    });
    let cause = background();
    assert_eq!(
        cause,
        Some(Refusal::Pressure {
            reactive: false,
            cause: Some(Binding::Swap)
        })
    );
    let message = refusal_message("git status", cause.unwrap());
    assert!(
        message.message().contains("swap") && !message.message().contains("memory pressure"),
        "{}",
        message.message()
    );
    drop(swap);

    // Cleared: everything resumes.
    let _calm = crate::sysload::inject_governor(GovernorInput::test_pressure(Level::Normal));
    assert_eq!(
        (background(), reactive(), interactive()),
        (None, None, None)
    );
}

/// The cleaner's own checks are admitted while the disk is the only thing the
/// host is short of, and shed like any other spawn under every other cause.
/// Without `with_disk_relief` the same checks are shed under a full disk, so a
/// run started because the disk is full would examine nothing.
#[test]
fn disk_relief_admits_checks_for_a_disk_cause_only() {
    use super::{with_admission, with_background_processes, with_disk_relief, Admission, Refusal};
    use crate::sysload::{GovernorInput, Level, Reading, Readings};
    let gate = super::SpawnGate::new(8);
    let far = || Instant::now() + Duration::from_secs(10);
    let reactive = || {
        with_admission(Admission::Reactive, || {
            gate.acquire_until(far(), &|| false).err()
        })
    };
    let background = || with_background_processes(|| gate.acquire_until(far(), &|| false).err());
    let low_disk = GovernorInput {
        readings: Readings {
            free_disk: Reading::Value(1 << 30),
            ..Readings::calm()
        },
        ..GovernorInput::test_pressure(Level::Normal)
    };
    let _disk = crate::sysload::inject_governor(low_disk);
    assert_eq!(
        crate::sysload::shed_cause(),
        Some(Binding::LowDisk),
        "the fixture must bind on the disk"
    );
    assert!(
        matches!(reactive(), Some(Refusal::Pressure { reactive: true, .. })),
        "without relief the check is shed under a full disk"
    );
    // Admission needs both: this thread asked for relief, and the governor says
    // the disk is the only thing wrong. The injected governor has no history
    // and answers false, so the governor's answer is forced here; its own
    // answer is tested with the governor.
    use super::with_disk_only_answer;
    assert_eq!(
        with_disk_only_answer(true, || with_disk_relief(reactive)),
        None,
        "relief asked and disk-only: admitted"
    );
    assert_eq!(
        with_disk_only_answer(true, || with_disk_relief(background)),
        None
    );
    assert!(
        matches!(
            with_disk_only_answer(false, || with_disk_relief(reactive)),
            Some(Refusal::Pressure { reactive: true, .. })
        ),
        "relief asked, but another input is not clear (or there is no governor): shed"
    );
    assert!(
        matches!(
            with_disk_only_answer(true, reactive),
            Some(Refusal::Pressure { reactive: true, .. })
        ),
        "disk-only, but the thread never asked for relief: shed"
    );
    assert_eq!(
        with_disk_relief(reactive),
        None,
        "relief asked and the real accessor: free disk is under the Severe floor, so the remedy is due"
    );
    assert!(
        with_disk_only_answer(true, reactive).is_some(),
        "relief ends with its scope, on this thread"
    );
    drop(_disk);

    for (name, input) in [
        (
            "swap",
            GovernorInput {
                readings: Readings {
                    swap_used: Reading::Value(60 << 30),
                    ..Readings::calm()
                },
                ..GovernorInput::test_pressure(Level::Normal)
            },
        ),
        ("memory", GovernorInput::test_pressure(Level::Critical)),
    ] {
        let _held = crate::sysload::inject_governor(input);
        assert!(
            !matches!(
                crate::sysload::shed_cause(),
                Some(Binding::LowDisk | Binding::DiskFilling)
            ),
            "{name}: the fixture must not bind on the disk"
        );
        assert!(
            matches!(with_disk_relief(background), Some(Refusal::Pressure { .. })),
            "{name}: relief must not admit a spawn the host sheds for another cause"
        );
    }
}

/// The same admission through the process-wide accessor, with no seam: a
/// governor that has history, as the app's sampler builds one. A relieved spawn
/// is admitted when the disk alone is Severe, shed without relief, and shed with
/// relief when anything else is wrong beside the disk.
#[test]
fn disk_relief_follows_the_real_accessor() {
    use super::{with_admission, with_disk_relief, Admission, Refusal};
    use crate::sysload::{
        inject_governor_with_history, shed_is_disk_only, GovernorInput, Level, Reading, Readings,
    };
    let gate = super::SpawnGate::new(8);
    let spawn = || {
        with_admission(Admission::Reactive, || {
            gate.acquire_until(Instant::now() + Duration::from_secs(10), &|| false)
                .err()
        })
    };
    let shed = |r: Option<Refusal>| matches!(r, Some(Refusal::Pressure { .. }));
    let disk_only = |free_gib: u64| GovernorInput {
        readings: Readings {
            free_disk: Reading::Value(free_gib << 30),
            ..Readings::calm()
        },
        ..GovernorInput::test_pressure(Level::Normal)
    };
    let _disk = inject_governor_with_history(std::iter::repeat_n(disk_only(8), 10));
    assert!(shed_is_disk_only(), "the fixture is a disk-only Severe");
    assert_eq!(
        with_disk_relief(spawn),
        None,
        "relieved and disk-only: admitted"
    );
    assert!(shed(spawn()), "the same spawn without relief is shed");
    drop(_disk);

    // CPU Elevated beside a Severe disk: not the disk alone, but free disk is
    // below the Severe floor, so the remedy is due whatever else is pressed.
    let mut busy = disk_only(8);
    busy.readings.load = Reading::Value([4.0 * 18.0; 3]);
    let _cpu = inject_governor_with_history(std::iter::repeat_n(busy, 10));
    assert!(!shed_is_disk_only());
    assert_eq!(
        with_disk_relief(spawn),
        None,
        "disk below the Severe floor and CPU Elevated: admitted"
    );
    drop(_cpu);

    // Memory Critical beside a Severe disk: a nearly full disk brings memory
    // pressure with it; the relieved check proceeds, an unrelieved one is shed.
    let mut memory = disk_only(8);
    memory.level = Some(Level::Critical);
    let _memory = inject_governor_with_history(std::iter::repeat_n(memory, 10));
    assert!(!shed_is_disk_only());
    assert_eq!(
        with_disk_relief(spawn),
        None,
        "disk below the Severe floor and memory Severe: admitted"
    );
    assert!(shed(spawn()), "without relief it is shed");
    drop(_memory);

    // Low disk only Elevated (24 GiB, between the floors) with memory Severe.
    let mut elevated = disk_only(24);
    elevated.level = Some(Level::Critical);
    let _elevated = inject_governor_with_history(std::iter::repeat_n(elevated, 10));
    assert!(
        shed(with_disk_relief(spawn)),
        "disk Elevated and memory Severe: relief admits nothing"
    );
    drop(_elevated);

    // The fill projection alone (space well above the floor) with memory Severe.
    let mut filling = GovernorInput::test_pressure(Level::Critical);
    filling.readings.free_disk = Reading::Value(40 << 30);
    filling.readings.disk_fill_sustained = Reading::Value(10 << 30);
    let _filling = inject_governor_with_history(std::iter::repeat_n(filling, 10));
    assert!(
        shed(with_disk_relief(spawn)),
        "a fill projection alone with memory Severe: relief admits nothing"
    );
}

/// A waiting user action takes the next free slot even with refreshes
/// queued ahead of it.
#[test]
fn a_waiting_user_action_takes_the_next_slot_before_a_queued_refresh() {
    use super::{with_admission, Admission};
    use std::sync::Arc;
    let gate = Arc::new(super::SpawnGate::new(1));
    let held = gate
        .acquire(Instant::now() + std::time::Duration::from_secs(1))
        .unwrap();
    let (order_tx, order_rx) = std::sync::mpsc::channel();
    let refresh = {
        let gate = Arc::clone(&gate);
        let order_tx = order_tx.clone();
        std::thread::spawn(move || {
            let permit = gate.acquire(Instant::now() + std::time::Duration::from_secs(5));
            order_tx.send("refresh").unwrap();
            drop(permit);
        })
    };
    std::thread::sleep(std::time::Duration::from_millis(100));
    let action = {
        let gate = Arc::clone(&gate);
        std::thread::spawn(move || {
            with_admission(Admission::Interactive, || {
                let permit = gate.acquire(Instant::now() + std::time::Duration::from_secs(5));
                order_tx.send("action").unwrap();
                std::thread::sleep(std::time::Duration::from_millis(50));
                drop(permit);
            })
        })
    };
    let deadline = Instant::now() + std::time::Duration::from_secs(3);
    while gate.state.lock().unwrap().waiting_interactive == 0 {
        assert!(Instant::now() < deadline, "the action never queued");
        std::thread::yield_now();
    }
    drop(held);
    let first = order_rx
        .recv_timeout(std::time::Duration::from_secs(5))
        .unwrap();
    assert_eq!(first, "action");
    refresh.join().unwrap();
    action.join().unwrap();
}

/// A deferral is recognized only in the shape the gate writes it, at the
/// start (after the ledger's optional `[code] `), for every label the
/// callers pass and every refusal wording. Text that quotes the phrase,
/// in a path or a verdict, is not one.
#[test]
fn a_deferral_is_recognized_only_where_the_gate_writes_it() {
    use super::{is_deferred_under_load, refusal_message, Refusal};
    let waited = std::time::Duration::from_millis(1_500);
    for label in [
        "git worktree",
        "git status",
        "git check-ignore",
        "devmap",
        "kingfisher",
        "gh",
        "npm",
        "ps",
    ] {
        for refusal in [
            Refusal::Shed,
            Refusal::Refused { waited },
            Refusal::Pressure {
                reactive: true,
                cause: None,
            },
        ] {
            let message = refusal_message(label, refusal);
            assert!(
                is_deferred_under_load(message.message()),
                "{message}",
                message = message.message()
            );
            assert!(
                is_deferred_under_load(&format!(
                    "[deferred_worktree] {message}",
                    message = message.message()
                )),
                "{message}",
                message = message.message()
            );
        }
    }
    let path = "/tmp/x deferred under load after 1.000s: y";
    for hostile in [
        format!("Not a Git repository: {path}"),
        format!("Worktree '{path}' does not belong to repository '/r'"),
        format!("fatal: cannot chdir to '{path}': No such file"),
        format!("[invalid_worktree] Not a Git repository: {path}"),
        format!("git status failed: {path}"),
        " deferred under load after 1s".to_string(),
        "git status deferred under load after soon".to_string(),
        "git status deferred under load after 1s'".to_string(),
    ] {
        assert!(!is_deferred_under_load(&hostile), "{hostile}");
    }
}

/// Each refusal carries exactly one marker, and the classifiers that the
/// secrets runner and `run_captured` rely on agree with the formatter.
#[test]
fn every_refusal_formats_to_the_one_cause_its_classifiers_name() {
    use super::{
        is_deferred_under_load, is_slot_wait_timeout, is_timeout_error, refusal_message, Refusal,
    };
    let waited = std::time::Duration::from_millis(2_013);
    let deferred = refusal_message("git status", Refusal::Refused { waited });
    assert!(
        deferred.contains("2.013s"),
        "{deferred}",
        deferred = deferred.message()
    );
    let shed = refusal_message("git status", Refusal::Shed);
    for message in [&deferred, &shed] {
        assert!(
            is_deferred_under_load(message.message()),
            "{message}",
            message = message.message()
        );
        assert!(
            !is_slot_wait_timeout(message.message()),
            "{message}",
            message = message.message()
        );
        assert!(
            !is_timeout_error("git status", message.message()),
            "{message}",
            message = message.message()
        );
    }
    let timed_out = refusal_message(
        "git status",
        Refusal::TimedOut {
            deadline: std::time::Duration::from_secs(90),
        },
    );
    assert!(
        is_slot_wait_timeout(timed_out.message()),
        "{timed_out}",
        timed_out = timed_out.message()
    );
    assert!(
        is_timeout_error("git status", timed_out.message()),
        "{timed_out}",
        timed_out = timed_out.message()
    );
    assert!(
        !is_deferred_under_load(timed_out.message()),
        "{timed_out}",
        timed_out = timed_out.message()
    );
    let cancelled = refusal_message("git status", Refusal::Cancelled);
    assert!(
        !is_deferred_under_load(cancelled.message()) && !is_slot_wait_timeout(cancelled.message())
    );
    let report = super::format_gate_report(
        &[super::AdmissionCounters::default(); 3],
        "budget=per-process",
    );
    for class in super::Admission::ALL {
        assert!(report.contains(class.name()), "{report}");
    }
    assert!(report.ends_with("| budget=per-process"), "{report}");
}

#[test]
fn spawn_report_labels_never_carry_a_path_or_ref() {
    for (label, expected) in [
        ("git status", "git status"),
        ("git for-each-ref", "git for-each-ref"),
        ("gh", "gh"),
        ("devmap status --json", "devmap status"),
        ("git /Users/someone/private-repo", "git …"),
        ("git refs/heads/secret-branch", "git …"),
        ("git ../escape", "git …"),
        ("git .hidden", "git …"),
        ("git name with spaces", "git name"),
        ("", "…"),
        ("   ", "…"),
        (&"x".repeat(33), "…"),
        ("git sübcommand", "git …"),
    ] {
        assert_eq!(super::report_label(label), expected, "{label:?}");
    }
}

#[test]
fn spawn_report_attribution_is_bounded_ordered_and_sums_the_rest() {
    let mut counts = super::LabelCounts::default();
    assert_eq!(counts.report(), "top: none");
    for _ in 0..5 {
        counts.note("git status");
    }
    for _ in 0..3 {
        counts.note("git for-each-ref");
    }
    counts.note("gh");
    counts.note("git /tmp/a");
    counts.note("git /tmp/b");
    // Sanitized labels merge rather than counting each path separately.
    assert_eq!(
        counts.report(),
        "top: git status=5 git for-each-ref=3 git …=2 gh=1"
    );

    // A stream of novel labels cannot grow the map past its bound.
    for i in 0..10_000 {
        counts.note(&format!("tool{i}"));
    }
    assert!(counts.counts.len() <= super::LabelCounts::MAX_LABELS + 1);
    let total: u64 = counts.counts.values().sum();
    assert_eq!(total, 5 + 3 + 1 + 2 + 10_000, "no admission is dropped");
    let report = counts.report();
    assert!(report.starts_with("top: other="), "{report}");
    assert!(report.contains(" rest="), "{report}");
    // One `label=count` per reported label, plus `rest=`; labels may
    // themselves contain a space ("git status"), so count the `=`.
    assert_eq!(
        report.matches('=').count(),
        super::LabelCounts::REPORTED + 1,
        "{report}"
    );
}

#[cfg(unix)]
#[test]
fn admitted_spawns_are_attributed_to_their_label() {
    let gate: &'static SpawnGate = Box::leak(Box::new(SpawnGate::new(4)));
    for label in ["git status", "git status", "git log"] {
        let mut cmd = Command::new("/usr/bin/true");
        run_with_gate(
            &mut cmd,
            label,
            Duration::from_secs(10),
            None,
            1024,
            &mut (),
            gate,
        )
        .expect("spawn");
    }
    let report = gate
        .state
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .by_label
        .report();
    assert_eq!(report, "top: git status=2 git log=1");
}

#[test]
fn rev_parse_report_names_the_top_five_sources_in_order() {
    let mut counts = super::LabelCounts::default();
    assert_eq!(counts.sources_report(), "", "no rev-parse, no line");
    let launches: [(&'static str, u64); 7] = [
        ("a.small", 1),
        ("b.big", 9),
        ("c.mid", 4),
        ("d.tie", 4),
        ("e.low", 2),
        (super::UNATTRIBUTED_SOURCE, 6),
        ("f.tiny", 1),
    ];
    for (source, n) in launches {
        for _ in 0..n {
            counts.note_sourced("git rev-parse --verify HEAD", source);
        }
    }
    // Other commands are never attributed, whatever their source.
    for _ in 0..50 {
        counts.note_sourced("git status", "b.big");
    }
    assert_eq!(
        counts.sources_report(),
        "rev-parse by: b.big=9 unattributed=6 c.mid=4 d.tie=4 e.low=2 rest=2"
    );
    assert_eq!(counts.counts["git rev-parse"], 27, "label totals unchanged");
    assert!(
        counts.report().starts_with("top: git status=50 "),
        "{}",
        counts.report()
    );
}

#[test]
fn rev_parse_sources_are_bounded_and_overflow_is_counted() {
    const NAMES: [&str; 40] = [
        "s00", "s01", "s02", "s03", "s04", "s05", "s06", "s07", "s08", "s09", "s10", "s11", "s12",
        "s13", "s14", "s15", "s16", "s17", "s18", "s19", "s20", "s21", "s22", "s23", "s24", "s25",
        "s26", "s27", "s28", "s29", "s30", "s31", "s32", "s33", "s34", "s35", "s36", "s37", "s38",
        "s39",
    ];
    let mut counts = super::LabelCounts::default();
    for name in NAMES {
        counts.note_sourced("git rev-parse", name);
    }
    assert!(counts.rev_parse_sources.len() <= super::LabelCounts::MAX_SOURCES + 1);
    assert_eq!(counts.rev_parse_sources.values().sum::<u64>(), 40);
    assert_eq!(
        counts.rev_parse_sources["other"], 8,
        "32 kept, 8 overflowed"
    );
    // A source already kept still counts under its own name when full.
    counts.note_sourced("git rev-parse", "s00");
    assert_eq!(counts.rev_parse_sources["s00"], 2);
}

#[test]
fn spawn_source_is_unattributed_unset_nests_and_restores_after_a_panic() {
    assert_eq!(super::current_spawn_source(), super::UNATTRIBUTED_SOURCE);
    super::with_spawn_source("outer", || {
        assert_eq!(super::current_spawn_source(), "outer");
        super::with_spawn_source("inner", || {
            assert_eq!(super::current_spawn_source(), "inner");
        });
        assert_eq!(super::current_spawn_source(), "outer");
        let unwound = std::panic::catch_unwind(|| {
            super::with_spawn_source("doomed", || panic!("body unwinds"));
        });
        assert!(unwound.is_err());
        assert_eq!(super::current_spawn_source(), "outer", "restored on unwind");
        // A new thread starts unattributed: a hop must re-enter the scope.
        let hopped = std::thread::spawn(super::current_spawn_source)
            .join()
            .unwrap();
        assert_eq!(hopped, super::UNATTRIBUTED_SOURCE);
    });
    assert_eq!(super::current_spawn_source(), super::UNATTRIBUTED_SOURCE);
}

/// Interleaved A/B, min of 9 rounds: a scope plus the report bookkeeping
/// against the bookkeeping alone. A spawn costs milliseconds; the bound is
/// 5 us per spawn, far above what a thread-local swap and a map bump cost.
#[test]
fn spawn_source_tagging_adds_negligible_per_spawn_overhead() {
    // 20k x 9 rounds x 2 arms keeps this near 2 s in a debug build; the
    // 5 us bound is ~100x what the swap and map bump cost.
    const N: u32 = 20_000;
    let time = |tagged: bool| {
        let mut counts = super::LabelCounts::default();
        let started = std::time::Instant::now();
        for _ in 0..N {
            if tagged {
                super::with_spawn_source("bench.site", || {
                    counts.note_sourced("git rev-parse", super::current_spawn_source());
                });
            } else {
                counts.note_sourced("git rev-parse", super::UNATTRIBUTED_SOURCE);
            }
        }
        std::hint::black_box(&counts);
        started.elapsed()
    };
    let (mut plain, mut tagged) = (Duration::MAX, Duration::MAX);
    for _ in 0..9 {
        plain = plain.min(time(false));
        tagged = tagged.min(time(true));
    }
    let extra = tagged.saturating_sub(plain) / N;
    println!(
        "spawn-source A/B min-of-9 x {N}: plain {plain:?} tagged {tagged:?} extra {extra:?}/spawn"
    );
    assert!(extra < Duration::from_micros(5), "{extra:?} per spawn");
}

#[cfg(unix)]
#[test]
fn admitted_rev_parse_is_attributed_to_the_scope_it_ran_under() {
    let gate: &'static SpawnGate = Box::leak(Box::new(SpawnGate::new(4)));
    let run = |label: &str, source: Option<&'static str>| {
        let mut cmd = Command::new("/usr/bin/true");
        let go = |cmd: &mut Command| {
            run_with_gate(
                cmd,
                label,
                Duration::from_secs(10),
                None,
                1024,
                &mut (),
                gate,
            )
            .expect("spawn");
        };
        match source {
            Some(source) => super::with_spawn_source(source, || go(&mut cmd)),
            None => go(&mut cmd),
        }
    };
    run("git rev-parse", Some("one.site"));
    run("git rev-parse", Some("one.site"));
    run("git rev-parse", Some("two.site"));
    run("git rev-parse", None);
    run("git log", Some("one.site"));
    assert_eq!(
        gate.label_report(),
        "top: git rev-parse=4 git log=1 | rev-parse by: one.site=2 two.site=1 unattributed=1"
    );
}

#[test]
fn spawn_rate_cap_stops_a_burst_from_becoming_a_sustained_storm() {
    let gate = super::SpawnGate::with_rate(8, 4, 4);
    let mut held = Vec::new();
    for _ in 0..4 {
        held.push(
            gate.acquire(std::time::Instant::now() + std::time::Duration::from_millis(200))
                .expect("burst slot"),
        );
    }
    assert!(
        gate.acquire(std::time::Instant::now() + std::time::Duration::from_millis(80))
            .is_none(),
        "the fifth spawn inside the burst must not start"
    );
    drop(held);
    assert!(
        gate.acquire(std::time::Instant::now() + std::time::Duration::from_millis(80))
            .is_none(),
        "a free concurrency slot must not bypass the per-second cap"
    );
    let (burst, rate) = super::production_spawn_rate();
    assert!(
        rate < 40,
        "sustained cap {rate}/s is still inside the measured 40-80/s storm"
    );
    assert!(burst >= rate);
    assert_eq!(rate, 8);
    assert_eq!(burst, 192);
}

#[test]
fn production_spawn_gate_spends_its_burst_and_then_stops() {
    let gate = super::configured_spawn_gate(false);
    let mut admitted = 0u32;
    let drain_until = std::time::Instant::now() + std::time::Duration::from_millis(400);
    while std::time::Instant::now() < drain_until {
        match gate.acquire(std::time::Instant::now() + std::time::Duration::from_millis(20)) {
            Some(permit) => {
                admitted += 1;
                drop(permit);
            }
            None => break,
        }
    }
    assert!(
        (192..192 + 16).contains(&admitted),
        "production burst admitted {admitted}, want 192 plus at most one refill window"
    );
}

#[test]
fn excess_waiters_fail_at_the_queue_budget_instead_of_the_git_deadline() {
    use std::sync::Arc;
    let gate = Arc::new(super::SpawnGate::with_rate(32, 4, 1));
    let started = std::time::Instant::now();
    let mut threads = Vec::new();
    for _ in 0..12 {
        let gate = Arc::clone(&gate);
        threads.push(std::thread::spawn(move || {
            let began = std::time::Instant::now();
            let admitted = gate
                .acquire(std::time::Instant::now() + std::time::Duration::from_secs(30))
                .is_some();
            (admitted, began.elapsed())
        }));
    }
    let mut admitted = 0u32;
    let mut slowest = std::time::Duration::ZERO;
    for thread in threads {
        let (got, waited) = thread.join().unwrap();
        if got {
            admitted += 1;
        }
        slowest = slowest.max(waited);
    }
    assert!(
        started.elapsed()
            < crate::test_support::coverage_relaxed(std::time::Duration::from_secs(4)),
        "join took {:?}; a refused spawn must not sit until the 90s git deadline",
        started.elapsed()
    );
    assert!(
        slowest < std::time::Duration::from_secs(3),
        "slowest acquire waited {slowest:?}"
    );
    assert!(
        (4..=8).contains(&admitted),
        "admitted {admitted}; burst 4 plus about two refill tokens, not all 12"
    );
}

#[test]
fn spawn_rate_cap_bounds_a_concurrent_storm() {
    use std::sync::atomic::{AtomicU32, Ordering};
    use std::sync::Arc;
    let gate = Arc::new(super::SpawnGate::with_rate(64, 8, 4));
    let admitted = Arc::new(AtomicU32::new(0));
    let started = std::time::Instant::now();
    let mut threads = Vec::new();
    for _ in 0..8 {
        let gate = Arc::clone(&gate);
        let admitted = Arc::clone(&admitted);
        threads.push(std::thread::spawn(move || {
            while started.elapsed()
                < crate::test_support::coverage_relaxed(std::time::Duration::from_millis(500))
            {
                if let Some(permit) =
                    gate.acquire(std::time::Instant::now() + std::time::Duration::from_millis(15))
                {
                    admitted.fetch_add(1, Ordering::Relaxed);
                    drop(permit);
                }
            }
        }));
    }
    for thread in threads {
        thread.join().unwrap();
    }
    // The cap is a rate, so it is measured against the window that actually
    // ran: coverage_relaxed stretches the 500 ms window tenfold under
    // instrumentation, and a bound fixed for 500 ms then fails on refills
    // the gate was right to grant. Windows up to a second keep the bound the
    // fixed version had (a second of refill plus 4 of slack); an uncapped
    // gate admits hundreds in the same window.
    let window = started.elapsed().as_secs_f64();
    let n = admitted.load(Ordering::Relaxed);
    let cap = 8 + (4.0 * window.max(1.0)).ceil() as u32 + 4;
    assert!(n >= 8, "burst was not issued: {n}");
    assert!(
        n <= cap,
        "concurrent storm admitted {n} in {window:.2}s; cap is burst 8 plus 4/s = {cap}"
    );
}

#[test]
fn background_waiter_gets_capacity_and_timeout_releases_its_reservation() {
    let gate = std::sync::Arc::new(super::SpawnGate::new(4));
    let mut foreground: Vec<_> = (0..4)
        .map(|_| {
            gate.acquire(Instant::now() + std::time::Duration::from_secs(1))
                .unwrap()
        })
        .collect();
    let worker_gate = gate.clone();
    let (acquired_tx, acquired_rx) = std::sync::mpsc::channel();
    let (release_tx, release_rx) = std::sync::mpsc::channel();
    let worker = std::thread::spawn(move || {
        super::with_background_processes(|| {
            let _permit = worker_gate
                .acquire(Instant::now() + std::time::Duration::from_secs(5))
                .expect("background must eventually run");
            acquired_tx.send(()).unwrap();
            release_rx
                .recv_timeout(std::time::Duration::from_secs(5))
                .unwrap();
        })
    });
    let state = gate.state.lock().unwrap();
    let (state, waited) = gate
        .released
        .wait_timeout_while(state, std::time::Duration::from_secs(3), |state| {
            state.waiting_background == 0
        })
        .unwrap();
    assert!(!waited.timed_out());
    drop(state);
    foreground.pop();
    acquired_rx
        .recv_timeout(std::time::Duration::from_secs(3))
        .unwrap();
    assert!(gate
        .acquire(Instant::now() + std::time::Duration::from_millis(20))
        .is_none());
    release_tx.send(()).unwrap();
    worker.join().unwrap();
    let last = gate
        .acquire(Instant::now() + std::time::Duration::from_secs(1))
        .unwrap();
    assert!(super::with_background_processes(
        || gate.acquire(Instant::now() + std::time::Duration::from_millis(20))
    )
    .is_none());
    assert_eq!(gate.state.lock().unwrap().waiting_background, 0);
    drop(last);
    assert!(gate
        .acquire(Instant::now() + std::time::Duration::from_secs(1))
        .is_some());
}

#[test]
fn mixed_priority_contention_preserves_counts_and_makes_progress() {
    let gate = std::sync::Arc::new(super::SpawnGate::new(8));
    let workers: Vec<_> = (0..24)
        .map(|worker| {
            let gate = gate.clone();
            std::thread::spawn(move || {
                let run = || {
                    for _ in 0..50 {
                        let _permit = gate
                            .acquire(Instant::now() + std::time::Duration::from_secs(10))
                            .unwrap();
                        {
                            let state = gate.state.lock().unwrap();
                            assert!(state.in_flight <= 8);
                            assert!(state.background <= 2);
                        }
                        std::thread::yield_now();
                    }
                };
                if worker % 2 == 0 {
                    super::with_background_processes(run);
                } else {
                    run();
                }
            })
        })
        .collect();
    for worker in workers {
        worker.join().unwrap();
    }
    let state = gate.state.lock().unwrap();
    assert_eq!(
        (state.in_flight, state.background, state.waiting_background),
        (0, 0, 0)
    );
    assert!(state.peak <= 8);
}

#[test]
fn background_process_admission_reserves_foreground_capacity() {
    let gate = super::SpawnGate::new(4);
    let first = super::with_background_processes(|| {
        gate.acquire(Instant::now() + std::time::Duration::from_secs(1))
    })
    .expect("first background slot");
    let excess = super::with_background_processes(|| {
        gate.acquire(Instant::now() + std::time::Duration::from_millis(20))
    });
    assert!(
        excess.is_none(),
        "background work must not consume the foreground reserve"
    );
    let foreground: Vec<_> = (0..3)
        .map(|_| {
            gate.acquire(Instant::now() + std::time::Duration::from_secs(1))
                .unwrap()
        })
        .collect();
    assert_eq!(gate.peak(), 4);
    drop(foreground);
    drop(first);
}

#[test]
#[cfg(unix)]
fn audit_capture_refuses_incomplete_stderr() {
    let result = capture_command(
        "sh",
        &["-c", "echo version >&2; sleep 3 >/dev/null &"],
        None,
        Duration::from_secs(5),
        &[],
    );
    assert!(result.is_err(), "capture erased stderr completeness");
}

#[cfg(unix)]
#[test]
fn audit_zero_deadline_never_starts_a_command() {
    let dir = tempfile::tempdir().unwrap();
    let marker = dir.path().join("started");
    let mut cmd = Command::new("sh");
    cmd.args(["-c", "touch \"$1\"", "test"]).arg(&marker);
    assert!(run_bounded(cmd, "sh", Duration::ZERO, None).is_err());
    assert!(
        !marker.exists(),
        "an expired request must not mutate anything"
    );
}

#[cfg(unix)]
#[test]
fn audit_admission_wait_consumes_the_command_deadline() {
    // Exercise the production runner with an isolated gate. Acquiring
    // the global gate one permit at a time can monopolize most slots
    // while waiting for the last, starving unrelated concurrent tests.
    let gate: &'static SpawnGate = Box::leak(Box::new(SpawnGate::new(2)));
    let permits: Vec<_> = (0..gate.limit)
        .map(|_| {
            gate.acquire(Instant::now() + Duration::from_secs(30))
                .unwrap()
        })
        .collect();
    let dir = tempfile::tempdir().unwrap();
    let marker = dir.path().join("started");
    let copy = marker.clone();
    let worker = thread::spawn(move || {
        let mut cmd = Command::new("sh");
        cmd.args(["-c", "touch \"$1\"", "test"]).arg(copy);
        run_with_gate(
            &mut cmd,
            "sh",
            Duration::from_millis(50),
            None,
            1024,
            &mut (),
            gate,
        )
    });
    thread::sleep(Duration::from_millis(300));
    drop(permits);
    let result = worker.join().unwrap();
    assert!(result.is_err(), "queued request ran after its deadline");
    assert!(!marker.exists());
}

#[cfg(unix)]
#[test]
fn audit_retained_stdin_does_not_block_after_child_exit() {
    let mut cmd = Command::new("sh");
    cmd.args(["-c", "exec 3<&0; sleep 3 <&3 >/dev/null 2>&1 & exit 0"]);
    let started = Instant::now();
    let result = run_bounded(
        cmd,
        "sh",
        Duration::from_secs(1),
        Some(&vec![b'x'; 1024 * 1024]),
    );
    assert!(
        started.elapsed() < crate::test_support::coverage_relaxed(Duration::from_secs(2)),
        "stdin join outlived the child: {:?}",
        started.elapsed()
    );
    assert!(
        result.is_err(),
        "undelivered stdin is not successful delivery"
    );
}

#[cfg(unix)]
#[test]
fn audit_stderr_cap_is_reported() {
    let mut cmd = Command::new("sh");
    cmd.args(["-c", "head -c 5000000 /dev/zero >&2"]);
    let out = run_bounded(cmd, "sh", Duration::from_secs(10), None).unwrap();
    assert!(out.success && out.incomplete.is_none());
    assert!(String::from_utf8_lossy(&out.stderr).contains("stderr incomplete"));
}

#[cfg(unix)]
#[test]
fn audit_output_cap_cannot_disable_the_global_memory_bound() {
    let mut cmd = Command::new("sh");
    cmd.args(["-c", "printf ok"]);
    assert!(run_bounded_capped(cmd, "sh", Duration::from_secs(5), None, usize::MAX).is_err());
}

/// A draining reader keeps no stdout, so it may declare a stream budget
/// past the buffered limit and read past it, every byte still counted
/// against that budget; a buffered read may not, and nor may a draining
/// one past its own ceiling.
#[cfg(unix)]
#[test]
fn only_a_draining_reader_may_stream_past_the_buffered_output_limit() {
    #[derive(Default)]
    struct Counting(usize);
    impl ProcessObserver for Counting {
        fn output(&mut self, stream: OutputStream, bytes: &[u8]) {
            if matches!(stream, OutputStream::Stdout) {
                self.0 += bytes.len();
            }
        }
        fn drains_stdout(&self) -> bool {
            true
        }
    }
    let past = MAX_OUTPUT_BYTES + 1024;
    let script = format!("head -c {past} /dev/zero");
    let mut counting = Counting::default();
    let run = run_observed(
        Command::new("sh").args(["-c", &script]),
        "sh",
        Duration::from_secs(60),
        None,
        past,
        &mut counting,
    )
    .expect("a draining reader may declare a budget past the buffered limit");
    assert!(run.success && run.incomplete.is_none());
    assert_eq!(counting.0, past, "every byte reached the observer");
    assert!(run.stdout.len() < MAX_OUTPUT_BYTES, "and none was held");

    let mut cmd = Command::new("sh");
    cmd.args(["-c", "printf ok"]);
    assert!(run_bounded_capped(cmd, "sh", Duration::from_secs(5), None, past).is_err());
    assert!(run_observed(
        Command::new("sh").args(["-c", "printf ok"]),
        "sh",
        Duration::from_secs(5),
        None,
        MAX_DRAINED_OUTPUT_BYTES + 1,
        &mut Counting::default(),
    )
    .is_err());

    // The declared budget still bounds a draining read.
    let mut short = Counting::default();
    let run = run_observed(
        Command::new("sh").args(["-c", &script]),
        "sh",
        Duration::from_secs(60),
        None,
        1024,
        &mut short,
    )
    .expect("runs");
    assert!(run.incomplete.is_some(), "cut at the declared budget");
    assert!(short.0 <= 1024, "observer saw {} bytes", short.0);
}

/// A descendant retaining the write ends must not erase bytes the parent
/// already emitted. The result remains explicitly incomplete until EOF.
#[cfg(unix)]
#[test]
fn inherited_output_pipes_preserve_the_captured_prefix() {
    let mut cmd = Command::new("sh");
    cmd.args([
        "-c",
        "printf payload; printf diagnostic >&2; sleep 6 & exit 0",
    ]);
    let out = run_bounded(cmd, "sh", Duration::from_secs(10), None).expect("run");
    assert!(out.success);
    assert!(matches!(out.incomplete, Some(Incomplete::Unread(_))));
    assert_eq!(out.stdout, b"payload");
    assert!(out.stderr.starts_with(b"diagnostic"));
}

#[test]
fn interrupted_output_reads_resume_without_losing_bytes() {
    struct InterruptedOnce(bool);
    impl Read for InterruptedOnce {
        fn read(&mut self, bytes: &mut [u8]) -> std::io::Result<usize> {
            if !self.0 {
                self.0 = true;
                return Err(std::io::ErrorKind::Interrupted.into());
            }
            bytes[0] = b'x';
            Ok(1)
        }
    }
    // Stop after the first real byte so the synthetic source is finite.
    let result = drain_capped(Some(InterruptedOnce(false).take(1)), 8);
    assert_eq!(result.bytes, b"x");
    assert!(result.stop.is_none());
}

#[test]
fn poll_backoff_starts_far_below_the_old_fixed_quantum() {
    // The regression this replaces: every child was polled on a flat 15 ms
    // schedule, so a 2 ms `git rev-parse` took 15 ms of wall clock. The
    // first sleep has to be small enough that a fast command is observed
    // promptly rather than on the next tick.
    assert!(POLL_BACKOFF_START <= Duration::from_millis(1));
    assert!(
        POLL_BACKOFF_START > Duration::ZERO,
        "a zero sleep is a spin"
    );
}

#[test]
fn poll_backoff_doubles_and_stops_at_the_ceiling() {
    let mut d = POLL_BACKOFF_START;
    let mut steps = 0;
    while d < POLL_BACKOFF_MAX {
        let next = next_poll_backoff(d);
        assert!(next > d, "backoff must make progress from {d:?}");
        d = next;
        steps += 1;
        assert!(steps < 64, "backoff never reached its ceiling");
    }
    assert_eq!(d, POLL_BACKOFF_MAX);
    // Saturating at the ceiling is what keeps a long clone off a hot loop.
    assert_eq!(next_poll_backoff(POLL_BACKOFF_MAX), POLL_BACKOFF_MAX);
}

#[test]
fn reaching_the_ceiling_costs_a_bounded_number_of_polls() {
    // The whole ramp must be cheap: if getting to the ceiling took
    // thousands of wakeups, the fast case would be paid for by every long
    // running command.
    let mut d = POLL_BACKOFF_START;
    let mut polls = 0;
    let mut slept = Duration::ZERO;
    while d < POLL_BACKOFF_MAX {
        slept += d;
        d = next_poll_backoff(d);
        polls += 1;
    }
    assert!(polls <= 16, "ramp took {polls} polls");
    // A doubling ramp's total is about twice its last step, so the whole
    // warm-up costs on the order of ONE old tick — not orders of magnitude
    // more. That is the property that makes the fast case free rather than
    // borrowed from long-running commands.
    assert!(
        slept <= POLL_BACKOFF_MAX * 2,
        "the entire ramp ({slept:?}) should cost about one old tick"
    );
}

/// The timeout and spawn shapes every converged classifier must tell apart,
/// raw and wrapped the ways callers wrap them.
#[test]
fn run_error_classifies_each_runner_message_shape() {
    let slot = format!("devmap{TIMEOUT_MARKER}0.5{SLOT_WAIT_SUFFIX}");
    let ran_out = format!("devmap{TIMEOUT_MARKER}30s");
    let deferred = refusal_message(
        "devmap build",
        Refusal::Refused {
            waited: Duration::from_millis(500),
        },
    );
    let hostile = "Failed to spawn /Users/x/timeout-wrapper/devmap: no such file";
    let hostile_marker =
        format!("Failed to spawn /Users/x/devmap{TIMEOUT_MARKER}1s/devmap: permission denied");
    let not_found = spawn_error(
        "/Users/x/timeout/devmap",
        &std::io::Error::from(std::io::ErrorKind::NotFound),
    );
    assert_eq!(RunError::classify(&ran_out), RunError::Timeout);
    assert_eq!(
        RunError::classify(&format!("[some_code] {ran_out}")),
        RunError::Timeout,
        "a coded prefix does not hide a timeout"
    );
    assert_eq!(
        RunError::classify(&format!("[some_code] {slot}")),
        RunError::SlotWaitTimeout
    );
    assert_eq!(RunError::classify(&slot), RunError::SlotWaitTimeout);
    assert_eq!(RunError::classify(deferred.message()), RunError::Deferred);
    assert_eq!(
        RunError::classify(&format!(
            "[deferred_worktree] {deferred}",
            deferred = deferred.message()
        )),
        RunError::Deferred
    );
    assert_eq!(RunError::classify(hostile), RunError::SpawnFailed);
    assert_eq!(RunError::classify(&hostile_marker), RunError::SpawnFailed);
    assert_eq!(RunError::classify(&not_found), RunError::SpawnNotFound);
    assert_eq!(
        RunError::classify("devmap exited with status 2"),
        RunError::Other
    );
    assert!(is_run_timeout(&ran_out));
    for not_timeout in [slot.as_str(), deferred.message(), not_found.as_str()] {
        assert!(!is_run_timeout(not_timeout), "{not_timeout}");
    }
    assert!(!is_run_timeout(hostile));
    assert!(!is_run_timeout(&hostile_marker));
}

#[test]
fn run_captured_reports_a_finished_run_with_tails() {
    let outcome = run_captured(
        "git",
        &["--version"],
        None,
        DEFAULT_TIMEOUT,
        &[],
        MAX_OUTPUT_BYTES,
    )
    .unwrap();
    match outcome {
        RunOutcome::Finished(run) => {
            assert!(run.success);
            assert_eq!(run.status_code, 0);
            assert!(run.stdout_tail.contains("git version"));
            assert!(!run.truncated);
        }
        RunOutcome::TimedOut(_) => panic!("git --version must not time out"),
    }
}

/// Pins the timeout contract of [`run_captured`]: a deadline kill is an
/// outcome (`TimedOut`), never laundered into a spawn-style error. If
/// `run_bounded`'s message wording changes, this fails and the detector
/// in `run_captured` has to be updated with it.
#[cfg(unix)]
#[test]
fn run_captured_reports_timeout_as_an_outcome() {
    let outcome = run_captured(
        "sleep",
        &["5"],
        None,
        Duration::from_secs(1),
        &[],
        MAX_OUTPUT_BYTES,
    )
    .unwrap();
    assert!(
        matches!(outcome, RunOutcome::TimedOut(d) if d.as_secs() == 1),
        "expected TimedOut(1s), got {outcome:?}"
    );
}

#[test]
fn byte_tail_keeps_the_end_and_stays_char_safe() {
    assert_eq!(byte_tail(b"hello world", 5), "world");
    // A multibyte sequence split at the cap must not leak replacement
    // characters at the head of the tail.
    let prefix = "é".repeat(5000); // 2 bytes each
    assert!(!byte_tail(prefix.as_bytes(), 9999).starts_with('\u{FFFD}'));
}

/// "Not installed" has to be earned: only the OS failing to find the
/// program says it. A spawn refused under load said it too, which is how
/// the Health panel told a user with `gh` on PATH to install it.
#[test]
fn a_tool_probe_says_not_found_only_when_the_os_found_nothing() {
    // A real spawn of a name that resolves nowhere: the one "no".
    let missing = capture_command_with_env(
        "gitpulse-no-such-tool-7f3a",
        &["--version"],
        None,
        Duration::from_secs(5),
        &[],
        Some(std::ffi::OsStr::new("")),
        None,
        PathSource::Inherited,
    );
    match classify_tool_probe("gitpulse-no-such-tool-7f3a", missing) {
        ToolProbe::NotFound(error) => {
            assert!(
                error.starts_with("Failed to spawn gitpulse-no-such-tool-7f3a"),
                "{error}"
            );
            assert!(is_spawn_not_found(&error), "{error}");
        }
        other => panic!("a program that is nowhere is not found, got {other:?}"),
    }

    // Shed under load, refused by the rate limit, no slot in time,
    // cancelled: none of these started anything, so none is an absence.
    for refusal in [
        Refusal::Shed,
        Refusal::Refused {
            waited: Duration::from_millis(1500),
        },
        Refusal::TimedOut {
            deadline: Duration::from_secs(2),
        },
        Refusal::Cancelled,
    ] {
        let error = refusal_message("gh", refusal);
        assert!(
            matches!(
                classify_tool_probe("gh", Err(error.clone())),
                ToolProbe::NotRun(_)
            ),
            "{error}",
            error = error.message()
        );
    }

    // A spawn the OS refused for a reason other than absence.
    for error in [
        "Failed to spawn gh: Too many open files (os error 24)",
        "Failed to spawn gh: Argument list too long (os error 7)",
        "Failed to spawn gh: Resource temporarily unavailable (os error 35)",
    ] {
        assert!(!is_spawn_not_found(error), "{error}");
        assert!(
            matches!(
                classify_tool_probe("gh", Err(error.to_string().into())),
                ToolProbe::NotRun(_)
            ),
            "{error}"
        );
    }

    // The forced-failure seam the gh regression test uses is a spawn
    // refusal too, through the real gate.
    let forced = with_forced_spawn_failure(|| {
        capture_command("gh", &["--version"], None, Duration::from_secs(5), &[])
    });
    assert!(matches!(
        classify_tool_probe("gh", forced),
        ToolProbe::NotRun(_)
    ));

    // Started and then ran out of time: found, and failed.
    let hung = format!("gh{TIMEOUT_MARKER}10s");
    assert!(matches!(
        classify_tool_probe("gh", Err(hung.into())),
        ToolProbe::FoundButFailed(_)
    ));
}

/// The formatter in `run_bounded` and the matcher behind `run_captured`
/// share TIMEOUT_MARKER; this pins the round trip so neither side can
/// drift without breaking here first, even on a platform where the
/// end-to-end timeout test above does not run. Production always formats
/// with `label == program`, and the starts_with guard rejects errors
/// from any other program.
#[test]
fn timeout_marker_round_trips_between_formatter_and_matcher() {
    let formatted = format!("sleep{TIMEOUT_MARKER}3s");
    assert!(is_timeout_error("sleep", &formatted));
    // Same contract when the program is spelled as a path: the label is
    // then the full path too, so the prefix still matches.
    let path_formatted = format!("/usr/bin/sleep{TIMEOUT_MARKER}3s");
    assert!(is_timeout_error("/usr/bin/sleep", &path_formatted));
    // A spawn failure with unrelated text must not classify as a timeout…
    assert!(!is_timeout_error(
        "sleep",
        "Failed to spawn sleep: No such file"
    ));
    // …and another program's timeout must not match either.
    assert!(!is_timeout_error("git", &formatted));
}

#[test]
fn capture_command_keeps_stdout_on_nonzero() {
    let output = capture_command(
        "sh",
        &["-c", "echo findings; echo err >&2; exit 1"],
        None,
        Duration::from_secs(5),
        &[],
    )
    .expect("spawn sh");
    assert!(!output.success);
    assert_eq!(output.status_code, 1);
    assert!(output.stdout_text().contains("findings"));
    assert!(output.stderr_text().contains("err"));
}

/// Regression: the stdin payload must be fed from its own thread while
/// stdout/stderr drain concurrently. A child that emits 256 KiB on stdout
/// BEFORE reading stdin fills both 64 KiB pipe buffers, and a parent that
/// writes its >128 KiB payload inline first would block in `write_all`
/// forever — before the deadline loop ever runs. The watchdog here is the
/// proof: pre-fix this call never returns and the channel times out.
#[test]
fn stdin_write_cannot_deadlock_behind_a_chatty_child() {
    let payload = vec![b'x'; 192 * 1024];
    let (tx, rx) = std::sync::mpsc::channel();
    let started = Instant::now();
    thread::spawn(move || {
        // Writes 256 KiB to stdout first, then consumes all of stdin.
        let mut cmd = Command::new("sh");
        cmd.args(["-c", "head -c 262144 /dev/zero; cat > /dev/null"]);
        let result = run_bounded(cmd, "sh", Duration::from_secs(60), Some(&payload));
        let _ = tx.send(result);
    });
    match rx.recv_timeout(Duration::from_secs(10)) {
        Ok(Ok(out)) => {
            assert_eq!(out.stdout.len(), 262_144);
            assert!(out.success);
        }
        Ok(other) => panic!("expected success, got: {:?}", other.map(|o| o.success)),
        Err(_) => panic!(
            "stdin write deadlocked behind child stdout; no result within 10s \
                 (write_all blocked before the deadline loop could start)"
        ),
    }
    assert!(
        started.elapsed() < crate::test_support::coverage_relaxed(Duration::from_secs(10)),
        "bounded run should finish promptly once pipes drain"
    );
}

#[test]
fn run_command_still_fails_on_nonzero() {
    let err = run_command(
        "sh",
        &["-c", "echo boom >&2; exit 2"],
        Duration::from_secs(5),
    )
    .expect_err("nonzero");
    assert!(err.contains("boom"));
}

/// Regression: a GUI launch (Finder/Dock/ScholarLM) hands the app a minimal
/// PATH without `/opt/homebrew/bin`, so bare names like `gh` failed to
/// resolve and every GitHub view reported the CLI as "not installed". With
/// an empty PATH the resolver must fall back to the conventional install
/// directories (`~/.local/bin` here, standing in for Homebrew's).
#[cfg(unix)]
#[test]
fn spawn_resolution_finds_tool_in_fallback_dir_when_path_is_empty() {
    let home = tempfile::TempDir::new().unwrap();
    let bin = home.path().join(".local/bin");
    std::fs::create_dir_all(&bin).unwrap();
    let tool = bin.join("gitpulse-fake-tool");
    std::fs::write(&tool, "#!/bin/sh\nexit 0\n").unwrap();
    std::fs::set_permissions(&tool, std::os::unix::fs::PermissionsExt::from_mode(0o755)).unwrap();

    let resolved = resolve_spawn_program_with(
        "gitpulse-fake-tool",
        Some(std::ffi::OsStr::new("")),
        Some(home.path().as_os_str()),
    );
    assert_eq!(
        Path::new(&resolved),
        &tool,
        "bare name must resolve through the fallback dir on an empty PATH"
    );
}

/// An explicit PATH is the whole search path: the same tool in the same
/// fallback dir stays unresolved, and the child is handed that PATH
/// without the fallback dirs, so neither the spawn nor anything the child
/// re-resolves can reach past it.
#[cfg(unix)]
#[test]
fn an_explicit_path_is_not_widened_by_fallback_dirs() {
    let home = tempfile::TempDir::new().unwrap();
    let bin = home.path().join(".local/bin");
    std::fs::create_dir_all(&bin).unwrap();
    let tool = bin.join("gitpulse-fake-tool");
    std::fs::write(&tool, "#!/bin/sh\nexit 0\n").unwrap();
    std::fs::set_permissions(&tool, std::os::unix::fs::PermissionsExt::from_mode(0o755)).unwrap();

    assert_eq!(
        resolve_spawn_program_in(
            "gitpulse-fake-tool",
            Some(std::ffi::OsStr::new("/usr/bin:/bin")),
            Some(home.path().as_os_str()),
            PathSource::Explicit,
        ),
        "gitpulse-fake-tool",
        "a tool outside the explicit PATH must not resolve"
    );

    let cmd = build_capture_command_in(
        "gitpulse-fake-tool",
        &[],
        None,
        &[],
        Some(std::ffi::OsStr::new("/usr/bin::/bin")),
        Some(home.path().as_os_str()),
        PathSource::Explicit,
    );
    assert_eq!(
        cmd.get_program(),
        std::ffi::OsStr::new("gitpulse-fake-tool")
    );
    let child_path: Vec<Option<std::ffi::OsString>> = cmd
        .get_envs()
        .filter(|(key, _)| *key == std::ffi::OsStr::new("PATH"))
        .map(|(_, value)| value.map(std::ffi::OsStr::to_os_string))
        .collect();
    assert_eq!(
        child_path,
        vec![Some(std::ffi::OsString::from("/usr/bin:/bin"))],
        "the child gets the explicit PATH, empty entries dropped, nothing appended"
    );

    // No explicit value at all is not "inherit the process PATH".
    let unset = build_capture_command_in(
        "sh",
        &[],
        None,
        &[],
        None,
        Some(home.path().as_os_str()),
        PathSource::Explicit,
    );
    assert!(
        unset
            .get_envs()
            .any(|(key, value)| key == std::ffi::OsStr::new("PATH") && value.is_none()),
        "an explicit unset PATH must be removed from the child"
    );
}

/// Regression: the same GUI-launch miss as the `gh` case above, one
/// directory short. `rustup` installs the whole Rust toolchain — `cargo`,
/// `rustc`, `rustup` itself, and cargo subcommand binaries such as
/// `cargo-audit` and `cargo-llvm-cov` — into `~/.cargo/bin`, which is on
/// no GUI-launch PATH and was in no fallback dir. Running `cargo` from the
/// terminal panel died with "Failed to spawn cargo: No such file or
/// directory (os error 2)", the dependency scanner reported the Rust
/// ecosystem as unscannable, and the coverage panel reported
/// `cargo llvm-cov` absent — on machines where all of it was installed.
#[cfg(unix)]
#[test]
fn spawn_resolution_finds_rust_toolchain_in_cargo_bin() {
    let home = tempfile::TempDir::new().unwrap();
    let bin = home.path().join(".cargo/bin");
    std::fs::create_dir_all(&bin).unwrap();

    // Half the regression: the directory has to be in the list at all.
    // Every Rust entry point the app spawns by bare name — `cargo`,
    // `rustc`, `rustup`, and `cargo-*` subcommands like `cargo-audit` and
    // `cargo-llvm-cov` — lives in this one directory, so its absence took
    // all of them out together. Asserting the list directly is the only
    // part of that claim the host cannot influence.
    assert!(
        gui_launch_fallback_dirs(Some(home.path().as_os_str())).contains(&bin),
        "~/.cargo/bin must be a GUI-launch fallback directory"
    );

    // The other half: resolution has to walk far enough to reach it. It is
    // the LAST entry, behind real system directories (`/opt/homebrew/bin`,
    // `/usr/local/bin`) that this test cannot sandbox — so probing with a
    // real tool name measures the host, not the code. A macOS runner with
    // a Homebrew `rustup` resolved `/opt/homebrew/bin/rustup`, which is
    // the correct answer on that machine, and failed a test that meant to
    // ask something else. A name no machine can have crosses every earlier
    // directory and can only be found in the last one.
    let probe = "gitpulse-cargo-bin-probe";
    let path = bin.join(probe);
    std::fs::write(&path, "#!/bin/sh\nexit 0\n").unwrap();
    std::fs::set_permissions(&path, std::os::unix::fs::PermissionsExt::from_mode(0o755)).unwrap();

    // The GUI-launch PATH verbatim: this is what the bundled app gets from
    // launchd, and it is why the fallback list has to carry the directory.
    let resolved = resolve_spawn_program_with(
        probe,
        Some(std::ffi::OsStr::new("/usr/bin:/bin:/usr/sbin:/sbin")),
        Some(home.path().as_os_str()),
    );
    assert_eq!(
        Path::new(&resolved),
        &path,
        "resolution must reach ~/.cargo/bin on a GUI-launch PATH"
    );
}

#[cfg(unix)]
#[test]
fn spawn_resolution_finds_grok_in_its_install_dir() {
    let home = tempfile::TempDir::new().unwrap();
    let bin = home.path().join(".grok/bin");
    std::fs::create_dir_all(&bin).unwrap();
    assert!(
        gui_launch_fallback_dirs(Some(home.path().as_os_str())).contains(&bin),
        "~/.grok/bin must be a GUI-launch fallback directory"
    );
    let probe = "gitpulse-grok-bin-probe";
    let path = bin.join(probe);
    std::fs::write(&path, "#!/bin/sh\nexit 0\n").unwrap();
    std::fs::set_permissions(&path, std::os::unix::fs::PermissionsExt::from_mode(0o755)).unwrap();
    let resolved = resolve_spawn_program_with(
        probe,
        Some(std::ffi::OsStr::new("/usr/bin:/bin:/usr/sbin:/sbin")),
        Some(home.path().as_os_str()),
    );
    assert_eq!(
        Path::new(&resolved),
        &path,
        "resolution must reach ~/.grok/bin on a GUI-launch PATH"
    );
}

/// A fallback directory that does not depend on the user's home must not
/// be gated on knowing it. `/usr/local/go/bin` is a fixed system path, so
/// dropping it when `home` is unset (a launchd/daemon context, where this
/// mechanism matters most) withheld a directory that was still valid.
#[cfg(unix)]
#[test]
fn gui_fallback_keeps_system_dirs_when_home_is_unknown() {
    let dirs = gui_launch_fallback_dirs(None);
    for system_dir in [
        PathBuf::from("/opt/homebrew/bin"),
        PathBuf::from("/usr/local/bin"),
        PathBuf::from("/usr/local/go/bin"),
    ] {
        assert!(
            dirs.contains(&system_dir),
            "home-independent dir must survive an unknown home: {} in {dirs:?}",
            system_dir.display()
        );
    }
    // Home-relative dirs are the only ones an unknown home may cost.
    assert!(
        dirs.iter().all(|d| d.is_absolute()),
        "no relative entry may reach the search list: {dirs:?}"
    );
}

/// PATH order wins: a name present on the inherited PATH must not be
/// shadowed by a fallback-directory copy. Both candidates are executable
/// so the strict `find_in_dirs` scan considers them at all.
#[cfg(unix)]
#[test]
fn spawn_resolution_prefers_path_over_fallback_dirs() {
    let path_dir = tempfile::TempDir::new().unwrap();
    let home_dir = tempfile::TempDir::new().unwrap();
    for dir in [path_dir.path(), &home_dir.path().join(".local/bin")] {
        std::fs::create_dir_all(dir).unwrap();
        std::fs::write(dir.join("gitpulse-shadowed"), "#!/bin/sh\nexit 0\n").unwrap();
        std::fs::set_permissions(
            dir.join("gitpulse-shadowed"),
            std::os::unix::fs::PermissionsExt::from_mode(0o755),
        )
        .unwrap();
    }
    let resolved = resolve_spawn_program_with(
        "gitpulse-shadowed",
        Some(path_dir.path().as_os_str()),
        Some(home_dir.path().as_os_str()),
    );
    assert_eq!(
        Path::new(&resolved),
        path_dir.path().join("gitpulse-shadowed")
    );
}

/// A non-executable file must not win the lookup: `execvp` skips it and
/// keeps searching, so an executable copy later in the search order has to
/// be picked instead of failing the eventual spawn with PermissionDenied.
#[cfg(unix)]
#[test]
fn spawn_resolution_skips_non_executable_candidate_for_later_match() {
    let first = tempfile::TempDir::new().unwrap();
    let second = tempfile::TempDir::new().unwrap();
    std::fs::write(first.path().join("gitpulse-exec-probe"), "data, not code").unwrap();
    let good = second.path().join("gitpulse-exec-probe");
    std::fs::write(&good, "#!/bin/sh\nexit 0\n").unwrap();
    std::fs::set_permissions(&good, std::os::unix::fs::PermissionsExt::from_mode(0o755)).unwrap();

    let path_var = std::env::join_paths([first.path(), second.path()]).expect("join dirs");
    let resolved = resolve_spawn_program_with("gitpulse-exec-probe", Some(&path_var), None);
    assert_eq!(
        Path::new(&resolved),
        &good,
        "executable copy in a later dir must beat a non-executable earlier one"
    );
}

/// Empty PATH entries mean CWD (POSIX) and relative entries resolve
/// against whatever cwd the child ends up with — both must be ignored by
/// the resolver even when they really contain the tool.
#[cfg(unix)]
#[test]
fn spawn_resolution_ignores_empty_and_relative_path_entries() {
    struct Cleanup(PathBuf);
    impl Drop for Cleanup {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }
    let rel = Cleanup(PathBuf::from("gitpulse-rel-probe-dir"));
    std::fs::create_dir_all(&rel.0).unwrap();
    std::fs::write(rel.0.join("gitpulse-rel-probe-tool"), "#!/bin/sh\n").unwrap();
    std::fs::set_permissions(
        rel.0.join("gitpulse-rel-probe-tool"),
        std::os::unix::fs::PermissionsExt::from_mode(0o755),
    )
    .unwrap();

    for hostile in ["", "::", "gitpulse-rel-probe-dir"] {
        let resolved = resolve_spawn_program_with(
            "gitpulse-rel-probe-tool",
            Some(std::ffi::OsStr::new(hostile)),
            None,
        );
        assert_eq!(
            resolved, "gitpulse-rel-probe-tool",
            "entry {hostile:?} must not resolve a relative candidate"
        );
    }
}

/// Broken symlinks and directories that merely share the tool's name are
/// not spawnpable candidates.
#[cfg(unix)]
#[test]
fn spawn_resolution_skips_broken_symlink_and_directory_named_like_tool() {
    let dir = tempfile::TempDir::new().unwrap();
    std::os::unix::fs::symlink(
        "/definitely/does/not/exist",
        dir.path().join("gitpulse-dangling"),
    )
    .unwrap();
    std::fs::create_dir(dir.path().join("gitpulse-dirname")).unwrap();

    for name in ["gitpulse-dangling", "gitpulse-dirname"] {
        let resolved = resolve_spawn_program_with(name, Some(dir.path().as_os_str()), None);
        assert_eq!(
            resolved, name,
            "{name} must be skipped rather than selected as a candidate"
        );
    }
}

/// The child PATH never carries CWD-searching empty entries, and a fully
/// absent PATH is seeded with the Unix default set instead of leaving
/// children with only Homebrew/fallback dirs.
#[cfg(unix)]
#[test]
fn extended_child_path_drops_cwd_entries_and_seeds_default_when_unset() {
    let home = tempfile::TempDir::new().unwrap();
    let colon_joined = extended_child_path(
        Some(std::ffi::OsStr::new("::")),
        Some(home.path().as_os_str()),
    )
    .expect("join");
    let entries: Vec<PathBuf> = std::env::split_paths(&colon_joined).collect();
    assert!(
        entries.iter().all(|e| !e.as_os_str().is_empty()),
        "empty CWD entries must be dropped: {entries:?}"
    );
    assert_eq!(
        entries.first(),
        Some(&PathBuf::from("/opt/homebrew/bin")),
        "with nothing inherited, fallbacks start immediately"
    );

    let unset = extended_child_path(None, Some(home.path().as_os_str())).expect("join");
    let seeded: Vec<PathBuf> = std::env::split_paths(&unset).collect();
    assert_eq!(
        &seeded[..2],
        [Path::new("/usr/bin"), Path::new("/bin")],
        "unset PATH must seed the Unix default search set"
    );
}

/// A relative entry resolves against the child's cwd, which is the repo.
#[cfg(unix)]
#[test]
fn extended_child_path_drops_relative_entries() {
    let home = tempfile::TempDir::new().unwrap();
    let hostile = extended_child_path(
        Some(std::ffi::OsStr::new(
            "./node_modules/.bin:.:bin:/usr/local/bin:../x:node_modules/.bin",
        )),
        Some(home.path().as_os_str()),
    )
    .expect("join");
    let entries: Vec<PathBuf> = std::env::split_paths(&hostile).collect();
    assert!(
        entries.iter().all(|e| e.is_absolute()),
        "relative PATH entries must be dropped: {entries:?}"
    );
    assert_eq!(entries.first(), Some(&PathBuf::from("/usr/local/bin")));
}

/// The extended child PATH appends only the fallback dirs that are not
/// already present, preserving inherited order and precedence.
#[cfg(unix)]
/// `argv` and `envp` share one buffer and `execve` refuses the whole spawn
/// once it is full. Appending six directories to a PATH already at that
/// ceiling is how a helpful change becomes the reason a working spawn
/// stops working — surfacing as "Failed to spawn git: Argument list too
/// long", with nothing pointing back here.
///
/// The ceiling is measured on the host rather than taken from a manual,
/// and the guard is asserted to sit below it with room to spare: a guard
/// at or above the real limit protects nothing while looking like it does.
#[test]
#[cfg(unix)]
fn a_path_at_the_exec_ceiling_is_never_grown() {
    let home = tempfile::TempDir::new().unwrap();
    let entry = "/".repeat(1000);

    let under = std::iter::repeat_n(entry.as_str(), (MAX_INHERITED_PATH_BYTES / 1001) - 1)
        .collect::<Vec<_>>()
        .join(":");
    assert!(
        under.len() < MAX_INHERITED_PATH_BYTES,
        "fixture must sit under the guard"
    );
    let extended = extended_child_path(
        Some(std::ffi::OsStr::new(&under)),
        Some(home.path().as_os_str()),
    )
    .expect("a PATH under the guard must still be extended");
    assert!(
        extended.len() > under.len(),
        "the fallback dirs were not appended below the guard"
    );

    for size in [
        MAX_INHERITED_PATH_BYTES,
        MAX_INHERITED_PATH_BYTES * 2,
        MAX_INHERITED_PATH_BYTES * 16,
    ] {
        assert!(
            extended_child_path(
                Some(std::ffi::OsStr::new(&"x".repeat(size))),
                Some(home.path().as_os_str())
            )
            .is_none(),
            "a {size}-byte PATH was grown toward E2BIG"
        );
    }

    let mut refused_at = None;
    for bytes in [64 * 1024, 256 * 1024, 512 * 1024, 1024 * 1024, 4096 * 1024] {
        let refused = Command::new("/usr/bin/true")
            .env_clear()
            .env("PATH", "x".repeat(bytes))
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status_locked()
            .is_err();
        if refused {
            refused_at = Some(bytes);
            break;
        }
    }
    match refused_at {
        Some(ceiling) => assert!(
            MAX_INHERITED_PATH_BYTES < ceiling,
            "the guard ({MAX_INHERITED_PATH_BYTES}) is at or above this host's measured exec \
                 ceiling ({ceiling}), so it protects nothing"
        ),
        // A host with no reachable ceiling cannot falsify the guard.
        // Saying so is the honest outcome; passing quietly would make an
        // unmeasured host read like a measured one.
        None => eprintln!("exec accepted a 4 MiB PATH: no ceiling in range, guard unverified"),
    }
}

/// Extending must never hand a child a *smaller* search path.
///
/// A dropped entry silently relocates which `git`, `gh` or `devmap` the
/// child resolves — the same wrong-binary class the installation-health
/// report exists to catch, introduced by the code that reports it.
#[test]
#[cfg(unix)]
fn extending_never_removes_an_inherited_entry() {
    let home = tempfile::TempDir::new().unwrap();
    for case in [
        "/usr/bin:/bin",
        // Empty entries mean "the current directory" to POSIX and are
        // dropped deliberately; everything else must survive.
        "/usr/bin::/bin:",
        "/opt/homebrew/bin:/usr/bin",
        "/a b/bin:/c\td/bin",
        "/usr/bin:/usr/bin:/usr/bin",
        "/ünïcode/bin:/usr/bin",
    ] {
        let extended = extended_child_path(
            Some(std::ffi::OsStr::new(case)),
            Some(home.path().as_os_str()),
        )
        .unwrap_or_else(|| panic!("join failed for {case}"));
        let before: Vec<PathBuf> = std::env::split_paths(std::ffi::OsStr::new(case))
            .filter(|entry| !entry.as_os_str().is_empty())
            .collect();
        let after: Vec<PathBuf> = std::env::split_paths(&extended).collect();
        for entry in &before {
            assert!(
                after.contains(entry),
                "{} vanished from the child PATH for {case}",
                entry.display()
            );
        }
        assert_eq!(
            after[..before.len()],
            before[..],
            "inherited precedence changed for {case}"
        );
    }
}

/// Resolving a tool and running it are two different PATH questions.
///
/// GitPulse finds `devmap` in `~/.local/bin` through the GUI fallback dirs
/// and then, before this existed, spawned it with launchd's minimal
/// `/usr/bin:/bin:/usr/sbin:/sbin`. `devmap doctor` resolves the bare
/// `devmap` command named by host MCP configs against *its* PATH, found
/// nothing, and reported "host MCP config names a devmap path that is not
/// a file: devmap — this is not version skew". GitPulse rendered that as
/// the user's broken install. It was our PATH.
///
/// Asserted as set containment against [`gui_launch_fallback_dirs`] rather
/// than a spelled-out list: a directory added there and not handed to the
/// child is the same bug again, and a hand-written expectation here would
/// keep passing through it.
#[test]
#[cfg(unix)]
fn a_spawned_tool_can_see_the_directory_it_was_resolved_from() {
    let home = tempfile::TempDir::new().unwrap();
    let mut cmd = Command::new("/usr/bin/true");
    default_child_path_with_env(
        &mut cmd,
        Some(std::ffi::OsStr::new("/usr/bin:/bin:/usr/sbin:/sbin")),
        Some(home.path().as_os_str()),
    );
    let child_path = cmd
        .get_envs()
        .find(|(key, _)| *key == std::ffi::OsStr::new("PATH"))
        .and_then(|(_, value)| value)
        .expect("an absent PATH must be filled in")
        .to_owned();
    let entries: Vec<PathBuf> = std::env::split_paths(&child_path).collect();
    for fallback in gui_launch_fallback_dirs(Some(home.path().as_os_str())) {
        assert!(
            entries.contains(&fallback),
            "child cannot see {}: {entries:?}",
            fallback.display()
        );
    }
    // Inherited entries keep their precedence: a fallback dir must never
    // shadow a tool the user deliberately put earlier on PATH.
    assert_eq!(entries[0], PathBuf::from("/usr/bin"));
}

/// The caller's own PATH is a decision, not an omission.
///
/// `build_capture_command`, the PTY and the harness sidecar each set one;
/// re-deriving it from this process would quietly discard the environment
/// the user is actually running in.
#[test]
#[cfg(unix)]
fn a_path_the_caller_chose_is_never_replaced() {
    let home = tempfile::TempDir::new().unwrap();
    let mut cmd = Command::new("/usr/bin/true");
    cmd.env("PATH", "/caller/chosen");
    default_child_path_with_env(
        &mut cmd,
        Some(std::ffi::OsStr::new("/usr/bin:/bin")),
        Some(home.path().as_os_str()),
    );
    let values: Vec<_> = cmd
        .get_envs()
        .filter(|(key, _)| *key == std::ffi::OsStr::new("PATH"))
        .map(|(_, value)| value.map(std::ffi::OsStr::to_owned))
        .collect();
    assert_eq!(
        values,
        vec![Some(std::ffi::OsString::from("/caller/chosen"))],
        "caller PATH replaced or duplicated"
    );
}

/// Setting PATH for a bare program name drops `std` off `posix_spawn` onto
/// `fork` (`env_saw_path() && !program_is_path()`), and forking a GUI
/// process this size is both slow and the shape behind the concurrent
/// descriptor races [`SpawnGate`] exists to bound.
///
/// Bare names are not left unhelped by this: they arrive through
/// `build_capture_command` or `git_command_with_env`, which resolve the
/// program to a path *and* set PATH themselves.
#[test]
#[cfg(unix)]
fn a_bare_program_name_keeps_posix_spawn() {
    let home = tempfile::TempDir::new().unwrap();
    let mut cmd = Command::new("true");
    default_child_path_with_env(
        &mut cmd,
        Some(std::ffi::OsStr::new("/usr/bin:/bin")),
        Some(home.path().as_os_str()),
    );
    assert_eq!(
        cmd.get_envs()
            .filter(|(key, _)| *key == std::ffi::OsStr::new("PATH"))
            .count(),
        0,
        "a bare program name must not gain a PATH override"
    );
    // A relative path is still a path, and `std` treats it as one.
    let mut relative = Command::new("./devmap");
    default_child_path_with_env(
        &mut relative,
        Some(std::ffi::OsStr::new("/usr/bin:/bin")),
        Some(home.path().as_os_str()),
    );
    assert_eq!(
        relative
            .get_envs()
            .filter(|(key, _)| *key == std::ffi::OsStr::new("PATH"))
            .count(),
        1,
        "a relative path is a path and must be given the child PATH"
    );
}

#[test]
#[cfg(unix)]
fn extended_child_path_appends_missing_fallback_dirs_only() {
    let home = tempfile::TempDir::new().unwrap();
    let extended = extended_child_path(
        Some(std::ffi::OsStr::new("/usr/bin:/bin")),
        Some(home.path().as_os_str()),
    )
    .expect("join must succeed for plain entries");
    let entries: Vec<PathBuf> = std::env::split_paths(&extended).collect();
    assert_eq!(entries[0], PathBuf::from("/usr/bin"));
    assert_eq!(entries[1], PathBuf::from("/bin"));
    for fallback in [
        PathBuf::from("/opt/homebrew/bin"),
        PathBuf::from("/usr/local/bin"),
        home.path().join(".local/bin"),
        home.path().join(".grok/bin"),
        // Go toolchain locations so govulncheck's own `go` spawn resolves.
        PathBuf::from("/usr/local/go/bin"),
        home.path().join("go/bin"),
        // Rust toolchain location, for the same nested-lookup reason: a
        // `cargo` resolved here still spawns `rustc` and its own
        // `cargo-*` subcommand binaries out of the child's PATH.
        home.path().join(".cargo/bin"),
    ] {
        assert!(
            entries.contains(&fallback),
            "fallback dir must be appended: {} in {entries:?}",
            fallback.display()
        );
    }

    // Already-present entries are never duplicated.
    let deduped = extended_child_path(
        Some(std::ffi::OsStr::new("/opt/homebrew/bin")),
        Some(home.path().as_os_str()),
    )
    .expect("join must succeed");
    let count = std::env::split_paths(&deduped)
        .filter(|p| p == Path::new("/opt/homebrew/bin"))
        .count();
    assert_eq!(count, 1, "must not duplicate an existing entry");
}

/// An explicit caller-provided PATH (via `extra_env`) outranks the child
/// PATH extension — the extension only fills an absent decision.
#[test]
fn extra_env_path_overrides_child_path_extension() {
    let home = tempfile::TempDir::new().unwrap();
    let cmd = build_capture_command(
        "sh",
        &[],
        None,
        &[("PATH", "/caller/chosen")],
        Some(std::ffi::OsStr::new("")),
        Some(home.path().as_os_str()),
    );
    let path_values: Vec<String> = cmd
        .get_envs()
        .filter(|(k, _)| *k == std::ffi::OsStr::new("PATH"))
        .filter_map(|(_, v)| v.map(|v| v.to_string_lossy().into_owned()))
        .collect();
    assert_eq!(path_values, vec!["/caller/chosen".to_string()]);
}

/// Regression (GUI launch + shebang scripts): resolving the top-level
/// program through the fallback dirs is not enough. `/opt/homebrew/bin/npm`
/// is a symlink to a `#!/usr/bin/env node` script; under a GUI-minimal
/// inherited PATH the spawn's own `env` interpreter lookup fails (exit 127,
/// "env: node: No such file or directory"), which reads downstream as
/// "npm is not installed". The child must see the fallback dirs on its own
/// PATH too.
///
/// Asserted on the command GitPulse builds, which is the whole of GitPulse's
/// side of this contract: the top-level program resolves to the fallback-dir
/// tool, and the child is handed a PATH on which that tool's `#!/usr/bin/env`
/// interpreter resolves. Both are decided before anything is spawned, so this
/// case cannot be lost to how long the host takes to start a process — which
/// is what the end-to-end sibling below is exposed to.
#[cfg(unix)]
#[test]
fn shebang_tool_in_fallback_dir_is_given_a_child_path_reaching_its_interpreter() {
    let home = tempfile::TempDir::new().unwrap();
    let bin = home.path().join(".local/bin");
    std::fs::create_dir_all(&bin).unwrap();
    let interpreter = bin.join("gitpulse-fake-interp");
    std::fs::write(&interpreter, "#!/bin/sh\necho INTERP_OK\n").unwrap();
    std::fs::set_permissions(
        &interpreter,
        std::os::unix::fs::PermissionsExt::from_mode(0o755),
    )
    .unwrap();
    let tool = bin.join("gitpulse-fake-shebang-tool");
    std::fs::write(&tool, "#!/usr/bin/env gitpulse-fake-interp\n").unwrap();
    std::fs::set_permissions(&tool, std::os::unix::fs::PermissionsExt::from_mode(0o755)).unwrap();

    let cmd = build_capture_command(
        "gitpulse-fake-shebang-tool",
        &[],
        None,
        &[],
        Some(std::ffi::OsStr::new("")),
        Some(home.path().as_os_str()),
    );

    // The tool itself was found in the fallback dir, not left as a bare name
    // for an empty PATH to fail on.
    assert_eq!(
        Path::new(cmd.get_program()),
        tool,
        "the program must resolve to the fallback-dir tool"
    );

    // And the child's own PATH reaches the interpreter that tool's shebang
    // names. Resolved by searching, not by string matching: `env` will do a
    // PATH search, so this asserts the thing `env` will actually decide.
    let child_path = cmd
        .get_envs()
        .find(|(key, _)| *key == std::ffi::OsStr::new("PATH"))
        .and_then(|(_, value)| value)
        .expect("the child must be handed a PATH");
    let found = std::env::split_paths(child_path)
        .map(|dir| dir.join("gitpulse-fake-interp"))
        .find(|candidate| candidate.is_file());
    assert_eq!(
        found.as_deref(),
        Some(interpreter.as_path()),
        "child PATH must reach the interpreter: {child_path:?}"
    );
}

/// The same contract, proved by running it: `/usr/bin/env` really does
/// resolve the interpreter through the PATH the sibling test above asserts.
///
/// `#[ignore]`d because its verdict is not about GitPulse. It is bounded by a
/// 30s deadline on a child that does nothing but `echo`, and children on a
/// loaded host do not reliably reach `main` inside any such bound: measured
/// here, spawned stubs that blew a 30s and a 60s deadline went on to exit 0
/// one and fifty-seven seconds later respectively. As a gate this reported
/// host load; the assertion it was meant to make is made deterministically
/// above. Raising the deadline would only make the misreport rarer.
///
/// Run it deliberately after changing spawn resolution or child PATH with
/// `cargo test --manifest-path src-tauri/Cargo.toml --lib
/// shebang_tool_in_fallback_dir_finds_interpreter_through_child_path --
/// --ignored`.
#[cfg(unix)]
#[test]
#[ignore = "bounds a real process spawn on a wall clock; host load, not GitPulse, decides it"]
fn shebang_tool_in_fallback_dir_finds_interpreter_through_child_path() {
    let home = tempfile::TempDir::new().unwrap();
    let bin = home.path().join(".local/bin");
    std::fs::create_dir_all(&bin).unwrap();
    let interpreter = bin.join("gitpulse-fake-interp");
    std::fs::write(&interpreter, "#!/bin/sh\necho INTERP_OK\n").unwrap();
    std::fs::set_permissions(
        &interpreter,
        std::os::unix::fs::PermissionsExt::from_mode(0o755),
    )
    .unwrap();
    let tool = bin.join("gitpulse-fake-shebang-tool");
    std::fs::write(&tool, "#!/usr/bin/env gitpulse-fake-interp\n").unwrap();
    std::fs::set_permissions(&tool, std::os::unix::fs::PermissionsExt::from_mode(0o755)).unwrap();

    // An empty inherited PATH stands in for a Finder/Dock launch: the tool
    // resolves via ~/.local/bin and its interpreter must resolve the same
    // way inside the child.
    let cmd = build_capture_command(
        "gitpulse-fake-shebang-tool",
        &[],
        None,
        &[],
        Some(std::ffi::OsStr::new("")),
        Some(home.path().as_os_str()),
    );
    let out = run_bounded(
        cmd,
        "gitpulse-fake-shebang-tool",
        Duration::from_secs(30),
        None,
    )
    .expect("spawn");
    assert!(
        out.success,
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(
        String::from_utf8_lossy(&out.stdout).contains("INTERP_OK"),
        "stdout: {}",
        String::from_utf8_lossy(&out.stdout)
    );
}

/// Node ships `npm.cmd` and an extension-less `npm` shell script side by
/// side. Resolving the latter hands `CreateProcess` a non-PE file, which is
/// where "%1 is not a valid Win32 application" came from.
#[test]
fn spawn_resolution_prefers_a_windows_executable_over_a_bare_script() {
    let names = spawn_candidate_names("npm");
    if cfg!(windows) {
        assert_eq!(
            names.last().map(String::as_str),
            Some("npm"),
            "the bare name must be the LAST resort, not the first: {names:?}"
        );
        assert!(
            names.iter().any(|name| name == "npm.cmd"),
            "npm.cmd must be reachable: {names:?}"
        );
        let cmd = names.iter().position(|name| name == "npm.cmd");
        let bare = names.iter().position(|name| name == "npm");
        assert!(cmd < bare, "npm.cmd must be tried before npm: {names:?}");
    } else {
        assert_eq!(names, vec!["npm".to_string()]);
    }
}

/// A name that already carries an executable suffix is used as written.
#[test]
fn spawn_resolution_never_double_suffixes_an_executable_name() {
    for program in ["gh.exe", "GH.EXE", "thing.cmd"] {
        let names = spawn_candidate_names(program);
        assert_eq!(
            names,
            vec![program.to_string()],
            "{program} must not gain a second suffix"
        );
    }
}

/// git refuses a verbatim path as a working tree
/// ("could not create work tree dir '\\?\C:\...': Invalid argument"), and
/// canonicalize hands one back for every path on Windows. These are the
/// shapes it actually produces.
#[test]
fn a_verbatim_windows_path_is_rewritten_in_ordinary_form() {
    assert_eq!(
        simplified_windows_path(r"\\?\C:\Users\runneradmin\repo").as_deref(),
        Some(r"C:\Users\runneradmin\repo")
    );
    assert_eq!(
        simplified_windows_path(r"\\?\UNC\server\share\repo").as_deref(),
        Some(r"\\server\share\repo")
    );
}

/// Not every verbatim path has an ordinary spelling. Guessing one would
/// produce a path that names a different file, or no file at all.
#[test]
fn a_path_with_no_ordinary_spelling_keeps_its_prefix() {
    for path in [
        r"\\?\Volume{b75e2c83-0000-0000-0000-602f00000000}\repo",
        r"C:\already\ordinary",
        r"\\server\share\already\unc",
        "/unix/style/path",
        r"\\?\",
        r"\\?\C:",
    ] {
        assert_eq!(
            simplified_windows_path(path),
            None,
            "{path} has no ordinary spelling and must keep what it had"
        );
    }
}

/// The property the clone path depends on, checked against a real
/// canonicalize rather than a literal.
#[cfg(windows)]
#[test]
fn canonicalize_plain_answers_in_a_spelling_git_accepts() {
    let dir = tempfile::TempDir::new().unwrap();
    let plain = canonicalize_plain(dir.path()).expect("temp dir canonicalizes");
    assert!(
        !plain.to_string_lossy().starts_with(r"\\?\"),
        "canonicalize_plain still returned a verbatim path: {plain:?}"
    );
    // And it still names the same directory it resolved.
    assert_eq!(
        plain.canonicalize().unwrap(),
        dir.path().canonicalize().unwrap(),
        "the ordinary spelling must denote the same directory"
    );
}

/// The text form of [`canonicalize_plain`], for the paths the command gate
/// and `git worktree add` receive already spelled: a canonicalized checkout
/// joined with a lane directory.
#[cfg(windows)]
#[test]
fn plain_path_text_drops_the_verbatim_prefix_git_cannot_use() {
    assert_eq!(
        plain_path_text(r"\\?\C:\Users\runneradmin\repo\.gitpulse\worktrees\lane"),
        r"C:\Users\runneradmin\repo\.gitpulse\worktrees\lane"
    );
    assert_eq!(
        plain_path_text(r"\\?\UNC\server\share\repo"),
        r"\\server\share\repo"
    );
    // No ordinary spelling exists: unchanged rather than invented.
    assert_eq!(
        plain_path_text(r"\\?\Volume{0123}\repo"),
        r"\\?\Volume{0123}\repo"
    );
    assert_eq!(plain_path_text(r"C:\already\plain"), r"C:\already\plain");
}

/// Off Windows a backslash is a filename character and `\\?\` means
/// nothing, so every spelling is left exactly as given.
#[cfg(not(windows))]
#[test]
fn plain_path_text_is_the_identity_off_windows() {
    for path in [r"\\?\C:\repo\wt", "/tmp/repo/wt", r"/tmp/odd\name", ""] {
        assert_eq!(plain_path_text(path), path);
    }
}

/// Off Windows there is nothing to strip, and a backslash is an ordinary
/// filename character -- rewriting one would rename the file.
#[cfg(not(windows))]
#[test]
fn canonicalize_plain_is_canonicalize_off_windows() {
    let dir = tempfile::TempDir::new().unwrap();
    assert_eq!(
        canonicalize_plain(dir.path()).unwrap(),
        dir.path().canonicalize().unwrap()
    );
}

/// Script-host inputs are never resolved: naming a tool must not hand
/// `wscript` a file this process did not mean to run.
#[test]
fn spawn_resolution_never_reaches_a_script_host_input() {
    let names = spawn_candidate_names("tool");
    for forbidden in [".vbs", ".js", ".wsf", ".jse", ".msc", ".ps1"] {
        assert!(
            !names.iter().any(|name| name.ends_with(forbidden)),
            "{forbidden} must not be a candidate: {names:?}"
        );
    }
}

/// Absolute paths pass through untouched, and a name found nowhere comes
/// back unchanged so the spawn error keeps naming the requested tool.
#[test]
fn spawn_resolution_passes_through_separators_and_total_misses() {
    assert_eq!(
        resolve_spawn_program_with("/bin/sh", Some(std::ffi::OsStr::new("")), None),
        "/bin/sh"
    );
    assert_eq!(
        resolve_spawn_program_with(
            "gitpulse-no-such-tool",
            Some(std::ffi::OsStr::new("")),
            None
        ),
        "gitpulse-no-such-tool"
    );
}

/// The failure contract is preserved: an unresolvable program still fails
/// `capture_command` with the original tool name in the message.
#[test]
fn capture_command_error_still_names_unresolvable_tool() {
    let err = capture_command(
        "gitpulse-no-such-tool-xyz",
        &[],
        None,
        Duration::from_secs(5),
        &[],
    )
    .expect_err("unresolvable tool must error");
    assert!(
        err.contains("gitpulse-no-such-tool-xyz"),
        "error must keep the bare tool name, got: {err}",
        err = err.message()
    );
}

#[test]
fn lc_all_only_moves_into_lc_ctype_when_it_is_the_users_only_encoding() {
    use std::ffi::OsStr;
    let v = |s: &'static str| Some(OsStr::new(s));
    assert_eq!(
        lc_ctype_from_inherited(v("de_DE.UTF-8"), None, None).as_deref(),
        v("de_DE.UTF-8")
    );
    assert_eq!(
        lc_ctype_from_inherited(v("C"), v("en_US.UTF-8"), None),
        None
    );
    assert_eq!(
        lc_ctype_from_inherited(v("C"), None, v("en_US.UTF-8")),
        None
    );
    assert_eq!(lc_ctype_from_inherited(None, v("en_US.UTF-8"), None), None);
    assert_eq!(lc_ctype_from_inherited(v(""), None, None), None);
}

/// An observer that keeps what it needs sees every byte of a long stream
/// while the run retains almost none of it, and the byte cap still ends a
/// stream that outruns it.
#[cfg(unix)]
#[test]
fn a_draining_observer_streams_without_the_run_buffering_the_output() {
    struct Counting {
        seen: usize,
    }
    impl ProcessObserver for Counting {
        fn output(&mut self, stream: OutputStream, bytes: &[u8]) {
            if matches!(stream, OutputStream::Stdout) {
                self.seen += bytes.len();
            }
        }
        fn drains_stdout(&self) -> bool {
            true
        }
    }
    let run_with_cap = |cap: usize| {
        let mut cmd = Command::new("sh");
        cmd.args(["-c", "head -c 3000000 /dev/zero"]);
        let mut observer = Counting { seen: 0 };
        let run = run_observed(
            &mut cmd,
            "sh",
            Duration::from_secs(30),
            None,
            cap,
            &mut observer,
        )
        .expect("run");
        (run, observer.seen)
    };
    let (whole, seen) = run_with_cap(4_000_000);
    assert!(whole.incomplete.is_none(), "{:?}", whole.incomplete);
    assert_eq!(seen, 3_000_000, "the observer saw the whole stream");
    assert!(
        whole.stdout.len() < 1_000_000,
        "the run kept {} of 3,000,000 bytes",
        whole.stdout.len()
    );
    let (cut, seen) = run_with_cap(100_000);
    assert_eq!(cut.incomplete, Some(Incomplete::OverCap(100_000)));
    assert!(
        seen <= 100_000,
        "the cap bounds a streamed read too: {seen}"
    );
}

#[test]
fn network_timeout_exceeds_default_and_is_30_minutes() {
    assert_eq!(NETWORK_TIMEOUT, Duration::from_secs(30 * 60));
    assert!(NETWORK_TIMEOUT > DEFAULT_TIMEOUT);
}

/// A call that outlives its deadline must fail with the bounded runner's
/// timeout wording (the same engine `git_with_timeout` drives), not hang.
#[test]
fn short_timeout_kills_slow_command_with_timeout_error() {
    let started = Instant::now();
    let outcome = run_bounded(
        {
            let mut cmd = Command::new("sh");
            cmd.args(["-c", "sleep 30"]);
            cmd
        },
        "sh sleep 30",
        Duration::from_millis(300),
        None,
    );
    let Err(err) = outcome else {
        panic!("slow command must hit the deadline");
    };
    assert!(err.contains("timed out"), "got: {err}", err = err.message());
    assert!(
        started.elapsed() < crate::test_support::coverage_relaxed(Duration::from_secs(10)),
        "kill must be prompt, took {:?}",
        started.elapsed()
    );
}

#[test]
fn capture_command_surfaces_timeout_wording() {
    let err = capture_command(
        "sh",
        &["-c", "sleep 30"],
        None,
        Duration::from_millis(250),
        &[],
    )
    .expect_err("must time out");
    assert!(
        err.contains("timed out after"),
        "got: {err}",
        err = err.message()
    );
}

/// The pulse log is a capped read. Killing it at the deadline used to
/// drop every commit git had already printed and surface
/// `git log timed out after 90s` as a diagnostics error. The bytes stay,
/// and a caller that did not opt into a prefix still gets the error.
#[cfg(unix)]
#[test]
fn a_deadline_keeps_bytes_already_written_for_a_capped_read() {
    // `/bin/echo` exits before the sleep, so the line is in the pipe
    // even though a pipe is fully buffered for a still-running writer.
    let script = "/bin/echo pulse-prefix; sleep 30";
    let started = Instant::now();
    let kept = KeepDeadlinePrefix::enter();
    let run = run_bounded(
        {
            let mut cmd = Command::new("sh");
            cmd.args(["-c", script]);
            cmd
        },
        "git log",
        // Scaled: a starved `sh` that has not run `echo` by the deadline
        // leaves nothing to keep, which is not the behaviour under test.
        crate::test_support::coverage_relaxed(Duration::from_millis(300)),
        None,
    )
    .expect("bytes written before the deadline are the result");
    drop(kept);
    assert!(
        started.elapsed() < crate::test_support::coverage_relaxed(Duration::from_secs(5)),
        "kill must be prompt, took {:?}",
        started.elapsed()
    );
    assert!(!run.success);
    assert!(
        String::from_utf8_lossy(&run.stdout).contains("pulse-prefix"),
        "stdout: {}",
        String::from_utf8_lossy(&run.stdout)
    );
    match &run.incomplete {
        Some(Incomplete::Deadline {
            message,
            over_cap: None,
        }) => {
            assert!(message.contains("timed out after"), "{message}");
            assert!(!message.contains(SLOT_WAIT_SUFFIX), "{message}");
            assert!(!is_deferred_under_load(message), "{message}");
        }
        other => panic!("expected a deadline prefix, got {other:?}"),
    }

    let discarded = run_bounded(
        {
            let mut cmd = Command::new("sh");
            cmd.args(["-c", script]);
            cmd
        },
        "git log",
        Duration::from_millis(200),
        None,
    )
    .expect_err("a caller that needs the whole stream still fails");
    assert!(
        discarded.contains("timed out after"),
        "{discarded}",
        discarded = discarded.message()
    );
    assert!(
        !discarded.contains("pulse-prefix"),
        "{discarded}",
        discarded = discarded.message()
    );
}

/// A non-zero exit that printed something is the child failing, not a
/// deadline. Keeping prefixes must not relabel that run.
#[cfg(unix)]
#[test]
fn a_failing_child_is_not_a_deadline_prefix() {
    let _kept = KeepDeadlinePrefix::enter();
    let run = run_bounded(
        {
            let mut cmd = Command::new("sh");
            cmd.args(["-c", "/bin/echo not-a-timeout; exit 1"]);
            cmd
        },
        "git log",
        Duration::from_secs(5),
        None,
    )
    .expect("exit 1 still finishes; it is not a deadline");
    assert!(!run.success);
    assert!(
        run.incomplete.is_none(),
        "exit 1 must not be a deadline prefix: {:?}",
        run.incomplete
    );
    assert!(
        String::from_utf8_lossy(&run.stdout).contains("not-a-timeout"),
        "stdout: {}",
        String::from_utf8_lossy(&run.stdout)
    );
}

/// No bytes and a deadline is not an empty successful read.
#[cfg(unix)]
#[test]
fn an_empty_deadline_is_not_an_empty_success() {
    let _kept = KeepDeadlinePrefix::enter();
    let run = run_bounded(
        {
            let mut cmd = Command::new("sh");
            cmd.args(["-c", "sleep 30"]);
            cmd
        },
        "git log",
        Duration::from_millis(200),
        None,
    )
    .expect("the deadline is a prefix, including an empty one");
    assert!(run.stdout.is_empty());
    assert!(!run.success);
    assert!(matches!(
        run.incomplete,
        Some(Incomplete::Deadline { over_cap: None, .. })
    ));
}

/// Several capped reads killed together must each keep their own prefix
/// and must not leave the sleep behind.
#[cfg(unix)]
#[test]
fn concurrent_deadline_prefixes_are_reaped() {
    // A gate of its own with a slot per reader. The process-wide gate
    // sizes itself from the machine (`spawn_limit`): a 3-core runner gets
    // 6 slots, so two readers spent their whole 400ms queued and failed
    // as slot waits, never reaching the deadline prefix under test.
    let gate: &'static SpawnGate = Box::leak(Box::new(SpawnGate::new(8)));
    let dir = tempfile::tempdir().unwrap();
    let mut handles = Vec::new();
    for index in 0..8 {
        let pidfile = dir.path().join(format!("pid-{index}"));
        handles.push(std::thread::spawn(move || {
            let script = format!(
                "echo $$ > '{}'; /bin/echo row-{index}; sleep 30",
                pidfile.display()
            );
            let _kept = KeepDeadlinePrefix::enter();
            let started = Instant::now();
            let mut cmd = Command::new("sh");
            cmd.args(["-c", &script]);
            let run = run_with_gate(
                &mut cmd,
                "git log",
                crate::test_support::coverage_relaxed(Duration::from_millis(400)),
                None,
                MAX_OUTPUT_BYTES,
                &mut (),
                gate,
            )
            .expect("prefix");
            assert!(
                started.elapsed() < crate::test_support::coverage_relaxed(Duration::from_secs(5)),
                "reader {index} took {:?}",
                started.elapsed()
            );
            assert!(String::from_utf8_lossy(&run.stdout).contains(&format!("row-{index}")));
            assert!(matches!(run.incomplete, Some(Incomplete::Deadline { .. })));
            pidfile
        }));
    }
    let pidfiles: Vec<_> = handles
        .into_iter()
        .map(|handle| handle.join().expect("reader"))
        .collect();
    for pidfile in pidfiles {
        let pid = std::fs::read_to_string(&pidfile).expect("pid");
        let pid = pid.trim();
        let alive = crate::test_support::process_alive(pid.parse().expect("pid"));
        assert!(!alive, "deadline left pid {pid} running ({pidfile:?})");
    }
}

/// Regression (audit A4): the numbered-config channel injects arbitrary
/// git config without any of the fixed names; SSH/askpass overrides can
/// hijack transport and credentials. All must be classified as injected.
#[test]
fn numbered_config_and_transport_overrides_are_injected_env() {
    for name in [
        "GIT_CONFIG_COUNT",
        "GIT_CONFIG_KEY_0",
        "GIT_CONFIG_VALUE_0",
        "GIT_CONFIG_KEY_17",
        "git_config_key_3",
        "GIT_CONFIG_PARAMETERS",
        "GIT_CONFIG_GLOBAL",
        "GIT_CONFIG_SYSTEM",
        "GIT_SSH_COMMAND",
        "GIT_SSH_VARIANT",
        "GIT_ASKPASS",
        "GIT_CREDHELPER",
        "GIT_EXTERNAL_DIFF",
        "GIT_SEQUENCE_EDITOR",
        "GIT_EXEC_PATH",
        // Tracing floods the few bytes of stderr that are kept, and the
        // fatal line is the one that goes missing.
        "GIT_TRACE",
        "GIT_SSH",
        "GIT_PROXY_COMMAND",
        "GIT_DIFF_OPTS",
        "GIT_LITERAL_PATHSPECS",
    ] {
        assert!(is_injected_git_env(name), "{name} must be stripped");
    }
    for safe in ["GIT_AUTHOR_NAME", "GIT_PAGER", "EDITOR", "PATH"] {
        assert!(!is_injected_git_env(safe), "{safe} must survive");
    }
}

/// Captured external tools (`gh pr checkout` shells to git; npm/go/cargo
/// read git config) inherit no injected-GIT-* redirection either, while an
/// explicitly passed `extra_env` value still wins over the strip.
#[test]
fn capture_children_strip_injected_git_env_but_extra_env_wins() {
    let explicit_git_dir = "/explicit/caller-chosen";
    let cmd = build_capture_command(
        "gh",
        &["--version"],
        None,
        &[("GIT_DIR", explicit_git_dir)],
        None,
        None,
    );
    let value_of = |key: &str| -> Option<Option<std::ffi::OsString>> {
        cmd.get_envs()
            .find(|(k, _)| *k == std::ffi::OsStr::new(key))
            .map(|(_, v)| v.map(|v| v.to_os_string()))
    };
    for key in [
        "GIT_WORK_TREE",
        "GIT_INDEX_FILE",
        "GIT_OBJECT_DIRECTORY",
        "GIT_COMMON_DIR",
        "GIT_NAMESPACE",
        "GIT_CONFIG",
        "GIT_CONFIG_COUNT",
        "GIT_CONFIG_PARAMETERS",
        "GIT_EXTERNAL_DIFF",
        "GIT_EXEC_PATH",
    ] {
        assert_eq!(
            value_of(key),
            Some(None),
            "{key} must be stripped from captured-tool children"
        );
    }
    // The editors are pinned, not stripped: captured tools shell out to
    // git, and removal alone leaves git's `core.editor` config fallback
    // open (a repository carrying `sequence.editor = sh -c '…'` would get
    // it executed). See git_command_pins_editors_to_a_harmless_program.
    for key in ["GIT_EDITOR", "GIT_SEQUENCE_EDITOR"] {
        assert_eq!(
            value_of(key)
                .and_then(|v| v.map(|s| s.to_string_lossy().into_owned()))
                .as_deref(),
            Some("true"),
            "{key} must be pinned for captured-tool children, not merely removed"
        );
    }
    assert_eq!(
        value_of("GIT_DIR").and_then(|v| v.map(|s| s.to_string_lossy().into_owned())),
        Some(explicit_git_dir.to_string()),
        "caller-supplied extra_env must override the strip"
    );
    // gh-specific prompt suppression travels with every captured child.
    assert_eq!(
        value_of("GH_PROMPT_DISABLED").map(|v| v.is_some()),
        Some(true)
    );
}

/// Regression (M3): a child that exits successfully while a daemonized
/// grandchild keeps the stdout/stderr write ends open used to block the
/// unconditional drain joins forever. The run must return promptly with
/// the child's exit status.
#[cfg(unix)]
#[test]
fn run_bounded_success_does_not_hang_on_grandchild_holding_pipes() {
    let started = Instant::now();
    let mut cmd = Command::new("sh");
    // `sh` backgrounds `sleep 30` (which inherits both pipe write ends)
    // and then itself exits 0 immediately.
    cmd.args(["-c", "sleep 30 & exit 0"]);
    let out = run_bounded(cmd, "sh", Duration::from_secs(5), None).expect("run");
    assert!(out.success);
    // The measured span is one `DRAIN_JOIN_GRACE` per pipe (the success
    // path gives stdout and stderr a window each) plus `spawn_gate()`
    // queueing, which parks on a condvar with no timeout and is therefore
    // unbounded whenever the parallel test harness has the gate saturated.
    // On a loaded macOS runner that queueing alone pushed this past an 8s
    // budget. What the assertion defends is that the call does not wait
    // out the 30s grandchild, so the budget stays far below it — the same
    // tolerance the sibling timeout test already carries.
    let budget = DRAIN_JOIN_GRACE * 2 + Duration::from_secs(10);
    assert!(
        started.elapsed() < budget,
        "drain collection must be grace-bounded (budget {budget:?}), took {:?}",
        started.elapsed()
    );
    // The status is trustworthy; the output is not. EOF never came, so
    // what was collected is a prefix of unknown length and must not be
    // handed on as the child's complete output.
    assert!(
        out.incomplete.is_some(),
        "an undelivered drain must report as a prefix, not as the whole stream"
    );
    assert!(
        String::from_utf8_lossy(&out.stderr).contains("stdout incomplete"),
        "and it must say why: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    // And it must be the RIGHT prefix reason. `sh` printed nothing at all,
    // so anything that classifies this as an over-cap read is inventing a
    // cause — which is exactly how a 1,482-byte `for-each-ref` reached a
    // user's diagnostics log as "output exceeded 64 MB".
    assert!(
        matches!(out.incomplete, Some(Incomplete::Unread(_))),
        "an unread stream is not an over-cap one: {:?}",
        out.incomplete
    );
}

/// The end-to-end shape of the same defect: a caller that turns a prefix
/// into an error must name what happened. `capture_command` is the seam
/// every non-git tool goes through, and it shares `git_timeout`'s bug.
#[cfg(unix)]
#[test]
fn an_unread_stream_is_not_reported_as_an_over_cap_one() {
    // Exits 0 immediately; the backgrounded child inherits the pipe write
    // ends, so EOF never arrives and the drain misses its window.
    let err = capture_command(
        "sh",
        &["-c", "sleep 30 & exit 0"],
        None,
        Duration::from_secs(5),
        &[],
    )
    .expect_err("a stdout of unknown completeness must not pass as complete");
    assert!(
        !err.contains("exceeded"),
        "nothing was printed, so nothing exceeded any cap: {err}",
        err = err.message()
    );
    assert!(
        err.contains("could not be read to the end"),
        "the error must name the real cause: {err}",
        err = err.message()
    );
}

/// The other arm still reads the way it always did, so fixing the wrong
/// message did not cost the right one.
#[test]
fn an_over_cap_stream_still_reports_its_budget() {
    assert_eq!(
        Incomplete::OverCap(MAX_OUTPUT_BYTES).describe(),
        "exceeded 64 MB"
    );
    // A sub-megabyte budget must not round to "exceeded 0 MB".
    assert_eq!(Incomplete::OverCap(8_192).describe(), "exceeded 8192 bytes");
    assert!(Incomplete::Unread("reader did not finish".into())
        .describe()
        .contains("could not be read to the end"));
    // The clause carries no subject of its own, so a caller that names one
    // cannot end up saying "output output exceeded 64 MB".
    assert!(
        !Incomplete::OverCap(MAX_OUTPUT_BYTES)
            .describe()
            .contains("output"),
        "describe() must stay subject-free; callers supply the subject"
    );
}

/// Regression (M3): a timed-out child whose backgrounded grandchild keeps
/// the pipes open must still yield the timeout error within the grace
/// window instead of leaking unbounded blocking work into the caller.
#[cfg(unix)]
#[test]
fn run_bounded_timeout_returns_despite_grandchild_holding_pipes() {
    let started = Instant::now();
    let mut cmd = Command::new("sh");
    cmd.args(["-c", "sleep 30 & sleep 30"]);
    let err = run_bounded(cmd, "sh", Duration::from_secs(1), None).expect_err("timeout");
    assert!(err.contains("timed out"), "got: {err}", err = err.message());
    assert!(
        started.elapsed() < crate::test_support::coverage_relaxed(Duration::from_secs(15)),
        "timeout handling must stay bounded (one shared grace window), took {:?}",
        started.elapsed()
    );
}

/// A pipe that stops mid-stream must not read as a pipe that ended.
///
/// The prefix is well-formed either way, so nothing downstream can tell
/// the difference on the bytes alone: a half-read `git show -s --format=…`
/// is a valid string with too few fields, which is how the parser came to
/// report "Failed to parse commit metadata format" about a commit that was
/// perfectly fine. Only this flag separates the two.
#[test]
fn a_broken_read_is_not_reported_as_the_end_of_the_stream() {
    struct BreaksAfter(&'static [u8], bool);
    impl Read for BreaksAfter {
        fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
            if self.1 {
                return Err(std::io::Error::other("pipe went away"));
            }
            self.1 = true;
            let n = self.0.len().min(buf.len());
            buf[..n].copy_from_slice(&self.0[..n]);
            Ok(n)
        }
    }

    let broken = drain_capped(Some(BreaksAfter(b"abc\0def", false)), 1024);
    assert_eq!(broken.bytes, b"abc\0def");
    assert!(!broken.truncated, "it did not hit the byte cap");
    assert!(
        matches!(broken.stop, Some(Stop::Broken(_))),
        "a read that failed must say so, or its prefix passes for the whole output"
    );

    let clean = drain_capped(Some(std::io::Cursor::new(b"abc\0def".to_vec())), 1024);
    assert_eq!(clean.bytes, b"abc\0def");
    assert!(clean.stop.is_none(), "a clean read reports no stop reason");

    let capped = drain_capped(Some(std::io::Cursor::new(vec![b'x'; 64])), 8);
    assert_eq!(capped.bytes.len(), 8);
    assert!(capped.truncated, "the cap is truncation, not failure");
    assert!(capped.stop.is_none());

    // No pipe at all is an empty stream, not a broken one.
    let none = drain_capped(None::<std::io::Cursor<Vec<u8>>>, 8);
    assert!(none.bytes.is_empty() && !none.truncated && none.stop.is_none());
}

/// A drain that never delivered produced *unknown*, and "the child printed
/// nothing" is a real, common answer — so the two must not arrive as the
/// same empty value.
#[test]
fn an_undelivered_drain_is_not_an_empty_output() {
    let (tx, rx) = mpsc::channel::<Drained>();
    let deadline = Instant::now() + Duration::from_millis(20);
    let missing = collect_drained_deadline(&rx, deadline);
    assert!(
        matches!(missing.stop, Some(Stop::Undelivered(_))),
        "nothing arrived, which is not the same as nothing being there"
    );
    assert!(missing.bytes.is_empty());

    tx.send(Drained::default()).expect("send");
    let delivered = collect_drained_deadline(&rx, Instant::now() + Duration::from_secs(1));
    assert!(
        delivered.stop.is_none(),
        "a child that genuinely printed nothing reports no stop reason"
    );
    assert!(delivered.bytes.is_empty());
}

/// The gate's own contract, checked without racing a real process: once
/// `limit` permits are out, the next acquire parks until one is dropped.
#[test]
fn spawn_gate_parks_callers_once_the_limit_is_reached() {
    let gate = std::sync::Arc::new(SpawnGate::new(2));
    let first = gate
        .acquire(Instant::now() + Duration::from_secs(30))
        .unwrap();
    let second = gate
        .acquire(Instant::now() + Duration::from_secs(30))
        .unwrap();
    assert_eq!(gate.peak(), 2);

    let (tx, rx) = mpsc::channel();
    let waiter = {
        let gate = std::sync::Arc::clone(&gate);
        thread::spawn(move || {
            let permit = gate
                .acquire(Instant::now() + Duration::from_secs(30))
                .unwrap();
            let _ = tx.send(());
            drop(permit);
        })
    };

    assert!(
        rx.recv_timeout(Duration::from_millis(250)).is_err(),
        "a third caller must park while both permits are held"
    );
    drop(second);
    rx.recv_timeout(Duration::from_secs(5))
        .expect("dropping a permit must wake a parked caller");
    waiter.join().expect("waiter thread");
    drop(first);
    assert_eq!(
        gate.peak(),
        2,
        "the high-water mark must never exceed the limit"
    );
}

#[cfg(unix)]
#[test]
fn queued_commands_expire_before_a_subprocess_slot_is_available() {
    let gate: &'static SpawnGate = Box::leak(Box::new(SpawnGate::new(2)));
    let permits: Vec<_> = (0..gate.limit)
        .map(|_| {
            gate.acquire(Instant::now() + Duration::from_secs(10))
                .expect("test permit")
        })
        .collect();
    let (tx, rx) = mpsc::channel();
    let child_gate = gate;
    let worker = thread::spawn(move || {
        let result = run_with_gate(
            &mut Command::new("/usr/bin/true"),
            "queue-test",
            Duration::from_millis(20),
            None,
            MAX_OUTPUT_BYTES,
            &mut (),
            child_gate,
        );
        tx.send(result).unwrap();
    });
    let result = rx.recv_timeout(Duration::from_millis(300));
    drop(permits);
    worker.join().unwrap();
    let error = result
        .expect("queue wait must obey the command deadline")
        .expect_err("the queued command must not start");
    assert!(
        error.contains("waiting for a process slot"),
        "{error}",
        error = error.message()
    );
    assert!(
        error.contains(TIMEOUT_MARKER),
        "{error}",
        error = error.message()
    );
}

/// Deterministic, dependency-free generator for the stress tests: a fixed
/// seed reproduces the exact schedule of classes, deadlines and cancels.
struct Lcg(u64);
impl Lcg {
    fn next(&mut self) -> u64 {
        self.0 = self
            .0
            .wrapping_mul(6_364_136_223_846_793_005)
            .wrapping_add(1_442_695_040_888_963_407);
        self.0 >> 33
    }
}

/// `cancelled` is caller code. A waiter whose check panicked left its
/// waiting reservation behind, and a stranded interactive or background
/// reservation makes every later refresh ineligible for good.
#[test]
fn a_panicking_cancel_check_does_not_strand_its_reservation() {
    use super::{with_admission, Admission};
    for class in [Admission::Interactive, Admission::Background] {
        let gate = Arc::new(SpawnGate::new(1));
        let held = gate
            .acquire(Instant::now() + Duration::from_secs(1))
            .unwrap();
        let waiter = {
            let gate = Arc::clone(&gate);
            thread::spawn(move || {
                std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                    with_admission(class, || {
                        let _ = gate
                            .acquire_until(Instant::now() + Duration::from_secs(2), &|| {
                                panic!("observer failed")
                            });
                    })
                }))
                .is_err()
            })
        };
        let (tx, rx) = mpsc::channel();
        thread::spawn(move || {
            let _ = tx.send(waiter.join());
        });
        let panicked = rx
            .recv_timeout(Duration::from_secs(5))
            .expect("the waiter never finished")
            .expect("the waiter thread itself panicked");
        assert!(panicked, "the check must have panicked inside the gate");
        drop(held);
        assert!(
            gate.acquire(Instant::now() + Duration::from_millis(300))
                .is_some(),
            "{class:?} reservation stranded by a panicking check"
        );
        let state = gate
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        assert_eq!(
            (state.waiting_interactive, state.waiting_background),
            (0, 0)
        );
    }
}

/// A cancel check that took a lock of its own, or re-entered the gate,
/// deadlocked against a gate that called it while holding its state.
#[test]
fn the_cancel_check_runs_without_the_gate_lock() {
    use std::sync::atomic::{AtomicBool, Ordering};
    let gate = Arc::new(SpawnGate::new(1));
    let held = gate
        .acquire(Instant::now() + Duration::from_secs(1))
        .unwrap();
    let checked = AtomicBool::new(false);
    let lock_free = AtomicBool::new(true);
    let probe = Arc::clone(&gate);
    let outcome = gate.acquire_until(Instant::now() + Duration::from_millis(150), &|| {
        checked.store(true, Ordering::Relaxed);
        if probe.state.try_lock().is_err() {
            lock_free.store(false, Ordering::Relaxed);
        }
        false
    });
    assert!(outcome.is_err(), "the only slot is held");
    assert!(checked.load(Ordering::Relaxed), "the check never ran");
    assert!(
        lock_free.load(Ordering::Relaxed),
        "the cancel check ran while the gate held its own lock"
    );
    drop(held);
}

/// Seeded stress across every class, deadline and cancellation shape. It
/// asserts the gate's invariants, never a duration: a slow host only
/// loosens the rate bound.
#[test]
fn mixed_class_stress_holds_every_gate_invariant() {
    use super::{with_admission, Admission};
    use std::sync::atomic::{AtomicU64, Ordering};
    const THREADS: u64 = 24;
    const ROUNDS: usize = 40;
    let (limit, burst, rate) = (8usize, 16u32, 20u32);
    let gate = Arc::new(SpawnGate::with_rate(limit, burst, rate));
    let attempts = Arc::new([AtomicU64::new(0), AtomicU64::new(0), AtomicU64::new(0)]);
    let violations = Arc::new(Mutex::new(Vec::<String>::new()));
    let started = Instant::now();
    let (done_tx, done_rx) = mpsc::channel();
    for seed in 0..THREADS {
        let gate = Arc::clone(&gate);
        let attempts = Arc::clone(&attempts);
        let violations = Arc::clone(&violations);
        let done_tx = done_tx.clone();
        thread::spawn(move || {
            let mut rng = Lcg(0x9E37_79B9_7F4A_7C15 ^ (seed + 1).wrapping_mul(0xBF58_476D));
            for _ in 0..ROUNDS {
                let class = Admission::ALL[(rng.next() % 3) as usize];
                let wait = Duration::from_millis(1 + rng.next() % 30);
                let cancel_at = rng
                    .next()
                    .is_multiple_of(4)
                    .then(|| Instant::now() + Duration::from_millis(rng.next() % 10));
                attempts[class.index()].fetch_add(1, Ordering::Relaxed);
                let outcome = with_admission(class, || {
                    gate.acquire_until(Instant::now() + wait, &|| {
                        cancel_at.is_some_and(|at| Instant::now() >= at)
                    })
                });
                if let Ok(permit) = outcome {
                    {
                        let state = gate
                            .state
                            .lock()
                            .unwrap_or_else(std::sync::PoisonError::into_inner);
                        let mut found = violations.lock().unwrap();
                        if state.in_flight > limit {
                            found.push(format!("in_flight {} > {limit}", state.in_flight));
                        }
                        if state.background > (limit / 4).max(1) {
                            found.push(format!("background {}", state.background));
                        }
                    }
                    let hold = rng.next() % 3;
                    if hold > 0 {
                        thread::sleep(Duration::from_millis(hold));
                    }
                    drop(permit);
                }
            }
            let _ = done_tx.send(());
        });
    }
    drop(done_tx);
    for _ in 0..THREADS {
        done_rx
            .recv_timeout(Duration::from_secs(60))
            .expect("a stress thread hung inside the gate");
    }
    let elapsed = started.elapsed();
    let found = violations.lock().unwrap();
    assert!(found.is_empty(), "{found:?}");
    let state = gate.state.lock().unwrap();
    assert_eq!(
        (
            state.in_flight,
            state.background,
            state.waiting_background,
            state.waiting_interactive
        ),
        (0, 0, 0, 0),
        "a reservation outlived its waiter"
    );
    assert!(state.peak <= limit);
    for class in Admission::ALL {
        let c = state.counters[class.index()];
        assert_eq!(
            c.admitted + c.refused + c.shed + c.timed_out + c.cancelled,
            attempts[class.index()].load(Ordering::Relaxed),
            "{class:?}: every attempt has exactly one outcome: {c:?}"
        );
    }
    let rate_limited = state.counters[Admission::Reactive.index()].admitted
        + state.counters[Admission::Background.index()].admitted;
    let bound = u64::from(burst) + (f64::from(rate) * elapsed.as_secs_f64()).ceil() as u64 + 1;
    assert!(
        rate_limited <= bound,
        "{rate_limited} rate-limited admissions in {elapsed:?}; the cap allows {bound}"
    );
    let reactive = state.counters[Admission::Reactive.index()];
    assert!(
        reactive.refused + reactive.timed_out + reactive.cancelled > 0,
        "the schedule never pushed the gate past its limits: {reactive:?}"
    );
    assert!(state.counters[Admission::Interactive.index()].admitted > 0);
}

/// The hazard shared reads exist to avoid, under load: a caller must
/// never be answered by a child that started before it asked. A writer
/// bumps an epoch file (by rename) and publishes the new value only
/// afterwards; each child `cat`s the file, so its answer is at least the
/// epoch its caller saw on arrival — unless it joined an older child.
#[cfg(unix)]
#[test]
fn a_shared_read_never_answers_with_a_child_older_than_its_caller() {
    use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
    let gate: &'static SpawnGate = Box::leak(Box::new(SpawnGate::new(2)));
    let dir = tempfile::TempDir::new().unwrap();
    let cwd = dir.path().canonicalize().unwrap();
    std::fs::write(cwd.join("epoch"), "0").unwrap();
    let published = Arc::new(AtomicU64::new(0));
    let stop = Arc::new(AtomicBool::new(false));
    let writer = {
        let (cwd, published, stop) = (cwd.clone(), Arc::clone(&published), Arc::clone(&stop));
        thread::spawn(move || {
            let mut epoch = 0u64;
            while !stop.load(Ordering::Relaxed) {
                epoch += 1;
                let tmp = cwd.join("epoch.tmp");
                std::fs::write(&tmp, epoch.to_string()).unwrap();
                std::fs::rename(&tmp, cwd.join("epoch")).unwrap();
                published.store(epoch, Ordering::SeqCst);
                thread::sleep(Duration::from_millis(2));
            }
        })
    };
    // Start with every slot held so the first wave provably shares.
    let held: Vec<_> = (0..2)
        .map(|_| {
            gate.acquire(Instant::now() + Duration::from_secs(1))
                .unwrap()
        })
        .collect();
    let (done_tx, done_rx) = mpsc::channel();
    for reader in 0..8u64 {
        let (cwd, published, done_tx) = (cwd.clone(), Arc::clone(&published), done_tx.clone());
        thread::spawn(move || {
            let mut stale = Vec::new();
            // Without jitter the readers stay in lockstep — all answered
            // together, all asking again together — so every join lands
            // before the child starts and the hazard is never exercised.
            let mut rng = Lcg(0xD1B5_4A32_D192_ED03 ^ (reader + 1));
            for _ in 0..25 {
                thread::sleep(Duration::from_millis(rng.next() % 26));
                let arrived = published.load(Ordering::SeqCst);
                // Read first, then linger: a caller that joins this child
                // after it started is then provably answered with an
                // epoch older than the one it arrived at.
                let mut cmd = Command::new("/bin/sh");
                cmd.args(["-c", "cat epoch; sleep 0.02"]).current_dir(&cwd);
                let run =
                    super::run_read_shared_with(cmd, "epoch", Duration::from_secs(10), 64, gate)
                        .expect("epoch read");
                let seen: u64 = String::from_utf8_lossy(&run.stdout).trim().parse().unwrap();
                if seen < arrived {
                    stale.push((arrived, seen));
                }
            }
            let _ = done_tx.send(stale);
        });
    }
    drop(done_tx);
    let deadline = Instant::now() + Duration::from_secs(5);
    while gate.counters()[super::Admission::Reactive.index()].coalesced == 0 {
        assert!(
            Instant::now() < deadline,
            "the first wave never shared a child"
        );
        thread::yield_now();
    }
    drop(held);
    let mut stale = Vec::new();
    for _ in 0..8 {
        stale.extend(
            done_rx
                .recv_timeout(Duration::from_secs(60))
                .expect("a reader hung"),
        );
    }
    stop.store(true, Ordering::Relaxed);
    writer.join().unwrap();
    assert!(
        stale.is_empty(),
        "answered from before the caller arrived: {stale:?}"
    );
    // Every call either started a child or joined one; nothing is lost
    // and nothing is counted twice.
    let children = spawn_log::spawns_in(&cwd).len() as u64;
    let joined = gate.counters()[super::Admission::Reactive.index()].coalesced;
    assert_eq!(
        children + joined,
        8 * 25,
        "{children} children + {joined} joins"
    );
    eprintln!("shared-read stress: {children} children served 200 reads ({joined} joined)");
    let leftover = super::shared_reads()
        .lock()
        .unwrap()
        .keys()
        .filter(|key| key.cwd == cwd)
        .count();
    assert_eq!(leftover, 0, "a finished read stayed joinable");
}

/// Identical reads queued behind a full gate share one child. Each child
/// appends its pid to a file, so the count is of processes, not of
/// results.
#[cfg(unix)]
#[test]
fn identical_queued_reads_share_one_child() {
    let gate: &'static SpawnGate = Box::leak(Box::new(SpawnGate::new(1)));
    let dir = tempfile::TempDir::new().unwrap();
    let cwd = dir.path().canonicalize().unwrap();
    let pids = cwd.join("pids");
    let script = format!("echo $$ >> '{}'; echo shared", pids.display());
    let read = {
        let cwd = cwd.clone();
        move || {
            let mut cmd = Command::new("/bin/sh");
            cmd.args(["-c", script.as_str()]).current_dir(&cwd);
            super::run_read_shared_with(cmd, "shared-read", Duration::from_secs(10), 4096, gate)
        }
    };
    let held = gate
        .acquire(Instant::now() + Duration::from_secs(1))
        .unwrap();
    let readers: Vec<_> = (0..3)
        .map(|_| {
            let read = read.clone();
            thread::spawn(read)
        })
        .collect();
    let deadline = Instant::now() + Duration::from_secs(5);
    while gate.counters()[super::Admission::Reactive.index()].coalesced < 2 {
        assert!(
            Instant::now() < deadline,
            "the readers never queued together"
        );
        thread::yield_now();
    }
    drop(held);
    for reader in readers {
        let run = reader.join().unwrap().expect("shared read");
        assert_eq!(String::from_utf8_lossy(&run.stdout).trim(), "shared");
    }
    let started = std::fs::read_to_string(&pids).unwrap();
    assert_eq!(
        started.lines().count(),
        1,
        "three queued reads, one child: {started}"
    );
}

/// A read that arrives once the leader's child is running must start its
/// own: it may be asking because of a write that child cannot have seen.
#[cfg(unix)]
#[test]
fn a_read_arriving_after_the_child_started_does_not_join_it() {
    let gate: &'static SpawnGate = Box::leak(Box::new(SpawnGate::new(4)));
    let dir = tempfile::TempDir::new().unwrap();
    let cwd = dir.path().canonicalize().unwrap();
    let pids = cwd.join("pids");
    let script = format!("echo $$ >> '{}'; sleep 0.4", pids.display());
    let read = {
        let cwd = cwd.clone();
        move || {
            let mut cmd = Command::new("/bin/sh");
            cmd.args(["-c", script.as_str()]).current_dir(&cwd);
            super::run_read_shared_with(cmd, "late-read", Duration::from_secs(10), 4096, gate)
        }
    };
    let first = thread::spawn(read.clone());
    let deadline = Instant::now() + Duration::from_secs(5);
    while std::fs::read_to_string(&pids).map_or(0, |s| s.lines().count()) == 0 {
        assert!(Instant::now() < deadline, "the first child never started");
        thread::yield_now();
    }
    let second = thread::spawn(read);
    first.join().unwrap().expect("first read");
    second.join().unwrap().expect("second read");
    let started = std::fs::read_to_string(&pids).unwrap();
    assert_eq!(
        started.lines().count(),
        2,
        "the late read joined a running child: {started}"
    );
    assert_eq!(
        gate.counters()[super::Admission::Reactive.index()].coalesced,
        0
    );
}

/// The harness for joiner-deadline tests: a gate whose only slot is held,
/// so every read queues and the first caller leads a flight the rest join.
#[cfg(unix)]
struct JoinerRig {
    gate: &'static SpawnGate,
    _dir: tempfile::TempDir,
    cwd: PathBuf,
    held: Vec<super::SpawnPermit<'static>>,
}

#[cfg(unix)]
impl JoinerRig {
    fn new() -> Self {
        Self::with_slots(1)
    }

    /// A gate of `slots` slots, every one held until [`Self::release`].
    fn with_slots(slots: usize) -> Self {
        let gate: &'static SpawnGate = Box::leak(Box::new(SpawnGate::new(slots)));
        let dir = tempfile::TempDir::new().unwrap();
        let cwd = dir.path().canonicalize().unwrap();
        let held = (0..slots)
            .map(|_| {
                gate.acquire(Instant::now() + Duration::from_secs(5))
                    .unwrap()
            })
            .collect();
        Self {
            gate,
            _dir: dir,
            cwd,
            held,
        }
    }

    /// A read of `script` (run by `sh -c`) in the rig's directory.
    fn read(
        &self,
        script: &str,
        timeout: Duration,
    ) -> thread::JoinHandle<(Result<super::BoundedRun, CommandError>, Duration)> {
        self.read_as(script, timeout, false)
    }

    /// [`Self::read`] from a caller that keeps a deadline's prefix, as
    /// `git_run_inner` does for every production read.
    fn read_keeping(
        &self,
        script: &str,
        timeout: Duration,
    ) -> thread::JoinHandle<(Result<super::BoundedRun, CommandError>, Duration)> {
        self.read_as(script, timeout, true)
    }

    fn read_as(
        &self,
        script: &str,
        timeout: Duration,
        keep_prefix: bool,
    ) -> thread::JoinHandle<(Result<super::BoundedRun, CommandError>, Duration)> {
        let (gate, cwd, script) = (self.gate, self.cwd.clone(), script.to_string());
        thread::spawn(move || {
            let _kept = keep_prefix.then(KeepDeadlinePrefix::enter);
            let mut cmd = Command::new("/bin/sh");
            cmd.args(["-c", script.as_str()]).current_dir(&cwd);
            let started = Instant::now();
            let result = super::run_read_shared_with(cmd, "joined-read", timeout, 4096, gate);
            (result, started.elapsed())
        })
    }

    fn flight_open(&self) -> bool {
        super::shared_reads()
            .lock()
            .unwrap()
            .keys()
            .any(|key| key.cwd == self.cwd)
    }

    fn wait_for_flight(&self) {
        let until = Instant::now() + Duration::from_secs(5);
        while !self.flight_open() {
            assert!(Instant::now() < until, "the leader never queued");
            thread::yield_now();
        }
    }

    fn wait_for_joins(&self, joins: u64) {
        let until = Instant::now() + Duration::from_secs(5);
        while self.gate.counters()[super::Admission::Reactive.index()].coalesced < joins {
            assert!(Instant::now() < until, "the joiners never queued");
            thread::yield_now();
        }
    }

    fn release(&mut self) {
        self.held.clear();
    }
}

/// A joiner returns by its own deadline, not the leader's child's, and the
/// leader neither dies nor loses its answer.
#[cfg(unix)]
#[test]
fn a_joiner_returns_by_its_own_deadline_while_the_leader_runs_on() {
    let mut rig = JoinerRig::new();
    let leader = rig.read("sleep 2; echo leader-done", Duration::from_secs(10));
    rig.wait_for_flight();
    let joiner = rig.read("sleep 2; echo leader-done", Duration::from_millis(200));
    rig.wait_for_joins(1);
    rig.release();
    let (joined, waited) = joiner.join().unwrap();
    let error = joined.expect_err("the joiner's 200 ms ran out");
    assert_eq!(error, format!("joined-read{TIMEOUT_MARKER}0.2s"));
    assert!(is_timed_out(error.message()));
    assert!(
        waited < crate::test_support::coverage_relaxed(Duration::from_millis(1200)),
        "the joiner sat for {waited:?}"
    );
    let (led, _) = leader.join().unwrap();
    let led = led.expect("the leader was not disturbed");
    assert!(led.success && led.incomplete.is_none(), "{led:?}");
    assert_eq!(String::from_utf8_lossy(&led.stdout).trim(), "leader-done");
}

/// The leader's deadline is the leader's: a joiner with budget left runs
/// its own read instead of being handed the leader's timeout.
#[cfg(unix)]
#[test]
fn a_joiner_with_budget_left_does_not_inherit_the_leaders_timeout() {
    let mut rig = JoinerRig::new();
    let script = "if [ -e ran ]; then echo second; else touch ran; sleep 5; fi";
    let leader = rig.read(script, Duration::from_millis(300));
    rig.wait_for_flight();
    let joiner = rig.read(script, Duration::from_secs(10));
    rig.wait_for_joins(1);
    rig.release();
    let (led, _) = leader.join().unwrap();
    let error = led.expect_err("the leader's 300 ms ran out");
    assert!(
        is_timed_out(error.message()),
        "{error}",
        error = error.message()
    );
    let (joined, _) = joiner.join().unwrap();
    let run = joined.expect("the joiner had budget to run its own read");
    assert!(run.success && run.incomplete.is_none(), "{run:?}");
    assert_eq!(String::from_utf8_lossy(&run.stdout).trim(), "second");
}

/// The leader's deadline error is a joiner's answer once the joiner's own
/// budget is gone too: it does not loop on a clock that has already run.
#[cfg(unix)]
#[test]
fn a_joiner_whose_budget_also_ran_out_reports_its_own_timeout() {
    let mut rig = JoinerRig::new();
    let leader = rig.read("sleep 5", Duration::from_millis(300));
    rig.wait_for_flight();
    let joiner = rig.read("sleep 5", Duration::from_millis(350));
    rig.wait_for_joins(1);
    rig.release();
    let (led, _) = leader.join().unwrap();
    assert!(is_timed_out(led.expect_err("leader deadline").message()));
    let (joined, waited) = joiner.join().unwrap();
    let error = joined.expect_err("joiner deadline");
    assert!(
        is_timed_out(error.message()),
        "{error}",
        error = error.message()
    );
    assert!(
        waited < crate::test_support::coverage_relaxed(Duration::from_secs(2)),
        "{waited:?}"
    );
}

/// Many joiners with mixed deadlines each leave at their own, and the
/// joiner that outlasts the child shares it: one process in all.
#[cfg(unix)]
#[test]
fn joiners_with_mixed_deadlines_each_leave_at_their_own() {
    let mut rig = JoinerRig::new();
    let script = format!(
        "echo $$ >> '{}/pids'; sleep 1.5; echo shared",
        rig.cwd.display()
    );
    let leader = rig.read(&script, Duration::from_secs(10));
    rig.wait_for_flight();
    let short: Vec<_> = [150u64, 300, 450, 150, 300]
        .iter()
        .map(|ms| (*ms, rig.read(&script, Duration::from_millis(*ms))))
        .collect();
    let long = rig.read(&script, Duration::from_secs(10));
    rig.wait_for_joins(6);
    rig.release();
    for (ms, joiner) in short {
        let (joined, waited) = joiner.join().unwrap();
        let error = joined.expect_err("a short joiner outlasted by the child");
        assert!(
            is_timed_out(error.message()),
            "{error}",
            error = error.message()
        );
        assert!(
            waited
                < Duration::from_millis(ms)
                    + crate::test_support::coverage_relaxed(Duration::from_millis(900)),
            "{ms} ms joiner took {waited:?}"
        );
    }
    for handle in [leader, long] {
        let (run, _) = handle.join().unwrap();
        let run = run.expect("the shared read finished");
        assert_eq!(String::from_utf8_lossy(&run.stdout).trim(), "shared");
    }
    let started = std::fs::read_to_string(rig.cwd.join("pids")).unwrap();
    assert_eq!(started.lines().count(), 1, "one child for all: {started}");
}

/// A joiner still hears a leader that panics: the wait is bounded, but the
/// wake from `Publish` stays.
#[cfg(unix)]
#[test]
fn a_leader_that_panics_still_wakes_a_joiner_before_its_deadline() {
    let rig = JoinerRig::new();
    let (gate, cwd) = (rig.gate, rig.cwd.clone());
    let leader = thread::spawn(move || {
        let mut cmd = Command::new("/bin/sh");
        cmd.args(["-c", "true"]).current_dir(&cwd);
        let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            super::with_forced_spawn_failure_then(
                move || {
                    let until = Instant::now() + Duration::from_secs(5);
                    while gate.counters()[super::Admission::Reactive.index()].coalesced < 1
                        && Instant::now() < until
                    {
                        thread::yield_now();
                    }
                    panic!("the leader's read panicked");
                },
                || {
                    super::run_read_shared_with(
                        cmd,
                        "joined-read",
                        Duration::from_secs(10),
                        4096,
                        gate,
                    )
                },
            )
        }));
    });
    rig.wait_for_flight();
    let joiner = rig.read("true", Duration::from_secs(10));
    let (joined, waited) = joiner.join().unwrap();
    leader.join().unwrap();
    let error = joined.expect_err("a panicked leader has no answer");
    assert!(
        error.contains("ended without a result"),
        "{error}",
        error = error.message()
    );
    assert!(waited < Duration::from_secs(4), "{waited:?}");
}

/// A joiner handed almost no time, as a lock retry passes, answers with
/// its timeout at once instead of waiting on the leader.
#[cfg(unix)]
#[test]
fn a_joiner_with_almost_no_time_left_does_not_wait_for_the_leader() {
    let mut rig = JoinerRig::new();
    let leader = rig.read("sleep 1.5; echo done", Duration::from_secs(10));
    rig.wait_for_flight();
    let joiner = rig.read("sleep 1.5; echo done", Duration::from_millis(1));
    rig.wait_for_joins(1);
    rig.release();
    let (joined, waited) = joiner.join().unwrap();
    assert!(is_timed_out(joined.expect_err("1 ms").message()));
    assert!(
        waited < crate::test_support::coverage_relaxed(Duration::from_millis(800)),
        "{waited:?}"
    );
    assert!(leader.join().unwrap().0.expect("leader").success);
}

/// A joiner whose deadline falls exactly when the leader publishes gets
/// one coherent answer, never a hang and never a torn result.
#[cfg(unix)]
#[test]
fn a_joiner_deadline_racing_the_publish_gets_one_coherent_answer() {
    for round in 0..10u64 {
        let mut rig = JoinerRig::new();
        let leader = rig.read("sleep 0.3; echo raced", Duration::from_secs(10));
        rig.wait_for_flight();
        let joiner = rig.read(
            "sleep 0.3; echo raced",
            Duration::from_millis(300 + round * 3),
        );
        rig.wait_for_joins(1);
        rig.release();
        let (joined, waited) = joiner.join().unwrap();
        match joined {
            Ok(run) => assert_eq!(String::from_utf8_lossy(&run.stdout).trim(), "raced"),
            Err(error) => assert!(
                is_timed_out(error.message()),
                "{error}",
                error = error.message()
            ),
        }
        assert!(waited < Duration::from_secs(3), "{waited:?}");
        assert!(leader.join().unwrap().0.expect("leader").success);
    }
}

/// A caller that keeps deadline prefixes, as every production read does,
/// gets the marked form when its joiner deadline passes. The joiner has no
/// bytes of its own: the child is the leader's.
#[cfg(unix)]
#[test]
fn a_keeping_joiner_that_runs_out_reports_a_marked_empty_run() {
    let mut rig = JoinerRig::new();
    let leader = rig.read_keeping("sleep 2; echo late", Duration::from_secs(10));
    rig.wait_for_flight();
    let joiner = rig.read_keeping("sleep 2; echo late", Duration::from_millis(200));
    rig.wait_for_joins(1);
    rig.release();
    let (joined, _) = joiner.join().unwrap();
    let run = joined.expect("a keeping caller gets the prefix form, not an error");
    assert!(!run.success && run.stdout.is_empty(), "{run:?}");
    let Some(Incomplete::Deadline { message, over_cap }) = &run.incomplete else {
        panic!("expected a deadline marker: {run:?}");
    };
    assert_eq!(message.as_str(), format!("joined-read{TIMEOUT_MARKER}0.2s"));
    assert!(over_cap.is_none());
    assert!(leader.join().unwrap().0.expect("leader").success);
}

/// The leader's prefix is the best answer a keeping joiner can get when
/// its own rerun has too little time left to print anything: a solo call
/// with the joiner's budget would have returned those bytes.
#[cfg(unix)]
#[test]
fn a_rerunning_joiner_keeps_the_leaders_prefix_when_its_own_run_yields_less() {
    let mut rig = JoinerRig::new();
    let script = "if [ -e ran ]; then sleep 5; else touch ran; echo partial; sleep 5; fi";
    let leader = rig.read_keeping(script, Duration::from_millis(400));
    rig.wait_for_flight();
    let joiner = rig.read_keeping(script, Duration::from_millis(900));
    rig.wait_for_joins(1);
    rig.release();
    let (led, _) = leader.join().unwrap();
    let led = led.expect("keeping leader");
    assert_eq!(String::from_utf8_lossy(&led.stdout).trim(), "partial");
    let (joined, waited) = joiner.join().unwrap();
    let run = joined.expect("keeping joiner");
    assert!(
        matches!(run.incomplete, Some(Incomplete::Deadline { .. })),
        "{run:?}"
    );
    assert_eq!(
        String::from_utf8_lossy(&run.stdout).trim(),
        "partial",
        "the leader's prefix was thrown away"
    );
    assert!(
        waited < crate::test_support::coverage_relaxed(Duration::from_millis(2500)),
        "{waited:?}"
    );
}

/// A rerun spends what is left of the caller's own budget, never a fresh
/// one: a leader's timeout does not give its joiner more time in all, and
/// the timeout it reports is still the budget it was given.
#[cfg(unix)]
#[test]
fn a_rerun_never_outlasts_the_joiners_total_budget() {
    let mut rig = JoinerRig::new();
    let leader = rig.read("sleep 5", Duration::from_millis(300));
    rig.wait_for_flight();
    let joiner = rig.read("sleep 5", Duration::from_millis(900));
    rig.wait_for_joins(1);
    rig.release();
    assert!(is_timed_out(
        leader.join().unwrap().0.expect_err("leader").message()
    ));
    let (joined, waited) = joiner.join().unwrap();
    let error = joined.expect_err("joiner deadline");
    assert!(
        is_timed_out(error.message()),
        "{error}",
        error = error.message()
    );
    assert!(
        waited < crate::test_support::coverage_relaxed(Duration::from_millis(1900)),
        "{waited:?}"
    );
    assert_eq!(
        error,
        format!("joined-read{TIMEOUT_MARKER}0.9s"),
        "a joiner's timeout names its own budget, not the remainder"
    );
}

/// Joiners of a leader that timed out all ask again. They ask once, as one
/// flight: a hung git must not meet a stampede of children where it used
/// to meet one.
#[cfg(unix)]
#[test]
fn a_leaders_timeout_sends_its_joiners_to_one_new_child() {
    // Whether the unfixed code stampedes depends on how fast the first
    // rerun reaches the gate, so ask over several rounds.
    for round in 0..4 {
        let mut rig = JoinerRig::with_slots(16);
        let script = format!("echo $$ >> '{}/pids'; sleep 2", rig.cwd.display());
        let leader = rig.read(&script, Duration::from_millis(300));
        rig.wait_for_flight();
        let joiners: Vec<_> = (0..12)
            .map(|_| rig.read(&script, Duration::from_millis(1000)))
            .collect();
        rig.wait_for_joins(12);
        rig.release();
        assert!(is_timed_out(
            leader.join().unwrap().0.expect_err("leader").message()
        ));
        for joiner in joiners {
            let (joined, _) = joiner.join().unwrap();
            assert!(is_timed_out(
                joined.expect_err("every rerun hung").message()
            ));
        }
        let started = std::fs::read_to_string(rig.cwd.join("pids")).unwrap();
        assert_eq!(
            started.lines().count(),
            2,
            "round {round}: the leader's child and one shared retry: {started}"
        );
    }
}

/// `coalesced` counts reads answered without a child of their own. A
/// joiner whose leader timed out and who then ran its own child was not.
#[cfg(unix)]
#[test]
fn a_joiner_that_reruns_is_not_counted_as_coalesced() {
    let mut rig = JoinerRig::new();
    let script = "if [ -e ran ]; then echo second; else touch ran; sleep 5; fi";
    let leader = rig.read(script, Duration::from_millis(300));
    rig.wait_for_flight();
    let joiner = rig.read(script, Duration::from_secs(10));
    rig.wait_for_joins(1);
    rig.release();
    assert!(is_timed_out(
        leader.join().unwrap().0.expect_err("leader").message()
    ));
    assert!(joiner.join().unwrap().0.expect("rerun").success);
    let counters = rig.gate.counters()[super::Admission::Reactive.index()];
    assert_eq!(counters.admitted, 3, "the rig's held slot and two children");
    assert_eq!(
        counters.coalesced, 0,
        "no read was answered by another's child"
    );
}

/// A rate-limit refusal after ~2s used to print the command's own 30s or
/// 90s deadline as "timed out after 89.99s waiting for a process slot",
/// so a launch under load read as git hanging. It must say it was
/// deferred, and how long it actually waited.
#[cfg(unix)]
#[test]
fn a_rate_limit_refusal_reports_the_wait_not_the_command_deadline() {
    let gate: &'static SpawnGate = Box::leak(Box::new(SpawnGate::with_rate(32, 1, 1)));
    let started = Instant::now();
    let (tx, rx) = mpsc::channel();
    for _ in 0..8 {
        let tx = tx.clone();
        thread::spawn(move || {
            let result = run_with_gate(
                &mut Command::new("/usr/bin/true"),
                "slot-wait",
                Duration::from_secs(30),
                None,
                MAX_OUTPUT_BYTES,
                &mut (),
                gate,
            );
            let _ = tx.send(result);
        });
    }
    drop(tx);
    let mut refusals = Vec::new();
    for _ in 0..8 {
        if let Err(error) = rx
            .recv_timeout(Duration::from_secs(10))
            .expect("queue budget")
        {
            refusals.push(error);
        }
    }
    assert!(
        started.elapsed() < crate::test_support::coverage_relaxed(Duration::from_secs(10)),
        "refusals sat until the command deadline: {:?}",
        started.elapsed()
    );
    assert!(
        !refusals.is_empty(),
        "a burst of 1 at 1/s cannot admit all 8 in 2s"
    );
    for error in &refusals {
        let reported: f64 = error
            .message()
            .split(" deferred under load after ")
            .nth(1)
            .and_then(|rest| rest.split('s').next())
            .and_then(|number| number.parse().ok())
            .unwrap_or_else(|| panic!("no deferral and wait in {error}", error = error.message()));
        assert!(
            (1.5..6.0).contains(&reported),
            "reported {reported}s from {error}; the command deadline is 30s",
            error = error.message()
        );
        assert!(
            !error.contains(TIMEOUT_MARKER),
            "a deferral is not a timeout: {error}",
            error = error.message()
        );
    }
}

#[cfg(unix)]
#[test]
fn a_queued_command_still_gets_its_full_runtime_after_a_slot() {
    // The queue wait is bounded by `timeout`, but spending that budget
    // in the gate used to leave the child with leftover milliseconds and
    // report "timed out" for work that had not started. Hold the only
    // slot long enough that the leftover would be less than `sleep`,
    // then prove the child still finishes.
    // The deadline is scaled so a loaded host can still start `sh` and
    // finish the 0.4s sleep inside it; the hold scales with it, leaving
    // 250ms of the deadline after the slot frees, so the old behaviour
    // would still time out before `sleep 0.4` ends.
    let deadline = crate::test_support::coverage_relaxed(Duration::from_millis(600));
    let hold = deadline.saturating_sub(Duration::from_millis(250));
    let gate: &'static SpawnGate = Box::leak(Box::new(SpawnGate::new(1)));
    let holder = gate
        .acquire(Instant::now() + Duration::from_secs(10))
        .expect("holder");
    let (tx, rx) = mpsc::channel();
    let worker = thread::spawn(move || {
        let mut cmd = Command::new("sh");
        cmd.args(["-c", "sleep 0.4; echo queued-ok"]);
        let result = run_with_gate(
            &mut cmd,
            "queued-run",
            deadline,
            None,
            MAX_OUTPUT_BYTES,
            &mut (),
            gate,
        );
        tx.send(result).unwrap();
    });
    thread::sleep(hold);
    drop(holder);
    let result = rx
        .recv_timeout(deadline + crate::test_support::coverage_relaxed(Duration::from_secs(5)))
        .expect("queued command must start after the slot is freed")
        .expect("a command that waited in the gate must still get its full runtime");
    worker.join().unwrap();
    assert!(
        result.success,
        "stderr: {}",
        String::from_utf8_lossy(&result.stderr)
    );
    assert!(
        String::from_utf8_lossy(&result.stdout).contains("queued-ok"),
        "stdout: {}",
        String::from_utf8_lossy(&result.stdout)
    );
}

#[cfg(unix)]
#[test]
fn inherited_stdin_cannot_hold_the_command_runner_after_the_parent_exits() {
    let (tx, rx) = mpsc::channel();
    let worker = thread::spawn(move || {
        let mut cmd = Command::new("/bin/sh");
        cmd.args(["-c", "sleep 4 <&0 >/dev/null 2>&1 & exit 0"]);
        let result = run_with_gate(
            &mut cmd,
            "stdin-test",
            Duration::from_secs(10),
            Some(&vec![b'x'; 1024 * 1024]),
            1024,
            &mut (),
            Box::leak(Box::new(SpawnGate::new(1))),
        );
        tx.send(result).unwrap();
    });
    let result = rx.recv_timeout(Duration::from_secs(3));
    worker.join().unwrap();
    let error = result
        .expect("stdin must have a bounded settle window")
        .expect_err("undelivered stdin cannot report success");
    assert!(error.contains("stdin"), "{error}", error = error.message());
}

#[cfg(unix)]
#[test]
fn cancellable_stdin_delivers_large_payloads_while_stdout_drains() {
    let bytes = vec![b'q'; 8 * 1024 * 1024];
    let result = run_with_gate(
        &mut Command::new("/bin/cat"),
        "stdin-roundtrip",
        Duration::from_secs(10),
        Some(&bytes),
        bytes.len(),
        &mut (),
        Box::leak(Box::new(SpawnGate::new(1))),
    )
    .unwrap();
    assert!(result.success);
    assert!(result.incomplete.is_none());
    assert_eq!(result.stdout, bytes);
}

/// A permit released by a panicking holder must not leak its slot, or one
/// failed git call would shrink the budget for the rest of the session.
#[test]
fn spawn_gate_reclaims_a_permit_dropped_by_a_panic() {
    let gate = std::sync::Arc::new(SpawnGate::new(1));
    let poisoner = {
        let gate = std::sync::Arc::clone(&gate);
        thread::spawn(move || {
            let _permit = gate
                .acquire(Instant::now() + Duration::from_secs(30))
                .unwrap();
            panic!("holder blew up mid-run");
        })
    };
    assert!(poisoner.join().is_err(), "the holder was supposed to panic");

    let (tx, rx) = mpsc::channel();
    let gate2 = std::sync::Arc::clone(&gate);
    thread::spawn(move || {
        let _permit = gate2
            .acquire(Instant::now() + Duration::from_secs(30))
            .unwrap();
        let _ = tx.send(());
    });
    rx.recv_timeout(Duration::from_secs(5))
        .expect("the panicked holder's slot must come back");
}

/// Regression: nothing bounded how many children `run_bounded` kept alive
/// at once, so a workspace-wide refresh put hundreds of `git` processes on
/// the host simultaneously and exhausted the descriptor table -- every
/// spawn past the ceiling failing with "Too many open files (os error
/// 24)".
///
/// Concurrency is counted by the children themselves rather than by the
/// gate: each `sh` creates a marker file for as long as it runs, and a
/// sampler watches how many exist at once. An assertion that read the
/// gate's own counter could not tell a working gate from one that is
/// never consulted, which is exactly the bug this pins.
///
/// The gate is private. 64 waiters on the process-wide `spawn_gate()`
/// starve every other short-deadline `run_bounded` test in the same
/// binary — llvm-cov still shares one gate even with a thread cap, and a
/// 5s slot wait then fails as "timed out waiting for a process slot"
/// even though the drain under test is fine. Production `run_observed`
/// always passes `spawn_gate()`; this test pins that `run_with_gate`
/// actually limits live children.
#[cfg(unix)]
#[test]
fn run_bounded_never_exceeds_the_spawn_limit_under_fan_out() {
    const CALLERS: usize = 64;
    let limit = spawn_limit();
    assert!(
        CALLERS > limit,
        "the fan-out must exceed the limit to prove anything ({CALLERS} vs {limit})"
    );
    let gate: &'static SpawnGate = Box::leak(Box::new(SpawnGate::new(limit)));

    let dir = tempfile::TempDir::new().expect("tempdir");
    let live_dir = dir.path().to_path_buf();
    let sampling = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(true));
    let peak_live = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let sampler = {
        let live_dir = live_dir.clone();
        let sampling = std::sync::Arc::clone(&sampling);
        let peak_live = std::sync::Arc::clone(&peak_live);
        thread::spawn(move || {
            while sampling.load(std::sync::atomic::Ordering::SeqCst) {
                if let Ok(entries) = std::fs::read_dir(&live_dir) {
                    let n = entries.count();
                    peak_live.fetch_max(n, std::sync::atomic::Ordering::SeqCst);
                }
                thread::sleep(Duration::from_millis(5));
            }
        })
    };

    let ready = std::sync::Arc::new(std::sync::Barrier::new(CALLERS));
    let handles: Vec<_> = (0..CALLERS)
        .map(|i| {
            let ready = std::sync::Arc::clone(&ready);
            let marker = live_dir.join(format!("child-{i}"));
            thread::spawn(move || {
                // Release every caller at once so they contend for real.
                ready.wait();
                let marker = marker.to_string_lossy().into_owned();
                let mut cmd = Command::new("sh");
                cmd.args([
                    "-c",
                    // Present for exactly as long as this child runs.
                    r#": > "$1"; sleep 0.3; rm -f "$1""#,
                    "sh",
                    &marker,
                ]);
                run_with_gate(
                    &mut cmd,
                    "sh",
                    Duration::from_secs(30),
                    None,
                    MAX_OUTPUT_BYTES,
                    &mut (),
                    gate,
                )
            })
        })
        .collect();

    for handle in handles {
        let out = handle.join().expect("caller thread").expect("run");
        assert!(out.success, "every bounded run must still complete");
    }
    sampling.store(false, std::sync::atomic::Ordering::SeqCst);
    sampler.join().expect("sampler thread");

    let observed = peak_live.load(std::sync::atomic::Ordering::SeqCst);
    assert!(
        observed > 1,
        "the callers must genuinely have overlapped, saw {observed} at once"
    );
    assert!(
        observed <= limit,
        "at most {limit} children may be alive at once, saw {observed}"
    );
}

#[test]
fn a_scrubbed_child_keeps_only_allowlisted_inherited_variables() {
    use std::ffi::OsString;
    let inherited = [
        ("PATH", "/usr/bin"),
        ("LC_ALL", "C"),
        ("HTTPS_PROXY", "http://p:1"),
        ("NPM_TOKEN", "t1"),
        ("GITHUB_TOKEN", "t2"),
        ("AWS_SESSION_TOKEN", "t3"),
        ("MY_API_KEY", "t4"),
    ]
    .map(|(k, v)| (OsString::from(k), OsString::from(v)));
    let mut cmd = Command::new("true");
    cmd.env("EXPLICIT", "yes").env_remove("PATH");
    restrict_env_to_allowlist_plus(
        &mut cmd,
        inherited,
        Some(std::ffi::OsStr::new("/h")),
        &["MY_API_KEY".to_string()],
    );
    let seen: std::collections::BTreeMap<String, Option<String>> = cmd
        .get_envs()
        .map(|(k, v)| {
            (
                k.to_string_lossy().into_owned(),
                v.map(|v| v.to_string_lossy().into_owned()),
            )
        })
        .collect();
    let names: Vec<&str> = seen.keys().map(String::as_str).collect();
    assert_eq!(
        names,
        ["EXPLICIT", "HOME", "HTTPS_PROXY", "LC_ALL", "MY_API_KEY"],
        "{seen:?}"
    );
    // A PATH the builder removed is not resurrected from the inherited one.
    assert!(!seen.contains_key("PATH"));
    assert_eq!(seen["HOME"].as_deref(), Some("/h"));
}

#[test]
#[cfg(unix)]
fn audit_full_duplex_stress_preserves_bytes_and_deadline() {
    let payload = vec![b'x'; 2 * 1024 * 1024];
    for _ in 0..8 {
        let mut cmd = Command::new("sh");
        cmd.args(["-c", "head -c 2000000 /dev/zero >&2 & cat; wait"]);
        let out = run_bounded(cmd, "duplex", Duration::from_secs(10), Some(&payload)).unwrap();
        assert!(out.success);
        assert!(out.incomplete.is_none() && out.stderr_incomplete.is_none());
        assert_eq!(out.stdout, payload);
        assert_eq!(out.stderr, vec![0; 2_000_000]);
    }
    let mut cmd = Command::new("sh");
    cmd.args(["-c", "while :; do head -c 65536 /dev/zero; done"]);
    let started = Instant::now();
    assert!(
        run_bounded_capped(cmd, "hot-output", Duration::from_millis(100), None, 1024)
            .unwrap_err()
            .contains("timed out")
    );
    assert!(started.elapsed() < crate::test_support::coverage_relaxed(Duration::from_secs(2)));
}

/// A child that floods stdout and, once asked to stop, writes a large
/// cleanup burst before releasing its marker. The marker is only removed
/// if the run keeps reading its pipes while the child is being asked.
#[cfg(unix)]
const GRACE_FLOODER: &str = r#"
        trap 'head -c 80000 /dev/zero; echo cleaned >&2; rm -f "$1"; exit 0' TERM
        touch "$1"
        while :; do echo xxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxx; done
    "#;

#[cfg(unix)]
fn grace_flooder(script: &str, marker: &Path) -> Command {
    let mut cmd = Command::new("sh");
    cmd.arg("-c").arg(script).arg("sh").arg(marker);
    cmd
}

/// R2-02: the grace after a deadline reads the pipes, so a child blocked
/// on a full pipe can run its SIGTERM cleanup instead of being killed.
#[cfg(unix)]
#[test]
fn a_deadline_grace_keeps_draining_so_cleanup_after_sigterm_completes() {
    let _kept = KeepDeadlinePrefix::enter();
    let dir = tempfile::tempdir().unwrap();
    let marker = dir.path().join("lock");
    let run = run_bounded(
        grace_flooder(GRACE_FLOODER, &marker),
        "flooder",
        // Scaled: SIGTERM before a starved `sh` has installed its trap
        // kills it outright, so there is no cleanup for the grace to drain.
        crate::test_support::coverage_relaxed(Duration::from_millis(600)),
        None,
    )
    .expect("a deadline keeps its prefix");
    // A deadline discards the exit status, so marker removal is the signal
    // that the child was reaped by the ask: only its SIGTERM trap removes it.
    assert!(
        !marker.exists(),
        "SIGTERM cleanup never ran: the child was wedged on a full pipe until SIGKILL"
    );
    assert!(
        String::from_utf8_lossy(&run.stderr).contains("cleaned"),
        "the cleanup line was not read: {}",
        String::from_utf8_lossy(&run.stderr)
    );
    assert!(!run.success, "a deadline is never success");
    assert!(!run.cancelled);
    assert!(
        matches!(run.incomplete, Some(Incomplete::Deadline { .. })),
        "{:?}",
        run.incomplete
    );
}

/// R2-02, cancel path: the same shape, reached through the observer.
#[cfg(unix)]
#[test]
fn a_cancel_grace_keeps_draining_so_cleanup_after_sigterm_completes() {
    struct CancelOnceStarted(PathBuf);
    impl ProcessObserver for CancelOnceStarted {
        fn cancelled(&self) -> bool {
            self.0.exists()
        }
    }
    let dir = tempfile::tempdir().unwrap();
    let marker = dir.path().join("lock");
    let mut observer = CancelOnceStarted(marker.clone());
    let mut cmd = grace_flooder(GRACE_FLOODER, &marker);
    let run = run_observed(
        &mut cmd,
        "flooder",
        Duration::from_secs(20),
        None,
        MAX_OUTPUT_BYTES,
        &mut observer,
    )
    .expect("a cancel returns the run");
    assert!(run.cancelled);
    assert!(!run.success);
    // Reaped by the ask: the trap exits 0, a SIGKILLed child reports -1.
    assert_eq!(run.status_code, 0, "the child was killed, not asked");
    assert!(
        !marker.exists(),
        "SIGTERM cleanup never ran: the child was wedged on a full pipe until SIGKILL"
    );
    assert!(
        String::from_utf8_lossy(&run.stderr).contains("cleaned"),
        "the cleanup line was not read: {}",
        String::from_utf8_lossy(&run.stderr)
    );
}

/// R2-02: a child that ignores the ask is still killed once the grace is
/// spent, however much it writes meanwhile, and what it wrote while being
/// asked is read. The ask is observed (the trap runs) only because the
/// pipes are drained.
#[cfg(unix)]
#[test]
fn a_child_that_ignores_sigterm_is_killed_at_the_end_of_a_drained_grace() {
    let _kept = KeepDeadlinePrefix::enter();
    let dir = tempfile::tempdir().unwrap();
    let marker = dir.path().join("lock");
    let script = r#"
            trap 'head -c 80000 /dev/zero; echo term-seen >&2' TERM
            touch "$1"
            while :; do echo xxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxx; done
        "#;
    // Scaled for the same reason as the drained-grace test: the trap has
    // to be installed before SIGTERM for there to be anything to observe.
    let deadline = crate::test_support::coverage_relaxed(Duration::from_millis(300));
    let started = Instant::now();
    let run = run_bounded(grace_flooder(script, &marker), "stubborn", deadline, None)
        .expect("a deadline keeps its prefix");
    assert!(
        started.elapsed()
            < deadline + crate::test_support::coverage_relaxed(Duration::from_secs(2)),
        "the grace must stay bounded: {:?}",
        started.elapsed()
    );
    assert!(matches!(run.incomplete, Some(Incomplete::Deadline { .. })));
    assert!(!run.success);
    assert!(
        String::from_utf8_lossy(&run.stderr).contains("term-seen"),
        "the trap could not run while the pipe was full: {}",
        String::from_utf8_lossy(&run.stderr)
    );
}

#[test]
#[cfg(unix)]
fn audit_observer_cancellation_preserves_progress_and_kills_child() {
    struct CancelOnOutput(Vec<u8>);
    impl ProcessObserver for CancelOnOutput {
        fn cancelled(&self) -> bool {
            !self.0.is_empty()
        }
        fn output(&mut self, _: OutputStream, bytes: &[u8]) {
            self.0.extend_from_slice(bytes);
        }
    }
    let mut observer = CancelOnOutput(Vec::new());
    let mut cmd = Command::new("sh");
    cmd.args(["-c", "printf progress; sleep 30"]);
    let started = Instant::now();
    let out = run_observed(
        &mut cmd,
        "cancel",
        Duration::from_secs(10),
        None,
        1024,
        &mut observer,
    )
    .unwrap();
    assert!(out.cancelled && !out.success);
    assert_eq!(out.stdout, b"progress");
    assert_eq!(observer.0, b"progress");
    assert!(started.elapsed() < crate::test_support::coverage_relaxed(Duration::from_secs(2)));
}

// ------------------------------------------------------------------
// Adversarial batch-stream cases (CARD 16).
// ------------------------------------------------------------------

/// The cross-process budget, the command scope and the post-action credit.
#[cfg(test)]
mod admission_scope_tests {
    use super::*;

    #[cfg(unix)]
    use super::hold_credit_open;
    use super::{frozen_gate as frozen, leak_gate as leak};

    fn shared_gate(path: &Path, burst: u32, rate: u32) -> &'static SpawnGate {
        let gate = SpawnGate::with_rate(16, burst, rate).sharing(Ok(path.to_path_buf()));
        assert!(gate.shared.is_some(), "{:?}", gate.shared_note);
        leak(gate)
    }

    fn admits(gate: &SpawnGate) -> bool {
        gate.acquire(Instant::now() + Duration::from_millis(20))
            .is_some()
    }

    #[test]
    fn two_processes_sharing_one_budget_obey_one_combined_burst() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("budget");
        // Two gates opening one record are two open-file descriptions, which
        // is what two processes hold.
        let a = shared_gate(&path, 6, 1);
        let b = shared_gate(&path, 6, 1);
        let started = Instant::now();
        let admitted = (0..20)
            .filter(|i| admits(if i % 2 == 0 { a } else { b }))
            .count();
        // One refill token per elapsed second may land meanwhile.
        let refill = started.elapsed().as_secs() as usize + 1;
        assert!(
            (6..=6 + refill).contains(&admitted),
            "admitted {admitted}: one burst between both, not one each"
        );
    }

    #[test]
    fn background_is_shed_at_the_shared_reserve_whoever_spent_it() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("budget");
        let a = shared_gate(&path, 8, 1);
        let b = shared_gate(&path, 8, 1);
        for _ in 0..6 {
            assert!(admits(a));
        }
        // B has spent nothing itself, but together the two are at the
        // reserve (8 / 4 = 2), so B's background work is shed at once.
        let shed = with_admission(Admission::Background, || {
            b.acquire_until(Instant::now() + Duration::from_secs(5), &|| false)
                .map(|_| ())
        });
        assert_eq!(shed.err(), Some(Refusal::Shed));
        assert!(admits(b), "a reactive read still has the reserve");
    }

    /// B has every process-bucket token, and the shared record is spent. When
    /// B cannot rewrite the record the denial is still a denial: no
    /// non-interactive spawn starts and it is not counted as a fallback.
    /// A user action never asks the record, so it still starts.
    #[test]
    fn a_denial_the_record_cannot_persist_does_not_admit_a_background_spawn() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("budget");
        let a = shared_gate(&path, 8, 1);
        let b = shared_gate(&path, 8, 1);
        while admits(a) {}
        b.shared.as_ref().unwrap().fail_writes(true);
        assert!(
            !admits(b),
            "a reactive spawn rode a failed write past the budget"
        );
        let shed = with_admission(Admission::Background, || {
            b.acquire_until(Instant::now() + Duration::from_millis(50), &|| false)
                .map(|_| ())
        });
        assert_eq!(shed.err(), Some(Refusal::Shed));
        assert_eq!(b.state.lock().unwrap().shared_fallbacks, 0);
        // The report counts those denials on their own line, apart from the
        // ordinary denial A met when it spent the record and from fallbacks.
        let (fallbacks, unrecorded) = {
            let state = b.state.lock().unwrap();
            (state.shared_fallbacks, state.shared_unrecorded_denials)
        };
        assert!(unrecorded >= 2, "the reactive and the background denial");
        let report = b.sharing_report(fallbacks, unrecorded);
        assert!(
            report.ends_with(&format!("unrecorded_denials={unrecorded} fallbacks=0")),
            "{report}"
        );
        let ordinary = {
            let state = a.state.lock().unwrap();
            (state.shared_fallbacks, state.shared_unrecorded_denials)
        };
        assert_eq!(ordinary, (0, 0), "a recorded denial is neither");
        let clicked = with_admission(Admission::Interactive, || {
            b.acquire_until(Instant::now() + Duration::from_millis(50), &|| false)
                .map(|_| ())
        });
        assert_eq!(clicked, Ok(()), "a user action does not ask the record");
    }

    /// Spawns `gate` admits in `class` before it refuses, its own bucket
    /// frozen so only the budgets decide.
    fn drain(gate: &SpawnGate, class: Admission) -> usize {
        // The deadline is checked before the attempt. Background runs at
        // utility QoS, and a stall longer than the deadline is counted as an
        // empty bucket. An empty background bucket returns `Refusal::Shed`
        // without waiting, so the longer cap does not add grants. Reactive
        // stays on the short deadline: it waits out an empty bucket, and a
        // long one would pad every measurement with that wait.
        let budget = if class == Admission::Background {
            Duration::from_secs(1)
        } else {
            Duration::from_millis(20)
        };
        let mut admitted = 0;
        while with_admission(class, || gate.acquire(Instant::now() + budget).is_some()) {
            admitted += 1;
        }
        admitted
    }

    /// CHARACTERIZATION, awaiting the owner's ruling (ft-d41b8491, part 2): a
    /// grant whose record write fails is the documented fail-open, so when
    /// writes fail on every take the record never decrements and the shared
    /// cap is gone: each of N processes is granted its own whole bucket. This
    /// pins the aggregate as it is, with the healthy record beside it, so a
    /// ruling that bounds it (a fixed floor after repeated write failures)
    /// has to change this test on purpose.
    #[test]
    fn writes_failing_on_every_take_grant_each_process_its_own_bucket() {
        let (burst, rate) = production_spawn_rate();
        let reserve = (burst / 4) as usize;
        let burst = burst as usize;
        let mut rows = Vec::new();
        for processes in [1usize, 2, 4, 8] {
            let mut aggregate = [0usize; 4];
            for (slot, (writes_fail, class)) in [
                (false, Admission::Reactive),
                (true, Admission::Reactive),
                (false, Admission::Background),
                (true, Admission::Background),
            ]
            .into_iter()
            .enumerate()
            {
                let dir = tempfile::tempdir().unwrap();
                let path = dir.path().join("budget");
                let gates: Vec<_> = (0..processes)
                    .map(|_| {
                        let gate =
                            SpawnGate::with_rate(16, burst as u32, rate).sharing(Ok(path.clone()));
                        let gate = frozen(gate);
                        gate.shared.as_ref().unwrap().fail_writes(writes_fail);
                        gate
                    })
                    .collect();
                let started = Instant::now();
                aggregate[slot] = gates.iter().map(|gate| drain(gate, class)).sum();
                // The shared record refills in real time at `rate`.
                let refill =
                    (started.elapsed().as_secs_f64() * f64::from(rate)).ceil() as usize + 1;
                let (own, shared) = match class {
                    Admission::Background => (burst - reserve, burst - reserve),
                    _ => (burst, burst),
                };
                if writes_fail {
                    assert_eq!(
                        aggregate[slot],
                        processes * own,
                        "{processes} processes, {class:?}: each its own bucket"
                    );
                } else {
                    assert!(
                        (shared..=shared + refill).contains(&aggregate[slot]),
                        "{processes} processes, {class:?}: {} for one shared bucket",
                        aggregate[slot]
                    );
                }
            }
            rows.push(format!(
                "N={processes}: reactive healthy {} / writes failing {}; background healthy {} / writes failing {}",
                aggregate[0], aggregate[1], aggregate[2], aggregate[3]
            ));
        }
        println!("{}", rows.join("\n"));
    }

    /// The two counters are separate, add exactly under concurrent deciders of
    /// one gate, and saturate rather than wrap; the reason (which names the
    /// path) is only logged, never part of the report's counts.
    #[test]
    fn unrecorded_denials_and_fallbacks_count_apart_exactly_and_saturate() {
        let dir = tempfile::tempdir().unwrap();
        let gate = shared_gate(&dir.path().join("budget"), 8, 1);
        std::thread::scope(|scope| {
            for _ in 0..2 {
                scope.spawn(|| {
                    for i in 0..500 {
                        let taken = if i % 5 == 0 {
                            shared_budget::Take::Unavailable("held".into())
                        } else {
                            shared_budget::Take::DeniedUnrecorded("full".into())
                        };
                        let mut state = gate.state.lock().unwrap();
                        assert_eq!(gate.settle_shared(&mut state, taken), i % 5 == 0);
                    }
                });
            }
        });
        let (fallbacks, unrecorded) = {
            let state = gate.state.lock().unwrap();
            (state.shared_fallbacks, state.shared_unrecorded_denials)
        };
        assert_eq!((fallbacks, unrecorded), (200, 800));

        let mut state = gate.state.lock().unwrap();
        state.shared_unrecorded_denials = u64::MAX;
        state.shared_fallbacks = u64::MAX;
        assert!(!gate.settle_shared(
            &mut state,
            shared_budget::Take::DeniedUnrecorded("x".into())
        ));
        assert!(gate.settle_shared(&mut state, shared_budget::Take::Unavailable("x".into())));
        assert_eq!(
            (state.shared_fallbacks, state.shared_unrecorded_denials),
            (u64::MAX, u64::MAX)
        );
        let report = gate.sharing_report(state.shared_fallbacks, state.shared_unrecorded_denials);
        assert!(
            report.ends_with(&format!(
                "unrecorded_denials={} fallbacks={}",
                u64::MAX,
                u64::MAX
            )),
            "{report}"
        );
    }

    #[test]
    fn a_user_action_neither_waits_for_nor_spends_the_shared_budget() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("budget");
        let a = shared_gate(&path, 3, 1);
        let b = shared_gate(&path, 3, 1);
        while admits(a) {}
        let clicked = with_admission(Admission::Interactive, || {
            b.acquire_until(Instant::now() + Duration::from_millis(50), &|| false)
                .map(|_| ())
        });
        assert_eq!(clicked, Ok(()));
        let shared = b.shared.as_ref().unwrap();
        assert_eq!(
            shared.take(0, 3, 1),
            shared_budget::Take::Denied,
            "the click took no shared token"
        );
    }

    /// The shared record's file lock can sleep-poll for a few milliseconds and
    /// a denial writes the file. With the gate mutex held across that, every
    /// other acquire, a user's click included, queued behind a background
    /// re-poll.
    #[test]
    fn a_user_action_does_not_queue_behind_the_shared_budget_io() {
        use std::sync::atomic::{AtomicBool, Ordering};
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("budget");
        let gate = shared_gate(&path, 100_000, 100_000);
        // Another process pinning the record: every shared take now waits out
        // the whole lock patience before falling back.
        let holder = std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .open(&path)
            .unwrap();
        holder.lock().unwrap();
        let stop = std::sync::Arc::new(AtomicBool::new(false));
        let pollers: Vec<_> = (0..3)
            .map(|_| {
                let stop = std::sync::Arc::clone(&stop);
                std::thread::spawn(move || {
                    while !stop.load(Ordering::Relaxed) {
                        let _ = gate.acquire(Instant::now() + Duration::from_millis(200));
                    }
                })
            })
            .collect();
        std::thread::sleep(Duration::from_millis(30));
        let mut fastest = Duration::MAX;
        for _ in 0..20 {
            let started = Instant::now();
            let clicked = with_admission(Admission::Interactive, || {
                gate.acquire_until(Instant::now() + Duration::from_millis(500), &|| false)
                    .map(|_| ())
            });
            fastest = fastest.min(started.elapsed());
            assert_eq!(clicked, Ok(()));
            std::thread::sleep(Duration::from_millis(3));
        }
        stop.store(true, Ordering::Relaxed);
        for poller in pollers {
            poller.join().unwrap();
        }
        holder.unlock().unwrap();
        assert!(
            fastest < Duration::from_millis(5),
            "every user action waited behind shared-budget IO: fastest was {fastest:?}"
        );
    }

    #[test]
    fn an_unusable_shared_record_leaves_the_gate_on_its_own_budget_and_says_so() {
        let dir = tempfile::tempdir().unwrap();
        // A directory where the record should be.
        let gate = leak(SpawnGate::with_rate(16, 4, 1).sharing(Ok(dir.path().to_path_buf())));
        assert!(gate.shared.is_none());
        let report = gate.sharing_report(0, 0);
        assert!(report.starts_with("budget=per-process ("), "{report}");
        let admitted = (0..8).filter(|_| admits(gate)).count();
        assert_eq!(admitted, 4, "the process bucket still holds");

        let unpathed = SpawnGate::with_rate(16, 4, 1).sharing(Err("no home".into()));
        assert_eq!(
            unpathed.sharing_report(0, 0),
            "budget=per-process (no home)"
        );
        // The unlimited test gate has nothing to share, and says that.
        let unlimited = SpawnGate::new(4).sharing(Ok(dir.path().join("x")));
        assert!(unlimited.shared.is_none());
        assert!(!dir.path().join("x").exists(), "no record created for it");
    }

    #[test]
    fn a_held_shared_lock_falls_back_to_the_process_budget_and_is_counted() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("budget");
        let gate = shared_gate(&path, 4, 1);
        let holder = std::fs::File::options()
            .read(true)
            .write(true)
            .open(&path)
            .unwrap();
        holder.lock().unwrap();
        assert!(admits(gate), "a stopped holder must not stop this process");
        let fallbacks = gate.state.lock().unwrap().shared_fallbacks;
        assert_eq!(fallbacks, 1);
        assert!(gate.sharing_report(fallbacks, 0).ends_with("fallbacks=1"));
        holder.unlock().unwrap();
    }

    #[test]
    fn the_report_names_where_the_budget_lives() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("budget");
        let gate = shared_gate(&path, 4, 1);
        let report = gate.sharing_report(0, 0);
        assert!(report.starts_with("budget=shared("), "{report}");
        assert!(report.contains(&path.display().to_string()), "{report}");
    }

    fn deferral() -> CommandError {
        refusal_message(
            "git status",
            Refusal::Refused {
                waited: Duration::from_millis(2_013),
            },
        )
    }

    #[test]
    fn a_read_commands_deferral_stays_retryable() {
        let out: Result<(), CommandError> = run_command_scope(|| Err(deferral()));
        assert!(is_deferred_under_load(out.unwrap_err().message()));
    }

    #[test]
    fn a_user_actions_deferral_is_never_handed_out_as_retryable() {
        let dir = tempfile::tempdir().unwrap();
        let repo = dir.path().to_str().unwrap().to_string();
        let out: Result<(), CommandError> = run_command_scope(|| {
            mark_user_action(&repo);
            Err(deferral())
        });
        let message = out.unwrap_err();
        assert!(
            !is_deferred_under_load(message.message()),
            "{message}",
            message = message.message()
        );
        assert!(
            message.contains("deferred under load"),
            "the cause stays: {message}",
            message = message.message()
        );
        assert!(
            message.contains("2.013"),
            "{message}",
            message = message.message()
        );
        // Other failures pass through untouched.
        let plain: Result<(), CommandError> = run_command_scope(|| {
            mark_user_action(&repo);
            Err("fatal: bad revision".into())
        });
        assert_eq!(plain.unwrap_err(), "fatal: bad revision");
    }

    #[test]
    fn a_command_scope_restores_the_class_and_repo_even_when_it_panics() {
        let dir = tempfile::tempdir().unwrap();
        let repo = dir.path().to_str().unwrap().to_string();
        assert_eq!(current_admission(), Admission::Reactive);
        let caught = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let _: Result<(), CommandError> = run_command_scope(|| {
                mark_user_action(&repo);
                panic!("expected");
            });
        }));
        assert!(caught.is_err());
        assert_eq!(current_admission(), Admission::Reactive);
        assert!(marked_repo_is_none());
        // A panicking action grants nothing.
        let canonical = dir.path().canonicalize().unwrap();
        assert!(!take_post_action_credit(&canonical, Instant::now()));
    }

    #[cfg(unix)]
    fn reactive_spawn_in(gate: &'static SpawnGate, cwd: &Path) -> Result<BoundedRun, CommandError> {
        let mut cmd = Command::new("/bin/sh");
        cmd.args(["-c", "true"]).current_dir(cwd);
        run_with_gate(
            &mut cmd,
            "credit-probe",
            Duration::from_millis(200),
            None,
            1024,
            &mut (),
            gate,
        )
    }

    #[cfg(unix)]
    #[test]
    fn the_refresh_after_a_user_action_is_admitted_within_its_credit_and_no_further() {
        let dir = tempfile::tempdir().unwrap();
        let repo = dir.path().canonicalize().unwrap();
        let other = tempfile::tempdir().unwrap();
        let other = other.path().canonicalize().unwrap();
        // One token, and no refill at all inside this test.
        let gate = frozen(SpawnGate::with_rate(16, 1, 1));
        reactive_spawn_in(gate, &repo).expect("the one token");
        let starved = reactive_spawn_in(gate, &repo).unwrap_err();
        assert!(
            is_deferred_under_load(starved.message()) || is_slot_wait_timeout(starved.message()),
            "{starved}",
            starved = starved.message()
        );

        let acted: Result<(), CommandError> = run_command_scope(|| {
            mark_user_action(repo.to_str().unwrap());
            Ok(())
        });
        acted.unwrap();
        // The credit is per repository, and a read command never earns one.
        let read: Result<(), CommandError> = run_command_scope(|| Ok(()));
        read.unwrap();
        assert!(reactive_spawn_in(gate, &other).is_err());
        hold_credit_open(&repo);
        for i in 0..POST_ACTION_CREDIT_SPAWNS {
            reactive_spawn_in(gate, &repo)
                .unwrap_or_else(|e| panic!("credited spawn {i} refused: {e}", e = e.message()));
        }
        // Spent, and the bucket cannot refill: nothing more is admitted.
        let extra = (0..6)
            .take_while(|_| reactive_spawn_in(gate, &repo).is_ok())
            .count();
        assert_eq!(
            extra, 0,
            "{extra} spawns past the {POST_ACTION_CREDIT_SPAWNS}-spawn credit"
        );
    }

    /// What the two credit tests rely on: a frozen bucket takes no token from
    /// the passing of time, however long a loaded machine makes a test, while
    /// an ordinary one does.
    #[test]
    fn a_frozen_bucket_does_not_refill_and_an_ordinary_one_does() {
        let later = Instant::now() + Duration::from_secs(30);
        for (gate, refilled) in [
            (frozen(SpawnGate::with_rate(16, 4, 1)), false),
            (leak(SpawnGate::with_rate(16, 4, 1)), true),
        ] {
            let mut state = gate.state.lock().unwrap();
            state.tokens = 0;
            if refilled {
                // An ordinary gate starts its clock at its first look.
                state.tokens_at = Some(Instant::now());
            }
            gate.refill(&mut state, later);
            assert_eq!(state.tokens > 0, refilled, "tokens {}", state.tokens);
        }
    }

    #[test]
    fn a_credit_expires_with_its_window() {
        let dir = tempfile::tempdir().unwrap();
        let repo = dir.path().canonicalize().unwrap();
        let at = Instant::now();
        grant_post_action_credit(&repo, at);
        assert!(take_post_action_credit(
            &repo,
            at + Duration::from_millis(10)
        ));
        assert!(!take_post_action_credit(
            &repo,
            at + POST_ACTION_CREDIT_WINDOW + Duration::from_millis(1)
        ));
        // Expired entries are dropped, so the map cannot grow past its bound.
        for i in 0..(POST_ACTION_CREDIT_REPOS + 8) {
            grant_post_action_credit(&repo.join(i.to_string()), at);
        }
        let held = post_action_credits().lock().unwrap().len();
        assert!(held <= POST_ACTION_CREDIT_REPOS, "{held}");
    }

    #[cfg(unix)]
    #[test]
    fn a_swallowed_failure_can_still_be_reported() {
        let gate = leak(SpawnGate::new(2));
        let dir = tempfile::tempdir().unwrap();
        let (failures, message) = with_forced_spawn_failure(|| {
            let before = process_failures();
            let _ = reactive_spawn_in(gate, dir.path());
            (process_failures() - before, last_process_failure())
        });
        assert_eq!(failures, 1);
        assert!(message.is_some_and(|message| message.contains("forced by test")));
    }
}

/// The admission class decides which children and threads are starved.
#[cfg(all(test, target_os = "macos"))]
mod qos_band_tests {
    use super::*;

    /// Polls of 50 ms a probe child waits to see itself in the band. The child
    /// enters it before it execs, so what is waited for is `ps` showing it
    /// (a new image can read a transient higher priority first); the polls are
    /// also a hang backstop for a host so loaded that `ps` itself is starved,
    /// and a probe that gives up first proves nothing.
    const POLLS_TO_BE_DEMOTED: u32 = 1200;

    /// A child that reports its own scheduler priority once it reads as being
    /// in the band, or after `waits` polls of 50 ms when it never does. `ps`
    /// reports the background band as 4 and a default process far above 10.
    fn priority_probe(waits: u32) -> Command {
        let mut cmd = Command::new("/bin/sh");
        cmd.arg("-c").arg(format!(
            "i=0; p=; while [ $i -lt {waits} ]; do \
             p=$(/bin/ps -o pri= -p $$ | tr -d ' '); \
             [ \"$p\" -lt 10 ] && break; sleep 0.05; i=$((i+1)); done; \
             /bin/ps -o pri= -p $$ | tr -d ' '"
        ));
        cmd
    }

    /// Runs a probe child under the current class and returns its priority,
    /// the demotions that succeeded and the ones the OS refused meanwhile.
    fn priority_and_outcomes(waits: u32) -> (i32, u64, u64) {
        let (entered, failed) = band_outcomes();
        let priority = priority_of_child(waits);
        let (entered_after, failed_after) = band_outcomes();
        (priority, entered_after - entered, failed_after - failed)
    }

    fn priority_of_child(waits: u32) -> i32 {
        // A background spawn is shed when the shared budget is low; waiting
        // for it makes the test about the band, not about the budget.
        let run = with_background_budget_wait(true, || {
            run_bounded(
                priority_probe(waits),
                "priority-probe",
                // Under coverage on a loaded host the scaled limit passed
                // 1800 s, which run_bounded refuses before spawning anything.
                crate::test_support::coverage_relaxed(Duration::from_secs(120))
                    .min(NETWORK_TIMEOUT),
                None,
            )
        })
        .expect("probe ran");
        String::from_utf8_lossy(&run.stdout)
            .trim()
            .parse()
            .expect("priority")
    }

    #[test]
    fn a_background_class_child_is_in_the_background_band() {
        let (priority, entered, failed) = with_admission(Admission::Background, || {
            priority_and_outcomes(POLLS_TO_BE_DEMOTED)
        });
        assert_eq!(failed, 0, "the OS refused a demotion");
        assert!(entered >= 1, "no demotion succeeded");
        assert!(priority < 10, "background child ran at priority {priority}");
    }

    /// The mis-classed write must never be starved: a user action and a plain
    /// read both stay at default priority. The probe waits long enough that a
    /// late demotion would have shown.
    #[test]
    fn a_user_action_child_is_not_in_the_background_band() {
        for class in [Admission::Interactive, Admission::Reactive] {
            let priority = with_admission(class, || priority_of_child(12));
            assert!(
                priority >= 10,
                "{} child ran at priority {priority}",
                class.name()
            );
        }
    }

    #[test]
    fn the_background_band_scope_demotes_a_child_of_any_class() {
        let (priority, entered, failed) = with_admission(Admission::Reactive, || {
            with_background_band(|| priority_and_outcomes(POLLS_TO_BE_DEMOTED))
        });
        assert_eq!(failed, 0, "the OS refused a demotion");
        assert!(entered >= 1, "no demotion succeeded");
        assert!(priority < 10, "scoped child ran at priority {priority}");
        let after = with_admission(Admission::Reactive, || priority_of_child(12));
        assert!(after >= 10, "the scope leaked: priority {after}");
    }

    #[test]
    fn background_scopes_run_the_thread_at_utility_and_give_it_back() {
        use crate::pools::{current_thread_qos, QOS_UTILITY};
        std::thread::spawn(|| {
            let class = || current_thread_qos().expect("qos").0;
            assert_ne!(class(), QOS_UTILITY);
            with_background_processes(|| {
                assert_eq!(class(), QOS_UTILITY);
                // Nested background scope: unchanged, and its exit must not
                // restore early.
                with_background_processes(|| assert_eq!(class(), QOS_UTILITY));
                assert_eq!(class(), QOS_UTILITY);
            });
            assert_ne!(class(), QOS_UTILITY);
            with_admission(Admission::Interactive, || {
                assert_ne!(class(), QOS_UTILITY);
            });
            let original = class();
            with_background_processes(|| {
                with_admission(Admission::Interactive, || assert_eq!(class(), original));
                assert_eq!(class(), QOS_UTILITY);
            });
            assert_eq!(class(), original);
        })
        .join()
        .expect("qos thread");
    }
}

#[test]
fn fake_clock_refills_expires_credits_and_throttles_warnings_without_sleeping() {
    use super::clock::{Clock, FakeClock};
    use super::gate::{POST_ACTION_CREDIT_WINDOW, REFUSAL_REPORT_INTERVAL, SPAWN_QUEUE_BUDGET};

    let start = Instant::now();
    let clock = Arc::new(FakeClock::stepping(start));
    let gate = SpawnGate::with_clock(4, 1, 1, clock.clone());

    // Spending the single burst token then calling acquire_judged steps the
    // virtual clock forward to refill with zero wall-clock sleep.
    with_admission(Admission::Reactive, || {
        let _permit = gate
            .acquire_judged(clock.now() + Duration::from_secs(5), &|| false)
            .expect("burst token available");
    });
    with_admission(Admission::Reactive, || {
        let _permit = gate
            .acquire_judged(clock.now() + Duration::from_secs(5), &|| false)
            .expect("stepping clock refills 1 token without wall sleep");
    });
    assert!(
        clock.now().saturating_duration_since(start) >= Duration::from_secs(1),
        "stepping clock advanced virtually to refill interval"
    );

    // Freeze tokens so queue budget exhaustion returns Refused immediately.
    gate.state.lock().unwrap().tokens = 0;
    gate.state.lock().unwrap().tokens_at = Some(clock.now() + Duration::from_secs(86_400));
    let refused = with_admission(Admission::Reactive, || {
        gate.acquire_judged(
            clock.now() + SPAWN_QUEUE_BUDGET + Duration::from_secs(5),
            &|| false,
        )
    })
    .err()
    .expect("frozen gate refuses after queue budget");
    assert!(matches!(refused.0, Refusal::Refused { .. }), "{refused:?}");

    // Isolated post-action credits expire deterministically on the injected clock.
    let repo = PathBuf::from("/tmp/fake-clock-credit-repo");
    gate.grant_credit(&repo);
    assert!(gate.take_credit(&repo));
    clock.advance(POST_ACTION_CREDIT_WINDOW);
    assert!(
        !gate.take_credit(&repo),
        "credit must expire with FakeClock"
    );

    // Throttled warning interval advances via FakeClock without sleeping.
    assert!(gate.report_due_at(&gate.shared_fallback_report_at, clock.now()));
    clock.advance(Duration::from_secs(1));
    assert!(!gate.report_due_at(&gate.shared_fallback_report_at, clock.now()));
    clock.advance(REFUSAL_REPORT_INTERVAL);
    assert!(gate.report_due_at(&gate.shared_fallback_report_at, clock.now()));

    // Shared budget virtual clock respects FakeClock advances.
    let dir = tempfile::tempdir().unwrap();
    let shared_path = dir.path().join("spawn-budget.v2");
    let sclock = Arc::new(FakeClock::new(start));
    let sgate = SpawnGate::with_clock(4, 1, 1, sclock.clone()).sharing(Ok(shared_path));
    with_admission(Admission::Reactive, || {
        let _p = sgate
            .acquire_judged(sclock.now() + Duration::from_secs(5), &|| false)
            .expect("initial shared token");
        let denied = sgate.acquire_judged(sclock.now(), &|| false);
        assert!(denied.is_err(), "shared budget spent");
    });
    sclock.advance(Duration::from_secs(1));
    with_admission(Admission::Reactive, || {
        let _p = sgate
            .acquire_judged(sclock.now() + Duration::from_secs(5), &|| false)
            .expect("shared token refilled on FakeClock advance");
    });
}

#[cfg(all(test, target_os = "macos"))]
#[test]
fn run_options_explicit_qos_tier_overrides_interactive_admission() {
    let (_before_ok, _) = band_outcomes();
    let mut cmd = Command::new("/bin/sh");
    cmd.args(["-c", "true"]);
    let (before_ok, before_err) = band_outcomes();
    let run = with_admission(Admission::Interactive, || {
        let mut opts = RunOptions::new(DEFAULT_TIMEOUT);
        opts.qos = Some(QosTier::Background);
        run_observed_with(&mut cmd, "qos-override-test", opts, None, 1024, &mut ())
    })
    .expect("qos override runs");
    assert!(run.success);
    let (after_ok, after_err) = band_outcomes();
    assert_eq!(after_ok, before_ok + 1, "background band count incremented");
    assert_eq!(after_err, before_err);
}

#[cfg(unix)]
#[test]
fn run_options_custom_kill_grace_reaps_sigterm_ignoring_child_promptly() {
    let mut cmd = Command::new("/bin/sh");
    cmd.args(["-c", r#"trap "" TERM; exec sleep 60"#]);
    let mut opts = RunOptions::new(Duration::from_millis(40));
    opts.kill_grace = Duration::from_millis(20);
    opts.keep_deadline_prefix = true;
    let started = Instant::now();
    let run = run_observed_with(&mut cmd, "short-grace-test", opts, None, 1024, &mut ())
        .expect("keep_deadline_prefix returns BoundedRun");
    let elapsed = started.elapsed();
    assert!(!run.success);
    assert!(matches!(run.incomplete, Some(Incomplete::Deadline { .. })));
    assert!(
        elapsed < crate::test_support::coverage_relaxed(DEFAULT_KILL_GRACE),
        "reaping with 20ms grace took {elapsed:?} (>= default {:?})",
        DEFAULT_KILL_GRACE
    );
}

/// Records what the runner says about how a child's wait ended.
#[cfg(unix)]
#[derive(Default)]
struct RecordsExit {
    facts: Option<ExitFacts>,
    /// Cancels once the child has written something, which is after it started:
    /// a cancel raised earlier is a spawn that never happened.
    cancel_after_output: bool,
    heard: bool,
}

#[cfg(unix)]
impl ProcessObserver for RecordsExit {
    fn cancelled(&self) -> bool {
        self.cancel_after_output && self.heard
    }
    fn output(&mut self, _stream: OutputStream, bytes: &[u8]) {
        self.heard |= !bytes.is_empty();
    }
    fn exited(&mut self, facts: &ExitFacts) {
        assert!(self.facts.is_none(), "told twice");
        self.facts = Some(*facts);
    }
}

#[cfg(unix)]
#[test]
fn the_observer_is_told_the_raw_wait_status_of_a_child_a_signal_ended() {
    let mut observer = RecordsExit::default();
    let mut cmd = Command::new("sh");
    cmd.args(["-c", "kill -9 $$"]);
    let run = run_observed(
        &mut cmd,
        "killed",
        Duration::from_secs(20),
        None,
        1024,
        &mut observer,
    )
    .unwrap();
    let facts = observer.facts.expect("told how the wait ended");
    assert_eq!(facts.raw_wait_status, Some(libc::SIGKILL));
    assert_eq!(facts.signal, Some(libc::SIGKILL));
    assert_eq!(run.signal, facts.signal);
    assert!(!facts.core_dumped && facts.stopped_signal.is_none() && !facts.continued);
    assert!(!facts.cancelled && !facts.deadline_expired);
    assert!(
        !facts.runner_signalled_group,
        "the child killed itself; the runner signalled nothing"
    );
}

#[cfg(unix)]
#[test]
fn the_observer_is_told_when_the_runner_itself_stopped_the_child() {
    let mut cancelled = RecordsExit {
        cancel_after_output: true,
        ..RecordsExit::default()
    };
    let mut cmd = Command::new("sh");
    cmd.args(["-c", "echo started; sleep 30"]);
    let run = run_observed(
        &mut cmd,
        "cancelled",
        Duration::from_secs(60),
        None,
        1024,
        &mut cancelled,
    )
    .unwrap();
    assert!(run.cancelled);
    let facts = cancelled.facts.expect("told how the wait ended");
    assert!(facts.cancelled && facts.runner_signalled_group, "{facts:?}");
    assert!(!facts.deadline_expired);

    let mut late = RecordsExit::default();
    let mut cmd = Command::new("sh");
    cmd.args(["-c", "sleep 30"]);
    let _ = run_observed(
        &mut cmd,
        "slow",
        Duration::from_millis(200),
        None,
        1024,
        &mut late,
    );
    let facts = late.facts.expect("told how the wait ended");
    assert!(
        facts.deadline_expired && facts.runner_signalled_group,
        "{facts:?}"
    );
    assert_eq!(facts.raw_wait_status, None, "no status was read: {facts:?}");
}

/// A body that hands work to another thread keeps its admission class only if
/// it carries it: a new thread starts in the process default, which turned a
/// worktree create's devmap provisioning back into optional work.
#[test]
fn carry_admission_puts_the_callers_class_on_the_thread_that_runs_the_body() {
    let (plain, carried) = run_as_user_action(|| {
        assert_eq!(current_admission(), Admission::Interactive);
        thread::scope(|scope| {
            let plain = scope.spawn(current_admission);
            let carried = scope.spawn(carry_admission(current_admission));
            (plain.join().unwrap(), carried.join().unwrap())
        })
    });
    assert_eq!(plain, Admission::Reactive, "a bare spawn keeps the default");
    assert_eq!(carried, Admission::Interactive);
    assert_eq!(
        current_admission(),
        Admission::Reactive,
        "the user action ends with its body"
    );
}

/// A hand-run `--ignored` helper must not drain the developer's own per-user
/// record: without the marker an isolated test profile sets, the spend is
/// refused before the record is opened.
#[test]
fn exhaust_per_user_record_refuses_without_the_isolated_profile_marker() {
    assert!(std::env::var_os(super::EXHAUST_RECORD_MARKER).is_none());
    let refused = exhaust_per_user_record().expect_err("no marker, no spend");
    assert!(refused.contains(super::EXHAUST_RECORD_MARKER), "{refused}");
}

/// Unmetered spawns (a `devmap` read) neither wait for nor spend a git rate
/// token, but are still concurrency-limited: a spent bucket sheds background
/// work, and the same thread inside `with_unmetered_spawn_rate` is admitted.
#[test]
fn an_unmetered_background_spawn_is_admitted_on_a_spent_bucket_a_metered_one_is_shed() {
    let gate = spent_gate();
    let far = || Instant::now() + Duration::from_secs(5);
    with_background_processes(|| {
        assert_eq!(
            gate.acquire_until(far(), &|| false).err(),
            Some(Refusal::Shed),
            "the control: a metered background spawn on this bucket is shed"
        );
        let permit = with_unmetered_spawn_rate(|| gate.acquire_until(far(), &|| false));
        assert!(permit.is_ok(), "{:?}", permit.err());
        assert_eq!(
            gate.counters()[Admission::Background.index()].admitted,
            1,
            "it holds a slot like any other spawn"
        );
    });
    assert_eq!(gate.state.lock().unwrap().tokens, 0, "no token was spent");
}
