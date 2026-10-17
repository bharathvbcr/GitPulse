//! Adversarial review of ft-9cfa5374 (deferred_worktree). Tests marked
//! `#[should_panic]` pin a KNOWN BREAK: they assert the correct behaviour and
//! currently fail, so they flip red (and must be un-marked) once it is fixed.

use super::*;

const MARKER_DIR: &str = "hostile deferred under load after 1s";

/// A directory that exists, is not a repository, and whose NAME is the gate's
/// deferral marker: `validate_repo_path` quotes the path back in
/// "Not a Git repository: <path>".
fn hostile_dir() -> tempfile::TempDir {
    let parent = tempfile::tempdir().expect("tempdir");
    std::fs::create_dir(parent.path().join(MARKER_DIR)).expect("mkdir");
    parent
}

/// BREAK 1 (medium): `is_deferred_under_load` is a substring match, so a real
/// "Not a Git repository: <path>" whose path contains the marker is classified
/// `deferred_worktree`. Expected `invalid_worktree`.
#[test]
#[should_panic(expected = "deferred_worktree")]
fn a_hostile_path_cannot_make_an_invalid_worktree_read_as_deferred() {
    let parent = hostile_dir();
    let path = parent.path().join(MARKER_DIR);
    let error = crate::ledger::bindings::repository_address_readonly(path.to_str().unwrap())
        .expect_err("not a repository");
    assert_eq!(error.code, "invalid_worktree", "{error}");
}

/// BREAK 1b: the same text reaches the insights payload, so the facets of a
/// non-repository are marked `deferred: true` with `retry_after_ms`, telling
/// an agent to retry something that can never succeed.
#[test]
#[should_panic(expected = "can never succeed")]
fn a_hostile_path_is_not_marked_deferred_in_an_insights_payload() {
    let parent = hostile_dir();
    let path = parent.path().join(MARKER_DIR);
    let mut wire = json!(insights::snapshot(path.to_str().unwrap()));
    let marked = insights::mark_deferrals(&mut wire);
    assert!(
        !marked,
        "marked a real failure deferred; it can never succeed: {wire}"
    );
}

/// BREAK 2 (medium): a facet that merely QUOTES the marker inside a real error
/// (a path) is marked deferred.
#[test]
#[should_panic(expected = "was marked")]
fn a_facet_that_only_quotes_a_deferral_is_not_marked() {
    let mut payload = json!({ "changes": { "ok": false,
        "error": format!("fatal: cannot chdir to '/tmp/{MARKER_DIR}': No such file") } });
    assert!(
        !insights::mark_deferrals(&mut payload),
        "a real failure that quotes the marker was marked: {payload}"
    );
}

/// BREAK 3 (low): `mark_deferrals` marks an `ok: true` facet that carries a
/// deferral-worded `error` (the `own` arm ignores `ok`).
#[test]
#[should_panic(expected = "ok:true facet was marked")]
fn an_ok_true_facet_is_never_marked() {
    let mut payload = json!({ "worktrees": { "ok": true,
        "error": "git status deferred under load after 0.000s: x" } });
    insights::mark_deferrals(&mut payload);
    assert!(
        payload["worktrees"].get("deferred").is_none(),
        "ok:true facet was marked deferred: {payload}"
    );
}

/// BREAK 4 (low): a facet that failed for a REAL reason is marked deferred when
/// any descendant deferred (`found && failed`), overstating "retryable".
#[test]
#[should_panic(expected = "parent with a real error")]
fn a_real_failure_with_a_deferred_child_is_not_itself_deferred() {
    let mut payload = json!({ "agents": { "ok": false, "error": "real failure",
        "live": { "kinds": [ { "ok": false,
            "error": "ps deferred under load after 0.000s: x" } ] } } });
    insights::mark_deferrals(&mut payload);
    assert!(
        payload["agents"].get("deferred").is_none(),
        "parent with a real error marked deferred: {payload}"
    );
}

/// Pinned, not a break: nested arrays are marked, a cut payload is not.
#[test]
fn nested_array_deferral_is_marked_and_a_cut_payload_is_not() {
    let mut nested = json!({ "agents": { "ok": false, "live": { "kinds": [
        { "kind": "claude", "ok": false, "error": "ps deferred under load after 0.000s: x" }
    ] } } });
    assert!(insights::mark_deferrals(&mut nested));
    assert_eq!(
        nested["agents"]["live"]["kinds"][0]["deferred"],
        json!(true)
    );
    let mut cut = json!({ "agents": { "ok": false, "live": { "kinds": [] } } });
    assert!(!insights::mark_deferrals(&mut cut));
}

/// Pinned, not a break: at the production rate the delay is under 250 ms for
/// every jitter, and an already-expired deadline skips the retry. (The rate is
/// a `const`; the 250 ms figure is only true while it stays at 8/s.)
#[test]
fn retry_delay_is_under_250ms_and_an_expired_deadline_skips() {
    let base = insights::deferral_retry_after_ms();
    assert!(base > 0 && base * 2 <= 250, "base {base}");
    let origin = std::time::Instant::now();
    let slept = std::cell::Cell::new(false);
    let sleep = |_d: std::time::Duration| slept.set(true);
    let jitter = |bound: u64| bound - 1;
    let now = || origin + std::time::Duration::from_secs(60);
    let reads = std::cell::Cell::new(0);
    retry_once_on_deferral(
        || reads.set(reads.get() + 1),
        |_| true,
        &DeferralRetry {
            sleep: &sleep,
            jitter: &jitter,
            now: &now,
            deadline: origin,
        },
    );
    assert_eq!((reads.get(), slept.get()), (1, false));
    for hit in 0..2000u64 {
        let value = process_jitter(base);
        assert!(value < base, "{hit}: {value}");
    }
}

/// Paths where `payload` carries `deferred` but its schema node does not
/// declare it. A child the schema leaves untyped is still walked.
fn undeclared_deferral(payload: &Value, schema: &Value, at: &str, out: &mut Vec<String>) {
    let Some(map) = payload.as_object() else {
        return;
    };
    let props = schema.get("properties").and_then(Value::as_object);
    if map.contains_key("deferred") && props.is_none_or(|p| !p.contains_key("deferred")) {
        out.push(at.to_string());
    }
    for (key, child) in map {
        let sub = props.and_then(|p| p.get(key)).cloned().unwrap_or(json!({}));
        undeclared_deferral(child, &sub, &format!("{at}.{key}"), out);
    }
}

fn schema_of(tool: &str) -> Value {
    tools()
        .into_iter()
        .find(|t| t["name"] == tool)
        .expect("tool")["outputSchema"]
        .clone()
}

fn deferred_answer(tool: &str, repo: &str) -> Value {
    crate::engine::git_cli::with_forced_deferral(|| {
        handle_tool_call(tool, &json!({ "repo_path": repo })).expect(tool)
    })
}

/// Every wrapped read tool, forced into deferral, still satisfies its
/// advertised outputSchema (the schemas are open, so extra properties are not
/// violations).
#[test]
fn every_wrapped_tool_under_forced_deferral_meets_its_output_schema() {
    let repo = crate::test_support::git_repo();
    let path = repo.path().to_str().unwrap();
    for tool in [
        "gitpulse_insights",
        "gitpulse_collision_risk",
        "gitpulse_active_changes",
        "gitpulse_change_context",
    ] {
        let payload = deferred_answer(tool, path);
        let violations: Vec<String> = validate::validate(&payload, &schema_of(tool))
            .iter()
            .map(validate::Violation::render)
            .collect();
        assert!(violations.is_empty(), "{tool}: {violations:?}\n{payload}");
    }
}

/// BREAK 5 (low, contract honesty): `deferred`/`retry_after_ms` is declared
/// only on the collision facet. Insights' `worktrees`, `agents`, `changes`,
/// `ledger` are `{type: object}`, and `gitpulse_active_changes` /
/// `gitpulse_change_context` top levels do not declare it, yet all can emit it.
/// Not a validation failure (schemas are open).
#[test]
#[should_panic(expected = "undeclared deferred fields")]
fn every_emitted_deferred_field_is_declared_in_the_advertised_schema() {
    let repo = crate::test_support::git_repo();
    let path = repo.path().to_str().unwrap();
    let mut gaps = Vec::new();
    for tool in [
        "gitpulse_insights",
        "gitpulse_active_changes",
        "gitpulse_change_context",
    ] {
        let payload = deferred_answer(tool, path);
        undeclared_deferral(&payload, &schema_of(tool), tool, &mut gaps);
    }
    assert!(gaps.is_empty(), "undeclared deferred fields: {gaps:?}");
}

/// Write tools are never wrapped: the retry's only production callers are the
/// six read tools (insights, active_changes, collision_risk, change_context,
/// ledger_events, task_view).
#[test]
fn only_read_tools_call_the_production_retry() {
    let production = include_str!("mod.rs")
        .split("#[cfg(test)]\nmod tests")
        .next()
        .expect("production half");
    let sites: Vec<&str> = production
        .lines()
        .filter(|l| l.contains("with_production_retry("))
        .collect();
    assert_eq!(sites.len(), 6, "{sites:#?}");
    for write in [
        "add_task",
        "complete_task",
        "delete_task",
        "merge_tasks",
        "import_tasks",
    ] {
        let arm = production
            .split(&format!("\"gitpulse_{write}\" =>"))
            .nth(1)
            .and_then(|rest| rest.split("\n        \"gitpulse_").next())
            .unwrap_or("");
        assert!(!arm.contains("with_production_retry"), "{write} is wrapped");
    }
}
