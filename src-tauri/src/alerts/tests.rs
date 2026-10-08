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
    announced: Mutex<Vec<Attention>>,
    withdrawn: Mutex<Vec<String>>,
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
    fn announce(&self, attention: &Attention) {
        self.announced.lock().unwrap().push(attention.clone());
    }
    fn withdraw(&self, native: &str) {
        self.withdrawn.lock().unwrap().push(native.to_owned());
    }
}

fn notice(key: &str) -> Notice {
    Notice {
        key: key.to_owned(),
        origin: Origin::Terminal,
        label: "Claude Code".into(),
        place: Some("GitPulse".into()),
        event: None,
        detail: None,
        is_agent: true,
        channel: "bell",
        subject: None,
        subagent: false,
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
            if let Some(last) = admit(notice, now, &mut self.tracked, &self.counters) {
                self.host.announce(&last);
            }
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
    // Held, not dropped: each over the limit is counted once as delayed, and
    // none as lost.
    assert_eq!(
        driver.counters.deferred.load(Ordering::Relaxed),
        40 - u64::from(RATE_CAPACITY)
    );
    assert_eq!(driver.counters.rate_limited.load(Ordering::Relaxed), 0);
    // The bucket refills, so this is a delay and not a permanent gag.
    driver.tick(
        Some(notice("term-100")),
        now + RATE_REFILL + Duration::from_secs(1),
    );
    assert_eq!(driver.recorder.count(), RATE_CAPACITY as usize + 1);
    // And every one of them goes out in time, one token at a time, without a
    // new signal to prompt it.
    let mut at = now + RATE_REFILL + Duration::from_secs(1);
    for _ in 0..40 {
        at += RATE_REFILL;
        driver.tick(None, at);
    }
    assert_eq!(driver.recorder.count(), 41);
    assert_eq!(
        driver.counters.deferred.load(Ordering::Relaxed),
        41 - u64::from(RATE_CAPACITY)
    );
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
    n.event = Some("agent_completed");
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

// ---- Announcements: what the task's Agents pane is told ------------------

/// A hook report of `event`, by its name in [`bridge::EVENTS`].
fn hook(key: &str, event: &str, detail: Option<&str>) -> Notice {
    Notice {
        key: key.to_owned(),
        origin: Origin::Hook,
        label: "Claude Code".into(),
        place: None,
        event: Some(
            bridge::event(event)
                .unwrap_or_else(|| panic!("{event}"))
                .name,
        ),
        detail: detail.map(str::to_owned),
        is_agent: true,
        channel: "hook",
        subject: None,
        subagent: false,
    }
}

impl Recorder {
    fn announcements(&self) -> Vec<Attention> {
        self.announced.lock().unwrap().clone()
    }
}

#[test]
fn an_agent_notice_is_announced_at_once_even_when_its_banner_is_suppressed() {
    // Each of these suppresses the banner; none changes what the agent waits
    // for, and the pane must still be able to say so.
    type Suppress = Box<dyn Fn(&mut Driver)>;
    let suppressors: Vec<Suppress> = vec![
        Box::new(|d: &mut Driver| *d.recorder.attended.lock().unwrap() = Some("term-1".into())),
        Box::new(|d: &mut Driver| d.recorder.config.lock().unwrap().enabled = false),
        Box::new(|d: &mut Driver| {
            let mut config = d.recorder.config.lock().unwrap();
            config.quiet_start = Some(0);
            config.quiet_end = Some(23 * 60 + 59);
        }),
    ];
    for suppress in suppressors {
        let mut driver = Driver::new();
        suppress(&mut driver);
        driver.tick(
            Some(hook(
                "term-1",
                "permission_prompt",
                Some("Bash: cargo test"),
            )),
            Instant::now(),
        );
        assert_eq!(driver.recorder.count(), 0, "the banner was suppressed");
        assert_eq!(
            driver.recorder.announcements(),
            vec![Attention {
                session: "term-1".into(),
                channel: "hook",
                event: Some("permission_prompt"),
                detail: Some("Bash: cargo test".into()),
            }]
        );
    }
}

#[test]
fn announcements_are_not_held_for_the_banner_window() {
    let mut driver = Driver::new();
    let start = Instant::now();
    driver.tick(Some(hook("term-1", "agent_completed", None)), start);
    driver.tick(
        Some(hook("term-1", "permission_prompt", None)),
        start + ANNOUNCE_GAP + Duration::from_millis(1),
    );
    // Two announcements a second apart, while the banner for the second is
    // still waiting out its five-second window.
    assert_eq!(driver.recorder.announcements().len(), 2);
    assert_eq!(driver.recorder.count(), 1);
}

#[test]
fn a_burst_is_one_announcement_now_and_the_newest_after_the_gap() {
    let mut driver = Driver::new();
    let start = Instant::now();
    for step in 0..50u64 {
        let mut ring = notice("term-1");
        ring.detail = Some(format!("ring {step}"));
        driver.tick(Some(ring), start + Duration::from_millis(step * 10));
    }
    assert_eq!(driver.recorder.announcements().len(), 1);
    assert!(
        next_wake(&driver.tracked).is_some(),
        "the held announcement must wake the worker"
    );
    driver.tick(None, start + ANNOUNCE_GAP + Duration::from_millis(1));
    let said = driver.recorder.announcements();
    assert_eq!(said.len(), 2);
    assert_eq!(said[1].detail.as_deref(), Some("ring 49"));
    // A terminal signal says only that something was signalled.
    assert_eq!(said[1].event, None);
    assert_eq!(said[1].channel, "bell");
}

#[test]
fn every_hook_event_is_announced_by_its_name() {
    // Derived from the bridge's own table, so an event added there cannot
    // reach the pane as an unnamed signal.
    // A resolving event announces only when something stood for it to end,
    // which `answering_*` below covers.
    for event in bridge::EVENTS
        .iter()
        .filter(|e| e.role != bridge::Role::Resolve)
    {
        let mut driver = Driver::new();
        driver.tick(Some(hook("term-1", event.name, None)), Instant::now());
        assert_eq!(driver.recorder.announcements()[0].event, Some(event.name));
    }
}

#[test]
fn a_plain_shell_or_a_session_outside_gitpulse_is_never_announced() {
    let mut driver = Driver::new();
    let now = Instant::now();
    let mut shell = notice("term-1");
    shell.is_agent = false;
    driver.tick(Some(shell), now);
    driver.tick(
        Some(hook("hook-claude-GitPulse", "agent_completed", None)),
        now,
    );
    assert_eq!(driver.recorder.announcements(), vec![]);
}

#[test]
fn announced_text_is_sanitized_and_bounded() {
    let mut driver = Driver::new();
    let hostile = format!("\u{1b}]0;spoof\u{7}{}\u{1b}[31m", "y".repeat(5000));
    driver.tick(
        Some(hook("term-1", "agent_needs_input", Some(&hostile))),
        Instant::now(),
    );
    let said = driver.recorder.announcements();
    let detail = said[0].detail.as_deref().unwrap();
    assert!(detail.chars().count() <= ANNOUNCE_DETAIL_CHARS);
    assert!(
        !detail.contains('\u{1b}') && !detail.contains('\u{7}'),
        "{detail:?}"
    );
    // Blank text is absence, not an empty string the pane would render.
    let mut driver = Driver::new();
    driver.tick(
        Some(hook("term-2", "agent_needs_input", Some("   "))),
        Instant::now(),
    );
    assert_eq!(driver.recorder.announcements()[0].detail, None);
}

#[test]
fn a_closing_worker_makes_the_announcement_it_holds() {
    let mut driver = Driver::new();
    let start = Instant::now();
    driver.tick(Some(hook("term-1", "agent_completed", None)), start);
    let _ = admit(
        hook("term-1", "error", None),
        start,
        &mut driver.tracked,
        &driver.counters,
    );
    flush(
        start,
        true,
        &mut driver.tracked,
        &mut driver.bucket,
        &driver.host,
        &driver.counters,
        &driver.last_error,
    );
    let said = driver.recorder.announcements();
    assert_eq!(said.last().and_then(|a| a.event), Some("error"));
}

#[test]
fn a_held_announcement_survives_pruning_until_it_is_made() {
    let mut driver = Driver::new();
    let start = Instant::now();
    driver.tick(Some(hook("term-1", "agent_completed", None)), start);
    let _ = admit(
        hook("term-1", "agent_needs_input", None),
        start,
        &mut driver.tracked,
        &driver.counters,
    );
    prune(start + TRACK_TTL * 2, &mut driver.tracked);
    assert!(driver.tracked.contains_key("term-1"));
}

/// The announcement as it crosses into the renderer. The renderer's contract
/// test (`src/lib/terminal/sessionActivity.test.ts`) reads these two literals
/// and parses them, so a change to the struct's shape fails here first and
/// then shows the renderer the shape it must accept.
const WIRE_HOOK: &str = r#"{"session":"term-1","channel":"hook","event":"permission_prompt","detail":"Bash: cargo test"}"#;
const WIRE_SIGNAL: &str = r#"{"session":"term-1","channel":"bell","event":null,"detail":null}"#;

#[test]
fn attention_crosses_to_the_renderer_in_the_shape_it_parses() {
    let hook = Attention {
        session: "term-1".into(),
        channel: "hook",
        event: Some("permission_prompt"),
        detail: Some("Bash: cargo test".into()),
    };
    assert_eq!(serde_json::to_string(&hook).unwrap(), WIRE_HOOK);
    let signal = Attention {
        session: "term-1".into(),
        channel: "bell",
        event: None,
        detail: None,
    };
    assert_eq!(serde_json::to_string(&signal).unwrap(), WIRE_SIGNAL);
}

// ---- One request, two channels -------------------------------------------
//
// A GitPulse-launched Claude Code rings the terminal bell
// (`preferredNotifChannel: terminal_bell`) at the same moment its
// `Notification` hook reports over the socket, and both are keyed to the same
// PTY. Which lands first is a scheduling accident. Neither order may cost the
// reason the hook carried, and neither may cost the user a second banner.

#[test]
fn a_bell_behind_a_hook_report_does_not_erase_its_reason() {
    let mut driver = Driver::new();
    let start = Instant::now();
    driver.tick(
        Some(hook(
            "term-1",
            "permission_prompt",
            Some("Bash: cargo test"),
        )),
        start,
    );
    driver.tick(Some(notice("term-1")), start + Duration::from_millis(20));
    driver.tick(None, start + COALESCE * 3);
    assert_eq!(
        driver.recorder.bodies(),
        vec!["needs your permission — Bash: cargo test".to_string()],
        "the bell became a second, reasonless banner"
    );
    let said = driver.recorder.announcements();
    assert_eq!(said.len(), 1, "{said:?}");
    assert_eq!(said[0].event, Some("permission_prompt"));
}

#[test]
fn a_bell_racing_ahead_of_its_hook_report_yields_one_banner_with_the_reason() {
    let mut driver = Driver::new();
    let start = Instant::now();
    // The session has shown that its hooks report.
    driver.tick(Some(hook("term-1", "agent_completed", None)), start);
    let later = start + Duration::from_secs(120);
    driver.tick(Some(notice("term-1")), later);
    driver.tick(
        Some(hook(
            "term-1",
            "permission_prompt",
            Some("Bash: rm -rf target"),
        )),
        later + Duration::from_millis(40),
    );
    driver.tick(None, later + COALESCE * 3);
    assert_eq!(
        driver.recorder.bodies(),
        vec![
            "finished its work".to_string(),
            "needs your permission — Bash: rm -rf target".to_string()
        ],
        "a reasonless banner went out ahead of the report that explained it"
    );
}

#[test]
fn a_rate_limited_banner_is_delayed_not_lost() {
    let mut driver = Driver::new();
    let start = Instant::now();
    for session in 0..RATE_CAPACITY {
        driver.tick(Some(notice(&format!("term-{session}"))), start);
    }
    driver.tick(
        Some(hook("term-x", "permission_prompt", Some("Bash: deploy"))),
        start,
    );
    assert_eq!(driver.recorder.count(), RATE_CAPACITY as usize);
    assert!(
        next_wake(&driver.tracked).is_some(),
        "a held-back banner must wake the worker when a token returns"
    );
    driver.tick(None, start + RATE_REFILL + Duration::from_millis(1));
    assert_eq!(
        driver.recorder.bodies().last().map(String::as_str),
        Some("needs your permission — Bash: deploy"),
        "the rate limit discarded a permission prompt instead of delaying it"
    );
}

// ---- What stands, and what answers it ------------------------------------

fn tool(key: &str, event: &str, subject: &str, subagent: bool) -> Notice {
    let mut notice = hook(key, event, Some("Bash: cargo test"));
    notice.subject = Some(subject.to_owned());
    notice.subagent = subagent;
    notice
}

fn reader(key: &str) -> Notice {
    let mut notice = hook(key, "prompt_submitted", None);
    notice.origin = Origin::Reader;
    notice.channel = "reader";
    notice
}

impl Driver {
    fn standing(&self) -> Option<&'static str> {
        self.tracked
            .get("term-1")
            .and_then(|entry| entry.standing.as_ref())
            .and_then(|standing| standing.attention.event)
    }
    fn last_announced(&self) -> Option<&'static str> {
        self.recorder.announcements().last().and_then(|a| a.event)
    }
}

#[test]
fn a_permission_request_is_shown_at_once_and_never_bannered_by_itself() {
    let mut driver = Driver::new();
    let start = Instant::now();
    driver.tick(
        Some(tool("term-1", "permission_request", "aa", false)),
        start,
    );
    driver.tick(None, start + COALESCE * 4);
    assert_eq!(driver.recorder.count(), 0, "a state became a banner");
    assert_eq!(driver.last_announced(), Some("permission_request"));
    assert_eq!(driver.standing(), Some("permission_request"));
}

#[test]
fn the_notification_that_follows_a_permission_request_banners_it_with_the_tool() {
    let mut driver = Driver::new();
    let start = Instant::now();
    driver.tick(
        Some(tool("term-1", "permission_request", "aa", false)),
        start,
    );
    let mut late = hook(
        "term-1",
        "permission_prompt",
        Some("Claude needs your permission to use Bash"),
    );
    late.subject = None;
    driver.tick(Some(late), start + Duration::from_secs(6));
    assert_eq!(
        driver.recorder.bodies(),
        vec!["needs your permission — Bash: cargo test".to_string()]
    );
    // Same dialog, so the pane is not told twice and keeps the subject that
    // lets the tool's result end it.
    assert_eq!(driver.recorder.announcements().len(), 1);
    driver.tick(
        Some(tool("term-1", "tool_finished", "aa", false)),
        start + Duration::from_secs(9),
    );
    assert_eq!(driver.standing(), None);
    assert_eq!(driver.last_announced(), Some("tool_finished"));
}

#[test]
fn a_permission_ends_only_on_its_own_tool_call() {
    let mut driver = Driver::new();
    let start = Instant::now();
    driver.tick(
        Some(tool("term-1", "permission_request", "aa", false)),
        start,
    );
    // Another call in the same batch, and a subagent's, finish meanwhile.
    for (subject, subagent) in [("bb", false), ("cc", true)] {
        driver.tick(
            Some(tool("term-1", "tool_finished", subject, subagent)),
            start + Duration::from_secs(2),
        );
        assert_eq!(driver.standing(), Some("permission_request"), "{subject}");
    }
    // A result that names no call cannot be matched, so it cannot end one.
    let mut unnamed = hook("term-1", "tool_finished", None);
    unnamed.subject = None;
    driver.tick(Some(unnamed), start + Duration::from_secs(3));
    assert_eq!(driver.standing(), Some("permission_request"));
    // The subagent's own permission ends on the subagent's own result.
    driver.tick(
        Some(tool("term-1", "tool_finished", "aa", true)),
        start + Duration::from_secs(4),
    );
    assert_eq!(driver.standing(), None);
}

#[test]
fn the_main_agent_working_again_ends_a_finished_turn_but_a_subagent_does_not() {
    let mut driver = Driver::new();
    let start = Instant::now();
    driver.tick(Some(hook("term-1", "turn_finished", None)), start);
    driver.tick(
        Some(tool("term-1", "tool_finished", "aa", true)),
        start + Duration::from_secs(1),
    );
    assert_eq!(driver.standing(), Some("turn_finished"));
    driver.tick(
        Some(tool("term-1", "tool_finished", "bb", false)),
        start + Duration::from_secs(2),
    );
    assert_eq!(driver.standing(), None);
}

#[test]
fn an_answer_from_anywhere_ends_the_question_and_takes_its_banner_down() {
    for answer in [hook("term-1", "prompt_submitted", None), reader("term-1")] {
        let mut driver = Driver::new();
        let start = Instant::now();
        driver.tick(Some(hook("term-1", "idle_prompt", None)), start);
        assert_eq!(driver.recorder.count(), 1);
        driver.tick(Some(answer), start + Duration::from_secs(2));
        assert_eq!(driver.standing(), None);
        assert_eq!(driver.last_announced(), Some("prompt_submitted"));
        assert_eq!(
            *driver.recorder.withdrawn.lock().unwrap(),
            vec!["gitpulse.session.term-1".to_string()]
        );
        // Answered once; a second answer has nothing to end or take down.
        driver.tick(Some(reader("term-1")), start + Duration::from_secs(4));
        assert_eq!(driver.recorder.withdrawn.lock().unwrap().len(), 1);
        assert_eq!(driver.recorder.announcements().len(), 2);
    }
}

#[test]
fn a_banner_answered_before_it_was_due_is_never_shown() {
    let mut driver = Driver::new();
    let start = Instant::now();
    driver.tick(Some(hook("term-1", "agent_completed", None)), start);
    // Inside the window: held.
    driver.tick(
        Some(hook("term-1", "idle_prompt", None)),
        start + Duration::from_secs(1),
    );
    driver.tick(Some(reader("term-1")), start + Duration::from_secs(2));
    driver.tick(None, start + COALESCE * 3);
    assert_eq!(driver.recorder.count(), 1, "an answered question was asked");
    assert_eq!(driver.counters.resolved.load(Ordering::Relaxed), 1);
}

#[test]
fn a_session_that_ends_drops_its_question_but_keeps_its_last_word() {
    let ended = |key: &str| {
        let mut notice = hook(key, "session_ended", None);
        notice.origin = Origin::Terminal;
        notice.channel = "exit";
        notice
    };
    let mut driver = Driver::new();
    let start = Instant::now();
    driver.tick(Some(hook("term-1", "permission_prompt", None)), start);
    driver.tick(Some(ended("term-1")), start + Duration::from_secs(2));
    assert_eq!(driver.standing(), None, "a dead process still asks");
    for last in ["agent_completed", "error", "turn_finished"] {
        let mut driver = Driver::new();
        driver.tick(Some(hook("term-1", last, None)), start);
        driver.tick(Some(ended("term-1")), start + Duration::from_secs(2));
        assert_eq!(
            driver.standing(),
            Some(last),
            "{last} was erased by the exit"
        );
    }
}

#[test]
fn a_later_bell_reminds_of_the_hooks_question_instead_of_replacing_it() {
    let mut driver = Driver::new();
    let start = Instant::now();
    driver.tick(
        Some(hook("term-1", "permission_prompt", Some("Bash: deploy"))),
        start,
    );
    let later = start + Duration::from_secs(60);
    driver.tick(Some(notice("term-1")), later);
    driver.tick(None, later + HOOK_GRACE + Duration::from_millis(1));
    assert_eq!(
        driver.recorder.bodies(),
        vec![
            "needs your permission — Bash: deploy".to_string(),
            "needs your permission — Bash: deploy".to_string()
        ]
    );
    assert_eq!(driver.standing(), Some("permission_prompt"));
    assert_eq!(driver.recorder.announcements().len(), 1);
}

#[test]
fn a_session_without_hooks_still_bells_at_once() {
    // The grace is for a report that is coming. A session that never sent
    // one must not pay for it.
    let mut driver = Driver::new();
    driver.tick(Some(notice("term-1")), Instant::now());
    assert_eq!(driver.recorder.count(), 1);
}

#[test]
fn a_bell_whose_report_never_comes_is_still_delivered() {
    let mut driver = Driver::new();
    let start = Instant::now();
    driver.tick(Some(hook("term-1", "agent_completed", None)), start);
    let later = start + Duration::from_secs(600);
    driver.tick(Some(notice("term-1")), later);
    assert_eq!(driver.recorder.count(), 1, "held for its report");
    let wake = next_wake(&driver.tracked).expect("the held bell must wake the worker");
    assert!(wake <= HOOK_GRACE + Duration::from_secs(600));
    driver.tick(None, later + HOOK_GRACE);
    assert_eq!(driver.recorder.count(), 2, "the bell was lost waiting");
}

#[test]
fn under_the_rate_limit_a_stalled_agent_goes_before_a_finished_one() {
    let mut driver = Driver::new();
    let start = Instant::now();
    for session in 0..RATE_CAPACITY {
        driver.tick(Some(notice(&format!("term-{session}"))), start);
    }
    // Offered in the worse order: the finished one first.
    driver.tick(Some(hook("term-a", "agent_completed", None)), start);
    driver.tick(Some(hook("term-b", "permission_prompt", None)), start);
    driver.tick(None, start + RATE_REFILL);
    assert_eq!(
        driver.recorder.natives().last().map(String::as_str),
        Some("gitpulse.session.term-b")
    );
    driver.tick(None, start + RATE_REFILL * 2);
    assert_eq!(
        driver.recorder.natives().last().map(String::as_str),
        Some("gitpulse.session.term-a")
    );
}

#[test]
fn what_stands_is_what_a_page_that_was_not_listening_reads() {
    let mut driver = Driver::new();
    let start = Instant::now();
    driver.tick(
        Some(tool("term-1", "permission_request", "aa", false)),
        start,
    );
    driver.tick(Some(hook("term-2", "turn_finished", None)), start);
    driver.tick(Some(hook("term-3", "idle_prompt", None)), start);
    driver.tick(Some(reader("term-3")), start + Duration::from_secs(1));
    let standing = standing_of(&driver.tracked, start + Duration::from_secs(5));
    let events: Vec<_> = standing
        .iter()
        .map(|s| (s.session.as_str(), s.event.as_deref()))
        .collect();
    assert_eq!(
        events,
        vec![
            ("term-1", Some("permission_request")),
            ("term-2", Some("turn_finished"))
        ]
    );
    // Each says how long ago it was asked, so a page that missed it does not
    // call a five-second-old prompt new.
    assert_eq!(standing[0].age_ms, 5000);
    // A hook's question outlives the idle timeout that releases the rest.
    driver.tick(None, start + TRACK_TTL * 3);
    assert_eq!(standing_of(&driver.tracked, start).len(), 2);
}

/// What a page that was not listening reads, as it crosses into the renderer.
/// `sessionActivity.test.ts` parses this literal and replays it.
const WIRE_STANDING: &str = r#"[{"session":"term-1","channel":"hook","event":"permission_request","detail":"Bash: cargo test","age_ms":5000}]"#;

#[test]
fn what_stands_crosses_to_the_renderer_in_the_shape_it_replays() {
    let mut driver = Driver::new();
    let start = Instant::now();
    driver.tick(
        Some(tool("term-1", "permission_request", "aa", false)),
        start,
    );
    let standing = standing_of(&driver.tracked, start + Duration::from_secs(5));
    assert_eq!(serde_json::to_string(&standing).unwrap(), WIRE_STANDING);
}

#[test]
fn every_resolution_is_counted_so_the_outcomes_still_reconcile() {
    // Offered: 3. Delivered: 1. Coalesced into it: 1. Resolved before shown: 1.
    let mut driver = Driver::new();
    let start = Instant::now();
    driver.tick(Some(hook("term-1", "idle_prompt", None)), start);
    driver.tick(
        Some(hook("term-1", "agent_needs_input", None)),
        start + Duration::from_millis(100),
    );
    driver.tick(
        Some(hook("term-1", "idle_prompt", None)),
        start + Duration::from_millis(200),
    );
    driver.tick(Some(reader("term-1")), start + Duration::from_millis(300));
    let c = &driver.counters;
    assert_eq!(c.delivered.load(Ordering::Relaxed), 1);
    assert_eq!(c.coalesced.load(Ordering::Relaxed), 1);
    assert_eq!(c.resolved.load(Ordering::Relaxed), 1);
}

#[test]
fn an_answer_to_a_session_the_worker_forgot_still_clears_its_pane() {
    // Evicted past the bound, or asked before this process started: the pane
    // may still show the question, and the user's answer must still end it.
    let mut driver = Driver::new();
    driver.tick(Some(reader("term-1")), Instant::now());
    assert_eq!(driver.last_announced(), Some("prompt_submitted"));
    // Once known to be asking nothing, a second answer says nothing.
    driver.tick(
        Some(reader("term-1")),
        Instant::now() + Duration::from_secs(2),
    );
    assert_eq!(driver.recorder.announcements().len(), 1);
    // A plain shell, or a session outside GitPulse, has no pane to clear.
    let mut shell = reader("term-shell");
    shell.is_agent = false;
    driver.tick(Some(shell), Instant::now());
    driver.tick(Some(reader("hook-claude-x")), Instant::now());
    assert_eq!(driver.recorder.announcements().len(), 1);
}
