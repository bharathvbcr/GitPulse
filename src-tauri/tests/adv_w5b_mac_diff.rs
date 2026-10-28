//! W5b adversary: macOS (`Dialect::Bsd`) old/new differential for the signal
//! guard (ft-1d9f44d5). The hard rule is that no command line the guard asked
//! about or refused on 1ba087a0 may become quieter. This file is run, unchanged,
//! against a build of 1ba087a0 and a build of the lane; it prints one line per
//! command (`verdict`) and per pattern (`set`), and `tmp/build-1026/w5b-adv/
//! macdiff.py` compares the two runs. It reads the process table and signals
//! nothing. The `set` lines compare the guard's pid set with the real
//! `/usr/bin/pgrep` on the same host.

#![cfg(target_os = "macos")]

mod common;

use gitpulse_lib::hooks::signal_guard::{check_command, live_match_pids};
use gitpulse_lib::hooks::PermissionDecision;
use gitpulse_lib::procguard::LockedSpawn;
use std::collections::BTreeSet;
use std::process::{Command, Stdio};

const LINES: &str = include_str!("fixtures/adv_w5b_lines.txt");
const SHAPES: &str = include_str!("fixtures/adv_w5b_shapes.txt");

fn kind_of(command: &str) -> &'static str {
    let out = check_command(command);
    match out
        .hook_specific_output
        .as_ref()
        .and_then(|s| s.permission_decision)
    {
        Some(PermissionDecision::Deny) => "Deny",
        Some(PermissionDecision::Ask) => "Ask",
        None if out.system_message.is_some() => "Notice",
        None => "Silent",
    }
}

#[test]
#[ignore = "prints the guard's verdicts for a differential; reads the process table"]
fn mac_verdicts_for_every_line() {
    common::unmetered_spawns();
    let patterns = [
        "claude",
        "node",
        "zsh",
        "login",
        "Terminal",
        "launchd",
        "sleep",
        "mds",
        "a.b",
        ".",
        ".*",
        "\\d",
        "\\w+",
        "[[:alpha:]]+d$",
        "(zsh|bash)",
        "com\\.apple",
        "^/usr",
        "[a-z]",
        "x{2}",
        "a{",
    ];
    let mut total = 0;
    for line in LINES
        .lines()
        .chain(SHAPES.lines())
        .filter(|l| !l.is_empty())
    {
        total += 1;
        eprintln!("verdict [mac] `{}`: {}", line.escape_debug(), kind_of(line));
    }
    for pattern in patterns {
        for flags in [
            vec!["-f"],
            vec![],
            vec!["-x"],
            vec!["-i"],
            vec!["-fi"],
            vec!["-v"],
        ] {
            let mut args = flags.clone();
            args.push("--");
            args.push(pattern);
            let command = format!("pkill {} -- '{pattern}'", flags.join(" "));
            total += 1;
            eprintln!(
                "verdict [mac-pattern] `{}`: {}",
                command.escape_debug(),
                kind_of(&command)
            );
            let real = Command::new("/usr/bin/pgrep")
                .args(&args)
                .stdin(Stdio::null())
                .output_locked()
                .expect("run the real pgrep");
            let real: BTreeSet<i32> = String::from_utf8_lossy(&real.stdout)
                .split_whitespace()
                .filter_map(|w| w.parse().ok())
                .collect();
            let guard = live_match_pids("pgrep", &args);
            match guard {
                Ok(set) => {
                    let set: BTreeSet<i32> = set.into_iter().collect();
                    let missing = real.difference(&set).count();
                    let extra = set.difference(&real).count();
                    eprintln!(
                        "set [mac] `{}`: real {} guard {} missing {missing} extra {extra}",
                        command.escape_debug(),
                        real.len(),
                        set.len()
                    );
                }
                Err(why) => eprintln!(
                    "set [mac] `{}`: real {} guard Err({})",
                    command.escape_debug(),
                    real.len(),
                    why.chars().take(70).collect::<String>().escape_debug()
                ),
            }
        }
    }
    eprintln!("mac: {total} commands");
}

/// Starts `count` sleeping processes whose `argv[0]` is `name-<i>`; returns their
/// pids. macOS `pgrep` names a process by `argv[0]`, so these are `w5bmac*`.
fn sleepers(name: &str, count: usize) -> Vec<i32> {
    (0..count)
        .map(|i| {
            let child = Command::new("/bin/bash")
                .arg("-c")
                .arg("exec -a \"$0\" /bin/sleep 300")
                .arg(format!("{name}-{i}"))
                .stdin(Stdio::null())
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .spawn_locked()
                .expect("start a sleeper");
            let pid = i32::try_from(child.id()).expect("pid");
            std::mem::forget(child);
            pid
        })
        .collect()
}

/// The host's own process table moves between two runs, so a count-dependent
/// verdict (a pattern that reaches more than 25 processes) flickers between a
/// build and itself. Here the table is the host's plus 12 + 30 processes the
/// test starts, and the patterns are built on their names, so each verdict is
/// the same on every run of one build and a difference between two builds is
/// the guard's. The guard's pid set is compared with the real `pgrep`'s.
#[test]
#[ignore = "starts 42 sleeping processes and prints verdicts for a differential"]
fn mac_lab_verdicts_on_processes_this_test_starts() {
    common::unmetered_spawns();
    let few = sleepers("w5bmacfew", 12);
    let many = sleepers("w5bmac", 30);
    std::thread::sleep(std::time::Duration::from_millis(1500));
    let commands = [
        "pkill -f w5bmacfew",
        "pkill w5bmacfew",
        "pkill -x w5bmacfew-1",
        "pkill -fi W5BMACFEW",
        "pkill -f 'w5bmacfew-[0-9]+'",
        "pkill -f 'w5bmacfew-[[:digit:]]'",
        "pkill -f 'w5bmacfew-\\d'",
        "pkill -f '\\bw5bmacfew'",
        "pkill -f 'w5bmac(few)?-1'",
        "pkill -f w5bmac",
        "pkill w5bmac",
        "pkill -f 'w5bmac-[0-9]+'",
        "pkill -f 'w5bmac-[[:digit:]]+'",
        "pkill -v -f w5bmacfew",
        "pkill -u $USER -f w5bmacfew",
        "pkill -f w5bmacfew -u $USER",
        "pkill -U $UID w5bmacfew",
        "pkill -n -f w5bmacfew",
        "pkill -o -f w5bmac",
        "pkill -f '\\w+-1'",
        "pkill -f 'w5bmacfew-1|\\d'",
        "kill $(pgrep -f w5bmacfew)",
        "kill $(pgrep -f w5bmac)",
        "kill -9 $(pgrep -f 'w5bmac-\\d')",
        "pgrep -f w5bmac | xargs kill",
        "killall w5bmacfew-1",
        "killall -m w5bmacfew",
        "killall -m w5bmac",
        "killall -u $USER w5bmacfew-2",
        "killall w5bmacfew-1 w5bmacfew-2",
        "sudo pkill -f w5bmacfew",
        "p=pkill; $p -f w5bmacfew",
        "for p in $(pgrep -f w5bmacfew); do kill $p; done",
        "bash -o pipefail -c 'pkill -f w5bmac'",
        "pkill -f w5bmac # skill kill",
        "$EDITOR notes/skills.md",
        "echo done",
    ];
    for command in commands {
        eprintln!(
            "verdict [mac-lab] `{}`: {}",
            command.escape_debug(),
            kind_of(command)
        );
    }
    for (pattern, flags) in [
        ("w5bmacfew", vec!["-f"]),
        ("w5bmacfew-[0-9]+", vec!["-f"]),
        ("w5bmacfew-[[:digit:]]", vec!["-f"]),
        ("W5BMACFEW", vec!["-fi"]),
        ("w5bmac(few)?-1", vec!["-f"]),
        ("w5bmac", vec!["-f"]),
        ("w5bmac", vec![]),
        ("\\d", vec!["-f"]),
        ("w5bmac-\\d", vec!["-f"]),
        ("w5bmacfew-1$", vec!["-f"]),
        ("w5bmacfew-1", vec!["-x"]),
    ] {
        let mut args = flags.clone();
        args.push("--");
        args.push(pattern);
        let real = Command::new("/usr/bin/pgrep")
            .args(&args)
            .stdin(Stdio::null())
            .output_locked()
            .expect("run the real pgrep");
        let real: BTreeSet<i32> = String::from_utf8_lossy(&real.stdout)
            .split_whitespace()
            .filter_map(|w| w.parse().ok())
            .filter(|p| few.contains(p) || many.contains(p))
            .collect();
        let guard = live_match_pids("pgrep", &args);
        let text = match guard {
            Ok(set) => {
                let set: BTreeSet<i32> = set
                    .into_iter()
                    .filter(|p| few.contains(p) || many.contains(p))
                    .collect();
                format!(
                    "real {} guard {} missing {} extra {}",
                    real.len(),
                    set.len(),
                    real.difference(&set).count(),
                    set.difference(&real).count()
                )
            }
            Err(why) => format!(
                "real {} guard Err({})",
                real.len(),
                why.chars().take(60).collect::<String>().escape_debug()
            ),
        };
        eprintln!(
            "labset [mac-lab] `pgrep {} {pattern}`: {text}",
            flags.join(" ")
        );
    }
    for pid in few.iter().chain(&many) {
        // SAFETY: pids this test started, and SIGKILL.
        unsafe {
            libc::kill(*pid, libc::SIGKILL);
            let mut status = 0;
            libc::waitpid(*pid, &mut status, 0);
        }
    }
}
