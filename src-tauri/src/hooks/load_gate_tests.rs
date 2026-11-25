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
        disk: plenty_of_disk(),
        read_free: |_| Free::Missing,
        lease: LeaseSource::Fixed(LeaseFacts::default()),
        steward_throttle: None,
        relief: ReliefOutcome::Absent,
        terminal_session: None,
    }
}

const GIB: u64 = 1 << 30;

/// 200 GiB free against a 16 GiB floor and a 60 GiB coverage headroom, both
/// refusing: the numbers of a 64 GiB Mac at its defaults.
fn plenty_of_disk() -> DiskFacts {
    DiskFacts {
        free_bytes: Some(200 * GIB),
        floor_bytes: 16 * GIB,
        floor_mode: InputMode::Refuse,
        coverage_headroom_bytes: 60 * GIB,
        coverage_mode: InputMode::Refuse,
        measuring: true,
        refusing: true,
    }
}

fn with_free(dir: &Path, free_gib: u64) -> Gathered {
    let mut g = gathered(verdict(Tier::Clear, None, true), dir);
    g.disk.free_bytes = Some(free_gib * GIB);
    g
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
        Some(Binding::DiskFilling),
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
        let mut g = gather_live("");
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

#[test]
fn a_coverage_build_is_refused_below_its_headroom_and_not_above_it() {
    let dir = tempfile::tempdir().unwrap();
    let out = judged("cargo llvm-cov --lib", &with_free(dir.path(), 50));
    assert_eq!(decision_of(&out), Some(PermissionDecision::Deny));
    let text = reason(&out);
    for must in [
        "coverage build",
        "50.0 GiB",
        "60.0 GiB",
        "free disk space first",
        "cargo llvm-cov clean",
    ] {
        assert!(text.contains(must), "lacks {must:?}: {text}");
    }
    assert!(!text.contains("free memory"), "{text}");
    assert!(
        judged("cargo llvm-cov --lib", &with_free(dir.path(), 70)).is_silent(),
        "above the headroom, a clear host stays quiet"
    );
    // An ordinary build needs the floor, not the coverage headroom.
    assert!(judged("cargo test --lib", &with_free(dir.path(), 50)).is_silent());
}

#[test]
fn the_commands_that_give_the_disk_back_are_never_refused_for_a_full_disk() {
    let dir = tempfile::tempdir().unwrap();
    let full = with_free(dir.path(), 1);
    for command in [
        "cargo llvm-cov clean",
        "cargo llvm-cov clean --workspace",
        "cargo llvm-cov report --html",
        "cargo llvm-cov show-env",
        "cargo +nightly llvm-cov --manifest-path src-tauri/Cargo.toml clean",
    ] {
        assert!(judged(command, &full).is_silent(), "{command}");
    }
    for command in [
        "cargo llvm-cov",
        "cargo llvm-cov nextest",
        "cargo llvm-cov --no-report --lib",
        "cargo test report",
        "cargo test clean",
    ] {
        assert_eq!(
            decision_of(&judged(command, &full)),
            Some(PermissionDecision::Deny),
            "{command}"
        );
    }
}

#[test]
fn any_heavy_build_is_refused_below_the_severe_floor_with_the_disk_words() {
    let dir = tempfile::tempdir().unwrap();
    for command in ["cargo build", "go test ./...", "npm run test:browser"] {
        let out = judged(command, &with_free(dir.path(), 12));
        assert_eq!(
            decision_of(&out),
            Some(PermissionDecision::Deny),
            "{command}"
        );
        let text = reason(&out);
        assert!(
            text.contains("12.0 GiB") && text.contains("16.0 GiB floor"),
            "{text}"
        );
        assert!(!text.contains("wait a few minutes"), "{text}");
    }
    // Looping says disk, not memory.
    let g = with_free(dir.path(), 12);
    let mut last = String::new();
    for n in 0..LOOP_AT {
        let mut input = call("cargo build");
        input.tool_use_id = format!("toolu_{n:02}");
        last = reason(&judge(&input, || g.clone()));
    }
    assert!(
        last.contains("STOP") && last.contains("free disk space"),
        "{last}"
    );
    assert!(!last.contains("free memory"), "{last}");
}

#[test]
fn the_disk_modes_refuse_say_or_stay_silent() {
    let dir = tempfile::tempdir().unwrap();
    let mut shed = with_free(dir.path(), 12);
    shed.disk.floor_mode = InputMode::Shed;
    let out = judged("cargo build", &shed);
    assert_eq!(decision_of(&out), None);
    assert!(
        context(&out).contains("Free disk space is low"),
        "{}",
        context(&out)
    );

    let mut off = with_free(dir.path(), 12);
    off.disk.floor_mode = InputMode::Off;
    off.disk.coverage_mode = InputMode::Off;
    assert!(judged("cargo build", &off).is_silent());
    assert!(judged("cargo llvm-cov", &off).is_silent());

    // The hook's own Advise says the same and refuses nothing; Off adds nothing.
    let mut advise = with_free(dir.path(), 12);
    advise.mode = HostLoadHookMode::Advise;
    let out = judged("cargo build", &advise);
    assert_eq!(decision_of(&out), None);
    assert!(context(&out).contains("would refuse"), "{}", context(&out));
    let mut hook_off = with_free(dir.path(), 12);
    hook_off.mode = HostLoadHookMode::Off;
    assert!(judged("cargo build", &hook_off).is_silent());
    // The heavy lane's holder is exempt from the disk check too.
    let mut exempt = with_free(dir.path(), 12);
    exempt.exempt = true;
    assert!(judged("cargo build", &exempt).is_silent());
}

#[test]
fn a_disk_reading_that_could_not_be_taken_is_said_once_never_a_pass() {
    let dir = tempfile::tempdir().unwrap();
    let mut g = with_free(dir.path(), 200);
    g.disk.free_bytes = None;
    let first = judged("cargo build", &g);
    assert_eq!(decision_of(&first), None);
    assert!(
        context(&first).contains("did NOT check free disk space"),
        "{}",
        context(&first)
    );
    assert!(
        judged("cargo build", &g).is_silent(),
        "said once per session"
    );
}

#[test]
fn a_disk_refusal_wins_over_a_clear_verdict_and_keeps_a_verdict_refusal() {
    let dir = tempfile::tempdir().unwrap();
    // The verdict refuses for swap while the disk is also short: one refusal,
    // naming the disk, which the model can act on.
    let mut g = severe(dir.path());
    g.disk.free_bytes = Some(5 * GIB);
    let out = judged("cargo test", &g);
    assert_eq!(decision_of(&out), Some(PermissionDecision::Deny));
    assert!(reason(&out).contains("5.0 GiB"), "{}", reason(&out));
    // A short disk below the Shed mode still lets the verdict refuse.
    g.disk.floor_mode = InputMode::Shed;
    let out = judged("cargo test", &g);
    assert_eq!(decision_of(&out), Some(PermissionDecision::Deny));
    assert!(reason(&out).contains("swap in use"), "{}", reason(&out));
    // A verdict bound by the disk filling tells the model to free disk.
    let filling = gathered(
        verdict(Tier::Severe, Some(Binding::DiskFilling), false),
        dir.path(),
    );
    let text = reason(&judged("cargo build", &filling));
    assert!(
        text.contains("falling fast") && text.contains("free disk space first"),
        "{text}"
    );
}

/// What an installed hook built before the disk fill rule does when the
/// binding byte is one it does not know: the whole verdict reads as `Failed`
/// (`snapshot::ext_reading`), and the gate says "not load-checked" and refuses
/// nothing. This is the fail-open the published `LowDisk` byte exists to avoid,
/// pinned here on the code path that older reader runs.
#[test]
fn an_older_reader_that_cannot_place_the_binding_fails_open_which_is_why_the_byte_is_low_disk() {
    let dir = tempfile::tempdir().unwrap();
    let snapshot = snapshot_with(Reading::Unavailable(Why::Failed), true);
    let availability = availability_of(&report(SensorState::Fresh, None), Some(&snapshot));
    assert!(
        matches!(&availability, Availability::Unavailable(why) if why.contains("no governor verdict")),
        "{availability:?}"
    );
    let out = judged("cargo test", &gathered(availability, dir.path()));
    assert_eq!(decision_of(&out), None, "never a refusal");
    assert!(
        context(&out).contains("did NOT load-check"),
        "{}",
        context(&out)
    );

    // The same snapshot carrying the byte an older reader knows is refused.
    let published = PublishedVerdict {
        tier: Tier::Severe,
        binding: Some(Binding::LowDisk),
        admit: false,
        shed_background: true,
        shed_reactive: true,
    };
    let snapshot = snapshot_with(Reading::Value(published), true);
    let availability = availability_of(&report(SensorState::Fresh, None), Some(&snapshot));
    let out = judged("cargo test", &gathered(availability, dir.path()));
    assert_eq!(decision_of(&out), Some(PermissionDecision::Deny));
    assert!(
        reason(&out).contains("free disk space is below"),
        "{}",
        reason(&out)
    );
}

#[test]
fn with_launch_refusal_off_the_disk_checks_advise_and_never_refuse() {
    let dir = tempfile::tempdir().unwrap();
    let mut g = with_free(dir.path(), 9);
    g.disk.refusing = false;
    for command in ["cargo build", "cargo llvm-cov --lib"] {
        let out = judged(command, &g);
        assert_eq!(decision_of(&out), None, "{command}: {out:?}");
        assert!(
            context(&out).contains("Free disk space is low"),
            "{command}: {}",
            context(&out)
        );
    }
}

#[test]
fn with_host_load_measurement_off_the_disk_checks_say_nothing() {
    let dir = tempfile::tempdir().unwrap();
    let mut g = with_free(dir.path(), 9);
    g.disk.measuring = false;
    for command in ["cargo build", "cargo llvm-cov --lib"] {
        let out = judged(command, &g);
        assert!(out.is_silent(), "{command}: {out:?}");
    }
    g.disk.free_bytes = None;
    assert!(
        judged("cargo build", &g).is_silent(),
        "nothing is measured, so an unreadable disk is not worth a notice"
    );
}

#[test]
fn a_coverage_build_after_another_heavy_command_gets_the_coverage_headroom() {
    let dir = tempfile::tempdir().unwrap();
    for command in [
        "cargo check && cargo llvm-cov --lib",
        "cargo test --lib; cargo llvm-cov",
    ] {
        let out = judged(command, &with_free(dir.path(), 30));
        assert_eq!(
            decision_of(&out),
            Some(PermissionDecision::Deny),
            "{command}"
        );
        assert!(
            reason(&out).contains("coverage build"),
            "{command}: {}",
            reason(&out)
        );
    }
    assert!(judged("cargo check && cargo build", &with_free(dir.path(), 30)).is_silent());
}

#[test]
fn a_real_coverage_build_behind_a_subcommand_word_is_refused() {
    let dir = tempfile::tempdir().unwrap();
    for command in [
        "cargo llvm-cov -- clean",
        "cargo llvm-cov --test clean",
        "cargo llvm-cov --bin report",
        "cargo llvm-cov --exclude show-env",
        "cargo llvm-cov -p clean",
    ] {
        assert_eq!(
            decision_of(&judged(command, &with_free(dir.path(), 30))),
            Some(PermissionDecision::Deny),
            "{command}"
        );
    }
    assert_eq!(
        decision_of(&judged(
            "cargo llvm-cov clean && cargo build",
            &with_free(dir.path(), 5)
        )),
        Some(PermissionDecision::Deny)
    );
}

#[test]
fn free_space_is_read_where_the_command_runs_and_never_borrowed_from_another_volume() {
    // `/volumes/...` has no drive, so Windows reads it as relative.
    let abs = |unix: &str| {
        if cfg!(windows) {
            format!("C:{unix}")
        } else {
            unix.to_string()
        }
    };
    let (small, denied, gone) = (abs("/volumes/small/work"), abs("/denied"), abs("/gone"));
    let there = Path::new(&small);
    let here = PathBuf::from(abs("/volumes/big/hook"));
    let read = |path: &Path| {
        if path == there {
            Free::Bytes(5 * GIB)
        } else if path == here {
            Free::Bytes(500 * GIB)
        } else if path == Path::new(&denied) {
            Free::Unreadable
        } else {
            Free::Missing
        }
    };
    assert_eq!(
        live_free_bytes(&small, Some(here.clone()), read),
        Some(5 * GIB),
        "the command's own directory wins"
    );
    assert_eq!(
        live_free_bytes("", Some(here.clone()), read),
        Some(500 * GIB),
        "no cwd was given: the process's directory is the best guess"
    );
    // A cwd that was given and cannot be read is not "has room".
    for cwd in [gone.as_str(), denied.as_str(), "relative/dir", "."] {
        assert_eq!(
            live_free_bytes(cwd, Some(here.clone()), read),
            None,
            "{cwd:?} must not fall back to the hook process's volume"
        );
    }
    assert_eq!(live_free_bytes("", None, read), None);
    assert_eq!(live_free_bytes("", Some(here), |_| Free::Unreadable), None);
}

/// A reader for the build-directory tests. `/` is 200 GiB (a reader that
/// returned nothing for it would hide the walk-up), `/small` 5 GiB, `/big`
/// 500; `/denied` and below exist and cannot be read; anything else does not
/// exist.
fn volumes(path: &Path) -> Free {
    if path == Path::new("/small") {
        Free::Bytes(5 * GIB)
    } else if path == Path::new("/big") {
        Free::Bytes(500 * GIB)
    } else if path == Path::new("/") {
        Free::Bytes(200 * GIB)
    } else if path.starts_with("/denied") && path != Path::new("/denied/new") {
        Free::Unreadable
    } else {
        Free::Missing
    }
}

fn on_volumes(dir: &Path, cwd_free_gib: u64) -> Gathered {
    let mut g = with_free(dir, cwd_free_gib);
    g.read_free = volumes;
    g
}

#[test]
fn a_literal_absolute_build_directory_is_read_too_and_the_stricter_answer_wins() {
    let dir = tempfile::tempdir().unwrap();
    // The working directory has room; the named target directory's volume (its
    // nearest existing ancestor, since the directory is new) does not.
    for command in [
        "cargo build --target-dir /small/new/target",
        "cargo test --target-dir=/small/new/target",
        "CARGO_TARGET_DIR=/small/new/target cargo build",
        "env CARGO_TARGET_DIR=/small/t cargo test --lib",
        "cd /repo && cargo build --target-dir /small/t",
    ] {
        let out = judged(command, &on_volumes(dir.path(), 200));
        assert_eq!(
            decision_of(&out),
            Some(PermissionDecision::Deny),
            "{command}"
        );
        assert!(
            reason(&out).contains("5.0 GiB"),
            "{command}: {}",
            reason(&out)
        );
    }
    // The target directory has room, the working directory does not.
    let out = judged(
        "cargo build --target-dir /big/t",
        &on_volumes(dir.path(), 5),
    );
    assert_eq!(decision_of(&out), Some(PermissionDecision::Deny));
    // Both have room: nothing to say.
    assert!(judged(
        "cargo build --target-dir /big/t",
        &on_volumes(dir.path(), 200)
    )
    .is_silent());
    // A directory that does not exist yet reads its nearest existing ancestor,
    // and `/` is a real volume here: this is the walk-up, not a miss.
    assert!(judged(
        "cargo build --target-dir /nowhere/new/t",
        &on_volumes(dir.path(), 200)
    )
    .is_silent());
}

#[test]
fn has_room_needs_every_location_read_and_a_short_one_still_refuses() {
    let dir = tempfile::tempdir().unwrap();
    // The cwd could not be read, the target directory can and has room: not
    // "has room" (advice that it could not check), and never a refusal.
    let mut g = on_volumes(dir.path(), 200);
    g.disk.free_bytes = None;
    let out = judged("cargo build --target-dir /big/t", &g);
    assert_eq!(decision_of(&out), None);
    assert!(
        context(&out).contains("could not be read"),
        "{}",
        context(&out)
    );
    // The same with the target directory the one that is short: its own
    // number refuses, whatever the cwd did.
    let out = judged("cargo build --target-dir /small/t", &g);
    assert_eq!(decision_of(&out), Some(PermissionDecision::Deny));
    assert!(reason(&out).contains("5.0 GiB"), "{}", reason(&out));
    // The cwd was read and has room; the target directory is unreadable. A
    // fresh state directory: the first block already said so once this session.
    let fresh = tempfile::tempdir().unwrap();
    let g = on_volumes(fresh.path(), 200);
    let out = judged("cargo build --target-dir /denied/t", &g);
    assert_eq!(decision_of(&out), None);
    assert!(
        context(&out).contains("could not be read"),
        "{}",
        context(&out)
    );
}

#[test]
fn an_unreadable_directory_is_not_replaced_by_its_ancestors_volume() {
    // `/denied/t` exists and cannot be read; `/` could be, and has room.
    assert_eq!(free_at_or_above(Path::new("/denied/t"), volumes), None);
    // `/denied/new` is missing, and `/denied` above it is unreadable: stop.
    assert_eq!(free_at_or_above(Path::new("/denied/new/t"), volumes), None);
    // Missing all the way up reads the first volume that exists.
    assert_eq!(
        free_at_or_above(Path::new("/nowhere/a/b"), volumes),
        Some(200 * GIB)
    );
    assert_eq!(
        free_at_or_above(Path::new("/small/a/b"), volumes),
        Some(5 * GIB)
    );
}

#[test]
fn only_a_literal_absolute_directory_is_seen_never_one_the_hook_expands() {
    let dir = tempfile::tempdir().unwrap();
    // The lexer would expand these from this process's environment, which for
    // an orchestrated terminal is the app's: they are not seen, so the working
    // directory decides alone.
    for command in [
        "cargo build --target-dir ~/target",
        "cargo build --target-dir=~/target",
        "CARGO_TARGET_DIR=~/target cargo build",
        "cargo build --target-dir $HOME/target",
        "cargo build --target-dir \"$HOME/target\"",
        "cargo build --target-dir ${HOME}/target",
        "CARGO_TARGET_DIR=$TMPDIR/t cargo build",
        "cargo build --target-dir /small/$X",
    ] {
        assert!(
            classify(command).build_dirs.is_empty(),
            "{command}: {:?}",
            classify(command).build_dirs
        );
    }
    assert_eq!(
        classify("cargo build --target-dir /small/t").build_dirs,
        ["/small/t"]
    );
    assert!(judged("cargo build --target-dir ~/t", &on_volumes(dir.path(), 200)).is_silent());
}

#[test]
fn a_directory_changed_into_by_the_line_is_not_covered_and_the_docs_say_so() {
    let dir = tempfile::tempdir().unwrap();
    // `cd /small && cargo build` writes to /small, which the hook cannot know
    // without evaluating the shell: only the cwd it was given is read.
    assert!(judged("cd /small && cargo build", &on_volumes(dir.path(), 200)).is_silent());
    let docs = include_str!("../../../docs/LOAD_GOVERNOR.md");
    for named in [
        "cd /elsewhere && cargo build",
        "export CARGO_TARGET_DIR",
        "CARGO_ENCODED_RUSTFLAGS",
        "build.target-dir",
        "--target-dir=~/x",
    ] {
        assert!(
            docs.contains(named),
            "the uncovered case {named:?} is named"
        );
    }
}

#[test]
fn an_unreadable_given_cwd_advises_and_never_refuses_or_passes() {
    let dir = tempfile::tempdir().unwrap();
    let mut g = with_free(dir.path(), 200);
    g.disk.free_bytes = None;
    let out = judged("cargo build", &g);
    assert_eq!(decision_of(&out), None);
    assert!(
        context(&out).contains("could not be read"),
        "{}",
        context(&out)
    );
}

#[test]
fn the_real_reader_reads_a_real_directory_and_says_none_for_a_missing_or_relative_one() {
    let serial = crate::test_support::env::env_serial();
    let root = tempfile::tempdir().unwrap();
    let home = root.path().join("home");
    std::fs::create_dir_all(&home).unwrap();
    let tools = root.path().join("tools.json");
    std::fs::write(&tools, r#"{"version":1,"host_load_hook_mode":"enforce"}"#).unwrap();
    let _env = crate::test_support::env::bind_env(&serial)
        .set("HOME", &home)
        .set("XDG_CONFIG_HOME", home.join(".config"))
        .set(crate::tool_config::TOOL_CONFIG_ENV, &tools)
        .invalidating(crate::tool_config::invalidate_cache);
    let here = root.path().to_string_lossy().into_owned();
    let free = gather_live(&here).disk.free_bytes;
    // Windows has no free-space reader (`sysload::free_bytes` is Unsupported):
    // the answer there is unknown, never a number guessed from another source.
    if cfg!(unix) {
        assert!(free.is_some_and(|bytes| bytes > 0), "{free:?}");
    } else {
        assert_eq!(free, None, "no reader: unknown");
    }
    let gone = root.path().join("missing").to_string_lossy().into_owned();
    assert_eq!(
        gather_live(&gone).disk.free_bytes,
        None,
        "a missing cwd is not has room"
    );
    assert_eq!(gather_live("relative/dir").disk.free_bytes, None);
    assert_eq!(
        gather_live("").disk.free_bytes.is_some(),
        cfg!(unix),
        "no cwd given: the process's own directory, where a reader exists"
    );
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
            ..LeaseFacts::default()
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
            ..LeaseFacts::default()
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
        refusing: None,
        wait_max_s: 1_800,
        wait_hard_s: 14_400,
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
            text.contains("queue for up to 30 minutes")
                && text.contains("run it in the background"),
            "{text}"
        );
        assert!(
            !text.contains("600000"),
            "no timeout is promised that the wrapper's wait can outlast: {text}"
        );
        assert_eq!(decision_of(&out), None, "a queue is never a refusal");
    }
    // A refusal stays a refusal and does not grow a queue line it cannot use.
    let refused = judged("cargo build", &with_lease(severe(dir.path()), facts));
    assert_eq!(decision_of(&refused), Some(PermissionDecision::Deny));
    assert!(!reason(&refused).contains("slot is held"));
}

#[test]
fn a_heavy_command_under_load_that_refuses_builds_is_not_told_every_slot_is_held() {
    let dir = tempfile::tempdir().unwrap();
    let facts = LeaseFacts {
        on: true,
        not_gated: None,
        full: None,
        refusing: Some("no new builds while the host load is severe".into()),
        wait_max_s: 1_800,
        wait_hard_s: 14_400,
    };
    let out = judged(
        "cargo build",
        &with_lease(
            gathered(verdict(Tier::Clear, None, true), dir.path()),
            facts,
        ),
    );
    let text = context(&out);
    assert!(
        text.contains(
            "Host load is refusing new builds (no new builds while the host load is severe)"
        ),
        "{text}"
    );
    assert!(text.contains("up to 240 minutes"), "{text}");
    assert!(
        !text.contains("slot is held") && !text.contains("()"),
        "{text}"
    );
}

fn active_steward(binding: &str, reason: &str) -> crate::harness::steward::ActiveThrottleRecord {
    crate::harness::steward::ActiveThrottleRecord {
        version: crate::harness::steward::LEASE_VERSION,
        writer_pid: std::process::id(),
        writer_start_us: 1,
        issued_at_ms: 1_000,
        expires_at_ms: u64::MAX / 2,
        tier: "severe".into(),
        binding: binding.into(),
        reason: reason.into(),
        pace_ms: 900,
        repositories: Vec::new(),
    }
}

#[test]
fn steward_critical_slowdown_enforces_heavy_refusal_even_in_advise_or_off_and_respects_exempt() {
    let dir = tempfile::tempdir().unwrap();
    let throttle = active_steward("swap", "swap in use is critically high");

    // Even when `host_load_hook_mode` is `Advise`, active steward slowdown refuses heavy builds.
    let mut advise_g = severe(dir.path());
    advise_g.mode = HostLoadHookMode::Advise;
    advise_g.steward_throttle = Some(throttle.clone());
    let out = judged("cargo test", &advise_g);
    assert_eq!(decision_of(&out), Some(PermissionDecision::Deny));
    assert!(reason(&out).contains("swap in use"), "{}", reason(&out));

    // Even when `host_load_hook_mode` is `Off` or the snapshot temporarily reads Clear,
    // an active steward critical lease still refuses heavy builds.
    let mut off_g = gathered(verdict(Tier::Clear, None, true), dir.path());
    off_g.mode = HostLoadHookMode::Off;
    off_g.steward_throttle = Some(throttle.clone());
    let out = judged("cargo build", &off_g);
    assert_eq!(decision_of(&out), Some(PermissionDecision::Deny));
    assert!(
        reason(&out).contains("Manvi system steward critical slowdown"),
        "{}",
        reason(&out)
    );
    // The gate's own setting does not lift a steward refusal, so the text must
    // name the steward's switch instead.
    assert!(
        reason(&out).contains("Slow agents when host load is critical")
            && !reason(&out).contains("Advise or Off"),
        "{}",
        reason(&out)
    );

    // Disk-bound steward slowdown gives disk recovery instructions rather than "wait".
    let mut disk_g = gathered(verdict(Tier::Clear, None, true), dir.path());
    disk_g.steward_throttle = Some(active_steward(
        "low_disk",
        "free disk space is critically low",
    ));
    let out = judged("cargo build", &disk_g);
    assert_eq!(decision_of(&out), Some(PermissionDecision::Deny));
    assert!(
        reason(&out).contains("free disk space first"),
        "{}",
        reason(&out)
    );

    // Heavy-lane holder (`exempt = true`) is never blocked by steward slowdown.
    let mut exempt_g = off_g;
    exempt_g.exempt = true;
    assert!(judged("cargo build", &exempt_g).is_silent());
}

#[test]
fn steward_critical_slowdown_paces_unclassified_and_advise_commands_within_hook_budget() {
    let throttle = active_steward("memory_pressure", "the kernel reports memory pressure");

    let paced = Cell::new(Duration::ZERO);
    let out = judge_with_steward_and_pacer(
        &call("python3 scripts/custom_runner.py"),
        || unreachable!("unclassified commands do not gather snapshot"),
        || Some(throttle.clone()),
        Some(std::time::Instant::now()),
        |d| paced.set(d),
    );
    assert_eq!(
        decision_of(&out),
        None,
        "unclassified shell calls are paced, not hard-denied"
    );
    assert_eq!(paced.get(), Duration::from_millis(900));
    assert!(
        context(&out).contains("Manvi system steward critical slowdown")
            && context(&out).contains("900ms"),
        "{}",
        context(&out)
    );

    // When `STARTED` has nearly consumed the non-reserved window, pacing clamps so
    // `command_gate_with` never loses its 6s reserve.
    let started_late = std::time::Instant::now()
        .checked_sub(Duration::from_millis(2_400))
        .unwrap_or_else(std::time::Instant::now);
    assert_eq!(pace_duration_for(1_000, Some(started_late)), Duration::ZERO);
}

#[test]
fn without_a_hook_deadline_the_steward_slowdown_advises_but_never_sleeps() {
    // The app judges each input line of an orchestrated child terminal with no
    // hook deadline; a pause per line there would stall it for the whole send.
    let throttle = active_steward("memory_pressure", "the kernel reports memory pressure");
    let paced = Cell::new(Some(Duration::from_secs(99)));
    let out = judge_with_steward_and_pacer(
        &call("python3 scripts/custom_runner.py"),
        || unreachable!("unclassified commands do not gather snapshot"),
        || Some(throttle.clone()),
        None,
        |d| paced.set(Some(d)),
    );
    assert_eq!(paced.get(), Some(Duration::ZERO));
    assert_eq!(decision_of(&out), None);
    let text = context(&out);
    assert!(
        text.contains("Manvi system steward critical slowdown"),
        "{text}"
    );
    assert!(!text.contains("paced this command"), "{text}");
}

// -- the pressure-relief supervisor's status, as the hook says it ------------

mod relief {
    use super::*;
    use crate::sysload::relief_status::{
        FrozenTree, ReliefBinding, ReliefMode, ReliefRung, ReliefStatus,
    };

    const OURS: &str = "term-4000-ab12";

    fn tree(session: Option<&str>, repo: &str, advised_only: bool) -> FrozenTree {
        FrozenTree {
            pid: 4242,
            start_us: 1,
            agent_root_pid: 4000,
            terminal_session_id: session.map(str::to_owned),
            repo_path: repo.into(),
            command: "cargo test --workspace".into(),
            footprint_bytes: 9 << 30,
            since_ms: 1,
            advised_only,
        }
    }

    fn frozen(trees: Vec<FrozenTree>) -> ReliefOutcome {
        ReliefOutcome::Fresh {
            status: ReliefStatus {
                rung: ReliefRung::Freeze,
                binding: Some(ReliefBinding::CompressorSegments),
                frozen: trees,
                ..ReliefStatus::at_rest(ReliefMode::Act, 1)
            },
        }
    }

    fn with(relief: ReliefOutcome, dir: &Path) -> Gathered {
        let mut g = gathered(verdict(Tier::Elevated, Some(Binding::Swap), true), dir);
        g.relief = relief;
        g.terminal_session = Some(OURS.into());
        g
    }

    const HEAVY: &str = "cargo test --lib";

    #[test]
    fn a_tree_paused_for_this_session_is_named_and_the_agent_is_told_to_wait() {
        let dir = tempfile::tempdir().unwrap();
        let g = with(
            frozen(vec![tree(Some(OURS), "/elsewhere", false)]),
            dir.path(),
        );
        let text = context(&judged(HEAVY, &g));
        for must in [
            "paused `cargo test --workspace`",
            "memory protection",
            "compressor",
            "resume automatically",
            "Do not retry",
            "in parallel",
        ] {
            assert!(text.contains(must), "lacks {must:?}: {text}");
        }
    }

    #[test]
    fn a_tree_is_also_matched_by_the_sessions_cwd_inside_its_repository() {
        let dir = tempfile::tempdir().unwrap();
        // The call's cwd is /repo; the tree has no terminal id.
        let g = with(frozen(vec![tree(None, "/repo", false)]), dir.path());
        assert!(context(&judged(HEAVY, &g)).contains("paused `cargo test"));
        // A sibling directory with the same prefix is not inside it.
        let g = with(frozen(vec![tree(None, "/rep", false)]), dir.path());
        assert!(!context(&judged(HEAVY, &g)).contains("paused `cargo test"));
    }

    #[test]
    fn a_tree_paused_for_another_session_is_not_this_sessions_business() {
        let dir = tempfile::tempdir().unwrap();
        let g = with(
            frozen(vec![tree(Some("term-9-zz"), "/other/repo", false)]),
            dir.path(),
        );
        let text = context(&judged(HEAVY, &g));
        assert!(!text.contains("paused `"), "{text}");
        assert!(!text.contains("could NOT check"), "{text}");
    }

    #[test]
    fn an_advised_only_tree_is_not_reported_as_paused() {
        let dir = tempfile::tempdir().unwrap();
        let g = with(frozen(vec![tree(Some(OURS), "/repo", true)]), dir.path());
        assert!(!context(&judged(HEAVY, &g)).contains("paused `"));
    }

    #[test]
    fn a_slowdown_alone_is_one_line_of_advice() {
        let dir = tempfile::tempdir().unwrap();
        let relief = ReliefOutcome::Fresh {
            status: ReliefStatus {
                rung: ReliefRung::Slow,
                binding: Some(ReliefBinding::Swap),
                slowed: 5,
                ..ReliefStatus::at_rest(ReliefMode::Act, 1)
            },
        };
        let text = context(&judged(HEAVY, &with(relief, dir.path())));
        assert!(text.contains("lowered the priority of 5"), "{text}");
        assert!(text.contains("swap is nearly full"), "{text}");
        assert!(!text.contains("paused `"), "{text}");
    }

    #[test]
    fn a_stale_or_unreadable_status_says_it_could_not_check_and_never_nothing_paused() {
        let dir = tempfile::tempdir().unwrap();
        let stale = ReliefOutcome::Stale {
            status: ReliefStatus::at_rest(ReliefMode::Act, 1),
            age_ms: 61_000,
        };
        let unreadable = ReliefOutcome::Unreadable {
            reason: "not a status this build understands".into(),
        };
        for (relief, expect) in [(stale, "61s ago"), (unreadable, "not a status")] {
            let g = with(relief, dir.path());
            let text = context(&judged(HEAVY, &g));
            assert!(text.contains("could NOT check"), "{text}");
            assert!(text.contains(expect), "{text}");
            // It warns against assuming nothing is paused; it never claims it.
            assert!(text.contains("Do not assume nothing is paused"), "{text}");
            assert!(!text.contains("paused `"), "{text}");
        }
    }

    #[test]
    fn an_unchecked_status_is_said_once_per_session() {
        let dir = tempfile::tempdir().unwrap();
        let g = with(
            ReliefOutcome::Unreadable {
                reason: "garbled".into(),
            },
            dir.path(),
        );
        assert!(context(&judged(HEAVY, &g)).contains("could NOT check"));
        assert!(!context(&judged(HEAVY, &g)).contains("could NOT check"));
    }

    #[test]
    fn an_absent_status_and_a_status_at_rest_are_silent() {
        let dir = tempfile::tempdir().unwrap();
        for relief in [
            ReliefOutcome::Absent,
            ReliefOutcome::Fresh {
                status: ReliefStatus::at_rest(ReliefMode::Act, 1),
            },
        ] {
            let mut g = gathered(verdict(Tier::Clear, None, true), dir.path());
            g.relief = relief;
            assert!(judged(HEAVY, &g).is_silent());
        }
    }

    #[test]
    fn a_refusal_carries_the_paused_note_in_its_reason() {
        let dir = tempfile::tempdir().unwrap();
        let mut g = severe(dir.path());
        g.relief = frozen(vec![tree(Some(OURS), "/repo", false)]);
        g.terminal_session = Some(OURS.into());
        let out = judged(HEAVY, &g);
        assert_eq!(decision_of(&out), Some(PermissionDecision::Deny));
        assert!(reason(&out).contains("paused `cargo test --workspace`"));
    }

    #[test]
    fn a_gate_set_to_off_and_the_heavy_lane_hear_nothing() {
        let dir = tempfile::tempdir().unwrap();
        let mut g = with(frozen(vec![tree(Some(OURS), "/repo", false)]), dir.path());
        g.mode = HostLoadHookMode::Off;
        assert!(judged(HEAVY, &g).is_silent());
        let mut g = with(frozen(vec![tree(Some(OURS), "/repo", false)]), dir.path());
        g.exempt = true;
        assert!(judged(HEAVY, &g).is_silent());
    }

    #[test]
    fn the_echoed_command_is_cleaned_and_cut() {
        let dir = tempfile::tempdir().unwrap();
        let mut t = tree(Some(OURS), "/repo", false);
        t.command = format!("cargo test \u{202e}{}", "x".repeat(400));
        let text = context(&judged(HEAVY, &with(frozen(vec![t]), dir.path())));
        assert!(!text.contains('\u{202e}'), "{text}");
        assert!(text.contains('…') && text.len() < MAX_TEXT, "{text}");
    }
}
