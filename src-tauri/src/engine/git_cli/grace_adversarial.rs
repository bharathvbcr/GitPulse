//! Adversarial tests for R2-02 (the grace after an ask keeps draining).
//! Each test names the attack it makes; a failure is a break in
//! `ask_and_drain` / `stop_and_reap_waiting`.

use super::*;

fn sh(script: &str, args: &[&Path]) -> Command {
    let mut cmd = Command::new("sh");
    cmd.arg("-c").arg(script).arg("sh");
    for a in args {
        cmd.arg(a);
    }
    cmd
}

fn alive(pid: i32) -> bool {
    // SAFETY: signal 0 only probes.
    unsafe { libc::kill(pid, 0) == 0 }
}

fn thread_cpu() -> Duration {
    let mut ts = libc::timespec {
        tv_sec: 0,
        tv_nsec: 0,
    };
    // SAFETY: ts is a valid out-pointer.
    unsafe { libc::clock_gettime(libc::CLOCK_THREAD_CPUTIME_ID, &mut ts) };
    Duration::new(ts.tv_sec as u64, ts.tv_nsec as u32)
}

struct CancelWhenExists(PathBuf);
impl ProcessObserver for CancelWhenExists {
    fn cancelled(&self) -> bool {
        self.0.exists()
    }
}

/// Kills a pid recorded in `file` if the code under test leaked it; returns
/// the pid when it had to.
fn reap_leak(file: &Path) -> Option<i32> {
    let pid: i32 = std::fs::read_to_string(file).ok()?.trim().parse().ok()?;
    let until = Instant::now() + Duration::from_secs(2);
    while alive(pid) && Instant::now() < until {
        std::thread::sleep(Duration::from_millis(20));
    }
    if alive(pid) {
        // SAFETY: a pid this test recorded from its own child.
        unsafe { libc::kill(pid, libc::SIGKILL) };
        Some(pid)
    } else {
        None
    }
}

const BOUND: Duration = Duration::from_millis(1200);

/// Attack 1: a stderr-only flooder that ignores the ask while stdout is
/// closed. The grace must stay near CHILD_GRACE; CPU is reported.
#[test]
fn grace_is_bounded_when_stdout_is_closed_and_stderr_floods() {
    let _kept = KeepDeadlinePrefix::enter();
    let dir = tempfile::tempdir().unwrap();
    let marker = dir.path().join("m");
    let script = r#"trap '' TERM; exec 1>&-; touch "$1"; while :; do echo yyyyyyyyyyyyyyyyyyyyyyyyyyyyyyyyyyyyyyyy >&2; done"#;
    let cpu = thread_cpu();
    let started = Instant::now();
    let run = run_bounded(sh(script, &[&marker]), "f", Duration::from_millis(300), None)
        .expect("deadline keeps prefix");
    let wall = started.elapsed();
    let cpu = thread_cpu() - cpu;
    eprintln!("wall {wall:?} waiter cpu {cpu:?}");
    assert!(wall < Duration::from_millis(300) + BOUND, "{wall:?}");
    assert!(matches!(run.incomplete, Some(Incomplete::Deadline { .. })));
}

/// Attack 1: a grandchild in its own group keeps the pipes after the child
/// exits on the ask. The run must not wait for EOF past the grace.
#[test]
fn a_grandchild_retaining_the_pipe_does_not_stretch_the_grace() {
    let _kept = KeepDeadlinePrefix::enter();
    let dir = tempfile::tempdir().unwrap();
    let pidfile = dir.path().join("pid");
    let script = r#"set -m; sleep 30 & echo $! > "$1"; trap 'exit 0' TERM; while :; do sleep 0.05; done"#;
    let started = Instant::now();
    let run = run_bounded(sh(script, &[&pidfile]), "g", Duration::from_millis(400), None)
        .expect("deadline keeps prefix");
    let wall = started.elapsed();
    let leaked = reap_leak(&pidfile);
    assert!(wall < Duration::from_millis(400) + BOUND, "{wall:?}");
    assert!(matches!(run.incomplete, Some(Incomplete::Deadline { .. })));
    eprintln!("own-group grandchild leaked: {leaked:?}");
}

/// Attack 2: output past the caps during the grace sets the flags and the
/// buffers stay within their caps.
#[test]
fn output_over_the_caps_during_the_grace_sets_flags_and_stays_bounded() {
    let _kept = KeepDeadlinePrefix::enter();
    let dir = tempfile::tempdir().unwrap();
    let marker = dir.path().join("m");
    let script = r#"trap 'head -c 30000000 /dev/zero; head -c 30000000 /dev/zero >&2; exit 0' TERM; touch "$1"; while :; do sleep 0.05; done"#;
    let cap = 100_000;
    let run = run_bounded_capped(
        sh(script, &[&marker]),
        "c",
        Duration::from_millis(300),
        None,
        cap,
    )
    .expect("deadline keeps prefix");
    assert!(
        run.stdout.len() <= cap,
        "stdout {} past cap",
        run.stdout.len()
    );
    assert!(
        matches!(run.incomplete, Some(Incomplete::Deadline { over_cap: Some(c), .. }) if c == cap),
        "{:?}",
        run.incomplete
    );
    assert!(
        run.stderr.len() <= 4 * 1024 * 1024 + 256,
        "stderr {} past its cap",
        run.stderr.len()
    );
    assert_eq!(
        run.stderr_incomplete,
        Some(Incomplete::OverCap(4 * 1024 * 1024))
    );
}

/// Attack 3: a child that exits 0 inside the grace after a deadline is
/// still a deadline.
#[test]
fn a_clean_exit_inside_a_deadline_grace_is_still_a_deadline() {
    let _kept = KeepDeadlinePrefix::enter();
    let dir = tempfile::tempdir().unwrap();
    let marker = dir.path().join("m");
    let script = r#"trap 'echo bye; exit 0' TERM; touch "$1"; while :; do sleep 0.05; done"#;
    let run = run_bounded(sh(script, &[&marker]), "d", Duration::from_millis(300), None)
        .expect("deadline keeps prefix");
    assert!(!run.success);
    assert!(!run.cancelled);
    assert!(matches!(run.incomplete, Some(Incomplete::Deadline { .. })));
}

/// Attack 3: a cancel racing a natural exit never yields cancelled+success,
/// and the outcome is never neither.
#[test]
fn a_cancel_racing_a_natural_exit_reports_one_consistent_outcome() {
    for round in 0..40 {
        let dir = tempfile::tempdir().unwrap();
        let marker = dir.path().join("m");
        let mut cmd = sh(r#"touch "$1"; exit 0"#, &[&marker]);
        let mut observer = CancelWhenExists(marker.clone());
        let run = run_observed(
            &mut cmd,
            "r",
            Duration::from_secs(10),
            None,
            MAX_OUTPUT_BYTES,
            &mut observer,
        )
        .expect("run");
        assert!(
            !(run.success && run.cancelled),
            "round {round}: success and cancelled"
        );
        assert!(
            run.cancelled || run.success,
            "round {round}: neither cancelled nor success: {:?}",
            run.status_code
        );
    }
}

/// Attack 4: stdin still open (more input pending than the pipe takes) when
/// the ask arrives. A child whose cleanup reads stdin to EOF cannot finish
/// unless stdin is closed at the ask.
#[test]
fn a_cleanup_that_reads_stdin_to_eof_completes_when_stdin_closes_at_the_ask() {
    let _kept = KeepDeadlinePrefix::enter();
    let dir = tempfile::tempdir().unwrap();
    let marker = dir.path().join("lock");
    let script = r#"trap 'cat >/dev/null; rm -f "$1"; exit 0' TERM; touch "$1"; while :; do sleep 0.05; done"#;
    let input = vec![b'i'; 400_000];
    let run = run_bounded(
        sh(script, &[&marker]),
        "s",
        Duration::from_millis(400),
        Some(&input),
    )
    .expect("deadline keeps prefix");
    assert!(matches!(run.incomplete, Some(Incomplete::Deadline { .. })));
    assert!(
        !marker.exists(),
        "cleanup blocked on stdin EOF: stdin stayed open through the grace until SIGKILL"
    );
}

/// Attack 5: a same-group grandchild that traps the ask and keeps writing
/// after its parent exited is killed at the end of the grace, and its output
/// is read.
#[test]
fn a_term_trapping_grandchild_still_writing_is_killed_and_drained() {
    let _kept = KeepDeadlinePrefix::enter();
    let dir = tempfile::tempdir().unwrap();
    let pidfile = dir.path().join("pid");
    let script = r#"sh -c 'trap "" TERM; while :; do echo zzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzz; done' & echo $! > "$1"; wait"#;
    let started = Instant::now();
    let run = run_bounded(sh(script, &[&pidfile]), "h", Duration::from_millis(300), None)
        .expect("deadline keeps prefix");
    let wall = started.elapsed();
    let leaked = reap_leak(&pidfile);
    assert!(
        leaked.is_none(),
        "grandchild {leaked:?} survived the end of the grace"
    );
    assert!(wall < Duration::from_millis(300) + BOUND, "{wall:?}");
    assert!(run.stdout.starts_with(b"zzzz"), "its output was not drained");
    assert!(matches!(run.incomplete, Some(Incomplete::Deadline { .. })));
}

/// Attack 5, cancel path.
#[test]
fn a_cancel_kills_a_term_trapping_writer_and_reports_cancelled() {
    let dir = tempfile::tempdir().unwrap();
    let marker = dir.path().join("m");
    let script = r#"trap '' TERM; touch "$1"; while :; do echo zzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzz; done"#;
    let mut cmd = sh(script, &[&marker]);
    let mut observer = CancelWhenExists(marker.clone());
    let started = Instant::now();
    let run = run_observed(
        &mut cmd,
        "w",
        Duration::from_secs(20),
        None,
        MAX_OUTPUT_BYTES,
        &mut observer,
    )
    .expect("a cancel returns the run");
    assert!(
        started.elapsed() < Duration::from_millis(3000),
        "{:?}",
        started.elapsed()
    );
    assert!(run.cancelled && !run.success);
}
