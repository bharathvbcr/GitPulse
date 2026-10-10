//! The verification driver's state machine: stale head, a person who moved the
//! card meanwhile, a duplicate trigger, a sidecar that is down, a host that is
//! going away, and the bound on how many run at once.

use super::gate_tests::{head_of, record, store_record};
use super::tests::{filed, live, person_edits, profile};
use super::{complete_task, resolve};
use crate::workbench::verification_driver::{
    decide, status, status_batch, target_of, Backend, Driver, Facts, Report, Step, Target,
    MAX_ACTIVE,
};
use crate::workbench::verification_gate::{self as gate, Record};
use crate::workbench::WorkbenchError;
use dc_store::Store;
use serde_json::{json, Value};
use std::cell::{Cell, RefCell};
use std::time::{Duration, Instant};

struct Fake {
    starts: RefCell<Vec<Value>>,
    sidecar_down: Cell<bool>,
    cancelled: Cell<bool>,
    clock: Cell<Duration>,
    epoch: Instant,
}

impl Fake {
    fn new() -> Self {
        Self {
            starts: RefCell::new(Vec::new()),
            sidecar_down: Cell::new(false),
            cancelled: Cell::new(false),
            clock: Cell::new(Duration::ZERO),
            epoch: Instant::now(),
        }
    }
}

impl Backend for Fake {
    fn start(&self, params: Value) -> Result<Value, WorkbenchError> {
        if self.sidecar_down.get() {
            return Err(WorkbenchError::new(
                "worker_error",
                "the sidecar is not running",
            ));
        }
        self.starts.borrow_mut().push(params);
        Ok(json!({"ok": true}))
    }
    fn head_of(&self, worktree: &str) -> Result<String, String> {
        gate::current_head(worktree)
    }
    fn base_of(&self, _: &str, _: &str) -> Result<String, String> {
        Ok("base-oid".into())
    }
    fn now(&self) -> Instant {
        self.epoch + self.clock.get()
    }
    fn cancelled(&self) -> bool {
        self.cancelled.get()
    }
}

fn setup() -> (tempfile::TempDir, Store, tempfile::TempDir, String) {
    let (dir, path) = profile();
    let store = Store::open(&path).unwrap();
    let repo = crate::test_support::git_repo();
    crate::test_support::git_in(
        repo.path(),
        &["commit", "-q", "--allow-empty", "-m", "init"],
    );
    let id = filed(&store, &repo, "gp-drv", "Ship the exporter");
    (dir, store, repo, id)
}

/// The agent claims done and is held in review.
fn claim(store: &Store, repo: &tempfile::TempDir, id: &str) {
    let r = complete_task(
        store,
        repo.path().to_str().unwrap(),
        id,
        "done",
        None,
        Some("done"),
        None,
    )
    .unwrap();
    assert_eq!(r["status"], "review", "{r}");
}

fn status_of(store: &Store, id: &str) -> String {
    live(store, id).unwrap()["status"]
        .as_str()
        .unwrap()
        .to_owned()
}
fn logs(store: &Store, id: &str) -> String {
    live(store, id).unwrap()["logs"]
        .as_str()
        .unwrap_or_default()
        .to_owned()
}

/// What the sidecar writes once it has a verdict, under the request the driver sent.
fn verdict(store: &Store, repo: &tempfile::TempDir, id: &str, outcome: &str, fake: &Fake) {
    let sent = fake.starts.borrow();
    let mut r = record(id, outcome, "settled", repo, &head_of(repo));
    r["request_id"] = sent.last().unwrap()["request_id"].clone();
    store_record(store, id, &r);
}

#[test]
fn a_held_claim_starts_one_verification_at_its_head_and_a_duplicate_trigger_starts_none() {
    let (_d, store, repo, id) = setup();
    claim(&store, &repo, &id);
    let fake = Fake::new();
    let mut driver = Driver::default();
    assert_eq!(driver.sweep(&store, &fake).started, 1);
    assert_eq!(driver.sweep(&store, &fake).started, 0);
    assert_eq!(driver.sweep(&store, &fake).started, 0);
    let sent = fake.starts.borrow();
    assert_eq!(sent.len(), 1);
    assert_eq!(sent[0]["head"], head_of(&repo));
    assert_eq!(sent[0]["item_id"], id);
    assert_eq!(
        sent[0]["request_id"],
        format!("auto-{id}-{}", head_of(&repo))
    );
    assert_eq!(status_of(&store, &id), "review");
}

#[test]
fn a_restarted_driver_does_not_start_a_second_verification_for_the_same_head() {
    let (_d, store, repo, id) = setup();
    claim(&store, &repo, &id);
    let fake = Fake::new();
    Driver::default().sweep(&store, &fake);
    // The sidecar has it running; a fresh driver (new process) adopts it.
    let mut r = record(&id, "", "running", &repo, &head_of(&repo));
    r["request_id"] = fake.starts.borrow()[0]["request_id"].clone();
    store_record(&store, &id, &r);
    assert_eq!(Driver::default().sweep(&store, &fake).started, 0);
}

#[test]
fn a_pass_for_the_head_moves_the_held_card_to_done_with_the_verdict_in_its_history() {
    let (_d, store, repo, id) = setup();
    claim(&store, &repo, &id);
    let fake = Fake::new();
    let mut driver = Driver::default();
    driver.sweep(&store, &fake);
    verdict(&store, &repo, &id, "passed", &fake);
    assert_eq!(driver.sweep(&store, &fake).promoted, 1);
    assert_eq!(status_of(&store, &id), "done");
    assert!(logs(&store, &id).contains("Verification passed; moved to done"));
    assert_eq!(driver.sweep(&store, &fake), Report::default());
}

#[test]
fn a_new_head_after_the_claim_is_stale_and_is_neither_started_nor_promoted() {
    let (_d, store, repo, id) = setup();
    claim(&store, &repo, &id);
    crate::test_support::git_in(
        repo.path(),
        &["commit", "-q", "--allow-empty", "-m", "more"],
    );
    let fake = Fake::new();
    let mut driver = Driver::default();
    assert_eq!(driver.sweep(&store, &fake), Report::default());
    assert!(fake.starts.borrow().is_empty());
}

#[test]
fn a_pass_recorded_for_an_older_head_does_not_promote_the_card() {
    let (_d, store, repo, id) = setup();
    claim(&store, &repo, &id);
    let fake = Fake::new();
    let mut driver = Driver::default();
    driver.sweep(&store, &fake);
    let old = head_of(&repo);
    crate::test_support::git_in(
        repo.path(),
        &["commit", "-q", "--allow-empty", "-m", "more"],
    );
    store_record(&store, &id, &record(&id, "passed", "settled", &repo, &old));
    driver.sweep(&store, &fake);
    assert_eq!(status_of(&store, &id), "review");
}

#[test]
fn a_card_the_person_moved_while_it_verified_is_not_moved_by_a_pass() {
    let (_d, store, repo, id) = setup();
    claim(&store, &repo, &id);
    let fake = Fake::new();
    let mut driver = Driver::default();
    driver.sweep(&store, &fake);
    person_edits(&store, &id, |i| {
        i.insert("status".into(), json!("in_progress"));
    });
    verdict(&store, &repo, &id, "passed", &fake);
    driver.sweep(&store, &fake);
    assert_eq!(status_of(&store, &id), "in_progress");
    assert!(!logs(&store, &id).contains("moved to done"));
}

#[test]
fn a_card_the_person_edited_while_it_verified_stays_in_review_with_the_pass_recorded_once() {
    let (_d, store, repo, id) = setup();
    claim(&store, &repo, &id);
    let fake = Fake::new();
    let mut driver = Driver::default();
    driver.sweep(&store, &fake);
    person_edits(&store, &id, |i| {
        i.insert("title".into(), json!("Ship the exporter, v2"));
    });
    verdict(&store, &repo, &id, "passed", &fake);
    assert_eq!(driver.sweep(&store, &fake).parked, 1);
    assert_eq!(status_of(&store, &id), "review");
    let after = logs(&store, &id);
    assert!(after.contains("the card changed meanwhile"), "{after}");
    // A later sweep, even from a fresh driver, does not undo that restraint.
    assert_eq!(Driver::default().sweep(&store, &fake), Report::default());
    assert_eq!(status_of(&store, &id), "review");
    assert_eq!(logs(&store, &id), after);
}

#[test]
fn failed_and_degraded_verdicts_keep_the_card_in_review_and_are_recorded_once() {
    for outcome in ["failed", "degraded"] {
        let (_d, store, repo, id) = setup();
        claim(&store, &repo, &id);
        let fake = Fake::new();
        let mut driver = Driver::default();
        driver.sweep(&store, &fake);
        verdict(&store, &repo, &id, outcome, &fake);
        assert_eq!(driver.sweep(&store, &fake).parked, 1, "{outcome}");
        assert_eq!(status_of(&store, &id), "review", "{outcome}");
        let after = logs(&store, &id);
        assert!(
            after.contains(&format!("--- Verification {outcome} (")),
            "{after}"
        );
        assert_eq!(driver.sweep(&store, &fake), Report::default(), "{outcome}");
        assert_eq!(logs(&store, &id), after, "{outcome}");
    }
}

#[test]
fn a_sidecar_that_is_down_is_recorded_as_could_not_run_not_failed_and_not_retried() {
    let (_d, store, repo, id) = setup();
    claim(&store, &repo, &id);
    let fake = Fake::new();
    fake.sidecar_down.set(true);
    let mut driver = Driver::default();
    assert_eq!(driver.sweep(&store, &fake).could_not_run, 1);
    assert_eq!(status_of(&store, &id), "review");
    let after = logs(&store, &id);
    assert!(after.contains("Verification could not run"), "{after}");
    assert!(after.contains("not a failed verification"), "{after}");
    let shown = status(&store, &fake, &id).unwrap();
    assert_eq!(shown["state"], "could_not_run", "{shown}");
    // Neither a retry storm nor a pass.
    fake.sidecar_down.set(false);
    assert_eq!(driver.sweep(&store, &fake), Report::default());
    assert!(fake.starts.borrow().is_empty());
    let identity = resolve(repo.path().to_str().unwrap()).unwrap().identity;
    let held = gate::check(&store, &id, &identity);
    assert!(matches!(held, gate::Gate::Held(m) if m.contains("could not run")));
}

#[test]
fn a_verification_that_never_settles_is_recorded_as_could_not_run() {
    let (_d, store, repo, id) = setup();
    claim(&store, &repo, &id);
    let fake = Fake::new();
    let mut driver = Driver::default();
    driver.sweep(&store, &fake);
    fake.clock.set(Duration::from_secs(21 * 60));
    assert_eq!(driver.sweep(&store, &fake).could_not_run, 1);
    assert_eq!(
        status(&store, &fake, &id).unwrap()["state"],
        "could_not_run"
    );
}

#[test]
fn a_host_that_is_shutting_down_starts_nothing() {
    let (_d, store, repo, id) = setup();
    claim(&store, &repo, &id);
    let fake = Fake::new();
    fake.cancelled.set(true);
    assert_eq!(Driver::default().sweep(&store, &fake), Report::default());
    assert!(fake.starts.borrow().is_empty());
}

#[test]
fn only_a_bounded_number_of_verifications_run_at_once() {
    let (_d, path) = profile();
    let store = Store::open(&path).unwrap();
    let repo = crate::test_support::git_repo();
    crate::test_support::git_in(
        repo.path(),
        &["commit", "-q", "--allow-empty", "-m", "init"],
    );
    let ids: Vec<String> = (0..MAX_ACTIVE + 2)
        .map(|n| filed(&store, &repo, &format!("gp-{n}"), &format!("Task {n}")))
        .collect();
    for id in &ids {
        claim(&store, &repo, id);
    }
    let fake = Fake::new();
    let mut driver = Driver::default();
    assert_eq!(driver.sweep(&store, &fake).started, MAX_ACTIVE);
    assert_eq!(driver.sweep(&store, &fake).started, 0);
    assert_eq!(fake.starts.borrow().len(), MAX_ACTIVE);
}

#[test]
fn a_card_that_is_not_a_held_claim_is_never_verified() {
    let (_d, store, repo, id) = setup();
    person_edits(&store, &id, |i| {
        i.insert("status".into(), json!("review"));
    });
    assert!(target_of(&live(&store, &id).unwrap()).is_none());
    let fake = Fake::new();
    assert_eq!(Driver::default().sweep(&store, &fake), Report::default());
    let _ = repo;
}

#[test]
fn the_held_block_names_the_worktree_and_head_the_claim_was_made_at() {
    let (_d, store, repo, id) = setup();
    claim(&store, &repo, &id);
    let target = target_of(&live(&store, &id).unwrap()).unwrap();
    assert_eq!(target.head, head_of(&repo));
    assert!(std::fs::canonicalize(&target.worktree).is_ok());
}

#[test]
fn status_names_every_state_apart() {
    let (_d, store, repo, id) = setup();
    let fake = Fake::new();
    assert_eq!(status(&store, &fake, &id).unwrap()["state"], "none");
    let head = head_of(&repo);
    for (state, outcome, want) in [
        ("running", "", "running"),
        ("settled", "passed", "passed"),
        ("settled", "failed", "failed"),
        ("settled", "degraded", "degraded"),
        ("settled", "mystery", "degraded"),
    ] {
        store_record(&store, &id, &record(&id, outcome, state, &repo, &head));
        assert_eq!(status(&store, &fake, &id).unwrap()["state"], want);
    }
    crate::test_support::git_in(
        repo.path(),
        &["commit", "-q", "--allow-empty", "-m", "more"],
    );
    store_record(&store, &id, &record(&id, "passed", "settled", &repo, &head));
    assert_eq!(status(&store, &fake, &id).unwrap()["stale"], true);
}

#[test]
fn decide_is_a_pure_function_of_what_it_is_given() {
    let target = Target {
        worktree: "/w".into(),
        head: "h1".into(),
    };
    let base = |latest: Option<Record>, head: &str, attempt: Option<(Duration, i64)>| {
        decide(&Facts {
            revision: 3,
            logs: "",
            target: &target,
            current_head: Ok(head.into()),
            latest: Ok(latest),
            attempt,
            item_id: "i",
        })
    };
    assert_eq!(base(None, "h1", None), Step::Start);
    assert_eq!(base(None, "h2", None), Step::Skip);
    assert_eq!(base(None, "h1", Some((Duration::ZERO, 3))), Step::Wait);
    let passed = Record {
        item_id: "i".into(),
        head: "h1".into(),
        state: "settled".into(),
        outcome: "passed".into(),
        request_id: "r".into(),
        ..Record::default()
    };
    assert_eq!(
        base(Some(passed.clone()), "h1", Some((Duration::ZERO, 3))),
        Step::Promote
    );
    assert_eq!(
        base(Some(passed.clone()), "h1", Some((Duration::ZERO, 2))),
        Step::PassedButChanged
    );
    let other_head = Record {
        head: "h0".into(),
        ..passed
    };
    assert_eq!(base(Some(other_head), "h1", None), Step::Start);
}

#[test]
fn a_batched_read_names_only_verified_cards_and_marks_a_moved_head_stale() {
    let (_d, store, repo, id) = setup();
    let other = filed(&store, &repo, "gp-other", "Unverified");
    let fake = Fake::new();
    let head = head_of(&repo);
    store_record(&store, &id, &record(&id, "failed", "settled", &repo, &head));
    let ids = [id.clone(), other.clone()];
    let out = status_batch(&store, &fake, &ids).unwrap();
    assert_eq!(out["items"][&id]["state"], "failed", "{out}");
    assert_eq!(out["items"][&id]["stale"], false);
    assert!(out["items"].get(&other).is_none(), "{out}");
    crate::test_support::git_in(
        repo.path(),
        &["commit", "-q", "--allow-empty", "-m", "more"],
    );
    let out = status_batch(&store, &fake, &ids).unwrap();
    assert_eq!(out["items"][&id]["stale"], true);
}

#[test]
fn a_batched_read_that_cannot_read_a_record_fails_instead_of_reporting_none() {
    let (_d, store, repo, id) = setup();
    store_record(
        &store,
        &id,
        &record(&id, "passed", "settled", &repo, &head_of(&repo)),
    );
    store
        .evidence_append(
            gate::EVIDENCE_TYPE,
            Some(&id),
            None,
            None,
            "{\"request_id\": 7}",
        )
        .unwrap();
    let err = status_batch(&store, &Fake::new(), &[id]).unwrap_err();
    assert_eq!(err.code, "store_error");
}

#[test]
fn a_batched_read_is_bounded() {
    let (_d, store, _repo, _id) = setup();
    let ids: Vec<String> = (0..300).map(|n| format!("i{n}")).collect();
    assert_eq!(
        status_batch(&store, &Fake::new(), &ids).unwrap_err().code,
        "invalid_input"
    );
}
