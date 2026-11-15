use super::*;
use crate::hooks::{command_gate_call_with, BYPASS_PERMISSIONS};
use crate::sysload::{Clock, Why, WriterIdentity};
use std::cell::Cell;

const SESSION: &str = "7b1f5c2e-1111-4222-8333-944455556666";

fn call(command: &str) -> HookInput {
    HookInput {
        hook_event_name: PRE_TOOL_USE.to_string(),
        tool_name: "Bash".to_string(),
        cwd: "/repo".to_string(),
        command: command.to_string(),
        session_id: SESSION.to_string(),
        tool_use_id: "toolu_01".to_string(),
        ..Default::default()
    }
}

fn verdict(tier: Tier, binding: Option<Binding>, admit: bool) -> Availability {
    Availability::Verdict {
        verdict: PublishedVerdict {
            tier,
            binding,
            admit,
            shed_background: tier >= Tier::Elevated,
            shed_reactive: tier == Tier::Severe,
        },
        state: SensorState::Fresh,
        age_ms: Some(1_000),
    }
}

fn gathered(availability: Availability, dir: &Path) -> Gathered {
    Gathered {
        mode: HostLoadHookMode::Enforce,
        exempt: false,
        availability,
        state_dir: dir.join("state"),
        now: SystemTime::now(),
        lease: LeaseSource::Fixed(LeaseFacts::default()),
    }
}

fn severe(dir: &Path) -> Gathered {
    gathered(verdict(Tier::Severe, Some(Binding::Swap), false), dir)
}

fn decision_of(output: &HookOutput) -> Option<PermissionDecision> {
    output
        .hook_specific_output
        .as_ref()
        .and_then(|s| s.permission_decision)
}

fn reason(output: &HookOutput) -> String {
    output
        .hook_specific_output
        .as_ref()
        .and_then(|s| s.permission_decision_reason.clone())
        .unwrap_or_default()
}

fn context(output: &HookOutput) -> String {
    context_of(output).unwrap_or_default()
}

fn judged(command: &str, g: &Gathered) -> HookOutput {
    judge(&call(command), || g.clone())
}

#[test]
fn a_command_that_is_not_in_the_table_adds_nothing_and_reads_nothing() {
    let dir = tempfile::tempdir().unwrap();
    let read = Cell::new(false);
    for command in [
        "git status",
        "cargo run",
        "cargo fmt",
        "echo \"cargo test\"",
        "ls",
    ] {
        let out = judge(&call(command), || {
            read.set(true);
            severe(dir.path())
        });
        assert!(out.is_silent(), "{command}: {out:?}");
    }
    assert!(
        !read.get(),
        "a call that is not heavy must not read the snapshot or the settings"
    );
    assert!(judge(&call(""), || unreachable!("no command, nothing to read")).is_silent());
}

#[test]
fn a_heavy_command_the_governor_refuses_is_refused_with_a_reason_the_model_can_act_on() {
    let dir = tempfile::tempdir().unwrap();
    let out = judged("cd src-tauri && cargo test --lib", &severe(dir.path()));
    assert_eq!(decision_of(&out), Some(PermissionDecision::Deny));
    let text = reason(&out);
    for must in [
        "cargo test",
        "severe",
        "swap in use is above",
        "wait a few minutes",
        "Do not retry in a loop",
        "another build tool",
        "CARGO_BUILD_JOBS=2",
        "Settings > Agents > Host load",
        "Advise or Off",
    ] {
        assert!(text.contains(must), "reason lacks {must:?}: {text}");
    }
    assert!(
        !text.contains("cd src-tauri"),
        "the command line is never echoed: {text}"
    );
    assert!(
        out.system_message.is_none(),
        "the first refusal does not pester the person"
    );
}

#[test]
fn every_binding_has_words_and_names_the_resource() {
    let dir = tempfile::tempdir().unwrap();
    for binding in [
        None,
        Some(Binding::MemoryPressure),
        Some(Binding::CompressorThrash),
        Some(Binding::CompressorSegments),
        Some(Binding::Swap),
        Some(Binding::LowDisk),
        Some(Binding::Thermal),
        Some(Binding::CpuLoad),
        Some(Binding::Battery),
    ] {
        let g = gathered(verdict(Tier::Severe, binding, false), dir.path());
        let out = judged("cargo build", &g);
        assert!(
            reason(&out).contains(binding_phrase(binding)),
            "{binding:?}"
        );
    }
}

#[test]
fn a_stale_recent_reading_still_refuses_and_says_how_old_it_is() {
    let dir = tempfile::tempdir().unwrap();
    let g = gathered(
        Availability::Verdict {
            verdict: PublishedVerdict {
                tier: Tier::Severe,
                binding: Some(Binding::MemoryPressure),
                admit: false,
                shed_background: true,
                shed_reactive: true,
            },
            state: SensorState::StaleRecent,
            age_ms: Some(45_000),
        },
        dir.path(),
    );
    let out = judged("cargo build", &g);
    assert_eq!(decision_of(&out), Some(PermissionDecision::Deny));
    assert!(
        reason(&out).contains("the reading is 45s old"),
        "{}",
        reason(&out)
    );
}

#[test]
fn advise_mode_says_the_same_and_refuses_nothing() {
    let dir = tempfile::tempdir().unwrap();
    let mut g = severe(dir.path());
    g.mode = HostLoadHookMode::Advise;
    let out = judged("cargo test", &g);
    assert_eq!(decision_of(&out), None);
    let text = context(&out);
    assert!(
        text.contains("would refuse this heavy command (cargo test)"),
        "{text}"
    );
    assert!(text.contains("set to Advise"), "{text}");
    assert!(text.contains("swap in use"), "{text}");
}

#[test]
fn off_adds_nothing_even_when_the_host_is_down() {
    let dir = tempfile::tempdir().unwrap();
    let mut g = severe(dir.path());
    g.mode = HostLoadHookMode::Off;
    assert!(judged("cargo test", &g).is_silent());
    assert!(judged("grep -r x .", &g).is_silent());
}

#[test]
fn an_elevated_but_admitted_host_gets_one_line_of_advice_with_the_capped_form() {
    let dir = tempfile::tempdir().unwrap();
    let g = gathered(
        verdict(Tier::Elevated, Some(Binding::CpuLoad), true),
        dir.path(),
    );
    let out = judged("cargo test", &g);
    assert_eq!(decision_of(&out), None);
    let text = context(&out);
    assert!(
        text.contains("elevated") && text.contains("CPU load average"),
        "{text}"
    );
    assert!(text.contains("CARGO_BUILD_JOBS=2"), "{text}");
    assert_eq!(text.lines().count(), 1, "{text}");
    // A clear host stays quiet.
    let quiet = gathered(verdict(Tier::Clear, None, true), dir.path());
    assert!(judged("cargo test", &quiet).is_silent());
}

#[test]
fn a_verdict_that_could_not_be_read_is_said_never_refused_and_never_silent() {
    let dir = tempfile::tempdir().unwrap();
    let g = gathered(Availability::Unavailable("no sampler".into()), dir.path());
    let out = judged("npm run test:browser", &g);
    assert_eq!(decision_of(&out), None);
    let text = context(&out);
    assert!(
        text.contains("did NOT load-check") && text.contains("no sampler"),
        "{text}"
    );
    assert!(!out.is_silent());
    // A line that could not be read at all says so too, and refuses nothing.
    let unreadable = judged("cargo test \"oops", &severe(dir.path()));
    assert_eq!(decision_of(&unreadable), None);
    assert!(context(&unreadable).contains("did NOT load-check"));
}

fn report(state: SensorState, reason: Option<&str>) -> SensorReport {
    SensorReport {
        state,
        age_ms: Some(match state {
            SensorState::Fresh => 1_000,
            SensorState::StaleRecent => 60_000,
            SensorState::StaleOld => 300_000,
            SensorState::Dead => 900_000,
        }),
        level: None,
        enabled: Some(true),
        reason: reason.map(str::to_string),
    }
}

fn snapshot_with(verdict: Reading<PublishedVerdict>, enabled: bool) -> Snapshot {
    let mut snapshot = Snapshot::empty(Clock::now(), WriterIdentity::default(), enabled);
    snapshot.verdict = verdict;
    snapshot
}

#[test]
fn the_sensor_state_decides_whether_a_verdict_may_be_acted_on() {
    let published = PublishedVerdict {
        tier: Tier::Severe,
        binding: Some(Binding::Swap),
        admit: false,
        shed_background: true,
        shed_reactive: true,
    };
    let snapshot = snapshot_with(Reading::Value(published), true);
    for state in [SensorState::Fresh, SensorState::StaleRecent] {
        assert!(
            matches!(
                availability_of(&report(state, None), Some(&snapshot)),
                Availability::Verdict { .. }
            ),
            "{state:?}"
        );
    }
    for state in [SensorState::StaleOld, SensorState::Dead] {
        match availability_of(&report(state, None), Some(&snapshot)) {
            Availability::Unavailable(why) => assert!(why.contains("s ago"), "{state:?}: {why}"),
            other => panic!("{state:?} must not give a verdict: {other:?}"),
        }
    }
    // Dead because nothing was ever written: the reader's own reason is used.
    match availability_of(&report(SensorState::Dead, Some("no sampler")), None) {
        Availability::Unavailable(why) => assert_eq!(why, "no sampler"),
        other => panic!("{other:?}"),
    }
    // Fresh, but no snapshot to read.
    assert!(matches!(
        availability_of(&report(SensorState::Fresh, None), None),
        Availability::Unavailable(_)
    ));
    // A version-1 record (the 664-byte writer) or a first tick carries no verdict.
    let older = snapshot_with(Reading::Unavailable(Why::Unsupported), true);
    match availability_of(&report(SensorState::Fresh, None), Some(&older)) {
        Availability::Unavailable(why) => assert!(why.contains("no governor verdict"), "{why}"),
        other => panic!("{other:?}"),
    }
    // The governor switched off: said, not refused.
    let off = snapshot_with(Reading::Value(published), false);
    match availability_of(&report(SensorState::Fresh, None), Some(&off)) {
        Availability::Unavailable(why) => assert!(why.contains("switched off"), "{why}"),
        other => panic!("{other:?}"),
    }
}

#[test]
fn the_holder_of_the_heavy_lane_passes_silently() {
    let dir = tempfile::tempdir().unwrap();
    let mut g = severe(dir.path());
    g.exempt = true;
    assert!(judged("cargo test", &g).is_silent());
    // And by the wrapper, with the label unset.
    assert!(judged("mac_heavy.sh cargo test", &severe(dir.path())).is_silent());
}

#[test]
fn advise_only_rows_speak_only_on_a_host_already_under_load_and_never_refuse() {
    let dir = tempfile::tempdir().unwrap();
    let quiet = gathered(verdict(Tier::Clear, None, true), dir.path());
    assert!(
        judged("grep -r foo .", &quiet).is_silent(),
        "a quiet host stays quiet"
    );
    let unavailable = gathered(Availability::Unavailable("no sampler".into()), dir.path());
    assert!(judged("grep -r foo .", &unavailable).is_silent());
    for tier in [Tier::Elevated, Tier::Severe] {
        // `admit` is false at severe: still never a refusal for these rows.
        let g = gathered(
            verdict(tier, Some(Binding::Swap), tier == Tier::Elevated),
            dir.path(),
        );
        for command in [
            "grep -r foo .",
            "rg -uu foo",
            "go test -fuzz F ./p",
            "python3 train.py",
        ] {
            let out = judged(command, &g);
            let heavy_go = command.starts_with("go test");
            assert_eq!(
                decision_of(&out),
                if heavy_go && tier == Tier::Severe {
                    Some(PermissionDecision::Deny)
                } else {
                    None
                },
                "{command} at {tier:?}"
            );
            assert!(!out.is_silent(), "{command} at {tier:?}");
        }
    }
    let g = gathered(
        verdict(Tier::Elevated, Some(Binding::Swap), true),
        dir.path(),
    );
    assert!(context(&judged("grep -r foo .", &g)).contains("21 GiB"));
}

#[test]
fn a_load_refusal_survives_the_harness_budget_expiring() {
    let dir = tempfile::tempdir().unwrap();
    let g = severe(dir.path());
    // `run` returns None: the harness did not answer within its budget.
    let out = command_gate_call_with(&call("cargo test"), |_job| None, || g.clone());
    assert_eq!(decision_of(&out), Some(PermissionDecision::Deny), "{out:?}");
    assert!(reason(&out).contains("cargo test"));
    assert!(
        out.system_message.unwrap_or_default().contains("UNGATED"),
        "the harness notice is kept beside the refusal"
    );
    // Without a load problem the same call is just the harness notice.
    let clear = gathered(verdict(Tier::Clear, None, true), dir.path());
    let out = command_gate_call_with(&call("cargo test"), |_job| None, || clear.clone());
    assert_eq!(decision_of(&out), None);
}

#[test]
fn bypass_permissions_does_not_waive_a_load_refusal() {
    let dir = tempfile::tempdir().unwrap();
    let g = severe(dir.path());
    let mut input = call("cargo test");
    input.permission_mode = BYPASS_PERMISSIONS.to_string();
    let out = command_gate_call_with(&input, |_job| None, || g.clone());
    assert_eq!(decision_of(&out), Some(PermissionDecision::Deny), "{out:?}");
}

#[test]
fn a_child_terminal_command_with_no_session_is_refused_plainly_and_never_counted() {
    let dir = tempfile::tempdir().unwrap();
    let g = severe(dir.path());
    let mut input = call("cargo test");
    input.session_id.clear();
    input.tool_use_id.clear();
    for _ in 0..8 {
        let out = judge(&input, || g.clone());
        assert_eq!(decision_of(&out), Some(PermissionDecision::Deny));
        assert!(!reason(&out).contains("looping"));
    }
    assert!(!g.state_dir.exists(), "no session, no state written");
}

#[test]
fn load_is_folded_into_every_other_answer_without_downgrading_a_decision() {
    let notice = HookOutput::notice("ungated");
    let advice_out = advice("hot");
    let ask = HookOutput {
        hook_specific_output: Some(decision(
            PRE_TOOL_USE,
            PermissionDecision::Ask,
            "sure?".into(),
        )),
        system_message: None,
    };
    let deny = HookOutput {
        hook_specific_output: Some(decision(
            PRE_TOOL_USE,
            PermissionDecision::Deny,
            "scope".into(),
        )),
        system_message: Some("gate says no".into()),
    };
    let load_deny = refusal_for_fold();
    let outputs = vec![
        ("silent", HookOutput::silent()),
        ("notice", notice),
        ("advice", advice_out),
        ("ask", ask),
        ("deny", deny),
    ];
    // The load stage answers with nothing, advice or a refusal, never a question.
    let loads = vec![
        ("silent", HookOutput::silent()),
        ("advice", advice("hot")),
        ("load deny", load_deny.clone()),
    ];
    for (rest_name, rest) in &outputs {
        for (load_name, load) in &loads {
            let out = fold(rest.clone(), load.clone());
            let want = if decision_of(load) == Some(PermissionDecision::Deny)
                || decision_of(rest) == Some(PermissionDecision::Deny)
            {
                Some(PermissionDecision::Deny)
            } else if decision_of(rest) == Some(PermissionDecision::Ask)
                || decision_of(load) == Some(PermissionDecision::Ask)
            {
                Some(PermissionDecision::Ask)
            } else {
                None
            };
            assert_eq!(
                decision_of(&out),
                want,
                "rest {rest_name}, load {load_name}"
            );
            // Nothing either side said is lost.
            for side in [rest, load] {
                if let Some(message) = &side.system_message {
                    assert!(out
                        .system_message
                        .clone()
                        .unwrap_or_default()
                        .contains(message.as_str()));
                }
                if let Some(text) = context_of(side) {
                    assert!(
                        context(&out).contains(&text),
                        "rest {rest_name}, load {load_name}"
                    );
                }
            }
        }
    }
    // A double refusal keeps both reasons.
    let both = fold(outputs[4].1.clone(), load_deny);
    assert!(reason(&both).contains("scope") && reason(&both).contains("heavy command"));
}

fn refusal_for_fold() -> HookOutput {
    let dir = tempfile::tempdir().unwrap();
    judged("cargo test", &severe(dir.path()))
}

fn counted(dir: &Path, id: &str, now: SystemTime) -> Option<usize> {
    count_denials(dir, SESSION, id, "cargo test", now)
}

#[test]
fn the_fifth_identical_refusal_in_ten_minutes_says_stop_and_grants_nothing() {
    let dir = tempfile::tempdir().unwrap();
    let g = severe(dir.path());
    let mut outputs = Vec::new();
    for n in 1..=7 {
        let mut input = call("cd x && cargo test");
        input.tool_use_id = format!("toolu_{n}");
        outputs.push(judge(&input, || g.clone()));
    }
    for (n, out) in outputs.iter().enumerate() {
        assert_eq!(
            decision_of(out),
            Some(PermissionDecision::Deny),
            "refusal {}",
            n + 1
        );
        assert!(!context(out).contains("allow"));
        let stop = reason(out).contains("STOP: you are looping");
        assert_eq!(stop, n + 1 >= LOOP_AT, "refusal {}: {}", n + 1, reason(out));
    }
    let fifth = &outputs[4];
    assert!(reason(fifth).contains("5 times"), "{}", reason(fifth));
    assert!(reason(fifth).contains("This grants nothing"));
    assert!(
        fifth
            .system_message
            .as_deref()
            .unwrap_or_default()
            .contains("loop"),
        "the person is told"
    );
    assert!(outputs[3].system_message.is_none());
}

#[test]
fn the_count_is_per_session_per_command_and_per_distinct_call() {
    let dir = tempfile::tempdir().unwrap();
    let state = dir.path().join("s");
    let now = SystemTime::now();
    assert_eq!(counted(&state, "toolu_1", now), Some(1));
    // Two copies of the hook answering one call leave one marker.
    assert_eq!(counted(&state, "toolu_1", now), Some(1));
    assert_eq!(counted(&state, "toolu_2", now), Some(2));
    // Another command, and another session, count apart.
    assert_eq!(
        count_denials(&state, SESSION, "toolu_3", "cargo build", now),
        Some(1)
    );
    assert_eq!(
        count_denials(&state, "other-session", "toolu_4", "cargo test", now),
        Some(1)
    );
    // Spelling the same command with spaces around it is the same command.
    assert_eq!(
        count_denials(&state, SESSION, "toolu_5", "  cargo test ", now),
        Some(3)
    );
}

#[test]
fn denials_older_than_the_window_stop_counting() {
    let dir = tempfile::tempdir().unwrap();
    let state = dir.path().join("s");
    let now = SystemTime::now();
    for n in 1..=4 {
        counted(&state, &format!("toolu_{n}"), now).unwrap();
    }
    // Three of the four were refused 11 minutes ago.
    let old = now - LOOP_WINDOW - Duration::from_secs(60);
    for entry in std::fs::read_dir(&state).unwrap().flatten() {
        let name = entry.file_name().to_string_lossy().into_owned();
        if ["toolu_1", "toolu_2", "toolu_3"]
            .iter()
            .any(|id| name.ends_with(id))
        {
            let file = OpenOptions::new().write(true).open(entry.path()).unwrap();
            file.set_modified(old).unwrap();
        }
    }
    assert_eq!(
        counted(&state, "toolu_5", now),
        Some(2),
        "the 4th and the 5th"
    );
}

#[test]
fn state_that_cannot_be_trusted_falls_back_to_a_plain_refusal_never_an_allow() {
    let dir = tempfile::tempdir().unwrap();
    let now = SystemTime::now();
    // The state path is a regular file.
    let file = dir.path().join("file");
    std::fs::write(&file, b"not a directory").unwrap();
    assert_eq!(counted(&file, "toolu_1", now), None);
    // No session or call id.
    let state = dir.path().join("s");
    assert_eq!(
        count_denials(&state, "", "toolu_1", "cargo test", now),
        None
    );
    assert_eq!(count_denials(&state, SESSION, "", "cargo test", now), None);
    assert_eq!(
        count_denials(&state, "../escape", "toolu_1", "cargo test", now),
        None
    );
    assert_eq!(
        count_denials(&state, SESSION, "a/b", "cargo test", now),
        None
    );
    #[cfg(unix)]
    {
        use std::os::unix::fs::{symlink, PermissionsExt};
        // A directory other users can write is refused, not used.
        let open = dir.path().join("open");
        std::fs::create_dir(&open).unwrap();
        std::fs::set_permissions(&open, std::fs::Permissions::from_mode(0o777)).unwrap();
        assert_eq!(counted(&open, "toolu_1", now), None);
        // A symlink planted under a marker's name is not a marker, and a
        // directory is not one either.
        let fresh = dir.path().join("fresh");
        assert_eq!(counted(&fresh, "toolu_1", now), Some(1));
        let key = std::fs::read_dir(&fresh)
            .unwrap()
            .flatten()
            .next()
            .unwrap()
            .file_name();
        let key = key
            .to_string_lossy()
            .trim_end_matches("toolu_1")
            .to_string();
        symlink("/etc/hosts", fresh.join(format!("{key}toolu_2"))).unwrap();
        std::fs::create_dir(fresh.join(format!("{key}toolu_3"))).unwrap();
        assert_eq!(
            counted(&fresh, "toolu_4", now),
            Some(2),
            "only real markers count"
        );
        // An unreadable directory.
        std::fs::set_permissions(&fresh, std::fs::Permissions::from_mode(0o000)).unwrap();
        assert_eq!(counted(&fresh, "toolu_5", now), None);
        std::fs::set_permissions(&fresh, std::fs::Permissions::from_mode(0o700)).unwrap();
    }
    // Through the gate, each of those is a plain refusal.
    let mut g = severe(dir.path());
    g.state_dir = file;
    let out = judged("cargo test", &g);
    assert_eq!(decision_of(&out), Some(PermissionDecision::Deny));
    assert!(!reason(&out).contains("looping"));
}

/// Benchmark: what the load stage adds to one `command-gate` call.
///
/// Ignored by default; it measures, it does not assert. It runs the stage
/// in-process, because the hook's own start-up (about 25 ms, see
/// `tests/hook_gate_bench.rs`) is thirty times the quantity under test and
/// would bury it. A debug build is an upper bound on a release one.
///
/// ```text
/// cargo test --lib -- --ignored --nocapture load_stage_cost
/// ```
///
/// Arms are interleaved inside every round and rotated between rounds, so
/// drift in machine load lands on all of them alike. `null` is the loop and
/// the clock reads alone: the noise floor. Each row is the minimum, the median
/// and the maximum of the per-round medians. `heavy/*` run the real
/// `gather_live`, with the settings cold (the hook is a new process on every
/// call) and the snapshot read from disk; `heavy/refused` also writes the
/// loop-breaker marker. Tune with `GITPULSE_BENCH_ROUNDS` and
/// `GITPULSE_BENCH_CALLS`.
#[test]
#[ignore = "benchmark"]
fn load_stage_cost() {
    use std::time::Instant;
    fn env_usize(key: &str, default: usize) -> usize {
        std::env::var(key)
            .ok()
            .and_then(|v| v.parse().ok())
            .unwrap_or(default)
    }
    let rounds = env_usize("GITPULSE_BENCH_ROUNDS", 9);
    let calls = env_usize("GITPULSE_BENCH_CALLS", 200);

    let serial = crate::test_support::env::env_serial();
    let root = tempfile::tempdir().unwrap();
    let home = root.path().join("home");
    let config_dir = if cfg!(target_os = "macos") {
        home.join("Library/Application Support/GitPulse")
    } else {
        home.join(".config").join("gitpulse")
    };
    std::fs::create_dir_all(&config_dir).unwrap();
    let tools = root.path().join("tools.json");
    std::fs::write(&tools, r#"{"version":1,"host_load_hook_mode":"enforce"}"#).unwrap();
    let _env = crate::test_support::env::bind_env(&serial)
        .set("HOME", &home)
        .set("XDG_CONFIG_HOME", home.join(".config"))
        .set(crate::tool_config::TOOL_CONFIG_ENV, &tools)
        .invalidating(crate::tool_config::invalidate_cache);
    let publish = |verdict: PublishedVerdict| {
        let mut snapshot = Snapshot::empty(Clock::now(), WriterIdentity::default(), true);
        snapshot.verdict = Reading::Value(verdict);
        let mut writer = crate::sysload::SnapshotWriter::elect(&config_dir)
            .unwrap()
            .unwrap();
        writer.publish(&snapshot).unwrap();
    };
    let refused = PublishedVerdict {
        tier: Tier::Severe,
        binding: Some(Binding::Swap),
        admit: false,
        shed_background: true,
        shed_reactive: true,
    };
    let clear = PublishedVerdict {
        tier: Tier::Clear,
        binding: None,
        admit: true,
        shed_background: false,
        shed_reactive: false,
    };

    let long = format!("echo {}", "a".repeat(10_000));
    let heavy_line = "cd src-tauri && cargo test --lib -- hooks::";
    let live = |state: &Path| {
        let mut g = gather_live();
        g.state_dir = state.to_path_buf();
        g
    };
    type Arm<'a> = (&'static str, Box<dyn FnMut(usize) + 'a>);
    let mut arms: Vec<Arm> = vec![
        ("null", Box::new(|_| {})),
        (
            "not-heavy",
            Box::new(|_| {
                let out = judge(&call("git status --short"), || unreachable!("not heavy"));
                std::hint::black_box(out);
            }),
        ),
        (
            "not-heavy/10KB",
            Box::new(|_| {
                let out = judge(&call(&long), || unreachable!("not heavy"));
                std::hint::black_box(out);
            }),
        ),
        (
            "brief-line",
            Box::new(|_| {
                std::hint::black_box(brief_line_live());
            }),
        ),
        (
            "heavy/clear",
            Box::new(|_| {
                crate::tool_config::invalidate_cache();
                std::hint::black_box(judge(&call(heavy_line), || live(root.path())));
            }),
        ),
    ];

    let mut samples: Vec<Vec<f64>> = vec![Vec::new(); arms.len() + 1];
    for round in 0..rounds {
        // Each round publishes the arm's own verdict before it runs.
        let order: Vec<usize> = (0..arms.len() + 1)
            .map(|i| (i + round) % (arms.len() + 1))
            .collect();
        for index in order {
            let mut times = Vec::with_capacity(calls);
            if index == arms.len() {
                publish(refused);
                // A fresh marker directory per round: the loop breaker's
                // cost grows with the markers it finds, and a real session
                // has a handful.
                let state = root.path().join(format!("state{round}"));
                for n in 0..calls {
                    let mut input = call(heavy_line);
                    input.tool_use_id = format!("toolu_{round}_{n}");
                    let started = Instant::now();
                    crate::tool_config::invalidate_cache();
                    std::hint::black_box(judge(&input, || live(&state)));
                    times.push(started.elapsed().as_secs_f64() * 1e6);
                }
            } else {
                if arms[index].0 == "heavy/clear" || arms[index].0 == "brief-line" {
                    publish(clear);
                }
                for n in 0..calls {
                    let started = Instant::now();
                    (arms[index].1)(n);
                    times.push(started.elapsed().as_secs_f64() * 1e6);
                }
            }
            times.sort_by(f64::total_cmp);
            samples[index].push(times[times.len() / 2]);
        }
    }
    let load = {
        use crate::procguard::LockedSpawn;
        std::process::Command::new("sysctl")
            .args(["-n", "vm.loadavg"])
            .output_locked()
    };
    eprintln!(
        "load stage, per call, microseconds; {rounds} rounds x {calls} calls per arm; \
         host load average {}; {} build",
        load.map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
            .unwrap_or_default(),
        if cfg!(debug_assertions) {
            "debug"
        } else {
            "release"
        },
    );
    eprintln!("{:<16}{:>10}{:>10}{:>10}", "arm", "min", "median", "max");
    for (index, times) in samples.iter_mut().enumerate() {
        times.sort_by(f64::total_cmp);
        let name = if index == arms.len() {
            "heavy/refused"
        } else {
            arms[index].0
        };
        eprintln!(
            "{name:<16}{:>10.1}{:>10.1}{:>10.1}",
            times[0],
            times[times.len() / 2],
            times[times.len() - 1]
        );
    }
}

fn counts(launched: u32, observed: u32) -> Reading<crate::sysload::SessionCount> {
    Reading::Value(crate::sysload::SessionCount { launched, observed })
}

#[test]
fn the_session_brief_line_says_what_the_snapshot_says_and_what_it_could_not_read() {
    let published = PublishedVerdict {
        tier: Tier::Severe,
        binding: Some(Binding::MemoryPressure),
        admit: false,
        shed_background: true,
        shed_reactive: true,
    };
    let mut snapshot = snapshot_with(Reading::Value(published), true);
    snapshot.claude = counts(3, 14);
    snapshot.codex = Reading::Unavailable(Why::Failed);
    let line = brief_line(&report(SensorState::Fresh, None), Some(&snapshot));
    for must in [
        "host load: severe, the kernel reports memory pressure",
        "sensor fresh",
        "not admitting launches",
        "refuses heavy build and test commands",
        "Claude 3 launched / 14 observed",
        "Codex unknown",
    ] {
        assert!(line.contains(must), "{must:?} missing: {line}");
    }
    assert!(!line.contains('\n'), "one line: {line}");

    // A clear host is one short clause, with no refusal in it.
    let calm = PublishedVerdict {
        tier: Tier::Clear,
        binding: None,
        admit: true,
        shed_background: false,
        shed_reactive: false,
    };
    let mut snapshot = snapshot_with(Reading::Value(calm), true);
    snapshot.claude = counts(0, 2);
    snapshot.codex = counts(0, 0);
    let line = brief_line(&report(SensorState::StaleRecent, None), Some(&snapshot));
    assert!(
        line.starts_with("host load: clear (sensor stale, reading recent);"),
        "{line}"
    );
    assert!(
        !line.contains("refuses") && !line.contains("not admitting"),
        "{line}"
    );

    // Nothing to read is UNKNOWN, with the reason, and says nothing was checked.
    for (report, snapshot) in [
        (report(SensorState::Dead, Some("no sampler")), None),
        (
            report(SensorState::StaleOld, None),
            Some(snapshot_with(Reading::Value(calm), true)),
        ),
        (
            report(SensorState::Fresh, None),
            Some(snapshot_with(Reading::Unavailable(Why::Unsupported), true)),
        ),
    ] {
        let line = brief_line(&report, snapshot.as_ref());
        assert!(line.starts_with("host load: UNKNOWN ("), "{line}");
        assert!(line.contains("not load-checked"), "{line}");
    }
}

fn advises(out: &HookOutput) -> bool {
    decision_of(out).is_none() && context(out).contains("did NOT load-check")
}

#[test]
fn a_not_checked_notice_is_said_once_per_session_per_reason() {
    let dir = tempfile::tempdir().unwrap();
    let no_sampler = gathered(Availability::Unavailable("no sampler".into()), dir.path());
    let first = judged("cargo test", &no_sampler);
    assert!(advises(&first), "{first:?}");
    assert!(
        context(&first).contains("will not repeat for this session"),
        "{}",
        context(&first)
    );
    // The same reason, the same session, even for a different heavy command.
    assert!(judged("cargo test", &no_sampler).is_silent());
    assert!(judged("npm run test:browser", &no_sampler).is_silent());
    // A different reason is news.
    let older = gathered(
        Availability::Unavailable("the snapshot carries no governor verdict".into()),
        dir.path(),
    );
    assert!(advises(&judged("cargo test", &older)));
    assert!(judged("cargo test", &older).is_silent());
    // A different session is told again.
    let mut other = call("cargo test");
    other.session_id = "00000000-aaaa-4bbb-8ccc-dddddddddddd".into();
    assert!(advises(&judge(&other, || no_sampler.clone())));
    // The unreadable-line notice follows the same rule, with its own reason.
    let unreadable = judged("cargo test \"oops", &no_sampler);
    assert!(advises(&unreadable) && context(&unreadable).contains("will not repeat"));
    assert!(judged("cargo test \"oops", &no_sampler).is_silent());
    // A refusal is never muted by any of this.
    assert_eq!(
        decision_of(&judged("cargo test", &severe(dir.path()))),
        Some(PermissionDecision::Deny)
    );
}

#[test]
fn a_notice_that_cannot_be_counted_is_said_every_time() {
    let dir = tempfile::tempdir().unwrap();
    let g = gathered(Availability::Unavailable("no sampler".into()), dir.path());
    // No session id: nothing to count against.
    let mut anonymous = call("cargo test");
    anonymous.session_id.clear();
    for _ in 0..3 {
        let out = judge(&anonymous, || g.clone());
        assert!(
            advises(&out) && !context(&out).contains("will not repeat"),
            "{out:?}"
        );
    }
    // A session id that is not a plain token cannot name a marker.
    let mut odd = call("cargo test");
    odd.session_id = "../escape".into();
    assert!(advises(&judge(&odd, || g.clone())) && advises(&judge(&odd, || g.clone())));
    // A dedupe directory that is a file, or that anyone can write, or cannot be read.
    let file = dir.path().join("file");
    std::fs::write(&file, b"x").unwrap();
    let mut broken = g.clone();
    broken.state_dir = file;
    for _ in 0..2 {
        assert!(advises(&judged("cargo test", &broken)));
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let open = dir.path().join("open");
        std::fs::create_dir(&open).unwrap();
        std::fs::set_permissions(&open, std::fs::Permissions::from_mode(0o777)).unwrap();
        let mut shared = g.clone();
        shared.state_dir = open;
        for _ in 0..2 {
            assert!(advises(&judged("cargo test", &shared)));
        }
        let sealed = dir.path().join("sealed");
        let mut locked = g.clone();
        locked.state_dir = sealed.clone();
        assert!(advises(&judged("cargo test", &locked)));
        std::fs::set_permissions(&sealed, std::fs::Permissions::from_mode(0o500)).unwrap();
        // The marker for this reason exists; a read-only directory cannot take a new one.
        let mut other = call("cargo build");
        other.session_id = "11111111-aaaa-4bbb-8ccc-dddddddddddd".into();
        let out = judge(&other, || locked.clone());
        assert!(
            advises(&out),
            "an unwritable directory must advise, not go silent: {out:?}"
        );
        std::fs::set_permissions(&sealed, std::fs::Permissions::from_mode(0o700)).unwrap();
    }
}

fn with_lease(mut g: Gathered, facts: LeaseFacts) -> Gathered {
    g.lease = LeaseSource::Fixed(facts);
    g
}

#[test]
fn a_leased_build_in_a_session_without_the_wrapper_is_told_so_once_and_why() {
    let dir = tempfile::tempdir().unwrap();
    let g = with_lease(
        gathered(verdict(Tier::Clear, None, true), dir.path()),
        LeaseFacts {
            on: true,
            not_gated: Some(
                "this session's PATH does not start with GitPulse's lease folder".into(),
            ),
            full: None,
        },
    );
    let first = context(&judged("cargo build", &g));
    assert!(
        first.contains("did NOT lease-gate this build (cargo build)"),
        "{first}"
    );
    assert!(
        first.contains("PATH does not start with GitPulse's lease folder"),
        "{first}"
    );
    assert!(
        first.contains("will not repeat for this session"),
        "{first}"
    );
    // The same session, the same reason: said once.
    assert!(judged("cargo test", &g).is_silent());
    // A different session is told again.
    let mut other = call("cargo build");
    other.session_id = "00000000-aaaa-4bbb-8ccc-000000000000".into();
    assert!(context(&judge(&other, || g.clone())).contains("did NOT lease-gate"));
    // A command with no lease row (a package script) and a session with leases
    // switched off are told nothing.
    assert!(judged("npm test", &g).is_silent());
    let off = with_lease(
        gathered(verdict(Tier::Clear, None, true), dir.path()),
        LeaseFacts {
            on: false,
            not_gated: Some("x".into()),
            full: Some("y".into()),
        },
    );
    assert!(judged("cargo build", &off).is_silent());
}

#[test]
fn a_heavy_command_with_every_slot_held_is_told_it_will_queue() {
    let dir = tempfile::tempdir().unwrap();
    let facts = LeaseFacts {
        on: true,
        not_gated: None,
        full: Some("cargo test in /w/a (pid 41); cargo build in /w/b (pid 42)".into()),
    };
    for availability in [
        verdict(Tier::Clear, None, true),
        verdict(Tier::Elevated, Some(Binding::Swap), true),
        Availability::Unavailable("no snapshot".into()),
    ] {
        let out = judged(
            "cargo build",
            &with_lease(gathered(availability, dir.path()), facts.clone()),
        );
        let text = context(&out);
        assert!(
            text.contains("Every build slot is held (cargo test in /w/a (pid 41)"),
            "{text}"
        );
        assert!(
            text.contains("600000 ms timeout or run it in the background"),
            "{text}"
        );
        assert_eq!(decision_of(&out), None, "a queue is never a refusal");
    }
    // A refusal stays a refusal and does not grow a queue line it cannot use.
    let refused = judged("cargo build", &with_lease(severe(dir.path()), facts));
    assert_eq!(decision_of(&refused), Some(PermissionDecision::Deny));
    assert!(!reason(&refused).contains("slot is held"));
}
