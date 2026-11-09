//! Adversarial empirical challenge suite for Milestone 4 Iteration 2:
//! Stress-testing UTF-8 character boundary truncation in agent_bus:
//! 1. Multi-byte UTF-8 string truncation across boundary offsets 54, 55, 56, 57, 58, 59, 60, 61 with:
//!    - 4-byte emojis (🚀, 👨‍👩‍👧‍👦, 🦀)
//!    - 3-byte CJK ideographs (中, 文, 漢)
//!    - 2-byte Latin/Greek/Cyrillic characters (é, Ω, Ж)
//!    - Strings with combining characters and zero-width joiners
//!    - 0-byte, 1-byte, exactly 57-char, 60-char, 61-char strings
//! 2. Verification that `list_inbox_threads` and MCP inbox tools NEVER panic under any UTF-8 input.

mod common;

use gitpulse_lib::engine::agent_bus::{list_inbox_threads, post_thread_message};
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
        "io.modelcontextprotocol/clientInfo": { "name": "gitpulse-challenger-m4-iter2", "version": "1.0" },
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
fn test_multibyte_utf8_truncation_across_boundary_offsets_54_to_61() {
    let repo = create_test_repo();
    let repo_path = repo.path().to_str().unwrap();

    // Offsets 54, 55, 56, 57, 58, 59, 60, 61
    for offset in 54..=61 {
        // 1. 4-byte emoji 🚀
        let body_rocket = format!(
            "{}🚀{}",
            "A".repeat(offset),
            "tail_exceeding_the_sixty_characters_limit_by_a_comfortable_margin"
        );
        let key_rocket = format!("t-rocket-{}", offset);
        post_thread_message(
            repo_path,
            "agent-challenger",
            None,
            Some(&key_rocket),
            "note",
            &body_rocket,
            None,
        )
        .expect("post rocket message");

        // 2. 4-byte emoji 🦀
        let body_crab = format!(
            "{}🦀{}",
            "B".repeat(offset),
            "tail_exceeding_the_sixty_characters_limit_by_a_comfortable_margin"
        );
        let key_crab = format!("t-crab-{}", offset);
        post_thread_message(
            repo_path,
            "agent-challenger",
            None,
            Some(&key_crab),
            "note",
            &body_crab,
            None,
        )
        .expect("post crab message");

        // 3. Multi-character ZWJ emoji family 👨‍👩‍👧‍👦 (7 chars, 25 bytes)
        let body_family = format!(
            "{}👨‍👩‍👧‍👦{}",
            "C".repeat(offset),
            "tail_exceeding_the_sixty_characters_limit_by_a_comfortable_margin"
        );
        let key_family = format!("t-family-{}", offset);
        post_thread_message(
            repo_path,
            "agent-challenger",
            None,
            Some(&key_family),
            "note",
            &body_family,
            None,
        )
        .expect("post family message");

        // 4. 3-byte CJK ideographs 中, 文, 漢
        let body_cjk = format!(
            "{}中文漢{}",
            "D".repeat(offset),
            "tail_exceeding_the_sixty_characters_limit_by_a_comfortable_margin"
        );
        let key_cjk = format!("t-cjk-{}", offset);
        post_thread_message(
            repo_path,
            "agent-challenger",
            None,
            Some(&key_cjk),
            "note",
            &body_cjk,
            None,
        )
        .expect("post cjk message");

        // 5. 2-byte Latin/Greek/Cyrillic characters é, Ω, Ж
        let body_eur = format!(
            "{}éΩЖ{}",
            "E".repeat(offset),
            "tail_exceeding_the_sixty_characters_limit_by_a_comfortable_margin"
        );
        let key_eur = format!("t-eur-{}", offset);
        post_thread_message(
            repo_path,
            "agent-challenger",
            None,
            Some(&key_eur),
            "note",
            &body_eur,
            None,
        )
        .expect("post european message");

        // 6. Decomposed combining characters: e + acute, n + tilde, a + diaeresis
        let body_combining = format!(
            "{}e\u{0301}n\u{0303}a\u{0308}{}",
            "F".repeat(offset),
            "tail_exceeding_the_sixty_characters_limit_by_a_comfortable_margin"
        );
        let key_combining = format!("t-combining-{}", offset);
        post_thread_message(
            repo_path,
            "agent-challenger",
            None,
            Some(&key_combining),
            "note",
            &body_combining,
            None,
        )
        .expect("post combining message");

        // 7. ZWJ professions and skin tones: 👩‍💻, 🧑‍🔬, 👍🏽
        let body_zwj = format!(
            "{}👩‍💻🧑‍🔬👍🏽{}",
            "G".repeat(offset),
            "tail_exceeding_the_sixty_characters_limit_by_a_comfortable_margin"
        );
        let key_zwj = format!("t-zwj-{}", offset);
        post_thread_message(
            repo_path,
            "agent-challenger",
            None,
            Some(&key_zwj),
            "note",
            &body_zwj,
            None,
        )
        .expect("post zwj message");
    }

    // Now invoke list_inbox_threads and verify ZERO panics across all threads
    let threads = list_inbox_threads(repo_path, None, None)
        .expect("list_inbox_threads must never panic on multi-byte UTF-8");

    // 8 offsets * 7 payloads = 56 threads
    assert_eq!(
        threads.len(),
        56,
        "All 56 UTF-8 test threads must be retrieved"
    );

    for thread in &threads {
        assert!(
            thread.title.ends_with('…'),
            "Truncated thread title must end with ellipsis: '{}' for key '{}'",
            thread.title,
            thread.thread_key
        );
        assert_eq!(
            thread.title.chars().count(),
            58,
            "Truncated thread title must contain exactly 57 chars + 1 ellipsis char: '{}'",
            thread.title
        );
    }
}

#[test]
fn test_exact_length_boundaries_0_1_57_60_61_char_strings() {
    let repo = create_test_repo();
    let repo_path = repo.path().to_str().unwrap();

    // 1. Post validation: 0-byte and whitespace-only strings are rejected by post_thread_message
    let err_empty = post_thread_message(
        repo_path,
        "agent-0",
        None,
        Some("thread-empty"),
        "note",
        "",
        None,
    )
    .unwrap_err();
    assert!(format!("{err_empty:?}").contains("message body must not be empty"));

    let err_ws = post_thread_message(
        repo_path,
        "agent-ws",
        None,
        Some("thread-whitespace"),
        "note",
        "   \t\n   ",
        None,
    )
    .unwrap_err();
    assert!(format!("{err_ws:?}").contains("message body must not be empty"));

    // 2. 1-byte ASCII string
    post_thread_message(
        repo_path,
        "agent-1",
        None,
        Some("thread-1byte"),
        "note",
        "X",
        None,
    )
    .expect("post 1byte message");

    // 3. 1-char 4-byte emoji string
    post_thread_message(
        repo_path,
        "agent-emoji1",
        None,
        Some("thread-1emoji"),
        "note",
        "🦀",
        None,
    )
    .expect("post 1emoji message");

    // 4. Exactly 57-char ASCII string (57 bytes)
    let text_57_ascii = "A".repeat(57);
    post_thread_message(
        repo_path,
        "agent-57a",
        None,
        Some("thread-57-ascii"),
        "note",
        &text_57_ascii,
        None,
    )
    .expect("post 57 ascii");

    // 5. Exactly 57-char multi-byte CJK string (171 bytes)
    let text_57_cjk = "中".repeat(57);
    assert_eq!(text_57_cjk.chars().count(), 57);
    assert_eq!(text_57_cjk.len(), 171);
    post_thread_message(
        repo_path,
        "agent-57cjk",
        None,
        Some("thread-57-cjk"),
        "note",
        &text_57_cjk,
        None,
    )
    .expect("post 57 cjk");

    // 6. Exactly 60-char ASCII string (60 bytes)
    let text_60_ascii = "B".repeat(60);
    post_thread_message(
        repo_path,
        "agent-60a",
        None,
        Some("thread-60-ascii"),
        "note",
        &text_60_ascii,
        None,
    )
    .expect("post 60 ascii");

    // 7. Exactly 60-char multi-byte CJK string (180 bytes)
    let text_60_cjk = "漢".repeat(60);
    assert_eq!(text_60_cjk.chars().count(), 60);
    assert_eq!(text_60_cjk.len(), 180);
    post_thread_message(
        repo_path,
        "agent-60cjk",
        None,
        Some("thread-60-cjk"),
        "note",
        &text_60_cjk,
        None,
    )
    .expect("post 60 cjk");

    // 8. Exactly 61-char ASCII string (61 bytes) -> triggers truncation!
    let text_61_ascii = "C".repeat(61);
    post_thread_message(
        repo_path,
        "agent-61a",
        None,
        Some("thread-61-ascii"),
        "note",
        &text_61_ascii,
        None,
    )
    .expect("post 61 ascii");

    // 9. Exactly 61-char multi-byte CJK string (183 bytes) -> triggers truncation!
    let text_61_cjk = "文".repeat(61);
    assert_eq!(text_61_cjk.chars().count(), 61);
    assert_eq!(text_61_cjk.len(), 183);
    post_thread_message(
        repo_path,
        "agent-61cjk",
        None,
        Some("thread-61-cjk"),
        "note",
        &text_61_cjk,
        None,
    )
    .expect("post 61 cjk");

    // 10. Exactly 61-char 4-byte emojis (244 bytes) -> triggers truncation!
    let text_61_emoji = "🚀".repeat(61);
    assert_eq!(text_61_emoji.chars().count(), 61);
    assert_eq!(text_61_emoji.len(), 244);
    post_thread_message(
        repo_path,
        "agent-61emoji",
        None,
        Some("thread-61-emoji"),
        "note",
        &text_61_emoji,
        None,
    )
    .expect("post 61 emoji");

    // 11. Now inject an external message record into messages.json with body: "" directly
    // to test that list_inbox_threads safely falls back to thread_key without panicking
    let lanes_dir = repo.path().join(".git").join("gitpulse").join("lanes");
    let messages_file = lanes_dir.join("messages.json");
    if messages_file.exists() {
        let content = std::fs::read_to_string(&messages_file).unwrap();
        let mut state: serde_json::Value = serde_json::from_str(&content).unwrap();
        if let Some(msgs) = state.get_mut("messages").and_then(|m| m.as_array_mut()) {
            msgs.push(json!({
                "seq": 9999,
                "from_agent": "external-agent",
                "to_agent": null,
                "thread_key": "thread-injected-empty",
                "kind": "note",
                "body": "",
                "branch": null,
                "created_at_ms": 1730959200000i64
            }));
            std::fs::write(&messages_file, serde_json::to_string(&state).unwrap()).unwrap();
        }
    }

    let threads =
        list_inbox_threads(repo_path, None, None).expect("must list threads without panic");
    assert_eq!(threads.len(), 10); // 9 posted + 1 injected

    let t_injected = threads
        .iter()
        .find(|t| t.thread_key == "thread-injected-empty")
        .unwrap();
    assert_eq!(t_injected.title, "thread-injected-empty");

    let t_1b = threads
        .iter()
        .find(|t| t.thread_key == "thread-1byte")
        .unwrap();
    assert_eq!(t_1b.title, "X");

    let t_1e = threads
        .iter()
        .find(|t| t.thread_key == "thread-1emoji")
        .unwrap();
    assert_eq!(t_1e.title, "🦀");

    let t_57a = threads
        .iter()
        .find(|t| t.thread_key == "thread-57-ascii")
        .unwrap();
    assert_eq!(t_57a.title, "A".repeat(57));
    assert_eq!(t_57a.title.chars().count(), 57);

    let t_57cjk = threads
        .iter()
        .find(|t| t.thread_key == "thread-57-cjk")
        .unwrap();
    assert_eq!(t_57cjk.title, "中".repeat(57));
    assert_eq!(t_57cjk.title.chars().count(), 57);

    let t_60a = threads
        .iter()
        .find(|t| t.thread_key == "thread-60-ascii")
        .unwrap();
    assert_eq!(t_60a.title, "B".repeat(60));
    assert_eq!(t_60a.title.chars().count(), 60);

    let t_60cjk = threads
        .iter()
        .find(|t| t.thread_key == "thread-60-cjk")
        .unwrap();
    assert_eq!(t_60cjk.title, "漢".repeat(60));
    assert_eq!(t_60cjk.title.chars().count(), 60);

    let t_61a = threads
        .iter()
        .find(|t| t.thread_key == "thread-61-ascii")
        .unwrap();
    assert_eq!(t_61a.title, format!("{}…", "C".repeat(57)));
    assert_eq!(t_61a.title.chars().count(), 58);

    let t_61cjk = threads
        .iter()
        .find(|t| t.thread_key == "thread-61-cjk")
        .unwrap();
    assert_eq!(t_61cjk.title, format!("{}…", "文".repeat(57)));
    assert_eq!(t_61cjk.title.chars().count(), 58);

    let t_61emoji = threads
        .iter()
        .find(|t| t.thread_key == "thread-61-emoji")
        .unwrap();
    assert_eq!(t_61emoji.title, format!("{}…", "🚀".repeat(57)));
    assert_eq!(t_61emoji.title.chars().count(), 58);
}

#[test]
fn test_mcp_inbox_tools_never_panic_under_utf8_stress() {
    let repo = create_test_repo();
    let repo_path = repo.path().to_str().unwrap();

    // 1. Post via MCP gitpulse_inbox_post with multi-byte text
    let multi_body = format!(
        "{}🚀 Crab 🦀 CJK 中文漢 Greek Ω Cyrillic Ж Family 👨‍👩‍👧‍👦 {}",
        "Prefix-".repeat(6),
        "Suffix_beyond_the_limit_to_force_truncation_in_snippet_generator"
    );

    let (err_post, res_post) = mcp_call(
        "gitpulse_inbox_post",
        json!({
            "repo_path": repo_path,
            "from_agent": "challenger-mcp",
            "thread_key": "mcp-utf8-thread",
            "kind": "ask",
            "body": multi_body
        }),
    );
    assert!(!err_post, "mcp gitpulse_inbox_post failed: {res_post}");
    assert_eq!(res_post["ok"], true);

    // 2. Read inbox threads via MCP gitpulse_inbox_read
    let (err_read, res_read) = mcp_call(
        "gitpulse_inbox_read",
        json!({
            "repo_path": repo_path,
            "thread_key": "mcp-utf8-thread"
        }),
    );
    assert!(!err_read, "mcp gitpulse_inbox_read failed: {res_read}");
    assert_eq!(res_read["ok"], true);
    let threads = res_read["threads"].as_array().expect("threads array");
    assert_eq!(threads.len(), 1);
    let thread = &threads[0];
    let title = thread["title"].as_str().expect("title str");
    assert!(
        title.ends_with('…'),
        "MCP thread title must be truncated safely with ellipsis"
    );
    assert_eq!(
        title.chars().count(),
        58,
        "MCP thread title must contain exactly 57 chars + 1 ellipsis char"
    );

    // 3. Read raw thread messages via MCP gitpulse_worktree_inbox
    let (err_wt_inbox, res_wt_inbox) = mcp_call(
        "gitpulse_worktree_inbox",
        json!({
            "repo_path": repo_path,
            "thread_key": "mcp-utf8-thread"
        }),
    );
    assert!(
        !err_wt_inbox,
        "mcp gitpulse_worktree_inbox failed: {res_wt_inbox}"
    );
    assert_eq!(res_wt_inbox["ok"], true);
    let msgs = res_wt_inbox["messages"].as_array().expect("messages array");
    assert_eq!(msgs.len(), 1);
    assert_eq!(msgs[0]["from_agent"], "challenger-mcp");

    // 4. Withdraw ask via MCP gitpulse_inbox_withdraw
    let (err_withdraw, res_withdraw) = mcp_call(
        "gitpulse_inbox_withdraw",
        json!({
            "repo_path": repo_path,
            "from_agent": "challenger-mcp",
            "thread_key": "mcp-utf8-thread",
            "reason": "Stress test verified with multi-byte text 🚀"
        }),
    );
    assert!(
        !err_withdraw,
        "mcp gitpulse_inbox_withdraw failed: {res_withdraw}"
    );
    assert_eq!(res_withdraw["ok"], true);

    // 5. Re-read to confirm status transition to withdrawn
    let (err_read2, res_read2) = mcp_call(
        "gitpulse_inbox_read",
        json!({
            "repo_path": repo_path,
            "status": "withdrawn"
        }),
    );
    assert!(!err_read2, "mcp gitpulse_inbox_read failed: {res_read2}");
    let withdrawn_threads = res_read2["threads"].as_array().expect("threads");
    assert_eq!(withdrawn_threads.len(), 1);
    assert_eq!(withdrawn_threads[0]["status"], "withdrawn");
}

#[test]
fn test_adversarial_malformed_json_and_exotic_scripts() {
    let repo = create_test_repo();
    let repo_path = repo.path().to_str().unwrap();

    // 1. Malformed JSON with multi-byte characters
    let malformed_json =
        "{ \"title\": \"Incomplete json string 🚀🦀中文éΩЖ and tail exceeding sixty chars";
    post_thread_message(
        repo_path,
        "agent-malformed",
        None,
        Some("t-malformed-json"),
        "note",
        malformed_json,
        None,
    )
    .expect("post malformed json");

    // 2. RTL scripts (Arabic and Hebrew) mixed with LTR
    let bidi_text = "مرحبا بالعالم 🚀 שלום עולם 🦀 Hebrew and Arabic mixed with English tail exceeding sixty chars";
    post_thread_message(
        repo_path,
        "agent-bidi",
        None,
        Some("t-bidi"),
        "note",
        bidi_text,
        None,
    )
    .expect("post bidi");

    // 3. Mathematical alphanumeric symbols (4 bytes each: U+1D56C = 𝕬, U+1D56D = 𝕭)
    let math_text = "𝕬𝕭𝕮𝕯 1234567890 1234567890 1234567890 1234567890 1234567890 1234567890";
    post_thread_message(
        repo_path,
        "agent-math",
        None,
        Some("t-math"),
        "note",
        math_text,
        None,
    )
    .expect("post math");

    let threads =
        list_inbox_threads(repo_path, None, None).expect("must list threads without panic");
    assert_eq!(threads.len(), 3);

    for t in &threads {
        assert!(t.title.ends_with('…'));
        assert_eq!(t.title.chars().count(), 58);
    }
}
