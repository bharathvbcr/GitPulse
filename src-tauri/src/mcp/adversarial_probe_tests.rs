//! Adversarial probes of the budget-probe coverage lists (task ft-541eafaf):
//! tools the lane parked in `EMPTY_ON_FIXTURES` are given data here and held to
//! the 64 KiB budget for real.

use super::budget::{self, TRUNCATION_KEY};
use super::*;

const BUDGET: usize = 64 * 1024;

fn git_repo() -> (tempfile::TempDir, String) {
    let dir = tempfile::tempdir().expect("tempdir");
    let repo = dir.path().to_string_lossy().into_owned();
    let init = std::process::Command::new("git")
        .args(["init", "-q", &repo])
        .output()
        .expect("git init");
    assert!(init.status.success(), "git init failed");
    crate::test_support::trust_repo(dir.path());
    (dir, repo)
}

fn schema_of(name: &str) -> Value {
    tools()
        .into_iter()
        .find(|tool| tool["name"] == name)
        .map(|tool| tool["outputSchema"].clone())
        .unwrap_or_else(|| panic!("{name} is not advertised"))
}

/// The wire cost the budget probe uses: the escaped text mirror plus the
/// structured content.
fn wire(fitted: &budget::Fitted) -> usize {
    budget::escaped_len(&fitted.text)
        + serde_json::to_string(fitted.structured.as_ref().expect("structured"))
            .expect("serializes")
            .len()
}

#[test]
fn ledger_events_with_heavy_rows_holds_the_64k_budget_and_a_continuable_cursor() {
    let (_dir, repo) = git_repo();
    let address = crate::ledger::bindings::repository_address(&repo).expect("address");
    for n in 0..300 {
        let id = crate::ledger::record(crate::ledger::Draft {
            repo_path: address.anchor.clone(),
            action: "session.command".into(),
            object: Some(format!("echo {n} {}", "o".repeat(1500))),
            actor_kind: Some(crate::ledger::ActorKind::Agent),
            outcome: Some(crate::ledger::Outcome::Ok),
            detail_json: Some(json!({ "note": "d".repeat(3000) }).to_string()),
            ..Default::default()
        });
        assert!(id.is_some(), "row {n} was not recorded");
    }
    let payload = handle_tool_call(
        "gitpulse_ledger_events",
        &json!({ "repo_path": repo, "limit": 300 }),
    )
    .expect("ledger events");
    let events = payload["events"].as_array().expect("events").len();
    assert!(events > 20, "the fixture reached only {events} rows");

    let fitted = budget::fit(true, payload.clone(), BUDGET).expect("fits");
    let structured = fitted.structured.clone().expect("structured");
    assert!(wire(&fitted) <= BUDGET, "{} bytes", wire(&fitted));
    assert!(
        structured[TRUNCATION_KEY].is_object(),
        "cut without saying so"
    );
    let violations: Vec<String> =
        validate::validate(&structured, &schema_of("gitpulse_ledger_events"))
            .iter()
            .map(validate::Violation::render)
            .collect();
    assert!(violations.is_empty(), "{violations:?}");
    // The cursor continues from the last row kept, not from the rows dropped.
    let kept = structured["events"].as_array().unwrap();
    assert!(!kept.is_empty() && kept.len() < events);
    assert_eq!(
        structured["next_cursor"],
        kept.last().unwrap()["id"],
        "next_cursor skips the dropped rows"
    );
    assert_eq!(structured["truncated"], true);
}

#[test]
fn list_tasks_with_many_long_tasks_holds_the_64k_budget() {
    let dir = tempfile::tempdir().expect("tempdir");
    let repo_dir = tempfile::tempdir().expect("tempdir");
    let repo = repo_dir.path().to_string_lossy().into_owned();
    let init = std::process::Command::new("git")
        .args(["init", "-q", &repo])
        .output()
        .expect("git init");
    assert!(init.status.success());
    crate::test_support::trust_repo(repo_dir.path());
    PROFILE_OVERRIDE.with(|cell| {
        *cell.borrow_mut() = Some(dir.path().join("workbench.sqlite"));
    });
    let words = [
        "quartz", "falcon", "tundra", "marble", "cobalt", "saffron", "glacier", "ember",
    ];
    let mut filed = 0;
    for n in 0..60 {
        let title = format!(
            "{} {} {} {}{n}",
            words[n % 8],
            words[(n * 3 + 1) % 8],
            words[(n * 5 + 2) % 8],
            ["zz", "yy", "xx", "ww"][n % 4]
        );
        let outcome = handle_tool_call(
            "gitpulse_add_task",
            &json!({
                "repo_path": repo,
                "task_id": format!("probe-{n}"),
                "title": title,
                "status": "ready",
                "priority": 2,
                "description": "long ".repeat(12000),
            }),
        );
        if outcome.is_ok() {
            filed += 1;
        }
    }
    assert!(filed >= 3, "only {filed} tasks could be filed");
    let payload = handle_tool_call(
        "gitpulse_list_tasks",
        &json!({ "repo_path": repo, "limit": 100 }),
    )
    .expect("list tasks");
    let listed = payload["tasks"].as_array().map_or(0, Vec::len);
    assert!(listed >= 3, "list_tasks returned {listed} tasks");
    match budget::fit(true, payload, BUDGET) {
        Ok(fitted) => {
            assert!(wire(&fitted) <= BUDGET, "{} bytes", wire(&fitted));
            let structured = fitted.structured.expect("structured");
            let violations: Vec<String> =
                validate::validate(&structured, &schema_of("gitpulse_list_tasks"))
                    .iter()
                    .map(validate::Violation::render)
                    .collect();
            assert!(violations.is_empty(), "{violations:?}");
        }
        // A refusal that names the smaller `limit` is the honest alternative
        // to a cursor that skips rows.
        Err(message) => assert!(message.contains("limit"), "{message}"),
    }
}
