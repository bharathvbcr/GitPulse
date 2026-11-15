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
    /// Free disk, in GiB; lower is worse.
    pub disk: Rule,
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
                elevated_at: 20.0,
                severe_at: 10.0,
            },
        }
    }
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
    pub fn is_default(&self) -> bool {
        *self == Self::default()
    }

    /// The first problem with any rule, or `None`. Used by the save path so a
    /// bad value is refused with a reason; a stored one is read leniently
    /// ([`Thresholds::from_stored`]).
    pub fn problem(&self) -> Option<String> {
        self.problems().into_iter().next()
    }

    fn problems(&self) -> Vec<String> {
        [
            rule_problem("swap", &self.swap, MAX_SWAP_RATIO, false),
            rule_problem(
                "compressor_segments",
                &self.compressor_segments,
                1.0,
                false,
            ),
            rule_problem("load", &self.load, MAX_LOAD_PER_CPU, false),
            rule_problem("disk", &self.disk, MAX_DISK_GIB, true),
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
                _ => serde_json::from_value(value.clone()).map(|r| candidate.disk = r),
            };
            if let Err(error) = parsed {
                log::warn!(
                    target: "tool_config",
                    "ignoring unreadable host_load_thresholds.{key}, it keeps its default: {error}"
                );
                continue;
            }
            if let Some(problem) = candidate.problems().into_iter().find(|p| p.starts_with(key)) {
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
        put(
            "disk",
            self.disk != base.disk,
            serde_json::to_value(self.disk),
        );
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

impl Thresholds {
    /// Each input's tier (capped by its mode) and the warnings for inputs
    /// that are governed on but unreadable. Pure.
    pub fn assess(&self, readings: &Readings) -> Assessment {
        let mut out = Assessment::default();
        let mut take = |binding: Binding,
                        mode: InputMode,
                        label: &str,
                        tier: Result<Tier, Gap>| {
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
            self.disk.mode,
            "free disk space",
            readings.free_gib().map(|v| low_tier(v, &self.disk)),
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
            Binding::LowDisk => {
                let at = match held {
                    Tier::Severe => self.disk.severe_at,
                    _ => self.disk.elevated_at,
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
                other => format!("the thermal state is {other:?} (was serious or worse)")
                    .to_lowercase(),
            },
            Binding::Battery => match readings.power.value()? {
                Power::Battery => "the computer is running on battery".to_string(),
                Power::Ac => "the computer is back on mains power (held until it stays so)"
                    .to_string(),
            },
        })
    }
}

fn gib(bytes: u64) -> String {
    format!("{:.1} GiB", bytes as f64 / GIB)
}

#[cfg(test)]
mod tests;
