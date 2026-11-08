//! Adversarial empirical challenge suite for Milestone 1 Iteration 2:
//! 1. Sibling subtask collision (2, 5, 10 subtasks with overlapping keywords & prefixes).
//! 2. Error fail-loud on child task errors (empty title, invalid characters, oversized titles).
//! 3. Empty and whitespace `thread_key` rejection across agent_bus and MCP.
//! 4. Full subtask lineage and parent linkage verification.

mod common;

use gitpulse_lib::engine::agent_bus::{answer_ask, withdraw_ask};
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
        "io.modelcontextprotocol/clientInfo": { "name": "gitpulse-challenger-m1-iter2", "version": "1.0" },
        "io.modelcontextprotocol/clientCapabilities": {}
    })
}

static MCP_MUTEX: std::sync::Mutex<()> = std::sync::Mutex::new(());

fn mcp_call(tool_name: &str, arguments: Value) -> (bool, Value) {
    let _guard = MCP_MUTEX.lock().unwrap_or_else(|e| e.into_inner());
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
fn test_adversarial_sibling_subtask_collision_2_5_10() {
    let repo = create_test_repo();
    let repo_path = repo.path().to_str().unwrap();

    // Dimension 1A: 2 sibling subtasks with overlapping keywords
    let (err_2, res_2) = mcp_call(
        "gitpulse_add_task",
        json!({
            "repo_path": repo_path,
            "task_id": "epic-coll-2",
            "title": "Authentication Microservice Overhaul",
            "subtasks": [
                "Refactor authentication microservice login handler",
                "Refactor authentication microservice logout handler"
            ]
        }),
    );
    assert!(!err_2, "Failed to add 2 sibling subtasks: {res_2}");
    let subtasks_2 = res_2["subtasks"].as_array().expect("subtasks array");
    assert_eq!(
        subtasks_2.len(),
        2,
        "Exactly 2 subtasks must be created without drops"
    );
    assert_eq!(subtasks_2[0]["task_id"], "epic-coll-2-sub-1");
    assert_eq!(subtasks_2[1]["task_id"], "epic-coll-2-sub-2");

    // Dimension 1B: 5 sibling subtasks sharing domain keywords
    let (err_5, res_5) = mcp_call(
        "gitpulse_add_task",
        json!({
            "repo_path": repo_path,
            "task_id": "epic-coll-5",
            "title": "Payment Processing Pipeline Redesign",
            "subtasks": [
                "Payment processing pipeline Stripe webhook ingestion",
                "Payment processing pipeline PayPal charge verification",
                "Payment processing pipeline ledger double entry",
                "Payment processing pipeline receipt mail dispatch",
                "Payment processing pipeline refund reconciliation"
            ]
        }),
    );
    assert!(!err_5, "Failed to add 5 sibling subtasks: {res_5}");
    let subtasks_5 = res_5["subtasks"].as_array().expect("subtasks_5");
    assert_eq!(
        subtasks_5.len(),
        5,
        "100% of 5 sibling subtasks must be created (0% drop rate)"
    );
    for i in 1..=5 {
        assert_eq!(
            subtasks_5[i - 1]["task_id"],
            format!("epic-coll-5-sub-{}", i)
        );
    }

    // Dimension 1C: 10 sibling subtasks sharing identical prefixes and domain words
    let subtask_titles_10: Vec<String> = (1..=10)
        .map(|i| format!("Database cluster migration shard {} partition schema", i))
        .collect();
    let (err_10, res_10) = mcp_call(
        "gitpulse_add_task",
        json!({
            "repo_path": repo_path,
            "task_id": "epic-coll-10",
            "title": "Database Cluster Shard Migration Epic",
            "subtasks": subtask_titles_10
        }),
    );
    assert!(!err_10, "Failed to add 10 sibling subtasks: {res_10}");
    let subtasks_10 = res_10["subtasks"].as_array().expect("subtasks_10");
    assert_eq!(
        subtasks_10.len(),
        10,
        "100% of 10 sibling subtasks must be created (0% drop rate)"
    );
    for i in 1..=10 {
        assert_eq!(
            subtasks_10[i - 1]["task_id"],
            format!("epic-coll-10-sub-{}", i)
        );
    }

    // Verify parent linkage and retrieval across all 10 subtasks via gitpulse_get_task
    for i in 1..=10 {
        let sub_key = format!("epic-coll-10-sub-{}", i);
        let (err_get, get_res) = mcp_call(
            "gitpulse_get_task",
            json!({
                "repo_path": repo_path,
                "task_id": sub_key
            }),
        );
        assert!(
            !err_get,
            "Subtask {} must be retrievable via gitpulse_get_task: {get_res}",
            sub_key
        );
        let task_obj = &get_res["task"];
        assert_eq!(
            task_obj["kind"], "subtask",
            "Subtask kind must be 'subtask'"
        );
        assert_eq!(
            task_obj["status"], "ready",
            "Subtask status must default to 'ready'"
        );
    }
}

#[test]
fn test_adversarial_error_fail_loud_on_child_task_errors() {
    let repo = create_test_repo();
    let repo_path = repo.path().to_str().unwrap();

    // 1. Inject invalid child task with empty title
    let (err_empty, res_empty) = mcp_call(
        "gitpulse_add_task",
        json!({
            "repo_path": repo_path,
            "task_id": "epic-fail-empty",
            "title": "Parent with Invalid Subtask Empty Title",
            "subtasks": [
                "Valid subtask one",
                "",
                "Valid subtask three"
            ]
        }),
    );
    assert!(
        err_empty,
        "gitpulse_add_task must fail loud when a child task has an empty title"
    );
    let err_str = res_empty.to_string();
    assert!(
        err_str.contains("title is empty") || err_str.contains("title must not be empty"),
        "Error message must explicitly mention title validation, got: {err_str}"
    );

    // 2. Inject invalid child task with multiline title (newlines are invalid for task titles)
    let (err_nl, res_nl) = mcp_call(
        "gitpulse_add_task",
        json!({
            "repo_path": repo_path,
            "task_id": "epic-fail-newline",
            "title": "Parent with Invalid Subtask Multiline Title",
            "subtasks": [
                "Valid subtask",
                "Invalid\nmultiline\nsubtask title"
            ]
        }),
    );
    assert!(
        err_nl,
        "gitpulse_add_task must fail loud when a child task has newlines in title"
    );
    let err_nl_str = res_nl.to_string();
    assert!(
        err_nl_str.contains("spans lines") || err_nl_str.contains("title"),
        "Error message must indicate title validation failure, got: {err_nl_str}"
    );

    // 3. Inject invalid child task with oversized title (> 300/500 bytes)
    let oversized_title = "x".repeat(600);
    let (err_oversized, res_oversized) = mcp_call(
        "gitpulse_add_task",
        json!({
            "repo_path": repo_path,
            "task_id": "epic-fail-oversized",
            "title": "Parent with Oversized Child Subtask",
            "subtasks": [
                oversized_title
            ]
        }),
    );
    assert!(
        err_oversized,
        "gitpulse_add_task must fail loud when a child task has an oversized title"
    );
    let err_over_str = res_oversized.to_string();
    assert!(
        err_over_str.contains("characters") || err_over_str.contains("bytes"),
        "Error message must describe character/byte limit exceeded, got: {err_over_str}"
    );

    // 4. Non-existent parent task ID in parent_task_id must fail loud
    let (err_nonexistent_parent, res_nonexistent_parent) = mcp_call(
        "gitpulse_add_task",
        json!({
            "repo_path": repo_path,
            "task_id": "subtask-orphan",
            "title": "Subtask Referencing Non-Existent Parent",
            "parent_task_id": "totally-fake-parent-id-that-does-not-exist"
        }),
    );
    assert!(
        err_nonexistent_parent,
        "gitpulse_add_task must fail loud when parent_task_id does not exist"
    );
    let err_parent_str = res_nonexistent_parent.to_string();
    assert!(
        err_parent_str.contains("not_found") || err_parent_str.contains("No task"),
        "Error message must indicate parent not found, got: {err_parent_str}"
    );
}

#[test]
fn test_adversarial_thread_key_validation_across_bus_and_mcp() {
    let repo = create_test_repo();
    let repo_path = repo.path().to_str().unwrap();

    // 1. Direct Rust API: withdraw_ask with empty string
    let err_wd_empty = withdraw_ask(repo_path, "agent-1", "", "Valid reason").unwrap_err();
    assert!(
        err_wd_empty.message().contains("thread_key must not be empty"),
        "withdraw_ask must reject empty thread_key, got: {}",
        err_wd_empty.message()
    );

    // 2. Direct Rust API: withdraw_ask with whitespace-only string
    let err_wd_ws = withdraw_ask(repo_path, "agent-1", "   \t\n  ", "Valid reason").unwrap_err();
    assert!(
        err_wd_ws.message().contains("thread_key must not be empty"),
        "withdraw_ask must reject whitespace-only thread_key, got: {}",
        err_wd_ws.message()
    );

    // 3. Direct Rust API: answer_ask with empty string
    let err_ans_empty = answer_ask(repo_path, "agent-1", "", "Valid answer").unwrap_err();
    assert!(
        err_ans_empty.message().contains("thread_key must not be empty"),
        "answer_ask must reject empty thread_key, got: {}",
        err_ans_empty.message()
    );

    // 4. Direct Rust API: answer_ask with whitespace-only string
    let err_ans_ws = answer_ask(repo_path, "agent-1", "   \r\n  ", "Valid answer").unwrap_err();
    assert!(
        err_ans_ws.message().contains("thread_key must not be empty"),
        "answer_ask must reject whitespace-only thread_key, got: {}",
        err_ans_ws.message()
    );

    // 5. MCP tool: gitpulse_inbox_withdraw with empty thread_key
    let (err_mcp_wd_empty, res_mcp_wd_empty) = mcp_call(
        "gitpulse_inbox_withdraw",
        json!({
            "repo_path": repo_path,
            "from_agent": "agent-tester",
            "thread_key": "",
            "reason": "Test reason"
        }),
    );
    assert!(
        err_mcp_wd_empty,
        "MCP gitpulse_inbox_withdraw must return isError: true on empty thread_key"
    );
    let wd_empty_str = res_mcp_wd_empty.to_string();
    assert!(
        wd_empty_str.contains("thread_key"),
        "MCP withdraw must report thread_key error, got: {wd_empty_str}"
    );

    // 6. MCP tool: gitpulse_inbox_withdraw with whitespace thread_key
    let (err_mcp_wd_ws, res_mcp_wd_ws) = mcp_call(
        "gitpulse_inbox_withdraw",
        json!({
            "repo_path": repo_path,
            "from_agent": "agent-tester",
            "thread_key": "    \t  ",
            "reason": "Test reason"
        }),
    );
    assert!(
        err_mcp_wd_ws,
        "MCP gitpulse_inbox_withdraw must return isError: true on whitespace thread_key"
    );
    let wd_ws_str = res_mcp_wd_ws.to_string();
    assert!(
        wd_ws_str.contains("thread_key"),
        "MCP withdraw must report thread_key error, got: {wd_ws_str}"
    );

    // 7. MCP tool: gitpulse_inbox_post with empty thread_key is rejected by schema validator
    let (err_mcp_post_empty, res_mcp_post_empty) = mcp_call(
        "gitpulse_inbox_post",
        json!({
            "repo_path": repo_path,
            "from_agent": "agent-tester",
            "kind": "reply",
            "thread_key": "",
            "body": "Test answer"
        }),
    );
    assert!(
        err_mcp_post_empty,
        "MCP gitpulse_inbox_post must return isError: true on empty thread_key"
    );
    let post_empty_str = res_mcp_post_empty.to_string();
    assert!(
        post_empty_str.contains("thread_key"),
        "MCP post must report thread_key error, got: {post_empty_str}"
    );

    // 8. post_thread_message normalizes whitespace-only thread_key to None (unthreaded broadcast)
    let msg_ws = gitpulse_lib::engine::agent_bus::post_thread_message(
        repo_path,
        "agent-tester",
        None,
        Some("   \t\n  "),
        "ask",
        "Test question unthreaded",
        None,
    )
    .expect("post_thread_message with whitespace thread_key normalizes to unthreaded");
    assert_eq!(
        msg_ws.thread_key, None,
        "Whitespace thread_key must be normalized to None"
    );
}
