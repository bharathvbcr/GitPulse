//! How fast free disk space is falling, from the samples the sampler already
//! takes.
//!
//! # Why
//!
//! In one incident free disk fell from 99 to 45 GiB in about twenty minutes
//! (about 2.7 GiB/min) under one coverage build, and on 2026-11-15 another took
//! it from about 150 GiB to nothing in about fifteen (about 10 GiB/min). The
//! governor, which only looked at the level, would have said nothing until 32
//! GiB and then had a few minutes left. The rate says "this will not stop"
//! while there is still room to act, so the `disk_fill` rule (`thresholds.rs`)
//! turns it into minutes until the severe floor.
//!
//! # What this owns
//!
//! A bounded history of `(time, free bytes)` and two rates read off it, in
//! bytes of free space lost per minute. It runs in the writer only: the rates
//! are published in `host-load.v1` as readings, so a hook or a second GitPulse
//! that has no history still has them, and the time to the floor is worked out
//! in the pure path (`Thresholds::assess`) where a replay test needs no
//! sleeping.
//!
//! # Two rates, because the two tiers are asked different questions
//!
//! Shedding GitPulse's own background work on a burst is cheap and was always
//! acceptable; refusing launches on one is not. So:
//!
//! * the **quick** rate (Elevated) is the sensitive one: the smaller of the
//!   least-squares falls over the last minute and the last three, or the
//!   leave-one-burst-out rate below when that is larger;
//! * the **sustained** rate (Severe) needs evidence that neither one burst nor
//!   a pause can fake: the larger of the leave-one-burst-out rate and the net
//!   fall over the last ten minutes.
//!
//! *Leave-one-burst-out* (three-minute window): the net fall, less the largest
//! net fall inside any 60 s sub-window, over the window less those 60 s. It is
//! zero for one burst of a minute or less however large, exact for a steady
//! fall, and positive across pauses, because a build that writes in lumps with
//! gaps between them loses space in more than one sub-window.
//!
//! The *ten-minute net fall* divides what was lost over the history by the
//! longer of that history and five minutes, so a cold start does not inflate
//! it: a burst of `h` GiB reads as `h / 5` GiB a minute at most, and refuses
//! only when it alone ate half of what is left above the floor. Pauses do not
//! matter to it, because it never looks inside the window.
//!
//! The rejected alternatives, from the model in the review of this rule: a fit
//! over the whole long window reads a step of `h` GiB as about `1.5 h / W`;
//! requiring every third of the window to fall reads zero whenever a pause
//! covers a third, so 5 GiB every 90 s never bound and a build writing 8 GiB/min
//! for 40 s of every 120 reached Severe with six minutes left.
//!
//! # What it will not say
//!
//! Unknown is never a rate, and never "not falling". Too few samples, too
//! short a span, a gap in the timestamps (the sampler paused, the machine
//! slept), a different boot, a change between two ticks no disk can make (a
//! volume remounted, or a hundred gigabytes deleted at once) all leave the
//! tracker empty or its answer `NotYet`; a disk that cannot be read says so
//! from the first tick (`Failed`, or `Unsupported` where the platform has no
//! reader), not "warming up". A rise inside the window is a smaller fall, or a
//! rise, so deleting the output unbinds the rule without a special case.

use super::sensor::{Reading, Why};
use std::collections::VecDeque;

/// The short window: what is happening now.
pub const SHORT_WINDOW_MS: u64 = 60_000;
/// The long window: what has been happening in the last few minutes.
pub const LONG_WINDOW_MS: u64 = 180_000;
/// The window of the sustained net fall.
pub const NET_WINDOW_MS: u64 = 600_000;
/// The least the net fall's divisor is, so a cold start does not inflate it.
const NET_MIN_DIVISOR_MS: u64 = 300_000;
/// The sub-window a burst is taken out of, in the leave-one-burst-out rate.
const BURST_MS: u64 = 60_000;
/// The least span of samples each window must cover before it speaks.
const SHORT_MIN_SPAN_MS: u64 = 30_000;
const LONG_MIN_SPAN_MS: u64 = 120_000;
/// The fewest samples any window needs.
const MIN_SAMPLES: usize = 8;
/// A silence between two samples longer than this is a pause, not a trend.
/// Five ticks at the 2 s cadence.
pub const MAX_GAP_MS: u64 = 10_000;
/// More than this between two consecutive ticks is not a disk filling: a volume
/// remounted, or a very large delete. Either way the history is not one.
pub const MAX_STEP_BYTES: u64 = 32 << 30;
/// Samples held at the tick cadence: the long window at one every 2 s, with slack.
const CAPACITY: usize = 128;
/// One sample is kept this often for the ten-minute window.
const COARSE_EVERY_MS: u64 = 10_000;

/// The two rates the writer publishes.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct FillRates {
    /// What Elevated decides on: the sensitive one.
    pub quick: Reading<u64>,
    /// What Severe decides on: the one a burst cannot fake and a pause cannot hide.
    pub sustained: Reading<u64>,
}

impl FillRates {
    fn both(reading: Reading<u64>) -> Self {
        Self {
            quick: reading,
            sustained: reading,
        }
    }
}

/// The bounded history and the boot it belongs to.
#[derive(Debug, Default)]
pub struct FillTracker {
    boot: Option<u64>,
    samples: VecDeque<(u64, u64)>,
    /// One sample every [`COARSE_EVERY_MS`] over the net window.
    coarse: VecDeque<(u64, u64)>,
    /// Why the last reading could not be taken, until one is.
    unreadable: Option<Why>,
}

impl FillTracker {
    /// Forgets everything: the next rate waits for new samples.
    pub fn reset(&mut self) {
        self.boot = None;
        self.clear();
        self.unreadable = None;
    }

    fn clear(&mut self) {
        self.samples.clear();
        self.coarse.clear();
    }

    /// Takes one tick's reading, `at_ms` on the monotonic clock of boot
    /// `boot`. An unreadable reading adds nothing, but is remembered as the
    /// reason there is no rate; the silence it leaves is what
    /// [`FillTracker::rates`] and the next sample judge.
    pub fn observe(&mut self, boot: u64, at_ms: u64, free: Reading<u64>) {
        if self.boot != Some(boot) {
            self.clear();
            self.boot = Some(boot);
        }
        let free = match free {
            Reading::Value(free) => free,
            Reading::Unavailable(why) => {
                self.unreadable = Some(why);
                return;
            }
        };
        self.unreadable = None;
        if let Some(&(last_at, last_free)) = self.samples.back() {
            let in_order = at_ms > last_at;
            let gap = at_ms.saturating_sub(last_at) > MAX_GAP_MS;
            let step = free.abs_diff(last_free) > MAX_STEP_BYTES;
            if !in_order || gap || step {
                self.clear();
            }
        }
        self.samples.push_back((at_ms, free));
        while self.samples.len() > CAPACITY {
            self.samples.pop_front();
        }
        if self
            .coarse
            .back()
            .is_none_or(|&(at, _)| at_ms >= at + COARSE_EVERY_MS)
        {
            self.coarse.push_back((at_ms, free));
        }
        while self
            .coarse
            .front()
            .is_some_and(|&(at, _)| at + NET_WINDOW_MS < at_ms)
        {
            self.coarse.pop_front();
        }
    }

    /// Bytes of free space lost per minute as of `now_ms`, for each tier:
    /// `Value(0)` when it is not falling, `NotYet` when there is not enough
    /// history to say, `Failed` when the disk has not been readable for longer
    /// than a gap (or was never readable), and `Unsupported` where the
    /// platform has no way to read it.
    pub fn rates(&self, now_ms: u64) -> FillRates {
        let Some(&(last_at, _)) = self.samples.back() else {
            return FillRates::both(Reading::Unavailable(match self.unreadable {
                Some(Why::Unsupported) => Why::Unsupported,
                Some(_) => Why::Failed,
                None => Why::NotYet,
            }));
        };
        if now_ms.saturating_sub(last_at) > MAX_GAP_MS {
            return FillRates::both(Reading::Unavailable(Why::Failed));
        }
        let short = self.fall_per_min(last_at, SHORT_WINDOW_MS, SHORT_MIN_SPAN_MS);
        let long = self.fall_per_min(last_at, LONG_WINDOW_MS, LONG_MIN_SPAN_MS);
        let fitted = short.zip(long).map(|(short, long)| short.min(long));
        let lumpy = self.leave_one_burst_out(last_at);
        let net = self.net_fall(last_at);
        FillRates {
            quick: reading(largest(fitted, lumpy)),
            sustained: reading(largest(net, lumpy)),
        }
    }

    /// The samples in the `window_ms` ending at `end_ms`.
    fn within(&self, end_ms: u64, window_ms: u64) -> Vec<(u64, u64)> {
        let from = end_ms.saturating_sub(window_ms);
        self.samples
            .iter()
            .copied()
            .filter(|(at, _)| *at >= from)
            .collect()
    }

    /// The least-squares fall, in bytes per minute (negative when rising), of
    /// the samples in the `window_ms` ending at `end_ms`; `None` when they are
    /// too few or cover too little time.
    fn fall_per_min(&self, end_ms: u64, window_ms: u64, min_span_ms: u64) -> Option<f64> {
        let inside = self.within(end_ms, window_ms);
        if inside.len() < MIN_SAMPLES || span(&inside) < min_span_ms {
            return None;
        }
        slope_fall(&inside)
    }

    /// The leave-one-burst-out fall over the long window, in bytes per minute.
    fn leave_one_burst_out(&self, end_ms: u64) -> Option<f64> {
        let inside = self.within(end_ms, LONG_WINDOW_MS);
        let covered = span(&inside);
        if inside.len() < MIN_SAMPLES || covered < LONG_MIN_SPAN_MS {
            return None;
        }
        let (first, last) = (inside.first()?.1, inside.last()?.1);
        let net = first.saturating_sub(last) as f64;
        // The most any sub-window of a minute lost: for each sample, the
        // highest free space within the minute before it, less its own.
        let mut burst = 0u64;
        let mut from = 0;
        for (at, free) in inside.iter().copied() {
            while inside[from].0 + BURST_MS < at {
                from += 1;
            }
            let high = inside[from..]
                .iter()
                .take_while(|(t, _)| *t <= at)
                .map(|&(_, f)| f)
                .max()
                .unwrap_or(free);
            burst = burst.max(high.saturating_sub(free));
        }
        let minutes = (covered - BURST_MS) as f64 / 60_000.0;
        Some(((net - burst as f64) / minutes).max(0.0))
    }

    /// What was lost over the last ten minutes of history, per minute, over
    /// the longer of that history and five minutes.
    fn net_fall(&self, end_ms: u64) -> Option<f64> {
        let (oldest_at, oldest) = *self.coarse.front()?;
        let newest = self.samples.back()?.1;
        let covered = end_ms.saturating_sub(oldest_at);
        if covered < LONG_MIN_SPAN_MS {
            return None;
        }
        let divisor = covered.clamp(NET_MIN_DIVISOR_MS, NET_WINDOW_MS + COARSE_EVERY_MS);
        Some(oldest.saturating_sub(newest) as f64 / (divisor as f64 / 60_000.0))
    }
}

/// The larger of two estimates that may each be missing; missing only when both are.
fn largest(a: Option<f64>, b: Option<f64>) -> Option<f64> {
    match (a, b) {
        (Some(a), Some(b)) => Some(a.max(b)),
        (a, b) => a.or(b),
    }
}

/// A rate as a reading: `NotYet` when there is none, and a fall of a byte a
/// minute or less is noise, not a trend.
fn reading(fall: Option<f64>) -> Reading<u64> {
    match fall {
        None => Reading::Unavailable(Why::NotYet),
        Some(fall) if fall.is_finite() && fall >= 1.0 => Reading::Value(fall as u64),
        Some(_) => Reading::Value(0),
    }
}

/// Milliseconds the (ordered) samples cover.
fn span(samples: &[(u64, u64)]) -> u64 {
    match (samples.first(), samples.last()) {
        (Some(first), Some(last)) => last.0 - first.0,
        _ => 0,
    }
}

/// The least-squares slope of free bytes over time, as a fall in bytes per
/// minute (negative when rising); `None` for a degenerate fit.
fn slope_fall(samples: &[(u64, u64)]) -> Option<f64> {
    let (t0, y0) = *samples.first()?;
    // Centred on the first sample so the sums stay small.
    let points: Vec<(f64, f64)> = samples
        .iter()
        .map(|&(at, free)| ((at - t0) as f64 / 1000.0, free as f64 - y0 as f64))
        .collect();
    let n = points.len() as f64;
    let (mean_t, mean_y) = (
        points.iter().map(|p| p.0).sum::<f64>() / n,
        points.iter().map(|p| p.1).sum::<f64>() / n,
    );
    let (mut num, mut den) = (0.0, 0.0);
    for &(t, y) in &points {
        num += (t - mean_t) * (y - mean_y);
        den += (t - mean_t) * (t - mean_t);
    }
    if den <= 0.0 {
        return None;
    }
    // Slope in bytes per second; a fall is its negative.
    Some(-(num / den) * 60.0)
}

#[cfg(test)]
#[path = "diskrate_tests.rs"]
mod tests;
