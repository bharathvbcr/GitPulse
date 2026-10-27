//! Adversarial tests for lane W4b (the macOS background band entered in the
//! child before exec, ft-b597add5). Test-only.
//!
//! The band is read two ways. `ps -o pri` (what the lane's own tests wait on)
//! is a scheduler priority that a new image can show higher for a few
//! milliseconds; a child's own `getpriority(PRIO_DARWIN_PROCESS, 0)` is the
//! Darwin background state itself, 1 in the band and 0 outside it, and is read
//! by the child at its first instruction. The probes here re-run this very test
//! binary as the child (`ADV_W4B_PROBE` set) and ask it for that number.

use crate::engine::git_cli::{
    run_bounded, run_bounded_capped, run_observed, with_admission, with_background_band,
    with_background_processes, Admission,
};
use crate::procguard::{self, band_refusal, spawn_background, with_band_hook};
use std::os::raw::c_int;
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicBool, AtomicI32, Ordering};
use std::sync::Mutex;
use std::time::Duration;

const PROBE_VAR: &str = "ADV_W4B_PROBE";
const PROBE_GIT_VAR: &str = "ADV_W4B_PROBE_GIT";
const PROBE: &str = "adv_w4b_tests::adv_w4b_probe_self_band";
const PROBE_GIT: &str = "adv_w4b_tests::adv_w4b_probe_background_git";

/// Not a test of anything when run directly: as a child of the tests below it
/// prints the Darwin background state of its own process.
#[test]
fn adv_w4b_probe_self_band() {
    if std::env::var_os(PROBE_VAR).is_none() {
        return;
    }
    // SAFETY: plain syscall on integers.
    let state = unsafe { libc::getpriority(libc::PRIO_DARWIN_PROCESS, 0) };
    println!("selfband={state}");
}

fn probe_command() -> Command {
    let exe = std::env::current_exe().expect("test binary");
    let mut cmd = Command::new(exe);
    cmd.args(["--exact", PROBE, "--nocapture", "--test-threads=1"])
        .env(PROBE_VAR, "1")
        .stdin(Stdio::null());
    cmd
}

/// The same probe behind `sh`, which forks it as a grandchild (the `;` keeps
/// the shell from exec'ing it in place).
fn probe_behind_sh() -> Command {
    let exe = std::env::current_exe().expect("test binary");
    let mut cmd = Command::new("/bin/sh");
    cmd.arg("-c")
        .arg(format!(
            "'{}' --exact {PROBE} --nocapture --test-threads=1; echo sh-done",
            exe.display()
        ))
        .env(PROBE_VAR, "1")
        .stdin(Stdio::null());
    cmd
}

fn self_band(stdout: &[u8]) -> i32 {
    let text = String::from_utf8_lossy(stdout);
    let (_, rest) = text
        .split_once("selfband=")
        .unwrap_or_else(|| panic!("the probe printed no reading:\n{text}"));
    rest.lines()
        .next()
        .and_then(|value| value.trim().parse().ok())
        .unwrap_or_else(|| panic!("the reading is not a number:\n{text}"))
}

/// Every API a production caller reaches the seam through, inside a background
/// scope: the child must read 1 at its first instruction, directly and as a
/// grandchild behind `sh`. 30 rounds each.
#[test]
fn adv_w4b_every_entry_point_banded_under_background_scope() {
    for round in 0..30 {
        for (name, build) in [
            ("direct", probe_command as fn() -> Command),
            ("behind-sh", probe_behind_sh),
        ] {
            let via_bounded = with_background_processes(|| {
                run_bounded(build(), "adv-w4b-bounded", Duration::from_secs(60), None)
            })
            .expect("run_bounded");
            assert_eq!(self_band(&via_bounded.stdout), 1, "{name} bounded #{round}");
            let via_capped = with_background_processes(|| {
                run_bounded_capped(
                    build(),
                    "adv-w4b-capped",
                    Duration::from_secs(60),
                    None,
                    1 << 20,
                )
            })
            .expect("run_bounded_capped");
            assert_eq!(self_band(&via_capped.stdout), 1, "{name} capped #{round}");
            let mut cmd = build();
            let via_observed = with_background_processes(|| {
                run_observed(
                    &mut cmd,
                    "adv-w4b-observed",
                    Duration::from_secs(60),
                    None,
                    1 << 20,
                    &mut (),
                )
            })
            .expect("run_observed");
            assert_eq!(
                self_band(&via_observed.stdout),
                1,
                "{name} observed #{round}"
            );
        }
    }
}

/// The forced band (devmap build under a user's click) reaches the child
/// whatever the admission class, and an unforced user action or read does not.
#[test]
fn adv_w4b_forced_band_and_unbanded_classes() {
    for round in 0..20 {
        for class in [Admission::Interactive, Admission::Reactive] {
            let run = with_admission(class, || {
                run_bounded(
                    probe_command(),
                    "adv-w4b-user",
                    Duration::from_secs(60),
                    None,
                )
            })
            .expect("run");
            assert_eq!(self_band(&run.stdout), 0, "{class:?} #{round} was banded");
            let forced = with_admission(class, || {
                with_background_band(|| {
                    run_bounded(
                        probe_behind_sh(),
                        "adv-w4b-forced",
                        Duration::from_secs(60),
                        None,
                    )
                })
            })
            .expect("run");
            assert_eq!(
                self_band(&forced.stdout),
                1,
                "{class:?} forced #{round} not banded"
            );
        }
    }
}

/// The band is the child's own state at its first instruction, not a state the
/// scheduler settles into: 150 children on a loaded host, none may read 0.
#[test]
fn adv_w4b_first_instruction_reads_the_band_every_time() {
    let mut zero = 0;
    for _ in 0..150 {
        let run = with_background_processes(|| {
            run_bounded(
                probe_command(),
                "adv-w4b-first",
                Duration::from_secs(60),
                None,
            )
        })
        .expect("run");
        if self_band(&run.stdout) != 1 {
            zero += 1;
        }
    }
    assert_eq!(
        zero, 0,
        "{zero} of 150 background children started outside the band"
    );
}

static NEXT_ERRNO: AtomicI32 = AtomicI32::new(0);
static SERIAL: Mutex<()> = Mutex::new(());

fn hook_failing_with_the_chosen_errno() -> Result<(), c_int> {
    Err(NEXT_ERRNO.load(Ordering::Relaxed))
}

/// Every errno a hook can fail with, up to the tag bit, round-trips as a band
/// refusal with its own errno text, never as a missing program, and the kind is
/// `Other` for every one of them.
#[test]
fn adv_w4b_every_hook_errno_round_trips_as_a_band_refusal() {
    let _serial = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
    let mut wrong = Vec::new();
    for errno in (1..=0x200).chain([0x7ff, 0xfff]) {
        NEXT_ERRNO.store(errno, Ordering::Relaxed);
        let mut cmd = Command::new("/usr/bin/true");
        cmd.stdin(Stdio::null()).stdout(Stdio::null());
        let error = with_band_hook(hook_failing_with_the_chosen_errno, || {
            spawn_background(&mut cmd, "adv-w4b-errno").err()
        });
        let expected = format!(
            "could not enter the background band: {}",
            std::io::Error::from_raw_os_error(errno)
        );
        match error {
            Some(error)
                if band_refusal(&error)
                    && error.to_string() == expected
                    && error.kind() == std::io::ErrorKind::Other => {}
            other => wrong.push((errno, format!("{other:?}"))),
        }
    }
    assert!(wrong.is_empty(), "misdecoded: {wrong:?}");
}

/// Real exec and chdir failures, with the real band hook in front of them, are
/// not band refusals, and keep their own errno.
#[test]
fn adv_w4b_real_spawn_errors_are_never_band_refusals() {
    use std::os::unix::fs::PermissionsExt;
    let dir = tempfile::tempdir().expect("tempdir");
    let not_executable = dir.path().join("plain-file");
    std::fs::write(&not_executable, b"#!/bin/sh\n").expect("write");
    std::fs::set_permissions(&not_executable, std::fs::Permissions::from_mode(0o644))
        .expect("chmod");
    let through_a_file = not_executable.join("child");
    let cases: Vec<(&str, Command, c_int)> = vec![
        (
            "missing program",
            Command::new("/nonexistent/adv-w4b"),
            libc::ENOENT,
        ),
        (
            "not executable",
            Command::new(&not_executable),
            libc::EACCES,
        ),
        ("directory as program", Command::new("/tmp"), libc::EACCES),
        (
            "component is a file",
            Command::new(&through_a_file),
            libc::ENOTDIR,
        ),
    ];
    for (name, mut cmd, errno) in cases {
        cmd.stdin(Stdio::null()).stdout(Stdio::null());
        let Err(error) = spawn_background(&mut cmd, name) else {
            panic!("{name}: the spawn must fail");
        };
        assert!(!band_refusal(&error), "{name}: {error:?}");
        assert_eq!(error.raw_os_error(), Some(errno), "{name}: {error:?}");
        assert!(
            !error.to_string().contains("background band"),
            "{name}: {error}"
        );
    }
}

/// The hook runs in a forked child of a process whose other threads allocate
/// and take locks all the time. Hammer that: 6 threads churn the allocator and
/// a shared mutex while this one makes 400 background spawns. A hook that took
/// a lock or allocated can hang here (the test would time out) instead of
/// finishing. Evidence about the hook's behaviour, not a proof of safety.
#[test]
fn adv_w4b_hook_survives_fork_beside_allocating_threads() {
    let stop = AtomicBool::new(false);
    let shared = Mutex::new(Vec::<Vec<u8>>::new());
    std::thread::scope(|scope| {
        for seed in 0..6usize {
            let (stop, shared) = (&stop, &shared);
            scope.spawn(move || {
                let mut n = seed;
                while !stop.load(Ordering::Relaxed) {
                    n = n.wrapping_mul(31).wrapping_add(7);
                    let block = vec![n as u8; 1 + (n % 4096)];
                    let mut held = shared.lock().unwrap_or_else(|e| e.into_inner());
                    held.push(block);
                    if held.len() > 64 {
                        held.clear();
                    }
                }
            });
        }
        for round in 0..400 {
            let mut cmd = Command::new("/usr/bin/true");
            cmd.stdin(Stdio::null())
                .stdout(Stdio::null())
                .stderr(Stdio::null());
            let (mut child, guard) =
                spawn_background(&mut cmd, "adv-w4b-hammer").unwrap_or_else(|e| {
                    stop.store(true, Ordering::Relaxed);
                    panic!("round {round}: {e}")
                });
            let status = guard.reap(|| child.wait()).expect("reap");
            assert!(status.success());
        }
        stop.store(true, Ordering::Relaxed);
    });
}

/// `spawn_background` leaves its hook on the caller's `Command`: a command
/// reused for a later plain `spawn` starts in the band too. Not reachable from
/// production today (every `run_*` builds its command once), but the function's
/// own contract does not say so.
#[test]
fn adv_w4b_the_band_hook_stays_on_a_reused_command() {
    let mut cmd = probe_command();
    cmd.stdout(Stdio::piped());
    let (mut first, first_guard) = spawn_background(&mut cmd, "adv-w4b-first").expect("spawn");
    let mut out = String::new();
    std::io::Read::read_to_string(&mut first.stdout.take().expect("stdout"), &mut out)
        .expect("read");
    first_guard.reap(|| first.wait()).expect("reap");
    assert_eq!(self_band(out.as_bytes()), 1);

    let (mut second, second_guard) = procguard::spawn(&mut cmd, "adv-w4b-plain").expect("spawn");
    let mut out = String::new();
    std::io::Read::read_to_string(&mut second.stdout.take().expect("stdout"), &mut out)
        .expect("read");
    second_guard.reap(|| second.wait()).expect("reap");
    assert_eq!(
        self_band(out.as_bytes()),
        1,
        "a plain spawn of a command that was once spawned in the band is banded"
    );
}

/// Runs only as a child of the sandbox test below.
#[test]
fn adv_w4b_probe_background_git() {
    if std::env::var_os(PROBE_GIT_VAR).is_none() {
        return;
    }
    let mut git = Command::new("git");
    git.arg("--version");
    match with_background_processes(|| run_bounded(git, "git", Duration::from_secs(30), None)) {
        Ok(run) => println!(
            "BACKGROUND-OK {}",
            String::from_utf8_lossy(&run.stdout).trim()
        ),
        Err(error) => println!("BACKGROUND-ERR {}", error.message()),
    }
    let mut git = Command::new("git");
    git.arg("--version");
    match with_admission(Admission::Interactive, || {
        run_bounded(git, "git", Duration::from_secs(30), None)
    }) {
        Ok(run) => println!(
            "INTERACTIVE-OK {}",
            String::from_utf8_lossy(&run.stdout).trim()
        ),
        Err(error) => println!("INTERACTIVE-ERR {}", error.message()),
    }
}

/// FAIL-CLOSED IMPACT. A seatbelt profile that does not grant `system-sched`
/// makes `setpriority(PRIO_DARWIN_PROCESS, 0, PRIO_DARWIN_BG)` fail with EPERM
/// (checked with `sandbox-exec -p '(version 1)(allow default)(deny
/// system-sched)'`). Before this lane a background git then ran at normal
/// priority with a warning; now every background-class command fails with the
/// band's reason, while user actions still run.
#[test]
fn adv_w4b_a_seatbelt_without_system_sched_fails_every_background_command() {
    let exe = std::env::current_exe().expect("test binary");
    use crate::procguard::LockedSpawn;
    let output = Command::new("/usr/bin/sandbox-exec")
        .args(["-p", "(version 1)(allow default)(deny system-sched)"])
        .arg(exe)
        .args(["--exact", PROBE_GIT, "--nocapture", "--test-threads=1"])
        .env(PROBE_GIT_VAR, "1")
        .output_locked()
        .expect("spawn sandbox-exec");
    let text = String::from_utf8_lossy(&output.stdout).into_owned();
    eprintln!(
        "sandboxed probe said:\n{text}\n{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(
        text.contains(
            "BACKGROUND-ERR Failed to spawn git: could not enter the background band: Operation not permitted"
        ),
        "the refusal must reach the caller with its reason:\n{text}"
    );
    assert!(
        text.contains("INTERACTIVE-OK git version"),
        "user actions are not banded and must still run:\n{text}"
    );
}
