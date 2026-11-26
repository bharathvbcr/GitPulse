//! Adversarial attacks on the `msg_id` retry rule of `post_peer_message`: a reused id is the same
//! message only when sender, recipient, kind, thread and body all match under the bus's own
//! identity rules (trim only; no case folding, no Unicode normalisation).

mod common;

use std::sync::Barrier;
use tempfile::TempDir;
use waylode_lib::engine::agent_bus::{
    ack_messages, post_peer_message, read_messages, read_messages_for_peer, AgentBusMessage,
    MessageOrigin, MAX_BUS_MESSAGES,
};
use waylode_lib::ipc_error::CommandError;

fn repo() -> TempDir {
    common::unmetered_spawns();
    let dir = tempfile::tempdir().expect("temp repo");
    common::run_git(dir.path(), &["init", "-q", "-b", "main"]);
    common::run_git(dir.path(), &["commit", "--allow-empty", "-q", "-m", "init"]);
    common::trust_repo(dir.path());
    dir
}

fn send(
    repo: &str,
    from: &str,
    to: Option<&str>,
    thread: Option<&str>,
    kind: &str,
    body: &str,
    id: &str,
) -> Result<AgentBusMessage, CommandError> {
    post_peer_message(
        repo,
        MessageOrigin::Agent,
        from,
        to,
        thread,
        kind,
        body,
        None,
        Some(id),
        1,
        6,
    )
}

fn bus_len(repo: &str) -> usize {
    read_messages(repo, None, 0).expect("bus").messages.len()
}

#[test]
fn recipient_identity_is_trim_only_so_case_and_normalisation_variants_are_refused() {
    let dir = repo();
    let r = dir.path().to_str().unwrap();
    let first = send(
        r,
        "claude:a",
        Some("claude:B"),
        None,
        "note",
        "hi",
        "id_rcpt",
    )
    .unwrap();

    // Whitespace around the recipient is the same recipient: a genuine retry.
    let padded = send(
        r,
        "claude:a",
        Some("  claude:B \t"),
        None,
        "note",
        "hi",
        "id_rcpt",
    )
    .expect("padded recipient is the same recipient");
    assert_eq!(padded.seq, first.seq);

    // The bus routes by exact string, so a case variant is a different inbox: prove it, then
    // prove the retry rule agrees with it.
    let lower_inbox =
        read_messages_for_peer(r, &["claude:b".to_string()], 0, None, false, true).unwrap();
    assert!(
        lower_inbox.messages.is_empty(),
        "'claude:b' is not B's inbox"
    );
    let err = send(
        r,
        "claude:a",
        Some("claude:b"),
        None,
        "note",
        "hi",
        "id_rcpt",
    )
    .expect_err("case variant is another recipient")
    .into_message();
    assert!(err.contains("to_agent"), "{err}");

    // NFC vs NFD spellings of the same glyph are different bytes, hence different inboxes.
    let nfc = send(
        r,
        "claude:a",
        Some("claude:caf\u{e9}"),
        None,
        "note",
        "hi",
        "id_nfc",
    )
    .unwrap();
    let nfd = send(
        r,
        "claude:a",
        Some("claude:cafe\u{301}"),
        None,
        "note",
        "hi",
        "id_nfc",
    )
    .expect_err("NFD is not the NFC inbox")
    .into_message();
    assert!(nfd.contains("to_agent"), "{nfd}");
    assert_eq!(
        send(
            r,
            "claude:a",
            Some("claude:caf\u{e9}"),
            None,
            "note",
            "hi",
            "id_nfc"
        )
        .unwrap()
        .seq,
        nfc.seq
    );

    // Unicode whitespace the bus trims (U+00A0 is trimmed by str::trim) stays a retry.
    assert_eq!(
        send(
            r,
            "claude:a",
            Some("\u{a0}claude:B\u{a0}"),
            None,
            "note",
            "hi",
            "id_rcpt"
        )
        .unwrap()
        .seq,
        first.seq
    );
    assert_eq!(bus_len(r), 2);
}

#[test]
fn thread_key_none_empty_and_blank_are_one_value_and_padding_is_trimmed() {
    let dir = repo();
    let r = dir.path().to_str().unwrap();
    let first = send(
        r,
        "claude:a",
        Some("claude:b"),
        None,
        "note",
        "hi",
        "id_thr",
    )
    .unwrap();
    for variant in [Some(""), Some("   "), Some("\t\n"), None] {
        let again = send(
            r,
            "claude:a",
            Some("claude:b"),
            variant,
            "note",
            "hi",
            "id_thr",
        )
        .unwrap_or_else(|e| panic!("{variant:?} is an unthreaded retry: {}", e.into_message()));
        assert_eq!(again.seq, first.seq, "{variant:?}");
    }
    assert!(send(
        r,
        "claude:a",
        Some("claude:b"),
        Some("t"),
        "note",
        "hi",
        "id_thr"
    )
    .is_err());

    let threaded = send(
        r,
        "claude:a",
        Some("claude:b"),
        Some(" t-1 "),
        "note",
        "hi",
        "id_thr2",
    )
    .unwrap();
    assert_eq!(threaded.thread_key.as_deref(), Some("t-1"));
    for variant in [Some("t-1"), Some("\tt-1\n")] {
        assert_eq!(
            send(
                r,
                "claude:a",
                Some("claude:b"),
                variant,
                "note",
                "hi",
                "id_thr2"
            )
            .unwrap()
            .seq,
            threaded.seq
        );
    }
    for variant in [Some("T-1"), Some(""), None] {
        assert!(
            send(
                r,
                "claude:a",
                Some("claude:b"),
                variant,
                "note",
                "hi",
                "id_thr2"
            )
            .is_err(),
            "{variant:?} must differ from 't-1'"
        );
    }

    // A broadcast (no recipient) retried as a blank recipient is still the broadcast; naming a
    // recipient is a different message.
    let bcast = send(r, "claude:a", None, None, "note", "all", "id_bc").unwrap();
    assert_eq!(
        send(r, "claude:a", Some("  "), None, "note", "all", "id_bc")
            .unwrap()
            .seq,
        bcast.seq
    );
    assert!(send(
        r,
        "claude:a",
        Some("claude:b"),
        None,
        "note",
        "all",
        "id_bc"
    )
    .is_err());
    assert_eq!(bus_len(r), 3);
}

#[test]
fn kind_has_no_aliases_and_body_compares_after_crlf_and_trim() {
    let dir = repo();
    let r = dir.path().to_str().unwrap();
    let first = send(
        r,
        "claude:a",
        Some("claude:b"),
        None,
        "note",
        "a\r\nb\n",
        "id_kind",
    )
    .unwrap();
    assert_eq!(
        send(
            r,
            "claude:a",
            Some("claude:b"),
            None,
            " note ",
            "\n a\nb ",
            "id_kind"
        )
        .unwrap()
        .seq,
        first.seq
    );
    for kind in ["NOTE", "peer_message", "ask"] {
        let err = send(
            r,
            "claude:a",
            Some("claude:b"),
            None,
            kind,
            "a\nb",
            "id_kind",
        )
        .expect_err(kind)
        .into_message();
        assert!(err.contains("kind"), "{kind}: {err}");
    }
    assert_eq!(bus_len(r), 1);
}

#[test]
fn a_large_hydrated_payload_retried_to_a_second_recipient_is_refused() {
    let dir = repo();
    let r = dir.path().to_str().unwrap();
    let large = "payload-line\n".repeat(700);
    let first = send(
        r,
        "claude:a",
        Some("claude:b"),
        Some("t"),
        "handoff",
        &large,
        "id_big",
    )
    .unwrap();
    let err = send(
        r,
        "claude:a",
        Some("claude:c"),
        Some("t"),
        "handoff",
        &large,
        "id_big",
    )
    .expect_err("second recipient")
    .into_message();
    assert!(err.contains("to_agent"), "{err}");
    let again = send(
        r,
        "claude:a",
        Some("claude:b"),
        Some("t"),
        "handoff",
        &large,
        "id_big",
    )
    .unwrap();
    assert_eq!(again.seq, first.seq);
    assert_eq!(again.body, first.body);
    assert_eq!(bus_len(r), 1);
    let c_inbox =
        read_messages_for_peer(r, &["claude:c".to_string()], 0, None, false, true).unwrap();
    assert!(c_inbox.messages.is_empty());
}

#[test]
fn a_msg_id_is_one_namespace_across_senders() {
    let dir = repo();
    let r = dir.path().to_str().unwrap();
    send(
        r,
        "claude:a",
        Some("claude:b"),
        None,
        "note",
        "hi",
        "id_shared",
    )
    .unwrap();
    let err = send(
        r,
        "claude:z",
        Some("claude:b"),
        None,
        "note",
        "hi",
        "id_shared",
    )
    .expect_err("another sender may not reuse or read back the id")
    .into_message();
    assert!(err.contains("from_agent"), "{err}");
    assert_eq!(bus_len(r), 1);
}

#[test]
fn a_refused_reuse_does_not_consume_a_sequence_number() {
    let dir = repo();
    let r = dir.path().to_str().unwrap();
    let first = send(
        r,
        "claude:a",
        Some("claude:b"),
        None,
        "note",
        "hi",
        "id_seq",
    )
    .unwrap();
    for to in ["claude:c", "claude:d", "claude:e"] {
        assert!(send(r, "claude:a", Some(to), None, "note", "hi", "id_seq").is_err());
    }
    let next = send(
        r,
        "claude:a",
        Some("claude:b"),
        None,
        "note",
        "next",
        "id_seq2",
    )
    .unwrap();
    assert_eq!(next.seq, first.seq + 1);
}

#[test]
fn a_retry_after_the_original_was_acknowledged_returns_the_row_with_its_read_state() {
    let dir = repo();
    let r = dir.path().to_str().unwrap();
    let first = send(
        r,
        "claude:a",
        Some("claude:b"),
        None,
        "note",
        "hi",
        "id_ack",
    )
    .unwrap();
    assert_eq!(
        ack_messages(r, "claude:b", &["id_ack".to_string()]).unwrap(),
        1
    );
    let retry = send(
        r,
        "claude:a",
        Some("claude:b"),
        None,
        "note",
        "hi",
        "id_ack",
    )
    .unwrap();
    assert_eq!(retry.seq, first.seq);
    assert_eq!(
        retry.read_by,
        vec!["claude:b".to_string()],
        "retry must not reset read state"
    );
    assert!(send(
        r,
        "claude:a",
        Some("claude:c"),
        None,
        "note",
        "hi",
        "id_ack"
    )
    .is_err());
    assert_eq!(bus_len(r), 1);
}

#[test]
fn a_retry_after_the_original_row_was_pruned_is_a_fresh_message() {
    let dir = repo();
    let r = dir.path().to_str().unwrap();
    let first = send(
        r,
        "claude:a",
        Some("claude:b"),
        None,
        "note",
        "hi",
        "id_prune",
    )
    .unwrap();
    for n in 0..=MAX_BUS_MESSAGES {
        send(
            r,
            "claude:a",
            Some("claude:b"),
            None,
            "note",
            &format!("fill {n}"),
            &format!("fill_{n}"),
        )
        .unwrap();
    }
    let page = read_messages(r, None, 0).unwrap();
    assert!(
        page.messages
            .iter()
            .all(|m| m.msg_id.as_deref() != Some("id_prune")),
        "the original must have been evicted"
    );
    // The row is gone, so there is nothing to compare against: the id is reusable, even for
    // another recipient. Idempotency is bounded by the ring buffer.
    let again = send(
        r,
        "claude:a",
        Some("claude:c"),
        None,
        "note",
        "hi",
        "id_prune",
    )
    .unwrap();
    assert!(again.seq > first.seq);
    assert_eq!(again.to_agent.as_deref(), Some("claude:c"));
}

#[test]
fn concurrent_identical_retries_store_exactly_one_row_and_both_succeed() {
    let dir = repo();
    let r = dir.path().to_str().unwrap();
    for round in 0..12 {
        let id = format!("id_same_{round}");
        let gate = Barrier::new(2);
        let results: Vec<Result<AgentBusMessage, CommandError>> = std::thread::scope(|s| {
            let workers: Vec<_> = (0..2)
                .map(|_| {
                    s.spawn(|| {
                        gate.wait();
                        send(
                            r,
                            "claude:a",
                            Some("claude:b"),
                            Some("t"),
                            "note",
                            "hi",
                            &id,
                        )
                    })
                })
                .collect();
            workers
                .into_iter()
                .map(|w| w.join().expect("worker"))
                .collect()
        });
        let seqs: Vec<u64> = results
            .into_iter()
            .map(|res| {
                res.unwrap_or_else(|e| panic!("round {round}: {}", e.into_message()))
                    .seq
            })
            .collect();
        assert_eq!(seqs[0], seqs[1], "round {round}");
    }
    assert_eq!(bus_len(r), 12);
}

#[test]
fn concurrent_same_id_different_recipient_sends_have_one_winner_and_no_loser_row() {
    let dir = repo();
    let r = dir.path().to_str().unwrap();
    for round in 0..12 {
        let id = format!("id_race_{round}");
        let gate = Barrier::new(2);
        let (b, c) = std::thread::scope(|s| {
            let wb = s.spawn(|| {
                gate.wait();
                send(r, "claude:a", Some("claude:b"), None, "note", "hi", &id)
            });
            let wc = s.spawn(|| {
                gate.wait();
                send(r, "claude:a", Some("claude:c"), None, "note", "hi", &id)
            });
            (wb.join().expect("b"), wc.join().expect("c"))
        });
        assert_eq!(
            b.is_ok() as u8 + c.is_ok() as u8,
            1,
            "round {round}: exactly one winner"
        );
        let (winner, loser) = if b.is_ok() {
            ("claude:b", "claude:c")
        } else {
            ("claude:c", "claude:b")
        };
        let page = read_messages(r, None, 0).unwrap();
        let rows: Vec<_> = page
            .messages
            .iter()
            .filter(|m| m.msg_id.as_deref() == Some(id.as_str()))
            .collect();
        assert_eq!(rows.len(), 1, "round {round}");
        assert_eq!(rows[0].to_agent.as_deref(), Some(winner));
        let loser_inbox =
            read_messages_for_peer(r, &[loser.to_string()], 0, None, false, false).unwrap();
        assert!(
            loser_inbox
                .messages
                .iter()
                .all(|m| m.msg_id.as_deref() != Some(id.as_str())),
            "round {round}: the loser must have no row"
        );
    }
}
