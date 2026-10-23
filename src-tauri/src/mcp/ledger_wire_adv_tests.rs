//! Adversarial probes for the LedgerView fold (ft-a2936457), wave 6 reviewer.
//!
//! `wire_dump_*` tests write every ledger-shaped object an MCP surface emits to
//! the directory named by `LEDGER_WIRE_DUMP`, so the same file can be run on the
//! base (28f8ebe1) and on head and diffed. They assert nothing themselves.

use serde_json::{json, Map, Value};

use super::resources::repo_facet;

fn trusted_repo() -> (tempfile::TempDir, String) {
    let dir = tempfile::tempdir().expect("tempdir");
    crate::test_support::git_in(dir.path(), &["init", "-q"]);
    crate::test_support::trust_repo(dir.path());
    let repo = dir.path().canonicalize().expect("canonical");
    (dir, repo.to_string_lossy().into_owned())
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

/// Every object anywhere in `value` carrying a ledger-status key, by JSON path.
fn ledger_objects(value: &Value, path: String, out: &mut Map<String, Value>, repo: &str) {
    match value {
        Value::Object(map) => {
            if map.contains_key("recording")
                || map.contains_key("freshness_ok")
                || map.contains_key("stale_after_secs")
            {
                let mut copy = map.clone();
                for key in ["events", "next_cursor"] {
                    copy.remove(key);
                }
                for key in ["as_of", "last_catch_up"] {
                    if copy.get(key).is_some_and(Value::is_string) {
                        copy.insert(key.into(), json!("<timestamp>"));
                    }
                }
                let text = Value::Object(copy).to_string().replace(repo, "<repo>");
                out.insert(path.clone(), serde_json::from_str(&text).expect("json"));
            }
            for (k, v) in map {
                ledger_objects(v, format!("{path}/{k}"), out, repo);
            }
        }
        Value::Array(items) => {
            for (i, v) in items.iter().enumerate() {
                ledger_objects(v, format!("{path}[{i}]"), out, repo);
            }
        }
        _ => {}
    }
}

fn dump_surfaces(repo: &str) -> Value {
    let mut out = Map::new();
    for tool in [
        "gitpulse_insights",
        "gitpulse_status",
        "gitpulse_ledger_events",
        "gitpulse_change_context",
        "gitpulse_task_view",
        "gitpulse_list_tasks",
    ] {
        let doc = crate::mcp::handle_tool_call(tool, &json!({ "repo_path": repo }))
            .unwrap_or_else(|e| json!({ "ERR": e.replace(repo, "<repo>") }));
        ledger_objects(&doc, format!("tool:{tool}"), &mut out, repo);
        if doc.get("ERR").is_some() {
            out.insert(format!("tool:{tool}:ERR"), doc);
        }
    }
    for facet in [
        "insights",
        "status",
        "context",
        "ledger",
        "tasks",
        "collisions",
        "changes",
    ] {
        let doc = match repo_facet(facet, repo) {
            Ok(doc) => doc,
            Err(e) => json!({ "ERR": format!("{e:?}").replace(repo, "<repo>") }),
        };
        ledger_objects(&doc, format!("res:{facet}"), &mut out, repo);
        if doc.get("ERR").is_some() {
            out.insert(format!("res:{facet}:ERR"), doc);
        }
    }
    Value::Object(out)
}

fn write_dump(name: &str, value: &Value) {
    if let Ok(dir) = std::env::var("LEDGER_WIRE_DUMP") {
        std::fs::write(
            format!("{dir}/{name}.json"),
            serde_json::to_string_pretty(value).expect("pretty"),
        )
        .expect("write dump");
    }
}

/// A ledger with a recorded catch-up: freshness populated, not the default.
#[test]
fn wire_dump_after_a_recorded_catch_up() {
    let (_dir, repo) = trusted_repo();
    record_event(&repo);
    let record = crate::ledger::CatchUpRecord {
        completed: true,
        recorded: 3,
        transcripts: 2,
        ..Default::default()
    };
    crate::ledger::record_catch_up(&repo, &repo, &record, crate::ledger::ids::now_millis())
        .expect("catch up");
    write_dump("catch_up", &dump_surfaces(&repo));
}

/// A ledger file that exists but is not a database.
#[test]
fn wire_dump_corrupt_ledger() {
    let (_dir, repo) = trusted_repo();
    let path = crate::ledger::ledger_path(&repo);
    std::fs::create_dir_all(path.parent().expect("parent")).expect("state dir");
    std::fs::write(&path, b"this is not a sqlite database at all, just bytes").expect("garbage");
    write_dump("corrupt", &dump_surfaces(&repo));
}

/// A ledger whose catch-up is old (stale), with several rows.
#[test]
fn wire_dump_stale_catch_up() {
    let (_dir, repo) = trusted_repo();
    let _ = crate::ledger::status(&repo);
    let record = crate::ledger::CatchUpRecord {
        completed: true,
        ..Default::default()
    };
    let long_ago = crate::ledger::ids::now_millis().saturating_sub(3 * 3600 * 1000);
    crate::ledger::record_catch_up(&repo, &repo, &record, long_ago).expect("catch up");
    write_dump("stale", &dump_surfaces(&repo));
}
