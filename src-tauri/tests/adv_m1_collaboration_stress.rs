//! Adversarial empirical stress suite for Milestone 1:
//! 1. `gitpulse_add_task` with subtasks, deep lineage, and sibling subtask word collision.
//! 2. Agent bus & inbox edge cases (empty strings, large payloads, non-existent threads).
//! 3. Ring buffer eviction behavior under heavy message load.
//! 4. MCP tool lifecycle and error boundaries.

mod common;

use gitpulse_lib::engine::agent_bus::{
    answer_ask, list_inbox_threads, post_thread_message, read_messages,
    read_thread_messages, withdraw_ask, MAX_BUS_MESSAGES, MAX_MESSAGE_BODY_BYTES,
};
use gitpulse_lib::mcp::{process_request, Era, JsonRpcRequest, PROTOCOL_VERSION};
use serde_json::{json, Value};
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

fn modern_meta() -> Value {
    json!({
        "io.modelcontextprotocol/protocolVersion": PROTOCOL_VERSION,
        "io.modelcontextprotocol/clientInfo": { "name": "gitpulse-test", "version": "0" },
        "io.modelcontextprotocol/clientCapabilities": {}
    })
}

fn mcp_call(tool_name: &str, arguments: Value) -> (bool, Value) {
    let mut era = Era::Modern;
    let req = JsonRpcRequest {
        jsonrpc: "2.0".to_string(),
        id: Some(json!(1)),
        method: "tools/call".to_string(),
        params: json!({
            "name": tool_name,
            "arguments": arguments,
            "_meta": modern_meta()
        }),
    };

    let resp = process_request(req, &mut era).expect("mcp response");
    if let Some(err) = resp.error {
        return (true, json!(err));
    }
    let res = resp.result.expect("result value");
    let is_err = res.get("isError").and_then(Value::as_bool).unwrap_or(false);
    let text = res
        .get("content")
        .and_then(Value::as_array)
        .and_then(|arr| arr.first())
        .and_then(|item| item.get("text"))
        .and_then(Value::as_str)
        .unwrap_or_default();
    let parsed: Value = serde_json::from_str(text).unwrap_or_else(|_| json!(text));
    (is_err, parsed)
}

#[test]
fn test_agent_bus_empty_and_whitespace_validation() {
    let repo = create_test_repo();
    let repo_path = repo.path().to_str().unwrap();

    // 1. Empty body must be rejected
    let err = post_thread_message(
        repo_path,
        "agent-1",
        None,
        Some("thread-1"),
        "ask",
        "",
        None,
    )
    .unwrap_err();
    assert!(
        err.message().contains("empty"),
        "Expected empty body rejection, got: {}", err.message()
    );

    // 2. Whitespace-only body must be rejected
    let err_ws = post_thread_message(
        repo_path,
        "agent-1",
        None,
        Some("thread-1"),
        "ask",
        "   \t\n   ",
        None,
    )
    .unwrap_err();
    assert!(
        err_ws.message().contains("empty"),
        "Expected whitespace body rejection, got: {}", err_ws.message()
    );

    // 3. Null byte in body must be rejected
    let err_null = post_thread_message(
        repo_path,
        "agent-1",
        None,
        Some("thread-1"),
        "ask",
        "hello\0world",
        None,
    )
    .unwrap_err();
    assert!(
        err_null.message().contains("empty"),
        "Expected null byte rejection, got: {}", err_null.message()
    );

    // 4. Empty from_agent must be rejected
    let err_agent = post_thread_message(
        repo_path,
        "",
        None,
        Some("thread-1"),
        "ask",
        "valid body",
        None,
    )
    .unwrap_err();
    assert!(
        err_agent.message().contains("empty"),
        "Expected empty from_agent rejection, got: {}", err_agent.message()
    );

    // 5. Empty reason in withdraw must be rejected
    let err_reason = withdraw_ask(repo_path, "agent-1", "thread-1", "").unwrap_err();
    assert!(
        err_reason.message().contains("empty"),
        "Expected empty reason rejection, got: {}", err_reason.message()
    );

    // 6. Non-empty thread_key validation in withdraw_ask
    let unattached_withdraw = withdraw_ask(repo_path, "agent-1", "", "reason");
    assert!(
        unattached_withdraw.is_err(),
        "withdraw_ask must reject empty thread_key"
    );
    let err_tk = unattached_withdraw.unwrap_err();
    assert!(
        err_tk.message().contains("empty"),
        "Expected empty thread_key rejection, got: {}",
        err_tk.message()
    );

    let ws_withdraw = withdraw_ask(repo_path, "agent-1", "   \t\n  ", "reason");
    assert!(
        ws_withdraw.is_err(),
        "withdraw_ask must reject whitespace thread_key"
    );
}

#[test]
fn test_agent_bus_payload_boundaries() {
    let repo = create_test_repo();
    let repo_path = repo.path().to_str().unwrap();

    // 1. Exact MAX_MESSAGE_BODY_BYTES (65536 bytes) must succeed
    let exact_body = "x".repeat(MAX_MESSAGE_BODY_BYTES);
    let msg_ok = post_thread_message(
        repo_path,
        "agent-1",
        None,
        Some("thread-bound"),
        "ask",
        &exact_body,
        None,
    );
    assert!(msg_ok.is_ok(), "Expected exact max bytes to succeed");

    // 2. MAX_MESSAGE_BODY_BYTES + 1 (65537 bytes) must be rejected
    let excess_body = "x".repeat(MAX_MESSAGE_BODY_BYTES + 1);
    let err_body = post_thread_message(
        repo_path,
        "agent-1",
        None,
        Some("thread-bound-2"),
        "ask",
        &excess_body,
        None,
    )
    .unwrap_err();
    assert!(
        err_body.message().contains("exceeds maximum of"),
        "Expected payload limit rejection, got: {}", err_body.message()
    );

    // 3. Thread key max length 80 chars
    let exact_tk = "k".repeat(80);
    assert!(post_thread_message(
        repo_path,
        "agent-1",
        None,
        Some(&exact_tk),
        "ask",
        "valid",
        None
    )
    .is_ok());

    let excess_tk = "k".repeat(81);
    let err_tk = post_thread_message(
        repo_path,
        "agent-1",
        None,
        Some(&excess_tk),
        "ask",
        "valid",
        None
    )
    .unwrap_err();
    assert!(
        err_tk.message().contains("exceeds maximum length of 80"),
        "Expected thread_key limit rejection, got: {}", err_tk.message()
    );

    // 4. from_agent max length 128 chars
    let exact_agent = "a".repeat(128);
    assert!(post_thread_message(
        repo_path,
        &exact_agent,
        None,
        Some("thread-1"),
        "ask",
        "valid",
        None
    )
    .is_ok());

    let excess_agent = "a".repeat(129);
    let err_agent = post_thread_message(
        repo_path,
        &excess_agent,
        None,
        Some("thread-1"),
        "ask",
        "valid",
        None
    )
    .unwrap_err();
    assert!(
        err_agent
            .message()
            .contains("exceeds maximum length of 128"),
        "Expected from_agent limit rejection, got: {}", err_agent.message()
    );

    // 5. withdraw reason max length 600 chars
    let exact_reason = "r".repeat(600);
    assert!(withdraw_ask(repo_path, "agent-1", "thread-1", &exact_reason).is_ok());

    let excess_reason = "r".repeat(601);
    let err_reason = withdraw_ask(repo_path, "agent-1", "thread-1", &excess_reason).unwrap_err();
    assert!(
        err_reason
            .message()
            .contains("exceeds maximum length of 600"),
        "Expected reason limit rejection, got: {}", err_reason.message()
    );
}

#[test]
fn test_agent_bus_nonexistent_thread_and_filtering() {
    let repo = create_test_repo();
    let repo_path = repo.path().to_str().unwrap();

    // 1. Reading nonexistent thread should return empty list without error
    let empty_page = read_thread_messages(repo_path, Some("nonexistent-thread-key"), None, 0)
        .expect("read nonexistent thread");
    assert_eq!(empty_page.messages.len(), 0);

    // 2. Listing inbox threads when bus is completely empty
    let empty_threads = list_inbox_threads(repo_path, None, None).expect("list empty inbox");
    assert_eq!(empty_threads.len(), 0);

    // 3. Post a message to thread-A, thread-B
    post_thread_message(
        repo_path,
        "agent-alpha",
        Some("agent-beta"),
        Some("thread-A"),
        "ask",
        r#"{"ask_type":"decide","question":"Choice A or B?"}"#,
        None,
    )
    .expect("post thread-A");

    post_thread_message(
        repo_path,
        "agent-gamma",
        Some("agent-delta"),
        Some("thread-B"),
        "ask",
        r#"{"ask_type":"do","steps":["Step 1"]}"#,
        None,
    )
    .expect("post thread-B");

    // Filter by agent_id
    let alpha_threads = list_inbox_threads(repo_path, Some("agent-alpha"), None).expect("list alpha");
    assert_eq!(alpha_threads.len(), 1);
    assert_eq!(alpha_threads[0].thread_key, "thread-A");

    let beta_threads = list_inbox_threads(repo_path, Some("agent-beta"), None).expect("list beta");
    assert_eq!(beta_threads.len(), 1);
    assert_eq!(beta_threads[0].thread_key, "thread-A");

    let unknown_threads =
        list_inbox_threads(repo_path, Some("agent-unknown"), None).expect("list unknown");
    assert_eq!(unknown_threads.len(), 0);

    // Filter by status
    let open_threads = list_inbox_threads(repo_path, None, Some("open")).expect("list open");
    assert_eq!(open_threads.len(), 2);

    let answered_threads =
        list_inbox_threads(repo_path, None, Some("answered")).expect("list answered");
    assert_eq!(answered_threads.len(), 0);

    // Answer thread-A
    answer_ask(repo_path, "agent-beta", "thread-A", "Choice A confirmed").expect("answer A");

    let updated_open = list_inbox_threads(repo_path, None, Some("open")).expect("list open");
    assert_eq!(updated_open.len(), 1);
    assert_eq!(updated_open[0].thread_key, "thread-B");

    let updated_answered =
        list_inbox_threads(repo_path, None, Some("answered")).expect("list answered");
    assert_eq!(updated_answered.len(), 1);
    assert_eq!(updated_answered[0].thread_key, "thread-A");
}

#[test]
fn test_agent_bus_ring_buffer_eviction_threads_integrity() {
    let repo = create_test_repo();
    let repo_path = repo.path().to_str().unwrap();

    // Post 300 messages across 3 threads
    for i in 1..=100 {
        post_thread_message(
            repo_path,
            "agent-worker",
            None,
            Some("thread-1"),
            if i == 1 { "ask" } else { "reply" },
            &format!("{{\"msg\":{i}}}"),
            None,
        )
        .expect("post thread-1");
    }

    for i in 101..=200 {
        post_thread_message(
            repo_path,
            "agent-worker",
            None,
            Some("thread-2"),
            if i == 101 { "ask" } else { "reply" },
            &format!("{{\"msg\":{i}}}"),
            None,
        )
        .expect("post thread-2");
    }

    for i in 201..=300 {
        post_thread_message(
            repo_path,
            "agent-worker",
            None,
            Some("thread-3"),
            if i == 201 { "ask" } else { "reply" },
            &format!("{{\"msg\":{i}}}"),
            None,
        )
        .expect("post thread-3");
    }

    // Bus cap is 256 messages. Total posted = 300.
    // 44 messages dropped (seq 1..44).
    let page = read_messages(repo_path, None, 0).expect("read bus");
    assert_eq!(page.messages.len(), MAX_BUS_MESSAGES);
    assert_eq!(page.dropped_before_seq, 45);
    assert_eq!(page.latest_seq, 300);

    // Threads listing must still be robust and not panic
    let threads = list_inbox_threads(repo_path, None, None).expect("list threads after eviction");
    assert_eq!(threads.len(), 3);
    for t in &threads {
        assert!(!t.messages.is_empty());
        assert!(t.messages.first().unwrap().seq >= 45);
    }
}

#[test]
fn test_mcp_inbox_tools_and_add_task_validation() {
    let repo = create_test_repo();
    let repo_path = repo.path().to_str().unwrap();

    // 1. gitpulse_inbox_post with empty body must report isError
    let (is_err, resp) = mcp_call(
        "gitpulse_inbox_post",
        json!({
            "repo_path": repo_path,
            "from_agent": "worker-1",
            "thread_key": "thread-test-1",
            "body": ""
        }),
    );
    assert!(is_err, "Expected error on empty body, got: {resp}");

    // 2. gitpulse_inbox_post with oversized thread_key must report isError
    let (is_err_tk, resp_tk) = mcp_call(
        "gitpulse_inbox_post",
        json!({
            "repo_path": repo_path,
            "from_agent": "worker-1",
            "thread_key": "t".repeat(81),
            "body": "Valid message"
        }),
    );
    assert!(is_err_tk, "Expected error on 81 char thread_key, got: {resp_tk}");

    // 3. gitpulse_inbox_withdraw with oversized reason must report isError
    let (is_err_rs, resp_rs) = mcp_call(
        "gitpulse_inbox_withdraw",
        json!({
            "repo_path": repo_path,
            "from_agent": "worker-1",
            "thread_key": "thread-test-1",
            "reason": "r".repeat(601)
        }),
    );
    assert!(is_err_rs, "Expected error on 601 char reason, got: {resp_rs}");

    // 4. Successful gitpulse_inbox_post lifecycle
    let (err_post, post_res) = mcp_call(
        "gitpulse_inbox_post",
        json!({
            "repo_path": repo_path,
            "from_agent": "worker-1",
            "thread_key": "thread-decision-42",
            "kind": "ask",
            "body": serde_json::json!({
                "ask_type": "decide",
                "question": "Choose architecture",
                "options": ["Monolith", "Microservices"]
            }).to_string()
        }),
    );
    assert!(!err_post, "Failed to post ask: {post_res}");
    assert_eq!(post_res["thread_key"], "thread-decision-42");

    // 5. Read inbox via MCP tool
    let (err_read, read_res) = mcp_call(
        "gitpulse_inbox_read",
        json!({
            "repo_path": repo_path,
            "status": "open",
            "thread_key": "thread-decision-42"
        }),
    );
    assert!(!err_read, "Failed to read inbox: {read_res}");
    let threads = read_res["threads"].as_array().expect("threads array");
    assert_eq!(threads.len(), 1);
    assert_eq!(threads[0]["status"], "open");
    assert_eq!(threads[0]["ask_type"], "decide");

    // 6. Withdraw ask via MCP tool
    let (err_wd, wd_res) = mcp_call(
        "gitpulse_inbox_withdraw",
        json!({
            "repo_path": repo_path,
            "from_agent": "worker-1",
            "thread_key": "thread-decision-42",
            "reason": "Decision taken out of band"
        }),
    );
    assert!(!err_wd, "Failed to withdraw: {wd_res}");

    // Verify it is now withdrawn
    let (err_read_wd, read_wd_res) = mcp_call(
        "gitpulse_inbox_read",
        json!({
            "repo_path": repo_path,
            "status": "withdrawn",
            "thread_key": "thread-decision-42"
        }),
    );
    assert!(!err_read_wd, "Failed to read withdrawn: {read_wd_res}");
    let wd_threads = read_wd_res["threads"].as_array().expect("threads");
    assert_eq!(wd_threads.len(), 1);
    assert_eq!(wd_threads[0]["status"], "withdrawn");

    // 7. Test task decomposition via gitpulse_add_task
    let (err_task, task_res) = mcp_call(
        "gitpulse_add_task",
        json!({
            "repo_path": repo_path,
            "task_id": "parent-epic-test",
            "title": "Adversarial Parent Task",
            "subtasks": [
                "Subtask Alpha First",
                "Subtask Beta Second",
                "Subtask Gamma Third"
            ]
        }),
    );
    assert!(!err_task, "Failed to add parent task with subtasks: {task_res}");
    assert_eq!(task_res["task_id"], "parent-epic-test");
    let subtasks = task_res["subtasks"].as_array().expect("subtasks array");
    assert_eq!(subtasks.len(), 3);
    assert_eq!(subtasks[0]["task_id"], "parent-epic-test-sub-1");
    assert_eq!(subtasks[1]["task_id"], "parent-epic-test-sub-2");
    assert_eq!(subtasks[2]["task_id"], "parent-epic-test-sub-3");

    // 8. Test nested subtask decomposition: decompose Subtask Alpha into grandchild subtasks
    let (err_child, child_res) = mcp_call(
        "gitpulse_add_task",
        json!({
            "repo_path": repo_path,
            "task_id": "parent-epic-test-sub-1-child",
            "title": "Grandchild Task Implementation",
            "parent_task_id": "parent-epic-test-sub-1",
            "subtasks": [
                "Leaf Subtask Level 3"
            ]
        }),
    );
    assert!(!err_child, "Failed to add nested grandchild task: {child_res}");
    let child_subtasks = child_res["subtasks"].as_array().expect("child subtasks");
    assert_eq!(child_subtasks.len(), 1);
    assert_eq!(child_subtasks[0]["task_id"], "parent-epic-test-sub-1-child-sub-1");

    // 9. EMPIRICAL HYPOTHESIS TEST: Sibling subtasks with overlapping title words!
    // What if parent task has subtasks with shared words (e.g. "Build login UI" and "Build login API")?
    let (err_overlap, overlap_res) = mcp_call(
        "gitpulse_add_task",
        json!({
            "repo_path": repo_path,
            "task_id": "epic-overlap-test",
            "title": "Epic with Colliding Subtasks",
            "subtasks": [
                "Build login UI components",
                "Build login API endpoints"
            ]
        }),
    );
    assert!(!err_overlap, "Call failed: {overlap_res}");
    let overlap_subtasks = overlap_res["subtasks"].as_array().expect("overlap subtasks");
    assert_eq!(overlap_subtasks.len(), 2, "Both overlap subtasks must be created");
    assert_eq!(overlap_subtasks[0]["task_id"], "epic-overlap-test-sub-1");
    assert_eq!(overlap_subtasks[1]["task_id"], "epic-overlap-test-sub-2");

    // 10. Real-world scenario: Epic decomposed into 5 domain subtasks sharing words
    let (err_5, res_5) = mcp_call(
        "gitpulse_add_task",
        json!({
            "repo_path": repo_path,
            "task_id": "auth-refactor-epic",
            "title": "Refactor Authentication Service",
            "subtasks": [
                "Refactor auth service login",
                "Refactor auth service logout",
                "Refactor auth service session",
                "Refactor auth service token",
                "Refactor auth service refresh"
            ]
        }),
    );
    assert!(!err_5, "Call failed: {res_5}");
    let subtasks_5 = res_5["subtasks"].as_array().expect("subtasks_5");
    assert_eq!(subtasks_5.len(), 5, "All 5 domain subtasks must be created without drops");
    for i in 1..=5 {
        assert_eq!(subtasks_5[i - 1]["task_id"], format!("auth-refactor-epic-sub-{}", i));
    }

    // 11. gitpulse_inbox_post with title parameter incorporated into body & read
    let (err_title, res_title) = mcp_call(
        "gitpulse_inbox_post",
        json!({
            "repo_path": repo_path,
            "from_agent": "worker-title-test",
            "thread_key": "thread-title-test-1",
            "title": "Database Engine Choice",
            "body": "We should decide between SQLite and PostgreSQL"
        }),
    );
    assert!(!err_title, "Failed to post with title: {res_title}");
    let (err_read_title, read_title_res) = mcp_call(
        "gitpulse_inbox_read",
        json!({
            "repo_path": repo_path,
            "thread_key": "thread-title-test-1"
        }),
    );
    assert!(!err_read_title, "Failed to read thread: {read_title_res}");
    let title_threads = read_title_res["threads"].as_array().expect("threads");
    assert_eq!(title_threads.len(), 1);
    assert_eq!(title_threads[0]["title"], "Database Engine Choice");
}

