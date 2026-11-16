//! The host-load exec probe's place in the spawn gate: bounded like any
//! background child, but never shed by the verdict it feeds and never spending
//! or refused by the git rate bucket.

use super::{with_admission, with_background_processes, Admission, Refusal, SpawnGate};
use crate::sysload::{inject_governor, GovernorInput, Level};
use std::time::{Duration, Instant};

fn far() -> Instant {
    Instant::now() + Duration::from_secs(10)
}

/// An empty rate bucket sheds a background spawn; the probe, which must not be
/// blinded by it, is admitted from the same gate and spends nothing.
#[test]
fn the_probe_is_admitted_with_the_rate_bucket_empty_and_spends_no_token() {
    let gate = SpawnGate::with_rate(16, 8, 1);
    for _ in 0..6 {
        drop(with_background_processes(|| gate.acquire(far())).expect("above reserve"));
    }
    let shed = with_background_processes(|| gate.acquire_until(far(), &|| false).err());
    assert_eq!(shed, Some(Refusal::Shed), "the control is shed");
    let probe = with_background_processes(|| {
        super::gate::with_exec_probe(|| gate.acquire_until(far(), &|| false))
    });
    assert!(probe.is_ok(), "the probe is not refused by the bucket");
    drop(probe);
    let again = with_background_processes(|| gate.acquire_until(far(), &|| false).err());
    assert_eq!(
        again,
        Some(Refusal::Shed),
        "the probe took no token from the bucket"
    );
}

/// Under critical memory pressure every class but a click is shed; the probe
/// is still admitted, and the exemption ends with its scope.
#[test]
fn the_probe_is_admitted_while_the_verdict_sheds_everything_else() {
    let gate = SpawnGate::with_rate(16, 8, 1);
    let _pressed = inject_governor(GovernorInput::test_pressure(Level::Critical));
    let background = || with_background_processes(|| gate.acquire_until(far(), &|| false).err());
    assert!(
        matches!(background(), Some(Refusal::Pressure { .. })),
        "the control is shed"
    );
    let probe = with_background_processes(|| {
        super::gate::with_exec_probe(|| gate.acquire_until(far(), &|| false))
    });
    assert!(probe.is_ok(), "the probe must not be shed by its own verdict");
    drop(probe);
    assert!(
        matches!(background(), Some(Refusal::Pressure { .. })),
        "the exemption ended with the probe's scope"
    );
    assert!(
        with_admission(Admission::Reactive, || {
            gate.acquire_until(far(), &|| false).err()
        })
        .is_some(),
        "reactive work is still shed"
    );
}

/// A real child through the real funnel: a time, from admission to reaped.
#[cfg(unix)]
#[test]
fn a_trivial_child_is_timed_through_the_runner_even_under_pressure() {
    use super::{time_exec_probe, ExecProbeRun};
    let _pressed = inject_governor(GovernorInput::test_pressure(Level::Critical));
    match time_exec_probe(std::path::Path::new("/usr/bin/true"), Duration::from_secs(30)) {
        ExecProbeRun::Ran(took) => assert!(took < Duration::from_secs(30), "{took:?}"),
        other => panic!("expected a timing, got {other:?}"),
    }
}

/// A program that cannot be started gives no timing: unknown, never fast.
#[test]
fn a_probe_that_cannot_start_is_a_failure_not_a_timing() {
    use super::{time_exec_probe, ExecProbeRun};
    assert_eq!(
        time_exec_probe(
            std::path::Path::new("/nonexistent/gitpulse-exec-probe"),
            Duration::from_secs(5)
        ),
        ExecProbeRun::Failed
    );
}

/// A child that outlives the deadline is the slowest outcome, not a failure.
#[cfg(unix)]
#[test]
fn a_probe_past_its_deadline_is_a_deadline_hit() {
    use super::{time_exec_probe, ExecProbeRun};
    let hang = std::path::Path::new("/bin/sleep");
    if !hang.exists() {
        return;
    }
    // `sleep` with no operand exits at once with a usage error, so run a
    // script that waits instead.
    let dir = tempfile::tempdir().expect("tempdir");
    let script = dir.path().join("wait.sh");
    std::fs::write(&script, "#!/bin/sh\nexec /bin/sleep 30\n").expect("write");
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).expect("chmod");
    }
    assert_eq!(
        time_exec_probe(&script, Duration::from_millis(300)),
        ExecProbeRun::DeadlineHit
    );
}

/// Slow start-up, as the verdict says it: background work is refused with the
/// deferral marker and the cause named, a read someone waits on is admitted.
#[test]
fn slow_start_up_defers_background_spawns_by_name_and_admits_reactive_ones() {
    use super::{is_deferred_under_load, refusal_message};
    use crate::sysload::{Binding, ExecLatency, Reading};
    let mut input = GovernorInput::test_pressure(Level::Normal);
    input.readings.exec_latency = Reading::Value(ExecLatency {
        median_ms: 1_360,
        n: 12,
        slow: true,
    });
    let _slow = inject_governor(input);
    let gate = SpawnGate::with_rate(16, 8, 1);
    let background = with_background_processes(|| gate.acquire_until(far(), &|| false).err());
    let refusal = background.expect("background work is shed");
    assert_eq!(
        refusal,
        Refusal::Pressure {
            reactive: false,
            cause: Some(Binding::ExecLatency)
        }
    );
    let message = refusal_message("git status", refusal);
    assert!(
        message.message().contains(
            "background work is shed while process start-up is slow (median 1.4 s)"
        ),
        "{}",
        message.message()
    );
    assert!(
        is_deferred_under_load(message.message()),
        "{}",
        message.message()
    );
    assert!(
        with_admission(Admission::Reactive, || gate.acquire_until(far(), &|| false)).is_ok(),
        "reactive reads are admitted"
    );
    assert!(
        with_admission(Admission::Interactive, || gate.acquire_until(far(), &|| false)).is_ok(),
        "a click is admitted"
    );
}
