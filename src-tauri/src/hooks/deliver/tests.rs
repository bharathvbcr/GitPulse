use super::*;
use crate::hooks::diagnostic;
use std::time::{Instant, SystemTime};

const SESSION: &str = "6f1c2a7e-0d4b-4c1e-9a55-3b0e8f2d9c11";

fn stop(session: &str) -> HookInput {
    HookInput {
        hook_event_name: "Stop".into(),
        session_id: session.into(),
        ..HookInput::default()
    }
}

fn mailbox() -> (tempfile::TempDir, Mailbox) {
    let dir = tempfile::tempdir().unwrap();
    let mailbox = Mailbox::new(dir.path().join("agent-mailbox"));
    (dir, mailbox)
}

fn waiters_on(mailbox: &Mailbox, session: &str) -> usize {
    std::fs::read_dir(mailbox_dir(mailbox, session))
        .map(|d| {
            d.flatten()
                .filter(|e| e.file_name().to_string_lossy().starts_with("w-"))
                .count()
        })
        .unwrap_or(0)
}

fn mailbox_dir(mailbox: &Mailbox, session: &str) -> std::path::PathBuf {
    mailbox.session_path(session).unwrap()
}

#[test]
fn a_message_wakes_the_agent_with_exactly_what_the_person_approved_and_exits_2() {
    let (_dir, mailbox) = mailbox();
    mailbox
        .deposit(
            SESSION,
            "r1",
            "CI failed on `build`.\nRun: https://example.test/run/1",
            SystemTime::now(),
        )
        .unwrap();
    let outcome = run_in(&mailbox, &stop(SESSION), Duration::from_secs(30));
    let Err(error) = outcome else {
        panic!("a pending message did not wake the agent: {outcome:?}");
    };
    let HookError::Wake(text) = &error else {
        panic!("a pending message was not a wake: {error:?}");
    };
    assert!(
        text.contains("CI failed on `build`.\nRun: https://example.test/run/1"),
        "{text}"
    );
    assert!(
        text.contains("person using GitPulse") && text.contains("not tool output"),
        "{text}"
    );
    assert_eq!(diagnostic(&error).exit_code, 2);
    // Delivered once: the message is claimed, so a second stop finds nothing.
    assert_eq!(mailbox.pending(SESSION), 0);
    assert!(matches!(
        run_in(&mailbox, &stop(SESSION), Duration::from_millis(100)),
        Ok(ref output) if output.is_silent()
    ));
}

#[test]
fn with_nothing_to_say_it_waits_its_limit_and_exits_0() {
    let (_dir, mailbox) = mailbox();
    let began = Instant::now();
    let outcome = run_in(&mailbox, &stop(SESSION), Duration::from_millis(400));
    assert!(
        matches!(outcome, Ok(ref output) if output.is_silent()),
        "{outcome:?}"
    );
    assert!(began.elapsed() >= Duration::from_millis(400));
    assert!(began.elapsed() < crate::test_support::coverage_relaxed(Duration::from_secs(5)));
    assert_eq!(
        waiters_on(&mailbox, SESSION),
        0,
        "the waiter left its registration behind"
    );
}

#[test]
fn a_session_it_does_not_know_exits_0_at_once_and_makes_no_mailbox() {
    let (dir, mailbox) = mailbox();
    for bad in [
        "",
        "not-a-session",
        "../../etc/passwd",
        "6f1c2a7e-0d4b-4c1e-9a55-3b0e8f2d9c1g",
    ] {
        let began = Instant::now();
        let outcome = run_in(&mailbox, &stop(bad), Duration::from_secs(30));
        assert!(
            matches!(outcome, Ok(ref output) if output.is_silent()),
            "{bad:?}: {outcome:?}"
        );
        assert!(
            began.elapsed() < crate::test_support::coverage_relaxed(Duration::from_secs(2)),
            "{bad:?} waited"
        );
    }
    assert!(!dir.path().join("agent-mailbox").exists());
}

/// Claude Code does not deduplicate a plugin's handler against a settings one,
/// so two copies of this hook run for one stop. One message must wake the
/// agent once.
#[test]
fn two_copies_of_the_hook_deliver_one_message_once() {
    let (_dir, mailbox) = mailbox();
    let (a, b) = std::thread::scope(|scope| {
        let first = scope.spawn(|| run_in(&mailbox, &stop(SESSION), Duration::from_millis(1500)));
        let second = scope.spawn(|| run_in(&mailbox, &stop(SESSION), Duration::from_millis(1500)));
        let seen = Instant::now();
        while waiters_on(&mailbox, SESSION) < 2 {
            assert!(
                seen.elapsed() < crate::test_support::coverage_relaxed(Duration::from_secs(10)),
                "the copies never both waited"
            );
            std::thread::sleep(Duration::from_millis(5));
        }
        mailbox
            .deposit(SESSION, "r1", "only one wake", SystemTime::now())
            .unwrap();
        (first.join().unwrap(), second.join().unwrap())
    });
    let wakes = [&a, &b]
        .iter()
        .filter(|o| matches!(o, Err(HookError::Wake(_))))
        .count();
    let quiet = [&a, &b]
        .iter()
        .filter(|o| matches!(o, Ok(output) if output.is_silent()))
        .count();
    assert_eq!((wakes, quiet), (1, 1), "{a:?} / {b:?}");
}

/// The listening window runs from the latest stop. Hooks from an earlier stop
/// that are still alive give way to a later stop's, so a session whose first
/// stop's hooks are about to time out is not deaf the moment they do. (Ranking
/// oldest-first declined every later stop's hooks and left exactly that.)
#[test]
fn a_later_stops_hooks_displace_an_earlier_stops_and_hear_the_message() {
    let (_dir, mailbox) = mailbox();
    let cap = crate::agent_mailbox::MAX_WAITERS_PER_SESSION;
    std::thread::scope(|scope| {
        let spawn_stop = || {
            (0..cap)
                .map(|_| {
                    let handle =
                        scope.spawn(|| run_in(&mailbox, &stop(SESSION), Duration::from_secs(20)));
                    std::thread::sleep(Duration::from_millis(20));
                    handle
                })
                .collect::<Vec<_>>()
        };
        let wait_for = |count: usize| {
            let seen = Instant::now();
            while waiters_on(&mailbox, SESSION) < count {
                assert!(
                    seen.elapsed() < crate::test_support::coverage_relaxed(Duration::from_secs(10)),
                    "waiters never registered"
                );
                std::thread::sleep(Duration::from_millis(5));
            }
        };
        let earlier = spawn_stop();
        wait_for(cap);
        let later = spawn_stop();
        // The earlier stop's hooks leave quietly, well inside their own limit.
        let began = Instant::now();
        for handle in earlier {
            let outcome = handle.join().unwrap();
            assert!(
                matches!(outcome, Ok(ref output) if output.is_silent()),
                "{outcome:?}"
            );
        }
        assert!(
            began.elapsed() < crate::test_support::coverage_relaxed(Duration::from_secs(5)),
            "an earlier stop's hooks kept waiting"
        );
        // The later stop's are the ones listening: a message wakes one of them.
        wait_for(cap);
        for n in 0..cap {
            mailbox
                .deposit(
                    SESSION,
                    &format!("r{n}"),
                    "for the latest stop",
                    SystemTime::now(),
                )
                .unwrap();
        }
        for handle in later {
            let outcome = handle.join().unwrap();
            assert!(matches!(outcome, Err(HookError::Wake(_))), "{outcome:?}");
        }
    });
}

/// A session with no place, arriving when every place in the mailbox is taken,
/// leaves at once with exit 0 instead of waiting.
#[test]
fn a_waiter_past_the_global_cap_exits_0_at_once() {
    let (_dir, mailbox) = mailbox();
    let per = crate::agent_mailbox::MAX_WAITERS_PER_SESSION;
    let sessions = crate::agent_mailbox::MAX_WAITERS_TOTAL / per;
    let session = |n: usize| format!("6f1c2a7e-0d4b-4c1e-9a55-{n:012x}");
    std::thread::scope(|scope| {
        let mut held = Vec::new();
        for n in 0..sessions {
            for _ in 0..per {
                let id = session(n);
                held.push(scope.spawn({
                    let mailbox = &mailbox;
                    move || run_in(mailbox, &stop(&id), Duration::from_secs(20))
                }));
                std::thread::sleep(Duration::from_millis(20));
            }
        }
        let seen = Instant::now();
        while (0..sessions)
            .map(|n| waiters_on(&mailbox, &session(n)))
            .sum::<usize>()
            < sessions * per
        {
            assert!(
                seen.elapsed() < crate::test_support::coverage_relaxed(Duration::from_secs(20)),
                "waiters never registered"
            );
            std::thread::sleep(Duration::from_millis(5));
        }
        let began = Instant::now();
        let extra = run_in(&mailbox, &stop(&session(99)), Duration::from_secs(20));
        assert!(
            matches!(extra, Ok(ref output) if output.is_silent()),
            "{extra:?}"
        );
        assert!(
            began.elapsed() < crate::test_support::coverage_relaxed(Duration::from_secs(2)),
            "the extra waiter waited"
        );
        for n in 0..sessions {
            for m in 0..per {
                mailbox
                    .deposit(&session(n), &format!("r{m}"), "release", SystemTime::now())
                    .unwrap();
            }
        }
        for handle in held {
            assert!(matches!(handle.join().unwrap(), Err(HookError::Wake(_))));
        }
    });
}

/// `stop_hook_active` is true while Claude is continuing because of a stop
/// hook, and the turn an `asyncRewake` wake starts is one. The stop that ends
/// it must still wait, or a session that has just been woken has no waiter
/// until the person types.
#[test]
fn a_stop_that_continues_a_stop_hook_still_waits_and_hears_a_later_message() {
    let (_dir, mailbox) = mailbox();
    let mut chained = stop(SESSION);
    chained.stop_hook_active = true;
    let outcome = std::thread::scope(|scope| {
        let waiter = scope.spawn(|| run_in(&mailbox, &chained, Duration::from_secs(20)));
        let seen = Instant::now();
        while waiters_on(&mailbox, SESSION) < 1 {
            assert!(
                seen.elapsed() < crate::test_support::coverage_relaxed(Duration::from_secs(10)),
                "a chained stop never waited"
            );
            std::thread::sleep(Duration::from_millis(5));
        }
        mailbox
            .deposit(SESSION, "late", "after the wake", SystemTime::now())
            .unwrap();
        waiter.join().unwrap()
    });
    assert!(matches!(outcome, Err(HookError::Wake(_))), "{outcome:?}");
}

/// …and with nothing to say it waits its own limit like any other stop and
/// leaves nothing behind.
#[test]
fn a_chained_stop_with_nothing_to_say_waits_its_limit_and_cleans_up() {
    let (_dir, mailbox) = mailbox();
    let mut chained = stop(SESSION);
    chained.stop_hook_active = true;
    let began = Instant::now();
    let outcome = run_in(&mailbox, &chained, Duration::from_millis(500));
    assert!(
        matches!(outcome, Ok(ref output) if output.is_silent()),
        "{outcome:?}"
    );
    assert!(began.elapsed() >= Duration::from_millis(500));
    assert_eq!(waiters_on(&mailbox, SESSION), 0);
}

#[cfg(unix)]
#[test]
fn a_mailbox_it_cannot_trust_is_a_logged_failure_that_still_exits_0() {
    use std::os::unix::fs::PermissionsExt;
    let (_dir, mailbox) = mailbox();
    mailbox
        .deposit(SESSION, "r1", "text", SystemTime::now())
        .unwrap();
    let session_dir = mailbox_dir(&mailbox, SESSION);
    std::fs::set_permissions(&session_dir, std::fs::Permissions::from_mode(0o775)).unwrap();
    let outcome = run_in(&mailbox, &stop(SESSION), Duration::from_secs(30));
    let Err(error) = outcome else {
        panic!("an untrusted mailbox was used: {outcome:?}");
    };
    assert!(matches!(error, HookError::Failed(_)), "{error:?}");
    assert_eq!(diagnostic(&error).exit_code, 0);
}

#[test]
fn the_wait_can_be_shortened_from_the_environment_and_never_lengthened() {
    assert_eq!(wait_limit(None), DELIVER_WAIT);
    assert_eq!(wait_limit(Some("250")), Duration::from_millis(250));
    assert_eq!(wait_limit(Some(" 250 ")), Duration::from_millis(250));
    assert_eq!(wait_limit(Some("0")), Duration::ZERO);
    assert_eq!(wait_limit(Some("99999999999")), DELIVER_WAIT);
    assert_eq!(wait_limit(Some("soon")), DELIVER_WAIT);
    assert_eq!(wait_limit(Some("-5")), DELIVER_WAIT);
}

#[test]
fn only_a_wake_is_exit_2() {
    for error in [
        HookError::Failed("x".into()),
        HookError::Unsupported("x".into()),
    ] {
        assert_eq!(diagnostic(&error).exit_code, 0);
    }
    assert_eq!(diagnostic(&HookError::Wake("x".into())).exit_code, 2);
}

/// The entry point reads the process environment: a session GitPulse did not
/// launch (no `GITPULSE_SESSION_ID`) is left alone; one it did is served from
/// the mailbox in GitPulse's configuration directory.
#[test]
fn the_entry_point_serves_only_sessions_gitpulse_launched() {
    let serial = crate::test_support::env::env_serial();
    let home = tempfile::tempdir().unwrap();
    let _env = crate::test_support::env::bind_env(&serial)
        .set("HOME", home.path())
        .set("XDG_CONFIG_HOME", home.path().join(".config"))
        .set(WAIT_ENV, "100")
        .remove(crate::alerts::bridge::SESSION_ENV);
    let mailbox = Mailbox::at_default().unwrap();
    mailbox
        .deposit(SESSION, "r1", "for a GitPulse session", SystemTime::now())
        .unwrap();

    // Not launched by GitPulse: it does not even look.
    let outsider = run(&stop(SESSION));
    assert!(
        matches!(outsider, Ok(ref output) if output.is_silent()),
        "{outsider:?}"
    );
    assert_eq!(
        mailbox.pending(SESSION),
        1,
        "a session GitPulse did not launch took a message"
    );

    drop(_env);
    let _env = crate::test_support::env::bind_env(&serial)
        .set("HOME", home.path())
        .set("XDG_CONFIG_HOME", home.path().join(".config"))
        .set(WAIT_ENV, "100")
        .set(crate::alerts::bridge::SESSION_ENV, "term-4-1");
    assert!(matches!(run(&stop(SESSION)), Err(HookError::Wake(_))));
    // And with nothing left it waits only the shortened limit.
    let began = Instant::now();
    assert!(matches!(run(&stop(SESSION)), Ok(ref output) if output.is_silent()));
    assert!(began.elapsed() < crate::test_support::coverage_relaxed(Duration::from_secs(5)));
}

#[test]
fn antigravity_stop_claims_immediately_without_blocking_and_respects_user_cancel() {
    let (_dir, mailbox) = mailbox();
    let mut agy_stop = HookInput {
        hook_event_name: "Stop".into(),
        session_id: SESSION.into(),
        is_agy: true,
        termination_reason: "NO_TOOL_CALL".into(),
        ..HookInput::default()
    };

    // Empty mailbox: returns immediately (0 ms poll) even when `wait` is 30 s.
    let began = Instant::now();
    let empty = run_in(&mailbox, &agy_stop, Duration::from_secs(30)).unwrap();
    assert!(empty.is_silent(), "{empty:?}");
    assert!(
        began.elapsed() < crate::test_support::coverage_relaxed(Duration::from_secs(1)),
        "Antigravity synchronous Stop hook blocked on an empty mailbox"
    );
    assert_eq!(
        empty.render_for_invocation("deliver", &agy_stop),
        Some("{}".to_string())
    );

    // User-canceled turn leaves any pending message untouched in the mailbox.
    mailbox
        .deposit(SESSION, "r-agy", "Check the split diff", SystemTime::now())
        .unwrap();
    agy_stop.termination_reason = "USER_CANCELED".into();
    let canceled = run_in(&mailbox, &agy_stop, Duration::from_secs(30)).unwrap();
    assert!(canceled.is_silent(), "{canceled:?}");
    assert_eq!(mailbox.pending(SESSION), 1);

    // Normal turn completion claims the message once and emits Antigravity's
    // `{"decision":"continue","reason":...}` JSON with exit code 0 (`Ok(...)`).
    agy_stop.termination_reason = "NO_TOOL_CALL".into();
    let claimed = run_in(&mailbox, &agy_stop, Duration::from_secs(30)).unwrap();
    let rendered = claimed
        .render_for_invocation("deliver", &agy_stop)
        .expect("continue JSON");
    let parsed: serde_json::Value = serde_json::from_str(&rendered).unwrap();
    assert_eq!(parsed["decision"], "continue");
    assert!(
        parsed["reason"]
            .as_str()
            .is_some_and(|r| r.contains("Check the split diff")),
        "{parsed}"
    );
    assert_eq!(mailbox.pending(SESSION), 0);
}

#[test]
fn peer_agent_messages_render_untrusted_xml_framing_and_pause_auto_wake_at_max_hops() {
    let (_dir, mailbox) = mailbox();
    mailbox
        .deposit_with_provenance(
            SESSION,
            "msg-peer-1",
            "Review `src/lib.rs` <tag> & \"quotes\"",
            EnvelopeProvenance::PeerAgent {
                msg_id: "msg-peer-1".into(),
                from_peer: "antigravity:peer-a".into(),
                provider: "antigravity".into(),
                branch: "feat/a2a".into(),
                thread_key: "thread-1".into(),
                hop: 1,
                max_hops: 6,
                truncated_in_bus: false,
                repo_root: String::new(),
            },
            SystemTime::now(),
        )
        .unwrap();
    let Err(HookError::Wake(rendered)) = run_in(&mailbox, &stop(SESSION), Duration::from_secs(5))
    else {
        panic!("expected wake for hop < max_hops");
    };
    assert!(!rendered.contains("person using GitPulse"), "{rendered}");
    assert!(
        rendered.contains("trust=\"unverified_peer_agent\""),
        "{rendered}"
    );
    assert!(
        rendered.contains("&lt;tag&gt; &amp; \"quotes\""),
        "{rendered}"
    );

    // At hop == max_hops, auto-wake declines without claiming so manual/boundary inspection keeps it.
    mailbox
        .deposit_with_provenance(
            SESSION,
            "msg-peer-cap",
            "Hop limit reached",
            EnvelopeProvenance::PeerAgent {
                msg_id: "msg-peer-cap".into(),
                from_peer: "antigravity:peer-a".into(),
                provider: "antigravity".into(),
                branch: "feat/a2a".into(),
                thread_key: "thread-1".into(),
                hop: 6,
                max_hops: 6,
                truncated_in_bus: false,
                repo_root: String::new(),
            },
            SystemTime::now(),
        )
        .unwrap();
    let declined = run_in(&mailbox, &stop(SESSION), Duration::from_secs(5));
    assert!(
        matches!(declined, Ok(ref out) if out.is_silent()),
        "{declined:?}"
    );
    assert_eq!(mailbox.pending(SESSION), 1);
}
