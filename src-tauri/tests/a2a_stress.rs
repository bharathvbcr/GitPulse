//! Stress and gap probes for the agent-to-agent channel (agent_bus + agent_mailbox + the
//! gitpulse_agent_* MCP tools). Each test states the behaviour the channel should have; a
//! failure is a finding, printed with the measured numbers. The handler-level cases run the
//! real gitpulse-mcp binary as separate processes, the way agents reach it, so the file lock
//! (not only the in-process mutex) is contended.

mod common;

use serde_json::Value;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Instant;
use tempfile::TempDir;
use waylode_lib::engine::agent_bus::{post_peer_message, read_messages_for_peer, MessageOrigin};
use waylode_lib::procguard::LockedSpawn;

fn repo() -> TempDir {
    common::unmetered_spawns();
    let dir = tempfile::tempdir().expect("temp repo");
    common::run_git(dir.path(), &["init", "-q", "-b", "main"]);
    common::run_git(dir.path(), &["commit", "--allow-empty", "-q", "-m", "init"]);
    common::trust_repo(dir.path());
    dir
}

/// One gitpulse-mcp call in a child process: (exit code, parsed stdout or the raw text).
fn call(tool: &str, args: &[(&str, String)]) -> (i32, Value, String) {
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_waylode-mcp"));
    cmd.arg("call").arg(tool);
    // The child logs into the hermetic home this process runs under, not the
    // user's real log directory (tests/test_log_isolation.rs).
    let home = PathBuf::from(std::env::var_os("HOME").expect("HOME"));
    cmd.env(waylode_lib::logging::LOG_DIR_ENV, home.join("logs"));
    for (k, v) in args {
        cmd.arg(format!("{k}={v}"));
    }
    cmd.arg("--write").arg("--compact");
    let out = cmd.output_locked().expect("spawn gitpulse-mcp");
    let stdout = String::from_utf8_lossy(&out.stdout).into_owned();
    let stderr = String::from_utf8_lossy(&out.stderr).into_owned();
    let v = serde_json::from_str(stdout.trim()).unwrap_or(Value::Null);
    (
        out.status.code().unwrap_or(-1),
        v,
        format!("{stdout}{stderr}"),
    )
}

/// Why a call failed: its exit code and its output without the `WARN [...]`
/// log lines a Windows child prints first. Filtering only `[setup]` left a
/// `[spawn_gate]` summary in the 300 characters shown, so the actual error
/// still went unread; when nothing but log lines was printed, those are shown.
fn failure_reason(code: i32, raw: &str) -> String {
    let lines: Vec<&str> = raw
        .lines()
        .filter(|line| !line.contains(" WARN ["))
        .collect();
    let shown = if lines.is_empty() {
        raw.to_owned()
    } else {
        lines.join(" | ")
    };
    format!(
        "exit {code}: {}",
        shown.chars().take(300).collect::<String>()
    )
}

fn s(x: impl ToString) -> String {
    x.to_string()
}

fn register(repo: &str, sid: &str, role: &str) {
    let (code, v, raw) = call(
        "gitpulse_agent_register",
        &[
            ("repo_path", s(repo)),
            ("provider", s("agy")),
            ("surface", s("terminal")),
            ("provider_session_id", s(sid)),
            ("role", s(role)),
            ("pid", s(std::process::id())),
        ],
    );
    assert_eq!(code, 0, "register {role}: {raw}");
    assert_eq!(v["ok"], true, "register {role}: {raw}");
}

fn send(
    repo: &str,
    from: &str,
    to: &str,
    body: &str,
    extra: &[(&str, String)],
) -> (i32, Value, String) {
    let mut args = vec![
        ("repo_path", s(repo)),
        ("from_peer", s(from)),
        ("to_peer", s(to)),
        ("body", s(body)),
    ];
    args.extend(extra.iter().cloned());
    call("gitpulse_agent_send", &args)
}

fn pending_in_mailbox(sid: &str) -> usize {
    let home = PathBuf::from(std::env::var_os("HOME").expect("HOME"));
    fn find(dir: &Path, depth: usize) -> Option<PathBuf> {
        if depth > 6 {
            return None;
        }
        for e in std::fs::read_dir(dir).ok()?.flatten() {
            let p = e.path();
            if p.is_dir() {
                if p.file_name().is_some_and(|n| n == "agent-mailbox") {
                    return Some(p);
                }
                if let Some(f) = find(&p, depth + 1) {
                    return Some(f);
                }
            }
        }
        None
    }
    let Some(root) = find(&home, 0) else { return 0 };
    std::fs::read_dir(root.join(sid))
        .map(|rd| {
            rd.flatten()
                .filter(|e| e.file_name().to_string_lossy().starts_with("p-"))
                .count()
        })
        .unwrap_or(0)
}

fn mailbox_text(sid: &str) -> String {
    let home = PathBuf::from(std::env::var_os("HOME").expect("HOME"));
    let mut out = String::new();
    let mut stack = vec![(home, 0usize)];
    while let Some((d, depth)) = stack.pop() {
        let Ok(rd) = std::fs::read_dir(&d) else {
            continue;
        };
        for e in rd.flatten() {
            let p = e.path();
            if p.is_dir() {
                if depth < 8 {
                    stack.push((p, depth + 1));
                }
            } else if p
                .parent()
                .and_then(|x| x.file_name())
                .is_some_and(|n| n == sid)
                && e.file_name().to_string_lossy().starts_with("p-")
            {
                out.push_str(&std::fs::read_to_string(&p).unwrap_or_default());
            }
        }
    }
    out
}

fn post(repo: &str, from: &str, to: Option<&str>, body: &str) -> Result<u64, String> {
    post_peer_message(
        repo,
        MessageOrigin::Agent,
        from,
        to,
        None,
        "note",
        body,
        None,
        None,
        1,
        6,
    )
    .map(|m| m.seq)
    .map_err(|e| e.message().to_string())
}

/// The body cap counts the body as stored (JSON-escaped), so no run of accepted posts can push
/// the bus past its read cap. Oversized bodies are refused at the boundary, naming the size,
/// and the bus stays usable throughout.
#[test]
fn escape_heavy_bodies_are_refused_at_the_boundary_and_never_brick_the_bus() {
    let dir = repo();
    let r = dir.path().to_str().unwrap();
    // 16 KiB of quotes is 32 KiB stored: refused. 8000 quotes is under 16 KiB stored: accepted.
    let too_big = "\"".repeat(16 * 1024);
    let fits = "\"".repeat(8000);
    let mut refusals = 0;
    for i in 0..300 {
        let body = if i % 2 == 0 { &too_big } else { &fits };
        match post(r, "claude:src", Some("claude:sink"), body) {
            Ok(_) => assert!(i % 2 == 1, "a 32 KiB-stored body was accepted at post {i}"),
            Err(e) => {
                assert!(
                    i % 2 == 0,
                    "a body under the stored cap was refused at post {i}: {e}"
                );
                assert!(e.contains("bytes"), "the refusal names the size: {e}");
                refusals += 1;
            }
        }
    }
    let size = std::fs::metadata(dir.path().join(".git/gitpulse/lanes/messages.json"))
        .map(|m| m.len())
        .unwrap_or(0);
    let after = post(r, "claude:src", Some("claude:sink"), "a short follow-up");
    let read = read_messages_for_peer(r, &["claude:sink".into()], 0, None, false, true)
        .map(|p| p.messages.len());
    eprintln!("[quote] refusals {refusals}; messages.json = {size} bytes; short post after: {after:?}; reader ok: {}", read.is_ok());
    assert_eq!(refusals, 150);
    assert!(
        size <= 8 * 1024 * 1024,
        "the bus grew past its read cap: {size} bytes"
    );
    assert!(after.is_ok() && read.is_ok(), "the bus stays usable");
}

/// A bus file already over its cap (written by an older build, or by hand) heals on the next
/// write: the oldest rows go, loudly, and the sequence never rewinds, because readers page by it.
#[test]
fn an_over_cap_bus_heals_on_the_next_post_without_rewinding_the_sequence() {
    let dir = repo();
    let r = dir.path().to_str().unwrap();
    let lanes = dir.path().join(".git/gitpulse/lanes");
    std::fs::create_dir_all(&lanes).unwrap();
    let big = "a".repeat(30 * 1024);
    let messages: Vec<Value> = (1..=300u64)
        .map(|seq| serde_json::json!({
            "seq": seq, "from_agent": "claude:old", "to_agent": "claude:sink", "kind": "note",
            "body": big, "branch": null, "created_at_ms": 1_795_000_000_000u64 + seq,
            "thread_key": null, "msg_id": format!("old-{seq}"), "read_by": [], "hop": 1, "max_hops": 6
        }))
        .collect();
    let state =
        serde_json::json!({ "next_seq": 300, "dropped_before_seq": 0, "messages": messages });
    std::fs::write(
        lanes.join("messages.json"),
        serde_json::to_vec_pretty(&state).unwrap(),
    )
    .unwrap();
    let before = std::fs::metadata(lanes.join("messages.json"))
        .unwrap()
        .len();
    assert!(
        before > 8 * 1024 * 1024,
        "fixture must start over the cap ({before})"
    );
    let seq = post(r, "claude:new", Some("claude:sink"), "after the heal");
    let size = std::fs::metadata(lanes.join("messages.json"))
        .unwrap()
        .len();
    let page = read_messages_for_peer(r, &["claude:sink".into()], 0, None, false, true);
    eprintln!(
        "[heal] post -> {seq:?}; {before} -> {size} bytes; dropped_before_seq {:?}",
        page.as_ref().map(|p| p.dropped_before_seq)
    );
    assert_eq!(seq, Ok(301), "the next post continues the sequence");
    assert!(
        size <= 8 * 1024 * 1024,
        "the healed bus is under its cap: {size}"
    );
    let page = page.expect("the healed bus reads");
    assert!(
        page.dropped_before_seq > 1,
        "the dropped rows are reported through dropped_before_seq"
    );
    assert!(
        page.messages.iter().any(|m| m.seq == 301),
        "the new row is readable"
    );
}

/// An unread direct message must not be evicted by other agents' traffic.
#[test]
fn an_unread_direct_message_survives_other_agents_traffic() {
    let dir = repo();
    let r = dir.path().to_str().unwrap();
    post(
        r,
        "claude:boss",
        Some("claude:quiet"),
        "please review src/auth before you merge",
    )
    .unwrap();
    for i in 0..300 {
        post(
            r,
            "claude:chatty",
            Some("claude:other"),
            &format!("status update {i}"),
        )
        .unwrap();
    }
    let page = read_messages_for_peer(r, &["claude:quiet".into()], 0, None, false, true).unwrap();
    eprintln!(
        "quiet peer unread = {}, dropped_before_seq = {}",
        page.messages.len(),
        page.dropped_before_seq
    );
    assert_eq!(
        page.messages.len(),
        1,
        "the quiet peer's only message was evicted by another pair's chatter"
    );
}

/// The mailbox is what wakes the recipient, so a send whose envelope was not deposited must say
/// so (delivery_outcome "bus_only", mailbox.deposited false, mailbox.reason), and a full mailbox
/// keeps its last slot for one notice telling the recipient that more is waiting on the bus.
#[test]
fn a_send_whose_mailbox_deposit_fails_says_so() {
    let dir = repo();
    let r = dir.path().to_str().unwrap();
    let sid = "77777777-7777-4777-8777-777777777771";
    register(r, sid, "reviewer");
    let mut rows = Vec::new();
    for i in 0..10 {
        let (code, v, raw) = send(r, "claude:coder", sid, &format!("finding {i}"), &[]);
        assert_eq!(code, 0, "send {i}: {raw}");
        rows.push((
            v["delivery_outcome"].as_str().unwrap_or("?").to_string(),
            v["mailbox"]["deposited"].as_bool(),
            v["mailbox"]["reason"].as_str().map(str::to_string),
        ));
    }
    let pending = pending_in_mailbox(sid);
    eprintln!("rows = {rows:?}; envelopes pending = {pending}");
    for (i, row) in rows.iter().enumerate() {
        if i < 7 {
            assert_eq!(row.1, Some(true), "send {i} should deposit: {row:?}");
            assert_ne!(row.0, "bus_only", "send {i}: {row:?}");
        } else {
            assert_eq!(
                row.0, "bus_only",
                "send {i} did not reach the mailbox and must say so: {row:?}"
            );
            assert_eq!(row.1, Some(false), "send {i}: {row:?}");
            assert_eq!(row.2.as_deref(), Some("full"), "send {i}: {row:?}");
        }
    }
    assert_eq!(pending, 8, "seven peer envelopes plus one overflow notice");
    let notice = mailbox_text(sid);
    assert!(
        notice.contains("gitpulse_agent_inbox"),
        "the overflow notice tells the recipient how to read the rest"
    );
    // The inbox needs for_peer, and a read only peeks: the notice must give both.
    assert!(
        notice.contains("for_peer") && notice.contains(sid),
        "the overflow notice names for_peer with this session's id: {notice}"
    );
    assert!(
        notice.contains("ack"),
        "the overflow notice says how to acknowledge what it reads: {notice}"
    );
}

/// The hop ceiling exists to stop agent ping-pong. A reply names what it answers (in_reply_to,
/// or a thread), the count climbs from that, and a caller's explicit hop can raise it but never
/// lower it.
#[test]
fn the_hop_ceiling_engages_on_a_reply_chain() {
    let dir = repo();
    let r = dir.path().to_str().unwrap();
    register(r, "77777777-7777-4777-8777-777777777772", "ping");
    register(r, "77777777-7777-4777-8777-777777777773", "pong");
    let mut threaded = Vec::new();
    for i in 0..8 {
        let (to, from) = if i % 2 == 0 {
            ("pong", "agy:ping")
        } else {
            ("ping", "agy:pong")
        };
        let (_, v, _) = send(r, from, to, &format!("t{i}"), &[("thread_key", s("loop"))]);
        threaded.push((
            v["hop"].as_u64(),
            v["delivery_outcome"].as_str().map(str::to_string),
        ));
    }
    let (_, reset, _) = send(
        r,
        "agy:ping",
        "pong",
        "reset",
        &[("thread_key", s("loop")), ("hop", s(1))],
    );
    let mut chain = Vec::new();
    let mut prev: Option<String> = None;
    for i in 0..8 {
        let (to, from) = if i % 2 == 0 {
            ("pong", "agy:ping")
        } else {
            ("ping", "agy:pong")
        };
        let mut extra = Vec::new();
        if let Some(ref id) = prev {
            extra.push(("in_reply_to", id.clone()));
        }
        let (code, v, raw) = send(r, from, to, &format!("c{i}"), &extra);
        assert_eq!(code, 0, "chain {i}: {raw}");
        prev = v["msg_id"].as_str().map(str::to_string);
        chain.push((
            v["hop"].as_u64(),
            v["delivery_outcome"].as_str().map(str::to_string),
        ));
    }
    eprintln!("threaded = {threaded:?}\nexplicit hop=1 on that thread -> hop {} outcome {}\nin_reply_to chain = {chain:?}", reset["hop"], reset["delivery_outcome"]);
    assert_eq!(
        threaded.iter().map(|t| t.0).collect::<Vec<_>>(),
        (1..=8).map(Some).collect::<Vec<_>>()
    );
    assert!(
        reset["hop"].as_u64().unwrap_or(0) > 8,
        "a caller lowered the thread's hop count: {}",
        reset["hop"]
    );
    assert_eq!(
        chain.iter().map(|c| c.0).collect::<Vec<_>>(),
        (1..=8).map(Some).collect::<Vec<_>>(),
        "in_reply_to carries the count"
    );
    assert_eq!(
        chain[6].1.as_deref(),
        Some("paused_hop_limit"),
        "the ceiling engages on an unthreaded reply chain"
    );
}

/// A reader naming another peer, with the default ack, must not consume that peer's messages.
#[test]
fn reading_someone_elses_inbox_does_not_consume_it() {
    let dir = repo();
    let r = dir.path().to_str().unwrap();
    let sid = "77777777-7777-4777-8777-777777777774";
    register(r, sid, "owner");
    let (c, _, raw) = send(r, "claude:a", "owner", "for the owner only", &[]);
    assert_eq!(c, 0, "{raw}");
    let (_, other, _) = call(
        "gitpulse_agent_inbox",
        &[("repo_path", s(r)), ("for_peer", s("owner"))],
    );
    let (_, mine, _) = call(
        "gitpulse_agent_inbox",
        &[
            ("repo_path", s(r)),
            ("for_peer", s("owner")),
            ("provider_session_id", s(sid)),
            ("ack", s(false)),
        ],
    );
    let other_n = other["messages"].as_array().map_or(0, Vec::len);
    let mine_n = mine["messages"].as_array().map_or(0, Vec::len);
    eprintln!(
        "another caller read {other_n} (acked_count {}); the owner then sees {mine_n}",
        other["acked_count"]
    );
    assert_eq!(
        mine_n, 1,
        "a read by another caller consumed the owner's message"
    );
}

/// A reply must be able to reach the sender of the message it answers.
#[test]
fn a_recipient_can_reply_to_whoever_wrote_to_it() {
    let dir = repo();
    let r = dir.path().to_str().unwrap();
    register(r, "77777777-7777-4777-8777-777777777775", "target");
    let (c, v, raw) = send(r, "claude:never-registered", "target", "question?", &[]);
    assert_eq!(c, 0, "{raw}");
    let from = v["message"]["from_agent"].as_str().unwrap().to_string();
    let (code, rv, reply) = send(r, "agy:target", &from, "answer", &[]);
    eprintln!("reply to {from}: exit {code}: {}", reply.trim());
    assert_eq!(
        code, 0,
        "the recipient cannot address a reply to the sender it was handed"
    );
    assert_eq!(
        rv["delivery_outcome"], "bus_only",
        "a reply to an unregistered sender reaches the bus only: {reply}"
    );
    let page =
        read_messages_for_peer(r, std::slice::from_ref(&from), 0, None, false, true).unwrap();
    assert!(
        page.messages.iter().any(|m| m.body == "answer"),
        "the reply is on the bus, addressed to {from}"
    );
}

/// Many agents sending at once, each its own process: nothing lost, duplicated or corrupted.
#[test]
fn concurrent_senders_in_separate_processes_lose_nothing() {
    let dir = repo();
    let r = dir.path().to_str().unwrap().to_string();
    register(&r, "77777777-7777-4777-8777-777777777776", "hub");
    let procs = 8usize;
    let each = 25usize;
    let start = Instant::now();
    let handles: Vec<_> = (0..procs)
        .map(|p| {
            let r = r.clone();
            std::thread::spawn(move || {
                let mut res = Vec::new();
                for i in 0..each {
                    let t = Instant::now();
                    let (code, v, raw) = send(
                        &r,
                        &format!("claude:w{p}"),
                        "hub",
                        &format!("w{p}-{i}"),
                        &[],
                    );
                    res.push((
                        code,
                        v["message"]["seq"].as_u64(),
                        t.elapsed().as_millis(),
                        if code == 0 {
                            String::new()
                        } else {
                            failure_reason(code, &raw)
                        },
                    ));
                }
                res
            })
        })
        .collect();
    let all: Vec<_> = handles
        .into_iter()
        .flat_map(|h| h.join().unwrap())
        .collect();
    let wall = start.elapsed();
    let failures: Vec<_> = all
        .iter()
        .filter(|x| x.0 != 0)
        .map(|x| x.3.chars().take(200).collect::<String>())
        .collect();
    let mut seqs: Vec<u64> = all.iter().filter_map(|x| x.1).collect();
    seqs.sort_unstable();
    let unique = {
        let mut u = seqs.clone();
        u.dedup();
        u.len()
    };
    let mut lat: Vec<u128> = all.iter().map(|x| x.2).collect();
    lat.sort_unstable();
    let pct = |q: f64| lat[((lat.len() as f64 - 1.0) * q) as usize];
    let stored = read_messages_for_peer(&r, &[], 0, None, true, false).map(|p| p.messages.len());
    eprintln!(
        "{} sends from {procs} processes in {:.1}s; failures {}; unique seqs {unique}; stored rows {stored:?}; latency ms p50 {} p90 {} max {}; first failures {:?}",
        all.len(), wall.as_secs_f64(), failures.len(), pct(0.5), pct(0.9), lat[lat.len() - 1], failures.iter().take(3).collect::<Vec<_>>()
    );
    // A send the policy gate refused because the spawn gate deferred its
    // binding lookup twice (one bounded retry, then fail closed) was told so,
    // with a typed, retryable reason: nothing was examined and nothing was
    // stored. That is the gate's designed answer on a saturated host (1 of 200
    // on Windows CI), so it is allowed, and bounded. Any other refusal is not.
    let (deferred, other): (Vec<_>, Vec<_>) = failures
        .iter()
        .partition(|reason| reason.contains("[deferred_worktree]"));
    assert!(
        other.is_empty(),
        "{} of {} sends failed for a reason other than a deferred lookup: {other:?}",
        other.len(),
        all.len()
    );
    assert!(
        deferred.len() * 20 <= all.len(),
        "{} of {} sends were deferred past the retry: more than 1 in 20",
        deferred.len(),
        all.len()
    );
    let accepted = all.len() - failures.len();
    assert_eq!(
        seqs.len(),
        accepted,
        "every accepted send reported its sequence number"
    );
    assert_eq!(unique, accepted, "sequence numbers were duplicated or lost");
    assert_eq!(
        stored,
        Ok(accepted),
        "the bus holds exactly the accepted sends"
    );
}

/// Control bytes other than newline and tab are refused at the boundary, by the same rule the
/// mailbox applies, so a body the bus accepts is always one the mailbox can carry.
#[test]
fn control_byte_bodies_are_refused_and_never_brick_the_bus() {
    let dir = repo();
    let r = dir.path().to_str().unwrap();
    let refused = post(
        r,
        "claude:src",
        Some("claude:sink"),
        &"\u{1}".repeat(16 * 1024),
    );
    let tabbed = post(r, "claude:src", Some("claude:sink"), "a\tb\nc");
    eprintln!("[control] {refused:?}; tab/newline {tabbed:?}");
    assert!(
        refused.as_ref().is_err_and(|e| e.contains("control")),
        "{refused:?}"
    );
    assert!(tabbed.is_ok(), "newline and tab stay allowed: {tabbed:?}");
}

/// A terminal escape is refused at send, loudly, instead of being stored on the bus and
/// silently skipped by the mailbox.
#[test]
fn a_body_the_mailbox_would_refuse_is_refused_at_send() {
    let dir = repo();
    let r = dir.path().to_str().unwrap();
    let sid = "77777777-7777-4777-8777-777777777777";
    register(r, sid, "colour");
    let (code, _, raw) = send(
        r,
        "claude:coder",
        "colour",
        "build is \u{1b}[31mred\u{1b}[0m",
        &[],
    );
    let pending = pending_in_mailbox(sid);
    eprintln!("exit {code}; envelopes pending {pending}; {}", raw.trim());
    assert_ne!(code, 0, "the send must be refused: {raw}");
    assert!(
        raw.contains("control"),
        "the refusal names the cause: {raw}"
    );
    assert_eq!(pending, 0);
}

/// The same concurrency, with a bus near its size: every write rewrites the whole file under
/// one lock, so the hold time grows with the bus.
#[test]
fn concurrent_large_senders_lose_nothing() {
    let dir = repo();
    let r = dir.path().to_str().unwrap().to_string();
    register(&r, "77777777-7777-4777-8777-777777777778", "bighub");
    let procs = 8usize;
    let each = 30usize;
    let start = Instant::now();
    let handles: Vec<_> = (0..procs)
        .map(|p| {
            let r = r.clone();
            std::thread::spawn(move || {
                (0..each)
                    .map(|i| {
                        let body = format!("w{p}-{i} ") + &"x".repeat(12 * 1024);
                        let t = Instant::now();
                        let (code, _, raw) =
                            send(&r, &format!("claude:big{p}"), "bighub", &body, &[]);
                        (
                            code,
                            t.elapsed().as_millis(),
                            if code == 0 {
                                String::new()
                            } else {
                                failure_reason(code, &raw)
                            },
                        )
                    })
                    .collect::<Vec<_>>()
            })
        })
        .collect();
    let all: Vec<_> = handles
        .into_iter()
        .flat_map(|h| h.join().unwrap())
        .collect();
    let fails: Vec<_> = all
        .iter()
        .filter(|x| x.0 != 0)
        .map(|x| x.2.clone())
        .collect();
    let mut lat: Vec<u128> = all.iter().map(|x| x.1).collect();
    lat.sort_unstable();
    let size = std::fs::metadata(dir.path().join(".git/gitpulse/lanes/messages.json"))
        .map(|m| m.len())
        .unwrap_or(0);
    eprintln!(
        "{} large sends in {:.1}s; failures {}; bus {} bytes; latency ms p50 {} p90 {} max {}; first {:?}",
        all.len(), start.elapsed().as_secs_f64(), fails.len(), size,
        lat[lat.len() / 2], lat[lat.len() * 9 / 10], lat[lat.len() - 1], fails.first()
    );
    assert!(
        fails.is_empty(),
        "{} of {} large sends failed",
        fails.len(),
        all.len()
    );
}
