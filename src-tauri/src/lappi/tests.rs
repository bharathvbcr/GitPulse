//! The caller contract, attacked from the outside: a fake agent on a real Unix
//! socket for every outcome and every way a reply can go wrong, the admission
//! rule, the record writer's caps and permissions, and the records themselves
//! against the contract's checker when its binary is available.

use super::client::{self, NotAskedReason, Outcome, UnavailableReason, MAX_PAYLOAD_BYTES};
use super::record::{self, Appended, Pending, Store};
use super::{consult, observe_commit, Consult};
use crate::ai::commit_brief::{self, CommitDraft};

use std::collections::BTreeSet;
use std::io::{BufRead, BufReader, Write};
use std::os::unix::fs::PermissionsExt;
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::thread;
use std::time::{Duration, Instant};

use serde_json::Value;

/// `LAPPI_COLLECT` is process-wide, and [`Store::append`] reads it on every
/// call, so every test that appends or sets it holds this.
static COLLECT_ENV: Mutex<()> = Mutex::new(());

fn collect_env() -> std::sync::MutexGuard<'static, ()> {
    COLLECT_ENV
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

const ID: &str = "0123456789abcdef0123456789abcdef";
const DEADLINE: Duration = Duration::from_secs(2);

const ANSWER_FIX: &str = r#"{"status":"ok","schema_version":1,"backend":"qd-metal/qwen3.5-2b-base/tessl","degraded":false,"slots":{"commit_type":{"value":"fix","conformal_set":["fix"],"score":0.8,"noul":false,"degraded":false}}}"#;
const ANSWER_NOUL: &str = r#"{"status":"ok","schema_version":1,"backend":"qd-metal/qwen3.5-2b-base/tessl","degraded":false,"slots":{"commit_type":{"value":null,"conformal_set":null,"score":0.01,"noul":true,"degraded":false}}}"#;
const ANSWER_DEGRADED: &str = r#"{"status":"ok","schema_version":1,"backend":"reference-deterministic-v1","degraded":true,"slots":{"commit_type":{"value":"fix","conformal_set":["fix"],"score":0.7,"noul":false,"degraded":true}}}"#;
const ANSWER_WIP: &str = r#"{"status":"ok","schema_version":1,"backend":"qd-metal/qwen3.5-2b-base/tessl","degraded":false,"slots":{"commit_type":{"value":"wip","conformal_set":["wip"],"score":0.8,"noul":false,"degraded":false}}}"#;
const REFUSED: &str = r#"{"status":"refused","schema_version":1,"refusal":{"kind":"task_not_trained","task":"gitpulse.commit_type","available":["code.defect_class"]},"message":"not trained"}"#;
const ERROR: &str = r#"{"status":"error","schema_version":1,"error":{"kind":"overloaded","limit":8},"message":"busy"}"#;

/* ── a fake agent ─────────────────────────────────────────────────────────── */

/// A listener under `/tmp`: macOS caps `sun_path` at 104 bytes, and a
/// session's `$TMPDIR` alone can be longer than that.
fn socket_dir() -> tempfile::TempDir {
    tempfile::Builder::new()
        .prefix("lappi")
        .tempdir_in("/tmp")
        .expect("a temp dir under /tmp")
}

struct Agent {
    _dir: tempfile::TempDir,
    path: PathBuf,
    requests: std::sync::mpsc::Receiver<Vec<u8>>,
}

/// Accept one connection, read its request line, hand it to the test, then
/// behave as `behave` says.
fn agent(behave: impl FnOnce(UnixStream) + Send + 'static) -> Agent {
    let dir = socket_dir();
    let path = dir.path().join("qd.sock");
    let listener = UnixListener::bind(&path).expect("the fake agent binds");
    let (tx, requests) = std::sync::mpsc::channel();
    thread::spawn(move || {
        let Ok((stream, _)) = listener.accept() else {
            return;
        };
        let mut line = Vec::new();
        if let Ok(reader) = stream.try_clone() {
            let _ = BufReader::new(reader).read_until(b'\n', &mut line);
        }
        let _ = tx.send(line);
        behave(stream);
    });
    Agent {
        _dir: dir,
        path,
        requests,
    }
}

fn replying(reply: &'static str) -> Agent {
    agent(move |mut stream| {
        let _ = stream.write_all(reply.as_bytes());
        let _ = stream.write_all(b"\n");
    })
}

fn request_line() -> Vec<u8> {
    super::request::commit_type_request(b"diff --git a/x b/x\n", ID).expect("small request")
}

fn unavailable(reason: UnavailableReason) -> Outcome {
    Outcome::Unavailable { reason }
}

/* ── the six outcomes over a real socket ──────────────────────────────────── */

#[test]
fn an_answer_is_model_answered_and_the_request_arrives_whole() {
    let agent = replying(ANSWER_FIX);
    let outcome = client::ask(&agent.path, &request_line(), DEADLINE);
    assert_eq!(outcome.reading(), "model_answered");
    assert_eq!(outcome.chosen("commit_type"), Some("fix"));

    let sent = agent
        .requests
        .recv_timeout(DEADLINE)
        .expect("request captured");
    assert_eq!(sent.last(), Some(&b'\n'), "one framed line");
    let request: Value = serde_json::from_slice(&sent[..sent.len() - 1]).expect("JSON request");
    assert_eq!(request["task"], "gitpulse.commit_type");
    assert_eq!(
        request["example_id"],
        format!("gitpulse:gitpulse.commit_type:{ID}")
    );
}

#[test]
fn a_reply_written_at_accept_is_read_even_when_the_request_write_fails() {
    // `qd serve` over its connection cap writes `overloaded` the moment it
    // accepts, then closes without reading. The request then meets a closed
    // socket (and macOS refuses SO_SNDTIMEO/SO_RCVTIMEO on it with EINVAL), but
    // the reply is already in the receive buffer and must be the outcome.
    // Repeated so the race lands both ways.
    for round in 0..30 {
        let dir = socket_dir();
        let path = dir.path().join("qd.sock");
        let listener = UnixListener::bind(&path).expect("binds");
        let refuser = thread::spawn(move || {
            let (mut stream, _) = listener.accept().expect("accepts");
            stream.write_all(ERROR.as_bytes()).expect("writes");
            stream.write_all(b"\n").expect("writes");
            let _ = stream.shutdown(std::net::Shutdown::Both);
        });
        // Larger than a Unix socket buffer, so the write cannot complete
        // before the close.
        let request = vec![b' '; 512 * 1024];
        let outcome = client::ask(&path, &request, DEADLINE);
        refuser.join().expect("refuser thread");
        assert_eq!(
            outcome.reading(),
            "backend_failed",
            "round {round}: {outcome:?}"
        );
    }
}

#[test]
fn every_slot_noul_is_model_abstained() {
    let agent = replying(ANSWER_NOUL);
    assert_eq!(
        client::ask(&agent.path, &request_line(), DEADLINE).reading(),
        "model_abstained"
    );
}

#[test]
fn a_degraded_answer_is_model_abstained_whatever_its_slots_say() {
    let agent = replying(ANSWER_DEGRADED);
    let outcome = client::ask(&agent.path, &request_line(), DEADLINE);
    assert_eq!(outcome.reading(), "model_abstained");
    assert_eq!(outcome.chosen("commit_type"), None);
}

#[test]
fn a_refusal_is_request_refused_with_its_kind() {
    let agent = replying(REFUSED);
    assert_eq!(
        client::ask(&agent.path, &request_line(), DEADLINE),
        Outcome::RequestRefused {
            kind: "task_not_trained".into()
        }
    );
}

#[test]
fn a_backend_error_is_backend_failed_with_its_kind() {
    let agent = replying(ERROR);
    assert_eq!(
        client::ask(&agent.path, &request_line(), DEADLINE),
        Outcome::BackendFailed {
            kind: "overloaded".into()
        }
    );
}

#[test]
fn no_socket_is_socket_not_found() {
    let dir = socket_dir();
    assert_eq!(
        client::ask(&dir.path().join("absent.sock"), &request_line(), DEADLINE),
        unavailable(UnavailableReason::SocketNotFound)
    );
}

#[test]
fn a_socket_nobody_listens_on_is_connect_refused() {
    let dir = socket_dir();
    let path = dir.path().join("stale.sock");
    drop(UnixListener::bind(&path).expect("bind"));
    assert!(path.exists(), "the stale socket file stays behind");
    assert_eq!(
        client::ask(&path, &request_line(), DEADLINE),
        unavailable(UnavailableReason::ConnectRefused)
    );
}

#[test]
fn a_line_over_the_cap_is_not_asked_and_never_connects() {
    let dir = socket_dir();
    // No listener at all: had it tried to connect, this would be socket_not_found.
    let line = vec![b'x'; MAX_PAYLOAD_BYTES + 1];
    assert_eq!(
        client::ask(&dir.path().join("absent.sock"), &line, DEADLINE),
        Outcome::NotAsked {
            reason: NotAskedReason::OverPayloadCap
        }
    );
}

#[test]
fn a_zero_deadline_is_refused_before_connecting() {
    let dir = socket_dir();
    assert_eq!(
        client::ask(
            &dir.path().join("absent.sock"),
            &request_line(),
            Duration::ZERO
        ),
        unavailable(UnavailableReason::Deadline)
    );
}

/* ── the deadline is one deadline ─────────────────────────────────────────── */

#[test]
fn a_dripping_agent_cannot_hold_the_caller_past_the_whole_exchange_deadline() {
    // A byte every 150 ms resets any per-read timeout forever; only a deadline
    // over the whole exchange ends this.
    let agent = agent(|mut stream| {
        for _ in 0..40 {
            if stream.write_all(b"x").is_err() {
                return;
            }
            thread::sleep(Duration::from_millis(150));
        }
    });
    let started = Instant::now();
    let outcome = client::ask(&agent.path, &request_line(), Duration::from_millis(500));
    let elapsed = started.elapsed();
    assert_eq!(outcome, unavailable(UnavailableReason::Deadline));
    assert!(
        elapsed < Duration::from_millis(1500),
        "held for {elapsed:?} by a dripping agent"
    );
}

#[test]
fn an_agent_that_accepts_and_never_writes_is_a_deadline() {
    let agent = agent(|stream| {
        thread::sleep(Duration::from_secs(2));
        drop(stream);
    });
    let started = Instant::now();
    let outcome = client::ask(&agent.path, &request_line(), Duration::from_millis(300));
    assert_eq!(outcome, unavailable(UnavailableReason::Deadline));
    assert!(started.elapsed() < Duration::from_millis(1500));
}

#[test]
fn a_close_before_the_newline_is_closed_without_reply() {
    let agent = agent(|mut stream| {
        let _ = stream.write_all(br#"{"status":"ok","schema_version":1"#);
    });
    assert_eq!(
        client::ask(&agent.path, &request_line(), DEADLINE),
        unavailable(UnavailableReason::ClosedWithoutReply)
    );
}

#[test]
fn a_close_with_no_bytes_is_closed_without_reply() {
    let agent = agent(drop);
    assert_eq!(
        client::ask(&agent.path, &request_line(), DEADLINE),
        unavailable(UnavailableReason::ClosedWithoutReply)
    );
}

#[test]
fn a_reply_over_the_cap_is_reply_over_cap_not_an_allocation() {
    let agent = agent(|mut stream| {
        let _ = stream.write_all(&vec![b'a'; MAX_PAYLOAD_BYTES + 16]);
        let _ = stream.write_all(b"\n");
    });
    assert_eq!(
        client::ask(&agent.path, &request_line(), Duration::from_secs(5)),
        unavailable(UnavailableReason::ReplyOverCap)
    );
}

#[test]
fn a_garbage_reply_is_reply_unparseable() {
    let agent = replying("hello, I am not JSON");
    assert_eq!(
        client::ask(&agent.path, &request_line(), DEADLINE),
        unavailable(UnavailableReason::ReplyUnparseable)
    );
}

#[test]
fn an_unknown_status_is_reply_unparseable_never_a_guess() {
    let agent = replying(r#"{"status":"partial","schema_version":1,"slots":{}}"#);
    assert_eq!(
        client::ask(&agent.path, &request_line(), DEADLINE),
        unavailable(UnavailableReason::ReplyUnparseable)
    );
}

/* ── admission ────────────────────────────────────────────────────────────── */

const MODIFIED_SOURCE: &str = "diff --git a/src/lib.rs b/src/lib.rs\n\
index 1111111..2222222 100644\n\
--- a/src/lib.rs\n\
+++ b/src/lib.rs\n\
@@ -1 +1 @@\n\
-fn a() {}\n\
+fn a() { b() }\n";

const ADDED_DOC: &str = "diff --git a/README.md b/README.md\n\
new file mode 100644\n\
index 0000000..1111111\n\
--- /dev/null\n\
+++ b/README.md\n\
@@ -0,0 +1 @@\n\
+# hello\n";

fn subjects(conventional: bool) -> Vec<String> {
    if conventional {
        vec!["feat: add a".into(), "fix: mend b".into(), "docs: c".into()]
    } else {
        vec!["Add a".into(), "Mend b".into(), "Write c".into()]
    }
}

fn low_confidence(conventional: bool) -> CommitDraft {
    let draft = commit_brief::draft_change(
        MODIFIED_SOURCE,
        None,
        None,
        &subjects(conventional),
        "main",
        false,
    );
    assert!(!draft.high_confidence && draft.change_type.is_none() && draft.prefix.is_none());
    draft
}

/// Consult with asking on and no store, against an agent that replies `reply`.
fn consult_with(reply: &'static str, draft: &mut CommitDraft) -> super::Consulted {
    let agent = replying(reply);
    let pending = Mutex::new(Pending::new());
    let ctx = Consult {
        ask: true,
        socket: Some(&agent.path),
        deadline: DEADLINE,
        store: None,
        pending: &pending,
    };
    consult(&ctx, "/repo", MODIFIED_SOURCE.as_bytes(), draft)
}

#[test]
fn a_known_answer_pre_selects_the_missing_type_and_nothing_else() {
    let before = low_confidence(true);
    let mut draft = before.clone();
    let consulted = consult_with(ANSWER_FIX, &mut draft);
    assert_eq!(consulted.prefilled, Some("fix"));
    assert!(draft.subject.starts_with("fix"), "{}", draft.subject);
    assert!(draft.message.starts_with(&draft.subject));
    assert!(draft.brief.contains("Keep this prefix exactly: fix"));
    assert!(
        !draft.high_confidence,
        "a suggestion is not the classifier's certainty"
    );
    assert_eq!(
        draft.change_type, None,
        "the classifier's own choice is untouched"
    );
    assert_eq!(draft.body, before.body);
    assert_eq!(draft.facts, before.facts);
}

#[test]
fn an_answer_outside_the_known_types_changes_nothing() {
    let before = low_confidence(true);
    let mut draft = before.clone();
    let consulted = consult_with(ANSWER_WIP, &mut draft);
    assert_eq!(consulted.outcome.reading(), "model_answered");
    assert_eq!(consulted.prefilled, None);
    assert_eq!(draft, before);
}

#[test]
fn an_answer_cannot_replace_a_type_the_classifier_already_chose() {
    let before = commit_brief::draft_change(ADDED_DOC, None, None, &subjects(true), "main", false);
    assert!(before.high_confidence && before.change_type.is_some());
    let mut draft = before.clone();
    let consulted = consult_with(ANSWER_FIX, &mut draft);
    assert_eq!(consulted.prefilled, None);
    assert_eq!(draft, before);
    // And the guard holds below the consult gate too.
    let mut direct = before.clone();
    assert!(!commit_brief::prefill_type(&mut direct, "fix"));
    assert_eq!(direct, before);
}

#[test]
fn an_answer_never_writes_a_prefix_into_a_non_conventional_repository() {
    let before = low_confidence(false);
    let mut draft = before.clone();
    assert_eq!(consult_with(ANSWER_FIX, &mut draft).prefilled, None);
    assert_eq!(draft, before);
}

#[test]
fn abstentions_refusals_and_errors_leave_the_draft_byte_for_byte() {
    for reply in [ANSWER_NOUL, ANSWER_DEGRADED, REFUSED, ERROR, "garbage"] {
        let before = low_confidence(true);
        let mut draft = before.clone();
        let consulted = consult_with(reply, &mut draft);
        assert_eq!(consulted.prefilled, None, "{reply}");
        assert_eq!(draft, before, "{reply}");
    }
}

#[test]
fn asking_off_sends_nothing_and_changes_nothing() {
    let agent = replying(ANSWER_FIX);
    let pending = Mutex::new(Pending::new());
    let ctx = Consult {
        ask: false,
        socket: Some(&agent.path),
        deadline: DEADLINE,
        store: None,
        pending: &pending,
    };
    let before = low_confidence(true);
    let mut draft = before.clone();
    let consulted = consult(&ctx, "/repo", MODIFIED_SOURCE.as_bytes(), &mut draft);
    assert_eq!(
        consulted.outcome,
        Outcome::NotAsked {
            reason: NotAskedReason::Disabled
        }
    );
    assert_eq!(draft, before);
    assert!(
        agent
            .requests
            .recv_timeout(Duration::from_millis(200))
            .is_err(),
        "no connection was made"
    );
}

#[test]
fn a_cut_patch_is_not_asked() {
    let agent = replying(ANSWER_FIX);
    let pending = Mutex::new(Pending::new());
    let ctx = Consult {
        ask: true,
        socket: Some(&agent.path),
        deadline: DEADLINE,
        store: None,
        pending: &pending,
    };
    let mut draft =
        commit_brief::draft_change(MODIFIED_SOURCE, None, None, &subjects(true), "main", true);
    let before = draft.clone();
    let consulted = consult(&ctx, "/repo", MODIFIED_SOURCE.as_bytes(), &mut draft);
    assert_eq!(
        consulted.outcome,
        Outcome::NotAsked {
            reason: NotAskedReason::PatchTruncated
        }
    );
    assert_eq!(draft, before);
}

/* ── the writer ───────────────────────────────────────────────────────────── */

fn held_out_store() -> (tempfile::TempDir, Store) {
    let root = tempfile::tempdir().expect("temp dir");
    let dir = root.path().join("Lappi/heldout/caller-records/gitpulse");
    (root, Store::new(dir).expect("held-out store"))
}

const DAY: &str = "2026-10-06";

fn mode(path: &Path) -> u32 {
    std::fs::metadata(path)
        .expect("metadata")
        .permissions()
        .mode()
        & 0o777
}

#[test]
fn records_are_append_only_0600_in_0700_directories() {
    let _env = collect_env();
    let (root, store) = held_out_store();
    assert_eq!(
        store.append(DAY, b"{\"n\":1}").expect("append"),
        Appended::Written
    );
    let file = store.dir().join(format!("{DAY}.jsonl"));
    let first = std::fs::read(&file).expect("read");
    assert_eq!(
        store.append(DAY, b"{\"n\":2}").expect("append"),
        Appended::Written
    );
    let both = std::fs::read(&file).expect("read");
    assert!(
        both.starts_with(&first),
        "an append never rewrites what is there"
    );
    assert_eq!(both, b"{\"n\":1}\n{\"n\":2}\n");
    assert_eq!(mode(&file), 0o600);
    for dir in [
        store.dir().to_path_buf(),
        root.path().join("Lappi/heldout/caller-records"),
        root.path().join("Lappi/heldout"),
        root.path().join("Lappi"),
    ] {
        assert_eq!(mode(&dir), 0o700, "{}", dir.display());
    }
    assert_eq!(store.status().written, 2);
}

#[test]
fn a_loose_existing_file_is_tightened_to_0600() {
    let _env = collect_env();
    let (_root, store) = held_out_store();
    std::fs::create_dir_all(store.dir()).expect("mkdir");
    let file = store.dir().join(format!("{DAY}.jsonl"));
    std::fs::write(&file, b"").expect("create");
    std::fs::set_permissions(&file, std::fs::Permissions::from_mode(0o644)).expect("chmod");
    store.append(DAY, b"{}").expect("append");
    assert_eq!(mode(&file), 0o600);
}

#[test]
fn a_line_over_64_kib_is_dropped_and_counted() {
    let _env = collect_env();
    let (_root, store) = held_out_store();
    let line = vec![b'a'; record::MAX_RECORD_BYTES + 1];
    assert_eq!(
        store.append(DAY, &line).expect("append"),
        Appended::Dropped("line_over_cap")
    );
    assert_eq!(
        store.append(DAY, b"a\nb").expect("append"),
        Appended::Dropped("line_over_cap")
    );
    let exact = vec![b'a'; record::MAX_RECORD_BYTES];
    assert_eq!(
        store.append(DAY, &exact).expect("append"),
        Appended::Written
    );
    let status = store.status();
    assert_eq!(
        (status.written, status.dropped, status.stopped),
        (1, 2, None)
    );
}

#[test]
fn a_full_day_file_stops_recording_and_counts_every_drop() {
    let _env = collect_env();
    let (_root, store) = held_out_store();
    std::fs::create_dir_all(store.dir()).expect("mkdir");
    let file = store.dir().join(format!("{DAY}.jsonl"));
    // Sparse: the length is what the cap reads, not the blocks.
    let handle = std::fs::File::create(&file).expect("create");
    handle
        .set_len(record::MAX_DAY_FILE_BYTES - 4)
        .expect("set_len");
    drop(handle);
    assert_eq!(
        store.append(DAY, b"{\"n\":1}").expect("append"),
        Appended::Dropped("day_file_over_cap")
    );
    assert_eq!(
        store.append("2026-10-07", b"{}").expect("append"),
        Appended::Dropped("stopped")
    );
    let status = store.status();
    assert_eq!(status.stopped.as_deref(), Some("day_file_over_cap"));
    assert_eq!((status.written, status.dropped), (0, 2));
    assert_eq!(
        std::fs::metadata(&file).expect("metadata").len(),
        record::MAX_DAY_FILE_BYTES - 4,
        "nothing was appended past the cap"
    );
}

#[test]
fn a_full_store_stops_recording() {
    let _env = collect_env();
    let (_root, store) = held_out_store();
    std::fs::create_dir_all(store.dir()).expect("mkdir");
    for (i, day) in ["2026-01-01", "2026-01-02", "2026-01-03", "2026-01-04"]
        .iter()
        .enumerate()
    {
        let handle =
            std::fs::File::create(store.dir().join(format!("{day}.jsonl"))).expect("create");
        let len = if i == 3 {
            record::MAX_STORE_BYTES - 3 * (record::MAX_STORE_BYTES / 4)
        } else {
            record::MAX_STORE_BYTES / 4
        };
        handle.set_len(len).expect("set_len");
    }
    assert_eq!(
        store.append(DAY, b"{}").expect("append"),
        Appended::Dropped("store_over_cap")
    );
    assert_eq!(store.status().stopped.as_deref(), Some("store_over_cap"));
    assert!(!store.dir().join(format!("{DAY}.jsonl")).exists());
}

#[test]
fn lappi_collect_zero_forces_recording_off() {
    let serial = collect_env();
    let (_root, store) = held_out_store();
    {
        let _off = crate::test_support::env::bind_env(&serial).set(record::COLLECT_ENV, "0");
        assert_eq!(
            store.append(DAY, b"{}").expect("append"),
            Appended::ForcedOff
        );
        assert!(!store.dir().exists(), "nothing was created");
        assert_eq!(store.status().dropped, 0, "forced off is not a drop");
    }
    let _on = crate::test_support::env::bind_env(&serial).set(record::COLLECT_ENV, "1");
    assert_eq!(store.append(DAY, b"{}").expect("append"), Appended::Written);
}

#[test]
fn concurrent_writers_produce_only_whole_lines() {
    let _env = collect_env();
    let root = tempfile::tempdir().expect("temp dir");
    let dir = root.path().join("heldout/caller-records/gitpulse");
    const WRITERS: usize = 8;
    const LINES: usize = 40;
    // Separate stores, so nothing in-process serialises them: only O_APPEND
    // and one write per line keep the lines whole.
    let handles: Vec<_> = (0..WRITERS)
        .map(|writer| {
            let dir = dir.clone();
            thread::spawn(move || {
                let store = Store::new(dir).expect("store");
                for line in 0..LINES {
                    let body = format!(
                        "{{\"writer\":{writer},\"line\":{line},\"pad\":\"{}\"}}",
                        "p".repeat(4096)
                    );
                    assert_eq!(
                        store.append(DAY, body.as_bytes()).expect("append"),
                        Appended::Written
                    );
                }
            })
        })
        .collect();
    for handle in handles {
        handle.join().expect("writer thread");
    }
    let text = std::fs::read_to_string(dir.join(format!("{DAY}.jsonl"))).expect("read");
    assert!(text.ends_with('\n'));
    let mut seen = BTreeSet::new();
    for line in text.lines() {
        let value: Value = serde_json::from_str(line).expect("every line is whole JSON");
        seen.insert((value["writer"].as_u64(), value["line"].as_u64()));
    }
    assert_eq!(seen.len(), WRITERS * LINES);
}

/* ── records ──────────────────────────────────────────────────────────────── */

const DECISION_KEYS: [&str; 13] = [
    "record",
    "record_version",
    "kind",
    "record_id",
    "app",
    "app_version",
    "decision_point",
    "created_at",
    "admission",
    "facts",
    "app_choice",
    "lappi",
    "redaction",
];
const OUTCOME_KEYS: [&str; 11] = [
    "record",
    "record_version",
    "kind",
    "record_id",
    "app",
    "app_version",
    "decision_point",
    "created_at",
    "admission",
    "observed",
    "redaction",
];
const LAPPI_KEYS: [&str; 7] = [
    "asked",
    "task",
    "reading",
    "kind",
    "backend",
    "slots",
    "latency_ms",
];

fn keys(value: &Value) -> BTreeSet<String> {
    value
        .as_object()
        .expect("an object")
        .keys()
        .cloned()
        .collect()
}

fn set(names: &[&str]) -> BTreeSet<String> {
    names.iter().map(|name| (*name).to_string()).collect()
}

/// The contract's shape and `lappi` consistency rules, asserted on one line.
/// The checker binary is the authority; this keeps the suite honest without it.
fn assert_record_shape(line: &[u8]) -> Value {
    assert!(line.len() <= record::MAX_RECORD_BYTES);
    let value: Value = serde_json::from_slice(line).expect("a JSON line");
    assert_eq!(value["record"], "lappi.caller_record");
    assert_eq!(value["record_version"], 1);
    assert_eq!(value["admission"], "not_admitted");
    assert_eq!(value["app"], "gitpulse");
    assert_eq!(value["decision_point"], "gitpulse.commit_type");
    assert_eq!(value["redaction"]["policy"], "gitpulse.ledger.redact");
    assert!(value["redaction"]["fields_redacted"].is_u64());
    assert_eq!(
        keys(&value["redaction"]),
        set(&["policy", "fields_redacted"])
    );
    let created = value["created_at"].as_str().expect("created_at");
    assert!(
        created.ends_with('Z') && created.as_bytes()[10] == b'T',
        "{created}"
    );
    let id = value["record_id"].as_str().expect("record_id");
    assert!(
        id.len() == 32
            && id
                .bytes()
                .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
    );
    match value["kind"].as_str() {
        Some("decision") => {
            assert_eq!(keys(&value), set(&DECISION_KEYS));
            let lappi = &value["lappi"];
            assert_eq!(keys(lappi), set(&LAPPI_KEYS));
            let reading = lappi["reading"].as_str().expect("reading");
            let asked = lappi["asked"].as_bool().expect("asked");
            assert_eq!(asked, reading != "not_asked");
            assert_eq!(asked, lappi["task"].is_string());
            match reading {
                "model_answered" | "model_abstained" => {
                    assert!(lappi["slots"].is_object() && lappi["backend"].is_string());
                    assert!(lappi["kind"].is_null());
                }
                "request_refused" | "backend_failed" | "unavailable" => {
                    assert!(lappi["kind"].is_string() && lappi["slots"].is_null());
                    assert!(lappi["backend"].is_null());
                }
                "not_asked" => {
                    for key in ["kind", "backend", "slots", "latency_ms"] {
                        assert!(lappi[key].is_null(), "{key}");
                    }
                }
                other => panic!("unknown reading {other}"),
            }
        }
        Some("outcome") => assert_eq!(keys(&value), set(&OUTCOME_KEYS)),
        other => panic!("unknown kind {other:?}"),
    }
    value
}

fn decision_for(outcome: &Outcome, id: &str) -> Vec<u8> {
    let draft = low_confidence(true);
    record::decision_line(&record::DecisionInput {
        record_id: id,
        created_at: "2026-10-06T15:04:05.123Z",
        draft: &draft,
        prefilled: None,
        outcome,
        latency_ms: Some(3),
    })
    .expect("a decision line")
}

fn every_outcome() -> Vec<Outcome> {
    vec![
        client::parse_reply(ANSWER_FIX.as_bytes()),
        client::parse_reply(ANSWER_NOUL.as_bytes()),
        client::parse_reply(ANSWER_DEGRADED.as_bytes()),
        client::parse_reply(REFUSED.as_bytes()),
        client::parse_reply(ERROR.as_bytes()),
        unavailable(UnavailableReason::Deadline),
        Outcome::NotAsked {
            reason: NotAskedReason::Disabled,
        },
    ]
}

#[test]
fn decision_records_have_the_contract_shape_for_every_outcome() {
    for outcome in every_outcome() {
        let value = assert_record_shape(&decision_for(&outcome, ID));
        assert_eq!(value["lappi"]["reading"], outcome.reading());
        // Structure, not content: nothing of the patch is in the facts.
        let text = value.to_string();
        assert!(
            !text.contains("src/lib.rs") && !text.contains("fn a()"),
            "{text}"
        );
        assert_eq!(value["facts"]["roles"]["source"], 1);
        assert_eq!(value["facts"]["kinds"]["modified"], 1);
        assert_eq!(value["app_choice"]["type"], Value::Null);
        assert_eq!(value["app_choice"]["high_confidence"], false);
    }
}

#[test]
fn outcome_records_carry_the_parsed_subject_never_the_message() {
    let line = record::outcome_line(
        ID,
        "2026-10-06T15:06:00.250Z",
        "fix(engine)!: stop leaking the secret plan\n\nlong body text",
    )
    .expect("an outcome line");
    let value = assert_record_shape(&line);
    assert_eq!(value["observed"]["type"], "fix");
    assert_eq!(value["observed"]["scope"], "engine");
    assert_eq!(value["observed"]["breaking"], true);
    assert_eq!(value["observed"]["committed"], true);
    let text = value.to_string();
    assert!(
        !text.contains("secret plan") && !text.contains("long body"),
        "{text}"
    );

    let plain = assert_record_shape(
        &record::outcome_line(ID, "2026-10-06T15:06:00.250Z", "Make it work").expect("line"),
    );
    assert_eq!(plain["observed"]["type"], Value::Null);
    assert_eq!(plain["observed"]["conventional"], false);
}

#[test]
fn a_credential_in_a_reply_is_redacted_before_it_is_written() {
    let token = "ghp_0123456789abcdefghijklmnopqrstuvwxyzA";
    let reply = format!(
        r#"{{"status":"ok","schema_version":1,"backend":"{token}","degraded":false,"slots":{{"commit_type":{{"value":"fix","conformal_set":null,"score":0.5,"noul":false,"degraded":false}}}}}}"#
    );
    let outcome = client::parse_reply(reply.as_bytes());
    let line = decision_for(&outcome, ID);
    let text = String::from_utf8(line.clone()).expect("UTF-8");
    assert!(!text.contains(token), "{text}");
    let value = assert_record_shape(&line);
    assert!(value["redaction"]["fields_redacted"].as_u64() >= Some(1));
}

#[test]
fn a_decision_and_its_commit_pair_through_the_pending_map() {
    let _env = collect_env();
    let (_root, store) = held_out_store();
    let agent = replying(REFUSED);
    let pending = Mutex::new(Pending::new());
    let repo = tempfile::tempdir().expect("repo dir");
    let repo_path = repo.path().to_string_lossy().to_string();
    let ctx = Consult {
        ask: true,
        socket: Some(&agent.path),
        deadline: DEADLINE,
        store: Some(&store),
        pending: &pending,
    };
    let mut draft = low_confidence(true);
    let consulted = consult(&ctx, &repo_path, MODIFIED_SOURCE.as_bytes(), &mut draft);
    assert_eq!(consulted.recorded, Some(Appended::Written));
    assert_eq!(consulted.outcome.reading(), "request_refused");

    let observed = observe_commit(Some(&store), &pending, &repo_path, "refactor: split it");
    assert_eq!(observed, Some(Appended::Written));
    assert_eq!(
        observe_commit(Some(&store), &pending, &repo_path, "fix: again"),
        None,
        "a decision pairs with at most one outcome"
    );

    let lines = read_store(&store);
    assert_eq!(lines.len(), 2);
    let decision = assert_record_shape(lines[0].as_bytes());
    let outcome = assert_record_shape(lines[1].as_bytes());
    assert_eq!(decision["record_id"], outcome["record_id"]);
    assert_eq!(decision["lappi"]["kind"], "task_not_trained");
    assert_eq!(outcome["observed"]["type"], "refactor");
}

#[test]
fn recording_off_writes_nothing_even_when_asking() {
    let agent = replying(REFUSED);
    let pending = Mutex::new(Pending::new());
    let ctx = Consult {
        ask: true,
        socket: Some(&agent.path),
        deadline: DEADLINE,
        store: None,
        pending: &pending,
    };
    let mut draft = low_confidence(true);
    let consulted = consult(&ctx, "/repo", MODIFIED_SOURCE.as_bytes(), &mut draft);
    assert_eq!(consulted.recorded, None);
    assert!(pending.lock().expect("pending").is_empty());
}

fn read_store(store: &Store) -> Vec<String> {
    let mut lines = Vec::new();
    let mut files: Vec<_> = std::fs::read_dir(store.dir())
        .expect("store dir")
        .map(|entry| entry.expect("entry").path())
        .collect();
    files.sort();
    for file in files {
        let text = std::fs::read_to_string(file).expect("read");
        lines.extend(text.lines().map(str::to_string));
    }
    lines
}

#[test]
fn the_shapes_match_the_contract_fixture_when_it_is_present() {
    let Some(root) = std::env::var_os("LAPPI_DECISION_DIR") else {
        eprintln!("fixture key-set check NOT RUN: LAPPI_DECISION_DIR is unset");
        return;
    };
    let fixture = Path::new(&root).join("fixtures/caller/valid-records.jsonl");
    let text = std::fs::read_to_string(&fixture)
        .unwrap_or_else(|error| panic!("{} unreadable: {error}", fixture.display()));
    let ours_decision: Value =
        serde_json::from_slice(&decision_for(&every_outcome()[3], ID)).expect("JSON");
    let ours_outcome: Value = serde_json::from_slice(
        &record::outcome_line(ID, "2026-10-06T15:06:00.250Z", "fix(engine): x").expect("line"),
    )
    .expect("JSON");
    let mut matched = 0;
    for line in text
        .lines()
        .filter(|line| line.contains("\"app\":\"gitpulse\""))
    {
        let theirs: Value = serde_json::from_str(line).expect("fixture JSON");
        let ours = if theirs["kind"] == "decision" {
            &ours_decision
        } else {
            &ours_outcome
        };
        assert_eq!(keys(ours), keys(&theirs), "top-level keys");
        assert_eq!(keys(&ours["redaction"]), keys(&theirs["redaction"]));
        if theirs["kind"] == "decision" {
            assert_eq!(keys(&ours["lappi"]), keys(&theirs["lappi"]));
            for key in keys(&theirs["facts"]) {
                assert!(ours["facts"].get(&key).is_some(), "facts.{key}");
            }
        }
        matched += 1;
    }
    assert_eq!(matched, 2, "the fixture's two gitpulse lines");
}

#[test]
fn records_pass_the_contract_checker_binary_when_it_is_available() {
    let _env = collect_env();
    let (_root, store) = held_out_store();
    let mut outcomes = every_outcome();
    // A reply carrying a credential: what the redactor leaves must still pass
    // the checker's own secret screen.
    outcomes.push(client::parse_reply(
        br#"{"status":"ok","schema_version":1,"backend":"ghp_0123456789abcdefghijklmnopqrstuvwxyzA","degraded":false,"slots":{"commit_type":{"value":"fix","conformal_set":null,"score":0.5,"noul":false,"degraded":false}}}"#,
    ));
    for (i, outcome) in outcomes.iter().enumerate() {
        let id = format!("{i:032x}");
        let line = decision_for(outcome, &id);
        assert_eq!(store.append(DAY, &line).expect("append"), Appended::Written);
        let outcome_line =
            record::outcome_line(&id, "2026-10-06T15:06:00.250Z", "feat(ui)!: x").expect("line");
        assert_eq!(
            store.append(DAY, &outcome_line).expect("append"),
            Appended::Written
        );
    }
    let file = store.dir().join(format!("{DAY}.jsonl"));
    for line in read_store(&store) {
        assert_record_shape(line.as_bytes());
    }

    let Some(bin) = std::env::var_os("QD_CALLER_RECORDS_BIN") else {
        eprintln!(
            "cross-check NOT RUN: QD_CALLER_RECORDS_BIN is unset, so {} was not checked by \
             qd-caller-records",
            file.display()
        );
        return;
    };
    use crate::procguard::LockedSpawn as _;
    let output = std::process::Command::new(&bin)
        .arg(&file)
        .output_locked()
        .unwrap_or_else(|error| panic!("{} did not run: {error}", Path::new(&bin).display()));
    let report = String::from_utf8_lossy(&output.stdout);
    assert!(
        output.status.success(),
        "qd-caller-records refused the records ({}):\n{report}\n{}",
        output.status,
        String::from_utf8_lossy(&output.stderr)
    );
    let report: Value = serde_json::from_str(report.trim()).expect("a JSON report");
    eprintln!("cross-check RUN: {report}");
}

/* ── settings ─────────────────────────────────────────────────────────────── */

#[test]
fn both_switches_default_off_and_a_malformed_block_stays_off() {
    let serial = crate::tool_config::lock_config_env();
    let dir = tempfile::tempdir().expect("temp dir");
    let path = dir.path().join("tools.json");
    let _env = crate::test_support::env::bind_env(&serial)
        .set(crate::tool_config::TOOL_CONFIG_ENV, &path)
        .invalidating(crate::tool_config::invalidate_cache);
    crate::tool_config::invalidate_cache();

    let absent = crate::tool_config::lappi_settings();

    std::fs::write(
        &path,
        r#"{"version":1,"devmap":{"binary":"/opt/devmap"},"lappi":{"ask_on_ambiguous_commit_type":"yes"}}"#,
    )
    .expect("write config");
    crate::tool_config::invalidate_cache();
    let malformed = crate::tool_config::lappi_settings();
    let kept = crate::tool_config::load().map(|cfg| cfg.devmap.binary);

    crate::tool_config::set_lappi_settings(crate::tool_config::LappiSettings {
        ask_on_ambiguous_commit_type: true,
        record_caller_data: false,
    })
    .expect("save");
    crate::tool_config::invalidate_cache();
    let saved = crate::tool_config::lappi_settings();

    assert_eq!(absent, crate::tool_config::LappiSettings::default());
    assert!(!absent.ask_on_ambiguous_commit_type && !absent.record_caller_data);
    assert_eq!(malformed, crate::tool_config::LappiSettings::default());
    assert_eq!(
        kept,
        Ok(Some("/opt/devmap".to_string())),
        "other settings survive"
    );
    assert!(saved.ask_on_ambiguous_commit_type && !saved.record_caller_data);
}
