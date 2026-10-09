//! A webview's content process died (WebKit crashed, ran out of memory, or was
//! killed). The window is left blank until its page is loaded again.
//!
//! Tauri's default answer is an immediate reload every time, logged only at
//! debug level — and its "web content process terminated" line is written when
//! the webview is *built*, not when it dies, so the app log never recorded a
//! crash. A page that crashes as it loads therefore reloaded in a tight loop,
//! invisibly. The listener that confirms quits went with the page, so a quit in
//! that window waited on an acknowledgement nobody could send.
//!
//! Here every termination is logged as an error with its count, the quit
//! confirmation is disarmed until the reloaded page listens again, and reloads
//! back off once crashes repeat. Reloading never stops: a blank window that
//! will not come back is worse than one that comes back slowly.

use std::collections::{HashMap, VecDeque};
use std::sync::Mutex;
use std::time::{Duration, Instant};

use tauri::{Manager, Runtime, Webview};

/// Crashes counted together.
const WINDOW: Duration = Duration::from_secs(60);
/// Crashes in [`WINDOW`] that reload at once.
const FREE: usize = 2;
/// First backed-off delay; each further crash doubles it.
const BASE: Duration = Duration::from_secs(1);
const CAP: Duration = Duration::from_secs(30);

/// Recent crashes of one webview, and how long to wait before reloading it.
#[derive(Default)]
pub struct Breaker {
    recent: VecDeque<Instant>,
}

impl Breaker {
    /// Records a crash at `now`. Returns the crashes counted in the window
    /// (this one included) and the delay before reloading.
    pub fn record(&mut self, now: Instant) -> (usize, Duration) {
        while self
            .recent
            .front()
            .is_some_and(|at| now.saturating_duration_since(*at) >= WINDOW)
        {
            self.recent.pop_front();
        }
        self.recent.push_back(now);
        // Bounded by the window anyway; this caps a clock that never advances.
        while self.recent.len() > 64 {
            self.recent.pop_front();
        }
        let count = self.recent.len();
        let delay = match count.checked_sub(FREE + 1) {
            None => Duration::ZERO,
            Some(extra) => BASE
                .checked_mul(1u32.checked_shl(extra as u32).unwrap_or(u32::MAX))
                .map_or(CAP, |delay| delay.min(CAP)),
        };
        (count, delay)
    }
}

static BREAKERS: Mutex<Option<HashMap<String, Breaker>>> = Mutex::new(None);

/// Tauri's `on_web_content_process_terminate` hook.
pub fn on_terminated<R: Runtime>(webview: &Webview<R>) {
    let label = webview.label().to_string();
    let (count, delay) = {
        let mut breakers = BREAKERS
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        breakers
            .get_or_insert_with(HashMap::new)
            .entry(label.clone())
            .or_default()
            .record(Instant::now())
    };
    log::error!(
        target: "desktop",
        "web content process of '{label}' terminated ({count} in the last {}s); reloading {}",
        WINDOW.as_secs(),
        if delay.is_zero() {
            "now".to_string()
        } else {
            format!("in {}s", delay.as_secs())
        }
    );
    if label == "main" {
        super::exit::disarm(webview.app_handle(), "the main window's page crashed");
    }
    if delay.is_zero() {
        reload(webview);
        return;
    }
    let later = webview.clone();
    let spawned = std::thread::Builder::new()
        .name("webview-crash-reload".into())
        .spawn(move || {
            std::thread::sleep(delay);
            reload(&later);
        });
    if let Err(error) = spawned {
        log::error!(target: "desktop", "delayed reload did not start ({error}); reloading now");
        reload(webview);
    }
}

fn reload<R: Runtime>(webview: &Webview<R>) {
    if let Err(error) = webview.reload() {
        log::error!(target: "desktop", "could not reload '{}' after its crash: {error}", webview.label());
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_first_crashes_reload_at_once_then_back_off_to_a_cap() {
        let mut breaker = Breaker::default();
        let start = Instant::now();
        let delays: Vec<u64> = (0..10)
            .map(|step| {
                breaker
                    .record(start + Duration::from_millis(step))
                    .1
                    .as_secs()
            })
            .collect();
        assert_eq!(delays, [0, 0, 1, 2, 4, 8, 16, 30, 30, 30]);
    }

    #[test]
    fn crashes_outside_the_window_are_forgotten() {
        let mut breaker = Breaker::default();
        let start = Instant::now();
        for step in 0..5 {
            breaker.record(start + Duration::from_millis(step));
        }
        let (count, delay) = breaker.record(start + WINDOW + Duration::from_secs(1));
        assert_eq!(
            (count, delay),
            (1, Duration::ZERO),
            "an old crash storm is over"
        );
    }

    #[test]
    fn a_clock_that_never_advances_cannot_grow_the_history() {
        let mut breaker = Breaker::default();
        let now = Instant::now();
        let mut last = Duration::ZERO;
        for _ in 0..10_000 {
            last = breaker.record(now).1;
        }
        assert!(breaker.recent.len() <= 64);
        assert_eq!(last, CAP);
    }
}
