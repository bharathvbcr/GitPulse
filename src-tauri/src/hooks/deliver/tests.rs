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
    assert!(began.elapsed() < Duration::from_secs(5));
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
        assert!(began.elapsed() < Duration::from_secs(2), "{bad:?} waited");
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
                seen.elapsed() < Duration::from_secs(10),
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

#[test]
fn a_waiter_past_the_per_session_cap_exits_0_at_once() {
    let (_dir, mailbox) = mailbox();
    let cap = crate::agent_mailbox::MAX_WAITERS_PER_SESSION;
    std::thread::scope(|scope| {
        let held: Vec<_> = (0..cap)
            .map(|_| {
                let handle =
                    scope.spawn(|| run_in(&mailbox, &stop(SESSION), Duration::from_secs(20)));
                std::thread::sleep(Duration::from_millis(20));
                handle
            })
            .collect();
        let seen = Instant::now();
        while waiters_on(&mailbox, SESSION) < cap {
            assert!(
                seen.elapsed() < Duration::from_secs(10),
                "waiters never registered"
            );
            std::thread::sleep(Duration::from_millis(5));
        }
        let began = Instant::now();
        let extra = run_in(&mailbox, &stop(SESSION), Duration::from_secs(20));
        assert!(
            matches!(extra, Ok(ref output) if output.is_silent()),
            "{extra:?}"
        );
        assert!(
            began.elapsed() < Duration::from_secs(2),
            "the extra waiter waited"
        );
        // Release the ones holding places.
        for n in 0..cap {
            mailbox
                .deposit(SESSION, &format!("r{n}"), "release", SystemTime::now())
                .unwrap();
        }
        for handle in held {
            assert!(matches!(handle.join().unwrap(), Err(HookError::Wake(_))));
        }
    });
}

/// `stop_hook_active` is true while Claude is continuing because of a stop
/// hook, and a chain of stops can fire in quick succession. Each must not
/// start a waiter that lives for half an hour.
#[test]
fn a_repeat_in_a_stop_chain_looks_once_and_does_not_start_another_waiter() {
    let (_dir, mailbox) = mailbox();
    let mut chained = stop(SESSION);
    chained.stop_hook_active = true;
    let began = Instant::now();
    let outcome = run_in(&mailbox, &chained, Duration::from_secs(30));
    assert!(
        matches!(outcome, Ok(ref output) if output.is_silent()),
        "{outcome:?}"
    );
    assert!(
        began.elapsed() < Duration::from_secs(2),
        "a chained stop waited"
    );
    assert_eq!(waiters_on(&mailbox, SESSION), 0);
    // It still takes a message that is already there.
    mailbox
        .deposit(SESSION, "r1", "already here", SystemTime::now())
        .unwrap();
    assert!(matches!(
        run_in(&mailbox, &chained, Duration::from_secs(30)),
        Err(HookError::Wake(_))
    ));
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
    assert!(began.elapsed() < Duration::from_secs(5));
}
