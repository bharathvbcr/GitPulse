//! `gitpulse_merge_conflicts` as an agent meets it: advertised read-only, its
//! answer matches its declared schema, the three states reach the wire, and the
//! repository's object database is the same afterwards.

use super::*;
use crate::test_support::{git_in, repo_with_worktrees, write};
use std::path::Path;

fn commit_file(dir: &Path, file: &str, content: &str) {
    write(dir, file, content);
    git_in(dir, &["add", "-A"]);
    git_in(dir, &["commit", "-q", "-m", file]);
}

/// Every file under `<repo>/.git/objects`: name, size and bytes, in order.
fn object_store(repo: &Path) -> Vec<(String, u64, Vec<u8>)> {
    fn walk(dir: &Path, root: &Path, out: &mut Vec<(String, u64, Vec<u8>)>) {
        for entry in std::fs::read_dir(dir).unwrap() {
            let path = entry.unwrap().path();
            if path.is_dir() {
                walk(&path, root, out);
            } else {
                let bytes = std::fs::read(&path).unwrap();
                let name = path.strip_prefix(root).unwrap().display().to_string();
                out.push((name, bytes.len() as u64, bytes));
            }
        }
    }
    let root = repo.join(".git").join("objects");
    let mut out = Vec::new();
    walk(&root, &root, &mut out);
    out.sort();
    out
}

#[test]
fn it_is_advertised_as_a_read_only_tool_with_the_repo_path_argument_only() {
    let tool = tools()
        .into_iter()
        .find(|t| t["name"] == "gitpulse_merge_conflicts")
        .expect("advertised");
    assert_eq!(tool["annotations"]["readOnlyHint"], true);
    assert_eq!(tool["annotations"]["destructiveHint"], false);
    assert_eq!(tool["inputSchema"]["required"], json!(["repo_path"]));
    assert_eq!(tool["inputSchema"]["additionalProperties"], false);
}

#[test]
fn the_three_states_reach_the_wire_and_the_object_store_is_untouched() {
    let (main, _parent, linked) = repo_with_worktrees(3);
    let root = main.path().canonicalize().unwrap();
    commit_file(&linked[0], "seed.txt", "from lane 0");
    git_in(&linked[1], &["switch", "-q", "--detach"]);
    commit_file(&linked[1], "seed.txt", "from lane 1");
    commit_file(&linked[2], "elsewhere.txt", "lane 2");
    let before = object_store(&root);

    let arguments = json!({ "repo_path": root.to_str().unwrap() });
    let payload = handle_tool_call("gitpulse_merge_conflicts", &arguments).expect("call");

    let schema = tools()
        .into_iter()
        .find(|t| t["name"] == "gitpulse_merge_conflicts")
        .unwrap()["outputSchema"]
        .clone();
    let violations: Vec<String> = validate::validate(&payload, &schema)
        .iter()
        .map(validate::Violation::render)
        .collect();
    assert!(violations.is_empty(), "{violations:?}\n{payload}");
    assert_eq!(payload["ok"], true, "{payload}");
    assert_eq!(payload["truncated"], false);
    assert_eq!(payload["pairs_total"], 6);
    assert_eq!(payload["conflict"], 1, "{payload}");
    assert_eq!(payload["clean"], 5, "{payload}");
    let items = payload["items"].as_array().unwrap();
    let conflicted: Vec<&Value> = items.iter().filter(|i| i["state"] == "conflict").collect();
    assert_eq!(conflicted.len(), 1);
    assert_eq!(conflicted[0]["paths"], json!(["seed.txt"]));
    assert!(object_store(&root) == before, "the object store changed");
}

#[test]
fn a_path_that_is_not_a_repository_is_a_failed_read_and_not_an_empty_clean_one() {
    let dir = tempfile::tempdir().unwrap();
    let arguments = json!({ "repo_path": dir.path().to_str().unwrap() });
    let payload = handle_tool_call("gitpulse_merge_conflicts", &arguments).expect("call");
    assert_eq!(payload["ok"], false, "{payload}");
    assert!(!payload["error"].as_str().unwrap_or_default().is_empty());
    assert_eq!(payload["clean"], 0);
}
