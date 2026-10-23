//! The ledger JSON every MCP surface serialises, pinned field for field.
//!
//! The ledger facet of `gitpulse_insights` and `gitpulse_status`, the honesty
//! block of `gitpulse_ledger_events`, and the `ledger`, `insights` and `status`
//! resources are a public contract. This test holds the exact names, types and
//! values (`ledger_wire_golden.json`) for a ledger that opened read-only and for
//! a repository with no ledger, so the representation behind them can change
//! without the wire moving. Only the machine-specific path and the two
//! timestamps are normalised.

use serde_json::{json, Map, Value};

use super::resources::repo_facet;

const GOLDEN: &str = include_str!("ledger_wire_golden.json");

fn trusted_repo() -> (tempfile::TempDir, String) {
    let dir = tempfile::tempdir().expect("tempdir");
    crate::test_support::git_in(dir.path(), &["init", "-q"]);
    crate::test_support::trust_repo(dir.path());
    let repo = dir.path().canonicalize().expect("canonical");
    let repo = repo.to_string_lossy().into_owned();
    (dir, repo)
}

fn record_event(repo: &str) {
    crate::ledger::append(crate::ledger::Draft {
        repo_path: repo.to_string(),
        action: "git.commit".to_string(),
        actor_kind: Some(crate::ledger::ActorKind::Human),
        outcome: Some(crate::ledger::Outcome::Ok),
        ..Default::default()
    })
    .expect("append");
}

/// The machine-specific values replaced by a placeholder, nothing else.
fn normalised(mut value: Value) -> Value {
    if let Some(object) = value.as_object_mut() {
        if object.get("path").is_some_and(Value::is_string) {
            object.insert("path".into(), json!("<ledger path>"));
        }
        for key in ["as_of", "last_catch_up"] {
            if object.get(key).is_some_and(Value::is_string) {
                object.insert(key.into(), json!("<timestamp>"));
            }
        }
    }
    value
}

/// A ledger-events page without its rows and cursor, which are not the ledger
/// block and carry ids and times.
fn page_block(page: Value) -> Value {
    let mut object: Map<String, Value> = page.as_object().expect("a page object").clone();
    for row_field in ["events", "next_cursor"] {
        object.remove(row_field);
    }
    normalised(Value::Object(object))
}

fn surfaces(repo: &str) -> Value {
    let tool = |name: &str| {
        crate::mcp::handle_tool_call(name, &json!({ "repo_path": repo })).expect(name)
    };
    let facet = |name: &str| repo_facet(name, repo).expect(name);
    json!({
        "tool_insights_ledger": normalised(tool("gitpulse_insights")["ledger"].clone()),
        "tool_status_ledger": normalised(tool("gitpulse_status")["ledger"].clone()),
        "tool_ledger_events": page_block(tool("gitpulse_ledger_events")),
        "resource_ledger": page_block(facet("ledger")),
        "resource_insights_ledger": normalised(facet("insights")["ledger"].clone()),
        "resource_status_ledger": normalised(facet("status")["ledger"].clone()),
    })
}

#[test]
fn the_mcp_ledger_json_is_unchanged_for_a_readable_ledger_and_for_none() {
    let (_with_dir, with_ledger) = trusted_repo();
    record_event(&with_ledger);
    let (_without_dir, without_ledger) = trusted_repo();

    let actual = json!({
        "readable_ledger": surfaces(&with_ledger),
        "no_ledger": surfaces(&without_ledger),
    });
    println!(
        "GOLDEN_BEGIN\n{}\nGOLDEN_END",
        serde_json::to_string_pretty(&actual).expect("serialise")
    );

    let golden: Value = serde_json::from_str(GOLDEN).expect("the golden file is JSON");
    assert_eq!(
        actual, golden,
        "an MCP ledger field changed name, type, presence or value"
    );
}
