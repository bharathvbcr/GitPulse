//! Host coordinator for the cross-repository execution and integration queue.
//!
//! `queue.enqueue` and `queue.verify` in `dc-store` record checkout
//! snapshots, acquire leases across repositories in canonical `repository_id`
//! order, and verify every repository member with `publication` fixed to
//! `"manual_per_repository"`. The host owns observing real local checkouts
//! (`intake::resolve`, `terminal_launch::checkout`), checking repository
//! identity, reading Manvi verification verdicts (`verification_gate`), and
//! sweeping expired queues so renderers cannot forge checkout paths, commits,
//! or verification outcomes.

use super::verification_gate::{self, Gate};
use super::{intake, is_record_id, query, terminal_launch, WorkbenchError, WorkbenchState};
use dc_store::Store;
use serde_json::{json, Map, Value};
use std::collections::{HashMap, HashSet};

const SWEEP_PAGE_SIZE: usize = 100;
const SWEEP_MAX_PAGES: usize = 10;

fn invalid(message: impl Into<String>) -> WorkbenchError {
    WorkbenchError::new("invalid_input", message)
}

fn parse_object(raw_input: &str, name: &str) -> Result<Map<String, Value>, WorkbenchError> {
    let value: Value = serde_json::from_str(raw_input).map_err(|e| invalid(e.to_string()))?;
    value
        .as_object()
        .cloned()
        .ok_or_else(|| invalid(format!("{name} must be a JSON object")))
}

fn check_allowed_keys(
    obj: &Map<String, Value>,
    allowed: &[&str],
    context: &str,
) -> Result<(), WorkbenchError> {
    for key in obj.keys() {
        if !allowed.contains(&key.as_str()) {
            return Err(invalid(format!("unexpected field {key} in {context}")));
        }
    }
    Ok(())
}

fn required_string<'a>(obj: &'a Map<String, Value>, key: &str) -> Result<&'a str, WorkbenchError> {
    obj.get(key)
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .ok_or_else(|| invalid(format!("{key} must be a non-empty string")))
}

fn optional_hex_oid(value: Option<&str>) -> Option<String> {
    value
        .map(str::trim)
        .filter(|oid| {
            [40, 64].contains(&oid.len())
                && oid
                    .bytes()
                    .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
        })
        .map(ToOwned::to_owned)
}

fn lookup_repository_identity(
    store: &Store,
    repository_id: &str,
    cache: &mut HashMap<String, String>,
) -> Result<String, WorkbenchError> {
    if let Some(identity) = cache.get(repository_id) {
        return Ok(identity.clone());
    }
    let response = query(
        store,
        "repositories.get",
        &json!({ "id": repository_id }).to_string(),
    )?;
    let identity = response["item"]["identity_key"]
        .as_str()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .ok_or_else(|| {
            WorkbenchError::new(
                "protocol_error",
                format!("repository {repository_id} is missing an identity_key"),
            )
        })?
        .to_owned();
    cache.insert(repository_id.to_owned(), identity.clone());
    Ok(identity)
}

fn resolve_verified_checkout(
    repo_path: &str,
    repository_id: &str,
    expected_identity: &str,
) -> Result<terminal_launch::Checkout, WorkbenchError> {
    let local = intake::resolve(repo_path)?;
    if local.identity != expected_identity {
        return Err(WorkbenchError::new(
            "repository_mismatch",
            format!("checkout {repo_path} does not belong to repository {repository_id}"),
        ));
    }
    terminal_launch::checkout(repo_path)
}

fn resolve_run_checkout_head(store: &Store, run_id: &str) -> Option<String> {
    let run = query(store, "runs.get", &json!({ "id": run_id }).to_string()).ok()?;
    let cwd = run["item"]["cwd"]
        .as_str()
        .map(str::trim)
        .filter(|s| !s.is_empty())?;
    let observed = terminal_launch::checkout(cwd).ok()?;
    optional_hex_oid(observed.head_oid.as_deref())
}

/// Host coordinator for `queue.enqueue_host`.
///
/// Resolves each member's local checkout against its registered `work_repositories`
/// row (`identity_key`), snapshots `(repository_id, git_dir, base_oid)`, and
/// delegates atomic canonical-order lease acquisition to `queue.enqueue`.
pub(super) fn enqueue_host(
    state: &WorkbenchState,
    raw_input: &str,
) -> Result<Value, WorkbenchError> {
    let top = parse_object(raw_input, "queue.enqueue_host")?;
    check_allowed_keys(
        &top,
        &[
            "id",
            "request_id",
            "expected_revision",
            "title",
            "concurrency_limit",
            "active_timeout_seconds",
            "elapsed_timeout_seconds",
            "members",
        ],
        "queue.enqueue_host",
    )?;
    let _id = required_string(&top, "id")?;
    let _request_id = required_string(&top, "request_id")?;
    let members_raw = top
        .get("members")
        .and_then(Value::as_array)
        .filter(|arr| !arr.is_empty())
        .ok_or_else(|| invalid("members must be a non-empty array"))?;

    let mut prepared_members = Vec::with_capacity(members_raw.len());
    let mut repo_cache = HashMap::new();
    for entry in members_raw {
        let member = entry
            .as_object()
            .ok_or_else(|| invalid("each queue member must be a JSON object"))?;
        check_allowed_keys(
            member,
            &[
                "member_id",
                "repository_id",
                "repo_path",
                "task_id",
                "depends_on",
            ],
            "queue member",
        )?;
        let member_id = required_string(member, "member_id")?;
        let repository_id = required_string(member, "repository_id")?;
        if !is_record_id(repository_id) {
            return Err(invalid(format!("invalid repository_id {repository_id}")));
        }
        let repo_path = required_string(member, "repo_path")?;

        let expected_identity = state.with_store(|store| {
            lookup_repository_identity(store, repository_id, &mut repo_cache)
        })?;
        let observed = resolve_verified_checkout(repo_path, repository_id, &expected_identity)?;
        let base_oid = observed
            .head_oid
            .as_deref()
            .and_then(|h| optional_hex_oid(Some(h)))
            .ok_or_else(|| {
                invalid(format!(
                    "checkout {repo_path} has no valid HEAD commit to snapshot"
                ))
            })?;

        let mut forwarded = Map::new();
        forwarded.insert("member_id".into(), json!(member_id));
        forwarded.insert("repository_id".into(), json!(repository_id));
        forwarded.insert("git_dir".into(), json!(observed.git_dir));
        forwarded.insert("base_oid".into(), json!(base_oid));
        if let Some(task_id) = member.get("task_id").filter(|v| !v.is_null()) {
            forwarded.insert("task_id".into(), task_id.clone());
        }
        if let Some(depends_on) = member.get("depends_on").filter(|v| !v.is_null()) {
            forwarded.insert("depends_on".into(), depends_on.clone());
        }
        prepared_members.push(Value::Object(forwarded));
    }

    let mut payload = top;
    payload.insert("members".into(), Value::Array(prepared_members));
    let raw = Value::Object(payload).to_string();
    state.with_store(|store| query(store, "queue.enqueue", &raw))
}

/// Host coordinator for `queue.verify_combined`.
///
/// Enforces `publication: "manual_per_repository"`, enriches missing commit
/// heads from verified checkouts or run working trees, and evaluates Manvi
/// `verification_gate` verdicts automatically when `results` is omitted.
pub(super) fn verify_combined(
    state: &WorkbenchState,
    raw_input: &str,
) -> Result<Value, WorkbenchError> {
    let top = parse_object(raw_input, "queue.verify_combined")?;
    check_allowed_keys(
        &top,
        &[
            "id",
            "request_id",
            "expected_revision",
            "publication",
            "checkouts",
            "results",
        ],
        "queue.verify_combined",
    )?;
    let queue_id = required_string(&top, "id")?;
    let request_id = required_string(&top, "request_id")?;
    let expected_revision = top
        .get("expected_revision")
        .and_then(Value::as_i64)
        .filter(|rev| *rev >= 1)
        .ok_or_else(|| invalid("expected_revision must be a positive integer"))?;
    if let Some(publication) = top.get("publication").filter(|v| !v.is_null()) {
        if publication.as_str() != Some("manual_per_repository") {
            return Err(invalid(
                "cross-repository queue publication must be manual_per_repository",
            ));
        }
    }

    let queue_doc = state
        .with_store(|store| query(store, "queue.get", &json!({ "id": queue_id }).to_string()))?;
    let members = queue_doc["item"]["members"]
        .as_array()
        .ok_or_else(|| WorkbenchError::new("protocol_error", "queue.get omitted members"))?;
    let by_member: HashMap<&str, &Value> = members
        .iter()
        .filter_map(|m| m["member_id"].as_str().map(|id| (id, m)))
        .collect();

    let mut checkout_paths: HashMap<String, String> = HashMap::new();
    if let Some(raw_checkouts) = top.get("checkouts").filter(|v| !v.is_null()) {
        let map = raw_checkouts
            .as_object()
            .ok_or_else(|| invalid("checkouts must be an object keyed by member_id"))?;
        for (member_id, path_val) in map {
            if !by_member.contains_key(member_id.as_str()) {
                return Err(invalid(format!(
                    "checkouts names unknown queue member {member_id}"
                )));
            }
            let repo_path = path_val
                .as_str()
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .ok_or_else(|| {
                    invalid(format!(
                        "checkout path for member {member_id} must be a non-empty string"
                    ))
                })?;
            checkout_paths.insert(member_id.clone(), repo_path.to_owned());
        }
    }

    let mut repo_cache = HashMap::new();
    let mut checkout_heads: HashMap<String, Option<String>> = HashMap::new();
    for (member_id, repo_path) in &checkout_paths {
        let member = by_member[member_id.as_str()];
        let repository_id = member["repository_id"].as_str().unwrap_or_default();
        let expected_identity = state.with_store(|store| {
            lookup_repository_identity(store, repository_id, &mut repo_cache)
        })?;
        let observed = resolve_verified_checkout(repo_path, repository_id, &expected_identity)?;
        checkout_heads.insert(
            member_id.clone(),
            optional_hex_oid(observed.head_oid.as_deref()),
        );
    }

    let verified_results = match top.get("results").filter(|v| !v.is_null()) {
        Some(raw_results) => {
            let entries = raw_results
                .as_array()
                .filter(|arr| !arr.is_empty())
                .ok_or_else(|| invalid("results must be a non-empty array"))?;
            let mut out = Vec::with_capacity(entries.len());
            for entry in entries {
                let row = entry
                    .as_object()
                    .ok_or_else(|| invalid("each verification result must be a JSON object"))?;
                check_allowed_keys(
                    row,
                    &[
                        "member_id",
                        "ok",
                        "head_oid",
                        "command",
                        "exit_code",
                        "summary",
                    ],
                    "verification result",
                )?;
                let member_id = required_string(row, "member_id")?;
                let member = by_member
                    .get(member_id)
                    .ok_or_else(|| invalid(format!("unknown queue member {member_id}")))?;
                let head_oid = match row.get("head_oid").and_then(Value::as_str) {
                    Some(explicit) => Some(explicit.to_owned()),
                    None => checkout_heads
                        .get(member_id)
                        .and_then(|o| o.clone())
                        .or_else(|| {
                            let run_id = member["run_id"].as_str()?;
                            state
                                .with_store(|store| Ok(resolve_run_checkout_head(store, run_id)))
                                .ok()
                                .flatten()
                        }),
                };
                let mut forwarded = row.clone();
                if let Some(oid) = head_oid {
                    forwarded.insert("head_oid".into(), json!(oid));
                }
                out.push(Value::Object(forwarded));
            }
            out
        }
        None => {
            let mut out = Vec::with_capacity(members.len());
            for member in members {
                let member_id = member["member_id"].as_str().unwrap_or_default();
                let repository_id = member["repository_id"].as_str().unwrap_or_default();
                let member_state = member["state"].as_str().unwrap_or_default();
                let checkout_head = checkout_heads
                    .get(member_id)
                    .and_then(|o| o.clone())
                    .or_else(|| {
                        let run_id = member["run_id"].as_str()?;
                        state
                            .with_store(|store| Ok(resolve_run_checkout_head(store, run_id)))
                            .ok()
                            .flatten()
                    });

                let (ok, summary, head_oid) = if member_state != "passed" {
                    (
                        false,
                        format!("member did not pass execution (state: {member_state})"),
                        checkout_head,
                    )
                } else if let Some(task_id) = member["task_id"].as_str() {
                    let repo_identity = state.with_store(|store| {
                        lookup_repository_identity(store, repository_id, &mut repo_cache)
                    })?;
                    let (gate, latest_head) = state.with_store(|store| {
                        let memo = verification_gate::Memo::shared();
                        let gate = verification_gate::check(
                            memo,
                            store,
                            task_id,
                            &repo_identity,
                            checkout_head.as_deref(),
                        );
                        let latest_head = memo
                            .latest(store, task_id)
                            .ok()
                            .flatten()
                            .and_then(|rec| optional_hex_oid(Some(&rec.head)));
                        Ok((gate, latest_head))
                    })?;
                    match gate {
                        Gate::Passed(reason) => match (&checkout_head, &latest_head) {
                            (Some(current), Some(verified)) if current != verified => (
                                false,
                                format!(
                                    "verification recorded for {verified} does not match checkout HEAD {current}"
                                ),
                                Some(current.clone()),
                            ),
                            (None, _) | (_, None) => (
                                false,
                                "verification passed but the checkout HEAD or the verified head could not be read, so the verdict cannot be tied to the work".to_owned(),
                                checkout_head.or(latest_head),
                            ),
                            _ => (true, reason, checkout_head.or(latest_head)),
                        },
                        Gate::Held(reason) => (false, reason, checkout_head.or(latest_head)),
                    }
                } else {
                    (
                        false,
                        "member has no linked task for automatic verification gate check"
                            .to_owned(),
                        checkout_head,
                    )
                };

                let mut row = Map::new();
                row.insert("member_id".into(), json!(member_id));
                row.insert("ok".into(), json!(ok));
                row.insert("summary".into(), json!(summary));
                if let Some(oid) = head_oid {
                    row.insert("head_oid".into(), json!(oid));
                }
                out.push(Value::Object(row));
            }
            out
        }
    };

    let verify_payload = json!({
        "id": queue_id,
        "request_id": request_id,
        "expected_revision": expected_revision,
        "publication": "manual_per_repository",
        "results": verified_results,
    });
    state.with_store(|store| query(store, "queue.verify", &verify_payload.to_string()))
}

/// Sweeps active queues (`leased` / `verifying`) and times out any queue that
/// exceeded its active (`45m` max) or elapsed (`2h` max) bound.
pub(super) fn sweep_expired_queues(state: &WorkbenchState) -> usize {
    let mut candidates: Vec<(String, i64)> = Vec::new();
    let mut seen_ids: HashSet<String> = HashSet::new();

    for q_state in ["leased", "verifying"] {
        let mut cursor: Option<String> = None;
        for _ in 0..SWEEP_MAX_PAGES {
            let mut input = json!({ "state": q_state, "limit": SWEEP_PAGE_SIZE });
            if let Some(ref next) = cursor {
                input["cursor"] = json!(next);
            }
            let page =
                match state.with_store(|store| query(store, "queue.list", &input.to_string())) {
                    Ok(page) => page,
                    Err(error) => {
                        log::warn!(
                            target: "workbench",
                            "queue expiry sweep could not list {q_state} queues: {}: {}",
                            error.code,
                            error.message
                        );
                        break;
                    }
                };
            for item in page["items"].as_array().into_iter().flatten() {
                if let (Some(id), Some(revision)) = (item["id"].as_str(), item["revision"].as_i64())
                {
                    if seen_ids.insert(id.to_owned()) {
                        candidates.push((id.to_owned(), revision));
                    }
                }
            }
            match page["next_cursor"].as_str() {
                Some(next) if page["has_more"] == true => cursor = Some(next.to_owned()),
                _ => break,
            }
        }
    }

    let mut timed_out = 0;
    for (id, revision) in candidates {
        let req = json!({
            "id": id,
            "request_id": format!("host-queue-sweep-{id}-r{revision}"),
            "expected_revision": revision,
        });
        match state.with_store(|store| query(store, "queue.sweep", &req.to_string())) {
            Ok(resp) if resp["item"]["state"] == "timed_out" => timed_out += 1,
            Ok(_) => {}
            Err(error)
                if matches!(
                    error.code.as_str(),
                    "not_expired" | "invalid_state" | "revision_conflict"
                ) => {}
            Err(error) => {
                log::warn!(
                    target: "workbench",
                    "queue sweep skipped {id}: {}: {}",
                    error.code,
                    error.message
                );
            }
        }
    }
    timed_out
}

#[cfg(test)]
mod tests {
    use super::super::intake;
    use super::super::verification_gate::EVIDENCE_TYPE;
    use super::super::Inner;
    use super::*;
    use crate::engine::git_cli::{git_global, git_text};
    use std::path::Path;
    use std::sync::Arc;

    fn host_state(profile_path: &Path) -> WorkbenchState {
        WorkbenchState(Arc::new(Inner {
            path: Some(profile_path.to_path_buf()),
            ..Inner::default()
        }))
    }

    fn git_init(root: &Path) {
        std::fs::create_dir_all(root).unwrap();
        git_global(&["init", "-b", "main", root.to_str().unwrap()]).unwrap();
        crate::test_support::trust_repo(root);
        for (k, v) in [
            ("user.name", "Test"),
            ("user.email", "test@example.com"),
            ("commit.gpgsign", "false"),
        ] {
            git_text(root, &["config", k, v]).unwrap();
        }
    }

    fn git_commit(root: &Path, filename: &str) -> String {
        std::fs::write(root.join(filename), "content\n").unwrap();
        git_text(root, &["add", filename]).unwrap();
        git_text(root, &["commit", "-m", "commit"]).unwrap();
        git_text(root, &["rev-parse", "HEAD"])
            .unwrap()
            .trim()
            .to_owned()
    }

    fn register_repo(state: &WorkbenchState, id: &str, root: &Path) {
        let local = intake::resolve(root.to_str().unwrap()).unwrap();
        state
            .request(
                "repositories.put",
                &json!({
                    "id": id,
                    "request_id": format!("put-repo-{id}"),
                    "expected_revision": 0,
                    "name": id,
                    "identity_key": local.identity,
                })
                .to_string(),
            )
            .unwrap();
    }

    fn create_task(state: &WorkbenchState, id: &str, repo_id: &str) {
        state
            .request(
                "items.put",
                &json!({
                    "id": id,
                    "request_id": format!("put-task-{id}"),
                    "expected_revision": 0,
                    "title": format!("Task {id}"),
                    "description": "Verify me",
                    "repository_ids": [repo_id],
                    "primary_repository_id": repo_id,
                })
                .to_string(),
            )
            .unwrap();
    }

    fn record_verification_evidence(
        state: &WorkbenchState,
        item_id: &str,
        worktree: &Path,
        head: &str,
        outcome: &str,
        summary: &str,
    ) {
        let record = json!({
            "request_id": format!("ev-{item_id}-{outcome}"),
            "item_id": item_id,
            "state": "settled",
            "outcome": outcome,
            "worktree": worktree.canonicalize().unwrap(),
            "head": head,
            "criteria": [{
                "index": 1,
                "text": "combined queue check",
                "status": if outcome == "passed" { "met" } else { "unmet" },
                "findings": if outcome == "passed" { json!([]) } else { json!([summary]) }
            }],
            "degraded": json!([]),
        });
        state
            .with_store(|store| {
                store
                    .evidence_append(
                        EVIDENCE_TYPE,
                        Some(item_id),
                        None,
                        None,
                        &record.to_string(),
                    )
                    .map_err(|e| WorkbenchError::new("store_error", e.to_string()))?;
                Ok(())
            })
            .unwrap();
    }

    #[test]
    fn enqueue_host_resolves_checkouts_leases_canonically_and_rejects_mismatch() {
        let dir = tempfile::tempdir().unwrap();
        let repo_a = dir.path().join("repo-a");
        let repo_b = dir.path().join("repo-b");
        git_init(&repo_a);
        git_init(&repo_b);
        let head_a = git_commit(&repo_a, "a.txt");
        let head_b = git_commit(&repo_b, "b.txt");

        let state = host_state(&dir.path().join("profile.sqlite"));
        register_repo(&state, "repo-alpha", &repo_a);
        register_repo(&state, "repo-beta", &repo_b);

        // Mismatched checkout for repository_id must fail closed before writing.
        let mismatch = state
            .request(
                "queue.enqueue_host",
                &json!({
                    "id": "q-bad",
                    "request_id": "req-bad",
                    "expected_revision": 0,
                    "title": "Wrong checkout",
                    "members": [{
                        "member_id": "m-b",
                        "repository_id": "repo-beta",
                        "repo_path": repo_a,
                    }]
                })
                .to_string(),
            )
            .unwrap_err();
        assert_eq!(mismatch.code, "repository_mismatch");

        // Enqueue out of alphabetical repository_id order; store leases canonically
        // and snapshots each repository's real git_dir + base_oid.
        let queued = state
            .request(
                "queue.enqueue_host",
                &json!({
                    "id": "q-1",
                    "request_id": "req-q1",
                    "expected_revision": 0,
                    "title": "Cross-repo ship",
                    "concurrency_limit": 2,
                    "active_timeout_seconds": 2700,
                    "elapsed_timeout_seconds": 7200,
                    "members": [
                        {
                            "member_id": "m-beta",
                            "repository_id": "repo-beta",
                            "repo_path": repo_b,
                        },
                        {
                            "member_id": "m-alpha",
                            "repository_id": "repo-alpha",
                            "repo_path": repo_a,
                        }
                    ]
                })
                .to_string(),
            )
            .unwrap();
        assert_eq!(queued["item"]["state"], "queued");
        assert_eq!(
            queued["item"]["lease_order"],
            json!(["repo-alpha", "repo-beta"])
        );
        let acquired = state
            .request(
                "queue.acquire",
                &json!({
                    "id": "q-1",
                    "request_id": "req-acq-q1",
                    "expected_revision": queued["item"]["revision"],
                })
                .to_string(),
            )
            .unwrap();
        assert_eq!(acquired["item"]["state"], "leased");
        let members = acquired["item"]["members"].as_array().unwrap();
        let alpha = members
            .iter()
            .find(|m| m["member_id"] == "m-alpha")
            .unwrap();
        let beta = members.iter().find(|m| m["member_id"] == "m-beta").unwrap();
        assert_eq!(alpha["state"], "leased");
        assert_eq!(alpha["lease_held"], true);
        assert_eq!(alpha["base_oid"], head_a);
        assert_eq!(beta["state"], "leased");
        assert_eq!(beta["lease_held"], true);
        assert_eq!(beta["base_oid"], head_b);
    }

    #[test]
    fn verify_combined_uses_verification_gate_and_enforces_manual_per_repository() {
        let dir = tempfile::tempdir().unwrap();
        let repo_a = dir.path().join("repo-a");
        let repo_b = dir.path().join("repo-b");
        git_init(&repo_a);
        git_init(&repo_b);
        let head_a = git_commit(&repo_a, "a.txt");
        let head_b = git_commit(&repo_b, "b.txt");

        let state = host_state(&dir.path().join("profile.sqlite"));
        register_repo(&state, "repo-a", &repo_a);
        register_repo(&state, "repo-b", &repo_b);
        create_task(&state, "task-a", "repo-a");
        create_task(&state, "task-b", "repo-b");

        let queued = state
            .request(
                "queue.enqueue_host",
                &json!({
                    "id": "q-verify",
                    "request_id": "req-qv",
                    "expected_revision": 0,
                    "title": "Verify two repos",
                    "concurrency_limit": 2,
                    "members": [
                        {
                            "member_id": "m-a",
                            "repository_id": "repo-a",
                            "repo_path": repo_a,
                            "task_id": "task-a",
                        },
                        {
                            "member_id": "m-b",
                            "repository_id": "repo-b",
                            "repo_path": repo_b,
                            "task_id": "task-b",
                        }
                    ]
                })
                .to_string(),
            )
            .unwrap();
        let acquired = state
            .request(
                "queue.acquire",
                &json!({
                    "id": "q-verify",
                    "request_id": "req-acq-qv",
                    "expected_revision": queued["item"]["revision"],
                })
                .to_string(),
            )
            .unwrap();
        let mut rev = acquired["item"]["revision"].as_i64().unwrap();
        for (mid, req) in [("m-a", "t1"), ("m-b", "t2")] {
            let next = state
                .request(
                    "queue.member.update",
                    &json!({
                        "id": "q-verify",
                        "request_id": req,
                        "expected_revision": rev,
                        "member_id": mid,
                        "state": "passed",
                    })
                    .to_string(),
                )
                .unwrap();
            rev = next["item"]["revision"].as_i64().unwrap();
        }

        // First fail: task-b failed verification gate.
        record_verification_evidence(&state, "task-a", &repo_a, &head_a, "passed", "A ok");
        record_verification_evidence(&state, "task-b", &repo_b, &head_b, "failed", "B failed");
        let held = state
            .request(
                "queue.verify_combined",
                &json!({
                    "id": "q-verify",
                    "request_id": "verify-1",
                    "expected_revision": rev,
                    "checkouts": {
                        "m-a": repo_a,
                        "m-b": repo_b,
                    }
                })
                .to_string(),
            )
            .unwrap();
        assert_eq!(held["item"]["state"], "failed");
        assert_eq!(
            held["item"]["verification"]["publication"],
            "manual_per_repository"
        );
        rev = held["item"]["revision"].as_i64().unwrap();

        // Now record passing verification for task-b and re-verify.
        record_verification_evidence(&state, "task-b", &repo_b, &head_b, "passed", "B ok");
        let passed = state
            .request(
                "queue.verify_combined",
                &json!({
                    "id": "q-verify",
                    "request_id": "verify-2",
                    "expected_revision": rev,
                    "checkouts": {
                        "m-a": repo_a,
                        "m-b": repo_b,
                    }
                })
                .to_string(),
            )
            .unwrap();
        assert_eq!(passed["item"]["state"], "passed");
        assert_eq!(passed["item"]["verification"]["ok"], true);
        assert_eq!(
            passed["item"]["verification"]["publication"],
            "manual_per_repository"
        );
    }

    #[test]
    fn verify_combined_never_passes_a_member_whose_checkout_head_cannot_be_read() {
        let dir = tempfile::tempdir().unwrap();
        let repo_a = dir.path().join("repo-a");
        git_init(&repo_a);
        let head_a = git_commit(&repo_a, "a.txt");
        let state = host_state(&dir.path().join("profile.sqlite"));
        register_repo(&state, "repo-a", &repo_a);
        create_task(&state, "task-a", "repo-a");
        let queued = state
            .request(
                "queue.enqueue_host",
                &json!({
                    "id": "q-nohead", "request_id": "req-nh", "expected_revision": 0,
                    "title": "No checkout", "concurrency_limit": 2,
                    "members": [{
                        "member_id": "m-a", "repository_id": "repo-a",
                        "repo_path": repo_a, "task_id": "task-a",
                    }]
                })
                .to_string(),
            )
            .unwrap();
        let acquired = state
            .request(
                "queue.acquire",
                &json!({
                    "id": "q-nohead", "request_id": "req-acq-nh",
                    "expected_revision": queued["item"]["revision"],
                })
                .to_string(),
            )
            .unwrap();
        let updated = state
            .request(
                "queue.member.update",
                &json!({
                    "id": "q-nohead", "request_id": "t1",
                    "expected_revision": acquired["item"]["revision"],
                    "member_id": "m-a", "state": "passed",
                })
                .to_string(),
            )
            .unwrap();
        record_verification_evidence(&state, "task-a", &repo_a, &head_a, "passed", "A ok");
        // No checkouts and no run: the member's checkout head is unknown, so a
        // passed gate cannot be tied to the work.
        let verdict = state
            .request(
                "queue.verify_combined",
                &json!({
                    "id": "q-nohead", "request_id": "verify-nh",
                    "expected_revision": updated["item"]["revision"],
                })
                .to_string(),
            )
            .unwrap();
        assert_eq!(verdict["item"]["state"], "failed", "{verdict}");
        assert_eq!(verdict["item"]["verification"]["ok"], false, "{verdict}");
    }

    #[test]
    fn sweep_expired_queues_times_out_exceeded_leases() {
        let dir = tempfile::tempdir().unwrap();
        let repo_a = dir.path().join("repo-a");
        git_init(&repo_a);
        git_commit(&repo_a, "a.txt");

        let state = host_state(&dir.path().join("profile.sqlite"));
        register_repo(&state, "repo-a", &repo_a);
        let queued = state
            .request(
                "queue.enqueue_host",
                &json!({
                    "id": "q-exp",
                    "request_id": "req-exp",
                    "expected_revision": 0,
                    "title": "Expires soon",
                    "elapsed_timeout_seconds": 60,
                    "members": [{
                        "member_id": "m-a",
                        "repository_id": "repo-a",
                        "repo_path": repo_a,
                    }]
                })
                .to_string(),
            )
            .unwrap();
        state
            .request(
                "queue.acquire",
                &json!({
                    "id": "q-exp",
                    "request_id": "req-acq-exp",
                    "expected_revision": queued["item"]["revision"],
                })
                .to_string(),
            )
            .unwrap();

        // Not expired yet: sweep leaves it leased.
        assert_eq!(sweep_expired_queues(&state), 0);

        // Move leased_at into the past beyond elapsed_timeout_seconds.
        state
            .with_store(|store| {
                store
                    .connection()
                    .execute(
                        "UPDATE work_queue SET body=json_set(body,'$.leased_at',1) WHERE id='q-exp'",
                        [],
                    )
                    .map_err(|e| WorkbenchError::new("store_error", e.to_string()))?;
                Ok(())
            })
            .unwrap();

        assert_eq!(sweep_expired_queues(&state), 1);
        let after = state
            .request("queue.get", &json!({ "id": "q-exp" }).to_string())
            .unwrap();
        assert_eq!(after["item"]["state"], "timed_out");
        assert_eq!(after["item"]["members"][0]["lease_held"], false);
    }
}
