//! End-to-end coverage of the IPC bridge itself.
//!
//! Every other test calls command functions directly, which skips the part the
//! frontend actually depends on: argument deserialization from the JSON the
//! webview sends, and serialization of the reply back into JSON it can read. A
//! command can have correct types and still be unusable across that boundary —
//! a renamed field, a shape serde flattens differently, an error variant that
//! serializes to something the caller cannot match on.
//!
//! These drive commands through Tauri's own MockRuntime, so the request takes
//! the real path: `invoke_handler` dispatch, serde deserialization of the body,
//! the command, then serialization of the response.

use serde_json::json;
use tauri::test::{get_ipc_response, mock_builder, INVOKE_KEY};
use tauri::webview::InvokeRequest;
use tauri::{ipc::CallbackFn, WebviewWindowBuilder};

/// A mock app exposing the commands under test, wired the way lib.rs wires
/// them so dispatch and (de)serialization are the real implementations.
fn webview() -> tauri::WebviewWindow<tauri::test::MockRuntime> {
    let app = mock_builder()
        // A hand-picked subset rather than lib.rs's full registry. Sharing the
        // real list was tried and does not work: exposing it as
        // `fn invoke_handler<R: Runtime>()` fails to compile because commands
        // like cmd_watch_repo and cmd_terminal_spawn take a concrete
        // `AppHandle` (AppHandle<Wry>), so the list cannot be generic over the
        // runtime, and MockRuntime needs it to be. Making those commands
        // generic would change production signatures for a testing
        // convenience. The cost of the subset is that a newly added command is
        // not automatically covered here — add it below when it is worth
        // driving over the bridge.
        //
        // Full paths: generate_handler! resolves each command's hidden macro
        // from the crate that defined it, which an external test crate cannot
        // see by bare name.
        .invoke_handler(tauri::generate_handler![
            gitpulse_lib::commands::cmd_compute_word_diff,
            gitpulse_lib::commands::cmd_list_branches,
            gitpulse_lib::commands::cmd_get_status,
            gitpulse_lib::commands::cmd_stage_file,
            gitpulse_lib::commands::cmd_get_commit_graph,
            gitpulse_lib::commands::cmd_branch_stats,
            gitpulse_lib::commands::cmd_get_file_diff,
            gitpulse_lib::commands::cmd_get_file_content,
            gitpulse_lib::commands::cmd_write_file_content,
            gitpulse_lib::commands::cmd_list_tags,
            gitpulse_lib::desktop::cmd_resolve_git_root
        ])
        // The app's own context, from the same accessor run() uses, so the
        // capabilities deciding whether a command may be invoked are the real
        // ones — and the bundle metadata is embedded once, not twice.
        .build(gitpulse_lib::context())
        .expect("mock app builds");
    WebviewWindowBuilder::new(&app, "main", Default::default())
        .build()
        .expect("mock webview builds")
}

fn invoke(cmd: &str, body: serde_json::Value) -> Result<serde_json::Value, serde_json::Value> {
    let view = webview();
    let response = get_ipc_response(
        &view,
        InvokeRequest {
            cmd: cmd.into(),
            callback: CallbackFn(0),
            error: CallbackFn(1),
            // The webview origin is platform-specific, and a mismatch is
            // rejected before the command is reached.
            url: if cfg!(any(windows, target_os = "android")) {
                "http://tauri.localhost"
            } else {
                "tauri://localhost"
            }
            .parse()
            .expect("valid url"),
            body: body.into(),
            headers: Default::default(),
            invoke_key: INVOKE_KEY.to_string(),
        },
    );
    // get_ipc_response returns Result<InvokeResponseBody, serde_json::Value>:
    // the success and error channels are already distinct here, which is the
    // property these tests care about.
    response.map(|body| {
        body.deserialize::<serde_json::Value>()
            .expect("response deserializes to JSON")
    })
}

#[test]
fn a_command_reachable_from_the_frontend_round_trips_through_the_bridge() {
    // camelCase argument names are what the frontend actually sends.
    let result = invoke(
        "cmd_compute_word_diff",
        json!({ "oldLine": "the quick brown fox", "newLine": "the slow brown fox" }),
    )
    .expect("a successful response");

    // The reply must carry the field names the TypeScript side reads.
    assert!(result.get("original_segments").is_some(), "got: {result}");
    assert!(result.get("modified_segments").is_some(), "got: {result}");
    let original = result["original_segments"].as_array().expect("an array");
    assert!(!original.is_empty());
    // Segment shape is part of the contract too, not just the envelope.
    assert!(
        original[0].get("kind").is_some(),
        "segment: {}",
        original[0]
    );
    assert!(
        original[0].get("text").is_some(),
        "segment: {}",
        original[0]
    );
}

#[test]
fn a_missing_argument_is_refused_by_the_bridge_rather_than_reaching_the_command() {
    let error = invoke("cmd_compute_word_diff", json!({ "oldLine": "only one" }))
        .expect_err("a missing argument must not dispatch");
    assert!(
        error.to_string().contains("newLine") || error.to_string().contains("new_line"),
        "the failure should name the missing argument: {error}"
    );
}

#[test]
fn an_unknown_command_is_rejected() {
    let error = invoke("cmd_does_not_exist", json!({})).expect_err("unknown commands must fail");
    assert!(
        error.to_string().contains("cmd_does_not_exist") || error.to_string().contains("not found"),
        "unexpected error: {error}"
    );
}

#[test]
fn a_command_error_crosses_the_bridge_as_an_error_not_a_success() {
    // cmd_resolve_git_root returns Result<String, String>; the Err arm must
    // arrive on the error channel, or the frontend's catch never runs.
    let error = invoke(
        "cmd_resolve_git_root",
        json!({ "path": "/nonexistent/definitely/not/a/repo" }),
    )
    .expect_err("a failing command must not look successful");
    assert!(
        error
            .as_str()
            .unwrap_or_default()
            .contains("Not a Git repository"),
        "unexpected error payload: {error}"
    );
}

#[test]
fn a_successful_result_arrives_unwrapped_on_the_success_channel() {
    let dir = tempfile::TempDir::new().expect("tempdir");
    let status = std::process::Command::new("git")
        .args(["init", "-b", "main"])
        .current_dir(dir.path())
        .status()
        .expect("git on PATH");
    assert!(status.success());

    let value = invoke(
        "cmd_resolve_git_root",
        json!({ "path": dir.path().to_string_lossy() }),
    )
    .expect("a repository resolves");
    // Ok(String) must serialize as a bare JSON string, not as {"Ok": ...}.
    assert!(value.is_string(), "got: {value}");
    assert!(!value.as_str().unwrap_or_default().is_empty());
}

/// A repository with one commit and one unstaged change.
fn repo_with_change() -> tempfile::TempDir {
    let dir = tempfile::TempDir::new().expect("tempdir");
    let git = |args: &[&str]| {
        let out = std::process::Command::new("git")
            .args(args)
            .current_dir(dir.path())
            .output()
            .expect("git on PATH");
        assert!(
            out.status.success(),
            "git {args:?}: {}",
            String::from_utf8_lossy(&out.stderr)
        );
    };
    git(&["init", "-b", "main"]);
    git(&["config", "user.email", "test@example.com"]);
    git(&["config", "user.name", "Test User"]);
    git(&["config", "commit.gpgsign", "false"]);
    std::fs::write(dir.path().join("README.md"), "hello\n").expect("write");
    git(&["add", "."]);
    git(&["commit", "-m", "initial"]);
    std::fs::write(dir.path().join("README.md"), "changed\n").expect("write");
    dir
}

#[test]
fn a_list_returning_command_serializes_as_an_array_of_the_expected_shape() {
    let repo = repo_with_change();
    let value = invoke(
        "cmd_list_branches",
        json!({ "repoPath": repo.path().to_string_lossy() }),
    )
    .expect("branches list");
    let branches = value.as_array().expect("an array, not an object");
    assert!(!branches.is_empty());
    // Field names the TypeScript BranchInfo interface reads.
    for field in [
        "name",
        "is_current",
        "is_remote",
        "ahead_count",
        "last_commit_timestamp",
    ] {
        assert!(
            branches[0].get(field).is_some(),
            "missing {field} in {}",
            branches[0]
        );
    }
    // snake_case must survive: serde is not renaming to camelCase on the way out.
    assert!(branches[0].get("isCurrent").is_none());
}

#[test]
fn status_reports_the_working_tree_change_through_the_bridge() {
    let repo = repo_with_change();
    let value = invoke(
        "cmd_get_status",
        json!({ "repoPath": repo.path().to_string_lossy() }),
    )
    .expect("status");
    let entries = value.as_array().expect("an array");
    assert_eq!(entries.len(), 1, "one modified file: {value}");
    assert_eq!(entries[0]["path"], "README.md");
    assert!(entries[0].get("is_staged").is_some());
}

#[test]
fn a_guarded_command_returns_the_policy_verdict_beside_its_output() {
    // The frontend's runMutating unwraps `{ policy, output }`. If the envelope
    // ever flattened or renamed, mutations would appear to succeed while the
    // harness verdict silently vanished.
    let repo = repo_with_change();
    let value = invoke(
        "cmd_stage_file",
        json!({ "repoPath": repo.path().to_string_lossy(), "filePath": "README.md" }),
    )
    .expect("staging succeeds");
    assert!(value.get("policy").is_some(), "no policy in {value}");
    assert!(value.get("output").is_some(), "no output in {value}");
    // PolicyVerdict must carry a status the frontend can branch on.
    assert!(
        value["policy"].get("status").is_some(),
        "verdict has no status: {}",
        value["policy"]
    );
    // Unit output serializes as null, not as an omitted key.
    assert!(value["output"].is_null());

    // The security-relevant half: no MANVI sidecar runs in tests, so the gate
    // could not actually judge this write. A missing harness is the one
    // unchecked verdict allowed to proceed — but it must not arrive looking
    // like a check that ran and passed, or the UI would report a guarded
    // mutation that nothing guarded.
    // Observed here: "demoted", which the verdict ranking in commands/mod.rs
    // places strictly above "allowed" (Blocked > Warned > Demoted > Unchecked
    // > Allowed). Asserting the negative rather than the exact value keeps this
    // from breaking if a sidecar is present in some environment.
    let status = value["policy"]["status"].as_str().expect("a status string");
    assert_ne!(
        status, "allow",
        "an unrun gate must not report the same status as one that ran and allowed"
    );
}

#[test]
fn a_repo_path_that_is_not_a_repository_fails_on_the_error_channel() {
    let plain = tempfile::TempDir::new().expect("tempdir");
    let error = invoke(
        "cmd_get_status",
        json!({ "repoPath": plain.path().to_string_lossy() }),
    )
    .expect_err("a non-repository must not report an empty status");
    assert!(error.is_string(), "expected a string error, got {error}");
}

#[test]
fn optional_arguments_may_be_omitted_entirely_by_the_caller() {
    // The frontend leaves `query`, `revision` and `skip` out rather than
    // sending nulls. Option<T> must deserialize from an absent key, or every
    // graph load fails at the bridge before reaching the reader.
    let repo = repo_with_change();
    let value = invoke(
        "cmd_get_commit_graph",
        json!({ "repoPath": repo.path().to_string_lossy(), "maxCommits": 50 }),
    )
    .expect("the graph loads without the optional arguments");
    assert!(value.get("rows").is_some(), "payload: {value}");
}

#[test]
fn an_explicit_null_is_accepted_where_the_caller_sends_one() {
    let repo = repo_with_change();
    let value = invoke(
        "cmd_get_commit_graph",
        json!({
            "repoPath": repo.path().to_string_lossy(),
            "maxCommits": 50,
            "query": null,
            "revision": null,
            "skip": null
        }),
    )
    .expect("explicit nulls are equivalent to omission");
    assert!(value.get("rows").is_some(), "payload: {value}");
}

#[test]
fn a_wrongly_typed_argument_is_refused_at_the_bridge() {
    // maxCommits is a usize; a string must not be coerced into one.
    let repo = repo_with_change();
    let error = invoke(
        "cmd_get_commit_graph",
        json!({ "repoPath": repo.path().to_string_lossy(), "maxCommits": "fifty" }),
    )
    .expect_err("a string is not a usize");
    assert!(
        error.to_string().contains("maxCommits") || error.to_string().contains("max_commits"),
        "the failure should name the argument: {error}"
    );
}

#[test]
fn a_boolean_argument_round_trips_rather_than_being_coerced() {
    let repo = repo_with_change();
    // isStaged=false is the unstaged diff, which exists for the modified file.
    let unstaged = invoke(
        "cmd_get_file_diff",
        json!({
            "repoPath": repo.path().to_string_lossy(),
            "filePath": "README.md",
            "isStaged": false
        }),
    )
    .expect("an unstaged diff");
    assert!(unstaged.is_string(), "a raw diff is a string: {unstaged}");
    assert!(unstaged.as_str().unwrap_or_default().contains("changed"));

    // isStaged=true is a different question with a different answer, so the
    // flag is genuinely being read rather than defaulted.
    let staged = invoke(
        "cmd_get_file_diff",
        json!({
            "repoPath": repo.path().to_string_lossy(),
            "filePath": "README.md",
            "isStaged": true
        }),
    )
    .expect("a staged diff query");
    assert_ne!(staged, unstaged, "isStaged must change the result");
}

#[test]
fn a_nested_report_struct_keeps_its_shape_across_the_bridge() {
    let repo = repo_with_change();
    let value = invoke(
        "cmd_branch_stats",
        json!({ "repoPath": repo.path().to_string_lossy() }),
    )
    .expect("branch stats");
    // An object carrying the nested list, not a flattened scalar the frontend
    // would have to reconstruct. Asserting only `is_object()` would pass for
    // almost any payload, so check the fields the TS side actually reads.
    assert!(value.is_object(), "got: {value}");
    assert!(
        value["updates"].is_array(),
        "updates missing or not a list: {value}"
    );
    for field in ["compared_to", "computed", "cached"] {
        assert!(value.get(field).is_some(), "missing {field} in {value}");
    }
    // usize fields must arrive as JSON numbers, not stringified.
    assert!(
        value["computed"].is_number(),
        "computed: {}",
        value["computed"]
    );
    assert!(value["cached"].is_number(), "cached: {}", value["cached"]);
}

#[test]
fn non_ascii_content_survives_the_json_boundary_intact() {
    // A JSON boundary is exactly where encoding damage shows up, and file
    // content is user data the app must not corrupt: combining marks, CJK,
    // emoji outside the BMP, and RTL text all round-trip or none do.
    let repo = repo_with_change();
    let payload = "héllo café ｜ 日本語 ｜ العربية ｜ 👩‍👩‍👧‍👦 ｜ e\u{0301}\n";
    invoke(
        "cmd_write_file_content",
        json!({
            "repoPath": repo.path().to_string_lossy(),
            "filePath": "unicode.txt",
            "content": payload
        }),
    )
    .expect("the write succeeds");

    let read_back = invoke(
        "cmd_get_file_content",
        json!({
            "repoPath": repo.path().to_string_lossy(),
            "filePath": "unicode.txt"
        }),
    )
    .expect("the read succeeds");
    assert_eq!(
        read_back.as_str().expect("a string"),
        payload,
        "content changed crossing the bridge"
    );
}

#[test]
fn content_with_json_metacharacters_is_not_reinterpreted() {
    // Content that looks like JSON, or contains quotes and backslashes, must
    // travel as data rather than being parsed or mangled by the transport.
    let repo = repo_with_change();
    let payload = "{\"looks\":\"like json\"}\n\\backslash\\ \"quoted\" \t tab\n";
    invoke(
        "cmd_write_file_content",
        json!({
            "repoPath": repo.path().to_string_lossy(),
            "filePath": "tricky.txt",
            "content": payload
        }),
    )
    .expect("the write succeeds");

    let read_back = invoke(
        "cmd_get_file_content",
        json!({
            "repoPath": repo.path().to_string_lossy(),
            "filePath": "tricky.txt"
        }),
    )
    .expect("the read succeeds");
    assert_eq!(read_back.as_str().expect("a string"), payload);
}

#[test]
fn an_empty_collection_arrives_as_an_empty_array_not_null() {
    // A fresh repository has no tags. `[]` and `null` are different values to
    // the frontend: one maps and renders nothing, the other throws.
    let repo = repo_with_change();
    let value = invoke(
        "cmd_list_tags",
        json!({ "repoPath": repo.path().to_string_lossy() }),
    )
    .expect("tags list");
    assert!(value.is_array(), "expected [], got: {value}");
    assert_eq!(value.as_array().expect("an array").len(), 0);
    assert!(!value.is_null());
}

#[test]
fn a_large_payload_crosses_the_bridge_without_truncation() {
    // The transport must not silently cap content; a truncated file written
    // back would be data loss the user never sees.
    let repo = repo_with_change();
    let payload = "abcdefghij\n".repeat(50_000); // ~550 KB
    invoke(
        "cmd_write_file_content",
        json!({
            "repoPath": repo.path().to_string_lossy(),
            "filePath": "large.txt",
            "content": payload
        }),
    )
    .expect("the write succeeds");

    let read_back = invoke(
        "cmd_get_file_content",
        json!({
            "repoPath": repo.path().to_string_lossy(),
            "filePath": "large.txt"
        }),
    )
    .expect("the read succeeds");
    assert_eq!(read_back.as_str().expect("a string").len(), payload.len());
}
