use super::a2a::{
    clamp_wait_timeout_ms, handle_agent_wait_with_counter, WaitCallSlot, MAX_WAIT_CALLS,
    MAX_WAIT_TIMEOUT_MS,
};
use super::{handle_tool_call, AgentWaitArgs};
use crate::agent_mailbox::{Delivery, Mailbox};
use serde_json::json;
use std::sync::atomic::AtomicUsize;
use std::time::{Duration, Instant, SystemTime};

fn trusted_git_repo() -> (tempfile::TempDir, String) {
    use crate::procguard::LockedSpawn;
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().to_string_lossy().into_owned();
    let run = |args: &[&str]| {
        let out = std::process::Command::new("git")
            .args(args)
            .output_locked()
            .expect("git");
        assert!(
            out.status.success(),
            "git {args:?} failed: {}",
            String::from_utf8_lossy(&out.stderr)
        );
    };
    run(&["init", "-q", "-b", "main", &path]);
    run(&["-C", &path, "config", "user.name", "A2A Test"]);
    run(&["-C", &path, "config", "user.email", "a2a@example.com"]);
    run(&[
        "-C",
        &path,
        "commit",
        "-q",
        "--allow-empty",
        "-m",
        "initial commit",
    ]);
    crate::test_support::trust_repo(dir.path());
    (dir, path)
}

#[test]
fn register_and_peers_reconcile_live_and_exited_sessions() {
    let (_dir, repo) = trusted_git_repo();
    let live_sid = "11111111-2222-4333-8444-555555555555";
    let dead_sid = "aaaaaaaa-bbbb-4ccc-8ddd-eeeeeeeeeeee";

    let reg_live = handle_tool_call(
        "gitpulse_agent_register",
        &json!({
            "repo_path": repo,
            "provider": "agy",
            "surface": "terminal",
            "provider_session_id": live_sid,
            "terminal_session_id": "term-live-1",
            "role": "implementer",
            "wake_mode": "auto",
            "pid": std::process::id(),
        }),
    )
    .expect("register live peer");
    assert_eq!(reg_live["ok"], true);
    assert_eq!(reg_live["peer"]["auto_wake"], true);
    assert_eq!(reg_live["peer"]["role"], "implementer");

    // Updating role/wake_mode without repeating `provider` infers it from the live session.
    let updated_live = handle_tool_call(
        "gitpulse_agent_register",
        &json!({
            "repo_path": repo,
            "role": "lead-implementer",
            "wake_mode": "boundary",
        }),
    )
    .expect("update live peer without explicit provider");
    assert_eq!(updated_live["peer"]["provider"], "agy");
    assert_eq!(updated_live["peer"]["role"], "lead-implementer");
    assert_eq!(updated_live["peer"]["auto_wake"], false);

    // Register a dead PID (u32::MAX - 1) with no saved transcript -> exited.
    handle_tool_call(
        "gitpulse_agent_register",
        &json!({
            "repo_path": repo,
            "provider": "claude",
            "surface": "external_cli",
            "provider_session_id": dead_sid,
            "role": "stale-reviewer",
            "pid": 4_000_000_000u32,
        }),
    )
    .expect("register dead peer");

    let active_only = handle_tool_call(
        "gitpulse_agent_peers",
        &json!({
            "repo_path": repo,
            "include_exited": false,
        }),
    )
    .expect("list active peers");
    let peers = active_only["peers"].as_array().expect("peers array");
    assert!(
        peers
            .iter()
            .any(|p| p["provider_session_id"] == live_sid && p["liveness"] == "live"),
        "live peer must be visible: {peers:?}"
    );
    assert!(
        !peers.iter().any(|p| p["provider_session_id"] == dead_sid),
        "exited non-resumable peer must be omitted when include_exited=false"
    );

    let with_exited = handle_tool_call(
        "gitpulse_agent_peers",
        &json!({
            "repo_path": repo,
            "include_exited": true,
        }),
    )
    .expect("list all peers");
    let all_peers = with_exited["peers"].as_array().expect("peers array");
    assert!(
        all_peers
            .iter()
            .any(|p| p["provider_session_id"] == dead_sid && p["liveness"] == "exited"),
        "exited peer must appear when include_exited=true: {all_peers:?}"
    );
}

#[test]
fn send_fails_closed_on_ambiguous_provider_and_routes_explicit_selector() {
    let (_dir, repo) = trusted_git_repo();
    let sid_a = "11111111-1111-4111-8111-111111111111";
    let sid_b = "22222222-2222-4222-8222-222222222222";

    for (sid, role) in [(sid_a, "planner"), (sid_b, "verifier")] {
        handle_tool_call(
            "gitpulse_agent_register",
            &json!({
                "repo_path": repo,
                "provider": "agy",
                "surface": "terminal",
                "provider_session_id": sid,
                "role": role,
                "pid": std::process::id(),
            }),
        )
        .expect("register live agy peer");
    }

    let ambiguous = handle_tool_call(
        "gitpulse_agent_send",
        &json!({
            "repo_path": repo,
            "from_peer": "claude:orchestrator",
            "to_peer": "agy",
            "body": "Please audit the patch.",
        }),
    )
    .expect_err("bare provider must fail closed when 2 live sessions match");
    assert!(
        ambiguous.contains("ambiguous_peer"),
        "expected ambiguous_peer error, got: {ambiguous}"
    );

    let targeted = handle_tool_call(
        "gitpulse_agent_send",
        &json!({
            "repo_path": repo,
            "from_peer": "claude:orchestrator",
            "to_peer": "verifier",
            "body": "Please audit the patch.",
            "thread_key": "audit-1",
        }),
    )
    .expect("unique role selector succeeds");
    assert_eq!(targeted["to_peer"], format!("agy:{sid_b}"));
    assert_eq!(targeted["hop"], 1);
}

#[test]
fn large_payload_hydrates_from_bus_without_duplication_and_cross_acks_mailbox() {
    let (_dir, repo) = trusted_git_repo();
    let sid = "33333333-3333-4333-8333-333333333333";

    handle_tool_call(
        "gitpulse_agent_register",
        &json!({
            "repo_path": repo,
            "provider": "claude",
            "surface": "terminal",
            "provider_session_id": sid,
            "role": "worker",
            "pid": std::process::id(),
        }),
    )
    .expect("register worker");

    let large_body = "B".repeat(10 * 1024);
    let sent = handle_tool_call(
        "gitpulse_agent_send",
        &json!({
            "repo_path": repo,
            "from_peer": "agy:lead",
            "to_peer": "worker",
            "msg_id": "msg_large_10k",
            "body": large_body,
        }),
    )
    .expect("send 10 KiB payload");
    assert_eq!(sent["truncated_in_mailbox"], true);

    let mb = Mailbox::at_default().expect("default mailbox");
    assert_eq!(
        mb.status(sid, "msg_large_10k"),
        Delivery::Pending,
        "mailbox holds pending pointer envelope before inbox ack"
    );

    // Peek without ack leaves message pending in both stores.
    let peeked = handle_tool_call(
        "gitpulse_agent_inbox",
        &json!({
            "repo_path": repo,
            "for_peer": "worker",
            "ack": false,
        }),
    )
    .expect("peek inbox");
    let peek_msgs = peeked["messages"].as_array().expect("messages");
    assert_eq!(
        peek_msgs.len(),
        1,
        "bus + mailbox must deduplicate by msg_id"
    );
    assert_eq!(peek_msgs[0]["body"].as_str().unwrap().len(), 10 * 1024);
    assert_eq!(mb.status(sid, "msg_large_10k"), Delivery::Pending);

    // Reading with ack=true marks claimed in BOTH agent_bus and agent_mailbox.
    let acked = handle_tool_call(
        "gitpulse_agent_inbox",
        &json!({
            "repo_path": repo,
            "for_peer": "worker",
            "ack": true,
        }),
    )
    .expect("ack inbox");
    assert_eq!(acked["messages"].as_array().unwrap().len(), 1);
    assert_eq!(
        mb.status(sid, "msg_large_10k"),
        Delivery::Claimed,
        "inbox ack must cross-mark mailbox p-* -> c-* to prevent hook double-delivery"
    );

    let after = handle_tool_call(
        "gitpulse_agent_inbox",
        &json!({
            "repo_path": repo,
            "for_peer": "worker",
            "ack": true,
        }),
    )
    .expect("second inbox read");
    assert!(after["messages"].as_array().unwrap().is_empty());
}

#[test]
fn hop_ceiling_pauses_auto_wake_while_human_resets_hop_to_zero() {
    let (_dir, repo) = trusted_git_repo();
    let sid = "44444444-4444-4444-8444-444444444444";

    handle_tool_call(
        "gitpulse_agent_register",
        &json!({
            "repo_path": repo,
            "provider": "agy",
            "surface": "terminal",
            "provider_session_id": sid,
            "role": "reviewer",
            "pid": std::process::id(),
        }),
    )
    .expect("register reviewer");

    let paused = handle_tool_call(
        "gitpulse_agent_send",
        &json!({
            "repo_path": repo,
            "from_peer": "claude:coder",
            "to_peer": "reviewer",
            "body": "6th bounce",
            "hop": 6,
            "max_hops": 6,
        }),
    )
    .expect("send at hop ceiling");
    assert_eq!(paused["delivery_outcome"], "paused_hop_limit");

    let human_reset = handle_tool_call(
        "gitpulse_agent_send",
        &json!({
            "repo_path": repo,
            "from_peer": "developer",
            "to_peer": "reviewer",
            "body": "Human approved another round.",
            "hop": 6,
            "max_hops": 6,
        }),
    )
    .expect("human send");
    assert_eq!(human_reset["hop"], 0);
    assert_ne!(human_reset["delivery_outcome"], "paused_hop_limit");
}

#[test]
fn wait_clamps_timeout_wakes_on_filesystem_write_and_enforces_slot_ceiling() {
    assert_eq!(
        clamp_wait_timeout_ms(Some(60_000), 60_000),
        MAX_WAIT_TIMEOUT_MS
    );
    assert_eq!(clamp_wait_timeout_ms(Some(45_000), 20_000), 15_000);
    assert_eq!(clamp_wait_timeout_ms(Some(10_000), 4_000), 0);

    let counter = AtomicUsize::new(0);
    let mut guards = Vec::new();
    for _ in 0..MAX_WAIT_CALLS {
        guards.push(WaitCallSlot::acquire(&counter).expect("slot within cap"));
    }
    let fifth = handle_agent_wait_with_counter(
        AgentWaitArgs {
            repo_path: "/tmp".into(),
            for_peer: "p".into(),
            provider_session_id: None,
            since_seq: None,
            thread_key: None,
            include_broadcasts: None,
            timeout_ms: Some(100),
            ack: None,
            limit: None,
        },
        &counter,
        60_000,
    );
    // Trust runs first on untrusted /tmp; verify slot refusal on a trusted repo.
    let (_dir, repo) = trusted_git_repo();
    let sid = "55555555-5555-4555-8555-555555555555";
    handle_tool_call(
        "gitpulse_agent_register",
        &json!({
            "repo_path": repo,
            "provider": "claude",
            "surface": "terminal",
            "provider_session_id": sid,
            "role": "waiter-target",
            "pid": std::process::id(),
        }),
    )
    .expect("register target");

    let saturated = handle_agent_wait_with_counter(
        AgentWaitArgs {
            repo_path: repo.clone(),
            for_peer: "waiter-target".into(),
            provider_session_id: Some(sid.into()),
            since_seq: None,
            thread_key: None,
            include_broadcasts: None,
            timeout_ms: Some(500),
            ack: Some(true),
            limit: Some(10),
        },
        &counter,
        60_000,
    )
    .expect_err("5th waiter must be refused");
    assert!(saturated.contains("wait_slots_saturated"), "{saturated}");
    assert!(fifth.is_err());
    drop(guards);

    // Exercise real filesystem wakeup (direct `agent_bus::post_peer_message` +
    // `Mailbox` deposit from a background thread without calling in-process condvar).
    let repo_writer = repo.clone();
    let writer = std::thread::spawn(move || {
        std::thread::sleep(Duration::from_millis(120));
        let mb = Mailbox::at_default().expect("mailbox");
        mb.deposit(
            sid,
            "msg_fs_wake",
            "Wake from sibling process",
            SystemTime::now(),
        )
        .expect("deposit");
        crate::engine::agent_bus::post_peer_message(
            &repo_writer,
            "agy:sender",
            Some(&format!("claude:{sid}")),
            Some("wake-thread"),
            "note",
            "Wake from sibling process",
            Some("main"),
            Some("msg_fs_wake"),
            1,
            6,
        )
        .expect("post bus");
    });

    let started = Instant::now();
    let woken = handle_agent_wait_with_counter(
        AgentWaitArgs {
            repo_path: repo.clone(),
            for_peer: "waiter-target".into(),
            provider_session_id: Some(sid.into()),
            since_seq: Some(0),
            thread_key: None,
            include_broadcasts: Some(false),
            timeout_ms: Some(5_000),
            ack: Some(true),
            limit: Some(10),
        },
        &counter,
        60_000,
    )
    .expect("wait succeeds");
    writer.join().expect("writer thread");

    assert_eq!(woken["timed_out"], false);
    assert!(
        started.elapsed() < crate::test_support::coverage_relaxed(Duration::from_secs(2)),
        "watcher/stat wake took too long: {:?}",
        started.elapsed()
    );
    assert_eq!(woken["messages"].as_array().unwrap().len(), 1);
    assert_eq!(woken["messages"][0]["msg_id"], "msg_fs_wake");
    let mb = Mailbox::at_default().expect("mailbox");
    assert_eq!(
        mb.status(sid, "msg_fs_wake"),
        Delivery::Claimed,
        "gitpulse_agent_wait must cross-mark mailbox p-* -> c-*"
    );
}

#[cfg(target_os = "macos")]
#[test]
fn kickoff_agent_spawn_routes_to_desktop_owner_over_instance_socket() {
    use crate::desktop::instance::{claim, Claim, Request};
    let (_dir, repo) = trusted_git_repo();
    let profile = tempfile::Builder::new()
        .prefix("gpko")
        .tempdir()
        .expect("short socket tempdir");
    crate::browser::TEST_PROFILE
        .with(|slot| *slot.borrow_mut() = Some(profile.path().to_path_buf()));

    let owner = match claim(profile.path(), Duration::from_secs(2)) {
        Claim::Primary(owner) => owner,
        _ => panic!("test must claim primary socket"),
    };
    let seen = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
    let seen_sink = std::sync::Arc::clone(&seen);
    owner.attach_agent_deliver(std::sync::Arc::new(move |req| {
        seen_sink.lock().unwrap().push(req.clone());
        Ok(json!({
            "ok": true,
            "delivery_outcome": "woken_immediately",
        }))
    }));

    let res = handle_tool_call(
        "gitpulse_kickoff_agent",
        &json!({
            "repo_path": repo,
            "from_agent": "claude:lead",
            "to_agent": "agy:worker",
            "task_id": "gp-a2a-kickoff",
            "provider": "agy",
            "branch": "main",
            "spawn": "terminal",
            "instructions": "Implement the zero-reflow overlay pill.",
        }),
    )
    .expect("kickoff with spawn=terminal");

    assert_eq!(res["spawn_outcome"], "woken_immediately");
    let captured = seen.lock().unwrap();
    assert_eq!(captured.len(), 1);
    match &captured[0] {
        Request::AgentDeliver {
            spawn,
            provider,
            task_id,
            ..
        } => {
            assert_eq!(spawn.as_deref(), Some("terminal"));
            assert_eq!(provider.as_deref(), Some("agy"));
            assert_eq!(task_id.as_deref(), Some("gp-a2a-kickoff"));
        }
        other => panic!("expected Request::AgentDeliver, got {other:?}"),
    }

    drop(owner);
    crate::browser::TEST_PROFILE.with(|slot| *slot.borrow_mut() = None);
}

#[test]
fn antigravity_provider_alias_canonicalizes_in_register_and_peers_filter() {
    let (_dir, repo) = trusted_git_repo();
    let sid = "66666666-6666-4666-8666-666666666666";

    let reg = handle_tool_call(
        "gitpulse_agent_register",
        &json!({
            "repo_path": repo,
            "provider": "antigravity",
            "surface": "terminal",
            "provider_session_id": sid,
            "role": "agy-alias-peer",
            "pid": std::process::id(),
        }),
    )
    .expect("register with provider=antigravity");
    assert_eq!(reg["peer"]["provider"], "agy");
    assert_eq!(reg["peer"]["peer_id"], format!("agy:{sid}"));

    // Re-registering without explicit ids in the same worktree using `provider: "antigravity"`
    // must match the existing `"agy"` live peer rather than inventing `"antigravity:..."`.
    let rereg = handle_tool_call(
        "gitpulse_agent_register",
        &json!({
            "repo_path": repo,
            "provider": "antigravity",
            "role": "agy-alias-peer",
        }),
    )
    .expect("re-register with provider=antigravity");
    assert_eq!(rereg["peer"]["peer_id"], format!("agy:{sid}"));

    // Filtering `gitpulse_agent_peers` by `"antigravity"` or `"agy"` must both return the peer.
    for query_prov in ["antigravity", "agy"] {
        let listed = handle_tool_call(
            "gitpulse_agent_peers",
            &json!({
                "repo_path": repo,
                "provider": query_prov,
                "include_exited": true,
            }),
        )
        .expect("list peers");
        let peers = listed["peers"].as_array().unwrap();
        assert!(
            peers.iter().any(|p| p["peer_id"] == format!("agy:{sid}")),
            "provider filter '{query_prov}' missed canonical agy peer: {listed}"
        );
    }
}
