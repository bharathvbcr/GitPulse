//! Host coordinator for two-way GitHub issue synchronization and the durable outbox.
//!
//! Inbound changes (`state`, `title`, `body`, `labels`) are polled from GitHub
//! and written to `work_issue_sync` as accept/dismiss proposals via
//! `github_sync.observe`, honoring task `locked_fields`.
//!
//! Outbound writes execute only from opt-in, explicitly confirmed `work_outbox`
//! entries after three-way preflight conflict detection. Startup sweeps any
//! interrupted `delivering` entries into `outcome_unknown`, and delivery
//! reconciles remote marker / remote state before ever creating or updating an
//! issue on GitHub.

use super::{is_record_id, query, WorkbenchError, WorkbenchState};
use crate::github::{
    discover_github_remote, execute_outbox_create_issue, execute_outbox_update_issue,
    find_issue_by_marker, strip_outbox_marker, view_issue, GhMutationOutcome, GitHubRepoRef,
    IssueContent, IssueDetail,
};
use serde_json::{json, Map, Value};
use std::collections::HashSet;

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

fn sanitize_req_part(part: &str) -> String {
    part.chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '-' || c == '_' {
                c
            } else {
                '-'
            }
        })
        .collect()
}

fn scoped_request_id(base: &str, revision: i64, step: &str) -> String {
    let clean_step = sanitize_req_part(step);
    let suffix = format!("-r{revision}-{clean_step}");
    let max_base = 128usize.saturating_sub(suffix.len());
    let mut prefix: String = sanitize_req_part(base).chars().take(max_base).collect();
    if prefix.is_empty() {
        prefix.push_str("host");
    }
    format!("{prefix}{suffix}")
}

/// Abstracts GitHub CLI issue transport so host state-machine logic can be
/// exercised deterministically in unit tests without spawning `gh`.
pub(super) trait IssueTransport {
    fn resolve_remote(
        &self,
        repo_path: &str,
        expected_slug: &str,
    ) -> Result<GitHubRepoRef, WorkbenchError>;

    fn view_issue(
        &self,
        repo_path: &str,
        remote: &GitHubRepoRef,
        number: u64,
    ) -> Result<IssueDetail, WorkbenchError>;

    fn find_issue_by_marker(
        &self,
        repo_path: &str,
        remote: &GitHubRepoRef,
        idempotency_key: &str,
    ) -> Result<Option<IssueDetail>, WorkbenchError>;

    fn create_issue(
        &self,
        repo_path: &str,
        remote: &GitHubRepoRef,
        content: IssueContent<'_>,
        idempotency_key: &str,
    ) -> GhMutationOutcome;

    fn update_issue(
        &self,
        repo_path: &str,
        remote: &GitHubRepoRef,
        number: u64,
        fields: &[String],
        content: IssueContent<'_>,
        current_remote: Option<&IssueDetail>,
    ) -> GhMutationOutcome;
}

struct GhCliTransport;

impl IssueTransport for GhCliTransport {
    fn resolve_remote(
        &self,
        repo_path: &str,
        expected_slug: &str,
    ) -> Result<GitHubRepoRef, WorkbenchError> {
        let remote = discover_github_remote(repo_path)
            .map_err(|e| WorkbenchError::new("github_unavailable", e.into_message()))?
            .ok_or_else(|| {
                WorkbenchError::new(
                    "not_github_remote",
                    format!("No GitHub remote configured in {repo_path}"),
                )
            })?;
        if !remote.slug().eq_ignore_ascii_case(expected_slug) {
            return Err(WorkbenchError::new(
                "repository_mismatch",
                format!(
                    "Checkout remote {} does not match linked GitHub repository {}",
                    remote.slug(),
                    expected_slug
                ),
            ));
        }
        Ok(remote)
    }

    fn view_issue(
        &self,
        repo_path: &str,
        remote: &GitHubRepoRef,
        number: u64,
    ) -> Result<IssueDetail, WorkbenchError> {
        view_issue(repo_path, remote, number)
            .map_err(|e| WorkbenchError::new("github_unavailable", e.into_message()))
    }

    fn find_issue_by_marker(
        &self,
        repo_path: &str,
        remote: &GitHubRepoRef,
        idempotency_key: &str,
    ) -> Result<Option<IssueDetail>, WorkbenchError> {
        find_issue_by_marker(repo_path, remote, idempotency_key)
            .map_err(|e| WorkbenchError::new("github_unavailable", e.into_message()))
    }

    fn create_issue(
        &self,
        repo_path: &str,
        remote: &GitHubRepoRef,
        content: IssueContent<'_>,
        idempotency_key: &str,
    ) -> GhMutationOutcome {
        execute_outbox_create_issue(repo_path, remote, content, idempotency_key)
    }

    fn update_issue(
        &self,
        repo_path: &str,
        remote: &GitHubRepoRef,
        number: u64,
        fields: &[String],
        content: IssueContent<'_>,
        current_remote: Option<&IssueDetail>,
    ) -> GhMutationOutcome {
        execute_outbox_update_issue(repo_path, remote, number, fields, content, current_remote)
    }
}

/// Converts a fetched [`IssueDetail`] into the exact 5-key `remote` snapshot
/// object accepted by `dc-store`'s `IssueSnapshot::from_json`.
fn remote_snapshot_json(detail: &IssueDetail) -> Value {
    let updated_at = if detail.updated_at.trim().is_empty() {
        Value::Null
    } else {
        json!(detail.updated_at)
    };
    json!({
        "title": detail.title,
        "body": strip_outbox_marker(&detail.body),
        "state": detail.state,
        "labels": detail.labels,
        "updated_at": updated_at,
    })
}

/// Host coordinator for `github_sync.poll`.
///
/// Fetches the linked GitHub issue via `gh` and records any inbound delta as an
/// accept/dismiss proposal on `work_issue_sync`.
pub(super) fn poll(state: &WorkbenchState, raw_input: &str) -> Result<Value, WorkbenchError> {
    poll_with(state, raw_input, &GhCliTransport)
}

pub(super) fn poll_with(
    state: &WorkbenchState,
    raw_input: &str,
    transport: &dyn IssueTransport,
) -> Result<Value, WorkbenchError> {
    let top = parse_object(raw_input, "github_sync.poll")?;
    check_allowed_keys(
        &top,
        &[
            "id",
            "task_id",
            "repo_slug",
            "issue_number",
            "repo_path",
            "request_id",
        ],
        "github_sync.poll",
    )?;
    let repo_path = required_string(&top, "repo_path")?;
    let request_id = required_string(&top, "request_id")?;

    let mut get_req = Map::new();
    if let Some(id) = top
        .get("id")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|s| !s.is_empty())
    {
        if !is_record_id(id) {
            return Err(invalid("invalid sync link id"));
        }
        get_req.insert("id".into(), json!(id));
    } else if let Some(tid) = top
        .get("task_id")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|s| !s.is_empty())
    {
        if !is_record_id(tid) {
            return Err(invalid("invalid task_id"));
        }
        get_req.insert("task_id".into(), json!(tid));
    } else if let (Some(slug), Some(num)) = (
        top.get("repo_slug")
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|s| !s.is_empty()),
        top.get("issue_number")
            .and_then(Value::as_u64)
            .filter(|n| *n >= 1),
    ) {
        get_req.insert("repo_slug".into(), json!(slug));
        get_req.insert("issue_number".into(), json!(num));
    } else {
        return Err(invalid(
            "github_sync.poll requires id, task_id, or (repo_slug, issue_number)",
        ));
    }

    let sync_doc = state.with_store(|store| {
        query(
            store,
            "github_sync.get",
            &Value::Object(get_req).to_string(),
        )
    })?;
    let item = &sync_doc["item"];
    let sync_id = item["id"]
        .as_str()
        .ok_or_else(|| WorkbenchError::new("protocol_error", "github_sync.get omitted id"))?;
    let revision = item["revision"]
        .as_i64()
        .ok_or_else(|| WorkbenchError::new("protocol_error", "github_sync.get omitted revision"))?;
    let repo_slug = item["repo_slug"].as_str().ok_or_else(|| {
        WorkbenchError::new("protocol_error", "github_sync.get omitted repo_slug")
    })?;
    let issue_number = item["issue_number"]
        .as_u64()
        .filter(|n| *n >= 1)
        .ok_or_else(|| {
            WorkbenchError::new(
                "protocol_error",
                "github_sync.get omitted valid issue_number",
            )
        })?;

    let remote = transport.resolve_remote(repo_path, repo_slug)?;
    let detail = transport.view_issue(repo_path, &remote, issue_number)?;

    let observe_req = json!({
        "id": sync_id,
        "request_id": request_id,
        "expected_revision": revision,
        "remote": remote_snapshot_json(&detail),
    });
    state.with_store(|store| query(store, "github_sync.observe", &observe_req.to_string()))
}

/// Startup reconciliation: moves any `delivering` outbox entries left open
/// across a process restart into `outcome_unknown` so delivery reconciles with
/// GitHub before retrying.
pub(super) fn sweep_delivering_on_startup(state: &WorkbenchState) -> usize {
    let mut candidates: Vec<(String, i64)> = Vec::new();
    let mut seen: HashSet<String> = HashSet::new();
    let mut cursor: Option<String> = None;

    for _ in 0..SWEEP_MAX_PAGES {
        let mut input = json!({ "state": "delivering", "limit": SWEEP_PAGE_SIZE });
        if let Some(ref next) = cursor {
            input["cursor"] = json!(next);
        }
        let page = match state.with_store(|store| query(store, "outbox.list", &input.to_string())) {
            Ok(page) => page,
            Err(error) => {
                log::warn!(
                    target: "workbench",
                    "outbox startup sweep could not list delivering rows: {}: {}",
                    error.code,
                    error.message
                );
                break;
            }
        };
        for item in page["items"].as_array().into_iter().flatten() {
            if let (Some(id), Some(revision)) = (item["id"].as_str(), item["revision"].as_i64()) {
                if seen.insert(id.to_owned()) {
                    candidates.push((id.to_owned(), revision));
                }
            }
        }
        match page["next_cursor"].as_str() {
            Some(next) if page["has_more"] == true => cursor = Some(next.to_owned()),
            _ => break,
        }
    }

    let mut transitioned = 0;
    for (id, revision) in candidates {
        let req = json!({
            "id": id,
            "request_id": scoped_request_id("host-outbox-restart", revision, &id),
            "expected_revision": revision,
        });
        match state.with_store(|store| query(store, "outbox.sweep_restart", &req.to_string())) {
            Ok(resp) if resp["item"]["state"] == "outcome_unknown" => transitioned += 1,
            Ok(_) => {}
            Err(error) if matches!(error.code.as_str(), "invalid_state" | "revision_conflict") => {}
            Err(error) => {
                log::warn!(
                    target: "workbench",
                    "outbox startup sweep skipped {id}: {}: {}",
                    error.code,
                    error.message
                );
            }
        }
    }
    transitioned
}

fn string_vec(val: &Value) -> Vec<String> {
    val.as_array()
        .into_iter()
        .flatten()
        .filter_map(Value::as_str)
        .map(ToOwned::to_owned)
        .collect()
}

/// Host coordinator for `outbox.deliver`.
///
/// Enforces opt-in, explicit confirmation, `outcome_unknown` preflight
/// reconciliation (marker search / remote read), 3-way conflict refusal before
/// writing to GitHub, durable `claim -> gh -> complete/mark_unknown/fail`
/// transitions, and restart idempotency.
pub(super) fn deliver(state: &WorkbenchState, raw_input: &str) -> Result<Value, WorkbenchError> {
    deliver_with(state, raw_input, &GhCliTransport)
}

pub(super) fn deliver_with(
    state: &WorkbenchState,
    raw_input: &str,
    transport: &dyn IssueTransport,
) -> Result<Value, WorkbenchError> {
    let top = parse_object(raw_input, "outbox.deliver")?;
    check_allowed_keys(&top, &["id", "repo_path", "request_id"], "outbox.deliver")?;
    let id = required_string(&top, "id")?;
    if !is_record_id(id) {
        return Err(invalid("invalid outbox id"));
    }
    let repo_path = required_string(&top, "repo_path")?;
    let request_id = required_string(&top, "request_id")?;

    let initial_doc =
        state.with_store(|store| query(store, "outbox.get", &json!({ "id": id }).to_string()))?;
    let entry = &initial_doc["item"];

    if entry["outbound_opt_in"].as_bool() != Some(true) {
        return Err(WorkbenchError::new(
            "outbound_disabled",
            "outbound GitHub writes require repository/link outbound opt-in",
        ));
    }
    if entry["confirmed"].as_bool() != Some(true) {
        return Err(WorkbenchError::new(
            "confirmation_required",
            "outbound GitHub write requires explicit confirmation",
        ));
    }
    let has_digest = entry["payload_digest"]
        .as_str()
        .map(str::trim)
        .is_some_and(|d| !d.is_empty());
    if !has_digest {
        return Err(WorkbenchError::new(
            "invalid_state",
            "outbox entry is missing payload_digest",
        ));
    }

    let current_state = entry["state"]
        .as_str()
        .ok_or_else(|| WorkbenchError::new("protocol_error", "outbox.get omitted state"))?;
    match current_state {
        "delivered" => return Ok(initial_doc),
        "conflict" => {
            return Err(WorkbenchError::new(
                "conflict",
                "outbox entry has an unresolved 3-way conflict",
            ))
        }
        "delivering" => {
            return Err(WorkbenchError::new(
                "in_flight",
                "outbox entry is already delivering; restart recovery must run first if a previous process died",
            ))
        }
        "pending" | "outcome_unknown" => {}
        other => {
            return Err(WorkbenchError::new(
                "invalid_state",
                format!("cannot deliver outbox entry in state {other}"),
            ))
        }
    }

    let mut revision = entry["revision"]
        .as_i64()
        .ok_or_else(|| WorkbenchError::new("protocol_error", "outbox.get omitted revision"))?;
    let attempts = entry["attempts"].as_i64().unwrap_or(0);
    let op = entry["op"]
        .as_str()
        .ok_or_else(|| WorkbenchError::new("protocol_error", "outbox.get omitted op"))?;
    let repo_slug = entry["repo_slug"]
        .as_str()
        .ok_or_else(|| WorkbenchError::new("protocol_error", "outbox.get omitted repo_slug"))?;
    let idempotency_key = entry["idempotency_key"].as_str().ok_or_else(|| {
        WorkbenchError::new("protocol_error", "outbox.get omitted idempotency_key")
    })?;
    let issue_number = entry["issue_number"]
        .as_u64()
        .or_else(|| entry["payload"]["issue_number"].as_u64());

    let remote = transport.resolve_remote(repo_path, repo_slug)?;
    let mut current_remote: Option<IssueDetail> = None;

    if current_state == "outcome_unknown" {
        if op == "create_issue" {
            let found = transport.find_issue_by_marker(repo_path, &remote, idempotency_key)?;
            let preflight_req = match found {
                Some(ref detail) => json!({
                    "id": id,
                    "request_id": scoped_request_id(request_id, revision, "preflight"),
                    "expected_revision": revision,
                    "matched_remote": true,
                    "issue_number": detail.number,
                    "issue_url": detail.url,
                    "remote": remote_snapshot_json(detail),
                }),
                None => json!({
                    "id": id,
                    "request_id": scoped_request_id(request_id, revision, "preflight"),
                    "expected_revision": revision,
                    "matched_remote": false,
                }),
            };
            let preflight_resp = state
                .with_store(|store| query(store, "outbox.preflight", &preflight_req.to_string()))?;
            if preflight_resp["item"]["state"] == "delivered" {
                return Ok(preflight_resp);
            }
            revision = preflight_resp["item"]["revision"].as_i64().ok_or_else(|| {
                WorkbenchError::new("protocol_error", "outbox.preflight omitted revision")
            })?;
        } else {
            let num = issue_number.filter(|n| *n >= 1).ok_or_else(|| {
                WorkbenchError::new(
                    "protocol_error",
                    "update_issue outbox entry omitted issue_number",
                )
            })?;
            let detail = transport.view_issue(repo_path, &remote, num)?;
            let preflight_req = json!({
                "id": id,
                "request_id": scoped_request_id(request_id, revision, "preflight"),
                "expected_revision": revision,
                "issue_number": detail.number,
                "issue_url": detail.url,
                "remote": remote_snapshot_json(&detail),
            });
            let preflight_resp = state
                .with_store(|store| query(store, "outbox.preflight", &preflight_req.to_string()))?;
            let next_state = preflight_resp["item"]["state"].as_str().unwrap_or_default();
            if matches!(next_state, "delivered" | "conflict") {
                return Ok(preflight_resp);
            }
            revision = preflight_resp["item"]["revision"].as_i64().ok_or_else(|| {
                WorkbenchError::new("protocol_error", "outbox.preflight omitted revision")
            })?;
            current_remote = Some(detail);
        }
    } else if op == "update_issue" {
        let num = issue_number.filter(|n| *n >= 1).ok_or_else(|| {
            WorkbenchError::new(
                "protocol_error",
                "update_issue outbox entry omitted issue_number",
            )
        })?;
        let detail = transport.view_issue(repo_path, &remote, num)?;
        let preflight_req = json!({
            "id": id,
            "request_id": scoped_request_id(request_id, revision, "preflight"),
            "expected_revision": revision,
            "issue_number": detail.number,
            "issue_url": detail.url,
            "remote": remote_snapshot_json(&detail),
        });
        let preflight_resp = state
            .with_store(|store| query(store, "outbox.preflight", &preflight_req.to_string()))?;
        if preflight_resp["item"]["state"] == "conflict" {
            return Ok(preflight_resp);
        }
        revision = preflight_resp["item"]["revision"].as_i64().ok_or_else(|| {
            WorkbenchError::new("protocol_error", "outbox.preflight omitted revision")
        })?;
        current_remote = Some(detail);
    } else if op == "create_issue" && attempts > 0 {
        // If a previous attempt failed without leaving outcome_unknown, double-check
        // whether our idempotency marker already landed before creating again.
        if let Some(found) = transport.find_issue_by_marker(repo_path, &remote, idempotency_key)? {
            let claim_resp = state.with_store(|store| {
                query(
                    store,
                    "outbox.claim",
                    &json!({
                        "id": id,
                        "request_id": scoped_request_id(request_id, revision, "claim"),
                        "expected_revision": revision,
                    })
                    .to_string(),
                )
            })?;
            let claim_rev = claim_resp["item"]["revision"].as_i64().ok_or_else(|| {
                WorkbenchError::new("protocol_error", "outbox.claim omitted revision")
            })?;
            return state.with_store(|store| {
                query(
                    store,
                    "outbox.complete",
                    &json!({
                        "id": id,
                        "request_id": scoped_request_id(request_id, claim_rev, "complete"),
                        "expected_revision": claim_rev,
                        "issue_number": found.number,
                        "issue_url": found.url,
                        "remote": remote_snapshot_json(&found),
                    })
                    .to_string(),
                )
            });
        }
    }

    let claim_resp = state.with_store(|store| {
        query(
            store,
            "outbox.claim",
            &json!({
                "id": id,
                "request_id": scoped_request_id(request_id, revision, "claim"),
                "expected_revision": revision,
            })
            .to_string(),
        )
    })?;
    let claim_rev = claim_resp["item"]["revision"]
        .as_i64()
        .ok_or_else(|| WorkbenchError::new("protocol_error", "outbox.claim omitted revision"))?;

    let payload = &entry["payload"];
    let title = payload["title"].as_str().unwrap_or_default();
    let body = payload["body"].as_str().unwrap_or_default();
    let state_str = payload["state"].as_str().unwrap_or("open");
    let labels = string_vec(&payload["labels"]);
    let fields = string_vec(&payload["fields"]);
    let content = IssueContent {
        title,
        body,
        state: state_str,
        labels: &labels,
    };

    let outcome = match op {
        "create_issue" => transport.create_issue(repo_path, &remote, content, idempotency_key),
        "update_issue" => {
            let num = issue_number.filter(|n| *n >= 1).ok_or_else(|| {
                WorkbenchError::new(
                    "protocol_error",
                    "update_issue outbox entry omitted issue_number",
                )
            })?;
            transport.update_issue(
                repo_path,
                &remote,
                num,
                &fields,
                content,
                current_remote.as_ref(),
            )
        }
        other => {
            return Err(WorkbenchError::new(
                "protocol_error",
                format!("unsupported outbox operation {other}"),
            ))
        }
    };

    match outcome {
        GhMutationOutcome::Applied(detail) => state.with_store(|store| {
            query(
                store,
                "outbox.complete",
                &json!({
                    "id": id,
                    "request_id": scoped_request_id(request_id, claim_rev, "complete"),
                    "expected_revision": claim_rev,
                    "issue_number": detail.number,
                    "issue_url": detail.url,
                    "remote": remote_snapshot_json(&detail),
                })
                .to_string(),
            )
        }),
        GhMutationOutcome::Uncertain { message } => state.with_store(|store| {
            query(
                store,
                "outbox.mark_unknown",
                &json!({
                    "id": id,
                    "request_id": scoped_request_id(request_id, claim_rev, "unknown"),
                    "expected_revision": claim_rev,
                    "reason": message,
                })
                .to_string(),
            )
        }),
        GhMutationOutcome::DefinitiveFailure { message, retryable } => state.with_store(|store| {
            query(
                store,
                "outbox.fail",
                &json!({
                    "id": id,
                    "request_id": scoped_request_id(request_id, claim_rev, "fail"),
                    "expected_revision": claim_rev,
                    "error": message,
                    "retryable": retryable,
                })
                .to_string(),
            )
        }),
    }
}

#[cfg(test)]
mod tests {
    use super::super::Inner;
    use super::*;
    use crate::github::outbox_marker;
    use std::cell::RefCell;
    use std::collections::HashMap;
    use std::path::Path;
    use std::sync::Arc;

    struct MockTransport {
        slug: String,
        viewed: RefCell<HashMap<u64, IssueDetail>>,
        marker_hits: RefCell<HashMap<String, IssueDetail>>,
        create_outcomes: RefCell<Vec<GhMutationOutcome>>,
        update_outcomes: RefCell<Vec<GhMutationOutcome>>,
        create_calls: RefCell<usize>,
        update_calls: RefCell<usize>,
    }

    impl MockTransport {
        fn new(slug: &str) -> Self {
            Self {
                slug: slug.to_owned(),
                viewed: RefCell::new(HashMap::new()),
                marker_hits: RefCell::new(HashMap::new()),
                create_outcomes: RefCell::new(Vec::new()),
                update_outcomes: RefCell::new(Vec::new()),
                create_calls: RefCell::new(0),
                update_calls: RefCell::new(0),
            }
        }

        fn git_ref(&self) -> GitHubRepoRef {
            let (owner, name) = self.slug.split_once('/').unwrap();
            GitHubRepoRef {
                host: "github.com".into(),
                owner: owner.into(),
                name: name.into(),
            }
        }
    }

    impl IssueTransport for MockTransport {
        fn resolve_remote(
            &self,
            _repo_path: &str,
            expected_slug: &str,
        ) -> Result<GitHubRepoRef, WorkbenchError> {
            assert_eq!(expected_slug, self.slug);
            Ok(self.git_ref())
        }

        fn view_issue(
            &self,
            _repo_path: &str,
            _remote: &GitHubRepoRef,
            number: u64,
        ) -> Result<IssueDetail, WorkbenchError> {
            self.viewed
                .borrow()
                .get(&number)
                .cloned()
                .ok_or_else(|| WorkbenchError::new("not_found", format!("missing #{number}")))
        }

        fn find_issue_by_marker(
            &self,
            _repo_path: &str,
            _remote: &GitHubRepoRef,
            idempotency_key: &str,
        ) -> Result<Option<IssueDetail>, WorkbenchError> {
            Ok(self.marker_hits.borrow().get(idempotency_key).cloned())
        }

        fn create_issue(
            &self,
            _repo_path: &str,
            _remote: &GitHubRepoRef,
            _content: IssueContent<'_>,
            _idempotency_key: &str,
        ) -> GhMutationOutcome {
            *self.create_calls.borrow_mut() += 1;
            self.create_outcomes.borrow_mut().remove(0)
        }

        fn update_issue(
            &self,
            _repo_path: &str,
            _remote: &GitHubRepoRef,
            _number: u64,
            _fields: &[String],
            _content: IssueContent<'_>,
            _current_remote: Option<&IssueDetail>,
        ) -> GhMutationOutcome {
            *self.update_calls.borrow_mut() += 1;
            self.update_outcomes.borrow_mut().remove(0)
        }
    }

    fn host_state(profile_path: &Path) -> WorkbenchState {
        WorkbenchState(Arc::new(Inner {
            path: Some(profile_path.to_path_buf()),
            ..Inner::default()
        }))
    }

    fn seed_task(state: &WorkbenchState, locked: &[&str]) {
        state
            .request(
                "repositories.put",
                &json!({
                    "id": "repo",
                    "request_id": "put-repo",
                    "expected_revision": 0,
                    "name": "repo",
                    "identity_key": "local:/tmp/repo",
                })
                .to_string(),
            )
            .unwrap();
        state
            .request(
                "items.put",
                &json!({
                    "id": "task-1",
                    "request_id": "put-task-1",
                    "expected_revision": 0,
                    "title": "Local title",
                    "description": "Local body",
                    "status": "ready",
                    "labels": ["sync"],
                    "locked_fields": locked,
                    "repository_ids": ["repo"],
                    "primary_repository_id": "repo",
                })
                .to_string(),
            )
            .unwrap();
    }

    #[test]
    fn inbound_poll_surfaces_proposals_honoring_locked_fields_and_dismissal() {
        let dir = tempfile::tempdir().unwrap();
        let state = host_state(&dir.path().join("profile.sqlite"));
        seed_task(&state, &["title"]);

        state
            .request(
                "github_sync.link",
                &json!({
                    "id": "sync-1",
                    "request_id": "link-1",
                    "expected_revision": 0,
                    "task_id": "task-1",
                    "repo_slug": "acme/widgets",
                    "issue_number": 42,
                    "issue_url": "https://github.com/acme/widgets/issues/42",
                    "outbound_opt_in": false,
                    "last_seen": {
                        "title": "Local title",
                        "body": "Local body",
                        "state": "open",
                        "labels": ["sync"],
                    }
                })
                .to_string(),
            )
            .unwrap();

        let transport = MockTransport::new("acme/widgets");
        transport.viewed.borrow_mut().insert(
            42,
            IssueDetail {
                number: 42,
                title: "Remote renamed title".into(),
                body: format!("Remote updated body\n\n{}", outbox_marker("prev-key")),
                state: "open".into(),
                url: "https://github.com/acme/widgets/issues/42".into(),
                labels: vec!["sync".into(), "triage".into()],
                updated_at: "2026-11-14T09:00:00Z".into(),
            },
        );

        let polled = poll_with(
            &state,
            &json!({
                "id": "sync-1",
                "repo_path": dir.path(),
                "request_id": "poll-1",
            })
            .to_string(),
            &transport,
        )
        .unwrap();
        let proposal = &polled["item"]["pending_proposal"];
        assert_eq!(proposal["locked_skipped"], json!(["title"]));
        assert_eq!(proposal["remote"]["body"], "Remote updated body");

        let rev = polled["item"]["revision"].as_i64().unwrap();
        state
            .request(
                "github_sync.accept",
                &json!({
                    "id": "sync-1",
                    "request_id": "accept-1",
                    "expected_revision": rev,
                })
                .to_string(),
            )
            .unwrap();
        let updated_task = state
            .request("items.get", &json!({ "id": "task-1" }).to_string())
            .unwrap();
        // Locked `title` kept local value; unlocked `body` and `labels` accepted remote.
        assert_eq!(updated_task["item"]["title"], "Local title");
        assert_eq!(updated_task["item"]["description"], "Remote updated body");
        assert_eq!(updated_task["item"]["labels"], json!(["sync", "triage"]));

        // Next remote edit -> dismiss ignores identical remote digest until changed again.
        transport.viewed.borrow_mut().insert(
            42,
            IssueDetail {
                number: 42,
                title: "Remote renamed title".into(),
                body: "Second remote body".into(),
                state: "closed".into(),
                url: "https://github.com/acme/widgets/issues/42".into(),
                labels: vec!["triage".into()],
                updated_at: "2026-11-14T09:05:00Z".into(),
            },
        );
        let sync_rev = state
            .request("github_sync.get", &json!({ "id": "sync-1" }).to_string())
            .unwrap()["item"]["revision"]
            .as_i64()
            .unwrap();
        assert_eq!(sync_rev, 3);
        let second_poll = poll_with(
            &state,
            &json!({
                "task_id": "task-1",
                "repo_path": dir.path(),
                "request_id": "poll-2",
            })
            .to_string(),
            &transport,
        )
        .unwrap();
        assert!(second_poll["item"]["pending_proposal"].is_object());
        let dismissed = state
            .request(
                "github_sync.dismiss",
                &json!({
                    "id": "sync-1",
                    "request_id": "dismiss-1",
                    "expected_revision": second_poll["item"]["revision"],
                })
                .to_string(),
            )
            .unwrap();
        assert!(dismissed["item"]["pending_proposal"].is_null());
        let re_polled = poll_with(
            &state,
            &json!({
                "id": "sync-1",
                "repo_path": dir.path(),
                "request_id": "poll-3",
            })
            .to_string(),
            &transport,
        )
        .unwrap();
        assert!(
            re_polled["item"]["pending_proposal"].is_null(),
            "dismissed digest must suppress re-proposing unchanged remote state"
        );
    }

    #[test]
    fn outbound_create_recovers_after_restart_without_duplicating_issue() {
        let dir = tempfile::tempdir().unwrap();
        let state = host_state(&dir.path().join("profile.sqlite"));
        seed_task(&state, &[]);

        // Outbound enqueue refuses without explicit confirmation; delivery refuses
        // when `outbound_opt_in` is false.
        let unconfirmed = state
            .request(
                "outbox.enqueue",
                &json!({
                    "id": "ob-unconfirmed",
                    "request_id": "enq-unconfirmed",
                    "expected_revision": 0,
                    "task_id": "task-1",
                    "op": "create_issue",
                    "repo_slug": "acme/widgets",
                    "outbound_opt_in": true,
                    "confirmed": false,
                    "idempotency_key": "task1-create-unconfirmed",
                })
                .to_string(),
            )
            .unwrap_err();
        assert_eq!(unconfirmed.code, "confirmation_required");

        state
            .request(
                "outbox.enqueue",
                &json!({
                    "id": "ob-disabled",
                    "request_id": "enq-disabled",
                    "expected_revision": 0,
                    "task_id": "task-1",
                    "op": "create_issue",
                    "repo_slug": "acme/widgets",
                    "outbound_opt_in": false,
                    "confirmed": true,
                    "idempotency_key": "task1-create-disabled",
                })
                .to_string(),
            )
            .unwrap();
        let disabled_transport = MockTransport::new("acme/widgets");
        let refused = deliver_with(
            &state,
            &json!({
                "id": "ob-disabled",
                "repo_path": dir.path(),
                "request_id": "del-disabled",
            })
            .to_string(),
            &disabled_transport,
        )
        .unwrap_err();
        assert_eq!(refused.code, "outbound_disabled");

        let preview = state
            .request(
                "outbox.preview",
                &json!({
                    "task_id": "task-1",
                    "op": "create_issue",
                    "repo_slug": "acme/widgets",
                })
                .to_string(),
            )
            .unwrap();
        let digest = preview["preview"]["payload_digest"]
            .as_str()
            .unwrap()
            .to_owned();

        let enqueued = state
            .request(
                "outbox.enqueue",
                &json!({
                    "id": "ob-1",
                    "request_id": "enq-1",
                    "expected_revision": 0,
                    "task_id": "task-1",
                    "op": "create_issue",
                    "repo_slug": "acme/widgets",
                    "outbound_opt_in": true,
                    "confirmed": true,
                    "confirm_payload_digest": digest,
                    "idempotency_key": "task1-create-1",
                })
                .to_string(),
            )
            .unwrap();
        assert_eq!(enqueued["item"]["state"], "pending");

        // Simulate crash mid-flight: store claimed the row (`delivering`) and
        // GitHub actually created issue #77 containing our idempotency marker.
        state
            .with_store(|store| {
                query(
                    store,
                    "outbox.claim",
                    &json!({
                        "id": "ob-1",
                        "request_id": "crash-claim",
                        "expected_revision": 1,
                    })
                    .to_string(),
                )
            })
            .unwrap();
        assert_eq!(sweep_delivering_on_startup(&state), 1);

        let transport = MockTransport::new("acme/widgets");
        transport.marker_hits.borrow_mut().insert(
            "task1-create-1".into(),
            IssueDetail {
                number: 77,
                title: "Local title".into(),
                body: format!("Local body\n\n{}", outbox_marker("task1-create-1")),
                state: "open".into(),
                url: "https://github.com/acme/widgets/issues/77".into(),
                labels: vec!["sync".into()],
                updated_at: "2026-11-14T09:10:00Z".into(),
            },
        );

        let reconciled = deliver_with(
            &state,
            &json!({
                "id": "ob-1",
                "repo_path": dir.path(),
                "request_id": "deliver-recover",
            })
            .to_string(),
            &transport,
        )
        .unwrap();
        assert_eq!(reconciled["item"]["state"], "delivered");
        assert_eq!(reconciled["item"]["issue_number"], 77);
        assert_eq!(
            *transport.create_calls.borrow(),
            0,
            "marker recovery must link existing issue without creating a duplicate"
        );
    }

    #[test]
    fn outbound_update_halts_on_conflict_and_retries_transient_error() {
        let dir = tempfile::tempdir().unwrap();
        let state = host_state(&dir.path().join("profile.sqlite"));
        seed_task(&state, &[]);
        state
            .request(
                "github_sync.link",
                &json!({
                    "id": "sync-u",
                    "request_id": "link-u",
                    "expected_revision": 0,
                    "task_id": "task-1",
                    "repo_slug": "acme/widgets",
                    "issue_number": 91,
                    "outbound_opt_in": true,
                    "last_seen": {
                        "title": "Original title",
                        "body": "Local body",
                        "state": "open",
                        "labels": ["sync"],
                    }
                })
                .to_string(),
            )
            .unwrap();

        let preview = state
            .request(
                "outbox.preview",
                &json!({
                    "task_id": "task-1",
                    "op": "update_issue",
                    "fields": ["title"],
                })
                .to_string(),
            )
            .unwrap();
        state
            .request(
                "outbox.enqueue",
                &json!({
                    "id": "ob-conflict",
                    "request_id": "enq-conflict",
                    "expected_revision": 0,
                    "task_id": "task-1",
                    "op": "update_issue",
                    "fields": ["title"],
                    "outbound_opt_in": true,
                    "confirmed": true,
                    "confirm_payload_digest": preview["preview"]["payload_digest"],
                    "idempotency_key": "task1-update-conflict",
                })
                .to_string(),
            )
            .unwrap();

        let transport = MockTransport::new("acme/widgets");
        transport.viewed.borrow_mut().insert(
            91,
            IssueDetail {
                number: 91,
                title: "Edited remotely too".into(),
                body: "Local body".into(),
                state: "open".into(),
                url: "https://github.com/acme/widgets/issues/91".into(),
                labels: vec!["sync".into()],
                updated_at: "2026-11-14T09:12:00Z".into(),
            },
        );
        let halted = deliver_with(
            &state,
            &json!({
                "id": "ob-conflict",
                "repo_path": dir.path(),
                "request_id": "del-conflict",
            })
            .to_string(),
            &transport,
        )
        .unwrap();
        assert_eq!(halted["item"]["state"], "conflict");
        assert_eq!(halted["item"]["conflict"]["fields"], json!(["title"]));
        assert_eq!(*transport.update_calls.borrow(), 0);

        // Now enqueue a non-conflicting body update (remote matches last_seen body)
        // and verify retryable failure -> retry -> delivered.
        transport.viewed.borrow_mut().insert(
            91,
            IssueDetail {
                number: 91,
                title: "Original title".into(),
                body: "Local body".into(),
                state: "open".into(),
                url: "https://github.com/acme/widgets/issues/91".into(),
                labels: vec!["sync".into()],
                updated_at: "2026-11-14T09:13:00Z".into(),
            },
        );
        state
            .request(
                "items.put",
                &json!({
                    "id": "task-1",
                    "request_id": "edit-task-body",
                    "expected_revision": 1,
                    "title": "Original title",
                    "description": "Updated local body",
                    "status": "ready",
                    "labels": ["sync"],
                    "repository_ids": ["repo"],
                    "primary_repository_id": "repo",
                })
                .to_string(),
            )
            .unwrap();
        // Reset base snapshot to before the edit cleanly.
        state
            .with_store(|store| {
                store
                    .connection()
                    .execute(
                        "UPDATE work_issue_sync SET body=json_set(body,'$.last_seen.body','Local body') WHERE id='sync-u'",
                        [],
                    )
                    .map_err(|e| WorkbenchError::new("store_error", e.to_string()))?;
                Ok(())
            })
            .unwrap();

        let p2 = state
            .request(
                "outbox.preview",
                &json!({
                    "task_id": "task-1",
                    "op": "update_issue",
                    "fields": ["body"],
                })
                .to_string(),
            )
            .unwrap();
        state
            .request(
                "outbox.enqueue",
                &json!({
                    "id": "ob-retry",
                    "request_id": "enq-retry",
                    "expected_revision": 0,
                    "task_id": "task-1",
                    "op": "update_issue",
                    "fields": ["body"],
                    "outbound_opt_in": true,
                    "confirmed": true,
                    "confirm_payload_digest": p2["preview"]["payload_digest"],
                    "idempotency_key": "task1-update-retry",
                })
                .to_string(),
            )
            .unwrap();

        transport
            .update_outcomes
            .borrow_mut()
            .push(GhMutationOutcome::DefinitiveFailure {
                message: "rate limited".into(),
                retryable: true,
            });
        transport
            .update_outcomes
            .borrow_mut()
            .push(GhMutationOutcome::Applied(IssueDetail {
                number: 91,
                title: "Original title".into(),
                body: "Updated local body".into(),
                state: "open".into(),
                url: "https://github.com/acme/widgets/issues/91".into(),
                labels: vec!["sync".into()],
                updated_at: "2026-11-14T09:15:00Z".into(),
            }));

        let first_try = deliver_with(
            &state,
            &json!({
                "id": "ob-retry",
                "repo_path": dir.path(),
                "request_id": "del-retry-1",
            })
            .to_string(),
            &transport,
        )
        .unwrap();
        assert_eq!(first_try["item"]["state"], "pending");
        assert_eq!(first_try["item"]["attempts"], 1);
        assert_eq!(first_try["item"]["last_error"], "rate limited");

        // Advance past retry backoff to simulate elapsed delay cleanly.
        state
            .with_store(|store| {
                store
                    .connection()
                    .execute(
                        "UPDATE work_outbox SET body=json_set(body,'$.next_attempt_at',1) WHERE id='ob-retry'",
                        [],
                    )
                    .map_err(|e| WorkbenchError::new("store_error", e.to_string()))?;
                Ok(())
            })
            .unwrap();

        let second_try = deliver_with(
            &state,
            &json!({
                "id": "ob-retry",
                "repo_path": dir.path(),
                "request_id": "del-retry-2",
            })
            .to_string(),
            &transport,
        )
        .unwrap();
        assert_eq!(second_try["item"]["state"], "delivered");
        assert_eq!(*transport.update_calls.borrow(), 2);
    }
}
