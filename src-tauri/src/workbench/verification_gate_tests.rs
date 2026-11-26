//! The evidence gate on an agent's `done`: one failing-first test per way a
//! claim can lack evidence, plus the paths that must keep working.

use super::tests::{filed, git_repo, live, person_edits, profile};
use super::{complete_task, resend, resolve};
use crate::workbench::verification_gate::{
    checkout_of, current_head, landed_on_default, EVIDENCE_TYPE,
};
use dc_store::Store;
use serde_json::{json, Value};

/// A repository with one commit, so it has a head to verify.
fn committed_repo() -> tempfile::TempDir {
    let repo = git_repo();
    crate::test_support::git_in(
        repo.path(),
        &["commit", "-q", "--allow-empty", "-m", "init"],
    );
    repo
}

pub(crate) fn head_of(repo: &tempfile::TempDir) -> String {
    current_head(repo.path().to_str().unwrap()).unwrap()
}

/// What Manvi's `work.verify.task` stores, as the sidecar would write it.
pub(crate) fn record(
    item: &str,
    outcome: &str,
    state: &str,
    repo: &tempfile::TempDir,
    head: &str,
) -> Value {
    json!({
        "request_id": "req-1", "item_id": item, "state": state, "outcome": outcome,
        "worktree": repo.path().to_str().unwrap(), "head": head,
        "criteria": [
            {"index": 1, "text": "exports csv", "status": if outcome == "passed" { "met" } else { "unmet" },
             "findings": if outcome == "passed" { json!([]) } else { json!(["no csv writer found"]) }}
        ],
        "degraded": if outcome == "degraded" { json!(["the critic did not complete"]) } else { json!([]) },
    })
}

pub(crate) fn store_record(store: &Store, item: &str, record: &Value) {
    store
        .evidence_append(EVIDENCE_TYPE, Some(item), None, None, &record.to_string())
        .unwrap();
}

/// A passed record for the repository's current head, for tests of other
/// behaviour that need a task an agent may finish.
pub(crate) fn verified(store: &Store, repo: &tempfile::TempDir, item: &str) {
    let head = head_of(repo);
    store_record(store, item, &record(item, "passed", "settled", repo, &head));
}

fn claim(store: &Store, repo: &tempfile::TempDir, id: &str, summary: Option<&str>) -> Value {
    complete_task(
        store,
        repo.path().to_str().unwrap(),
        id,
        "done",
        None,
        summary,
        None,
    )
    .unwrap()
}

fn logs(store: &Store, id: &str) -> String {
    live(store, id).unwrap()["logs"]
        .as_str()
        .unwrap_or_default()
        .to_owned()
}

fn setup() -> (tempfile::TempDir, Store, tempfile::TempDir, String) {
    let (dir, path) = profile();
    let store = Store::open(&path).unwrap();
    let repo = committed_repo();
    let id = filed(&store, &repo, "gp-gate", "Ship the exporter");
    (dir, store, repo, id)
}

fn assert_held(store: &Store, id: &str, response: &Value, reason: &str) {
    assert_eq!(response["status"], "review", "{response}");
    assert_eq!(response["requested_status"], "done");
    assert_eq!(response["held_for_review"], true);
    assert!(
        response["verification"].as_str().unwrap().contains(reason),
        "{response}"
    );
    assert_eq!(live(store, id).unwrap()["status"], "review");
    let history = logs(store, id);
    assert!(history.contains("held in review"), "{history}");
    assert!(history.contains(reason), "{history}");
}

#[test]
fn an_agent_claiming_done_with_no_verification_record_lands_in_review() {
    let (_d, store, repo, id) = setup();
    let response = claim(&store, &repo, &id, Some("All done"));
    assert_held(&store, &id, &response, "no verification record");
    assert!(
        logs(&store, &id).trim_end().ends_with("---\nAll done"),
        "the agent's summary is still recorded last"
    );
}

#[test]
fn a_degraded_record_does_not_let_an_agent_finish() {
    let (_d, store, repo, id) = setup();
    let head = head_of(&repo);
    store_record(
        &store,
        &id,
        &record(&id, "degraded", "settled", &repo, &head),
    );
    let response = claim(&store, &repo, &id, None);
    assert_held(&store, &id, &response, "outcome \"degraded\"");
    assert!(logs(&store, &id).contains("the critic did not complete"));
}

#[test]
fn a_failed_record_does_not_let_an_agent_finish_and_names_the_unmet_criterion() {
    let (_d, store, repo, id) = setup();
    let head = head_of(&repo);
    store_record(&store, &id, &record(&id, "failed", "settled", &repo, &head));
    let response = claim(&store, &repo, &id, None);
    assert_held(&store, &id, &response, "outcome \"failed\"");
    assert!(logs(&store, &id).contains("no csv writer found"));
}

#[test]
fn a_verification_still_running_is_not_a_verdict() {
    let (_d, store, repo, id) = setup();
    let head = head_of(&repo);
    store_record(&store, &id, &record(&id, "", "running", &repo, &head));
    let response = claim(&store, &repo, &id, None);
    assert_held(&store, &id, &response, "still \"running\"");
}

#[test]
fn a_passed_record_for_an_older_head_on_an_unlanded_lane_is_stale() {
    let (_d, store, repo, id) = setup();
    // The work is on a lane that has not landed: a head that moved past a
    // pass on the default branch itself would be landed work.
    crate::test_support::git_in(repo.path(), &["checkout", "-q", "-b", "lane"]);
    std::fs::write(repo.path().join("work.txt"), "v1\n").unwrap();
    crate::test_support::git_in(repo.path(), &["add", "work.txt"]);
    crate::test_support::git_in(repo.path(), &["commit", "-q", "-m", "lane work"]);
    verified(&store, &repo, &id);
    // Change tree contents (empty commits preserve tree_oid and remain valid).
    std::fs::write(repo.path().join("work.txt"), "v2\n").unwrap();
    crate::test_support::git_in(repo.path(), &["add", "work.txt"]);
    crate::test_support::git_in(repo.path(), &["commit", "-q", "-m", "more work"]);
    let response = claim(&store, &repo, &id, None);
    assert_held(
        &store,
        &id,
        &response,
        "the work changed after it was verified",
    );
}

/// The main-checkout case: the record is for the default branch's own head M,
/// new commits land on top, and the claim is made from that checkout. M is an
/// ancestor of the tip, but the claim is for M2, which was never verified.
#[test]
fn a_passed_record_for_an_older_head_is_stale() {
    let (_d, store, repo, id) = setup();
    verified(&store, &repo, &id);
    // Change tree contents (empty commits preserve tree_oid and remain valid).
    std::fs::write(repo.path().join("work.txt"), "v2\n").unwrap();
    crate::test_support::git_in(repo.path(), &["add", "work.txt"]);
    crate::test_support::git_in(repo.path(), &["commit", "-q", "-m", "more work"]);
    let response = claim(&store, &repo, &id, None);
    assert_held(
        &store,
        &id,
        &response,
        "the work changed after it was verified",
    );
}

/// A verified lane landed and was removed, then more commits landed on top: a
/// claim from the main checkout is for the new tip, not the verified commit.
#[test]
fn a_landed_pass_does_not_cover_commits_that_landed_after_it() {
    let (_d, store, repo, id) = setup();
    let (_lane_dir, lane, _m) = verified_lane(&store, &repo, &id);
    land(&repo);
    tear_down(&repo, &lane);
    // Change tree contents (empty commits preserve tree_oid and remain valid).
    std::fs::write(repo.path().join("followup.txt"), "new\n").unwrap();
    crate::test_support::git_in(repo.path(), &["add", "followup.txt"]);
    crate::test_support::git_in(repo.path(), &["commit", "-q", "-m", "unverified follow-up"]);
    let response = claim(&store, &repo, &id, None);
    assert_held(&store, &id, &response, "the claim is for");
}

#[test]
fn a_record_for_a_different_item_is_not_evidence_for_this_one() {
    let (_d, store, repo, id) = setup();
    let head = head_of(&repo);
    store_record(
        &store,
        &id,
        &record("some-other-item", "passed", "settled", &repo, &head),
    );
    let response = claim(&store, &repo, &id, None);
    assert_held(&store, &id, &response, "for another task");
}

#[test]
fn a_passed_record_for_another_repositorys_worktree_is_refused() {
    let (_d, store, repo, id) = setup();
    let other = committed_repo();
    let head = head_of(&other);
    store_record(
        &store,
        &id,
        &record(&id, "passed", "settled", &other, &head),
    );
    let response = claim(&store, &repo, &id, None);
    assert_held(&store, &id, &response, "belongs to another repository");
}

#[test]
fn a_newer_unreadable_record_is_not_shadowed_by_an_older_pass() {
    let (_d, store, repo, id) = setup();
    verified(&store, &repo, &id);
    store
        .evidence_append(EVIDENCE_TYPE, Some(&id), None, None, "{\"request_id\": 7}")
        .unwrap();
    let response = claim(&store, &repo, &id, None);
    assert_held(&store, &id, &response, "could not be parsed");
}

#[test]
fn an_unreadable_evidence_store_fails_closed_to_review() {
    let (_d, path) = profile();
    let store = Store::open(&path).unwrap();
    let repo = committed_repo();
    let id = filed(&store, &repo, "gp-gate", "Ship the exporter");
    verified(&store, &repo, &id);
    // Another connection takes the evidence table away from under the store.
    let other = rusqlite::Connection::open(&path).unwrap();
    other
        .execute_batch("ALTER TABLE evidence RENAME TO evidence_gone")
        .unwrap();
    let response = claim(&store, &repo, &id, None);
    assert_held(&store, &id, &response, "could not be read");
}

#[test]
fn a_passed_record_for_the_current_head_lets_the_agent_finish_and_is_attached() {
    let (_d, store, repo, id) = setup();
    verified(&store, &repo, &id);
    let response = claim(&store, &repo, &id, Some("Exporter shipped"));
    assert_eq!(response["status"], "done", "{response}");
    assert_eq!(response["held_for_review"], false);
    assert!(response["verification"]
        .as_str()
        .unwrap()
        .contains("Verification passed"));
    let history = logs(&store, &id);
    assert!(history.contains("--- Verification passed ("), "{history}");
    assert!(
        history.trim_end().ends_with("---\nExporter shipped"),
        "{history}"
    );
}

#[test]
fn retrying_a_held_or_passed_claim_writes_nothing_twice() {
    let (_d, store, repo, id) = setup();
    let first = claim(&store, &repo, &id, Some("All done"));
    assert_eq!(first["outcome"], "updated");
    let held_logs = logs(&store, &id);
    let again = claim(&store, &repo, &id, Some("All done"));
    assert_eq!(again["outcome"], "unchanged", "{again}");
    assert_eq!(again["status"], "review");
    assert_eq!(logs(&store, &id), held_logs);

    // Once verified, the same claim finishes it, and a retry of that is a no-op.
    verified(&store, &repo, &id);
    let done = claim(&store, &repo, &id, Some("All done"));
    assert_eq!(done["status"], "done", "{done}");
    let done_logs = logs(&store, &id);
    let retry = claim(&store, &repo, &id, Some("All done"));
    assert_eq!(retry["outcome"], "unchanged", "{retry}");
    assert_eq!(logs(&store, &id), done_logs);
}

#[test]
fn a_person_moving_the_card_to_done_is_not_gated() {
    let (_d, store, _repo, id) = setup();
    let moved = person_edits(&store, &id, |input| {
        input.insert("status".into(), json!("done"));
    });
    assert_eq!(moved["status"], "done");
    let item = live(&store, &id).unwrap();
    let again = resend(&item, &id, item["revision"].as_i64().unwrap(), "person");
    assert_eq!(again["status"], "done");
}

#[test]
fn concurrent_claims_never_land_unverified_work_in_done() {
    let (_d, path) = profile();
    let repo = committed_repo();
    let id = {
        let store = Store::open(&path).unwrap();
        filed(&store, &repo, "gp-gate", "Ship the exporter")
    };
    let repo_path = repo.path().to_str().unwrap().to_owned();
    let handles: Vec<_> = (0..4)
        .map(|n| {
            let (path, repo_path, id) = (path.clone(), repo_path.clone(), id.clone());
            std::thread::spawn(move || {
                let store = Store::open(&path).unwrap();
                complete_task(
                    &store,
                    &repo_path,
                    &id,
                    "done",
                    None,
                    Some(&format!("claim {n}")),
                    None,
                )
            })
        })
        .collect();
    for handle in handles {
        // A lost race is reported as a conflict, never as a move to done.
        if let Ok(response) = handle.join().unwrap() {
            assert_eq!(response["status"], "review", "{response}");
        }
    }
    let store = Store::open(&path).unwrap();
    assert_eq!(live(&store, &id).unwrap()["status"], "review");
}

#[test]
fn a_start_request_names_only_the_work_and_defaults_to_the_worktrees_head() {
    let repo = committed_repo();
    let head = head_of(&repo);
    let input = json!({"id": "item-1", "request_id": "req-1", "worktree": repo.path().to_str().unwrap(), "base": "main"});
    let params = super::super::verification_gate::start_params(&input.to_string()).unwrap();
    assert_eq!(params["item_id"], "item-1");
    assert_eq!(params["head"], head);
    assert_eq!(params["base"], "main");
    assert_eq!(params.as_object().unwrap().len(), 5, "{params}");
}

#[test]
fn a_start_request_cannot_carry_a_command_or_a_bad_identity() {
    use super::super::verification_gate::{get_params, start_params};
    let good = json!({"id": "item-1", "request_id": "req-1", "worktree": "/tmp", "base": "main", "head": "abc"});
    assert!(start_params(&good.to_string()).is_ok());
    for extra in [("command", json!("rm -rf /")), ("criteria", json!(["x"]))] {
        let mut input = good.clone();
        input[extra.0] = extra.1;
        assert_eq!(
            start_params(&input.to_string()).unwrap_err().code,
            "invalid_input"
        );
    }
    for bad in [
        json!({"id": "a b"}),
        json!({"request_id": ""}),
        json!({"base": " "}),
    ] {
        let mut input = good.clone();
        input
            .as_object_mut()
            .unwrap()
            .extend(bad.as_object().unwrap().clone());
        assert_eq!(
            start_params(&input.to_string()).unwrap_err().code,
            "invalid_input",
            "{input}"
        );
    }
    assert_eq!(start_params("[]").unwrap_err().code, "invalid_input");
    assert_eq!(
        get_params(r#"{"id":"item-1","request_id":"x y"}"#)
            .unwrap_err()
            .code,
        "invalid_input"
    );
    assert_eq!(
        get_params(r#"{"id":"item-1"}"#).unwrap(),
        json!({"item_id": "item-1"})
    );
}

/// A lane worktree with one commit of its own, verified at that commit. Returns
/// the lane directory (keep it alive), the lane path and the verified commit.
fn verified_lane(
    store: &Store,
    repo: &tempfile::TempDir,
    id: &str,
) -> (tempfile::TempDir, String, String) {
    let parent = tempfile::tempdir().unwrap();
    let lane = parent.path().join("lane").to_str().unwrap().to_owned();
    crate::test_support::git_in(repo.path(), &["worktree", "add", "-q", "-b", "lane", &lane]);
    // Commit real file content so the lane's tree differs from an empty/unmerged base.
    let lane_path = std::path::Path::new(&lane);
    std::fs::write(lane_path.join("lane_work.txt"), "verified work\n").unwrap();
    crate::test_support::git_in(lane_path, &["add", "lane_work.txt"]);
    crate::test_support::git_in(lane_path, &["commit", "-q", "-m", "the work"]);
    let m = current_head(&lane).unwrap();
    let mut passed = record(id, "passed", "settled", repo, &m);
    passed["worktree"] = json!(lane);
    store_record(store, id, &passed);
    (parent, lane, m)
}

fn land(repo: &tempfile::TempDir) {
    crate::test_support::git_in(
        repo.path(),
        &["merge", "-q", "--no-ff", "-m", "land the lane", "lane"],
    );
}

/// Land the lane by fast-forward, so the default branch's tip is the verified
/// commit itself and a claim made from that checkout is for that commit.
fn land_ff(repo: &tempfile::TempDir) {
    crate::test_support::git_in(repo.path(), &["merge", "-q", "--ff-only", "lane"]);
}

fn tear_down(repo: &tempfile::TempDir, lane: &str) {
    crate::test_support::git_in(repo.path(), &["worktree", "remove", "--force", lane]);
}

#[test]
fn a_pass_for_a_commit_that_landed_on_the_default_branch_survives_the_lane_being_removed() {
    let (_d, store, repo, id) = setup();
    let (_lane_dir, lane, _m) = verified_lane(&store, &repo, &id);
    land_ff(&repo);
    tear_down(&repo, &lane);
    crate::test_support::git_in(repo.path(), &["branch", "-q", "-D", "lane"]);
    let response = claim(&store, &repo, &id, Some("Landed"));
    assert_eq!(response["status"], "done", "{response}");
    assert_eq!(response["held_for_review"], false);
    assert!(
        response["verification"]
            .as_str()
            .unwrap()
            .contains("default branch"),
        "{response}"
    );
}

#[test]
fn a_pass_for_a_landed_commit_passes_while_the_lane_moved_on_past_it() {
    let (_d, store, repo, id) = setup();
    let (_lane_dir, lane, _m) = verified_lane(&store, &repo, &id);
    land_ff(&repo);
    crate::test_support::git_in(
        std::path::Path::new(&lane),
        &["commit", "-q", "--allow-empty", "-m", "unrelated follow-up"],
    );
    let response = claim(&store, &repo, &id, None);
    assert_eq!(response["status"], "done", "{response}");
}

#[test]
fn a_pass_for_a_commit_that_never_landed_stays_held_when_the_lane_is_gone() {
    let (_d, store, repo, id) = setup();
    let (_lane_dir, lane, m) = verified_lane(&store, &repo, &id);
    tear_down(&repo, &lane);
    // The claim is for the verified commit itself, so the landed check is
    // reached and says the commit is not on the default branch.
    crate::test_support::git_in(repo.path(), &["checkout", "-q", "--detach", &m]);
    let response = claim(&store, &repo, &id, None);
    assert_held(&store, &id, &response, "not on the default branch");
}

#[test]
fn a_lane_that_moved_to_a_commit_without_the_verified_one_stays_held() {
    let (_d, store, repo, id) = setup();
    let (_lane_dir, lane, _m) = verified_lane(&store, &repo, &id);
    let lane_path = std::path::Path::new(&lane);
    crate::test_support::git_in(lane_path, &["reset", "-q", "--hard", "HEAD~1"]);
    // Change tree contents (empty commits preserve tree_oid and remain valid).
    std::fs::write(lane_path.join("different.txt"), "alt\n").unwrap();
    crate::test_support::git_in(lane_path, &["add", "different.txt"]);
    crate::test_support::git_in(
        lane_path,
        &["commit", "-q", "-m", "a different line of work"],
    );
    let response = claim(&store, &repo, &id, None);
    assert_held(
        &store,
        &id,
        &response,
        "the work changed after it was verified",
    );
}

#[test]
fn a_record_whose_commit_the_repository_does_not_know_stays_held() {
    let (_d, store, repo, id) = setup();
    let gone = tempfile::tempdir().unwrap();
    let mut passed = record(&id, "passed", "settled", &repo, &"ab".repeat(20));
    passed["worktree"] = json!(gone.path().join("nowhere").to_str().unwrap());
    store_record(&store, &id, &passed);
    let response = claim(&store, &repo, &id, None);
    assert_held(&store, &id, &response, "could not be resolved");
    assert_held(&store, &id, &response, "the claim is for");
}

// ---- Adversarial attacks on the landed-commit relaxation (bl1121w3) ----
// Each asserts the SAFE outcome: a failure here is a found break.

/// Store a passed record whose worktree is `worktree` and whose head is the
/// given string, then claim done from the repository checkout.
fn claim_with_record(
    store: &Store,
    repo: &tempfile::TempDir,
    id: &str,
    worktree: &str,
    head: &str,
) -> Value {
    let mut passed = record(id, "passed", "settled", repo, head);
    passed["worktree"] = json!(worktree);
    store_record(store, id, &passed);
    // The claim is for the commit the checkout is at. Put it at the recorded
    // head where the repository has that commit, so the landed check itself is
    // what the attack meets rather than a claim for some other commit.
    if crate::engine::git_cli::git_text(
        repo.path(),
        &["cat-file", "-e", &format!("{head}^{{commit}}")],
    )
    .is_ok()
    {
        crate::test_support::git_in(repo.path(), &["checkout", "-q", "--detach", head]);
    }
    claim(store, repo, id, None)
}

fn git(repo: &std::path::Path, args: &[&str]) {
    crate::test_support::git_in(repo, args);
}

fn is_done(response: &Value) -> bool {
    response["status"] == "done"
}

#[test]
fn attack_heads_that_are_not_full_commit_ids_never_pass_even_when_the_commit_landed() {
    let mut breaks = Vec::new();
    type Spell = Box<dyn Fn(&str) -> String>;
    let forms: Vec<(&str, Spell)> = vec![
        ("HEAD", Box::new(|_| "HEAD".into())),
        ("HEAD~1", Box::new(|_| "HEAD~1".into())),
        ("main", Box::new(|_| "main".into())),
        ("--all", Box::new(|_| "--all".into())),
        ("-1", Box::new(|_| "-1".into())),
        ("leading space", Box::new(|m| format!(" {m}"))),
        ("trailing newline", Box::new(|m| format!("{m}\n"))),
        ("abbreviated 12", Box::new(|m| m[..12].to_owned())),
        ("abbreviated 7", Box::new(|m| m[..7].to_owned())),
        ("uppercase", Box::new(|m| m.to_uppercase())),
        ("hex-shaped unknown 40", Box::new(|_| "ab".repeat(20))),
        ("sha256-shaped 64", Box::new(|m| format!("{m}{}", &m[..24]))),
        ("sha256-shaped zeros", Box::new(|_| "0".repeat(64))),
        (
            "hex-named tag on landed commit",
            Box::new(|_| "cafe123".into()),
        ),
        (
            "hex-named branch on landed commit",
            Box::new(|_| "beef456".into()),
        ),
    ];
    for (label, make) in &forms {
        let (_d, store, repo, id) = setup();
        let (_lane_dir, lane, m) = verified_lane(&store, &repo, &id);
        land(&repo);
        git(repo.path(), &["tag", "cafe123", &m]);
        git(repo.path(), &["branch", "beef456", &m]);
        tear_down(&repo, &lane);
        let head = make(&m);
        let response = claim_with_record(&store, &repo, &id, &lane, &head);
        if is_done(&response) {
            breaks.push(format!(
                "{label}: head {head:?} reached done: {}",
                response["verification"]
            ));
        }
    }
    assert!(breaks.is_empty(), "BREAKS:\n{}", breaks.join("\n"));
}

#[test]
fn attack_a_head_hex_shaped_that_is_a_tag_on_an_unlanded_commit_stays_held() {
    let (_d, store, repo, id) = setup();
    let (_lane_dir, lane, m) = verified_lane(&store, &repo, &id);
    git(repo.path(), &["tag", "cafe123", &m]);
    tear_down(&repo, &lane);
    let response = claim_with_record(&store, &repo, &id, &lane, "cafe123");
    assert!(!is_done(&response), "{response}");
}

#[test]
fn attack_a_pass_taken_in_another_clone_is_not_landed_work_here() {
    let (_d, store, repo, id) = setup();
    let other = crate::test_support::git_repo();
    git(
        other.path(),
        &["commit", "-q", "--allow-empty", "-m", "elsewhere"],
    );
    let m = current_head(other.path().to_str().unwrap()).unwrap();
    let response = claim_with_record(&store, &repo, &id, other.path().to_str().unwrap(), &m);
    assert!(!is_done(&response), "{response}");
}

#[test]
fn attack_a_pass_for_a_commit_only_another_clone_has_stays_held_when_that_clone_is_gone() {
    let (_d, store, repo, id) = setup();
    let other = crate::test_support::git_repo();
    git(
        other.path(),
        &["commit", "-q", "--allow-empty", "-m", "elsewhere"],
    );
    let m = current_head(other.path().to_str().unwrap()).unwrap();
    let gone = other.path().to_str().unwrap().to_owned();
    drop(other);
    let response = claim_with_record(&store, &repo, &id, &gone, &m);
    assert!(!is_done(&response), "{response}");
}

#[test]
fn attack_a_shared_object_store_sibling_does_not_make_a_commit_landed() {
    // A clone that borrows this repository's objects, with its own commit on
    // its own main: the commit is not in the card's repository at all.
    let (_d, store, repo, id) = setup();
    let other = crate::test_support::git_repo();
    let alternates = repo.path().join(".git/objects");
    std::fs::write(
        other.path().join(".git/objects/info/alternates"),
        format!("{}\n", alternates.display()),
    )
    .unwrap();
    git(
        other.path(),
        &["commit", "-q", "--allow-empty", "-m", "shared sibling"],
    );
    let m = current_head(other.path().to_str().unwrap()).unwrap();
    let response = claim_with_record(&store, &repo, &id, other.path().to_str().unwrap(), &m);
    assert!(!is_done(&response), "{response}");
}

#[test]
fn attack_a_local_branch_named_main_is_not_the_default_when_origin_head_names_another() {
    let (_d, store, repo, id) = setup();
    let init = head_of(&repo);
    git(repo.path(), &["branch", "trunk"]);
    git(
        repo.path(),
        &["remote", "add", "origin", repo.path().to_str().unwrap()],
    );
    git(
        repo.path(),
        &["update-ref", "refs/remotes/origin/trunk", &init],
    );
    git(
        repo.path(),
        &[
            "symbolic-ref",
            "refs/remotes/origin/HEAD",
            "refs/remotes/origin/trunk",
        ],
    );
    let (_lane_dir, lane, _m) = verified_lane(&store, &repo, &id);
    land(&repo); // lands on the checked-out local branch, which is not trunk
    tear_down(&repo, &lane);
    let base = crate::engine::git_reader::default_base(repo.path());
    let response = claim(&store, &repo, &id, None);
    assert!(
        !is_done(&response),
        "default base read as {:?}; {response}",
        base.base
    );
}

#[test]
fn attack_the_first_alphabetical_branch_is_not_a_default_when_no_main_exists() {
    // No main/master/trunk/develop: the lane's own branch sorts first and must
    // not be taken for the default branch.
    let (_d, store, repo, id) = setup();
    let (_lane_dir, lane, _m) = verified_lane(&store, &repo, &id);
    tear_down(&repo, &lane);
    git(repo.path(), &["branch", "-m", "lane", "aaa-lane"]);
    git(repo.path(), &["branch", "-m", "main", "zzz-work"]);
    let response = claim(&store, &repo, &id, None);
    assert!(!is_done(&response), "{response}");
}

#[test]
fn attack_a_detached_checkout_with_no_branches_has_no_default_branch() {
    let (_d, store, repo, id) = setup();
    let (_lane_dir, lane, _m) = verified_lane(&store, &repo, &id);
    land(&repo);
    tear_down(&repo, &lane);
    git(repo.path(), &["checkout", "-q", "--detach"]);
    git(repo.path(), &["branch", "-D", "main", "lane"]);
    let response = claim(&store, &repo, &id, None);
    assert!(!is_done(&response), "{response}");
}

#[test]
fn attack_init_default_branch_naming_a_missing_branch_does_not_promote_unlanded_work() {
    let (_d, store, repo, id) = setup();
    git(repo.path(), &["config", "init.defaultBranch", "develop"]);
    let (_lane_dir, lane, _m) = verified_lane(&store, &repo, &id);
    tear_down(&repo, &lane);
    let response = claim(&store, &repo, &id, None);
    assert!(!is_done(&response), "{response}");
}

#[test]
fn attack_a_default_branch_reset_past_the_commit_holds_it_though_the_object_remains() {
    let (_d, store, repo, id) = setup();
    let (_lane_dir, lane, m) = verified_lane(&store, &repo, &id);
    land(&repo);
    tear_down(&repo, &lane);
    git(repo.path(), &["reset", "-q", "--hard", "HEAD~1"]);
    git(repo.path(), &["branch", "-q", "-D", "lane"]);
    git(repo.path(), &["cat-file", "-e", &m]);
    let response = claim(&store, &repo, &id, None);
    assert!(!is_done(&response), "{response}");
}

#[test]
fn attack_a_default_branch_force_moved_to_an_unrelated_root_holds_it() {
    let (_d, store, repo, id) = setup();
    let (_lane_dir, lane, _m) = verified_lane(&store, &repo, &id);
    land(&repo);
    tear_down(&repo, &lane);
    git(repo.path(), &["checkout", "-q", "--orphan", "fresh"]);
    // `git checkout --orphan` keeps the previous index staged; clear it so
    // `fresh` is genuinely an unrelated tree rather than a squash of `main`.
    git(repo.path(), &["rm", "-q", "-rf", "."]);
    std::fs::write(repo.path().join("unrelated.txt"), "unrelated\n").unwrap();
    git(repo.path(), &["add", "unrelated.txt"]);
    git(repo.path(), &["commit", "-q", "-m", "unrelated root"]);
    git(repo.path(), &["branch", "-q", "-M", "main"]);
    let response = claim(&store, &repo, &id, None);
    assert!(!is_done(&response), "{response}");
}

#[test]
fn attack_a_different_repository_created_at_the_lane_path_is_not_the_lane() {
    let (_d, store, repo, id) = setup();
    let (_lane_dir, lane, m) = verified_lane(&store, &repo, &id);
    tear_down(&repo, &lane);
    std::fs::create_dir_all(&lane).unwrap();
    git(std::path::Path::new(&lane), &["init", "-q"]);
    git(
        std::path::Path::new(&lane),
        &["commit", "-q", "--allow-empty", "-m", "squatter"],
    );
    let response = claim_with_record(&store, &repo, &id, &lane, &m);
    assert!(!is_done(&response), "{response}");
    assert_held(&store, &id, &response, "another repository");
}

#[test]
fn attack_a_commit_missing_from_a_shallow_clone_holds() {
    let origin = crate::test_support::git_repo();
    let mut first = String::new();
    for n in 0..3 {
        git(
            origin.path(),
            &["commit", "-q", "--allow-empty", "-m", &format!("c{n}")],
        );
        if n == 0 {
            first = current_head(origin.path().to_str().unwrap()).unwrap();
        }
    }
    let (_dir, path) = profile();
    let store = Store::open(&path).unwrap();
    let shallow = crate::test_support::git_repo();
    git(
        shallow.path(),
        &[
            "fetch",
            "-q",
            "--depth",
            "1",
            &format!("file://{}", origin.path().to_str().unwrap()),
            "main",
        ],
    );
    git(
        shallow.path(),
        &["checkout", "-q", "-B", "main", "FETCH_HEAD"],
    );
    let id = filed(&store, &shallow, "gp-shallow", "Ship the exporter");
    let gone = tempfile::tempdir().unwrap();
    let response = claim_with_record(
        &store,
        &shallow,
        &id,
        gone.path().join("nowhere").to_str().unwrap(),
        &first,
    );
    assert!(!is_done(&response), "{response}");
}

#[test]
fn attack_a_corrupt_default_branch_ref_is_a_read_error_not_a_yes() {
    let (_d, store, repo, id) = setup();
    let (_lane_dir, lane, m) = verified_lane(&store, &repo, &id);
    land(&repo);
    tear_down(&repo, &lane);
    let identity = resolve(repo.path().to_str().unwrap()).unwrap().identity;
    std::fs::write(repo.path().join(".git/refs/heads/main"), "not-a-sha\n").unwrap();
    let answer = landed_on_default(&identity, &m);
    assert!(!matches!(answer, Ok(true)), "{answer:?}");
}

#[test]
fn attack_a_repository_with_no_checkout_is_a_read_error_not_a_yes() {
    let answer = landed_on_default("local:/definitely/not/a/dir/.git", &"ab".repeat(20));
    assert!(answer.is_err(), "{answer:?}");
    let answer = landed_on_default("https://example.com/r.git", &"ab".repeat(20));
    assert!(answer.is_err(), "{answer:?}");
}

/// The identity a repository resolves to names its git directory, spelled as
/// the platform's repository identity spells it (`\\?\C:\...\.git` on
/// Windows); the checkout is the directory holding it, not the git directory.
#[test]
fn a_repository_identity_names_the_checkout_that_holds_its_git_dir() {
    let repo = committed_repo();
    let identity = resolve(repo.path().to_str().unwrap()).unwrap().identity;
    let checkout = checkout_of(&identity).expect("a local checkout");
    assert_eq!(
        std::fs::canonicalize(&checkout).unwrap(),
        std::fs::canonicalize(repo.path()).unwrap(),
        "{identity}"
    );
}

/// Both separators before `.git` name the checkout on Windows, where either
/// can spell a path; only the one that is a separator does elsewhere.
#[test]
fn checkout_of_reads_the_git_dir_by_path_component() {
    let repo = committed_repo();
    let dir = repo.path().to_str().unwrap();
    assert_eq!(
        checkout_of(&format!("local:{dir}/.git")).as_deref(),
        Some(dir)
    );
    assert_eq!(checkout_of(&format!("local:{dir}")).as_deref(), Some(dir));
    assert_eq!(checkout_of(&format!("local:{dir}/missing/.git")), None);
    assert_eq!(checkout_of("https://example.com/r.git"), None);
    #[cfg(windows)]
    assert_eq!(
        checkout_of(&format!("local:{dir}\\.git")).as_deref(),
        Some(dir)
    );
}
