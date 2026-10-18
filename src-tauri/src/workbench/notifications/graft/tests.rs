use super::fake_server_tests::{FakeGraft, Reply};
use super::*;
use crate::test_support::coverage_relaxed;
use crate::test_support::env::{bind_env, env_serial};
use serde_json::Value;
use std::time::{Duration, Instant};

const ACTIVITY: &str = "gitpulse.0123456789abcdef0123456789abcdef.event-42";
const SESSION: &str = "gitpulse.session.term-1";

fn post_request(key: &str, title: &str, text: &str) -> Request {
    Request::Post {
        key: key.into(),
        title: title.into(),
        text: text.into(),
        silent: false,
    }
}

fn answer(text: &str) -> Reply {
    Reply::Line(text.into())
}

/// Sends `request` to a fake that replies `reply`, returning the outcome and
/// the one line the fake received (if any).
fn round_trip(request: &Request, reply: Reply) -> (Outcome, Option<Vec<u8>>) {
    let fake = FakeGraft::start(reply);
    let outcome = forward_one(Some(fake.path().to_path_buf()), request, BUDGET);
    (outcome, fake.next_request(Duration::from_millis(500)))
}

fn parsed(line: &[u8]) -> Value {
    assert_eq!(line.last(), Some(&b'\n'), "one request, LF terminated");
    assert_eq!(line.iter().filter(|&&b| b == b'\n').count(), 1);
    serde_json::from_slice(&line[..line.len() - 1]).expect("one JSON object")
}

#[test]
fn a_post_is_one_json_line_in_the_contracted_shape() {
    let request = Request::Post {
        key: ACTIVITY.into(),
        title: "GitPulse activity".into(),
        text: "Task enhancement ready to review".into(),
        silent: true,
    };
    let (outcome, line) = round_trip(&request, answer(r#"{"ok":true,"sent":true}"#));
    assert_eq!(outcome, Outcome::Accepted { sent: true });
    let sent = parsed(&line.expect("the fake received a request"));
    assert_eq!(
        sent,
        serde_json::json!({
            "op":"post","src":"gitpulse","key":ACTIVITY,"app":"GitPulse",
            "title":"GitPulse activity","text":"Task enhancement ready to review",
            "silent":true,
        })
    );
}

#[test]
fn a_remove_carries_only_the_op_the_source_and_the_key() {
    let (outcome, line) = round_trip(
        &Request::Remove {
            key: SESSION.into(),
        },
        answer(r#"{"ok":true,"sent":false}"#),
    );
    assert_eq!(outcome, Outcome::Accepted { sent: false });
    assert_eq!(
        parsed(&line.unwrap()),
        serde_json::json!({"op":"remove","src":"gitpulse","key":SESSION})
    );
}

#[test]
fn display_fields_are_cut_on_a_code_point_boundary_at_graft_caps() {
    // "é" is 2 bytes and "日" is 3, so neither cap falls on a boundary: 256 is
    // even but 2048 is not a multiple of 3.
    let title = "é".repeat(300);
    let text = "日".repeat(1000);
    let line = encode(&post_request(ACTIVITY, &title, &text)).unwrap();
    let sent = parsed(&line);
    let (cut_title, cut_text) = (
        sent["title"].as_str().unwrap(),
        sent["text"].as_str().unwrap(),
    );
    assert_eq!(cut_title.len(), 256, "128 two-byte characters");
    assert_eq!(cut_text.len(), 2046, "682 three-byte characters, not 2048");
    assert!(title.starts_with(cut_title) && text.starts_with(cut_text));
    assert!(sent["app"].as_str().unwrap().len() <= MAX_APP);

    // Short fields are left alone, and an empty body is a body.
    let short = parsed(&encode(&post_request(ACTIVITY, "t", "")).unwrap());
    assert_eq!(
        (short["title"].as_str(), short["text"].as_str()),
        (Some("t"), Some(""))
    );
}

#[test]
fn a_field_that_json_escaping_inflates_is_shortened_not_dropped() {
    // 2048 control characters are within the text cap and escape to 6 bytes
    // each: 12 KiB on the wire against an 8 KiB request limit.
    let text = "\u{1}".repeat(MAX_TEXT);
    let line = encode(&post_request(ACTIVITY, "title", &text)).expect("still sendable");
    assert!(line.len() <= MAX_REQUEST, "{} bytes", line.len());
    let sent = parsed(&line);
    let kept = sent["text"].as_str().unwrap();
    assert!(!kept.is_empty() && kept.len() < MAX_TEXT && text.starts_with(kept));
    assert_eq!(sent["title"], "title");
}

#[test]
fn the_key_is_the_banner_identity_and_stays_inside_the_key_grammar() {
    for native in [ACTIVITY, SESSION, "gitpulse.session.a_b-c"] {
        let key = key_for(native).expect(native);
        assert_eq!(
            Some(key.clone()),
            key_for(native),
            "the same banner, the same key"
        );
        assert!((1..=128).contains(&key.len()));
        assert!(key
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"._:-".contains(&b)));
    }
    assert_ne!(key_for(ACTIVITY), key_for(SESSION));
    // The longest session identity the notifier can produce fits.
    let longest = format!("{}{}", crate::alerts::NATIVE_PREFIX, "k".repeat(96));
    assert!(key_for(&longest).is_some());
    // Outside the grammar: rejected, never truncated or rewritten.
    for bad in [
        "",
        "has space",
        "slash/inside",
        "new\nline",
        "é",
        &"k".repeat(129),
    ] {
        assert_eq!(key_for(bad), None, "{bad:?}");
    }
}

#[test]
fn an_invalid_key_never_reaches_the_socket() {
    let fake = FakeGraft::start(answer(r#"{"ok":true,"sent":true}"#));
    for request in [
        post_request("bad key", "t", "x"),
        Request::Remove { key: String::new() },
    ] {
        let outcome = forward_one(Some(fake.path().to_path_buf()), &request, BUDGET);
        assert_eq!(outcome, Outcome::Unsendable);
    }
    assert_eq!(fake.connections(), 0);
}

#[test]
fn every_refusal_code_is_named_and_none_is_mistaken_for_success() {
    let cases = [
        ("disabled", Refusal::Disabled),
        ("source-not-allowed", Refusal::SourceNotAllowed),
        ("rate-limited", Refusal::RateLimited),
        ("bad-request", Refusal::BadRequest),
        ("too-large", Refusal::TooLarge),
        ("a-code-from-a-newer-graft", Refusal::Unrecognised),
    ];
    for (code, refusal) in cases {
        let (outcome, _) = round_trip(
            &post_request(ACTIVITY, "t", "x"),
            answer(&format!(r#"{{"ok":false,"err":"{code}"}}"#)),
        );
        assert_eq!(outcome, Outcome::Refused(refusal), "{code}");
        assert!(outcome.word().is_some(), "{code} is logged");
    }
    // `ok:false` with no code is still a refusal, not a success.
    let (outcome, _) = round_trip(&post_request(ACTIVITY, "t", "x"), answer(r#"{"ok":false}"#));
    assert_eq!(outcome, Outcome::Refused(Refusal::Unrecognised));
}

#[test]
fn an_answer_that_is_not_an_answer_is_garbled() {
    for text in ["not json", r#"{"ok":"yes"}"#, r#"{"sent":true}"#, "[]", ""] {
        let (outcome, _) = round_trip(&post_request(ACTIVITY, "t", "x"), answer(text));
        assert_eq!(outcome, Outcome::Garbled, "{text:?}");
    }
    // Hanging up without answering is the same, not a success. This is also
    // the regression for macOS refusing `setsockopt` (EINVAL) on a Unix socket
    // whose peer has closed: with strict timeouts it reads `Io` here, and a
    // Graft that answers promptly and hangs up is the same situation. Checked
    // by mutation (strict timeouts make this assertion fail); it is a race, so
    // a green run on a fast machine proves less than that failure does.
    let (outcome, line) = round_trip(&post_request(ACTIVITY, "t", "x"), Reply::Hangup);
    assert_eq!(outcome, Outcome::Garbled);
    assert!(line.is_some(), "the request had been delivered");
    // An answer with no line end that keeps growing is cut off, not buffered.
    let (outcome, _) = round_trip(
        &post_request(ACTIVITY, "t", "x"),
        answer(&"x".repeat(MAX_ANSWER + 200)),
    );
    assert_eq!(outcome, Outcome::Garbled);
}

#[test]
fn a_missing_or_dead_socket_is_a_quiet_no_op() {
    let dir = tempfile::tempdir().unwrap();
    let request = post_request(ACTIVITY, "t", "x");

    // Graft never started: no socket file at all.
    let missing = dir.path().join("notify.sock");
    assert_eq!(
        forward_one(Some(missing.clone()), &request, BUDGET),
        Outcome::Absent
    );

    // Graft quit and left its socket file behind: nobody is listening.
    drop(std::os::unix::net::UnixListener::bind(&missing).unwrap());
    assert!(missing.exists());
    assert_eq!(
        forward_one(Some(missing), &request, BUDGET),
        Outcome::Absent
    );

    // No home directory to look in.
    assert_eq!(forward_one(None, &request, BUDGET), Outcome::Absent);
    assert_eq!(Outcome::Absent.word(), None, "and nothing is logged for it");
}

#[test]
fn a_server_that_never_answers_costs_one_budget_and_no_more() {
    let fake = FakeGraft::start(Reply::Silent);
    let started = Instant::now();
    let outcome = forward_one(
        Some(fake.path().to_path_buf()),
        &post_request(ACTIVITY, "t", "x"),
        BUDGET,
    );
    let elapsed = started.elapsed();
    assert_eq!(outcome, Outcome::TimedOut);
    assert!(elapsed >= BUDGET, "gave up early: {elapsed:?}");
    assert!(
        elapsed < coverage_relaxed(Duration::from_millis(1500)),
        "ran past the budget: {elapsed:?}"
    );
    assert_eq!(fake.connections(), 1, "and it was not retried");
}

#[test]
fn a_server_that_drips_bytes_cannot_stretch_the_budget() {
    let fake = FakeGraft::start(Reply::Drip);
    let started = Instant::now();
    let outcome = forward_one(
        Some(fake.path().to_path_buf()),
        &post_request(ACTIVITY, "t", "x"),
        Duration::from_millis(400),
    );
    let elapsed = started.elapsed();
    assert_eq!(outcome, Outcome::TimedOut);
    assert!(
        elapsed < coverage_relaxed(Duration::from_millis(1500)),
        "{elapsed:?}"
    );
}

/// Starts a forwarder on `socket` and returns it with the outcomes it reports.
fn forwarder_on(socket: Option<PathBuf>, capacity: usize) -> (Forwarder, mpsc::Receiver<Outcome>) {
    let (reported, outcomes) = mpsc::channel();
    let forwarder = Forwarder::start(
        move || socket.clone(),
        BUDGET,
        capacity,
        move |outcome| {
            let _ = reported.send(outcome);
        },
    )
    .expect("the worker thread starts");
    (forwarder, outcomes)
}

#[test]
fn a_hung_socket_never_blocks_the_caller_and_a_full_queue_drops() {
    let fake = FakeGraft::start(Reply::Silent);
    let (forwarder, outcomes) = forwarder_on(Some(fake.path().to_path_buf()), 4);

    let started = Instant::now();
    let queued = (0..30)
        .filter(|n| forwarder.offer(post_request(ACTIVITY, "t", &n.to_string())))
        .count();
    let elapsed = started.elapsed();
    // Thirty offers while the worker sits in its first one-second exchange.
    assert!(
        elapsed < coverage_relaxed(Duration::from_millis(250)),
        "offering waited on the socket: {elapsed:?}"
    );
    assert!(
        queued <= 5,
        "queue of 4 plus the one in flight, got {queued}"
    );
    assert_eq!(forwarder.dropped(), 30 - queued as u64);
    assert!(forwarder.dropped() >= 25);

    // The stalled exchange is bounded: the worker reports it timed out.
    let first = outcomes
        .recv_timeout(coverage_relaxed(Duration::from_secs(4)))
        .expect("the stalled exchange ended");
    assert_eq!(first, Outcome::TimedOut);
}

#[test]
fn a_missing_socket_through_the_worker_reports_absent_and_nothing_else() {
    let dir = tempfile::tempdir().unwrap();
    let (forwarder, outcomes) = forwarder_on(Some(dir.path().join("no-such.sock")), 4);
    assert!(forwarder.offer(post_request(ACTIVITY, "t", "x")));
    let outcome = outcomes
        .recv_timeout(coverage_relaxed(Duration::from_secs(3)))
        .unwrap();
    assert_eq!(outcome, Outcome::Absent);
    assert!(outcomes.try_recv().is_err());
}

#[test]
fn a_refusal_is_reported_once_and_never_retried() {
    let fake = FakeGraft::start(answer(r#"{"ok":false,"err":"rate-limited"}"#));
    let (forwarder, outcomes) = forwarder_on(Some(fake.path().to_path_buf()), 4);
    assert!(forwarder.offer(post_request(ACTIVITY, "t", "x")));
    let outcome = outcomes
        .recv_timeout(coverage_relaxed(Duration::from_secs(3)))
        .unwrap();
    assert_eq!(outcome, Outcome::Refused(Refusal::RateLimited));
    std::thread::sleep(Duration::from_millis(300));
    assert_eq!(fake.connections(), 1, "one request, one connection");
    assert!(outcomes.try_recv().is_err());
}

#[test]
fn a_stopped_worker_drops_instead_of_blocking() {
    // A worker that has stopped (a panic is recorded by the supervisor) leaves
    // a disconnected queue behind.
    let (sender, receiver) = mpsc::sync_channel::<Request>(1);
    drop(receiver);
    let dead = Forwarder {
        sender,
        dropped: AtomicU64::new(0),
    };
    assert!(!dead.offer(post_request(ACTIVITY, "t", "x")));
    assert_eq!(dead.dropped(), 1);
}

#[test]
fn each_refusal_code_is_logged_once_and_never_with_content() {
    let seen = LogOnce::default();
    for word in ["disabled", "source-not-allowed", "rate-limited", "timeout"] {
        assert!(seen.first(word), "first {word}");
        assert!(!seen.first(word), "second {word}");
    }
    assert!(seen.first("bad-request"), "an unlogged code is still new");
    // An outcome holds a code and nothing else, so what it can say is a fixed
    // word: no title, text or key is in reach of the log line.
    let words: Vec<_> = [
        Outcome::Refused(Refusal::Disabled),
        Outcome::Refused(Refusal::SourceNotAllowed),
        Outcome::Refused(Refusal::RateLimited),
        Outcome::Refused(Refusal::BadRequest),
        Outcome::Refused(Refusal::TooLarge),
        Outcome::Refused(Refusal::Unrecognised),
        Outcome::TimedOut,
        Outcome::Io,
        Outcome::Garbled,
        Outcome::Unsendable,
    ]
    .into_iter()
    .map(|o| o.word().unwrap())
    .collect();
    let mut unique = words.clone();
    unique.sort_unstable();
    unique.dedup();
    assert_eq!(unique.len(), words.len(), "{words:?}");
    assert!(words.iter().all(|w| !w.contains("gitpulse")));
    assert_eq!(Outcome::Accepted { sent: true }.word(), None);
}

#[test]
fn the_socket_path_is_the_override_else_the_default_under_home() {
    let serial = env_serial();
    let home = tempfile::tempdir().unwrap();
    let explicit = home.path().join("custom.sock");
    {
        let _env = bind_env(&serial)
            .set("HOME", home.path())
            .set(SOCKET_ENV, &explicit);
        assert_eq!(socket_path(), Some(explicit.clone()));
    }
    {
        let _env = bind_env(&serial)
            .set("HOME", home.path())
            .set(SOCKET_ENV, "");
        assert_eq!(
            socket_path(),
            Some(
                home.path()
                    .join("Library/Application Support/Graft/notify.sock")
            ),
            "an empty override is no override"
        );
    }
    {
        let _env = bind_env(&serial)
            .set("HOME", home.path())
            .remove(SOCKET_ENV);
        assert!(socket_path().unwrap().starts_with(home.path()));
    }
}

#[test]
fn the_process_wide_entry_points_reach_the_socket_named_by_the_environment() {
    let serial = env_serial();
    let fake = FakeGraft::start(answer(r#"{"ok":true,"sent":true}"#));
    let _env = bind_env(&serial).set(SOCKET_ENV, fake.path());

    // A banner the Mac showed with sound is audible on the phone; one it
    // showed silently is silent.
    post(ACTIVITY, "GitPulse activity", "Run finished", true);
    let line = fake
        .next_request(coverage_relaxed(Duration::from_secs(3)))
        .expect("the post arrived");
    let sent = parsed(&line);
    assert_eq!(sent["op"], "post");
    assert_eq!(sent["silent"], false);
    assert_eq!(sent["key"], ACTIVITY);
    assert_eq!(sent["text"], "Run finished");

    post(SESSION, "Agent", "Needs input", false);
    let sent = parsed(
        &fake
            .next_request(coverage_relaxed(Duration::from_secs(3)))
            .unwrap(),
    );
    assert_eq!(
        (sent["key"].as_str(), sent["silent"].as_bool()),
        (Some(SESSION), Some(true))
    );

    // Withdrawal sends one remove per banner, with the post's key; an
    // identity outside the grammar is skipped, not sent.
    remove(&[
        ACTIVITY.to_owned(),
        "not a key".to_owned(),
        SESSION.to_owned(),
    ]);
    for key in [ACTIVITY, SESSION] {
        let sent = parsed(
            &fake
                .next_request(coverage_relaxed(Duration::from_secs(3)))
                .unwrap(),
        );
        assert_eq!(
            (sent["op"].as_str(), sent["key"].as_str()),
            (Some("remove"), Some(key))
        );
    }
    assert!(fake.next_request(Duration::from_millis(200)).is_none());
}
