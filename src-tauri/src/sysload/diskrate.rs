//! How fast free disk space is falling, from the samples the sampler already
//! takes.
//!
//! # Why
//!
//! In one incident free disk fell from 99 to 45 GiB in about twenty minutes
//! (about 2.7 GiB/min) under one coverage build, and on 2026-11-15 another took
//! it from about 150 GiB to nothing in about fifteen (about 10 GiB/min). The
//! governor, which only looked at the level, would have said nothing until 32
//! GiB and then had a few minutes left. The rate says "this will not stop" while there is still room to
//! act, so the `disk_fill` rule (`thresholds.rs`) turns it into minutes until
//! the severe floor.
//!
//! # What this owns
//!
//! A bounded history of `(time, free bytes)` and the rate read off it, in
//! bytes of free space lost per minute. It runs in the writer only: the rate is
//! published in `host-load.v1` as a reading, so a hook or a second GitPulse
//! that has no history still has it, and the time to the floor is worked out in
//! the pure path (`Thresholds::assess`) where a replay test needs no sleeping.
//!
//! # The rate
//!
//! The smallest of four least-squares falls: the short window's, and each third
//! of the long window's. A compile burst writes in lumps and a big copy lands
//! in seconds: the short window follows both, so a 20 GiB copy would read as 40
//! GiB/min for a minute. A fit over the whole long window does not rescue it: a
//! step of `h` GiB in the middle of a window of `W` seconds fits a slope of
//! about `1.5 h / W` however flat the rest is, which on a 64 GiB Mac read a
//! 12 GiB burst at 70 GiB free as a fill that refuses launches.
//!
//! So the long window must fall throughout, not on average. A one-off event
//! shorter than a third of the window (a minute) cannot touch all three
//! thirds, so one of them is flat and the minimum is zero; a fill that is
//! really going on, or a staircase of steps each minute, falls in every third.
//! Why the minimum and not the median of pairwise slopes: a step in the middle
//! of a window makes just over half of the pairs span it, so a median still
//! moves with a burst; the minimum of thirds does not.
//!
//! # What it will not say
//!
//! Unknown is never a rate. Too few samples, too short a span, a gap in the
//! timestamps (the sampler paused, the machine slept), a different boot, a
//! change between two ticks no disk can make (a volume remounted, or a
//! hundred gigabytes deleted at once) and a reading that could not be taken for
//! longer than the gap all leave the tracker empty or its answer `NotYet`. A
//! rise inside the window is a smaller fall, or a rise, so deleting the output
//! unbinds the rule without a special case.

use super::sensor::{Reading, Why};
use std::collections::VecDeque;

/// The short window: what is happening now.
pub const SHORT_WINDOW_MS: u64 = 60_000;
/// The long window: what has been happening. It is read in thirds, each of
/// which must fall.
pub const LONG_WINDOW_MS: u64 = 180_000;
/// The least span of samples each window must cover before it speaks.
const SHORT_MIN_SPAN_MS: u64 = 30_000;
const LONG_MIN_SPAN_MS: u64 = 120_000;
/// The fewest samples either window needs.
const MIN_SAMPLES: usize = 8;
/// The fewest samples one third of the long window needs.
const MIN_THIRD_SAMPLES: usize = 4;
/// How many parts the long window is read in.
const LONG_PARTS: u64 = 3;
/// A silence between two samples longer than this is a pause, not a trend.
/// Five ticks at the 2 s cadence.
pub const MAX_GAP_MS: u64 = 10_000;
/// More than this between two consecutive ticks is not a disk filling: a volume
/// remounted, or a very large delete. Either way the history is not one.
pub const MAX_STEP_BYTES: u64 = 32 << 30;
/// Samples held: the long window at one every 2 s, with slack.
const CAPACITY: usize = 128;

/// The bounded history and the boot it belongs to.
#[derive(Debug, Default)]
pub struct FillTracker {
    boot: Option<u64>,
    samples: VecDeque<(u64, u64)>,
}

impl FillTracker {
    /// Forgets everything: the next rate waits for new samples.
    pub fn reset(&mut self) {
        self.boot = None;
        self.samples.clear();
    }

    /// Takes one tick's reading, `at_ms` on the monotonic clock of boot
    /// `boot`. An unreadable reading adds nothing; the silence it leaves is
    /// what [`FillTracker::rate`] and the next sample judge.
    pub fn observe(&mut self, boot: u64, at_ms: u64, free: Reading<u64>) {
        if self.boot != Some(boot) {
            self.samples.clear();
            self.boot = Some(boot);
        }
        let Reading::Value(free) = free else { return };
        if let Some(&(last_at, last_free)) = self.samples.back() {
            let in_order = at_ms > last_at;
            let gap = at_ms.saturating_sub(last_at) > MAX_GAP_MS;
            let step = free.abs_diff(last_free) > MAX_STEP_BYTES;
            if !in_order || gap || step {
                self.samples.clear();
            }
        }
        self.samples.push_back((at_ms, free));
        while self.samples.len() > CAPACITY {
            self.samples.pop_front();
        }
    }

    /// Bytes of free space lost per minute as of `now_ms`: `Value(0)` when it
    /// is not falling, `NotYet` when there is not enough history to say, and
    /// `Failed` when the disk has not been readable for longer than a gap.
    pub fn rate(&self, now_ms: u64) -> Reading<u64> {
        let Some(&(last_at, _)) = self.samples.back() else {
            return Reading::Unavailable(Why::NotYet);
        };
        if now_ms.saturating_sub(last_at) > MAX_GAP_MS {
            return Reading::Unavailable(Why::Failed);
        }
        let short = self.fall_per_min(last_at, SHORT_WINDOW_MS, SHORT_MIN_SPAN_MS);
        let long = self.long_fall_per_min(last_at);
        match (short, long) {
            (Some(short), Some(long)) => {
                let fall = short.min(long);
                // A fall of a byte a minute is noise, not a trend.
                Reading::Value(if fall.is_finite() && fall >= 1.0 {
                    fall as u64
                } else {
                    0
                })
            }
            _ => Reading::Unavailable(Why::NotYet),
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

    /// The smallest fall among the [`LONG_PARTS`] equal parts of the long
    /// window's span; `None` when the window is too short to speak or a part
    /// holds too few samples to fit.
    fn long_fall_per_min(&self, end_ms: u64) -> Option<f64> {
        let inside = self.within(end_ms, LONG_WINDOW_MS);
        if inside.len() < MIN_SAMPLES || span(&inside) < LONG_MIN_SPAN_MS {
            return None;
        }
        let first = inside.first()?.0;
        let part_ms = span(&inside) / LONG_PARTS;
        let mut least = f64::INFINITY;
        for part in 0..LONG_PARTS {
            let from = first + part * part_ms;
            let to = first + (part + 1) * part_ms;
            let in_part: Vec<(u64, u64)> = inside
                .iter()
                .copied()
                // The last part runs to the newest sample, whatever the
                // division left over.
                .filter(|(at, _)| *at >= from && (*at < to || part == LONG_PARTS - 1))
                .collect();
            if in_part.len() < MIN_THIRD_SAMPLES {
                return None;
            }
            least = least.min(slope_fall(&in_part)?);
        }
        Some(least)
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
