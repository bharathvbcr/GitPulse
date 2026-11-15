//! How fast free disk space is falling, from the samples the sampler already
//! takes.
//!
//! # Why
//!
//! On 2026-11-15 free disk fell from 99 to 45 GiB in about twenty minutes
//! (about 2.7 GiB/min) under one coverage build, and the governor, which only
//! looked at the level, would have said nothing until 32 GiB and then had a few
//! minutes left. The rate says "this will not stop" while there is still room to
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
//! A least-squares slope over two windows, and the smaller fall of the two. A
//! compile burst writes in lumps and a big copy lands in seconds: one short
//! window follows both, so a 20 GiB copy would read as 40 GiB/min for a minute.
//! The long window has to agree, which a burst that is over by then cannot make
//! it do, and a fill that is really going on makes both say so.
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
/// The long window: what has been happening, which a burst cannot fake.
pub const LONG_WINDOW_MS: u64 = 180_000;
/// The least span of samples each window must cover before it speaks.
const SHORT_MIN_SPAN_MS: u64 = 30_000;
const LONG_MIN_SPAN_MS: u64 = 120_000;
/// The fewest samples either window needs.
const MIN_SAMPLES: usize = 8;
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
        let long = self.fall_per_min(last_at, LONG_WINDOW_MS, LONG_MIN_SPAN_MS);
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

    /// The least-squares fall, in bytes per minute (negative when rising), of
    /// the samples in the `window_ms` ending at `end_ms`; `None` when they are
    /// too few or cover too little time.
    fn fall_per_min(&self, end_ms: u64, window_ms: u64, min_span_ms: u64) -> Option<f64> {
        let from = end_ms.saturating_sub(window_ms);
        let inside: Vec<(u64, u64)> = self
            .samples
            .iter()
            .copied()
            .filter(|(at, _)| *at >= from)
            .collect();
        let (first, _) = *inside.first()?;
        let (last, _) = *inside.last()?;
        if inside.len() < MIN_SAMPLES || last - first < min_span_ms {
            return None;
        }
        // Centred on the first sample so the sums stay small.
        let (t0, y0) = (first, inside[0].1);
        let n = inside.len() as f64;
        let points = inside.iter().map(|&(at, free)| {
            (
                (at - t0) as f64 / 1000.0,
                free as f64 - y0 as f64, // bytes, relative
            )
        });
        let (mut st, mut sy) = (0.0, 0.0);
        let collected: Vec<(f64, f64)> = points.collect();
        for &(t, y) in &collected {
            st += t;
            sy += y;
        }
        let (mean_t, mean_y) = (st / n, sy / n);
        let (mut num, mut den) = (0.0, 0.0);
        for &(t, y) in &collected {
            num += (t - mean_t) * (y - mean_y);
            den += (t - mean_t) * (t - mean_t);
        }
        if den <= 0.0 {
            return None;
        }
        // Slope in bytes per second; a fall is its negative.
        Some(-(num / den) * 60.0)
    }
}

#[cfg(test)]
#[path = "diskrate_tests.rs"]
mod tests;
