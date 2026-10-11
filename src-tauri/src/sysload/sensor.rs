//! The readings one sample carries, and the seam that produces them.
//!
//! Every reading is a [`Reading`]: a value, or the reason there is none. A
//! measurement the OS refused is [`Why::Unreadable`] — never zero — because a
//! consumer deciding whether the machine has headroom must not mistake "we
//! could not look" for "nothing is there".

use serde::{Deserialize, Serialize};

/// Why a reading has no value.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Why {
    /// This platform has no way to measure it.
    Unsupported,
    /// The OS refused (`EPERM`, `EACCES`): unreadable, never zero.
    Unreadable,
    /// Attempted, and failed for another reason.
    Failed,
    /// A delta or rate that needs a previous sample.
    NotYet,
}

impl Why {
    pub(super) fn code(self) -> u8 {
        match self {
            Self::Unsupported => 1,
            Self::Unreadable => 2,
            Self::Failed => 3,
            Self::NotYet => 4,
        }
    }

    pub(super) fn from_code(code: u8) -> Option<Self> {
        match code {
            1 => Some(Self::Unsupported),
            2 => Some(Self::Unreadable),
            3 => Some(Self::Failed),
            4 => Some(Self::NotYet),
            _ => None,
        }
    }

    /// The class of an OS error, by errno.
    pub(super) fn of_errno(errno: Option<i32>) -> Self {
        match errno {
            #[cfg(unix)]
            Some(libc::EPERM | libc::EACCES) => Self::Unreadable,
            // A guard rather than a pattern: Linux defines ENOTSUP and
            // EOPNOTSUPP as one value, macOS as two.
            #[cfg(unix)]
            Some(e) if e == libc::ENOENT || e == libc::ENOTSUP || e == libc::EOPNOTSUPP => {
                Self::Unsupported
            }
            _ => Self::Failed,
        }
    }
}

/// A value, or why there is none.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "status", content = "value")]
pub enum Reading<T> {
    Value(T),
    Unavailable(Why),
}

impl<T> Default for Reading<T> {
    fn default() -> Self {
        Self::Unavailable(Why::Unsupported)
    }
}

impl<T: Copy> Reading<T> {
    pub fn value(&self) -> Option<T> {
        match self {
            Self::Value(v) => Some(*v),
            Self::Unavailable(_) => None,
        }
    }

    pub fn map<U>(self, f: impl FnOnce(T) -> U) -> Reading<U> {
        match self {
            Self::Value(v) => Reading::Value(f(v)),
            Self::Unavailable(why) => Reading::Unavailable(why),
        }
    }
}

/// The kernel's memory-pressure level (`kern.memorystatus_vm_pressure_level`
/// on macOS: 1, 2 and 4).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Level {
    Normal,
    Warn,
    Critical,
}

impl Level {
    pub fn from_kernel(raw: u32) -> Option<Self> {
        match raw {
            1 => Some(Self::Normal),
            2 => Some(Self::Warn),
            4 => Some(Self::Critical),
            _ => None,
        }
    }

    pub fn kernel(self) -> u32 {
        match self {
            Self::Normal => 1,
            Self::Warn => 2,
            Self::Critical => 4,
        }
    }
}

/// Cumulative compressor counters, monotonic within one boot.
///
/// There is no `compressor_bytes_used` or segment-limit sysctl; these are the
/// counters that exist (`vm.compressor.swapper.wakeups.*`,
/// `vm.compressor.swapper.swapouts_pressure`,
/// `vm.compressor.compactor.major_compaction_segments_freed`).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Compressor {
    pub thrashing_detected: u64,
    pub free_below_reserved: u64,
    pub swapouts_pressure: u64,
    pub major_compaction_segments_freed: u64,
}

/// How much each compressor counter rose since the previous sample.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct CompressorDelta {
    pub thrashing_detected: u64,
    pub free_below_reserved: u64,
    pub swapouts_pressure: u64,
    pub major_compaction_segments_freed: u64,
}

/// Logical CPUs per performance level. Apple silicon names its levels
/// (`hw.perflevel0` "Super" with 6 here, `perflevel1` "Performance" with
/// 12, and no efficiency level at all), so "the P-core count" is not a
/// number this can publish; it publishes the levels as they are. Intel and
/// Rosetta have no levels and report `levels: 0` with the logical count.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Cores {
    pub logical: u16,
    pub levels: u8,
    pub perf0: u16,
    pub perf1: u16,
}

/// `NSProcessInfo.thermalState`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Thermal {
    Nominal,
    Fair,
    Serious,
    Critical,
}

impl Thermal {
    pub(super) fn code(self) -> u8 {
        match self {
            Self::Nominal => 0,
            Self::Fair => 1,
            Self::Serious => 2,
            Self::Critical => 3,
        }
    }

    pub(super) fn from_code(code: u8) -> Option<Self> {
        match code {
            0 => Some(Self::Nominal),
            1 => Some(Self::Fair),
            2 => Some(Self::Serious),
            3 => Some(Self::Critical),
            _ => None,
        }
    }
}

/// What the machine is running on.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Power {
    Ac,
    Battery,
}

/// The host-wide readings of one sample. Process-table readings (sessions,
/// footprints, disk writes) are the process scan's.
#[derive(Debug, Clone, Copy, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct HostSample {
    pub pressure: Reading<Level>,
    pub swap_used: Reading<u64>,
    pub swap_total: Reading<u64>,
    pub compressor: Reading<Compressor>,
    /// Physical memory the compressor occupies (`host_statistics64`
    /// `compressor_page_count` × page size).
    pub compressed_bytes: Reading<u64>,
    pub load: Reading<[f64; 3]>,
    pub cores: Reading<Cores>,
    pub memsize: Reading<u64>,
    /// `kern.num_files`, against `kern.maxfiles`.
    pub open_files: Reading<u64>,
    pub max_files: Reading<u64>,
    /// Bytes available to this user on the volume holding GitPulse's state.
    pub free_disk: Reading<u64>,
    pub thermal: Reading<Thermal>,
    pub power: Reading<Power>,
}

/// The seam every host reading comes through: the OS on a real run, a trace
/// on a replay.
pub trait Sensor {
    fn sample(&mut self) -> HostSample;
}

/// Host readings from this operating system.
pub struct OsSensor {
    probe: super::probe::HostProbe,
}

impl OsSensor {
    /// `disk_path` names the volume whose free space is reported.
    pub fn new(disk_path: &std::path::Path) -> Self {
        Self {
            probe: super::probe::HostProbe::new(disk_path),
        }
    }
}

impl Sensor for OsSensor {
    fn sample(&mut self) -> HostSample {
        self.probe.sample()
    }
}

/// Plays back a recorded trace: one [`HostSample`] per line, as JSON, with
/// absent fields unsupported. After the last line it keeps returning the
/// last one, as a sensor that stopped changing would.
pub struct ReplaySensor {
    trace: Vec<HostSample>,
    at: usize,
}

impl ReplaySensor {
    pub fn new(trace: Vec<HostSample>) -> Self {
        Self { trace, at: 0 }
    }

    /// Parses a JSON-lines trace. Blank lines and `#` comments are skipped.
    pub fn from_trace(text: &str) -> Result<Self, String> {
        let mut trace = Vec::new();
        for (n, line) in text.lines().enumerate() {
            let line = line.trim();
            if line.is_empty() || line.starts_with('#') {
                continue;
            }
            trace.push(
                serde_json::from_str(line).map_err(|e| format!("trace line {}: {e}", n + 1))?,
            );
        }
        if trace.is_empty() {
            return Err("the trace holds no samples".into());
        }
        Ok(Self::new(trace))
    }
}

impl Sensor for ReplaySensor {
    fn sample(&mut self) -> HostSample {
        let sample = self
            .trace
            .get(self.at)
            .or_else(|| self.trace.last())
            .copied()
            .unwrap_or_default();
        self.at = self.at.saturating_add(1);
        sample
    }
}

/// How many consecutive rising samples make the compressor "thrashing".
pub const THRASH_SAMPLES: usize = 3;

/// Turns cumulative compressor counters into per-sample deltas and the
/// `compressor_thrash` flag: thrashing wakeups or pressure swapouts rising in
/// each of the last [`THRASH_SAMPLES`] samples. One quiet sample clears it.
#[derive(Debug, Default)]
pub struct CompressorTrend {
    previous: Option<Compressor>,
    rising_run: usize,
}

impl CompressorTrend {
    pub fn observe(&mut self, now: Reading<Compressor>) -> (Reading<CompressorDelta>, bool) {
        let Reading::Value(now) = now else {
            // A gap in the counters is not evidence of calm: forget the run
            // and the baseline, so the next three samples must earn it again.
            self.previous = None;
            self.rising_run = 0;
            return (now.map(|_| CompressorDelta::default()), false);
        };
        let Some(before) = self.previous.replace(now) else {
            return (Reading::Unavailable(Why::NotYet), false);
        };
        // A counter that went down was reset (a new boot, an overflow); its
        // delta is unknown, so the sample rebases rather than counting.
        let reset = now.thrashing_detected < before.thrashing_detected
            || now.free_below_reserved < before.free_below_reserved
            || now.swapouts_pressure < before.swapouts_pressure
            || now.major_compaction_segments_freed < before.major_compaction_segments_freed;
        if reset {
            self.rising_run = 0;
            return (Reading::Unavailable(Why::NotYet), false);
        }
        let delta = CompressorDelta {
            thrashing_detected: now.thrashing_detected - before.thrashing_detected,
            free_below_reserved: now.free_below_reserved - before.free_below_reserved,
            swapouts_pressure: now.swapouts_pressure - before.swapouts_pressure,
            major_compaction_segments_freed: now.major_compaction_segments_freed
                - before.major_compaction_segments_freed,
        };
        if delta.thrashing_detected > 0 || delta.swapouts_pressure > 0 {
            self.rising_run = self.rising_run.saturating_add(1);
        } else {
            self.rising_run = 0;
        }
        (Reading::Value(delta), self.rising_run >= THRASH_SAMPLES)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn counters(thrash: u64, swapouts: u64) -> Reading<Compressor> {
        Reading::Value(Compressor {
            thrashing_detected: thrash,
            swapouts_pressure: swapouts,
            ..Compressor::default()
        })
    }

    /// Replayed: a recorded trace, not the live kernel.
    #[test]
    fn replayed_thrash_needs_three_rising_samples_and_one_quiet_one_clears_it() {
        let trace = r#"
            # thrashing wakeups climb for three samples, then stop
            {"compressor":{"status":"value","value":{"thrashing_detected":10,"free_below_reserved":0,"swapouts_pressure":100,"major_compaction_segments_freed":0}}}
            {"compressor":{"status":"value","value":{"thrashing_detected":11,"free_below_reserved":0,"swapouts_pressure":100,"major_compaction_segments_freed":0}}}
            {"compressor":{"status":"value","value":{"thrashing_detected":11,"free_below_reserved":0,"swapouts_pressure":140,"major_compaction_segments_freed":0}}}
            {"compressor":{"status":"value","value":{"thrashing_detected":15,"free_below_reserved":2,"swapouts_pressure":140,"major_compaction_segments_freed":0}}}
            {"compressor":{"status":"value","value":{"thrashing_detected":15,"free_below_reserved":2,"swapouts_pressure":140,"major_compaction_segments_freed":9}}}
        "#;
        let mut sensor = ReplaySensor::from_trace(trace).unwrap();
        let mut trend = CompressorTrend::default();
        let flags: Vec<bool> = (0..5)
            .map(|_| trend.observe(sensor.sample().compressor).1)
            .collect();
        // Sample 1 has no delta; 2, 3 and 4 rise; 5 rises only in compaction,
        // which is not thrash.
        assert_eq!(flags, [false, false, false, true, false]);
    }

    #[test]
    fn a_gap_or_a_reset_restarts_the_run_and_never_reads_as_a_delta() {
        let mut trend = CompressorTrend::default();
        assert_eq!(
            trend.observe(counters(1, 1)).0,
            Reading::Unavailable(Why::NotYet)
        );
        trend.observe(counters(2, 1));
        trend.observe(counters(3, 1));
        // Unreadable counters: no delta, the flag drops, the run is forgotten.
        let (delta, thrash) = trend.observe(Reading::Unavailable(Why::Unreadable));
        assert_eq!(delta, Reading::Unavailable(Why::Unreadable));
        assert!(!thrash);
        assert_eq!(
            trend.observe(counters(4, 1)).0,
            Reading::Unavailable(Why::NotYet)
        );
        // A counter that went down (a reboot) is a rebase, not a negative
        // delta and not a rise.
        trend.observe(counters(5, 1));
        let (delta, thrash) = trend.observe(counters(0, 0));
        assert_eq!(delta, Reading::Unavailable(Why::NotYet));
        assert!(!thrash);
        trend.observe(counters(1, 0));
        trend.observe(counters(2, 0));
        assert!(
            trend.observe(counters(3, 0)).1,
            "three rises after the rebase"
        );
    }

    #[test]
    fn a_trace_must_hold_samples_and_parse() {
        assert!(ReplaySensor::from_trace("# nothing\n\n").is_err());
        let bad = ReplaySensor::from_trace("{\"pressure\": 7}").err().unwrap();
        assert!(bad.contains("line 1"), "{bad}");
        let mut one =
            ReplaySensor::from_trace(r#"{"pressure":{"status":"value","value":"critical"}}"#)
                .unwrap();
        assert_eq!(one.sample().pressure, Reading::Value(Level::Critical));
        // Past the end it holds the last sample, and absent fields are
        // unsupported rather than zero.
        let again = one.sample();
        assert_eq!(again.pressure, Reading::Value(Level::Critical));
        assert_eq!(again.swap_used, Reading::Unavailable(Why::Unsupported));
    }

    #[test]
    fn levels_and_codes_round_trip() {
        for level in [Level::Normal, Level::Warn, Level::Critical] {
            assert_eq!(Level::from_kernel(level.kernel()), Some(level));
        }
        assert_eq!(Level::from_kernel(3), None);
        for why in [Why::Unsupported, Why::Unreadable, Why::Failed, Why::NotYet] {
            assert_eq!(Why::from_code(why.code()), Some(why));
        }
        assert_eq!(Why::from_code(0), None);
        for t in [
            Thermal::Nominal,
            Thermal::Fair,
            Thermal::Serious,
            Thermal::Critical,
        ] {
            assert_eq!(Thermal::from_code(t.code()), Some(t));
        }
        #[cfg(unix)]
        {
            assert_eq!(Why::of_errno(Some(libc::EPERM)), Why::Unreadable);
            assert_eq!(Why::of_errno(Some(libc::ENOENT)), Why::Unsupported);
        }
        assert_eq!(Why::of_errno(None), Why::Failed);
    }
}
