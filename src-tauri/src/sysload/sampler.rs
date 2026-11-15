//! The sampling thread: one tick every [`CADENCE_MS`], in the app process.
//!
//! Each tick elects (or keeps) the writer, samples the host and the process
//! table, publishes `host-load.v1`, and leaves a status for Health and a
//! pulse for the frontend. The cadence is fixed: a tick that runs long is an
//! overrun, a slot the thread woke too late for is missed, and both are
//! published rather than absorbed by stretching the interval.
//!
//! The thread asks for the user-initiated QoS class for its whole life: it is
//! the sensor the governor fails closed on and must not be starved under the
//! pressure it measures. It holds an `NSProcessInfo` activity, so App Nap does
//! not coalesce its timer while the window is hidden, only while
//! `host_load.enabled` is true: the activity begins on the tick that sees the
//! switch on and ends on the tick that sees it off. Either request failing is
//! recorded in the status, not hidden.

use super::governor::{self, Governor, GovernorPulse, GovernorReport, Input};
use super::procscan::{ProcView, ScanContext, Scanner};
use super::sensor::{CompressorTrend, Level, OsSensor, Reading, Sensor, Why};
use super::snapshot::{self, PublishedVerdict, Snapshot, TickStats, Writer, WriterIdentity};
use super::trace::{self, PreviousBoot, TraceFeed, TraceRecord};
use super::{Clock, Platform, SensorReport, SensorState, CADENCE_MS};
use serde::Serialize;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant};

/// What the background gate (card 1c) consumes: a level when the sensor may
/// be acted on, otherwise unavailable — never a guessed "normal".
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum PressureInput {
    Normal,
    Warn,
    Critical,
    Unavailable,
}

impl PressureInput {
    fn of(report: &SensorReport) -> Self {
        let enabled = report.enabled.unwrap_or(false);
        match (report.state.governs() && enabled, report.level) {
            (true, Some(Level::Normal)) => Self::Normal,
            (true, Some(Level::Warn)) => Self::Warn,
            (true, Some(Level::Critical)) => Self::Critical,
            _ => Self::Unavailable,
        }
    }
}

/// This process's part in sampling.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Role {
    /// This process writes the snapshot.
    Writer,
    /// Another GitPulse holds the election; this one reads its snapshot.
    Reader,
    /// No sampler runs in this process (a CLI, the MCP server, a test).
    NotSampling,
    /// No sensor on this platform.
    Unsupported,
}

/// Everything Health shows about host load.
#[derive(Debug, Clone, Serialize)]
pub struct HostLoadStatus {
    pub platform: Platform,
    pub role: Role,
    pub sensor: SensorReport,
    pub pressure: PressureInput,
    /// What the governor decided from this sensor: admission, shedding and
    /// why (`sysload::governor`).
    pub governor: GovernorReport,
    pub snapshot_path: Option<String>,
    pub writer: Option<WriterIdentity>,
    pub writer_is_self: bool,
    pub snapshot: Option<Snapshot>,
    /// The detailed process view, present only where this process is the
    /// writer: another process's sessions are not ours to rescan per click.
    pub procs: Option<ProcView>,
    pub tick: TickStats,
    /// Requests the sampler made that the OS refused (QoS, activity,
    /// election, publish).
    pub problems: Vec<String>,
    /// The tail of the previous boot's trace (`host-load-trace.prev.v1`) and
    /// its peaks, read once when the sampler started, for "Before the last
    /// restart". `None` when there is none; a file that could not be trusted
    /// is a value that says so.
    pub previous_boot: Option<PreviousBoot>,
}

/// The small per-tick message the frontend listens for.
#[derive(Debug, Clone, Copy, PartialEq, Serialize)]
pub struct HostLoadPulse {
    pub state: SensorState,
    pub pressure: PressureInput,
    pub level: Option<Level>,
    pub age_ms: Option<u64>,
    pub enabled: bool,
    pub compressor_thrash: bool,
    pub role: Role,
    pub governor: GovernorPulse,
}

pub fn pulse_of(status: &HostLoadStatus) -> HostLoadPulse {
    HostLoadPulse {
        state: status.sensor.state,
        pressure: status.pressure,
        level: status.sensor.level,
        age_ms: status.sensor.age_ms,
        enabled: status.sensor.enabled.unwrap_or(false),
        compressor_thrash: status
            .snapshot
            .as_ref()
            .is_some_and(|s| s.compressor_thrash),
        role: status.role,
        governor: GovernorPulse::of(&status.governor),
    }
}

/// How to run one sampler.
pub struct SamplerOptions {
    /// The per-user config directory the snapshot and its lock live in.
    pub dir: PathBuf,
    pub cadence: Duration,
    /// `host_load.enabled`, read once per tick.
    pub enabled: Box<dyn Fn() -> bool + Send>,
    /// `host_load.govern_launches`, read once per tick: whether severe
    /// pressure refuses agent launches.
    pub govern_launches: Box<dyn Fn() -> bool + Send>,
    /// Each input's mode and thresholds, read once per tick like
    /// `govern_launches`.
    pub thresholds: Box<dyn Fn() -> super::Thresholds + Send>,
    /// Called after every tick with that tick's pulse.
    pub on_tick: Option<Box<dyn Fn(HostLoadPulse) + Send>>,
    /// Begins the App Nap opt-out; dropping what it returns ends it. A seam so
    /// the begin/end transitions can be observed without a real activity.
    pub begin_activity: Box<dyn Fn() -> Result<Box<dyn Send>, String> + Send>,
    /// Opens where the pre-crash trace is written, on the trace thread. A seam
    /// so a test can make the write block.
    pub trace_opener: trace::TraceOpener,
}

impl SamplerOptions {
    pub fn new(dir: PathBuf) -> Self {
        Self {
            dir,
            cadence: Duration::from_millis(CADENCE_MS),
            enabled: Box::new(|| true),
            govern_launches: Box::new(|| true),
            thresholds: Box::new(super::Thresholds::default),
            on_tick: None,
            begin_activity: Box::new(|| {
                super::probe::Activity::begin("GitPulse host-load sampler")
                    .map(|activity| Box::new(activity) as Box<dyn Send>)
            }),
            trace_opener: trace::file_opener(),
        }
    }
}

/// The App Nap opt-out, held exactly while the sensor is switched on.
#[derive(Default)]
struct ActivityHold {
    held: Option<Box<dyn Send>>,
    /// Whether the last tick saw the sensor on. The request is made on a
    /// change, not every tick, so a refusal is recorded once and not retried
    /// at the cadence.
    engaged: bool,
    failure: Option<String>,
}

impl ActivityHold {
    /// Follows `enabled`: begins the activity on the tick it turns on and ends
    /// it on the tick it turns off. Returns the standing problem, if the OS
    /// refused the last request.
    fn follow(
        &mut self,
        enabled: bool,
        begin: &dyn Fn() -> Result<Box<dyn Send>, String>,
    ) -> Option<&str> {
        if enabled != self.engaged {
            self.engaged = enabled;
            self.held = None;
            self.failure = None;
            if enabled {
                match begin() {
                    Ok(activity) => self.held = Some(activity),
                    Err(why) => self.failure = Some(format!("App Nap activity: {why}")),
                }
            }
        }
        self.failure.as_deref()
    }
}

struct Shared {
    dir: PathBuf,
    status: Mutex<Option<HostLoadStatus>>,
    /// Read once when the sampler starts; refreshed when the trace rotates.
    previous_boot: Mutex<Option<PreviousBoot>>,
    /// This sampler's governor. Only the app's is registered for the rest
    /// of the process to ask (`governor::report`).
    governor: Arc<Mutex<Governor>>,
    stop: AtomicBool,
}

/// A running sampler. Dropping it does not stop the thread; [`StopHandle::stop`]
/// does, and waits for it.
pub struct StopHandle {
    shared: Arc<Shared>,
    thread: Option<std::thread::JoinHandle<()>>,
}

impl StopHandle {
    pub fn stop(mut self) {
        self.shared.stop.store(true, Ordering::Release);
        if let Some(thread) = self.thread.take() {
            thread.thread().unpark();
            let _ = thread.join();
        }
    }

    /// The latest status this sampler left.
    pub fn status(&self) -> Option<HostLoadStatus> {
        self.shared
            .status
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone()
    }
}

/// The app's own sampler, whose status [`status`] serves.
static APP_SAMPLER: OnceLock<Arc<Shared>> = OnceLock::new();

/// Starts the app's sampler: the one [`status`] reports. Called once, from
/// `lib.rs::run`; a second call is refused rather than starting a second
/// writer candidate in the same process.
pub fn start_app(options: SamplerOptions) -> Result<StopHandle, String> {
    let handle = start(options)?;
    if APP_SAMPLER.set(handle.shared.clone()).is_err()
        || !governor::register_app(handle.shared.governor.clone())
    {
        handle.stop();
        return Err("the app's host-load sampler is already running".into());
    }
    Ok(handle)
}

/// Starts a sampler thread that [`status`] does not report: what the tests,
/// the soak and the benchmark run.
pub fn start(options: SamplerOptions) -> Result<StopHandle, String> {
    if Platform::current() == Platform::Unavailable {
        return Err("no host-load sensor on this platform".into());
    }
    let shared = Arc::new(Shared {
        dir: options.dir.clone(),
        status: Mutex::new(None),
        previous_boot: Mutex::new(None),
        governor: Arc::new(Mutex::new(Governor::default())),
        stop: AtomicBool::new(false),
    });
    let worker = shared.clone();
    // Restarted after a panic: the body rebuilds its sensor, scanner and
    // writer election from `options`, and a sampler that keeps panicking
    // stops visibly (Diagnostics) instead of leaving the last pulse standing.
    let thread = crate::supervisor::spawn_service_with(
        SUBSYSTEM,
        crate::supervisor::Policy::RESTART,
        move || run(&worker, &options),
    )
    .map_err(|e| format!("could not start the host-load sampler: {e}"))?;
    Ok(StopHandle {
        shared,
        thread: Some(thread),
    })
}

/// Host load as this process can see it: the app's sampler's latest status,
/// or — in a process that does not sample — what the snapshot says.
pub fn status() -> HostLoadStatus {
    if let Some(shared) = APP_SAMPLER.get() {
        if let Some(status) = shared
            .status
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone()
        {
            // Aged to now, not to the tick that stored it: a sampler that
            // stopped ticking must not leave a fresh-looking verdict.
            return HostLoadStatus {
                governor: governor::report(),
                previous_boot: shared
                    .previous_boot
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner)
                    .clone(),
                ..status
            };
        }
        return reader_status(&shared.dir, Role::Reader, Vec::new(), TickStats::default());
    }
    match crate::tool_config::default_config_dir() {
        Some(dir) => reader_status(&dir, Role::NotSampling, Vec::new(), TickStats::default()),
        None => HostLoadStatus {
            platform: Platform::current(),
            role: Role::NotSampling,
            sensor: SensorReport {
                state: SensorState::Dead,
                age_ms: None,
                level: None,
                enabled: None,
                reason: Some("no per-user config directory (HOME / APPDATA unset)".into()),
            },
            pressure: PressureInput::Unavailable,
            governor: governor::report(),
            snapshot_path: None,
            writer: None,
            writer_is_self: false,
            snapshot: None,
            procs: None,
            tick: TickStats::default(),
            problems: Vec::new(),
            previous_boot: None,
        },
    }
}

fn reader_status(dir: &Path, role: Role, problems: Vec<String>, tick: TickStats) -> HostLoadStatus {
    let path = dir.join(snapshot::SNAPSHOT_FILE_NAME);
    let (sensor, snap) = snapshot::report(&path, Clock::now());
    let role = if Platform::current() == Platform::Unavailable {
        Role::Unsupported
    } else {
        role
    };
    HostLoadStatus {
        platform: Platform::current(),
        role,
        pressure: PressureInput::of(&sensor),
        governor: governor::report(),
        sensor,
        snapshot_path: Some(path.to_string_lossy().into_owned()),
        writer: snap.as_ref().map(|s| s.writer.clone()),
        writer_is_self: false,
        snapshot: snap,
        procs: None,
        tick,
        problems,
        previous_boot: None,
    }
}

fn own_identity() -> WriterIdentity {
    let pid = std::process::id();
    WriterIdentity {
        pid,
        start_us: i32::try_from(pid)
            .ok()
            .and_then(super::procscan::probe_one)
            .map_or(0, |f| f.start_us),
        version: env!("CARGO_PKG_VERSION").to_string(),
        path: std::env::current_exe()
            .map(|p| p.to_string_lossy().into_owned())
            .unwrap_or_default(),
    }
}

/// Fills a snapshot's readings from one tick's host sample and scan.
fn fill(
    snap: &mut Snapshot,
    sensor: &mut dyn Sensor,
    trend: &mut CompressorTrend,
    view: &ProcView,
) {
    snap.host = sensor.sample();
    let (delta, thrash) = trend.observe(snap.host.compressor);
    snap.compressor_delta = delta;
    snap.compressor_thrash = thrash;
    if view.ok {
        snap.processes = Reading::Value(view.processes);
        snap.unreadable_processes = view.unreadable;
        snap.claude = Reading::Value(view.claude);
        snap.codex = Reading::Value(view.codex);
    }
    snap.launched_footprint = view.launched_footprint;
    snap.observed_footprint = view.observed_footprint;
    snap.largest_foreign = view.largest_foreign;
    snap.launched_write_rate = view.launched_write_rate;
    snap.max_tree_write_rate = view.max_tree_write_rate;
    snap.memguard_present = view.memguard.present;
    snap.memguard_warn = view.memguard.warn;
    snap.memguard_pid = view
        .memguard
        .pid
        .and_then(|p| u32::try_from(p).ok())
        .unwrap_or(0);
    snap.orphans = u32::try_from(view.orphans.len()).unwrap_or(u32::MAX);
}

/// What tells one published sample from another: the writer's boot, its
/// sequence number and when it wrote.
type SampleKey = (u64, u64, u64);

/// Whether `snapshot` is a sample the governor has not counted yet, noting it
/// in `last` when it is. A reader ticks on its own clock and rereads its
/// writer's last record until the next one lands; counting each reread as a
/// sample would let one reading from a stalled writer trip the hysteresis.
/// No snapshot is never a new sample: there is nothing to count.
fn new_sample(last: &mut Option<SampleKey>, snapshot: Option<&Snapshot>) -> bool {
    let Some(snapshot) = snapshot else {
        return false;
    };
    let key = (
        snapshot.written.boot_id,
        snapshot.seq,
        snapshot.written.mono_ns,
    );
    if *last == Some(key) {
        return false;
    }
    *last = Some(key);
    true
}

/// The supervised thread's name and its Diagnostics subsystem.
const SUBSYSTEM: &str = "host-load-sampler";

fn run(shared: &Shared, options: &SamplerOptions) {
    let mut problems = Vec::new();
    if let Err(why) = super::probe::set_user_initiated_qos() {
        problems.push(format!("QoS: {why}"));
    }
    let mut activity = ActivityHold::default();
    let mut sensor = OsSensor::new(&options.dir);
    let mut scanner = Scanner::os();
    let mut trend = CompressorTrend::default();
    let mut writer: Option<Writer> = None;
    let identity = own_identity();
    let cadence = options.cadence.max(Duration::from_millis(10));
    let origin = Instant::now();
    let cpu_origin = super::probe::thread_cpu_ns();
    let mut slot: u64 = 0;
    let mut tick = TickStats::default();
    let mut last_sample = None;
    // The governor's held verdict after the last tick's count, for the next
    // record to carry; pending until there has been a count.
    let mut held_verdict: Reading<PublishedVerdict> = Reading::Unavailable(Why::NotYet);
    // The pre-crash trace: opened only once this process holds the writer
    // election, so a reader process never appends.
    // The ring is written on its own thread; the tick only hands a record to a
    // one-slot feed, which drops and counts rather than wait.
    let mut trace_feed: Option<TraceFeed> = None;
    let mut trace_problem: Option<String> = None;
    let mut drops_seen: u64 = 0;
    let mut last_drop: Option<Instant> = None;
    *shared
        .previous_boot
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner) = trace::previous_boot(&shared.dir);

    while !shared.stop.load(Ordering::Acquire) {
        let began = Instant::now();
        let mut tick_problems = problems.clone();
        let enabled = (options.enabled)();
        tick_problems.extend(
            activity
                .follow(enabled, options.begin_activity.as_ref())
                .map(str::to_string),
        );
        if writer.is_none() {
            match Writer::elect(&shared.dir) {
                Ok(elected) => writer = elected,
                Err(why) => tick_problems.push(format!("election: {why}")),
            }
        }
        if writer.is_some() && trace_feed.is_none() {
            // The feed's thread opens the ring (which moves one written under
            // another boot to `host-load-trace.prev.v1`), so the open never
            // runs on this tick.
            match TraceFeed::start(
                options.trace_opener.clone(),
                shared.dir.clone(),
                Clock::now().boot_id,
            ) {
                Ok(started) => trace_feed = Some(started),
                Err(why) => trace_problem = Some(format!("trace: {why}")),
            }
        }
        if let Some(feed) = &trace_feed {
            // The previous boot is read once the thread has rotated the ring.
            if feed.take_opened() {
                *shared
                    .previous_boot
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner) =
                    trace::previous_boot(&shared.dir);
            }
            trace_problem = feed.problem();
            let dropped = feed.dropped();
            if dropped > drops_seen {
                drops_seen = dropped;
                last_drop = Some(Instant::now());
            }
        }
        tick_problems.extend(trace_problem.clone());
        if let Some(at) = last_drop.filter(|at| at.elapsed() < DROP_NOTICE) {
            tick_problems.push(format!(
                "trace: {drops_seen} reading(s) dropped while the trace write was blocked or failing (last {} s ago); the ring has a gap",
                at.elapsed().as_secs()
            ));
        }
        let status = match writer.as_mut() {
            Some(w) => {
                let mut snap = Snapshot::empty(Clock::now(), identity.clone(), enabled);
                let mut procs = None;
                if enabled {
                    let view = scanner.scan(&ScanContext::here());
                    fill(&mut snap, &mut sensor, &mut trend, &view);
                    procs = Some(view);
                } else {
                    // Off is not calm: the next run starts its thrash count
                    // from nothing rather than from samples hours old.
                    trend = CompressorTrend::default();
                }
                snap.tick = tick;
                // The verdict the previous tick's count produced (pending on
                // the first): the record is published before this tick's
                // sample is counted.
                snap.verdict = if enabled { held_verdict } else { Reading::default() };
                if let Err(why) = w.publish(&snap) {
                    tick_problems.push(format!("publish: {why}"));
                }
                let mut status = reader_status(&shared.dir, Role::Writer, tick_problems, tick);
                status.writer_is_self = true;
                status.snapshot_path = Some(w.path().to_string_lossy().into_owned());
                status.procs = procs;
                status
            }
            None => reader_status(&shared.dir, Role::Reader, tick_problems, tick),
        };

        let govern_launches = (options.govern_launches)();
        let thresholds = (options.thresholds)();
        let mut status = status;
        {
            let mut governor = shared
                .governor
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            let input = Input::from_report(
                &status.sensor,
                status.snapshot.as_ref(),
                govern_launches,
                thresholds,
            );
            let observation = if new_sample(&mut last_sample, status.snapshot.as_ref()) {
                governor.observe(Instant::now(), input)
            } else {
                governor.observe_repeat(Instant::now(), input)
            };
            if observation.notice_raised {
                log::warn!(
                    target: "host_load",
                    "refusing agent launches for {} min under host load: {}",
                    governor::REFUSAL_NOTICE_AFTER.as_secs() / 60,
                    observation.verdict.message
                );
            }
            status.governor = governor.report(Instant::now());
            held_verdict = Reading::Value(PublishedVerdict::of(&status.governor.verdict));
        }

        // One record every fifth tick (10 s), from the elected writer only.
        // A write that fails is a problem in Health, not a silent gap.
        if trace::due(writer.is_some(), enabled, tick.ticks + 1) {
            if let (Some(feed), Some(snapshot)) = (trace_feed.as_ref(), status.snapshot.as_ref()) {
                let verdict = &status.governor.verdict;
                // Dropped when the thread is still writing the last one; the
                // feed counts it and the next record that lands says so.
                feed.offer(TraceRecord::of(
                    snapshot,
                    verdict.tier,
                    verdict.binding,
                    verdict.admit,
                ));
            }
        }

        let took = began.elapsed();
        tick.ticks += 1;
        tick.last_us = u32::try_from(took.as_micros()).unwrap_or(u32::MAX);
        tick.max_us = tick.max_us.max(tick.last_us);
        if took > cadence {
            tick.overruns += 1;
        }
        if let (Some(origin), Some(now)) = (cpu_origin, super::probe::thread_cpu_ns()) {
            tick.cpu_ns = now.saturating_sub(origin);
        }
        let pulse = pulse_of(&status);
        *shared
            .status
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) =
            Some(HostLoadStatus { tick, ..status });
        if let Some(on_tick) = &options.on_tick {
            on_tick(pulse);
        }
        // A restarted sampler is healthy again once a tick completes; the
        // supervisor clears its record only when the body returns.
        crate::supervisor::note_recovered(SUBSYSTEM);

        // The next slot on the fixed grid. Waking later than one whole
        // cadence past it means slots went by with no tick: those are missed.
        slot += 1;
        let mut next = origin + cadence * u32::try_from(slot).unwrap_or(u32::MAX);
        let now = Instant::now();
        if now > next {
            let behind = (now - next).as_nanos() / cadence.as_nanos().max(1);
            let behind = u64::try_from(behind).unwrap_or(u64::MAX);
            tick.missed = tick.missed.saturating_add(behind);
            slot = slot.saturating_add(behind);
            next = origin + cadence * u32::try_from(slot).unwrap_or(u32::MAX);
        }
        while !shared.stop.load(Ordering::Acquire) {
            let now = Instant::now();
            if now >= next {
                break;
            }
            std::thread::park_timeout(next - now);
        }
    }
    // Stopped: close the feed. The thread writes a record still in its slot;
    // a write blocked in the kernel is detached after `STOP_WAIT` rather than
    // held up, so stopping the sampler stays bounded.
    if let Some(mut feed) = trace_feed.take() {
        feed.stop(trace::STOP_WAIT);
    }
}

/// How long a dropped reading stays a line in the status after the last one.
const DROP_NOTICE: Duration = Duration::from_secs(60);

#[cfg(test)]
mod tests {
    use super::*;

    /// A reader rereads its writer's last record until the next one lands:
    /// the reread is not a new sample, a new sequence or a new boot is.
    #[test]
    fn a_reread_record_is_not_a_new_sample() {
        let clock = |mono_ns, boot_id| Clock {
            wall_ms: 1_700_000_000_000,
            mono_ns,
            boot_id,
        };
        let record = |seq, mono_ns, boot_id| {
            let mut snap = Snapshot::empty(clock(mono_ns, boot_id), own_identity(), true);
            snap.seq = seq;
            snap
        };
        let mut last = None;
        assert!(!new_sample(&mut last, None), "no record, nothing to count");
        assert!(new_sample(&mut last, Some(&record(7, 100, 1))));
        assert!(!new_sample(&mut last, Some(&record(7, 100, 1))), "a reread");
        assert!(!new_sample(&mut last, Some(&record(7, 100, 1))), "a reread");
        assert!(new_sample(&mut last, Some(&record(8, 200, 1))));
        // A new writer that restarted its sequence, or a new boot.
        assert!(new_sample(&mut last, Some(&record(8, 300, 1))));
        assert!(new_sample(&mut last, Some(&record(8, 300, 2))));
    }

    fn report(state: SensorState, level: Option<Level>, enabled: bool) -> SensorReport {
        SensorReport {
            state,
            age_ms: Some(0),
            level,
            enabled: Some(enabled),
            reason: None,
        }
    }

    #[test]
    fn the_pressure_input_is_a_level_only_when_the_sensor_may_be_acted_on() {
        use PressureInput as P;
        for (state, level, enabled, want) in [
            (SensorState::Fresh, Some(Level::Critical), true, P::Critical),
            (SensorState::StaleRecent, Some(Level::Warn), true, P::Warn),
            (SensorState::Fresh, Some(Level::Normal), true, P::Normal),
            (
                SensorState::StaleOld,
                Some(Level::Critical),
                true,
                P::Unavailable,
            ),
            (
                SensorState::Dead,
                Some(Level::Critical),
                true,
                P::Unavailable,
            ),
            (SensorState::Fresh, None, true, P::Unavailable),
            // The kill switch off: nothing to govern on.
            (
                SensorState::Fresh,
                Some(Level::Critical),
                false,
                P::Unavailable,
            ),
        ] {
            assert_eq!(
                P::of(&report(state, level, enabled)),
                want,
                "{state:?} {level:?}"
            );
        }
    }

    /// Real: a sampler on this host, at a short cadence, in a temp config
    /// directory. It elects itself, publishes fresh snapshots a reader can
    /// decode, and a second sampler beside it reads instead of writing.
    #[cfg(any(target_os = "macos", target_os = "linux"))]
    #[test]
    fn real_sampler_publishes_and_a_second_one_reads() {
        let dir = tempfile::tempdir().unwrap();
        let ticks = Arc::new(Mutex::new(Vec::new()));
        let seen = ticks.clone();
        let mut options = SamplerOptions::new(dir.path().to_path_buf());
        options.cadence = Duration::from_millis(200);
        options.on_tick = Some(Box::new(move |pulse| seen.lock().unwrap().push(pulse)));
        let first = start(options).unwrap();
        let deadline = Instant::now() + Duration::from_secs(20);
        while ticks.lock().unwrap().len() < 3 && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(50));
        }
        let status = first.status().expect("a status after three ticks");
        assert_eq!(status.role, Role::Writer, "{:?}", status.problems);
        assert!(status.writer_is_self);
        assert_eq!(
            status.sensor.state,
            SensorState::Fresh,
            "{:?}",
            status.sensor
        );
        assert_eq!(status.writer.as_ref().unwrap().pid, std::process::id());
        let procs = status
            .procs
            .as_ref()
            .expect("the writer has the process view");
        assert!(procs.ok && procs.processes > 1, "{procs:?}");
        #[cfg(target_os = "macos")]
        assert!(status.problems.is_empty(), "{:?}", status.problems);

        let mut second = SamplerOptions::new(dir.path().to_path_buf());
        second.cadence = Duration::from_millis(200);
        let second = start(second).unwrap();
        let deadline = Instant::now() + Duration::from_secs(10);
        let mut reader = None;
        while reader.is_none() && Instant::now() < deadline {
            reader = second.status();
            std::thread::sleep(Duration::from_millis(50));
        }
        let reader = reader.unwrap();
        assert_eq!(reader.role, Role::Reader);
        assert!(reader.procs.is_none());
        assert_eq!(reader.writer.unwrap().pid, std::process::id());
        second.stop();
        first.stop();
        let pulses = ticks.lock().unwrap();
        assert!(pulses.iter().all(|p| p.role == Role::Writer));
    }

    /// Real: the governor's held verdict rides in the snapshot a reader with
    /// no history can read. The record is published before the tick's sample
    /// is counted, so the first one has none and later ones carry the verdict
    /// the previous count produced.
    #[cfg(any(target_os = "macos", target_os = "linux"))]
    #[test]
    fn real_sampler_publishes_the_held_verdict_one_tick_behind() {
        let dir = tempfile::tempdir().unwrap();
        let ticks = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let seen = ticks.clone();
        let mut options = SamplerOptions::new(dir.path().to_path_buf());
        options.cadence = Duration::from_millis(50);
        options.on_tick = Some(Box::new(move |_| {
            seen.fetch_add(1, Ordering::SeqCst);
        }));
        let sampler = start(options).unwrap();
        crate::test_support::wait_until(Duration::from_secs(20), || ticks.load(Ordering::SeqCst) >= 6);
        sampler.stop();
        let record = snapshot::read(&dir.path().join(snapshot::SNAPSHOT_FILE_NAME)).unwrap();
        assert_eq!(record.version, snapshot::VERSION);
        let Reading::Value(verdict) = record.verdict else {
            panic!("no verdict after six ticks: {:?}", record.verdict);
        };
        // A host this test runs on may be under load; what must hold is that
        // the published fields agree with each other.
        if verdict.tier == super::governor::Tier::Clear {
            assert!(verdict.admit && !verdict.shed_background && !verdict.shed_reactive);
            assert_eq!(verdict.binding, None);
        }
        assert!(!verdict.shed_reactive || verdict.shed_background);
    }

    /// Real: the elected sampler appends a record every fifth tick, and on
    /// starting after a reboot keeps the old boot's ring as the previous
    /// one and reports its tail. A reader beside it appends nothing.
    #[cfg(any(target_os = "macos", target_os = "linux"))]
    #[test]
    fn real_sampler_traces_every_fifth_tick_and_keeps_the_last_boots_ring() {
        let dir = tempfile::tempdir().unwrap();
        let foreign_boot = Clock::now().boot_id ^ 0x5eed;
        {
            // A ring a previous boot left behind.
            let mut old = super::trace::TraceWriter::open(dir.path(), foreign_boot).unwrap();
            let snap = Snapshot::empty(Clock::now(), own_identity(), true);
            old.append(&TraceRecord::of(&snap, super::governor::Tier::Severe, None, false))
                .unwrap();
        }
        let ticks = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let seen = ticks.clone();
        let mut options = SamplerOptions::new(dir.path().to_path_buf());
        options.cadence = Duration::from_millis(20);
        options.on_tick = Some(Box::new(move |_| {
            seen.fetch_add(1, Ordering::SeqCst);
        }));
        let writer = start(options).unwrap();
        crate::test_support::wait_until(Duration::from_secs(30), || ticks.load(Ordering::SeqCst) >= 16);
        let status = writer.status().expect("a status");
        assert_eq!(status.role, Role::Writer, "{:?}", status.problems);
        // A second sampler in the same directory loses the election and
        // reads: it must append nothing.
        let mut second = SamplerOptions::new(dir.path().to_path_buf());
        second.cadence = Duration::from_millis(20);
        let reader = start(second).unwrap();
        std::thread::sleep(Duration::from_millis(400));
        reader.stop();
        // What Health shows under "Before the last restart" (`status()` adds
        // it to the stored status): the tail of the ring the old boot left.
        let shown = writer
            .shared
            .previous_boot
            .lock()
            .unwrap()
            .clone()
            .expect("the previous boot is reported");
        assert_eq!((shown.records.len(), shown.error.as_deref()), (1, None));
        assert_eq!(shown.peaks.max_tier, Some(super::governor::Tier::Severe));
        assert!(shown.peaks.refused);
        writer.stop();

        let held = trace::read_ring(&dir.path().join(trace::TRACE_FILE_NAME)).unwrap();
        let counted = ticks.load(Ordering::SeqCst);
        assert!(held.len() >= 3, "{} records after {counted} ticks", held.len());
        assert!(
            held.len() <= counted / 5 + 1,
            "{} records after {counted} ticks: more than one per five",
            held.len()
        );
        assert!(held.iter().all(|r| r.boot_id == Clock::now().boot_id));
        let previous = trace::read_ring(&dir.path().join(trace::PREV_TRACE_FILE_NAME)).unwrap();
        assert_eq!(previous.len(), 1, "the old boot's ring was kept");
        assert_eq!(previous[0].tier, Some(super::governor::Tier::Severe));
        assert_eq!(previous[0].admit, Some(false));
    }

    /// Real: while the trace write is blocked the sampler keeps publishing at
    /// cadence and its governor keeps counting; readings it cannot hand over
    /// are dropped, said in the status, and the count is in the ring once the
    /// write resumes. A write on the tick would have stopped all of it.
    #[cfg(any(target_os = "macos", target_os = "linux"))]
    #[test]
    fn a_blocked_trace_write_does_not_stop_the_sampler_and_the_drops_are_counted() {
        use super::trace::blocked_sink_tests::{blocked_writes, Gate};
        let dir = tempfile::tempdir().unwrap();
        let gate = Gate::new();
        let mut options = SamplerOptions::new(dir.path().to_path_buf());
        options.cadence = Duration::from_millis(20);
        options.trace_opener = blocked_writes(gate.clone());
        let sampler = start(options).unwrap();
        let snapshot_path = dir.path().join(snapshot::SNAPSHOT_FILE_NAME);
        let wait = crate::test_support::coverage_relaxed(Duration::from_secs(30));
        assert!(gate.wait_entered(wait), "no record ever reached the trace write");
        let seq_when_blocked = snapshot::read(&snapshot_path).unwrap().seq;
        let ticks_when_blocked = sampler.status().expect("a status").tick.ticks;

        // Thirty cadences later, with the write still blocked.
        let mut dropping = false;
        crate::test_support::wait_until(Duration::from_secs(30), || {
            let status = sampler.status().expect("a status");
            dropping = status.problems.iter().any(|p| p.contains("dropped while the trace write was blocked"));
            dropping && status.tick.ticks >= ticks_when_blocked + 30
        });
        let status = sampler.status().expect("a status");
        let record = snapshot::read(&snapshot_path).unwrap();
        assert!(
            record.seq >= seq_when_blocked + 20,
            "snapshots stopped while the write was blocked: seq {} then {}",
            seq_when_blocked,
            record.seq
        );
        assert!(status.tick.ticks >= ticks_when_blocked + 20, "{:?}", status.tick);
        assert!(
            matches!(record.verdict, Reading::Value(_)),
            "the governor stopped counting: {:?}",
            record.verdict
        );
        assert!(dropping, "the drops were not said: {:?}", status.problems);

        gate.release();
        sampler.stop();
        let held = trace::read_ring(&dir.path().join(trace::TRACE_FILE_NAME)).unwrap();
        let carried: u64 = held.iter().map(|r| r.dropped_before).sum();
        assert!(carried >= 3, "the ring says {carried} dropped: {held:?}");
        assert_eq!(held.last().map(|r| r.dropped_total), Some(carried));
    }

    /// Real: opening the ring (which can rename the previous boot's) is on
    /// the trace thread too, so a stuck open leaves the sampler publishing.
    #[cfg(any(target_os = "macos", target_os = "linux"))]
    #[test]
    fn a_blocked_trace_open_does_not_stop_the_sampler() {
        use super::trace::blocked_sink_tests::{blocked_open, Gate};
        let dir = tempfile::tempdir().unwrap();
        let gate = Gate::new();
        let mut options = SamplerOptions::new(dir.path().to_path_buf());
        options.cadence = Duration::from_millis(20);
        options.trace_opener = blocked_open(gate.clone());
        let sampler = start(options).unwrap();
        let wait = crate::test_support::coverage_relaxed(Duration::from_secs(30));
        assert!(gate.wait_entered(wait), "the open was never attempted");
        let snapshot_path = dir.path().join(snapshot::SNAPSHOT_FILE_NAME);
        // The open is attempted in the tick that wins the election, before
        // that tick's first publish: wait for the first snapshot to exist.
        let seq = || snapshot::read(&snapshot_path).map_or(0, |s| s.seq);
        crate::test_support::wait_until(Duration::from_secs(30), || seq() > 0);
        let first = seq();
        assert!(first > 0, "no snapshot was ever published");
        crate::test_support::wait_until(Duration::from_secs(30), || seq() >= first + 15);
        assert!(
            seq() >= first + 15,
            "snapshots stopped while the open was blocked"
        );
        gate.release();
        sampler.stop();
    }

    /// Real: a tick that panics does not end the sampler. A raw thread died
    /// with its first panic and the pulse simply stopped, so the status bar
    /// kept showing the last reading as though it were current.
    #[cfg(any(target_os = "macos", target_os = "linux"))]
    #[test]
    fn real_sampler_keeps_publishing_after_a_tick_panics() {
        let dir = tempfile::tempdir().unwrap();
        let ticks = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let seen = ticks.clone();
        let mut options = SamplerOptions::new(dir.path().to_path_buf());
        options.cadence = Duration::from_millis(50);
        options.on_tick = Some(Box::new(move |_| {
            if seen.fetch_add(1, Ordering::SeqCst) == 0 {
                panic!("injected sampler panic");
            }
        }));
        let sampler = start(options).unwrap();
        let deadline = Instant::now() + Duration::from_secs(20);
        while ticks.load(Ordering::SeqCst) < 4 && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(20));
        }
        let after = ticks.load(Ordering::SeqCst);
        sampler.stop();
        assert!(
            after >= 4,
            "the sampler stopped ticking after its first tick panicked ({after} ticks)"
        );
    }

    /// Real: the kill switch off still publishes — enabled false and no
    /// readings — so a reader can tell "switched off" from "no sampler".
    #[cfg(any(target_os = "macos", target_os = "linux"))]
    #[test]
    fn real_sampler_switched_off_publishes_a_heartbeat_without_readings() {
        let dir = tempfile::tempdir().unwrap();
        let mut options = SamplerOptions::new(dir.path().to_path_buf());
        options.cadence = Duration::from_millis(100);
        options.enabled = Box::new(|| false);
        let sampler = start(options).unwrap();
        let deadline = Instant::now() + Duration::from_secs(10);
        let mut status = None;
        while status.is_none() && Instant::now() < deadline {
            status = sampler.status();
            std::thread::sleep(Duration::from_millis(20));
        }
        sampler.stop();
        let status = status.unwrap();
        assert_eq!(status.sensor.enabled, Some(false));
        assert_eq!(status.pressure, PressureInput::Unavailable);
        let snap = status.snapshot.unwrap();
        assert!(!snap.enabled);
        assert_eq!(snap.host.pressure, Reading::default());
        assert!(status.procs.is_none());
    }

    /// Counts activities begun and still alive; dropping one ends it.
    struct Live(Arc<std::sync::atomic::AtomicUsize>);

    impl Drop for Live {
        fn drop(&mut self) {
            self.0.fetch_sub(1, Ordering::SeqCst);
        }
    }

    fn counting_begin(
        begun: &Arc<std::sync::atomic::AtomicUsize>,
        live: &Arc<std::sync::atomic::AtomicUsize>,
    ) -> impl Fn() -> Result<Box<dyn Send>, String> {
        let (begun, live) = (begun.clone(), live.clone());
        move || {
            begun.fetch_add(1, Ordering::SeqCst);
            live.fetch_add(1, Ordering::SeqCst);
            Ok(Box::new(Live(live.clone())) as Box<dyn Send>)
        }
    }

    #[test]
    fn the_activity_follows_the_switch_and_is_never_held_while_it_is_off() {
        let begun = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let live = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let begin = counting_begin(&begun, &live);
        let mut hold = ActivityHold::default();
        let at = |live: &Arc<std::sync::atomic::AtomicUsize>| live.load(Ordering::SeqCst);

        // Disabled from the first tick: nothing is ever begun.
        for _ in 0..3 {
            assert_eq!(hold.follow(false, &begin), None);
        }
        assert_eq!((begun.load(Ordering::SeqCst), at(&live)), (0, 0));

        // Enabled: begun once, not once per tick.
        for _ in 0..3 {
            assert_eq!(hold.follow(true, &begin), None);
        }
        assert_eq!((begun.load(Ordering::SeqCst), at(&live)), (1, 1));

        // Disabled: ended on that tick.
        assert_eq!(hold.follow(false, &begin), None);
        assert_eq!((begun.load(Ordering::SeqCst), at(&live)), (1, 0));

        // Enabled again: begun again.
        assert_eq!(hold.follow(true, &begin), None);
        assert_eq!((begun.load(Ordering::SeqCst), at(&live)), (2, 1));
    }

    #[test]
    fn a_refused_activity_is_reported_once_and_not_retried_every_tick() {
        let attempts = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let counted = attempts.clone();
        let refuse = move || -> Result<Box<dyn Send>, String> {
            counted.fetch_add(1, Ordering::SeqCst);
            Err("no".to_string())
        };
        let mut hold = ActivityHold::default();
        for _ in 0..4 {
            assert_eq!(hold.follow(true, &refuse), Some("App Nap activity: no"));
        }
        assert_eq!(attempts.load(Ordering::SeqCst), 1);
        assert_eq!(hold.follow(false, &refuse), None, "off clears the problem");
    }

    /// Real: the sampler thread itself, not just the state machine, holds no
    /// activity at start while `host_load.enabled` is false, begins one when
    /// it turns on and ends it when it turns off.
    #[cfg(any(target_os = "macos", target_os = "linux"))]
    #[test]
    fn real_sampler_holds_its_activity_only_while_enabled() {
        let dir = tempfile::tempdir().unwrap();
        let enabled = Arc::new(AtomicBool::new(false));
        let begun = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let live = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let mut options = SamplerOptions::new(dir.path().to_path_buf());
        options.cadence = Duration::from_millis(50);
        let switch = enabled.clone();
        options.enabled = Box::new(move || switch.load(Ordering::SeqCst));
        options.begin_activity = Box::new(counting_begin(&begun, &live));
        let sampler = start(options).unwrap();
        let limit = crate::test_support::coverage_relaxed(Duration::from_secs(20));
        let wait_for = |what: &str, done: &dyn Fn() -> bool| {
            let deadline = Instant::now() + limit;
            while !done() {
                assert!(Instant::now() < deadline, "timed out waiting for {what}");
                std::thread::sleep(Duration::from_millis(10));
            }
        };

        wait_for("the first published tick", &|| {
            sampler
                .status()
                .is_some_and(|status| status.tick.ticks >= 3)
        });
        assert_eq!(begun.load(Ordering::SeqCst), 0, "begun while disabled");

        enabled.store(true, Ordering::SeqCst);
        wait_for("the activity to begin", &|| {
            live.load(Ordering::SeqCst) == 1
        });
        enabled.store(false, Ordering::SeqCst);
        wait_for("the activity to end", &|| live.load(Ordering::SeqCst) == 0);
        enabled.store(true, Ordering::SeqCst);
        wait_for("the activity to begin again", &|| {
            begun.load(Ordering::SeqCst) == 2 && live.load(Ordering::SeqCst) == 1
        });
        sampler.stop();
        assert_eq!(live.load(Ordering::SeqCst), 0, "stop ends the activity");
    }
}
