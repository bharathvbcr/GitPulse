//! The workbench half of the agent-output review gate: which run an attempt
//! branch belongs to, what has been decided about the range it would land, and
//! recording a person's decision. The Git half and the gate itself are in
//! `harness::review`.
//!
//! Reviews are `change_review` / `merge_unreviewed` decisions in the run's own
//! `work_decisions` (dc-store `feat/change-review-decisions`). Both are
//! host-raised: the renderer cannot create them (`decisions.create` is
//! host-only), and this module creates and decides one in a single step, on
//! the person's explicit action, so there is never an undecided review for an
//! agent or a stale window to find.

use serde::Serialize;
use serde_json::{json, Value};

use super::reconcile::{walk, HELD};
use super::{query, WorkbenchError, WorkbenchState};

/// The run states in which an attempt has ended and its branch can no longer
/// move under a review (dc-store `runs::TERMINAL`).
const ENDED: [&str; 3] = ["exited", "failed", "cancelled"];

/// A review lasts as long as the store allows a host decision to.
const REVIEW_TTL_SECS: u64 = 30 * 24 * 60 * 60;

/// Upper bound on decisions read for one run: the store's own per-run total.
const MAX_DECISIONS: usize = 2048;

/// The store a guarded Git action consults. Production reads the profile at
/// its default path; a test points the thread it runs on at its own profile.
pub(crate) fn store() -> WorkbenchState {
    #[cfg(test)]
    if let Some(path) = TEST_PROFILE.with(|slot| slot.borrow().clone()) {
        return WorkbenchState::for_profile(&path);
    }
    WorkbenchState::default()
}

#[cfg(test)]
thread_local! {
    static TEST_PROFILE: std::cell::RefCell<Option<std::path::PathBuf>> =
        const { std::cell::RefCell::new(None) };
}

/// Points [`store`] at `path` on this thread until the guard drops.
#[cfg(test)]
pub(crate) fn use_test_profile(path: &std::path::Path) -> impl Drop {
    struct Reset;
    impl Drop for Reset {
        fn drop(&mut self) {
            TEST_PROFILE.with(|slot| *slot.borrow_mut() = None);
        }
    }
    TEST_PROFILE.with(|slot| *slot.borrow_mut() = Some(path.to_path_buf()));
    Reset
}

/// The newest run whose recorded branch is `refs/heads/<branch>`, held or
/// ended, or `None` when no run names that branch.
pub(crate) fn attempt_run(
    state: &WorkbenchState,
    branch: &str,
) -> Result<Option<Value>, WorkbenchError> {
    let head_ref = format!("refs/heads/{branch}");
    let mut newest: Option<Value> = None;
    for run_state in ENDED.iter().chain(HELD.iter()) {
        let complete = walk(state, run_state, true, |run| {
            if run["head_ref"].as_str() == Some(head_ref.as_str()) {
                let created = run["created_at"].as_i64().unwrap_or(i64::MIN);
                let newer = newest
                    .as_ref()
                    .is_none_or(|best| created > best["created_at"].as_i64().unwrap_or(i64::MIN));
                if newer {
                    newest = Some(run.clone());
                }
                return false;
            }
            true
        })?;
        if !complete {
            return Err(WorkbenchError::new(
                "too_many_runs",
                format!("too many {run_state} runs are recorded to find the one for {branch}"),
            ));
        }
    }
    Ok(newest)
}

/// Whether a run has ended, so its range can be reviewed.
pub(crate) fn ended(run: &Value) -> bool {
    ENDED.contains(&run["state"].as_str().unwrap_or(""))
}

/// What has been decided about landing `head_oid` from a run.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "status", rename_all = "snake_case")]
pub enum Review {
    /// A person approved exactly this range.
    Approved { note: Option<String> },
    /// A person chose to land exactly this range unreviewed, and said why.
    Overridden { note: String },
    /// A person asked for changes to this range.
    ChangesRequested { note: Option<String> },
    /// A person refused this range.
    Denied { note: Option<String> },
    /// Something was approved, but not this tip: the branch moved since.
    Stale { reviewed_head: String },
    /// Nothing was decided about this range.
    Unreviewed,
}

/// Reads the run's review decisions and classifies them against `head_oid`.
///
/// A refusal of the exact range outranks an approval of it — the two cannot
/// coexist for one head, since the store allows one decision per kind per head
/// per run, but an override and a refusal can, and the refusal is the more
/// recent judgement on what was reviewed.
pub(crate) fn review_of(
    state: &WorkbenchState,
    run_id: &str,
    head_oid: &str,
) -> Result<Review, WorkbenchError> {
    let mut decisions = Vec::new();
    let mut cursor: Option<String> = None;
    loop {
        let mut input = json!({"run_id": run_id, "limit": 100});
        if let Some(cursor) = &cursor {
            input["cursor"] = json!(cursor);
        }
        let page = state.with_store(|store| query(store, "decisions.list", &input.to_string()))?;
        for item in page["items"].as_array().into_iter().flatten() {
            if matches!(
                item["kind"].as_str(),
                Some("change_review" | "merge_unreviewed")
            ) {
                decisions.push(item.clone());
            }
        }
        if decisions.len() > MAX_DECISIONS {
            return Err(WorkbenchError::new(
                "too_many_decisions",
                "more decisions are recorded for this run than the store allows",
            ));
        }
        match page["next_cursor"].as_str() {
            Some(next) if page["has_more"] == true => cursor = Some(next.to_owned()),
            _ => break,
        }
    }
    let head_of = |item: &Value| -> Option<String> {
        serde_json::from_str::<Value>(item["payload"].as_str()?)
            .ok()?
            .get("head_oid")?
            .as_str()
            .map(str::to_owned)
    };
    let note = |item: &Value| item["answer"].as_str().map(str::to_owned);
    let decided = |item: &&Value| item["state"] == "decided";

    let here: Vec<&Value> = decisions
        .iter()
        .filter(|item| head_of(item).as_deref() == Some(head_oid))
        .filter(decided)
        .collect();
    let review = here.iter().find(|item| item["kind"] == "change_review");
    match review.map(|item| (item["decision"].as_str().unwrap_or(""), note(item))) {
        Some(("request_changes", note)) => return Ok(Review::ChangesRequested { note }),
        Some(("deny", note)) => return Ok(Review::Denied { note }),
        Some(("approve", note)) => return Ok(Review::Approved { note }),
        _ => {}
    }
    if let Some(item) = here
        .iter()
        .find(|item| item["kind"] == "merge_unreviewed" && item["decision"] == "allow_once")
    {
        return Ok(Review::Overridden {
            note: note(item).unwrap_or_default(),
        });
    }
    if let Some(reviewed_head) = decisions
        .iter()
        .filter(decided)
        .filter(|item| item["kind"] == "change_review" && item["decision"] == "approve")
        .filter_map(head_of)
        .next_back()
    {
        return Ok(Review::Stale { reviewed_head });
    }
    Ok(Review::Unreviewed)
}

/// Records a person's decision on landing `payload`'s range from `run`.
///
/// `decision` is `approve`, `request_changes` or `deny` (a `change_review`),
/// or `merge_unreviewed` (an override, which the store requires a note for).
pub(crate) fn record(
    state: &WorkbenchState,
    run: &Value,
    payload: &Value,
    decision: &str,
    note: Option<&str>,
) -> Result<Value, WorkbenchError> {
    let (kind, stored_decision) = match decision {
        "approve" | "request_changes" | "deny" => ("change_review", decision),
        "merge_unreviewed" => ("merge_unreviewed", "allow_once"),
        other => {
            return Err(WorkbenchError::new(
                "invalid_input",
                format!("unknown review decision {other:?}"),
            ))
        }
    };
    let head = payload["head_oid"].as_str().unwrap_or_default();
    let run_id = run["id"].as_str().unwrap_or_default();
    let text = payload.to_string();
    let digest = crate::tool_install::release::bytes_sha256_hex(text.as_bytes())
        .map_err(|e| WorkbenchError::new("store_error", e))?;
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or_default();
    let short = |s: &str, n: usize| {
        s.chars()
            .filter(char::is_ascii_alphanumeric)
            .take(n)
            .collect::<String>()
    };
    let id = format!(
        "{}-{}-{}",
        short(kind, 6),
        short(run_id, 24),
        short(head, 16)
    );
    let create = json!({
        "id": id,
        "request_id": format!("{id}-create"),
        "expected_revision": 0,
        "run_id": run_id,
        "owner_id": run["owner_id"],
        "session_id": run["session_id"],
        "provider_thread_id": "host",
        "provider_turn_id": kind,
        "protocol_request_id": head,
        "kind": kind,
        "payload": text,
        "payload_digest": digest,
        "deadline": now + REVIEW_TTL_SECS,
    });
    // One id per (kind, run, head), as the store allows one such decision.
    // A retry after a decide that failed finds the decision pending and
    // finishes it; one already decided is refused rather than overwritten.
    let existing =
        state.with_store(|store| query(store, "decisions.get", &json!({"id": id}).to_string()));
    match existing {
        Ok(found) if found["item"]["state"] == "pending" => {}
        Ok(found) => {
            return Err(WorkbenchError::new(
                "already_decided",
                format!(
                    "this range was already decided ({}); a new review needs a new commit",
                    found["item"]["decision"].as_str().unwrap_or("unknown")
                ),
            ))
        }
        Err(error) if error.code == "not_found" => {
            state.with_store(|store| query(store, "decisions.create", &create.to_string()))?;
        }
        Err(error) => return Err(error),
    }
    let mut decide = json!({
        "id": id,
        "request_id": format!("{id}-decide"),
        "expected_revision": 1,
        "payload_digest": digest,
        "decision": stored_decision,
    });
    if let Some(note) = note.map(str::trim).filter(|note| !note.is_empty()) {
        decide["answer"] = json!(note);
    }
    state.with_store(|store| query(store, "decisions.decide", &decide.to_string()))
}

/// One attempt's review, as its task panel shows it.
#[derive(Debug, Clone, Serialize)]
pub struct AttemptReview {
    pub run_id: String,
    pub branch: String,
    pub base_oid: String,
    pub head_oid: String,
    pub files_changed: u64,
    /// Whether the attempt has ended, so its range can be decided.
    pub ended: bool,
    pub review: Review,
    /// Whether the review gate applies to this repository; `None` with
    /// `gate_error` set when that could not be read.
    pub gate_on: Option<bool>,
    pub gate_error: Option<String>,
}

/// Reads one attempt's review — and, with `decision`, records the person's
/// decision on its current range first.
pub(crate) fn attempt_review(
    state: &WorkbenchState,
    run_id: &str,
    decision: Option<(&str, Option<&str>)>,
) -> Result<AttemptReview, String> {
    let run = state
        .request("runs.get", &json!({"id": run_id}).to_string())
        .map_err(|e| e.message)?["item"]
        .clone();
    let branch = run["head_ref"]
        .as_str()
        .and_then(|r| r.strip_prefix("refs/heads/"))
        .filter(|b| b.starts_with(crate::harness::review::ATTEMPT_BRANCH_PREFIX))
        .ok_or("this attempt did not run on its own attempt branch, so it has no range to review")?
        .to_string();
    let base = run["head_oid"]
        .as_str()
        .ok_or("this attempt recorded no starting commit")?
        .to_string();
    let cwd = run["cwd"]
        .as_str()
        .ok_or("this attempt recorded no checkout")?;
    let repo = std::path::Path::new(cwd);
    let tip = crate::engine::git_cli::git_text(
        repo,
        &[
            "rev-parse",
            "--verify",
            &format!("refs/heads/{branch}^{{commit}}"),
        ],
    )?
    .trim()
    .to_string();
    let repository = run["repository_id"].as_str().unwrap_or_default();
    let payload = crate::harness::review::review_payload(repo, repository, &branch, &base, &tip)?;
    if let Some((decision, note)) = decision {
        if !ended(&run) {
            return Err(
                "this attempt has not ended; its commits can be reviewed once it has".into(),
            );
        }
        record(state, &run, &payload, decision, note).map_err(|e| e.message)?;
    }
    let review = review_of(state, run_id, &tip).map_err(|e| e.message)?;
    let gate = crate::harness::review::common_dir(repo)
        .and_then(|dir| crate::tool_config::review_gate_enabled(&dir));
    Ok(AttemptReview {
        run_id: run_id.to_string(),
        branch,
        base_oid: base,
        head_oid: tip,
        files_changed: payload["files_changed"].as_u64().unwrap_or_default(),
        ended: ended(&run),
        review,
        gate_on: gate.as_ref().ok().copied(),
        gate_error: gate.err(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::engine::git_cli::git_text;
    use crate::harness::review as gate;
    use std::path::Path;
    use std::sync::Arc;

    const BRANCH: &str = "gitpulse/fix-1234abcd";

    fn git(root: &Path, args: &[&str]) -> String {
        let mut all = vec![
            "-c",
            "user.name=Review Test",
            "-c",
            "user.email=review@example.invalid",
            "-c",
            "commit.gpgsign=false",
        ];
        all.extend_from_slice(args);
        git_text(root, &all).unwrap().trim().to_string()
    }

    fn commit(root: &Path, name: &str) -> String {
        std::fs::write(root.join(name), name).unwrap();
        git(root, &["add", name]);
        git(root, &["commit", "-q", "-m", name]);
        git(root, &["rev-parse", "HEAD"])
    }

    /// An attempt that ran on `BRANCH`, committed, and ended; the checkout is
    /// back on `main` with the attempt's commits not yet landed.
    fn ended_attempt(root: &Path, state: &WorkbenchState) -> (Value, String) {
        crate::engine::git_cli::git_global(&["init", "-q", "-b", "main", root.to_str().unwrap()])
            .unwrap();
        crate::test_support::trust_repo(root);
        commit(root, "base.txt");
        git(root, &["switch", "-q", "-c", BRANCH]);
        state
            .register(root.to_str().unwrap(), "repo", "register")
            .unwrap();
        state.request("items.put", &json!({"id":"task","request_id":"task","expected_revision":0,"title":"Fix it","description":"d","repository_ids":["repo"],"primary_repository_id":"repo"}).to_string()).unwrap();
        state.request("runs.prepare_terminal", &json!({"id":"attempt","request_id":"prepare","task_id":"task","source_revision":1,"repository_id":"repo","repository_revision":1,"provider":"claude","permission_mode":"ask","repo_path":root}).to_string()).unwrap();
        let store = |method: &str, input: Value| {
            state
                .with_store(|store| query(store, method, &input.to_string()))
                .unwrap()
        };
        let owner = "native-1-1-1";
        store(
            "runs.claim",
            json!({"id":"attempt","request_id":"claim","expected_revision":1,"owner_id":owner,"session_id":"session"}),
        );
        let pid = std::process::id();
        let birth = crate::workbench::process_birth::read(pid).unwrap();
        store(
            "runs.started",
            json!({"id":"attempt","request_id":"started","expected_revision":2,"owner_id":owner,"session_id":"session","process_id":pid,"process_start":birth}),
        );
        store(
            "runs.finish",
            json!({"id":"attempt","request_id":"finish","expected_revision":3,"owner_id":owner,"session_id":"session","outcome":"exited","reason":"done","exit_code":0}),
        );
        let tip = commit(root, "agent.txt");
        git(root, &["switch", "-q", "main"]);
        let run = attempt_run(state, BRANCH)
            .unwrap()
            .expect("the attempt's run");
        assert_eq!(run["state"], "exited", "{run}");
        (run, tip)
    }

    fn payload(root: &Path, run: &Value, tip: &str) -> Value {
        gate::review_payload(root, "repo", BRANCH, run["head_oid"].as_str().unwrap(), tip).unwrap()
    }

    /// The acceptance case: with the gate on, an attempt nobody reviewed
    /// cannot be merged — not by name, not squashed, not cherry-picked by id —
    /// and the refusal is GitPulse's, before the harness is asked.
    #[test]
    fn an_unreviewed_attempt_cannot_be_merged() {
        let config_lock = crate::tool_config::lock_config_env();
        let config = tempfile::tempdir().unwrap();
        let _env = crate::test_support::env::bind_env(&config_lock)
            .set(
                crate::tool_config::TOOL_CONFIG_ENV,
                config.path().join("tools.json"),
            )
            .invalidating(crate::tool_config::invalidate_cache);
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().canonicalize().unwrap();
        let profile = dir.path().join("profile.sqlite");
        let _profile = use_test_profile(&profile);
        let state = WorkbenchState(Arc::new(super::super::Inner {
            path: Some(profile.clone()),
            ..super::super::Inner::default()
        }));
        let (run, tip) = ended_attempt(&root, &state);
        let repo = root.to_str().unwrap();
        crate::tool_config::set_review_gate(Some(&gate::common_dir(&root).unwrap()), Some(true))
            .unwrap();

        let refused = crate::harness::guard_command(repo, &["git", "merge", "--no-edit", BRANCH])
            .expect_err("an unreviewed attempt was allowed to merge");
        assert!(refused.contains(gate::RULE_UNREVIEWED), "{refused}");
        assert!(refused.contains("Blocked by GitPulse"), "{refused}");
        for argv in [
            vec!["git", "merge", "--squash", BRANCH],
            vec!["git", "merge", "--ff-only", &tip],
            vec!["git", "cherry-pick", "--no-edit", &tip],
        ] {
            let verdict = gate::gate(repo, &argv).expect_err("unreviewed landing allowed");
            assert_eq!(verdict.rule, gate::RULE_UNREVIEWED, "{argv:?}");
        }
        // Branches that are not attempts, and resuming a merge, are not judged.
        git(&root, &["branch", "feature"]);
        assert!(gate::gate(repo, &["git", "merge", "feature"]).is_ok());
        assert!(gate::gate(repo, &["git", "merge", "--abort"]).is_ok());

        // Approved: exactly this range lands.
        record(
            &state,
            &run,
            &payload(&root, &run, &tip),
            "approve",
            Some("looks right"),
        )
        .unwrap();
        assert_eq!(
            review_of(&state, "attempt", &tip).unwrap(),
            Review::Approved {
                note: Some("looks right".into())
            }
        );
        gate::gate(repo, &["git", "merge", "--no-edit", BRANCH]).expect("an approved range lands");

        // The branch moves after approval: stale, not approved.
        git(&root, &["switch", "-q", BRANCH]);
        let moved = commit(&root, "more.txt");
        git(&root, &["switch", "-q", "main"]);
        let stale = gate::gate(repo, &["git", "merge", BRANCH])
            .expect_err("a moved branch landed on an old approval");
        assert_eq!(stale.rule, gate::RULE_STALE, "{}", stale.reason);

        // Changes requested on the new tip: refused with the note.
        record(
            &state,
            &run,
            &payload(&root, &run, &moved),
            "request_changes",
            Some("add a test"),
        )
        .unwrap();
        let asked =
            gate::gate(repo, &["git", "merge", BRANCH]).expect_err("changes were requested");
        assert_eq!(asked.rule, gate::RULE_REFUSED);
        assert!(asked.reason.contains("add a test"), "{}", asked.reason);
        // A decided range cannot be decided again; a new commit is a new range.
        assert_eq!(
            record(&state, &run, &payload(&root, &run, &moved), "approve", None)
                .unwrap_err()
                .code,
            "already_decided"
        );

        // An explicit, explained override lands an unreviewed range.
        git(&root, &["switch", "-q", BRANCH]);
        let third = commit(&root, "third.txt");
        git(&root, &["switch", "-q", "main"]);
        assert!(
            record(
                &state,
                &run,
                &payload(&root, &run, &third),
                "merge_unreviewed",
                None
            )
            .is_err(),
            "an override needs a reason"
        );
        record(
            &state,
            &run,
            &payload(&root, &run, &third),
            "merge_unreviewed",
            Some("hotfix, reviewed on call"),
        )
        .unwrap();
        gate::gate(repo, &["git", "merge", BRANCH]).expect("an explained override lands");

        // Off for this repository: nothing is judged.
        git(&root, &["switch", "-q", BRANCH]);
        commit(&root, "fourth.txt");
        git(&root, &["switch", "-q", "main"]);
        assert!(gate::gate(repo, &["git", "merge", BRANCH]).is_err());
        crate::tool_config::set_review_gate(Some(&gate::common_dir(&root).unwrap()), Some(false))
            .unwrap();
        assert!(gate::gate(repo, &["git", "merge", BRANCH]).is_ok());
    }
}
