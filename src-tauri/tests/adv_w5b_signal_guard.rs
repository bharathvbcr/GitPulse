//! W5b adversary pass over the signal guard (ft-1d9f44d5, lane commit a87a100c).
//!
//! Every test here states a claim of the lane and fails when the claim is
//! false, with the real tool as the oracle: decoy processes (copies of a
//! signal-logging `sink` under `gitpulse-*` names, which are GitPulse processes
//! to the guard) are started under many names and command lines, the real
//! procps `pgrep`, psmisc `killall`, sysvinit `pidof` or a real shell running
//! the very command line says which of them it reaches, and the guard is asked
//! about the same line. A command that reaches a decoy while the guard says
//! nothing (Silent or Notice) is a break.
//!
//! Companion of `adv_w4a_signal_guard.rs` (same container, same sink); see
//! `tests/fixtures/adv_w4a/run.sh`. Every test is `#[ignore]`d and refuses to
//! run outside the container, because the shell shapes send real signals.
//! The root tests also need the container started as root and `ADV_W5B_ROOT=1`.

#![cfg(target_os = "linux")]

mod common;

use gitpulse_lib::hooks::signal_guard::{check_command, live_match_pids};
use gitpulse_lib::hooks::{HookOutput, PermissionDecision};
use gitpulse_lib::procguard::LockedSpawn;
use std::collections::BTreeSet;
use std::ffi::OsString;
use std::os::unix::ffi::OsStringExt;
use std::os::unix::fs::PermissionsExt;
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::OnceLock;
use std::time::{Duration, Instant};

const SINK_SOURCE: &str = include_str!("fixtures/adv_w4a_sink.c");
const LINES: &str = include_str!("fixtures/adv_w5b_lines.txt");
const SHAPES: &str = include_str!("fixtures/adv_w5b_shapes.txt");

fn container_only() {
    assert!(
        std::env::var_os("ADV_W4A_CONTAINER").is_some(),
        "this test sends real signals and runs only inside the W4a container \
         (tests/fixtures/adv_w4a/run.sh sets ADV_W4A_CONTAINER); refusing to run here"
    );
    common::unmetered_spawns();
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

fn quiet(k: Kind) -> bool {
    matches!(k, Kind::Silent | Kind::Notice)
}

fn read(path: &Path) -> String {
    std::fs::read(path)
        .map(|b| String::from_utf8_lossy(&b).into_owned())
        .unwrap_or_default()
}

fn quote(word: &str) -> String {
    format!("'{}'", word.replace('\'', "'\\''"))
}

// ───────────────────────────── the lab ──────────────────────────────────────

fn sink() -> &'static Path {
    static SINK: OnceLock<PathBuf> = OnceLock::new();
    SINK.get_or_init(|| {
        let dir = std::env::temp_dir().join(format!("adv-w5b-sink-{}", std::process::id()));
        std::fs::create_dir_all(&dir).expect("sink directory");
        let source = dir.join("sink.c");
        std::fs::write(&source, SINK_SOURCE).expect("write the sink source");
        let exe = dir.join("sink");
        let built = Command::new("cc")
            .args(["-O1", "-o"])
            .arg(&exe)
            .arg(&source)
            .output_locked()
            .expect("run cc");
        assert!(
            built.status.success(),
            "cc failed: {}",
            String::from_utf8_lossy(&built.stderr)
        );
        exe
    })
}

/// A decoy to start: a copy of the sink under `exe`, run with the given
/// `argv[0]` (bare `exe` unless named), arguments (bytes, so invalid UTF-8 is
/// possible) and environment.
struct Want {
    label: &'static str,
    exe: String,
    argv0: Option<Vec<u8>>,
    args: Vec<Vec<u8>>,
    env: Vec<(&'static str, String)>,
    /// Remove the executable once the process runs it.
    delete_exe: bool,
    /// Run through this dynamic loader (`ld.so EXE ARGS`).
    loader: Option<String>,
}

impl Want {
    fn new(label: &'static str, exe: &str) -> Want {
        Want {
            label,
            exe: exe.to_string(),
            argv0: None,
            args: Vec::new(),
            env: Vec::new(),
            delete_exe: false,
            loader: None,
        }
    }
    fn argv0(mut self, a: &[u8]) -> Want {
        self.argv0 = Some(a.to_vec());
        self
    }
    fn arg(mut self, a: &[u8]) -> Want {
        self.args.push(a.to_vec());
        self
    }
    fn env(mut self, key: &'static str, value: &str) -> Want {
        self.env.push((key, value.to_string()));
        self
    }
    fn deleted(mut self) -> Want {
        self.delete_exe = true;
        self
    }
    fn via(mut self, loader: &str) -> Want {
        self.loader = Some(loader.to_string());
        self
    }
}

struct Decoy {
    label: &'static str,
    pid: i32,
    log: PathBuf,
}

struct Lab {
    dir: PathBuf,
    decoys: Vec<Decoy>,
}

fn lab_counter() -> usize {
    static N: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
    N.fetch_add(1, std::sync::atomic::Ordering::SeqCst)
}

impl Lab {
    fn start(wants: Vec<Want>) -> Lab {
        let dir =
            std::env::temp_dir().join(format!("adv-w5b-{}-{}", std::process::id(), lab_counter()));
        std::fs::create_dir_all(&dir).expect("lab directory");
        let mut decoys = Vec::new();
        for want in wants {
            let exe = dir.join(&want.exe);
            if let Some(parent) = exe.parent() {
                std::fs::create_dir_all(parent).expect("executable directory");
            }
            std::fs::copy(sink(), &exe).expect("copy the sink under its name");
            let log = dir.join(format!("{}.log", want.label));
            std::fs::write(&log, "").expect("decoy log");
            let argv0 = want
                .argv0
                .clone()
                .unwrap_or_else(|| want.exe.clone().into_bytes());
            let mut command = match &want.loader {
                Some(loader) => {
                    let mut c = Command::new(loader);
                    c.arg0(loader).arg(&exe);
                    c
                }
                None => {
                    let mut c = Command::new(&exe);
                    c.arg0(OsString::from_vec(argv0));
                    c
                }
            };
            for a in &want.args {
                command.arg(OsString::from_vec(a.clone()));
            }
            command
                .env("SINK_LOG", &log)
                .env("SINK_SECS", "900")
                .stdin(Stdio::null())
                .stdout(Stdio::null())
                .stderr(Stdio::null());
            for (key, value) in &want.env {
                command.env(key, value);
            }
            let child = command.spawn_locked().expect("start the decoy");
            let pid = i32::try_from(child.id()).expect("a pid fits an i32");
            std::mem::forget(child);
            let started = Instant::now();
            while started.elapsed() < common::env::coverage_relaxed(Duration::from_secs(20)) {
                if read(&log).contains("R ") {
                    break;
                }
                std::thread::sleep(Duration::from_millis(10));
            }
            if want.delete_exe {
                std::fs::remove_file(&exe).expect("remove the executable");
            }
            decoys.push(Decoy {
                label: want.label,
                pid,
                log,
            });
        }
        std::thread::sleep(Duration::from_millis(300));
        Lab { dir, decoys }
    }

    fn protected(&self) -> BTreeSet<i32> {
        self.decoys.iter().map(|d| d.pid).collect()
    }

    fn describe(&self, pids: &BTreeSet<i32>) -> String {
        let named: Vec<String> = pids
            .iter()
            .filter_map(|p| {
                self.decoys
                    .iter()
                    .find(|d| d.pid == *p)
                    .map(|d| format!("{}({})", p, d.label))
            })
            .collect();
        named.join(",")
    }

    fn decoy(&self, label: &str) -> &Decoy {
        self.decoys
            .iter()
            .find(|d| d.label == label)
            .unwrap_or_else(|| panic!("no decoy {label}"))
    }

    /// The command with `@DIR@` and `@D<label>@`-free placeholders replaced.
    fn text(&self, command: &str) -> String {
        command.replace("@DIR@", &self.dir.to_string_lossy())
    }
}

impl Drop for Lab {
    fn drop(&mut self) {
        for d in &self.decoys {
            // SAFETY: a pid this test started, and SIGKILL.
            unsafe {
                libc::kill(d.pid, libc::SIGKILL);
                let mut status = 0;
                libc::waitpid(d.pid, &mut status, 0);
            }
        }
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

impl Decoy {
    fn signals(&self) -> usize {
        read(&self.log)
            .lines()
            .filter(|l| l.starts_with('S'))
            .count()
    }
}

// ─────────────────────── the real tools, and the guard ──────────────────────

/// What a real procps `pgrep`, psmisc `killall` or sysvinit `pidof` reaches,
/// under `locale` (`LC_ALL`). `pkill` is asked as `pgrep`: the same selection,
/// and nothing is signalled.
fn real_set(prog: &str, args: &[String], locale: Option<&str>) -> BTreeSet<i32> {
    let (tool, mut real): (&str, Vec<String>) = match prog {
        "pkill" | "pgrep" => ("pgrep", Vec::new()),
        "killall" => ("killall", vec!["-v".to_string()]),
        other => (other, Vec::new()),
    };
    real.extend(args.iter().cloned());
    let mut command = Command::new(format!("/usr/bin/{tool}"));
    command.args(&real).stdin(Stdio::null());
    if let Some(locale) = locale {
        command.env("LC_ALL", locale);
    }
    let output = command.output_locked().expect("run the real tool");
    let mut text = String::from_utf8_lossy(&output.stdout).into_owned();
    text.push('\n');
    text.push_str(&String::from_utf8_lossy(&output.stderr));
    text.lines()
        .flat_map(|line| {
            let mut found = Vec::new();
            if tool == "killall" {
                if let Some(end) = line.rfind(") with signal") {
                    if let Some(start) = line[..end].rfind('(') {
                        found.extend(line[start + 1..end].parse::<i32>().ok());
                    }
                }
            } else {
                found.extend(
                    line.split_whitespace()
                        .filter_map(|w| w.parse::<i32>().ok()),
                );
            }
            found
        })
        .collect()
}

fn command_of(prog: &str, args: &[&str]) -> String {
    let quoted: Vec<String> = args.iter().map(|a| quote(a)).collect();
    format!("{prog} {}", quoted.join(" "))
}

/// One pattern case: the real tool under each locale against the guard.
/// Returns the break message, if any.
fn judge_set(
    lab: &Lab,
    tag: &str,
    prog: &str,
    args: &[&str],
    locales: &[Option<&str>],
) -> Option<String> {
    let owned: Vec<String> = args.iter().map(|a| (*a).to_string()).collect();
    let protected = lab.protected();
    let mut reached_in: Vec<String> = Vec::new();
    let mut real_all = BTreeSet::new();
    for locale in locales {
        let set: BTreeSet<i32> = real_set(prog, &owned, *locale)
            .intersection(&protected)
            .copied()
            .collect();
        if !set.is_empty() {
            reached_in.push(format!(
                "{}:{}",
                locale.unwrap_or("unset"),
                lab.describe(&set)
            ));
        }
        real_all.extend(set);
    }
    let command = command_of(prog, args);
    let said = kind(&check_command(&command));
    let probe = if prog == "pkill" { "pgrep" } else { prog };
    let guard = live_match_pids(probe, args);
    let guard_text = match &guard {
        Ok(set) => format!("Ok({} pids, decoys {})", set.len(), {
            let hit: BTreeSet<i32> = set
                .iter()
                .copied()
                .filter(|p| protected.contains(p))
                .collect();
            lab.describe(&hit)
        }),
        Err(why) => format!("Err({})", why.chars().take(60).collect::<String>()),
    };
    eprintln!(
        "case [{tag}] `{}`: real reaches {}; guard {guard_text}, {said:?}",
        command.escape_debug(),
        if reached_in.is_empty() {
            "no decoy".to_string()
        } else {
            reached_in.join(" ")
        }
    );
    (!real_all.is_empty() && quiet(said)).then(|| {
        format!(
            "BROKE [{tag}] `{}`: the real tool reaches {}; guard {guard_text}, {said:?}",
            command.escape_debug(),
            reached_in.join(" ")
        )
    })
}

fn finish(name: &str, cases: usize, breaks: Vec<String>) {
    eprintln!("{name}: {cases} cases, {} BROKE", breaks.len());
    for line in &breaks {
        eprintln!("{}", line.chars().take(500).collect::<String>());
    }
    assert!(breaks.is_empty(), "{name}: {} break(s)", breaks.len());
}

// ───────────────────── attack 1: regular expressions ────────────────────────

fn unicode_wants() -> Vec<Want> {
    vec![
        Want::new("alpha", "gitpulse-w5b-alpha").arg("caf\u{e9}".as_bytes()),
        Want::new("upper", "gitpulse-w5b-upper").arg("CAF\u{c9}".as_bytes()),
        Want::new("greek", "gitpulse-w5b-greek").arg("x\u{3b1}".as_bytes()),
        Want::new("cjk", "gitpulse-w5b-cjk").arg("x\u{4e2d}".as_bytes()),
        Want::new("emspace", "gitpulse-w5b-emspace").arg("a\u{2003}b".as_bytes()),
        Want::new("punct", "gitpulse-w5b-punct").arg("x\u{2014}".as_bytes()),
        Want::new("dotless", "gitpulse-w5b-dotless").argv0("g\u{131}tpulse-w5b-dotless".as_bytes()),
        Want::new("uname", "gitpulse-w5b-\u{e9}"),
    ]
}

/// Claim (header, class 1): the guard tries every subject in the readings
/// procps gives it and "asks about a pattern whose syntax the two engines read
/// differently", the only unmodelled things being locale collation order, other
/// locales and other releases. glibc's classes and its `-i` are Unicode-aware in
/// a UTF-8 locale (`[[:alpha:]]` matches a letter with an accent, `-i` folds the
/// dotless i to `I`), the `regex` crate's POSIX classes are ASCII only and its
/// case folding is simple folding: not asked about, not modelled, not stated.
#[test]
#[ignore = "sends real signals; runs only inside the W4a container: tests/fixtures/adv_w4a/run.sh"]
fn unicode_classes_and_case_folding_in_a_utf8_locale_are_modelled_or_asked() {
    container_only();
    let lab = Lab::start(unicode_wants());
    let utf8 = [Some("C.UTF-8")];
    let both = [Some("C.UTF-8"), Some("C")];
    let mut breaks = Vec::new();
    let mut cases = 0;
    let mut add = |case: Option<String>| {
        cases += 1;
        breaks.extend(case);
    };
    for (pat, locales) in [
        ("gitpulse-w5b-alpha caf[[:alpha:]]$", &utf8),
        ("gitpulse-w5b-alpha caf[[:alnum:]]$", &utf8),
        ("gitpulse-w5b-alpha caf[[:lower:]]$", &utf8),
        ("gitpulse-w5b-alpha caf[[:graph:]]$", &utf8),
        ("gitpulse-w5b-alpha caf[[:print:]]$", &utf8),
        ("gitpulse-w5b-upper CAF[[:upper:]]$", &utf8),
        ("gitpulse-w5b-greek x[[:alpha:]]$", &utf8),
        ("gitpulse-w5b-cjk x[[:alpha:]]$", &utf8),
        ("gitpulse-w5b-emspace a[[:space:]]b$", &utf8),
        ("gitpulse-w5b-emspace a[[:blank:]]b$", &utf8),
        ("gitpulse-w5b-punct x[[:punct:]]$", &utf8),
    ] {
        add(judge_set(
            &lab,
            "unicode-class",
            "pkill",
            &["-f", pat],
            locales,
        ));
    }
    for pat in [
        "gitpulse-w5b-dotless",
        "GITPULSE-W5B-DOTLESS",
        "gitpulse-w5b-dotl.ss",
    ] {
        add(judge_set(
            &lab,
            "dotless-i",
            "pkill",
            &["-f", "-i", pat],
            &utf8,
        ));
    }
    // by name: the executable's name has an accented letter (comm, 15 bytes)
    add(judge_set(
        &lab,
        "unicode-name",
        "pkill",
        &["gitpulse-w5b-[[:alpha:]]$"],
        &utf8,
    ));
    add(judge_set(
        &lab,
        "unicode-name",
        "killall",
        &["-r", "gitpulse-w5b-[[:alpha:]]$"],
        &utf8,
    ));
    add(judge_set(
        &lab,
        "unicode-name-icase",
        "pkill",
        &["-i", "GITPULSE-W5B-\u{c9}"],
        &both,
    ));
    finish("unicode-classes", cases, breaks);
}

/// Claim (class 1): "a blanked command line, control characters, invalid bytes
/// are read as procps reads them". Invalid UTF-8 that is more than one byte
/// long is `?` per byte to procps (and one per surrogate-or-overlong code point
/// in a UTF-8 locale); `String::from_utf8_lossy` replaces a maximal invalid
/// prefix with ONE character, so a pattern that counts characters (`x.{2}y`)
/// sees a different line than the tool does.
#[test]
#[ignore = "sends real signals; runs only inside the W4a container: tests/fixtures/adv_w4a/run.sh"]
fn invalid_utf8_in_a_command_line_is_counted_as_procps_counts_it() {
    container_only();
    let lab = Lab::start(vec![
        Want::new("trunc3", "gitpulse-w5b-t3").arg(b"x\xe2\x82y"),
        Want::new("trunc4", "gitpulse-w5b-t4").arg(b"x\xf0\x9f\x98y"),
        Want::new("overlong", "gitpulse-w5b-ov").arg(b"x\xc0\x80y"),
        Want::new("surrogate", "gitpulse-w5b-sg").arg(b"x\xed\xa0\x80y"),
        Want::new("toobig", "gitpulse-w5b-tb").arg(b"x\xf4\x90\x80\x80y"),
        Want::new("single", "gitpulse-w5b-s1").arg(b"x\xffy"),
    ]);
    let both = [Some("C.UTF-8"), Some("C")];
    let mut breaks = Vec::new();
    let mut cases = 0;
    for (name, pats) in [
        (
            "t3",
            vec![
                "gitpulse-w5b-t3 x.{2}y$",
                "gitpulse-w5b-t3 x\\?\\?y$",
                "gitpulse-w5b-t3 x..y$",
            ],
        ),
        (
            "t4",
            vec!["gitpulse-w5b-t4 x.{3}y$", "gitpulse-w5b-t4 x...y$"],
        ),
        ("ov", vec!["gitpulse-w5b-ov x..y$"]),
        ("sg", vec!["gitpulse-w5b-sg x.y$", "gitpulse-w5b-sg x...y$"]),
        (
            "tb",
            vec!["gitpulse-w5b-tb x.y$", "gitpulse-w5b-tb x....y$"],
        ),
        ("s1", vec!["gitpulse-w5b-s1 x.y$", "gitpulse-w5b-s1 x\\?y$"]),
    ] {
        for pat in pats {
            cases += 1;
            breaks.extend(judge_set(&lab, name, "pkill", &["-f", pat], &both));
        }
    }
    finish("invalid-utf8", cases, breaks);
}

/// Claim (class 1, with the old "procps reads about 128 KiB, the guard 1 MiB,
/// so the guard sees more" argument of W4a): procps cuts a command line at
/// 131071 bytes, so what an end-anchored pattern sees is the cut line. The
/// guard sees up to 1 MiB, so a `$` pattern that the cut line satisfies and the
/// whole line does not is a process the tool reaches and the guard does not.
#[test]
#[ignore = "sends real signals; runs only inside the W4a container: tests/fixtures/adv_w4a/run.sh"]
fn an_end_anchored_pattern_sees_the_command_line_as_procps_cuts_it() {
    container_only();
    let filler = vec![b'a'; 100_000];
    let lab = Lab::start(vec![Want::new("trunc", "gitpulse-w5b-trunc")
        .arg(&filler)
        .arg(&filler)
        .arg(b"END")]);
    let mut breaks = Vec::new();
    let both = [Some("C.UTF-8"), Some("C")];
    let mut cases = 0;
    for pat in [
        "gitpulse-w5b-trunc.*a$",
        "gitpulse-w5b-trunc a+ a+$",
        "a$",
        "^gitpulse-w5b-trunc.*a{10}$",
    ] {
        cases += 1;
        breaks.extend(judge_set(&lab, "cut-131071", "pkill", &["-f", pat], &both));
    }
    finish("end-anchor-cut", cases, breaks);
}

/// A seeded differential over a vocabulary of ERE atoms the lane's own fuzzer
/// does not have (repetition stacks, empty alternatives and groups, intervals
/// with no lower bound, brackets with `]`, `-`, classes and ranges in odd
/// places, escapes of metacharacters, anchors inside groups), against decoys
/// whose command lines are punctuation heavy. The real `pgrep -f` is the
/// oracle, in `C.UTF-8` and in `C`.
#[test]
#[ignore = "sends real signals; runs only inside the W4a container: tests/fixtures/adv_w4a/run.sh"]
fn fuzz_regular_expression_atoms_the_lane_did_not_try() {
    container_only();
    let lab = Lab::start(vec![
        Want::new("punct1", "gitpulse-w5b-p1")
            .arg(b"a.b")
            .arg(b"x[1]")
            .arg(b"p{2}")
            .arg(b"q|r")
            .arg(b"s^t")
            .arg(b"u$v"),
        Want::new("punct2", "gitpulse-w5b-p2")
            .arg(b"(w)")
            .arg(b"a+b")
            .arg(b"c*d")
            .arg(b"e?f")
            .arg(b"g\\h")
            .arg(b"i-j")
            .arg(b"k]l")
            .arg(b"m}n"),
        Want::new("ws", "gitpulse-w5b-ws")
            .arg(b"tab\there")
            .arg(b"  two  spaces"),
        Want::new("nl", "gitpulse-w5b-nl").arg(b"line1\nline2"),
        Want::new("digits", "gitpulse-w5b-dg")
            .arg(b"12345")
            .arg(b"v1.2.3-rc.4"),
        Want::new("case", "gitpulse-w5b-case")
            .arg(b"MixedCASE")
            .arg(b"ALLUP")
            .arg(b"alllow"),
    ]);
    let atoms: &[&str] = &[
        "a",
        "b",
        "x",
        "p",
        "q",
        "w",
        "1",
        "2",
        ".",
        "..",
        "[1]",
        "[]",
        "[]1]",
        "[^]1]",
        "[a-]",
        "[-a]",
        "[]-a]",
        "[a-z]",
        "[[:alpha:]]",
        "[[:digit:]]",
        "[[:alnum:]]",
        "[[:space:]]",
        "[[:punct:]]",
        "[[:upper:]]",
        "[[:lower:]]",
        "[^[:alpha:]]",
        "[[:alpha:][:digit:]]",
        "[[:alpha:]-]",
        "[[:alpha:]-z]",
        "[a-[:digit:]]",
        "[+--]",
        "[.]",
        "[*]",
        "[$]",
        "[(]",
        "[)]",
        "[|]",
        "[{]",
        "[}]",
        "[\\]",
        "\\.",
        "\\[",
        "\\]",
        "\\(",
        "\\)",
        "\\{",
        "\\}",
        "\\|",
        "\\^",
        "\\$",
        "\\*",
        "\\+",
        "\\?",
        "\\\\",
        "*",
        "+",
        "?",
        "**",
        "*?",
        "+*",
        "?*",
        "++",
        "{2}",
        "{1,}",
        "{,2}",
        "{0}",
        "{1,2}",
        "{2,1}",
        "{",
        "}",
        "{1",
        "{a}",
        "(",
        ")",
        "()",
        "(|)",
        "(a|)",
        "(|a)",
        "|",
        "||",
        "^",
        "$",
        "^^",
        "$$",
        "(^a)",
        "(a$)",
        "^*",
        "$*",
        "a^b",
        "a$b",
        " ",
        "-",
        "_",
        "=",
        ",",
        "/",
        "\t",
        "\n",
        "é",
        "ß",
        "İ",
        "ı",
    ];
    let (cases, breaks) = fuzz_atoms(&lab, atoms, 0x9e37_79b9_7f4a_7c15, 1500);
    finish("fuzz-atoms", cases, breaks);
}

/// `count` random patterns of `atoms`, each with one of the flag sets, the real
/// `pgrep -f` (two locales) against the guard, by pid and by verdict.
fn fuzz_atoms(lab: &Lab, atoms: &[&str], seed: u64, count: usize) -> (usize, Vec<String>) {
    let flags: &[&[&str]] = &[&["-f"], &["-f", "-i"], &["-f", "-x"], &["-f", "-v"]];
    let mut breaks = Vec::new();
    let mut cases = 0usize;
    let mut set_misses = 0usize;
    let locales = [Some("C.UTF-8"), Some("C")];
    let mut state: u64 = seed;
    let mut next = move || {
        state ^= state << 13;
        state ^= state >> 7;
        state ^= state << 17;
        state
    };
    let protected = lab.protected();
    for _ in 0..count {
        let n = 1 + (next() % 5) as usize;
        let mut pattern = String::new();
        for _ in 0..n {
            pattern.push_str(atoms[(next() as usize) % atoms.len()]);
        }
        if pattern.is_empty() {
            continue;
        }
        let fl = flags[(next() as usize) % flags.len()];
        let mut args: Vec<&str> = fl.to_vec();
        args.push("--");
        args.push(&pattern);
        cases += 1;
        // the guard's own set against the real set, by pid, for the decoys
        let owned: Vec<String> = args.iter().map(|a| (*a).to_string()).collect();
        let mut real = BTreeSet::new();
        for locale in &locales {
            real.extend(
                real_set("pgrep", &owned, *locale)
                    .intersection(&protected)
                    .copied(),
            );
        }
        if let Ok(set) = live_match_pids("pgrep", &args) {
            if real.iter().any(|p| !set.contains(p)) {
                set_misses += 1;
            }
        }
        if let Some(why) = judge_set(lab, "fuzz-atoms", "pkill", &args, &locales) {
            breaks.push(why);
        }
    }
    eprintln!("fuzz-atoms seed {seed:#x}: {cases} patterns, {set_misses} where the guard's own set lacks a decoy the tool reaches");
    (cases, breaks)
}

/// The same differential with ASCII atoms only, three seeds, more patterns:
/// whatever is left is the dialect difference proper, not the Unicode one above.
#[test]
#[ignore = "sends real signals; runs only inside the W4a container: tests/fixtures/adv_w4a/run.sh"]
fn fuzz_ascii_regular_expression_atoms_over_three_seeds() {
    container_only();
    let lab = Lab::start(vec![
        Want::new("punct1", "gitpulse-w5b-p1")
            .arg(b"a.b")
            .arg(b"x[1]")
            .arg(b"p{2}")
            .arg(b"q|r")
            .arg(b"s^t")
            .arg(b"u$v"),
        Want::new("punct2", "gitpulse-w5b-p2")
            .arg(b"(w)")
            .arg(b"a+b")
            .arg(b"c*d")
            .arg(b"e?f")
            .arg(b"g\\h")
            .arg(b"i-j")
            .arg(b"k]l")
            .arg(b"m}n"),
        Want::new("ws", "gitpulse-w5b-ws")
            .arg(b"tab\there")
            .arg(b"  two  spaces"),
        Want::new("nl", "gitpulse-w5b-nl").arg(b"line1\nline2"),
        Want::new("digits", "gitpulse-w5b-dg")
            .arg(b"12345")
            .arg(b"v1.2.3-rc.4"),
        Want::new("case", "gitpulse-w5b-case")
            .arg(b"MixedCASE")
            .arg(b"ALLUP")
            .arg(b"alllow"),
        Want::new("empty", "gitpulse-w5b-em")
            .arg(b"")
            .arg(b"after-empty"),
        Want::new("empty2", "gitpulse-w5b-e2")
            .arg(b"lead")
            .arg(b"")
            .arg(b"")
            .arg(b"tail")
            .arg(b"")
            .arg(b""),
        Want::new("ctl", "gitpulse-w5b-ctl")
            .arg(b"a\x01b")
            .arg(b"c\x7fd")
            .arg(b"e\x1bf"),
    ]);
    let atoms: &[&str] = &[
        "gitpulse",
        "-w5b-",
        "p1",
        "a",
        "b",
        "x",
        "p",
        "q",
        "w",
        "s",
        "u",
        "c",
        "d",
        "e",
        "f",
        "g",
        "h",
        "i",
        "j",
        "k",
        "l",
        "m",
        "n",
        "t",
        "v",
        "1",
        "2",
        "3",
        "4",
        "5",
        "A",
        "C",
        "M",
        "U",
        "Z",
        ".",
        "..",
        "[1]",
        "[]",
        "[]1]",
        "[^]1]",
        "[a-]",
        "[-a]",
        "[]-a]",
        "[a-z]",
        "[A-Z]",
        "[a-zA-Z]",
        "[[:alpha:]]",
        "[[:digit:]]",
        "[[:alnum:]]",
        "[[:space:]]",
        "[[:punct:]]",
        "[[:upper:]]",
        "[[:lower:]]",
        "[[:cntrl:]]",
        "[[:print:]]",
        "[[:graph:]]",
        "[[:xdigit:]]",
        "[[:blank:]]",
        "[^[:alpha:]]",
        "[[:alpha:][:digit:]]",
        "[[:alpha:]-]",
        "[[:alpha:]-z]",
        "[a-[:digit:]]",
        "[+--]",
        "[.]",
        "[*]",
        "[$]",
        "[(]",
        "[)]",
        "[|]",
        "[{]",
        "[}]",
        "[\\]",
        "[a\\]",
        "\\.",
        "\\[",
        "\\]",
        "\\(",
        "\\)",
        "\\{",
        "\\}",
        "\\|",
        "\\^",
        "\\$",
        "\\*",
        "\\+",
        "\\?",
        "\\\\",
        "\\-",
        "\\/",
        "\\a",
        "\\n",
        "\\t",
        "\\d",
        "\\w",
        "\\s",
        "\\b",
        "\\<",
        "\\>",
        "\\1",
        "\\'",
        "\\`",
        "*",
        "+",
        "?",
        "**",
        "*?",
        "+*",
        "?*",
        "++",
        "{2}",
        "{1,}",
        "{,2}",
        "{0}",
        "{1,2}",
        "{2,1}",
        "{",
        "}",
        "{1",
        "{a}",
        "(",
        ")",
        "()",
        "(|)",
        "(a|)",
        "(|a)",
        "|",
        "||",
        "^",
        "$",
        "^^",
        "$$",
        "(^a)",
        "(a$)",
        "^*",
        "$*",
        "a^b",
        "a$b",
        " ",
        "  ",
        "-",
        "_",
        "=",
        ",",
        "/",
        "\t",
        "\n",
        "\x01",
        "\x7f",
        "(?:",
        "(?i)",
        "(?s)",
        "(?P<n>",
        "[[.a.]]",
        "[[=a=]]",
        "[[:foo:]]",
        "[a&&b]",
        "[a--b]",
    ];
    let mut breaks = Vec::new();
    let mut total = 0;
    for seed in [
        0x1234_5678_9abc_def1_u64,
        0x0fed_cba9_8765_4321,
        0x5555_aaaa_3333_cccc,
    ] {
        let (cases, found) = fuzz_atoms(&lab, atoms, seed, 3000);
        total += cases;
        breaks.extend(found);
    }
    finish("fuzz-ascii-atoms", total, breaks);
}

/// Claim (class 1): "a pattern whose syntax the two engines read differently is
/// asked about, but only after a refusal: a Deny is never downgraded". A pattern
/// that carries an unfamiliar escape and whose evaluable part reaches a GitPulse
/// process must still be refused, in every position and under every selector;
/// and one that reaches nothing must be asked about, not passed.
#[test]
#[ignore = "sends no signals; runs only inside the W4a container: tests/fixtures/adv_w4a/run.sh"]
fn an_unfamiliar_escape_never_turns_a_refusal_into_a_question() {
    container_only();
    let lab = Lab::start(vec![
        Want::new("ord", "gitpulse-w5b-ord").arg(b"w5b-ord-marker")
    ]);
    let mut wrong = Vec::new();
    let cases: &[(&str, Kind)] = &[
        ("pkill -f 'w5b-ord-marker|\\d'", Kind::Deny),
        ("pkill -f '\\d|w5b-ord-marker'", Kind::Deny),
        ("pkill -f 'w5b-ord-marker\\w*'", Kind::Deny),
        ("pkill -f '\\bw5b-ord'", Kind::Deny),
        ("pkill -f -i 'W5B-ORD-MARKER|\\1'", Kind::Ask),
        (
            "pkill -fx 'gitpulse-w5b-ord w5b-ord-marker|\\d'",
            Kind::Deny,
        ),
        ("kill $(pgrep -f 'w5b-ord-marker|\\d')", Kind::Deny),
        ("pkill -u runner -f '\\bw5b-ord'", Kind::Deny),
        ("pkill -f 'w5b-ord-marker[\\d]'", Kind::Ask),
        ("pkill -f 'w5b-ord-marker{1'", Kind::Ask),
        ("pkill -f '\\dw5b-ord-marker-nothing'", Kind::Ask),
        ("pkill -f 'w5b-no-such-\\w+'", Kind::Ask),
        ("killall -r 'gitpulse-w5b-ord|\\d'", Kind::Deny),
        ("killall -r '\\dgitpulse-w5b-nothing'", Kind::Ask),
        ("pkill 'gitpulse-w5b-ord|\\d'", Kind::Deny),
    ];
    for (command, expect) in cases {
        let said = kind(&check_command(command));
        eprintln!(
            "case [ordering] `{}`: {said:?} (expected {expect:?}); {}",
            command.escape_debug(),
            lab.decoys.len()
        );
        if said != *expect {
            wrong.push(format!("`{command}` is {said:?}, expected {expect:?}"));
        }
    }
    assert!(wrong.is_empty(), "{wrong:#?}");
}

/// Degenerate patterns (the empty one, bare anchors, empty groups and
/// alternatives, a lone dot) against decoys with ordinary, blanked, empty-first
/// and control-character command lines: each matches every process or a process
/// with a special command line, and the real `pgrep` says which.
#[test]
#[ignore = "sends real signals; runs only inside the W4a container: tests/fixtures/adv_w4a/run.sh"]
fn degenerate_patterns_reach_what_the_real_tool_reaches() {
    container_only();
    let lab = Lab::start(vec![
        Want::new("plain", "gitpulse-w5b-dg1").arg(b"one two"),
        Want::new("noargs", "gitpulse-w5b-dg2"),
        Want::new("blank", "gitpulse-w5b-dg3")
            .arg(b"aaaa")
            .env("SINK_BLANK", "1"),
        Want::new("emptyfirst", "gitpulse-w5b-dg4")
            .arg(b"")
            .arg(b"x"),
        Want::new("ctl", "gitpulse-w5b-dg5").arg(b"a\x01b"),
    ]);
    let both = [Some("C.UTF-8"), Some("C")];
    let mut breaks = Vec::new();
    let mut cases = 0;
    for pat in [
        "",
        "^",
        "$",
        "^$",
        "()",
        "|",
        "a|",
        "|a",
        ".",
        ".*",
        "?",
        ".?",
        "^.*$",
        "(^)",
        "($)",
        "^()$",
        "x*",
        "[[:cntrl:]]",
        "[^[:print:]]",
        "\\?",
        "[?]",
    ] {
        for flags in [
            vec!["-f"],
            vec![],
            vec!["-x"],
            vec!["-f", "-x"],
            vec!["-f", "-i"],
            vec!["-f", "-v"],
            vec!["-v"],
        ] {
            let mut args = flags.clone();
            args.push("--");
            args.push(pat);
            cases += 1;
            breaks.extend(judge_set(&lab, "degenerate", "pkill", &args, &both));
        }
    }
    for pat in ["", ".*", "^$", "()", "a|"] {
        cases += 1;
        breaks.extend(judge_set(
            &lab,
            "degenerate-killall",
            "killall",
            &["-r", "--", pat],
            &both,
        ));
    }
    finish("degenerate", cases, breaks);
}

// ───────────────────── attack 2: pidof by path ──────────────────────────────

/// One shell command line, really run, against one decoy: does it signal it,
/// and what does the guard say about it?
fn shape_case(
    lab: &Lab,
    decoy: &Decoy,
    tag: &str,
    command: &str,
    cwd: &Path,
) -> (bool, Kind, String) {
    let text = lab.text(command);
    let said = kind(&check_command(&text));
    let before = decoy.signals();
    let _ = Command::new("/bin/bash")
        .arg("-c")
        .arg(&text)
        .current_dir(cwd)
        .stdin(Stdio::null())
        .output_locked()
        .expect("run the shell line");
    let started = Instant::now();
    let reached = loop {
        if decoy.signals() > before {
            break true;
        }
        if started.elapsed() > common::env::coverage_relaxed(Duration::from_millis(400)) {
            break false;
        }
        std::thread::sleep(Duration::from_millis(5));
    };
    eprintln!(
        "case [{tag}] `{}`: real shell {}; guard {said:?}",
        text.escape_debug(),
        if reached {
            "SIGNALS the decoy"
        } else {
            "does not signal it"
        }
    );
    (reached, said, text)
}

fn pidof_lab() -> Lab {
    let lab = Lab::start(vec![Want::new("exe", "gitpulse-w5b-exe")]);
    let exe = lab.dir.join("gitpulse-w5b-exe");
    std::fs::hard_link(&exe, lab.dir.join("hl")).expect("hard link");
    std::os::unix::fs::symlink(&exe, lab.dir.join("sl")).expect("symlink");
    std::fs::create_dir_all(lab.dir.join("sub")).expect("sub");
    std::os::unix::fs::symlink("../gitpulse-w5b-exe", lab.dir.join("sub/rl"))
        .expect("relative link");
    std::os::unix::fs::symlink(&lab.dir, lab.dir.join("dl")).expect("directory link");
    lab
}

/// Claim (class 2): `kill $(pidof /path)` is modelled: the path as written and
/// as it resolves, symbolic links followed. Stated limit: a hard link under
/// another path. Not stated: a relative path (the shell's directory is not the
/// hook's), a glob or a brace list (the shell expands them, the guard reads the
/// word), a script's redirect, a pid list read from a file.
#[test]
#[ignore = "sends real signals; runs only inside the W4a container: tests/fixtures/adv_w4a/run.sh"]
fn kill_of_a_pidof_substitution_by_path_in_every_spelling() {
    container_only();
    let lab = pidof_lab();
    let d = lab.decoy("exe");
    let cwd = lab.dir.clone();
    let mut breaks = Vec::new();
    // (command, stated in the header?)
    let shapes: &[(&str, bool)] = &[
        ("kill $(pidof @DIR@/gitpulse-w5b-exe)", false),
        ("kill $(pidof @DIR@/sl)", false),
        ("kill $(pidof @DIR@/dl/sl)", false),
        ("kill $(pidof @DIR@/dl/dl/gitpulse-w5b-exe)", false),
        ("kill $(pidof @DIR@/sub/../sl)", false),
        ("kill $(pidof @DIR@//sl)", false),
        ("kill $(pidof @DIR@/./sl)", false),
        ("kill $(pidof \"@DIR@/sl\")", false),
        ("kill `pidof @DIR@/sl`", false),
        ("kill -TERM $(pidof -s @DIR@/sl)", false),
        ("kill $(pidof -o 1 @DIR@/sl)", false),
        ("pidof @DIR@/sl | xargs kill", false),
        ("for p in $(pidof @DIR@/sl); do kill $p; done", false),
        // `killall` with a path (psmisc matches the executable)
        ("killall @DIR@/sl", false),
        ("killall @DIR@/gitpulse-w5b-exe", false),
        ("killall -s TERM @DIR@/sl", false),
        ("killall @DIR@/hl", false),
        // a hard link: stated limit (and, measured, not followed by pidof either)
        ("kill $(pidof @DIR@/hl)", true),
        // relative to the shell's directory, which is not the hook's
        ("cd @DIR@ && kill $(pidof ./sl)", false),
        ("cd @DIR@/sub && kill $(pidof ./rl)", false),
        ("cd @DIR@/sub && kill $(pidof ../sl)", false),
        ("cd @DIR@ && kill $(pidof dl/sl)", false),
        // expanded by the shell
        ("kill $(pidof @DIR@/gitpulse-w5b-e*)", false),
        ("kill $(pidof @DIR@/gitpulse-w5b-ex?)", false),
        ("kill $(pidof @DIR@/s[l])", false),
        ("kill $(pidof @DIR@/{sl,nothing})", false),
        ("kill $(pidof @DIR@/{gitpulse-w5b-exe,nothing})", false),
        // pids taken from a redirect or a file
        ("xargs kill < <(pidof @DIR@/sl)", false),
        ("pidof @DIR@/sl > @DIR@/pids; kill $(cat @DIR@/pids)", false),
        ("mapfile -t p < <(pidof @DIR@/sl); kill ${p[@]}", false),
    ];
    for (command, stated) in shapes {
        let (reached, said, text) = shape_case(&lab, d, "pidof", command, &cwd);
        if reached && quiet(said) {
            let line = format!(
                "BROKE [pidof{}] `{}`: signals the decoy, guard {said:?}",
                if *stated { ", stated limit" } else { "" },
                text.escape_debug()
            );
            if !*stated {
                breaks.push(line);
            } else {
                eprintln!("{line}");
            }
        }
    }
    finish("pidof-paths", shapes.len(), breaks);
}

/// Claim (header): `pidof -x NAME` is not modelled (a script's name). Probe it
/// so the report can say what it does to the guard: a shell running a script.
#[test]
#[ignore = "sends real signals; runs only inside the W4a container: tests/fixtures/adv_w4a/run.sh"]
fn pidof_x_for_a_script_run_by_a_gitpulse_named_shell() {
    container_only();
    let lab = Lab::start(vec![
        Want::new("sh", "gitpulse-w5b-sh").arg(b"w5b-script.sh")
    ]);
    let d = lab.decoy("sh");
    let cwd = lab.dir.clone();
    let mut breaks = Vec::new();
    let shapes = [
        "kill $(pidof -x w5b-script.sh)",
        "kill $(pidof -x @DIR@/w5b-script.sh)",
    ];
    for command in shapes {
        let (reached, said, text) = shape_case(&lab, d, "pidof-x", command, &cwd);
        if reached && quiet(said) {
            breaks.push(format!(
                "BROKE [pidof -x] `{}`: signals the decoy, guard {said:?}",
                text.escape_debug()
            ));
        }
    }
    finish("pidof-x", shapes.len(), breaks);
}

// ───────────────────── attack 3: programs the reader does not resolve ───────

/// One shape of class 3. `stated` is the header's reason when the limit is
/// listed there; `None` is a break the header does not own up to.
struct Shape {
    command: &'static str,
    stated: Option<&'static str>,
}

fn shape(command: &'static str) -> Shape {
    Shape {
        command,
        stated: None,
    }
}

fn limit(command: &'static str, why: &'static str) -> Shape {
    Shape {
        command,
        stated: Some(why),
    }
}

/// Claim (class 3): where the shape is visible the guard asks; everything else
/// is a limit the header states with its reason. Each line is run by a real
/// bash against a GitPulse-named decoy, and the guard is asked about the same
/// line: a line that signals the decoy with no word from the guard is a break,
/// and it is only excused when the header names the shape.
#[test]
#[ignore = "sends real signals; runs only inside the W4a container: tests/fixtures/adv_w4a/run.sh"]
fn every_real_signalling_shape_is_asked_about_or_a_stated_limit() {
    container_only();
    let lab = Lab::start(vec![Want::new("shape", "gitpulse-w5b-shape")]);
    let d = lab.decoy("shape");
    let cwd = lab.dir.clone();
    let shapes = vec![
        // controls: the reader resolves these
        shape("pkill -f gitpulse-w5b-shape"),
        shape("kill $(pgrep -f gitpulse-w5b-shape)"),
        shape("timeout 5 pkill -f gitpulse-w5b-shape"),
        shape("bash -c 'pkill -f gitpulse-w5b-shape'"),
        shape("nohup bash -c 'pkill -f gitpulse-w5b-shape' >/dev/null"),
        // the lane's claim: a variable program, on a line that says kill
        shape("p=pkill; $p -f gitpulse-w5b-shape"),
        shape("shopt -s expand_aliases\nalias k=pkill\nk -f gitpulse-w5b-shape"),
        shape("find /usr/bin -maxdepth 1 -name pkill -exec pkill -f gitpulse-w5b-shape \\;"),
        // the variable word is not written with the string `kill` in it
        shape("p=pk''ill; $p -f gitpulse-w5b-shape"),
        shape("p=pk\"\"ill; $p -f gitpulse-w5b-shape"),
        shape("p=pk; ${p}ill -f gitpulse-w5b-shape"),
        shape("p=pkil; ${p}l -f gitpulse-w5b-shape"),
        // a program built by a command, with no signal tool a word of the
        // command: a stated limit (the program is what the command prints)
        limit(
            "c=$(printf 'pk%s' ill); $c -f gitpulse-w5b-shape",
            "a program built by a command substitution, no signal tool a word of it",
        ),
        shape("P=/usr/bin/pk; ${P}ill -f gitpulse-w5b-shape"),
        shape("a=p; b=ki; c=ll; $a$b$c -f gitpulse-w5b-shape"),
        limit(
            "eval \"$(printf 'pk%s' ill) -f gitpulse-w5b-shape\"",
            "a program built by a command substitution, no signal tool a word of it",
        ),
        limit(
            "bash -c \"$(printf 'pk%s' ill) -f gitpulse-w5b-shape\"",
            "a program built by a command substitution, no signal tool a word of it",
        ),
        // the program word is not a plain word
        shape("$'\\x70kill' -f gitpulse-w5b-shape"),
        shape("$'p\\153ill' -f gitpulse-w5b-shape"),
        shape("{pkill,-f,gitpulse-w5b-shape}"),
        shape("/usr/bin/pk[i]ll -f gitpulse-w5b-shape"),
        shape("/usr/bin/pk?ll -f gitpulse-w5b-shape"),
        shape("/usr/bin/pk*ll -f gitpulse-w5b-shape"),
        shape("/usr/bin/p{k,k}ill -f gitpulse-w5b-shape"),
        // a script the shell is handed, not as `-c`
        shape("bash -o pipefail -c 'pkill -f gitpulse-w5b-shape'"),
        shape("bash -O extglob -c 'pkill -f gitpulse-w5b-shape'"),
        shape("bash --noprofile --norc -o errexit -c 'pkill -f gitpulse-w5b-shape'"),
        shape("bash -eu -o pipefail -c 'pkill -f gitpulse-w5b-shape'"),
        shape("echo 'pkill -f gitpulse-w5b-shape' | sh"),
        shape("echo 'pkill -f gitpulse-w5b-shape' | bash"),
        shape("printf 'pkill -f gitpulse-w5b-shape\\n' | sh -s"),
        shape("bash <<< 'pkill -f gitpulse-w5b-shape'"),
        shape("source <(echo 'pkill -f gitpulse-w5b-shape')"),
        limit(
            "printf 'pkill -f gitpulse-w5b-shape\\n' > @DIR@/s.sh; bash @DIR@/s.sh",
            "a script file the line writes and then runs",
        ),
        limit(
            "printf 'pkill -f gitpulse-w5b-shape\\n' > @DIR@/s2.sh; chmod +x @DIR@/s2.sh; @DIR@/s2.sh",
            "a script file the line writes and then runs",
        ),
        // wrappers that are not in the reader's list
        shape("ionice -c3 pkill -f gitpulse-w5b-shape"),
        shape("taskset -c 0 pkill -f gitpulse-w5b-shape"),
        shape("chrt -o 0 pkill -f gitpulse-w5b-shape"),
        shape("flock @DIR@/lock pkill -f gitpulse-w5b-shape"),
        shape("setpriv --no-new-privs pkill -f gitpulse-w5b-shape"),
        shape("unshare -r pkill -f gitpulse-w5b-shape"),
        limit("script -qc 'pkill -f gitpulse-w5b-shape' /dev/null", "script -c"),
        limit("env -S 'pkill -f gitpulse-w5b-shape'", "env -S"),
        limit("watch -n0.1 -t 'pkill -f gitpulse-w5b-shape' & sleep 0.5; kill $!", "watch"),
        // the program is another file named for pkill
        shape("ln -s /usr/bin/pkill @DIR@/xpkill; @DIR@/xpkill -f gitpulse-w5b-shape"),
        // the program is found by something else
        shape("find /usr/bin -maxdepth 1 -name pkill -exec {} -f gitpulse-w5b-shape \\;"),
        shape("find /usr/bin -maxdepth 1 -name pkill -execdir {} -f gitpulse-w5b-shape \\;"),
        shape("echo /usr/bin/pkill | xargs -I{} {} -f gitpulse-w5b-shape"),
        shape("command -v pkill | xargs -I{} {} -f gitpulse-w5b-shape"),
        shape("echo pkill | xargs -I{} sh -c '{} -f gitpulse-w5b-shape'"),
        // pids that come from somewhere the reader does not follow
        shape("for p in $(pgrep -f gitpulse-w5b-shape); do kill $p; done"),
        shape("pgrep -f gitpulse-w5b-shape | while read p; do kill $p; done"),
        shape("pids=$(pgrep -f gitpulse-w5b-shape); kill $pids"),
        shape("xargs kill < <(pgrep -f gitpulse-w5b-shape)"),
        shape("pgrep -f gitpulse-w5b-shape > @DIR@/pids; kill $(cat @DIR@/pids)"),
        shape("mapfile -t p < <(pgrep -f gitpulse-w5b-shape); kill \"${p[@]}\""),
        shape("kill $(pgrep -f gitpulse-w5b-shape | tr '\\n' ' ')"),
        shape("pgrep -f gitpulse-w5b-shape | xargs -r kill"),
        shape("kill $(ps -eo pid,args | awk '/gitpulse-w5b-shape/ {print $1}')"),
        shape("kill $(ps aux | grep gitpulse-w5b-shape | awk '{print $2}')"),
        // other programs that signal
        limit("python3 -c \"import os;[os.kill(int(x),15) for x in os.popen('pgrep -f gitpulse-w5b-shape').read().split()]\"", "other programs that signal"),
        limit("perl -e 'kill 15, split \" \", `pgrep -f gitpulse-w5b-shape`'", "other programs that signal"),
        limit("skill -TERM -c gitpulse-w5b-shape", "other programs that signal (skill)"),
    ];
    let mut breaks = Vec::new();
    let mut stated_hits = 0;
    for s in &shapes {
        let (reached, said, text) = shape_case(&lab, d, "shapes", s.command, &cwd);
        if reached && quiet(said) {
            match s.stated {
                None => breaks.push(format!(
                    "BROKE [shapes] `{}`: the real shell signals the decoy, guard {said:?}",
                    text.escape_debug()
                )),
                Some(why) => {
                    stated_hits += 1;
                    eprintln!(
                        "STATED [shapes] `{}`: signals the decoy, guard {said:?} ({why})",
                        text.escape_debug()
                    );
                }
            }
        }
    }
    eprintln!(
        "shapes: {} run, {stated_hits} silent but stated",
        shapes.len()
    );
    finish("shapes", shapes.len(), breaks);
}

// ───────────────────── attack 5: argv[0] spoof ──────────────────────────────

fn loader() -> String {
    [
        "/lib/ld-linux-aarch64.so.1",
        "/lib64/ld-linux-x86-64.so.2",
        "/lib/ld-linux-armhf.so.3",
    ]
    .into_iter()
    .find(|p| Path::new(p).exists())
    .expect("a dynamic loader")
    .to_string()
}

/// Claim (class 5): a GitPulse or agent executable run under another `argv[0]`
/// is still one, because the table carries `/proc/<pid>/exe`. Where that link is
/// not readable (a process that is not dumpable), already deleted (an agent
/// binary replaced by an update shows as `... (deleted)`), or the program runs
/// through the loader (`ld.so PROGRAM`), the identity falls back to a spelling
/// the process chose.
#[test]
#[ignore = "sends real signals; runs only inside the W4a container: tests/fixtures/adv_w4a/run.sh"]
fn a_spoofed_argv0_is_identified_by_the_executable_or_asked_about() {
    container_only();
    let lab = Lab::start(vec![
        Want::new("plain", "gitpulse-w5b-plain").argv0(b"/opt/other/plainprog"),
        Want::new("nodump", "gitpulse-w5b-nd")
            .argv0(b"/opt/other/ndprog")
            .env("SINK_NODUMP", "1"),
        Want::new("nodump-bare", "gitpulse-w5b-ndb")
            .argv0(b"ndbareprog")
            .env("SINK_NODUMP", "1"),
        Want::new("deleted-gitpulse", "gitpulse-w5b-del")
            .argv0(b"/opt/other/delprog")
            .deleted(),
        // An agent CLI is known by the executable's name, `claude`.
        Want::new("deleted-agent", "a1/claude")
            .argv0(b"claude-spoofed-del")
            .deleted(),
        Want::new("agent-plain", "a2/claude").argv0(b"/opt/other/agentprog"),
        Want::new("agent-bare", "a3/claude").argv0(b"claude"),
        // the binary was replaced by an update while the session runs: its
        // `/proc/<pid>/exe` reads `.../claude (deleted)`
        Want::new("deleted-agent-bare", "a4/claude")
            .argv0(b"claude")
            .arg(b"w5b-del-bare")
            .deleted(),
        Want::new("loader", "gitpulse-w5b-ld")
            .via(&loader())
            .arg(b"ldarg"),
    ]);
    let both = [Some("C.UTF-8"), Some("C")];
    let mut breaks = Vec::new();
    let mut cases = 0;
    for (tag, prog, args) in [
        ("spoof", "pkill", vec!["-f", "plainprog"]),
        ("spoof-by-comm", "pkill", vec!["gitpulse-w5b-plain"]),
        ("nodump", "pkill", vec!["-f", "ndprog"]),
        ("nodump-by-comm", "pkill", vec!["gitpulse-w5b-nd"]),
        ("nodump-bare", "pkill", vec!["-f", "ndbareprog"]),
        ("nodump-bare-by-comm", "pkill", vec!["gitpulse-w5b-ndb"]),
        ("deleted-gitpulse", "pkill", vec!["-f", "delprog"]),
        ("deleted-agent", "pkill", vec!["-f", "claude-spoofed-del"]),
        ("agent-by-exe", "pkill", vec!["-f", "agentprog"]),
        ("agent-bare-by-name", "pkill", vec!["^claude$"]),
        ("deleted-agent-bare-by-name", "pkill", vec!["^claude$"]),
        (
            "deleted-agent-bare-by-args",
            "pkill",
            vec!["-f", "w5b-del-bare"],
        ),
        ("loader", "pkill", vec!["-f", "gitpulse-w5b-ld"]),
        ("loader-arg", "pkill", vec!["-f", "ldarg"]),
    ] {
        cases += 1;
        breaks.extend(judge_set(&lab, tag, prog, &args, &both));
    }
    finish("argv0-spoof", cases, breaks);
}

// ───────────────────── attack 6: noise ──────────────────────────────────────

/// Not a claim of correctness: prints the guard's answer for each common
/// harmless line (`tests/fixtures/adv_w5b_lines.txt`) so two builds can be
/// diffed. A line the lane asks about that the base did not is noise.
#[test]
#[ignore = "prints verdicts only; runs anywhere the guard reads a process table, in the W4a container for the Linux numbers"]
fn harmless_command_lines_and_what_the_guard_says_about_them() {
    common::unmetered_spawns();
    let mut counts = [0usize; 4];
    for line in LINES.lines().filter(|l| !l.is_empty()) {
        let said = kind(&check_command(line));
        counts[said as usize] += 1;
        eprintln!("line [noise] `{}`: {said:?}", line.escape_debug());
    }
    eprintln!(
        "noise: {} lines, silent {}, notice {}, ask {}, deny {}",
        counts.iter().sum::<usize>(),
        counts[0],
        counts[1],
        counts[2],
        counts[3]
    );
    // The same lines, in every shape the attacks above use: printed so a
    // differential of two builds shows which became louder.
    for line in SHAPES.lines().filter(|l| !l.is_empty()) {
        eprintln!(
            "line [shapes] `{}`: {:?}",
            line.escape_debug(),
            kind(&check_command(line))
        );
    }
}

// ───────────────────── attack 7: the uid selector pid sets ──────────────────

fn state_of(pid: i32) -> Option<char> {
    let text = read(Path::new(&format!("/proc/{pid}/stat")));
    let rest = &text[text.rfind(')')? + 2..];
    rest.chars().next()
}

/// The lane called five smaller pid sets on `-u runner`, `-U 1001` and `--uid`
/// lines noise without proof. Here the real `pgrep` and the guard's own set are
/// taken back to back, many times, in a quiet lab, and each pid in the
/// difference is put into a cause: the test process itself (the real `pgrep`
/// does not leave its parent out, the guard leaves itself out), a zombie, a
/// process that is gone, or none of those. Run on 1ba087a0 and on the lane,
/// the two distributions are compared by `advdiff`-style tabulation of the
/// printed `uidset` lines.
#[test]
#[ignore = "sends no signals but needs the container's process table; run it on both builds"]
fn uid_selector_pid_sets_are_the_real_tools_sets_run_after_run() {
    container_only();
    let lab = Lab::start(
        (0..12)
            .map(|i| Want::new("uid", &format!("gitpulse-w5b-u{i}")))
            .collect(),
    );
    uidset_runs(&lab, "uidset");
}

/// The same with two decoys that each leave an unreaped child behind: the real
/// tool lists a zombie, the guard's table skips it (a signal to a zombie does
/// nothing), so the guard's set is smaller by exactly the zombies, on both
/// builds. This is the constant offset the lane's own suite shows beside its
/// `-u`/`-U`/`--uid` lines.
#[test]
#[ignore = "sends no signals but needs the container's process table; run it on both builds"]
fn uid_selector_pid_sets_with_zombie_children_differ_by_exactly_the_zombies() {
    container_only();
    let mut wants: Vec<Want> = (0..6)
        .map(|i| Want::new("uid", &format!("gitpulse-w5b-u{i}")))
        .collect();
    wants.push(Want::new("zomb1", "gitpulse-w5b-z1").env("SINK_ZOMBIE", "1"));
    wants.push(Want::new("zomb2", "gitpulse-w5b-z2").env("SINK_ZOMBIE", "1"));
    let lab = Lab::start(wants);
    uidset_runs(&lab, "uidset-z");
}

fn uidset_runs(lab: &Lab, tag: &str) {
    let me = i32::try_from(std::process::id()).expect("pid");
    let selectors: &[(&str, &[&str])] = &[
        ("u-runner", &["-u", "runner"]),
        ("U-1001", &["-U", "1001"]),
        ("uid-1001", &["--uid", "1001"]),
        ("u-1001-f", &["-u", "1001", "-f", "gitpulse"]),
        ("any", &["-f", "."]),
    ];
    let mut other_total = 0usize;
    for (name, args) in selectors {
        let owned: Vec<String> = args.iter().map(|a| (*a).to_string()).collect();
        for run in 0..40 {
            let real = real_set("pgrep", &owned, None);
            let guard: BTreeSet<i32> = match live_match_pids("pgrep", args) {
                Ok(set) => set.into_iter().collect(),
                Err(why) => {
                    eprintln!("{tag} [{name}] run={run}: guard Err({why})");
                    continue;
                }
            };
            let cause = |pid: i32| -> &'static str {
                if pid == me {
                    "self"
                } else if state_of(pid) == Some('Z') {
                    "zombie"
                } else if !Path::new(&format!("/proc/{pid}")).exists() {
                    "gone"
                } else {
                    "other"
                }
            };
            let mut only_real = [0usize; 4];
            let mut only_guard = [0usize; 4];
            let slot = |c: &str| {
                ["self", "zombie", "gone", "other"]
                    .iter()
                    .position(|x| *x == c)
                    .unwrap_or(3)
            };
            for pid in real.difference(&guard) {
                only_real[slot(cause(*pid))] += 1;
            }
            for pid in guard.difference(&real) {
                only_guard[slot(cause(*pid))] += 1;
            }
            other_total += only_real[3] + only_guard[3];
            eprintln!(
                "{tag} [{name}] run={run}: real={} guard={} only_real(self,zombie,gone,other)={:?} only_guard={:?}",
                real.len(),
                guard.len(),
                only_real,
                only_guard
            );
        }
    }
    eprintln!("{tag}: pids in a difference that are neither this process, a zombie nor gone: {other_total} ({} decoys)", lab.decoys.len());
}

// ───────────────────── attack 4: the root hook ──────────────────────────────

fn root_only() {
    container_only();
    // SAFETY: `geteuid` has no preconditions and cannot fail.
    assert!(
        unsafe { libc::geteuid() } == 0 && std::env::var_os("ADV_W5B_ROOT").is_some(),
        "this test needs the container started as root (ADV_USER=0) and ADV_W5B_ROOT=1"
    );
}

/// Starts `count` GitPulse-free processes of uid 1001 (a copy of the sink named
/// `fleet`) with the given arguments, returns their pids.
fn foreign_fleet(dir: &Path, count: usize, args: &[Vec<u8>]) -> Vec<i32> {
    let exe = dir.join("fleet");
    std::fs::copy(sink(), &exe).expect("copy the sink");
    let log = dir.join("fleet.log");
    std::fs::write(&log, "").expect("fleet log");
    let mut pids = Vec::new();
    for _ in 0..count {
        let mut command = Command::new("/usr/bin/setpriv");
        command
            .args(["--reuid=1001", "--regid=1001", "--clear-groups"])
            .arg(&exe);
        for a in args {
            command.arg(OsString::from_vec(a.clone()));
        }
        command
            .env("SINK_LOG", &log)
            .env("SINK_SECS", "900")
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null());
        let child = command.spawn_locked().expect("start a fleet process");
        pids.push(i32::try_from(child.id()).expect("pid"));
        std::mem::forget(child);
    }
    pids
}

fn reap(pids: &[i32]) {
    for pid in pids {
        // SAFETY: pids this test started, and SIGKILL.
        unsafe {
            libc::kill(*pid, libc::SIGKILL);
            let mut status = 0;
            libc::waitpid(*pid, &mut status, 0);
        }
    }
}

fn loadavg() -> String {
    read(Path::new("/proc/loadavg")).trim().to_string()
}

fn timed(command: &str, runs: usize) -> (Kind, Vec<u128>) {
    let mut said = Kind::Silent;
    let mut times = Vec::new();
    for _ in 0..runs {
        let started = Instant::now();
        said = kind(&check_command(command));
        times.push(started.elapsed().as_millis());
    }
    (said, times)
}

/// Claim (class 4): a root sender reads every process's arguments, bounded;
/// the whole table read stays inside the guard's 2 s budget
/// (`GUARD_BUDGET`, hooks/signal_guard.rs; `GATE_APP_DEADLINE`, 750 ms, is the
/// gate's app call and not this guard's) and a `-f` pattern aimed at another
/// user's GitPulse process is refused. Measures `check_command` over thousands
/// of processes of another user.
#[test]
#[ignore = "needs a root container with a raised pids limit: tmp/build-1026/w5b-adv/run-root.sh"]
fn root_hook_over_thousands_of_foreign_processes_refuses_and_stays_in_budget() {
    root_only();
    let lab = Lab::start(vec![]);
    let marker = vec![b'm'; 40];
    let mut padded = marker.clone();
    padded.extend_from_slice(b"-fleet-padding-0123456789abcdef0123456789abcdef0123456789abcdef");
    let fleet = foreign_fleet(&lab.dir, 3000, &[padded]);
    // a GitPulse-named process of another user whose arguments carry the target
    let exe = lab.dir.join("gitpulse-w5b-root");
    std::fs::copy(sink(), &exe).expect("copy");
    let log = lab.dir.join("root.log");
    std::fs::write(&log, "").expect("log");
    std::fs::set_permissions(&log, std::fs::Permissions::from_mode(0o666)).expect("chmod");
    let child = Command::new("/usr/bin/setpriv")
        .args(["--reuid=1001", "--regid=1001", "--clear-groups"])
        .arg(&exe)
        .arg("w5b-root-target-marker")
        .env("SINK_LOG", &log)
        .env("SINK_SECS", "900")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn_locked()
        .expect("start the target");
    let target = i32::try_from(child.id()).expect("pid");
    std::mem::forget(child);
    std::thread::sleep(Duration::from_millis(1500));
    let procs = std::fs::read_dir("/proc")
        .map(|d| {
            d.filter(|e| {
                e.as_ref()
                    .is_ok_and(|e| e.file_name().to_string_lossy().parse::<u32>().is_ok())
            })
            .count()
        })
        .unwrap_or(0);
    eprintln!("root: {procs} processes in the table, load {}", loadavg());
    let mut failures = Vec::new();
    for (tag, command, expect_refusal) in [
        ("target-in-args", "pkill -f w5b-root-target-marker", true),
        ("absent", "pkill -f w5b-absent-xyz", false),
        (
            "fleet-member",
            "pkill -f 'mmmmmmmmmmmmmmmmmmmmmmmmmmmmmmmmmmmmmmmm-fleet'",
            false,
        ),
    ] {
        let (said, times) = timed(command, 7);
        let worst = times.iter().copied().max().unwrap_or(0);
        eprintln!("root-latency [{tag}] `{command}`: {said:?}, ms {times:?}, worst {worst} ms");
        if expect_refusal && said != Kind::Deny {
            failures.push(format!(
                "[{tag}] `{command}` is {said:?}, not Deny, though it reaches a GitPulse process"
            ));
        }
        if worst > 2_000 {
            failures.push(format!("[{tag}] `{command}` took {worst} ms, past the 2 s budget: the hook would answer Ask for a timeout"));
        }
    }
    reap(&fleet);
    reap(&[target]);
    assert!(failures.is_empty(), "{failures:#?}");
}

/// Claim (class 4): argument bytes of other users' processes are read up to
/// 64 MiB and "rows past it stay unread, never Silent". 330 processes of
/// another user with 240 kB of arguments each are 79 MB; the target, started
/// last, is in the tail. Every answer must be Ask or Deny, never Silent/Notice,
/// and the table read must stay inside the 2 s budget.
#[test]
#[ignore = "needs a root container with a raised pids limit: tmp/build-1026/w5b-adv/run-root.sh"]
fn root_hook_past_the_argument_budget_is_asked_never_silent() {
    root_only();
    let lab = Lab::start(vec![]);
    let big = vec![b'a'; 120_000];
    let fleet = foreign_fleet(&lab.dir, 330, &[big.clone(), big]);
    let exe = lab.dir.join("gitpulse-w5b-tail");
    std::fs::copy(sink(), &exe).expect("copy");
    let log = lab.dir.join("tail.log");
    std::fs::write(&log, "").expect("log");
    std::fs::set_permissions(&log, std::fs::Permissions::from_mode(0o666)).expect("chmod");
    let child = Command::new("/usr/bin/setpriv")
        .args(["--reuid=1001", "--regid=1001", "--clear-groups"])
        .arg(&exe)
        .arg("w5b-tail-marker")
        .env("SINK_LOG", &log)
        .env("SINK_SECS", "900")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn_locked()
        .expect("start the target");
    let target = i32::try_from(child.id()).expect("pid");
    std::mem::forget(child);
    std::thread::sleep(Duration::from_millis(1500));
    eprintln!("root-budget: load {}", loadavg());
    let mut failures = Vec::new();
    for (tag, command) in [
        ("tail-by-args", "pkill -f w5b-tail-marker"),
        ("absent", "pkill -f w5b-absent-xyz"),
        ("tail-by-args-i", "pkill -if W5B-TAIL-MARKER"),
        ("kill-pgrep", "kill $(pgrep -f w5b-tail-marker)"),
        // patterns that make the guard scan every byte it read, not find a literal
        ("heavy-alt", "pkill -f '(a+)+b$'"),
        ("heavy-icase-class", "pkill -if '[a-z]+[0-9]+z'"),
        ("heavy-dots", "pkill -f '.*q.*q.*q$'"),
        ("heavy-unicode-word", "pkill -f '\\w+\\d+q$'"),
        ("heavy-unicode-icase", "pkill -if 'é+ı+ß'"),
    ] {
        let (said, times) = timed(command, 5);
        let worst = times.iter().copied().max().unwrap_or(0);
        eprintln!("root-budget [{tag}] `{command}`: {said:?}, ms {times:?}, worst {worst} ms");
        if quiet(said) {
            failures.push(format!(
                "[{tag}] `{command}` is {said:?}: rows past the budget were not asked about"
            ));
        }
        if worst > 2_000 {
            failures.push(format!(
                "[{tag}] `{command}` took {worst} ms, past the 2 s budget"
            ));
        }
    }
    reap(&fleet);
    reap(&[target]);
    assert!(failures.is_empty(), "{failures:#?}");
}
