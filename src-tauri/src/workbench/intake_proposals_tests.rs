//! Reviewing Manvi's proposals: filing is idempotent and keeps the board's
//! related-task protection, applying is revisioned, and a sidecar that is down
//! or answers partially is shown as exactly that.

use super::tests::{git_repo, items, profile, registered, task};
use super::*;

const FP: &str = "0123456789abcdef";

fn setup() -> (tempfile::TempDir, Store, tempfile::TempDir) {
    let (dir, path) = profile();
    let store = Store::open(&path).unwrap();
    (dir, store, git_repo())
}

fn file_input(repo: &tempfile::TempDir, extra: Value) -> String {
    let mut input = json!({
        "repo_path": repo.path().to_str().unwrap(),
        "origin": {"kind": "scan", "fingerprint": FP},
        "title": "Remove the unreferenced helper quuxify",
        "description": "quuxify has no callers.",
        "labels": ["manvi-scan"],
    });
    for (key, value) in extra.as_object().unwrap() {
        input[key] = value.clone();
    }
    input.to_string()
}

fn board(store: &Store, repo: &tempfile::TempDir) -> Vec<Value> {
    let repository = registered(store, repo);
    items(store, &repository)
}

#[test]
fn filing_the_same_proposal_twice_files_one_card() {
    let (_d, store, repo) = setup();
    let first = file_proposal(&store, &file_input(&repo, json!({}))).unwrap();
    assert_eq!(first["outcome"], "filed", "{first}");
    // The double click, and the retry after a lost reply, and an edited
    // second accept: all find the card the first one made.
    for edited in [json!({}), json!({"title": "Quuxify is dead code"})] {
        let again = file_proposal(&store, &file_input(&repo, edited)).unwrap();
        assert_eq!(again["outcome"], "already_filed", "{again}");
        assert_eq!(again["item_id"], first["item_id"]);
    }
    let cards = board(&store, &repo);
    assert_eq!(cards.len(), 1, "{cards:?}");
    assert_eq!(cards[0]["status"], "inbox");
}

#[test]
fn a_filed_scan_proposal_keeps_the_fingerprint_the_next_scan_dedupes_on() {
    let (_d, store, repo) = setup();
    // The person edited the description and dropped the label; the marker and
    // label are not theirs to remove, or the next scan proposes it again.
    let filed = file_proposal(
        &store,
        &file_input(
            &repo,
            json!({"description": "Rewritten by hand.", "labels": []}),
        ),
    )
    .unwrap();
    let card = get_item(&store, filed["item_id"].as_str().unwrap())
        .unwrap()
        .unwrap();
    let labels = card["labels"].to_string();
    assert!(labels.contains(&format!("scan:{FP}")), "{labels}");
    assert!(
        card["description"]
            .as_str()
            .unwrap()
            .contains(&format!("manvi-scan: {FP}")),
        "{card}"
    );
    assert!(card["description"]
        .as_str()
        .unwrap()
        .contains("Rewritten by hand."));
}

#[test]
fn a_proposal_that_looks_like_open_work_is_not_filed_until_the_person_names_it() {
    let (_d, store, repo) = setup();
    let repository = registered(&store, &repo);
    let existing = place(
        &store,
        &repository,
        &task("gp-x", "Remove the unreferenced helper quuxify everywhere"),
        false,
        now_millis(),
    )
    .unwrap()
    .item_id;

    let held = file_proposal(&store, &file_input(&repo, json!({}))).unwrap();
    assert_eq!(held["ok"], false, "{held}");
    assert_eq!(held["outcome"], "related_tasks_exist");
    assert_eq!(held["related"][0]["item_id"], existing);
    assert_eq!(board(&store, &repo).len(), 1, "nothing was filed");

    let confirmed = file_proposal(
        &store,
        &file_input(&repo, json!({"reviewed_related": [existing]})),
    )
    .unwrap();
    assert_eq!(confirmed["outcome"], "filed", "{confirmed}");
    assert_eq!(board(&store, &repo).len(), 2);
}

#[test]
fn split_parts_are_one_card_each_however_often_they_are_accepted() {
    let (_d, store, repo) = setup();
    let part = |index: u32, title: &str| {
        json!({
            "repo_path": repo.path().to_str().unwrap(),
            "origin": {"kind": "split", "task_id": "ft-parent", "request_id": "req-1", "index": index},
            "title": title,
            "description": "Part of the parent.",
        })
        .to_string()
    };
    let a = file_proposal(&store, &part(0, "Alpha indexer")).unwrap();
    let b = file_proposal(&store, &part(1, "Zebra exporter")).unwrap();
    assert_eq!(a["outcome"], "filed", "{a}");
    assert_eq!(b["outcome"], "filed", "{b}");
    assert_ne!(a["item_id"], b["item_id"]);
    assert_eq!(
        file_proposal(&store, &part(0, "Alpha indexer")).unwrap()["outcome"],
        "already_filed"
    );
    assert_eq!(board(&store, &repo).len(), 2);
}

#[test]
fn filing_refuses_malformed_proposals_before_touching_the_board() {
    let (_d, store, repo) = setup();
    let path = repo.path().to_str().unwrap();
    for bad in [
        file_input(&repo, json!({"origin": {"kind": "scan", "fingerprint": "XYZ"}})),
        file_input(&repo, json!({"title": ""})),
        file_input(&repo, json!({"priority": 9})),
        file_input(&repo, json!({"surprise": true})),
        "[]".to_owned(),
        json!({"repo_path": path, "origin": {"kind": "split", "task_id": "../x", "request_id": "r", "index": 0}, "title": "t"}).to_string(),
        json!({"repo_path": path, "origin": {"kind": "split", "task_id": "t", "request_id": "r", "index": 99}, "title": "t"}).to_string(),
        "x".repeat(super::proposals::MAX_REQUEST_BYTES + 1),
    ] {
        let error = file_proposal(&store, &bad).unwrap_err();
        assert!(
            matches!(error.code.as_str(), "invalid_input"),
            "{bad:.80}: {error:?}"
        );
    }
    assert!(board(&store, &repo).is_empty());
}

fn reviewed(store: &Store, repo: &tempfile::TempDir) -> (String, i64) {
    let repository = registered(store, repo);
    let placed = place(
        store,
        &repository,
        &ExternalTask {
            acceptance_criteria: vec!["Existing criterion".into()],
            ..task("gp-parent", "Ship the exporter")
        },
        false,
        now_millis(),
    )
    .unwrap();
    (
        placed.item_id,
        placed.item.unwrap()["revision"].as_i64().unwrap(),
    )
}

fn apply_input(repo: &tempfile::TempDir, id: &str, revision: i64, extra: Value) -> String {
    let mut input = json!({
        "repo_path": repo.path().to_str().unwrap(),
        "id": id,
        "expected_revision": revision,
        "acceptance_criteria": ["Exports CSV with a header row"],
    });
    for (key, value) in extra.as_object().unwrap() {
        input[key] = value.clone();
    }
    input.to_string()
}

#[test]
fn an_accepted_refinement_is_one_revisioned_write_that_keeps_what_was_there() {
    let (_d, store, repo) = setup();
    let (id, revision) = reviewed(&store, &repo);
    let other = {
        let repository = registered(&store, &repo);
        place(
            &store,
            &repository,
            &task("gp-orig", "Original exporter"),
            false,
            now_millis(),
        )
        .unwrap()
        .item_id
    };
    let applied = apply_refinement(
        &store,
        &apply_input(&repo, &id, revision, json!({"duplicate_of": [other]})),
    )
    .unwrap();
    assert_eq!(applied["outcome"], "applied", "{applied}");
    assert_eq!(applied["revision"], revision + 1);
    let card = get_item(&store, &id).unwrap().unwrap();
    assert_eq!(
        card["acceptance_criteria"],
        json!(["Existing criterion", "Exports CSV with a header row"])
    );
    assert_eq!(card["links"][0]["kind"], "duplicate_of");
    assert_eq!(card["links"][0]["item_id"], other.as_str());
    assert_eq!(
        card["title"], "Ship the exporter",
        "title is not Manvi's to change"
    );
}

#[test]
fn a_task_that_changed_since_review_is_refused_and_left_as_the_person_made_it() {
    let (_d, store, repo) = setup();
    let (id, revision) = reviewed(&store, &repo);
    // The person edits the card after the proposal was shown.
    let item = get_item(&store, &id).unwrap().unwrap();
    let mut edit = resend(&item, &id, revision, "person");
    edit.insert("title".into(), json!("Ship the exporter, retitled"));
    query(&store, "items.put", &Value::Object(edit).to_string()).unwrap();

    let error =
        apply_refinement(&store, &apply_input(&repo, &id, revision, json!({}))).unwrap_err();
    assert_eq!(error.code, "revision_conflict", "{error:?}");
    assert!(
        error
            .message
            .contains(&format!("revision {}", revision + 1)),
        "{error:?}"
    );
    let card = get_item(&store, &id).unwrap().unwrap();
    assert_eq!(card["acceptance_criteria"], json!(["Existing criterion"]));
    assert_eq!(card["title"], "Ship the exporter, retitled");
    assert_eq!(card["revision"], revision + 1, "nothing was written");
}

#[test]
fn applying_the_same_acceptance_again_is_unchanged_not_a_second_write() {
    let (_d, store, repo) = setup();
    let (id, revision) = reviewed(&store, &repo);
    let first = apply_refinement(&store, &apply_input(&repo, &id, revision, json!({}))).unwrap();
    let again = apply_refinement(
        &store,
        &apply_input(&repo, &id, first["revision"].as_i64().unwrap(), json!({})),
    )
    .unwrap();
    assert_eq!(again["outcome"], "unchanged", "{again}");
    assert_eq!(again["revision"], first["revision"]);
}

#[test]
fn refinement_refuses_nothing_accepted_missing_originals_and_foreign_tasks() {
    let (_d, store, repo) = setup();
    let (id, revision) = reviewed(&store, &repo);
    let none = json!({"acceptance_criteria": []});
    assert_eq!(
        apply_refinement(&store, &apply_input(&repo, &id, revision, none))
            .unwrap_err()
            .code,
        "invalid_input"
    );
    let missing = apply_input(
        &repo,
        &id,
        revision,
        json!({"duplicate_of": ["ft-does-not-exist"]}),
    );
    assert_eq!(
        apply_refinement(&store, &missing).unwrap_err().code,
        "not_found"
    );
    let itself = apply_input(&repo, &id, revision, json!({"duplicate_of": [id]}));
    assert_eq!(
        apply_refinement(&store, &itself).unwrap_err().code,
        "invalid_input"
    );

    // A task of another repository is not this board's to edit.
    let other_repo = git_repo();
    let (foreign, foreign_revision) = reviewed(&store, &other_repo);
    let error = apply_refinement(
        &store,
        &apply_input(&repo, &foreign, foreign_revision, json!({})),
    )
    .unwrap_err();
    assert_eq!(error.code, "not_found", "{error:?}");
    let card = get_item(&store, &id).unwrap().unwrap();
    assert_eq!(card["revision"], revision);
}

#[test]
fn scan_params_name_the_open_repositorys_canonical_root_and_nothing_else() {
    let repo = git_repo();
    let path = repo.path().to_str().unwrap();
    let params = scan_start_params(
        &json!({"repo_path": path, "since": " main ", "max_proposals": 7}).to_string(),
    )
    .unwrap();
    assert_eq!(
        params["repo_root"],
        std::fs::canonicalize(repo.path())
            .unwrap()
            .to_str()
            .unwrap()
    );
    assert_eq!(params["since"], "main");
    assert_eq!(params["max_proposals"], 7);
    let bare = scan_start_params(&json!({"repo_path": path}).to_string()).unwrap();
    assert!(bare.get("since").is_none() && bare.get("max_proposals").is_none());

    for bad in [
        json!({"repo_path": path, "repo_root": "/etc"}).to_string(),
        json!({"repo_path": "/no/such/dir"}).to_string(),
        "null".to_owned(),
    ] {
        assert!(scan_start_params(&bad).is_err(), "{bad}");
    }
    assert!(scan_ref_params(r#"{"scan_id":"a b"}"#).is_err());
    assert!(scan_ref_params(r#"{"scan_id":""}"#).is_err());
    assert_eq!(
        scan_ref_params(r#"{"scan_id":"scan-1"}"#).unwrap(),
        json!({"scan_id": "scan-1"})
    );
}

#[test]
fn refine_carries_the_open_repositorys_canonical_root_and_never_defaults() {
    let repo = git_repo();
    let path = repo.path().to_str().unwrap();
    let (root, rest) = refine_split(
        &json!({"repo_path": path, "id": "t", "request_id": "r", "expected_revision": 2})
            .to_string(),
    )
    .unwrap();
    assert_eq!(
        root,
        std::fs::canonicalize(repo.path())
            .unwrap()
            .to_str()
            .unwrap()
    );
    let rest: Value = serde_json::from_str(&rest).unwrap();
    assert!(rest.get("repo_path").is_none() && rest["id"] == "t");
    for bad in [
        json!({"id": "t", "request_id": "r", "expected_revision": 2}).to_string(),
        json!({"repo_path": "/no/such/dir", "id": "t"}).to_string(),
        "[]".to_owned(),
    ] {
        assert!(refine_split(&bad).is_err(), "{bad}");
    }
}

// ---------------------------------------------------------------------------
// Through the host: a sidecar that is down, and one that answers partially.
// ---------------------------------------------------------------------------

mod host {
    use super::*;
    use crate::harness::sidecar::{bind_test_binary, test_serial};
    use crate::workbench::{Inner, WorkbenchState};
    use std::sync::Arc;

    fn state(path: &std::path::Path) -> WorkbenchState {
        WorkbenchState(Arc::new(Inner {
            path: Some(path.to_path_buf()),
            ..Inner::default()
        }))
    }

    #[test]
    fn a_sidecar_that_is_down_is_a_visible_error_never_an_empty_scan() {
        let serial = test_serial();
        let _binary = bind_test_binary(&serial, "/no/such/manvi-sidecar");
        let dir = tempfile::tempdir().unwrap();
        let host = state(&dir.path().join("workbench.sqlite"));
        let repo = git_repo();
        let path = repo.path().to_str().unwrap();
        for (method, input) in [
            ("scans.start", json!({"repo_path": path}).to_string()),
            ("scans.get", r#"{"scan_id":"scan-1"}"#.to_owned()),
            ("scans.cancel", r#"{"scan_id":"scan-1"}"#.to_owned()),
            (
                "enhancements.refine",
                json!({"repo_path": path, "id": "t", "request_id": "r", "expected_revision": 2})
                    .to_string(),
            ),
        ] {
            let error = host
                .request(method, &input)
                .expect_err("a down sidecar must not read as a result");
            assert!(
                matches!(
                    error.code.as_str(),
                    "unavailable" | "not_installed" | "protocol_error"
                ),
                "{method}: {error:?}"
            );
            assert!(!error.message.is_empty(), "{method}");
        }
    }

    #[cfg(unix)]
    #[test]
    fn a_partial_scan_reaches_the_caller_with_both_coverage_numbers() {
        use std::os::unix::fs::PermissionsExt;
        let serial = test_serial();
        let dir = tempfile::tempdir().unwrap();
        let binary = dir.path().join("scan-sidecar");
        let ops = r#"["hello","work.scan.gaps","work.scan.get","work.scan.cancel","work.enhancements.refine"]"#;
        let partial = r#"{"scan_id":"scan-1","state":"done","repo_root":"/r","max_proposals":25,"started_at":1,"complete":false,"board_complete":true,"new_count":25,"attached_count":0,"over_cap":17,"coverage":[{"category":"dead_symbols","status":"truncated","detail":"60 of 140 checked","hidden":80}],"proposals":[]}"#;
        let script = format!(
            r#"#!/bin/sh
while IFS= read -r line; do
  id=$(printf '%s' "$line" | sed -n 's/^{{"id":"\([0-9]*\)".*/\1/p')
  case "$line" in
    *'"op":"hello"'*) result='{{"protocol":1,"ops":{ops},"posture":"host"}}' ;;
    *'"op":"work.scan.get"'*) result='{partial}' ;;
    *) result='{{"scan_id":"scan-1","state":"running"}}' ;;
  esac
  printf '{{"id":"%s","ok":true,"result":%s}}\n' "$id" "$result"
done
"#
        );
        std::fs::write(&binary, script).unwrap();
        std::fs::set_permissions(&binary, std::fs::Permissions::from_mode(0o700)).unwrap();
        let _binary = bind_test_binary(&serial, binary.to_str().unwrap());
        let host = state(&dir.path().join("workbench.sqlite"));
        let repo = git_repo();
        let started = host
            .request(
                "scans.start",
                &json!({"repo_path": repo.path().to_str().unwrap()}).to_string(),
            )
            .unwrap();
        assert_eq!(started["scan_id"], "scan-1");
        let view = host
            .request("scans.get", r#"{"scan_id":"scan-1"}"#)
            .unwrap();
        // Nothing between Manvi and the reviewer may round a partial scan up.
        assert_eq!(view["complete"], false, "{view}");
        assert_eq!(view["over_cap"], 17);
        assert_eq!(view["coverage"][0]["status"], "truncated");
        assert_eq!(view["coverage"][0]["hidden"], 80);
        host.shutdown();
    }

    /// A sidecar that refuses a request without `repo_root` (E_BAD_ROOT): the
    /// host must send one, and a request naming no repository never reaches it.
    #[cfg(unix)]
    #[test]
    fn refine_sends_the_root_and_refuses_a_request_without_one() {
        use std::os::unix::fs::PermissionsExt;
        let serial = test_serial();
        let dir = tempfile::tempdir().unwrap();
        let binary = dir.path().join("root-sidecar");
        let script = r#"#!/bin/sh
while IFS= read -r line; do
  id=$(printf '%s' "$line" | sed -n 's/^{"id":"\([0-9]*\)".*/\1/p')
  case "$line" in
    *'"op":"hello"'*) printf '{"id":"%s","ok":true,"result":{"protocol":1,"ops":["hello","work.enhancements.refine"],"posture":"host"}}\n' "$id" ;;
    *'"repo_root":"/'*) printf '{"id":"%s","ok":true,"result":{"ok":true,"saw_root":true}}\n' "$id" ;;
    *) printf '{"id":"%s","ok":false,"error":{"code":"E_BAD_ROOT","message":"repo_root is required"}}\n' "$id" ;;
  esac
done
"#;
        std::fs::write(&binary, script).unwrap();
        std::fs::set_permissions(&binary, std::fs::Permissions::from_mode(0o700)).unwrap();
        let _binary = bind_test_binary(&serial, binary.to_str().unwrap());
        let host = state(&dir.path().join("workbench.sqlite"));
        let repo = git_repo();
        let input = json!({"repo_path": repo.path().to_str().unwrap(), "id": "t", "request_id": "r", "expected_revision": 2});
        let ok = host
            .request("enhancements.refine", &input.to_string())
            .unwrap();
        assert_eq!(ok["saw_root"], true, "{ok}");
        let error = host
            .request(
                "enhancements.refine",
                r#"{"id":"t","request_id":"r","expected_revision":2}"#,
            )
            .unwrap_err();
        assert_eq!(
            error.code, "invalid_input",
            "no root is refused before the sidecar: {error:?}"
        );
        host.shutdown();
    }

    #[cfg(unix)]
    #[test]
    fn a_sidecar_that_does_not_advertise_scan_is_refused_not_guessed_at() {
        use std::os::unix::fs::PermissionsExt;
        let serial = test_serial();
        let dir = tempfile::tempdir().unwrap();
        let binary = dir.path().join("old-sidecar");
        let script = r#"#!/bin/sh
while IFS= read -r line; do
  id=$(printf '%s' "$line" | sed -n 's/^{"id":"\([0-9]*\)".*/\1/p')
  case "$line" in
    *'"op":"hello"'*) result='{"protocol":1,"ops":["hello","work.enhancements.configuration"],"posture":"host"}' ;;
    *) result='{"ok":true}' ;;
  esac
  printf '{"id":"%s","ok":true,"result":%s}\n' "$id" "$result"
done
"#;
        std::fs::write(&binary, script).unwrap();
        std::fs::set_permissions(&binary, std::fs::Permissions::from_mode(0o700)).unwrap();
        let _binary = bind_test_binary(&serial, binary.to_str().unwrap());
        let host = state(&dir.path().join("workbench.sqlite"));
        let error = host
            .request("scans.get", r#"{"scan_id":"scan-1"}"#)
            .expect_err("an op the sidecar never advertised must be refused");
        assert!(!error.message.is_empty(), "{error:?}");
        host.shutdown();
    }
}
