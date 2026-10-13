//! The load governor's one verdict, and the two thin callers built on it.
//!
//! The sensor ([`super::sampler`]) measures and publishes. This module is the
//! only place that turns what it published into a decision, and every consumer
//! asks it rather than reading pressure itself:
//!
//! * **Admission (card 1b):** [`admit`] is called before an agent launch's
//!   checkout probes, its capacity check and its PTY spawn. It refuses only on
//!   memory pressure, and fails open on everything else.
//! * **Shedding (card 1c):** the spawn gate (`engine::git_cli`) asks
//!   [`shed`] which of its own admission classes to refuse, and refuses them
//!   with the deferral marker the frontend already retries on. Nothing here
//!   schedules anything.
//! * Cards 3 and 4 consume [`decide`] and [`Input::from_report`] the same way:
//!   a pure function over a sensor report, testable with the replay sensor.
//!
//! # What binds
//!
//! Two inputs, each named in the reason and in the ledger so the person
//! reading a refusal learns which resource and what value:
//!
//! * [`Binding::MemoryPressure`]: the kernel's pressure level.
//! * [`Binding::CompressorThrash`]: the sensor's `compressor_thrash` flag. The
//!   2026-10-07 panic was compressor-segment exhaustion while the pressure
//!   level lagged, so the level alone would have admitted into it.
//!
//! `warn` sheds background work and still admits a launch. `critical`, or a
//! thrashing compressor, also sheds reactive work and refuses launches.
//! Interactive work (a click: commit, save, stop, kill, cancel, release,
//! reconcile) is never shed and never refused: [`Site`] lists the only places
//! [`admit`] is called, and none of those verbs is in it.
//!
//! # Hysteresis
//!
//! A decision counts sensor ticks, as the sensor's contract says downstream
//! hysteresis must: a tier is entered after [`ENTER_SAMPLES`] consecutive
//! samples at or above it and left after [`EXIT_SAMPLES`] consecutive samples
//! below it. A level flapping at a threshold therefore neither flaps the
//! verdict nor escalates it.
//!
//! # Sensor states
//!
//! Fresh and stale-recent (at most 120 s) govern on the last-known level, with
//! a "stale Ns" warning. Stale-old and dead admit with a warning and no extra
//! shedding: fail open, and the ledger records `host_load.sensor_unavailable`.
//!
//! # A refusal never quietly becomes an allow
//!
//! After [`REFUSAL_NOTICE_AFTER`] of continuous pressure refusal the governor
//! raises a notice (the pulse carries it) with a one-click setting flip. The
//! verdict itself does not change: only a person flipping
//! `host_load.govern_launches`, or answering Yes to "Admit anyway" for
//! [`OVERRIDE_WINDOW`], admits what pressure refuses. There is no agent
//! override.

use super::{Level, Platform, SensorReport, SensorState, Snapshot};
use serde::Serialize;
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant};

/// Consecutive samples at or above a tier before it is entered.
pub const ENTER_SAMPLES: u32 = 3;
/// Consecutive samples below the held tier before it is left.
pub const EXIT_SAMPLES: u32 = 5;
/// Continuous pressure refusal after which the person is told.
pub const REFUSAL_NOTICE_AFTER: Duration = Duration::from_secs(30 * 60);
/// How long a person's "Admit anyway" lasts.
pub const OVERRIDE_WINDOW: Duration = Duration::from_secs(120);

/// The resource whose value is the reason.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Binding {
    /// The kernel's memory-pressure level.
    MemoryPressure,
    /// `compressor_thrash`: thrashing or pressure swapouts rising over three
    /// samples.
    CompressorThrash,
}

/// How hard the machine is pressed, as the verdict sees it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Tier {
    Clear,
    /// Pressure `warn`: shed background work.
    Elevated,
    /// Pressure `critical` or a thrashing compressor: shed reactive work too
    /// and refuse launches.
    Severe,
}

/// Why the verdict is what it is.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Reason {
    Healthy,
    /// Pressure `warn`: background work is shed, launches are admitted.
    Elevated,
    /// Severe pressure: launches are refused and reactive work is shed.
    Pressure,
    /// Severe pressure, but the person switched launch refusal off.
    LaunchGateOff,
    /// Stale-old, dead, or an unreadable level: admitted with a warning.
    SensorUnavailable,
    /// `host_load.enabled` is off.
    SwitchedOff,
    /// A platform (or process) the governor does not govern.
    NotGoverned,
}

impl Reason {
    /// Whether an admission under this reason leaves a ledger record.
    /// Switched off and ungoverned are states, not decisions.
    fn recorded(self) -> bool {
        !matches!(self, Self::SwitchedOff | Self::NotGoverned)
    }
}

/// Which of the spawn gate's classes are refused. Interactive is never in it.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize)]
pub struct Shed {
    pub background: bool,
    pub reactive: bool,
}

/// What the governor decides from, copied out of a sensor report so the
/// decision is a pure function of it.
#[derive(Debug, Clone, PartialEq)]
pub struct Input {
    pub platform: Platform,
    pub state: SensorState,
    pub age_ms: Option<u64>,
    /// The last-known pressure level, whatever the state.
    pub level: Option<Level>,
    /// `host_load.enabled`, as the writer published it.
    pub enabled: bool,
    pub compressor_thrash: bool,
    /// Last sample's rise in thrashing wakeups and pressure swapouts.
    pub thrash_wakeups: Option<u64>,
    pub swapouts: Option<u64>,
    /// The heaviest process GitPulse did not start, for the refusal notice.
    pub largest_foreign: Option<u64>,
    /// `host_load.govern_launches`: whether severe pressure refuses launches.
    pub govern_launches: bool,
}

impl Input {
    /// From what a reader learns from the snapshot.
    pub fn from_report(
        report: &SensorReport,
        snapshot: Option<&Snapshot>,
        govern_launches: bool,
    ) -> Self {
        Self {
            platform: snapshot.map_or_else(Platform::current, |s| s.platform),
            state: report.state,
            age_ms: report.age_ms,
            level: report.level,
            enabled: report.enabled.unwrap_or(false),
            compressor_thrash: snapshot.is_some_and(|s| s.compressor_thrash),
            thrash_wakeups: snapshot
                .and_then(|s| s.compressor_delta.value())
                .map(|d| d.thrashing_detected),
            swapouts: snapshot
                .and_then(|s| s.compressor_delta.value())
                .map(|d| d.swapouts_pressure),
            largest_foreign: snapshot.and_then(|s| s.largest_foreign.value()),
            govern_launches,
        }
    }

    /// This input `extra` later: the age grows, the state follows it.
    fn aged(&self, extra: Duration) -> Self {
        let mut aged = self.clone();
        if let Some(age) = self.age_ms {
            let age = age.saturating_add(u64::try_from(extra.as_millis()).unwrap_or(u64::MAX));
            aged.age_ms = Some(age);
            aged.state = staler(SensorState::from_age_ms(age), self.state);
        }
        aged
    }
}

/// The staler of two states, so ageing never makes a reading look fresher.
fn staler(a: SensorState, b: SensorState) -> SensorState {
    fn rank(state: SensorState) -> u8 {
        match state {
            SensorState::Fresh => 0,
            SensorState::StaleRecent => 1,
            SensorState::StaleOld => 2,
            SensorState::Dead => 3,
        }
    }
    if rank(b) > rank(a) {
        b
    } else {
        a
    }
}

/// Whether the input can be governed on at all, and if not, why.
enum Availability {
    Governing,
    Off(Reason, &'static str),
    Unavailable(String),
}

fn availability(input: &Input) -> Availability {
    if !input.enabled && input.state.governs() {
        return Availability::Off(Reason::SwitchedOff, "host-load measurement is switched off");
    }
    if input.platform != Platform::Governed {
        return Availability::Off(
            Reason::NotGoverned,
            "this platform is advised on, never governed",
        );
    }
    match (input.state, input.level) {
        (SensorState::StaleOld | SensorState::Dead, _) => Availability::Unavailable(format!(
            "the load sensor is {}: memory pressure is unknown, so nothing is shed or refused",
            match input.state {
                SensorState::Dead => "not running".to_string(),
                _ => input
                    .age_ms
                    .map_or("stale".to_string(), |ms| format!("stale {}s", ms / 1000)),
            }
        )),
        (_, None) => Availability::Unavailable(
            "memory pressure is unreadable, so nothing is shed or refused".to_string(),
        ),
        _ => Availability::Governing,
    }
}

/// One sample's tier and the resource that put it there.
fn sample(input: &Input) -> Option<(Tier, Option<Binding>)> {
    if !matches!(availability(input), Availability::Governing) {
        return None;
    }
    if input.level == Some(Level::Critical) {
        return Some((Tier::Severe, Some(Binding::MemoryPressure)));
    }
    if input.compressor_thrash {
        return Some((Tier::Severe, Some(Binding::CompressorThrash)));
    }
    if input.level == Some(Level::Warn) {
        return Some((Tier::Elevated, Some(Binding::MemoryPressure)));
    }
    Some((Tier::Clear, None))
}

fn level_words(level: Option<Level>) -> String {
    match level {
        Some(Level::Critical) => "critical (kernel level 4)".to_string(),
        Some(Level::Warn) => "warn (kernel level 2)".to_string(),
        Some(Level::Normal) => "normal (kernel level 1)".to_string(),
        None => "unreadable".to_string(),
    }
}

/// The binding resource and its value, in words.
fn describe(binding: Binding, input: &Input) -> String {
    match binding {
        Binding::MemoryPressure => format!("memory pressure {}", level_words(input.level)),
        Binding::CompressorThrash => {
            let wakeups = input
                .thrash_wakeups
                .map_or("unknown".to_string(), |n| format!("+{n}"));
            let swapouts = input
                .swapouts
                .map_or("unknown".to_string(), |n| format!("+{n}"));
            format!(
                "the memory compressor is thrashing ({wakeups} thrash wakeups, {swapouts} pressure swapouts in the last sample; memory pressure {})",
                level_words(input.level)
            )
        }
    }
}

/// What the governor says.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct GovernorVerdict {
    /// Whether an agent launch is admitted.
    pub admit: bool,
    pub shed: Shed,
    pub reason: Reason,
    pub tier: Tier,
    pub binding: Option<Binding>,
    pub sensor: SensorState,
    pub age_ms: Option<u64>,
    /// One sentence naming the binding resource and its value, or what is
    /// unknown.
    pub message: String,
    /// Admitted-with-a-caveat notes: stale age, unreadable sensor.
    pub warnings: Vec<String>,
}

/// The decision for one input, given the tier the hysteresis holds.
///
/// Pure: the same input and held tier always give the same verdict. A reader
/// with no sample history (a hook, a CLI) holds the tier the single sample
/// gives, see [`Governor::instant`].
pub fn decide(input: &Input, held: Tier, held_binding: Option<Binding>) -> GovernorVerdict {
    let base =
        |reason, admit, tier, binding, message: String, warnings: Vec<String>| GovernorVerdict {
            admit,
            shed: Shed::default(),
            reason,
            tier,
            binding,
            sensor: input.state,
            age_ms: input.age_ms,
            message,
            warnings,
        };
    match availability(input) {
        Availability::Off(reason, why) => {
            return base(reason, true, Tier::Clear, None, why.to_string(), Vec::new())
        }
        Availability::Unavailable(why) => {
            return base(
                Reason::SensorUnavailable,
                true,
                Tier::Clear,
                None,
                why.clone(),
                vec![why],
            )
        }
        Availability::Governing => {}
    }
    let mut warnings = Vec::new();
    if input.state == SensorState::StaleRecent {
        let seconds = input.age_ms.map_or(0, |ms| ms / 1000);
        warnings.push(format!(
            "stale {seconds}s: governing on the last-known level"
        ));
    }
    let binding = held_binding.unwrap_or(Binding::MemoryPressure);
    match held {
        Tier::Clear => base(
            Reason::Healthy,
            true,
            Tier::Clear,
            None,
            format!("memory pressure {}", level_words(input.level)),
            warnings,
        ),
        Tier::Elevated => {
            let mut verdict = base(
                Reason::Elevated,
                true,
                Tier::Elevated,
                Some(binding),
                format!(
                    "{}: background work is shed, launches are admitted",
                    describe(binding, input)
                ),
                warnings,
            );
            verdict.shed.background = true;
            verdict
        }
        Tier::Severe => {
            let what = describe(binding, input);
            let mut verdict = if input.govern_launches {
                base(
                    Reason::Pressure,
                    false,
                    Tier::Severe,
                    Some(binding),
                    format!("{what}: agent launches are refused until it clears"),
                    warnings,
                )
            } else {
                warnings.push("launch refusal is switched off in Settings".to_string());
                base(
                    Reason::LaunchGateOff,
                    true,
                    Tier::Severe,
                    Some(binding),
                    format!("{what}: background and reactive work is shed, launches are admitted"),
                    warnings,
                )
            };
            verdict.shed = Shed {
                background: true,
                reactive: true,
            };
            verdict
        }
    }
}

/// The hysteresis, the last sample, and the refusal clock: one per sampler.
#[derive(Debug)]
pub struct Governor {
    held: Tier,
    binding: Option<Binding>,
    up_streak: u32,
    /// The lowest tier, and its binding, of the current run above the held
    /// one: what the run enters.
    up: (Tier, Option<Binding>),
    down_streak: u32,
    /// The highest tier, and its binding, of the current run below the held
    /// one: what the run leaves to.
    down: (Tier, Option<Binding>),
    last: Option<(Instant, Input)>,
    refusing_since: Option<Instant>,
    noticed: bool,
    override_until: Option<Instant>,
}

impl Default for Governor {
    fn default() -> Self {
        Self {
            held: Tier::Clear,
            binding: None,
            up_streak: 0,
            up: (Tier::Severe, None),
            down_streak: 0,
            down: (Tier::Clear, None),
            last: None,
            refusing_since: None,
            noticed: false,
            override_until: None,
        }
    }
}

/// What one observation produced.
#[derive(Debug, Clone, PartialEq)]
pub struct Observation {
    pub verdict: GovernorVerdict,
    /// True exactly once per continuous refusal, on the observation that
    /// crosses [`REFUSAL_NOTICE_AFTER`].
    pub notice_raised: bool,
}

impl Governor {
    /// A governor holding the tier one sample gives, for a reader with no
    /// history. `now` is when the sample was taken.
    pub fn instant(now: Instant, input: Input) -> Self {
        let mut governor = Self::default();
        if let Some((tier, binding)) = sample(&input) {
            governor.held = tier;
            governor.binding = binding;
        }
        governor.last = Some((now, input));
        governor
    }

    /// Counts one sensor tick. `now` is monotonic; one that is earlier than
    /// the previous observation is treated as equal to it, so a clock that
    /// steps back can neither shorten nor lengthen a run.
    pub fn observe(&mut self, now: Instant, input: Input) -> Observation {
        self.observe_counting(now, input, true)
    }

    /// A tick that read the same snapshot as the last one: a reader whose
    /// writer has not published since. The reading ages and the refusal clock
    /// runs, but nothing is counted, so one sample read three times can never
    /// enter a tier that needs three samples.
    pub fn observe_repeat(&mut self, now: Instant, input: Input) -> Observation {
        self.observe_counting(now, input, false)
    }

    fn observe_counting(&mut self, now: Instant, input: Input, counts: bool) -> Observation {
        let now = self.last.as_ref().map_or(now, |(at, _)| now.max(*at));
        match sample(&input) {
            // Nothing to govern on: forget the run rather than carry a trip
            // from before the sensor went quiet into whenever it returns.
            None => {
                self.held = Tier::Clear;
                self.binding = None;
                self.up_streak = 0;
                self.down_streak = 0;
            }
            Some((tier, binding)) if counts => self.count(tier, binding),
            Some(_) => {}
        }
        self.last = Some((now, input));
        let verdict = self.verdict(now);
        let mut notice_raised = false;
        if verdict.reason == Reason::Pressure {
            let since = *self.refusing_since.get_or_insert(now);
            if !self.noticed && now.saturating_duration_since(since) >= REFUSAL_NOTICE_AFTER {
                self.noticed = true;
                notice_raised = true;
            }
        } else {
            self.refusing_since = None;
            self.noticed = false;
        }
        Observation {
            verdict,
            notice_raised,
        }
    }

    fn count(&mut self, tier: Tier, binding: Option<Binding>) {
        use std::cmp::Ordering;
        match tier.cmp(&self.held) {
            Ordering::Greater => {
                self.down_streak = 0;
                if self.up_streak == 0 || tier < self.up.0 {
                    self.up = (tier, binding);
                }
                self.up_streak += 1;
                if self.up_streak >= ENTER_SAMPLES {
                    (self.held, self.binding) = self.up;
                    self.up_streak = 0;
                }
            }
            Ordering::Less => {
                self.up_streak = 0;
                // The run leaves to the highest tier it held throughout, as
                // it enters the lowest: four warn samples and one clear leave
                // severe for elevated, not for clear.
                if self.down_streak == 0 || tier > self.down.0 {
                    self.down = (tier, binding);
                }
                self.down_streak += 1;
                if self.down_streak >= EXIT_SAMPLES {
                    (self.held, self.binding) = self.down;
                    self.down_streak = 0;
                }
            }
            Ordering::Equal => {
                self.up_streak = 0;
                self.down_streak = 0;
                if tier != Tier::Clear {
                    self.binding = binding;
                }
            }
        }
    }

    /// The verdict at `now`, the last input aged to it: a sampler that stops
    /// ticking walks the sensor through stale-recent, stale-old and dead
    /// while this is asked.
    pub fn verdict(&self, now: Instant) -> GovernorVerdict {
        match &self.last {
            None => decide(
                &Input {
                    platform: Platform::current(),
                    state: SensorState::Dead,
                    age_ms: None,
                    level: None,
                    enabled: true,
                    compressor_thrash: false,
                    thrash_wakeups: None,
                    swapouts: None,
                    largest_foreign: None,
                    govern_launches: true,
                },
                Tier::Clear,
                None,
            ),
            Some((at, input)) => {
                let aged = input.aged(now.saturating_duration_since(*at));
                // An input the ageing made ungovernable holds no tier.
                let held = if sample(&aged).is_some() {
                    self.held
                } else {
                    Tier::Clear
                };
                decide(&aged, held, self.binding)
            }
        }
    }

    /// A person said "Admit anyway": launches pass for [`OVERRIDE_WINDOW`].
    pub fn grant_override(&mut self, now: Instant) {
        self.override_until = Some(now + OVERRIDE_WINDOW);
    }

    fn override_remaining(&self, now: Instant) -> Option<Duration> {
        self.override_until
            .map(|until| until.saturating_duration_since(now))
            .filter(|left| !left.is_zero())
    }

    /// Everything a surface shows: the verdict, how long refusal has run,
    /// whether the person has been told, and a live override.
    pub fn report(&self, now: Instant) -> GovernorReport {
        let verdict = self.verdict(now);
        // The refusal clock is advanced by ticks; a sampler that stopped
        // ticking ages the verdict out of refusing, and the run ends with it
        // rather than reading "refusing for an hour" beside an admit.
        let refusing = verdict.reason == Reason::Pressure;
        let refusing_for = self
            .refusing_since
            .filter(|_| refusing)
            .map(|since| now.saturating_duration_since(since));
        GovernorReport {
            verdict,
            refusing_for_s: refusing_for.map(|d| d.as_secs()),
            notice: self.noticed && refusing,
            largest_foreign: self
                .last
                .as_ref()
                .and_then(|(_, input)| input.largest_foreign),
            override_remaining_s: self.override_remaining(now).map(|d| d.as_secs().max(1)),
        }
    }
}

/// What Health and the pulse show about the governor.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct GovernorReport {
    pub verdict: GovernorVerdict,
    /// Seconds of continuous pressure refusal, `None` when not refusing.
    pub refusing_for_s: Option<u64>,
    /// Refusal has run past [`REFUSAL_NOTICE_AFTER`]: tell the person.
    pub notice: bool,
    /// The heaviest process GitPulse did not start, named in the notice.
    pub largest_foreign: Option<u64>,
    /// Seconds left of a person's "Admit anyway", when one is live.
    pub override_remaining_s: Option<u64>,
}

impl GovernorReport {
    /// The report of a process that does not govern.
    fn ungoverned() -> Self {
        Self {
            verdict: GovernorVerdict {
                admit: true,
                shed: Shed::default(),
                reason: Reason::NotGoverned,
                tier: Tier::Clear,
                binding: None,
                sensor: SensorState::Dead,
                age_ms: None,
                message: "this process does not run the load governor".to_string(),
                warnings: Vec::new(),
            },
            refusing_for_s: None,
            notice: false,
            largest_foreign: None,
            override_remaining_s: None,
        }
    }
}

/// The small, copyable part of a [`GovernorReport`] the per-tick pulse carries.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct GovernorPulse {
    pub admit: bool,
    pub shed_background: bool,
    pub shed_reactive: bool,
    pub binding: Option<Binding>,
    pub refusing_for_s: Option<u64>,
    pub notice: bool,
    pub overridden: bool,
    /// The heaviest process GitPulse did not start, bytes, for the notice.
    pub largest_foreign: Option<u64>,
}

impl GovernorPulse {
    pub fn of(report: &GovernorReport) -> Self {
        Self {
            admit: report.verdict.admit,
            shed_background: report.verdict.shed.background,
            shed_reactive: report.verdict.shed.reactive,
            binding: report.verdict.binding,
            refusing_for_s: report.refusing_for_s,
            notice: report.notice,
            overridden: report.override_remaining_s.is_some(),
            largest_foreign: report.largest_foreign,
        }
    }
}

/// The places [`admit`] is called: the only gated operations there are.
///
/// Stop, kill, cancel, save, commit, release and reconcile are deliberately
/// not here, so no pressure can refuse them; `tests::the_never_gated_verbs_have_no_site`
/// holds that, and `tests/governor_sites.rs` holds the call sites to this list.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Site {
    /// Before a launch's checkout probes (`git rev-parse` and friends).
    CheckoutProbe,
    /// Before a launch's capacity check and worktree build.
    EnsureRoom,
    /// Before an agent's PTY is opened.
    PtySpawn,
}

impl Site {
    /// Every gated site, for the contract test.
    pub const ALL: [Site; 3] = [Site::CheckoutProbe, Site::EnsureRoom, Site::PtySpawn];

    pub fn name(self) -> &'static str {
        match self {
            Self::CheckoutProbe => "checkout_probe",
            Self::EnsureRoom => "ensure_room",
            Self::PtySpawn => "pty_spawn",
        }
    }
}

/// An admitted launch step.
#[derive(Debug, Clone, PartialEq)]
pub struct Admitted {
    /// Why it was admitted with a caveat, when it was.
    pub warning: Option<String>,
    /// A person's "Admit anyway" admitted what pressure refuses.
    pub overridden: bool,
}

/// A refused launch step: typed, so a surface can offer Retry and
/// "Admit anyway" without parsing prose.
#[derive(Debug, Clone, PartialEq)]
pub struct Refused {
    pub site: Site,
    pub verdict: GovernorVerdict,
}

/// The error code a refused launch carries across IPC.
pub const REFUSAL_CODE: &str = "host_pressure";

impl Refused {
    pub fn message(&self) -> String {
        format!(
            "Agent launch refused ({}): {}. Retry when it clears, or choose Admit anyway in Health, Host load.",
            self.site.name(),
            self.verdict.message
        )
    }
}

/// The governor the app's sampler feeds, registered once by `start_app`.
static APP_GOVERNOR: OnceLock<Arc<Mutex<Governor>>> = OnceLock::new();

pub(super) fn register_app(governor: Arc<Mutex<Governor>>) -> bool {
    APP_GOVERNOR.set(governor).is_ok()
}

#[cfg(test)]
thread_local! {
    static INJECTED: std::cell::RefCell<Option<(Input, Governor)>> = const { std::cell::RefCell::new(None) };
}

/// Test seam: makes this thread's governor hold `input`, as one sample would,
/// until the guard drops. Thread-local so a parallel test that asks the
/// governor is never pressed by this one.
#[cfg(test)]
pub(crate) fn inject(input: Input) -> Injected {
    let governor = Governor::instant(Instant::now(), input.clone());
    INJECTED.with(|slot| *slot.borrow_mut() = Some((input, governor)));
    Injected
}

/// The input this thread's injection holds, for a test that must hand it to
/// the pool thread a command hops to (the slot is thread-local).
#[cfg(test)]
pub(crate) fn injected_input() -> Option<Input> {
    INJECTED.with(|slot| slot.borrow().as_ref().map(|(input, _)| input.clone()))
}

#[cfg(test)]
pub(crate) struct Injected;

#[cfg(test)]
impl Drop for Injected {
    fn drop(&mut self) {
        INJECTED.with(|slot| *slot.borrow_mut() = None);
    }
}

#[cfg(test)]
impl Input {
    /// A governed machine under `level`, sensor fresh, launches governed.
    pub(crate) fn test_pressure(level: Level) -> Self {
        Self {
            platform: Platform::Governed,
            state: SensorState::Fresh,
            age_ms: Some(0),
            level: Some(level),
            enabled: true,
            compressor_thrash: false,
            thrash_wakeups: None,
            swapouts: None,
            largest_foreign: None,
            govern_launches: true,
        }
    }
}

/// The current report in this process: the app's sampler's governor; in a
/// process that does not sample (a CLI, the MCP server, a hook) nothing is
/// governed here.
pub fn report() -> GovernorReport {
    let now = Instant::now();
    #[cfg(test)]
    if let Some(report) = INJECTED.with(|slot| slot.borrow().as_ref().map(|(_, g)| g.report(now))) {
        return report;
    }
    match APP_GOVERNOR.get() {
        Some(governor) => governor
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .report(now),
        None => GovernorReport::ungoverned(),
    }
}

/// Which of the spawn gate's classes to refuse right now. The gate asks this
/// per spawn, so it takes one short lock and does no IO.
pub fn shed() -> Shed {
    let now = Instant::now();
    #[cfg(test)]
    if let Some(shed) =
        INJECTED.with(|slot| slot.borrow().as_ref().map(|(_, g)| g.verdict(now).shed))
    {
        return shed;
    }
    // A process with no governor (the MCP server, the daemon, a hook) sheds
    // nothing, and says so without building a report for every spawn.
    APP_GOVERNOR.get().map_or_else(Shed::default, |governor| {
        governor
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .verdict(now)
            .shed
    })
}

/// A person's "Admit anyway": launches pass for [`OVERRIDE_WINDOW`]. Only the
/// Tauri command behind a native confirmation calls this; there is no agent
/// path to it.
pub fn grant_override() -> Result<(), String> {
    let Some(governor) = APP_GOVERNOR.get() else {
        return Err("the load governor is not running in this process".to_string());
    };
    governor
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .grant_override(Instant::now());
    Ok(())
}

/// A person's "Admit anyway": after a native Yes/No naming what is binding
/// (`confirm` is `desktop::confirm::native` in production, which webview
/// script cannot click), launches pass for [`OVERRIDE_WINDOW`]. Nothing
/// refusing means nothing to override; a No, or a host that cannot draw the
/// dialog, changes nothing.
pub fn admit_anyway(confirm: &dyn Fn(&str, &str) -> bool) -> Result<String, String> {
    let verdict = report().verdict;
    if verdict.admit {
        return Ok("Nothing is refusing agent launches right now.".to_string());
    }
    let question = format!(
        "GitPulse is refusing agent launches because {}.\n\nAdmit launches for the next {} seconds anyway? An agent started now competes for the memory the machine is short of.",
        verdict.message,
        OVERRIDE_WINDOW.as_secs()
    );
    if !confirm("Admit agent launches under memory pressure?", &question) {
        return Err("Not admitted: launches stay refused while the pressure lasts.".to_string());
    }
    grant_override()?;
    Ok(format!(
        "Agent launches are admitted for the next {} seconds.",
        OVERRIDE_WINDOW.as_secs()
    ))
}

/// Admits or refuses one step of an agent launch in `repo_path`.
///
/// Fails open: a sensor that cannot be read, a platform that is not governed
/// and a switched-off governor all admit. It refuses only when severe
/// pressure binds and a person has not said otherwise. Every decision that
/// is not "ungoverned" is recorded in the ledger.
pub fn admit(site: Site, repo_path: &str) -> Result<Admitted, Refused> {
    let report = report();
    let decision = admit_on(site, &report);
    record(site, repo_path, &report, &decision);
    if let Ok(Admitted {
        warning: Some(warning),
        ..
    }) = &decision
    {
        log::warn!(target: "host_load", "launch step {} admitted with a warning: {warning}", site.name());
    }
    decision
}

/// [`admit`]'s decision, over a report: pure, so it is tested without a
/// sampler or a ledger.
pub fn admit_on(site: Site, report: &GovernorReport) -> Result<Admitted, Refused> {
    let verdict = &report.verdict;
    if verdict.admit {
        return Ok(Admitted {
            warning: (!verdict.warnings.is_empty()).then(|| verdict.warnings.join("; ")),
            overridden: false,
        });
    }
    if report.override_remaining_s.is_some() {
        return Ok(Admitted {
            warning: Some(format!("admitted over {}", verdict.message)),
            overridden: true,
        });
    }
    Err(Refused {
        site,
        verdict: verdict.clone(),
    })
}

fn record(
    site: Site,
    repo_path: &str,
    report: &GovernorReport,
    decision: &Result<Admitted, Refused>,
) {
    let verdict = &report.verdict;
    if !verdict.reason.recorded() {
        return;
    }
    let (action, outcome) = match decision {
        Err(_) => ("host_load.refuse", crate::ledger::Outcome::Blocked),
        Ok(admitted) if admitted.overridden => {
            ("host_load.override_admit", crate::ledger::Outcome::Ok)
        }
        Ok(_) if verdict.reason == Reason::SensorUnavailable => {
            ("host_load.sensor_unavailable", crate::ledger::Outcome::Ok)
        }
        Ok(_) => ("host_load.admit", crate::ledger::Outcome::Ok),
    };
    let detail = serde_json::json!({
        "site": site.name(),
        "reason": verdict.reason,
        "binding": verdict.binding,
        "sensor": verdict.sensor,
        "age_ms": verdict.age_ms,
        "message": verdict.message,
        "warnings": verdict.warnings,
    });
    let draft = crate::ledger::Draft {
        repo_path: repo_path.to_string(),
        actor_kind: Some(crate::ledger::ActorKind::System),
        action: action.to_string(),
        object: Some(site.name().to_string()),
        outcome: Some(outcome),
        detail_json: Some(detail.to_string()),
        ..crate::ledger::Draft::default()
    };
    // On the launch path, so never waited on: the ledger has a busy timeout
    // and a launch must not queue behind it.
    #[cfg(test)]
    {
        let _ = crate::ledger::record(draft);
    }
    #[cfg(not(test))]
    if let Err(error) =
        crate::supervisor::spawn_once("host-load-ledger", "host-load ledger record", move || {
            let _ = crate::ledger::record(draft);
        })
    {
        log::warn!(target: "host_load", "{action} was not recorded: {error}");
    }
}

#[cfg(test)]
mod tests;
