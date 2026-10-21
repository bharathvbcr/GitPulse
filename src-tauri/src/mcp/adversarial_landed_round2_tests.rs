//! Adversarial review (round 2) of the `gitpulse_landed` MCP surface: argument
//! validation at the trust boundary and what a hostile revision can do once it
//! is let through.

use super::*;
use crate::procguard::LockedSpawn;
use std::process::Command;

fn repo_with_commit() -> (tempfile::TempDir, String, String) {
    let dir = tempfile::tempdir().expect("tempdir");
    let repo = dir.path().to_string_lossy().into_owned();
    let git = |args: &[&str]| {
        let out = Command::new("git")
            .args(args)
            .current_dir(&repo)
            .output_locked()
            .expect("git");
        assert!(out.status.success(), "git {args:?}: {out:?}");
        String::from_utf8_lossy(&out.stdout).trim().to_string()
    };
    git(&["init", "-q", "-b", "main"]);
    git(&["config", "user.name", "T"]);
    git(&["config", "user.email", "t@e.com"]);
    git(&["config", "commit.gpgsign", "false"]);
    crate::test_support::trust_repo(dir.path());
    std::fs::write(dir.path().join("a.txt"), "a\n").unwrap();
    git(&["add", "-A"]);
    git(&["commit", "-q", "-m", "root"]);
    let sha = git(&["rev-parse", "HEAD"]);
    (dir, repo, sha)
}

#[test]
fn oversize_unknown_and_empty_arguments_are_refused_before_git_runs() {
    let (_dir, repo, sha) = repo_with_commit();
    let long = "a".repeat(4_097);
    let cases = [
        json!({"repo_path": repo, "commit": long}),
        json!({"repo_path": repo, "commit": sha, "base": long}),
        json!({"repo_path": repo, "commit": ""}),
        json!({"repo_path": repo, "commit": sha, "base": ""}),
        json!({"repo_path": repo, "commit": sha, "extra": 1}),
        json!({"repo_path": repo}),
        json!({"repo_path": repo, "commit": 7}),
    ];
    for arguments in cases {
        assert!(
            validate_arguments("gitpulse_landed", &arguments).is_err(),
            "{arguments} was accepted"
        );
    }
}

#[test]
fn hostile_revisions_are_unresolvable_and_never_an_answer() {
    let (_dir, repo, sha) = repo_with_commit();
    let hostile = [
        "--output=/tmp/adv-landed-pwned".to_string(),
        "-h".to_string(),
        "\0".to_string(),
        format!("{sha}\0"),
        format!("{sha}\n{sha}"),
        format!("{sha}\n"),
        format!("{sha}^{{tree}}"),
        "HEAD:a.txt".to_string(),
        ":/root".to_string(),
        "@{-1}".to_string(),
    ];
    for commit in hostile {
        for base in [None, Some("-x".to_string()), Some(format!("{sha}\n{sha}"))] {
            let mut arguments = json!({"repo_path": repo, "commit": commit});
            if let Some(base) = &base {
                arguments["base"] = json!(base);
            }
            if validate_arguments("gitpulse_landed", &arguments).is_err() {
                continue;
            }
            let answer = handle_tool_call("gitpulse_landed", &arguments)
                .unwrap_or_else(|e| panic!("{commit:?}/{base:?}: handler errored: {e}"));
            // A hostile revision may resolve to something real (`:/root`), but a
            // multi-line or option-shaped one must not be read as two commits.
            if commit.contains('\n') || commit.starts_with('-') || base.is_some() {
                assert_eq!(
                    answer["landed"],
                    Value::Null,
                    "{commit:?}/{base:?} was answered: {answer}"
                );
                assert_eq!(answer["reason"], "unresolvable", "{answer}");
            }
        }
    }
    assert!(!std::path::Path::new("/tmp/adv-landed-pwned").exists());
}

#[test]
fn the_tool_is_advertised_read_only_and_with_a_closed_input_schema() {
    let tool = catalog()
        .iter()
        .find(|t| t["name"] == "gitpulse_landed")
        .expect("advertised")
        .clone();
    assert_eq!(tool["annotations"]["readOnlyHint"], true);
    assert_eq!(tool["inputSchema"]["additionalProperties"], false);
    assert_eq!(
        tool["inputSchema"]["required"],
        json!(["repo_path", "commit"])
    );
}
