//! Empirical adversarial stress test suite for Milestone 1 (GitPulse Agent Inbox & Bus).
//! Authored by Challenger 1.

mod common;

use gitpulse_lib::engine::agent_bus::{
    answer_ask, list_inbox_threads, post_thread_message, read_messages, read_thread_messages,
    withdraw_ask, MAX_BUS_MESSAGES, MAX_MESSAGE_BODY_BYTES,
};
use tempfile::TempDir;

fn create_test_repo() -> TempDir {
    common::unmetered_spawns();
    let dir = tempfile::tempdir().expect("create temp dir for repo");
    let p = dir.path();
    common::run_git(p, &["init", "-q", "-b", "main"]);
    common::run_git(p, &["commit", "--allow-empty", "-q", "-m", "init"]);
    common::trust_repo(p);
    dir
}

#[test]
fn test_adversarial_oversized_message_bodies() {
    let repo = create_test_repo();
    let repo_path = repo.path().to_str().unwrap();

    // 1. Boundary: Exactly MAX_MESSAGE_BODY_BYTES (16 KiB = 16384 bytes)
    let exact_16k = "a".repeat(MAX_MESSAGE_BODY_BYTES);
    let msg_exact = post_thread_message(
        repo_path,
        "agent-tester",
        None,
        Some("thread-exact-16k"),
        "note",
        &exact_16k,
        None,
    );
    assert!(msg_exact.is_ok(), "16 KiB exact body must be accepted");
    let msg = msg_exact.unwrap();
    assert_eq!(msg.body.len(), MAX_MESSAGE_BODY_BYTES);

    // 2. Boundary + 1 byte: 16385 bytes must be rejected
    let over_16k = "a".repeat(MAX_MESSAGE_BODY_BYTES + 1);
    let msg_over = post_thread_message(
        repo_path,
        "agent-tester",
        None,
        Some("thread-over-16k"),
        "note",
        &over_16k,
        None,
    );
    assert!(msg_over.is_err(), "16 KiB + 1 byte body must be rejected");
    let err_str = msg_over.err().unwrap().message().to_string();
    assert!(
        err_str.contains("exceeds maximum of 16384 bytes"),
        "error message must describe body length limit, got: {err_str}"
    );

    // 3. Huge body (1 MiB) must be rejected
    let huge = "x".repeat(1024 * 1024);
    assert!(post_thread_message(
        repo_path,
        "agent-tester",
        None,
        Some("thread-huge"),
        "note",
        &huge,
        None
    )
    .is_err());

    // 4. Multibyte UTF-8 near the 16 KiB boundary
    // "🦀" is 4 bytes. 16380 bytes of 'a' + "🦀" = 16384 bytes (OK)
    let mut utf8_boundary_ok = "a".repeat(16380);
    utf8_boundary_ok.push('🦀');
    assert_eq!(utf8_boundary_ok.len(), 16384);
    assert!(post_thread_message(
        repo_path,
        "agent-tester",
        None,
        Some("thread-utf8-ok"),
        "note",
        &utf8_boundary_ok,
        None
    )
    .is_ok());

    // 16381 bytes of 'a' + "🦀" = 16385 bytes (FAIL)
    let mut utf8_boundary_err = "a".repeat(16381);
    utf8_boundary_err.push('🦀');
    assert_eq!(utf8_boundary_err.len(), 16385);
    assert!(post_thread_message(
        repo_path,
        "agent-tester",
        None,
        Some("thread-utf8-err"),
        "note",
        &utf8_boundary_err,
        None
    )
    .is_err());

    // 5. Empty or whitespace-only bodies
    assert!(post_thread_message(repo_path, "agent", None, None, "note", "", None).is_err());
    assert!(
        post_thread_message(repo_path, "agent", None, None, "note", "   \t\n  ", None).is_err()
    );

    // 6. Null byte in body
    assert!(
        post_thread_message(repo_path, "agent", None, None, "note", "hello\0world", None).is_err()
    );
}

#[test]
fn test_adversarial_ring_buffer_300_messages_wrapping_and_monotonicity() {
    let repo = create_test_repo();
    let repo_path = repo.path().to_str().unwrap();

    const TOTAL_MESSAGES: usize = 300;

    // Append 300 messages sequentially
    for i in 1..=TOTAL_MESSAGES {
        let msg = post_thread_message(
            repo_path,
            "stream-agent",
            Some("collector"),
            Some(&format!("thread-{}", i % 5)),
            "ping",
            &format!("{{\"index\":{i}}}"),
            Some("main"),
        )
        .expect("posting sequential message");

        assert_eq!(msg.seq, i as u64, "Sequence must be strictly sequential");
    }

    // Read full inbox
    let page = read_messages(repo_path, None, 0).expect("read full inbox page");

    // 1. Retention cap: Must be exactly MAX_BUS_MESSAGES (256)
    assert_eq!(
        page.messages.len(),
        MAX_BUS_MESSAGES,
        "Inbox page must contain exactly 256 messages"
    );

    // 2. Monotonic sequence & correct dropped bounds
    assert_eq!(page.latest_seq, 300, "latest_seq must reflect 300");
    // Dropped count: 300 - 256 = 44 dropped. Messages dropped are 1..=44.
    // dropped_before_seq should be 45.
    assert_eq!(page.dropped_before_seq, 45, "dropped_before_seq must be 45");

    assert_eq!(page.messages.first().unwrap().seq, 45);
    assert_eq!(page.messages.last().unwrap().seq, 300);

    // 3. Ensure all sequence numbers are strictly monotonic: seq[k] == 45 + k
    for (idx, m) in page.messages.iter().enumerate() {
        assert_eq!(
            m.seq,
            (45 + idx) as u64,
            "Strict monotonicity violated at index {idx}"
        );
    }

    // 4. Pagination checks
    // Read since seq 44 (which was dropped) -> gets all 256 messages
    let page_since_44 = read_messages(repo_path, None, 44).expect("read since 44");
    assert_eq!(page_since_44.messages.len(), 256);

    // Read since seq 100 -> gets messages with seq 101..=300 (total 200 messages)
    let page_since_100 = read_messages(repo_path, None, 100).expect("read since 100");
    assert_eq!(page_since_100.messages.len(), 200);
    assert_eq!(page_since_100.messages.first().unwrap().seq, 101);
    assert_eq!(page_since_100.messages.last().unwrap().seq, 300);

    // Read since seq 300 -> 0 messages
    let page_since_300 = read_messages(repo_path, None, 300).expect("read since 300");
    assert_eq!(page_since_300.messages.len(), 0);

    // 5. list_inbox_threads survives buffer wrapping cleanly
    let threads = list_inbox_threads(repo_path, None, None).expect("list threads after wrapping");
    // Since i % 5 was used, there are 5 active thread keys: thread-0, thread-1, thread-2, thread-3, thread-4
    assert_eq!(threads.len(), 5);
    for t in threads {
        assert!(!t.messages.is_empty());
        // Latest sequence must be between 296 and 300
        assert!(t.latest_msg.seq >= 296 && t.latest_msg.seq <= 300);
    }
}

#[test]
fn test_adversarial_thread_grouping_edge_cases() {
    let repo = create_test_repo();
    let repo_path = repo.path().to_str().unwrap();

    // 1. Missing thread_keys (None)
    let m1 = post_thread_message(
        repo_path,
        "agent-1",
        None,
        None,
        "note",
        "unthreaded 1",
        None,
    )
    .unwrap();
    let m2 = post_thread_message(
        repo_path,
        "agent-1",
        None,
        None,
        "note",
        "unthreaded 2",
        None,
    )
    .unwrap();

    // 2. Whitespace-only thread_key should normalize to None / standalone
    let m3 = post_thread_message(
        repo_path,
        "agent-1",
        None,
        Some("   "),
        "note",
        "whitespace key",
        None,
    )
    .unwrap();

    // Verify unthreaded messages are isolated into distinct thread keys msg-{seq}
    let threads_unthreaded = list_inbox_threads(repo_path, None, None).unwrap();
    assert_eq!(threads_unthreaded.len(), 3);
    let keys: Vec<String> = threads_unthreaded
        .iter()
        .map(|t| t.thread_key.clone())
        .collect();
    assert!(keys.contains(&format!("msg-{}", m1.seq)));
    assert!(keys.contains(&format!("msg-{}", m2.seq)));
    assert!(keys.contains(&format!("msg-{}", m3.seq)));

    // 3. Special characters in thread_key
    // Slashes, colons, hashes, at-sign, brackets, spaces
    let complex_key = "feat/ui#issue-102:branch/sub-task.v1@beta [urgent]";
    let m4 = post_thread_message(
        repo_path,
        "agent-1",
        Some("agent-2"),
        Some(complex_key),
        "ask",
        "{\"ask_type\":\"decide\",\"question\":\"Approve complex slug?\"}",
        None,
    )
    .expect("complex thread key must be accepted");
    assert_eq!(m4.thread_key.as_deref(), Some(complex_key));

    // 4. Unicode / emoji in thread_key
    let emoji_key = "🚀-launch-task-🔥";
    let m5 = post_thread_message(
        repo_path,
        "agent-1",
        None,
        Some(emoji_key),
        "note",
        "Launch checklist initiated",
        None,
    )
    .expect("emoji thread key must be accepted");
    assert_eq!(m5.thread_key.as_deref(), Some(emoji_key));

    // 5. Path traversal symbols in thread_key: must be treated as verbatim literal slug, not file path
    let traversal_key = "../../../etc/passwd";
    let m6 = post_thread_message(
        repo_path,
        "agent-1",
        None,
        Some(traversal_key),
        "note",
        "Traversal test",
        None,
    )
    .expect("traversal thread key should be stored as literal token without fs traversal");
    assert_eq!(m6.thread_key.as_deref(), Some(traversal_key));

    // 6. Max length boundary for thread_key: exactly 80 bytes
    let key_80 = "k".repeat(80);
    assert!(post_thread_message(
        repo_path,
        "agent-1",
        None,
        Some(&key_80),
        "note",
        "80 bytes",
        None
    )
    .is_ok());

    // Exceeds 80 bytes: 81 bytes must be rejected
    let key_81 = "k".repeat(81);
    let err_81 = post_thread_message(
        repo_path,
        "agent-1",
        None,
        Some(&key_81),
        "note",
        "81 bytes",
        None,
    );
    assert!(err_81.is_err());
    assert!(err_81
        .err()
        .unwrap()
        .message()
        .to_string()
        .contains("exceeds maximum length of 80 bytes"));

    // 7. Null byte in thread_key must be rejected
    assert!(post_thread_message(
        repo_path,
        "agent-1",
        None,
        Some("bad\0key"),
        "note",
        "bad",
        None
    )
    .is_err());

    // 8. Query by thread key and verify retrieval of complex & emoji threads
    let complex_page = read_thread_messages(repo_path, Some(complex_key), None, 0).unwrap();
    assert_eq!(complex_page.messages.len(), 1);
    assert_eq!(
        complex_page.messages[0].thread_key.as_deref(),
        Some(complex_key)
    );

    let emoji_page = read_thread_messages(repo_path, Some(emoji_key), None, 0).unwrap();
    assert_eq!(emoji_page.messages.len(), 1);
    assert_eq!(
        emoji_page.messages[0].thread_key.as_deref(),
        Some(emoji_key)
    );

    // 9. Thread list reflects all aggregated threads
    let all_threads = list_inbox_threads(repo_path, None, None).unwrap();
    let all_keys: Vec<String> = all_threads.iter().map(|t| t.thread_key.clone()).collect();
    assert!(all_keys.contains(&complex_key.to_string()));
    assert!(all_keys.contains(&emoji_key.to_string()));
    assert!(all_keys.contains(&traversal_key.to_string()));
    assert!(all_keys.contains(&key_80));
}

#[test]
fn test_adversarial_structured_ask_state_machine_transitions() {
    let repo = create_test_repo();
    let repo_path = repo.path().to_str().unwrap();

    // ==========================================
    // 1. DECIDE: decide -> answered
    // ==========================================
    let t_decide = "thread-ask-decide";
    post_thread_message(
        repo_path,
        "claude",
        Some("developer"),
        Some(t_decide),
        "ask",
        r#"{"ask_type":"decide","question":"Pick DB","options":["SQLite","Postgres"],"recommendation":"SQLite"}"#,
        None,
    ).unwrap();

    let th = list_inbox_threads(repo_path, None, Some("open")).unwrap();
    let d = th.iter().find(|t| t.thread_key == t_decide).unwrap();
    assert_eq!(d.status, "open");
    assert_eq!(d.ask_type.as_deref(), Some("decide"));
    assert_eq!(d.title, "Pick DB");

    answer_ask(repo_path, "developer", t_decide, "Selected SQLite").unwrap();
    let th_answered = list_inbox_threads(repo_path, None, Some("answered")).unwrap();
    assert!(th_answered.iter().any(|t| t.thread_key == t_decide));

    // ==========================================
    // 2. DO: do -> answered
    // ==========================================
    let t_do = "thread-ask-do";
    post_thread_message(
        repo_path,
        "claude",
        Some("developer"),
        Some(t_do),
        "ask",
        r#"{"ask_type":"do","question":"Run migrations","steps":["Step 1","Step 2"]}"#,
        None,
    )
    .unwrap();

    let th_do = list_inbox_threads(repo_path, None, Some("open")).unwrap();
    let d2 = th_do.iter().find(|t| t.thread_key == t_do).unwrap();
    assert_eq!(d2.status, "open");
    assert_eq!(d2.ask_type.as_deref(), Some("do"));
    assert_eq!(d2.title, "Run migrations");

    answer_ask(repo_path, "developer", t_do, "Marked steps done").unwrap();
    let th_do_ans = list_inbox_threads(repo_path, None, Some("answered")).unwrap();
    assert!(th_do_ans.iter().any(|t| t.thread_key == t_do));

    // ==========================================
    // 3. ANSWER: answer -> answered
    // ==========================================
    let t_answer = "thread-ask-answer";
    post_thread_message(
        repo_path,
        "claude",
        Some("developer"),
        Some(t_answer),
        "ask",
        r#"{"ask_type":"answer","question":"What is the API key format?","prompt":"Provide format"}"#,
        None,
    ).unwrap();

    let th_ans = list_inbox_threads(repo_path, None, Some("open")).unwrap();
    let d3 = th_ans.iter().find(|t| t.thread_key == t_answer).unwrap();
    assert_eq!(d3.status, "open");
    assert_eq!(d3.ask_type.as_deref(), Some("answer"));

    answer_ask(repo_path, "developer", t_answer, "Format is sk-live-...").unwrap();
    let th_ans_done = list_inbox_threads(repo_path, None, Some("answered")).unwrap();
    assert!(th_ans_done.iter().any(|t| t.thread_key == t_answer));

    // ==========================================
    // 4. REVIEW: review -> answered (approved or changes requested)
    // ==========================================
    let t_review = "thread-ask-review";
    post_thread_message(
        repo_path,
        "claude",
        Some("developer"),
        Some(t_review),
        "ask",
        r#"{"ask_type":"review","question":"Review branch diff","branch":"feat/inbox","summary":"Added inbox"}"#,
        None,
    ).unwrap();

    let th_rev = list_inbox_threads(repo_path, None, Some("open")).unwrap();
    let d4 = th_rev.iter().find(|t| t.thread_key == t_review).unwrap();
    assert_eq!(d4.status, "open");
    assert_eq!(d4.ask_type.as_deref(), Some("review"));

    answer_ask(repo_path, "developer", t_review, "Approved: LGTM!").unwrap();
    let th_rev_done = list_inbox_threads(repo_path, None, Some("answered")).unwrap();
    assert!(th_rev_done.iter().any(|t| t.thread_key == t_review));

    // ==========================================
    // 5. WITHDRAW: open -> withdrawn
    // ==========================================
    let t_withdraw = "thread-ask-withdraw";
    post_thread_message(
        repo_path,
        "claude",
        Some("developer"),
        Some(t_withdraw),
        "ask",
        r#"{"ask_type":"decide","question":"To be cancelled"}"#,
        None,
    )
    .unwrap();

    withdraw_ask(repo_path, "claude", t_withdraw, "Cancelled by user").unwrap();
    let th_withdrawn = list_inbox_threads(repo_path, None, Some("withdrawn")).unwrap();
    assert!(th_withdrawn.iter().any(|t| t.thread_key == t_withdraw));

    // Open filter should no longer list it
    let th_open_after = list_inbox_threads(repo_path, None, Some("open")).unwrap();
    assert!(!th_open_after.iter().any(|t| t.thread_key == t_withdraw));

    // ==========================================
    // 6. Cyclic transition: Withdrawn ask re-opened by new ask
    // ==========================================
    post_thread_message(
        repo_path,
        "claude",
        Some("developer"),
        Some(t_withdraw),
        "ask",
        r#"{"ask_type":"decide","question":"Re-opened decision"}"#,
        None,
    )
    .unwrap();

    let th_reopened = list_inbox_threads(repo_path, None, Some("open")).unwrap();
    let re = th_reopened
        .iter()
        .find(|t| t.thread_key == t_withdraw)
        .unwrap();
    assert_eq!(re.status, "open");
    assert_eq!(re.title, "Re-opened decision");

    // ==========================================
    // 7. Malformed JSON body in ask: Graceful handling without panics
    // ==========================================
    let t_raw = "thread-ask-malformed-json";
    post_thread_message(
        repo_path,
        "claude",
        None,
        Some(t_raw),
        "ask",
        "This is not JSON at all, just plain text request",
        None,
    )
    .unwrap();

    let th_raw = list_inbox_threads(repo_path, None, Some("open")).unwrap();
    let raw_entry = th_raw.iter().find(|t| t.thread_key == t_raw).unwrap();
    assert_eq!(raw_entry.status, "open");
    assert_eq!(raw_entry.ask_type, None); // Cannot infer ask_type, remains None safely
    assert_eq!(
        raw_entry.title,
        "This is not JSON at all, just plain text request"
    );
}

#[test]
fn test_open_asks_survive_ring_buffer_note_flood_until_resolved() {
    let repo = create_test_repo();
    let repo_path = repo.path().to_str().unwrap();

    let ask_msg = post_thread_message(
        repo_path,
        "agy-worker",
        Some("developer"),
        Some("critical-open-ask"),
        "ask",
        r#"{"ask_type":"decide","question":"Keep migration reversible?","options":["Yes","No"]}"#,
        Some("feat/db"),
    )
    .expect("post critical open ask");
    assert_eq!(ask_msg.seq, 1);

    // Flood the 256-message ring buffer with 300 chatter notes on unrelated threads.
    for i in 1..=300 {
        post_thread_message(
            repo_path,
            "telemetry-bot",
            None,
            Some(&format!("chatter-{}", i % 8)),
            "note",
            &format!("status heartbeat {i}"),
            None,
        )
        .expect("post chatter note");
    }

    let page = read_messages(repo_path, None, 0).expect("read compacted bus");
    assert_eq!(page.messages.len(), MAX_BUS_MESSAGES);

    let open_threads = list_inbox_threads(repo_path, None, Some("open")).expect("list open asks");
    let survived = open_threads
        .iter()
        .find(|t| t.thread_key == "critical-open-ask")
        .expect("unresolved open ask must survive note flood compaction");
    assert_eq!(survived.status, "open");
    assert_eq!(survived.ask_type.as_deref(), Some("decide"));
    assert_eq!(survived.title, "Keep migration reversible?");

    // Answering the open ask unpins seq 1 so subsequent compaction evicts it cleanly.
    answer_ask(
        repo_path,
        "developer",
        "critical-open-ask",
        "Decision selected: Yes",
    )
    .expect("answer open ask");
    let open_after = list_inbox_threads(repo_path, None, Some("open")).expect("list open after");
    assert!(
        !open_after
            .iter()
            .any(|t| t.thread_key == "critical-open-ask"),
        "answered ask must no longer appear in open filter"
    );
}
