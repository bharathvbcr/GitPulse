//! Adversarial tests for `RunError` and slot-wait/timeout refusal diagnostics.

use super::*;
#[cfg(unix)]
use std::process::Command;
use std::time::Duration;

// ---- RunError ---------------------------------------------------------------

#[test]
fn run_error_message_shapes() {
    let slot = format!("devmap{TIMEOUT_MARKER}0.5{SLOT_WAIT_SUFFIX}");
    let ran = format!("devmap{TIMEOUT_MARKER}30s");
    let deferred = refusal_message(
        "devmap build",
        Refusal::Refused {
            waited: Duration::from_millis(500),
        },
    );
    // raw, coded, doubly coded
    assert_eq!(RunError::classify(&ran), RunError::Timeout);
    assert_eq!(RunError::classify(&format!("[a] {ran}")), RunError::Timeout);
    assert_eq!(
        RunError::classify(&format!("[a] [b] {ran}")),
        RunError::Other,
        "one code prefix is stripped, for every anchored check alike"
    );
    assert_eq!(
        RunError::classify(&format!(
            "[a] [b] {deferred}",
            deferred = deferred.message()
        )),
        RunError::Other,
        "nested code prefix is not stripped by is_deferred_under_load"
    );
    // a label that spells "deferred"
    assert_eq!(
        RunError::classify(&format!("devmap deferred{TIMEOUT_MARKER}1s")),
        RunError::Timeout
    );
    // a path containing the timeout phrase is only a timeout if the message is
    // the runner's: an exit-status failure that names such a path is not.
    assert_eq!(
        RunError::classify("devmap exited with status 2: /tmp/job timed out after 3 retries/x"),
        RunError::Other,
        "exit-failure text that merely contains the marker is not the runner's deadline"
    );
    assert_eq!(RunError::classify(&slot), RunError::SlotWaitTimeout);
    assert_eq!(RunError::classify(deferred.message()), RunError::Deferred);
}

/// BREAK: a caller-prefixed spawn failure whose program path spells the timeout
/// phrase still reads as a timeout: the `Failed to spawn` check is anchored at
/// the start of the message, so any prefix (a `[code]` or a caller wrapper)
/// defeats the precedence the doc comment promises.
#[test]
fn prefixed_spawn_failure_with_hostile_path_is_not_a_timeout() {
    let hostile =
        format!("Failed to spawn /Users/x/devmap{TIMEOUT_MARKER}1s/devmap: permission denied");
    assert!(!is_run_timeout(&format!("[spawn_failed] {hostile}")));
    assert!(!is_run_timeout(&format!("Install failed: {hostile}")));
}

/// BREAK: a child's own stderr relayed through a failure message that happens
/// to carry " timed out after " (curl and cargo print "Operation timed out
/// after 30001 milliseconds") reads as the runner's deadline.
#[test]
fn a_relayed_child_timeout_is_not_a_runner_timeout() {
    assert!(!is_run_timeout(
        "git fetch failed: fatal: unable to access 'https://h/r.git/': Operation timed out after 30001 milliseconds with 0 bytes received"
    ));
}

/// A real runner timeout, from a real slow child and a tiny deadline, through
/// the exact runners devmap::cli (`run_observed`) and tool_install
/// (`run_bounded`) use, is still `Timeout`.
#[cfg(unix)]
#[test]
fn a_real_slow_child_is_classified_as_a_timeout_by_both_runners() {
    use std::os::unix::fs::PermissionsExt;
    let dir = tempfile::TempDir::new().unwrap();
    let bin = dir.path().join("devmap");
    std::fs::write(&bin, "#!/bin/sh\nsleep 5\n").unwrap();
    std::fs::set_permissions(&bin, std::fs::Permissions::from_mode(0o755)).unwrap();

    let err = run_bounded(
        Command::new(&bin),
        "devmap",
        Duration::from_millis(200),
        None,
    )
    .expect_err("slow child must time out");
    assert!(is_run_timeout(err.message()), "{err}", err = err.message());
    assert_eq!(
        RunError::classify(err.message()),
        RunError::Timeout,
        "{err}",
        err = err.message()
    );

    let mut cmd = Command::new(&bin);
    let err = run_observed(
        &mut cmd,
        "devmap",
        Duration::from_millis(200),
        None,
        MAX_OUTPUT_BYTES,
        &mut (),
    )
    .expect_err("slow child must time out");
    assert!(is_run_timeout(err.message()), "{err}", err = err.message());

    let missing = run_bounded(
        Command::new(dir.path().join("nope")),
        "devmap",
        Duration::from_secs(5),
        None,
    )
    .expect_err("missing binary");
    assert_eq!(
        RunError::classify(missing.message()),
        RunError::SpawnNotFound,
        "{missing}",
        missing = missing.message()
    );
}

/// A child that never started because no process slot came free inside its
/// deadline is not a run that timed out: `run_captured` hands it back as the
/// gate's error, which classifies as `SlotWaitTimeout`.
#[cfg(unix)]
#[test]
fn run_captured_reports_a_slot_wait_as_an_error_not_a_timed_out_run() {
    use std::time::Instant;
    let gate = leak_gate(SpawnGate::new(1));
    let permit = gate
        .acquire(Instant::now() + Duration::from_secs(10))
        .expect("slot");
    let out = with_gate_for_test(gate, || {
        run_captured(
            "/usr/bin/true",
            &[],
            None,
            Duration::from_millis(50),
            &[],
            MAX_OUTPUT_BYTES,
        )
    });
    drop(permit);
    match out {
        Err(e) => assert_eq!(
            RunError::classify(e.message()),
            RunError::SlotWaitTimeout,
            "{}",
            e.message()
        ),
        Ok(outcome) => panic!("a slot wait came back as an outcome: {outcome:?}"),
    }
}

/// The slot-wait check is anchored at the message's end: a program whose path
/// spells the phrase and which really ran out of time is still a timed-out run.
#[cfg(unix)]
#[test]
fn run_captured_keeps_a_real_timeout_when_the_program_path_spells_the_slot_phrase() {
    let dir = tempfile::tempdir().unwrap();
    let hostile = dir
        .path()
        .join(format!("x{TIMEOUT_MARKER}1{SLOT_WAIT_SUFFIX}"));
    std::fs::create_dir(&hostile).unwrap();
    let program = hostile.join("sleep");
    std::os::unix::fs::symlink("/bin/sleep", &program).unwrap();
    let out = run_captured(
        program.to_str().unwrap(),
        &["5"],
        None,
        Duration::from_secs(1),
        &[],
        MAX_OUTPUT_BYTES,
    )
    .expect("a real timeout is an outcome");
    assert!(
        matches!(out, RunOutcome::TimedOut(d) if d == Duration::from_secs(1)),
        "{out:?}"
    );
}
