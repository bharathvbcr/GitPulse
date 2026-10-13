//! The signal guard against the real process table.
//!
//! Three promises, each measured against the tool the guard imitates and never
//! by signalling anything:
//!
//! 1. **It picks the same processes `pgrep` does** for the same arguments —
//!    including the arguments macOS reads differently from what a person means,
//!    where an option after the first operand is another pattern.
//! 2. **It is safe to run many at once**, as several agents' hooks are.
//! 3. **It stays inside its deadline** on a table of this machine's size.

// The hermetic test environment: a per-process home, git config and trust
// store, rewritten before `main`. See `src/test_support/env.rs`.
#[path = "../src/test_support/env.rs"]
mod hermetic_env;
#[path = "common/marked_children.rs"]
mod marked_children;

use gitpulse_lib::hooks::signal_guard::{check_command, live_match_pids};
use gitpulse_lib::procguard::LockedSpawn;
use marked_children::{pgrep, Marked};
use std::collections::BTreeSet;
use std::time::{Duration, Instant};

fn user_id() -> String {
    // SAFETY: `getuid` has no preconditions and cannot fail.
    unsafe { libc::getuid() }.to_string()
}

/// Arguments the guard and `pgrep` are both given, with what to compare.
struct Case {
    args: Vec<String>,
    /// The result is bounded by the marker, so it must be identical. A broad
    /// case (`-v`, an option read as a pattern) is compared with a tolerance
    /// for the processes that start and exit while the two tools run.
    exact: bool,
}

fn cases(marker: &str) -> Vec<Case> {
    let uid = user_id();
    let upper = marker.to_ascii_uppercase();
    let exact = |args: &[&str]| Case {
        args: args.iter().map(|a| (*a).to_string()).collect(),
        exact: true,
    };
    let broad = |args: &[&str]| Case {
        args: args.iter().map(|a| (*a).to_string()).collect(),
        exact: false,
    };
    vec![
        exact(&[marker]),
        exact(&["-f", marker]),
        exact(&["-fx", &format!("{marker} 600")]),
        exact(&["-x", marker]),
        exact(&["-i", &upper]),
        exact(&["-n", marker]),
        exact(&["-o", marker]),
        exact(&["-u", &uid, marker]),
        exact(&["-U", &uid, "-f", marker]),
        exact(&["-u", &uid, "-fx", &format!("{marker} 600")]),
        exact(&[&format!("{marker}|nomatchzzq")]),
        exact(&["-f", &format!("^{marker} 6")]),
        exact(&["-P", &std::process::id().to_string(), marker]),
        exact(&["-a", "-f", marker]),
        // An option after the first operand is one more pattern, OR'd with it:
        // the shape of the incident, over a set this test cannot bound.
        broad(&["-f", marker, "-u", &uid]),
        broad(&[marker, "-f"]),
        broad(&["-f", "600", "-x"]),
        broad(&["-v", "-f", marker]),
        broad(&["-u", &uid]),
    ]
}

#[test]
fn the_guard_picks_exactly_the_processes_pgrep_picks() {
    let marked = Marked::spawn(12);
    let mut failures = Vec::new();
    for case in cases(&marked.marker) {
        let args: Vec<&str> = case.args.iter().map(String::as_str).collect();
        let mut last = None;
        let mut agreed = false;
        for _ in 0..6 {
            // `pgrep` lists every user's processes; only this user's can be
            // signalled, so those are the ones the guard counts.
            let mine: BTreeSet<i32> = pgrep(&["-a", "-u", &user_id()]).into_iter().collect();
            let want = pgrep(&args);
            let got = live_match_pids("pgrep", &args).expect("the guard evaluates it");
            let wanted: BTreeSet<i32> = want.iter().copied().filter(|p| mine.contains(p)).collect();
            let gotten: BTreeSet<i32> = got.iter().copied().collect();
            let difference = wanted.symmetric_difference(&gotten).count();
            // Whatever else is running, the marked children are in or out of
            // both answers together.
            let ours: BTreeSet<i32> = marked.pids().into_iter().collect();
            let marked_agree = wanted.intersection(&ours).count()
                == gotten.intersection(&ours).count()
                && wanted.intersection(&ours).eq(gotten.intersection(&ours));
            if marked_agree && (difference == 0 || (!case.exact && difference <= 4)) {
                agreed = true;
                break;
            }
            let only: Vec<String> = wanted
                .symmetric_difference(&gotten)
                .take(12)
                .map(|pid| {
                    let side = if wanted.contains(pid) {
                        "pgrep"
                    } else {
                        "guard"
                    };
                    let ps = std::process::Command::new("/bin/ps")
                        .args([
                            "-o",
                            "pid=,ppid=,state=,etime=,command=",
                            "-p",
                            &pid.to_string(),
                        ])
                        .output_locked()
                        .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
                        .unwrap_or_default();
                    format!("{side} only: {ps}")
                })
                .collect();
            last = Some((wanted.len(), gotten.len(), difference, only));
            std::thread::sleep(Duration::from_millis(100));
        }
        if !agreed {
            failures.push(format!(
                "pgrep {args:?}: (pgrep, guard, difference) = {last:?}"
            ));
        }
    }
    assert!(failures.is_empty(), "\n{}", failures.join("\n"));
}

#[test]
fn killall_by_name_picks_what_an_exact_name_match_picks() {
    // killall matches a name exactly against the process name, which is
    // `pgrep -x`; `killall -d` was read against it by hand and agrees.
    let marked = Marked::spawn(5);
    let got = live_match_pids("killall", &[&marked.marker]).expect("evaluates");
    assert_eq!(got, pgrep(&["-x", &marked.marker]));
    assert_eq!(got, marked.pids());
}

#[test]
fn pkill_reads_a_leading_signal_and_then_selects_as_pgrep_does() {
    let marked = Marked::spawn(4);
    let got = live_match_pids("pkill", &["-TERM", "-f", &marked.marker]).expect("evaluates");
    assert_eq!(got, marked.pids());
    let none = live_match_pids("pkill", &["-0", "-f", &marked.marker]);
    assert!(none.is_err(), "signal 0 sends nothing, so there is no set");
}

#[test]
fn the_decision_over_the_real_table_agrees_with_the_count() {
    let marked = Marked::spawn(30);
    let command = format!("pkill -f {}", marked.marker);
    let output = check_command(&command);
    let json = output.render().expect("a refusal renders");
    assert!(json.contains("\"deny\""), "{json}");
    assert!(
        json.contains("30 of your processes"),
        "the count is the marked children, exactly: {json}"
    );

    let few = Marked::spawn(3);
    let quiet = check_command(&format!("pkill -f {}", few.marker));
    assert!(
        quiet
            .render()
            .is_none_or(|j| !j.contains("permissionDecision")),
        "three marked children are no decision: {quiet:?}"
    );
}

#[test]
fn many_hooks_deciding_at_once_all_agree_and_none_panics() {
    let marked = Marked::spawn(30);
    let few = Marked::spawn(2);
    let heavy = format!("pkill -f {}", marked.marker);
    let light = format!("pkill -f {}", few.marker);
    let started = Instant::now();
    let threads: Vec<_> = (0..8)
        .map(|n| {
            let heavy = heavy.clone();
            let light = light.clone();
            std::thread::spawn(move || {
                let mut verdicts = Vec::new();
                for round in 0..6 {
                    let command = if (n + round) % 2 == 0 { &heavy } else { &light };
                    let rendered = check_command(command).render().unwrap_or_default();
                    verdicts.push((command == &heavy, rendered.contains("\"deny\"")));
                }
                verdicts
            })
        })
        .collect();
    for thread in threads {
        for (was_heavy, denied) in thread.join().expect("no decision panicked") {
            assert_eq!(denied, was_heavy, "every thread reaches the same verdict");
        }
    }
    assert!(
        started.elapsed() < hermetic_env::coverage_relaxed(Duration::from_secs(60)),
        "48 decisions over the real table took {:?}",
        started.elapsed()
    );
}

#[test]
fn one_decision_over_the_real_table_is_far_inside_the_hook_budget() {
    let started = Instant::now();
    let _ = check_command("pkill -f \"sleep 60\" -u nobody-here");
    assert!(
        started.elapsed() < hermetic_env::coverage_relaxed(Duration::from_secs(2)),
        "took {:?}, and the guard's own deadline is 2s",
        started.elapsed()
    );
}
