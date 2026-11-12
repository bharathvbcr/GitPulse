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
