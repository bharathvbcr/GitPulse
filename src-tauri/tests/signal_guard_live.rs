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
use std::collections::{BTreeMap, BTreeSet};
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
        // Another user's processes are not this user's to signal, so these
        // select nothing the guard counts; a guard that ignored the user
        // filter would select the marked children (and, without a marker,
        // every process of this user).
        case(&["-u", "0", marker]),
        case(&["-U", "0", "-f", marker]),
        case(&["-u", "0"]),
        case(&["-U", &uid, "-f", marker]),
        case(&["-u", &uid, "-fx", &format!("{marker} 600")]),
        case(&[&format!("{marker}|nomatchzzq")]),
        case(&["-f", &format!("^{marker} 6")]),
        case(&["-P", &std::process::id().to_string(), marker]),
        case(&["-a", "-f", marker]),
        // `-a` puts this process's ancestors back in the answer, and they are
        // not marked, so a guard that dropped them differs over the whole table.
        case(&["-a", "-u", &uid]),
        // An option after the first operand is one more pattern, OR'd with it:
        // the shape of the incident. These select by something other than the
        // marker, so they run over the whole table, which other sessions keep
        // changing; `unexplained` is what tells that churn from a real difference.
        case(&["-f", marker, "-u", &uid]),
        case(&[marker, "-f"]),
        case(&["-f", "600", "-x"]),
        case(&["-v", "-f", marker]),
        case(&["-u", &uid]),
    ]
}

/// What identifies a process across two reads of the table: when it started
/// and the text a matcher reads (its name and its argument list). A pid that
/// was reused starts at another time; one that called `exec` or rewrote its
/// own argument list in place (`setproctitle`) reads differently.
type Identity = (String, String);

/// Every process that was running, by pid.
type Snapshot = BTreeMap<i32, Identity>;

/// The table as `ps` prints it. `pids` limits it to those processes; `None`
/// takes every process of this user, which is what the guard counts.
fn snapshot(pids: Option<&[i32]>) -> Snapshot {
    let mut ps = std::process::Command::new("/bin/ps");
    ps.args(["-ww", "-o", "pid=,uid=,lstart=,ucomm=,command="]);
    match pids {
        Some(pids) => {
            let list: Vec<String> = pids.iter().map(i32::to_string).collect();
            ps.args(["-p", &list.join(",")])
        }
        None => ps.arg("-ax"),
    };
    let output = ps.output_locked().expect("run ps");
    let me = user_id();
    String::from_utf8_lossy(&output.stdout)
        .lines()
        .filter_map(|line| {
            let mut fields = line.split_whitespace();
            let pid = fields.next()?.parse::<i32>().ok()?;
            let uid = fields.next()?;
            // `lstart` is five words: weekday, month, day, time, year.
            let started = fields.by_ref().take(5).collect::<Vec<_>>().join(" ");
            let text = fields.collect::<Vec<_>>().join(" ");
            (uid == me).then_some((pid, (started, text)))
        })
        .collect()
}

/// Processes on which the guard and `pgrep` differ for a reason that is not a
/// disagreement about matching.
///
/// Two things excuse a difference over a pid. It was not the same process at
/// both ends of the comparison: absent from `before` or `after`, or started at
/// another time, or reading differently (an `exec` or a rewritten argument
/// list in between). `pgrep` read the table at one moment and the guard at
/// another, so either answer may be right for the state it saw. And the
/// modelled one: with `-v` or `-a` the guard leaves out only itself while
/// `pgrep` leaves out only `pgrep`, so the process running the guard (`own`) is
/// in `pgrep`'s answer alone. A process that was the same throughout must be in
/// both answers, so a difference over it is returned.
fn unexplained(
    wanted: &BTreeSet<i32>,
    gotten: &BTreeSet<i32>,
    before: &Snapshot,
    after: &Snapshot,
    own: Option<i32>,
) -> Vec<(i32, &'static str)> {
    wanted
        .symmetric_difference(gotten)
        .filter(|pid| Some(**pid) != own)
        .filter(|pid| matches!((before.get(pid), after.get(pid)), (Some(a), Some(b)) if a == b))
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

/// How many times a disputed pid is read again to see whether it holds still.
const SAMPLES: usize = 16;

/// The disputed pids that survive a second, targeted ask.
///
/// `disputed` are the pids `unexplained` returned. Both tools are asked again
/// at once about the whole case and only those pids are read from the answers.
/// `reads` are the identities the pids had, in time order, before, after and
/// again right after the second ask; a pid that is gone from any of them, or
/// reads differently in any of them, was not one process throughout (it can
/// flip faster than two reads can tell, so the later reads are many). It is
/// excused then. It is also excused when the two tools now agree about it while
/// it read the same in every read: a read that raced something the table does
/// not show (a process mid-`exec` can be missing from one `pgrep` pass), the
/// only case in which the disagreement is not repeated. A genuine disagreement
/// is a function of the process's fixed identity and the guard's fixed rules,
/// so it is reproduced on the very next ask over a process that reads the same
/// every time, and this returns it. Nothing is asked again except once, and
/// nothing is read except the pids already in dispute.
fn persistent(
    disputed: &[(i32, &'static str)],
    reads: &[&Snapshot],
    wanted_now: &BTreeSet<i32>,
    gotten_now: &BTreeSet<i32>,
) -> Vec<(i32, &'static str)> {
    disputed
        .iter()
        .copied()
        .filter(|(pid, _)| {
            let first = reads.first().and_then(|read| read.get(pid));
            let same = first.is_some() && reads.iter().all(|read| read.get(pid) == first);
            same && wanted_now.contains(pid) != gotten_now.contains(pid)
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

/// Both tools' answers over this user's processes, with the pids that are not
/// this user's dropped from `pgrep`'s.
fn ask(argv: &[&str], known: &Snapshot) -> (BTreeSet<i32>, BTreeSet<i32>) {
    let wanted = pgrep(argv)
        .into_iter()
        .filter(|p| known.contains_key(p))
        .collect();
    let got = live_match_pids("pgrep", argv).expect("the guard evaluates it");
    (wanted, got.into_iter().collect())
}

#[test]
fn the_guard_picks_exactly_the_processes_pgrep_picks() {
    let marked = Marked::spawn(12);
    let ours: BTreeSet<i32> = marked.pids().into_iter().collect();
    let own = i32::try_from(std::process::id()).expect("a pid fits an i32");
    let mut failures = Vec::new();
    for args in cases(&marked.marker) {
        let argv: Vec<&str> = args.iter().map(String::as_str).collect();
        let models_self = argv.iter().any(|a| *a == "-v" || *a == "-a");
        let own_pid = models_self.then_some(own);
        // `pgrep` lists every user's processes; only this user's can be
        // signalled, so those are the ones the guard counts.
        let before = snapshot(None);
        let (mut wanted, mut gotten) = ask(&argv, &before);
        let after = snapshot(None);
        let mut differ = unexplained(&wanted, &gotten, &before, &after, own_pid);
        let first = differ.clone();
        if !differ.is_empty() {
            let pids: Vec<i32> = differ.iter().map(|(pid, _)| *pid).collect();
            let (wanted_now, gotten_now) = ask(&argv, &before);
            let samples: Vec<Snapshot> = (0..SAMPLES).map(|_| snapshot(Some(&pids))).collect();
            let reads: Vec<&Snapshot> = [&before, &after].into_iter().chain(&samples).collect();
            differ = persistent(&differ, &reads, &wanted_now, &gotten_now);
            (wanted, gotten) = (wanted_now, gotten_now);
        }
        // The marked children live throughout, so they are in or out of both
        // answers together, whatever else is running.
        let marked_agree = wanted.intersection(&ours).eq(gotten.intersection(&ours));
        // `unexplained` excuses the running process either way, so a guard that
        // stopped leaving itself out of `-v`/`-a` would agree with `pgrep` and
        // go unseen; the modelled behaviour is that it is never in the answer.
        let leaks_self = models_self && gotten.contains(&own);
        if leaks_self {
            failures.push(format!(
                "pgrep {argv:?}: the guard returned its own pid {own}"
            ));
        }
        if !marked_agree || !differ.is_empty() {
            let only: Vec<String> = differ
                .iter()
                .take(12)
                .map(|(pid, side)| format!("{side} only: {}", describe(*pid)))
                .collect();
            failures.push(format!(
                "pgrep {argv:?}: marked agree {marked_agree}, (pgrep, guard) = ({}, {}), \
                 disputed first {first:?}, still unexplained {only:?}",
                wanted.len(),
                gotten.len()
            ));
        }
    }
    assert!(failures.is_empty(), "\n{}", failures.join("\n"));
}

fn identity(started: &str, text: &str) -> Identity {
    (started.to_string(), text.to_string())
}

#[test]
fn churn_between_the_snapshots_is_excused_and_a_stable_disagreement_is_not() {
    let set = |pids: &[i32]| pids.iter().copied().collect::<BTreeSet<i32>>();
    let table = |pids: &[i32]| {
        pids.iter()
            .map(|p| (*p, identity("t0", &format!("proc{p} 600"))))
            .collect::<Snapshot>()
    };
    let stable = table(&[1, 2, 3, 4]);
    // Born after the first table, gone before the second, in one answer only.
    let born = table(&[1, 2, 3, 4, 5]);
    assert!(unexplained(
        &set(&[1, 2, 3, 4, 5]),
        &set(&[1, 2, 3, 4]),
        &stable,
        &born,
        None
    )
    .is_empty());
    assert!(unexplained(
        &set(&[1, 2, 3, 4]),
        &set(&[1, 2, 3, 4, 5]),
        &born,
        &stable,
        None
    )
    .is_empty());
    // The same process at both ends and in one answer only: a difference about
    // matching, from either side.
    assert_eq!(
        unexplained(&set(&[1, 2, 3]), &set(&[1, 2]), &stable, &stable, None),
        vec![(3, "pgrep")]
    );
    assert_eq!(
        unexplained(&set(&[1, 2]), &set(&[1, 2, 4]), &stable, &stable, None),
        vec![(4, "guard")]
    );
    // The modelled difference excuses the running process and nothing else.
    assert!(unexplained(&set(&[1, 2]), &set(&[1]), &stable, &stable, Some(2)).is_empty());
    assert_eq!(
        unexplained(&set(&[1, 2, 3]), &set(&[1]), &stable, &stable, Some(2)),
        vec![(3, "pgrep")]
    );
    // An exec, an argument list rewritten in place, and a reused pid all change
    // what a pid reads as between the tables, and so excuse a difference.
    let mut changed = stable.clone();
    changed.insert(3, identity("t0", "proc3"));
    changed.insert(4, identity("t1", "proc4 600"));
    assert!(unexplained(&set(&[1, 2, 3, 4]), &set(&[1, 2]), &stable, &changed, None).is_empty());
    // ...and does not excuse a process that read the same.
    assert_eq!(
        unexplained(&set(&[1, 2, 3, 4]), &set(&[1]), &stable, &changed, None),
        vec![(2, "pgrep")]
    );
}

#[test]
fn a_disagreement_that_is_asked_again_is_excused_only_if_it_does_not_repeat() {
    let set = |pids: &[i32]| pids.iter().copied().collect::<BTreeSet<i32>>();
    let one = |p: i32, text: &str| {
        [(p, identity("t0", text))]
            .into_iter()
            .collect::<Snapshot>()
    };
    let was = one(7, "worker 600");
    let disputed = [(7, "pgrep")];
    // Still in dispute and still the same process: a real disagreement.
    let steady = [&was, &was, &was, &was];
    assert_eq!(
        persistent(&disputed, &steady, &set(&[7]), &set(&[])),
        vec![(7, "pgrep")]
    );
    assert_eq!(
        persistent(&[(7, "guard")], &steady, &set(&[]), &set(&[7])),
        vec![(7, "guard")]
    );
    // The tools now agree about the unchanged process: the read raced.
    assert!(persistent(&disputed, &steady, &set(&[7]), &set(&[7])).is_empty());
    assert!(persistent(&disputed, &steady, &set(&[]), &set(&[])).is_empty());
    // It has gone, or it read differently in any one read, even one between
    // two that agree (a title flipping back and forth): not one process.
    let gone = Snapshot::new();
    let other = one(7, "worker");
    for odd in [&gone, &other] {
        for at in 0..4 {
            let mut reads = steady;
            reads[at] = odd;
            assert!(
                persistent(&disputed, &reads, &set(&[7]), &set(&[])).is_empty(),
                "read {at} differs"
            );
        }
    }
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
