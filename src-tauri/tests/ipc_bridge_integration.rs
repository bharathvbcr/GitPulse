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
        // Full paths: generate_handler! resolves each command's hidden macro
        // from the crate that defined it, which an external test crate cannot
        // see by bare name.
        .invoke_handler(tauri::generate_handler![
            gitpulse_lib::commands::cmd_compute_word_diff,
            gitpulse_lib::desktop::cmd_resolve_git_root
        ])
        // The real context, not a mock one: it carries the capabilities that
        // decide whether a command is allowed to be invoked at all.
        .build(tauri::generate_context!())
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
