//! The signal guard against the real process table.
//!
//! Three promises, each measured against the tool the guard imitates and never
//! by signalling anything:
//!
//! 1. **It picks the same processes `pgrep` does** for the same arguments —
//!    including the arguments macOS reads differently from what a person means,
//!    where an option after the first operand is another pattern. On Linux the
//!    same holds where the guard and the tool read a command the same way. Where
//!    the guard deliberately counts more (an option after the first operand,
//!    which procps reads as an option and macOS as a pattern, and a name whose
//!    `argv[0]` differs from the executable's) it is held to reaching at least
//!    what `pgrep` reaches, and the difference is asserted to be there.
//! 2. **It is safe to run many at once**, as several agents' hooks are.
//! 3. **It stays inside its deadline** on a table of this machine's size.

#![cfg(unix)]

// The hermetic test environment: a per-process home, git config and trust
// store, rewritten before `main`. See `src/test_support/env.rs`.
#[path = "../src/test_support/env.rs"]
mod hermetic_env;
#[path = "common/marked_children.rs"]
mod marked_children;

use gitpulse_lib::hooks::signal_guard::{check_command, live_match_pids};
use gitpulse_lib::procguard::LockedSpawn;
use marked_children::Marked;
use std::collections::{BTreeMap, BTreeSet};
use std::time::{Duration, Instant};

/// The real `pgrep`, read-only, with exactly the arguments given. The pid is
/// the first word of each line: procps `-a` is `--list-full` and prints
/// `<pid> <command line>`, where a word of the command line (`600`) is not a
/// pid, while macOS `-a` prints bare pids.
fn pgrep(args: &[&str]) -> Vec<i32> {
    let output = std::process::Command::new("/usr/bin/pgrep")
        .args(args)
        .output_locked()
        .expect("run pgrep");
    let mut pids: Vec<i32> = String::from_utf8_lossy(&output.stdout)
        .lines()
        .filter_map(|line| line.split_whitespace().next()?.parse().ok())
        .collect();
    pids.sort_unstable();
    pids
}

#[cfg(target_os = "linux")]
use std::os::unix::process::CommandExt;
#[cfg(target_os = "linux")]
use std::process::{Child, Command, Stdio};

#[cfg(not(target_os = "linux"))]
use Marked as Sleepers;
/// The sleepers the equivalence cases run against. On macOS that is
/// [`Marked`]: `pgrep` matches `argv[0]`, which is the marker. On Linux procps
/// matches `comm`, the name of the executable file, so [`Marked`] children are
/// all `sleep` there; [`NamedSleepers`] are sleepers whose `comm` is their
/// marker, where the guard's name match and the tool's are the same set.
#[cfg(target_os = "linux")]
use NamedSleepers as Sleepers;

/// Sleepers whose kernel name is their marker: each is `/bin/sleep` run
/// through a symlink named by the marker, with the marker as `argv[0]` too.
/// The marker is at most 15 bytes so `comm` is not cut.
#[cfg(target_os = "linux")]
struct NamedSleepers {
    marker: String,
    dir: std::path::PathBuf,
    children: Vec<Child>,
}

#[cfg(target_os = "linux")]
impl NamedSleepers {
    fn spawn(count: usize) -> Self {
        static SEQUENCE: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);
        let marker = format!(
            "gp{:x}n{:x}",
            std::process::id(),
            SEQUENCE.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
        );
        assert!(marker.len() <= 15, "{marker} would be cut as a comm");
        let dir = std::env::temp_dir().join(format!("gpsg-{marker}"));
        std::fs::create_dir_all(&dir).expect("a directory for the symlink");
        let link = dir.join(&marker);
        std::os::unix::fs::symlink("/bin/sleep", &link).expect("link sleep under the marker");
        let children = (0..count)
            .map(|_| {
                Command::new(&link)
                    .arg0(&marker)
                    .arg("600")
                    .stdin(Stdio::null())
                    .stdout(Stdio::null())
                    .stderr(Stdio::null())
                    .spawn_locked()
                    .expect("spawn a named sleeper")
            })
            .collect::<Vec<_>>();
        let sleepers = Self {
            marker,
            dir,
            children,
        };
        let started = Instant::now();
        while started.elapsed() < hermetic_env::coverage_relaxed(Duration::from_secs(20)) {
            if pgrep(&["-x", &sleepers.marker]).len() == count {
                break;
            }
            std::thread::sleep(Duration::from_millis(20));
        }
        sleepers
    }

    fn pids(&self) -> Vec<i32> {
        let mut pids: Vec<i32> = self
            .children
            .iter()
            .filter_map(|c| i32::try_from(c.id()).ok())
            .collect();
        pids.sort_unstable();
        pids
    }
}

#[cfg(target_os = "linux")]
impl Drop for NamedSleepers {
    fn drop(&mut self) {
        for child in &mut self.children {
            let _ = child.kill();
            let _ = child.wait();
        }
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

fn user_id() -> String {
    // SAFETY: `getuid` has no preconditions and cannot fail.
    unsafe { libc::getuid() }.to_string()
}

/// One set of `pgrep` arguments. `late` is set when an option follows the
/// first operand: procps reads it as an option, macOS as one more pattern, and
/// on Linux the guard counts what either reading would.
struct Case {
    args: Vec<String>,
    late: bool,
}

fn cases(marker: &str) -> Vec<Case> {
    let uid = user_id();
    let upper = marker.to_ascii_uppercase();
    let case = |args: &[&str]| Case {
        args: args.iter().map(|a| (*a).to_string()).collect::<Vec<_>>(),
        late: false,
    };
    let late = |args: &[&str]| Case {
        late: true,
        ..case(args)
    };
    #[cfg_attr(not(target_os = "linux"), allow(unused_mut))]
    let mut cases = vec![
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
        // `-a` puts this process's ancestors back in the answer on macOS, and
        // they are not marked, so a guard that dropped them differs over the
        // whole table. On Linux `-a` only lists, and ancestors are always in.
        case(&["-a", "-u", &uid]),
        // An option after the first operand is one more pattern, OR'd with it:
        // the shape of the incident. These select by something other than the
        // marker, so they run over the whole table, which other sessions keep
        // changing; `unexplained` is what tells that churn from a real difference.
        late(&["-f", marker, "-u", &uid]),
        late(&[marker, "-f"]),
        late(&["-f", "600", "-x"]),
        case(&["-v", "-f", marker]),
        case(&["-u", &uid]),
    ];
    // Spellings only procps has: long options (a macOS `pgrep` has none, and
    // the guard's macOS reading calls them illegal options), a signal-free
    // `--euid`, and the empty pattern, which matches every process there.
    #[cfg(target_os = "linux")]
    cases.extend([
        case(&["--full", marker]),
        case(&["--ignore-case", "--exact", &upper]),
        case(&["--euid", &uid, "--full", marker]),
        case(&["--uid=0", "--full", marker]),
        case(&["--inverse", "--full", marker]),
        case(&["--ignore-case", &upper]),
        case(&[""]),
    ]);
    cases
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

/// Whether a case is held to `pgrep`'s exact answer. Every one is, except on
/// Linux where an option follows the first operand: procps reads it as an
/// option, macOS as one more pattern, and the guard counts what either reading
/// would signal, so there it is held to reaching at least every process
/// `pgrep` reaches, the direction that can never let a signal through unasked.
fn exact(case: &Case) -> bool {
    !(cfg!(target_os = "linux") && case.late)
}

#[test]
fn the_guard_picks_exactly_the_processes_pgrep_picks() {
    let marked = Sleepers::spawn(12);
    let ours: BTreeSet<i32> = marked.pids().into_iter().collect();
    let own = i32::try_from(std::process::id()).expect("a pid fits an i32");
    let mut failures = Vec::new();
    for case in cases(&marked.marker) {
        let exact = exact(&case);
        let argv: Vec<&str> = case.args.iter().map(String::as_str).collect();
        // The guard never returns its own pid. macOS `pgrep` leaves out its
        // ancestors, this process among them, except with `-v` or `-a`; procps
        // leaves out only itself, so this process is in its answer whenever it
        // matches, in every case.
        let models_self =
            cfg!(target_os = "linux") || argv.iter().any(|a| *a == "-v" || *a == "-a");
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
        let marked_agree = if exact {
            wanted.intersection(&ours).eq(gotten.intersection(&ours))
        } else {
            wanted.intersection(&ours).all(|pid| gotten.contains(pid))
        };
        if !exact {
            differ.retain(|(_, side)| *side == "pgrep");
        }
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
    // `pgrep -x`: macOS `killall -d` was read against it by hand and agrees,
    // and on Linux psmisc and procps both read `comm`, which for these
    // sleepers is the marker.
    let marked = Sleepers::spawn(5);
    let got = live_match_pids("killall", &[&marked.marker]).expect("evaluates");
    assert_eq!(got, pgrep(&["-x", &marked.marker]));
    assert_eq!(got, marked.pids());
}

/// Linux procps and psmisc match a name against `comm`, the executable's
/// name, never `argv[0]`. These sleepers are `sleep` to them and the marker to
/// the guard's macOS reading; the guard tries both, so what `pgrep` finds by
/// the executable's name (the login shell `-zsh` is `zsh` to it, the same
/// shape) is found by the guard, and what only `argv[0]` names is counted
/// beyond `pgrep`: the direction that never lets a signal through unasked.
#[cfg(target_os = "linux")]
#[test]
fn a_process_whose_argv0_differs_from_its_executable_is_counted_by_either_name() {
    let marked = Marked::spawn(6);
    let parent = std::process::id().to_string();
    let mine: BTreeSet<i32> = marked.pids().into_iter().collect();
    // Both tools over the children of this process, which is where the
    // sleepers are (other tests' sleepers are too, all `sleep`).
    let ask = |name: &[&str]| -> (BTreeSet<i32>, BTreeSet<i32>) {
        let mut args: Vec<&str> = name.to_vec();
        args.extend(["-P", &parent]);
        let wanted = pgrep(&args).into_iter().collect();
        let got = live_match_pids("pgrep", &args)
            .expect("the guard evaluates it")
            .into_iter()
            .collect();
        (wanted, got)
    };
    // By the executable's name: `pgrep -x sleep` finds all of them, and so
    // must the guard, whose macOS reading looked at `argv[0]` only and saw none.
    let (wanted, got) = ask(&["-x", "sleep"]);
    assert!(mine.is_subset(&wanted), "pgrep -x sleep: {wanted:?}");
    assert!(
        wanted.is_subset(&got),
        "pgrep -x sleep reached {wanted:?}, the guard only {got:?}"
    );
    // By `argv[0]`: procps finds none, the guard counts them all.
    let (wanted, got) = ask(&["-x", &marked.marker]);
    assert!(wanted.is_empty(), "procps matched argv[0]: {wanted:?}");
    assert_eq!(got, mine, "the guard also tries argv[0]");
}

/// A process that is not dumpable (`ssh-agent` marks itself so, set-uid
/// programs are, `prctl(PR_SET_DUMPABLE, 0)` does it by hand) is still this
/// user's: `pgrep` reads its uid from `/proc/<pid>/status` and so does the
/// guard. (The kernel hands the files inside `/proc/<pid>` to root for such a
/// process but keeps the directory itself the effective uid's, `task_dump_owner`
/// in `fs/proc/base.c`, so this does not fail on the owner-of-`/proc/<pid>`
/// read the table used; it pins the behaviour.) The child is this test binary
/// again, running only the test below that marks itself.
#[cfg(target_os = "linux")]
const NON_DUMPABLE_CHILD: &str = "GITPULSE_TEST_NON_DUMPABLE_CHILD";

#[cfg(target_os = "linux")]
#[test]
fn non_dumpable_child_body() {
    if std::env::var_os(NON_DUMPABLE_CHILD).is_none() {
        return;
    }
    // SAFETY: `prctl(PR_SET_DUMPABLE, 0)` takes no pointers and changes only
    // this process's dumpable flag.
    let rc = unsafe { libc::prctl(libc::PR_SET_DUMPABLE, 0, 0, 0, 0) };
    assert_eq!(rc, 0, "prctl(PR_SET_DUMPABLE, 0)");
    std::thread::sleep(Duration::from_secs(600));
}

#[cfg(target_os = "linux")]
struct NonDumpable(Child);

#[cfg(target_os = "linux")]
impl Drop for NonDumpable {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

#[cfg(target_os = "linux")]
#[test]
fn a_not_dumpable_process_of_this_user_is_counted() {
    // SAFETY: `getuid` has no preconditions and cannot fail.
    let uid = unsafe { libc::getuid() };
    if uid == 0 {
        // Root can read every process's files whether or not it is dumpable.
        eprintln!("skipped: running as root, where dumpable changes nothing visible");
        return;
    }
    let child = NonDumpable(
        std::process::Command::new(std::env::current_exe().expect("this test binary"))
            .args(["--exact", "non_dumpable_child_body", "--test-threads=1"])
            .env(NON_DUMPABLE_CHILD, "1")
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn_locked()
            .expect("start the not-dumpable child"),
    );
    let pid = i32::try_from(child.0.id()).expect("a pid fits an i32");
    // The child is not dumpable once its own environment is closed to its
    // own user (the kernel's ptrace check on `/proc/<pid>/environ`).
    let started = Instant::now();
    loop {
        let environ = std::fs::read(format!("/proc/{pid}/environ"));
        if environ
            .as_ref()
            .is_err_and(|e| e.kind() == std::io::ErrorKind::PermissionDenied)
        {
            break;
        }
        assert!(
            started.elapsed() < hermetic_env::coverage_relaxed(Duration::from_secs(30)),
            "the child never became not dumpable: environ read {:?}",
            environ.map(|e| e.len())
        );
        std::thread::sleep(Duration::from_millis(20));
    }
    // Its real, effective and saved uids are still this user's.
    let status = std::fs::read_to_string(format!("/proc/{pid}/status")).expect("status");
    let ids = status
        .lines()
        .find_map(|l| l.strip_prefix("Uid:"))
        .map(|l| l.split_whitespace().collect::<Vec<_>>().join(" "));
    assert!(
        ids.as_deref()
            .is_some_and(|ids| ids.starts_with(&format!("{uid} {uid} {uid}"))),
        "{ids:?}"
    );
    let own = std::process::id().to_string();
    let uid_text = uid.to_string();
    for args in [vec!["-P", &own], vec!["-u", &uid_text, "-P", &own]] {
        let wanted = pgrep(&args);
        assert!(wanted.contains(&pid), "pgrep {args:?}: {wanted:?}");
        let got = live_match_pids("pgrep", &args).expect("the guard evaluates it");
        assert!(
            got.contains(&pid),
            "pgrep {args:?} reaches the not-dumpable child {pid}; the guard counted {got:?}"
        );
        assert_eq!(got, wanted, "pgrep {args:?}");
    }
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
