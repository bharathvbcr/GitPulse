//! Stress and gap probes for the agent-to-agent channel (agent_bus + agent_mailbox + the
//! gitpulse_agent_* MCP tools). Each test states the behaviour the channel should have; a
//! failure is a finding, printed with the measured numbers. The handler-level cases run the
//! real gitpulse-mcp binary as separate processes, the way agents reach it, so the file lock
//! (not only the in-process mutex) is contended.

mod common;

use gitpulse_lib::engine::agent_bus::{post_peer_message, read_messages_for_peer, MessageOrigin};
use gitpulse_lib::procguard::LockedSpawn;
use serde_json::Value;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Instant;
use tempfile::TempDir;

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
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_gitpulse-mcp"));
    cmd.arg("call").arg(tool);
    for (k, v) in args {
        cmd.arg(format!("{k}={v}"));
    }
    cmd.arg("--write").arg("--compact");
    let out = cmd.output_locked().expect("spawn gitpulse-mcp");
    let stdout = String::from_utf8_lossy(&out.stdout).into_owned();
    let stderr = String::from_utf8_lossy(&out.stderr).into_owned();
    let v = serde_json::from_str(stdout.trim()).unwrap_or(Value::Null);
    (out.status.code().unwrap_or(-1), v, format!("{stdout}{stderr}"))
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

fn send(repo: &str, from: &str, to: &str, body: &str, extra: &[(&str, String)]) -> (i32, Value, String) {
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

fn post(repo: &str, from: &str, to: Option<&str>, body: &str) -> Result<u64, String> {
    post_peer_message(repo, MessageOrigin::Agent, from, to, None, "note", body, None, None, 1, 6)
        .map(|m| m.seq)
        .map_err(|e| e.message().to_string())
}

/// A body within the 16 KiB cap must never make the bus unusable. The cap counts raw bytes;
/// the 8 MiB read cap counts the pretty-printed JSON, where a quote or a control byte grows.
#[test]
fn escape_heavy_bodies_within_the_body_cap_never_brick_the_bus() {
    for (label, unit) in [("quote", "\""), ("control_0x01", "\u{1}")] {
        let dir = repo();
        let r = dir.path().to_str().unwrap();
        let body = unit.repeat(16 * 1024 / unit.len());
        let mut first_err = None;
        for i in 0..300 {
            if let Err(e) = post(r, "claude:src", Some("claude:sink"), &body) {
                first_err = Some((i, e));
                break;
            }
        }
        let size = std::fs::metadata(dir.path().join(".git/gitpulse/lanes/messages.json")).map(|m| m.len()).unwrap_or(0);
        let after = post(r, "claude:src", Some("claude:sink"), "a short follow-up");
        let read = read_messages_for_peer(r, &["claude:sink".into()], 0, None, false, true).map(|p| p.messages.len());
        eprintln!("[{label}] first failure: {first_err:?}; messages.json = {size} bytes; short post after: {after:?}; reader: {:?}", read.as_ref().map_err(|e| e.message().to_string()));
        assert!(first_err.is_none(), "[{label}] bus refused post #{:?} ({size} bytes on disk)", first_err.map(|f| f.0));
        assert!(after.is_ok() && read.is_ok(), "[{label}] the bus stays usable");
    }
}

/// An unread direct message must not be evicted by other agents' traffic.
#[test]
fn an_unread_direct_message_survives_other_agents_traffic() {
    let dir = repo();
    let r = dir.path().to_str().unwrap();
    post(r, "claude:boss", Some("claude:quiet"), "please review src/auth before you merge").unwrap();
    for i in 0..300 {
        post(r, "claude:chatty", Some("claude:other"), &format!("status update {i}")).unwrap();
    }
    let page = read_messages_for_peer(r, &["claude:quiet".into()], 0, None, false, true).unwrap();
    eprintln!("quiet peer unread = {}, dropped_before_seq = {}", page.messages.len(), page.dropped_before_seq);
    assert_eq!(page.messages.len(), 1, "the quiet peer's only message was evicted by another pair's chatter");
}

/// When the recipient's mailbox refuses the envelope (full, or text it will not carry), the
/// sender must be told: the mailbox is what wakes the recipient.
#[test]
fn a_send_whose_mailbox_deposit_fails_says_so() {
    let dir = repo();
    let r = dir.path().to_str().unwrap();
    let sid = "77777777-7777-4777-8777-777777777771";
    register(r, sid, "reviewer");
    let mut outcomes = Vec::new();
    for i in 0..10 {
        let (code, v, raw) = send(r, "claude:coder", sid, &format!("finding {i}"), &[]);
        assert_eq!(code, 0, "send {i}: {raw}");
        outcomes.push(v["delivery_outcome"].as_str().unwrap_or("?").to_string());
    }
    let pending = pending_in_mailbox(sid);
    let (_, ctl, _) = send(r, "claude:coder", "reviewer", "colour \u{1b}[31mred\u{1b}[0m", &[]);
    eprintln!("outcomes = {outcomes:?}; envelopes pending = {pending}; control-char send = {}", ctl["delivery_outcome"]);
    assert_eq!(pending, 10, "only {pending} of 10 sends reached the mailbox, and every send reported {outcomes:?}");
}

/// The hop ceiling exists to stop agent ping-pong. It should engage on a reply chain without
/// each agent having to thread or count correctly, and a caller should not be able to lower it.
#[test]
fn the_hop_ceiling_engages_on_a_reply_chain() {
    let dir = repo();
    let r = dir.path().to_str().unwrap();
    register(r, "77777777-7777-4777-8777-777777777772", "ping");
    register(r, "77777777-7777-4777-8777-777777777773", "pong");
    // A threaded chain: hops should climb to the ceiling.
    let mut threaded = Vec::new();
    for i in 0..8 {
        let (to, from) = if i % 2 == 0 { ("pong", "agy:ping") } else { ("ping", "agy:pong") };
        let (_, v, raw) = send(r, from, to, &format!("t{i}"), &[("thread_key", s("loop"))]);
        threaded.push((v["hop"].as_u64(), v["delivery_outcome"].as_str().map(str::to_string), raw.len()));
    }
    // The same thread, the caller naming hop=1 again.
    let (_, reset, _) = send(r, "agy:ping", "pong", "reset", &[("thread_key", s("loop")), ("hop", s(1))]);
    // An unthreaded chain.
    let mut bare = Vec::new();
    for i in 0..8 {
        let (to, from) = if i % 2 == 0 { ("pong", "agy:ping") } else { ("ping", "agy:pong") };
        let (_, v, _) = send(r, from, to, &format!("b{i}"), &[]);
        bare.push(v["hop"].as_u64());
    }
    eprintln!("threaded = {threaded:?}\nexplicit hop=1 on that thread -> hop {} outcome {}\nunthreaded hops = {bare:?}", reset["hop"], reset["delivery_outcome"]);
    assert_ne!(reset["hop"].as_u64(), Some(1), "a caller reset the thread's hop count to 1");
    assert!(bare.iter().any(|h| h.unwrap_or(0) >= 6), "an unthreaded 8-message exchange never left hop 1: {bare:?}");
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
    let (_, other, _) = call("gitpulse_agent_inbox", &[("repo_path", s(r)), ("for_peer", s("owner"))]);
    let (_, mine, _) = call(
        "gitpulse_agent_inbox",
        &[("repo_path", s(r)), ("for_peer", s("owner")), ("provider_session_id", s(sid)), ("ack", s(false))],
    );
    let other_n = other["messages"].as_array().map_or(0, Vec::len);
    let mine_n = mine["messages"].as_array().map_or(0, Vec::len);
    eprintln!("another caller read {other_n} (acked_count {}); the owner then sees {mine_n}", other["acked_count"]);
    assert_eq!(mine_n, 1, "a read by another caller consumed the owner's message");
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
    let (code, _, reply) = send(r, "agy:target", &from, "answer", &[]);
    eprintln!("reply to {from}: exit {code}: {}", reply.trim());
    assert_eq!(code, 0, "the recipient cannot address a reply to the sender it was handed");
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
                    let (code, v, raw) = send(&r, &format!("claude:w{p}"), "hub", &format!("w{p}-{i}"), &[]);
                    res.push((code, v["message"]["seq"].as_u64(), t.elapsed().as_millis(), if code == 0 { String::new() } else { raw }));
                }
                res
            })
        })
        .collect();
    let all: Vec<_> = handles.into_iter().flat_map(|h| h.join().unwrap()).collect();
    let wall = start.elapsed();
    let failures: Vec<_> = all.iter().filter(|x| x.0 != 0).map(|x| x.3.chars().take(200).collect::<String>()).collect();
    let mut seqs: Vec<u64> = all.iter().filter_map(|x| x.1).collect();
    seqs.sort_unstable();
    let unique = { let mut u = seqs.clone(); u.dedup(); u.len() };
    let mut lat: Vec<u128> = all.iter().map(|x| x.2).collect();
    lat.sort_unstable();
    let pct = |q: f64| lat[((lat.len() as f64 - 1.0) * q) as usize];
    let stored = read_messages_for_peer(&r, &[], 0, None, true, false).map(|p| p.messages.len());
    eprintln!(
        "{} sends from {procs} processes in {:.1}s; failures {}; unique seqs {unique}; stored rows {stored:?}; latency ms p50 {} p90 {} max {}; first failures {:?}",
        all.len(), wall.as_secs_f64(), failures.len(), pct(0.5), pct(0.9), lat[lat.len() - 1], failures.iter().take(3).collect::<Vec<_>>()
    );
    assert!(failures.is_empty(), "{} of {} sends failed", failures.len(), all.len());
    assert_eq!(unique, procs * each, "sequence numbers were duplicated or lost");
}


/// Control bytes escape to six bytes each, so a much smaller run reaches the cap.
#[test]
fn control_byte_bodies_never_brick_the_bus() {
    let dir = repo();
    let r = dir.path().to_str().unwrap();
    let body = "\u{1}".repeat(16 * 1024);
    let mut first_err = None;
    for i in 0..300 {
        if let Err(e) = post(r, "claude:src", Some("claude:sink"), &body) {
            first_err = Some((i, e.chars().take(80).collect::<String>()));
            break;
        }
    }
    eprintln!("[control] first failure: {first_err:?}");
    assert!(first_err.is_none(), "bus refused post #{:?}", first_err.map(|f| f.0));
}

/// A body the bus accepts but the mailbox refuses (a terminal escape) still has to reach,
/// or be reported as not reaching, an empty mailbox.
#[test]
fn a_body_the_mailbox_refuses_is_reported() {
    let dir = repo();
    let r = dir.path().to_str().unwrap();
    let sid = "77777777-7777-4777-8777-777777777777";
    register(r, sid, "colour");
    let (code, v, raw) = send(r, "claude:coder", "colour", "build is \u{1b}[31mred\u{1b}[0m", &[]);
    let pending = pending_in_mailbox(sid);
    eprintln!("exit {code}; outcome {}; envelopes pending {pending}", v["delivery_outcome"]);
    assert_eq!(code, 0, "{raw}");
    assert_eq!(pending, 1, "the send reported {} but deposited nothing", v["delivery_outcome"]);
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
                        let (code, _, raw) = send(&r, &format!("claude:big{p}"), "bighub", &body, &[]);
                        (code, t.elapsed().as_millis(), raw.chars().take(160).collect::<String>())
                    })
                    .collect::<Vec<_>>()
            })
        })
        .collect();
    let all: Vec<_> = handles.into_iter().flat_map(|h| h.join().unwrap()).collect();
    let fails: Vec<_> = all.iter().filter(|x| x.0 != 0).map(|x| x.2.clone()).collect();
    let mut lat: Vec<u128> = all.iter().map(|x| x.1).collect();
    lat.sort_unstable();
    let size = std::fs::metadata(dir.path().join(".git/gitpulse/lanes/messages.json")).map(|m| m.len()).unwrap_or(0);
    eprintln!(
        "{} large sends in {:.1}s; failures {}; bus {} bytes; latency ms p50 {} p90 {} max {}; first {:?}",
        all.len(), start.elapsed().as_secs_f64(), fails.len(), size,
        lat[lat.len() / 2], lat[lat.len() * 9 / 10], lat[lat.len() - 1], fails.first()
    );
    assert!(fails.is_empty(), "{} of {} large sends failed", fails.len(), all.len());
}
