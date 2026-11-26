use super::*;
use crate::sysload::{CompressorSegments, Governor, Level, Reading};
use std::sync::{Arc, Mutex};

/// The relief's action lock (`ACT`) is one per process, and `cargo test`
/// runs tests as threads of one process: a test that holds it to stand in for
/// a hung tick made a relief test running beside it read "busy" and do
/// nothing (three of these failed together on a local run; nextest's process
/// per test never shared the lock). Every test that reaches the lock holds
/// this first: `Rig` for its whole life, a test without one at its top.
/// Reentrant per thread, so a test with two rigs does not wait on itself.
static ACT_SERIAL: Mutex<()> = Mutex::new(());

thread_local! {
    static HOLDS_ACT_SERIAL: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

/// Holds [`ACT_SERIAL`] until dropped, unless this thread already does.
struct ActSerial(Option<std::sync::MutexGuard<'static, ()>>);

impl Drop for ActSerial {
    fn drop(&mut self) {
        if self.0.is_some() {
            HOLDS_ACT_SERIAL.with(|held| held.set(false));
        }
    }
}

fn act_serial() -> ActSerial {
    if HOLDS_ACT_SERIAL.with(std::cell::Cell::get) {
        return ActSerial(None);
    }
    let guard = ACT_SERIAL
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    HOLDS_ACT_SERIAL.with(|held| held.set(true));
    ActSerial(Some(guard))
}

const STEP: Duration = Duration::from_secs(2);
const MIB: u64 = 1 << 20;

/// The process that started this test: a live process the test did not
/// start, which is what "another running Waylode" stands in for.
fn parent_id() -> u32 {
    #[cfg(unix)]
    {
        std::os::unix::process::parent_id()
    }
    #[cfg(windows)]
    {
        crate::workbench::process_birth::parent_pid().expect("the test's parent is running")
    }
}

// ---------------------------------------------------------------------------
// Fakes. Nothing here signals a process this test did not start.
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq, Eq)]
enum Call {
    Stop(i32),
    Cont(i32),
    Term(i32),
    Kill(i32),
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
    /// Set when a priority change found the ledger locked for good: the caller
    /// held it across the call. (Another test's brief hold is waited out.)
    ledger_held_across_a_call: Arc<Mutex<bool>>,
    /// Processes that are stopped now: by a relief's `Stop`, or by a person
    /// (a test inserts one). `Cont` clears it.
    stopped: Arc<Mutex<HashSet<i32>>>,
    /// `(pid, ppid, start_us)` of every process the host lists, for the
    /// replay's walk of inherited priorities.
    table: Arc<Mutex<Vec<(i32, i32, u64)>>>,
    /// A process somebody stops by hand *between* two reads of its state: the
    /// n-th read of it (n is the value) and every later one finds it stopped.
    flip_on_read: Arc<Mutex<HashMap<i32, u32>>>,
    reads: Arc<Mutex<HashMap<i32, u32>>>,
    /// Processes that exit on `SIGTERM` (the others ignore it).
    exits_on_term: Arc<Mutex<HashSet<i32>>>,
    /// Processes that survive even `SIGKILL` (a process stuck in the kernel).
    survives_kill: Arc<Mutex<HashSet<i32>>>,
}

impl Fake {
    /// A process somebody else stopped (a Ctrl-Z job, a debugger).
    fn stop_by_hand(&self, pid: i32) {
        self.stopped.lock().unwrap().insert(pid);
    }

    fn is_stopped_now(&self, pid: i32) -> bool {
        self.stopped.lock().unwrap().contains(&pid)
    }

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
            Some(_) => Err(Err("pid was recycled by a newer process".into())),
            None => Err(Ok(Delivery::Gone)),
        }
    }
}

impl Actuator for Fake {
    fn set_background(&self, pid: i32, start_us: u64, enable: bool) -> Result<bool, String> {
        // The ledger must not be held across a priority change: the steward
        // and every other caller would wait behind a pass over hundreds.
        let free = lock_within(&LEDGER, Duration::from_secs(2)).is_some();
        if !free {
            *self.ledger_held_across_a_call.lock().unwrap() = true;
        }
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
            Signal::Term => self.calls.lock().unwrap().push(Call::Term(pid)),
            Signal::Kill => self.calls.lock().unwrap().push(Call::Kill(pid)),
        }
        match self.check(pid, start_us) {
            Ok(()) => {
                match signal {
                    Signal::Stop => self.stopped.lock().unwrap().insert(pid),
                    Signal::Cont => self.stopped.lock().unwrap().remove(&pid),
                    // The fake's processes ignore SIGTERM (unless listed) and
                    // die of SIGKILL (unless listed).
                    Signal::Term => {
                        if self.exits_on_term.lock().unwrap().contains(&pid) {
                            self.live.lock().unwrap().remove(&pid);
                        }
                        false
                    }
                    Signal::Kill => {
                        if !self.survives_kill.lock().unwrap().contains(&pid) {
                            self.live.lock().unwrap().remove(&pid);
                        }
                        self.stopped.lock().unwrap().remove(&pid)
                    }
                };
                Ok(Delivery::Delivered)
            }
            Err(outcome) => outcome,
        }
    }

    fn is_background(&self, pid: i32, start_us: u64) -> Option<bool> {
        self.check(pid, start_us).ok()?;
        Some(self.background.lock().unwrap().contains(&pid))
    }

    fn is_stopped(&self, pid: i32, start_us: u64) -> Result<bool, String> {
        match self.check(pid, start_us) {
            Ok(()) => {
                let flip = self.flip_on_read.lock().unwrap().get(&pid).copied();
                if let Some(flip) = flip {
                    let mut reads = self.reads.lock().unwrap();
                    let n = reads.entry(pid).or_default();
                    *n += 1;
                    if *n >= flip {
                        self.stopped.lock().unwrap().insert(pid);
                    }
                }
                Ok(self.is_stopped_now(pid))
            }
            Err(Ok(_)) => Err("process is no longer running".into()),
            Err(Err(why)) => Err(why),
        }
    }

    fn process_table(&self) -> Vec<(i32, i32, u64)> {
        self.table.lock().unwrap().clone()
    }
}

#[derive(Clone)]
struct FakeProbe {
    default_cwd: PathBuf,
    cwd: Arc<Mutex<HashMap<i32, PathBuf>>>,
    env: Arc<Mutex<HashMap<i32, Vec<String>>>>,
    env_unreadable: Arc<Mutex<HashSet<i32>>>,
    /// The arguments of a process; every other one is an agent's tool shell.
    argv: Arc<Mutex<HashMap<i32, Vec<String>>>>,
    argv_unreadable: Arc<Mutex<HashSet<i32>>>,
    leases: Arc<Mutex<Result<LeaseBoard, String>>>,
    /// How many times the environment of each process was read.
    env_reads: Arc<Mutex<HashMap<i32, u32>>>,
}

impl FakeProbe {
    /// A probe whose every process works in a clean worktree.
    fn new(clean: &Path) -> Self {
        Self {
            default_cwd: clean.to_path_buf(),
            cwd: Arc::default(),
            env: Arc::default(),
            env_unreadable: Arc::default(),
            argv: Arc::default(),
            argv_unreadable: Arc::default(),
            leases: Arc::new(Mutex::new(Ok(LeaseBoard::default()))),
            env_reads: Arc::default(),
        }
    }

    /// The board holds `holders` and `waiters`.
    fn board(&self, holders: &[i32], waiters: &[i32]) {
        *self.leases.lock().unwrap() = Ok(LeaseBoard {
            holders: holders.iter().copied().collect(),
            waiters: waiters.iter().copied().collect(),
        });
    }
}

/// What Claude Code's Bash tool starts a command with.
fn tool_shell_argv() -> Vec<String> {
    vec![
        "/bin/zsh".into(),
        "-c".into(),
        "source /Users/x/.claude/shell-snapshots/snapshot-zsh-1795-abc.sh 2>/dev/null || true && eval 'cargo build'".into(),
    ]
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
        *self.env_reads.lock().unwrap().entry(pid).or_default() += 1;
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

    fn argv(&self, pid: i32) -> Option<Vec<String>> {
        if self.argv_unreadable.lock().unwrap().contains(&pid) {
            return None;
        }
        Some(
            self.argv
                .lock()
                .unwrap()
                .get(&pid)
                .cloned()
                .unwrap_or_else(tool_shell_argv),
        )
    }

    fn lease_board(&self) -> Result<LeaseBoard, String> {
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
    /// What the lease board says to each tick.
    refuses: bool,
    /// Last, so it is released after `relief`, whose drop takes the lock.
    _serial: ActSerial,
}

impl Rig {
    fn new(mode: SupervisorMode) -> Self {
        let serial = act_serial();
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
            refuses: false,
            _serial: serial,
        }
    }

    fn tick(&mut self, input: &GovernorInput, table: &ReliefTable) {
        self.now += STEP;
        self.relief
            .tick(self.now, input, self.refuses, &|| table.clone());
    }

    /// The ledger namespace this rig's relief and steward calls share.
    fn ns(&self) -> PathBuf {
        self.dir.path().to_path_buf()
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

/// Swap, in MiB, at which the default thresholds read Severe (0.75 of 64 GiB
/// is 48 GiB; this is a margin above it).
const SEVERE_SWAP_MIB: u64 = 52 * 1024;

/// A host that reads Severe through *swap* with the kernel at Warn: memory
/// pressure is real but not pinned (the kernel is not Critical, the compressor
/// is not thrashing), so a rising swap is what worsens it and a flat one is
/// not. `above` MiB over the Severe floor.
fn severe(above: u64) -> GovernorInput {
    input(Level::Warn, SEVERE_SWAP_MIB + above)
}

/// Severe, with swap climbing `rate` MiB per sample.
fn rising(rate: u64) -> impl Fn(usize) -> GovernorInput {
    move |i| severe(2048 + rate * i as u64)
}

/// Severe, with swap parked high.
fn flat(_: usize) -> GovernorInput {
    severe(40 * 1024)
}

/// Kernel pressure Critical, swap flat: pinned at the edge.
fn critical_flat(_: usize) -> GovernorInput {
    let mut input = GovernorInput::test_pressure(Level::Critical);
    input.readings.swap_used = Reading::Value(2048 * MIB);
    input
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
        ..Sample::default()
    }
}

/// [`severe_sample`] with the kernel at Warn, so rung A has a cause.
fn warn_sample(swap_mib: u64, segment_pct: f64) -> Sample {
    Sample {
        cause: true,
        ..severe_sample(swap_mib, segment_pct)
    }
}

/// Pinned at the edge: kernel Critical, swap as given.
fn critical_sample(swap_mib: u64) -> Sample {
    Sample {
        tier: Some((Tier::Severe, Some(Binding::MemoryPressure))),
        cause: true,
        pinned: true,
        ..severe_sample(swap_mib, 10.0)
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
    // 400 samples (13 minutes) of Severe with swap parked high and the kernel
    // at Warn.
    let out = trigger_run(|_| warn_sample(40 * 1024, 10.0), 400);
    assert!(out.iter().all(|(_, v)| v.freeze == Step::Hold));
    // ...though rung A holds the whole time: the kernel is complaining.
    assert!(out.last().unwrap().1.slow);
}

#[test]
fn a_high_flat_swap_with_a_normal_kernel_slows_nobody() {
    // Swap does not drain after a recovery: 400 samples at Severe by swap
    // *level* alone, the kernel Normal, the compressor idle, nothing rising.
    // That is a host that was in trouble, not one that is.
    let out = trigger_run(|_| severe_sample(60 * 1024, 10.0), 400);
    assert!(
        out.iter().all(|(_, v)| !v.slow),
        "rung A must not turn on for swap level alone"
    );
    assert!(out.iter().all(|(_, v)| v.freeze == Step::Hold));
}

#[test]
fn a_swap_that_is_rising_slows_without_the_kernel_saying_so() {
    // Rising swap is a cause of its own: 15 samples are enough to see it, and
    // three more to enter.
    let out = trigger_run(|i| severe_sample(2048 + 100 * i as u64, 10.0), 40);
    let first = out.iter().find(|(_, v)| v.slow).expect("it slows").0;
    assert!(
        (SLOW_TREND_SAMPLES.min(2 * TREND_ENDS)..SLOW_TREND_SAMPLES + 5).contains(&first),
        "slowed at sample {first}"
    );
}

#[test]
fn slowing_ends_when_the_cause_does_even_though_swap_stays_high() {
    // The kernel complains for 20 samples, then goes Normal; swap stays high.
    let out = trigger_run(
        |i| {
            if i < 20 {
                warn_sample(60 * 1024, 10.0)
            } else {
                severe_sample(60 * 1024, 10.0)
            }
        },
        40,
    );
    assert!(out[19].1.slow);
    assert!(
        !out[20 + EXIT_SAMPLES as usize].1.slow,
        "released {EXIT_SAMPLES} samples after the cause ended"
    );
}

#[test]
fn a_critical_kernel_that_is_flat_still_begins_a_freeze() {
    // Nothing rises: kernel Critical for 60 s is failing on its own.
    let out = trigger_run(|_| critical_sample(2048), 40);
    let first = out
        .iter()
        .find(|(_, v)| v.freeze == Step::Begin)
        .expect("a pinned kernel begins a freeze")
        .0;
    assert_eq!(first, SEVERE_SAMPLES as usize - 1);
}

#[test]
fn compressor_segments_at_ninety_five_percent_count_as_worsening_when_flat() {
    let at = |pct: f64| {
        trigger_run(
            |_| Sample {
                pinned: pct >= PINNED_SEGMENT_PCT,
                cause: true,
                ..severe_sample(2048, pct)
            },
            40,
        )
        .iter()
        .any(|(_, v)| v.freeze == Step::Begin)
    };
    assert!(at(96.0), "segments pinned at 96% of the limit");
    assert!(!at(90.0), "90% and flat is not pinned");
}

#[test]
fn a_pinned_freeze_is_not_ended_by_flat_but_a_swap_level_one_is() {
    let resumed = |feed: &dyn Fn(usize) -> Sample| {
        let mut trigger = Trigger::default();
        let mut now = Instant::now();
        let mut began = None;
        for i in 0..200 {
            now += STEP;
            let verdict = trigger.observe(now, &feed(i));
            match verdict.freeze {
                Step::Begin => {
                    trigger.began(now, true);
                    began = Some(i);
                }
                Step::Resume(reason) => return Some((began?, i, reason)),
                Step::Hold => {}
            }
        }
        None
    };
    // Rising to begin, then parked: the swap-level case resumes flat.
    let swap =
        resumed(&|i| warn_sample(2048 + if i < 40 { 100 * i as u64 } else { 100 * 40 }, 10.0));
    let (began, at, reason) = swap.expect("a flat swap ends the freeze");
    assert_eq!(reason, ResumeReason::Flat);
    assert!(at - began >= FLAT_SAMPLES);
    // Kernel Critical the whole time: nothing to call flat.
    assert_eq!(
        resumed(&|_| critical_sample(2048)),
        None,
        "a pinned kernel is not 'flat': the freeze runs to its cap, not before"
    );
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
            cause: true,
            pinned: false,
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
        cause: true,
        pinned: false,
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
                ..Sample::default()
            },
        ),
        (
            "ungovernable",
            Sample {
                tier: None,
                swap_used: None,
                segment_pct: None,
                ..Sample::default()
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
    pick_while(table, probe, room, false)
}

/// [`pick`] while the lease board is (not) refusing new builds.
fn pick_while(
    table: &ReliefTable,
    probe: &FakeProbe,
    room: usize,
    refusing: bool,
) -> Result<Vec<Selection>, String> {
    select(
        table,
        probe,
        &|_| true,
        &Want {
            busy: &BTreeSet::new(),
            room,
            refusing,
            spent: &HashSet::new(),
        },
        &mut MarkCache::default(),
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
fn a_build_lease_waiter_and_everything_under_it_is_never_selected() {
    let (probe, _w) = fresh_probe();
    // The waiter is the cargo: the subtree and its rustc are both barred,
    // whether or not the board is refusing.
    probe.board(&[], &[102]);
    for refusing in [false, true] {
        assert!(pick_while(&standard(), &probe, 3, refusing)
            .unwrap()
            .is_empty());
    }
    // The waiter is a process inside the subtree.
    probe.board(&[], &[103]);
    for refusing in [false, true] {
        assert!(pick_while(&standard(), &probe, 3, refusing)
            .unwrap()
            .is_empty());
    }
    // Another build's waiter leaves this one alone.
    probe.board(&[], &[4242]);
    assert_eq!(pick(&standard(), &probe, 3).unwrap().len(), 1);
    // A board that could not be read freezes nothing, and says why.
    *probe.leases.lock().unwrap() = Err("slot-3 unreadable".into());
    let why = pick(&standard(), &probe, 3).unwrap_err();
    assert!(why.contains("slot-3 unreadable"), "{why}");
}

#[test]
fn a_build_lease_holder_is_selected_only_while_the_board_is_refusing() {
    let (probe, _w) = fresh_probe();
    // The holder is the cargo.
    probe.board(&[102], &[]);
    assert!(
        pick_while(&standard(), &probe, 3, false)
            .unwrap()
            .is_empty(),
        "a board that takes builds leaves its holders alone"
    );
    let picks = pick_while(&standard(), &probe, 3, true).unwrap();
    assert_eq!(
        picks.len(),
        1,
        "a refusing board frees its holder for rung B"
    );
    assert_eq!(picks[0].root.pid, 102);
    assert!(
        picks[0].holder,
        "the selection says a lease holder is in it"
    );
    // The holder is a process inside the subtree: same.
    probe.board(&[103], &[]);
    assert!(pick_while(&standard(), &probe, 3, false)
        .unwrap()
        .is_empty());
    assert!(pick_while(&standard(), &probe, 3, true).unwrap()[0].holder);
    // A tree with no holder in it says so.
    probe.board(&[4242], &[]);
    assert!(!pick(&standard(), &probe, 3).unwrap()[0].holder);
}

#[test]
fn a_held_marker_in_a_subtree_is_eligible_only_while_the_board_is_refusing() {
    for marker in [
        "WAYLODE_LEASE_HELD=2",
        "GITPULSE_LEASE_HELD=2",
        "CARGO_SLOT_HELD=1",
        "WAYLODE_LEASE_HELD=anything",
        "GITPULSE_LEASE_HELD=anything",
    ] {
        let (probe, _w) = fresh_probe();
        probe
            .env
            .lock()
            .unwrap()
            .insert(103, vec!["PATH=/usr/bin".into(), marker.into()]);
        assert!(
            pick_while(&standard(), &probe, 3, false)
                .unwrap()
                .is_empty(),
            "{marker}: barred while the board takes builds"
        );
        let picks = pick_while(&standard(), &probe, 3, true).unwrap();
        assert_eq!(
            picks.len(),
            1,
            "{marker}: eligible while it refuses (any value)"
        );
        assert!(picks[0].holder);
    }
    // mac_heavy.sh's marker is a person's heavy job: never, refusing or not.
    let (probe, _w) = fresh_probe();
    probe
        .env
        .lock()
        .unwrap()
        .insert(103, vec!["MAC_HEAVY_LABEL=go-test".into()]);
    for refusing in [false, true] {
        assert!(pick_while(&standard(), &probe, 3, refusing)
            .unwrap()
            .is_empty());
    }
}

#[test]
fn trees_carrying_a_heavy_marker_or_an_unreadable_environment_are_never_selected() {
    let (probe, _w) = fresh_probe();
    probe.env.lock().unwrap().insert(
        103,
        vec!["PATH=/usr/bin".into(), "MAC_HEAVY_LABEL=go-test".into()],
    );
    assert!(
        pick(&standard(), &probe, 3).unwrap().is_empty(),
        "MAC_HEAVY_LABEL on a member bars the subtree"
    );
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
    let want = Want {
        busy: &BTreeSet::new(),
        room: 3,
        refusing: false,
        spent: &HashSet::new(),
    };
    let untrusted = select(
        &table,
        &probe,
        &|_| false,
        &want,
        &mut MarkCache::default(),
        &mut notes,
    )
    .unwrap();
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
        fn argv(&self, pid: i32) -> Option<Vec<String>> {
            self.0.argv(pid)
        }
        fn lease_board(&self) -> Result<LeaseBoard, String> {
            self.0.lease_board()
        }
    }
    let picks = select(
        &standard(),
        &Blind(probe),
        &|_| true,
        &want,
        &mut MarkCache::default(),
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
    let picks = select(
        &table,
        &probe,
        &|_| true,
        &Want {
            busy: &busy,
            room: 3,
            refusing: false,
            spent: &HashSet::new(),
        },
        &mut MarkCache::default(),
        &mut Vec::new(),
    )
    .unwrap();
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
        rig.tick(&severe(5000), &table);
        assert!(rig.fake.conts().is_empty(), "sample {n} after the freeze");
    }
    rig.tick(&severe(5000), &table);
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
        rig.tick(&severe(9000 + 100 * n as u64), &table);
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
    assert!(ledger_get(&lock(&LEDGER), &rig.ns(), 102).is_none());
    // The build ended but its processes are still running (they left the
    // agent's tree): what was slowed is put back, not forgotten in the
    // background.
    let mut restored = rig.fake.restored();
    restored.sort_unstable();
    assert_eq!(restored, [102, 103], "restored on absence from the trees");
    assert!(
        !rig.fake.background.lock().unwrap().contains(&102)
            && !rig.fake.background.lock().unwrap().contains(&103),
        "and they read back at normal priority"
    );
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
    let parent = parent_id();
    let live_start = crate::sysload::process_start_us(parent).expect("the parent is running");
    let entries = vec![JournalEntry {
        pid: 301,
        start_us: 77,
        session_id: "300-1".into(),
        at_unix_ms: 1,
        rung: JournalRung::Freeze,
        was_running: true,
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
    let replay = replay_journal(dir.path(), dir.path(), &fake);
    assert_eq!(replay.left_to, Some(parent));
    assert!(fake.calls().is_empty());
    assert!(journal_path(dir.path()).exists(), "and its journal stays");
    // The same pid with another start time is a recycled one: the writer is
    // gone, so the journal is ours to replay.
    write(parent, live_start + 1);
    let replay = replay_journal(dir.path(), dir.path(), &fake);
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
    rig.tick(&severe(9000), &table);
    assert!(rig.relief.status.frozen.is_empty());
    assert_eq!(rig.fake.conts(), [103, 102]);
    let note = rig.relief.status.last_action.clone().unwrap();
    assert_eq!(note.kind, ActionKind::Resume);
    assert!(note.detail.contains("the person asked"), "{note:?}");
    // Severe and still worsening for a long time: held off, nothing refreezes.
    rig.ticks(120, |i| severe(9000 + 100 * i as u64), &table);
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
        |i| severe(9000 + 100 * i as u64),
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
        .is_some_and(|d| d.contains("recycled")));
    assert_eq!(
        rig.fake.conts(),
        [103, 102],
        "the refused one was attempted, not skipped"
    );
}

#[test]
fn resume_all_leaves_another_running_gitpulses_processes_alone_and_without_a_relief_does_nothing() {
    let _serial = act_serial();
    let dir = tempfile::tempdir().unwrap();
    let parent = parent_id();
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
            was_running: true,
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
    assert_eq!(
        snap.binding,
        Some(Binding::Swap),
        "Severe through swap, with the kernel at Warn"
    );
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
        rig.tick(&severe(9000 + 100 * n as u64), &table);
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
        .any(|f| f.pid == Some(103) && f.what.contains("recycled")));
    assert!(snap.failure_count >= 1);
}

#[test]
fn nothing_to_freeze_is_a_refused_action() {
    let mut rig = Rig::new(SupervisorMode::Act);
    let table = standard();
    live_all(&rig, &table);
    // A waiter on the build: never eligible, whatever the board says.
    rig.probe.board(&[], &[102]);
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
    rig.tick(&severe(9000), &grown);
    assert_eq!(rig.fake.stops(), [102, 103, 120]);
    assert_eq!(rig.relief.status.frozen[0].members, 3);
    // When the subtree's own root ends, the rest is let go at once.
    let mut ended = grown.clone();
    ended.sessions[0].members.retain(|m| m.pid != 102);
    rig.tick(&severe(9100), &ended);
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
    rig.tick(&severe(9000), &table);
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
    let mut off = severe(9000);
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
            .any(|f| f.what.contains("recycled")),
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
    let _serial = act_serial();
    let dir = tempfile::tempdir().unwrap();
    let entries = vec![
        JournalEntry {
            pid: 301,
            start_us: 77,
            session_id: "300-1".into(),
            at_unix_ms: 1,
            rung: JournalRung::Freeze,
            was_running: true,
        },
        JournalEntry {
            pid: 302,
            start_us: 88,
            session_id: "300-1".into(),
            at_unix_ms: 1,
            rung: JournalRung::Freeze,
            was_running: true,
        },
        JournalEntry {
            pid: 303,
            start_us: 99,
            session_id: "300-1".into(),
            at_unix_ms: 1,
            rung: JournalRung::Slow,
            was_running: true,
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
            .any(|f| f.what.contains("302") && f.what.contains("recycled")),
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
    let replay = replay_journal(dir.path(), dir.path(), &Fake::default());
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
        steward_throttle_with(&rig.fake, &rig.ns(), 9_000_102, start, false),
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
        steward_throttle_with(&rig.fake, &rig.ns(), 9_100_102, start, true),
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
        steward_throttle_with(&rig.fake, &rig.ns(), 9_100_102, start, false),
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
// The lease ruling: a refusing board frees its holders; waiters never.
// ---------------------------------------------------------------------------

/// A relief in act mode, its lease board refusing new builds (or not).
fn rig_with_board(refuses: bool, holders: &[i32], waiters: &[i32]) -> Rig {
    let mut rig = Rig::new(SupervisorMode::Act);
    rig.refuses = refuses;
    rig.probe.board(holders, waiters);
    rig
}

#[test]
fn a_lease_holder_subtree_is_frozen_at_severe_while_the_board_refuses_and_a_waiter_never_is() {
    let table = standard();
    let mut rig = rig_with_board(true, &[102], &[]);
    live_all(&rig, &table);
    rig.ticks(SEVERE_SAMPLES as usize, rising(100), &table);
    assert_eq!(
        rig.fake.stops(),
        [102, 103],
        "a holder, with the board refusing"
    );
    assert!(rig.relief.frozen[0].holder);

    let mut rig = rig_with_board(false, &[102], &[]);
    live_all(&rig, &table);
    rig.ticks(SEVERE_SAMPLES as usize, rising(100), &table);
    assert!(rig.fake.stops().is_empty(), "the board is taking builds");

    let mut rig = rig_with_board(true, &[], &[102]);
    live_all(&rig, &table);
    rig.ticks(SEVERE_SAMPLES as usize, rising(100), &table);
    assert!(
        rig.fake.stops().is_empty(),
        "a waiter is about to run: never, refusing or not"
    );

    let mut rig = rig_with_board(true, &[], &[]);
    rig.probe
        .env
        .lock()
        .unwrap()
        .insert(103, vec!["MAC_HEAVY_LABEL=go-test".into()]);
    live_all(&rig, &table);
    rig.ticks(SEVERE_SAMPLES as usize, rising(100), &table);
    assert!(rig.fake.stops().is_empty(), "a person's heavy job: never");
}

#[test]
fn a_frozen_holder_resumes_on_the_tick_the_board_stops_refusing_while_swap_is_still_high() {
    let table = standard();
    let mut rig = rig_with_board(true, &[102], &[]);
    live_all(&rig, &table);
    rig.ticks(SEVERE_SAMPLES as usize, rising(100), &table);
    assert_eq!(rig.fake.stops(), [102, 103]);
    assert!(rig.fake.conts().is_empty());
    // Swap is still climbing, so "flat" could not end this; the board does.
    rig.refuses = false;
    rig.tick(&severe(2048 + 100 * SEVERE_SAMPLES as u64), &table);
    assert_eq!(rig.fake.conts(), [103, 102], "on that very tick");
    assert!(rig.relief.status.frozen.is_empty());
    let last = rig.relief.status.last_action.clone().unwrap();
    assert_eq!(last.kind, ActionKind::Resume);
    assert!(last.detail.contains("lease"), "{last:?}");
    assert!(
        read_journal(rig.dir.path())
            .unwrap()
            .is_none_or(|j| j.entries.iter().all(|e| e.rung != JournalRung::Freeze)),
        "and its journal entries went with it"
    );
    // The board takes builds, so the holder is no longer eligible: nothing
    // freezes it again however long swap climbs.
    rig.ticks(100, |i| severe(9000 + 100 * i as u64), &table);
    assert_eq!(rig.fake.stops(), [102, 103]);
}

#[test]
fn a_frozen_build_that_holds_no_lease_is_not_continued_when_the_board_opens() {
    let table = standard();
    let mut rig = rig_with_board(true, &[], &[]);
    live_all(&rig, &table);
    rig.ticks(SEVERE_SAMPLES as usize, rising(100), &table);
    assert_eq!(rig.fake.stops(), [102, 103]);
    assert!(!rig.relief.frozen[0].holder);
    rig.refuses = false;
    rig.tick(&severe(2048 + 100 * SEVERE_SAMPLES as u64), &table);
    assert!(rig.fake.conts().is_empty(), "its own conditions still hold");
    assert_eq!(rig.relief.status.frozen.len(), 1);
}

// ---------------------------------------------------------------------------
// Rung A needs a cause; rung B counts a pinned edge as worsening.
// ---------------------------------------------------------------------------

#[test]
fn a_high_flat_swap_under_a_normal_kernel_slows_nobody_and_releases_existing_slowing() {
    let table = standard();
    let mut rig = Rig::new(SupervisorMode::Act);
    live_all(&rig, &table);
    rig.ticks(200, |_| input(Level::Normal, 60 * 1024), &table);
    assert!(rig.fake.calls().is_empty(), "{:?}", rig.fake.calls());
    assert_eq!(rig.relief.status.rung, Rung::None);
    assert_eq!(
        rig.relief.status.tier,
        Some(Tier::Severe),
        "it did read Severe"
    );

    // Slowing that is already in force lets go once the cause has.
    let mut rig = Rig::new(SupervisorMode::Act);
    live_all(&rig, &table);
    rig.ticks(5, |_| input(Level::Warn, 0), &table);
    assert_eq!(rig.relief.status.slowed, 5);
    rig.ticks(
        EXIT_SAMPLES as usize,
        |_| input(Level::Normal, 60 * 1024),
        &table,
    );
    assert_eq!(rig.relief.status.slowed, 0);
    let mut restored = rig.fake.restored();
    restored.sort_unstable();
    assert_eq!(restored, [101, 102, 103, 105, 106]);
}

#[test]
fn a_critical_kernel_with_flat_swap_freezes_and_stays_frozen_until_the_tier_eases() {
    let table = standard();
    let mut rig = Rig::new(SupervisorMode::Act);
    live_all(&rig, &table);
    rig.ticks(SEVERE_SAMPLES as usize, critical_flat, &table);
    assert_eq!(rig.fake.stops(), [102, 103], "pinned at Critical for 60 s");
    // 200 s more of the same: nothing is "flat" about a kernel at Critical.
    rig.ticks(100, critical_flat, &table);
    assert!(rig.fake.conts().is_empty(), "{:?}", rig.fake.conts());
    rig.ticks(
        SEVERE_BREAK_SAMPLES as usize,
        |_| input(Level::Normal, 0),
        &table,
    );
    assert_eq!(rig.fake.conts(), [103, 102]);
}

#[test]
fn compressor_segments_pinned_near_their_limit_freeze_even_when_nothing_rises() {
    let table = standard();
    let mut rig = Rig::new(SupervisorMode::Act);
    live_all(&rig, &table);
    let segments = |pct: u64| {
        move |_: usize| {
            let mut input = GovernorInput::test_pressure(Level::Normal);
            input.readings.segments = Reading::Value(CompressorSegments {
                total: pct,
                limit: 100,
            });
            input
        }
    };
    // 90 % is Severe by segments but is not the edge, and flat is not worsening.
    rig.ticks(SEVERE_SAMPLES as usize + 10, segments(90), &table);
    assert!(rig.fake.stops().is_empty(), "90% and flat");
    let mut rig = Rig::new(SupervisorMode::Act);
    live_all(&rig, &table);
    rig.ticks(SEVERE_SAMPLES as usize, segments(96), &table);
    assert_eq!(rig.fake.stops(), [102, 103], "96% of the limit, flat");
}

// ---------------------------------------------------------------------------
// Rung B enters only what is positively an agent's tool shell, and judges
// late joiners by the same predicate.
// ---------------------------------------------------------------------------

#[test]
fn only_a_shell_identified_as_an_agent_tool_shell_is_entered() {
    let (probe, _w) = fresh_probe();
    // An MCP server's launcher is a shell too: `zsh -c exec some-mcp`.
    probe.argv.lock().unwrap().insert(
        101,
        vec![
            "/bin/zsh".into(),
            "-c".into(),
            "exec context7-server --stdio".into(),
        ],
    );
    assert!(pick(&standard(), &probe, 3).unwrap().is_empty());
    // A shell whose arguments cannot be read is not identified either.
    probe.argv.lock().unwrap().clear();
    probe.argv_unreadable.lock().unwrap().insert(101);
    assert!(pick(&standard(), &probe, 3).unwrap().is_empty());
    // Claude Code's Bash tool shell, under any config directory.
    probe.argv_unreadable.lock().unwrap().clear();
    for config in ["/Users/x/.claude", "/Users/x/.claude-sarat"] {
        probe.argv.lock().unwrap().insert(
            101,
            vec![
                "/bin/zsh".into(),
                "-c".into(),
                format!("source {config}/shell-snapshots/snapshot-zsh-17-abc.sh && cargo build"),
            ],
        );
        assert_eq!(pick(&standard(), &probe, 3).unwrap().len(), 1, "{config}");
    }
}

#[test]
fn a_late_joiner_is_judged_by_the_predicate_that_chose_the_subtree() {
    let table = standard();
    let mut rig = Rig::new(SupervisorMode::Act);
    live_all(&rig, &table);
    rig.ticks(SEVERE_SAMPLES as usize, rising(100), &table);
    assert_eq!(rig.fake.stops(), [102, 103]);
    let locked = clean_worktree();
    std::fs::write(locked.path().join(".git/index.lock"), b"").unwrap();
    // Seven processes appear under the frozen rustc: only one may be stopped.
    let mut grown = table.clone();
    for (pid, name) in [
        (120, "cc1"),
        (121, "context7-mcp"),
        (122, "ld"),
        (123, "ld"),
        (124, "ld"),
        (125, "ld"),
        (126, "git"),
    ] {
        grown.sessions[0].members.push(proc(pid, 103, name, 50));
    }
    live_all(&rig, &grown);
    rig.probe
        .env
        .lock()
        .unwrap()
        .insert(122, vec!["MAC_HEAVY_LABEL=go-test".into()]);
    rig.probe
        .env
        .lock()
        .unwrap()
        .insert(125, vec!["GITPULSE_LEASE_HELD=1".into()]);
    rig.probe.board(&[], &[123]);
    rig.probe
        .cwd
        .lock()
        .unwrap()
        .insert(124, locked.path().to_path_buf());
    rig.tick(&severe(5000), &grown);
    assert_eq!(
        rig.fake.stops(),
        [102, 103, 120],
        "not the MCP server (121), the heavy job (122), the lease waiter (123), \
         the one in a locked worktree (124), the lease-marker carrier (125), or git (126)"
    );
}

#[test]
fn late_joiners_are_not_stopped_when_the_lease_board_cannot_be_read() {
    let table = standard();
    let mut rig = Rig::new(SupervisorMode::Act);
    live_all(&rig, &table);
    rig.ticks(SEVERE_SAMPLES as usize, rising(100), &table);
    let mut grown = table.clone();
    grown.sessions[0].members.push(proc(120, 103, "cc1", 50));
    live_all(&rig, &grown);
    *rig.probe.leases.lock().unwrap() = Err("slot-1 unreadable".into());
    rig.tick(&severe(5000), &grown);
    assert_eq!(rig.fake.stops(), [102, 103]);
    assert!(rig
        .relief
        .status
        .failures
        .iter()
        .any(|f| f.what.contains("slot-1 unreadable")));
}

// ---------------------------------------------------------------------------
// The watchdog, the bounded shutdown, and a live writer's aged journal.
// ---------------------------------------------------------------------------

#[test]
fn the_watchdog_continues_what_is_past_the_cap_and_its_grace_and_nothing_younger() {
    let fake = Fake::default();
    let watch = Watch::default();
    let t0 = Instant::now();
    let limit = t0 + FREEZE_CAP + WATCHDOG_GRACE;
    watch.beat(limit);
    watch.set(vec![
        Watched {
            pid: 1,
            start_us: 10,
            since: t0,
        },
        Watched {
            pid: 2,
            start_us: 20,
            since: t0 + Duration::from_secs(300),
        },
    ]);
    assert!(
        watch.pass(&fake, limit - Duration::from_secs(1)).is_empty(),
        "still inside the sampler's own cap plus the grace"
    );
    assert_eq!(watch.pass(&fake, limit), [(1, 10)]);
    assert_eq!(fake.conts(), [1], "only the overdue one");
    assert!(watch.pass(&fake, limit).is_empty(), "and only once");
}

#[test]
fn the_watchdog_continues_everything_when_the_tick_stops_advancing() {
    let fake = Fake::default();
    let watch = Watch::default();
    let t0 = Instant::now();
    watch.beat(t0);
    watch.set(vec![
        Watched {
            pid: 1,
            start_us: 10,
            since: t0,
        },
        Watched {
            pid: 2,
            start_us: 20,
            since: t0,
        },
    ]);
    assert!(watch
        .pass(&fake, t0 + WATCHDOG_STALL - Duration::from_secs(1))
        .is_empty());
    watch.beat(t0 + Duration::from_secs(30));
    assert!(
        watch
            .pass(&fake, t0 + WATCHDOG_STALL + Duration::from_secs(1))
            .is_empty(),
        "a tick at 30 s is a sampler that is alive"
    );
    assert_eq!(
        watch.pass(&fake, t0 + Duration::from_secs(30) + WATCHDOG_STALL),
        [(2, 20), (1, 10)],
        "last stopped first"
    );
}

#[test]
fn a_freeze_the_sampler_can_no_longer_end_is_continued_by_the_watchdog_without_the_action_lock() {
    let table = standard();
    let mut rig = Rig::new(SupervisorMode::Act);
    live_all(&rig, &table);
    rig.ticks(SEVERE_SAMPLES as usize, rising(100), &table);
    assert_eq!(rig.fake.stops(), [102, 103]);
    let watch = rig.relief.watch();
    // The tick hangs on a disk write, holding the action lock. The watchdog
    // needs neither it nor the journal.
    let hung = lock(&ACT);
    let sent = watch.pass(&rig.fake, rig.now + WATCHDOG_STALL + STEP);
    drop(hung);
    assert_eq!(sent, [(103, 1_000_103), (102, 1_000_102)]);
    assert_eq!(rig.fake.conts(), [103, 102]);
}

#[test]
fn the_watch_follows_what_the_relief_freezes_and_resumes() {
    let table = standard();
    let mut rig = Rig::new(SupervisorMode::Act);
    live_all(&rig, &table);
    let watch = rig.relief.watch();
    rig.ticks(SEVERE_SAMPLES as usize, rising(100), &table);
    assert_eq!(lock(&watch.state).frozen.len(), 2);
    rig.ticks(
        SEVERE_BREAK_SAMPLES as usize,
        |_| input(Level::Normal, 0),
        &table,
    );
    assert!(lock(&watch.state).frozen.is_empty());
}

#[test]
fn shutdown_does_not_wait_for_a_hung_tick_and_continues_from_memory() {
    let table = standard();
    let mut rig = Rig::new(SupervisorMode::Act);
    live_all(&rig, &table);
    rig.ticks(SEVERE_SAMPLES as usize, rising(100), &table);
    let watch = rig.relief.watch();
    let hung = lock(&ACT);
    let began = Instant::now();
    let replayed = shutdown_within(
        rig.dir.path(),
        &rig.ns(),
        Some(&*watch),
        &rig.fake,
        Duration::from_millis(150),
    );
    let took = began.elapsed();
    drop(hung);
    assert!(!replayed, "the action lock never came");
    assert!(took < Duration::from_secs(2), "{took:?}");
    assert_eq!(
        rig.fake.conts(),
        [103, 102],
        "continued from the memory copy"
    );
    assert!(
        journal_path(rig.dir.path()).exists(),
        "the journal is left for the next start"
    );
    // With the lock free, the journal is replayed as well.
    assert!(shutdown_within(
        rig.dir.path(),
        &rig.ns(),
        Some(&*watch),
        &rig.fake,
        Duration::from_millis(150),
    ));
    assert!(!journal_path(rig.dir.path()).exists());
}

#[test]
fn recovery_leaves_a_live_writers_journal_entirely_alone_however_old_it_is() {
    let _serial = act_serial();
    let dir = tempfile::tempdir().unwrap();
    let parent = parent_id();
    let live_start = crate::sysload::process_start_us(parent).expect("the parent is running");
    let now = unix_ms();
    let grace = u64::try_from((FREEZE_CAP + WATCHDOG_GRACE).as_millis()).unwrap();
    let entry = |pid: i32, at: u64, rung: JournalRung| JournalEntry {
        pid,
        start_us: 1000 + pid as u64,
        session_id: "300-1".into(),
        at_unix_ms: at,
        rung,
        was_running: true,
    };
    let entries = vec![
        entry(301, now - grace - 1000, JournalRung::Freeze),
        entry(302, now - 60_000, JournalRung::Freeze),
        entry(303, now - grace - 1000, JournalRung::Slow),
    ];
    write_journal_as(dir.path(), &entries, parent, live_start).unwrap();
    let fake = Fake::default();
    let worktree = clean_worktree();
    let mut relief = Relief::new(
        dir.path().to_path_buf(),
        Box::new(fake.clone()),
        Box::new(FakeProbe::new(worktree.path())),
        Box::new(|| SupervisorMode::Act),
        Box::new(|_| true),
    );
    relief.recover();
    // A live writer's watchdog continues what it stopped; a second GitPulse
    // that guessed at its entries by age could signal a process the writer did
    // not stop (a job the user paused), so it signals nothing.
    assert!(fake.conts().is_empty(), "{:?}", fake.conts());
    assert!(fake.restored().is_empty(), "slowed ones are the writer's");
    let left = read_journal(dir.path()).unwrap().unwrap();
    assert_eq!(left.writer_pid, parent, "the writer's file is untouched");
    assert_eq!(left.entries, entries);
}

#[test]
fn a_resume_all_that_lands_while_a_tick_waits_for_the_action_lock_is_seen_by_that_tick() {
    let table = standard();
    let mut rig = Rig::new(SupervisorMode::Act);
    live_all(&rig, &table);
    rig.ticks(SEVERE_SAMPLES as usize, rising(100), &table);
    let dir = rig.dir.path().to_path_buf();
    let before = external_of(&dir).generation;
    let (now, sample) = (rig.now + STEP, severe(9000));
    // The hook runs on the tick's thread at the last moment before it asks for
    // the action lock: everything the tick reads before the lock is read by the
    // time the test hears of it, so a generation read ahead of the lock would
    // be `before`. No sleep: the test waits for that event.
    let (reached, heard) = std::sync::mpsc::channel::<()>();
    let reached = Mutex::new(reached);
    rig.relief.before_act = Some(Arc::new(move || {
        let _ = reached.lock().unwrap().send(());
    }));
    let held = lock(&ACT);
    std::thread::scope(|scope| {
        let relief = &mut rig.relief;
        let tick = scope.spawn(move || relief.tick(now, &sample, false, &|| table.clone()));
        heard.recv().expect("the tick reached the lock");
        {
            let mut all = lock(&EXTERNAL);
            let external = all.entry(dir.clone()).or_default();
            external.generation += 1;
            external.reason = "a late request".into();
        }
        drop(held);
        tick.join().unwrap();
    });
    assert_eq!(rig.relief.seen_external, before + 1);
    assert!(rig.relief.status.frozen.is_empty());
    let note = rig.relief.status.last_action.clone().unwrap();
    assert!(note.detail.contains("a late request"), "{note:?}");
}

// ---------------------------------------------------------------------------
// Slowing: restore what left the trees, restore inheritors, one namespace per
// relief, and a bounded pass.
// ---------------------------------------------------------------------------

#[test]
fn putting_back_a_slowed_process_also_puts_back_the_children_that_inherited_its_priority() {
    let table = standard();
    let mut rig = Rig::new(SupervisorMode::Act);
    live_all(&rig, &table);
    rig.ticks(5, |_| input(Level::Warn, 0), &table);
    assert_eq!(rig.relief.status.slowed, 5);
    // Children that started after the last pass inherited the priority; the
    // relief never named them. One is the steward's own, one is a sibling of
    // the agent's shell, under no process the relief slowed.
    let mut grown = table.clone();
    for (pid, ppid) in [(120, 102), (121, 103), (122, 102), (130, 100)] {
        grown.sessions[0].members.push(proc(pid, ppid, "cc1", 50));
    }
    live_all(&rig, &grown);
    for pid in [120, 121, 130] {
        rig.fake.background.lock().unwrap().insert(pid);
    }
    let start = 1_000_122;
    assert_eq!(
        steward_throttle_with(&rig.fake, &rig.ns(), 122, start, true),
        Ok(true)
    );
    // The last quiet sample ends the slowing, on a tick that makes no pass.
    rig.ticks(
        EXIT_SAMPLES as usize - 1,
        |_| input(Level::Normal, 0),
        &table,
    );
    rig.tick(&input(Level::Normal, 0), &grown);
    let restored = rig.fake.restored();
    assert!(
        restored.contains(&120) && restored.contains(&121),
        "inheritors of a slowed process: {restored:?}"
    );
    assert!(
        !restored.contains(&122),
        "the steward's own is the steward's"
    );
    assert!(!restored.contains(&130), "under nothing the relief slowed");
    let background = rig.fake.background.lock().unwrap();
    assert!(background.contains(&122) && background.contains(&130));
}

#[test]
fn two_reliefs_that_share_pids_do_not_share_a_ledger() {
    let table = standard();
    let mut a = Rig::new(SupervisorMode::Act);
    let mut b = Rig::new(SupervisorMode::Act);
    live_all(&a, &table);
    live_all(&b, &table);
    // In A's namespace the steward owns the cargo.
    assert_eq!(
        steward_throttle_with(&a.fake, &a.ns(), 102, 1_000_102, true),
        Ok(true)
    );
    b.ticks(ENTER_SAMPLES as usize, |_| input(Level::Warn, 0), &table);
    assert!(
        b.fake.backgrounded().contains(&102) && b.relief.slowed.contains_key(&102),
        "B's relief is not told that A's steward owns pid 102"
    );
    a.ticks(ENTER_SAMPLES as usize, |_| input(Level::Warn, 0), &table);
    assert!(
        !a.relief.slowed.contains_key(&102),
        "but in A's own namespace it does"
    );
}

#[test]
fn a_pass_slows_a_bounded_number_without_holding_the_ledger_and_the_rest_follow() {
    let mut members = vec![proc(101, 100, "zsh", 10)];
    members.extend((0..300).map(|n| proc(1000 + n, 101, "worker", 1)));
    let table = ReliefTable {
        sessions: vec![session(100, true, members)],
    };
    let mut rig = Rig::new(SupervisorMode::Act);
    live_all(&rig, &table);
    rig.ticks(ENTER_SAMPLES as usize, |_| input(Level::Warn, 0), &table);
    assert_eq!(
        rig.fake.backgrounded().len(),
        MAX_SLOW_PER_PASS,
        "the first pass is bounded"
    );
    rig.ticks(SLOW_PASS_EVERY as usize, |_| input(Level::Warn, 0), &table);
    assert_eq!(
        rig.fake.backgrounded().len(),
        301,
        "the next pass takes the rest"
    );
    assert!(
        !*rig.fake.ledger_held_across_a_call.lock().unwrap(),
        "no priority change ran with the ledger locked"
    );
    rig.ticks(EXIT_SAMPLES as usize, |_| input(Level::Normal, 0), &table);
    assert_eq!(rig.fake.restored().len(), 301);
    assert!(!*rig.fake.ledger_held_across_a_call.lock().unwrap());
}

#[test]
fn environments_and_arguments_are_read_once_per_process_across_selections() {
    let (probe, _w) = fresh_probe();
    let mut marks = MarkCache::default();
    let want = Want {
        busy: &BTreeSet::new(),
        room: 3,
        refusing: false,
        spent: &HashSet::new(),
    };
    for _ in 0..4 {
        let picks = select(
            &standard(),
            &probe,
            &|_| true,
            &want,
            &mut marks,
            &mut Vec::new(),
        )
        .unwrap();
        assert_eq!(picks.len(), 1);
    }
    let reads = probe.env_reads.lock().unwrap().clone();
    assert!(!reads.is_empty());
    assert!(
        reads.values().all(|n| *n == 1),
        "an environment cannot change after exec, so it is read once: {reads:?}"
    );
    // A read that failed is not remembered as an answer.
    let (probe, _w) = fresh_probe();
    probe.env_unreadable.lock().unwrap().insert(102);
    let mut marks = MarkCache::default();
    for _ in 0..3 {
        let _ = select(
            &standard(),
            &probe,
            &|_| true,
            &want,
            &mut marks,
            &mut Vec::new(),
        );
    }
    assert!(
        probe.env_reads.lock().unwrap()[&102] >= 3,
        "an unreadable environment is asked for again, every selection"
    );
}

// ---------------------------------------------------------------------------
// The published status file and the resume request seam.
// ---------------------------------------------------------------------------

use crate::sysload::relief_status as file;

/// A rig whose relief publishes `host-relief.v1.json` beside its journal.
fn publishing_rig(mode: SupervisorMode) -> Rig {
    let mut rig = Rig::new(mode);
    let dir = rig.dir.path().to_path_buf();
    rig.relief = std::mem::replace(
        &mut rig.relief,
        // A throwaway to move out of; nothing ticks it.
        Relief::new(
            dir.clone(),
            Box::new(Fake::default()),
            Box::new(FakeProbe::new(&dir)),
            Box::new(|| SupervisorMode::Off),
            Box::new(|_| false),
        ),
    )
    .with_status_dir(dir);
    rig
}

fn published(rig: &Rig) -> file::ReliefStatus {
    match file::read_status(rig.dir.path()) {
        file::ReliefOutcome::Fresh { status } | file::ReliefOutcome::Stale { status, .. } => status,
        other => panic!("no status file: {other:?}"),
    }
}

fn two_sessions() -> ReliefTable {
    ReliefTable {
        sessions: (0..2)
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
            .collect(),
    }
}

#[test]
fn the_status_file_is_written_at_startup_every_active_tick_and_on_the_way_back_to_rest() {
    let table = standard();
    let mut rig = publishing_rig(SupervisorMode::Act);
    live_all(&rig, &table);
    // A previous run's claim is replaced at startup, not left to be believed.
    let mut stale = file::ReliefStatus::at_rest(file::ReliefMode::Act, file::now_ms());
    stale.rung = file::ReliefRung::Freeze;
    file::write_status(rig.dir.path(), &stale).unwrap();
    rig.relief.recover();
    let at_start = published(&rig);
    assert_eq!(at_start.rung, file::ReliefRung::None);
    assert_eq!(at_start.writer_pid, std::process::id());

    rig.ticks(ENTER_SAMPLES as usize, |_| input(Level::Warn, 0), &table);
    let slow = published(&rig);
    assert_eq!(slow.rung, file::ReliefRung::Slow);
    assert_eq!((slow.slowed, slow.would_slow), (5, 0));
    assert_eq!(slow.binding, Some(file::ReliefBinding::MemoryPressure));
    // Every active tick rewrites it, so its age is what says the writer lives.
    std::fs::remove_file(rig.dir.path().join(file::STATUS_FILE_NAME)).unwrap();
    rig.tick(&input(Level::Warn, 0), &table);
    assert_eq!(published(&rig).rung, file::ReliefRung::Slow);

    rig.ticks(SEVERE_SAMPLES as usize, rising(100), &table);
    let frozen = published(&rig);
    assert_eq!(frozen.rung, file::ReliefRung::Freeze);
    let tree = &frozen.frozen[0];
    assert_eq!((tree.pid, tree.agent_root_pid), (102, 100));
    assert!(!tree.advised_only && tree.since_ms > 0);
    assert_eq!(tree.terminal_session_id.as_deref(), Some("term-100"));
    assert_eq!(frozen.paused_count(), 1);
    assert_eq!(
        frozen.last_action.as_ref().map(|a| a.kind),
        Some(file::ReliefActionKind::Freeze)
    );

    rig.ticks(
        SEVERE_BREAK_SAMPLES as usize,
        |_| input(Level::Normal, 0),
        &table,
    );
    let rest = published(&rig);
    assert_eq!(
        rest.rung,
        file::ReliefRung::None,
        "the transition to rest is written"
    );
    assert_eq!((rest.slowed, rest.frozen.len(), rest.binding), (0, 0, None));
    assert!(rest.last_action.is_some());
    // At rest and unchanged, it is not rewritten every tick.
    std::fs::remove_file(rig.dir.path().join(file::STATUS_FILE_NAME)).unwrap();
    rig.tick(&input(Level::Normal, 0), &table);
    assert!(matches!(
        file::read_status(rig.dir.path()),
        file::ReliefOutcome::Absent
    ));
}

#[test]
fn advise_publishes_would_slow_and_advised_only_trees_and_slows_nothing() {
    let table = standard();
    let mut rig = publishing_rig(SupervisorMode::Advise);
    live_all(&rig, &table);
    rig.ticks(SEVERE_SAMPLES as usize + 5, rising(100), &table);
    let status = published(&rig);
    assert_eq!(status.mode, file::ReliefMode::Advise);
    assert_eq!((status.slowed, status.would_slow), (0, 5));
    assert_eq!(status.frozen.len(), 1);
    assert!(status.frozen[0].advised_only);
    assert_eq!(status.paused_count(), 0, "nothing is paused in advise");
}

#[test]
fn a_resume_request_resumes_an_older_pause_and_not_a_newer_one() {
    let table = two_sessions();
    let mut rig = publishing_rig(SupervisorMode::Act);
    live_all(&rig, &table);
    rig.ticks(SEVERE_SAMPLES as usize, rising(100), &table);
    let mut stops = rig.fake.stops();
    stops.sort_unstable();
    assert_eq!(stops, [202, 212]);
    // The session at 210 was paused ten seconds after the one at 200.
    let newer = rig
        .relief
        .frozen
        .iter()
        .position(|f| f.session.0 == 210)
        .unwrap();
    let older_since = rig.relief.frozen[1 - newer].info.since_unix_ms;
    rig.relief.frozen[newer].info.since_unix_ms = older_since + 10_000;
    let session = rig.relief.frozen[newer].info.session_id.clone();
    for entry in &mut rig.relief.journal {
        if entry.session_id == session {
            entry.at_unix_ms = older_since + 10_000;
        }
    }
    rig.relief.sync_journal().unwrap();
    // The person asked after the first pause and before the second.
    file::request_resume_in(rig.dir.path(), "banner", older_since + 5_000).unwrap();
    rig.tick(&severe(9000), &table);
    assert_eq!(rig.fake.conts(), [202], "the older pause, and only it");
    assert_eq!(rig.relief.status.frozen.len(), 1);
    assert_eq!(rig.relief.status.frozen[0].session_pid, 210);
    let note = rig.relief.status.last_action.clone().unwrap();
    assert_eq!(note.kind, ActionKind::Resume);
    assert!(note.detail.contains("banner"), "{note:?}");
    assert_eq!(published(&rig).paused_count(), 1);
    assert!(
        !rig.dir.path().join(file::RESUME_REQUEST_FILE_NAME).exists(),
        "a request is taken once"
    );
    // The episode is held off: nothing is frozen again meanwhile.
    rig.ticks(100, |i| severe(9000 + 100 * i as u64), &table);
    assert_eq!(rig.fake.stops().len(), 2);
}

#[test]
fn an_expired_resume_request_resumes_nothing() {
    let table = standard();
    let mut rig = publishing_rig(SupervisorMode::Act);
    live_all(&rig, &table);
    rig.ticks(SEVERE_SAMPLES as usize, rising(100), &table);
    let old = file::now_ms() - file::REQUEST_TTL_MS - 5_000;
    file::request_resume_in(rig.dir.path(), "too late", old).unwrap();
    rig.tick(&severe(9000), &table);
    assert!(rig.fake.conts().is_empty(), "{:?}", rig.fake.conts());
    assert_eq!(rig.relief.status.frozen.len(), 1);
    assert!(
        !rig.dir.path().join(file::RESUME_REQUEST_FILE_NAME).exists(),
        "and it is dropped, not kept for later"
    );
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
                was_running: true,
            }],
        )
        .unwrap();
        // "A fresh process": nothing of the freezing relief survives but the file.
        let replay = replay_journal(dir.path(), dir.path(), &OsActuator);
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
                was_running: true,
            }],
        )
        .unwrap();
        let replay = replay_journal(dir.path(), dir.path(), &OsActuator);
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
        rig.ticks(FLAT_SAMPLES, |_| severe(5000), &table);
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
            .any(|f| f.what.contains("recycled") || f.what.contains("different process")));
        assert!(rig.relief.status.frozen.is_empty());
    }

    #[cfg(any(target_os = "macos", target_os = "linux"))]
    #[test]
    fn the_os_actuator_reads_back_the_background_priority_it_sets() {
        let child = Sleeper::spawn();
        let (pid, start) = (child.pid(), child.start_us());
        assert_eq!(OsActuator.is_background(pid, start), Some(false));
        // Linux slows a process only when it could put it back: lowering nice
        // again needs the target's RLIMIT_NICE to cover nice 0 (20), or root.
        // A host without that (the CI runner's limit is 0) refuses, says why,
        // and leaves the process at normal priority, which is the answer to
        // check there.
        #[cfg(target_os = "linux")]
        {
            let mut limit = libc::rlimit {
                rlim_cur: 0,
                rlim_max: 0,
            };
            // SAFETY: reads this process's limit (the child inherits it) into a local.
            assert_eq!(unsafe { libc::getrlimit(libc::RLIMIT_NICE, &mut limit) }, 0);
            // SAFETY: `geteuid` has no preconditions.
            let privileged = unsafe { libc::geteuid() } == 0;
            if !privileged && limit.rlim_cur < 20 {
                let refused = OsActuator.set_background(pid, start, true).unwrap_err();
                assert!(refused.contains("RLIMIT_NICE"), "{refused}");
                assert_eq!(
                    OsActuator.is_background(pid, start),
                    Some(false),
                    "a refused slowing leaves the process at normal priority"
                );
                return;
            }
        }
        assert_eq!(OsActuator.set_background(pid, start, true), Ok(true));
        assert_eq!(OsActuator.is_background(pid, start), Some(true));
        assert_eq!(
            OsActuator.is_background(pid, start + 1),
            None,
            "a recycled pid reads as unknown, not as a priority"
        );
        assert_eq!(OsActuator.set_background(pid, start, false), Ok(true));
        assert_eq!(OsActuator.is_background(pid, start), Some(false));
        // A process that ended is refused as gone, not as a failure to restore.
        drop(child);
        let gone = OsActuator.set_background(pid, start, false).unwrap_err();
        assert!(process_is_gone(&gone), "{gone}");
    }

    #[cfg(any(target_os = "macos", target_os = "linux"))]
    #[test]
    fn the_os_actuator_reads_a_real_processs_stopped_state_as_ps_does() {
        let child = Sleeper::spawn();
        let (pid, start) = (child.pid(), child.start_us());
        assert_eq!(OsActuator.is_stopped(pid, start), Ok(false));
        assert!(!child.state().starts_with('T'), "{:?}", child.state());
        child.stop_by_hand();
        assert_eq!(OsActuator.is_stopped(pid, start), Ok(true));
        let recycled = OsActuator.is_stopped(pid, start + 1).unwrap_err();
        assert!(process_is_gone(&recycled), "{recycled}");
        assert_eq!(
            OsActuator.signal(pid, start, Signal::Cont),
            Ok(Delivery::Delivered)
        );
        assert!(child.wait_for(false));
        assert_eq!(OsActuator.is_stopped(pid, start), Ok(false));
        drop(child);
        let gone = OsActuator.is_stopped(pid, start).unwrap_err();
        assert!(process_is_gone(&gone), "{gone}");
        // Reserved pids are refused, not read as running.
        assert!(OsActuator.is_stopped(1, 1).is_err());
    }

    #[test]
    fn a_real_child_somebody_else_stopped_stays_stopped_after_the_relief_resumes_its_subtree() {
        let build = Sleeper::spawn();
        let job = Sleeper::spawn();
        job.stop_by_hand();
        let member = |child: &Sleeper, ppid: i32, mib: u64| ReliefProc {
            pid: child.pid(),
            ppid,
            start_us: child.start_us(),
            name: "sleep".into(),
            footprint: Some(mib * MIB),
            exempt: false,
        };
        let table = ReliefTable {
            sessions: vec![ReliefSession {
                members: vec![
                    proc(8_200_101, 8_200_100, "zsh", 10),
                    member(&build, 8_200_101, 900),
                    member(&job, build.pid(), 100),
                ],
                ..session(8_200_100, true, Vec::new())
            }],
        };
        let mut rig = Rig::new(SupervisorMode::Act);
        rig.relief.actuator = Box::new(OsActuator);
        rig.ticks(SEVERE_SAMPLES as usize, rising(100), &table);
        assert!(build.wait_for(true), "state {:?}", build.state());
        assert_eq!(rig.relief.status.rung, Rung::Freeze);
        rig.ticks(FLAT_SAMPLES, |_| severe(5000), &table);
        assert!(build.wait_for(false), "state {:?}", build.state());
        assert!(
            job.state().starts_with('T'),
            "the job its owner stopped was resumed: {:?}",
            job.state()
        );
        // The same for the watchdog's path and a person's request.
        let again = Sleeper::spawn();
        let again_job = Sleeper::spawn();
        again_job.stop_by_hand();
        let table = ReliefTable {
            sessions: vec![ReliefSession {
                members: vec![
                    proc(8_300_101, 8_300_100, "zsh", 10),
                    member(&again, 8_300_101, 900),
                    member(&again_job, again.pid(), 100),
                ],
                ..session(8_300_100, true, Vec::new())
            }],
        };
        let mut rig = Rig::new(SupervisorMode::Act);
        rig.relief.actuator = Box::new(OsActuator);
        rig.ticks(SEVERE_SAMPLES as usize, rising(100), &table);
        assert!(again.wait_for(true), "state {:?}", again.state());
        let sent = rig
            .relief
            .watch()
            .pass(&OsActuator, rig.now + WATCHDOG_STALL + STEP);
        assert_eq!(sent.len(), 1, "{sent:?}");
        assert!(again.wait_for(false), "state {:?}", again.state());
        assert!(
            again_job.state().starts_with('T'),
            "{:?}",
            again_job.state()
        );
    }

    /// The identification of an agent's tool shell reads `/bin/zsh`'s own
    /// arguments, and `/bin/zsh` is an Apple platform binary: macOS hides the
    /// *environment* of those, and this test is what shows it does not hide
    /// the arguments. Elsewhere the shell is whatever the host has (the
    /// Ubuntu runner has no zsh); the identification reads only the argument
    /// mark, never the shell's name.
    #[test]
    fn a_shell_started_with_a_tool_snapshot_is_identified_through_real_argv() {
        let shell = if cfg!(target_os = "macos") {
            "/bin/zsh"
        } else {
            "/bin/sh"
        };
        let spawn = |script: &str| {
            Sleeper(
                std::process::Command::new(shell)
                    .args(["-c", script])
                    .spawn_locked()
                    .unwrap_or_else(|error| panic!("{shell}: {error}")),
            )
        };
        let tool = spawn("sleep 60; echo # source /x/.claude/shell-snapshots/snapshot-zsh-1-a.sh");
        let plain = spawn("sleep 60; echo # a login shell, no snapshot");
        let probe = OsProbe {
            config: tempfile::tempdir().unwrap().path().to_path_buf(),
        };
        let as_proc = |child: &Sleeper| ReliefProc {
            pid: child.pid(),
            ppid: 1,
            start_us: child.start_us(),
            name: "zsh".into(),
            footprint: None,
            exempt: false,
        };
        // zsh may take a moment to exec its `sleep` child; its own argv is what
        // is read, and it is there from the start.
        let argv = probe
            .argv(tool.pid())
            .expect("zsh's arguments are readable");
        assert!(
            argv.iter().any(|a| a.contains(TOOL_SHELL_MARK)),
            "argv: {argv:?}"
        );
        let mut marks = MarkCache::default();
        assert_eq!(marks.tool_shell(&probe, &as_proc(&tool)), Some(true));
        assert_eq!(marks.tool_shell(&probe, &as_proc(&plain)), Some(false));
        let gone = ReliefProc {
            pid: 2_000_000_000,
            ..as_proc(&plain)
        };
        assert_eq!(
            marks.tool_shell(&probe, &gone),
            None,
            "unreadable is not 'not a tool shell'"
        );
    }

    /// A read-only measurement, not a test: what one priority pass costs per
    /// process on this host, the way it was (a full process probe, an
    /// argument buffer of `KERN_ARGMAX` bytes) and the way it is (who the
    /// process is, no footprint, no I/O counters, no argument block unless the
    /// owner is us). Run it with
    /// `cargo test --lib relief_tests::real_signals::measure -- --ignored --nocapture`.
    #[cfg(target_os = "macos")]
    #[test]
    #[ignore = "a read-only measurement of this host, run on demand"]
    fn measure_the_per_process_cost_of_a_priority_pass() {
        /// `PROC_ALL_PIDS` of `<libproc.h>`, which `libc` does not export.
        const PROC_ALL_PIDS: u32 = 1;
        let mut pids = vec![0i32; 8192];
        // SAFETY: the buffer is `pids.len() * 4` bytes, as stated.
        let bytes = unsafe {
            libc::proc_listpids(
                PROC_ALL_PIDS,
                0,
                pids.as_mut_ptr().cast(),
                i32::try_from(pids.len() * 4).unwrap(),
            )
        };
        pids.truncate(usize::try_from(bytes).unwrap() / 4);
        let live: Vec<(i32, u64)> = pids
            .into_iter()
            .filter(|p| *p > 1)
            .filter_map(|p| {
                crate::sysload::process_start_us(u32::try_from(p).ok()?).map(|s| (p, s))
            })
            .take(2048)
            .collect();
        let pass = |f: &dyn Fn(i32, u64)| {
            let began = Instant::now();
            for &(pid, start) in &live {
                f(pid, start);
            }
            began.elapsed().as_secs_f64() * 1000.0
        };
        // Interleaved, so a load spike lands on both arms alike; the minimum is
        // the cost, the median says how noisy the host was.
        let (mut before, mut after) = (Vec::new(), Vec::new());
        for _ in 0..25 {
            before.push(pass(&|pid, _| {
                let _ = crate::sysload::procscan::probe_one(pid);
                let _ = crate::sysload::procscan::darwin_task_priority(pid);
            }));
            after.push(pass(&|pid, start| {
                let _ = crate::sysload::procscan::is_background(pid, start);
            }));
        }
        let summarize = |v: &mut Vec<f64>| {
            v.sort_by(f64::total_cmp);
            (v[0], v[v.len() / 2])
        };
        let (b, a) = (summarize(&mut before), summarize(&mut after));
        println!(
            "priority pass over {} processes, 25 interleaved rounds: before min {:.1} ms / median {:.1} ms, after min {:.1} ms / median {:.1} ms",
            live.len(),
            b.0,
            b.1,
            a.0,
            a.1
        );
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
        was_running: true,
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

// ---------------------------------------------------------------------------
// Second review: a host pinned by something unrelated, a freeze the watchdog
// ended, and a process somebody else stopped.
// ---------------------------------------------------------------------------

/// Kernel Critical with swap climbing `rate` MiB per sample: pinned *and*
/// worsening.
fn critical_rising(rate: u64) -> impl Fn(usize) -> GovernorInput {
    move |i| {
        let mut input = GovernorInput::test_pressure(Level::Critical);
        input.readings.swap_used = Reading::Value((2048 + rate * i as u64) * MIB);
        input
    }
}

#[test]
fn a_pinned_host_may_begin_one_freeze_per_severe_episode_and_then_needs_a_fresh_rise() {
    let mut trigger = Trigger::default();
    let mut now = Instant::now();
    let mut begins = Vec::new();
    // Pinned and flat for 700 samples (23 minutes: two caps), then worsening.
    for i in 0..1200usize {
        now += STEP;
        let swap = 2048 + 100 * i.saturating_sub(700) as u64;
        let verdict = trigger.observe(now, &critical_sample(swap));
        if verdict.freeze == Step::Begin {
            trigger.began(now, true);
            begins.push(i);
        }
    }
    assert_eq!(begins[0], SEVERE_SAMPLES as usize - 1, "the first freeze");
    assert!(
        begins.len() >= 2 && begins[1] >= 700,
        "after the cap a flat, pinned host is not frozen again until swap rises: {begins:?}"
    );
}

#[test]
fn a_host_pinned_by_something_unrelated_does_not_refreeze_the_same_build_after_every_cap() {
    let table = standard();
    let mut rig = Rig::new(SupervisorMode::Act);
    live_all(&rig, &table);
    rig.ticks(SEVERE_SAMPLES as usize, critical_flat, &table);
    assert_eq!(rig.fake.stops(), [102, 103]);
    // Through the ten-minute cap.
    let cap_ticks = (FREEZE_CAP.as_secs() / STEP.as_secs()) as usize + 1;
    rig.ticks(cap_ticks, critical_flat, &table);
    assert_eq!(rig.fake.conts(), [103, 102], "the cap resumed it");
    assert_eq!(rig.relief.status.cap_hits, 1);
    // Twenty more minutes pinned and flat: it was not the cause, and it is not
    // frozen again.
    rig.ticks(600, critical_flat, &table);
    assert_eq!(rig.fake.stops(), [102, 103], "{:?}", rig.fake.calls());
    assert!(rig.relief.status.frozen.is_empty());
    // A fresh rise is a new reason, but the same build is never frozen twice
    // in one episode: nothing else qualifies, so nothing is.
    rig.ticks(80, critical_rising(200), &table);
    assert_eq!(rig.fake.stops(), [102, 103], "{:?}", rig.fake.calls());
    assert_eq!(
        rig.relief.status.last_action.as_ref().map(|a| a.kind),
        Some(ActionKind::Refused)
    );
}

#[test]
fn a_pinned_run_survives_one_or_two_unpinned_samples_but_not_three() {
    // Kernel Critical flickers: a sample or two below the edge is not a host
    // that recovered.
    let out = trigger_run(
        |i| {
            if i == 29 {
                severe_sample(2048, 10.0)
            } else {
                critical_sample(2048)
            }
        },
        60,
    );
    let first = out.iter().find(|(_, v)| v.freeze == Step::Begin).unwrap();
    assert_eq!(
        first.0, SEVERE_SAMPLES as usize,
        "one flicker costs nothing"
    );
    let out = trigger_run(
        |i| {
            if (29..32).contains(&i) {
                severe_sample(2048, 10.0)
            } else {
                critical_sample(2048)
            }
        },
        90,
    );
    let first = out.iter().find(|(_, v)| v.freeze == Step::Begin).unwrap();
    assert!(
        first.0 > SEVERE_SAMPLES as usize + 20,
        "three in a row starts the count again: {}",
        first.0
    );
}

#[test]
fn a_freeze_the_watchdog_continued_is_forgotten_and_its_late_children_are_not_stopped() {
    let table = standard();
    let mut rig = Rig::new(SupervisorMode::Act);
    live_all(&rig, &table);
    rig.ticks(SEVERE_SAMPLES as usize, rising(100), &table);
    assert_eq!(rig.fake.stops(), [102, 103]);
    // The tick stalled past the watchdog's limit and the watchdog continued
    // the builds from its own copy.
    let watch = rig.relief.watch();
    let sent = watch.pass(&rig.fake, rig.now + WATCHDOG_STALL + STEP);
    assert_eq!(sent.len(), 2);
    assert_eq!(rig.fake.conts(), [103, 102]);
    // The ticks resume, and the running build has grown a child.
    let mut grown = table.clone();
    grown.sessions[0].members.push(proc(120, 103, "cc1", 50));
    live_all(&rig, &grown);
    rig.tick(&severe(9000), &grown);
    assert_eq!(
        rig.fake.stops(),
        [102, 103],
        "a late child of a running build is not stopped"
    );
    assert!(rig.relief.frozen.is_empty());
    assert_ne!(rig.relief.status.rung, Rung::Freeze);
    assert!(rig.relief.status.frozen.is_empty());
    // (The slowed processes of rung A stay listed; the freeze does not.)
    assert!(read_journal(rig.dir.path())
        .unwrap()
        .is_none_or(|j| j.entries.iter().all(|e| e.rung != JournalRung::Freeze)));
    let note = rig.relief.status.last_action.clone().unwrap();
    assert_eq!(note.kind, ActionKind::Resume);
    assert!(note.detail.contains("watchdog"), "{note:?}");
    // And rung B holds off, as after a cap: a whole new Severe run first.
    rig.ticks(10, rising(100), &grown);
    assert_eq!(rig.fake.stops(), [102, 103]);
}

/// The relief's subtree is cargo (102) over rustc (103), and somebody stopped
/// rustc by hand (a Ctrl-Z job) before the freeze.
fn frozen_with_a_hand_stopped_child() -> Rig {
    let table = standard();
    let mut rig = Rig::new(SupervisorMode::Act);
    live_all(&rig, &table);
    rig.fake.stop_by_hand(103);
    rig.ticks(SEVERE_SAMPLES as usize, rising(100), &table);
    assert_eq!(
        rig.fake.stops(),
        [102],
        "rustc was already stopped: not ours to stop"
    );
    assert_eq!(rig.relief.status.frozen.len(), 1);
    rig
}

#[test]
fn a_process_somebody_else_stopped_is_not_resumed_when_the_relief_calms() {
    let mut rig = frozen_with_a_hand_stopped_child();
    let table = standard();
    rig.ticks(
        SEVERE_BREAK_SAMPLES as usize,
        |_| input(Level::Normal, 0),
        &table,
    );
    assert_eq!(rig.fake.conts(), [102]);
    assert!(rig.fake.is_stopped_now(103), "still stopped by its owner");
}

#[test]
fn a_process_somebody_else_stopped_is_not_resumed_by_a_persons_request() {
    let rig = frozen_with_a_hand_stopped_child();
    let out = resume_all_in(rig.dir.path(), &rig.fake, true, "banner");
    assert_eq!(out.results.iter().map(|r| r.pid).collect::<Vec<_>>(), [102]);
    assert_eq!(rig.fake.conts(), [102]);
    assert!(rig.fake.is_stopped_now(103));
}

#[test]
fn a_process_somebody_else_stopped_is_not_resumed_by_the_watchdog_or_the_shutdown() {
    let rig = frozen_with_a_hand_stopped_child();
    let watch = rig.relief.watch();
    let sent = watch.pass(&rig.fake, rig.now + WATCHDOG_STALL + STEP);
    assert_eq!(sent, [(102, 1_000_102)]);
    assert!(rig.fake.is_stopped_now(103));
    // The journal replay at shutdown (the lock is free) is the other path.
    assert!(shutdown_within(
        rig.dir.path(),
        &rig.ns(),
        Some(&*watch),
        &rig.fake,
        Duration::from_millis(150),
    ));
    assert!(rig.fake.is_stopped_now(103), "{:?}", rig.fake.calls());
    assert!(!rig.fake.conts().contains(&103));
}

#[test]
fn a_subtree_whose_root_somebody_else_stopped_is_not_frozen_at_all() {
    let table = standard();
    let mut rig = Rig::new(SupervisorMode::Act);
    live_all(&rig, &table);
    rig.fake.stop_by_hand(102);
    rig.ticks(SEVERE_SAMPLES as usize, rising(100), &table);
    assert!(rig.fake.stops().is_empty(), "{:?}", rig.fake.calls());
    assert!(rig.relief.status.frozen.is_empty());
    rig.ticks(
        SEVERE_BREAK_SAMPLES as usize,
        |_| input(Level::Normal, 0),
        &table,
    );
    assert!(rig.fake.conts().is_empty(), "{:?}", rig.fake.conts());
    assert!(rig.fake.is_stopped_now(102));
}

#[test]
fn a_journal_entry_that_was_not_verified_running_is_never_resumed() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(dir.path().join(JOURNAL_DIR)).unwrap();
    // An entry with no `was_running`: written by something that did not read
    // the process's state before it stopped it.
    let body = format!(
        r#"{{"version":1,"writer_pid":{},"writer_start_us":0,"entries":[{{"pid":301,"start_us":77,"session_id":"1-1","at_unix_ms":1,"rung":"freeze"}}]}}"#,
        std::process::id()
    );
    std::fs::write(journal_path(dir.path()), body).unwrap();
    let fake = Fake::default();
    let replay = replay_journal(dir.path(), dir.path(), &fake);
    assert_eq!(replay.resumed, 0, "{replay:?}");
    assert!(fake.conts().is_empty(), "{:?}", fake.conts());
}

#[test]
fn the_watchdogs_copy_and_the_journal_hold_only_what_the_relief_really_stopped() {
    let rig = frozen_with_a_hand_stopped_child();
    let watched: Vec<i32> = lock(&rig.relief.watch.state)
        .frozen
        .iter()
        .map(|w| w.pid)
        .collect();
    assert_eq!(watched, [102], "rustc (103) was somebody else's");
    let journal = read_journal(rig.dir.path()).unwrap().unwrap();
    let freezes: Vec<(i32, bool)> = journal
        .entries
        .iter()
        .filter(|e| e.rung == JournalRung::Freeze)
        .map(|e| (e.pid, e.was_running))
        .collect();
    assert_eq!(freezes, [(102, true)]);
}

#[test]
fn a_late_child_is_not_stopped_when_the_subtrees_root_was_continued_by_someone_else() {
    let table = standard();
    let mut rig = Rig::new(SupervisorMode::Act);
    live_all(&rig, &table);
    rig.ticks(SEVERE_SAMPLES as usize, rising(100), &table);
    assert_eq!(rig.fake.stops(), [102, 103]);
    // A person (or a debugger) continues the cargo behind the relief's back.
    rig.fake
        .signal(102, 1_000_102, Signal::Cont)
        .expect("a live process");
    let mut grown = table.clone();
    grown.sessions[0].members.push(proc(120, 103, "cc1", 50));
    live_all(&rig, &grown);
    rig.tick(&severe(9000), &grown);
    assert_eq!(rig.fake.stops(), [102, 103], "{:?}", rig.fake.calls());
    assert!(rig.relief.frozen.is_empty());
    let note = rig.relief.status.last_action.clone().unwrap();
    assert!(note.detail.contains("something else"), "{note:?}");
}

#[test]
fn a_resume_request_that_finds_the_action_lock_busy_answers_busy_and_is_retried() {
    let table = standard();
    let mut rig = publishing_rig(SupervisorMode::Act);
    live_all(&rig, &table);
    rig.ticks(SEVERE_SAMPLES as usize, rising(100), &table);
    rig.relief.act_wait = Duration::from_millis(20);
    // The call itself: it gives up, says so, and signals nothing.
    let held = lock(&ACT);
    let out = resume_up_to_within(
        rig.dir.path(),
        &rig.fake,
        true,
        "banner",
        None,
        Duration::from_millis(20),
    );
    assert!(out.busy && out.refused.is_some(), "{out:?}");
    assert!(out.results.is_empty());
    assert!(rig.fake.conts().is_empty());
    // The consumer keeps the request it already took from its file.
    file::request_resume_in(rig.dir.path(), "banner", file::now_ms()).unwrap();
    rig.relief.take_resume_request();
    assert!(rig.relief.pending_request.is_some());
    assert!(!rig.dir.path().join(file::RESUME_REQUEST_FILE_NAME).exists());
    assert!(rig.fake.conts().is_empty());
    drop(held);
    // The next tick's attempt carries it out.
    rig.relief.take_resume_request();
    assert!(rig.relief.pending_request.is_none());
    assert_eq!(rig.fake.conts(), [103, 102]);
}

#[test]
fn a_watchdog_thread_that_did_not_start_is_a_failure_in_the_status_once() {
    let mut rig = Rig::new(SupervisorMode::Act);
    lock(&rig.relief.watch.state).start_error = Some("thread limit reached".into());
    let table = standard();
    rig.tick(&input(Level::Normal, 0), &table);
    rig.tick(&input(Level::Normal, 0), &table);
    let failures: Vec<_> = rig
        .relief
        .status
        .failures
        .iter()
        .filter(|f| f.what.contains("watchdog thread did not start"))
        .collect();
    assert_eq!(failures.len(), 1, "{:?}", rig.relief.status.failures);
    assert!(failures[0].what.contains("thread limit reached"));
}

#[test]
fn replay_also_puts_back_the_children_that_inherited_a_slowed_processs_priority() {
    let dir = tempfile::tempdir().unwrap();
    let slowed_at_ms = 1_000;
    write_journal(
        dir.path(),
        &[JournalEntry {
            pid: 102,
            start_us: 1_000_102,
            session_id: "100-1".into(),
            at_unix_ms: slowed_at_ms,
            rung: JournalRung::Slow,
            was_running: true,
        }],
    )
    .unwrap();
    let fake = Fake::default();
    // 120 was born after the slowing and inherited the priority; 121 was born
    // before it; 122 is the steward's, and 123 is under that.
    *fake.table.lock().unwrap() = vec![
        (102, 101, 1_000_102),
        (120, 102, 5_000_000),
        (121, 102, 500_000),
        (122, 102, 5_000_122),
        (123, 122, 5_000_123),
    ];
    for pid in [102, 120, 121, 123] {
        fake.background.lock().unwrap().insert(pid);
    }
    assert_eq!(
        steward_throttle_with(&fake, dir.path(), 122, 5_000_122, true),
        Ok(true)
    );
    let replay = replay_journal(dir.path(), dir.path(), &fake);
    let mut restored = fake.restored();
    restored.sort_unstable();
    assert_eq!(restored, [102, 120], "{replay:?}");
    assert_eq!(replay.restored, 2);
    let background = fake.background.lock().unwrap();
    assert!(background.contains(&121) && background.contains(&122) && background.contains(&123));
}

#[test]
fn a_late_joiner_that_holds_a_lease_slot_makes_its_subtree_a_holder() {
    let table = standard();
    let mut rig = Rig::new(SupervisorMode::Act);
    live_all(&rig, &table);
    rig.refuses = true;
    rig.ticks(SEVERE_SAMPLES as usize, rising(100), &table);
    assert_eq!(rig.fake.stops(), [102, 103]);
    assert!(!rig.relief.frozen[0].holder, "no build of the lease's yet");
    let mut grown = table.clone();
    grown.sessions[0].members.push(proc(120, 103, "cc1", 50));
    live_all(&rig, &grown);
    rig.probe
        .env
        .lock()
        .unwrap()
        .insert(120, vec!["GITPULSE_LEASE_HELD=1".into()]);
    rig.tick(&severe(9000), &grown);
    assert_eq!(
        rig.fake.stops(),
        [102, 103, 120],
        "a holder, while refusing"
    );
    // The board stops refusing: the subtree holds a slot now, so it goes on.
    rig.refuses = false;
    rig.tick(&severe(9100), &grown);
    assert!(
        rig.relief.frozen.is_empty(),
        "{:?}",
        rig.relief.status.frozen
    );
    assert_eq!(rig.fake.conts(), [120, 103, 102]);
}

#[test]
fn putting_back_leaves_alone_what_was_born_before_the_slowing_and_what_is_under_the_stewards() {
    let mut table = standard();
    // An exempt process under the cargo: never slowed by name, and in the
    // background on its own account.
    table.sessions[0].members.push(ReliefProc {
        exempt: true,
        ..proc(124, 102, "cc1", 50)
    });
    let mut rig = Rig::new(SupervisorMode::Act);
    live_all(&rig, &table);
    rig.fake.background.lock().unwrap().insert(124);
    rig.ticks(5, |_| input(Level::Warn, 0), &table);
    assert_eq!(rig.relief.status.slowed, 5);
    // Born after the pass, so they inherited the priority: 120 and 121 under
    // the relief's own, 122 the steward's, 123 under that.
    let mut grown = table.clone();
    for (pid, ppid) in [(120, 102), (121, 103), (122, 102), (123, 122)] {
        grown.sessions[0].members.push(ReliefProc {
            start_us: 9_000_000 + pid as u64,
            ..proc(pid, ppid, "cc1", 50)
        });
    }
    live_all(&rig, &grown);
    for pid in [120, 121, 123] {
        rig.fake.background.lock().unwrap().insert(pid);
    }
    assert_eq!(
        steward_throttle_with(&rig.fake, &rig.ns(), 122, 9_000_122, true),
        Ok(true)
    );
    rig.ticks(
        EXIT_SAMPLES as usize - 1,
        |_| input(Level::Normal, 0),
        &table,
    );
    rig.tick(&input(Level::Normal, 0), &grown);
    let restored = rig.fake.restored();
    assert!(
        restored.contains(&120) && restored.contains(&121),
        "{restored:?}"
    );
    assert!(!restored.contains(&122), "the steward's own");
    assert!(!restored.contains(&123), "under the steward's process");
    assert!(
        !restored.contains(&124),
        "born before the slowing, and in the background on its own account"
    );
}

#[path = "relief_tests/review.rs"]
mod review;
#[path = "relief_tests/rung_c.rs"]
mod rung_c;
