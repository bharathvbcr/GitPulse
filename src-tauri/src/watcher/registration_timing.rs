//! Wall-clock measurement of repository-watcher registration, not a pass/fail
//! gate. `#[ignore]`d for the reason the `[[bench]]` targets set `test =
//! false`: a timing assertion fails on a loaded machine for reasons that have
//! nothing to do with the code.
//!
//! The question it answers: does the OS serialize watch registrations, so that
//! a startup opening many repositories at once pays one registration per
//! repository end to end? On macOS each registration is an `FSEventStreamStart`
//! (an RPC to `fseventsd`) made inside `RepoFileWatcher::watch_repo`'s single
//! `commit()`.
//!
//! Two layers, both production code:
//! - `native`: `RepoFileWatcher::watch_repo` with the same arguments
//!   `start_watch_observed` passes it (git dirs resolved outside the timer);
//! - `full`: `start_watch_inner`, the whole `cmd_watch_repo` body minus the
//!   Tauri emit closures — validation, trust check, git-dir resolution,
//!   native registration and the debounce-loop thread spawn.
//!
//! Each layer runs as one registration alone, ten serially on one thread, and
//! ten concurrently from ten threads released by one barrier. Every run gets
//! ten freshly created repositories (`git init` + one commit), built outside
//! the timer; modes are interleaved and the order rotated each round.
//!
//! Run:
//! `cargo test --manifest-path src-tauri/Cargo.toml --lib watcher_registration_timing -- --ignored --nocapture`
//! `GITPULSE_WATCH_TIMING_ROUNDS` overrides the round count (default 9).

use super::{start_watch_inner, unwatch_all, RepoFileWatcher, WatcherState};
use crate::engine::git_cli::{resolve_git_common_dir, resolve_git_dir};
use crate::test_support::git_in;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Barrier};
use std::thread;
use std::time::{Duration, Instant};
use tempfile::TempDir;

const REPOS: usize = 10;

struct Repo {
    _dir: TempDir,
    root: PathBuf,
}

fn fresh_repos(count: usize) -> Vec<Repo> {
    (0..count)
        .map(|_| {
            let dir = TempDir::new().expect("tempdir");
            git_in(dir.path(), &["init", "-b", "main"]);
            git_in(dir.path(), &["commit", "--allow-empty", "-m", "init"]);
            let root = dir.path().canonicalize().expect("canonical repo");
            Repo { _dir: dir, root }
        })
        .collect()
}

/// The arguments `start_watch_observed` hands `watch_repo` for a checkout.
fn native_args(root: &Path) -> (PathBuf, PathBuf, Option<PathBuf>) {
    let git_dir = resolve_git_dir(root).expect("git dir");
    let common = resolve_git_common_dir(root).ok();
    (git_dir, root.to_path_buf(), common)
}

fn native_register(args: &(PathBuf, PathBuf, Option<PathBuf>)) -> (RepoFileWatcher, Duration) {
    let started = Instant::now();
    let watcher = RepoFileWatcher::watch_repo(&args.0, Some(&args.1), args.2.as_deref())
        .expect("native registration");
    (watcher, started.elapsed())
}

fn full_register(state: &WatcherState, root: &Path) -> Duration {
    let started = Instant::now();
    start_watch_inner(state, root.to_string_lossy().into_owned(), |_| {})
        .expect("full registration");
    started.elapsed()
}

/// Writes a top-level file (covered by the non-recursive worktree watch) and
/// waits for an event naming it. Earlier unrelated events are skipped.
fn first_event_after_write(watcher: &RepoFileWatcher, root: &Path, tag: &str) -> Option<Duration> {
    let probe = root.join(format!("probe-{tag}"));
    let started = Instant::now();
    std::fs::write(&probe, tag).expect("write probe");
    let deadline = started + Duration::from_secs(5);
    while let Some(left) = deadline.checked_duration_since(Instant::now()) {
        match watcher.receiver.recv_timeout(left) {
            Ok(Ok(event))
                if event
                    .paths
                    .iter()
                    .any(|p| p.ends_with(probe.file_name().unwrap())) =>
            {
                return Some(started.elapsed());
            }
            Ok(_) => continue,
            Err(_) => return None,
        }
    }
    None
}

#[derive(Default)]
struct Samples {
    single: Vec<Duration>,
    serial_total: Vec<Duration>,
    serial_call: Vec<Duration>,
    concurrent_total: Vec<Duration>,
    concurrent_call: Vec<Duration>,
    first_event: Vec<Duration>,
    first_event_missed: usize,
}

#[derive(Clone, Copy, Debug)]
enum Mode {
    Single,
    Serial,
    Concurrent,
}

fn run_native(mode: Mode, samples: &mut Samples) {
    match mode {
        Mode::Single => {
            let repos = fresh_repos(1);
            let args = native_args(&repos[0].root);
            let (watcher, took) = native_register(&args);
            samples.single.push(took);
            match first_event_after_write(&watcher, &repos[0].root, "single") {
                Some(latency) => samples.first_event.push(latency),
                None => samples.first_event_missed += 1,
            }
            drop(watcher);
        }
        Mode::Serial => {
            let repos = fresh_repos(REPOS);
            let args: Vec<_> = repos.iter().map(|r| native_args(&r.root)).collect();
            let started = Instant::now();
            let mut watchers = Vec::with_capacity(REPOS);
            for a in &args {
                let (watcher, took) = native_register(a);
                samples.serial_call.push(took);
                watchers.push(watcher);
            }
            samples.serial_total.push(started.elapsed());
            drop(watchers);
        }
        Mode::Concurrent => {
            let repos = fresh_repos(REPOS);
            let args: Vec<_> = repos.iter().map(|r| native_args(&r.root)).collect();
            let gate = Arc::new(Barrier::new(REPOS + 1));
            let lanes: Vec<_> = args
                .into_iter()
                .map(|a| {
                    let gate = gate.clone();
                    thread::spawn(move || {
                        gate.wait();
                        native_register(&a)
                    })
                })
                .collect();
            gate.wait();
            let started = Instant::now();
            let results: Vec<_> = lanes.into_iter().map(|l| l.join().expect("lane")).collect();
            samples.concurrent_total.push(started.elapsed());
            samples
                .concurrent_call
                .extend(results.iter().map(|(_, took)| *took));
            drop(results);
        }
    }
}

fn run_full(mode: Mode, samples: &mut Samples) {
    let state = WatcherState::default();
    match mode {
        Mode::Single => {
            let repos = fresh_repos(1);
            samples.single.push(full_register(&state, &repos[0].root));
        }
        Mode::Serial => {
            let repos = fresh_repos(REPOS);
            let started = Instant::now();
            for r in &repos {
                samples.serial_call.push(full_register(&state, &r.root));
            }
            samples.serial_total.push(started.elapsed());
        }
        Mode::Concurrent => {
            let repos = fresh_repos(REPOS);
            let gate = Arc::new(Barrier::new(REPOS + 1));
            let calls = thread::scope(|scope| {
                let lanes: Vec<_> = repos
                    .iter()
                    .map(|r| {
                        let gate = gate.clone();
                        let state = &state;
                        let root = r.root.clone();
                        scope.spawn(move || {
                            gate.wait();
                            full_register(state, &root)
                        })
                    })
                    .collect();
                gate.wait();
                let started = Instant::now();
                let calls: Vec<_> = lanes.into_iter().map(|l| l.join().expect("lane")).collect();
                samples.concurrent_total.push(started.elapsed());
                calls
            });
            samples.concurrent_call.extend(calls);
        }
    }
    unwatch_all(&state).expect("retire watches");
}

fn ms(d: Duration) -> f64 {
    d.as_secs_f64() * 1000.0
}

fn row(label: &str, values: &[Duration]) {
    if values.is_empty() {
        println!("{label:<34} (no samples)");
        return;
    }
    let mut sorted = values.to_vec();
    sorted.sort();
    println!(
        "{label:<34} n={:<4} min={:>8.2}ms  median={:>8.2}ms  max={:>8.2}ms",
        sorted.len(),
        ms(sorted[0]),
        ms(sorted[sorted.len() / 2]),
        ms(sorted[sorted.len() - 1]),
    );
}

fn median(values: &[Duration]) -> Duration {
    let mut sorted = values.to_vec();
    sorted.sort();
    sorted[sorted.len() / 2]
}

fn report(layer: &str, s: &Samples) {
    println!("--- {layer} ---");
    row("1 registration alone", &s.single);
    row("10 serial: total wall", &s.serial_total);
    row("10 serial: per call", &s.serial_call);
    row("10 concurrent: total wall", &s.concurrent_total);
    row("10 concurrent: per call", &s.concurrent_call);
    if !s.first_event.is_empty() || s.first_event_missed > 0 {
        row("write -> first event (single)", &s.first_event);
        println!("first event missed within 5s: {}", s.first_event_missed);
    }
    if !s.single.is_empty() && !s.serial_total.is_empty() && !s.concurrent_total.is_empty() {
        let single = ms(median(&s.single));
        let serial = ms(median(&s.serial_total));
        let concurrent = ms(median(&s.concurrent_total));
        println!(
            "median ratios: serial10/single={:.2}  concurrent10/single={:.2}  concurrent10/serial10={:.2}  \
             concurrent per-call/single={:.2}",
            serial / single,
            concurrent / single,
            concurrent / serial,
            ms(median(&s.concurrent_call)) / single,
        );
    }
}

#[test]
#[ignore = "wall-clock measurement of watcher registration; run explicitly with --ignored --nocapture"]
fn watcher_registration_timing() {
    let rounds: usize = std::env::var("GITPULSE_WATCH_TIMING_ROUNDS")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(9);
    assert!(
        rounds >= 1,
        "GITPULSE_WATCH_TIMING_ROUNDS must be at least 1"
    );

    // Warm-up, discarded: first-touch costs (dyld, CoreServices, the trust
    // store) belong to process start, not to registration.
    run_native(Mode::Single, &mut Samples::default());
    run_full(Mode::Single, &mut Samples::default());

    let mut native = Samples::default();
    let mut full = Samples::default();
    let plan = [
        (true, Mode::Single),
        (true, Mode::Serial),
        (true, Mode::Concurrent),
        (false, Mode::Single),
        (false, Mode::Serial),
        (false, Mode::Concurrent),
    ];
    for round in 0..rounds {
        for step in 0..plan.len() {
            let (is_native, mode) = plan[(step + round) % plan.len()];
            if is_native {
                run_native(mode, &mut native);
            } else {
                run_full(mode, &mut full);
            }
        }
    }

    println!(
        "watcher registration timing: {} rounds, {REPOS} repositories per batch, os={}",
        rounds,
        std::env::consts::OS
    );
    report("native: RepoFileWatcher::watch_repo", &native);
    report("full: start_watch_inner (cmd_watch_repo body)", &full);
}
