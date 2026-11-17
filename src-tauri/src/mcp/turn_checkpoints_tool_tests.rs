//! `gitpulse_turn_checkpoints` as an agent meets it: advertised read-only, its
//! answers match the declared schema, a list and a patch reach the wire, and
//! a read that could not run says so instead of answering with an empty list.

use super::*;
use crate::engine::turn_checkpoint as tc;
use crate::test_support::{git_in, git_repo, write};

fn schema() -> Value {
    tools()
        .into_iter()
        .find(|t| t["name"] == "gitpulse_turn_checkpoints")
        .expect("advertised")["outputSchema"]
        .clone()
}

fn conforms(payload: &Value) {
    let violations: Vec<String> = validate::validate(payload, &schema())
        .iter()
        .map(validate::Violation::render)
        .collect();
    assert!(violations.is_empty(), "{violations:?}\n{payload}");
}

fn take(path: &str, now_ms: u64) -> tc::CaptureResult {
    let index = tempfile::tempdir().unwrap();
    let cfg = tc::CaptureConfig {
        now_ms,
        index_dir: Some(index.path().to_path_buf()),
        ..tc::CaptureConfig::production()
    };
    tc::capture_turn_checkpoint(
        path,
        &tc::CaptureRequest {
            event: tc::TurnEvent::TurnEnd,
            session_id: Some("s1"),
            label: Some("done"),
        },
        &cfg,
    )
}

#[test]
fn it_is_advertised_read_only() {
    let tool = tools()
        .into_iter()
        .find(|t| t["name"] == "gitpulse_turn_checkpoints")
        .expect("advertised");
    assert_eq!(tool["annotations"]["readOnlyHint"], true);
    assert_eq!(tool["annotations"]["destructiveHint"], false);
    assert_eq!(tool["inputSchema"]["required"], json!(["repo_path"]));
}

#[test]
fn a_list_and_a_patch_reach_the_wire() {
    let repo = git_repo();
    let path = repo.path().to_str().unwrap().to_string();
    write(repo.path(), "seed.txt", "seed\n");
    git_in(repo.path(), &["add", "seed.txt"]);
    git_in(repo.path(), &["commit", "-q", "-m", "seed"]);
    write(repo.path(), "notes.txt", "hello\n");
    let first = take(&path, 50_000_000);
    assert_eq!(first.outcome, tc::CaptureOutcome::Captured, "{first:?}");

    let listed =
        handle_tool_call("gitpulse_turn_checkpoints", &json!({ "repo_path": path })).expect("call");
    conforms(&listed);
    assert_eq!(listed["ok"], true, "{listed}");
    assert_eq!(listed["total"], 1);
    assert_eq!(listed["truncated"], false);
    let entry = &listed["checkpoints"][0];
    assert_eq!(entry["event"], "turn_end");
    assert_eq!(entry["label"], "done");
    assert_eq!(entry["session_id"], "s1");
    assert!(entry["omissions"]["untracked_admitted"].as_u64().unwrap() >= 1);

    let id = entry["id"].as_str().unwrap();
    let patch = handle_tool_call(
        "gitpulse_turn_checkpoints",
        &json!({ "repo_path": path, "id": id }),
    )
    .expect("call");
    conforms(&patch);
    assert_eq!(patch["ok"], true, "{patch}");
    assert!(patch["diff"]["patch"].as_str().unwrap().contains("+hello"));
    assert_eq!(patch["diff"]["truncated"], false);

    let limited = handle_tool_call(
        "gitpulse_turn_checkpoints",
        &json!({ "repo_path": path, "limit": 1 }),
    )
    .expect("call");
    assert_eq!(limited["checkpoints"].as_array().unwrap().len(), 1);
}

#[test]
fn a_read_that_could_not_run_is_not_an_empty_list() {
    let repo = git_repo();
    let path = repo.path().to_str().unwrap().to_string();
    let unknown = handle_tool_call(
        "gitpulse_turn_checkpoints",
        &json!({ "repo_path": path, "id": "123" }),
    )
    .expect("call");
    conforms(&unknown);
    assert_eq!(unknown["ok"], false, "{unknown}");
    assert!(!unknown["error"].as_str().unwrap().is_empty());

    let orphan = handle_tool_call(
        "gitpulse_turn_checkpoints",
        &json!({ "repo_path": path, "against": "123" }),
    )
    .expect("call");
    assert_eq!(orphan["ok"], false);

    let dir = tempfile::tempdir().unwrap();
    let not_a_repo = handle_tool_call(
        "gitpulse_turn_checkpoints",
        &json!({ "repo_path": dir.path().to_str().unwrap() }),
    )
    .expect("call");
    conforms(&not_a_repo);
    assert_eq!(not_a_repo["ok"], false);

    let empty =
        handle_tool_call("gitpulse_turn_checkpoints", &json!({ "repo_path": path })).expect("call");
    assert_eq!(empty["ok"], true);
    assert_eq!(empty["total"], 0);
}

#[test]
fn the_arguments_are_bounded_at_the_wire() {
    let repo = git_repo();
    let path = repo.path().to_str().unwrap();
    for bad in [
        json!({ "repo_path": path, "limit": 51 }),
        json!({ "repo_path": path, "limit": 0 }),
        json!({ "repo_path": path, "id": "1".repeat(21) }),
        json!({ "repo_path": path, "unknown": true }),
    ] {
        assert!(
            validate_arguments("gitpulse_turn_checkpoints", &bad).is_err(),
            "{bad}"
        );
    }
}
