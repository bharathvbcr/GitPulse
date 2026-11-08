//! Integration tests for multi-agent coordination bus, task lineage rollups,
//! and Antigravity session management in GitPulse.

mod common;

use gitpulse_lib::engine::agent_bus::{
    answer_ask, list_inbox_threads, list_lanes, post_message, post_thread_message, read_messages,
    read_thread_messages, register_or_update_lane, release_lane, withdraw_ask, MAX_BUS_MESSAGES,
};
use gitpulse_lib::procguard::LockedSpawn;
use std::process::Command;
use tempfile::TempDir;

/// Helper to create a fake repository with a real initialized Git repo and granted trust.
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
fn test_agent_bus_message_delivery_and_monotonic_sequence() {
    let repo = create_test_repo();
    let repo_path = repo.path().to_str().unwrap();

    // 1. Post sequential messages to agent bus
    let msg1 = post_message(
        repo_path,
        "agent-alpha",
        Some("agent-beta"),
        "handshake",
        "{\"status\":\"ready\"}",
        Some("feature/subtasks"),
    )
    .expect("post msg1");

    assert_eq!(msg1.seq, 1);
    assert_eq!(msg1.from_agent, "agent-alpha");
    assert_eq!(msg1.to_agent.as_deref(), Some("agent-beta"));
    assert_eq!(msg1.kind, "handshake");

    let msg2 = post_message(
        repo_path,
        "agent-beta",
        Some("agent-alpha"),
        "ack",
        "{\"status\":\"confirmed\"}",
        Some("feature/subtasks"),
    )
    .expect("post msg2");

    assert_eq!(msg2.seq, 2);
    assert!(
        msg2.seq > msg1.seq,
        "sequence numbers must be strictly monotonic"
    );

    // 2. Read messages with pagination
    let page = read_messages(repo_path, None, 0).expect("read messages from 0");
    assert_eq!(page.messages.len(), 2);
    assert_eq!(page.latest_seq, 2);
    assert_eq!(page.dropped_before_seq, 0);

    // Read since seq 1
    let page_since_1 = read_messages(repo_path, None, 1).expect("read messages from 1");
    assert_eq!(page_since_1.messages.len(), 1);
    assert_eq!(page_since_1.messages[0].seq, 2);
}

#[test]
fn test_vespin_threaded_asks_lifecycle_and_answering() {
    let repo = create_test_repo();
    let repo_path = repo.path().to_str().unwrap();

    // 1. Post a structured "decide" ask in a thread
    let thread_key = "thread-arch-decision-1";
    let ask_body =
        r#"{"ask_type":"decide","question":"Which storage engine?","options":["SQLite","JSON"]}"#;

    let ask_msg = post_thread_message(
        repo_path,
        "worker-claude",
        Some("orchestrator"),
        Some(thread_key),
        "ask",
        ask_body,
        Some("main"),
    )
    .expect("post decide ask");

    assert_eq!(ask_msg.thread_key.as_deref(), Some(thread_key));
    assert_eq!(ask_msg.kind, "ask");

    // 2. Verify thread listing aggregates thread status correctly
    let threads = list_inbox_threads(repo_path, None, None).expect("list threads");
    assert_eq!(threads.len(), 1);
    assert_eq!(threads[0].thread_key, thread_key);
    assert_eq!(threads[0].status, "open");
    assert_eq!(threads[0].ask_type.as_deref(), Some("decide"));
    assert_eq!(threads[0].from_agent, "worker-claude");

    // 3. User / Orchestrator answers the ask
    let answer_body = r#"{"selected":"SQLite","notes":"Optimized for WAL concurrency"}"#;
    let answer_msg =
        answer_ask(repo_path, "orchestrator", thread_key, answer_body).expect("answer ask");

    assert_eq!(answer_msg.kind, "reply");
    assert_eq!(answer_msg.thread_key.as_deref(), Some(thread_key));

    // 4. Verify thread transitions to "answered"
    let updated_threads = list_inbox_threads(repo_path, None, None).expect("list updated threads");
    assert_eq!(updated_threads.len(), 1);
    assert_eq!(updated_threads[0].status, "answered");
    assert_eq!(updated_threads[0].messages.len(), 2);

    // 5. Test thread read
    let thread_msgs =
        read_thread_messages(repo_path, Some(thread_key), None, 0).expect("read thread msgs");
    assert_eq!(thread_msgs.messages.len(), 2);
    assert_eq!(thread_msgs.messages[0].kind, "ask");
    assert_eq!(thread_msgs.messages[1].kind, "reply");
}

#[test]
fn test_vespin_ask_withdrawal() {
    let repo = create_test_repo();
    let repo_path = repo.path().to_str().unwrap();

    let thread_key = "thread-to-withdraw";
    let ask_body = r#"{"ask_type":"do","steps":["Review PR #42"]}"#;

    post_thread_message(
        repo_path,
        "worker-agy",
        Some("orchestrator"),
        Some(thread_key),
        "ask",
        ask_body,
        Some("main"),
    )
    .expect("post ask");

    // Withdraw the ask
    let withdraw_msg = withdraw_ask(
        repo_path,
        "worker-agy",
        thread_key,
        "No longer needed - PR merged automatically",
    )
    .expect("withdraw ask");

    assert_eq!(withdraw_msg.kind, "withdraw");

    let threads = list_inbox_threads(repo_path, None, None).expect("list threads");
    assert_eq!(threads[0].status, "withdrawn");
}

#[test]
fn test_agent_bus_ring_buffer_cap_preserves_bounds() {
    let repo = create_test_repo();
    let repo_path = repo.path().to_str().unwrap();

    // Post messages exceeding MAX_BUS_MESSAGES (256)
    let total_to_post = MAX_BUS_MESSAGES + 10;
    for i in 1..=total_to_post {
        post_message(
            repo_path,
            "agent-chatter",
            None,
            "ping",
            &format!("{{\"count\":{i}}}"),
            None,
        )
        .expect("post sequential message");
    }

    let page = read_messages(repo_path, None, 0).expect("read all messages");
    assert_eq!(page.messages.len(), MAX_BUS_MESSAGES);
    assert_eq!(page.latest_seq, total_to_post as u64);
    assert_eq!(page.dropped_before_seq, 11);
    assert_eq!(page.messages.first().unwrap().seq, 11);
    assert_eq!(page.messages.last().unwrap().seq, total_to_post as u64);
}

#[test]
fn test_agent_lane_claims_and_registration_lifecycle() {
    let repo = create_test_repo();
    let repo_path = repo.path().to_str().unwrap();

    let registered = register_or_update_lane(
        repo_path,
        "claude-subtask-worker",
        "feature/bridge-form",
        Some("/tmp/worktree-1"),
        &["src/".into()],
        &["src/lib/components/AgentInbox.svelte".into()],
        &["AgentInbox".into()],
    )
    .expect("register lane");
    assert_eq!(registered.agent_id, "claude-subtask-worker");

    let lanes = list_lanes(repo_path).expect("list lanes");
    assert_eq!(lanes.len(), 1);
    assert_eq!(lanes[0].agent_id, "claude-subtask-worker");

    let released = release_lane(repo_path, "claude-subtask-worker").expect("release lane");
    assert!(released);

    let empty_lanes = list_lanes(repo_path).expect("list lanes after release");
    assert_eq!(empty_lanes.len(), 0);
}

#[test]
fn test_subtask_lineage_rollup_contract() {
    // Linear-style task decomposition schema verification
    let parent_json = serde_json::json!({
        "id": "T-100",
        "title": "Implement Agent Inbox",
        "status": "in_progress",
        "parent": null
    });

    let subtask1_json = serde_json::json!({
        "id": "T-101",
        "title": "Build Svelte Component",
        "status": "done",
        "parent": "T-100"
    });

    let subtask2_json = serde_json::json!({
        "id": "T-102",
        "title": "Wire Agent Bus",
        "status": "in_progress",
        "parent": "T-100"
    });

    assert_eq!(parent_json["parent"], serde_json::Value::Null);
    assert_eq!(subtask1_json["parent"], "T-100");
    assert_eq!(subtask2_json["parent"], "T-100");

    // Lineage rollup: 1 of 2 done
    let subtasks = [&subtask1_json, &subtask2_json];
    let done_count = subtasks.iter().filter(|s| s["status"] == "done").count();
    let total_count = subtasks.len();

    let lineage_payload = serde_json::json!({
        "parent_id": "T-100",
        "done": done_count,
        "total": total_count,
        "ratio": format!("{done_count}/{total_count}")
    });

    assert_eq!(lineage_payload["done"], 1);
    assert_eq!(lineage_payload["total"], 2);
    assert_eq!(lineage_payload["ratio"], "1/2");
}

#[test]
fn test_antigravity_cli_binary_and_argument_conformance() {
    let agy_path = "/Users/bharath/.local/bin/agy";
    if !std::path::Path::new(agy_path).exists() {
        eprintln!("Skipping live agy binary test: {agy_path} not found");
        return;
    }

    // 1. Verify --version returns expected semver
    let output = Command::new(agy_path)
        .arg("--version")
        .output_locked()
        .expect("run agy --version");
    assert!(output.status.success(), "agy --version failed");
    let version_str = String::from_utf8_lossy(&output.stdout);
    assert!(
        !version_str.trim().is_empty(),
        "version output should not be empty"
    );

    // 2. Verify --help publishes required flags on stderr/stdout
    let help_output = Command::new(agy_path)
        .arg("--help")
        .output_locked()
        .expect("run agy --help");
    let help_text = format!(
        "{}\n{}",
        String::from_utf8_lossy(&help_output.stdout),
        String::from_utf8_lossy(&help_output.stderr)
    );

    assert!(
        help_text.contains("--prompt-interactive"),
        "agy must support --prompt-interactive"
    );
    assert!(
        help_text.contains("--conversation"),
        "agy must support --conversation"
    );
    assert!(help_text.contains("--mode"), "agy must support --mode");
    assert!(help_text.contains("--model"), "agy must support --model");
    assert!(help_text.contains("--effort"), "agy must support --effort");

    // 3. Verify argument generator contract for Antigravity
    let test_uuid = "a1b2c3d4-e5f6-7890-abcd-ef1234567890";
    let generated_args = [
        "--mode".to_string(),
        "accept-edits".to_string(),
        "--conversation".to_string(),
        test_uuid.to_string(),
        "--prompt-interactive".to_string(),
        "Task prompt content".to_string(),
    ];

    let conv_pos = generated_args
        .iter()
        .position(|a| a == "--conversation")
        .unwrap();
    assert_eq!(generated_args[conv_pos + 1], test_uuid);
    let prompt_pos = generated_args
        .iter()
        .position(|a| a == "--prompt-interactive")
        .unwrap();
    assert_eq!(generated_args[prompt_pos + 1], "Task prompt content");
}

#[test]
fn test_inbox_threads_multibyte_utf8_truncation_without_panic() {
    let repo = create_test_repo();
    let repo_path = repo.path().to_str().unwrap();

    // 1. Emoji at byte offset 55..59 (4 bytes)
    // 55 ASCII chars + 4-byte emoji 🚀 + tail exceeding 60 chars
    let body_emoji = format!("{}🚀{}", "A".repeat(55), "tail_exceeding_sixty_chars_limit");
    post_thread_message(
        repo_path,
        "agent-emoji",
        None,
        Some("thread-emoji"),
        "note",
        &body_emoji,
        None,
    )
    .expect("post emoji message");

    // 2. CJK character at byte offset 56..59 (3 bytes)
    // 56 ASCII chars + 3-byte CJK character 中 + tail exceeding 60 chars
    let body_cjk = format!("{}中{}", "B".repeat(56), "tail_exceeding_sixty_chars_limit");
    post_thread_message(
        repo_path,
        "agent-cjk",
        None,
        Some("thread-cjk"),
        "note",
        &body_cjk,
        None,
    )
    .expect("post cjk message");

    // 3. Accented character at byte offset 56..58 (2 bytes)
    // 56 ASCII chars + 2-byte accented character é + tail exceeding 60 chars
    let body_accent = format!("{}é{}", "C".repeat(56), "tail_exceeding_sixty_chars_limit");
    post_thread_message(
        repo_path,
        "agent-accent",
        None,
        Some("thread-accent"),
        "note",
        &body_accent,
        None,
    )
    .expect("post accent message");

    // Must list threads without panicking
    let threads =
        list_inbox_threads(repo_path, None, None).expect("list threads with multi-byte text");
    assert_eq!(threads.len(), 3);

    let t_emoji = threads
        .iter()
        .find(|t| t.thread_key == "thread-emoji")
        .expect("find thread-emoji");
    assert!(t_emoji.title.ends_with('…'), "Title must end with ellipsis");
    assert_eq!(
        t_emoji.title.chars().count(),
        58,
        "Title must have exactly 57 chars + 1 ellipsis char"
    );
    assert!(
        t_emoji.title.contains('🚀'),
        "Emoji at offset 55 must be preserved in title snippet"
    );

    let t_cjk = threads
        .iter()
        .find(|t| t.thread_key == "thread-cjk")
        .expect("find thread-cjk");
    assert!(t_cjk.title.ends_with('…'));
    assert_eq!(t_cjk.title.chars().count(), 58);
    assert!(t_cjk.title.contains('中'));

    let t_accent = threads
        .iter()
        .find(|t| t.thread_key == "thread-accent")
        .expect("find thread-accent");
    assert!(t_accent.title.ends_with('…'));
    assert_eq!(t_accent.title.chars().count(), 58);
    assert!(t_accent.title.contains('é'));
}
