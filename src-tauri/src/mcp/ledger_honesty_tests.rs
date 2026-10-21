//! The ledger tool, resource and change-context answers say how current they
//! are. Kept out of `resources.rs` because it dispatches through
//! `handle_tool_call`, which that file is forbidden to name.

use serde_json::{json, Value};

use super::resources::repo_facet;

fn trusted_repo() -> (tempfile::TempDir, String) {
    let dir = tempfile::tempdir().expect("tempdir");
    crate::test_support::git_in(dir.path(), &["init", "-q"]);
    crate::test_support::trust_repo(dir.path());
    let repo = dir.path().canonicalize().expect("canonical");
    let repo = repo.to_string_lossy().into_owned();
    (dir, repo)
}

fn record_events(repo: &str, count: usize) {
    for _ in 0..count {
        crate::ledger::append(crate::ledger::Draft {
            repo_path: repo.to_string(),
            action: "git.commit".to_string(),
            actor_kind: Some(crate::ledger::ActorKind::Human),
            outcome: Some(crate::ledger::Outcome::Ok),
            ..Default::default()
        })
        .expect("append");
    }
}

fn tool_page(repo: &str) -> Value {
    crate::mcp::handle_tool_call("gitpulse_ledger_events", &json!({ "repo_path": repo }))
        .expect("ledger events")
}

#[test]
fn the_ledger_tool_and_resource_say_how_current_they_are() {
    let (_dir, repo) = trusted_repo();
    record_events(&repo, 2);

    let tool = tool_page(&repo);
    let resource = repo_facet("ledger", &repo).expect("ledger facet");

    for page in [&tool, &resource] {
        assert_eq!(page["ok"], true, "{page}");
        assert!(page["as_of"].is_string(), "{page}");
        assert_eq!(page["stale"], false, "rows were just written: {page}");
        assert_eq!(page["freshness_ok"], true, "{page}");
        assert_eq!(page["last_catch_up_basis"], "newest_row", "{page}");
        assert_eq!(page["caveat"], "", "{page}");
        assert_eq!(page["caveat_code"], "", "{page}");
        assert!(page["catch_up"].is_null(), "{page}");
    }
    assert_eq!(tool["as_of"], resource["as_of"]);
}

#[test]
fn a_repository_with_no_ledger_is_stale_with_no_time_in_the_tool_answer() {
    let (_dir, repo) = trusted_repo();

    let page = tool_page(&repo);

    assert_eq!(page["ok"], false, "{page}");
    assert!(page["as_of"].is_null(), "{page}");
    assert_eq!(page["stale"], true, "{page}");
    assert_eq!(page["freshness_ok"], false, "{page}");
}

#[test]
fn a_sibling_ledger_not_yet_consolidated_is_a_caveat_on_the_tool_resource_and_status() {
    let (dir, repo) = trusted_repo();
    record_events(&repo, 1);
    crate::test_support::git_in(dir.path(), &["commit", "--allow-empty", "-m", "seed"]);
    let parent = tempfile::tempdir().expect("worktree parent");
    let linked = parent.path().join("task-worktree");
    crate::test_support::git_in(
        dir.path(),
        &[
            "worktree",
            "add",
            "--detach",
            linked.to_str().expect("utf8"),
        ],
    );
    crate::test_support::trust_repo(&linked);
    let linked = linked.canonicalize().expect("canonical");
    record_events(&linked.to_string_lossy(), 1);

    let tool = tool_page(&repo);
    let resource = repo_facet("ledger", &repo).expect("ledger facet");
    let status = super::resources::status_document(&repo).expect("status");

    let message = crate::ledger::bindings::LEGACY_UNCONSOLIDATED_MESSAGE;
    for page in [&tool, &resource] {
        assert_eq!(page["caveat"], message, "{page}");
        assert_eq!(
            page["caveat_code"], "legacy_ledger_unconsolidated",
            "{page}"
        );
    }
    // One text for all three surfaces.
    assert_eq!(status["ledger"]["error"], message, "{status}");
    assert_eq!(
        status["ledger"]["error_code"], "legacy_ledger_unconsolidated",
        "{status}"
    );
    assert_eq!(status["ledger"]["recording"], false, "{status}");
    assert_eq!(status["ledger"]["recording_state"], "readable", "{status}");
}

#[test]
fn change_context_names_the_as_of_of_the_ledger_it_read() {
    let (_dir, repo) = trusted_repo();
    record_events(&repo, 1);
    let as_of = tool_page(&repo)["as_of"].clone();

    let context =
        crate::mcp::handle_tool_call("gitpulse_change_context", &json!({ "repo_path": repo }))
            .expect("change context");

    assert_eq!(context["ledger_freshness"]["as_of"], as_of, "{context}");
    assert_eq!(context["ledger_freshness"]["stale"], false, "{context}");
}

#[test]
fn every_mcp_ledger_surface_serialises_the_honest_view() {
    let (_dir, repo) = trusted_repo();
    record_events(&repo, 1);

    let insights = crate::mcp::handle_tool_call("gitpulse_insights", &json!({ "repo_path": repo }))
        .expect("insights");
    let status = crate::mcp::handle_tool_call("gitpulse_status", &json!({ "repo_path": repo }))
        .expect("status");
    let page = tool_page(&repo);
    let resource = repo_facet("ledger", &repo).expect("ledger resource");
    let insights_resource = repo_facet("insights", &repo).expect("insights resource");
    let status_resource = repo_facet("status", &repo).expect("status resource");

    for (name, ledger) in [
        ("gitpulse_insights", &insights["ledger"]),
        ("insights resource", &insights_resource["ledger"]),
        ("gitpulse_status", &status["ledger"]),
        ("status resource", &status_resource["ledger"]),
        ("gitpulse_ledger_events", &page),
        ("ledger resource", &resource),
    ] {
        assert_eq!(ledger["recording"], false, "{name}: {ledger}");
        assert_eq!(ledger["recording_state"], "readable", "{name}: {ledger}");
        assert_eq!(ledger["dropped_scope"], "this_process", "{name}: {ledger}");
        assert!(ledger["as_of"].is_string(), "{name}: {ledger}");
        assert_eq!(ledger["stale"], false, "{name}: {ledger}");
    }
}
