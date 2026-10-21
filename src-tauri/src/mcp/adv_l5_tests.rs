//! Adversarial probes of lane L5 (MCP contract, read paths). Tests only.

use super::budget::{self, TRUNCATION_KEY};
use super::*;
use std::os::unix::fs::PermissionsExt;
use std::time::Instant;

const SURFACE_TOOLS: [&str; 4] = [
    "gitpulse_status",
    "gitpulse_insights",
    "gitpulse_ledger_events",
    "gitpulse_change_context",
];

fn trusted_repo() -> (tempfile::TempDir, String) {
    let dir = tempfile::tempdir().expect("tempdir");
    crate::test_support::git_in(dir.path(), &["init", "-q"]);
    crate::test_support::trust_repo(dir.path());
    let repo = dir.path().canonicalize().expect("canonical");
    (dir, repo.to_string_lossy().into_owned())
}

fn record(repo: &str, n: usize) {
    for _ in 0..n {
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

fn schema_of(name: &str) -> Value {
    tools()
        .into_iter()
        .find(|tool| tool["name"] == name)
        .map(|tool| tool["outputSchema"].clone())
        .unwrap_or_else(|| panic!("{name} is not advertised"))
}

fn shape(prefix: &str, v: &Value, out: &mut Vec<String>) {
    match v {
        Value::Object(m) => {
            out.push(format!("{prefix}:object"));
            for (k, v) in m {
                shape(&format!("{prefix}.{k}"), v, out);
            }
        }
        Value::Array(a) => {
            out.push(format!("{prefix}:array"));
            if let Some(first) = a.first() {
                shape(&format!("{prefix}[]"), first, out);
            }
        }
        Value::String(_) => out.push(format!("{prefix}:string")),
        Value::Number(_) => out.push(format!("{prefix}:number")),
        Value::Bool(_) => out.push(format!("{prefix}:bool")),
        Value::Null => out.push(format!("{prefix}:null")),
    }
}

/// Writes the JSON shape of every surface to `$GITPULSE_ADV_SHAPE_OUT` (when
/// set) so the same fixture can be run against the base commit and diffed.
#[test]
fn adv_dump_shapes_for_the_base_diff() {
    let Ok(out_path) = std::env::var("GITPULSE_ADV_SHAPE_OUT") else {
        return;
    };
    let (_dir, repo) = trusted_repo();
    record(&repo, 2);
    let mut lines = Vec::new();
    for tool in SURFACE_TOOLS {
        let payload = handle_tool_call(tool, &json!({ "repo_path": repo }))
            .unwrap_or_else(|e| json!({"err": e}));
        shape(tool, &payload, &mut lines);
    }
    for facet in ["ledger", "status", "insights", "context"] {
        let uri = format!("gitpulse://{facet}{repo}");
        let doc = resources::read(&uri).map(|c| c[0]["text"].clone());
        let parsed: Value = doc
            .ok()
            .and_then(|t| serde_json::from_str(t.as_str().unwrap_or("null")).ok())
            .unwrap_or(Value::Null);
        shape(&format!("resource:{facet}"), &parsed, &mut lines);
    }
    lines.sort();
    lines.dedup();
    std::fs::write(out_path, lines.join("\n")).unwrap();
}

#[test]
fn adv_every_surface_validates_against_its_declared_output_schema() {
    for with_ledger in [true, false] {
        let (_dir, repo) = trusted_repo();
        if with_ledger {
            record(&repo, 2);
        }
        for tool in SURFACE_TOOLS {
            let payload = handle_tool_call(tool, &json!({ "repo_path": repo })).expect(tool);
            let violations: Vec<String> = validate::validate(&payload, &schema_of(tool))
                .iter()
                .map(validate::Violation::render)
                .collect();
            assert!(
                violations.is_empty(),
                "{tool} (ledger={with_ledger}) violates its outputSchema: {violations:?}"
            );
        }
    }
}

#[test]
fn adv_new_fields_are_declared_in_the_manifest_tool_schemas() {
    let manifest = resources::read("gitpulse://server/manifest").expect("manifest");
    let doc: Value = serde_json::from_str(manifest[0]["text"].as_str().unwrap()).unwrap();
    let find = |name: &str| {
        doc["tools"]
            .as_array()
            .unwrap()
            .iter()
            .find(|t| t["name"] == name)
            .unwrap_or_else(|| panic!("{name} missing from the manifest"))
            .clone()
    };
    let events = &find("gitpulse_ledger_events")["outputSchema"]["properties"];
    for field in [
        "as_of",
        "last_catch_up",
        "last_catch_up_basis",
        "stale",
        "stale_reason",
        "stale_after_secs",
        "freshness_ok",
        "freshness_error",
        "catch_up",
        "caveat",
        "caveat_code",
        "recording",
        "recording_state",
        "dropped_scope",
    ] {
        assert!(
            events[field].is_object(),
            "ledger_events schema lacks {field}"
        );
    }
    for tool in ["gitpulse_status", "gitpulse_insights"] {
        let ledger = &find(tool)["outputSchema"]["properties"]["ledger"]["properties"];
        for field in [
            "stale",
            "as_of",
            "recording_state",
            "dropped_scope",
            "catch_up",
        ] {
            assert!(
                ledger[field].is_object(),
                "{tool}.ledger schema lacks {field}"
            );
        }
    }
    let ctx = &find("gitpulse_change_context")["outputSchema"]["properties"];
    assert!(ctx["ledger_freshness"]["properties"]["stale"].is_object());
}

#[test]
fn adv_a_budget_cut_page_keeps_its_freshness_and_stays_in_budget() {
    let (_dir, repo) = trusted_repo();
    let address = crate::ledger::bindings::repository_address(&repo).expect("address");
    for n in 0..300 {
        crate::ledger::record(crate::ledger::Draft {
            repo_path: address.anchor.clone(),
            action: "session.command".into(),
            object: Some(format!("echo {n} {}", "o".repeat(1500))),
            actor_kind: Some(crate::ledger::ActorKind::Agent),
            outcome: Some(crate::ledger::Outcome::Ok),
            detail_json: Some(json!({ "note": "d".repeat(3000) }).to_string()),
            ..Default::default()
        })
        .expect("row");
    }
    let payload = handle_tool_call(
        "gitpulse_ledger_events",
        &json!({ "repo_path": repo, "limit": 300 }),
    )
    .expect("page");
    let budget_bytes = 64 * 1024;
    let fitted = budget::fit(true, payload, budget_bytes).expect("fits");
    let structured = fitted.structured.clone().expect("structured");
    let wire =
        budget::escaped_len(&fitted.text) + serde_json::to_string(&structured).unwrap().len();
    assert!(
        wire <= budget_bytes,
        "{wire} bytes over the {budget_bytes} budget"
    );
    assert!(structured[TRUNCATION_KEY].is_object());
    for key in [
        "stale",
        "as_of",
        "freshness_ok",
        "recording_state",
        "caveat",
    ] {
        assert!(
            !structured[key].is_null() || key == "as_of",
            "the budget cut dropped {key}: {}",
            structured
                .as_object()
                .unwrap()
                .keys()
                .cloned()
                .collect::<Vec<_>>()
                .join(",")
        );
    }
}

fn lock_down(repo: &str) -> Vec<std::path::PathBuf> {
    let db = crate::ledger::ledger_path(repo);
    {
        let conn = rusqlite::Connection::open(&db).unwrap();
        let _ = conn.query_row("PRAGMA wal_checkpoint(TRUNCATE)", [], |_| Ok(()));
    }
    let dir = db.parent().unwrap().to_path_buf();
    let mut paths = Vec::new();
    for e in std::fs::read_dir(&dir).unwrap() {
        let p = e.unwrap().path();
        std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o444)).unwrap();
        paths.push(p);
    }
    std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o555)).unwrap();
    paths.push(dir);
    paths
}

fn unlock(paths: &[std::path::PathBuf]) {
    for p in paths.iter().rev() {
        let mode = if p.is_dir() { 0o755 } else { 0o644 };
        let _ = std::fs::set_permissions(p, std::fs::Permissions::from_mode(mode));
    }
}

fn tree_bytes(dir: &std::path::Path) -> Vec<(String, Vec<u8>, std::time::SystemTime)> {
    let mut v: Vec<_> = std::fs::read_dir(dir)
        .unwrap()
        .map(|e| {
            let p = e.unwrap().path();
            (
                p.file_name().unwrap().to_string_lossy().into_owned(),
                std::fs::read(&p).unwrap(),
                std::fs::metadata(&p).unwrap().modified().unwrap(),
            )
        })
        .collect();
    v.sort_by(|a, b| a.0.cmp(&b.0));
    v
}

#[test]
fn adv_every_mcp_read_path_on_a_read_only_ledger_never_writes_and_never_hangs() {
    let (_dir, repo) = trusted_repo();
    record(&repo, 2);
    let locked = lock_down(&repo);
    let state_dir = crate::ledger::ledger_path(&repo)
        .parent()
        .unwrap()
        .to_path_buf();
    let before = tree_bytes(&state_dir);
    let mut report = Vec::new();
    for tool in SURFACE_TOOLS {
        let started = Instant::now();
        let result =
            std::panic::catch_unwind(|| handle_tool_call(tool, &json!({ "repo_path": repo })));
        let elapsed = started.elapsed();
        match result {
            Err(_) => report.push(format!("{tool}: PANIC")),
            Ok(Err(e)) => report.push(format!("{tool}: Err({e}) in {elapsed:?}")),
            Ok(Ok(v)) => {
                let facet = if tool == "gitpulse_ledger_events" {
                    &v
                } else {
                    &v["ledger"]
                };
                let unreadable_but_fresh = facet["freshness_ok"] == false && facet["stale"] != true;
                if unreadable_but_fresh {
                    report.push(format!("{tool}: freshness_ok:false but stale!=true"));
                }
                if facet["freshness_ok"] == false && facet["freshness_error"] == "" {
                    report.push(format!("{tool}: freshness_ok:false with no reason"));
                }
                if elapsed.as_secs() >= 10 {
                    report.push(format!("{tool}: slow {elapsed:?}"));
                }
            }
        }
    }
    for facet in ["ledger", "status", "insights", "context"] {
        let started = Instant::now();
        let r = std::panic::catch_unwind(|| resources::read(&format!("gitpulse://{facet}{repo}")));
        if r.is_err() {
            report.push(format!("resource {facet}: PANIC"));
        } else if started.elapsed().as_secs() >= 10 {
            report.push(format!("resource {facet}: slow {:?}", started.elapsed()));
        }
    }
    let after = tree_bytes(&state_dir);
    unlock(&locked);
    assert_eq!(before, after, "a read path wrote to the ledger directory");
    assert!(report.is_empty(), "{report:#?}");
}

#[test]
fn adv_every_mcp_read_path_on_a_ledger_locked_by_another_process_is_bounded() {
    let (_dir, repo) = trusted_repo();
    record(&repo, 2);
    let db = crate::ledger::ledger_path(&repo);
    let mut writer = std::process::Command::new("sqlite3")
        .arg(&db)
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::inherit())
        .stderr(std::process::Stdio::inherit())
        .spawn()
        .expect("sqlite3 on PATH");
    {
        use std::io::Write;
        let stdin = writer.stdin.as_mut().unwrap();
        stdin
            .write_all(b".timeout 5000\nBEGIN IMMEDIATE;\nUPDATE events SET ts_utc = ts_utc WHERE id = 1;\n")
            .unwrap();
        stdin.flush().unwrap();
    }
    std::thread::sleep(std::time::Duration::from_millis(800));
    {
        let probe = rusqlite::Connection::open(&db).unwrap();
        probe
            .busy_timeout(std::time::Duration::from_millis(100))
            .unwrap();
        let held = probe.execute("UPDATE events SET ts_utc = ts_utc WHERE id = 1", []);
        assert!(held.is_err(), "the fixture's lock is not held: {held:?}");
    }
    let mut timings = Vec::new();
    let mut panics = Vec::new();
    let mut bad = Vec::new();
    for tool in SURFACE_TOOLS {
        let started = Instant::now();
        let r = std::panic::catch_unwind(|| handle_tool_call(tool, &json!({ "repo_path": repo })));
        timings.push(format!("{tool}: {:?}", started.elapsed()));
        match r {
            Err(_) => panics.push(tool),
            Ok(Ok(v)) => {
                let facet = if tool == "gitpulse_ledger_events" {
                    &v
                } else {
                    &v["ledger"]
                };
                if facet["freshness_ok"] == true
                    && facet["stale"] != false
                    && facet["as_of"].is_null()
                {
                    bad.push(format!("{tool}: {facet}"));
                }
                if facet["freshness_ok"] == false && facet["stale"] != true {
                    bad.push(format!("{tool}: unreadable but not stale"));
                }
            }
            Ok(Err(_)) => {}
        }
    }
    writer.kill().unwrap();
    writer.wait().unwrap();
    eprintln!("LOCKED TIMINGS: {timings:#?}");
    assert!(panics.is_empty(), "{panics:?}");
    assert!(bad.is_empty(), "{bad:#?}");
    let slowest = timings
        .iter()
        .filter_map(|t| t.rsplit(": ").next())
        .map(|s| s.to_string())
        .collect::<Vec<_>>();
    assert!(
        !slowest.iter().any(|s| s.ends_with('s')
            && !s.ends_with("ms")
            && !s.ends_with("µs")
            && s.trim_end_matches('s')
                .parse::<f64>()
                .map_or(false, |v| v > 30.0)),
        "{timings:#?}"
    );
}
