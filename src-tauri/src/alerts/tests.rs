//! The worker's behaviour, driven without an OS, a clock, or a config file.
//!
//! [`super::flush`] and [`super::admit`] are exercised directly rather than
//! through [`super::start`]: the hub is a process-wide singleton, and a test
//! that owned it would be the only test able to run.

use super::*;
use crate::tool_config::SessionAlertSettings;

#[derive(Default)]
struct Recorder {
    sent: Mutex<Vec<(String, String, String, bool)>>,
    attended: Mutex<Option<String>>,
    config: Mutex<SessionAlertSettings>,
    minute: Mutex<Option<u16>>,
    refuse: Mutex<bool>,
}

impl Recorder {
    fn new() -> Arc<Self> {
        Arc::new(Self {
            minute: Mutex::new(Some(12 * 60)),
            ..Self::default()
        })
    }
    fn headings(&self) -> Vec<String> {
        self.sent
            .lock()
            .unwrap()
            .iter()
            .map(|s| s.1.clone())
            .collect()
    }
    fn bodies(&self) -> Vec<String> {
        self.sent
            .lock()
            .unwrap()
            .iter()
            .map(|s| s.2.clone())
            .collect()
    }
    fn natives(&self) -> Vec<String> {
        self.sent
            .lock()
            .unwrap()
            .iter()
            .map(|s| s.0.clone())
            .collect()
    }
    fn count(&self) -> usize {
        self.sent.lock().unwrap().len()
    }
}

impl Host for Recorder {
    fn submit(&self, native: &str, heading: &str, body: &str, sound: bool) -> Result<bool, String> {
        if *self.refuse.lock().unwrap() {
            return Err("notification centre unavailable".into());
        }
        self.sent.lock().unwrap().push((
            native.to_owned(),
            heading.to_owned(),
            body.to_owned(),
            sound,
        ));
        Ok(true)
    }
    fn attended(&self, key: &str) -> bool {
        self.attended.lock().unwrap().as_deref() == Some(key)
    }
    fn config(&self) -> SessionAlertSettings {
        self.config.lock().unwrap().clone()
    }
    fn minute(&self) -> Option<u16> {
        *self.minute.lock().unwrap()
    }
}

fn notice(key: &str) -> Notice {
    Notice {
        key: key.to_owned(),
        origin: Origin::Terminal,
        label: "Claude Code".into(),
        place: Some("GitPulse".into()),
        reason: None,
        detail: None,
        is_agent: true,
        channel: "bell",
    }
}

/// A worker turned inside out: the caller supplies the clock.
struct Driver {
    tracked: HashMap<String, Tracked>,
    bucket: Bucket,
    counters: Counters,
    last_error: Mutex<Option<String>>,
    host: Arc<dyn Host>,
    recorder: Arc<Recorder>,
}

impl Driver {
    fn new() -> Self {
        let recorder = Recorder::new();
        let now = Instant::now();
        Self {
            tracked: HashMap::new(),
            bucket: Bucket::new(now),
            counters: Counters::default(),
            last_error: Mutex::new(None),
            host: recorder.clone(),
            recorder,
        }
    }
    fn tick(&mut self, notice: Option<Notice>, now: Instant) {
        if let Some(notice) = notice {
            admit(notice, now, &mut self.tracked, &self.counters);
        }
        flush(
            now,
            false,
            &mut self.tracked,
            &mut self.bucket,
            &self.host,
            &self.counters,
            &self.last_error,
        );
        prune(now, &mut self.tracked);
    }
    fn counter(&self, verdict: Verdict) -> u64 {
        match verdict {
            Verdict::Deliver => self.counters.delivered.load(Ordering::Relaxed),
            Verdict::Disabled => self.counters.suppressed_disabled.load(Ordering::Relaxed),
            Verdict::NotAnAgent => self.counters.suppressed_not_agent.load(Ordering::Relaxed),
            Verdict::Attended => self.counters.suppressed_attended.load(Ordering::Relaxed),
            Verdict::Quiet => self.counters.suppressed_quiet.load(Ordering::Relaxed),
        }
    }
}

#[test]
fn the_first_signal_from_a_session_is_delivered_at_once() {
    let mut driver = Driver::new();
    let now = Instant::now();
    driver.tick(Some(notice("term-1")), now);
    assert_eq!(driver.recorder.count(), 1);
    assert_eq!(driver.recorder.headings(), vec!["Claude Code · GitPulse"]);
    assert_eq!(
        driver.recorder.natives(),
        vec!["gitpulse.session.term-1".to_string()]
    );
}

#[test]
fn a_burst_from_one_session_becomes_one_banner_then_one_more() {
    let mut driver = Driver::new();
    let start = Instant::now();
    for step in 0..40u64 {
        driver.tick(
            Some(notice("term-1")),
            start + Duration::from_millis(step * 100),
        );
    }
    // Four seconds of signals at 10/s: one immediate banner, and the last
    // notice still held because the window has not elapsed.
    assert_eq!(driver.recorder.count(), 1);
    driver.tick(None, start + COALESCE + Duration::from_millis(1));
    assert_eq!(driver.recorder.count(), 2);
    // And then nothing more: the queue held one replacement, not forty.
    driver.tick(None, start + COALESCE * 4);
    assert_eq!(driver.recorder.count(), 2);
}

#[test]
fn the_banner_a_window_finally_shows_is_the_latest_state_not_the_first() {
    let mut driver = Driver::new();
    let start = Instant::now();
    let mut first = notice("term-1");
    first.detail = Some("Reading files".into());
    driver.tick(Some(first), start);
    let mut second = notice("term-1");
    second.detail = Some("Needs permission for Bash".into());
    driver.tick(Some(second), start + Duration::from_millis(50));
    driver.tick(None, start + COALESCE + Duration::from_millis(1));
    assert_eq!(
        driver.recorder.bodies(),
        vec![
            "Reading files".to_string(),
            "Needs permission for Bash".to_string()
        ]
    );
}

#[test]
fn separate_sessions_do_not_share_a_window() {
    let mut driver = Driver::new();
    let now = Instant::now();
    driver.tick(Some(notice("term-1")), now);
    driver.tick(Some(notice("term-2")), now);
    driver.tick(Some(notice("term-3")), now);
    assert_eq!(driver.recorder.count(), 3);
}

#[test]
fn a_session_you_are_watching_is_silent_and_says_so() {
    let mut driver = Driver::new();
    *driver.recorder.attended.lock().unwrap() = Some("term-1".into());
    driver.tick(Some(notice("term-1")), Instant::now());
    assert_eq!(driver.recorder.count(), 0);
    assert_eq!(driver.counter(Verdict::Attended), 1);
}

#[test]
fn looking_away_notifies_immediately_rather_than_serving_out_an_unused_window() {
    let mut driver = Driver::new();
    let start = Instant::now();
    *driver.recorder.attended.lock().unwrap() = Some("term-1".into());
    driver.tick(Some(notice("term-1")), start);
    assert_eq!(driver.recorder.count(), 0);
    *driver.recorder.attended.lock().unwrap() = None;
    driver.tick(Some(notice("term-1")), start + Duration::from_millis(10));
    assert_eq!(
        driver.recorder.count(),
        1,
        "a suppressed notice consumed the coalescing window"
    );
}

#[test]
fn every_suppression_is_counted_under_its_own_name() {
    for (setup, verdict) in [
        (
            Box::new(|d: &mut Driver| d.recorder.config.lock().unwrap().enabled = false)
                as Box<dyn Fn(&mut Driver)>,
            Verdict::Disabled,
        ),
        (
            Box::new(|d: &mut Driver| {
                let mut config = d.recorder.config.lock().unwrap();
                config.quiet_start = Some(22 * 60);
                config.quiet_end = Some(7 * 60);
                *d.recorder.minute.lock().unwrap() = Some(23 * 60);
            }),
            Verdict::Quiet,
        ),
        (
            Box::new(|d: &mut Driver| {
                *d.recorder.attended.lock().unwrap() = Some("term-1".into());
            }),
            Verdict::Attended,
        ),
    ] {
        let mut driver = Driver::new();
        setup(&mut driver);
        driver.tick(Some(notice("term-1")), Instant::now());
        assert_eq!(driver.recorder.count(), 0, "{}", verdict.label());
        assert_eq!(driver.counter(verdict), 1, "{}", verdict.label());
        assert_eq!(driver.counter(Verdict::Deliver), 0, "{}", verdict.label());
    }
}

#[test]
fn a_plain_shell_is_silent_until_the_user_opts_in() {
    let mut driver = Driver::new();
    let mut shell = notice("term-1");
    shell.is_agent = false;
    shell.label = "Shell".into();
    driver.tick(Some(shell.clone()), Instant::now());
    assert_eq!(driver.recorder.count(), 0);
    assert_eq!(driver.counter(Verdict::NotAnAgent), 1);

    driver.recorder.config.lock().unwrap().shell_bell = true;
    driver.tick(Some(shell), Instant::now() + COALESCE * 2);
    assert_eq!(driver.recorder.count(), 1);
}

#[test]
fn many_sessions_ringing_at_once_cannot_outrun_the_rate_limit() {
    let mut driver = Driver::new();
    let now = Instant::now();
    for session in 0..40 {
        driver.tick(Some(notice(&format!("term-{session}"))), now);
    }
    assert_eq!(driver.recorder.count(), RATE_CAPACITY as usize);
    assert_eq!(
        driver.counters.rate_limited.load(Ordering::Relaxed),
        40 - u64::from(RATE_CAPACITY)
    );
    // The bucket refills, so this is a delay and not a permanent gag.
    driver.tick(
        Some(notice("term-100")),
        now + RATE_REFILL + Duration::from_secs(1),
    );
    assert_eq!(driver.recorder.count(), RATE_CAPACITY as usize + 1);
}

#[test]
fn the_tracked_map_is_bounded_however_many_sessions_appear() {
    let mut driver = Driver::new();
    let start = Instant::now();
    for session in 0..1000u64 {
        driver.tick(
            Some(notice(&format!("term-{session}"))),
            start + Duration::from_millis(session),
        );
        assert!(driver.tracked.len() <= MAX_TRACKED);
    }
}

#[test]
fn coalescing_state_is_released_once_a_session_goes_quiet() {
    let mut driver = Driver::new();
    let start = Instant::now();
    driver.tick(Some(notice("term-1")), start);
    assert_eq!(driver.tracked.len(), 1);
    driver.tick(None, start + TRACK_TTL + Duration::from_secs(1));
    assert!(driver.tracked.is_empty());
}

#[test]
fn an_os_refusal_is_counted_and_reported_rather_than_read_as_delivery() {
    let mut driver = Driver::new();
    *driver.recorder.refuse.lock().unwrap() = true;
    driver.tick(Some(notice("term-1")), Instant::now());
    assert_eq!(driver.counters.failed.load(Ordering::Relaxed), 1);
    assert_eq!(driver.counter(Verdict::Deliver), 0);
    assert_eq!(
        driver.last_error.lock().unwrap().as_deref(),
        Some("notification centre unavailable")
    );
}

#[test]
fn a_later_success_clears_the_error_it_replaces() {
    let mut driver = Driver::new();
    let start = Instant::now();
    *driver.recorder.refuse.lock().unwrap() = true;
    driver.tick(Some(notice("term-1")), start);
    assert!(driver.last_error.lock().unwrap().is_some());
    *driver.recorder.refuse.lock().unwrap() = false;
    driver.tick(Some(notice("term-2")), start);
    assert!(driver.last_error.lock().unwrap().is_none());
}

#[test]
fn a_bell_with_nothing_to_say_still_says_something() {
    let mut driver = Driver::new();
    driver.tick(Some(notice("term-1")), Instant::now());
    assert_eq!(
        driver.recorder.bodies(),
        vec!["This session is asking for your attention.".to_string()]
    );
}

#[test]
fn a_reason_and_an_identical_detail_are_not_printed_twice() {
    let mut driver = Driver::new();
    let mut n = notice("term-1");
    n.reason = Some("finished its work".into());
    n.detail = Some("finished its work".into());
    driver.tick(Some(n), Instant::now());
    assert_eq!(
        driver.recorder.bodies(),
        vec!["finished its work".to_string()]
    );
}

#[test]
fn the_native_identifier_cannot_escape_its_namespace() {
    for key in [
        "../../../etc/passwd",
        "term-1;rm -rf /",
        "gitpulse.0123456789abcdef0123456789abcdef.event-1",
        &"x".repeat(400),
    ] {
        let native = notice(key).native_id();
        assert!(native.starts_with(NATIVE_PREFIX), "{native}");
        let parsed = native_session_key(&native).unwrap_or_else(|| panic!("{native}"));
        assert!(parsed.len() <= 96);
        assert!(parsed
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_'));
    }
}

#[test]
fn a_workbench_activity_identifier_is_not_a_session_identifier() {
    assert_eq!(
        native_session_key("gitpulse.0123456789abcdef0123456789abcdef.event-1"),
        None
    );
    assert_eq!(native_session_key("gitpulse.session."), None);
    assert_eq!(native_session_key("gitpulse.session.a/b"), None);
    assert_eq!(native_session_key("term-1"), None);
}
