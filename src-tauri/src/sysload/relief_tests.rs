use super::*;
use crate::sysload::{CompressorSegments, Governor, Level, Reading};
use std::sync::{Arc, Mutex};

const STEP: Duration = Duration::from_secs(2);
const MIB: u64 = 1 << 20;

// ---------------------------------------------------------------------------
// Fakes. Nothing here signals a process this test did not start.
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq, Eq)]
enum Call {
    Stop(i32),
    Cont(i32),
    Bg(i32, bool),
}

#[derive(Clone, Default)]
struct Fake {
    calls: Arc<Mutex<Vec<Call>>>,
    /// Processes that exist, by start time. Empty accepts every pid.
    live: Arc<Mutex<HashMap<i32, u64>>>,
    background: Arc<Mutex<HashSet<i32>>>,
    /// Where the journal is, so a stop can check it was written first.
    journal_dir: Arc<Mutex<Option<PathBuf>>>,
    journaled_before_stop: Arc<Mutex<Vec<(i32, bool)>>>,
}

impl Fake {
    fn calls(&self) -> Vec<Call> {
        self.calls.lock().unwrap().clone()
    }

    fn stops(&self) -> Vec<i32> {
        self.calls()
            .into_iter()
            .filter_map(|c| match c {
                Call::Stop(pid) => Some(pid),
                _ => None,
            })
            .collect()
    }

    fn conts(&self) -> Vec<i32> {
        self.calls()
            .into_iter()
            .filter_map(|c| match c {
                Call::Cont(pid) => Some(pid),
                _ => None,
            })
            .collect()
    }

    fn backgrounded(&self) -> Vec<i32> {
        self.calls()
            .into_iter()
            .filter_map(|c| match c {
                Call::Bg(pid, true) => Some(pid),
                _ => None,
            })
            .collect()
    }

    fn restored(&self) -> Vec<i32> {
        self.calls()
            .into_iter()
            .filter_map(|c| match c {
                Call::Bg(pid, false) => Some(pid),
                _ => None,
            })
            .collect()
    }

    fn check(&self, pid: i32, start_us: u64) -> Result<(), Result<Delivery, String>> {
        let live = self.live.lock().unwrap();
        if live.is_empty() {
            return Ok(());
        }
        match live.get(&pid) {
            Some(start) if *start == start_us => Ok(()),
            Some(_) => Err(Err("pid now belongs to a different process".into())),
            None => Err(Ok(Delivery::Gone)),
        }
    }
}

impl Actuator for Fake {
    fn set_background(&self, pid: i32, start_us: u64, enable: bool) -> Result<bool, String> {
        self.calls.lock().unwrap().push(Call::Bg(pid, enable));
        match self.check(pid, start_us) {
            Ok(()) => {}
            Err(Ok(_)) => return Err("process is no longer running".into()),
            Err(Err(why)) => return Err(why),
        }
        let mut background = self.background.lock().unwrap();
        Ok(if enable {
            background.insert(pid)
        } else {
            background.remove(&pid)
        })
    }

    fn signal(&self, pid: i32, start_us: u64, signal: Signal) -> Result<Delivery, String> {
        match signal {
            Signal::Stop => {
                if let Some(dir) = self.journal_dir.lock().unwrap().as_ref() {
                    let listed = read_journal(dir).ok().flatten().is_some_and(|j| {
                        j.entries
                            .iter()
                            .any(|e| e.pid == pid && e.rung == JournalRung::Freeze)
                    });
                    self.journaled_before_stop
                        .lock()
                        .unwrap()
                        .push((pid, listed));
                }
                self.calls.lock().unwrap().push(Call::Stop(pid));
            }
            Signal::Cont => self.calls.lock().unwrap().push(Call::Cont(pid)),
        }
        match self.check(pid, start_us) {
            Ok(()) => Ok(Delivery::Delivered),
            Err(outcome) => outcome,
        }
    }
}

#[derive(Clone)]
struct FakeProbe {
    default_cwd: PathBuf,
    cwd: Arc<Mutex<HashMap<i32, PathBuf>>>,
    env: Arc<Mutex<HashMap<i32, Vec<String>>>>,
    env_unreadable: Arc<Mutex<HashSet<i32>>>,
    leases: Arc<Mutex<Result<BTreeSet<i32>, String>>>,
}

impl FakeProbe {
    /// A probe whose every process works in a clean worktree.
    fn new(clean: &Path) -> Self {
        Self {
            default_cwd: clean.to_path_buf(),
            cwd: Arc::default(),
            env: Arc::default(),
            env_unreadable: Arc::default(),
            leases: Arc::new(Mutex::new(Ok(BTreeSet::new()))),
        }
    }
}

impl Probe for FakeProbe {
    fn cwd(&self, pid: i32) -> Option<PathBuf> {
        Some(
            self.cwd
                .lock()
                .unwrap()
                .get(&pid)
                .cloned()
                .unwrap_or_else(|| self.default_cwd.clone()),
        )
    }

    fn environment(&self, pid: i32) -> Option<Vec<String>> {
        if self.env_unreadable.lock().unwrap().contains(&pid) {
            return None;
        }
        Some(
            self.env
                .lock()
                .unwrap()
                .get(&pid)
                .cloned()
                .unwrap_or_default(),
        )
    }

    fn lease_pids(&self) -> Result<BTreeSet<i32>, String> {
        self.leases.lock().unwrap().clone()
    }
}

/// A clean worktree: a `.git` directory with no lock in it.
fn clean_worktree() -> tempfile::TempDir {
    let dir = tempfile::tempdir().unwrap();
    std::fs::create_dir(dir.path().join(".git")).unwrap();
    dir
}

struct Rig {
    relief: Relief,
    fake: Fake,
    probe: FakeProbe,
    mode: Arc<Mutex<SupervisorMode>>,
    trusted: Arc<Mutex<bool>>,
    dir: tempfile::TempDir,
    _worktree: tempfile::TempDir,
    now: Instant,
}

impl Rig {
    fn new(mode: SupervisorMode) -> Self {
        let dir = tempfile::tempdir().unwrap();
        let worktree = clean_worktree();
        let fake = Fake::default();
        *fake.journal_dir.lock().unwrap() = Some(dir.path().to_path_buf());
        let probe = FakeProbe::new(worktree.path());
        let mode = Arc::new(Mutex::new(mode));
        let trusted = Arc::new(Mutex::new(true));
        let relief = Relief::new(
            dir.path().to_path_buf(),
            Box::new(fake.clone()),
            Box::new(probe.clone()),
            Box::new({
                let mode = mode.clone();
                move || *mode.lock().unwrap()
            }),
            Box::new({
                let trusted = trusted.clone();
                move |_| *trusted.lock().unwrap()
            }),
        );
        Self {
            relief,
            fake,
            probe,
            mode,
            trusted,
            dir,
            _worktree: worktree,
            now: Instant::now(),
        }
    }

    fn tick(&mut self, input: &GovernorInput, table: &ReliefTable) {
        self.now += STEP;
        self.relief.tick(self.now, input, &|| table.clone());
    }

    fn ticks(&mut self, n: usize, input: impl Fn(usize) -> GovernorInput, table: &ReliefTable) {
        for i in 0..n {
            let sample = input(i);
            self.tick(&sample, table);
        }
    }
}

fn input(level: Level, swap_mib: u64) -> GovernorInput {
    let mut input = GovernorInput::test_pressure(level);
    input.readings.swap_used = Reading::Value(swap_mib * MIB);
    input
}

/// Severe, with swap climbing `rate` MiB per sample from 2 GiB.
fn rising(rate: u64) -> impl Fn(usize) -> GovernorInput {
    move |i| input(Level::Critical, 2048 + rate * i as u64)
}

/// Severe, with swap parked at 40 GiB.
fn flat(_: usize) -> GovernorInput {
    input(Level::Critical, 40 * 1024)
}

fn proc(pid: i32, ppid: i32, name: &str, mib: u64) -> ReliefProc {
    ReliefProc {
        pid,
        ppid,
        start_us: 1_000_000 + pid as u64,
        name: name.into(),
        footprint: Some(mib * MIB),
        exempt: false,
    }
}

fn session(root: i32, launched: bool, members: Vec<ReliefProc>) -> ReliefSession {
    ReliefSession {
        root: ReliefProc {
            exempt: true,
            ..proc(root, 1, "claude", 300)
        },
        kind: super::super::AgentKind::Claude,
        launched,
        terminal_session_id: Some(format!("term-{root}")),
        run_id: None,
        members,
    }
}

/// An agent (100) whose shell (101) runs a `cargo` (102) with a `rustc`
/// (103), beside an MCP server (105) the agent started directly.
fn standard() -> ReliefTable {
    ReliefTable {
        sessions: vec![session(
            100,
            true,
            vec![
                proc(101, 100, "zsh", 10),
                proc(102, 101, "cargo", 700),
                proc(103, 102, "rustc", 400),
                proc(105, 100, "npx", 3000),
                proc(106, 105, "node", 2000),
            ],
        )],
    }
}

fn live_all(rig: &Rig, table: &ReliefTable) {
    let mut live = rig.fake.live.lock().unwrap();
    for s in &table.sessions {
        live.insert(s.root.pid, s.root.start_us);
        for m in &s.members {
            live.insert(m.pid, m.start_us);
        }
    }
}

// ---------------------------------------------------------------------------
// The trigger.
// ---------------------------------------------------------------------------

fn trigger_run(mut feed: impl FnMut(usize) -> Sample, n: usize) -> Vec<(usize, Verdict)> {
    let mut trigger = Trigger::default();
    let mut now = Instant::now();
    let mut out = Vec::new();
    for i in 0..n {
        now += STEP;
        let verdict = trigger.observe(now, &feed(i));
        if verdict.freeze == Step::Begin {
            trigger.began(now, true);
        }
        out.push((i, verdict));
    }
    out
}

fn severe_sample(swap_mib: u64, segment_pct: f64) -> Sample {
    Sample {
        tier: Some((Tier::Severe, Some(Binding::Swap))),
        swap_used: Some(swap_mib * MIB),
        segment_pct: Some(segment_pct),
    }
}

#[test]
fn severe_for_sixty_seconds_with_rising_swap_begins_a_freeze_on_the_thirtieth_sample() {
    let out = trigger_run(|i| severe_sample(2048 + 100 * i as u64, 10.0), 40);
    let first = out
        .iter()
        .find(|(_, v)| v.freeze == Step::Begin)
        .expect("a freeze begins");
    assert_eq!(
        first.0,
        SEVERE_SAMPLES as usize - 1,
        "60 s of Severe, no sooner"
    );
}

#[test]
fn a_high_but_flat_swap_never_begins_a_freeze() {
    // 400 samples (13 minutes) of Severe with swap parked high.
    let out = trigger_run(|_| severe_sample(40 * 1024, 10.0), 400);
    assert!(out.iter().all(|(_, v)| v.freeze == Step::Hold));
    // ...though rung A holds the whole time.
    assert!(out.last().unwrap().1.slow);
}

#[test]
fn deterioration_has_a_minimum_swap_rise_and_a_minimum_segment_rise() {
    // 25 samples separate the means of the first and last five readings.
    let at = |rate_mib: u64| {
        trigger_run(|i| severe_sample(2048 + rate_mib * i as u64, 10.0), 30)
            .iter()
            .any(|(_, v)| v.freeze == Step::Begin)
    };
    assert!(
        !at(10),
        "250 MiB is under the {SWAP_RISE_BYTES}-byte minimum"
    );
    assert!(at(11), "275 MiB is over it");
    let segments = |step: f64| {
        trigger_run(|i| severe_sample(40 * 1024, 50.0 + step * i as f64), 30)
            .iter()
            .any(|(_, v)| v.freeze == Step::Begin)
    };
    assert!(!segments(0.03), "0.75 points is under the segment minimum");
    assert!(segments(0.05), "1.25 points is over it");
}

#[test]
fn one_noisy_reading_is_not_a_trend() {
    let out = trigger_run(
        |i| severe_sample(if i == 29 { 60 * 1024 } else { 2048 }, 10.0),
        60,
    );
    assert!(out.iter().all(|(_, v)| v.freeze == Step::Hold));
}

#[test]
fn unreadable_readings_never_begin_a_freeze() {
    let out = trigger_run(
        |_| Sample {
            tier: Some((Tier::Severe, Some(Binding::MemoryPressure))),
            swap_used: None,
            segment_pct: None,
        },
        200,
    );
    assert!(out.iter().all(|(_, v)| v.freeze == Step::Hold));
}

#[test]
fn a_severe_run_survives_one_dip_but_not_a_recovery() {
    // A single Elevated reading inside the minute does not restart it.
    let dip = trigger_run(
        |i| {
            let mut s = severe_sample(2048 + 100 * i as u64, 10.0);
            if i == 12 {
                s.tier = Some((Tier::Elevated, Some(Binding::Swap)));
            }
            s
        },
        45,
    );
    assert!(dip.iter().any(|(_, v)| v.freeze == Step::Begin));
    // Five quiet readings are a recovery: the minute starts over.
    let recovered = trigger_run(
        |i| {
            let mut s = severe_sample(2048 + 100 * i as u64, 10.0);
            if (12..12 + SEVERE_BREAK_SAMPLES as usize).contains(&i) {
                s.tier = Some((Tier::Clear, None));
            }
            s
        },
        40,
    );
    assert!(
        recovered.iter().all(|(_, v)| v.freeze == Step::Hold),
        "the run restarted at sample 17, so 40 samples cannot reach 30 of it"
    );
}

#[test]
fn slowing_follows_elevated_for_three_samples_and_lets_go_after_five_quiet_ones() {
    let elevated = Sample {
        tier: Some((Tier::Elevated, Some(Binding::MemoryPressure))),
        swap_used: None,
        segment_pct: None,
    };
    let clear = Sample {
        tier: Some((Tier::Clear, None)),
        ..elevated
    };
    let mut trigger = Trigger::default();
    let mut now = Instant::now();
    let mut slow = Vec::new();
    for s in [elevated, elevated, elevated]
        .iter()
        .chain([clear; 5].iter())
    {
        now += STEP;
        slow.push(trigger.observe(now, s).slow);
    }
    assert_eq!(slow, [false, false, true, true, true, true, true, false]);
}

#[test]
fn a_severe_that_cpu_load_or_the_disk_holds_is_not_a_memory_sample() {
    // Each is Severe for the governor as a whole, and Clear for the relief.
    let mut cpu = GovernorInput::test_pressure(Level::Normal);
    cpu.readings.load = Reading::Value([500.0, 500.0, 500.0]);
    let mut disk = GovernorInput::test_pressure(Level::Normal);
    disk.readings.free_disk = Reading::Value(MIB);
    for (what, input, binding) in [
        ("cpu", cpu, Binding::CpuLoad),
        ("disk", disk, Binding::LowDisk),
    ] {
        let now = Instant::now();
        let whole = Governor::instant(now, input.clone()).verdict(now);
        assert!(
            whole.tier >= Tier::Elevated && whole.binding == Some(binding),
            "{what}: the governor itself sheds on it: {whole:?}"
        );
        assert_eq!(
            Sample::of(&input).tier,
            Some((Tier::Clear, None)),
            "{what}: the relief reads nothing"
        );
    }
}

#[test]
fn memory_bound_samples_name_their_input() {
    for (input, want) in [
        (
            GovernorInput::test_pressure(Level::Critical),
            Binding::MemoryPressure,
        ),
        (
            GovernorInput {
                compressor_thrash: true,
                ..GovernorInput::test_pressure(Level::Normal)
            },
            Binding::CompressorThrash,
        ),
    ] {
        assert_eq!(Sample::of(&input).tier, Some((Tier::Severe, Some(want))));
    }
    let mut swap = GovernorInput::test_pressure(Level::Normal);
    swap.readings.swap_used = Reading::Value(60 << 30);
    assert!(matches!(
        Sample::of(&swap).tier,
        Some((tier, Some(Binding::Swap))) if tier >= Tier::Elevated
    ));
    let mut segments = GovernorInput::test_pressure(Level::Normal);
    segments.readings.segments = Reading::Value(CompressorSegments {
        total: 100,
        limit: 100,
    });
    assert!(matches!(
        Sample::of(&segments).tier,
        Some((tier, Some(Binding::CompressorSegments))) if tier >= Tier::Elevated
    ));
}

#[test]
fn a_frozen_tree_resumes_after_fifteen_flat_samples_while_swap_stays_high() {
    let mut trigger = Trigger::default();
    let mut now = Instant::now();
    let mut began = None;
    let mut resumed = None;
    for i in 0..80usize {
        now += STEP;
        // 30 rising samples, then swap parks at its high mark.
        let swap = 2048 + 100 * (i.min(29) as u64);
        let verdict = trigger.observe(now, &severe_sample(swap, 10.0));
        match verdict.freeze {
            Step::Begin => {
                trigger.began(now, true);
                began = Some(i);
            }
            Step::Resume(reason) => {
                resumed = Some((i, reason));
                break;
            }
            Step::Hold => {}
        }
    }
    let began = began.expect("began");
    let (at, reason) = resumed.expect("resumed");
    assert_eq!(reason, ResumeReason::Flat);
    // The 15th sample after the freeze whose last 15 readings did not rise.
    assert_eq!(at, began + FLAT_SAMPLES);
}

#[test]
fn the_ten_minute_cap_resumes_a_freeze_whose_swap_keeps_rising() {
    let mut trigger = Trigger::default();
    let origin = Instant::now();
    let mut now = origin;
    let mut frozen_at = None;
    let mut ended = None;
    for i in 0..600usize {
        now += STEP;
        let verdict = trigger.observe(now, &severe_sample(2048 + 100 * i as u64, 10.0));
        match verdict.freeze {
            Step::Begin => {
                trigger.began(now, true);
                frozen_at = Some(now);
            }
            Step::Resume(reason) => {
                ended = Some((now, reason));
                break;
            }
            Step::Hold => {}
        }
    }
    let (end, reason) = ended.expect("a freeze must end");
    assert_eq!(reason, ResumeReason::Cap);
    assert_eq!(end - frozen_at.unwrap(), FREEZE_CAP);
}

#[test]
fn the_cap_is_measured_on_the_clock_not_in_samples() {
    // A starved sampler ticks every 40 s: the cap still lands at ten minutes.
    let mut trigger = Trigger::default();
    let mut now = Instant::now();
    let mut frozen_at = None;
    for i in 0..200usize {
        now += if frozen_at.is_some() { 40 * STEP } else { STEP };
        let verdict = trigger.observe(now, &severe_sample(2048 + 100 * i as u64, 10.0));
        match verdict.freeze {
            Step::Begin => {
                trigger.began(now, true);
                frozen_at = Some(now);
            }
            Step::Resume(reason) => {
                assert_eq!(reason, ResumeReason::Cap);
                assert!(now - frozen_at.unwrap() >= FREEZE_CAP);
                assert!(now - frozen_at.unwrap() < FREEZE_CAP + 80 * STEP);
                return;
            }
            Step::Hold => {}
        }
    }
    panic!("the cap never ended the freeze");
}

#[test]
fn a_freeze_ends_when_the_tier_drops_and_unreadable_readings_never_hold_it() {
    for (name, after) in [
        (
            "calm",
            Sample {
                tier: Some((Tier::Clear, None)),
                swap_used: Some(2048 * MIB),
                segment_pct: Some(10.0),
            },
        ),
        (
            "ungovernable",
            Sample {
                tier: None,
                swap_used: None,
                segment_pct: None,
            },
        ),
    ] {
        let mut trigger = Trigger::default();
        let mut now = Instant::now();
        for i in 0..SEVERE_SAMPLES as usize {
            now += STEP;
            let v = trigger.observe(now, &severe_sample(2048 + 100 * i as u64, 10.0));
            if v.freeze == Step::Begin {
                trigger.began(now, true);
            }
        }
        assert!(trigger.frozen(), "{name}");
        let mut ended = None;
        for n in 0..SEVERE_BREAK_SAMPLES as usize + 1 {
            now += STEP;
            if let Step::Resume(reason) = trigger.observe(now, &after).freeze {
                ended = Some((n, reason));
                break;
            }
        }
        let (n, reason) = ended.expect(name);
        assert_eq!(
            (n, reason),
            (SEVERE_BREAK_SAMPLES as usize - 1, ResumeReason::Calmed)
        );
    }
}

#[test]
fn after_a_resume_a_whole_new_severe_minute_is_needed() {
    let mut trigger = Trigger::default();
    let mut now = Instant::now();
    let mut begins = 0;
    for i in 0..400usize {
        now += STEP;
        // Swap rises for the first 30 samples, then parks: the freeze ends
        // flat and the host stays Severe and flat, so nothing refreezes.
        let swap = 2048 + 100 * (i.min(29) as u64);
        let v = trigger.observe(now, &severe_sample(swap, 10.0));
        if v.freeze == Step::Begin {
            begins += 1;
            trigger.began(now, true);
        }
    }
    assert_eq!(begins, 1);
}

#[test]
fn a_freeze_that_selected_nothing_waits_before_looking_again() {
    let mut trigger = Trigger::default();
    let mut now = Instant::now();
    let mut begins = Vec::new();
    for i in 0..80usize {
        now += STEP;
        let v = trigger.observe(now, &severe_sample(2048 + 100 * i as u64, 10.0));
        if v.freeze == Step::Begin {
            begins.push(i);
            trigger.began(now, false);
        }
    }
    assert_eq!(begins[0], 29);
    assert_eq!(begins[1] - begins[0], RETRY_SAMPLES as usize);
}

// ---------------------------------------------------------------------------
// Selection.
// ---------------------------------------------------------------------------

fn pick(table: &ReliefTable, probe: &FakeProbe, room: usize) -> Result<Vec<Selection>, String> {
    select(
        table,
        probe,
        &|_| true,
        &BTreeSet::new(),
        room,
        &mut Vec::new(),
    )
}

fn fresh_probe() -> (FakeProbe, tempfile::TempDir) {
    let worktree = clean_worktree();
    (FakeProbe::new(worktree.path()), worktree)
}

#[test]
fn the_heaviest_subtree_below_the_agents_shell_is_chosen_not_the_shell_or_the_agent() {
    let (probe, _w) = fresh_probe();
    let picks = pick(&standard(), &probe, 3).unwrap();
    assert_eq!(picks.len(), 1);
    let chosen = &picks[0];
    assert_eq!(chosen.root.pid, 102, "cargo, with rustc below it");
    assert_eq!(
        chosen.members.iter().map(|m| m.pid).collect::<Vec<_>>(),
        [102, 103]
    );
    assert_eq!(chosen.footprint, 1100 * MIB);
    for never in [100, 101, 105, 106] {
        assert!(
            chosen.members.iter().all(|m| m.pid != never),
            "pid {never} (the agent, its shell, its MCP server) is never stopped"
        );
    }
}

#[test]
fn a_process_with_a_heavier_footprint_loses_to_the_exemptions() {
    let (probe, _w) = fresh_probe();
    let rows = |extra: Vec<ReliefProc>| {
        let mut members = vec![
            proc(101, 100, "zsh", 10),
            proc(102, 101, "cargo", 700),
            proc(103, 102, "rustc", 400),
        ];
        members.extend(extra);
        ReliefTable {
            sessions: vec![session(100, true, members)],
        }
    };
    // git, heavier than the build: never selected, the build is.
    let table = rows(vec![proc(110, 101, "git", 5000)]);
    let picks = pick(&table, &probe, 3).unwrap();
    assert_eq!(picks[0].root.pid, 102);
    // A git inside the build's own subtree bars that whole subtree.
    let table = rows(vec![proc(111, 103, "git", 1)]);
    assert!(pick(&table, &probe, 3).unwrap().is_empty());
    // A subtree member the exemption list spares bars the subtree.
    let mut table = rows(Vec::new());
    table.sessions[0].members[2].exempt = true;
    assert!(pick(&table, &probe, 3).unwrap().is_empty());
    // An MCP server under a shell, by name.
    let table = ReliefTable {
        sessions: vec![session(
            100,
            true,
            vec![
                proc(101, 100, "zsh", 10),
                proc(102, 101, "context7-mcp", 900),
            ],
        )],
    };
    assert!(pick(&table, &probe, 3).unwrap().is_empty());
}

#[test]
fn a_build_lease_holder_or_waiter_and_everything_under_it_is_never_selected() {
    let (probe, _w) = fresh_probe();
    // The holder is the cargo: the subtree and its rustc are both barred.
    *probe.leases.lock().unwrap() = Ok(BTreeSet::from([102]));
    assert!(pick(&standard(), &probe, 3).unwrap().is_empty());
    // The holder is a process inside the subtree.
    *probe.leases.lock().unwrap() = Ok(BTreeSet::from([103]));
    assert!(pick(&standard(), &probe, 3).unwrap().is_empty());
    // Another build's holder leaves this one alone.
    *probe.leases.lock().unwrap() = Ok(BTreeSet::from([4242]));
    assert_eq!(pick(&standard(), &probe, 3).unwrap().len(), 1);
    // A board that could not be read freezes nothing, and says why.
    *probe.leases.lock().unwrap() = Err("slot-3 unreadable".into());
    let why = pick(&standard(), &probe, 3).unwrap_err();
    assert!(why.contains("slot-3 unreadable"), "{why}");
}

#[test]
fn trees_carrying_a_held_marker_or_an_unreadable_environment_are_never_selected() {
    for marker in [
        "MAC_HEAVY_LABEL=go-test",
        "GITPULSE_LEASE_HELD=2",
        "CARGO_SLOT_HELD=1",
    ] {
        let (probe, _w) = fresh_probe();
        probe
            .env
            .lock()
            .unwrap()
            .insert(103, vec!["PATH=/usr/bin".into(), marker.into()]);
        assert!(
            pick(&standard(), &probe, 3).unwrap().is_empty(),
            "{marker} on a member bars the subtree"
        );
    }
    // An empty value is not a marker (the lease wrapper unsets, not blanks).
    let (probe, _w) = fresh_probe();
    probe
        .env
        .lock()
        .unwrap()
        .insert(103, vec!["MAC_HEAVY_LABEL=".into()]);
    assert_eq!(pick(&standard(), &probe, 3).unwrap().len(), 1);
    // An environment that cannot be read is unknown, not clear.
    let (probe, _w) = fresh_probe();
    probe.env_unreadable.lock().unwrap().insert(102);
    assert!(pick(&standard(), &probe, 3).unwrap().is_empty());
    // The marker on the shell above is inherited by what runs below it.
    let (probe, _w) = fresh_probe();
    probe
        .env
        .lock()
        .unwrap()
        .insert(101, vec!["MAC_HEAVY_LABEL=x".into()]);
    assert!(pick(&standard(), &probe, 3).unwrap().is_empty());
}

#[test]
fn a_live_git_index_lock_bars_the_subtree_in_a_worktree_and_in_a_linked_one() {
    // Plain checkout.
    let (probe, wt) = fresh_probe();
    std::fs::write(wt.path().join(".git/index.lock"), b"").unwrap();
    assert!(pick(&standard(), &probe, 3).unwrap().is_empty());
    std::fs::remove_file(wt.path().join(".git/index.lock")).unwrap();
    assert_eq!(pick(&standard(), &probe, 3).unwrap().len(), 1);
    // Only the process working in the locked worktree bars its own subtree.
    let other = clean_worktree();
    std::fs::write(other.path().join(".git/index.lock"), b"").unwrap();
    probe
        .cwd
        .lock()
        .unwrap()
        .insert(103, other.path().join("src"));
    assert!(pick(&standard(), &probe, 3).unwrap().is_empty());

    // A linked worktree: `.git` is a file naming a gitdir that holds the lock.
    let root = tempfile::tempdir().unwrap();
    let gitdir = root.path().join("main/.git/worktrees/lane");
    std::fs::create_dir_all(&gitdir).unwrap();
    let linked = root.path().join("lane");
    std::fs::create_dir_all(linked.join("src")).unwrap();
    std::fs::write(
        linked.join(".git"),
        format!("gitdir: {}\n", gitdir.display()),
    )
    .unwrap();
    assert!(!index_lock_live(&linked.join("src")));
    std::fs::write(gitdir.join("index.lock"), b"").unwrap();
    assert!(index_lock_live(&linked.join("src")));
    // A `.git` file that cannot be understood is a lock, not an absence.
    let broken = root.path().join("broken");
    std::fs::create_dir_all(&broken).unwrap();
    std::fs::write(broken.join(".git"), b"nonsense").unwrap();
    assert!(index_lock_live(&broken));
    // Outside every repository there is nothing to lock.
    assert!(!index_lock_live(Path::new("/")));
}

#[test]
fn only_launched_sessions_in_trusted_repositories_are_candidates() {
    let (probe, _w) = fresh_probe();
    let mut table = standard();
    table.sessions[0].launched = false;
    assert!(
        pick(&table, &probe, 3).unwrap().is_empty(),
        "observed session"
    );
    let table = standard();
    let mut notes = Vec::new();
    let untrusted = select(&table, &probe, &|_| false, &BTreeSet::new(), 3, &mut notes).unwrap();
    assert!(untrusted.is_empty());
    assert!(notes[0].contains("not trusted"), "{notes:?}");
    // A session whose working directory cannot be read is not guessed at.
    struct Blind(FakeProbe);
    impl Probe for Blind {
        fn cwd(&self, _: i32) -> Option<PathBuf> {
            None
        }
        fn environment(&self, pid: i32) -> Option<Vec<String>> {
            self.0.environment(pid)
        }
        fn lease_pids(&self) -> Result<BTreeSet<i32>, String> {
            self.0.lease_pids()
        }
    }
    let picks = select(
        &standard(),
        &Blind(probe),
        &|_| true,
        &BTreeSet::new(),
        3,
        &mut Vec::new(),
    )
    .unwrap();
    assert!(picks.is_empty());
}

#[test]
fn a_session_holds_one_subtree_and_the_host_three() {
    let (probe, _w) = fresh_probe();
    // One session with two big builds side by side: one pick.
    let table = ReliefTable {
        sessions: vec![session(
            100,
            true,
            vec![
                proc(101, 100, "zsh", 10),
                proc(102, 101, "cargo", 700),
                proc(103, 101, "go", 900),
            ],
        )],
    };
    let picks = pick(&table, &probe, 3).unwrap();
    assert_eq!(picks.len(), 1);
    assert_eq!(picks[0].root.pid, 103, "the heavier of the two");
    // Five sessions: the three heaviest, one each.
    let sessions: Vec<ReliefSession> = (0..5)
        .map(|n| {
            let base = 200 + 10 * n;
            session(
                base,
                true,
                vec![
                    proc(base + 1, base, "zsh", 10),
                    proc(base + 2, base + 1, "cargo", 500 + 100 * n as u64),
                ],
            )
        })
        .collect();
    let table = ReliefTable { sessions };
    let picks = pick(&table, &probe, MAX_FROZEN_PER_HOST).unwrap();
    assert_eq!(
        picks.iter().map(|p| p.session_pid).collect::<Vec<_>>(),
        [240, 230, 220]
    );
    // Room for none: nothing is chosen.
    assert!(pick(&table, &probe, 0).unwrap().is_empty());
    // A session that already has one frozen is not chosen again.
    let busy = BTreeSet::from([(240, 1_000_000 + 240)]);
    let picks = select(&table, &probe, &|_| true, &busy, 3, &mut Vec::new()).unwrap();
    assert!(picks.iter().all(|p| p.session_pid != 240));
}

#[test]
fn a_subtree_too_light_to_matter_is_left_running() {
    let (probe, _w) = fresh_probe();
    let table = ReliefTable {
        sessions: vec![session(
            100,
            true,
            vec![proc(101, 100, "zsh", 10), proc(102, 101, "cargo", 100)],
        )],
    };
    assert!(pick(&table, &probe, 3).unwrap().is_empty());
}

// ---------------------------------------------------------------------------
// The supervisor, with a fake host.
// ---------------------------------------------------------------------------

#[test]
fn with_no_steward_and_no_manvi_a_sustained_memory_severe_slows_then_freezes() {
    // Nothing but the relief runs here: no steward, no `manvi use` probe, no
    // spawn gate, no subprocess.
    let table = standard();
    let mut rig = Rig::new(SupervisorMode::Act);
    live_all(&rig, &table);
    rig.ticks(ENTER_SAMPLES as usize, rising(100), &table);
    let slowed = rig.fake.backgrounded();
    assert!(
        slowed.contains(&102) && slowed.contains(&103),
        "rung A: {slowed:?}"
    );
    assert!(
        !slowed.contains(&100),
        "the agent itself is never put in the background"
    );
    assert!(
        rig.fake.stops().is_empty(),
        "no freeze in the first seconds"
    );

    rig.ticks(SEVERE_SAMPLES as usize, |i| rising(100)(i + 3), &table);
    assert_eq!(
        rig.fake.stops(),
        [102, 103],
        "rung B: the build, root first"
    );
    assert_eq!(
        rig.fake.journaled_before_stop.lock().unwrap().as_slice(),
        [(102, true), (103, true)],
        "each process was in the journal before it was stopped"
    );
    let status = &rig.relief.status;
    assert_eq!(status.rung, Rung::Freeze);
    assert_eq!(status.tier, Some(Tier::Severe));
    assert_eq!(status.frozen.len(), 1);
    let frozen = &status.frozen[0];
    assert_eq!(frozen.command, "cargo");
    assert_eq!(frozen.session_id, "100-1000100");
    assert_eq!(frozen.terminal_session_id.as_deref(), Some("term-100"));
    assert_eq!(frozen.footprint_bytes, 1100 * MIB);
    assert!(!frozen.advised_only);
    assert!(status
        .last_action
        .as_ref()
        .unwrap()
        .detail
        .contains("froze cargo"));
}

#[test]
fn a_high_but_flat_swap_slows_but_never_freezes() {
    let table = standard();
    let mut rig = Rig::new(SupervisorMode::Act);
    live_all(&rig, &table);
    rig.ticks(300, flat, &table);
    assert!(!rig.fake.backgrounded().is_empty());
    assert!(rig.fake.stops().is_empty());
    assert_eq!(rig.relief.status.rung, Rung::Slow);
}

#[test]
fn a_severe_bound_to_cpu_load_or_the_disk_neither_slows_nor_freezes() {
    let table = standard();
    for what in ["cpu", "disk"] {
        let mut rig = Rig::new(SupervisorMode::Act);
        live_all(&rig, &table);
        rig.ticks(
            120,
            |_| {
                let mut input = GovernorInput::test_pressure(Level::Normal);
                if what == "cpu" {
                    input.readings.load = Reading::Value([500.0, 500.0, 500.0]);
                } else {
                    input.readings.free_disk = Reading::Value(MIB);
                }
                input
            },
            &table,
        );
        assert!(
            rig.fake.calls().is_empty(),
            "{what}: {:?}",
            rig.fake.calls()
        );
        assert_eq!(rig.relief.status.rung, Rung::None, "{what}");
    }
}

#[test]
fn a_frozen_tree_resumes_when_deterioration_is_flat_though_swap_stays_high() {
    let table = standard();
    let mut rig = Rig::new(SupervisorMode::Act);
    live_all(&rig, &table);
    rig.ticks(SEVERE_SAMPLES as usize, rising(100), &table);
    assert_eq!(rig.fake.stops(), [102, 103]);
    assert!(rig.fake.conts().is_empty());
    // Swap parks at 5 GiB, Severe throughout.
    for n in 1..FLAT_SAMPLES {
        rig.tick(&input(Level::Critical, 5000), &table);
        assert!(rig.fake.conts().is_empty(), "sample {n} after the freeze");
    }
    rig.tick(&input(Level::Critical, 5000), &table);
    assert_eq!(
        rig.fake.conts(),
        [103, 102],
        "children first, then the root"
    );
    assert!(rig.relief.status.frozen.is_empty());
    assert_eq!(rig.relief.status.cap_hits, 0);
    assert!(
        read_journal(rig.dir.path())
            .unwrap()
            .is_none_or(|j| j.entries.iter().all(|e| e.rung != JournalRung::Freeze)),
        "a resumed freeze leaves no journal entry"
    );
    // Still Severe and still flat: nothing is frozen again.
    rig.ticks(120, flat, &table);
    assert_eq!(rig.fake.stops(), [102, 103]);
}

#[test]
fn the_ten_minute_cap_resumes_and_is_counted() {
    let table = standard();
    let mut rig = Rig::new(SupervisorMode::Act);
    live_all(&rig, &table);
    rig.ticks(SEVERE_SAMPLES as usize, rising(100), &table);
    assert_eq!(rig.fake.stops(), [102, 103]);
    // Swap keeps climbing, so only the cap can end it.
    for n in 0..400usize {
        rig.tick(&input(Level::Critical, 9000 + 100 * n as u64), &table);
        if !rig.fake.conts().is_empty() {
            break;
        }
    }
    assert_eq!(rig.fake.conts(), [103, 102]);
    assert_eq!(rig.relief.status.cap_hits, 1);
    assert!(rig.relief.status.frozen.is_empty());
    let note = &rig.relief.status.last_action.as_ref().unwrap().detail;
    assert!(note.contains("10-minute"), "{note}");
}

#[test]
fn a_freeze_ends_when_memory_pressure_eases() {
    let table = standard();
    let mut rig = Rig::new(SupervisorMode::Act);
    live_all(&rig, &table);
    rig.ticks(SEVERE_SAMPLES as usize, rising(100), &table);
    rig.ticks(
        SEVERE_BREAK_SAMPLES as usize,
        |_| input(Level::Normal, 0),
        &table,
    );
    assert_eq!(rig.fake.conts(), [103, 102]);
    // The same five quiet samples that end the freeze end the slowing.
    assert_eq!(rig.relief.status.rung, Rung::None);
    assert_eq!(rig.relief.status.slowed, 0);
}

#[test]
fn moving_from_act_to_advise_puts_the_background_priorities_back_too() {
    let table = standard();
    let mut rig = Rig::new(SupervisorMode::Act);
    live_all(&rig, &table);
    rig.ticks(5, |_| input(Level::Warn, 0), &table);
    assert_eq!(rig.relief.status.slowed, 5);
    *rig.mode.lock().unwrap() = SupervisorMode::Advise;
    rig.tick(&input(Level::Warn, 0), &table);
    let mut restored = rig.fake.restored();
    restored.sort_unstable();
    assert_eq!(restored, [101, 102, 103, 105, 106]);
    assert_eq!(rig.relief.status.slowed, 0);
    assert_eq!(rig.relief.status.mode, SupervisorMode::Advise);
    assert!(read_journal(rig.dir.path()).unwrap().is_none());
    // And advise sends nothing more.
    let calls = rig.fake.calls().len();
    rig.ticks(30, |_| input(Level::Warn, 0), &table);
    assert_eq!(rig.fake.calls().len(), calls);
}

#[test]
fn rung_a_passes_run_at_once_and_then_every_fifth_sample() {
    let table = standard();
    let mut rig = Rig::new(SupervisorMode::Act);
    live_all(&rig, &table);
    rig.ticks(ENTER_SAMPLES as usize, |_| input(Level::Warn, 0), &table);
    assert_eq!(
        rig.fake.backgrounded().len(),
        5,
        "the first pass is immediate"
    );
    let mut grown = table.clone();
    grown.sessions[0].members.push(proc(120, 102, "cc1", 50));
    live_all(&rig, &grown);
    // A child that is not in the background yet waits for the next pass...
    rig.ticks(
        SLOW_PASS_EVERY as usize - 1,
        |_| input(Level::Warn, 0),
        &grown,
    );
    assert!(!rig.fake.backgrounded().contains(&120));
    // ...which is the fifth sample after the last.
    rig.tick(&input(Level::Warn, 0), &grown);
    assert!(rig.fake.backgrounded().contains(&120));
}

#[test]
fn what_has_ended_leaves_the_ledger_the_cap_and_the_journal() {
    let table = standard();
    let mut rig = Rig::new(SupervisorMode::Act);
    live_all(&rig, &table);
    rig.ticks(ENTER_SAMPLES as usize, |_| input(Level::Warn, 0), &table);
    assert_eq!(rig.relief.slowed.len(), 5);
    // The build ended: cargo and rustc are gone from the table.
    let mut after = table.clone();
    after.sessions[0]
        .members
        .retain(|m| m.pid != 102 && m.pid != 103);
    rig.ticks(SLOW_PASS_EVERY as usize, |_| input(Level::Warn, 0), &after);
    assert_eq!(rig.relief.slowed.len(), 3, "{:?}", rig.relief.slowed);
    let journal = read_journal(rig.dir.path()).unwrap().unwrap();
    let mut listed: Vec<i32> = journal.entries.iter().map(|e| e.pid).collect();
    listed.sort_unstable();
    assert_eq!(listed, [101, 105, 106]);
    assert!(lock(&LEDGER).get(&102).is_none_or(|(s, _)| *s != 1_000_102));
    // A pid whose start time differs is a new process, not the old one.
    let mut reused = after.clone();
    reused.sessions[0].members.push(ReliefProc {
        start_us: 5,
        ..proc(105, 100, "npx", 10)
    });
    reused.sessions[0]
        .members
        .retain(|m| !(m.pid == 105 && m.start_us != 5));
    rig.fake.live.lock().unwrap().insert(105, 5);
    // (The fake keys priorities by pid: the old process is gone, so is its mark.)
    rig.fake.background.lock().unwrap().remove(&105);
    rig.ticks(SLOW_PASS_EVERY as usize, |_| input(Level::Warn, 0), &reused);
    assert_eq!(
        rig.relief.slowed.get(&105),
        Some(&5),
        "the new process, not the old one"
    );
}

#[test]
fn a_refused_process_is_not_asked_again_every_pass() {
    let table = standard();
    let mut rig = Rig::new(SupervisorMode::Act);
    live_all(&rig, &table);
    // The cargo's pid is another process's now: every ask is refused.
    rig.fake.live.lock().unwrap().insert(102, 5);
    rig.ticks(
        ENTER_SAMPLES as usize + 3 * SLOW_PASS_EVERY as usize,
        |_| input(Level::Warn, 0),
        &table,
    );
    let asks = rig
        .fake
        .backgrounded()
        .iter()
        .filter(|p| **p == 102)
        .count();
    assert_eq!(asks, 1, "asked once, then left until the slowing ends");
}

#[test]
fn a_journal_another_running_gitpulse_wrote_is_left_alone() {
    let dir = tempfile::tempdir().unwrap();
    let parent = std::os::unix::process::parent_id();
    let live_start = crate::sysload::process_start_us(parent).expect("the parent is running");
    let entries = vec![JournalEntry {
        pid: 301,
        start_us: 77,
        session_id: "300-1".into(),
        at_unix_ms: 1,
        rung: JournalRung::Freeze,
    }];
    let write = |writer_pid: u32, writer_start_us: u64| {
        let journal = Journal {
            version: 1,
            writer_pid,
            writer_start_us,
            entries: entries.clone(),
        };
        std::fs::create_dir_all(dir.path().join(JOURNAL_DIR)).unwrap();
        std::fs::write(
            journal_path(dir.path()),
            serde_json::to_vec(&journal).unwrap(),
        )
        .unwrap();
    };
    let fake = Fake::default();
    // Written by a process that is still running, and is not this one: its.
    write(parent, live_start);
    let replay = replay_journal(dir.path(), &fake);
    assert_eq!(replay.left_to, Some(parent));
    assert!(fake.calls().is_empty());
    assert!(journal_path(dir.path()).exists(), "and its journal stays");
    // The same pid with another start time is a recycled one: the writer is
    // gone, so the journal is ours to replay.
    write(parent, live_start + 1);
    let replay = replay_journal(dir.path(), &fake);
    assert_eq!(replay.left_to, None);
    assert_eq!(fake.conts(), [301]);
    assert!(!journal_path(dir.path()).exists());
}

#[test]
fn resume_all_continues_every_frozen_process_through_the_journal_and_holds_off_the_episode() {
    let table = standard();
    let mut rig = Rig::new(SupervisorMode::Act);
    live_all(&rig, &table);
    rig.ticks(SEVERE_SAMPLES as usize, rising(100), &table);
    assert_eq!(rig.fake.stops(), [102, 103]);
    let outcome = resume_all_in(rig.dir.path(), &rig.fake, true, "the person asked");
    assert_eq!(outcome.refused, None);
    assert!(outcome.held_off);
    assert_eq!(
        outcome
            .results
            .iter()
            .map(|r| (r.pid, r.ok, r.detail.clone()))
            .collect::<Vec<_>>(),
        [(103, true, None), (102, true, None)],
        "last stopped first, each by pid"
    );
    assert_eq!(rig.fake.conts(), [103, 102]);
    assert!(
        read_journal(rig.dir.path())
            .unwrap()
            .is_none_or(|j| j.entries.iter().all(|e| e.rung != JournalRung::Freeze)),
        "cleared after the attempt"
    );
    // The relief takes account on its next tick, with nothing signalled twice.
    rig.tick(&input(Level::Critical, 9000), &table);
    assert!(rig.relief.status.frozen.is_empty());
    assert_eq!(rig.fake.conts(), [103, 102]);
    let note = rig.relief.status.last_action.clone().unwrap();
    assert_eq!(note.kind, ActionKind::Resume);
    assert!(note.detail.contains("the person asked"), "{note:?}");
    // Severe and still worsening for a long time: held off, nothing refreezes.
    rig.ticks(
        120,
        |i| input(Level::Critical, 9000 + 100 * i as u64),
        &table,
    );
    assert_eq!(rig.fake.stops(), [102, 103]);
    // Once the tier has dropped below Severe, a new episode may freeze again.
    rig.ticks(
        SEVERE_BREAK_SAMPLES as usize,
        |_| input(Level::Normal, 0),
        &table,
    );
    rig.ticks(SEVERE_SAMPLES as usize, rising(100), &table);
    assert_eq!(rig.fake.stops(), [102, 103, 102, 103]);
}

#[test]
fn resume_all_without_holding_off_lets_the_next_severe_minute_freeze_again() {
    let table = standard();
    let mut rig = Rig::new(SupervisorMode::Act);
    live_all(&rig, &table);
    rig.ticks(SEVERE_SAMPLES as usize, rising(100), &table);
    let outcome = resume_all_in(rig.dir.path(), &rig.fake, false, "test");
    assert!(!outcome.held_off);
    rig.ticks(
        SEVERE_SAMPLES as usize + 1,
        |i| input(Level::Critical, 9000 + 100 * i as u64),
        &table,
    );
    assert_eq!(rig.fake.stops().len(), 4);
}

#[test]
fn resume_all_reports_each_pid_and_records_a_refused_one_as_failed() {
    let table = standard();
    let mut rig = Rig::new(SupervisorMode::Act);
    live_all(&rig, &table);
    rig.ticks(SEVERE_SAMPLES as usize, rising(100), &table);
    // rustc's pid was recycled since it was stopped.
    rig.fake.live.lock().unwrap().insert(103, 5);
    let outcome = resume_all_in(rig.dir.path(), &rig.fake, false, "test");
    let by_pid: Vec<(i32, bool)> = outcome.results.iter().map(|r| (r.pid, r.ok)).collect();
    assert_eq!(by_pid, [(103, false), (102, true)]);
    assert!(outcome.results[0]
        .detail
        .as_deref()
        .is_some_and(|d| d.contains("different process")));
    assert_eq!(
        rig.fake.conts(),
        [103, 102],
        "the refused one was attempted, not skipped"
    );
}

#[test]
fn resume_all_leaves_another_running_gitpulses_processes_alone_and_without_a_relief_does_nothing() {
    let dir = tempfile::tempdir().unwrap();
    let parent = std::os::unix::process::parent_id();
    let journal = Journal {
        version: 1,
        writer_pid: parent,
        writer_start_us: crate::sysload::process_start_us(parent).unwrap(),
        entries: vec![JournalEntry {
            pid: 301,
            start_us: 77,
            session_id: "300-1".into(),
            at_unix_ms: 1,
            rung: JournalRung::Freeze,
        }],
    };
    std::fs::create_dir_all(dir.path().join(JOURNAL_DIR)).unwrap();
    std::fs::write(
        journal_path(dir.path()),
        serde_json::to_vec(&journal).unwrap(),
    )
    .unwrap();
    let fake = Fake::default();
    let outcome = resume_all_in(dir.path(), &fake, true, "test");
    assert!(outcome
        .refused
        .as_deref()
        .is_some_and(|r| r.contains(&parent.to_string())));
    assert!(fake.calls().is_empty());
    assert!(journal_path(dir.path()).exists());
    // No relief in this test process: the public entry point has nothing to do.
    let none = resume_all(true, "test");
    assert!(none.results.is_empty());
    assert!(none.refused.is_some());
}

#[test]
fn the_snapshot_says_what_is_frozen_and_what_was_last_done() {
    let table = standard();
    let mut rig = Rig::new(SupervisorMode::Act);
    live_all(&rig, &table);
    rig.ticks(SEVERE_SAMPLES as usize, rising(100), &table);
    let snap = &rig.relief.status;
    assert_eq!(snap.mode, SupervisorMode::Act);
    assert_eq!(snap.rung, Rung::Freeze);
    assert_eq!(snap.binding, Some(Binding::MemoryPressure));
    assert_eq!(snap.slowed, 5);
    let f = &snap.frozen[0];
    assert_eq!((f.root_pid, f.start_us), (102, 1_000_102));
    assert_eq!(f.session_pid, 100);
    assert_eq!(f.terminal_session_id.as_deref(), Some("term-100"));
    assert!(f.repo_path.as_deref().is_some_and(|p| !p.is_empty()));
    assert_eq!(f.command, "cargo");
    assert_eq!(f.footprint_bytes, 1100 * MIB);
    assert!(f.since_unix_ms > 0);
    assert!(!f.advised_only);
    let last = snap.last_action.as_ref().unwrap();
    assert_eq!(last.kind, ActionKind::Freeze);
    // A refused signal is a failure with its pid, and a cap resume is its own kind.
    rig.fake.live.lock().unwrap().insert(103, 5);
    for n in 0..400usize {
        rig.tick(&input(Level::Critical, 9000 + 100 * n as u64), &table);
        if rig.relief.status.frozen.is_empty() {
            break;
        }
    }
    let snap = &rig.relief.status;
    assert_eq!(snap.cap_hits, 1);
    assert_eq!(
        snap.last_action.as_ref().unwrap().kind,
        ActionKind::CapResume
    );
    assert!(snap
        .failures
        .iter()
        .any(|f| f.pid == Some(103) && f.what.contains("different process")));
    assert!(snap.failure_count >= 1);
}

#[test]
fn nothing_to_freeze_is_a_refused_action() {
    let mut rig = Rig::new(SupervisorMode::Act);
    let table = standard();
    live_all(&rig, &table);
    *rig.probe.leases.lock().unwrap() = Ok(BTreeSet::from([102]));
    rig.ticks(SEVERE_SAMPLES as usize, rising(100), &table);
    assert_eq!(
        rig.relief.status.last_action.as_ref().unwrap().kind,
        ActionKind::Refused
    );
    assert_eq!(rig.relief.status.rung, Rung::Slow);
}

#[test]
fn slowing_ends_with_a_restore_after_five_quiet_samples() {
    let table = standard();
    let mut rig = Rig::new(SupervisorMode::Act);
    live_all(&rig, &table);
    rig.ticks(5, |_| input(Level::Warn, 0), &table);
    assert_eq!(
        rig.relief.status.slowed, 5,
        "every member but the agent itself"
    );
    rig.ticks(EXIT_SAMPLES as usize, |_| input(Level::Normal, 0), &table);
    let mut restored = rig.fake.restored();
    restored.sort_unstable();
    assert_eq!(restored, [101, 102, 103, 105, 106]);
    assert_eq!(rig.relief.status.slowed, 0);
    assert_eq!(rig.relief.status.rung, Rung::None);
    assert!(read_journal(rig.dir.path()).unwrap().is_none());
}

#[test]
fn a_journal_that_cannot_be_written_means_no_signal() {
    let table = standard();
    let mut rig = Rig::new(SupervisorMode::Act);
    live_all(&rig, &table);
    // Rung A first, so the journal is attempted on a clean slate; then break
    // it: the journal's directory becomes a file.
    std::fs::write(rig.dir.path().join(JOURNAL_DIR), b"in the way").unwrap();
    rig.ticks(60, rising(100), &table);
    assert!(rig.fake.stops().is_empty(), "no SIGSTOP without a journal");
    assert!(rig.fake.backgrounded().is_empty(), "nor a priority change");
    assert!(rig.relief.status.failure_count > 0);
    assert!(
        rig.relief
            .status
            .failures
            .iter()
            .any(|f| f.what.contains("journal")),
        "{:?}",
        rig.relief.status.failures
    );
}

#[test]
fn an_unreadable_lease_board_freezes_nothing_and_says_so() {
    let table = standard();
    let mut rig = Rig::new(SupervisorMode::Act);
    live_all(&rig, &table);
    *rig.probe.leases.lock().unwrap() = Err("slot-2 unreadable".into());
    rig.ticks(SEVERE_SAMPLES as usize, rising(100), &table);
    assert!(rig.fake.stops().is_empty());
    assert!(rig
        .relief
        .status
        .failures
        .iter()
        .any(|f| f.what.contains("slot-2 unreadable")));
}

#[test]
fn an_untrusted_repository_is_slowed_but_never_frozen() {
    let table = standard();
    let mut rig = Rig::new(SupervisorMode::Act);
    live_all(&rig, &table);
    *rig.trusted.lock().unwrap() = false;
    rig.ticks(60, rising(100), &table);
    assert!(rig.fake.stops().is_empty());
    assert!(!rig.fake.backgrounded().is_empty());
}

#[test]
fn at_most_three_subtrees_are_frozen_on_the_host() {
    let sessions: Vec<ReliefSession> = (0..5)
        .map(|n| {
            let base = 200 + 10 * n;
            session(
                base,
                true,
                vec![
                    proc(base + 1, base, "zsh", 10),
                    proc(base + 2, base + 1, "cargo", 500 + 100 * n as u64),
                ],
            )
        })
        .collect();
    let table = ReliefTable { sessions };
    let mut rig = Rig::new(SupervisorMode::Act);
    live_all(&rig, &table);
    rig.ticks(SEVERE_SAMPLES as usize, rising(100), &table);
    let mut stops = rig.fake.stops();
    stops.sort_unstable();
    assert_eq!(
        stops,
        [222, 232, 242],
        "the heaviest three, one per session"
    );
    assert_eq!(rig.relief.status.frozen.len(), MAX_FROZEN_PER_HOST);
}

#[test]
fn a_process_that_appears_under_a_frozen_subtree_is_stopped_too() {
    let table = standard();
    let mut rig = Rig::new(SupervisorMode::Act);
    live_all(&rig, &table);
    rig.ticks(SEVERE_SAMPLES as usize, rising(100), &table);
    let mut grown = table.clone();
    grown.sessions[0].members.push(proc(120, 103, "cc1", 50));
    live_all(&rig, &grown);
    rig.tick(&input(Level::Critical, 9000), &grown);
    assert_eq!(rig.fake.stops(), [102, 103, 120]);
    assert_eq!(rig.relief.status.frozen[0].members, 3);
    // When the subtree's own root ends, the rest is let go at once.
    let mut ended = grown.clone();
    ended.sessions[0].members.retain(|m| m.pid != 102);
    rig.tick(&input(Level::Critical, 9100), &ended);
    assert_eq!(rig.fake.conts(), [120, 103, 102]);
    assert!(rig.relief.status.frozen.is_empty());
}

#[test]
fn advise_computes_and_publishes_but_sends_nothing() {
    let table = standard();
    let mut rig = Rig::new(SupervisorMode::Advise);
    live_all(&rig, &table);
    rig.ticks(SEVERE_SAMPLES as usize + 5, rising(100), &table);
    assert!(rig.fake.calls().is_empty(), "{:?}", rig.fake.calls());
    let status = &rig.relief.status;
    assert_eq!(status.mode, SupervisorMode::Advise);
    assert_eq!(status.would_slow, 5);
    assert_eq!(status.rung, Rung::Freeze);
    assert_eq!(status.frozen.len(), 1);
    assert!(status.frozen[0].advised_only, "{:?}", status.frozen);
    assert_eq!(status.frozen[0].command, "cargo");
    assert!(read_journal(rig.dir.path()).unwrap().is_none());
    // The decision is what the snapshot shows the UI.
    assert_eq!(
        status.last_action.as_ref().unwrap().kind,
        ActionKind::Freeze
    );
    assert!(status
        .last_action
        .as_ref()
        .unwrap()
        .detail
        .contains("would freeze"));
}

#[test]
fn leaving_act_or_switching_off_puts_everything_back() {
    let table = standard();
    let mut rig = Rig::new(SupervisorMode::Act);
    live_all(&rig, &table);
    rig.ticks(SEVERE_SAMPLES as usize, rising(100), &table);
    assert_eq!(rig.relief.status.frozen.len(), 1);
    *rig.mode.lock().unwrap() = SupervisorMode::Off;
    rig.tick(&input(Level::Critical, 9000), &table);
    assert_eq!(rig.fake.conts(), [103, 102]);
    let mut restored = rig.fake.restored();
    restored.sort_unstable();
    assert_eq!(restored, [101, 102, 103, 105, 106]);
    assert_eq!(rig.relief.status.rung, Rung::None);
    assert_eq!(rig.relief.status.mode, SupervisorMode::Off);
    // Off stays off however bad it gets.
    let calls = rig.fake.calls().len();
    rig.ticks(100, rising(100), &table);
    assert_eq!(rig.fake.calls().len(), calls);
}

#[test]
fn switching_host_load_off_stands_the_relief_down_too() {
    let table = standard();
    let mut rig = Rig::new(SupervisorMode::Act);
    live_all(&rig, &table);
    rig.ticks(SEVERE_SAMPLES as usize, rising(100), &table);
    let mut off = input(Level::Critical, 9000);
    off.enabled = false;
    rig.tick(&off, &table);
    assert_eq!(rig.fake.conts(), [103, 102]);
}

#[test]
fn a_recycled_pid_is_refused_and_recorded_as_failed() {
    let table = standard();
    let mut rig = Rig::new(SupervisorMode::Act);
    live_all(&rig, &table);
    // The cargo's pid now belongs to a newer process.
    rig.fake.live.lock().unwrap().insert(102, 5);
    rig.ticks(SEVERE_SAMPLES as usize, rising(100), &table);
    assert!(
        rig.relief.status.frozen.is_empty(),
        "nothing is recorded as frozen"
    );
    assert!(
        rig.relief
            .status
            .failures
            .iter()
            .any(|f| f.what.contains("different process")),
        "{:?}",
        rig.relief.status.failures
    );
    assert!(
        read_journal(rig.dir.path())
            .unwrap()
            .is_none_or(|j| j.entries.iter().all(|e| e.rung != JournalRung::Freeze)),
        "a refused freeze is not left in the journal"
    );
}

#[test]
fn a_journaled_freeze_is_resumed_by_a_new_relief_and_a_recycled_entry_is_a_failure() {
    let dir = tempfile::tempdir().unwrap();
    let entries = vec![
        JournalEntry {
            pid: 301,
            start_us: 77,
            session_id: "300-1".into(),
            at_unix_ms: 1,
            rung: JournalRung::Freeze,
        },
        JournalEntry {
            pid: 302,
            start_us: 88,
            session_id: "300-1".into(),
            at_unix_ms: 1,
            rung: JournalRung::Freeze,
        },
        JournalEntry {
            pid: 303,
            start_us: 99,
            session_id: "300-1".into(),
            at_unix_ms: 1,
            rung: JournalRung::Slow,
        },
    ];
    write_journal(dir.path(), &entries).unwrap();
    let fake = Fake::default();
    {
        let mut live = fake.live.lock().unwrap();
        live.insert(301, 77);
        live.insert(302, 12345); // recycled: another process holds 302 now
        live.insert(303, 99);
    }
    let worktree = clean_worktree();
    let mut relief = Relief::new(
        dir.path().to_path_buf(),
        Box::new(fake.clone()),
        Box::new(FakeProbe::new(worktree.path())),
        Box::new(|| SupervisorMode::Act),
        Box::new(|_| true),
    );
    relief.recover();
    assert_eq!(
        fake.conts(),
        [302, 301],
        "every journaled freeze was attempted"
    );
    assert_eq!(fake.restored(), [303]);
    assert!(
        read_journal(dir.path()).unwrap().is_none(),
        "cleared after the attempt"
    );
    assert!(
        relief
            .status
            .failures
            .iter()
            .any(|f| f.what.contains("302") && f.what.contains("different process")),
        "{:?}",
        relief.status.failures
    );
    assert!(!relief
        .status
        .failures
        .iter()
        .any(|f| f.what.contains("301")));
    // Recovery runs once.
    relief.recover();
    assert_eq!(fake.conts().len(), 2);
}

#[test]
fn an_unreadable_journal_is_cleared_and_reported() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::create_dir(dir.path().join(JOURNAL_DIR)).unwrap();
    std::fs::write(journal_path(dir.path()), b"{ not json").unwrap();
    let replay = replay_journal(dir.path(), &Fake::default());
    assert_eq!(replay.failures.len(), 1);
    assert!(
        replay.failures[0].contains("could not be read"),
        "{replay:?}"
    );
    assert!(!journal_path(dir.path()).exists());
}

#[test]
fn dropping_the_relief_resumes_what_it_stopped() {
    let table = standard();
    let fake;
    {
        let mut rig = Rig::new(SupervisorMode::Act);
        live_all(&rig, &table);
        rig.ticks(SEVERE_SAMPLES as usize, rising(100), &table);
        assert_eq!(rig.fake.stops(), [102, 103]);
        fake = rig.fake.clone();
    }
    let conts = fake.conts();
    assert!(conts.contains(&102) && conts.contains(&103), "{conts:?}");
    assert!(fake.restored().contains(&102));
}

// ---------------------------------------------------------------------------
// One owner of slowing.
// ---------------------------------------------------------------------------

#[test]
fn the_steward_cannot_put_back_what_the_relief_slowed() {
    let table = ReliefTable {
        sessions: vec![session(
            9_000_100,
            true,
            vec![
                proc(9_000_101, 9_000_100, "zsh", 10),
                proc(9_000_102, 9_000_101, "cargo", 700),
            ],
        )],
    };
    let mut rig = Rig::new(SupervisorMode::Act);
    live_all(&rig, &table);
    rig.ticks(5, |_| input(Level::Warn, 0), &table);
    assert!(rig.fake.backgrounded().contains(&9_000_102));
    let start = 1_000_000 + 9_000_102u64;
    // The steward's "Manvi is not configured: restore" path.
    let before = rig.fake.calls().len();
    assert_eq!(
        steward_throttle_with(&rig.fake, 9_000_102, start, false),
        Ok(false),
        "refused as not its own"
    );
    assert_eq!(
        rig.fake.calls().len(),
        before,
        "and no priority was touched"
    );
    assert!(rig.fake.background.lock().unwrap().contains(&9_000_102));
    // The relief puts it back itself, when pressure eases.
    rig.ticks(EXIT_SAMPLES as usize, |_| input(Level::Normal, 0), &table);
    assert!(rig.fake.restored().contains(&9_000_102));
    assert!(!rig.fake.background.lock().unwrap().contains(&9_000_102));
}

#[test]
fn what_the_steward_slowed_is_left_to_the_steward() {
    let table = ReliefTable {
        sessions: vec![session(
            9_100_100,
            true,
            vec![
                proc(9_100_101, 9_100_100, "zsh", 10),
                proc(9_100_102, 9_100_101, "cargo", 700),
            ],
        )],
    };
    let mut rig = Rig::new(SupervisorMode::Act);
    live_all(&rig, &table);
    let start = 1_000_000 + 9_100_102u64;
    assert_eq!(
        steward_throttle_with(&rig.fake, 9_100_102, start, true),
        Ok(true)
    );
    rig.ticks(5, |_| input(Level::Warn, 0), &table);
    assert_eq!(
        rig.fake
            .calls()
            .iter()
            .filter(|c| matches!(c, Call::Bg(9_100_102, _)))
            .count(),
        1,
        "only the steward's own call: {:?}",
        rig.fake.calls()
    );
    // The steward's own restore still works.
    assert_eq!(
        steward_throttle_with(&rig.fake, 9_100_102, start, false),
        Ok(true)
    );
    assert!(!rig.fake.background.lock().unwrap().contains(&9_100_102));
}

#[test]
fn a_child_that_inherited_the_background_is_recorded_as_the_reliefs_and_restored() {
    let table = ReliefTable {
        sessions: vec![session(
            9_200_100,
            true,
            vec![
                proc(9_200_101, 9_200_100, "zsh", 10),
                proc(9_200_102, 9_200_101, "cargo", 700),
                proc(9_200_103, 9_200_102, "rustc", 400),
            ],
        )],
    };
    let mut rig = Rig::new(SupervisorMode::Act);
    live_all(&rig, &table);
    // The kernel put rustc in the background when its parent went there.
    rig.fake.background.lock().unwrap().insert(9_200_103);
    rig.ticks(5, |_| input(Level::Warn, 0), &table);
    rig.ticks(EXIT_SAMPLES as usize, |_| input(Level::Normal, 0), &table);
    assert!(rig.fake.restored().contains(&9_200_103));
    // One that was in the background before, with a parent that is not ours,
    // is somebody else's and stays.
    let table = ReliefTable {
        sessions: vec![session(
            9_300_100,
            true,
            vec![
                proc(9_300_101, 9_300_100, "bash", 10),
                proc(9_300_102, 9_300_101, "make", 700),
            ],
        )],
    };
    let mut rig = Rig::new(SupervisorMode::Act);
    live_all(&rig, &table);
    rig.fake.background.lock().unwrap().insert(9_300_102);
    rig.fake.background.lock().unwrap().insert(9_300_101);
    rig.ticks(5, |_| input(Level::Warn, 0), &table);
    rig.ticks(EXIT_SAMPLES as usize, |_| input(Level::Normal, 0), &table);
    assert!(rig.fake.restored().is_empty(), "{:?}", rig.fake.calls());
}

// ---------------------------------------------------------------------------
// Real processes: real signals to a `sleep` this test started.
// ---------------------------------------------------------------------------

#[cfg(unix)]
mod real_signals {
    use super::*;
    use crate::procguard::LockedSpawn;

    /// A child that is always continued, killed and reaped, however the test ends.
    struct Sleeper(std::process::Child);

    impl Sleeper {
        fn spawn() -> Self {
            Self(
                std::process::Command::new("sleep")
                    .arg("120")
                    .spawn_locked()
                    .expect("sleep"),
            )
        }

        fn pid(&self) -> i32 {
            i32::try_from(self.0.id()).unwrap()
        }

        fn start_us(&self) -> u64 {
            crate::sysload::process_start_us(self.0.id()).expect("a start time")
        }

        fn state(&self) -> String {
            let out = std::process::Command::new("ps")
                .args(["-o", "state=", "-p", &self.pid().to_string()])
                .output_locked()
                .expect("ps");
            String::from_utf8_lossy(&out.stdout).trim().to_string()
        }

        fn wait_for(&self, stopped: bool) -> bool {
            let until = Instant::now() + Duration::from_secs(10);
            while Instant::now() < until {
                if self.state().starts_with('T') == stopped && !self.state().is_empty() {
                    return true;
                }
                std::thread::sleep(Duration::from_millis(20));
            }
            false
        }

        fn stop_by_hand(&self) {
            // SAFETY: signals the child this test started.
            unsafe { libc::kill(self.pid(), libc::SIGSTOP) };
            assert!(self.wait_for(true), "state {:?}", self.state());
        }
    }

    impl Drop for Sleeper {
        fn drop(&mut self) {
            // SAFETY: signals the child this test started, before reaping it.
            unsafe { libc::kill(self.0.id() as i32, libc::SIGCONT) };
            let _ = self.0.kill();
            let _ = self.0.wait();
        }
    }

    #[test]
    fn a_journaled_freeze_is_resumed_by_a_fresh_process_through_real_signals() {
        let child = Sleeper::spawn();
        child.stop_by_hand();
        let dir = tempfile::tempdir().unwrap();
        write_journal(
            dir.path(),
            &[JournalEntry {
                pid: child.pid(),
                start_us: child.start_us(),
                session_id: "1-1".into(),
                at_unix_ms: 1,
                rung: JournalRung::Freeze,
            }],
        )
        .unwrap();
        // "A fresh process": nothing of the freezing relief survives but the file.
        let replay = replay_journal(dir.path(), &OsActuator);
        assert_eq!(replay.resumed, 1, "{replay:?}");
        assert!(replay.failures.is_empty(), "{replay:?}");
        assert!(child.wait_for(false), "still {:?}", child.state());
        assert!(!journal_path(dir.path()).exists());
    }

    #[test]
    fn a_recycled_pid_in_the_journal_is_refused_through_the_real_start_time() {
        let child = Sleeper::spawn();
        child.stop_by_hand();
        let dir = tempfile::tempdir().unwrap();
        write_journal(
            dir.path(),
            &[JournalEntry {
                pid: child.pid(),
                start_us: child.start_us() + 1,
                session_id: "1-1".into(),
                at_unix_ms: 1,
                rung: JournalRung::Freeze,
            }],
        )
        .unwrap();
        let replay = replay_journal(dir.path(), &OsActuator);
        assert_eq!(replay.resumed, 0);
        assert_eq!(replay.failures.len(), 1, "{replay:?}");
        assert!(
            replay.failures[0].contains("different process"),
            "{replay:?}"
        );
        assert!(
            child.state().starts_with('T'),
            "the stranger was not signalled: {:?}",
            child.state()
        );
        assert!(
            !journal_path(dir.path()).exists(),
            "cleared after the attempt"
        );
    }

    #[test]
    fn the_os_actuator_stops_continues_and_reports_a_process_that_ended() {
        let child = Sleeper::spawn();
        let (pid, start) = (child.pid(), child.start_us());
        assert_eq!(
            OsActuator.signal(pid, start, Signal::Stop),
            Ok(Delivery::Delivered)
        );
        assert!(child.wait_for(true));
        assert_eq!(
            OsActuator.signal(pid, start, Signal::Cont),
            Ok(Delivery::Delivered)
        );
        assert!(child.wait_for(false));
        let err = OsActuator.signal(pid, start + 1, Signal::Stop).unwrap_err();
        assert!(err.contains("different process"), "{err}");
        assert!(!child.state().starts_with('T'));
        // Gone: reaped, so nothing holds the pid.
        drop(child);
        assert_eq!(
            OsActuator.signal(pid, start, Signal::Cont),
            Ok(Delivery::Gone)
        );
    }

    #[test]
    fn a_real_subtree_is_stopped_on_a_sustained_deteriorating_severe_and_resumed_flat() {
        let child = Sleeper::spawn();
        let (pid, start) = (child.pid(), child.start_us());
        let table = ReliefTable {
            sessions: vec![ReliefSession {
                members: vec![
                    proc(8_000_101, 8_000_100, "zsh", 10),
                    ReliefProc {
                        pid,
                        ppid: 8_000_101,
                        start_us: start,
                        name: "sleep".into(),
                        footprint: Some(900 * MIB),
                        exempt: false,
                    },
                ],
                ..session(8_000_100, true, Vec::new())
            }],
        };
        let mut rig = Rig::new(SupervisorMode::Act);
        // The real actuator, with the fake host around it.
        rig.relief.actuator = Box::new(OsActuator);
        rig.ticks(SEVERE_SAMPLES as usize, rising(100), &table);
        assert!(child.wait_for(true), "state {:?}", child.state());
        assert_eq!(rig.relief.status.rung, Rung::Freeze);
        rig.ticks(FLAT_SAMPLES, |_| input(Level::Critical, 5000), &table);
        assert!(child.wait_for(false), "state {:?}", child.state());
        assert!(rig.relief.status.frozen.is_empty());
    }

    #[test]
    fn a_stale_start_time_in_the_table_leaves_a_real_process_running() {
        let child = Sleeper::spawn();
        let table = ReliefTable {
            sessions: vec![ReliefSession {
                members: vec![
                    proc(8_100_101, 8_100_100, "zsh", 10),
                    ReliefProc {
                        pid: child.pid(),
                        ppid: 8_100_101,
                        start_us: child.start_us() + 7,
                        name: "sleep".into(),
                        footprint: Some(900 * MIB),
                        exempt: false,
                    },
                ],
                ..session(8_100_100, true, Vec::new())
            }],
        };
        let mut rig = Rig::new(SupervisorMode::Act);
        rig.relief.actuator = Box::new(OsActuator);
        rig.ticks(SEVERE_SAMPLES as usize, rising(100), &table);
        std::thread::sleep(Duration::from_millis(200));
        assert!(!child.state().starts_with('T'), "{:?}", child.state());
        assert!(rig
            .relief
            .status
            .failures
            .iter()
            .any(|f| f.what.contains("different process")));
        assert!(rig.relief.status.frozen.is_empty());
    }
}

// ---------------------------------------------------------------------------
// The journal, the status and the setting.
// ---------------------------------------------------------------------------

#[test]
fn the_journal_round_trips_and_an_empty_one_removes_the_file() {
    let dir = tempfile::tempdir().unwrap();
    let entries = vec![JournalEntry {
        pid: 5,
        start_us: 6,
        session_id: "7-8".into(),
        at_unix_ms: 9,
        rung: JournalRung::Freeze,
    }];
    write_journal(dir.path(), &entries).unwrap();
    let read = read_journal(dir.path()).unwrap().unwrap();
    assert_eq!(read.entries, entries);
    assert_eq!(read.writer_pid, std::process::id());
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = std::fs::metadata(journal_path(dir.path()))
            .unwrap()
            .permissions()
            .mode();
        assert_eq!(mode & 0o077, 0, "private: {mode:o}");
    }
    write_journal(dir.path(), &[]).unwrap();
    assert!(read_journal(dir.path()).unwrap().is_none());
}
