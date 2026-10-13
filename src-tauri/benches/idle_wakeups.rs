//! Idle wakeups per second of the GitPulse process, with N idle terminal
//! sessions.
//!
//! Run with `cargo bench --manifest-path src-tauri/Cargo.toml --bench idle_wakeups`.
//!
//! An idle app should be asleep. Every thread that wakes on a timer or a
//! blocking read that returns spuriously keeps a core out of its deepest idle
//! state, which is what a laptop user sees as battery drain and a warm chassis.
//! The audit cards `terminal-reader-kqueue` and `always-on-pollers` are about
//! exactly that, and this is their baseline.
//!
//! # What is measured, and what is not
//!
//! macOS: `proc_pid_rusage(RUSAGE_INFO_V4)` for this process, reading
//! `ri_interrupt_wkups` and `ri_pkg_idle_wkups` before and after a fixed idle
//! window (the recipe in `tmp/rusage_wakeups.py`). Linux has no equivalent
//! counter; the closest honest reading is the voluntary context switches the
//! kernel counts per task (`/proc/self/task/*/status`), reported under its own
//! name (`ctx_switches_per_s`) rather than dressed up as wakeups.
//!
//! **Per-process, not per-thread.** The kernel interface is per process, so
//! "per thread class" is derived rather than read: the process is measured with
//! 0, 1, 8 and 32 idle `cat` sessions, and the slope is the cost of one idle
//! session's reader. A thread class that is not tied to a session (the host
//! sampler, the watcher, the live-index poller) belongs to the app process and
//! is not started by this library's public API from a bench; the 0-session row
//! is this process's floor with none of them, and the report says so rather
//! than claiming them measured. They are in the installed-app qualification
//! (ft-9167572a).
//!
//! The row's time is the idle window itself, so it is not gated; the figure is
//! the `*_wkups_per_s` metric. Wakeups are tiny numbers on a quiet machine and
//! scheduler-dependent on a loaded one: read them with the host-load snapshot
//! in the JSON.

#[cfg(any(target_os = "macos", target_os = "linux"))]
mod common;

#[cfg(any(target_os = "macos", target_os = "linux"))]
mod imp {
    use super::common::fixture::{git, trust_repo};
    use super::common::{interleave, Case, Report, Sample};
    use gitpulse_lib::terminal::{
        acknowledge_output, kill_session, shutdown_sessions, spawn_session, TerminalSessions,
    };
    use std::time::{Duration, Instant};
    use tauri::Listener;

    /// (interrupt wakeups, idle wakeups) on macOS; (voluntary ctx switches, 0)
    /// on Linux.
    #[cfg(target_os = "macos")]
    fn counters() -> (u64, u64) {
        let mut info = std::mem::MaybeUninit::<libc::rusage_info_v4>::zeroed();
        // SAFETY: the buffer is one rusage_info_v4, as RUSAGE_INFO_V4 asks.
        let rc = unsafe {
            libc::proc_pid_rusage(
                std::process::id() as i32,
                libc::RUSAGE_INFO_V4,
                info.as_mut_ptr().cast(),
            )
        };
        assert_eq!(
            rc,
            0,
            "proc_pid_rusage failed: {}",
            std::io::Error::last_os_error()
        );
        // SAFETY: the call succeeded, so the struct is initialised.
        let info = unsafe { info.assume_init() };
        (info.ri_interrupt_wkups, info.ri_pkg_idle_wkups)
    }

    #[cfg(target_os = "linux")]
    fn counters() -> (u64, u64) {
        let mut total = 0;
        for task in std::fs::read_dir("/proc/self/task").expect("tasks") {
            let status = std::fs::read_to_string(task.expect("task").path().join("status"))
                .unwrap_or_default();
            total += status
                .lines()
                .find_map(|l| l.strip_prefix("voluntary_ctxt_switches:"))
                .and_then(|v| v.trim().parse::<u64>().ok())
                .unwrap_or(0);
        }
        (total, 0)
    }

    #[cfg(target_os = "macos")]
    const NAMES: (&str, &str) = ("interrupt_wkups_per_s", "pkg_idle_wkups_per_s");
    #[cfg(target_os = "linux")]
    const NAMES: (&str, &str) = ("ctx_switches_per_s", "unused");

    pub fn main() {
        gitpulse_lib::engine::git_cli::run_process_with_unlimited_spawn_rate();
        let mut report = Report::new("idle_wakeups");
        let window = if report.opts.smoke {
            Duration::from_millis(1500)
        } else {
            Duration::from_secs(5)
        };
        let app = tauri::test::mock_builder()
            .build(gitpulse_lib::context())
            .expect("mock app");
        let state = TerminalSessions::default();
        {
            let state = state.clone();
            app.listen("terminal-output", move |event| {
                let payload: serde_json::Value =
                    serde_json::from_str(event.payload()).unwrap_or_default();
                if let (Some(id), Some(bytes)) = (payload["id"].as_str(), payload["bytes"].as_u64())
                {
                    let _ = acknowledge_output(&state, id, bytes as usize);
                }
            });
        }
        let temp = tempfile::tempdir().expect("temp dir");
        let repo = temp.path().join("repo");
        std::fs::create_dir_all(&repo).expect("repo dir");
        git(&repo, &["init", "-q", "-b", "main"]);
        trust_repo(&repo);
        let repo_path = repo.to_str().expect("utf-8").to_owned();

        let idle_with = |sessions: usize| -> Sample {
            let ids: Vec<String> = (0..sessions)
                .map(|_| {
                    spawn_session(
                        app.handle(),
                        &state,
                        &repo_path,
                        24,
                        80,
                        Some("/bin/cat".into()),
                        Some(Vec::new()),
                        None,
                        gitpulse_lib::terminal::OutputSink::events(app.handle()),
                    )
                    .unwrap_or_else(|error| panic!("spawn cat: {error}"))
                    .id
                })
                .collect();
            // Let spawning settle before the window opens.
            std::thread::sleep(Duration::from_millis(600));
            let before = counters();
            let started = Instant::now();
            std::thread::sleep(window);
            let elapsed = started.elapsed();
            let after = counters();
            for id in &ids {
                let _ = kill_session(&state, id);
            }
            let secs = elapsed.as_secs_f64();
            Sample::default()
                .counter("sessions", sessions as u64)
                .metric(NAMES.0, after.0.saturating_sub(before.0) as f64 / secs)
                .metric(NAMES.1, after.1.saturating_sub(before.1) as f64 / secs)
                .elapsed(elapsed)
        };

        report.note(format!(
            "process-wide counters over a {window:?} idle window with N idle `cat` sessions; per-thread \
             attribution is not available from the kernel interface, the per-session cost is the slope"
        ));
        let cases: Vec<Case> = [0usize, 1, 8, 32]
            .into_iter()
            .map(|n| Case::new(format!("idle {n} sessions"), move || idle_with(n)).ungated())
            .collect();
        let rows = interleave(cases, report.opts.samples(3), 0);
        let wake = |name: &str| {
            rows.iter()
                .find(|r| r.name == name)
                .map(|r| r.metrics[NAMES.0])
        };
        if let (Some(floor), Some(many)) = (wake("idle 0 sessions"), wake("idle 32 sessions")) {
            report.note(format!(
                "{}: {floor:.1}/s with no sessions, {many:.1}/s with 32: about {:.2}/s per idle session",
                NAMES.0,
                (many - floor) / 32.0
            ));
        }
        report.extend(rows);
        let _ = shutdown_sessions(&state);
        report.finish();
    }
}

#[cfg(any(target_os = "macos", target_os = "linux"))]
fn main() {
    imp::main();
}

#[cfg(not(any(target_os = "macos", target_os = "linux")))]
fn main() {
    println!("no wakeup counter on this platform; nothing to measure");
}
