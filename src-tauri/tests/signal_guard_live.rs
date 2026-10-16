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

fn cases(marker: &str) -> Vec<Vec<String>> {
    let uid = user_id();
    let upper = marker.to_ascii_uppercase();
    let case = |args: &[&str]| args.iter().map(|a| (*a).to_string()).collect::<Vec<_>>();
    vec![
        case(&[marker]),
        case(&["-f", marker]),
        case(&["-fx", &format!("{marker} 600")]),
        case(&["-x", marker]),
        // `-x` anchors both ends, so a part of a name or of a command line
        // selects nothing, where an unanchored match would select every child.
        case(&["-x", &marker[..marker.len() - 1]]),
        case(&["-fx", marker]),
        case(&["-i", &upper]),
        case(&["-n", marker]),
        case(&["-o", marker]),
        case(&["-u", &uid, marker]),
        case(&["-U", &uid, "-f", marker]),
        case(&["-u", &uid, "-fx", &format!("{marker} 600")]),
        case(&[&format!("{marker}|nomatchzzq")]),
        case(&["-f", &format!("^{marker} 6")]),
        case(&["-P", &std::process::id().to_string(), marker]),
        case(&["-a", "-f", marker]),
        // An option after the first operand is one more pattern, OR'd with it:
        // the shape of the incident. These select by something other than the
        // marker, so they run over the whole table, which other sessions keep
        // changing; `explain` is what tells that churn from a real difference.
        case(&["-f", marker, "-u", &uid]),
        case(&[marker, "-f"]),
        case(&["-f", "600", "-x"]),
        case(&["-v", "-f", marker]),
        case(&["-u", &uid]),
    ]
}

/// This user's processes, ancestors included: the ones the guard counts.
fn mine() -> BTreeSet<i32> {
    pgrep(&["-a", "-u", &user_id()]).into_iter().collect()
}

/// Processes on which the guard and `pgrep` differ for a reason that is not a
/// disagreement about matching.
///
/// A pid is excused only when it was not in the process table at both ends of
/// the comparison (`before` and `after`): a process born or gone in between can
/// legitimately be in one answer and not the other. One alive throughout must
/// be in both, so a difference over it is returned. The one other excuse is the
/// modelled one: with `-v` or `-a` the guard leaves out only itself while
/// `pgrep` leaves out only `pgrep`, so the process running the guard (`own`)
/// is in `pgrep`'s answer alone.
fn unexplained(
    wanted: &BTreeSet<i32>,
    gotten: &BTreeSet<i32>,
    before: &BTreeSet<i32>,
    after: &BTreeSet<i32>,
    own: Option<i32>,
) -> Vec<(i32, &'static str)> {
    wanted
        .symmetric_difference(gotten)
        .filter(|pid| before.contains(pid) && after.contains(pid) && Some(**pid) != own)
        .map(|pid| {
            (
                *pid,
                if wanted.contains(pid) {
                    "pgrep"
                } else {
                    "guard"
                },
            )
        })
        .collect()
}

fn describe(pid: i32) -> String {
    std::process::Command::new("/bin/ps")
        .args([
            "-o",
            "pid=,ppid=,state=,etime=,command=",
            "-p",
            &pid.to_string(),
        ])
        .output_locked()
        .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
        .unwrap_or_default()
}

#[test]
fn the_guard_picks_exactly_the_processes_pgrep_picks() {
    let marked = Marked::spawn(12);
    let ours: BTreeSet<i32> = marked.pids().into_iter().collect();
    let own = i32::try_from(std::process::id()).expect("a pid fits an i32");
    let mut failures = Vec::new();
    for args in cases(&marked.marker) {
        let argv: Vec<&str> = args.iter().map(String::as_str).collect();
        // `pgrep` lists every user's processes; only this user's can be
        // signalled, so those are the ones the guard counts.
        let before = mine();
        let want = pgrep(&argv);
        let got = live_match_pids("pgrep", &argv).expect("the guard evaluates it");
        let after = mine();
        let wanted: BTreeSet<i32> = want
            .iter()
            .copied()
            .filter(|p| before.contains(p) || after.contains(p))
            .collect();
        let gotten: BTreeSet<i32> = got.iter().copied().collect();
        let models_self = argv.iter().any(|a| *a == "-v" || *a == "-a");
        let differ = unexplained(
            &wanted,
            &gotten,
            &before,
            &after,
            models_self.then_some(own),
        );
        // The marked children live throughout, so they are in or out of both
        // answers together, whatever else is running.
        let marked_agree = wanted.intersection(&ours).eq(gotten.intersection(&ours));
        if !marked_agree || !differ.is_empty() {
            let only: Vec<String> = differ
                .iter()
                .take(12)
                .map(|(pid, side)| format!("{side} only: {}", describe(*pid)))
                .collect();
            failures.push(format!(
                "pgrep {argv:?}: marked agree {marked_agree}, (pgrep, guard) = ({}, {}), \
                 unexplained {only:?}",
                wanted.len(),
                gotten.len()
            ));
        }
    }
    assert!(failures.is_empty(), "\n{}", failures.join("\n"));
}

#[test]
fn churn_between_the_snapshots_is_excused_and_a_stable_disagreement_is_not() {
    let set = |pids: &[i32]| pids.iter().copied().collect::<BTreeSet<i32>>();
    let table = set(&[1, 2, 3, 4]);
    // Born after the first table, gone before the second, in one answer only.
    let born = set(&[1, 2, 3, 4, 5]);
    assert!(unexplained(&born, &table, &table, &born, None).is_empty());
    assert!(unexplained(&table, &born, &born, &table, None).is_empty());
    // Alive at both ends and in one answer only: a difference about matching,
    // from either side.
    assert_eq!(
        unexplained(&set(&[1, 2, 3]), &set(&[1, 2]), &table, &table, None),
        vec![(3, "pgrep")]
    );
    assert_eq!(
        unexplained(&set(&[1, 2]), &set(&[1, 2, 4]), &table, &table, None),
        vec![(4, "guard")]
    );
    // The modelled difference excuses the running process and nothing else.
    assert!(unexplained(&set(&[1, 2]), &set(&[1]), &table, &table, Some(2)).is_empty());
    assert_eq!(
        unexplained(&set(&[1, 2, 3]), &set(&[1]), &table, &table, Some(2)),
        vec![(3, "pgrep")]
    );
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
