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
//! The pollers that are reachable from a library have their own rows: `idle N
//! watches` starts N repository watch sessions (the debounce loop that woke on
//! a 200 ms timer per watch), and `idle mailbox` claims a profile as `Primary`
//! (the single-instance accept loop that polled its listener every 500 ms).
//! Their difference from the 0-session floor is the cost of that poller.
//! `idle alerts bridge` binds the hook socket and leaves it with no clients (the
//! 200 ms `wait_readable` accept loop), and `idle host-load sampler` runs the
//! sampler at its production cadence with `host_load` enabled (the 2 s tick with
//! a full process scan). `idle external_changes (hidden)` and `(visible)` run
//! the workbench commit-counter loop through `workbench::external_changes::
//! bench_seam`, which swaps only the window system for a flag the row sets.
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

    #[cfg(target_os = "macos")]
    const SHORT_TMP: &str = "/private/tmp";
    #[cfg(target_os = "linux")]
    const SHORT_TMP: &str = "/tmp";

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
                    .unwrap_or_else(|error| panic!("spawn cat: {error}", error = error.message()))
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
        let repos: Vec<String> = (0..24)
            .map(|n| {
                let dir = temp.path().join(format!("watched-{n}"));
                std::fs::create_dir_all(&dir).expect("repo dir");
                git(&dir, &["init", "-q", "-b", "main"]);
                trust_repo(&dir);
                dir.to_str().expect("utf-8").to_owned()
            })
            .collect();
        let watch_state = gitpulse_lib::watcher::WatcherState::default();
        let idle_watches = |count: usize| -> Sample {
            for path in &repos[..count] {
                gitpulse_lib::watcher::bench_seam::start_idle_watch(&watch_state, path)
                    .unwrap_or_else(|error| {
                        panic!("watch {path}: {error}", error = error.message())
                    });
            }
            // Registration, the first ignore-rule read and the first scan all
            // settle before the window opens.
            std::thread::sleep(Duration::from_millis(1500));
            let (before, started) = (counters(), Instant::now());
            std::thread::sleep(window);
            let (elapsed, after) = (started.elapsed(), counters());
            let _ = gitpulse_lib::watcher::unwatch_all(&watch_state);
            std::thread::sleep(Duration::from_millis(600));
            let secs = elapsed.as_secs_f64();
            Sample::default()
                .counter("watches", count as u64)
                .metric(NAMES.0, after.0.saturating_sub(before.0) as f64 / secs)
                .metric(NAMES.1, after.1.saturating_sub(before.1) as f64 / secs)
                .elapsed(elapsed)
        };
        // The mailbox is the macOS app's single-instance channel.
        #[cfg(target_os = "macos")]
        let profile = temp.path().join("profile");
        #[cfg(target_os = "macos")]
        let idle_mailbox = || -> Sample {
            use gitpulse_lib::desktop::instance::{claim, Claim};
            let Claim::Primary(owner) = claim(&profile, Duration::from_secs(2)) else {
                panic!("the bench profile is not claimable as Primary");
            };
            assert!(owner.mailbox_error().is_none(), "the mailbox did not open");
            std::thread::sleep(Duration::from_millis(600));
            let (before, started) = (counters(), Instant::now());
            std::thread::sleep(window);
            let (elapsed, after) = (started.elapsed(), counters());
            drop(owner);
            std::thread::sleep(Duration::from_millis(700));
            let secs = elapsed.as_secs_f64();
            Sample::default()
                .counter("mailbox", 1)
                .metric(NAMES.0, after.0.saturating_sub(before.0) as f64 / secs)
                .metric(NAMES.1, after.1.saturating_sub(before.1) as f64 / secs)
                .elapsed(elapsed)
        };
        // The alerts bridge listens under `$HOME/Library/Application Support/
        // GitPulse` (`default_config_dir`), so the row points HOME at a short
        // private directory (a unix socket path is capped near 104 bytes, and
        // `bind_private` refuses a symlinked `/tmp`, hence `/private/tmp`) for
        // the window only and puts it back.
        let idle_alerts_bridge = || -> Sample {
            let home = tempfile::Builder::new()
                .prefix("gpb")
                .tempdir_in(SHORT_TMP)
                .expect("short home");
            let saved = std::env::var_os("HOME");
            std::env::set_var("HOME", home.path());
            gitpulse_lib::alerts::bridge::apply(true);
            let status = gitpulse_lib::alerts::bridge::status();
            assert!(
                status.listening,
                "the alerts bridge did not bind: {:?}",
                status.error
            );
            std::thread::sleep(Duration::from_millis(600));
            let (before, started) = (counters(), Instant::now());
            std::thread::sleep(window);
            let (elapsed, after) = (started.elapsed(), counters());
            gitpulse_lib::alerts::bridge::apply(false);
            std::thread::sleep(Duration::from_millis(500));
            match saved {
                Some(value) => std::env::set_var("HOME", value),
                None => std::env::remove_var("HOME"),
            }
            let secs = elapsed.as_secs_f64();
            Sample::default()
                .counter("bridge", 1)
                .metric(NAMES.0, after.0.saturating_sub(before.0) as f64 / secs)
                .metric(NAMES.1, after.1.saturating_sub(before.1) as f64 / secs)
                .elapsed(elapsed)
        };
        let sampler_dir = temp.path().join("sampler");
        std::fs::create_dir_all(&sampler_dir).expect("sampler dir");
        let idle_host_load = || -> Sample {
            let handle = gitpulse_lib::sysload::start(gitpulse_lib::sysload::SamplerOptions::new(
                sampler_dir.clone(),
            ))
            .unwrap_or_else(|error| panic!("host-load sampler: {error}"));
            // The first tick (full process scan, writer election) settles first.
            std::thread::sleep(Duration::from_millis(3000));
            assert!(handle.status().is_some(), "the sampler never ticked");
            let (before, started) = (counters(), Instant::now());
            std::thread::sleep(window);
            let (elapsed, after) = (started.elapsed(), counters());
            handle.stop();
            std::thread::sleep(Duration::from_millis(300));
            let secs = elapsed.as_secs_f64();
            Sample::default()
                .counter("sampler", 1)
                .metric(NAMES.0, after.0.saturating_sub(before.0) as f64 / secs)
                .metric(NAMES.1, after.1.saturating_sub(before.1) as f64 / secs)
                .elapsed(elapsed)
        };
        // The workbench external-changes loop, run through its bench seam on a
        // private profile. Hidden: the window is off screen, so each round is
        // retention's clock and a presence check. Visible: the 1 Hz pass, two
        // `stat` calls once the profile has settled past the racy window.
        let external_dir = temp.path().join("external");
        std::fs::create_dir_all(&external_dir).expect("external dir");
        let idle_external = |visible: bool| -> Sample {
            let looped = gitpulse_lib::workbench::external_changes::bench_seam::IdleLoop::start(
                &external_dir,
                visible,
            )
            .unwrap_or_else(|error| panic!("external_changes loop: {error}"));
            // The observer's first touch of the profile, and the racy window
            // that follows it, settle before the window opens.
            std::thread::sleep(Duration::from_millis(3500));
            let (before, started, rounds) = (counters(), Instant::now(), looped.rounds());
            std::thread::sleep(window);
            let (elapsed, after, rounds) =
                (started.elapsed(), counters(), looped.rounds() - rounds);
            looped.stop();
            let secs = elapsed.as_secs_f64();
            Sample::default()
                .counter("rounds", rounds)
                .metric(NAMES.0, after.0.saturating_sub(before.0) as f64 / secs)
                .metric(NAMES.1, after.1.saturating_sub(before.1) as f64 / secs)
                .elapsed(elapsed)
        };
        let mut cases = cases;
        cases.push(
            Case::new("idle external_changes (hidden)", move || {
                idle_external(false)
            })
            .ungated(),
        );
        cases.push(
            Case::new("idle external_changes (visible)", move || {
                idle_external(true)
            })
            .ungated(),
        );
        #[cfg(target_os = "macos")]
        cases.push(Case::new("idle mailbox", idle_mailbox).ungated());
        cases.push(Case::new("idle alerts bridge", idle_alerts_bridge).ungated());
        cases.push(Case::new("idle host-load sampler", idle_host_load).ungated());
        for n in [8usize, 24] {
            cases.push(Case::new(format!("idle {n} watches"), move || idle_watches(n)).ungated());
        }
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
