//! Inject failures at the actual macOS filesystem-event boundary.
#![cfg(target_os = "macos")]

use gitpulse_lib::watcher::RepoFileWatcher;
use notify::{RecommendedWatcher, RecursiveMode, Watcher};
use std::fs::{self, File};
use std::process::{Child, Command, Stdio};
use std::sync::Arc;
use std::time::{Duration, Instant};

fn finish(child: &mut Child, deadline: Duration) -> std::process::ExitStatus {
    let until = Instant::now() + deadline;
    loop {
        if let Some(status) = child.try_wait().unwrap() {
            return status;
        }
        if Instant::now() >= until {
            child.kill().unwrap();
            child.wait().unwrap();
            panic!("native watcher exceeded its process deadline");
        }
        std::thread::sleep(Duration::from_millis(10));
    }
}

fn probe(mode: &str) {
    let dir = tempfile::tempdir().unwrap();
    let source = dir.path().join("fault.c");
    let library = dir.path().join("fault.dylib");
    fs::write(&source, include_str!("fixtures/fsevent_fault.c")).unwrap();
    let build_log = dir.path().join("build.log");
    let output = File::create(&build_log).unwrap();
    let define = match mode {
        "start" => "-DGITPULSE_FAULT_START",
        "create" => "-DGITPULSE_FAULT_CREATE",
        "purge" => "-DGITPULSE_FAULT_PURGE",
        "busy" => "-DGITPULSE_FAULT_BUSY",
        "source" => "-DGITPULSE_FAULT_SOURCE",
        "early" => "-DGITPULSE_FAULT_EARLY",
        "healthy" => "-DGITPULSE_HEALTHY",
        _ => panic!("unknown fault mode"),
    };
    let mut compiler = Command::new("/usr/bin/xcrun")
        .args([
            "--sdk",
            "macosx",
            "clang",
            "-dynamiclib",
            "-framework",
            "CoreServices",
            "-o",
        ])
        .arg(&library)
        .arg(&source)
        .arg(define)
        .stderr(output.try_clone().unwrap())
        .stdout(output)
        .spawn()
        .unwrap();
    assert!(
        finish(&mut compiler, Duration::from_secs(20)).success(),
        "{}",
        fs::read_to_string(build_log).unwrap()
    );
    let child_log = dir.path().join("child.log");
    let output = File::create(&child_log).unwrap();
    let mut child = Command::new(std::env::current_exe().unwrap())
        .args(["--exact", "watcher_fault_child", "--nocapture"])
        .env("GITPULSE_FSEVENT_FAULT", mode)
        .env("DYLD_INSERT_LIBRARIES", library)
        .stdin(Stdio::null())
        .stderr(output.try_clone().unwrap())
        .stdout(output)
        .spawn()
        .unwrap();
    // Each registration/retirement has the same three-second bound. The
    // source-failure case starts 17 real streams, so an aggregate three-second
    // deadline confuses successful repeated cleanup with a shutdown hang.
    let mut completed = 0;
    let mut deadline = Instant::now() + Duration::from_secs(3);
    let status = loop {
        if let Some(status) = child.try_wait().unwrap() {
            break status;
        }
        let progress = fs::read_to_string(&child_log)
            .unwrap()
            .matches("GITPULSE_WATCH_PROBE_COMPLETE")
            .count();
        if progress > completed && progress <= 17 {
            completed = progress;
            deadline = Instant::now() + Duration::from_secs(3);
        }
        if Instant::now() >= deadline {
            child.kill().unwrap();
            child.wait().unwrap();
            panic!(
                "{mode}: registration {completed} exceeded its deadline: {}",
                fs::read_to_string(&child_log).unwrap()
            );
        }
        std::thread::sleep(Duration::from_millis(10));
    };
    assert!(
        status.success(),
        "{mode}: {}",
        fs::read_to_string(&child_log).unwrap()
    );
    if mode == "source" {
        assert_eq!(
            fs::read_to_string(child_log).unwrap().matches("GITPULSE_SHUTDOWN_SOURCE_FAULT").count(),
            17,
            "the initial registration and all 16 ownership probes must inject the shutdown allocation failure"
        );
    }
}

#[test]
fn watcher_fault_child() {
    let Ok(mode) = std::env::var("GITPULSE_FSEVENT_FAULT") else {
        return;
    };
    let dir = tempfile::tempdir().unwrap();
    let result = RepoFileWatcher::watch(dir.path());
    if mode == "start" || mode == "create" || mode == "source" {
        match result {
            Err(error) => assert!(error.contains("FSEvent"), "{error}"),
            Ok(watcher) => {
                // Do not let the broken pre-fix Drop hide a false-ready result.
                // The isolated process owns the intentionally abandoned handle.
                std::mem::forget(watcher);
                panic!("native startup failure was reported as a ready watcher");
            }
        }
        eprintln!("GITPULSE_WATCH_PROBE_COMPLETE");
        // Every public registration route must propagate errors and release
        // callback ownership, including the stream/context allocated before
        // native startup. Repetition catches retained failed generations.
        let owner = Arc::new(());
        for _ in 0..16 {
            let callback_owner = Arc::clone(&owner);
            let mut watcher = RecommendedWatcher::new(
                move |_| {
                    std::hint::black_box(&callback_owner);
                },
                notify::Config::default(),
            )
            .unwrap();
            assert!(watcher.watch(dir.path(), RecursiveMode::Recursive).is_err());
            drop(watcher);
            assert_eq!(
                Arc::strong_count(&owner),
                1,
                "failed stream retained its callback"
            );
            eprintln!("GITPULSE_WATCH_PROBE_COMPLETE");
        }
    } else {
        drop(result.expect("healthy stream must start"));
    }
}

#[test]
fn native_stream_start_failure_is_an_error_without_a_shutdown_hang() {
    probe("start");
}

#[test]
fn native_stream_allocation_failure_is_an_error_without_an_abort() {
    probe("create");
}

#[test]
fn watcher_retirement_does_not_call_the_volume_history_purge_rpc() {
    probe("purge");
}

#[test]
fn healthy_native_watcher_starts_and_retires() {
    probe("healthy");
}

#[test]
fn native_shutdown_does_not_require_an_idle_runloop() {
    probe("busy");
}

#[test]
fn native_shutdown_source_allocation_failure_is_an_error() {
    probe("source");
}

#[test]
fn native_stop_before_runloop_entry_is_delivered() {
    probe("early");
}
