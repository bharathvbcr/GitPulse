use super::*;
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

#[test]
fn a_waiter_beyond_the_per_session_cap_is_told_to_leave() {
    let (_dir, mailbox) = mailbox();
    let dir = mailbox.ensure_session(SESSION, SystemTime::now()).unwrap();
    let mut held = Vec::new();
    for _ in 0..MAX_WAITERS_PER_SESSION {
        held.push(Waiter::register(&mailbox, &dir, SESSION).unwrap());
        std::thread::sleep(Duration::from_millis(2));
    }
    assert!(matches!(
        Waiter::register(&mailbox, &dir, SESSION),
        Err(MailboxError::Full(_))
    ));
    // The refused one removed its own file, so it holds no place.
    assert_eq!(
        mailbox.live_waiters(SystemTime::now()).len(),
        MAX_WAITERS_PER_SESSION
    );
    drop(held.remove(0));
    assert!(Waiter::register(&mailbox, &dir, SESSION).is_ok());
}

#[test]
fn a_waiter_beyond_the_global_cap_is_told_to_leave() {
    let (_dir, mailbox) = mailbox();
    let mut held = Vec::new();
    for n in 0..MAX_WAITERS_TOTAL / MAX_WAITERS_PER_SESSION {
        let session = session_n(n);
        let dir = mailbox.ensure_session(&session, SystemTime::now()).unwrap();
        for _ in 0..MAX_WAITERS_PER_SESSION {
            held.push(Waiter::register(&mailbox, &dir, &session).unwrap());
            std::thread::sleep(Duration::from_millis(2));
        }
    }
    let extra = session_n(99);
    let dir = mailbox.ensure_session(&extra, SystemTime::now()).unwrap();
    assert!(matches!(
        Waiter::register(&mailbox, &dir, &extra),
        Err(MailboxError::Full(_))
    ));
}

#[test]
fn a_waiter_that_stops_beating_stops_counting() {
    let (_dir, mailbox) = mailbox();
    let dir = mailbox.ensure_session(SESSION, SystemTime::now()).unwrap();
    let _waiter = Waiter::register(&mailbox, &dir, SESSION).unwrap();
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
    assert!(began.elapsed() < Duration::from_secs(5));
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
                seen.elapsed() < Duration::from_secs(10),
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
fn a_waiter_over_the_cap_declines_instead_of_waiting() {
    let (_dir, mailbox) = mailbox();
    let dir = mailbox.ensure_session(SESSION, SystemTime::now()).unwrap();
    let held: Vec<Waiter> = (0..MAX_WAITERS_PER_SESSION)
        .map(|_| {
            let w = Waiter::register(&mailbox, &dir, SESSION).unwrap();
            std::thread::sleep(Duration::from_millis(2));
            w
        })
        .collect();
    let began = Instant::now();
    let outcome = mailbox.wait(SESSION, Duration::from_secs(30)).unwrap();
    assert!(matches!(outcome, Wait::Declined(_)), "{outcome:?}");
    assert!(began.elapsed() < Duration::from_secs(5));
    drop(held);
}
