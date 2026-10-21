//! Adversarial review of the `gitpulse_merge_conflicts` contract: hostile and
//! oversized input is refused before any read, and the answer matches its
//! output schema in the truncated and all-unknown states too.

use super::*;
use crate::insights::merge_conflicts::merge_conflicts_within;
use crate::test_support::{git_in, repo_with_worktrees, write};
use std::time::Duration;

fn commit_file(dir: &std::path::Path, file: &str, content: &str) {
    write(dir, file, content);
    git_in(dir, &["add", "-A"]);
    git_in(dir, &["commit", "-q", "-m", file]);
}

fn violations(payload: &Value) -> Vec<String> {
    let schema = tools()
        .into_iter()
        .find(|t| t["name"] == "gitpulse_merge_conflicts")
        .unwrap()["outputSchema"]
        .clone();
    validate::validate(payload, &schema)
        .iter()
        .map(validate::Violation::render)
        .collect()
}

#[test]
fn hostile_and_oversized_input_is_refused_before_any_read() {
    for arguments in [
        json!({ "repo_path": "x".repeat(4097) }),
        json!({ "repo_path": "" }),
        json!({ "repo_path": 7 }),
        json!({ "repo_path": "/tmp", "extra": true }),
        json!({}),
        json!({ "repo_path": ["/tmp"] }),
    ] {
        assert!(
            validate_arguments("gitpulse_merge_conflicts", &arguments).is_err(),
            "{arguments} was accepted"
        );
    }
}

#[test]
fn a_hostile_repo_path_is_a_failed_read_never_a_clean_one() {
    for path in ["--help", "-C/", "/nonexistent/../../etc", "a\0b", "~", "."] {
        let payload = handle_tool_call("gitpulse_merge_conflicts", &json!({ "repo_path": path }));
        let Ok(payload) = payload else { continue };
        assert_eq!(payload["ok"], false, "{path:?}: {payload}");
        assert_eq!(payload["clean"], 0, "{path:?}");
        assert!(
            violations(&payload).is_empty(),
            "{path:?}: {:?}",
            violations(&payload)
        );
    }
}

#[test]
fn truncated_and_all_unknown_answers_match_the_output_schema() {
    let (main, _parent, linked) = repo_with_worktrees(3);
    let root = main.path().canonicalize().unwrap();
    for (n, lane) in linked.iter().enumerate() {
        commit_file(lane, &format!("lane{n}.txt"), "work\n");
    }
    let path = root.to_str().unwrap();
    let truncated = json!(merge_conflicts_within(path, 2, Duration::from_secs(120)));
    assert_eq!(truncated["truncated"], true);
    assert!(
        violations(&truncated).is_empty(),
        "{:?}\n{truncated}",
        violations(&truncated)
    );
    let unknown = json!(merge_conflicts_within(path, 64, Duration::ZERO));
    assert!(unknown["unknown"].as_u64().unwrap() > 0, "{unknown}");
    assert!(
        violations(&unknown).is_empty(),
        "{:?}\n{unknown}",
        violations(&unknown)
    );
}
