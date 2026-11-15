use super::*;
use crate::sysload::lease::{Priority, MAX_SLOTS};
use crate::sysload::{Binding, PublishedVerdict, Tier};
use std::os::unix::fs::symlink;

fn os(words: &[&str]) -> Vec<OsString> {
    words.iter().map(OsString::from).collect()
}

fn verdict(tier: Tier, admit: bool) -> Load {
    Load::Verdict(PublishedVerdict {
        tier,
        binding: Some(Binding::Swap),
        admit,
        shed_background: false,
        shed_reactive: false,
    })
}

/// A clock that only moves when the wrapper sleeps, and a verdict that stays.
struct Fake {
    now: u64,
    load: Load,
    said: Vec<String>,
    slept: Duration,
}

impl Fake {
    fn new(load: Load) -> Self {
        Self {
            now: 1_000_000_000,
            load,
            said: Vec::new(),
            slept: Duration::ZERO,
        }
    }
}

impl Ports for Fake {
    fn load(&mut self) -> Load {
        self.load.clone()
    }
    fn now_ns(&self) -> u64 {
        self.now
    }
    fn sleep(&mut self, how_long: Duration) {
        self.now += how_long.as_nanos() as u64;
        self.slept += how_long;
    }
    fn say(&mut self, line: &str) {
        self.said.push(line.to_string());
    }
}

fn policy() -> Policy {
    Policy {
        slots: 2,
        wait_max: Duration::from_secs(60),
        wait_hard: Duration::from_secs(600),
        poll: Duration::from_secs(2),
        announce: Duration::from_secs(30),
    }
}

fn mine(now: u64) -> Record {
    Record {
        pid: i32::try_from(std::process::id()).unwrap(),
        start_us: 1,
        session_key: "agent:test".into(),
        priority: Priority::Observed,
        terminal_session: None,
        run_id: None,
        cwd: "/work/repo".into(),
        label: "cargo build".into(),
        enqueued_mono_ns: now,
        granted_mono_ns: None,
        tier_at_grant: None,
        verdict_unavailable_reason: None,
        overflow: false,
    }
}

fn dir() -> (tempfile::TempDir, Dir) {
    let temp = tempfile::tempdir().unwrap();
    let dir = Dir::open(&temp.path().join("leases")).unwrap();
    (temp, dir)
}

#[test]
fn it_runs_as_cargo_or_go_or_as_the_lease_subcommand_and_as_nothing_else() {
    assert_eq!(
        invocation(&os(&["cargo", "build", "--release"])),
        Some(("cargo".into(), os(&["build", "--release"])))
    );
    assert_eq!(
        invocation(&os(&["/x/lease-bin/go", "test", "./..."])),
        Some(("go".into(), os(&["test", "./..."])))
    );
    assert_eq!(
        invocation(&os(&["gitpulse-hook", "lease", "--", "cargo", "test"])),
        Some(("cargo".into(), os(&["test"])))
    );
    assert_eq!(
        invocation(&os(&[
            "gitpulse-hook",
            "lease",
            "/Users/x/.cargo/bin/cargo",
            "check"
        ])),
        Some(("cargo".into(), os(&["check"]))),
        "a program by path is named by its last segment"
    );
    for not_a_wrapper in [
        os(&["gitpulse-hook"]),
        os(&["gitpulse-hook", "command-gate"]),
        os(&["gitpulse-hook", "lease"]),
        os(&["gitpulse-hook", "lease", "--"]),
        os(&["gitpulse-hook", "--version"]),
        os(&[]),
    ] {
        assert_eq!(invocation(&not_a_wrapper), None, "{not_a_wrapper:?}");
    }
}

#[test]
fn the_real_program_is_found_past_the_wrappers_own_link_and_never_the_wrapper() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().canonicalize().unwrap();
    let (lease_bin, real_bin, other_bin) = (
        root.join("lease-bin"),
        root.join("real"),
        root.join("other"),
    );
    for d in [&lease_bin, &real_bin, &other_bin] {
        std::fs::create_dir(d).unwrap();
    }
    let hook = root.join("gitpulse-hook");
    std::fs::write(&hook, "#!/bin/sh\n").unwrap();
    std::fs::set_permissions(&hook, std::fs::Permissions::from_mode(0o755)).unwrap();
    symlink(&hook, lease_bin.join("cargo")).unwrap();
    // A second link to the wrapper elsewhere on PATH, which is not in the
    // skip list: found, recognised as the wrapper, passed over.
    symlink(&hook, other_bin.join("cargo")).unwrap();
    let real = real_bin.join("cargo");
    std::fs::write(&real, "#!/bin/sh\n").unwrap();
    std::fs::set_permissions(&real, std::fs::Permissions::from_mode(0o755)).unwrap();
    // A file that is not executable is not a program.
    let plain = other_bin.join("go");
    std::fs::write(&plain, "x").unwrap();

    let path = std::env::join_paths([&lease_bin, &other_bin, &real_bin]).unwrap();
    let found = resolve_program("cargo", &path, std::slice::from_ref(&lease_bin), &hook);
    assert_eq!(found, Some(real.clone()));
    // Without the skip list the wrapper's own link is still never chosen.
    assert_eq!(resolve_program("cargo", &path, &[], &hook), Some(real));
    assert_eq!(resolve_program("go", &path, &[], &hook), None);
    assert_eq!(resolve_program("cargo", OsStr::new(""), &[], &hook), None);
}

#[test]
fn the_build_starts_with_its_slot_held_marked_and_its_jobs_split() {
    let base = vec![
        (OsString::from("PATH"), OsString::from("/bin")),
        (OsString::from("CARGO_INCREMENTAL"), OsString::from("1")),
        (OsString::from("CARGO_BUILD_JOBS"), OsString::from("3")),
    ];
    let env = exec_env(
        base.clone(),
        &[
            ("GITPULSE_LEASE_HELD", "2".into()),
            ("CARGO_INCREMENTAL", "0".into()),
        ],
        &[("CARGO_BUILD_JOBS", "8".into()), ("OTHER", "x".into())],
    );
    let get = |name: &str| {
        env.iter()
            .find(|(k, _)| k == name)
            .map(|(_, v)| v.to_string_lossy().into_owned())
    };
    assert_eq!(get("GITPULSE_LEASE_HELD").as_deref(), Some("2"));
    assert_eq!(
        get("CARGO_INCREMENTAL").as_deref(),
        Some("0"),
        "set overrides"
    );
    assert_eq!(
        get("CARGO_BUILD_JOBS").as_deref(),
        Some("3"),
        "a default never overrides"
    );
    assert_eq!(get("OTHER").as_deref(), Some("x"));
    assert_eq!(
        env.iter().filter(|(k, _)| k == "CARGO_INCREMENTAL").count(),
        1
    );
    assert_eq!(jobs_for(18, 4), 4);
    assert_eq!(jobs_for(18, 1), 18);
    assert_eq!(jobs_for(4, 4), 2, "never below two");
    assert_eq!(jobs_for(8, 0), 8, "no division by zero");
    assert!(MAX_SLOTS >= 4);
}

#[test]
fn a_free_slot_is_granted_at_once_and_recorded_with_why_it_was_granted() {
    let (_temp, dir) = dir();
    let mut ports = Fake::new(verdict(Tier::Clear, true));
    let tpl = mine(ports.now);
    let Acquired::Granted {
        grant,
        waited,
        capacity,
    } = acquire(&dir, &tpl, &policy(), &mut ports).unwrap()
    else {
        panic!("a free slot is granted")
    };
    assert_eq!(grant.slot, 1);
    assert_eq!((waited, capacity.slots), (Duration::ZERO, 2));
    assert!(
        ports.said.is_empty(),
        "granted at once: nothing to announce"
    );
    assert_eq!(ports.slept, Duration::ZERO);
    let written: Record =
        serde_json::from_slice(&std::fs::read(dir.root().join("slot-1.json")).unwrap()).unwrap();
    assert_eq!(written.tier_at_grant, Some(Tier::Clear));
    assert_eq!(written.granted_mono_ns, Some(ports.now));
    assert!(!written.overflow);
    // The ticket is gone: a granted build is not also waiting.
    assert_eq!(
        std::fs::read_dir(dir.root().join("queue")).unwrap().count(),
        0
    );
}

#[test]
fn no_verdict_still_grants_and_records_why_it_was_not_load_checked() {
    let (_temp, dir) = dir();
    let mut ports = Fake::new(Load::Unknown("no readable host-load snapshot".into()));
    let tpl = mine(ports.now);
    let Acquired::Granted { capacity, .. } = acquire(&dir, &tpl, &policy(), &mut ports).unwrap()
    else {
        panic!("an unreadable verdict is not a refusal")
    };
    assert_eq!(capacity.slots, 2);
    let written: Record =
        serde_json::from_slice(&std::fs::read(dir.root().join("slot-1.json")).unwrap()).unwrap();
    assert_eq!(written.tier_at_grant, None);
    assert_eq!(
        written.verdict_unavailable_reason.as_deref(),
        Some("no readable host-load snapshot")
    );
}

#[test]
fn a_severe_verdict_keeps_a_build_waiting_until_the_hard_limit_and_builds_nothing() {
    let (_temp, dir) = dir();
    let mut ports = Fake::new(verdict(Tier::Severe, true));
    let tpl = mine(ports.now);
    let outcome = acquire(&dir, &tpl, &policy(), &mut ports).unwrap();
    let Acquired::GaveUp { waited } = outcome else {
        panic!("a refusing verdict gives up, it does not overflow: {outcome:?}")
    };
    assert!(waited >= Duration::from_secs(600), "{waited:?}");
    // It said where it was, more than once, and why no slot was free.
    assert!(ports.said.len() > 3, "{:?}", ports.said);
    assert!(
        ports.said[0].contains("1st of 1 in line"),
        "{}",
        ports.said[0]
    );
    assert!(ports.said[0].contains("severe"), "{}", ports.said[0]);
    // The wait was the hard limit and not the soft one: it never overflowed
    // at `wait_max` while the governor refused.
    assert!(waited > Duration::from_secs(60));
    assert_eq!(
        std::fs::read_dir(dir.root().join("queue")).unwrap().count(),
        0
    );
    assert!(!dir.root().join("slot-1.json").exists());
}

#[test]
fn an_elevated_verdict_halves_the_slots_a_build_may_take() {
    let (_temp, dir) = dir();
    let mut ports = Fake::new(verdict(Tier::Elevated, true));
    let tpl = mine(ports.now);
    let Acquired::Granted { capacity, .. } = acquire(&dir, &tpl, &policy(), &mut ports).unwrap()
    else {
        panic!("one slot of two still builds")
    };
    assert_eq!(capacity.slots, 1);
    assert!(capacity.note.unwrap().contains("halved"));
}

#[test]
fn the_waiting_line_names_the_place_the_holders_and_the_reason() {
    let capacity = crate::sysload::lease::capacity(4, &verdict(Tier::Elevated, true));
    let holder = |slot, pid| crate::sysload::lease::Holder {
        slot,
        pid,
        record: (pid != 7).then(|| Record {
            granted_mono_ns: Some(1_000_000_000),
            ..mine(1_000_000_000)
        }),
    };
    let line = waiting_line(
        1,
        3,
        &[holder(1, 10), holder(2, 7)],
        &capacity,
        721_000_000_000,
    );
    assert!(line.contains("2nd of 3 in line"), "{line}");
    assert!(line.contains("2 of 2 slots held"), "{line}");
    assert!(line.contains("cargo build in repo (pid 10, 12m)"), "{line}");
    assert!(
        line.contains("pid 7, unknown"),
        "a lock with no record is named as such: {line}"
    );
    assert!(line.contains("halved from 4"), "{line}");
    for (n, word) in [
        (1, "1st"),
        (2, "2nd"),
        (3, "3rd"),
        (4, "4th"),
        (11, "11th"),
        (12, "12th"),
        (21, "21st"),
        (112, "112th"),
    ] {
        assert_eq!(ordinal(n), word);
    }
}

#[test]
fn a_build_is_labelled_by_its_program_and_verb_and_nothing_it_could_inject() {
    assert_eq!(
        label_of("cargo", &os(&["+nightly", "--locked", "test", "-p", "x"])),
        "cargo test"
    );
    assert_eq!(label_of("go", &os(&["build", "./..."])), "go build");
    assert_eq!(label_of("cargo", &os(&[])), "cargo");
    assert_eq!(label_of("cargo", &os(&["te\nst;rm"])), "cargo te?st?rm");
}

#[test]
fn session_brief_writes_lease_path_to_claude_env_file() {
    let temp = tempfile::tempdir().unwrap();
    let config = temp.path().join("config");
    let hook = temp.path().join("gitpulse-hook");
    std::fs::write(&hook, "x").unwrap();
    let env_file = temp.path().join("env");
    std::fs::write(&env_file, "export KEEP=1\n").unwrap();

    // No links yet: nothing is written, and the brief says it is not gating.
    let route = route_session("sess-1", Some(env_file.as_os_str()), Some(&config), false);
    let Route::Skipped(why) = &route else {
        panic!("{route:?}")
    };
    assert!(why.contains("links"), "{why}");
    assert!(route.describe().contains("NOT gating"));
    assert_eq!(
        std::fs::read_to_string(&env_file).unwrap(),
        "export KEEP=1\n"
    );
    assert!(!session_has_path(&config, "sess-1"));

    lease::ensure_links(&config, &hook).unwrap();
    let route = route_session("sess-1", Some(env_file.as_os_str()), Some(&config), false);
    assert_eq!(route, Route::EnvFile);
    let written = std::fs::read_to_string(&env_file).unwrap();
    assert!(
        written.starts_with("export KEEP=1\n"),
        "appended, never replaced: {written}"
    );
    assert!(
        written.ends_with(&format!(
            "export PATH=\"{}:$PATH\"\n",
            lease::bin_dir(&config).display()
        )),
        "{written}"
    );
    assert!(session_has_path(&config, "sess-1"));
    assert!(!session_has_path(&config, "sess-2"), "per session");
    assert!(
        !session_has_path(&config, "../escape"),
        "only a plain token names a session"
    );

    // A host with no env file, and a session GitPulse launched.
    let none = route_session("sess-3", None, Some(&config), false);
    assert!(
        matches!(&none, Route::Skipped(w) if w.contains("CLAUDE_ENV_FILE")),
        "{none:?}"
    );
    assert!(!session_has_path(&config, "sess-3"));
    assert_eq!(
        route_session("sess-4", None, Some(&config), true),
        Route::Launched
    );
    assert!(session_has_path(&config, "sess-4"));
    // A folder that cannot be quoted safely is refused, not escaped.
    let hostile = temp.path().join("we\"ird$dir");
    lease::ensure_links(&hostile, &hook).unwrap();
    let route = route_session("sess-5", Some(env_file.as_os_str()), Some(&hostile), false);
    assert!(
        matches!(&route, Route::Skipped(w) if w.contains("quoted")),
        "{route:?}"
    );
}
