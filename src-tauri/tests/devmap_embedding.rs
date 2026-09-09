//! Consumer contracts shared by the database, map and graph adapters.

use gitpulse_lib::{codeintel, devmap};
use serde_json::json;
use std::fs;
use std::path::Path;

fn repository() -> tempfile::TempDir {
    let root = tempfile::tempdir().unwrap();
    let result = std::process::Command::new("git")
        .args(["init", "-b", "main"])
        .current_dir(root.path())
        .output()
        .unwrap();
    assert!(
        result.status.success(),
        "{}",
        String::from_utf8_lossy(&result.stderr)
    );
    root
}

fn artifacts(root: &Path, state: &str, id: &str) {
    let state = root.join(state);
    fs::create_dir_all(state.join("graph")).unwrap();
    fs::write(
        state.join("repo_map.json"),
        serde_json::to_vec(&json!({
            "files": [],
            "subsystems": [{"id": id, "area": id, "path": id, "name": id, "files": []}]
        }))
        .unwrap(),
    )
    .unwrap();
    fs::write(
        state.join("graph/code_graph.json"),
        serde_json::to_vec(&json!({
            "nodes": [{"id": id, "path": id, "kind": "file"}], "edges": []
        }))
        .unwrap(),
    )
    .unwrap();
}

#[test]
fn standalone_state_wins_for_every_consumer_when_both_layouts_exist() {
    let root = repository();
    artifacts(root.path(), ".devcouncil", "legacy.rs");
    artifacts(root.path(), ".devmap", "standalone.rs");
    let repo = root.path().to_str().unwrap();
    assert_eq!(
        codeintel::devmap_db_path(repo),
        root.path().join(".devmap/codeintel/devmap.sqlite")
    );
    assert_eq!(
        devmap::repo_map_path(root.path()),
        root.path().join(".devmap/repo_map.json")
    );
    assert_eq!(
        devmap::code_graph_path(root.path()),
        root.path().join(".devmap/graph/code_graph.json")
    );
    let graph = devmap::load_code_graph_viz(repo, None, None);
    assert!(graph.available, "{:?}", graph.reason);
    assert_eq!(graph.payload.unwrap()["nodes"][0]["id"], "standalone.rs");
    let map = devmap::load_map_preview(repo);
    assert!(map.available, "{:?}", map.reason);
    assert_eq!(map.payload.unwrap()["nodes"][0]["id"], "standalone.rs");
}

#[test]
fn fresh_and_legacy_repositories_follow_the_writer_layout() {
    let root = repository();
    let repo = root.path().to_str().unwrap();
    assert_eq!(
        codeintel::devmap_db_path(repo),
        root.path().join(".devmap/codeintel/devmap.sqlite")
    );
    assert_eq!(
        devmap::repo_map_path(root.path()),
        root.path().join(".devmap/repo_map.json")
    );
    assert_eq!(
        devmap::code_graph_path(root.path()),
        root.path().join(".devmap/graph/code_graph.json")
    );
    fs::create_dir(root.path().join(".devcouncil")).unwrap();
    assert_eq!(
        codeintel::devmap_db_path(repo),
        root.path().join(".devcouncil/codeintel/devmap.sqlite")
    );
    assert_eq!(
        devmap::repo_map_path(root.path()),
        root.path().join(".devcouncil/repo_map.json")
    );
    assert_eq!(
        devmap::code_graph_path(root.path()),
        root.path().join(".devcouncil/graph/code_graph.json")
    );
}

#[test]
fn malformed_artifacts_never_report_a_successful_empty_graph() {
    let root = repository();
    artifacts(root.path(), ".devcouncil", "a.rs");
    let repo = root.path().to_str().unwrap();
    for input in ["null", "{}", "{\"nodes\":[],\"edges\":{}}"] {
        fs::write(root.path().join(".devcouncil/graph/code_graph.json"), input).unwrap();
        let result = devmap::load_code_graph_viz(repo, None, None);
        assert!(!result.available, "malformed graph accepted: {input}");
        assert!(result.reason.is_some());
        assert!(result.payload.is_none());
    }
    fs::write(root.path().join(".devcouncil/repo_map.json"), "{}").unwrap();
    let result = devmap::load_map_preview(repo);
    assert!(!result.available, "malformed map accepted");
    assert!(result.reason.is_some());
    assert!(result.payload.is_none());
}

#[test]
fn a_selected_broken_standalone_map_never_falls_back_to_legacy_success() {
    let root = repository();
    artifacts(root.path(), ".devcouncil", "legacy.rs");
    fs::create_dir(root.path().join(".devmap")).unwrap();
    let repo = root.path().to_str().unwrap();
    assert!(!devmap::load_code_graph_viz(repo, None, None).available);
    assert!(!devmap::load_map_preview(repo).available);
    assert!(!devmap::load_repo_map(repo).available);
    assert!(!codeintel::status(repo).available);
}

#[test]
fn incompatible_exports_and_oversized_documents_are_refused() {
    let root = repository();
    artifacts(root.path(), ".devcouncil", "a.rs");
    let repo = root.path().to_str().unwrap();
    let graph = root.path().join(".devcouncil/graph/code_graph.json");
    for document in [
        json!({"schema_version":999,"nodes":[],"edges":[]}),
        json!({"meta":{"compatibility_export_tier":"stub"},"nodes":[],"edges":[]}),
        json!({"meta":{"graph_export_incomplete_reason":"capped"},"nodes":[],"edges":[]}),
    ] {
        fs::write(&graph, serde_json::to_vec(&document).unwrap()).unwrap();
        let result = devmap::load_code_graph_viz(repo, None, None);
        assert!(
            !result.available,
            "incompatible export accepted: {document}"
        );
        assert!(result.payload.is_none());
    }
    for path in [graph, root.path().join(".devcouncil/repo_map.json")] {
        fs::File::create(path)
            .unwrap()
            .set_len(128 * 1024 * 1024 + 1)
            .unwrap();
    }
    for result in [
        devmap::load_code_graph_viz(repo, None, None),
        devmap::load_map_preview(repo),
    ] {
        assert!(!result.available);
        assert!(result.reason.unwrap().contains("limit"));
    }
    let result = devmap::load_repo_map(repo);
    assert!(!result.available);
    assert!(result.reason.unwrap().contains("limit"));
}

#[test]
fn advisory_status_never_migrates_the_writers_database() {
    let root = repository();
    let repo = root.path().to_str().unwrap();
    let path = codeintel::devmap_db_path(repo);
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    drop(devmap_store::Store::open(&path).unwrap());
    let old_schema = devmap_store::CURRENT_SCHEMA_VERSION - 1;
    let conn = rusqlite::Connection::open(&path).unwrap();
    conn.pragma_update(None, "user_version", old_schema)
        .unwrap();
    drop(conn);
    let before = fs::read(&path).unwrap();
    let result = codeintel::status(repo);
    assert_eq!(
        devmap_store::Store::stored_schema_version(&path).unwrap(),
        Some(old_schema),
        "advisory status migrated the index"
    );
    assert_eq!(
        fs::read(&path).unwrap(),
        before,
        "advisory status rewrote the database"
    );
    assert!(!result.available);
    assert!(result.reason.unwrap().contains("schema"));
}

/// Explicit opt-in keeps an installed schema-19 binary out of schema-20 tests.
#[test]
#[ignore = "requires GITPULSE_DEVMAP_BIN pointing to the candidate CLI"]
fn candidate_cli_maps_are_isolated_and_readable_by_the_embedding() {
    let binary = std::env::var("GITPULSE_DEVMAP_BIN").expect("set the candidate binary explicitly");
    assert_eq!(devmap::cli::resolve_binary().unwrap().path, binary);
    let repos = [repository(), repository()];
    for (i, repo) in repos.iter().enumerate() {
        fs::write(
            repo.path().join("source.py"),
            format!(
                "def worktree_{i}():\n    return 1\ndef caller_{i}():\n    return worktree_{i}()\n"
            ),
        )
        .unwrap();
        let result = devmap::cli::build(repo.path().to_str().unwrap()).unwrap();
        assert!(result.ok && !result.timed_out, "{}", result.stderr);
        assert!(
            result.report.is_some(),
            "the CLI must return a complete build report"
        );
    }
    for (i, repo) in repos.iter().enumerate() {
        let root = repo.path().to_str().unwrap();
        let status = codeintel::status(root);
        assert!(status.available, "{:?}", status.reason);
        let own = codeintel::search(root, &format!("worktree_{i}"), Some(2000));
        assert!(own.available, "{:?}", own.reason);
        assert_eq!(own.total, 1);
        let foreign = codeintel::search(root, &format!("worktree_{}", 1 - i), Some(2000));
        assert!(foreign.available, "{:?}", foreign.reason);
        assert_eq!(foreign.total, 0);
        let impact = codeintel::impact(root, &format!("worktree_{i}"), Some(2000));
        assert!(impact.available, "{:?}", impact.reason);
        assert!(
            !impact.items.is_empty(),
            "the mapped caller must be discoverable"
        );
    }
}
