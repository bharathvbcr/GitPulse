//! Adversarial review of card 15 (S3-11 + S6-06), MCP half: the live
//! `gitpulse_landed` and `gitpulse_provenance` payloads against their
//! advertised `outputSchema` on every fixture, and the read-only claim.

use super::*;
use crate::procguard::LockedSpawn;
use std::process::Command;

fn git(repo: &str, args: &[&str]) -> String {
    let out = Command::new("git")
        .args(args)
        .current_dir(repo)
        .output_locked()
        .expect("git");
    assert!(out.status.success(), "git {args:?}: {out:?}");
    String::from_utf8_lossy(&out.stdout).trim().to_string()
}

fn repo_with_root() -> (tempfile::TempDir, String) {
    let dir = tempfile::tempdir().expect("tempdir");
    let repo = dir.path().to_string_lossy().into_owned();
    git(&repo, &["init", "-q", "-b", "main"]);
    git(&repo, &["config", "user.name", "T"]);
    git(&repo, &["config", "user.email", "t@e.com"]);
    git(&repo, &["config", "commit.gpgsign", "false"]);
    crate::test_support::trust_repo(dir.path());
    put(&repo, "root.txt", "root\n", "root");
    (dir, repo)
}

fn put(repo: &str, file: &str, content: &str, message: &str) -> String {
    std::fs::write(std::path::Path::new(repo).join(file), content).unwrap();
    git(repo, &["add", "-A"]);
    git(repo, &["commit", "-q", "-m", message]);
    git(repo, &["rev-parse", "HEAD"])
}

/// `(label, repository, commit, base)` for every landing shape and failure.
fn fixtures() -> (
    Vec<tempfile::TempDir>,
    Vec<(String, String, String, Option<String>)>,
) {
    let mut keep = Vec::new();
    let mut cases = Vec::new();
    let mut add =
        |label: &str, dir: tempfile::TempDir, repo: String, commit: String, base: Option<&str>| {
            keep.push(dir);
            cases.push((label.to_string(), repo, commit, base.map(str::to_string)));
        };

    // ff
    let (dir, repo) = repo_with_root();
    git(&repo, &["checkout", "-q", "-b", "f"]);
    let c = put(&repo, "a.txt", "1\n", "c1");
    git(&repo, &["checkout", "-q", "main"]);
    git(&repo, &["merge", "-q", "--ff-only", "f"]);
    add("ff", dir, repo, c, Some("main"));

    // merge
    let (dir, repo) = repo_with_root();
    git(&repo, &["checkout", "-q", "-b", "f"]);
    let c = put(&repo, "a.txt", "1\n", "c1");
    git(&repo, &["checkout", "-q", "main"]);
    put(&repo, "m.txt", "m\n", "main moves");
    git(&repo, &["merge", "-q", "--no-ff", "--no-edit", "f"]);
    add("merge", dir, repo, c, Some("main"));

    // squash
    let (dir, repo) = repo_with_root();
    git(&repo, &["checkout", "-q", "-b", "f"]);
    put(&repo, "a.txt", "1\n", "c1");
    let c = put(&repo, "b.txt", "2\n", "c2");
    git(&repo, &["checkout", "-q", "main"]);
    git(&repo, &["merge", "-q", "--squash", "f"]);
    git(&repo, &["commit", "-q", "-m", "squash"]);
    add("squash", dir, repo, c, Some("main"));

    // rebase
    let (dir, repo) = repo_with_root();
    git(&repo, &["checkout", "-q", "-b", "f"]);
    put(&repo, "a.txt", "1\n", "c1");
    let c = put(&repo, "b.txt", "2\n", "c2");
    git(&repo, &["checkout", "-q", "main"]);
    put(&repo, "m.txt", "m\n", "main moves");
    git(&repo, &["cherry-pick", "f~1", "f"]);
    add("rebase", dir, repo, c, Some("main"));

    // unmerged
    let (dir, repo) = repo_with_root();
    git(&repo, &["checkout", "-q", "-b", "f"]);
    let c = put(&repo, "a.txt", "1\n", "c1");
    git(&repo, &["checkout", "-q", "main"]);
    add("unmerged", dir, repo, c, Some("main"));

    // unrelated history
    let (dir, repo) = repo_with_root();
    git(&repo, &["checkout", "-q", "--orphan", "island"]);
    let c = put(&repo, "i.txt", "i\n", "island");
    git(&repo, &["checkout", "-q", "main"]);
    add("unrelated", dir, repo, c, Some("main"));

    // default base
    let (dir, repo) = repo_with_root();
    let c = git(&repo, &["rev-parse", "HEAD"]);
    add("default base", dir, repo, c, None);

    // unresolvable commit / base
    let (dir, repo) = repo_with_root();
    add("no such commit", dir, repo, "0".repeat(40), Some("main"));
    let (dir, repo) = repo_with_root();
    let c = git(&repo, &["rev-parse", "HEAD"]);
    add("no such base", dir, repo, c, Some("no-such-branch"));

    // option-looking input must stay an operand
    let (dir, repo) = repo_with_root();
    add(
        "option-like commit",
        dir,
        repo,
        "--output=/etc/gitpulse-should-not-exist".into(),
        Some("main"),
    );
    let (dir, repo) = repo_with_root();
    let c = git(&repo, &["rev-parse", "HEAD"]);
    add("option-like base", dir, repo, c, Some("--all"));

    // shallow clone, a commit below the boundary and an unmerged branch tip
    let (origin_dir, origin) = repo_with_root();
    git(&origin, &["checkout", "-q", "-b", "feature"]);
    let feature = put(&origin, "f.txt", "f\n", "feature");
    git(&origin, &["checkout", "-q", "main"]);
    let old = put(&origin, "old.txt", "o\n", "old");
    put(&origin, "new.txt", "n\n", "new");
    let clone_dir = tempfile::tempdir().expect("tempdir");
    let clone = clone_dir.path().to_string_lossy().into_owned();
    let out = Command::new("git")
        .args(["clone", "-q", "--depth", "1", "--no-single-branch"])
        .arg(format!("file://{origin}"))
        .arg(&clone)
        .output_locked()
        .expect("clone");
    assert!(out.status.success(), "{out:?}");
    crate::test_support::trust_repo(clone_dir.path());
    keep.push(origin_dir);
    cases.push((
        "shallow, below the boundary".into(),
        clone.clone(),
        old,
        Some("origin/main".into()),
    ));
    cases.push((
        "shallow, unmerged tip".into(),
        clone,
        feature,
        Some("origin/main".into()),
    ));
    keep.push(clone_dir);
    (keep, cases)
}

fn schema_of(name: &str) -> Value {
    tools()
        .into_iter()
        .find(|tool| tool["name"] == name)
        .unwrap_or_else(|| panic!("{name} not advertised"))["outputSchema"]
        .clone()
}

/// Every key the payload has is a property the schema names (the schema does
/// not close the object, so a missing declaration would otherwise pass), and
/// every property it requires is present.
fn assert_conforms(label: &str, payload: &Value, schema: &Value) {
    let violations: Vec<String> = validate::validate(payload, schema)
        .iter()
        .map(validate::Violation::render)
        .collect();
    assert!(violations.is_empty(), "{label}: {violations:?}\n{payload}");
    let properties = schema["properties"].as_object().expect("properties");
    for key in payload.as_object().expect("object").keys() {
        assert!(
            properties.contains_key(key),
            "{label}: undeclared field {key}"
        );
    }
    for required in schema["required"].as_array().expect("required") {
        assert!(
            payload.get(required.as_str().unwrap()).is_some(),
            "{label}: missing {required}"
        );
    }
}

#[test]
fn live_landed_and_provenance_payloads_meet_their_output_schemas_on_every_fixture() {
    let (_keep, cases) = fixtures();
    let landed_schema = schema_of("gitpulse_landed");
    let provenance_schema = schema_of("gitpulse_provenance");
    let mut seen = std::collections::BTreeSet::new();
    for (label, repo, commit, base) in &cases {
        let mut arguments = json!({ "repo_path": repo, "commit": commit });
        let mut provenance_arguments = json!({ "repo_path": repo, "commit_sha": commit });
        if let Some(base) = base {
            arguments["base"] = json!(base);
            provenance_arguments["base_branch"] = json!(base);
        }
        // An argument the boundary refuses never reaches the tool: that is an
        // answer too, and no payload to validate.
        if validate_arguments("gitpulse_landed", &arguments).is_err() {
            println!("OUTSCHEMA {label}: refused at the argument boundary");
            continue;
        }
        let landed = handle_tool_call("gitpulse_landed", &arguments)
            .unwrap_or_else(|e| panic!("{label}: landed errored: {e}"));
        assert_conforms(&format!("landed/{label}"), &landed, &landed_schema);
        println!(
            "OUTSCHEMA {label}: landed={} via={} error={:?}",
            landed["landed"], landed["via"], landed["error"]
        );
        // The enum is the schema's own claim about `via`.
        let via = &landed["via"];
        assert!(
            via.is_null()
                || ["ff", "merge", "squash", "rebase", "tree"].contains(&via.as_str().unwrap()),
            "{label}: via {via}"
        );
        // null landed always says why; true/false never carries a null-reason.
        if landed["landed"].is_null() {
            assert!(
                !landed["error"].as_str().unwrap().is_empty(),
                "{label}: {landed}"
            );
        }
        seen.insert(landed["landed"].to_string());

        if validate_arguments("gitpulse_provenance", &provenance_arguments).is_ok() {
            let provenance = handle_tool_call("gitpulse_provenance", &provenance_arguments)
                .unwrap_or_else(|e| panic!("{label}: provenance errored: {e}"));
            assert_conforms(
                &format!("provenance/{label}"),
                &provenance,
                &provenance_schema,
            );
            // A null relation always says why; a measured one never has a reason.
            let measured = !provenance["landed_on_base"].is_null();
            assert_eq!(
                provenance["base_relation_reason"]
                    .as_str()
                    .unwrap()
                    .is_empty(),
                measured,
                "{label}: {provenance}"
            );
            assert_eq!(
                provenance["on_base"], provenance["landed_on_base"],
                "{label}"
            );
        }
    }
    // The fixtures reached all three answers: true, false and null.
    for answer in ["true", "false", "null"] {
        assert!(
            seen.contains(answer),
            "no fixture answered {answer}: {seen:?}"
        );
    }
}

#[test]
fn gitpulse_landed_is_read_only_everywhere_the_server_says_so() {
    let tool = tools()
        .into_iter()
        .find(|t| t["name"] == "gitpulse_landed")
        .expect("advertised");
    assert_eq!(tool["annotations"]["readOnlyHint"], true);
    assert_eq!(tool["annotations"]["destructiveHint"], false, "{tool}");
    let contents = resources::read("gitpulse://server/manifest").expect("manifest");
    let manifest: Value =
        serde_json::from_str(contents[0]["text"].as_str().expect("text")).expect("json");
    let writes = manifest["writes"].as_array().expect("writes");
    assert!(
        !writes.iter().any(|name| name == "gitpulse_landed"),
        "{writes:?}"
    );
    // And it is in the manifest's tool list at all.
    let listed = manifest["tools"]
        .as_array()
        .map(|tools| {
            tools
                .iter()
                .any(|t| t == "gitpulse_landed" || t["name"] == "gitpulse_landed")
        })
        .unwrap_or(true);
    assert!(
        listed,
        "gitpulse_landed missing from the manifest's tools: {manifest}"
    );
}
