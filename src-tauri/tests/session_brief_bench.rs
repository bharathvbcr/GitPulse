//! Benchmark: `gitpulse-hook session-brief` wall time and `git` child counts,
//! before vs after asking the running app first over the collision-guard
//! mailbox.
//!
//! Ignored by default; it measures, it does not assert timing bounds (though
//! it asserts zero `git` children and `"served by: app (age ...)"` on the
//! `after` binary when the app cache is warm). Run against release hook
//! binaries:
//!
//! ```text
//! GITPULSE_BENCH_HOOK_BEFORE=/path/to/before/gitpulse-hook \
//! GITPULSE_BENCH_HOOK_AFTER=target/release/gitpulse-hook \
//! cargo test --release --manifest-path src-tauri/Cargo.toml --test session_brief_bench session_brief_cost_before_and_after -- --ignored --nocapture
//! ```

#![cfg(target_os = "macos")]

mod common;

use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::Arc;
use std::time::{Duration, Instant};
use waylode_lib::desktop::instance::{claim, Claim};
use waylode_lib::procguard::LockedSpawn;

fn env_usize(key: &str, default: usize) -> usize {
    std::env::var(key)
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(default)
}

#[derive(Default, Clone, Copy, Debug, PartialEq, Eq)]
struct GitSpawnCounts {
    worktree_list: usize,
    status: usize,
    other: usize,
}

impl GitSpawnCounts {
    fn total(self) -> usize {
        self.worktree_list + self.status + self.other
    }
}

fn load_average() -> String {
    Command::new("/usr/sbin/sysctl")
        .args(["-n", "vm.loadavg"])
        .output_locked()
        .ok()
        .and_then(|out| String::from_utf8(out.stdout).ok())
        .map(|s| s.trim().to_string())
        .unwrap_or_else(|| "unavailable".to_string())
}

fn shell_single_quote(value: &str) -> String {
    format!("'{}'", value.replace('\'', "'\\''"))
}

fn build_git_counter_shim(dir: &Path, log_path: &Path) -> std::ffi::OsString {
    use std::os::unix::fs::PermissionsExt;

    let orig_path = std::env::var_os("PATH").unwrap_or_default();
    let real_git = std::env::split_paths(&orig_path)
        .map(|entry| entry.join("git"))
        .find(|candidate| candidate.is_file())
        .expect("git executable in PATH");
    let shim_path = dir.join("git");
    let script = format!(
        "#!/bin/sh\nprintf '%s\\n' \"$*\" >> {}\nexec {} \"$@\"\n",
        shell_single_quote(&log_path.to_string_lossy()),
        shell_single_quote(&real_git.to_string_lossy()),
    );
    std::fs::write(&shim_path, script).expect("write git shim");
    std::fs::set_permissions(&shim_path, std::fs::Permissions::from_mode(0o755))
        .expect("chmod git shim");
    std::env::join_paths(
        std::iter::once(dir.to_path_buf()).chain(std::env::split_paths(&orig_path)),
    )
    .expect("join PATH with shim")
}

fn run_session_brief_sample(
    hook: &Path,
    home: &Path,
    logs: &Path,
    child_path: &std::ffi::OsStr,
    git_log: &Path,
    cwd: &Path,
) -> (Duration, GitSpawnCounts, String) {
    // Drop per-process disk family caches so every child starts cold.
    let _ = std::fs::remove_dir_all(home.join("Library/Caches"));
    std::fs::write(git_log, "").expect("truncate git shim log");

    let payload = serde_json::json!({
        "hook_event_name": "SessionStart",
        "cwd": cwd.to_string_lossy(),
        "source": "startup",
    })
    .to_string();

    let started = Instant::now();
    let mut child = Command::new(hook)
        .arg("session-brief")
        .env(waylode_lib::logging::LOG_DIR_ENV, logs)
        .env("HOME", home)
        .env("PATH", child_path)
        .env_remove("GITPULSE_SESSION_ID")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn_locked()
        .expect("spawn gitpulse-hook");
    child
        .stdin
        .take()
        .expect("stdin")
        .write_all(payload.as_bytes())
        .expect("write stdin");
    let out = child.wait_with_output().expect("wait hook");
    let elapsed = started.elapsed();
    assert_eq!(
        out.status.code(),
        Some(0),
        "hook exited {:?}: stderr={}",
        out.status.code(),
        String::from_utf8_lossy(&out.stderr)
    );

    let parsed: serde_json::Value =
        serde_json::from_slice(&out.stdout).expect("parse SessionStart JSON");
    let context = parsed["hookSpecificOutput"]["additionalContext"]
        .as_str()
        .unwrap_or("")
        .to_string();

    let log_text = std::fs::read_to_string(git_log).unwrap_or_default();
    let mut counts = GitSpawnCounts::default();
    for line in log_text.lines().filter(|l| !l.trim().is_empty()) {
        let mut words = line.split_whitespace();
        let is_worktree = words.clone().any(|w| w == "worktree");
        let is_status = words.any(|w| w == "status");
        if is_worktree {
            counts.worktree_list += 1;
        } else if is_status {
            counts.status += 1;
        } else {
            counts.other += 1;
        }
    }
    (elapsed, counts, context)
}

fn summarize_samples(label: &str, samples: &[(Duration, GitSpawnCounts)]) {
    let mut durations: Vec<Duration> = samples.iter().map(|(d, _)| *d).collect();
    durations.sort();
    let min = durations.first().copied().unwrap_or_default();
    let p50 = durations[durations.len() / 2];
    let max = durations.last().copied().unwrap_or_default();
    let n = samples.len();
    let wt: usize = samples.iter().map(|(_, c)| c.worktree_list).sum();
    let st: usize = samples.iter().map(|(_, c)| c.status).sum();
    let ot: usize = samples.iter().map(|(_, c)| c.other).sum();
    let first = samples.first().map(|(_, c)| *c).unwrap_or_default();
    let all_equal = samples.iter().all(|(_, c)| *c == first);
    println!(
        "{label:<8} n={n} wall min={:.2}ms p50={:.2}ms max={:.2}ms | git children total={} (wt_list={}, status={}, other={}) per_call_uniform={all_equal} first={:?}",
        min.as_secs_f64() * 1e3,
        p50.as_secs_f64() * 1e3,
        max.as_secs_f64() * 1e3,
        wt + st + ot,
        wt,
        st,
        ot,
        (first.worktree_list, first.status, first.other, first.total()),
    );
}

/// AC5 measurement: interleaved wall time and `git` child counts for
/// `gitpulse-hook session-brief` before vs after the ask-the-app-first change,
/// against a live app mailbox serving a 17-worktree repository.
#[test]
#[ignore = "a measurement, not a check; see the module docs"]
fn session_brief_cost_before_and_after() {
    common::unmetered_spawns();
    let before_bin = PathBuf::from(
        std::env::var_os("GITPULSE_BENCH_HOOK_BEFORE")
            .expect("GITPULSE_BENCH_HOOK_BEFORE names the pre-change gitpulse-hook binary"),
    );
    let after_bin = std::env::var_os("GITPULSE_BENCH_HOOK_AFTER")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(env!("CARGO_BIN_EXE_waylode-hook")));
    let pairs = env_usize("GITPULSE_BENCH_PAIRS", 15);
    let siblings = env_usize("GITPULSE_BENCH_SIBLINGS", 16);

    let serial = common::env::env_serial();
    let app_home = tempfile::Builder::new()
        .prefix("sb")
        .tempdir_in("/tmp")
        .unwrap();
    let app_home_path = app_home.path().canonicalize().unwrap();
    let _env = common::env::bind_env(&serial).set("HOME", &app_home_path);

    let scratch = tempfile::tempdir().unwrap();
    let logs = scratch.path().join("logs");
    std::fs::create_dir_all(&logs).unwrap();
    let shim_dir = scratch.path().join("shim");
    std::fs::create_dir_all(&shim_dir).unwrap();
    let git_log = scratch.path().join("git-spawns.log");
    let child_path = build_git_counter_shim(&shim_dir, &git_log);

    let repo_dir = scratch.path().join("repo");
    std::fs::create_dir_all(&repo_dir).unwrap();
    let git = |cwd: &Path, args: &[&str]| {
        let status = Command::new("git")
            .args(args)
            .current_dir(cwd)
            .status_locked()
            .expect("run git");
        assert!(status.success(), "git {args:?} failed in {}", cwd.display());
    };
    git(&repo_dir, &["init", "-q", "-b", "main"]);
    git(&repo_dir, &["config", "user.email", "test@example.com"]);
    git(&repo_dir, &["config", "user.name", "Test"]);
    std::fs::write(repo_dir.join("seed.txt"), "seed\n").unwrap();
    git(&repo_dir, &["add", "seed.txt"]);
    git(&repo_dir, &["commit", "-q", "-m", "init"]);

    let sib_root = scratch.path().join("siblings");
    std::fs::create_dir_all(&sib_root).unwrap();
    let mut sibling_dirs = Vec::with_capacity(siblings);
    for k in 0..siblings {
        let wt = sib_root.join(format!("sibling-{k}"));
        let branch = format!("b{k}");
        git(
            &repo_dir,
            &[
                "worktree",
                "add",
                "-q",
                "-b",
                &branch,
                &wt.to_string_lossy(),
            ],
        );
        std::fs::write(wt.join(format!("own-{k}.txt")), "dirty\n").unwrap();
        sibling_dirs.push(wt.canonicalize().unwrap());
    }
    std::fs::write(repo_dir.join("here.txt"), "dirty\n").unwrap();
    let repo = repo_dir.canonicalize().unwrap();
    let repo_str = repo.to_string_lossy().into_owned();

    let preview = waylode_lib::repository_trust::inspect(&repo_str).expect("inspect trust");
    waylode_lib::repository_trust::grant(&repo_str, &preview.identity, true)
        .expect("grant persistent trust");

    let profile = app_home_path.join("Library/Application Support/Waylode");
    let owner = match claim(&profile, Duration::from_millis(300)) {
        Claim::Primary(owner) => owner,
        _ => panic!("expected to own the profile"),
    };
    owner.attach_insights(Arc::new(waylode_lib::hooks::serve_insights));

    let expected_worktrees_line = format!(
        "worktrees: {} total, {} of {} scanned, {} with uncommitted work",
        siblings + 1,
        siblings + 1,
        siblings + 1,
        siblings + 1,
    );

    let expected_count = (siblings + 1) as u64;
    let refresh_statuses = |repo_str: &str| {
        let mut list =
            waylode_lib::engine::worktree::list_worktrees_lite(repo_str).expect("list worktrees");
        let clock = waylode_lib::engine::worktree::ScanDeadline::after(
            Instant::now(),
            Duration::from_secs(10),
        );
        let cut = waylode_lib::engine::worktree::read_dirty(&mut list, &clock);
        assert_eq!(cut, 0, "warm status scan cut {cut} worktrees");
    };
    let warm_app = |repo_str: &str| {
        for attempt in 0..8 {
            refresh_statuses(repo_str);
            match waylode_lib::hooks::serve_insights(repo_str) {
                Ok(reply) => {
                    let complete = reply["not_scanned"]
                        .as_array()
                        .is_some_and(|missing| missing.is_empty())
                        && reply["worktrees"]["dirty_unknown"].as_u64() == Some(0)
                        && reply["worktrees"]["scanned"].as_u64() == Some(expected_count);
                    let age = reply["age_ms"].as_u64().unwrap_or(u64::MAX);
                    if complete && age <= 1_600 {
                        return;
                    }
                    eprintln!(
                        "warm_app retry attempt {attempt}: age_ms={age} scanned={} dirty_unknown={} not_scanned={}",
                        reply["worktrees"]["scanned"],
                        reply["worktrees"]["dirty_unknown"],
                        reply["not_scanned"]
                    );
                    if complete {
                        std::thread::sleep(Duration::from_millis(500));
                    } else {
                        std::thread::sleep(Duration::from_millis(400));
                    }
                    continue;
                }
                Err(e) if attempt < 7 => {
                    eprintln!("warm_app retry after error (attempt {attempt}): {e}");
                }
                Err(e) => panic!("warm app cache failed: {e}"),
            }
            std::thread::sleep(Duration::from_millis(400));
        }
        panic!("app cache failed to warm all worktrees within 8 passes");
    };

    // Warm once before timing.
    warm_app(&repo_str);
    let _ = run_session_brief_sample(
        &before_bin,
        &app_home_path,
        &logs,
        &child_path,
        &git_log,
        &repo,
    );
    warm_app(&repo_str);
    let (_, linked_counts, linked_brief) = run_session_brief_sample(
        &after_bin,
        &app_home_path,
        &logs,
        &child_path,
        &git_log,
        &sibling_dirs[0],
    );
    assert_eq!(
        linked_counts,
        GitSpawnCounts::default(),
        "linked worktree cold SessionStart must spawn zero git children: {linked_counts:?}"
    );
    assert!(
        linked_brief.contains("served by: app (age ")
            && linked_brief.contains(&expected_worktrees_line),
        "unexpected linked brief: {linked_brief}"
    );

    let load_before = load_average();
    let mut before_samples = Vec::with_capacity(pairs);
    let mut after_samples = Vec::with_capacity(pairs);

    for pair in 0..pairs {
        let mut run_before = || {
            warm_app(&repo_str);
            let (elapsed, counts, brief) = run_session_brief_sample(
                &before_bin,
                &app_home_path,
                &logs,
                &child_path,
                &git_log,
                &repo,
            );
            assert!(
                !brief.contains("served by: app") && brief.contains(&expected_worktrees_line),
                "before binary unexpected brief: {brief}"
            );
            before_samples.push((elapsed, counts));
        };
        let mut run_after = || {
            warm_app(&repo_str);
            let (elapsed, counts, brief) = run_session_brief_sample(
                &after_bin,
                &app_home_path,
                &logs,
                &child_path,
                &git_log,
                &repo,
            );
            assert_eq!(
                counts,
                GitSpawnCounts::default(),
                "after binary spawned git children on warm app read: {counts:?}\nbrief:\n{brief}"
            );
            assert!(
                brief.contains("served by: app (age ") && brief.contains(&expected_worktrees_line),
                "after binary unexpected brief: {brief}"
            );
            after_samples.push((elapsed, counts));
        };
        if pair % 2 == 0 {
            run_before();
            run_after();
        } else {
            run_after();
            run_before();
        }
    }
    let load_after = load_average();

    println!(
        "\nsession-brief before vs after (worktrees={}, pairs={pairs}, interleaved)",
        siblings + 1
    );
    println!("before_bin: {}", before_bin.display());
    println!("after_bin:  {}", after_bin.display());
    println!("load average (1m 5m 15m): before={load_before} after={load_after}");
    summarize_samples("before", &before_samples);
    summarize_samples("after", &after_samples);
}
