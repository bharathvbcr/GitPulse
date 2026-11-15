//! What the governor decides on besides the kernel's pressure level and the
//! compressor's thrash flag: swap, the compressor's segment use, CPU load,
//! thermal state, power source and free disk, each with a mode and thresholds
//! the person can change (Settings, Agents, Host load).
//!
//! # Why
//!
//! On 2026-11-14 the owner's Mac (64 GiB, 18 logical CPUs) hung and reset
//! with swap at 0.9x physical memory, a 1-minute load of 120 (6.7 per CPU) and
//! about 32 Claude processes, while the kernel pressure level read `warn`. The
//! sensor already published every one of those readings and nothing decided on
//! them.
//!
//! # Modes
//!
//! * [`InputMode::Off`]: the input is ignored.
//! * [`InputMode::Shed`]: the input can raise the tier to Elevated at most.
//!   GitPulse sheds its own background work; launches are admitted.
//! * [`InputMode::Refuse`]: it can also reach Severe: reactive work is shed
//!   and agent launches are refused (still subject to
//!   `host_load.govern_launches`, which when off means nothing refuses).
//!
//! # Defaults are provisional
//!
//! Every default below was set from the one incident above and from nothing
//! else. They are protective starting points, not measured optima, and each is
//! changeable in Settings.
//!
//! * `swap`: refuse, elevated at 0.50x and severe at 0.75x. Swap USED divided
//!   by PHYSICAL memory. Never by swap total: macOS grows swap on demand, so
//!   the total reads 0 after boot.
//! * `compressor_segments`: refuse, 0.60 and 0.80 of the segment limit.
//! * `load`: shed, 3.0 and 6.0 for the 1-minute load average per logical CPU.
//! * `thermal`: shed; `serious` is elevated, `critical` is severe (reachable
//!   only in refuse).
//! * `battery`: off; when enabled, running on battery is elevated and never
//!   severe.
//! * `disk`: refuse, elevated below 20 GiB free and severe below 10 GiB (lower
//!   is worse). Owner decision 2026-11-14: low disk may now refuse a launch
//!   when this is set to refuse; before it was advice only (see `disk.rs`).
//!
//! # A reading that could not be taken
//!
//! An unreadable reading contributes no tier and adds a warning to the verdict
//! ("swap is unreadable: not governed on"): a check that could not run never
//! reads like one that passed. A reading the platform does not provide at all
//! (unsupported, or not published by an older writer) is not a failed check
//! and adds no warning.

use super::governor::{Binding, Tier};
use super::sensor::{CompressorSegments, Cores, Power, Reading, Thermal, Why};
use serde::{Deserialize, Serialize};

/// What an input is allowed to do to the verdict.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum InputMode {
    Off,
    Shed,
    Refuse,
}

impl InputMode {
    /// The highest tier an input in this mode can put the verdict in.
    fn cap(self, tier: Tier) -> Tier {
        match self {
            Self::Off => Tier::Clear,
            Self::Shed => tier.min(Tier::Elevated),
            Self::Refuse => tier,
        }
    }
}

/// A mode and two thresholds. For swap, segments and load a reading at or
/// above `elevated_at` is elevated and at or above `severe_at` severe. For
/// disk (in GiB free) lower is worse: below `elevated_at` is elevated and
/// below `severe_at` severe, so `severe_at <= elevated_at`.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct Rule {
    pub mode: InputMode,
    pub elevated_at: f64,
    pub severe_at: f64,
}

/// An input with no threshold: its states are fixed (thermal, battery).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct ModeRule {
    pub mode: InputMode,
}

/// How soon the disk is projected to reach its severe floor, as a mode and two
/// horizons in minutes. A projection under `elevated_within_min` is elevated and
/// under `severe_within_min` severe, so `severe_within_min <= elevated_within_min`.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct HorizonRule {
    pub mode: InputMode,
    pub elevated_within_min: f64,
    pub severe_within_min: f64,
}

/// The free space a coverage build needs before it starts, in GiB, and what the
/// command gate does about a build that starts without it.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct HeadroomRule {
    pub mode: InputMode,
    pub headroom_gib: f64,
}

/// The disk guards that are not one level threshold: the fill-rate horizon and
/// the coverage headroom. Stored apart from `host_load_thresholds` (see
/// `tool_config`): a build that predates them rewrites that key from the inputs
/// it knows, and would drop these.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct DiskGuard {
    pub fill: HorizonRule,
    pub coverage: HeadroomRule,
}

impl Default for DiskGuard {
    fn default() -> Self {
        Self {
            fill: HorizonRule {
                mode: InputMode::Refuse,
                elevated_within_min: 30.0,
                severe_within_min: 10.0,
            },
            coverage: HeadroomRule {
                mode: InputMode::Refuse,
                headroom_gib: 60.0,
            },
        }
    }
}

impl DiskGuard {
    pub fn is_default(&self) -> bool {
        *self == Self::default()
    }

    /// Reads a stored block one guard at a time: one that cannot be read or is
    /// invalid keeps its default, with a warning, and costs the other nothing.
    pub fn from_stored(raw: serde_json::Value) -> Self {
        let mut out = Self::default();
        let serde_json::Value::Object(map) = raw else {
            log::warn!(
                target: "tool_config",
                "ignoring unreadable host_load_disk_guard block, both guards keep their defaults"
            );
            return out;
        };
        if let Some(value) = map.get("fill") {
            match serde_json::from_value::<HorizonRule>(value.clone()) {
                Ok(rule) if fill_problem(&rule).is_none() => out.fill = rule,
                Ok(rule) => log::warn!(
                    target: "tool_config",
                    "ignoring invalid host_load_disk_guard.fill, it keeps its default: {}",
                    fill_problem(&rule).unwrap_or_default()
                ),
                Err(error) => log::warn!(
                    target: "tool_config",
                    "ignoring unreadable host_load_disk_guard.fill, it keeps its default: {error}"
                ),
            }
        }
        if let Some(value) = map.get("coverage") {
            match serde_json::from_value::<HeadroomRule>(value.clone()) {
                Ok(rule) if coverage_problem(&rule).is_none() => out.coverage = rule,
                Ok(rule) => log::warn!(
                    target: "tool_config",
                    "ignoring invalid host_load_disk_guard.coverage, it keeps its default: {}",
                    coverage_problem(&rule).unwrap_or_default()
                ),
                Err(error) => log::warn!(
                    target: "tool_config",
                    "ignoring unreadable host_load_disk_guard.coverage, it keeps its default: {error}"
                ),
            }
        }
        out
    }

    /// Only what differs from the default, so a later build that changes a
    /// default reaches a person who never chose a value.
    pub fn to_stored(&self) -> serde_json::Map<String, serde_json::Value> {
        let base = Self::default();
        let mut out = serde_json::Map::new();
        if self.fill != base.fill {
            if let Ok(value) = serde_json::to_value(self.fill) {
                out.insert("fill".to_string(), value);
            }
        }
        if self.coverage != base.coverage {
            if let Ok(value) = serde_json::to_value(self.coverage) {
                out.insert("coverage".to_string(), value);
            }
        }
        out
    }
}

/// The longest horizon that can be set: a week.
const MAX_HORIZON_MIN: f64 = 10_080.0;

fn fill_problem(rule: &HorizonRule) -> Option<String> {
    let (elevated, severe) = (rule.elevated_within_min, rule.severe_within_min);
    if !elevated.is_finite() || !severe.is_finite() {
        return Some("disk_fill: horizons must be finite numbers".to_string());
    }
    if elevated <= 0.0 || severe <= 0.0 || elevated > MAX_HORIZON_MIN || severe > MAX_HORIZON_MIN {
        return Some(format!(
            "disk_fill: horizons must be above 0 and at most {MAX_HORIZON_MIN} minutes"
        ));
    }
    if severe > elevated {
        return Some(
            "disk_fill: the severe horizon must not be longer than the elevated one".to_string(),
        );
    }
    None
}

fn coverage_problem(rule: &HeadroomRule) -> Option<String> {
    let gib = rule.headroom_gib;
    if !gib.is_finite() || gib <= 0.0 || gib > MAX_DISK_GIB {
        return Some(format!(
            "coverage: the headroom must be above 0 and at most {MAX_DISK_GIB} GiB"
        ));
    }
    None
}

/// Every input's rule. A missing input reads as its default.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Thresholds {
    /// Swap used over physical memory.
    pub swap: Rule,
    /// Compressor segments in use over its segment limit.
    pub compressor_segments: Rule,
    /// 1-minute load average per logical CPU.
    pub load: Rule,
    pub thermal: ModeRule,
    pub battery: ModeRule,
    /// Free disk, in GiB; lower is worse. While `disk_auto` the two numbers
    /// are not read: the floors follow physical memory ([`auto_disk_floors`]).
    pub disk: Rule,
    /// Whether the disk floors are the automatic ones, which scale with
    /// physical memory because swap shares the free-space pool. A person who
    /// sets a number turns it off; their numbers then win, always.
    pub disk_auto: bool,
    /// The disk guards beyond the level: the fill-rate horizon and the
    /// coverage headroom.
    pub disk_guard: DiskGuard,
}

impl Default for Thresholds {
    fn default() -> Self {
        Self {
            swap: Rule {
                mode: InputMode::Refuse,
                elevated_at: 0.50,
                severe_at: 0.75,
            },
            compressor_segments: Rule {
                mode: InputMode::Refuse,
                elevated_at: 0.60,
                severe_at: 0.80,
            },
            load: Rule {
                mode: InputMode::Shed,
                elevated_at: 3.0,
                severe_at: 6.0,
            },
            thermal: ModeRule {
                mode: InputMode::Shed,
            },
            battery: ModeRule {
                mode: InputMode::Off,
            },
            disk: Rule {
                mode: InputMode::Refuse,
                elevated_at: DISK_ELEVATED_FLOOR_GIB,
                severe_at: DISK_SEVERE_FLOOR_GIB,
            },
            disk_auto: true,
            disk_guard: DiskGuard::default(),
        }
    }
}

/// The least the automatic disk floors ever are, in GiB, and what they are
/// when physical memory cannot be read.
pub const DISK_ELEVATED_FLOOR_GIB: f64 = 20.0;
pub const DISK_SEVERE_FLOOR_GIB: f64 = 10.0;

/// The automatic disk floors `(elevated, severe)` in GiB for a machine with
/// `memsize` bytes of memory: swap files share the free-space pool with user
/// data, so a fixed 10 GiB leaves a 64 GiB Mac with less free than it may swap.
/// Severe is a quarter of memory and elevated a half, never below the fixed
/// floors, which are also what an unreadable memory size gets.
pub fn auto_disk_floors(memsize: Option<u64>) -> (f64, f64) {
    let ram_gib = memsize.filter(|m| *m > 0).map_or(0.0, |m| m as f64 / GIB);
    (
        (ram_gib / 2.0).clamp(DISK_ELEVATED_FLOOR_GIB, MAX_DISK_GIB),
        (ram_gib / 4.0).clamp(DISK_SEVERE_FLOOR_GIB, MAX_DISK_GIB),
    )
}

/// The inputs in the order their stored keys are written and read.
const KEYS: [&str; 6] = [
    "swap",
    "compressor_segments",
    "load",
    "thermal",
    "battery",
    "disk",
];

/// How far a ratio threshold may be set.
const MAX_SWAP_RATIO: f64 = 8.0;
const MAX_LOAD_PER_CPU: f64 = 256.0;
const MAX_DISK_GIB: f64 = 1_048_576.0;

fn rule_problem(name: &str, rule: &Rule, max: f64, lower_is_worse: bool) -> Option<String> {
    let (elevated, severe) = (rule.elevated_at, rule.severe_at);
    if !elevated.is_finite() || !severe.is_finite() {
        return Some(format!("{name}: thresholds must be finite numbers"));
    }
    if elevated <= 0.0 || severe <= 0.0 || elevated > max || severe > max {
        return Some(format!(
            "{name}: thresholds must be above 0 and at most {max}"
        ));
    }
    if lower_is_worse && severe > elevated {
        return Some(format!(
            "{name}: the severe threshold must not be above the elevated one (lower is worse)"
        ));
    }
    if !lower_is_worse && severe < elevated {
        return Some(format!(
            "{name}: the severe threshold must not be below the elevated one"
        ));
    }
    None
}

impl Thresholds {
    /// Whether nothing is stored for the inputs `host_load_thresholds` holds.
    /// The disk guards are stored apart ([`DiskGuard::to_stored`]).
    pub fn is_default(&self) -> bool {
        self.to_stored().is_empty()
    }

    /// These thresholds with the automatic disk floors worked out for a
    /// machine with `memsize` bytes of memory. What a surface that has no
    /// readings (Settings) shows, and what the pure path decides on.
    pub fn resolved(&self, memsize: Reading<u64>) -> Self {
        let mut out = self.clone();
        if self.disk_auto {
            let (elevated, severe) = auto_disk_floors(memsize.value());
            out.disk.elevated_at = elevated;
            out.disk.severe_at = severe;
        }
        out
    }

    /// The first problem with any rule, or `None`. Used by the save path so a
    /// bad value is refused with a reason; a stored one is read leniently
    /// ([`Thresholds::from_stored`]).
    pub fn problem(&self) -> Option<String> {
        self.problems()
            .into_iter()
            .chain(self.guard_problems())
            .next()
    }

    fn problems(&self) -> Vec<String> {
        [
            rule_problem("swap", &self.swap, MAX_SWAP_RATIO, false),
            rule_problem("compressor_segments", &self.compressor_segments, 1.0, false),
            rule_problem("load", &self.load, MAX_LOAD_PER_CPU, false),
            // Automatic floors are never read from `disk`, so what it holds
            // then cannot be wrong.
            if self.disk_auto {
                None
            } else {
                rule_problem("disk", &self.disk, MAX_DISK_GIB, true)
            },
        ]
        .into_iter()
        .flatten()
        .collect()
    }

    fn guard_problems(&self) -> Vec<String> {
        [
            fill_problem(&self.disk_guard.fill),
            coverage_problem(&self.disk_guard.coverage),
        ]
        .into_iter()
        .flatten()
        .collect()
    }

    /// Reads a stored block leniently, one input at a time: an input that
    /// cannot be read or fails validation falls back to its own default with
    /// a warning, never costing the other inputs, and a block that is not an
    /// object is all defaults. Unknown keys are ignored.
    pub fn from_stored(raw: serde_json::Value) -> Self {
        let mut out = Self::default();
        let serde_json::Value::Object(map) = raw else {
            log::warn!(
                target: "tool_config",
                "ignoring unreadable host_load_thresholds block, every input keeps its default"
            );
            return out;
        };
        for key in KEYS {
            let Some(value) = map.get(key) else { continue };
            let mut candidate = out.clone();
            let parsed = match key {
                "swap" => serde_json::from_value(value.clone()).map(|r| candidate.swap = r),
                "compressor_segments" => {
                    serde_json::from_value(value.clone()).map(|r| candidate.compressor_segments = r)
                }
                "load" => serde_json::from_value(value.clone()).map(|r| candidate.load = r),
                "thermal" => serde_json::from_value(value.clone()).map(|r| candidate.thermal = r),
                "battery" => serde_json::from_value(value.clone()).map(|r| candidate.battery = r),
                _ => serde_json::from_value(value.clone()).map(|r| {
                    candidate.disk = r;
                    // A value stored before the floors scaled has no `auto`:
                    // a person set it, so it stays theirs.
                    candidate.disk_auto =
                        value.get("auto").and_then(serde_json::Value::as_bool) == Some(true);
                }),
            };
            if let Err(error) = parsed {
                log::warn!(
                    target: "tool_config",
                    "ignoring unreadable host_load_thresholds.{key}, it keeps its default: {error}"
                );
                continue;
            }
            if let Some(problem) = candidate
                .problems()
                .into_iter()
                .find(|p| p.starts_with(key))
            {
                log::warn!(
                    target: "tool_config",
                    "ignoring invalid host_load_thresholds.{key}, it keeps its default: {problem}"
                );
                continue;
            }
            out = candidate;
        }
        out
    }

    /// What is written to disk: only the inputs that differ from their
    /// default, so a later build that changes a default reaches a person who
    /// never chose a value for that input.
    pub fn to_stored(&self) -> serde_json::Map<String, serde_json::Value> {
        let base = Self::default();
        let mut out = serde_json::Map::new();
        let mut put = |key: &str, differs: bool, value: serde_json::Result<serde_json::Value>| {
            if let (true, Ok(value)) = (differs, value) {
                out.insert(key.to_string(), value);
            }
        };
        put(
            "swap",
            self.swap != base.swap,
            serde_json::to_value(self.swap),
        );
        put(
            "compressor_segments",
            self.compressor_segments != base.compressor_segments,
            serde_json::to_value(self.compressor_segments),
        );
        put(
            "load",
            self.load != base.load,
            serde_json::to_value(self.load),
        );
        put(
            "thermal",
            self.thermal != base.thermal,
            serde_json::to_value(self.thermal),
        );
        put(
            "battery",
            self.battery != base.battery,
            serde_json::to_value(self.battery),
        );
        // Automatic floors store only a changed mode, with the fixed numbers
        // a build that predates them reads, and `auto` so this one does not
        // take them for a choice. Floors a person set are always stored, even
        // when they equal the old defaults: on a large Mac those are not the
        // automatic ones.
        let disk = if self.disk_auto {
            (self.disk.mode != base.disk.mode).then(|| {
                serde_json::to_value(Rule {
                    mode: self.disk.mode,
                    elevated_at: DISK_ELEVATED_FLOOR_GIB,
                    severe_at: DISK_SEVERE_FLOOR_GIB,
                })
                .map(|mut value| {
                    value["auto"] = serde_json::Value::Bool(true);
                    value
                })
            })
        } else {
            Some(serde_json::to_value(self.disk))
        };
        if let Some(Ok(value)) = disk {
            out.insert("disk".to_string(), value);
        }
        out
    }
}

/// The host readings the thresholds decide on, copied out of a sensor sample.
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct Readings {
    pub swap_used: Reading<u64>,
    /// Physical memory: the denominator for swap.
    pub memsize: Reading<u64>,
    pub load: Reading<[f64; 3]>,
    pub cores: Reading<Cores>,
    pub thermal: Reading<Thermal>,
    pub power: Reading<Power>,
    pub free_disk: Reading<u64>,
    pub segments: Reading<CompressorSegments>,
    /// Bytes of free disk lost per minute, as the writer's tracker measured it
    /// (`diskrate`): not part of a host sample, because one sample cannot have
    /// a rate. `NotYet` while the tracker warms up or after a gap.
    pub disk_fill: Reading<u64>,
}

impl Readings {
    pub fn of(host: &super::sensor::HostSample) -> Self {
        Self {
            swap_used: host.swap_used,
            memsize: host.memsize,
            load: host.load,
            cores: host.cores,
            thermal: host.thermal,
            power: host.power,
            free_disk: host.free_disk,
            segments: host.compressor_segments,
            disk_fill: Reading::default(),
        }
    }

    /// A machine with plenty of headroom, for tests that are not about these
    /// readings: every one is readable and well inside every default.
    #[cfg(test)]
    pub(crate) fn calm() -> Self {
        Self {
            swap_used: Reading::Value(0),
            memsize: Reading::Value(64 << 30),
            load: Reading::Value([0.5, 0.5, 0.5]),
            cores: Reading::Value(Cores {
                logical: 18,
                levels: 2,
                perf0: 6,
                perf1: 12,
            }),
            thermal: Reading::Value(Thermal::Nominal),
            power: Reading::Value(Power::Ac),
            free_disk: Reading::Value(200 << 30),
            segments: Reading::Value(CompressorSegments {
                total: 1,
                limit: 100,
            }),
            disk_fill: Reading::Value(0),
        }
    }

    fn swap_ratio(&self) -> Result<f64, Gap> {
        let used = value_or_gap(self.swap_used)?;
        let memory = value_or_gap(self.memsize)?;
        if memory == 0 {
            return Err(Gap::Failed);
        }
        Ok(used as f64 / memory as f64)
    }

    fn segment_fraction(&self) -> Result<f64, Gap> {
        let segments = value_or_gap(self.segments)?;
        if segments.limit == 0 {
            return Err(Gap::Failed);
        }
        Ok(segments.total as f64 / segments.limit as f64)
    }

    fn load_per_cpu(&self) -> Result<f64, Gap> {
        let load = value_or_gap(self.load)?[0];
        let cpus = value_or_gap(self.cores)?.logical;
        if cpus == 0 || !load.is_finite() {
            return Err(Gap::Failed);
        }
        Ok(load / f64::from(cpus))
    }

    fn free_gib(&self) -> Result<f64, Gap> {
        Ok(value_or_gap(self.free_disk)? as f64 / GIB)
    }
}

const GIB: f64 = (1u64 << 30) as f64;

/// Why a reading gave no number.
enum Gap {
    /// The platform has no such reading, or the writer did not publish it:
    /// nothing was attempted, so nothing is warned about.
    Silent,
    /// It was attempted and could not be taken.
    Failed,
}

fn value_or_gap<T: Copy>(reading: Reading<T>) -> Result<T, Gap> {
    match reading {
        Reading::Value(v) => Ok(v),
        Reading::Unavailable(Why::Unsupported | Why::NotYet) => Err(Gap::Silent),
        Reading::Unavailable(Why::Unreadable | Why::Failed) => Err(Gap::Failed),
    }
}

/// One input's tier, in the verdict's tie-break order.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Contribution {
    pub binding: Binding,
    pub tier: Tier,
}

/// What the thresholds make of one set of readings.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct Assessment {
    /// Compressor segments, swap, low disk, thermal, CPU load, battery: the
    /// order that breaks a tie between inputs at the same tier.
    pub contributions: Vec<Contribution>,
    /// One per input that is governed on and could not be read.
    pub warnings: Vec<String>,
}

fn high_tier(value: f64, rule: &Rule) -> Tier {
    if value >= rule.severe_at {
        Tier::Severe
    } else if value >= rule.elevated_at {
        Tier::Elevated
    } else {
        Tier::Clear
    }
}

fn low_tier(value: f64, rule: &Rule) -> Tier {
    if value < rule.severe_at {
        Tier::Severe
    } else if value < rule.elevated_at {
        Tier::Elevated
    } else {
        Tier::Clear
    }
}

/// A fall slower than this is noise, not a trend worth a projection.
const MIN_FILL_BYTES_PER_MIN: u64 = 256 << 20;

/// Where the disk is heading: the rate it loses space at and how long that
/// leaves before the severe floor.
#[derive(Debug, Clone, Copy, PartialEq, Serialize)]
pub struct DiskFill {
    /// Bytes of free space lost per minute.
    pub rate_bytes_per_min: u64,
    /// Minutes until free space reaches the severe floor at that rate; 0 when
    /// it is there already.
    pub eta_min: f64,
    pub free_bytes: u64,
    pub floor_bytes: u64,
    /// The tier the projection alone reaches, before the rule's mode caps it.
    pub tier: Tier,
}

impl Thresholds {
    /// The projection from the writer's measured rate, for these (already
    /// resolved) thresholds. `Ok(None)` when the disk is not falling fast
    /// enough to project.
    fn fill_projection(&self, readings: &Readings) -> Result<Option<DiskFill>, Gap> {
        let rate = value_or_gap(readings.disk_fill)?;
        let free = value_or_gap(readings.free_disk)?;
        if rate < MIN_FILL_BYTES_PER_MIN {
            return Ok(None);
        }
        let floor = self.disk.severe_at * GIB;
        let eta_min = if (free as f64) <= floor {
            0.0
        } else {
            (free as f64 - floor) / rate as f64
        };
        let guard = &self.disk_guard.fill;
        let tier = if eta_min < guard.severe_within_min {
            Tier::Severe
        } else if eta_min < guard.elevated_within_min {
            Tier::Elevated
        } else {
            Tier::Clear
        };
        Ok(Some(DiskFill {
            rate_bytes_per_min: rate,
            eta_min,
            free_bytes: free,
            floor_bytes: floor as u64,
            tier,
        }))
    }

    /// What Health shows of the fill rule: the projection, when the rule is on
    /// and the projection is at least elevated.
    pub fn disk_fill(&self, readings: &Readings) -> Option<DiskFill> {
        if self.disk_guard.fill.mode == InputMode::Off {
            return None;
        }
        self.resolved(readings.memsize)
            .fill_projection(readings)
            .ok()
            .flatten()
            .filter(|fill| fill.tier >= Tier::Elevated)
    }

    /// Each input's tier (capped by its mode) and the warnings for inputs
    /// that are governed on but unreadable. Pure.
    pub fn assess(&self, readings: &Readings) -> Assessment {
        let resolved = self.resolved(readings.memsize);
        let this = &resolved;
        let mut out = Assessment::default();
        let mut take = |binding: Binding, mode: InputMode, label: &str, tier: Result<Tier, Gap>| {
            if mode == InputMode::Off {
                return;
            }
            match tier {
                Ok(tier) => out.contributions.push(Contribution {
                    binding,
                    tier: mode.cap(tier),
                }),
                Err(Gap::Failed) => out
                    .warnings
                    .push(format!("{label} is unreadable: not governed on")),
                Err(Gap::Silent) => {}
            }
        };
        take(
            Binding::CompressorSegments,
            self.compressor_segments.mode,
            "the compressor's segment use",
            readings
                .segment_fraction()
                .map(|v| high_tier(v, &self.compressor_segments)),
        );
        take(
            Binding::Swap,
            self.swap.mode,
            "swap",
            readings.swap_ratio().map(|v| high_tier(v, &self.swap)),
        );
        take(
            Binding::LowDisk,
            this.disk.mode,
            "free disk space",
            readings.free_gib().map(|v| low_tier(v, &this.disk)),
        );
        // An unreadable disk is one failure, said once: by the level rule
        // when it is on, by this one only when it is the only one that is.
        let projection = match this.fill_projection(readings) {
            Err(Gap::Failed)
                if this.disk.mode != InputMode::Off && readings.free_gib().is_err() =>
            {
                Err(Gap::Silent)
            }
            other => other,
        };
        take(
            Binding::DiskFilling,
            this.disk_guard.fill.mode,
            "the disk fill rate",
            projection.map(|fill| fill.map_or(Tier::Clear, |fill| fill.tier)),
        );
        take(
            Binding::Thermal,
            self.thermal.mode,
            "the thermal state",
            value_or_gap(readings.thermal).map(|t| match t {
                Thermal::Serious => Tier::Elevated,
                Thermal::Critical => Tier::Severe,
                Thermal::Nominal | Thermal::Fair => Tier::Clear,
            }),
        );
        take(
            Binding::CpuLoad,
            self.load.mode,
            "CPU load",
            readings.load_per_cpu().map(|v| high_tier(v, &self.load)),
        );
        take(
            Binding::Battery,
            self.battery.mode,
            "the power source",
            value_or_gap(readings.power).map(|p| match p {
                Power::Battery => Tier::Elevated,
                Power::Ac => Tier::Clear,
            }),
        );
        out
    }

    /// The resource, its value and the threshold it crossed, in words, for
    /// the new bindings (`None` for the kernel's own two). `held` is the tier
    /// the hysteresis holds, which names the threshold; while the tier is
    /// still held after the reading recovered, the sentence says so rather
    /// than claiming a threshold is crossed.
    pub fn describe(&self, binding: Binding, readings: &Readings, held: Tier) -> Option<String> {
        let pick = |rule: &Rule| match held {
            Tier::Severe => rule.severe_at,
            _ => rule.elevated_at,
        };
        let crossing = |now: bool, at: String| {
            if now {
                format!("at or above {at}")
            } else {
                format!("now back under {at}, held until it stays clear")
            }
        };
        Some(match binding {
            Binding::MemoryPressure | Binding::CompressorThrash => return None,
            Binding::Swap => {
                let at = pick(&self.swap);
                let ratio = readings.swap_ratio().ok()?;
                let used = readings.swap_used.value()?;
                let memory = readings.memsize.value()?;
                format!(
                    "swap used is {ratio:.2}x physical memory ({} of {}), {}",
                    gib(used),
                    gib(memory),
                    crossing(ratio >= at, format!("{at:.2}x"))
                )
            }
            Binding::CompressorSegments => {
                let at = pick(&self.compressor_segments);
                let fraction = readings.segment_fraction().ok()?;
                let segments = readings.segments.value()?;
                format!(
                    "the memory compressor is using {fraction:.2} of its segment limit ({} of {} segments), {}",
                    segments.total,
                    segments.limit,
                    crossing(fraction >= at, format!("{at:.2}"))
                )
            }
            Binding::CpuLoad => {
                let at = pick(&self.load);
                let per_cpu = readings.load_per_cpu().ok()?;
                let load = readings.load.value()?[0];
                let cpus = readings.cores.value()?.logical;
                format!(
                    "the 1-minute load is {per_cpu:.2} per CPU ({load:.1} on {cpus} CPUs), {}",
                    crossing(per_cpu >= at, format!("{at:.2}"))
                )
            }
            Binding::DiskFilling => {
                let this = self.resolved(readings.memsize);
                let horizon = match held {
                    Tier::Severe => this.disk_guard.fill.severe_within_min,
                    _ => this.disk_guard.fill.elevated_within_min,
                };
                let Some(fill) = this.fill_projection(readings).ok().flatten() else {
                    return Some(
                        "free disk space is no longer falling fast enough to project, held until it stays clear"
                            .to_string(),
                    );
                };
                let (free, floor) = (fill.free_bytes as f64 / GIB, fill.floor_bytes as f64 / GIB);
                let rate = fill.rate_bytes_per_min as f64 / GIB;
                let verdict = if fill.eta_min < horizon {
                    format!("inside the {horizon:.0} minute horizon")
                } else {
                    format!(
                        "now outside the {horizon:.0} minute horizon, held until it stays clear"
                    )
                };
                format!(
                    "free disk space is {free:.1} GiB and falling by {rate:.1} GiB a minute, about {:.0} minutes until the {floor:.0} GiB floor, {verdict}",
                    fill.eta_min
                )
            }
            Binding::LowDisk => {
                let this = self.resolved(readings.memsize);
                let at = match held {
                    Tier::Severe => this.disk.severe_at,
                    _ => this.disk.elevated_at,
                };
                let free = readings.free_gib().ok()?;
                let verdict = if free < at {
                    format!("below {at:.1} GiB")
                } else {
                    format!("now back above {at:.1} GiB, held until it stays clear")
                };
                format!("free disk space is {free:.1} GiB, {verdict}")
            }
            Binding::Thermal => match readings.thermal.value()? {
                Thermal::Critical => "the thermal state is critical".to_string(),
                Thermal::Serious => "the thermal state is serious".to_string(),
                other => {
                    format!("the thermal state is {other:?} (was serious or worse)").to_lowercase()
                }
            },
            Binding::Battery => match readings.power.value()? {
                Power::Battery => "the computer is running on battery".to_string(),
                Power::Ac => {
                    "the computer is back on mains power (held until it stays so)".to_string()
                }
            },
        })
    }
}

fn gib(bytes: u64) -> String {
    format!("{:.1} GiB", bytes as f64 / GIB)
}

#[cfg(test)]
mod tests;
