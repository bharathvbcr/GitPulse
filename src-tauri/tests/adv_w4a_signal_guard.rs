//! W4a adversary pass over the Linux signal guard (ft-06723bb7).
//!
//! The guard's job on Linux is to refuse or ask about any command whose real
//! `pkill`/`killall` would signal a process the user did not mean. The
//! oracle here is the real tool: decoy processes are started under many names
//! (a `gitpulse-*` executable is a GitPulse process to the guard, so a command
//! that reaches one must be refused), the real procps `pkill -e` / psmisc
//! `killall -v` is asked which of them a command line reaches, and the guard
//! is asked about the same line. A command that reaches a decoy while the
//! guard says nothing is a break.
//!
//! Nothing is ever signalled with anything but a signal every process of the
//! run ignores or logs (the run starts with `trap '' TERM HUP ...`, and the
//! decoy is a `sink` that handles every catchable signal), so a real `pkill`
//! that selects too much is harmless. Linux only; run in the podman image the
//! W4a lane used (`tests/fixtures/adv_w4a/run.sh`).

#![cfg(target_os = "linux")]

mod common;

use gitpulse_lib::hooks::signal_guard::{check_command, live_match_pids};
use gitpulse_lib::hooks::{HookOutput, PermissionDecision};
use gitpulse_lib::procguard::LockedSpawn;
use std::collections::BTreeSet;
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::OnceLock;
use std::time::{Duration, Instant};

const SINK_SOURCE: &str = include_str!("fixtures/adv_w4a_sink.c");

/// These tests send real signals (always `TERM`, which every process of the run
/// ignores or logs) to every process of the user that a pattern reaches, up to
/// `pkill -u $USER`. That is safe only inside the throwaway container
/// `tests/fixtures/adv_w4a/run.sh` starts, which sets this variable. Every test
/// is `#[ignore]`d, so an ordinary run reports them as not run, never as
/// passed; and a test forced with `--ignored` outside the container panics
/// here rather than signal anything, because a developer's or a CI runner's own
/// session would be among the processes reached.
fn container_only() {
    assert!(
        std::env::var_os("ADV_W4A_CONTAINER").is_some(),
        "this test sends real signals and runs only inside the W4a container \
         (tests/fixtures/adv_w4a/run.sh sets ADV_W4A_CONTAINER); refusing to run here"
    );
    // Every test starts here: the guard and the process helpers below are the
    // library calls of this suite, and a deferral by the spawn rate cap would
    // be noise in a differential run.
    common::unmetered_spawns();
}

/// What the guard said about a command line.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Kind {
    Silent,
    Notice,
    Ask,
    Deny,
}

fn kind(output: &HookOutput) -> Kind {
    match output
        .hook_specific_output
        .as_ref()
        .and_then(|s| s.permission_decision)
    {
        Some(PermissionDecision::Deny) => Kind::Deny,
        Some(PermissionDecision::Ask) => Kind::Ask,
        None if output.system_message.is_some() => Kind::Notice,
        None => Kind::Silent,
    }
}

struct Spec {
    label: &'static str,
    exe: &'static str,
    /// `argv[0]`; `None` is the executable's bare name, `@PATH@` its full path.
    argv0: Option<&'static str>,
    args: Vec<String>,
    env: Vec<(&'static str, &'static str)>,
}

fn spec(label: &'static str, exe: &'static str) -> Spec {
    Spec {
        label,
        exe,
        argv0: None,
        args: Vec::new(),
        env: Vec::new(),
    }
}

fn specs() -> Vec<Spec> {
    let strings = |a: &[&str]| a.iter().map(|s| (*s).to_string()).collect::<Vec<_>>();
    let mut big: Vec<String> = (0..10).map(|_| "a".repeat(120_000)).collect();
    big.push("needleatthe1mbend".into());
    vec![
        spec("short14", "gitpulse-short"),
        spec("exact15", "gitpulse-abcdef"),
        spec("len16", "gitpulse-abcdefg"),
        spec("long", "gitpulse-this-is-a-long-daemon-name"),
        Spec {
            argv0: Some("-gitpulse-login"),
            ..spec("login-shell", "gitpulse-login")
        },
        Spec {
            argv0: Some("/opt/other/otherprog"),
            args: strings(&["--flag"]),
            ..spec("argv0-other", "gitpulse-aliased")
        },
        Spec {
            argv0: Some("@PATH@"),
            ..spec("argv0-abs", "gitpulse-abspath")
        },
        Spec {
            argv0: Some("bareother"),
            args: strings(&["--flag"]),
            ..spec("argv0-bare", "gitpulse-barearg")
        },
        Spec {
            env: vec![("SINK_COMM", "renamedcomm")],
            ..spec("prctl-comm", "gitpulse-prctl")
        },
        Spec {
            args: strings(&["aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"]),
            env: vec![("SINK_TITLE", "newtitle xyz")],
            ..spec("title-rewrite", "gitpulse-title")
        },
        Spec {
            args: strings(&["aaaa"]),
            env: vec![("SINK_BLANK", "1")],
            ..spec("blank-cmdline", "gitpulse-blank")
        },
        spec("utf8-cut", "gitpulse-x\u{e9}\u{e9}\u{e9}"),
        Spec {
            argv0: Some("GITPULSE-Upper"),
            ..spec("upper", "GITPULSE-Upper")
        },
        spec("paren-space", "gitpulse (x) y"),
        Spec {
            args: strings(&["first line\nsecond line", "tail"]),
            ..spec("newline-arg", "gitpulse-newl")
        },
        Spec {
            args: strings(&["a  b", "c\td"]),
            ..spec("spaces", "gitpulse-sp")
        },
        Spec {
            args: strings(&["", "x"]),
            ..spec("empty-arg", "gitpulse-empty")
        },
        Spec {
            args: strings(&["a\u{1}b"]),
            ..spec("control-arg", "gitpulse-ctl")
        },
        Spec {
            args: strings(&["caf\u{e9}"]),
            ..spec("utf8-arg", "gitpulse-u8")
        },
        Spec {
            env: vec![("SINK_NODUMP", "1")],
            ..spec("not-dumpable", "gitpulse-nodump")
        },
        Spec {
            env: vec![("SINK_ZOMBIE", "1")],
            ..spec("zombie-parent", "gitpulse-zombp")
        },
        Spec {
            args: big,
            ..spec("args-over-1mib", "gitpulse-big")
        },
        // A plain process that carries the same words, so a selection that
        // picks one of the two can be told from the other.
        Spec {
            args: strings(&["tiemark"]),
            ..spec("tie-plain", "plainsink")
        },
    ]
}

struct Lab {
    dir: PathBuf,
    /// label, pid
    decoys: Vec<(&'static str, i32)>,
    /// Ancestors of this process that are GitPulse-named: the wrapper.
    ancestors: Vec<i32>,
    zombies: Vec<i32>,
}

impl Lab {
    fn protected(&self) -> BTreeSet<i32> {
        self.decoys
            .iter()
            // `tie-plain` is the ordinary process beside a decoy. `argv0-other`
            // runs a GitPulse executable under another path as `argv[0]`: the
            // guard identifies it by the executable the kernel runs
            // (`/proc/<pid>/exe`), so it is protected like the others.
            .filter(|(label, _)| *label != "tie-plain")
            .map(|(_, pid)| *pid)
            .chain(self.ancestors.iter().copied())
            .collect()
    }
}

fn lab() -> &'static Lab {
    static LAB: OnceLock<Lab> = OnceLock::new();
    LAB.get_or_init(build_lab)
}

fn compile_sink(dir: &Path) -> PathBuf {
    let source = dir.join("sink.c");
    std::fs::write(&source, SINK_SOURCE).expect("write the sink source");
    let sink = dir.join("sink");
    let built = Command::new("cc")
        .args(["-O1", "-o"])
        .arg(&sink)
        .arg(&source)
        .output_locked()
        .expect("run cc");
    assert!(
        built.status.success(),
        "cc failed: {}",
        String::from_utf8_lossy(&built.stderr)
    );
    sink
}

fn ignore_catchable_signals() {
    for sig in [
        libc::SIGTERM,
        libc::SIGHUP,
        libc::SIGINT,
        libc::SIGQUIT,
        libc::SIGUSR1,
        libc::SIGUSR2,
        libc::SIGALRM,
    ] {
        // SAFETY: setting a disposition to SIG_IGN has no other precondition.
        unsafe { libc::signal(sig, libc::SIG_IGN) };
    }
}

fn spawn_decoy(dir: &Path, sink: &Path, log: &Path, spec: &Spec) -> Option<i32> {
    let exe = dir.join(spec.exe);
    std::fs::copy(sink, &exe).expect("copy the sink under its name");
    let argv0 = match spec.argv0 {
        Some("@PATH@") => exe.to_string_lossy().into_owned(),
        Some(other) => other.to_string(),
        None => spec.exe.to_string(),
    };
    let mut command = Command::new(&exe);
    command
        .arg0(argv0)
        .args(&spec.args)
        .env("SINK_LOG", log)
        .env("SINK_SECS", "3000")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    for (key, value) in &spec.env {
        command.env(key, value);
    }
    match command.spawn_locked() {
        Ok(child) => {
            let pid = i32::try_from(child.id()).expect("a pid fits an i32");
            std::mem::forget(child);
            Some(pid)
        }
        Err(e) => {
            eprintln!("decoy {} did not start: {e}", spec.label);
            None
        }
    }
}

fn read(path: &str) -> String {
    std::fs::read(path)
        .map(|b| String::from_utf8_lossy(&b).into_owned())
        .unwrap_or_default()
}

/// `(ppid, state)` from `/proc/<pid>/stat`.
fn stat_of(pid: i32) -> Option<(i32, char)> {
    let text = read(&format!("/proc/{pid}/stat"));
    let rest = &text[text.rfind(')')? + 2..];
    let mut fields = rest.split_whitespace();
    let state = fields.next()?.chars().next()?;
    let ppid = fields.next()?.parse().ok()?;
    Some((ppid, state))
}

fn all_pids() -> Vec<i32> {
    std::fs::read_dir("/proc")
        .expect("read /proc")
        .filter_map(|e| e.ok()?.file_name().to_str()?.parse().ok())
        .collect()
}

fn build_lab() -> Lab {
    ignore_catchable_signals();
    let dir = std::env::temp_dir().join(format!("adv-w4a-{}", std::process::id()));
    std::fs::create_dir_all(&dir).expect("lab directory");
    let sink = compile_sink(&dir);
    let log = dir.join("sink.log");
    std::fs::write(&log, "").expect("sink log");
    let mut decoys = Vec::new();
    for spec in specs() {
        if let Some(pid) = spawn_decoy(&dir, &sink, &log, &spec) {
            decoys.push((spec.label, pid));
        }
    }
    let started = Instant::now();
    while started.elapsed() < common::env::coverage_relaxed(Duration::from_secs(30)) {
        let ready = read(log.to_str().expect("utf8 path"))
            .lines()
            .filter(|l| l.starts_with("R "))
            .count();
        if ready >= decoys.len() {
            break;
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    std::thread::sleep(Duration::from_millis(300));
    // Ancestors whose executable is a GitPulse-named copy of bash (the wrapper
    // run.sh puts above this process), so a pkill that reaches its caller's
    // ancestors reaches something the guard must care about.
    let mut ancestors = Vec::new();
    let mut at = i32::try_from(std::process::id()).expect("pid");
    while let Some((ppid, _)) = stat_of(at) {
        if ppid <= 1 {
            break;
        }
        let exe = std::fs::read_link(format!("/proc/{ppid}/exe"))
            .map(|p| p.to_string_lossy().into_owned())
            .unwrap_or_default();
        if exe
            .rsplit('/')
            .next()
            .is_some_and(|b| b.starts_with("gitpulse-"))
        {
            ancestors.push(ppid);
        }
        at = ppid;
    }
    let parents: BTreeSet<i32> = decoys.iter().map(|(_, p)| *p).collect();
    let zombies = all_pids()
        .into_iter()
        .filter(|p| {
            stat_of(*p).is_some_and(|(ppid, state)| state == 'Z' && parents.contains(&ppid))
        })
        .collect();
    Lab {
        dir,
        decoys,
        ancestors,
        zombies,
    }
}

fn quote(word: &str) -> String {
    format!("'{}'", word.replace('\'', "'\\''"))
}

/// A line of text to a pid, for `pkill -e` ("name killed (pid N)") and
/// `killall -v` ("Killed name(N) with signal S"), and a bare pid for `pgrep`.
fn pids_in(prog: &str, text: &str) -> BTreeSet<i32> {
    text.lines()
        .filter_map(|line| match prog {
            "pkill" => {
                let at = line.rfind("(pid ")?;
                line[at + 5..].trim_end_matches(')').trim().parse().ok()
            }
            "killall" => {
                let end = line.rfind(") with signal")?;
                let start = line[..end].rfind('(')?;
                line[start + 1..end].parse().ok()
            }
            _ => line.split_whitespace().next()?.parse().ok(),
        })
        .chain(
            // `pidof` prints every pid on one line.
            text.lines()
                .filter(|_| prog == "pidof")
                .flat_map(|l| l.split_whitespace().filter_map(|w| w.parse::<i32>().ok())),
        )
        .collect()
}

fn run_real(prog: &str, args: &[String]) -> (BTreeSet<i32>, String) {
    let mut real: Vec<String> = Vec::new();
    match prog {
        "pkill" => real.push("-e".into()),
        "killall" => real.push("-v".into()),
        _ => {}
    }
    real.extend(args.iter().cloned());
    let output = Command::new(format!("/usr/bin/{prog}"))
        .args(&real)
        .stdin(Stdio::null())
        .output_locked()
        .expect("run the real tool");
    let mut text = String::from_utf8_lossy(&output.stdout).into_owned();
    let notes = String::from_utf8_lossy(&output.stderr).trim().to_string();
    text.push('\n');
    text.push_str(&notes);
    (pids_in(prog, &text), notes)
}

struct Verdict {
    command: String,
    real: BTreeSet<i32>,
    kind: Kind,
    guard: Result<BTreeSet<i32>, String>,
    broke: Option<String>,
    partial: Option<String>,
}

/// One command line, asked of the real tool and of the guard. `sig` replaces
/// every `@S@` in `args`; `prog` is `pkill`, `killall`, or `pgrep` (the guard
/// is asked about `kill $(pgrep ...)`).
fn ask(prog: &str, args: &[&str], sig: &str) -> Verdict {
    let args: Vec<String> = args.iter().map(|a| a.replace("@S@", sig)).collect();
    let quoted: Vec<String> = args.iter().map(|a| quote(a)).collect();
    let command = match prog {
        "pgrep" | "pidof" => format!("kill $({prog} {})", quoted.join(" ")),
        _ => format!("{prog} {}", quoted.join(" ")),
    };
    let (real, _) = run_real(prog, &args);
    let refs: Vec<&str> = args.iter().map(String::as_str).collect();
    let guard = live_match_pids(prog, &refs).map(|v| v.into_iter().collect::<BTreeSet<i32>>());
    let kind = kind(&check_command(&command));
    let protected = lab().protected();
    // A decoy that has died is a zombie the tools still list by name and no
    // signal can reach; the guard's table does not carry zombies.
    let zero = zero_signal(&args);
    let danger: BTreeSet<i32> = if zero {
        BTreeSet::new()
    } else {
        real.intersection(&protected)
            .copied()
            .filter(|p| stat_of(*p).is_some_and(|(_, state)| state != 'Z'))
            .collect()
    };
    let mut broke = None;
    let mut partial = None;
    if !danger.is_empty() {
        let list = describe(&danger.iter().copied().collect::<Vec<_>>());
        // The break: a command that signals a protected process and that the
        // guard let through without a word.
        if matches!(kind, Kind::Silent | Kind::Notice) {
            let why = match &guard {
                Ok(_) => "the guard evaluated it and said nothing".to_string(),
                Err(why) => format!("the guard could not read it ({why}) and said nothing"),
            };
            broke = Some(format!("the real tool reaches {list}; {why}"));
        } else if let Ok(set) = &guard {
            // Refused or asked for another reason, but its own set lacks a
            // process the tool reaches.
            let missed: Vec<i32> = danger.difference(set).copied().collect();
            if !missed.is_empty() {
                partial = Some(format!(
                    "the real tool reaches {} that the guard's set lacks (verdict {kind:?} comes from other processes)",
                    describe(&missed)
                ));
            }
        }
    }
    Verdict {
        command,
        real,
        kind,
        guard,
        broke,
        partial,
    }
}

/// Whether the arguments name signal 0, which sends nothing at all.
fn zero_signal(args: &[String]) -> bool {
    args.iter().enumerate().any(|(i, a)| {
        a == "-0"
            || a == "--signal=0"
            || (i > 0 && a == "0" && matches!(args[i - 1].as_str(), "-s" | "--signal"))
    })
}

fn describe(pids: &[i32]) -> String {
    let lab = lab();
    pids.iter()
        .map(|pid| {
            let label = lab
                .decoys
                .iter()
                .find(|(_, p)| p == pid)
                .map(|(l, _)| *l)
                .or_else(|| lab.ancestors.contains(pid).then_some("ancestor"))
                .or_else(|| lab.zombies.contains(pid).then_some("zombie"))
                .unwrap_or("other");
            format!("{pid}({label})")
        })
        .collect::<Vec<_>>()
        .join(",")
}

#[derive(Default)]
struct Tally {
    cases: usize,
    dangerous: usize,
    asked: usize,
    denied: usize,
    breaks: Vec<String>,
    partials: Vec<String>,
}

impl Tally {
    fn add(&mut self, tag: &str, v: &Verdict) {
        self.cases += 1;
        let protected = lab().protected();
        let hit = v.real.intersection(&protected).next().is_some();
        self.dangerous += usize::from(hit);
        self.asked += usize::from(v.kind == Kind::Ask);
        self.denied += usize::from(v.kind == Kind::Deny);
        if std::env::var_os("ADV_VERBOSE").is_some() {
            let guard = match &v.guard {
                Ok(set) => format!("Ok({} pids)", set.len()),
                Err(why) => format!("Err({why})"),
            };
            // One physical line per case, whatever the command holds, so two
            // runs can be compared line by line.
            eprintln!(
                "case [{tag}] `{}`: real reaches {} of which decoys {}; guard {}, {:?}",
                v.command.escape_debug(),
                v.real.len(),
                describe(&v.real.intersection(&protected).copied().collect::<Vec<_>>()),
                guard.escape_debug(),
                v.kind
            );
        }
        if let Some(why) = &v.partial {
            self.partials
                .push(format!("PARTIAL [{tag}] `{}`: {why}", v.command));
        }
        if let Some(why) = &v.broke {
            let guard = match &v.guard {
                Ok(set) => format!("Ok({} pids)", set.len()),
                Err(why) => format!("Err({why})"),
            };
            self.breaks.push(format!(
                "BROKE [{tag}] `{}`: {why}; guard {guard}, kind {:?}",
                v.command, v.kind
            ));
        }
    }

    fn finish(self, name: &str) {
        let breaks = self.report(name);
        assert!(breaks == 0, "{name}: {breaks} break(s)");
    }

    /// Prints the tally and returns how many cases broke, for a test that runs
    /// several seeds and must show every one of them before it fails.
    fn report(&self, name: &str) -> usize {
        eprintln!(
            "{name}: {} cases, {} reach a decoy, guard asked {} denied {}, {} BROKE",
            self.cases,
            self.dangerous,
            self.asked,
            self.denied,
            self.breaks.len()
        );
        for line in self.partials.iter().take(10) {
            eprintln!("{}", line.chars().take(420).collect::<String>());
        }
        for line in self.breaks.iter().take(30) {
            eprintln!("{}", line.chars().take(420).collect::<String>());
        }
        if self.breaks.len() > 30 {
            eprintln!("... and {} more", self.breaks.len() - 30);
        }
        self.breaks.len()
    }
}

// ───────────────────────── hand-written attacks ─────────────────────────────

fn pk(args: &[&str]) -> (&'static str, Vec<String>) {
    ("pkill", args.iter().map(|s| (*s).to_string()).collect())
}
fn ka(args: &[&str]) -> (&'static str, Vec<String>) {
    ("killall", args.iter().map(|s| (*s).to_string()).collect())
}
fn pg(args: &[&str]) -> (&'static str, Vec<String>) {
    ("pgrep", args.iter().map(|s| (*s).to_string()).collect())
}

fn run_named(name: &str, cases: Vec<(&'static str, Vec<String>)>) {
    let mut tally = Tally::default();
    for (prog, args) in cases {
        let refs: Vec<&str> = args.iter().map(String::as_str).collect();
        let v = ask(prog, &refs, "TERM");
        tally.add(name, &v);
    }
    tally.finish(name);
}

#[test]
#[ignore = "sends real signals; runs only inside the W4a container: tests/fixtures/adv_w4a/run.sh"]
fn comm_is_cut_at_fifteen_bytes_and_names_straddle_it() {
    container_only();
    run_named(
        "comm-cut",
        vec![
            pk(&["-TERM", "gitpulse-abcdef"]),
            pk(&["-TERM", "-x", "gitpulse-abcdef"]),
            pk(&["-TERM", "-x", "gitpulse-abcdefg"]),
            pk(&["-TERM", "gitpulse-abcdefg"]),
            pk(&["-TERM", "-x", "gitpulse-this-is-a"]),
            pk(&["-TERM", "-x", "gitpulse-this-is-a-long-daemon-name"]),
            pk(&["-TERM", "gitpulse-this-is-a-long-daemon-name"]),
            pk(&["-TERM", "-f", "gitpulse-this-is-a-long-daemon-name"]),
            pk(&["-TERM", "-xf", "gitpulse-this-is-a-long-daemon-name"]),
            pk(&["-TERM", "-x", "gitpulse-this-is"]),
            pk(&["-TERM", "^gitpulse-this-is-a-l$"]),
            pk(&["-TERM", "-i", "GITPULSE-THIS-IS-A-LONG"]),
            ka(&["-TERM", "gitpulse-abcdef"]),
            ka(&["-TERM", "gitpulse-abcdefg"]),
            ka(&["-TERM", "gitpulse-abcdefgh"]),
            ka(&["-TERM", "-e", "gitpulse-abcdefg"]),
            ka(&["-TERM", "-e", "gitpulse-abcdef"]),
            ka(&["-TERM", "gitpulse-this-is-a-long-daemon-name"]),
            ka(&["-TERM", "gitpulse-this-is-a-long-other-name"]),
            ka(&["-TERM", "-e", "gitpulse-this-is-a-long-other-name"]),
            ka(&["-TERM", "gitpulse-this-is-a"]),
            ka(&["-TERM", "-I", "GITPULSE-THIS-IS-A-LONG-DAEMON-NAME"]),
            ka(&["-TERM", "-I", "GITPULSE-THIS-IS-A-LONG-OTHER-NAME"]),
            ka(&["-TERM", "-r", "gitpulse-this-is-a-l"]),
            ka(&["-TERM", "-r", "-e", "gitpulse-this-is-a-l"]),
            ka(&["-TERM", "-rI", "GITPULSE-THIS"]),
            ka(&["-TERM", "-r", "^gitpulse-abcdef$"]),
            ka(&["-TERM", "-r", "^gitpulse-abcdefg$"]),
            pg(&["-x", "gitpulse-abcdef"]),
            pg(&["gitpulse-abcdefg"]),
        ],
    );
}

#[test]
#[ignore = "sends real signals; runs only inside the W4a container: tests/fixtures/adv_w4a/run.sh"]
fn a_comm_cut_inside_a_utf8_character_is_still_matched_by_the_tools_prefix_rule() {
    container_only();
    // `gitpulse-x` + three two-byte characters is 16 bytes; the kernel keeps 15,
    // cutting the last character in half. psmisc compares the first 15 bytes
    // of the name with them, so a different name with the same 15 bytes is
    // signalled.
    run_named(
        "utf8-cut",
        vec![
            ka(&["-TERM", "gitpulse-x\u{e9}\u{e9}\u{e9}"]),
            ka(&["-TERM", "gitpulse-x\u{e9}\u{e9}\u{e9}-other"]),
            ka(&["-TERM", "gitpulse-x\u{e9}\u{e9}\u{e9}-other-name"]),
            ka(&["-TERM", "gitpulse-x\u{e9}\u{e9}\u{e9}zzzzzz"]),
            ka(&["-TERM", "gitpulse-x\u{e9}\u{e9}"]),
            ka(&["-TERM", "-I", "GITPULSE-X\u{e9}\u{e9}\u{e9}-OTHER"]),
            ka(&["-TERM", "-r", "gitpulse-x\u{e9}\u{e9}"]),
            pk(&["-TERM", "gitpulse-x\u{e9}\u{e9}"]),
            pk(&["-TERM", "-x", "gitpulse-x\u{e9}\u{e9}\u{e9}"]),
            pk(&["-TERM", "-f", "gitpulse-x\u{e9}"]),
        ],
    );
}

#[test]
#[ignore = "sends real signals; runs only inside the W4a container: tests/fixtures/adv_w4a/run.sh"]
fn login_shells_titles_renamed_comms_and_argv0_paths() {
    container_only();
    run_named(
        "names",
        vec![
            pk(&["-TERM", "-x", "gitpulse-login"]),
            pk(&["-TERM", "-x", "--", "-gitpulse-login"]),
            pk(&["-TERM", "-f", "--", "-gitpulse-login"]),
            pk(&["-TERM", "gitpulse-login"]),
            ka(&["-TERM", "gitpulse-login"]),
            ka(&["-TERM", "-gitpulse-login"]),
            ka(&["-TERM", "--", "-gitpulse-login"]),
            pk(&["-TERM", "otherprog"]),
            pk(&["-TERM", "-f", "otherprog"]),
            pk(&["-TERM", "-fx", "/opt/other/otherprog --flag"]),
            pk(&["-TERM", "-x", "otherprog"]),
            ka(&["-TERM", "otherprog"]),
            ka(&["-TERM", "gitpulse-aliased"]),
            ka(&["/opt/other/otherprog"]),
            pk(&["-TERM", "-x", "gitpulse-abspath"]),
            pk(&["-TERM", "-f", "/gitpulse-abspath"]),
            ka(&["-TERM", "gitpulse-abspath"]),
            pk(&["-TERM", "renamedcomm"]),
            pk(&["-TERM", "-x", "renamedcomm"]),
            pk(&["-TERM", "gitpulse-prctl"]),
            pk(&["-TERM", "-x", "gitpulse-prctl"]),
            ka(&["-TERM", "renamedcomm"]),
            ka(&["-TERM", "gitpulse-prctl"]),
            pk(&["-TERM", "-f", "newtitle"]),
            pk(&["-TERM", "-fx", "newtitle xyz"]),
            pk(&["-TERM", "-f", "gitpulse-title"]),
            pk(&["-TERM", "gitpulse-title"]),
            ka(&["-TERM", "gitpulse-title"]),
            pk(&["-TERM", "-f", "gitpulse-blank"]),
            pk(&["-TERM", "-fx", "gitpulse-blank"]),
            pk(&["-TERM", "gitpulse-blank"]),
            ka(&["-TERM", "gitpulse-blank"]),
            pk(&["-TERM", "-f", "aaaa"]),
            pk(&["-TERM", "-i", "-f", "GITPULSE-UPPER"]),
            pk(&["-TERM", "-f", "GITPULSE-Upper"]),
            pk(&["-TERM", "gitpulse-upper"]),
            pk(&["-TERM", "-i", "gitpulse-upper"]),
            ka(&["-TERM", "-I", "gitpulse-upper"]),
            ka(&["-TERM", "gitpulse-upper"]),
            pk(&["-TERM", "gitpulse (x) y"]),
            pk(&["-TERM", "-x", "gitpulse \\(x\\) y"]),
            pk(&["-TERM", "\\(x\\)"]),
            ka(&["-TERM", "gitpulse (x) y"]),
            ka(&["-TERM", "gitpulse"]),
            pk(&["-TERM", "-f", "gitpulse (x) y"]),
        ],
    );
}

#[test]
#[ignore = "sends real signals; runs only inside the W4a container: tests/fixtures/adv_w4a/run.sh"]
fn full_command_lines_with_newlines_spaces_empties_and_over_a_mebibyte() {
    container_only();
    run_named(
        "cmdline",
        vec![
            pk(&["-TERM", "-f", "first line"]),
            pk(&["-TERM", "-f", "second line"]),
            pk(&["-TERM", "-f", "^gitpulse-newl first"]),
            pk(&["-TERM", "-fx", ".*"]),
            pk(&["-TERM", "-f", "a  b"]),
            pk(&["-TERM", "-f", "a b"]),
            pk(&["-TERM", "-f", "c.d"]),
            pk(&["-TERM", "-f", "gitpulse-sp a  b c\td"]),
            pk(&["-TERM", "-fx", "gitpulse-empty x"]),
            pk(&["-TERM", "-f", "gitpulse-empty x"]),
            // procps reads about 128 KiB of a command line; the guard reads a
            // mebibyte, so what the tool cannot see the guard still can.
            pk(&["-TERM", "-f", "needleatthe1mbend"]),
            pk(&["-TERM", "-f", "a+needleatthe1mbend"]),
            pk(&["-TERM", "-f", "gitpulse-big"]),
            pk(&["-TERM", "-fx", "gitpulse-big.*needleatthe1mbend"]),
            pk(&["-TERM", "-f", "needle"]),
        ],
    );
}

/// BROKE on 1ba087a0 (ft-1d9f44d5, class 1): the tool and the guard read a
/// process's command line differently where it holds a newline, a control
/// character, an empty argument or nothing at all. Verified against procps-ng
/// 4.0.4 under the image's locale, `C` and `C.UTF-8`:
///
/// * procps hands `regcomp` a command line with its control characters
///   replaced: a newline is a space; any other control character is `?` in a
///   UTF-8 locale and `.` in the C locale. `.` also crosses a newline, which
///   the `regex` crate's `.` does not by default.
/// * an empty argument between two others is a double space (`a  b`); a
///   trailing one is nothing.
/// * a process that blanked its argument area is a single `?`.
#[test]
#[ignore = "sends real signals; runs only inside the W4a container: tests/fixtures/adv_w4a/run.sh"]
fn newline_control_empty_argument_and_blank_command_line_are_read_as_the_tools_read_them() {
    container_only();
    run_named(
        "cmdline-readings",
        vec![
            pk(&["-TERM", "-f", "line.second"]),
            pk(&["-TERM", "-f", "line second"]),
            pk(&["-TERM", "-f", "first.*second"]),
            pk(&["-TERM", "-fx", "gitpulse-newl first line.second line tail"]),
            pk(&["-TERM", "-fx", "gitpulse-newl first line second line tail"]),
            pk(&["-TERM", "-fx", "gitpulse-newl.*"]),
            pk(&["-TERM", "-fx", "gitpulse-empty  x"]),
            pk(&["-TERM", "-f", "gitpulse-empty  x"]),
            pk(&["-TERM", "-fx", "gitpulse-empty x"]),
            pk(&["-TERM", "-fx", "."]),
            pk(&["-TERM", "-f", "^.$"]),
            pk(&["-TERM", "-f", "^\\?$"]),
            pk(&["-TERM", "-f", "^[^a-z]$"]),
            pk(&["-TERM", "-f", "a.b"]),
            pk(&["-TERM", "-f", "a\\?b"]),
            pk(&["-TERM", "-f", "a\\.b"]),
            pk(&["-TERM", "-f", "a?b"]),
            pk(&["-TERM", "-f", "a b"]),
            pk(&["-TERM", "-f", "c.d"]),
            pk(&["-TERM", "-f", "c\\?d"]),
            pk(&["-TERM", "-f", "c\\.d"]),
            pk(&["-TERM", "-f", "caf."]),
            pk(&["-TERM", "-f", "caf.."]),
            pk(&["-TERM", "-fx", "gitpulse-u8 caf."]),
            pk(&["-TERM", "-fx", "gitpulse-u8 caf.."]),
            pk(&["-TERM", "-f", "caf\u{e9}"]),
            pk(&["-TERM", "-f", "caf[^a]"]),
            pk(&["-TERM", "-f", "--inverse", "gitpulse-newl"]),
            pk(&["-TERM", "-f", "--inverse", "line second"]),
            pk(&["-TERM", "-fx", "--inverse", "gitpulse-short"]),
        ],
    );
}

#[test]
#[ignore = "sends real signals; runs only inside the W4a container: tests/fixtures/adv_w4a/run.sh"]
fn options_after_the_pattern_are_still_asked_about() {
    container_only();
    run_named(
        "late-options",
        vec![
            pk(&["-TERM", "-f", "gitpulse", "-u", "runner"]),
            pk(&["-f", "gitpulse", "-u", "runner", "-TERM"]),
            pk(&["gitpulse", "-f", "-TERM"]),
            pk(&["gitpulse", "-f", "-u", "runner", "-@S@"]),
            pk(&["-f", "gitpulse", "-u"]),
            pk(&["-f", "gitpulse", "-u", "0"]),
            pk(&["-f", "gitpulse", "-U", "0", "-@S@"]),
            pk(&["-f", "gitpulse", "-x"]),
            pk(&["-f", "gitpulse-short", "-x"]),
            pk(&["gitpulse-short", "-x", "-@S@"]),
            pk(&["gitpulse-short", "-v", "-@S@"]),
            pk(&["gitpulse-short", "--inverse", "-@S@"]),
            pk(&["gitpulse-short", "-v"]),
            pk(&["gitpulse-short", "--", "-@S@"]),
            pk(&["--", "gitpulse-short", "-@S@"]),
            pk(&["-@S@", "--", "gitpulse-short"]),
            pk(&["-@S@", "-f", "--", "-gitpulse-login"]),
            pk(&["-fu", "runner", "gitpulse"]),
            pk(&["-fu", "runner", "-@S@", "gitpulse"]),
            pk(&["-fx", "gitpulse-short", "-@S@"]),
            pk(&["-u", "runner", "-@S@"]),
            pk(&["-U", "runner", "-@S@"]),
            pk(&["--euid=runner", "-@S@"]),
            pk(&["--euid", "runner", "--signal", "@S@"]),
            pk(&["--signal=@S@", "--euid", "runner"]),
            pk(&["--signal", "@S@", "gitpulse-short"]),
            pk(&["--sig", "@S@", "gitpulse-short"]),
            pk(&["--eu", "runner", "-@S@"]),
            pk(&["-SIG@S@", "gitpulse-short"]),
            pk(&["-sig@S@", "gitpulse-short"]),
            pk(&["-@S@", "-x", "gitpulse-short"]),
            pk(&["-x", "-@S@", "gitpulse-short"]),
            pk(&["-x", "gitpulse-short", "-@S@"]),
            pk(&["-x", "gitpulse-short", "-0"]),
            ka(&["-s", "@S@", "gitpulse-short"]),
            ka(&["--signal", "@S@", "gitpulse-short"]),
            ka(&["--signal=@S@", "gitpulse-short"]),
            ka(&["gitpulse-short", "-@S@"]),
            ka(&["gitpulse-short", "-s", "@S@"]),
            ka(&["gitpulse-short", "-u", "runner"]),
            ka(&["gitpulse-short", "-e"]),
            ka(&["-@S@", "-u", "runner"]),
            ka(&["-u", "runner", "-@S@"]),
            ka(&["-u", "runner"]),
            ka(&["--user", "runner", "-@S@"]),
            ka(&["--user=runner"]),
            ka(&["-u"]),
            ka(&["-s"]),
            ka(&["-r", "gitpulse", "-I"]),
            ka(&["-vr", "gitpulse"]),
            ka(&["-er", "gitpulse-short"]),
            ka(&["-re", "gitpulse-short"]),
        ],
    );
}

/// The same command in the shapes a shell lets it be written. Each reaches the
/// `gitpulse-short` decoy (checked against the real `pgrep` first), so none may
/// be silent.
#[test]
#[ignore = "sends real signals; runs only inside the W4a container: tests/fixtures/adv_w4a/run.sh"]
fn the_same_kill_in_every_shell_shape_is_not_silent() {
    container_only();
    let decoy = lab()
        .decoys
        .iter()
        .find(|(l, _)| *l == "short14")
        .expect("the short14 decoy")
        .1;
    let (reached, _) = run_real("pgrep", &["-f".to_string(), "gitpulse-short".to_string()]);
    assert!(
        reached.contains(&decoy),
        "the real pgrep must reach the decoy"
    );
    let mut quiet = Vec::new();
    for command in [
        "pkill -f gitpulse-short",
        "pkill -TERM -f gitpulse-short",
        "sudo pkill -f gitpulse-short",
        "env FOO=1 pkill -f gitpulse-short",
        "command pkill -f gitpulse-short",
        "nohup pkill -f gitpulse-short",
        "/usr/bin/pkill -f gitpulse-short",
        "bash -c 'pkill -f gitpulse-short'",
        "sh -c \"pkill -f gitpulse-short\"",
        "true && pkill -f gitpulse-short",
        "echo x; pkill -f gitpulse-short",
        "(pkill -f gitpulse-short)",
        "{ pkill -f gitpulse-short; }",
        "x=$(pkill -f gitpulse-short)",
        "pkill -f gitpulse-short | cat",
        "for i in 1; do pkill -f gitpulse-short; done",
        "eval 'pkill -f gitpulse-short'",
        "pkill -f gitpulse-short &",
        "kill $(pgrep -f gitpulse-short)",
        "kill -9 $(pgrep -f gitpulse-short)",
        "kill -TERM `pgrep -f gitpulse-short`",
        "pgrep -f gitpulse-short | xargs kill",
        "pgrep -f gitpulse-short | xargs kill -9",
        "kill $(pidof gitpulse-short)",
        "killall gitpulse-short",
        "killall -9 gitpulse-short",
        "pkill -f gitpulse-$X",
        "pkill -f \"gitpulse-${X}short\"",
        "timeout 5 pkill -f gitpulse-short",
        "stdbuf -o0 pkill -f gitpulse-short",
        "exec pkill -f gitpulse-short",
        "time pkill -f gitpulse-short",
        "! pkill -f gitpulse-short",
        "if pkill -f gitpulse-short; then :; fi",
        "while pkill -f gitpulse-short; do :; done",
        "echo $(pkill -f gitpulse-short)",
        "cat <(pkill -f gitpulse-short)",
        "pkill -f gitpulse-short >/dev/null 2>&1",
        "pkill -f gitpulse-short || true",
        "pkill -f gitpulse-short; pkill -f gitpulse-short",
        "pkill\t-f\tgitpulse-short",
        "pkill -f 'gitpulse-short'",
        "pkill -f \"git\"'pulse'-short",
        "pkill -f $'gitpulse-short'",
        "xargs -a /dev/null pkill -f gitpulse-short",
    ] {
        let said = kind(&check_command(command));
        eprintln!("case [forms] `{command}`: real reaches 1 of which decoys {decoy}(short14); guard Ok(0 pids), {said:?}");
        if matches!(said, Kind::Silent | Kind::Notice) {
            quiet.push(command);
        }
    }
    eprintln!("shell forms: {} quiet: {quiet:?}", quiet.len());
    assert!(quiet.is_empty(), "silent for: {quiet:?}");
}

/// BROKE on 1ba087a0 (ft-1d9f44d5, class 3): each is a real `pkill` that
/// reaches the GitPulse-named decoy when a shell runs it, and the guard did
/// not see the program. They are asked about now, where the shape is visible
/// in the line: a busybox or toybox applet, a program (or an `eval` or `-c`
/// script) built from a variable on a line that says `kill`, an `alias` defined
/// on the line, `find -exec`, and `ssh` to this machine.
#[test]
#[ignore = "sends real signals; runs only inside the W4a container: tests/fixtures/adv_w4a/run.sh"]
fn a_pkill_the_shell_reader_does_not_resolve_is_asked_about() {
    container_only();
    let mut quiet = Vec::new();
    for command in [
        "busybox pkill -f gitpulse-short",
        "busybox killall gitpulse-short",
        "toybox pkill -f gitpulse-short",
        "sudo busybox pkill -f gitpulse-short",
        "p=pkill; $p -f gitpulse-short",
        "p=killall; $p gitpulse-short",
        "p=pkill; \"$p\" -f gitpulse-short",
        "p=pkill; sudo $p -f gitpulse-short",
        "p=pkill; ${p} -f gitpulse-short",
        "p=$(echo pkill); $p -f gitpulse-short",
        "P=/usr/bin/pkill; $P -f gitpulse-short",
        "c='pkill -f gitpulse-short'; eval $c",
        "c='pkill -f gitpulse-short'; bash -c \"$c\"",
        "c='pkill -f gitpulse-short'; sh -c $c",
        "alias k=pkill; k -f gitpulse-short",
        "alias k='pkill -f'; k gitpulse-short",
        "ssh localhost pkill -f gitpulse-short",
        "ssh user@127.0.0.1 'pkill -f gitpulse-short'",
        "ssh -p 22 localhost \"killall gitpulse-short\"",
        "ssh $HOST pkill -f gitpulse-short",
        "find . -name x -exec pkill -f gitpulse-short {} +",
        "find . -name x -execdir pkill -f gitpulse-short {} \\;",
        "find . -exec killall gitpulse-short \\;",
        "echo x | xargs $p pkill -f gitpulse-short",
    ] {
        let said = kind(&check_command(command));
        eprintln!("case [forms-resolve] `{command}`: real reaches 1 of which decoys 1(short14); guard Ok(0 pids), {said:?}");
        if matches!(said, Kind::Silent | Kind::Notice) {
            quiet.push(command);
        }
    }
    assert!(quiet.is_empty(), "silent for: {quiet:?}");
}

/// More shapes the shell reader resolves, and lines that signal nothing, so a
/// differential of two runs shows what a change to the reader made louder: the
/// first list is the same real `pkill` in a shell form (none may be silent),
/// the second is lines the guard has no business with (printed, not asserted,
/// so a new question on one of them shows in the comparison).
#[test]
#[ignore = "sends real signals; runs only inside the W4a container: tests/fixtures/adv_w4a/run.sh"]
fn shell_shapes_are_judged_as_before_or_louder() {
    container_only();
    let mut quiet = Vec::new();
    for command in [
        "f() { pkill -f gitpulse-short; }; f",
        "case x in x) pkill -f gitpulse-short;; esac",
        "pkill -f gitpulse-short <<< ''",
        "pkill -f gitpulse-short 2>/dev/null",
        "sudo -u runner pkill -f gitpulse-short",
        "sudo -n pkill -f gitpulse-short",
        "doas pkill -f gitpulse-short",
        "env -i pkill -f gitpulse-short",
        "env -u X pkill -f gitpulse-short",
        "env X=1 Y=2 pkill -f gitpulse-short",
        "nice -n 5 pkill -f gitpulse-short",
        "nohup setsid pkill -f gitpulse-short",
        "timeout -s KILL 5 pkill -f gitpulse-short",
        "command -p pkill -f gitpulse-short",
        "builtin command pkill -f gitpulse-short",
        "\\pkill -f gitpulse-short",
        "/usr/bin/env pkill -f gitpulse-short",
        "bash -c 'bash -c \"pkill -f gitpulse-short\"'",
        "sh -c 'echo a; pkill -f gitpulse-short'",
        "bash -lc 'pkill -f gitpulse-short'",
        "zsh -c 'pkill -f gitpulse-short'",
        "eval \"pkill -f gitpulse-short\"",
        "eval 'pkill' '-f' 'gitpulse-short'",
        "(cd /tmp && pkill -f gitpulse-short)",
        "{ echo x; pkill -f gitpulse-short; } | cat",
        "echo $(pkill -f gitpulse-short)",
        "echo `pkill -f gitpulse-short`",
        "x=$(echo $(pkill -f gitpulse-short))",
        "cat <(pkill -f gitpulse-short)",
        "for i in 1 2; do pkill -f gitpulse-short; done",
        "while true; do pkill -f gitpulse-short; break; done",
        "if true; then pkill -f gitpulse-short; fi",
        "true || pkill -f gitpulse-short",
        "false; pkill -f gitpulse-short",
        "pgrep -f gitpulse-short | xargs kill",
        "pgrep -f gitpulse-short | xargs -r kill -9",
        "pgrep -f gitpulse-short | head -1 | xargs kill",
        "kill $(pgrep -f gitpulse-short | head -1)",
        "kill -9 `pgrep -f gitpulse-short`",
        "kill -s TERM $(pgrep -f gitpulse-short)",
        "kill $(pidof gitpulse-short)",
        "killall -q gitpulse-short",
        "killall -s TERM gitpulse-short",
        "sudo killall gitpulse-short",
        "xargs -n1 pkill -f <<< gitpulse-short",
        "echo gitpulse-short | xargs pkill -f",
        "echo gitpulse-short | xargs -I{} pkill -f {}",
        "pkill -f gitpulse-short # ssh find alias busybox",
    ] {
        let said = kind(&check_command(command));
        eprintln!("case [forms2] `{}`: real reaches 1 of which decoys 1(short14); guard Ok(0 pids), {said:?}", command.escape_debug());
        if matches!(said, Kind::Silent | Kind::Notice) {
            quiet.push(command);
        }
    }
    assert!(quiet.is_empty(), "silent for: {quiet:?}");
    for command in [
        "ls -l",
        "echo kill",
        "echo pkill",
        "man pkill",
        "which killall",
        "command -v pkill",
        "type -a kill",
        "grep kill notes.txt",
        "git commit -m 'kill the bug'",
        "$EDITOR notes.txt",
        "exec \"$@\"",
        "$CC -o x y.c",
        "FOO=$HOME $TOOL --flag",
        "bash -c \"$script\"",
        "eval $setup",
        "alias ll='ls -l'",
        "alias",
        "unalias ll",
        "find . -name '*.rs' -exec grep -l todo {} +",
        "find . -type f -exec ls -l {} \\;",
        "find . -name killme.txt -print",
        "ssh build-host ls",
        "ssh localhost uptime",
        "busybox ls -l",
        "busybox --list",
        "docker kill web",
        "kill 99999",
        "kill -0 99999",
        "kill -l",
        "kill -s TERM 99999",
        "pgrep -f gitpulse-short",
        "pidof gitpulse-short",
        "pkill -0 -f gitpulse-short",
        "pkill -l",
        "killall -l",
        "killall -V",
        "sleep 1; echo done",
        "echo 'killall' | cat",
    ] {
        let said = kind(&check_command(command));
        eprintln!(
            "case [forms-harmless] `{}`: real reaches 0 of which decoys ; guard Ok(0 pids), {said:?}",
            command.escape_debug()
        );
    }
}

/// glibc `regcomp` (POSIX extended) is the tool's regular-expression engine and
/// the `regex` crate is the guard's. Wherever they read a pattern differently
/// the guard can count fewer processes than the tool picks, so each pattern is
/// run through both over every decoy, by name and over the full command line,
/// and the sets are compared pid by pid (a refusal for other reasons does not
/// hide a decoy the guard's own set lacks).
#[test]
#[ignore = "sends real signals; runs only inside the W4a container: tests/fixtures/adv_w4a/run.sh"]
fn glibc_and_rust_regular_expressions_pick_the_same_decoys() {
    container_only();
    let patterns = [
        r"gitpulse-short",
        r"gitpulse-sh.rt",
        r"gitpulse-sh\.rt",
        r"gitpulse-s[a-z]ort",
        r"gitpulse-[[:alpha:]]+$",
        r"[[:digit:]]",
        r"\D",
        r"\w+-short",
        r"\s",
        r"\S+ xyz",
        r"\bshort\b",
        r"\<short",
        r"short\>",
        r"\Bhort",
        r"^gitpulse",
        r"short$",
        r"^$",
        r"^",
        r"$",
        r"a|b",
        r"a|",
        r"|a",
        r"(a|gitpulse)-short",
        r"(short)+",
        r"s{1}hort",
        r"s{1,2}hort",
        r"s{,2}hort",
        r"s{2}hort",
        r"x*",
        r"x+",
        r"x?",
        r"x??",
        r"a**",
        r"\(",
        r"\)",
        r"\[",
        r"\]",
        r"\{",
        r"\}",
        r"\^",
        r"\$",
        r"\*",
        r"\+",
        r"\|",
        r"\\",
        r"\-",
        r"\/",
        r"\_",
        r"\e",
        r"\x41",
        r"\101",
        r"\0",
        r"\1",
        r"(s)\1",
        r"[]a]",
        r"[a-]",
        r"[[.hyphen.]]",
        r"[[=a=]]",
        r"[[:<:]]short",
        r"short[[:>:]]",
        r"[\w]",
        r"(?i)SHORT",
        r"(?:short)",
        r"(?=short)",
        r"(?<=gitpulse-)short",
        r"[[:upper:]]",
        r"[[:^lower:]]",
        r"gitpulse.short",
        r"gitpulse[-]short",
        r"git\pulse",
        r"\Qgitpulse\E",
        r"\Agitpulse",
        r"short\z",
        r"short\Z",
        r"é",
        r"x\u{e9}",
        r"[é]",
        r"x.e",
        r"(x)(é)",
        r"\p{L}",
        r"[[:alpha:]é]",
        r"gitpulse-x.{3}",
        r"gitpulse-x.{3}$",
        r"gitpulse-x.*",
        r"ÉÉ",
        r"É",
        r"é",
    ];
    let diffs = regex_diffs(&patterns);
    assert!(diffs.is_empty(), "{} regex difference(s)", diffs.len());
}

/// BROKE on 1ba087a0 (ft-1d9f44d5, class 1): patterns glibc and the `regex`
/// crate read differently, each of which left a process the tool picks out of
/// the guard's set. A backslash and a letter is that letter to glibc (`\a`,
/// `\n`, `\t` are `a`, `n`, `t`, not BEL, newline and tab) and `\d` is a
/// literal `d`: the guard asks about those and about a backslash in a bracket
/// expression, where it cannot say which process either reads. The others
/// (`\.`, `\?`, `[^]a]`, `.*`, `.+`) read the same, and differed only because
/// procps replaces control characters in what it matches: the guard now tries
/// the readings the tool may have.
#[test]
#[ignore = "sends real signals; runs only inside the W4a container: tests/fixtures/adv_w4a/run.sh"]
fn glibc_regular_expressions_that_read_differently_are_asked_about() {
    container_only();
    let patterns = [
        r"\d", r"\W", r"\.", r"\?", r"\a", r"\n", r"\t", r"[^]a]", r"[\d]", r"[\n]", r".*", r".+",
        r"\w", r"\s", r"\S", r"\b", r"\<", r"\>", r"\1", r"[a\]]", r"[[.a.]]", r"[[=a=]]", r"a{",
        r"a{x}", r"x\{1\}",
    ];
    let diffs = regex_diffs(&patterns);
    assert!(diffs.is_empty(), "{} regex difference(s)", diffs.len());
}

fn regex_diffs(patterns: &[&str]) -> Vec<String> {
    let mut diffs = Vec::new();
    let mut compared = 0;
    for pattern in patterns.iter().copied() {
        for flags in [
            vec!["-f"],
            vec![],
            vec!["-i"],
            vec!["-fi"],
            vec!["-x"],
            vec!["-fx"],
        ] {
            let mut args: Vec<&str> = vec!["-TERM"];
            args.extend(flags.iter().copied());
            args.push(pattern);
            let v = ask("pkill", &args, "TERM");
            compared += 1;
            let protected = lab().protected();
            let real: BTreeSet<i32> = v
                .real
                .intersection(&protected)
                .copied()
                .filter(|p| stat_of(*p).is_some_and(|(_, s)| s != 'Z'))
                .collect();
            if std::env::var_os("ADV_VERBOSE").is_some() {
                let guard = match &v.guard {
                    Ok(set) => format!("Ok({} pids)", set.len()),
                    Err(why) => format!("Err({why})"),
                };
                eprintln!(
                    "case [regex] `{}`: real reaches {} of which decoys {}; guard {}, {:?}",
                    v.command.escape_debug(),
                    v.real.len(),
                    describe(&real.iter().copied().collect::<Vec<_>>()),
                    guard.escape_debug(),
                    v.kind
                );
            }
            match &v.guard {
                Ok(set) => {
                    let missed: Vec<i32> = real.difference(set).copied().collect();
                    if !missed.is_empty() {
                        diffs.push(format!(
                            "REGEX-DIFF pkill {flags:?} {pattern:?}: the real tool reaches {} that the guard's set lacks; verdict {:?}",
                            describe(&missed),
                            v.kind
                        ));
                    }
                }
                Err(why) => {
                    if !real.is_empty() && matches!(v.kind, Kind::Silent | Kind::Notice) {
                        diffs.push(format!(
                            "REGEX-QUIET pkill {flags:?} {pattern:?}: real reaches {} and the guard answered `{why}` quietly",
                            describe(&real.iter().copied().collect::<Vec<_>>())
                        ));
                    }
                }
            }
        }
    }
    eprintln!(
        "regex: {compared} pattern/flag combinations compared, {} differ",
        diffs.len()
    );
    for d in &diffs {
        eprintln!("{d}");
    }
    diffs
}

#[test]
#[ignore = "sends real signals; runs only inside the W4a container: tests/fixtures/adv_w4a/run.sh"]
fn kill_of_a_pidof_substitution_by_name() {
    container_only();
    let mut tally = Tally::default();
    for args in [
        vec!["gitpulse-short".to_string()],
        vec!["gitpulse-abcdefg".to_string()],
        vec!["gitpulse-this-is-a-long-daemon-name".to_string()],
        vec!["-s".to_string(), "gitpulse-short".to_string()],
        vec!["-x".to_string(), "gitpulse-short".to_string()],
        vec!["-c".to_string(), "gitpulse-short".to_string()],
        vec![
            "-o".to_string(),
            "1".to_string(),
            "gitpulse-short".to_string(),
        ],
        vec!["otherprog".to_string()],
        vec!["bareother".to_string()],
        vec!["-gitpulse-login".to_string()],
        vec!["gitpulse-login".to_string()],
        vec!["renamedcomm".to_string()],
    ] {
        let refs: Vec<&str> = args.iter().map(String::as_str).collect();
        let v = ask("pidof", &refs, "TERM");
        tally.add("pidof", &v);
    }
    tally.finish("pidof");
}

fn base_of(dir: &str) -> &str {
    dir.rsplit('/').next().unwrap_or(dir)
}

/// BROKE on 1ba087a0 (ft-1d9f44d5, class 2). Linux `pidof` (sysvinit's
/// `killall5`) takes a word with a `/` in it for a path to an executable and
/// picks the processes that run that file, by whatever name (probed: through a
/// symbolic link, `//` and `./` and `..` in the path, a process whose `argv[0]`
/// is another path), so `kill $(pidof /usr/bin/claude)` reaches the process.
/// The guard compared the whole word with the process name and `argv[0]`'s last
/// component, found nothing, and said nothing. (`killall /path` is the same
/// shape and is asked about, by W4a.)
#[test]
#[ignore = "sends real signals; runs only inside the W4a container: tests/fixtures/adv_w4a/run.sh"]
fn kill_of_a_pidof_substitution_by_executable_path() {
    container_only();
    let dir = lab().dir.to_string_lossy().into_owned();
    let path = |name: &str| format!("{dir}/{name}");
    // A name that is not the executable's, for the same file.
    let link = path("innocuous-link");
    let _ = std::fs::remove_file(&link);
    std::os::unix::fs::symlink(path("gitpulse-short"), &link).expect("link to a decoy");
    let mut tally = Tally::default();
    for args in [
        vec![path("gitpulse-short")],
        vec![path("gitpulse-abcdefg")],
        vec![path("gitpulse-this-is-a-long-daemon-name")],
        vec![path("gitpulse-login")],
        vec![path("gitpulse-prctl")],
        // run as `/opt/other/otherprog`: only the file says what it is
        vec![path("gitpulse-aliased")],
        vec![link.clone()],
        vec![format!("{dir}//gitpulse-short")],
        vec![format!("{dir}/./gitpulse-short")],
        vec![format!("{dir}/../{}/gitpulse-short", base_of(&dir))],
        vec!["-x".to_string(), path("gitpulse-short")],
        vec!["-s".to_string(), path("gitpulse-short")],
        vec!["-o".to_string(), "1".to_string(), path("gitpulse-short")],
        vec!["unrelated-name".to_string(), path("gitpulse-short")],
    ] {
        let refs: Vec<&str> = args.iter().map(String::as_str).collect();
        let v = ask("pidof", &refs, "TERM");
        tally.add("pidof-path", &v);
    }
    tally.finish("pidof-path");
}

#[test]
#[ignore = "sends real signals; runs only inside the W4a container: tests/fixtures/adv_w4a/run.sh"]
fn kill_of_a_pgrep_substitution_and_pgrep_alone() {
    container_only();
    run_named(
        "pgrep-subst",
        vec![
            pg(&["gitpulse"]),
            pg(&["-f", "gitpulse"]),
            pg(&["-x", "gitpulse-short"]),
            pg(&["-u", "runner"]),
            pg(&["-U", "runner"]),
            pg(&["-fu", "runner", "gitpulse"]),
            pg(&["-f", "gitpulse", "-u", "runner"]),
            pg(&["-n", "gitpulse"]),
            pg(&["-o", "gitpulse"]),
            pg(&["-v", "gitpulse"]),
            pg(&["-A", "-f", "gitpulse"]),
            pg(&["--ignore-ancestors", "-f", "gitpulse"]),
            pg(&["-H", "gitpulse"]),
            pg(&["-w", "gitpulse"]),
            pg(&["-P", "1", "gitpulse"]),
            pg(&["-f", ""]),
            pg(&[""]),
            pg(&["."]),
            pg(&["-a", "gitpulse"]),
            pg(&["-l", "gitpulse"]),
        ],
    );
}

#[test]
#[ignore = "sends real signals; runs only inside the W4a container: tests/fixtures/adv_w4a/run.sh"]
fn pkill_alone_with_every_selector_and_no_pattern() {
    container_only();
    run_named(
        "selectors",
        vec![
            pk(&["-TERM", "-u", "runner"]),
            pk(&["-TERM", "-u", "1001"]),
            pk(&["-TERM", "-U", "1001"]),
            pk(&["-TERM", "--uid", "1001"]),
            pk(&["-TERM", "-u", "root"]),
            pk(&["-TERM", "-u", "0"]),
            pk(&["-TERM", "-U", "0"]),
            pk(&["-TERM", "-u", "runner,root"]),
            pk(&["-TERM", "-u", "root,runner"]),
            pk(&["-TERM", "-n"]),
            pk(&["-TERM", "-o"]),
            pk(&["-TERM", "-n", "-f", "tiemark"]),
            pk(&["-TERM", "-o", "-f", "tiemark"]),
            pk(&["-TERM", "-P", "1"]),
            pk(&["-TERM", "-g", "1"]),
            pk(&["-TERM", "-s", "1"]),
            pk(&["-TERM", "-G", "1001"]),
            pk(&["-TERM", "-t", "pts/0"]),
            pk(&["-TERM", "-O", "1"]),
            pk(&["-TERM", "-r", "S"]),
            pk(&["-TERM", "-H"]),
            pk(&["-TERM", "-H", "-u", "runner"]),
            pk(&["-TERM", "-v"]),
            pk(&["-TERM", "--inverse"]),
            pk(&["-TERM", "--inverse", "nosuchthing"]),
            pk(&["-TERM", "-A", "-u", "runner"]),
            pk(&["-TERM", "--ignore-ancestors", "-u", "runner"]),
            pk(&["-TERM", ""]),
            pk(&["-TERM", "-f", ""]),
            pk(&["-TERM", "-x", ""]),
            pk(&["-TERM", ".*"]),
            pk(&["-TERM", "-x", ".*"]),
            pk(&["-TERM", "^"]),
            pk(&["-TERM", "$"]),
            pk(&["-TERM", "a|"]),
            pk(&["-TERM", "|a"]),
            pk(&["-TERM", "()"]),
            pk(&["-TERM", "-f", "gitpulse|nomatchzzq"]),
            pk(&["-TERM", "-q", "1", "gitpulse"]),
            pk(&["-TERM", "-q", "gitpulse"]),
            pk(&["-TERM", "-e", "gitpulse"]),
            pk(&["-TERM", "-c", "gitpulse"]),
            pk(&["-TERM", "-L", "gitpulse"]),
            pk(&["-TERM", "-F", "/nonexistent", "gitpulse"]),
            pk(&["-TERM", "gitpulse", "gitpulse-short"]),
            pk(&["-TERM", "gitpulse", "-x"]),
            pk(&["-TERM", "-V"]),
            pk(&["-TERM", "-h"]),
            pk(&["-TERM", "-V", "gitpulse"]),
            pk(&["-TERM", "-h", "gitpulse"]),
            pk(&["-TERM", "-?", "gitpulse"]),
            pk(&["-TERM", "--help", "gitpulse"]),
            pk(&["-TERM", "--version", "gitpulse"]),
            pk(&["-TERM", "--cgroup", "x", "gitpulse"]),
            pk(&["-TERM", "--ns", "1", "gitpulse"]),
            pk(&["-TERM", "--nslist", "pid", "--ns", "1"]),
            pk(&["-TERM", "--lightweight", "gitpulse"]),
            pk(&["-TERM", "--runstates", "S", "gitpulse"]),
            pk(&["-TERM", "--older", "1", "gitpulse"]),
            pk(&["-TERM", "--session", "1", "gitpulse"]),
            pk(&["-TERM", "--terminal", "pts/0", "gitpulse"]),
            pk(&["-TERM", "--group", "1001", "gitpulse"]),
            pk(&["-TERM", "--pgroup", "1", "gitpulse"]),
            pk(&["-TERM", "--queue", "1", "gitpulse"]),
            pk(&["-TERM", "--pidfile", "/nonexistent", "gitpulse"]),
            pk(&["-TERM", "--logpidfile", "gitpulse"]),
            pk(&["-TERM", "--count", "gitpulse"]),
            pk(&["-TERM", "--echo", "gitpulse"]),
            pk(&["-TERM", "--require-handler", "gitpulse"]),
        ],
    );
}

#[test]
#[ignore = "sends real signals; runs only inside the W4a container: tests/fixtures/adv_w4a/run.sh"]
fn killall_alone_with_every_option() {
    container_only();
    run_named(
        "killall-options",
        vec![
            ka(&["-TERM", "-u", "runner"]),
            ka(&["-TERM", "-u", "1001"]),
            ka(&["-TERM", "-u", "root", "gitpulse-short"]),
            ka(&["-TERM", "-u", "runner", "gitpulse-short"]),
            ka(&["-TERM", "-g", "gitpulse-short"]),
            ka(&["-TERM", "--process-group", "gitpulse-short"]),
            ka(&["-TERM", "-y", "1m", "gitpulse-short"]),
            ka(&["-TERM", "-o", "1s", "gitpulse-short"]),
            ka(&["-TERM", "-n", "pid", "gitpulse-short"]),
            ka(&["-TERM", "-Z", ".*", "gitpulse-short"]),
            ka(&["-TERM", "-q", "gitpulse-short"]),
            ka(&["-TERM", "-v", "gitpulse-short"]),
            ka(&["-TERM", "-l"]),
            ka(&["-TERM", "-l", "gitpulse-short"]),
            ka(&["-TERM", "-V", "gitpulse-short"]),
            ka(&["-TERM", "--version", "gitpulse-short"]),
            ka(&["-TERM", "-Ie", "GITPULSE-SHORT"]),
            ka(&["-TERM", "-eI", "GITPULSE-SHORT"]),
            ka(&["-TERM", "-I", "-e", "GITPULSE-SHORT"]),
            ka(&["-TERM", "--exact", "gitpulse-short"]),
            ka(&["-TERM", "--ignore-case", "GITPULSE-SHORT"]),
            ka(&["-TERM", "--regexp", "gitpulse-sh"]),
            ka(&["-TERM", "--reg", "gitpulse-sh"]),
            ka(&["-TERM", "-ignore", "GITPULSE-SHORT"]),
            ka(&["-TERM", "-regexp", "gitpulse-sh"]),
            ka(&["-TERM", "-r", ".*"]),
            ka(&["-TERM", "-r", ""]),
            ka(&["-TERM", "-r", "^"]),
            ka(&["-TERM", "-r", "gitpulse-(short|login)"]),
            ka(&["-TERM", "-r", "-I", "GITPULSE"]),
            ka(&["-TERM", ""]),
            ka(&["-TERM", "gitpulse-short", "gitpulse-login"]),
            ka(&["-TERM", "gitpulse-short", "nosuch"]),
            ka(&["-TERM", "nosuch", "gitpulse-short"]),
            ka(&["-TERM", "/tmp/nonexistent/gitpulse-short"]),
            ka(&["-TERM", "./gitpulse-short"]),
            ka(&["-TERM", "-r", "/gitpulse-short"]),
            ka(&["-TERM", "-s", "TERM", "gitpulse-short"]),
            ka(&["-TERM", "-s", "0", "gitpulse-short"]),
            ka(&["-s", "0", "gitpulse-short"]),
            ka(&["-0", "gitpulse-short"]),
        ],
    );
}

#[test]
#[ignore = "sends real signals; runs only inside the W4a container: tests/fixtures/adv_w4a/run.sh"]
fn a_pkill_that_reaches_its_callers_ancestors_is_counted_unless_a_is_given() {
    container_only();
    let lab = lab();
    assert!(
        !lab.ancestors.is_empty(),
        "run.sh must start this test under a gitpulse-named bash so an ancestor is a decoy"
    );
    let mut tally = Tally::default();
    for args in [
        vec!["-TERM", "-f", "gitpulse-anc"],
        vec!["-TERM", "-x", "gitpulse-anc"],
        vec!["-TERM", "gitpulse-anc"],
        vec!["-TERM", "-u", "runner"],
        vec!["-TERM", "-A", "-f", "gitpulse-anc"],
        vec!["-TERM", "--ignore-ancestors", "-f", "gitpulse-anc"],
        vec!["-TERM", "-fA", "gitpulse-anc"],
        vec!["-TERM", "-f", "gitpulse-anc", "-A"],
        vec!["-TERM", "-v", "gitpulse-nothing"],
    ] {
        let v = ask("pkill", &args, "TERM");
        tally.add("ancestors", &v);
    }
    let v = ask("killall", &["-TERM", "gitpulse-anc"], "TERM");
    tally.add("ancestors", &v);
    tally.finish("ancestors");
}

// ───────────────────────────── the fuzzers ──────────────────────────────────

struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 >> 12;
        self.0 ^= self.0 << 25;
        self.0 ^= self.0 >> 27;
        self.0.wrapping_mul(0x2545_F491_4F6C_DD1D)
    }
    fn below(&mut self, n: usize) -> usize {
        usize::try_from(self.next() % u64::try_from(n).expect("n")).expect("fits")
    }
    fn pick<'a, T>(&mut self, items: &'a [T]) -> &'a T {
        &items[self.below(items.len())]
    }
}

const PATTERNS: &[&str] = &[
    "gitpulse",
    "gitpulse-short",
    "gitpulse-abcdef",
    "gitpulse-abcdefg",
    "abcdef",
    "gitpulse-this-is-a-long",
    "gitpulse-this-is-a-long-daemon-name",
    "renamedcomm",
    "newtitle",
    "newtitle xyz",
    "-gitpulse-login",
    "gitpulse-login",
    "otherprog",
    "gitpulse-abspath",
    "UPPER",
    "upper",
    "GITPULSE",
    "first line",
    "needleatthe1mbend",
    "^gitpulse",
    "gitpulse-short$",
    ".*",
    "",
    "tiemark",
    "gitpulse-x\u{e9}\u{e9}",
    "x",
    "sleep",
    "nomatchzzq",
];

const PKILL_OPTIONS: &[&[&str]] = &[
    &["-f"],
    &["-x"],
    &["-i"],
    &["-n"],
    &["-o"],
    &["-A"],
    &["-c"],
    &["-e"],
    &["-L"],
    &["-H"],
    &["-fx"],
    &["-xf"],
    &["-fi"],
    &["-fn"],
    &["-fA"],
    &["-ix"],
    &["--inverse"],
    &["--full"],
    &["--exact"],
    &["--ignore-case"],
    &["--newest"],
    &["--oldest"],
    &["--ignore-ancestors"],
    &["--count"],
    &["--echo"],
    &["--ig"],
    &["--fu"],
    &["--ex"],
    &["--ne"],
    &["--in"],
    &["-u", "runner"],
    &["-u", "1001"],
    &["-u", "0"],
    &["-u", "root"],
    &["-urunner"],
    &["--euid", "runner"],
    &["--euid=runner"],
    &["--eu=runner"],
    &["-U", "runner"],
    &["-U", "0"],
    &["--uid=1001"],
    &["-P", "1"],
    &["--parent", "1"],
    &["-g", "1"],
    &["-G", "1001"],
    &["-s", "1"],
    &["-t", "pts/0"],
    &["-O", "10"],
    &["-r", "S"],
    &["-F", "/nonexistent"],
    &["-q", "1"],
    &["-fu", "runner"],
    &["-uf", "runner"],
    &["-fx", "-u", "runner"],
    &["-w"],
    &["-l"],
    &["-a"],
    &["-v"],
    &["-d", ","],
];

const PKILL_SIGNALS: &[&[&str]] = &[
    &["-@S@"],
    &["-SIG@S@"],
    &["--signal", "@S@"],
    &["--signal=@S@"],
    &["--sig", "@S@"],
];

const SIGS: &[&str] = &["TERM", "HUP", "USR1", "15", "1", "term"];

fn fuzz_args(
    rng: &mut Rng,
    options: &[&[&str]],
    signals: &[&[&str]],
    signal_odds: usize,
) -> Vec<String> {
    let mut chunks: Vec<Vec<String>> = Vec::new();
    for _ in 0..rng.below(4) {
        chunks.push(rng.pick(options).iter().map(|s| (*s).to_string()).collect());
    }
    if rng.below(100) < signal_odds {
        chunks.push(rng.pick(signals).iter().map(|s| (*s).to_string()).collect());
    }
    if rng.below(100) < 85 {
        let mut pattern = vec![(*rng.pick(PATTERNS)).to_string()];
        if rng.below(100) < 15 {
            pattern.insert(0, "--".into());
        }
        chunks.push(pattern);
    }
    // A second pattern now and then.
    if rng.below(100) < 8 {
        chunks.push(vec![(*rng.pick(PATTERNS)).to_string()]);
    }
    // Shuffle the chunks, keeping the words of each chunk together (so an option
    // keeps its argument unless the fuzzer's own `--` breaks it).
    for i in (1..chunks.len()).rev() {
        let j = rng.below(i + 1);
        chunks.swap(i, j);
    }
    chunks.into_iter().flatten().collect()
}

fn fuzz(
    prog: &'static str,
    seed: u64,
    count: usize,
    options: &[&[&str]],
    signals: &[&[&str]],
) -> usize {
    let mut rng = Rng(seed);
    let mut tally = Tally::default();
    let mut seen = BTreeSet::new();
    // `pgrep -c` prints a count and `-d` joins the pids: neither prints one
    // pid per line, which is how the oracle reads it.
    let options: Vec<&[&str]> = options
        .iter()
        .copied()
        .filter(|o| prog != "pgrep" || !o.iter().any(|w| matches!(*w, "-c" | "--count" | "-d")))
        .collect();
    for _ in 0..count {
        let args = fuzz_args(
            &mut rng,
            &options,
            signals,
            if prog == "pgrep" { 0 } else { 80 },
        );
        let sig = *rng.pick(SIGS);
        let refs: Vec<&str> = args.iter().map(String::as_str).collect();
        let v = ask(prog, &refs, sig);
        if seen.insert(v.command.clone()) {
            tally.add(&format!("{prog} seed {seed}"), &v);
        }
    }
    tally.report(&format!("fuzz-{prog}-{seed}"))
}

#[test]
#[ignore = "sends real signals; runs only inside the W4a container: tests/fixtures/adv_w4a/run.sh"]
fn fuzz_pkill_option_grammar() {
    container_only();
    let mut breaks = 0;
    for seed in [
        0x9E37_79B9_7F4A_7C15_u64,
        0xD1B5_4A32_D192_ED03,
        0x1234_5678_9ABC_DEF1,
    ] {
        breaks += fuzz("pkill", seed, 900, PKILL_OPTIONS, PKILL_SIGNALS);
    }
    assert!(breaks == 0, "{breaks} break(s)");
}

#[test]
#[ignore = "sends real signals; runs only inside the W4a container: tests/fixtures/adv_w4a/run.sh"]
fn fuzz_pgrep_option_grammar_under_kill_substitution() {
    container_only();
    let breaks = fuzz("pgrep", 0xA5A5_5A5A_1357_9BDF, 1200, PKILL_OPTIONS, &[]);
    assert!(breaks == 0, "{breaks} break(s)");
}

/// Pieces of a regular expression, and of what a command line holds, in the
/// syntax glibc's `regcomp` and the `regex` crate read alike and in the syntax
/// they do not.
const RE_PIECES: &[&str] = &[
    "gitpulse",
    "gitpulse-",
    "short",
    "sh",
    "ort",
    "x",
    "a",
    "b",
    "line",
    "second",
    "first",
    "tail",
    "caf",
    "newl",
    "empty",
    "blank",
    "sp",
    "-",
    " ",
    "  ",
    "\u{e9}",
    "\t",
    "\n",
    "\u{1}",
    ".",
    ".*",
    ".+",
    ".?",
    "x*",
    "a+",
    "b?",
    "[a-z]",
    "[^a-z]",
    "[[:alpha:]]",
    "[[:space:]]",
    "[[:print:]]",
    "[[:cntrl:]]",
    "[^]a]",
    "[a-]",
    "[.]",
    "[?]",
    "(a|b)",
    "(short|long)",
    "(x)",
    "^",
    "$",
    "|",
    "{1}",
    "{1,2}",
    "{,2}",
    "\\.",
    "\\?",
    "\\*",
    "\\\\",
    "\\(",
    "\\[",
    "\\d",
    "\\D",
    "\\w",
    "\\s",
    "\\b",
    "\\<",
    "\\>",
    "\\1",
    "\\n",
    "\\t",
    "\\a",
    "[\\d]",
    "[\\n]",
    "[[.a.]]",
    "[[=a=]]",
    "[a&&b]",
    "a{",
    "?",
    "*",
    "+",
    "(",
    ")",
    "[",
    "]",
    "{",
];

/// Patterns made of those pieces, against the real `pkill`: the guard's set
/// must hold every decoy the tool reaches, or it must have said it could not
/// tell (the question is the guard's way of declining a pattern the two
/// engines read differently, and an `Err` from `live_match_pids` is its set).
fn fuzz_regular_expressions(seed: u64, count: usize) -> usize {
    let mut rng = Rng(seed);
    let mut tally = Tally::default();
    let mut seen = BTreeSet::new();
    let mut set_misses = Vec::new();
    let flag_sets: [&[&str]; 8] = [
        &[],
        &["-f"],
        &["-x"],
        &["-i"],
        &["-fx"],
        &["-fi"],
        &["-xi"],
        &["-fxi"],
    ];
    for _ in 0..count {
        let pieces = 1 + rng.below(4);
        let pattern: String = (0..pieces).map(|_| *rng.pick(RE_PIECES)).collect();
        let flags = *rng.pick(&flag_sets);
        let mut args: Vec<&str> = vec!["-TERM"];
        args.extend(flags.iter().copied());
        if pattern.starts_with('-') {
            args.push("--");
        }
        args.push(&pattern);
        let v = ask("pkill", &args, "TERM");
        if !seen.insert(v.command.clone()) {
            continue;
        }
        tally.add(&format!("re-fuzz seed {seed}"), &v);
        let protected = lab().protected();
        let real: BTreeSet<i32> = v
            .real
            .intersection(&protected)
            .copied()
            .filter(|p| stat_of(*p).is_some_and(|(_, s)| s != 'Z'))
            .collect();
        if let Ok(set) = &v.guard {
            let missed: Vec<i32> = real.difference(set).copied().collect();
            if !missed.is_empty() {
                set_misses.push(format!(
                    "SET-MISS `{}`: the real tool reaches {} that the guard's set lacks; verdict {:?}",
                    v.command,
                    describe(&missed),
                    v.kind
                ));
            }
        }
    }
    for miss in set_misses.iter().take(30) {
        eprintln!("{}", miss.chars().take(420).collect::<String>());
    }
    eprintln!(
        "re-fuzz seed {seed}: {} distinct patterns, {} set misses",
        seen.len(),
        set_misses.len()
    );
    tally.report(&format!("fuzz-regex-{seed}")) + set_misses.len()
}

#[test]
#[ignore = "sends real signals; runs only inside the W4a container: tests/fixtures/adv_w4a/run.sh"]
fn fuzz_regular_expression_syntax_and_command_lines() {
    container_only();
    let mut problems = 0;
    for seed in [
        0xC0FF_EE00_1234_5678_u64,
        0x0BAD_CAFE_8765_4321,
        0xFEED_FACE_0F0F_0F0F,
    ] {
        problems += fuzz_regular_expressions(seed, 700);
    }
    assert!(problems == 0, "{problems} break(s) or set miss(es)");
}

const KILLALL_OPTIONS: &[&[&str]] = &[
    &["-e"],
    &["-I"],
    &["-r"],
    &["-q"],
    &["-v"],
    &["-g"],
    &["-y", "1m"],
    &["-o", "1s"],
    &["-n", "x"],
    &["-Z", ".*"],
    &["-u", "runner"],
    &["-u", "1001"],
    &["-u", "root"],
    &["-urunner"],
    &["--user", "runner"],
    &["--user=runner"],
    &["--exact"],
    &["--ignore-case"],
    &["--regexp"],
    &["--ig"],
    &["--reg"],
    &["-ignore"],
    &["-eI"],
    &["-Ie"],
    &["-er"],
    &["-re"],
    &["-rI"],
    &["-V"],
    &["-l"],
];

const KILLALL_SIGNALS: &[&[&str]] = &[
    &["-@S@"],
    &["-s", "@S@"],
    &["--signal", "@S@"],
    &["--signal=@S@"],
];

const KILLALL_NAMES: &[&str] = &[
    "gitpulse-short",
    "gitpulse-abcdef",
    "gitpulse-abcdefg",
    "gitpulse-abcdefgh",
    "gitpulse-this-is-a-long-daemon-name",
    "gitpulse-this-is-a-long-other-name",
    "gitpulse-this-is-a",
    "renamedcomm",
    "gitpulse-prctl",
    "gitpulse-login",
    "-gitpulse-login",
    "otherprog",
    "GITPULSE-SHORT",
    "gitpulse-x\u{e9}\u{e9}\u{e9}",
    "gitpulse-x\u{e9}\u{e9}\u{e9}-other",
    "gitpulse-x\u{e9}\u{e9}",
    "gitpulse",
    "gitpulse-sh",
    "^gitpulse-short$",
    "gitpulse.*",
    ".*",
    "",
    "nosuch",
];

#[test]
#[ignore = "sends real signals; runs only inside the W4a container: tests/fixtures/adv_w4a/run.sh"]
fn fuzz_killall_option_grammar() {
    container_only();
    let mut rng = Rng(0xC0FF_EE00_DEAD_BEEF);
    let mut tally = Tally::default();
    let mut seen = BTreeSet::new();
    for _ in 0..1800 {
        let mut chunks: Vec<Vec<String>> = Vec::new();
        for _ in 0..rng.below(4) {
            chunks.push(
                rng.pick(KILLALL_OPTIONS)
                    .iter()
                    .map(|s| (*s).to_string())
                    .collect(),
            );
        }
        if rng.below(100) < 70 {
            chunks.push(
                rng.pick(KILLALL_SIGNALS)
                    .iter()
                    .map(|s| (*s).to_string())
                    .collect(),
            );
        }
        for _ in 0..(1 + rng.below(100) / 90) {
            let mut name = vec![(*rng.pick(KILLALL_NAMES)).to_string()];
            if rng.below(100) < 15 {
                name.insert(0, "--".into());
            }
            chunks.push(name);
        }
        for i in (1..chunks.len()).rev() {
            let j = rng.below(i + 1);
            chunks.swap(i, j);
        }
        let args: Vec<String> = chunks.into_iter().flatten().collect();
        let sig = *rng.pick(SIGS);
        let refs: Vec<&str> = args.iter().map(String::as_str).collect();
        let v = ask("killall", &refs, sig);
        if seen.insert(v.command.clone()) {
            tally.add("killall", &v);
        }
    }
    tally.finish("fuzz-killall");
}

// ───────────────────── what a signal word means to the real tool ────────────

/// The victim: a fresh sink named `gitpulse-victim`, with its own log. Returns
/// its pid and log path once it is ready.
fn victim(n: usize) -> (i32, PathBuf) {
    let lab = lab();
    let sink = lab.dir.join("sink");
    let exe = lab.dir.join("gitpulse-victim");
    if !exe.exists() {
        std::fs::copy(&sink, &exe).expect("copy sink");
    }
    let log = lab.dir.join(format!("victim-{n}.log"));
    std::fs::write(&log, "").expect("victim log");
    let child = Command::new(&exe)
        .arg0("gitpulse-victim")
        .env("SINK_LOG", &log)
        .env("SINK_SECS", "60")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn_locked()
        .expect("spawn the victim");
    let pid = i32::try_from(child.id()).expect("pid");
    std::mem::forget(child);
    let started = Instant::now();
    while started.elapsed() < common::env::coverage_relaxed(Duration::from_secs(10)) {
        if read(log.to_str().expect("utf8")).contains("R ") {
            break;
        }
        std::thread::sleep(Duration::from_millis(2));
    }
    (pid, log)
}

fn kill_victim(pid: i32) {
    // SAFETY: a pid this test started, and SIGKILL.
    unsafe {
        libc::kill(pid, libc::SIGCONT);
        libc::kill(pid, libc::SIGKILL);
        // And reaped, so a thousand victims do not leave a thousand zombies
        // that the real tools list by name.
        let mut status = 0;
        libc::waitpid(pid, &mut status, 0);
    }
}

/// What reached the victim: the signal numbers its log holds, `Some(9)` when it
/// was killed, `Some(19)` when it was stopped, and whether the tool refused.
///
/// A signal is delivered when the tool returns, but the victim's handler runs
/// when the victim is next scheduled, so a tool that matched (`may_deliver`) is
/// followed by polling for up to 400 ms instead of a fixed sleep: a word the
/// tool read as a real signal must not be mistaken for signal 0 because its log
/// line had not landed yet. A tool that matched nothing is not waited for.
fn delivered(pid: i32, log: &Path, may_deliver: bool) -> Vec<i32> {
    let started = Instant::now();
    loop {
        let mut got: Vec<i32> = read(log.to_str().expect("utf8"))
            .lines()
            .filter_map(|l| l.strip_prefix('S')?.parse().ok())
            .collect();
        match stat_of(pid).map(|(_, s)| s) {
            None | Some('Z') => got.push(9),
            Some('T') => got.push(19),
            _ => {}
        }
        if !got.is_empty()
            || !may_deliver
            || started.elapsed() > common::env::coverage_relaxed(Duration::from_millis(400))
        {
            return got;
        }
        std::thread::sleep(Duration::from_millis(5));
    }
}

fn signal_words() -> Vec<String> {
    let names = [
        "HUP", "INT", "QUIT", "ILL", "TRAP", "ABRT", "IOT", "BUS", "FPE", "KILL", "USR1", "SEGV",
        "USR2", "PIPE", "ALRM", "TERM", "STKFLT", "CHLD", "CLD", "CONT", "STOP", "TSTP", "TTIN",
        "TTOU", "URG", "XCPU", "XFSZ", "VTALRM", "PROF", "WINCH", "IO", "POLL", "PWR", "SYS",
        "EMT", "INFO", "LOST", "UNUSED", "RTMIN", "RTMAX", "EXIT", "NULL", "ZERO", "NONE", "0",
    ];
    let mut words: Vec<String> = Vec::new();
    for name in names {
        words.push(name.to_string());
        words.push(format!("SIG{name}"));
        words.push(name.to_ascii_lowercase());
        words.push(format!("Sig{}", name.to_ascii_lowercase()));
    }
    for n in 0..=70 {
        words.push(n.to_string());
    }
    for n in [100, 127, 128, 129, 255, 256, 1000] {
        words.push(n.to_string());
    }
    for n in 0..=36 {
        words.push(format!("RTMIN+{n}"));
        words.push(format!("SIGRTMIN+{n}"));
        words.push(format!("rtmin+{n}"));
    }
    for n in 0..=5 {
        words.push(format!("RTMAX-{n}"));
        words.push(format!("RTMIN-{n}"));
    }
    for odd in [
        "00",
        "000",
        "0000000",
        "+0",
        "+9",
        "-0",
        "-9",
        " 9",
        "9 ",
        " 0",
        "0 ",
        "09",
        "009",
        "0x9",
        "0x0",
        "0b1",
        "1e0",
        "9.0",
        "9a",
        "a9",
        "RTMIN",
        "RTMIN+",
        "RTMIN+x",
        "SIG",
        "SIGSIG",
        "SIGSIGKILL",
        "KILL ",
        " KILL",
        "K",
        "T",
        "TE",
        "TER",
        "TERMX",
        "term1",
        "HUP1",
        "USR",
        "USR3",
        "USR0",
        "0 0",
        "0,0",
        "٠",
        "０",
        "9９",
    ] {
        words.push(odd.to_string());
    }
    words
}

fn real_signal_word_test(prog: &'static str) {
    let words = signal_words();
    let mut tally = Tally::default();
    let mut zero = 0;
    let mut invalid = 0;
    let mut nonzero = 0;
    for (n, word) in words.iter().enumerate() {
        let (pid, log) = victim(n);
        let flag = format!("-{word}");
        // The guard is asked first, while the victim is alive: a real KILL or
        // STOP changes what there is to ask about.
        let command = format!("{prog} {} gitpulse-victim", quote(&flag));
        let said = kind(&check_command(&command));
        let real = Command::new(format!("/usr/bin/{prog}"))
            .args([flag.as_str(), "gitpulse-victim"])
            .stdin(Stdio::null())
            .output_locked()
            .expect("run the real tool");
        let rc = real.status.code().unwrap_or(-1);
        let got = delivered(pid, &log, rc == 0);
        kill_victim(pid);
        let note = format!("word {word:?}: real rc {rc}, delivered {got:?}, guard {said:?}");
        if rc != 0 && got.is_empty() {
            invalid += 1;
            continue;
        }
        if got.is_empty() {
            zero += 1;
            continue;
        }
        nonzero += 1;
        // A real signal reached the victim, a GitPulse process by name: the
        // guard must not have called it silent.
        if matches!(said, Kind::Silent | Kind::Notice) {
            tally
                .breaks
                .push(format!("BROKE [{prog} signal word] `{command}`: {note}"));
        }
        tally.cases += 1;
    }
    eprintln!(
        "signal words via {prog}: {} words, {zero} zero-signal, {invalid} refused by the tool, {nonzero} delivered",
        words.len()
    );
    tally.finish(&format!("signal-words-{prog}"));
}

#[test]
#[ignore = "sends real signals; runs only inside the W4a container: tests/fixtures/adv_w4a/run.sh"]
fn every_word_the_real_pkill_reads_as_a_nonzero_signal_is_not_silent_to_the_guard() {
    container_only();
    real_signal_word_test("pkill");
}

#[test]
#[ignore = "sends real signals; runs only inside the W4a container: tests/fixtures/adv_w4a/run.sh"]
fn every_word_the_real_killall_reads_as_a_nonzero_signal_is_not_silent_to_the_guard() {
    container_only();
    real_signal_word_test("killall");
}

// ───────────────────────────── newest / oldest ──────────────────────────────

#[test]
#[ignore = "sends real signals; runs only inside the W4a container: tests/fixtures/adv_w4a/run.sh"]
fn newest_and_oldest_pick_the_process_the_real_tool_picks_even_among_equal_start_times() {
    container_only();
    let lab = lab();
    let decoy_exe = lab.dir.join("gitpulse-tie");
    let plain_exe = lab.dir.join("plainsink2");
    let _ = std::fs::remove_file(&decoy_exe);
    std::fs::copy(lab.dir.join("sink"), &decoy_exe).expect("copy");
    std::fs::copy(lab.dir.join("sink"), &plain_exe).expect("copy");
    let mut breaks = Vec::new();
    let mut ties = 0;
    for round in 0..12 {
        let mark = format!("tiemark{round:02}x");
        // A decoy and a plain process with the same words in their arguments,
        // started back to back so they share a start tick; both orders.
        let order: [(&Path, &str); 2] = if round % 2 == 0 {
            [(&decoy_exe, "gitpulse-tie"), (&plain_exe, "plainsink")]
        } else {
            [(&plain_exe, "plainsink"), (&decoy_exe, "gitpulse-tie")]
        };
        let mut pids = Vec::new();
        for (exe, name) in order {
            let child = Command::new(exe)
                .arg0(name)
                .arg(&mark)
                .env("SINK_SECS", "30")
                .stdin(Stdio::null())
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .spawn_locked()
                .expect("spawn");
            pids.push((i32::try_from(child.id()).expect("pid"), name));
            std::mem::forget(child);
        }
        std::thread::sleep(Duration::from_millis(250));
        let ticks: Vec<String> = pids
            .iter()
            .map(|(p, _)| {
                let text = read(&format!("/proc/{p}/stat"));
                let rest = &text[text.rfind(')').expect("stat") + 2..];
                rest.split_whitespace().nth(19).unwrap_or("?").to_string()
            })
            .collect();
        let tied = ticks[0] == ticks[1];
        ties += usize::from(tied);
        let decoy = pids
            .iter()
            .find(|(_, n)| *n == "gitpulse-tie")
            .expect("decoy")
            .0;
        for flag in ["-n", "-o"] {
            let (real, _) = run_real("pkill", &[flag.into(), "-f".into(), mark.clone()]);
            let guard = live_match_pids("pkill", &[flag, "-f", &mark])
                .map(|v| v.into_iter().collect::<BTreeSet<i32>>());
            let verdict = kind(&check_command(&format!("pkill {flag} -f {mark}")));
            let real_has = real.contains(&decoy);
            let guard_has = guard.as_ref().is_ok_and(|g| g.contains(&decoy));
            eprintln!(
                "round {round} {flag}: ticks {ticks:?} tied {tied}, decoy {decoy}, real {real:?}, guard {guard:?}, verdict {verdict:?}"
            );
            if real_has && !guard_has && matches!(verdict, Kind::Silent | Kind::Notice) {
                breaks.push(format!(
                    "BROKE pkill {flag} -f {mark}: the real tool picks the decoy {decoy} (ticks {ticks:?}), the guard picks {guard:?} and says {verdict:?}"
                ));
            }
        }
    }
    eprintln!(
        "newest/oldest: 12 rounds, {ties} with a shared start tick, {} BROKE",
        breaks.len()
    );
    for line in &breaks {
        eprintln!("{line}");
    }
    assert!(breaks.is_empty(), "{} break(s)", breaks.len());
}

/// Processes appear and exit while the table is read: a row whose `stat`,
/// owner, `status` or `cmdline` vanishes between two reads must neither
/// panic nor make a decoy that is alive throughout fall out of the answer.
#[test]
#[ignore = "sends real signals; runs only inside the W4a container: tests/fixtures/adv_w4a/run.sh"]
fn a_table_read_while_processes_come_and_go_still_counts_the_decoys() {
    container_only();
    let decoy = lab()
        .decoys
        .iter()
        .find(|(l, _)| *l == "short14")
        .expect("the short14 decoy")
        .1;
    let stop = std::sync::atomic::AtomicBool::new(false);
    let mut quiet = Vec::new();
    let mut reads = 0;
    std::thread::scope(|scope| {
        for _ in 0..3 {
            scope.spawn(|| {
                while !stop.load(std::sync::atomic::Ordering::Relaxed) {
                    let _ = Command::new("/bin/true")
                        .stdin(Stdio::null())
                        .stdout(Stdio::null())
                        .stderr(Stdio::null())
                        .status_locked();
                }
            });
        }
        let started = Instant::now();
        while started.elapsed() < common::env::coverage_relaxed(Duration::from_secs(25))
            && reads < 400
        {
            for command in [
                "pkill -f gitpulse-short",
                "killall gitpulse-short",
                "kill $(pgrep -x gitpulse-short)",
            ] {
                let said = kind(&check_command(command));
                if matches!(said, Kind::Silent | Kind::Notice) {
                    quiet.push(format!("{command}: {said:?}"));
                }
            }
            let got = live_match_pids("pkill", &["-f", "gitpulse-short"]);
            if !got.as_ref().is_ok_and(|g| g.contains(&decoy)) {
                quiet.push(format!("live_match_pids: {got:?}"));
            }
            reads += 1;
        }
        stop.store(true, std::sync::atomic::Ordering::Relaxed);
    });
    eprintln!("churn: {reads} rounds of 4 reads, {} quiet", quiet.len());
    assert!(quiet.is_empty(), "{quiet:?}");
}

// ───────────────────────────── the uid model ────────────────────────────────

/// Processes with every mix of real, effective and saved uid, started by
/// `tests/fixtures/adv_w4a/run-uid.sh` as root and listed in
/// `$ADV_UID_LAB/pids` (`label pid`). The hook is this process: run as the
/// ordinary user it asks which of them the real `pkill`/`killall` can signal
/// (`pkill -e` lists only a signal that was delivered, so `kill(2)`'s own rule
/// is the oracle), and the guard is asked about the same lines.
#[test]
#[ignore = "sends real signals; runs only inside the W4a container, and only under tests/fixtures/adv_w4a/run-uid.sh, which builds the uid lab"]
fn the_kill_rule_over_real_effective_and_saved_uids_against_the_real_tools() {
    container_only();
    let dir = std::env::var_os("ADV_UID_LAB").expect(
        "ADV_UID_LAB is not set: this test needs the uid lab tests/fixtures/adv_w4a/run-uid.sh builds",
    );
    let table = read(&format!("{}/pids", dir.to_string_lossy()));
    let lab: Vec<(String, i32)> = table
        .lines()
        .filter_map(|l| {
            let mut it = l.split_whitespace();
            Some((it.next()?.to_string(), it.next()?.parse().ok()?))
        })
        .collect();
    assert!(!lab.is_empty(), "no uid-lab decoys listed");
    // SAFETY: `geteuid` has no preconditions.
    let me = unsafe { libc::geteuid() };
    let mut breaks = Vec::new();
    let mut lines = Vec::new();
    let mut cases: Vec<(&'static str, Vec<String>)> = Vec::new();
    let strs = |a: &[&str]| a.iter().map(|s| (*s).to_string()).collect::<Vec<_>>();
    for (prog, args) in [
        ("pkill", strs(&["-TERM", "gitpulse-uid-"])),
        ("pkill", strs(&["-TERM", "-f", "marker-uid"])),
        // The one process this pattern reaches carries its marker only in its
        // arguments: a hook running as root reads the arguments of root's own
        // processes, not of another user's.
        ("pkill", strs(&["-TERM", "-f", "marker-uid-1$"])),
        ("pkill", strs(&["-TERM", "-f", "marker-uid-5$"])),
        ("pkill", strs(&["-TERM", "-f", "marker-uid-4$"])),
        (
            "pkill",
            strs(&["-TERM", "-x", "-f", "gitpulse-uid-[0-9]+ marker-uid-[0-9]+"]),
        ),
        ("pkill", strs(&["-TERM", "-u", "0", "gitpulse-uid-"])),
        ("pkill", strs(&["-TERM", "-u", "1001", "gitpulse-uid-"])),
        ("pkill", strs(&["-TERM", "-U", "0", "gitpulse-uid-"])),
        ("pkill", strs(&["-TERM", "-U", "1001", "gitpulse-uid-"])),
        ("pkill", strs(&["-TERM", "-u", "root", "-f", "marker-uid"])),
        ("pkill", strs(&["-TERM", "-U", "root", "-f", "marker-uid"])),
        (
            "pkill",
            strs(&["-TERM", "-u", "runner", "-f", "marker-uid"]),
        ),
        ("pkill", strs(&["-TERM", "-f", "marker-uid", "-u", "0"])),
        ("pkill", strs(&["-TERM", "-v", "-f", "nothing-like-this"])),
        (
            "pkill",
            strs(&["-TERM", "--inverse", "-u", "1001", "nothing-like-this"]),
        ),
        ("killall", strs(&["-TERM", "-r", "gitpulse-uid-"])),
        (
            "killall",
            strs(&["-TERM", "-u", "root", "-r", "gitpulse-uid-"]),
        ),
        (
            "killall",
            strs(&["-TERM", "-u", "runner", "-r", "gitpulse-uid-"]),
        ),
    ] {
        cases.push((prog, args));
    }
    for (label, _) in &lab {
        cases.push(("killall", strs(&["-TERM", label.as_str()])));
        cases.push(("killall", strs(&["-TERM", "-u", "runner", label.as_str()])));
        cases.push(("killall", strs(&["-TERM", "-u", "root", label.as_str()])));
        cases.push(("pkill", strs(&["-TERM", "-x", label.as_str()])));
        cases.push(("pkill", strs(&["-TERM", "-U", "0", "-x", label.as_str()])));
        cases.push(("pkill", strs(&["-TERM", "-u", "0", "-x", label.as_str()])));
    }
    let protected: BTreeSet<i32> = lab.iter().map(|(_, p)| *p).collect();
    let name_of = |pid: i32| {
        lab.iter()
            .find(|(_, p)| *p == pid)
            .map_or_else(|| format!("{pid}"), |(l, _)| format!("{pid}={l}"))
    };
    for (prog, args) in cases {
        let (real, _) = run_real(prog, &args);
        let quoted: Vec<String> = args.iter().map(|a| quote(a)).collect();
        let command = format!("{prog} {}", quoted.join(" "));
        let verdict = kind(&check_command(&command));
        let refs: Vec<&str> = args.iter().map(String::as_str).collect();
        let guard = live_match_pids(prog, &refs).map(|v| v.into_iter().collect::<BTreeSet<i32>>());
        let danger: Vec<i32> = real.intersection(&protected).copied().collect();
        let counted: Vec<i32> = guard
            .as_ref()
            .map(|g| g.intersection(&protected).copied().collect())
            .unwrap_or_default();
        lines.push(format!(
            "uid {me} `{command}`: real signals [{}], guard counts [{}] ({}), {verdict:?}",
            danger
                .iter()
                .map(|p| name_of(*p))
                .collect::<Vec<_>>()
                .join(","),
            counted
                .iter()
                .map(|p| name_of(*p))
                .collect::<Vec<_>>()
                .join(","),
            match &guard {
                Ok(_) => "ok".to_string(),
                Err(why) => why.chars().take(60).collect(),
            }
        ));
        // BROKE on 1ba087a0 as root (ft-1d9f44d5, class 4): a hook running as
        // root read the arguments of root's own processes only, so a pattern
        // that only another user's command line carries (`marker-uid-1$`) was
        // "not tested", a notice, while the real tool signalled the process.
        // The table now reads every process's arguments for a root sender.
        if !danger.is_empty() && matches!(verdict, Kind::Silent | Kind::Notice) {
            breaks.push(format!(
                "BROKE `{command}`: the real tool signals [{}] and the guard said {verdict:?} (counts [{}])",
                danger.iter().map(|p| name_of(*p)).collect::<Vec<_>>().join(","),
                counted.iter().map(|p| name_of(*p)).collect::<Vec<_>>().join(",")
            ));
        }
    }
    for line in &lines {
        eprintln!("{line}");
    }
    for line in &breaks {
        eprintln!("{line}");
    }
    eprintln!(
        "uid model as {me}: {} cases, {} BROKE",
        lines.len(),
        breaks.len()
    );
    assert!(breaks.is_empty(), "{} break(s)", breaks.len());
}
