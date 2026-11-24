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
use std::sync::atomic::{AtomicBool, Ordering};
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
    // The evidence is taken before anything else can change it: the moment,
    // and the footprints the sampler had read up to it.
    let at = Instant::now();
    let at_ms = wall_ms();
    let ring = crate::sysload::webcontent::snapshot();
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
    let sessions = webview
        .try_state::<crate::terminal::TerminalSessions>()
        .map(|s| s.inner().clone());
    if let Some(sessions) = sessions.clone() {
        // Off this callback: listing the sessions asks the terminal host.
        let crashed = label.clone();
        let for_window = webview.clone();
        let ring = ring.clone();
        if let Err(error) = crate::supervisor::spawn_once(
            "webview-crash-record",
            "webview crash record",
            move || {
                crate::terminal::record_webview_terminated(&sessions, &crashed, count);
                let context = context_json(
                    &window_state(&for_window),
                    crate::terminal::session_attachment_counts(&sessions),
                    &ring.dump(at, None),
                );
                crate::terminal::record_webview_evidence(
                    &sessions,
                    "webview_terminated_context",
                    &crashed,
                    context,
                );
            },
        ) {
            log::warn!(target: "desktop", "the crash was not recorded in the session lifecycle log: {error}");
        }
    }
    start_attribution(label.clone(), at, at_ms, ring, sessions);
    if label == "main" {
        super::exit::disarm(webview.app_handle(), "the main window's page crashed");
    }
    if delay.is_zero() {
        reload(webview);
        return;
    }
    let later = webview.clone();
    let spawned =
        crate::supervisor::spawn_once("webview-crash-reload", "webview reload", move || {
            std::thread::sleep(delay);
            reload(&later);
        });
    if let Err(error) = spawned {
        log::error!(target: "desktop", "delayed reload did not start ({error}); reloading now");
        reload(webview);
    }
}

/// How far before the termination a Jetsam report may be dated and still be
/// counted as written for it.
const JETSAM_SLACK_MS: u64 = 15_000;

/// One attribution at a time: a crash storm queues no `log show` processes.
static ATTRIBUTING: AtomicBool = AtomicBool::new(false);

fn wall_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| u64::try_from(d.as_millis()).unwrap_or(u64::MAX))
}

/// What the window was doing. `None` where Tauri would not say.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
struct WindowState {
    visible: Option<bool>,
    minimized: Option<bool>,
    focused: Option<bool>,
}

fn window_state<R: Runtime>(webview: &Webview<R>) -> WindowState {
    let window = webview.window();
    WindowState {
        visible: window.is_visible().ok(),
        minimized: window.is_minimized().ok(),
        focused: window.is_focused().ok(),
    }
}

/// The context record: the window, the sessions' views, and the footprint
/// samples. Occlusion is not something Tauri exposes, and the record says so.
fn context_json(
    window: &WindowState,
    sessions: Option<(usize, usize)>,
    ring: &crate::sysload::webcontent::Dump,
) -> serde_json::Value {
    serde_json::json!({
        "window": {
            "visible": window.visible,
            "minimized": window.minimized,
            "focused": window.focused,
            "occluded": "not exposed by Tauri",
        },
        "sessions": match sessions {
            Some((attached, detached)) => serde_json::json!({ "attached": attached, "detached": detached }),
            None => serde_json::json!("unavailable: the terminal host could not be asked"),
        },
        "webcontent_samples": { "note": ring.note, "lines": ring.lines },
    })
}

/// The attribution record: what the unified log and the diagnostics directory
/// said, and the footprint samples narrowed to the pid the log named.
fn evidence_json(
    attribution: &crate::terminal::incident::WebAttribution,
    jetsam: &crate::terminal::incident::JetsamProbe,
    since_ms: u64,
    ring: &crate::sysload::webcontent::Dump,
) -> serde_json::Value {
    serde_json::json!({
        "attribution": attribution.to_json(),
        "jetsam_report": jetsam.to_json(since_ms),
        "webcontent_samples": { "note": ring.note, "lines": ring.lines },
    })
}

/// The one app-log line for a termination's evidence.
fn evidence_line(
    label: &str,
    attribution: &crate::terminal::incident::WebAttribution,
    jetsam: &crate::terminal::incident::JetsamProbe,
    since_ms: u64,
    ring: &crate::sysload::webcontent::Dump,
) -> String {
    let jetsam = match jetsam.to_json(since_ms) {
        serde_json::Value::Object(map) => match map.get("status").and_then(|s| s.as_str()) {
            Some("newest") => format!(
                "newest JetsamEvent report {} ({})",
                map.get("name").and_then(|n| n.as_str()).unwrap_or("?"),
                if map.get("written_since_termination_window") == Some(&serde_json::json!(true)) {
                    "written around this termination"
                } else {
                    "older than this termination"
                }
            ),
            Some("none_found") => "no JetsamEvent report exists".to_string(),
            _ => format!(
                "JetsamEvent check unavailable: {}",
                map.get("reason")
                    .and_then(|r| r.as_str())
                    .unwrap_or("unknown")
            ),
        },
        _ => "JetsamEvent check unavailable".to_string(),
    };
    let samples = if ring.lines.is_empty() {
        ring.note.clone()
    } else {
        format!("{} | {}", ring.note, ring.lines.join(" ; "))
    };
    format!(
        "web content process of '{label}' terminated: {}; {jetsam}; footprints: {samples}",
        attribution.summary()
    )
}

/// Clears [`ATTRIBUTING`] however the attribution thread ends.
struct Attributing;

impl Drop for Attributing {
    fn drop(&mut self) {
        ATTRIBUTING.store(false, Ordering::Release);
    }
}

/// Asks the unified log and the diagnostics directory what killed the content
/// process, on a supervised thread: `log show` takes seconds and must never
/// hold the page's reload or the main thread.
fn start_attribution(
    label: String,
    at: Instant,
    at_ms: u64,
    ring: crate::sysload::webcontent::Ring,
    sessions: Option<crate::terminal::TerminalSessions>,
) {
    if ATTRIBUTING.swap(true, Ordering::AcqRel) {
        record_unavailable(
            &label,
            sessions.as_ref(),
            "another attribution was still running",
        );
        return;
    }
    let for_failure = sessions.clone();
    let failed_label = label.clone();
    let spawned = crate::supervisor::spawn_once(
        "webview-crash-attribution",
        "webview crash attribution",
        move || {
            let _clear = Attributing;
            let since_ms = at_ms.saturating_sub(JETSAM_SLACK_MS);
            let attribution = crate::terminal::incident::attribute_webcontent(at_ms);
            let jetsam = crate::terminal::incident::diagnostic_reports_dir().map_or_else(
                || crate::terminal::incident::JetsamProbe::Unreadable("HOME is not set".into()),
                |dir| crate::terminal::incident::newest_jetsam_report(&dir),
            );
            let dump = ring.dump(at, attribution.webcontent_pid());
            log::error!(
                target: "desktop",
                "{}",
                evidence_line(&label, &attribution, &jetsam, since_ms, &dump)
            );
            if let Some(sessions) = sessions {
                crate::terminal::record_webview_evidence(
                    &sessions,
                    "webview_terminated_attribution",
                    &label,
                    evidence_json(&attribution, &jetsam, since_ms, &dump),
                );
            }
        },
    );
    if let Err(error) = spawned {
        ATTRIBUTING.store(false, Ordering::Release);
        record_unavailable(
            &failed_label,
            for_failure.as_ref(),
            &format!("its thread did not start: {error}"),
        );
    }
}

/// An attribution that never ran is recorded as unavailable, with why, in the
/// app log and the lifecycle log: never silence, which would read as "no cause".
fn record_unavailable(
    label: &str,
    sessions: Option<&crate::terminal::TerminalSessions>,
    reason: &str,
) {
    log::error!(
        target: "desktop",
        "web content process of '{label}' terminated: attribution unavailable: {reason}"
    );
    if let Some(sessions) = sessions {
        crate::terminal::record_webview_evidence(
            sessions,
            "webview_terminated_attribution",
            label,
            unavailable_json(reason),
        );
    }
}

fn unavailable_json(reason: &str) -> serde_json::Value {
    serde_json::json!({
        "attribution": {
            "status": "unavailable",
            "detail": reason,
            "summary": format!("attribution unavailable: {reason}"),
        },
    })
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

    fn ring_with(pid: i32) -> (crate::sysload::webcontent::Ring, Instant) {
        use crate::sysload::webcontent::{ProcSample, Ring};
        let mut ring = Ring::new();
        let at = Instant::now();
        ring.record(
            at,
            vec![ProcSample {
                pid,
                footprint: Some(812 * 1024 * 1024),
                cpu_ns: Some(45_000_000_000),
            }],
        );
        (ring, at + Duration::from_secs(4))
    }

    #[test]
    fn the_context_record_carries_window_sessions_and_footprints() {
        let (ring, now) = ring_with(1591);
        let window = WindowState {
            visible: Some(false),
            minimized: Some(true),
            focused: Some(false),
        };
        let json = context_json(&window, Some((3, 2)), &ring.dump(now, None));
        assert_eq!(json["window"]["minimized"], true);
        assert_eq!(json["window"]["visible"], false);
        assert_eq!(json["sessions"]["attached"], 3);
        assert_eq!(json["sessions"]["detached"], 2);
        assert_eq!(
            json["webcontent_samples"]["lines"][0],
            "pid 1591, footprint 812.0 MB, cpu 45.0 s at T-4s"
        );
        let unknown = context_json(&WindowState::default(), None, &ring.dump(now, None));
        assert!(
            unknown["window"]["visible"].is_null(),
            "unknown is not false"
        );
        assert!(unknown["sessions"]
            .as_str()
            .unwrap()
            .starts_with("unavailable"));
    }

    #[test]
    fn the_evidence_line_joins_attribution_jetsam_and_the_killed_pids_footprint() {
        use crate::terminal::incident::{
            JetsamProbe, WebAttribution, WebContentSignal, WebOutcome,
        };
        let (ring, now) = ring_with(1591);
        let found = WebAttribution {
            outcome: WebOutcome::Found(vec![WebContentSignal {
                caller: crate::terminal::incident::Sender {
                    pid: 1328,
                    name: "gitpulse".into(),
                },
                value: 9,
                signal: Some("SIGKILL"),
                webcontent_pid: 1591,
                owner_pid: Some(1328),
                at: "t".into(),
            }]),
            truncated: false,
            attempts: 1,
        };
        let jetsam = JetsamProbe::Newest {
            name: "JetsamEvent-x.ips".into(),
            mtime_ms: 10_000,
        };
        let dump = ring.dump(now, found.webcontent_pid());
        let line = evidence_line("main", &found, &jetsam, 5_000, &dump);
        assert!(line.contains("gitpulse[1328]"), "{line}");
        assert!(
            line.contains("JetsamEvent-x.ips (written around this termination)"),
            "{line}"
        );
        assert!(line.contains("pid 1591, footprint 812.0 MB"), "{line}");
        let json = evidence_json(&found, &jetsam, 5_000, &dump);
        assert_eq!(
            json["jetsam_report"]["written_since_termination_window"],
            true
        );
        assert_eq!(json["attribution"]["status"], "found");
    }

    #[test]
    fn a_skipped_attribution_is_an_unavailable_record_not_silence() {
        let json = unavailable_json("another attribution was still running");
        assert_eq!(json["attribution"]["status"], "unavailable");
        assert_eq!(
            json["attribution"]["summary"],
            "attribution unavailable: another attribution was still running"
        );
    }

    #[test]
    fn an_attribution_that_could_not_run_reads_unavailable_in_line_and_record() {
        use crate::terminal::incident::{
            attribute_webcontent_with, JetsamProbe, LogQuery, WebOutcome,
        };
        let (ring, now) = ring_with(1);
        let failed =
            attribute_webcontent_with(|| LogQuery::Failed("spawn denied".into()), Duration::ZERO);
        assert!(matches!(failed.outcome, WebOutcome::Unavailable(_)));
        let jetsam = JetsamProbe::Unreadable("denied".into());
        let line = evidence_line("main", &failed, &jetsam, 0, &ring.dump(now, None));
        assert!(
            line.contains("attribution unavailable: log query failed: spawn denied"),
            "{line}"
        );
        assert!(
            line.contains("JetsamEvent check unavailable: denied"),
            "{line}"
        );
        assert!(!line.contains("no cause"), "{line}");
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
