use super::*;
use crate::engine::agent_bus::DEFAULT_MAX_HOPS;
use std::sync::atomic::{AtomicUsize, Ordering};

const SESSION: &str = "6f1c2a7e-0d4b-4c1e-9a55-3b0e8f2d9c11";

fn mailbox() -> (tempfile::TempDir, Mailbox) {
    let dir = tempfile::tempdir().unwrap();
    let mailbox = Mailbox::new(dir.path().join("agent-mailbox"));
    (dir, mailbox)
}

fn session_n(n: usize) -> String {
    format!("6f1c2a7e-0d4b-4c1e-9a55-{n:012x}")
}

fn names(mailbox: &Mailbox, session: &str) -> Vec<String> {
    let mut found: Vec<String> = std::fs::read_dir(mailbox.root.join(session))
        .map(|d| {
            d.flatten()
                .filter_map(|e| e.file_name().into_string().ok())
                .collect()
        })
        .unwrap_or_default();
    found.sort();
    found
}

#[test]
fn a_message_is_published_whole_in_a_private_file_and_leaves_no_temporary_behind() {
    let (_dir, mailbox) = mailbox();
    let now = SystemTime::now();
    let deposit = mailbox
        .deposit(SESSION, "req-1", "CI failed on `build`.\nSee the run.", now)
        .unwrap();
    assert!(!deposit.duplicate);
    assert_eq!(
        deposit.expires_at_ms - deposit.created_at_ms,
        millis_of(PENDING_TTL)
    );
    let files = names(&mailbox, SESSION);
    assert_eq!(files.len(), 1, "{files:?}");
    assert!(
        files[0].starts_with("p-") && files[0].ends_with("-req-1.msg"),
        "{files:?}"
    );
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = |p: PathBuf| std::fs::metadata(p).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode(mailbox.root.clone()), 0o700);
        assert_eq!(mode(mailbox.root.join(SESSION)), 0o700);
        assert_eq!(mode(mailbox.root.join(SESSION).join(&files[0])), 0o600);
    }
    assert_eq!(
        read_message(&mailbox.root.join(SESSION).join(&files[0])).unwrap(),
        "CI failed on `build`.\nSee the run."
    );
}

#[test]
fn a_write_in_progress_is_never_a_message() {
    let (_dir, mailbox) = mailbox();
    let now = SystemTime::now();
    mailbox.deposit(SESSION, "req-1", "first", now).unwrap();
    let dir = mailbox.root.join(SESSION);
    // What a crashed writer leaves: a temporary with half a message.
    std::fs::write(dir.join(".tmp-1-deadbeef"), "half a mess").unwrap();
    let claim = mailbox.claim_next(SESSION, now).unwrap().unwrap();
    assert_eq!(claim.text, "first");
    assert_eq!(mailbox.claim_next(SESSION, now).unwrap(), None);
}

#[test]
fn of_many_racing_claimers_exactly_one_gets_a_message() {
    for round in 0..25 {
        let (_dir, mailbox) = mailbox();
        let now = SystemTime::now();
        mailbox.deposit(SESSION, "req-1", "only once", now).unwrap();
        let wins = AtomicUsize::new(0);
        let start = std::sync::Barrier::new(8);
        std::thread::scope(|scope| {
            for _ in 0..8 {
                scope.spawn(|| {
                    start.wait();
                    if mailbox.claim_next(SESSION, now).unwrap().is_some() {
                        wins.fetch_add(1, Ordering::SeqCst);
                    }
                });
            }
        });
        assert_eq!(wins.load(Ordering::SeqCst), 1, "round {round}");
        assert_eq!(mailbox.status(SESSION, "req-1"), Delivery::Claimed);
    }
}

#[test]
fn racing_claimers_split_several_messages_without_losing_or_repeating_one() {
    let (_dir, mailbox) = mailbox();
    let now = SystemTime::now();
    for n in 0..MAX_PENDING {
        mailbox
            .deposit(SESSION, &format!("req-{n}"), &format!("message {n}"), now)
            .unwrap();
    }
    let taken = Mutex::new(Vec::new());
    let start = std::sync::Barrier::new(6);
    std::thread::scope(|scope| {
        for _ in 0..6 {
            scope.spawn(|| {
                start.wait();
                while let Some(claim) = mailbox.claim_next(SESSION, now).unwrap() {
                    taken.lock().unwrap().push(claim.text);
                }
            });
        }
    });
    let mut taken = taken.into_inner().unwrap();
    taken.sort();
    let mut want: Vec<String> = (0..MAX_PENDING).map(|n| format!("message {n}")).collect();
    want.sort();
    assert_eq!(taken, want);
}

#[test]
fn messages_are_taken_oldest_first() {
    let (_dir, mailbox) = mailbox();
    let start = SystemTime::now();
    for (n, text) in ["one", "two", "three"].into_iter().enumerate() {
        mailbox
            .deposit(
                SESSION,
                &format!("req-{n}"),
                text,
                start + Duration::from_millis(n as u64 * 5),
            )
            .unwrap();
    }
    let order: Vec<String> = std::iter::from_fn(|| mailbox.claim_next(SESSION, start).unwrap())
        .map(|claim| claim.text)
        .collect();
    assert_eq!(order, ["one", "two", "three"]);
}

#[test]
fn an_unclaimed_message_expires_and_a_claimed_one_is_forgotten_after_its_window() {
    let (_dir, mailbox) = mailbox();
    let now = SystemTime::now();
    mailbox
        .deposit(SESSION, "stale", "never taken", now)
        .unwrap();
    let later = now + PENDING_TTL + Duration::from_secs(60);
    assert_eq!(
        mailbox.claim_next(SESSION, later).unwrap(),
        None,
        "an expired message was delivered"
    );
    assert_eq!(mailbox.status(SESSION, "stale"), Delivery::Absent);

    mailbox.deposit(SESSION, "taken", "taken", now).unwrap();
    assert!(mailbox.claim_next(SESSION, now).unwrap().is_some());
    assert_eq!(mailbox.status(SESSION, "taken"), Delivery::Claimed);
    assert_eq!(
        mailbox
            .claim_next(SESSION, now + CLAIMED_TTL + Duration::from_secs(60))
            .unwrap(),
        None
    );
    assert_eq!(mailbox.status(SESSION, "taken"), Delivery::Absent);
}

#[test]
fn the_caps_on_size_pending_count_and_sessions_are_enforced() {
    let (_dir, mailbox) = mailbox();
    let now = SystemTime::now();
    assert!(matches!(
        mailbox.deposit(SESSION, "big", &"x".repeat(MAX_MESSAGE_BYTES + 1), now),
        Err(MailboxError::Invalid(_))
    ));
    mailbox
        .deposit(SESSION, "exact", &"x".repeat(MAX_MESSAGE_BYTES), now)
        .unwrap();
    assert!(mailbox.claim_next(SESSION, now).unwrap().is_some());

    for n in 0..MAX_PENDING {
        mailbox
            .deposit(SESSION, &format!("req-{n}"), "text", now)
            .unwrap();
    }
    assert!(matches!(
        mailbox.deposit(SESSION, "one-too-many", "text", now),
        Err(MailboxError::Full(_))
    ));

    for n in 1..MAX_SESSIONS {
        mailbox.deposit(&session_n(n), "r", "text", now).unwrap();
    }
    assert!(matches!(
        mailbox.deposit(&session_n(MAX_SESSIONS + 1), "r", "text", now),
        Err(MailboxError::Full(_))
    ));
    // A session that already has a directory is still served at the cap.
    assert!(mailbox.claim_next(&session_n(1), now).unwrap().is_some());
}

#[test]
fn a_retried_request_writes_once_and_a_reused_id_with_other_text_is_refused() {
    let (_dir, mailbox) = mailbox();
    let now = SystemTime::now();
    let first = mailbox.deposit(SESSION, "req-1", "same", now).unwrap();
    let again = mailbox
        .deposit(SESSION, "req-1", "same", now + Duration::from_secs(5))
        .unwrap();
    assert!(again.duplicate);
    assert_eq!(again.created_at_ms, first.created_at_ms);
    assert_eq!(mailbox.pending(SESSION), 1);
    assert!(matches!(
        mailbox.deposit(SESSION, "req-1", "different", now),
        Err(MailboxError::Invalid(_))
    ));
    // After it has been claimed the retry still answers, and still writes nothing.
    mailbox.claim_next(SESSION, now).unwrap().unwrap();
    assert!(
        mailbox
            .deposit(SESSION, "req-1", "same", now)
            .unwrap()
            .duplicate
    );
    assert_eq!(mailbox.pending(SESSION), 0);
}

#[test]
fn only_a_canonical_session_id_names_a_mailbox() {
    let (_dir, mailbox) = mailbox();
    let now = SystemTime::now();
    for bad in [
        "",
        "../../etc",
        "6f1c2a7e",
        "6f1c2a7e-0d4b-4c1e-9a55-3b0e8f2d9c1g",
        "6f1c2a7e/0d4b-4c1e-9a55-3b0e8f2d9c11",
    ] {
        assert!(
            matches!(
                mailbox.deposit(bad, "r", "text", now),
                Err(MailboxError::Invalid(_))
            ),
            "{bad:?}"
        );
        assert!(mailbox.claim_next(bad, now).is_err(), "{bad:?}");
    }
    // Upper case names the same mailbox as lower case.
    mailbox
        .deposit(&SESSION.to_ascii_uppercase(), "r", "text", now)
        .unwrap();
    assert!(mailbox.claim_next(SESSION, now).unwrap().is_some());
}

#[test]
fn message_text_must_be_plain_and_nonblank() {
    for bad in [
        "",
        "   \n\t ",
        "a\0b",
        "esc \u{1b}[31m",
        "bell \u{7}",
        "del \u{7f}",
        "c1 \u{9b}",
    ] {
        assert!(validate_message(bad).is_err(), "{bad:?}");
    }
    assert!(validate_message("line one\n\tline two ✓").is_ok());
}

#[cfg(unix)]
mod hostile {
    use super::*;
    use std::os::unix::fs::{symlink, PermissionsExt};

    #[test]
    fn a_symlinked_session_directory_is_refused_and_nothing_is_written_through_it() {
        let (dir, mailbox) = mailbox();
        let now = SystemTime::now();
        let elsewhere = dir.path().join("elsewhere");
        std::fs::create_dir(&elsewhere).unwrap();
        std::fs::set_permissions(&elsewhere, std::fs::Permissions::from_mode(0o700)).unwrap();
        mailbox.deposit(&session_n(9), "r", "seed", now).unwrap();
        symlink(&elsewhere, mailbox.root.join(SESSION)).unwrap();
        assert!(matches!(
            mailbox.deposit(SESSION, "r", "text", now),
            Err(MailboxError::Insecure(_))
        ));
        assert!(matches!(
            mailbox.claim_next(SESSION, now),
            Err(MailboxError::Insecure(_))
        ));
        assert_eq!(std::fs::read_dir(&elsewhere).unwrap().count(), 0);
    }

    #[test]
    fn a_symlinked_or_widened_mailbox_root_is_refused() {
        let (dir, mailbox) = mailbox();
        let now = SystemTime::now();
        let real = dir.path().join("real");
        std::fs::create_dir(&real).unwrap();
        std::fs::set_permissions(&real, std::fs::Permissions::from_mode(0o700)).unwrap();
        symlink(&real, &mailbox.root).unwrap();
        assert!(matches!(
            mailbox.deposit(SESSION, "r", "text", now),
            Err(MailboxError::Insecure(_))
        ));

        let (_dir2, widened) = super::mailbox();
        widened.deposit(SESSION, "r", "text", now).unwrap();
        std::fs::set_permissions(&widened.root, std::fs::Permissions::from_mode(0o755)).unwrap();
        assert!(matches!(
            widened.deposit(SESSION, "r2", "text", now),
            Err(MailboxError::Insecure(_))
        ));
        std::fs::set_permissions(&widened.root, std::fs::Permissions::from_mode(0o700)).unwrap();
        std::fs::set_permissions(
            widened.root.join(SESSION),
            std::fs::Permissions::from_mode(0o770),
        )
        .unwrap();
        assert!(matches!(
            widened.claim_next(SESSION, now),
            Err(MailboxError::Insecure(_))
        ));
    }

    #[test]
    fn a_symlink_planted_as_a_message_is_never_followed() {
        let (dir, mailbox) = mailbox();
        let now = SystemTime::now();
        let secret = dir.path().join("secret.txt");
        std::fs::write(&secret, "do not deliver me").unwrap();
        std::fs::set_permissions(&secret, std::fs::Permissions::from_mode(0o600)).unwrap();
        mailbox.deposit(SESSION, "seed", "seed", now).unwrap();
        mailbox.claim_next(SESSION, now).unwrap().unwrap();
        let planted = mailbox
            .root
            .join(SESSION)
            .join(format!("p-{:013}-planted.msg", millis(now)));
        symlink(&secret, &planted).unwrap();
        assert_eq!(mailbox.claim_next(SESSION, now).unwrap(), None);
        assert_eq!(mailbox.pending(SESSION), 0);
        assert_eq!(
            std::fs::read_to_string(&secret).unwrap(),
            "do not deliver me"
        );
    }

    #[test]
    fn a_message_file_open_to_others_is_not_delivered() {
        let (_dir, mailbox) = mailbox();
        let now = SystemTime::now();
        mailbox.deposit(SESSION, "r", "text", now).unwrap();
        let file = names(&mailbox, SESSION).remove(0);
        let path = mailbox.root.join(SESSION).join(file);
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o644)).unwrap();
        assert_eq!(mailbox.claim_next(SESSION, now).unwrap(), None);
        assert!(read_message(&path).is_err());
    }

    #[test]
    fn an_oversized_or_non_text_file_is_dropped_and_the_next_message_still_arrives() {
        let (_dir, mailbox) = mailbox();
        let now = SystemTime::now();
        mailbox
            .deposit(
                SESSION,
                "good",
                "the good one",
                now + Duration::from_millis(10),
            )
            .unwrap();
        let dir = mailbox.root.join(SESSION);
        let bad = dir.join(format!("p-{:013}-bad.msg", millis(now)));
        std::fs::write(&bad, vec![b'x'; MAX_MESSAGE_BYTES + 1]).unwrap();
        std::fs::set_permissions(&bad, std::fs::Permissions::from_mode(0o600)).unwrap();
        let binary = dir.join(format!("p-{:013}-bin.msg", millis(now) + 1));
        std::fs::write(&binary, [0xff, 0xfe, 0x00]).unwrap();
        std::fs::set_permissions(&binary, std::fs::Permissions::from_mode(0o600)).unwrap();
        let claim = mailbox.claim_next(SESSION, now).unwrap().unwrap();
        assert_eq!(claim.text, "the good one");
        assert_eq!(mailbox.status(SESSION, "bad"), Delivery::Absent);
        assert_eq!(mailbox.status(SESSION, "bin"), Delivery::Absent);
    }
}

/// Registers a waiter and lets the clock tick, so the next one is newer by name.
fn register_next(mailbox: &Mailbox, dir: &Path) -> Waiter {
    let waiter = Waiter::register(mailbox, dir).unwrap();
    std::thread::sleep(Duration::from_millis(3));
    waiter
}

/// The per-session cap never declines a newcomer: the newest waiters hold the
/// places and the oldest gives way. (The reverse, oldest-first, left a session
/// deaf from the first stop's timeout even if it stopped a minute before.)
#[test]
fn the_newest_waiters_on_a_session_take_precedence_over_older_ones() {
    let (_dir, mailbox) = mailbox();
    let dir = mailbox.ensure_session(SESSION, SystemTime::now()).unwrap();
    let mut earlier: Vec<Waiter> = (0..MAX_WAITERS_PER_SESSION)
        .map(|_| register_next(&mailbox, &dir))
        .collect();
    for waiter in &mut earlier {
        assert!(!waiter.displaced().unwrap(), "no one is newer yet");
    }
    let mut later: Vec<Waiter> = (0..MAX_WAITERS_PER_SESSION)
        .map(|_| register_next(&mailbox, &dir))
        .collect();
    for waiter in &mut later {
        assert!(!waiter.displaced().unwrap(), "the latest stop's hooks stay");
    }
    for waiter in &mut earlier {
        assert!(waiter.displaced().unwrap(), "an earlier stop's hook stays");
    }
    // One newer waiter displaces only the oldest of a full set.
    let (_dir, mailbox) = self::mailbox();
    let dir = mailbox.ensure_session(SESSION, SystemTime::now()).unwrap();
    let mut held: Vec<Waiter> = (0..MAX_WAITERS_PER_SESSION)
        .map(|_| register_next(&mailbox, &dir))
        .collect();
    let mut newcomer = register_next(&mailbox, &dir);
    assert!(!newcomer.displaced().unwrap());
    assert!(held[0].displaced().unwrap());
    assert!(!held[1].displaced().unwrap());
    // A displaced waiter that is dropped frees its file.
    drop(held.remove(0));
    assert_eq!(
        mailbox.live_waiters(SystemTime::now()).len(),
        MAX_WAITERS_PER_SESSION
    );
}

/// The wait itself, not only the ranking: a waiter that a later stop's hooks
/// have displaced gives up with `Declined`, and does not take a message.
#[test]
fn a_waiter_displaced_by_a_later_stop_leaves_without_taking_the_message() {
    let (_dir, mailbox) = mailbox();
    let dir = mailbox.ensure_session(SESSION, SystemTime::now()).unwrap();
    let outcome = std::thread::scope(|scope| {
        let old = scope.spawn(|| mailbox.wait(SESSION, Duration::from_secs(30)).unwrap());
        let seen = Instant::now();
        while !mailbox.listening(SESSION, SystemTime::now()) {
            assert!(
                seen.elapsed() < crate::test_support::coverage_relaxed(Duration::from_secs(10)),
                "the waiter never registered"
            );
            std::thread::sleep(Duration::from_millis(5));
        }
        std::thread::sleep(Duration::from_millis(5));
        // The later stop's hooks arrive.
        let _newer: Vec<Waiter> = (0..MAX_WAITERS_PER_SESSION)
            .map(|_| register_next(&mailbox, &dir))
            .collect();
        old.join().unwrap()
    });
    assert!(matches!(outcome, Wait::Declined(_)), "{outcome:?}");
}

/// A process suspended long enough for its registration to be pruned as stale
/// puts it back under its own name, instead of carrying on uncounted.
#[test]
fn a_waiter_whose_registration_was_pruned_is_counted_again_under_its_own_name() {
    let (_dir, mailbox) = mailbox();
    let dir = mailbox.ensure_session(SESSION, SystemTime::now()).unwrap();
    let mut waiter = register_next(&mailbox, &dir);
    std::fs::remove_file(&waiter.path).unwrap();
    assert!(!mailbox.listening(SESSION, SystemTime::now()));
    assert!(!waiter.displaced().unwrap());
    assert!(mailbox.listening(SESSION, SystemTime::now()));
    assert_eq!(mailbox.live_waiters(SystemTime::now())[0].1, waiter.name);
}

#[test]
fn a_waiter_beyond_the_global_cap_is_told_to_leave() {
    let (_dir, mailbox) = mailbox();
    let mut held = Vec::new();
    for n in 0..MAX_WAITERS_TOTAL / MAX_WAITERS_PER_SESSION {
        let session = session_n(n);
        let dir = mailbox.ensure_session(&session, SystemTime::now()).unwrap();
        for _ in 0..MAX_WAITERS_PER_SESSION {
            held.push(Waiter::register(&mailbox, &dir).unwrap());
            std::thread::sleep(Duration::from_millis(2));
        }
    }
    let extra = session_n(99);
    let dir = mailbox.ensure_session(&extra, SystemTime::now()).unwrap();
    assert!(matches!(
        Waiter::register(&mailbox, &dir),
        Err(MailboxError::Full(_))
    ));
}

#[test]
fn a_waiter_that_stops_beating_stops_counting() {
    let (_dir, mailbox) = mailbox();
    let dir = mailbox.ensure_session(SESSION, SystemTime::now()).unwrap();
    let _waiter = Waiter::register(&mailbox, &dir).unwrap();
    let now = SystemTime::now();
    assert!(mailbox.listening(SESSION, now));
    assert!(!mailbox.listening(SESSION, now + WAITER_STALE + Duration::from_secs(1)));
    assert!(!mailbox.listening(&session_n(5), now));
}

#[test]
fn waiting_takes_a_message_that_is_already_there_at_once() {
    let (_dir, mailbox) = mailbox();
    mailbox
        .deposit(SESSION, "r", "waiting for you", SystemTime::now())
        .unwrap();
    let began = Instant::now();
    let outcome = mailbox.wait(SESSION, Duration::from_secs(30)).unwrap();
    assert_eq!(
        outcome,
        Wait::Message(Claim {
            request_id: "r".into(),
            text: "waiting for you".into()
        })
    );
    assert!(began.elapsed() < crate::test_support::coverage_relaxed(Duration::from_secs(5)));
}

#[test]
fn waiting_takes_a_message_that_arrives_while_it_waits_and_then_leaves_no_trace() {
    let (_dir, mailbox) = mailbox();
    let outcome = std::thread::scope(|scope| {
        let waiter = scope.spawn(|| mailbox.wait(SESSION, Duration::from_secs(30)).unwrap());
        // Let the waiter register before the message exists.
        let seen = Instant::now();
        while !mailbox.listening(SESSION, SystemTime::now()) {
            assert!(
                seen.elapsed() < crate::test_support::coverage_relaxed(Duration::from_secs(10)),
                "the waiter never registered"
            );
            std::thread::sleep(Duration::from_millis(5));
        }
        mailbox
            .deposit(SESSION, "late", "it came after", SystemTime::now())
            .unwrap();
        waiter.join().unwrap()
    });
    assert_eq!(
        outcome,
        Wait::Message(Claim {
            request_id: "late".into(),
            text: "it came after".into()
        })
    );
    assert!(
        !mailbox.listening(SESSION, SystemTime::now()),
        "the waiter left its registration behind"
    );
}

#[test]
fn waiting_with_nothing_to_take_times_out_at_its_own_limit() {
    let (_dir, mailbox) = mailbox();
    let began = Instant::now();
    let outcome = mailbox.wait(SESSION, Duration::from_millis(600)).unwrap();
    let took = began.elapsed();
    assert_eq!(outcome, Wait::TimedOut);
    assert!(took >= Duration::from_millis(600), "{took:?}");
    assert!(took < Duration::from_secs(5), "{took:?}");
    assert!(!mailbox.listening(SESSION, SystemTime::now()));
}

#[test]
fn a_waiter_over_the_global_cap_declines_instead_of_waiting() {
    let (_dir, mailbox) = mailbox();
    let mut held = Vec::new();
    for n in 0..MAX_WAITERS_TOTAL / MAX_WAITERS_PER_SESSION {
        let session = session_n(n);
        let dir = mailbox.ensure_session(&session, SystemTime::now()).unwrap();
        for _ in 0..MAX_WAITERS_PER_SESSION {
            held.push(register_next(&mailbox, &dir));
        }
    }
    let began = Instant::now();
    let outcome = mailbox.wait(SESSION, Duration::from_secs(30)).unwrap();
    assert!(matches!(outcome, Wait::Declined(_)), "{outcome:?}");
    assert!(began.elapsed() < crate::test_support::coverage_relaxed(Duration::from_secs(5)));
    drop(held);
}

/// With every place in the mailbox taken, a later stop on a session that
/// already holds two is still heard: its older waiters are leaving, so they do
/// not count against the cap. A session that holds none is declined.
#[test]
fn a_saturated_mailbox_still_lets_a_held_session_listen_from_its_latest_stop() {
    let (_dir, mailbox) = mailbox();
    let mut held = Vec::new();
    let mut dirs = Vec::new();
    for n in 0..MAX_WAITERS_TOTAL / MAX_WAITERS_PER_SESSION {
        let session = session_n(n);
        let dir = mailbox.ensure_session(&session, SystemTime::now()).unwrap();
        for _ in 0..MAX_WAITERS_PER_SESSION {
            held.push(register_next(&mailbox, &dir));
        }
        dirs.push(dir);
    }
    assert_eq!(
        mailbox.live_waiters(SystemTime::now()).len(),
        MAX_WAITERS_TOTAL
    );
    // A later stop on session 0: accepted, and its oldest waiter gives way.
    let mut newest = Waiter::register(&mailbox, &dirs[0]).expect("a held session was declined");
    assert!(!newest.displaced().unwrap());
    assert!(held[0].displaced().unwrap());
    // A session with no place is still declined at the cap.
    let extra = session_n(99);
    let extra_dir = mailbox.ensure_session(&extra, SystemTime::now()).unwrap();
    assert!(matches!(
        Waiter::register(&mailbox, &extra_dir),
        Err(MailboxError::Full(_))
    ));
    // The declined one removed its own file.
    assert_eq!(
        mailbox.live_waiters(SystemTime::now()).len(),
        MAX_WAITERS_TOTAL + 1
    );
}

/// A full session is not a closed door to the next stop: its hooks wait, and
/// the oldest waiter gives way. The previous rule declined them at once.
#[test]
fn a_waiter_arriving_at_a_full_session_waits_and_hears_the_message() {
    let (_dir, mailbox) = mailbox();
    let dir = mailbox.ensure_session(SESSION, SystemTime::now()).unwrap();
    let mut held: Vec<Waiter> = (0..MAX_WAITERS_PER_SESSION)
        .map(|_| register_next(&mailbox, &dir))
        .collect();
    let outcome = std::thread::scope(|scope| {
        let newcomer = scope.spawn(|| mailbox.wait(SESSION, Duration::from_secs(30)).unwrap());
        let seen = Instant::now();
        while mailbox.live_waiters(SystemTime::now()).len() <= MAX_WAITERS_PER_SESSION {
            assert!(
                seen.elapsed() < crate::test_support::coverage_relaxed(Duration::from_secs(10)),
                "the newcomer never registered"
            );
            std::thread::sleep(Duration::from_millis(5));
        }
        assert!(held[0].displaced().unwrap(), "the oldest kept its place");
        mailbox
            .deposit(SESSION, "r", "for the newest", SystemTime::now())
            .unwrap();
        newcomer.join().unwrap()
    });
    assert!(matches!(outcome, Wait::Message(_)), "{outcome:?}");
}

#[test]
fn crlf_is_normalized_to_lf_while_bare_carriage_return_is_still_refused() {
    let (_dir, mailbox) = mailbox();
    let now = SystemTime::now();
    mailbox
        .deposit(SESSION, "crlf-1", "first line\r\nsecond line\r\n", now)
        .unwrap();
    let claim = mailbox.claim_next(SESSION, now).unwrap().unwrap();
    assert_eq!(claim.text, "first line\nsecond line\n");
    assert!(matches!(
        mailbox.deposit(SESSION, "bare-cr", "safe\roverwrite", now),
        Err(MailboxError::Invalid(_))
    ));
}

#[test]
fn peer_provenance_and_large_preview_pointers_round_trip_without_spoofing_human_approval() {
    let (_dir, mailbox) = mailbox();
    let now = SystemTime::now();
    let body = "αβγ ".repeat(1500);
    assert!(body.len() > INLINE_PREVIEW_BYTES);
    let (preview, truncated) = preview_for_mailbox(&body, "msg-99");
    assert!(truncated);
    assert!(preview.len() <= INLINE_PREVIEW_BYTES);
    assert!(preview.contains("msg_id=msg-99"));

    let prov = EnvelopeProvenance::PeerAgent {
        msg_id: "msg-99".into(),
        from_peer: "peer-agy-1".into(),
        provider: "antigravity".into(),
        branch: "feat/a2a".into(),
        thread_key: "review-1".into(),
        hop: 2,
        max_hops: DEFAULT_MAX_HOPS,
        truncated_in_bus: truncated,
        repo_root: "/tmp/repo".into(),
    };
    mailbox
        .deposit_with_provenance(SESSION, "msg-99", &preview, prov.clone(), now)
        .unwrap();
    assert!(mailbox.has_pending(SESSION, now).unwrap());
    // Retrying same request_id with HumanConfirmed must be rejected!
    assert!(matches!(
        mailbox.deposit(SESSION, "msg-99", &preview, now),
        Err(MailboxError::Invalid(_))
    ));
    let batch = mailbox.try_claim_batch(SESSION, 8, now).unwrap();
    assert_eq!(batch.len(), 1);
    assert_eq!(batch[0].claim.request_id, "msg-99");
    assert_eq!(batch[0].provenance, prov);
    assert!(!mailbox.has_pending(SESSION, now).unwrap());
}

#[test]
fn external_waiters_are_capped_at_sixteen_and_never_starve_hosted_sessions() {
    let (_dir, mailbox) = mailbox();
    let mut external_held = Vec::new();
    let ext_sessions = MAX_WAITERS_EXTERNAL / MAX_WAITERS_PER_SESSION;
    for n in 0..ext_sessions {
        let session = session_n(500 + n);
        let dir = mailbox.ensure_session(&session, SystemTime::now()).unwrap();
        for _ in 0..MAX_WAITERS_PER_SESSION {
            external_held.push(
                Waiter::register_in_pool(&mailbox, &dir, &session, WaitPool::External).unwrap(),
            );
        }
    }
    let extra_ext = session_n(599);
    let extra_ext_dir = mailbox
        .ensure_session(&extra_ext, SystemTime::now())
        .unwrap();
    assert!(matches!(
        Waiter::register_in_pool(&mailbox, &extra_ext_dir, &extra_ext, WaitPool::External),
        Err(MailboxError::Full(_))
    ));

    // Hosted sessions still get all 48 guaranteed slots even with external saturated!
    let mut hosted_held = Vec::new();
    let hosted_sessions = MAX_WAITERS_HOSTED / MAX_WAITERS_PER_SESSION;
    for n in 0..hosted_sessions {
        let session = session_n(700 + n);
        let dir = mailbox.ensure_session(&session, SystemTime::now()).unwrap();
        for _ in 0..MAX_WAITERS_PER_SESSION {
            hosted_held.push(
                Waiter::register_in_pool(&mailbox, &dir, &session, WaitPool::Hosted).unwrap(),
            );
        }
    }
    assert_eq!(hosted_held.len(), MAX_WAITERS_HOSTED);
    let extra_hosted = session_n(799);
    let extra_hosted_dir = mailbox
        .ensure_session(&extra_hosted, SystemTime::now())
        .unwrap();
    assert!(matches!(
        Waiter::register_in_pool(&mailbox, &extra_hosted_dir, &extra_hosted, WaitPool::Hosted),
        Err(MailboxError::Full(_))
    ));
    drop(external_held);
    drop(hosted_held);
}

#[test]
fn bumping_turn_epoch_displaces_prior_stop_waiters_and_resets_continuations() {
    let (_dir, mailbox) = mailbox();
    assert_eq!(mailbox.bump_continuation(SESSION).unwrap(), 1);
    assert_eq!(mailbox.bump_continuation(SESSION).unwrap(), 2);
    assert_eq!(mailbox.continuation_count(SESSION).unwrap(), 2);

    let outcome = std::thread::scope(|scope| {
        let waiter = scope.spawn(|| {
            mailbox
                .wait_in_pool(SESSION, Duration::from_secs(15), WaitPool::Hosted)
                .unwrap()
        });
        assert!(
            crate::test_support::env::wait_until(Duration::from_secs(5), || {
                mailbox.listening(SESSION, SystemTime::now())
            }),
            "the waiter never registered"
        );
        assert_eq!(mailbox.bump_turn_epoch(SESSION).unwrap(), 1);
        waiter.join().unwrap()
    });
    assert!(matches!(outcome, EnvelopeWait::Declined(_)), "{outcome:?}");
    assert_eq!(mailbox.continuation_count(SESSION).unwrap(), 0);
}

#[test]
fn mark_claimed_by_id_and_hop_filter_preserve_exact_delivery_invariants() {
    let (_dir, mailbox) = mailbox();
    let now = SystemTime::now();
    mailbox
        .deposit_with_provenance(
            SESSION,
            "msg-hop-max",
            "at hop limit",
            EnvelopeProvenance::PeerAgent {
                msg_id: "msg-hop-max".into(),
                from_peer: "peer-1".into(),
                provider: "claude".into(),
                branch: "main".into(),
                thread_key: "t".into(),
                hop: 6,
                max_hops: 6,
                truncated_in_bus: false,
                repo_root: String::new(),
            },
            now,
        )
        .unwrap();

    let wait_res = mailbox
        .wait_in_pool_filtered(SESSION, Duration::from_secs(5), WaitPool::Hosted, |prov| {
            !matches!(prov, EnvelopeProvenance::PeerAgent { hop, max_hops, .. } if *hop >= *max_hops)
        })
        .unwrap();
    assert!(
        matches!(wait_res, EnvelopeWait::Declined(_)),
        "{wait_res:?}"
    );
    // Still pending for manual / boundary inspection!
    assert_eq!(mailbox.status(SESSION, "msg-hop-max"), Delivery::Pending);
    assert!(mailbox
        .mark_claimed_by_id(SESSION, "msg-hop-max", now)
        .unwrap());
    assert!(!mailbox
        .mark_claimed_by_id(SESSION, "msg-hop-max", now)
        .unwrap());
    assert_eq!(mailbox.status(SESSION, "msg-hop-max"), Delivery::Claimed);
}

#[test]
fn a_waiter_claims_through_the_filter_it_peeked_with_so_a_hop_limited_message_stays_pending() {
    let (_dir, mailbox) = mailbox();
    let now = SystemTime::now();
    let peer = |id: &str, hop: u32| EnvelopeProvenance::PeerAgent {
        msg_id: id.into(),
        from_peer: "claude:coder".into(),
        provider: "claude".into(),
        branch: String::new(),
        thread_key: String::new(),
        hop,
        max_hops: 6,
        truncated_in_bus: false,
        repo_root: "/tmp/repo".into(),
    };
    // The oldest envelope has lost its sidecar, so the peek lets it through and the claim
    // drops it; the next one is a peer message at its hop limit.
    mailbox
        .deposit_with_provenance(SESSION, "lost-2", "no provenance", peer("lost-2", 1), now)
        .unwrap();
    mailbox
        .deposit_with_provenance(
            SESSION,
            "at-limit",
            "peer at its hop limit",
            peer("at-limit", 6),
            now + Duration::from_secs(1),
        )
        .unwrap();
    let key = session_key(SESSION).unwrap();
    std::fs::remove_file(mailbox.meta_path(&key, "lost-2")).unwrap();

    let outcome = mailbox
        .wait_in_pool_filtered(SESSION, Duration::from_secs(2), WaitPool::Hosted, |prov| {
            !matches!(prov, EnvelopeProvenance::PeerAgent { hop, max_hops, .. } if *hop >= *max_hops)
        })
        .unwrap();
    assert!(
        matches!(outcome, EnvelopeWait::Declined(_)),
        "a hop-limited peer message must not be handed to the waiter: {outcome:?}"
    );
    assert_eq!(mailbox.status(SESSION, "at-limit"), Delivery::Pending);
}

#[test]
fn the_app_ui_deposit_still_arrives_as_human_confirmed_with_a_sidecar() {
    let (_dir, mailbox) = mailbox();
    let now = SystemTime::now();
    mailbox
        .deposit(SESSION, "ui-1", "the person approved this", now)
        .unwrap();
    let key = session_key(SESSION).unwrap();
    assert_eq!(
        mailbox.read_provenance(&key, "ui-1").unwrap(),
        EnvelopeProvenance::HumanConfirmed
    );
    let claimed = mailbox.try_claim_batch(SESSION, 8, now).unwrap();
    assert_eq!(claimed.len(), 1);
    assert_eq!(claimed[0].provenance, EnvelopeProvenance::HumanConfirmed);
}

#[test]
fn a_lost_provenance_sidecar_never_reads_as_human_confirmed() {
    let (_dir, mailbox) = mailbox();
    let now = SystemTime::now();
    let provenance = EnvelopeProvenance::PeerAgent {
        msg_id: "lost-1".into(),
        from_peer: "claude:coder".into(),
        provider: "claude".into(),
        branch: String::new(),
        thread_key: String::new(),
        hop: 1,
        max_hops: DEFAULT_MAX_HOPS,
        truncated_in_bus: false,
        repo_root: "/tmp/repo".into(),
    };
    mailbox
        .deposit_with_provenance(SESSION, "lost-1", "an agent message", provenance, now)
        .unwrap();
    let key = session_key(SESSION).unwrap();
    std::fs::remove_file(mailbox.meta_path(&key, "lost-1")).unwrap();

    assert!(
        mailbox.read_provenance(&key, "lost-1").is_err(),
        "a missing sidecar is an error, not a human"
    );
    let claimed = mailbox.try_claim_batch(SESSION, 8, now).unwrap();
    assert!(
        claimed
            .iter()
            .all(|env| env.provenance != EnvelopeProvenance::HumanConfirmed),
        "a lost sidecar must not upgrade an agent message to human-confirmed: {claimed:?}"
    );
}

fn peer_provenance(id: &str, hop: u32, max_hops: u32) -> EnvelopeProvenance {
    EnvelopeProvenance::PeerAgent {
        msg_id: id.into(),
        from_peer: "claude:coder".into(),
        provider: "claude".into(),
        branch: String::new(),
        thread_key: String::new(),
        hop,
        max_hops,
        truncated_in_bus: false,
        repo_root: "/tmp/repo".into(),
    }
}

#[test]
fn a_prepared_delivery_consumes_nothing_until_it_is_committed() {
    let (_dir, mailbox) = mailbox();
    let now = SystemTime::now();
    mailbox
        .deposit(SESSION, "ui-1", "the person approved this", now)
        .unwrap();

    let prepared = mailbox
        .prepare_delivery(
            SESSION,
            8,
            now,
            |_| true,
            |env| env.claim.text.to_uppercase(),
        )
        .unwrap();
    assert!(!prepared.is_empty());
    assert_eq!(mailbox.pending(SESSION), 1, "preparing is a peek");

    // A caller cut off while it built its text just lets go of the delivery.
    drop(prepared);
    assert_eq!(mailbox.pending(SESSION), 1, "dropping consumes nothing");
    assert_eq!(mailbox.status(SESSION, "ui-1"), Delivery::Pending);

    let prepared = mailbox
        .prepare_delivery(
            SESSION,
            8,
            now,
            |_| true,
            |env| env.claim.text.to_uppercase(),
        )
        .unwrap();
    let won = prepared.commit().unwrap();
    assert_eq!(won.len(), 1);
    assert_eq!(won[0].0.claim.request_id, "ui-1");
    assert_eq!(won[0].1, "THE PERSON APPROVED THIS");
    assert_eq!(mailbox.pending(SESSION), 0);
    assert_eq!(mailbox.status(SESSION, "ui-1"), Delivery::Claimed);
}

#[test]
fn two_consumers_prepared_from_one_message_deliver_it_once_between_them() {
    let (_dir, mailbox) = mailbox();
    let now = SystemTime::now();
    mailbox.deposit(SESSION, "ui-1", "only once", now).unwrap();
    let first = mailbox
        .prepare_delivery(SESSION, 8, now, |_| true, |env| env.claim.text.clone())
        .unwrap();
    let second = mailbox
        .prepare_delivery(SESSION, 8, now, |_| true, |env| env.claim.text.clone())
        .unwrap();
    assert!(!first.is_empty() && !second.is_empty());

    let from_first = first.commit().unwrap();
    let from_second = second.commit().unwrap();
    assert_eq!(
        from_first.len() + from_second.len(),
        1,
        "{from_first:?} {from_second:?}"
    );
}

#[test]
fn many_threads_committing_one_message_deliver_it_exactly_once() {
    let (_dir, mailbox) = mailbox();
    let now = SystemTime::now();
    mailbox.deposit(SESSION, "ui-1", "only once", now).unwrap();
    let deliveries: Vec<_> = (0..8)
        .map(|_| {
            mailbox
                .prepare_delivery(SESSION, 8, now, |_| true, |env| env.claim.text.clone())
                .unwrap()
        })
        .collect();
    let won = AtomicUsize::new(0);
    std::thread::scope(|scope| {
        for delivery in deliveries {
            let won = &won;
            scope.spawn(move || {
                won.fetch_add(delivery.commit().unwrap().len(), Ordering::SeqCst);
            });
        }
    });
    assert_eq!(won.load(Ordering::SeqCst), 1);
}

#[test]
fn the_filter_that_chose_what_to_prepare_also_gates_the_commit() {
    let (_dir, mailbox) = mailbox();
    let now = SystemTime::now();
    mailbox
        .deposit_with_provenance(
            SESSION,
            "peer-1",
            "a peer message",
            peer_provenance("peer-1", 1, DEFAULT_MAX_HOPS),
            now,
        )
        .unwrap();
    // The filter takes the message when the delivery is prepared and refuses it
    // by the time of the commit: nothing may be consumed.
    let open = std::sync::atomic::AtomicBool::new(true);
    let prepared = mailbox
        .prepare_delivery(
            SESSION,
            8,
            now,
            |_| open.load(Ordering::SeqCst),
            |env| env.claim.text.clone(),
        )
        .unwrap();
    assert!(!prepared.is_empty());
    open.store(false, Ordering::SeqCst);
    assert!(prepared.commit().unwrap().is_empty());
    assert_eq!(
        mailbox.pending(SESSION),
        1,
        "a message the filter refuses stays pending"
    );
}

#[test]
fn a_filter_that_refuses_a_message_at_prepare_time_leaves_it_out_and_pending() {
    let (_dir, mailbox) = mailbox();
    let now = SystemTime::now();
    mailbox
        .deposit_with_provenance(
            SESSION,
            "peer-1",
            "at the hop limit",
            peer_provenance("peer-1", DEFAULT_MAX_HOPS, DEFAULT_MAX_HOPS),
            now,
        )
        .unwrap();
    let prepared = mailbox
        .prepare_delivery(
            SESSION,
            8,
            now,
            |prov| {
                !matches!(
                    prov,
                    EnvelopeProvenance::PeerAgent { hop, max_hops, .. } if hop >= max_hops
                )
            },
            |env| env.claim.text.clone(),
        )
        .unwrap();
    assert!(prepared.is_empty());
    assert!(prepared.commit().unwrap().is_empty());
    assert_eq!(mailbox.pending(SESSION), 1);
}

#[test]
fn a_commit_takes_only_what_was_prepared() {
    let (_dir, mailbox) = mailbox();
    let now = SystemTime::now();
    for (n, id) in ["m-1", "m-2", "m-3"].into_iter().enumerate() {
        mailbox
            .deposit(
                SESSION,
                id,
                &format!("text {id}"),
                now + Duration::from_secs(n as u64),
            )
            .unwrap();
    }
    let prepared = mailbox
        .prepare_delivery(SESSION, 2, now, |_| true, |env| env.claim.text.clone())
        .unwrap();
    // A message deposited after the peek is not in the delivery either.
    mailbox
        .deposit(SESSION, "m-4", "late", now + Duration::from_secs(9))
        .unwrap();
    let won = prepared.commit().unwrap();
    let ids: Vec<_> = won
        .iter()
        .map(|(env, _)| env.claim.request_id.as_str())
        .collect();
    assert_eq!(ids, ["m-1", "m-2"]);
    assert_eq!(mailbox.pending(SESSION), 2);
}

#[test]
fn an_unreadable_envelope_is_skipped_without_hiding_the_readable_ones_behind_it() {
    let (_dir, mailbox) = mailbox();
    let now = SystemTime::now();
    for (n, id) in ["u-1", "u-2", "u-3"].into_iter().enumerate() {
        mailbox
            .deposit(
                SESSION,
                id,
                &format!("text {id}"),
                now + Duration::from_secs(n as u64),
            )
            .unwrap();
    }
    let key = session_key(SESSION).unwrap();
    std::fs::remove_file(mailbox.meta_path(&key, "u-1")).unwrap();

    let peeked = mailbox.peek_pending(SESSION, 2, now).unwrap();
    let ids: Vec<_> = peeked
        .iter()
        .map(|env| env.claim.request_id.as_str())
        .collect();
    assert_eq!(ids, ["u-2", "u-3"], "the unreadable one takes no slot");
    assert_eq!(
        mailbox.pending(SESSION),
        3,
        "an unreadable envelope is left in place, not deleted"
    );
}

/// The peer-inbox hook asks this on every tool call. An empty mailbox must be
/// answered without the session `flock` (no `.locks` entry appears), a pending
/// envelope must be seen, and a claimed one must not.
#[test]
fn has_unclaimed_takes_no_lock_and_sees_only_pending_envelopes() {
    let (dir, mailbox) = mailbox();
    assert!(!mailbox.has_unclaimed(SESSION).unwrap());
    assert!(
        !dir.path().join(".locks").exists(),
        "the empty check must not take the session lock"
    );

    mailbox
        .deposit(SESSION, "msg-fast-1", "ping", SystemTime::now())
        .unwrap();
    assert!(mailbox.has_unclaimed(SESSION).unwrap());

    let claimed = mailbox
        .try_claim_batch(SESSION, 4, SystemTime::now())
        .unwrap();
    assert_eq!(claimed.len(), 1);
    assert!(
        !mailbox.has_unclaimed(SESSION).unwrap(),
        "a claimed c-* envelope is not unclaimed"
    );
    assert!(
        mailbox.has_unclaimed("not a session").is_err(),
        "an invalid session key is an error, not an empty mailbox"
    );
}

#[test]
fn system_steward_deposits_supersede_older_unclaimed_directives_without_filling_mailbox() {
    let (_dir, mailbox) = mailbox();
    let now = SystemTime::now();
    for tick in 0..(MAX_PENDING + 4) {
        let req_id = format!("steward-tick-{tick}");
        mailbox
            .deposit_with_provenance(
                SESSION,
                &req_id,
                &format!("directive #{tick}"),
                EnvelopeProvenance::SystemSteward {
                    msg_id: req_id.clone(),
                    model: "claude-sonnet-4-6".into(),
                    mode: "cloud".into(),
                    tier: "severe".into(),
                    binding: "memory_pressure".into(),
                    repo_root: String::new(),
                    bus_seq: tick as u64 + 1,
                },
                now + Duration::from_secs(tick as u64),
            )
            .unwrap();
    }
    assert_eq!(
        mailbox.pending(SESSION),
        1,
        "only the latest unclaimed steward directive should remain pending"
    );

    // Human-confirmed and peer messages still fit cleanly alongside the steward slot.
    for n in 0..(MAX_PENDING - 1) {
        mailbox
            .deposit(
                SESSION,
                &format!("human-{n}"),
                &format!("user note {n}"),
                now + Duration::from_secs(100 + n as u64),
            )
            .unwrap();
    }
    assert_eq!(mailbox.pending(SESSION), MAX_PENDING);

    let claimed = mailbox
        .try_claim_batch(SESSION, MAX_PENDING, now + Duration::from_secs(200))
        .unwrap();
    assert_eq!(
        claimed[0].claim.text,
        format!("directive #{}", MAX_PENDING + 3)
    );
}

fn peer(msg_id: &str, repo: &str) -> EnvelopeProvenance {
    EnvelopeProvenance::PeerAgent {
        msg_id: msg_id.to_owned(),
        from_peer: "claude:coder".into(),
        provider: "claude".into(),
        branch: String::new(),
        thread_key: String::new(),
        hop: 1,
        max_hops: DEFAULT_MAX_HOPS,
        truncated_in_bus: false,
        repo_root: repo.to_owned(),
    }
}

#[test]
fn peer_envelopes_leave_the_last_slot_for_one_overflow_notice() {
    let (_dir, mailbox) = mailbox();
    let now = SystemTime::now();
    for i in 0..MAX_PEER_PENDING {
        let id = format!("m{i}");
        let kept = mailbox
            .deposit_peer_or_overflow(SESSION, &id, "peer text", peer(&id, "/repo"), now)
            .unwrap();
        assert!(matches!(kept, PeerDeposit::Deposited(_)), "{i}");
    }
    // Every further peer message overflows, and they all share the one notice.
    for i in 0..3 {
        let id = format!("extra{i}");
        match mailbox
            .deposit_peer_or_overflow(SESSION, &id, "peer text", peer(&id, "/repo"), now)
            .unwrap()
        {
            PeerDeposit::Overflowed { notice } => assert!(notice.is_ok(), "{notice:?}"),
            other => panic!("expected overflow: {other:?}"),
        }
    }
    assert_eq!(names(&mailbox, SESSION).len(), MAX_PENDING);
    // A human message still has the full cap, so it is refused only at MAX_PENDING.
    assert!(matches!(
        mailbox.deposit(SESSION, "human", "hello", now),
        Err(MailboxError::Full(_))
    ));
    let notice = mailbox.try_claim_batch(SESSION, MAX_PENDING, now).unwrap();
    let text = &notice
        .iter()
        .find(|c| c.claim.text.contains("gitpulse_agent_inbox"))
        .expect("the notice is pending")
        .claim
        .text;
    assert!(text.contains("repo_path=/repo"), "{text}");
    assert!(text.contains(&format!("for_peer={SESSION}")), "{text}");
    assert!(text.contains("ack=true"), "{text}");
    assert!(text.contains("since_seq"), "{text}");
}

#[test]
fn a_claimed_overflow_notice_is_reissued_by_the_next_overflow() {
    let (_dir, mailbox) = mailbox();
    let now = SystemTime::now();
    for i in 0..=MAX_PEER_PENDING {
        let id = format!("m{i}");
        mailbox
            .deposit_peer_or_overflow(SESSION, &id, "peer text", peer(&id, "/repo"), now)
            .unwrap();
    }
    let claimed = mailbox.try_claim_batch(SESSION, MAX_PENDING, now).unwrap();
    assert!(claimed
        .iter()
        .any(|c| c.claim.text.contains("gitpulse_agent_inbox")));
    for i in 0..=MAX_PEER_PENDING {
        let id = format!("n{i}");
        mailbox
            .deposit_peer_or_overflow(SESSION, &id, "peer text", peer(&id, "/repo"), now)
            .unwrap();
    }
    let again = mailbox.try_claim_batch(SESSION, MAX_PENDING, now).unwrap();
    assert!(
        again
            .iter()
            .any(|c| c.claim.text.contains("gitpulse_agent_inbox")),
        "the recipient was never told about the second overflow"
    );
}
