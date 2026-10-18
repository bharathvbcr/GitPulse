use super::*;
use crate::graph::{MAINLINE_COLOR, MAINLINE_COLUMN};
use crate::procguard::LockedSpawn;

#[test]
fn slow_command_is_visible_in_diagnostics_without_recording_its_payload() {
    crate::logging::init();
    crate::logging::performance::enable_test_facade();
    let result = tauri::async_runtime::block_on(off_thread(|| {
        std::thread::sleep(std::time::Duration::from_millis(1100));
        Err::<(), _>("private command payload".to_string())
    }));
    assert_eq!(result.unwrap_err().message, "private command payload");
    let lines = crate::logging::diagnostic_tail(500);
    let line = lines.iter().find(|line| {
        line.contains("[performance]") && line.contains("slow_command_is_visible_in_diagnostics")
    });
    let line = line.expect("a slow failed command must leave a timing record");
    assert!(line.contains("outcome=error"), "{line}");
    assert!(line.contains("queue_ms="), "{line}");
    assert!(line.contains("work_ms="), "{line}");
    assert!(!line.contains("private command payload"), "{line}");
}

/// A call that never returns used to leave no record at all. It must be
/// visible while it is still running, and its end must be recorded too.
#[test]
fn running_command_is_visible_in_diagnostics_before_it_finishes() {
    crate::logging::init();
    crate::logging::performance::enable_test_facade();
    let (release, blocked) = std::sync::mpsc::channel::<()>();
    let call = tauri::async_runtime::spawn(off_thread(move || {
        blocked
            .recv_timeout(std::time::Duration::from_secs(30))
            .map_err(|e| e.to_string())
    }));
    let marker = "running_command_is_visible_in_diagnostics";
    let find = |needle: &str| {
        crate::logging::diagnostic_tail(500)
            .into_iter()
            .find(|line| line.contains(needle) && line.contains(marker))
    };
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(15);
    let running = loop {
        if let Some(line) = find("operation still running") {
            break line;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "no still-running record within 15 s"
        );
        std::thread::sleep(std::time::Duration::from_millis(100));
    };
    assert!(running.contains("calls=1"), "{running}");
    assert!(running.contains("waiting_for_worker=0"), "{running}");
    assert!(running.contains("oldest_ms="), "{running}");
    release.send(()).unwrap();
    tauri::async_runtime::block_on(call).unwrap().unwrap();
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
    while find("operation no longer running").is_none() {
        assert!(
            std::time::Instant::now() < deadline,
            "no end record after release"
        );
        std::thread::sleep(std::time::Duration::from_millis(100));
    }
}

/// Pacing holds a second slow call inside its 30-second window; exit must
/// write it rather than drop it, along with any call still running.
#[test]
fn exit_flush_writes_paced_slow_calls_and_unfinished_calls() {
    crate::logging::init();
    crate::logging::performance::enable_test_facade();
    let slow = || {
        tauri::async_runtime::block_on(off_thread(|| {
            std::thread::sleep(std::time::Duration::from_millis(1100));
            Ok::<_, String>(())
        }))
        .unwrap()
    };
    let marker = "exit_flush_writes_paced_slow_calls";
    slow();
    slow();
    let (release, blocked) = std::sync::mpsc::channel::<()>();
    let call = tauri::async_runtime::spawn(off_thread(move || {
        blocked
            .recv_timeout(std::time::Duration::from_secs(30))
            .map_err(|e| e.to_string())
    }));
    std::thread::sleep(std::time::Duration::from_millis(200));
    crate::logging::performance::flush();
    release.send(()).unwrap();
    tauri::async_runtime::block_on(call).unwrap().unwrap();
    let lines = crate::logging::diagnostic_tail(500);
    let summary = lines
        .iter()
        .find(|line| line.contains("slow command summary at exit") && line.contains(marker))
        .expect("the paced second slow call must be flushed at exit");
    assert!(summary.contains("slow_calls_since_report=1"), "{summary}");
    assert!(lines
        .iter()
        .any(|line| line.contains("operation unfinished at exit") && line.contains(marker)));
}

fn commit(id: &str, parents: &[&str], author: &str, summary: &str) -> RawCommitNode {
    RawCommitNode {
        id: id.to_string(),
        parent_ids: parents.iter().map(|p| p.to_string()).collect(),
        timestamp: 1,
        author_name: author.to_string(),
        author_email: format!("{}@example.com", author.to_lowercase()),
        summary: summary.to_string(),
    }
}

/// main: m0 (merge of feature f1) -> m1 -> m2 -> m3; f1 forks from m2.
fn history() -> Vec<RawCommitNode> {
    vec![
        commit("m0", &["m1", "f1"], "Ada", "feat: merge feature"),
        commit("f1", &["m2"], "Bob", "fix: feature work"),
        commit("m1", &["m2"], "Ada", "chore: main work"),
        commit("m2", &["m3"], "Bob", "fix: shared base"),
        commit("m3", &[], "Ada", "feat: root"),
    ]
}

fn main_ref() -> Vec<RefDecoration> {
    vec![RefDecoration {
        name: "main".to_string(),
        kind: RefKind::Local,
        commit_id: "m0".to_string(),
        is_head: true,
        remote: None,
    }]
}

fn assemble(query: &str) -> CommitGraphPayload {
    assemble_commit_graph(
        history(),
        10,
        &CommitFilter::parse(query),
        main_ref(),
        Some("main"),
        Some("m0".to_string()),
        GraphNotes::default(),
    )
}

/// The parent cap's cut reaches the screen as a notice, not just the log.
#[test]
fn a_filtered_graph_that_hit_the_parent_cap_says_so() {
    let beyond: Vec<String> = (0..40).map(|n| format!("beyond{n}")).collect();
    let refs: Vec<&str> = beyond.iter().map(String::as_str).collect();
    let raw = vec![
        commit("a0", &refs, "Ada", "feat: wide octopus"),
        commit("b0", &[], "Bob", "fix: unrelated"),
    ];
    let payload = assemble_commit_graph(
        raw,
        10,
        &CommitFilter::parse("author:Ada"),
        Vec::new(),
        None,
        None,
        GraphNotes::default(),
    );
    assert!(
        payload.notices.iter().any(|n| n.contains("parent link")),
        "{:?}",
        payload.notices
    );
}

fn ids(payload: &CommitGraphPayload) -> Vec<&str> {
    payload.rows.iter().map(|r| r.id.as_str()).collect()
}

/// Every stub in a filtered graph must point outside the payload: a
/// dropped parent is relinked, never left as a fading line.
fn assert_connected(payload: &CommitGraphPayload) {
    let loaded: HashSet<&str> = payload.rows.iter().map(|r| r.id.as_str()).collect();
    for row in &payload.rows {
        for (k, conn) in row.connections.iter().enumerate() {
            let parent = row.parent_ids.get(k).map(String::as_str).unwrap_or("");
            assert!(
                !conn.is_dangling || !loaded.contains(parent),
                "{} -> {parent}: stub points at a loaded row",
                row.id
            );
        }
    }
    for row in payload.rows.iter().filter(|r| r.is_mainline) {
        assert_eq!(
            row.lane, MAINLINE_COLUMN,
            "{} left the mainline column",
            row.id
        );
        assert_eq!(
            row.color_index, MAINLINE_COLOR,
            "{} left the mainline colour",
            row.id
        );
    }
}

#[test]
fn no_filter_keeps_the_window_and_pins_main() {
    let payload = assemble("");
    assert_eq!(ids(&payload), ["m0", "f1", "m1", "m2", "m3"]);
    assert_eq!(payload.mainline_id.as_deref(), Some("m0"));
    assert_eq!(payload.mainline_name.as_deref(), Some("main"));
    assert!(!payload.has_more);
    assert_connected(&payload);
    let on_rail: Vec<&str> = payload
        .rows
        .iter()
        .filter(|r| r.is_mainline)
        .map(|r| r.id.as_str())
        .collect();
    assert_eq!(on_rail, ["m0", "m1", "m2", "m3"]);
}

/// Pre-fix, `retain` left m0 naming f1 and m1 naming m2 — both gone —
/// so the filtered graph was two stubs and a root.
#[test]
fn author_filter_relinks_survivors_and_keeps_main_straight() {
    let payload = assemble("author:ada");
    assert_eq!(ids(&payload), ["m0", "m1", "m3"]);
    assert_connected(&payload);
    let m0 = &payload.rows[0];
    assert_eq!(
        m0.parent_ids,
        ["m1", "m3"],
        "f1's lineage collapses onto m3"
    );
    assert!(m0.connections.iter().all(|c| !c.is_dangling));
    assert_eq!(payload.rows[1].parent_ids, ["m3"]);
    assert!(
        payload.rows.iter().all(|r| r.is_mainline),
        "every survivor is on main"
    );
    assert_eq!(payload.mainline_name.as_deref(), Some("main"));
}

/// The filter drops main's tip: the rail re-anchors on the first
/// survivor of the ORIGINAL chain and keeps the branch's name.
#[test]
fn a_filter_that_drops_the_tip_re_anchors_on_the_chain_s_first_survivor() {
    let payload = assemble("author:bob");
    assert_eq!(ids(&payload), ["f1", "m2"]);
    assert_connected(&payload);
    assert_eq!(payload.mainline_id.as_deref(), Some("m2"));
    assert_eq!(payload.mainline_name.as_deref(), Some("main"));
    assert!(payload.rows[1].is_mainline && !payload.rows[0].is_mainline);
    assert_eq!(payload.rows[0].parent_ids, ["m2"]);
    assert!(
        payload.rows[1].parent_ids.is_empty(),
        "m3 dropped: m2 is the filtered root"
    );
    assert!(payload.rows[1].is_root);
}

/// Nothing on main survives: the newest survivor's own chain is pinned
/// and the rail is unnamed rather than mislabelled `main`.
#[test]
fn a_filter_with_no_main_survivor_pins_the_newest_survivor_unnamed() {
    let payload = assemble("sha:f1");
    assert_eq!(ids(&payload), ["f1"]);
    assert_eq!(payload.mainline_id.as_deref(), Some("f1"));
    assert_eq!(payload.mainline_name, None);
    assert_connected(&payload);
}

/// Free text and conventional types go through the same path.
#[test]
fn text_and_type_filters_stay_connected() {
    let payload = assemble("fix:");
    assert_eq!(ids(&payload), ["f1", "m2"]);
    assert_connected(&payload);
    let payload = assemble("root merge");
    assert!(
        payload.rows.is_empty(),
        "conjunction over words matches nothing here"
    );
    let payload = assemble("feat:");
    assert_eq!(ids(&payload), ["m0", "m3"]);
    assert_eq!(
        payload.rows[0].parent_ids,
        ["m3"],
        "m1, m2 and f1 all collapse onto m3"
    );
    assert_connected(&payload);
}

/// `path:` is git's job (the walk already rewrote parents); on its own
/// it must not trigger a second rewrite or move the anchor.
#[test]
fn a_path_only_filter_does_not_resimplify() {
    let payload = assemble("path:src");
    assert_eq!(ids(&payload), ["m0", "f1", "m1", "m2", "m3"]);
    assert_eq!(payload.rows[0].parent_ids, ["m1", "f1"]);
    assert_eq!(payload.mainline_name.as_deref(), Some("main"));
}

/// The has_more probe row is dropped before filtering, so a filter can
/// never resurrect the row past the cap.
#[test]
fn the_probe_row_is_cut_before_the_filter_runs() {
    let payload = assemble_commit_graph(
        history(),
        4,
        &CommitFilter::parse("author:ada"),
        main_ref(),
        Some("main"),
        None,
        GraphNotes {
            warnings: vec!["HEAD unavailable".to_string()],
            ..GraphNotes::default()
        },
    );
    assert!(payload.has_more);
    assert_eq!(ids(&payload), ["m0", "m1"]);
    // m3 is past the cap: the relinked edge to it is an honest stub.
    assert_eq!(payload.rows[1].parent_ids, ["m3"]);
    assert!(payload.rows[1].connections[0].is_dangling);
    assert_eq!(payload.warnings, ["HEAD unavailable"]);
    assert_connected(&payload);
}

/// A stale persisted repository path is untrusted input. Before this
/// guard, asking for its ledger status created
/// `<missing>/.devcouncil/ledger.sqlite`, turning a broken session restore
/// into an unexpected filesystem mutation outside any repository.
#[test]
fn ledger_commands_refuse_a_missing_repo_without_creating_it() {
    let root = tempfile::tempdir().expect("temporary parent");
    let missing = root.path().join("removed-repository");

    let status_result =
        tauri::async_runtime::block_on(cmd_ledger_status(missing.to_string_lossy().into_owned()));
    let tail_result = tauri::async_runtime::block_on(cmd_ledger_tail(
        missing.to_string_lossy().into_owned(),
        0,
        100,
        None,
    ));
    let catch_up_result =
        tauri::async_runtime::block_on(cmd_catch_up(missing.to_string_lossy().into_owned()));
    let snapshot_result = tauri::async_runtime::block_on(cmd_record_pulse_snapshot(
        missing.to_string_lossy().into_owned(),
        crate::ledger::PulseSnapshotInput {
            day: "2026-09-03".to_string(),
            total_commits: 1,
            total_loc: 1,
            bus_factor: 1,
            coverage_pct: None,
            snapshot_json: "{}".to_string(),
        },
    ));
    let snapshots_result = tauri::async_runtime::block_on(cmd_get_pulse_snapshots(
        missing.to_string_lossy().into_owned(),
        Some(10),
    ));

    assert!(status_result.is_err(), "ledger status must reject it");
    assert!(tail_result.is_err(), "ledger tail must reject it");
    assert!(catch_up_result.is_err(), "catch-up must reject it");
    assert!(snapshot_result.is_err(), "snapshot writes must reject it");
    assert!(snapshots_result.is_err(), "snapshot reads must reject it");
    assert!(
        !missing.exists(),
        "a read must not create the missing repository or its ledger"
    );
}

/// Opening a linked worktree must address the same durable journal as the
/// main checkout. Otherwise an append notification and a later reload use
/// different cursors, making sibling actions either disappear or replay.
#[test]
fn ledger_commands_resolve_a_linked_worktree_to_the_family_anchor() {
    let main = tempfile::tempdir().expect("main checkout");
    let git = |args: &[&str]| {
        let output = std::process::Command::new("git")
            .args([
                "-c",
                "user.name=GitPulse",
                "-c",
                "user.email=gitpulse@test.local",
                "-c",
                "commit.gpgsign=false",
            ])
            .args(args)
            .current_dir(main.path())
            .output_locked()
            .expect("spawn git");
        assert!(
            output.status.success(),
            "git {args:?} failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
    };
    git(&["init", "-b", "main"]);
    std::fs::write(main.path().join("seed.txt"), "seed").expect("seed repository");
    git(&["add", "seed.txt"]);
    git(&["commit", "-m", "seed"]);

    let parent = tempfile::tempdir().expect("worktree parent");
    let linked = parent.path().join("linked");
    git(&[
        "worktree",
        "add",
        "--detach",
        linked.to_str().expect("utf8 worktree"),
    ]);
    let main_path = main.path().canonicalize().expect("main canonical");
    crate::test_support::trust_repo(&main_path);
    let linked_path = linked.canonicalize().expect("linked canonical");
    crate::test_support::trust_repo(&linked_path);
    crate::ledger::append(crate::ledger::Draft {
        repo_path: main_path.to_string_lossy().into_owned(),
        worktree_path: Some(linked_path.to_string_lossy().into_owned()),
        action: "file.modify".to_string(),
        actor_kind: Some(crate::ledger::ActorKind::Human),
        outcome: Some(crate::ledger::Outcome::Ok),
        ..Default::default()
    })
    .expect("family row");

    let status = tauri::async_runtime::block_on(cmd_ledger_status(
        linked_path.to_string_lossy().into_owned(),
    ))
    .expect("status from linked worktree");
    assert_eq!(
        status.path,
        main_path
            .join(".devcouncil/ledger.sqlite")
            .display()
            .to_string()
    );
    let events = tauri::async_runtime::block_on(cmd_ledger_tail(
        linked_path.to_string_lossy().into_owned(),
        0,
        100,
        None,
    ))
    .expect("tail from linked worktree");
    assert_eq!(events.len(), 1, "the shared row must appear exactly once");
    assert_eq!(events[0].repo_path, main_path.to_string_lossy());
    assert_eq!(
        events[0].worktree_path.as_deref(),
        Some(linked_path.to_string_lossy().as_ref())
    );

    tauri::async_runtime::block_on(cmd_record_pulse_snapshot(
        linked_path.to_string_lossy().into_owned(),
        crate::ledger::PulseSnapshotInput {
            day: "2026-09-03".to_string(),
            total_commits: 7,
            total_loc: 42,
            bus_factor: 2,
            coverage_pct: Some(88.5),
            snapshot_json: r#"{"source":"linked"}"#.to_string(),
        },
    ))
    .expect("record snapshot through linked worktree");
    let snapshots = tauri::async_runtime::block_on(cmd_get_pulse_snapshots(
        main_path.to_string_lossy().into_owned(),
        Some(10),
    ))
    .expect("read shared snapshots from main checkout");
    assert_eq!(snapshots.len(), 1);
    assert_eq!(snapshots[0].repo_path, main_path.to_string_lossy());
    assert_eq!(snapshots[0].total_commits, 7);
}

/// A command's error reaches the frontend typed, with the text a person reads
/// exactly what the engine wrote: the string it used to reject with.
#[test]
fn a_stale_stash_refusal_is_typed_and_its_text_is_the_engines() {
    let repo = crate::test_support::git_repo();
    let path = repo.path().to_string_lossy().into_owned();
    let engine = crate::engine::stash::run_action_with(
        &path,
        StashAction::Drop,
        3,
        "abcdef12",
        |_: &[&str]| Ok(()),
    )
    .expect_err("an empty stack holds no entry 3");

    let error = tauri::async_runtime::block_on(cmd_stash_action(
        path,
        StashAction::Drop,
        3,
        "abcdef12".to_string(),
    ))
    .expect_err("the command refuses");
    assert_eq!(error.kind, crate::ipc_error::IpcErrorKind::StaleStash);
    assert_eq!(error.message, engine);
    let wire = serde_json::to_value(&error).expect("serialises");
    assert_eq!(wire["kind"], "stale_stash");
    assert_eq!(wire["message"], engine.as_str());
}

#[test]
fn an_unapproved_repository_is_typed_and_its_text_is_the_trust_refusal() {
    let dir = tempfile::TempDir::new().expect("tempdir");
    let status = std::process::Command::new("git")
        .args(["init", "-b", "main"])
        .current_dir(dir.path())
        .status_locked()
        .expect("git init");
    assert!(status.success());
    let path = dir.path().to_string_lossy().into_owned();
    let engine = resolve_repo(&path).expect_err("nobody approved this repository");

    let error = tauri::async_runtime::block_on(cmd_resolve_repo(path)).expect_err("refused");
    assert_eq!(
        error.kind,
        crate::ipc_error::IpcErrorKind::RepositoryTrustRequired
    );
    assert_eq!(error.message, engine);
}

#[test]
fn a_command_error_that_says_nothing_reaches_the_frontend_unclassified() {
    let error = tauri::async_runtime::block_on(off_thread(|| {
        Err::<(), _>("Nothing to commit".to_string())
    }))
    .expect_err("the body failed");
    assert_eq!(error.kind, crate::ipc_error::IpcErrorKind::Unclassified);
    assert_eq!(error.message, "Nothing to commit");
}
