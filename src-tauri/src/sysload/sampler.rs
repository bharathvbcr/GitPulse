//! The sampling thread: one tick every [`CADENCE_MS`], in the app process.
//!
//! Each tick elects (or keeps) the writer, samples the host and the process
//! table, publishes `host-load.v1`, and leaves a status for Health and a
//! pulse for the frontend. The cadence is fixed: a tick that runs long is an
//! overrun, a slot the thread woke too late for is missed, and both are
//! published rather than absorbed by stretching the interval.
//!
//! The thread asks for the user-initiated QoS class and holds an
//! `NSProcessInfo` activity for its whole life, so App Nap does not coalesce
//! its timer while the window is hidden. Either request failing is recorded
//! in the status, not hidden.

use super::procscan::{ProcView, ScanContext, Scanner};
use super::sensor::{CompressorTrend, Level, OsSensor, Reading, Sensor};
use super::snapshot::{self, Snapshot, TickStats, Writer, WriterIdentity};
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
    }
}

/// How to run one sampler.
pub struct SamplerOptions {
    /// The per-user config directory the snapshot and its lock live in.
    pub dir: PathBuf,
    pub cadence: Duration,
    /// `host_load.enabled`, read once per tick.
    pub enabled: Box<dyn Fn() -> bool + Send>,
    /// Called after every tick with that tick's pulse.
    pub on_tick: Option<Box<dyn Fn(HostLoadPulse) + Send>>,
}

impl SamplerOptions {
    pub fn new(dir: PathBuf) -> Self {
        Self {
            dir,
            cadence: Duration::from_millis(CADENCE_MS),
            enabled: Box::new(|| true),
            on_tick: None,
        }
    }
}

struct Shared {
    dir: PathBuf,
    status: Mutex<Option<HostLoadStatus>>,
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
    if APP_SAMPLER.set(handle.shared.clone()).is_err() {
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
        stop: AtomicBool::new(false),
    });
    let worker = shared.clone();
    let thread = std::thread::Builder::new()
        .name("host-load-sampler".into())
        .spawn(move || run(&worker, options))
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
            return status;
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
            snapshot_path: None,
            writer: None,
            writer_is_self: false,
            snapshot: None,
            procs: None,
            tick: TickStats::default(),
            problems: Vec::new(),
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
        sensor,
        snapshot_path: Some(path.to_string_lossy().into_owned()),
        writer: snap.as_ref().map(|s| s.writer.clone()),
        writer_is_self: false,
        snapshot: snap,
        procs: None,
        tick,
        problems,
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

fn run(shared: &Shared, options: SamplerOptions) {
    let mut problems = Vec::new();
    if let Err(why) = super::probe::set_user_initiated_qos() {
        problems.push(format!("QoS: {why}"));
    }
    let _activity = match super::probe::Activity::begin("GitPulse host-load sampler") {
        Ok(activity) => Some(activity),
        Err(why) => {
            problems.push(format!("App Nap activity: {why}"));
            None
        }
    };
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

    while !shared.stop.load(Ordering::Acquire) {
        let began = Instant::now();
        let mut tick_problems = problems.clone();
        if writer.is_none() {
            match Writer::elect(&shared.dir) {
                Ok(elected) => writer = elected,
                Err(why) => tick_problems.push(format!("election: {why}")),
            }
        }
        let enabled = (options.enabled)();
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
}

#[cfg(test)]
mod tests {
    use super::*;

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
}
